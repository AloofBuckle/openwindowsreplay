//! 极小 HEVC/H.265 MP4 封装器。
//!
//! 这里故意只消费“已编码码流字节”，不接触 raw frame。编码后的 bitstream 已不属于
//! GPU-only raw frame 契约范围；封装阶段可以在 CPU 上整理 box 与 sample 表。

use crate::error::BackendError;
use std::collections::BTreeSet;
use std::fs;
use std::fs::File;
use std::io::Write;
use std::path::Path;

const VIDEO_TIMESCALE: u32 = 90_000;
const MOVIE_TIMESCALE: u32 = 1_000;

#[derive(Debug, Clone)]
pub struct HevcAccessUnit {
    pub timestamp_90k: u64,
    pub data: Vec<u8>,
    pub is_sync: bool,
    /// 只用于从预热帧中提取 VPS/SPS/PPS 等参数集，不写入 MP4 sample 表。
    pub discard_from_track: bool,
}

#[derive(Debug, Clone)]
pub struct HevcMp4Track {
    pub width: u16,
    pub height: u16,
    pub duration_90k: u64,
    pub color: NclxColorMetadata,
    pub codec: HevcCodecMetadata,
    pub samples: Vec<HevcAccessUnit>,
}

#[derive(Debug, Clone)]
pub struct AacAccessUnit {
    pub timestamp_ticks: u64,
    pub duration_ticks: u32,
    /// MP4 `mp4a` sample 应写入裸 AAC access unit，不包含 ADTS 头。
    pub data: Vec<u8>,
}

#[derive(Debug, Clone)]
pub struct AacLcMp4Track {
    pub sample_rate: u32,
    pub channel_count: u16,
    pub duration_ticks: u64,
    pub samples: Vec<AacAccessUnit>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct HevcCodecMetadata {
    pub profile_idc: u8,
    pub chroma_format_idc: u8,
    pub bit_depth_luma_minus8: u8,
    pub bit_depth_chroma_minus8: u8,
}

impl HevcCodecMetadata {
    pub const fn main_420_8() -> Self {
        Self {
            profile_idc: 1,
            chroma_format_idc: 1,
            bit_depth_luma_minus8: 0,
            bit_depth_chroma_minus8: 0,
        }
    }

    pub const fn main10_420_10() -> Self {
        Self {
            profile_idc: 2,
            chroma_format_idc: 1,
            bit_depth_luma_minus8: 2,
            bit_depth_chroma_minus8: 2,
        }
    }

