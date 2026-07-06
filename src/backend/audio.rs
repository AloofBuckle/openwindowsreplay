//! 音频后端基础结构。
//!
//! 文档目标是 WASAPI loopback + 麦克风 -> 48k stereo float PCM -> AAC LC -> MP4。
//! 本模块先实现与平台无关的时间戳保持、声道混合、线性重采样和 AAC 1024-sample
//! 输入分块；实际 WASAPI 采集与 Media Foundation AAC 编码后续接到这些结构上。
#![allow(dead_code)]

use crate::error::BackendError;

pub const TARGET_SAMPLE_RATE: u32 = 48_000;
pub const TARGET_CHANNELS: u16 = 2;
pub const AAC_LC_FRAME_SAMPLES: usize = 1024;
const HNS_PER_SECOND: i128 = 10_000_000;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
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
    frame.validate()?;
    let mono_or_stereo = remix_to_stereo(frame);
    let samples = if frame.format.sample_rate == TARGET_SAMPLE_RATE {
        mono_or_stereo
    } else {
        resample_linear(
            &mono_or_stereo,
            frame.format.sample_rate,
            TARGET_SAMPLE_RATE,
        )
    };
    Ok(StereoPcmFrame {
        start_time_100ns: frame.start_time_100ns,
        samples,
    })
}

pub fn mix_to_stereo_48k(frames: &[PcmFrame]) -> Result<Option<StereoPcmFrame>, BackendError> {
    let mut normalized = Vec::with_capacity(frames.len());
    for frame in frames {
        let converted = normalize_to_stereo_48k(frame)?;
        if !converted.samples.is_empty() {
            normalized.push(converted);
        }
    }
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

fn resample_linear(input: &[[f32; 2]], from_rate: u32, to_rate: u32) -> Vec<[f32; 2]> {
    if input.is_empty() || from_rate == 0 || to_rate == 0 {
        return Vec::new();
    }
    let out_len = ((input.len() as u128 * u128::from(to_rate)).div_ceil(u128::from(from_rate)))
        .max(1) as usize;
    let ratio = from_rate as f64 / to_rate as f64;
    let mut out = Vec::with_capacity(out_len);
    for out_index in 0..out_len {
        let src_pos = out_index as f64 * ratio;
        let i0 = src_pos.floor() as usize;
        let i1 = (i0 + 1).min(input.len() - 1);
        let t = (src_pos - i0 as f64) as f32;
        let a = input[i0.min(input.len() - 1)];
        let b = input[i1];
        out.push([a[0] + (b[0] - a[0]) * t, a[1] + (b[1] - a[1]) * t]);
    }
    out
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
}
