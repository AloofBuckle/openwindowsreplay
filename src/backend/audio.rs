//! 音频后端基础结构。
//!
//! 文档目标是 WASAPI loopback + 麦克风 -> 48k stereo float PCM -> AAC LC -> MP4。
//! 本模块先实现与平台无关的时间戳保持、声道混合、有状态带限重采样和 AAC 1024-sample
//! 输入分块；实际 WASAPI 采集与 Media Foundation AAC 编码后续接到这些结构上。
#![allow(dead_code)]

use crate::error::BackendError;
use std::collections::VecDeque;

pub const TARGET_SAMPLE_RATE: u32 = 48_000;
pub const TARGET_CHANNELS: u16 = 2;
pub const AAC_LC_FRAME_SAMPLES: usize = 1024;
const HNS_PER_SECOND: i128 = 10_000_000;
const RESAMPLE_FILTER_RADIUS: i64 = 32;
const MAX_RESAMPLE_GAP_SECONDS: i64 = 5;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum AudioSourceKind {
    Loopback,
    Microphone,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct PcmFormat {
    pub sample_rate: u32,
    pub channels: u16,
}

impl PcmFormat {
    pub const fn target() -> Self {
        Self {
            sample_rate: TARGET_SAMPLE_RATE,
            channels: TARGET_CHANNELS,
        }
    }

    fn validate(self) -> Result<(), BackendError> {
        if self.sample_rate == 0 || self.channels == 0 {
            return Err(BackendError::AudioUnsupported {
                reason: format!(
                    "PCM 格式非法: sample_rate={} channels={}",
                    self.sample_rate, self.channels
                ),
            });
        }
        Ok(())
    }
}

#[derive(Debug, Clone)]
pub struct PcmFrame {
    pub source: AudioSourceKind,
    /// 绝对时间戳，单位 100ns；来自 WASAPI QPC/device position 映射后不得丢弃。
    pub start_time_100ns: i64,
    pub format: PcmFormat,
    /// interleaved f32 PCM，范围允许暂时超过 [-1,1]，最终 mix 时限幅。
    pub samples: Vec<f32>,
}

impl PcmFrame {
    pub fn frame_count(&self) -> usize {
        let channels = usize::from(self.format.channels.max(1));
        self.samples.len() / channels
    }

    pub fn duration_100ns(&self) -> i64 {
        samples_to_100ns(self.frame_count() as u64, self.format.sample_rate)
    }

    pub fn end_time_100ns(&self) -> i64 {
        self.start_time_100ns.saturating_add(self.duration_100ns())
    }

    pub fn validate(&self) -> Result<(), BackendError> {
        self.format.validate()?;
        if !self
            .samples
            .len()
            .is_multiple_of(usize::from(self.format.channels))
        {
            return Err(BackendError::AudioUnsupported {
                reason: format!(
                    "PCM sample 数量 {} 不能被声道数 {} 整除",
                    self.samples.len(),
                    self.format.channels
                ),
            });
        }
        Ok(())
    }
}

#[derive(Debug, Clone)]
pub struct StereoPcmFrame {
    pub start_time_100ns: i64,
    pub samples: Vec<[f32; 2]>,
}

impl StereoPcmFrame {
    pub fn duration_100ns(&self) -> i64 {
        samples_to_100ns(self.samples.len() as u64, TARGET_SAMPLE_RATE)
    }

    pub fn end_time_100ns(&self) -> i64 {
        self.start_time_100ns.saturating_add(self.duration_100ns())
    }
}

/// Stateful, packet-boundary-independent PCM normalizer.
///
/// The resampling phase is anchored to the first packet timestamp and the
/// windowed-sinc kernel keeps enough history/future input to make packetized
/// WASAPI input equivalent to one continuous stream. Downsampling uses a
/// band-limited kernel so source content above the 48 kHz Nyquist frequency is
/// rejected instead of being folded into the saved replay.
#[derive(Debug)]
pub struct PcmStreamNormalizer {
    source: AudioSourceKind,
    input_format: Option<PcmFormat>,
    base_time_100ns: i64,
    input: VecDeque<[f32; 2]>,
    input_start_index: i64,
    input_end_index: i64,
    next_output_index: u64,
}

impl PcmStreamNormalizer {
    pub fn new(source: AudioSourceKind) -> Self {
        Self {
            source,
            input_format: None,
            base_time_100ns: 0,
            input: VecDeque::new(),
            input_start_index: 0,
            input_end_index: 0,
            next_output_index: 0,
        }
    }

    pub fn push(&mut self, frame: &PcmFrame) -> Result<Vec<PcmFrame>, BackendError> {
        frame.validate()?;
        if frame.source != self.source {
            return Err(BackendError::AudioUnsupported {
                reason: format!(
                    "音频流归一化器 source={:?} 收到 {:?} packet",
                    self.source, frame.source
                ),
            });
        }

        let mut out = Vec::new();
        if self
            .input_format
            .is_some_and(|format| format != frame.format)
        {
            out.extend(self.finish());
        }

        let stereo = remix_to_stereo(frame);
        if frame.format.sample_rate == TARGET_SAMPLE_RATE {
            if self.input_format.is_some() {
                out.extend(self.finish());
            }
            if !stereo.is_empty() {
                out.push(pcm_frame_from_stereo(
                    self.source,
                    frame.start_time_100ns,
                    stereo,
                ));
            }
            return Ok(out);
        }

        if self.input_format.is_none() {
            self.input_format = Some(frame.format);
            self.base_time_100ns = frame.start_time_100ns;
            self.input_start_index = 0;
            self.input_end_index = 0;
            self.next_output_index = 0;
        }

        let mut packet_start = duration_100ns_to_samples_round_signed(
            frame.start_time_100ns.saturating_sub(self.base_time_100ns),
            frame.format.sample_rate,
        );
        if packet_start > self.input_end_index {
            let gap = packet_start - self.input_end_index;
            let max_gap =
                i64::from(frame.format.sample_rate).saturating_mul(MAX_RESAMPLE_GAP_SECONDS);
            if gap > max_gap {
                // A device timestamp jump can be arbitrarily large after a
                // sleep/reconnect. Do not materialize unbounded silence; the
                // mixer already restores silence from the absolute timestamps.
                out.extend(self.finish());
                self.input_format = Some(frame.format);
                self.base_time_100ns = frame.start_time_100ns;
                self.input_start_index = 0;
                self.input_end_index = 0;
                self.next_output_index = 0;
                packet_start = 0;
            } else {
                let gap = usize::try_from(gap).map_err(|_| BackendError::AudioUnsupported {
                    reason: format!(
                        "PCM 时间戳间隙 {} samples 超出当前平台可寻址范围",
                        packet_start - self.input_end_index
                    ),
                })?;
                self.input.extend(std::iter::repeat_n([0.0, 0.0], gap));
                self.input_end_index = packet_start;
            }
        }
        let overlap = self.input_end_index.saturating_sub(packet_start).max(0) as usize;
        if overlap < stereo.len() {
            self.input.extend(stereo[overlap..].iter().copied());
            self.input_end_index = self
                .input_end_index
                .saturating_add((stereo.len() - overlap) as i64);
        }
        if let Some(frame) = self.emit_ready(false) {
            out.push(frame);
        }
        Ok(out)
    }

    pub fn finish(&mut self) -> Vec<PcmFrame> {
        let mut out = Vec::new();
        if let Some(frame) = self.emit_ready(true) {
            out.push(frame);
        }
        self.input_format = None;
        self.input.clear();
        self.input_start_index = 0;
        self.input_end_index = 0;
        self.next_output_index = 0;
        out
    }

    fn emit_ready(&mut self, flush: bool) -> Option<PcmFrame> {
        let input_rate = self.input_format?.sample_rate;
        if self.input_end_index <= 0 {
            return None;
        }
        let first_output_index = self.next_output_index;
        let flush_limit = ((self.input_end_index as u128 * u128::from(TARGET_SAMPLE_RATE))
            .div_ceil(u128::from(input_rate)))
        .min(u128::from(u64::MAX)) as u64;
        let mut samples = Vec::new();
        while self.next_output_index < flush_limit {
            let source_numerator = u128::from(self.next_output_index) * u128::from(input_rate);
            let source_center =
                (source_numerator / u128::from(TARGET_SAMPLE_RATE)).min(i64::MAX as u128) as i64;
            if !flush
                && source_center.saturating_add(RESAMPLE_FILTER_RADIUS) >= self.input_end_index
            {
                break;
            }
            samples.push(self.interpolate(source_numerator, input_rate));
            self.next_output_index = self.next_output_index.saturating_add(1);
        }
        self.prune_consumed_input(input_rate);
        (!samples.is_empty()).then(|| {
            pcm_frame_from_stereo(
                self.source,
                self.base_time_100ns
                    .saturating_add(samples_to_100ns(first_output_index, TARGET_SAMPLE_RATE)),
                samples,
            )
        })
    }

    fn interpolate(&self, source_numerator: u128, input_rate: u32) -> [f32; 2] {
        let denominator = u128::from(TARGET_SAMPLE_RATE);
        let center = (source_numerator / denominator).min(i64::MAX as u128) as i64;
        let remainder = source_numerator % denominator;
        let cutoff = (TARGET_SAMPLE_RATE as f64 / input_rate as f64).min(1.0);
        if cutoff == 1.0 && remainder == 0 {
            return self.sample_at(center);
        }
        let source_position = center as f64 + remainder as f64 / TARGET_SAMPLE_RATE as f64;
        let radius = RESAMPLE_FILTER_RADIUS as f64;
        let mut left = 0.0f64;
        let mut right = 0.0f64;
        let mut weight_sum = 0.0f64;
        for index in center.saturating_sub(RESAMPLE_FILTER_RADIUS)
            ..=center.saturating_add(RESAMPLE_FILTER_RADIUS)
        {
            let distance = source_position - index as f64;
            let normalized = distance / radius;
            if normalized.abs() >= 1.0 {
                continue;
            }
            let window = 0.42
                + 0.5 * (std::f64::consts::PI * normalized).cos()
                + 0.08 * (2.0 * std::f64::consts::PI * normalized).cos();
            let sinc_argument = cutoff * distance;
            let sinc = if sinc_argument.abs() < 1.0e-12 {
                1.0
            } else {
                let angle = std::f64::consts::PI * sinc_argument;
                angle.sin() / angle
            };
            let weight = cutoff * sinc * window;
            let sample = self.sample_at(index);
            left += f64::from(sample[0]) * weight;
            right += f64::from(sample[1]) * weight;
            weight_sum += weight;
        }
        if weight_sum.abs() < 1.0e-12 {
            [0.0, 0.0]
        } else {
            [(left / weight_sum) as f32, (right / weight_sum) as f32]
        }
    }

    fn sample_at(&self, index: i64) -> [f32; 2] {
        if index < self.input_start_index || index >= self.input_end_index {
            return [0.0, 0.0];
        }
        self.input
            .get((index - self.input_start_index) as usize)
            .copied()
            .unwrap_or([0.0, 0.0])
    }

    fn prune_consumed_input(&mut self, input_rate: u32) {
        let next_source_center = ((u128::from(self.next_output_index) * u128::from(input_rate))
            / u128::from(TARGET_SAMPLE_RATE))
        .min(i64::MAX as u128) as i64;
        let keep_from = next_source_center
            .saturating_sub(RESAMPLE_FILTER_RADIUS)
            .saturating_sub(1);
        let remove = keep_from
            .saturating_sub(self.input_start_index)
            .max(0)
            .min(self.input.len() as i64) as usize;
        self.input.drain(..remove);
        self.input_start_index = self.input_start_index.saturating_add(remove as i64);
    }
}

#[derive(Debug, Clone)]
pub struct AacPcmBlock {
    /// 48kHz audio timescale 下的绝对 sample tick。
    pub timestamp_ticks: u64,
    /// 1024 个 stereo f32 frame，interleaved 供 AAC encoder 输入。
    pub interleaved: Vec<f32>,
}

#[derive(Debug, Default)]
pub struct AacBlocker {
    next_timestamp_ticks: Option<u64>,
    pending: Vec<[f32; 2]>,
}

impl AacBlocker {
    pub fn push(&mut self, frame: &StereoPcmFrame) -> Vec<AacPcmBlock> {
        if self.next_timestamp_ticks.is_none() {
            self.next_timestamp_ticks = Some(time_100ns_to_sample_ticks(frame.start_time_100ns));
        }
        self.pending.extend_from_slice(&frame.samples);
        let mut out = Vec::new();
        while self.pending.len() >= AAC_LC_FRAME_SAMPLES {
            let timestamp_ticks = self.next_timestamp_ticks.unwrap_or(0);
            let mut interleaved = Vec::with_capacity(AAC_LC_FRAME_SAMPLES * 2);
            for [l, r] in self.pending.drain(..AAC_LC_FRAME_SAMPLES) {
                interleaved.push(l);
                interleaved.push(r);
            }
            self.next_timestamp_ticks = Some(timestamp_ticks + AAC_LC_FRAME_SAMPLES as u64);
            out.push(AacPcmBlock {
                timestamp_ticks,
                interleaved,
            });
        }
        out
    }

    pub fn flush_padded(&mut self) -> Option<AacPcmBlock> {
        if self.pending.is_empty() {
            return None;
        }
        let timestamp_ticks = self.next_timestamp_ticks.unwrap_or(0);
        while self.pending.len() < AAC_LC_FRAME_SAMPLES {
            self.pending.push([0.0, 0.0]);
        }
        let mut interleaved = Vec::with_capacity(AAC_LC_FRAME_SAMPLES * 2);
        for [l, r] in self.pending.drain(..AAC_LC_FRAME_SAMPLES) {
            interleaved.push(l);
            interleaved.push(r);
        }
        self.next_timestamp_ticks = Some(timestamp_ticks + AAC_LC_FRAME_SAMPLES as u64);
        Some(AacPcmBlock {
            timestamp_ticks,
            interleaved,
        })
    }
}

pub fn normalize_to_stereo_48k(frame: &PcmFrame) -> Result<StereoPcmFrame, BackendError> {
    let mut normalizer = PcmStreamNormalizer::new(frame.source);
    let mut normalized = normalizer.push(frame)?;
    normalized.extend(normalizer.finish());
    let start_time_100ns = normalized
        .first()
        .map(|frame| frame.start_time_100ns)
        .unwrap_or(frame.start_time_100ns);
    let samples = normalized
        .into_iter()
        .flat_map(|frame| {
            frame
                .samples
                .chunks_exact(2)
                .map(|pair| [pair[0], pair[1]])
                .collect::<Vec<_>>()
        })
        .collect();
    Ok(StereoPcmFrame {
        start_time_100ns,
        samples,
    })
}

fn normalize_pcm_streams(frames: &[PcmFrame]) -> Result<Vec<StereoPcmFrame>, BackendError> {
    let mut normalized = Vec::new();
    for source in [AudioSourceKind::Loopback, AudioSourceKind::Microphone] {
        let mut source_frames = frames
            .iter()
            .filter(|frame| frame.source == source)
            .collect::<Vec<_>>();
        source_frames.sort_by_key(|frame| frame.start_time_100ns);
        let mut normalizer = PcmStreamNormalizer::new(source);
        for frame in source_frames {
            for frame in normalizer.push(frame)? {
                normalized.push(stereo_frame_from_target_pcm(frame));
            }
        }
        for frame in normalizer.finish() {
            normalized.push(stereo_frame_from_target_pcm(frame));
        }
    }
    Ok(normalized)
}

pub fn mix_to_stereo_48k(frames: &[PcmFrame]) -> Result<Option<StereoPcmFrame>, BackendError> {
    let normalized = normalize_pcm_streams(frames)?;
    if normalized.is_empty() {
        return Ok(None);
    }

    let start = normalized
        .iter()
        .map(|frame| frame.start_time_100ns)
        .min()
        .unwrap_or(0);
    let end = normalized
        .iter()
        .map(StereoPcmFrame::end_time_100ns)
        .max()
        .unwrap_or(start);
    let output_frames =
        ((end.saturating_sub(start) as i128 * i128::from(TARGET_SAMPLE_RATE) + HNS_PER_SECOND - 1)
            / HNS_PER_SECOND)
            .max(0) as usize;
    let mut mixed = vec![[0.0f32, 0.0f32]; output_frames];

    for frame in normalized {
        let offset = ((frame.start_time_100ns.saturating_sub(start) as i128
            * i128::from(TARGET_SAMPLE_RATE)
            + HNS_PER_SECOND / 2)
            / HNS_PER_SECOND)
            .max(0) as usize;
        for (index, [l, r]) in frame.samples.into_iter().enumerate() {
            if let Some(dst) = mixed.get_mut(offset + index) {
                dst[0] += l;
                dst[1] += r;
            }
        }
    }

    for [l, r] in &mut mixed {
        *l = l.clamp(-1.0, 1.0);
        *r = r.clamp(-1.0, 1.0);
    }

    Ok(Some(StereoPcmFrame {
        start_time_100ns: start,
        samples: mixed,
    }))
}

pub fn mix_window_samples_to_stereo_48k(
    frames: &[PcmFrame],
    window_start_100ns: i64,
    output_samples: usize,
) -> Result<StereoPcmFrame, BackendError> {
    let mut mixed = vec![[0.0f32, 0.0f32]; output_samples];
    if output_samples == 0 {
        return Ok(StereoPcmFrame {
            start_time_100ns: window_start_100ns,
            samples: mixed,
        });
    }

    let window_start_tick = time_100ns_to_sample_ticks(window_start_100ns);
    let window_end_tick = window_start_tick.saturating_add(output_samples as u64);
    for converted in normalize_pcm_streams(frames)? {
        if converted.samples.is_empty() {
            continue;
        }
        let frame_start_tick = time_100ns_to_sample_ticks(converted.start_time_100ns);
        let frame_end_tick = frame_start_tick.saturating_add(converted.samples.len() as u64);
        let overlap_start_tick = frame_start_tick.max(window_start_tick);
        let overlap_end_tick = frame_end_tick.min(window_end_tick);
        if overlap_start_tick >= overlap_end_tick {
            continue;
        }
        let src_offset = overlap_start_tick.saturating_sub(frame_start_tick) as usize;
        let dst_offset = overlap_start_tick.saturating_sub(window_start_tick) as usize;
        let copy_samples = overlap_end_tick.saturating_sub(overlap_start_tick) as usize;
        for index in 0..copy_samples {
            if let (Some(src), Some(dst)) = (
                converted.samples.get(src_offset + index),
                mixed.get_mut(dst_offset + index),
            ) {
                dst[0] += src[0];
                dst[1] += src[1];
            }
        }
    }

    for [l, r] in &mut mixed {
        *l = l.clamp(-1.0, 1.0);
        *r = r.clamp(-1.0, 1.0);
    }

    Ok(StereoPcmFrame {
        start_time_100ns: window_start_100ns,
        samples: mixed,
    })
}

pub fn mix_window_to_stereo_48k(
    frames: &[PcmFrame],
    window_start_100ns: i64,
    window_duration_100ns: i64,
) -> Result<StereoPcmFrame, BackendError> {
    let output_samples = duration_100ns_to_samples_ceil(window_duration_100ns, TARGET_SAMPLE_RATE);
    mix_window_samples_to_stereo_48k(frames, window_start_100ns, output_samples)
}

pub fn clip_and_rebase_to_window(
    frames: &[PcmFrame],
    window_start_100ns: i64,
    window_duration_100ns: i64,
) -> Result<Vec<PcmFrame>, BackendError> {
    let window_end_100ns = window_start_100ns.saturating_add(window_duration_100ns.max(0));
    let mut out = Vec::new();
    for frame in frames {
        frame.validate()?;
        let frame_end = frame.end_time_100ns();
        if frame_end <= window_start_100ns || frame.start_time_100ns >= window_end_100ns {
            continue;
        }
        let rate = frame.format.sample_rate;
        let channels = usize::from(frame.format.channels);
        let frame_count = frame.frame_count();
        let skip_frames = if frame.start_time_100ns < window_start_100ns {
            duration_100ns_to_samples_ceil(
                window_start_100ns.saturating_sub(frame.start_time_100ns),
                rate,
            )
            .min(frame_count)
        } else {
            0
        };
        let clipped_start = frame
            .start_time_100ns
            .saturating_add(samples_to_100ns(skip_frames as u64, rate));
        let available_frames = frame_count.saturating_sub(skip_frames);
        let keep_frames =
            duration_100ns_to_samples_floor(window_end_100ns.saturating_sub(clipped_start), rate)
                .min(available_frames);
        if keep_frames == 0 {
            continue;
        }
        let sample_start = skip_frames * channels;
        let sample_end = sample_start + keep_frames * channels;
        out.push(PcmFrame {
            source: frame.source,
            start_time_100ns: clipped_start.saturating_sub(window_start_100ns).max(0),
            format: frame.format,
            samples: frame.samples[sample_start..sample_end].to_vec(),
        });
    }
    Ok(out)
}

pub fn silence_stereo_48k(duration_100ns: i64) -> StereoPcmFrame {
    let frame_count = duration_100ns_to_samples_ceil(duration_100ns.max(0), TARGET_SAMPLE_RATE);
    StereoPcmFrame {
        start_time_100ns: 0,
        samples: vec![[0.0, 0.0]; frame_count],
    }
}

pub fn aac_blocks_from_stereo(frame: &StereoPcmFrame) -> Vec<AacPcmBlock> {
    let mut blocker = AacBlocker::default();
    let mut blocks = blocker.push(frame);
    if let Some(tail) = blocker.flush_padded() {
        blocks.push(tail);
    }
    blocks
}

fn remix_to_stereo(frame: &PcmFrame) -> Vec<[f32; 2]> {
    let channels = usize::from(frame.format.channels);
    let mut out = Vec::with_capacity(frame.frame_count());
    for chunk in frame.samples.chunks_exact(channels) {
        let pair = match channels {
            1 => [chunk[0], chunk[0]],
            2 => [chunk[0], chunk[1]],
            _ => {
                let left = chunk[0];
                let right = chunk[1];
                let center = chunk.get(2).copied().unwrap_or(0.0) * 0.707_106_77;
                [left + center, right + center]
            }
        };
        out.push(pair);
    }
    out
}

fn pcm_frame_from_stereo(
    source: AudioSourceKind,
    start_time_100ns: i64,
    samples: Vec<[f32; 2]>,
) -> PcmFrame {
    PcmFrame {
        source,
        start_time_100ns,
        format: PcmFormat::target(),
        samples: samples.into_iter().flatten().collect(),
    }
}

fn stereo_frame_from_target_pcm(frame: PcmFrame) -> StereoPcmFrame {
    StereoPcmFrame {
        start_time_100ns: frame.start_time_100ns,
        samples: frame
            .samples
            .chunks_exact(2)
            .map(|pair| [pair[0], pair[1]])
            .collect(),
    }
}

fn duration_100ns_to_samples_round_signed(duration_100ns: i64, sample_rate: u32) -> i64 {
    if sample_rate == 0 {
        return 0;
    }
    let numerator = i128::from(duration_100ns) * i128::from(sample_rate);
    let rounded = if numerator >= 0 {
        (numerator + HNS_PER_SECOND / 2) / HNS_PER_SECOND
    } else {
        (numerator - HNS_PER_SECOND / 2) / HNS_PER_SECOND
    };
    rounded.clamp(i64::MIN as i128, i64::MAX as i128) as i64
}

fn samples_to_100ns(samples: u64, sample_rate: u32) -> i64 {
    if sample_rate == 0 {
        return 0;
    }
    ((u128::from(samples) * HNS_PER_SECOND as u128) / u128::from(sample_rate)) as i64
}

fn duration_100ns_to_samples_floor(duration_100ns: i64, sample_rate: u32) -> usize {
    if duration_100ns <= 0 || sample_rate == 0 {
        return 0;
    }
    ((duration_100ns as i128 * i128::from(sample_rate)) / HNS_PER_SECOND) as usize
}

fn duration_100ns_to_samples_ceil(duration_100ns: i64, sample_rate: u32) -> usize {
    if duration_100ns <= 0 || sample_rate == 0 {
        return 0;
    }
    ((duration_100ns as i128 * i128::from(sample_rate) + HNS_PER_SECOND - 1) / HNS_PER_SECOND)
        as usize
}

fn time_100ns_to_sample_ticks(time_100ns: i64) -> u64 {
    if time_100ns <= 0 {
        return 0;
    }
    ((time_100ns as i128 * i128::from(TARGET_SAMPLE_RATE) + HNS_PER_SECOND / 2) / HNS_PER_SECOND)
        as u64
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn mono_24k_resamples_to_48k_stereo() {
        let frame = PcmFrame {
            source: AudioSourceKind::Loopback,
            start_time_100ns: 1_000,
            format: PcmFormat {
                sample_rate: 24_000,
                channels: 1,
            },
            samples: vec![0.0, 0.5, 1.0, 0.5],
        };
        let out = normalize_to_stereo_48k(&frame).unwrap();
        assert_eq!(out.start_time_100ns, 1_000);
        assert_eq!(out.samples.len(), 8);
        assert_eq!(out.samples[2], [0.5, 0.5]);
    }

    #[test]
    fn mixer_aligns_absolute_timestamps() {
        let a = PcmFrame {
            source: AudioSourceKind::Loopback,
            start_time_100ns: 0,
            format: PcmFormat::target(),
            samples: vec![0.25, 0.25, 0.25, 0.25],
        };
        let one_sample_100ns = samples_to_100ns(1, TARGET_SAMPLE_RATE);
        let b = PcmFrame {
            source: AudioSourceKind::Microphone,
            start_time_100ns: one_sample_100ns,
            format: PcmFormat::target(),
            samples: vec![0.5, 0.5],
        };
        let mixed = mix_to_stereo_48k(&[a, b]).unwrap().unwrap();
        assert_eq!(mixed.samples.len(), 2);
        assert_eq!(mixed.samples[0], [0.25, 0.25]);
        assert_eq!(mixed.samples[1], [0.75, 0.75]);
    }

    #[test]
    fn aac_blocker_preserves_48k_ticks_and_pads_flush() {
        let mut blocker = AacBlocker::default();
        let frame = StereoPcmFrame {
            start_time_100ns: samples_to_100ns(480, TARGET_SAMPLE_RATE),
            samples: vec![[0.1, -0.1]; 1500],
        };
        let blocks = blocker.push(&frame);
        assert_eq!(blocks.len(), 1);
        assert_eq!(blocks[0].timestamp_ticks, 480);
        assert_eq!(blocks[0].interleaved.len(), AAC_LC_FRAME_SAMPLES * 2);
        let tail = blocker.flush_padded().unwrap();
        assert_eq!(tail.timestamp_ticks, 480 + AAC_LC_FRAME_SAMPLES as u64);
        assert_eq!(tail.interleaved.len(), AAC_LC_FRAME_SAMPLES * 2);
    }

    #[test]
    fn clip_and_rebase_keeps_absolute_audio_window() {
        let frame = PcmFrame {
            source: AudioSourceKind::Loopback,
            start_time_100ns: 1_000_000,
            format: PcmFormat {
                sample_rate: 10_000,
                channels: 2,
            },
            samples: (0..20).map(|v| v as f32).collect(),
        };
        let clipped = clip_and_rebase_to_window(&[frame], 1_001_000, 3_000)
            .unwrap()
            .remove(0);
        assert_eq!(clipped.start_time_100ns, 0);
        assert_eq!(clipped.frame_count(), 3);
        assert_eq!(clipped.samples, vec![2.0, 3.0, 4.0, 5.0, 6.0, 7.0]);
    }

    #[test]
    fn window_mixer_outputs_fixed_silence_without_pcm_padding_frame() {
        let mixed = mix_window_samples_to_stereo_48k(&[], 2_000_000, 4).unwrap();
        assert_eq!(mixed.start_time_100ns, 2_000_000);
        assert_eq!(mixed.samples, vec![[0.0, 0.0]; 4]);
    }

    #[test]
    fn window_mixer_clips_and_aligns_absolute_samples() {
        let one_sample_100ns = samples_to_100ns(1, TARGET_SAMPLE_RATE);
        let frame = PcmFrame {
            source: AudioSourceKind::Loopback,
            start_time_100ns: 1_000_000,
            format: PcmFormat::target(),
            samples: vec![0.1, 0.1, 0.2, 0.2, 0.3, 0.3, 0.4, 0.4],
        };
        let mixed =
            mix_window_samples_to_stereo_48k(&[frame], 1_000_000 + one_sample_100ns, 4).unwrap();
        assert_eq!(mixed.samples[0], [0.2, 0.2]);
        assert_eq!(mixed.samples[1], [0.3, 0.3]);
        assert_eq!(mixed.samples[2], [0.4, 0.4]);
        assert_eq!(mixed.samples[3], [0.0, 0.0]);
    }

    #[test]
    fn packetized_44k1_resampling_matches_continuous_stream() {
        let input_rate = 44_100u32;
        let frequency = 10_000.0f32;
        let input = (0..input_rate as usize)
            .map(|index| {
                let phase = std::f32::consts::TAU * frequency * index as f32 / input_rate as f32;
                phase.sin()
            })
            .collect::<Vec<_>>();
        let frame = |start: usize, samples: &[f32]| PcmFrame {
            source: AudioSourceKind::Loopback,
            start_time_100ns: samples_to_100ns(start as u64, input_rate),
            format: PcmFormat {
                sample_rate: input_rate,
                channels: 1,
            },
            samples: samples.to_vec(),
        };
        let continuous = normalize_to_stereo_48k(&frame(0, &input)).unwrap().samples;
        let mut normalizer = PcmStreamNormalizer::new(AudioSourceKind::Loopback);
        let mut packetized_frames = Vec::new();
        for (packet_index, packet) in input.chunks(441).enumerate() {
            packetized_frames.extend(normalizer.push(&frame(packet_index * 441, packet)).unwrap());
        }
        packetized_frames.extend(normalizer.finish());
        let packetized = packetized_frames
            .into_iter()
            .flat_map(|frame| {
                frame
                    .samples
                    .chunks_exact(2)
                    .map(|pair| [pair[0], pair[1]])
                    .collect::<Vec<_>>()
            })
            .collect::<Vec<_>>();
        assert_eq!(packetized.len(), continuous.len());
        let max_error = packetized
            .iter()
            .zip(&continuous)
            .map(|(packet, whole)| (packet[0] - whole[0]).abs())
            .fold(0.0f32, f32::max);
        assert!(max_error < 0.005, "packet boundary max error={max_error}");
    }

    #[test]
    fn irregular_packetized_resampling_preserves_phase_and_sample_count() {
        let input_rate = 44_100u32;
        let input_len = 44_100usize * 2 + 137;
        let input = (0..input_len)
            .map(|index| {
                let phase = std::f32::consts::TAU * 1_000.0 * index as f32 / input_rate as f32;
                phase.sin()
            })
            .collect::<Vec<_>>();
        let frame = |start: usize, samples: &[f32]| PcmFrame {
            source: AudioSourceKind::Loopback,
            start_time_100ns: samples_to_100ns(start as u64, input_rate),
            format: PcmFormat {
                sample_rate: input_rate,
                channels: 1,
            },
            samples: samples.to_vec(),
        };
        let continuous = normalize_to_stereo_48k(&frame(0, &input)).unwrap().samples;
        let mut normalizer = PcmStreamNormalizer::new(AudioSourceKind::Loopback);
        let mut packetized_frames = Vec::new();
        let packet_sizes = [17usize, 503, 7, 1_000, 64, 3, 2_048, 31];
        let mut start = 0usize;
        let mut packet_index = 0usize;
        while start < input.len() {
            let count = packet_sizes[packet_index % packet_sizes.len()]
                .min(input.len().saturating_sub(start));
            packetized_frames.extend(
                normalizer
                    .push(&frame(start, &input[start..start + count]))
                    .unwrap(),
            );
            start += count;
            packet_index += 1;
        }
        packetized_frames.extend(normalizer.finish());
        let packetized = packetized_frames
            .into_iter()
            .flat_map(|frame| {
                frame
                    .samples
                    .chunks_exact(2)
                    .map(|pair| [pair[0], pair[1]])
                    .collect::<Vec<_>>()
            })
            .collect::<Vec<_>>();
        assert_eq!(packetized.len(), continuous.len());
        let max_error = packetized
            .iter()
            .zip(&continuous)
            .map(|(packet, whole)| (packet[0] - whole[0]).abs())
            .fold(0.0f32, f32::max);
        assert!(max_error < 0.005, "irregular packet max error={max_error}");
    }

    #[test]
    fn interleaved_audio_sources_keep_independent_resampler_state() {
        let input_rate = 44_100u32;
        let packet = |source, start, value| PcmFrame {
            source,
            start_time_100ns: samples_to_100ns(start, input_rate),
            format: PcmFormat {
                sample_rate: input_rate,
                channels: 1,
            },
            samples: vec![value; 441],
        };
        let mut loopback = PcmStreamNormalizer::new(AudioSourceKind::Loopback);
        let mut microphone = PcmStreamNormalizer::new(AudioSourceKind::Microphone);
        let mut loopback_frames = Vec::new();
        let mut microphone_frames = Vec::new();
        for index in 0..4u64 {
            loopback_frames.extend(
                loopback
                    .push(&packet(AudioSourceKind::Loopback, index * 441, 0.25))
                    .unwrap(),
            );
            microphone_frames.extend(
                microphone
                    .push(&packet(AudioSourceKind::Microphone, index * 441, -0.75))
                    .unwrap(),
            );
        }
        loopback_frames.extend(loopback.finish());
        microphone_frames.extend(microphone.finish());
        assert!(!loopback_frames.is_empty());
        assert!(!microphone_frames.is_empty());
        assert!(
            loopback_frames
                .iter()
                .all(|frame| frame.source == AudioSourceKind::Loopback)
        );
        assert!(
            microphone_frames
                .iter()
                .all(|frame| frame.source == AudioSourceKind::Microphone)
        );
        let loopback_mean = loopback_frames
            .iter()
            .flat_map(|frame| frame.samples.iter())
            .copied()
            .sum::<f32>();
        let microphone_mean = microphone_frames
            .iter()
            .flat_map(|frame| frame.samples.iter())
            .copied()
            .sum::<f32>();
        assert!(loopback_mean > 0.0);
        assert!(microphone_mean < 0.0);
    }

    #[test]
    fn streaming_resampler_preserves_expected_count_for_multiple_rates() {
        for input_rate in [44_100u32, 88_200, 96_000, 192_000] {
            let input_len = 50_003usize;
            let input = vec![0.125f32; input_len];
            let mut normalizer = PcmStreamNormalizer::new(AudioSourceKind::Loopback);
            let mut output_frames = Vec::new();
            let mut start = 0usize;
            let packet_sizes = [1_003usize, 17, 2_047, 5, 509];
            let mut packet_index = 0usize;
            while start < input.len() {
                let count = packet_sizes[packet_index % packet_sizes.len()]
                    .min(input.len().saturating_sub(start));
                output_frames.extend(
                    normalizer
                        .push(&PcmFrame {
                            source: AudioSourceKind::Loopback,
                            start_time_100ns: samples_to_100ns(start as u64, input_rate),
                            format: PcmFormat {
                                sample_rate: input_rate,
                                channels: 1,
                            },
                            samples: input[start..start + count].to_vec(),
                        })
                        .unwrap(),
                );
                start += count;
                packet_index += 1;
            }
            output_frames.extend(normalizer.finish());
            let actual = output_frames
                .iter()
                .map(|frame| frame.samples.len() / 2)
                .sum::<usize>();
            let expected = (input_len * TARGET_SAMPLE_RATE as usize).div_ceil(input_rate as usize);
            assert_eq!(actual, expected, "rate={input_rate}");
        }
    }

    #[test]
    fn huge_timestamp_gap_reanchors_without_materializing_unbounded_silence() {
        let mut normalizer = PcmStreamNormalizer::new(AudioSourceKind::Loopback);
        let format = PcmFormat {
            sample_rate: 192_000,
            channels: 1,
        };
        normalizer
            .push(&PcmFrame {
                source: AudioSourceKind::Loopback,
                start_time_100ns: 0,
                format,
                samples: vec![0.25; 192],
            })
            .unwrap();
        let gap_start = samples_to_100ns(192_000 * 60, format.sample_rate);
        let output = normalizer
            .push(&PcmFrame {
                source: AudioSourceKind::Loopback,
                start_time_100ns: gap_start,
                format,
                samples: vec![-0.25; 192],
            })
            .unwrap();
        let tail = normalizer.finish();
        assert!(
            output
                .iter()
                .chain(tail.iter())
                .all(|frame| frame.start_time_100ns >= 0)
        );
    }

    #[test]
    fn downsampling_rejects_frequencies_above_target_nyquist() {
        let input_rate = 96_000u32;
        let frequency = 30_000.0f32;
        let input = (0..input_rate as usize)
            .map(|index| {
                let phase = std::f32::consts::TAU * frequency * index as f32 / input_rate as f32;
                phase.sin()
            })
            .collect::<Vec<_>>();
        let output = normalize_to_stereo_48k(&PcmFrame {
            source: AudioSourceKind::Loopback,
            start_time_100ns: 0,
            format: PcmFormat {
                sample_rate: input_rate,
                channels: 1,
            },
            samples: input,
        })
        .unwrap()
        .samples;
        let rms = (output
            .iter()
            .map(|sample| sample[0] * sample[0])
            .sum::<f32>()
            / output.len() as f32)
            .sqrt();
        assert!(rms < 0.01, "aliased output rms={rms}");
    }
}