    #[allow(dead_code)]
    pub const fn rext(chroma_format_idc: u8, bit_depth: u8) -> Self {
        let minus8 = bit_depth.saturating_sub(8);
        Self {
            profile_idc: 4,
            chroma_format_idc,
            bit_depth_luma_minus8: minus8,
            bit_depth_chroma_minus8: minus8,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct NclxColorMetadata {
    pub colour_primaries: u16,
    pub transfer_characteristics: u16,
    pub matrix_coefficients: u16,
    pub full_range: bool,
}

impl NclxColorMetadata {
    pub const fn bt2020_pq(full_range: bool) -> Self {
        Self {
            colour_primaries: 9,
            transfer_characteristics: 16,
            matrix_coefficients: 9,
            full_range,
        }
    }

    pub const fn bt2020_pq_full() -> Self {
        Self::bt2020_pq(true)
    }

    pub const fn bt2020_pq_limited() -> Self {
        Self::bt2020_pq(false)
    }

    pub const fn bt2020_sdr_10(full_range: bool) -> Self {
        Self {
            colour_primaries: 9,
            // ITU-T H.273/ISO 23091-2: BT.2020 10-bit transfer. Values
            // 1/6/14/15 are functionally equivalent for SDR, but 14 keeps the
            // BT.2020 SDR route distinguishable from BT.709 in metadata.
            transfer_characteristics: 14,
            matrix_coefficients: 9,
            full_range,
        }
    }

    pub const fn bt2020_sdr_8(full_range: bool) -> Self {
        Self {
            colour_primaries: 9,
            // 8-bit BT.2020 SDR has no dedicated H.273 transfer value; use
            // BT.709-compatible SDR transfer while preserving BT.2020 primaries
            // and non-constant-luminance matrix.
            transfer_characteristics: 1,
            matrix_coefficients: 9,
            full_range,
        }
    }

    pub const fn bt709(full_range: bool) -> Self {
        Self {
            colour_primaries: 1,
            transfer_characteristics: 1,
            matrix_coefficients: 1,
            full_range,
        }
    }

    #[allow(dead_code)]
    pub const fn bt709_full() -> Self {
        Self::bt709(true)
    }

    #[allow(dead_code)]
    pub const fn bt709_limited() -> Self {
        Self::bt709(false)
    }
}

#[derive(Debug, Clone)]
struct PreparedSample {
    duration_90k: u32,
    data: Vec<u8>,
    is_sync: bool,
}

#[derive(Debug, Clone)]
struct PreparedAudioSample {
    duration_ticks: u32,
    data: Vec<u8>,
}

#[derive(Debug, Clone, Copy)]
struct PreparedAacTrackRef<'a> {
    track: &'a AacLcMp4Track,
    samples: &'a [PreparedAudioSample],
    offsets: &'a [u64],
}

#[derive(Debug, Clone)]
struct ParameterSets {
    vps: Vec<Vec<u8>>,
    sps: Vec<Vec<u8>>,
    pps: Vec<Vec<u8>>,
}

#[allow(dead_code)]
pub fn write_hevc_mp4(path: &Path, track: &HevcMp4Track) -> Result<(), BackendError> {
    write_hevc_aac_mp4(path, track, None)
}

pub fn write_hevc_aac_mp4(
    path: &Path,
    video_track: &HevcMp4Track,
    audio_track: Option<&AacLcMp4Track>,
) -> Result<(), BackendError> {
    if let Some(audio) = audio_track {
        validate_aac_track(audio)?;
    }

    let (converted, parameter_sets, final_video_duration_90k) = prepare_video_track(video_track)?;
    let prepared_audio = audio_track.map(prepare_audio_track).transpose()?;

    let ftyp = make_ftyp();
    let mdat_payload_len: u64 = converted.iter().map(|s| s.data.len() as u64).sum::<u64>()
        + prepared_audio
            .as_ref()
            .map(|audio| audio.iter().map(|s| s.data.len() as u64).sum::<u64>())
            .unwrap_or(0);
    let mdat_header = make_mdat_header(mdat_payload_len);
    let first_sample_offset = ftyp.len() as u64 + mdat_header.len() as u64;
    let mut cursor = first_sample_offset;
    let mut video_offsets = Vec::with_capacity(converted.len());
    for sample in &converted {
        video_offsets.push(cursor);
        cursor += sample.data.len() as u64;
    }
    let mut audio_offsets = Vec::new();
    if let Some(audio_samples) = &prepared_audio {
        audio_offsets.reserve(audio_samples.len());
        for sample in audio_samples {
            audio_offsets.push(cursor);
            cursor += sample.data.len() as u64;
        }
    }

    let audio_ref = match (audio_track, prepared_audio.as_deref()) {
        (Some(track), Some(samples)) => Some(PreparedAacTrackRef {
            track,
            samples,
            offsets: &audio_offsets,
        }),
        _ => None,
    };
    let moov = make_moov(
        video_track,
        final_video_duration_90k,
        &converted,
        &video_offsets,
        &parameter_sets,
        audio_ref,
    )?;

    if let Some(parent) = path.parent()
        && !parent.as_os_str().is_empty()
    {
        fs::create_dir_all(parent).map_err(|err| BackendError::Io(err.to_string()))?;
    }
    let mut file = File::create(path).map_err(|err| BackendError::Io(err.to_string()))?;
    file.write_all(&ftyp)
        .map_err(|err| BackendError::Io(err.to_string()))?;
    file.write_all(&mdat_header)
        .map_err(|err| BackendError::Io(err.to_string()))?;
    write_mdat_payload(&mut file, &converted, prepared_audio.as_deref())?;
    file.write_all(&moov)
        .map_err(|err| BackendError::Io(err.to_string()))?;
    file.flush()
        .map_err(|err| BackendError::Io(err.to_string()))
}

fn prepare_video_track(
    track: &HevcMp4Track,
) -> Result<(Vec<PreparedSample>, ParameterSets, u64), BackendError> {
    let playable_samples = track
        .samples
        .iter()
        .filter(|sample| !sample.discard_from_track)
        .collect::<Vec<_>>();
    if playable_samples.is_empty() {
        return Err(BackendError::unsupported(
            "MP4 封装",
            "HEVC video track",
            "没有可封装的视频 sample",
        ));
    }

    let mut parameter_sets = ParameterSets {
        vps: Vec::new(),
        sps: Vec::new(),
        pps: Vec::new(),
    };
    let mut converted = Vec::with_capacity(playable_samples.len());
    let mut playable_index = 0usize;
    for sample in &track.samples {
        let (data, is_sync, found_sets) = hevc_annex_b_to_length_prefixed(&sample.data)?;
        merge_parameter_sets(&mut parameter_sets, found_sets);
        if sample.discard_from_track {
            continue;
        }
        let duration_90k = sample_duration(&playable_samples, track.duration_90k, playable_index);
        playable_index += 1;
        converted.push(PreparedSample {
            duration_90k,
            data,
            is_sync: sample.is_sync || is_sync || converted.is_empty(),
        });
    }
    let prepared_duration_90k = converted
        .iter()
        .map(|sample| u64::from(sample.duration_90k))
        .sum::<u64>();
    if let Some(last) = converted.last_mut()
        && prepared_duration_90k < track.duration_90k
    {
        let extra = track.duration_90k.saturating_sub(prepared_duration_90k);
        last.duration_90k = u64::from(last.duration_90k)
            .saturating_add(extra)
            .min(u32::MAX as u64) as u32;
    }

    if parameter_sets.vps.is_empty()
        || parameter_sets.sps.is_empty()
        || parameter_sets.pps.is_empty()
    {
        return Err(BackendError::unsupported(
            "MP4 封装",
            "hvcC",
            "HEVC 码流中没有找到完整 VPS/SPS/PPS 参数集",
        ));
    }

    let final_duration_90k = converted
        .iter()
        .map(|sample| u64::from(sample.duration_90k))
        .sum::<u64>()
        .max(1);
    Ok((converted, parameter_sets, final_duration_90k))
}

fn prepare_audio_track(track: &AacLcMp4Track) -> Result<Vec<PreparedAudioSample>, BackendError> {
    validate_aac_track(track)?;
    if track.samples.is_empty() {
        return Err(BackendError::unsupported(
            "MP4 封装",
            "AAC audio track",
            "请求写入音轨但没有可封装的 AAC sample",
        ));
    }

    let mut prepared = Vec::with_capacity(track.samples.len());
    for (index, sample) in track.samples.iter().enumerate() {
        let duration_ticks = if sample.duration_ticks == 0 {
            track
                .samples
                .get(index + 1)
                .map(|next| next.timestamp_ticks.saturating_sub(sample.timestamp_ticks))
                .unwrap_or_else(|| track.duration_ticks.saturating_sub(sample.timestamp_ticks))
                .max(1)
                .min(u32::MAX as u64) as u32
        } else {
            sample.duration_ticks
        };
        prepared.push(PreparedAudioSample {
            duration_ticks,
            data: sample.data.clone(),
        });
    }
    Ok(prepared)
}

fn sample_duration(samples: &[&HevcAccessUnit], track_duration_90k: u64, index: usize) -> u32 {
    let current = samples[index].timestamp_90k;
    let next = samples
        .get(index + 1)
        .map(|s| s.timestamp_90k)
        .unwrap_or(track_duration_90k);
    next.saturating_sub(current).max(1).min(u32::MAX as u64) as u32
}

fn merge_parameter_sets(dst: &mut ParameterSets, src: ParameterSets) {
    append_unique(&mut dst.vps, src.vps);
    append_unique(&mut dst.sps, src.sps);
    append_unique(&mut dst.pps, src.pps);
}

fn append_unique(dst: &mut Vec<Vec<u8>>, src: Vec<Vec<u8>>) {
    let mut seen: BTreeSet<Vec<u8>> = dst.iter().cloned().collect();
    for item in src {
        if seen.insert(item.clone()) {
            dst.push(item);
        }
    }
}

fn hevc_annex_b_to_length_prefixed(
    data: &[u8],
) -> Result<(Vec<u8>, bool, ParameterSets), BackendError> {
    let mut out = Vec::with_capacity(data.len());
    let mut sets = ParameterSets {
        vps: Vec::new(),
        sps: Vec::new(),
        pps: Vec::new(),
    };
    let mut is_sync = false;
    let mut pos = 0usize;
    let mut found = false;

    while let Some((start, code_len)) = find_start_code(data, pos) {
        let nal_start = start + code_len;
        let next = find_start_code(data, nal_start)
            .map(|(next_start, _)| next_start)
            .unwrap_or(data.len());
        pos = next;
        if nal_start >= next {
            continue;
        }
        let mut nal = &data[nal_start..next];
        while nal.last().copied() == Some(0) {
            nal = &nal[..nal.len() - 1];
        }
        if nal.len() < 2 {
            continue;
        }
        found = true;
        let nal_type = (nal[0] >> 1) & 0x3f;
        match nal_type {
            19..=21 => is_sync = true,
            32 => sets.vps.push(nal.to_vec()),
            33 => sets.sps.push(nal.to_vec()),
            34 => sets.pps.push(nal.to_vec()),
            _ => {}
        }
        out.extend_from_slice(&(nal.len() as u32).to_be_bytes());
        out.extend_from_slice(nal);
    }

    if !found {
        return Err(BackendError::unsupported(
            "MP4 封装",
            "HEVC Annex-B",
            "oneVPL 输出不是预期的 Annex-B start code 格式",
        ));
    }

    Ok((out, is_sync, sets))
}

fn find_start_code(data: &[u8], from: usize) -> Option<(usize, usize)> {
    let mut i = from;
    while i + 3 <= data.len() {
        if data[i] == 0 && data[i + 1] == 0 {
            if data[i + 2] == 1 {
                return Some((i, 3));
            }
            if i + 4 <= data.len() && data[i + 2] == 0 && data[i + 3] == 1 {
                return Some((i, 4));
            }
        }
        i += 1;
    }
    None
}

fn make_ftyp() -> Vec<u8> {
    let mut p = Vec::new();
    p.extend_from_slice(b"mp42");
    p.extend_from_slice(&0u32.to_be_bytes());
    p.extend_from_slice(b"isom");
    p.extend_from_slice(b"mp42");
    mp4_box(*b"ftyp", p)
}

fn make_mdat_header(payload_len: u64) -> Vec<u8> {
    let total = payload_len.saturating_add(8);
    if total <= u32::MAX as u64 {
        let mut out = Vec::with_capacity(8);
        be32(&mut out, total as u32);
        out.extend_from_slice(b"mdat");
        out
    } else {
        let mut out = Vec::with_capacity(16);
        be32(&mut out, 1);
        out.extend_from_slice(b"mdat");
        be64(&mut out, payload_len.saturating_add(16));
        out
    }
}

fn write_mdat_payload(
    out: &mut File,
    video_samples: &[PreparedSample],
    audio_samples: Option<&[PreparedAudioSample]>,
) -> Result<(), BackendError> {
    for sample in video_samples {
        out.write_all(&sample.data)
            .map_err(|err| BackendError::Io(err.to_string()))?;
    }
    if let Some(audio_samples) = audio_samples {
        for sample in audio_samples {
            out.write_all(&sample.data)
                .map_err(|err| BackendError::Io(err.to_string()))?;
        }
    }
    Ok(())
}

fn make_moov(
    video_track: &HevcMp4Track,
    video_duration_90k: u64,
    video_samples: &[PreparedSample],
    video_offsets: &[u64],
    sets: &ParameterSets,
    audio: Option<PreparedAacTrackRef<'_>>,
) -> Result<Vec<u8>, BackendError> {
    let video_duration_ms = scale_duration(video_duration_90k, VIDEO_TIMESCALE, MOVIE_TIMESCALE);
    let audio_duration_ms = audio
        .map(|audio| {
            scale_duration(
                audio.track.duration_ticks,
                audio.track.sample_rate,
                MOVIE_TIMESCALE,
            )
        })
        .unwrap_or(0);
    let movie_duration_ms = video_duration_ms.max(audio_duration_ms).max(1);
    let mut p = Vec::new();
    p.extend(make_mvhd(movie_duration_ms));
    p.extend(make_video_trak(
        video_track,
        video_duration_90k,
        video_samples,
        video_offsets,
        sets,
        video_duration_ms,
    )?);
    if let Some(audio) = audio {
        p.extend(make_audio_trak(
            audio.track,
            audio.samples,
            audio.offsets,
            audio_duration_ms,
        )?);
    }
    Ok(mp4_box(*b"moov", p))
}

fn make_mvhd(duration_ms: u32) -> Vec<u8> {
    let mut p = Vec::new();
    be32(&mut p, 0); // creation_time
    be32(&mut p, 0); // modification_time
    be32(&mut p, MOVIE_TIMESCALE);
    be32(&mut p, duration_ms);
    be32(&mut p, 0x0001_0000); // rate
    be16(&mut p, 0x0100); // volume
    be16(&mut p, 0);
    be32(&mut p, 0);
    be32(&mut p, 0);
    unity_matrix(&mut p);
    for _ in 0..6 {
        be32(&mut p, 0);
    }
    be32(&mut p, 3); // next_track_id
    full_box(*b"mvhd", 0, 0, p)
}

fn make_video_trak(
    track: &HevcMp4Track,
    duration_90k: u64,
    samples: &[PreparedSample],
    offsets: &[u64],
    sets: &ParameterSets,
    track_duration_ms: u32,
) -> Result<Vec<u8>, BackendError> {
    let mut p = Vec::new();
    p.extend(make_tkhd(
        1,
        track_duration_ms,
        0,
        (track.width as u32) << 16,
        (track.height as u32) << 16,
    ));
    p.extend(make_mdia(track, duration_90k, samples, offsets, sets)?);
    Ok(mp4_box(*b"trak", p))
}

fn make_audio_trak(
    track: &AacLcMp4Track,
    samples: &[PreparedAudioSample],
    offsets: &[u64],
    track_duration_ms: u32,
) -> Result<Vec<u8>, BackendError> {
    let mut p = Vec::new();
    p.extend(make_tkhd(2, track_duration_ms, 0x0100, 0, 0));
    p.extend(make_audio_mdia(track, samples, offsets)?);
    Ok(mp4_box(*b"trak", p))
}

fn make_tkhd(
    track_id: u32,
    duration_ms: u32,
    volume: u16,
    width_fixed: u32,
    height_fixed: u32,
) -> Vec<u8> {
    let mut p = Vec::new();
    be32(&mut p, 0);
    be32(&mut p, 0);
    be32(&mut p, track_id);
    be32(&mut p, 0);
    be32(&mut p, duration_ms);
    be32(&mut p, 0);
    be32(&mut p, 0);
    be16(&mut p, 0); // layer
    be16(&mut p, 0); // alternate_group
    be16(&mut p, volume);
    be16(&mut p, 0);
    unity_matrix(&mut p);
    be32(&mut p, width_fixed);
    be32(&mut p, height_fixed);
    full_box(*b"tkhd", 0, 0x000007, p)
}

fn make_mdia(
    track: &HevcMp4Track,
    duration_90k: u64,
    samples: &[PreparedSample],
    offsets: &[u64],
    sets: &ParameterSets,
) -> Result<Vec<u8>, BackendError> {
    let mut p = Vec::new();
    p.extend(make_mdhd(VIDEO_TIMESCALE, duration_90k));
    p.extend(make_hdlr(*b"vide", "VideoHandler"));
    p.extend(make_minf(track, samples, offsets, sets)?);
    Ok(mp4_box(*b"mdia", p))
}

fn make_audio_mdia(
    track: &AacLcMp4Track,
    samples: &[PreparedAudioSample],
    offsets: &[u64],
) -> Result<Vec<u8>, BackendError> {
    let mut p = Vec::new();
    p.extend(make_mdhd(track.sample_rate, track.duration_ticks));
    p.extend(make_hdlr(*b"soun", "SoundHandler"));
    p.extend(make_audio_minf(track, samples, offsets)?);
    Ok(mp4_box(*b"mdia", p))
}

fn make_mdhd(timescale: u32, duration: u64) -> Vec<u8> {
    let mut p = Vec::new();
    be32(&mut p, 0);
    be32(&mut p, 0);
    be32(&mut p, timescale);
    be32(&mut p, duration.min(u32::MAX as u64) as u32);
    be16(&mut p, 0x55c4); // "und"
    be16(&mut p, 0);
    full_box(*b"mdhd", 0, 0, p)
}

fn make_hdlr(handler_type: [u8; 4], name: &str) -> Vec<u8> {
    let mut p = Vec::new();
    be32(&mut p, 0);
    p.extend_from_slice(&handler_type);
    be32(&mut p, 0);
    be32(&mut p, 0);
    be32(&mut p, 0);
    p.extend_from_slice(name.as_bytes());
    p.push(0);
    full_box(*b"hdlr", 0, 0, p)
}

fn make_minf(
    track: &HevcMp4Track,
    samples: &[PreparedSample],
    offsets: &[u64],
    sets: &ParameterSets,
) -> Result<Vec<u8>, BackendError> {
    let mut p = Vec::new();
    p.extend(make_vmhd());
    p.extend(make_dinf());
    p.extend(make_stbl(track, samples, offsets, sets)?);
    Ok(mp4_box(*b"minf", p))
}

fn make_audio_minf(
    track: &AacLcMp4Track,
    samples: &[PreparedAudioSample],
    offsets: &[u64],
) -> Result<Vec<u8>, BackendError> {
    let mut p = Vec::new();
    p.extend(make_smhd());
    p.extend(make_dinf());
    p.extend(make_audio_stbl(track, samples, offsets)?);
    Ok(mp4_box(*b"minf", p))
}

fn make_vmhd() -> Vec<u8> {
    let mut p = Vec::new();
    be16(&mut p, 0);
    be16(&mut p, 0);
    be16(&mut p, 0);
    be16(&mut p, 0);
    full_box(*b"vmhd", 0, 1, p)
}

fn make_smhd() -> Vec<u8> {
    let mut p = Vec::new();
    be16(&mut p, 0); // balance
    be16(&mut p, 0);
    full_box(*b"smhd", 0, 0, p)
}

fn make_dinf() -> Vec<u8> {
    let url = full_box(*b"url ", 0, 1, Vec::new());
    let mut dref_payload = Vec::new();
    be32(&mut dref_payload, 1);
    dref_payload.extend(url);
    mp4_box(*b"dinf", full_box(*b"dref", 0, 0, dref_payload))
}

fn make_stbl(
    track: &HevcMp4Track,
    samples: &[PreparedSample],
    offsets: &[u64],
    sets: &ParameterSets,
) -> Result<Vec<u8>, BackendError> {
    let mut p = Vec::new();
    p.extend(make_stsd(track, sets)?);
    p.extend(make_stts(samples));
    p.extend(make_stss(samples));
    p.extend(make_stsc());
    p.extend(make_stsz(samples));
    p.extend(make_chunk_offsets(offsets)?);
    Ok(mp4_box(*b"stbl", p))
}

fn make_audio_stbl(
    track: &AacLcMp4Track,
    samples: &[PreparedAudioSample],
    offsets: &[u64],
) -> Result<Vec<u8>, BackendError> {
    let mut p = Vec::new();
    p.extend(make_audio_stsd(track)?);
    p.extend(make_audio_stts(samples));
    p.extend(make_stsc());
    p.extend(make_audio_stsz(samples));
    p.extend(make_chunk_offsets(offsets)?);
    Ok(mp4_box(*b"stbl", p))
}

fn make_stsd(track: &HevcMp4Track, sets: &ParameterSets) -> Result<Vec<u8>, BackendError> {
    let hvc1 = make_hvc1_sample_entry(track, sets)?;
    let mut p = Vec::new();
    be32(&mut p, 1);
    p.extend(hvc1);
    Ok(full_box(*b"stsd", 0, 0, p))
}

fn make_audio_stsd(track: &AacLcMp4Track) -> Result<Vec<u8>, BackendError> {
    let mp4a = make_mp4a_sample_entry(track)?;
    let mut p = Vec::new();
    be32(&mut p, 1);
    p.extend(mp4a);
    Ok(full_box(*b"stsd", 0, 0, p))
}

fn make_hvc1_sample_entry(
    track: &HevcMp4Track,
    sets: &ParameterSets,
) -> Result<Vec<u8>, BackendError> {
    let mut p = Vec::new();
    p.extend_from_slice(&[0; 6]);
    be16(&mut p, 1); // data_reference_index
    be16(&mut p, 0);
    be16(&mut p, 0);
    be32(&mut p, 0);
    be32(&mut p, 0);
    be32(&mut p, 0);
    be16(&mut p, track.width);
    be16(&mut p, track.height);
    be32(&mut p, 0x0048_0000);
    be32(&mut p, 0x0048_0000);
    be32(&mut p, 0);
    be16(&mut p, 1);
    let mut compressor = [0u8; 32];
    let name = b"oneVPL HEVC";
    compressor[0] = name.len() as u8;
    compressor[1..=name.len()].copy_from_slice(name);
    p.extend_from_slice(&compressor);
    be16(&mut p, 0x0018);
    be16(&mut p, 0xffff);
    p.extend(make_hvcc_with_codec(sets, track.codec)?);
    p.extend(make_colr_nclx(track.color));
    Ok(mp4_box(*b"hvc1", p))
}

fn make_mp4a_sample_entry(track: &AacLcMp4Track) -> Result<Vec<u8>, BackendError> {
    let mut p = Vec::new();
    p.extend_from_slice(&[0; 6]);
    be16(&mut p, 1); // data_reference_index
    be16(&mut p, 0); // version
    be16(&mut p, 0); // revision level
    be32(&mut p, 0); // vendor
    be16(&mut p, track.channel_count);
    be16(&mut p, 16); // sample size
    be16(&mut p, 0); // compression id
    be16(&mut p, 0); // packet size
    be32(&mut p, track.sample_rate << 16);
    p.extend(make_esds_aac_lc(track)?);
    Ok(mp4_box(*b"mp4a", p))
}

fn make_esds_aac_lc(track: &AacLcMp4Track) -> Result<Vec<u8>, BackendError> {
    let asc = aac_lc_audio_specific_config(track.sample_rate, track.channel_count)?;
    let decoder_specific = descriptor(0x05, asc);

    let mut decoder_config = Vec::new();
    decoder_config.push(0x40); // MPEG-4 Audio
    decoder_config.push(0x15); // AudioStream + reserved bit
    decoder_config.extend_from_slice(&[0, 0, 0]); // bufferSizeDB
    be32(&mut decoder_config, 0); // maxBitrate unknown
    be32(&mut decoder_config, 0); // avgBitrate unknown
    decoder_config.extend(decoder_specific);

    let sl_config = descriptor(0x06, vec![0x02]);

    let mut es = Vec::new();
    be16(&mut es, 1); // ES_ID
    es.push(0); // flags
    es.extend(descriptor(0x04, decoder_config));
    es.extend(sl_config);

    Ok(full_box(*b"esds", 0, 0, descriptor(0x03, es)))
}

fn descriptor(tag: u8, payload: Vec<u8>) -> Vec<u8> {
    let mut out = Vec::new();
    out.push(tag);
    write_descriptor_len(&mut out, payload.len() as u32);
    out.extend(payload);
    out
}

fn write_descriptor_len(out: &mut Vec<u8>, len: u32) {
    out.push(((len >> 21) as u8 & 0x7f) | 0x80);
    out.push(((len >> 14) as u8 & 0x7f) | 0x80);
    out.push(((len >> 7) as u8 & 0x7f) | 0x80);
    out.push((len as u8) & 0x7f);
}

fn aac_lc_audio_specific_config(
    sample_rate: u32,
    channel_count: u16,
) -> Result<Vec<u8>, BackendError> {
    let frequency_index = aac_sample_rate_index(sample_rate).ok_or_else(|| {
        BackendError::unsupported(
            "MP4 封装",
            format!("AAC sample_rate={sample_rate}"),
            "AAC LC AudioSpecificConfig 目前只接受标准 MPEG-4 采样率索引",
        )
    })?;
    if !(1..=7).contains(&channel_count) {
        return Err(BackendError::unsupported(
            "MP4 封装",
            format!("AAC channels={channel_count}"),
            "AAC LC AudioSpecificConfig 目前只接受 1..=7 声道配置",
        ));
    }
    let audio_object_type = 2u16; // AAC LC
    let bits = (audio_object_type << 11) | ((frequency_index as u16) << 7) | (channel_count << 3);
    Ok(bits.to_be_bytes().to_vec())
}

fn aac_sample_rate_index(sample_rate: u32) -> Option<u8> {
    match sample_rate {
        96_000 => Some(0),
        88_200 => Some(1),
        64_000 => Some(2),
        48_000 => Some(3),
        44_100 => Some(4),
        32_000 => Some(5),
        24_000 => Some(6),
        22_050 => Some(7),
        16_000 => Some(8),
        12_000 => Some(9),
        11_025 => Some(10),
        8_000 => Some(11),
        7_350 => Some(12),
        _ => None,
    }
}

fn make_hvcc_with_codec(
    sets: &ParameterSets,
    codec: HevcCodecMetadata,
) -> Result<Vec<u8>, BackendError> {
    let mut p = vec![
        1, // configurationVersion
        codec.profile_idc & 0x1f,
    ];
    p.extend_from_slice(&0x6000_0000u32.to_be_bytes());
    p.extend_from_slice(&[0; 6]);
    p.push(153); // level 5.1
    be16(&mut p, 0xf000);
    p.push(0xfc);
    p.push(0xfc | (codec.chroma_format_idc & 0x03));
    p.push(0xf8 | (codec.bit_depth_luma_minus8 & 0x07));
    p.push(0xf8 | (codec.bit_depth_chroma_minus8 & 0x07));
    be16(&mut p, 0);
    p.push(0x0f); // one temporal layer, nested, 4-byte NAL lengths
    p.push(3);
    append_hvcc_array(&mut p, 32, &sets.vps)?;
    append_hvcc_array(&mut p, 33, &sets.sps)?;
    append_hvcc_array(&mut p, 34, &sets.pps)?;
    Ok(mp4_box(*b"hvcC", p))
}

fn append_hvcc_array(p: &mut Vec<u8>, nal_type: u8, nals: &[Vec<u8>]) -> Result<(), BackendError> {
    if nals.len() > u16::MAX as usize {
        return Err(BackendError::unsupported(
            "MP4 封装",
            "hvcC arrays",
            "参数集数量超过 u16 上限",
        ));
    }
    p.push(0x80 | (nal_type & 0x3f));
    be16(p, nals.len() as u16);
    for nal in nals {
        if nal.len() > u16::MAX as usize {
            return Err(BackendError::unsupported(
                "MP4 封装",
                "hvcC NAL",
                "单个参数集超过 u16 上限",
            ));
        }
        be16(p, nal.len() as u16);
        p.extend_from_slice(nal);
    }
    Ok(())
}

fn make_colr_nclx(color: NclxColorMetadata) -> Vec<u8> {
    let mut p = Vec::new();
    p.extend_from_slice(b"nclx");
    be16(&mut p, color.colour_primaries);
    be16(&mut p, color.transfer_characteristics);
    be16(&mut p, color.matrix_coefficients);
    p.push(if color.full_range { 0x80 } else { 0x00 });
    mp4_box(*b"colr", p)
}

fn make_stts(samples: &[PreparedSample]) -> Vec<u8> {
    let mut entries: Vec<(u32, u32)> = Vec::new();
    for sample in samples {
        if let Some((count, duration)) = entries.last_mut()
            && *duration == sample.duration_90k
        {
            *count += 1;
            continue;
        }
        entries.push((1, sample.duration_90k));
    }
    let mut p = Vec::new();
    be32(&mut p, entries.len() as u32);
    for (count, duration) in entries {
        be32(&mut p, count);
        be32(&mut p, duration);
    }
    full_box(*b"stts", 0, 0, p)
}

fn make_audio_stts(samples: &[PreparedAudioSample]) -> Vec<u8> {
    let mut entries: Vec<(u32, u32)> = Vec::new();
    for sample in samples {
        if let Some((count, duration)) = entries.last_mut()
            && *duration == sample.duration_ticks
        {
            *count += 1;
            continue;
        }
        entries.push((1, sample.duration_ticks));
    }
    let mut p = Vec::new();
    be32(&mut p, entries.len() as u32);
    for (count, duration) in entries {
        be32(&mut p, count);
        be32(&mut p, duration);
    }
    full_box(*b"stts", 0, 0, p)
}

fn make_stss(samples: &[PreparedSample]) -> Vec<u8> {
    let mut sync = Vec::new();
    for (index, sample) in samples.iter().enumerate() {
        if sample.is_sync {
            sync.push((index + 1) as u32);
        }
    }
    if sync.is_empty() {
        sync.push(1);
    }
    let mut p = Vec::new();
    be32(&mut p, sync.len() as u32);
    for sample_number in sync {
        be32(&mut p, sample_number);
    }
    full_box(*b"stss", 0, 0, p)
}

fn make_stsc() -> Vec<u8> {
    let mut p = Vec::new();
    be32(&mut p, 1);
    be32(&mut p, 1);
    be32(&mut p, 1);
    be32(&mut p, 1);
    full_box(*b"stsc", 0, 0, p)
}

fn make_stsz(samples: &[PreparedSample]) -> Vec<u8> {
    let mut p = Vec::new();
    be32(&mut p, 0);
    be32(&mut p, samples.len() as u32);
    for sample in samples {
        be32(&mut p, sample.data.len() as u32);
    }
    full_box(*b"stsz", 0, 0, p)
}

fn make_audio_stsz(samples: &[PreparedAudioSample]) -> Vec<u8> {
    let mut p = Vec::new();
    be32(&mut p, 0);
    be32(&mut p, samples.len() as u32);
    for sample in samples {
        be32(&mut p, sample.data.len() as u32);
    }
    full_box(*b"stsz", 0, 0, p)
}

fn make_chunk_offsets(offsets: &[u64]) -> Result<Vec<u8>, BackendError> {
    if offsets.iter().any(|&offset| offset > u32::MAX as u64) {
        return make_co64(offsets);
    }
    make_stco(offsets)
}

fn make_stco(offsets: &[u64]) -> Result<Vec<u8>, BackendError> {
    let mut p = Vec::new();
    be32(&mut p, offsets.len() as u32);
    for &offset in offsets {
        be32(&mut p, offset as u32);
    }
    Ok(full_box(*b"stco", 0, 0, p))
}

fn make_co64(offsets: &[u64]) -> Result<Vec<u8>, BackendError> {
    if offsets.len() > u32::MAX as usize {
        return Err(BackendError::unsupported(
            "MP4 封装",
            "co64",
            "sample 数量超过 u32 entry_count 上限",
        ));
    }
    let mut p = Vec::new();
    be32(&mut p, offsets.len() as u32);
    for &offset in offsets {
        be64(&mut p, offset);
    }
    Ok(full_box(*b"co64", 0, 0, p))
}

fn validate_aac_track(track: &AacLcMp4Track) -> Result<(), BackendError> {
    if track.sample_rate == 0 {
        return Err(BackendError::unsupported(
            "MP4 封装",
            "AAC sample_rate=0",
            "音轨 timescale/采样率必须大于 0",
        ));
    }
    let _ = aac_lc_audio_specific_config(track.sample_rate, track.channel_count)?;
    Ok(())
}

fn scale_duration(duration: u64, source_timescale: u32, target_timescale: u32) -> u32 {
    if source_timescale == 0 {
        return 0;
    }
    ((duration * u64::from(target_timescale)).div_ceil(u64::from(source_timescale)))
        .min(u32::MAX as u64) as u32
}

fn mp4_box(name: [u8; 4], payload: Vec<u8>) -> Vec<u8> {
    let size = (payload.len() + 8) as u32;
    let mut out = Vec::with_capacity(size as usize);
    out.extend_from_slice(&size.to_be_bytes());
    out.extend_from_slice(&name);
    out.extend(payload);
    out
}

fn full_box(name: [u8; 4], version: u8, flags: u32, payload: Vec<u8>) -> Vec<u8> {
    let mut p = Vec::with_capacity(payload.len() + 4);
    p.push(version);
    p.push(((flags >> 16) & 0xff) as u8);
    p.push(((flags >> 8) & 0xff) as u8);
    p.push((flags & 0xff) as u8);
    p.extend(payload);
    mp4_box(name, p)
}

fn unity_matrix(p: &mut Vec<u8>) {
    be32(p, 0x0001_0000);
    be32(p, 0);
    be32(p, 0);
    be32(p, 0);
    be32(p, 0x0001_0000);
    be32(p, 0);
    be32(p, 0);
    be32(p, 0);
    be32(p, 0x4000_0000);
}

fn be16(p: &mut Vec<u8>, value: u16) {
    p.extend_from_slice(&value.to_be_bytes());
}

fn be32(p: &mut Vec<u8>, value: u32) {
    p.extend_from_slice(&value.to_be_bytes());
}

fn be64(p: &mut Vec<u8>, value: u64) {
    p.extend_from_slice(&value.to_be_bytes());
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn aac_lc_asc_for_48k_stereo_matches_mpeg4_bits() {
        assert_eq!(
            aac_lc_audio_specific_config(48_000, 2).unwrap(),
            vec![0x11, 0x90]
        );
    }

    #[test]
    fn bt2020_sdr_10_uses_nclx_2020_10bit_metadata() {
        let full = NclxColorMetadata::bt2020_sdr_10(true);
        assert_eq!(full.colour_primaries, 9);
        assert_eq!(full.transfer_characteristics, 14);
        assert_eq!(full.matrix_coefficients, 9);
        assert!(full.full_range);

        let limited = NclxColorMetadata::bt2020_sdr_10(false);
        assert_eq!(limited.colour_primaries, 9);
        assert_eq!(limited.transfer_characteristics, 14);
        assert_eq!(limited.matrix_coefficients, 9);
        assert!(!limited.full_range);
    }

    #[test]
    fn bt2020_sdr_8_uses_2020_primaries_with_sdr_transfer() {
        let full = NclxColorMetadata::bt2020_sdr_8(true);
        assert_eq!(full.colour_primaries, 9);
        assert_eq!(full.transfer_characteristics, 1);
        assert_eq!(full.matrix_coefficients, 9);
        assert!(full.full_range);

        let limited = NclxColorMetadata::bt2020_sdr_8(false);
        assert_eq!(limited.colour_primaries, 9);
        assert_eq!(limited.transfer_characteristics, 1);
        assert_eq!(limited.matrix_coefficients, 9);
        assert!(!limited.full_range);
    }

    #[test]
    fn muxer_can_emit_video_and_aac_tracks() {
        let dir = std::env::temp_dir();
        let unique = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_nanos();
        let path = dir.join(format!(
            "rustreplay_mux_audio_test_{}_{}.mp4",
            std::process::id(),
            unique
        ));
        let video = HevcMp4Track {
            width: 16,
            height: 16,
            duration_90k: 90_000,
            color: NclxColorMetadata::bt709_full(),
            codec: HevcCodecMetadata::main_420_8(),
            samples: vec![HevcAccessUnit {
                timestamp_90k: 0,
                data: fake_hevc_annex_b_access_unit(),
                is_sync: true,
                discard_from_track: false,
            }],
        };
        let audio = AacLcMp4Track {
            sample_rate: 48_000,
            channel_count: 2,
            duration_ticks: 48_000,
            samples: vec![
                AacAccessUnit {
                    timestamp_ticks: 0,
                    duration_ticks: 1024,
                    data: vec![0x21, 0x10, 0x04, 0x60],
                },
                AacAccessUnit {
                    timestamp_ticks: 1024,
                    duration_ticks: 1024,
                    data: vec![0x21, 0x10, 0x04, 0x61],
                },
            ],
        };
        write_hevc_aac_mp4(&path, &video, Some(&audio)).unwrap();
        let bytes = std::fs::read(&path).unwrap();
        let _ = std::fs::remove_file(&path);
        for needle in [b"hvc1".as_slice(), b"mp4a", b"esds", b"vide", b"soun"] {
            assert!(
                bytes.windows(needle.len()).any(|window| window == needle),
                "missing box/handler marker {:?}",
                String::from_utf8_lossy(needle)
            );
        }
    }

    #[test]
    fn mdat_header_switches_to_large_size() {
        let small = make_mdat_header(4);
        assert_eq!(&small[0..4], &12u32.to_be_bytes());
        assert_eq!(&small[4..8], b"mdat");

        let payload = u32::MAX as u64;
        let large = make_mdat_header(payload);
        assert_eq!(&large[0..4], &1u32.to_be_bytes());
        assert_eq!(&large[4..8], b"mdat");
        assert_eq!(&large[8..16], &(payload + 16).to_be_bytes());
    }

    #[test]
    fn chunk_offsets_switch_to_co64_when_needed() {
        let stco = make_chunk_offsets(&[32, u32::MAX as u64]).unwrap();
        assert!(stco.windows(4).any(|window| window == b"stco"));
        assert!(!stco.windows(4).any(|window| window == b"co64"));

        let co64 = make_chunk_offsets(&[32, u32::MAX as u64 + 1]).unwrap();
        assert!(co64.windows(4).any(|window| window == b"co64"));
        assert_eq!(&co64[16..24], &32u64.to_be_bytes());
        assert_eq!(&co64[24..32], &(u32::MAX as u64 + 1).to_be_bytes());
    }

    fn fake_hevc_annex_b_access_unit() -> Vec<u8> {
        let mut out = Vec::new();
        append_fake_nal(&mut out, 32, &[1, 2, 3]); // VPS
        append_fake_nal(&mut out, 33, &[4, 5, 6]); // SPS
        append_fake_nal(&mut out, 34, &[7, 8, 9]); // PPS
        append_fake_nal(&mut out, 19, &[10, 11, 12]); // IDR
        out
    }

    fn append_fake_nal(out: &mut Vec<u8>, nal_type: u8, payload: &[u8]) {
        out.extend_from_slice(&[0, 0, 0, 1]);
        out.push(nal_type << 1);
        out.push(1);
        out.extend_from_slice(payload);
    }
}
