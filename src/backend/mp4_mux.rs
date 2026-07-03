//! 极小 HEVC/H.265 MP4 封装器。
//!
//! 这里故意只消费“已编码码流字节”，不接触 raw frame。编码后的 bitstream 已不属于
//! GPU-only raw frame 契约范围；封装阶段可以在 CPU 上整理 box 与 sample 表。

use crate::error::BackendError;
use std::collections::BTreeSet;
use std::fs;
use std::path::Path;

const VIDEO_TIMESCALE: u32 = 90_000;
const MOVIE_TIMESCALE: u32 = 1_000;

#[derive(Debug, Clone)]
pub struct HevcAccessUnit {
    pub timestamp_90k: u64,
    pub data: Vec<u8>,
    pub is_sync: bool,
}

#[derive(Debug, Clone)]
pub struct HevcMp4Track {
    pub width: u16,
    pub height: u16,
    pub duration_90k: u64,
    pub samples: Vec<HevcAccessUnit>,
}

#[derive(Debug, Clone)]
struct PreparedSample {
    duration_90k: u32,
    data: Vec<u8>,
    is_sync: bool,
}

#[derive(Debug, Clone)]
struct ParameterSets {
    vps: Vec<Vec<u8>>,
    sps: Vec<Vec<u8>>,
    pps: Vec<Vec<u8>>,
}

pub fn write_hevc_mp4(path: &Path, track: &HevcMp4Track) -> Result<(), BackendError> {
    if track.samples.is_empty() {
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
    let mut converted = Vec::with_capacity(track.samples.len());
    for (index, sample) in track.samples.iter().enumerate() {
        let (data, is_sync, found_sets) = hevc_annex_b_to_length_prefixed(&sample.data)?;
        merge_parameter_sets(&mut parameter_sets, found_sets);
        let duration_90k = sample_duration(track, index);
        converted.push(PreparedSample {
            duration_90k,
            data,
            is_sync: sample.is_sync || is_sync || index == 0,
        });
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

    let ftyp = make_ftyp();
    let mdat_payload_len: u64 = converted.iter().map(|s| s.data.len() as u64).sum();
    let first_sample_offset = ftyp.len() as u64 + 8;
    let mut offsets = Vec::with_capacity(converted.len());
    let mut cursor = first_sample_offset;
    for sample in &converted {
        offsets.push(cursor);
        cursor += sample.data.len() as u64;
    }

    let mdat = make_mdat(&converted, mdat_payload_len)?;
    let moov = make_moov(track, &converted, &offsets, &parameter_sets)?;

    let mut file = Vec::with_capacity(ftyp.len() + mdat.len() + moov.len());
    file.extend(ftyp);
    file.extend(mdat);
    file.extend(moov);

    if let Some(parent) = path.parent()
        && !parent.as_os_str().is_empty()
    {
        fs::create_dir_all(parent).map_err(|err| BackendError::Io(err.to_string()))?;
    }
    fs::write(path, file).map_err(|err| BackendError::Io(err.to_string()))
}

fn sample_duration(track: &HevcMp4Track, index: usize) -> u32 {
    let current = track.samples[index].timestamp_90k;
    let next = track
        .samples
        .get(index + 1)
        .map(|s| s.timestamp_90k)
        .unwrap_or(track.duration_90k);
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

fn make_mdat(samples: &[PreparedSample], payload_len: u64) -> Result<Vec<u8>, BackendError> {
    let total = payload_len + 8;
    if total > u32::MAX as u64 {
        return Err(BackendError::unsupported(
            "MP4 封装",
            "mdat",
            "当前极小 muxer 尚未实现 large-size mdat",
        ));
    }
    let mut out = Vec::with_capacity(total as usize);
    out.extend_from_slice(&(total as u32).to_be_bytes());
    out.extend_from_slice(b"mdat");
    for sample in samples {
        out.extend_from_slice(&sample.data);
    }
    Ok(out)
}

fn make_moov(
    track: &HevcMp4Track,
    samples: &[PreparedSample],
    offsets: &[u64],
    sets: &ParameterSets,
) -> Result<Vec<u8>, BackendError> {
    let movie_duration_ms = ((track.duration_90k * MOVIE_TIMESCALE as u64) / VIDEO_TIMESCALE as u64)
        .min(u32::MAX as u64) as u32;
    let mut p = Vec::new();
    p.extend(make_mvhd(movie_duration_ms));
    p.extend(make_trak(track, samples, offsets, sets, movie_duration_ms)?);
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
    be32(&mut p, 2); // next_track_id
    full_box(*b"mvhd", 0, 0, p)
}

fn make_trak(
    track: &HevcMp4Track,
    samples: &[PreparedSample],
    offsets: &[u64],
    sets: &ParameterSets,
    movie_duration_ms: u32,
) -> Result<Vec<u8>, BackendError> {
    let mut p = Vec::new();
    p.extend(make_tkhd(track.width, track.height, movie_duration_ms));
    p.extend(make_mdia(track, samples, offsets, sets)?);
    Ok(mp4_box(*b"trak", p))
}

fn make_tkhd(width: u16, height: u16, duration_ms: u32) -> Vec<u8> {
    let mut p = Vec::new();
    be32(&mut p, 0);
    be32(&mut p, 0);
    be32(&mut p, 1); // track_id
    be32(&mut p, 0);
    be32(&mut p, duration_ms);
    be32(&mut p, 0);
    be32(&mut p, 0);
    be16(&mut p, 0); // layer
    be16(&mut p, 0); // alternate_group
    be16(&mut p, 0); // volume for video
    be16(&mut p, 0);
    unity_matrix(&mut p);
    be32(&mut p, (width as u32) << 16);
    be32(&mut p, (height as u32) << 16);
    full_box(*b"tkhd", 0, 0x000007, p)
}

fn make_mdia(
    track: &HevcMp4Track,
    samples: &[PreparedSample],
    offsets: &[u64],
    sets: &ParameterSets,
) -> Result<Vec<u8>, BackendError> {
    let mut p = Vec::new();
    p.extend(make_mdhd(track.duration_90k));
    p.extend(make_hdlr());
    p.extend(make_minf(track, samples, offsets, sets)?);
    Ok(mp4_box(*b"mdia", p))
}

fn make_mdhd(duration_90k: u64) -> Vec<u8> {
    let mut p = Vec::new();
    be32(&mut p, 0);
    be32(&mut p, 0);
    be32(&mut p, VIDEO_TIMESCALE);
    be32(&mut p, duration_90k.min(u32::MAX as u64) as u32);
    be16(&mut p, 0x55c4); // "und"
    be16(&mut p, 0);
    full_box(*b"mdhd", 0, 0, p)
}

fn make_hdlr() -> Vec<u8> {
    let mut p = Vec::new();
    be32(&mut p, 0);
    p.extend_from_slice(b"vide");
    be32(&mut p, 0);
    be32(&mut p, 0);
    be32(&mut p, 0);
    p.extend_from_slice(b"VideoHandler\0");
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

fn make_vmhd() -> Vec<u8> {
    let mut p = Vec::new();
    be16(&mut p, 0);
    be16(&mut p, 0);
    be16(&mut p, 0);
    be16(&mut p, 0);
    full_box(*b"vmhd", 0, 1, p)
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
    p.extend(make_stco(offsets)?);
    Ok(mp4_box(*b"stbl", p))
}

fn make_stsd(track: &HevcMp4Track, sets: &ParameterSets) -> Result<Vec<u8>, BackendError> {
    let hvc1 = make_hvc1_sample_entry(track, sets)?;
    let mut p = Vec::new();
    be32(&mut p, 1);
    p.extend(hvc1);
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
    p.extend(make_hvcc(sets)?);
    p.extend(make_colr_bt2020_pq_full());
    Ok(mp4_box(*b"hvc1", p))
}

fn make_hvcc(sets: &ParameterSets) -> Result<Vec<u8>, BackendError> {
    let mut p = vec![
        1,    // configurationVersion
        0x02, // Main10 profile_idc
    ];
    p.extend_from_slice(&0x6000_0000u32.to_be_bytes());
    p.extend_from_slice(&[0; 6]);
    p.push(153); // level 5.1
    be16(&mut p, 0xf000);
    p.push(0xfc);
    p.push(0xfc | 1); // 4:2:0
    p.push(0xf8 | 2); // 10-bit luma
    p.push(0xf8 | 2); // 10-bit chroma
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

fn make_colr_bt2020_pq_full() -> Vec<u8> {
    let mut p = Vec::new();
    p.extend_from_slice(b"nclx");
    be16(&mut p, 9); // BT.2020
    be16(&mut p, 16); // PQ
    be16(&mut p, 9); // BT.2020 non-constant
    p.push(0x80); // full range flag
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

fn make_stco(offsets: &[u64]) -> Result<Vec<u8>, BackendError> {
    let mut p = Vec::new();
    be32(&mut p, offsets.len() as u32);
    for &offset in offsets {
        if offset > u32::MAX as u64 {
            return Err(BackendError::unsupported(
                "MP4 封装",
                "stco",
                "当前极小 muxer 尚未实现 co64",
            ));
        }
        be32(&mut p, offset as u32);
    }
    Ok(full_box(*b"stco", 0, 0, p))
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
