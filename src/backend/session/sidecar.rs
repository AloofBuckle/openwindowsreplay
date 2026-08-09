use super::*;

pub(super) fn write_disk_segment_sidecar(
    path: &Path,
    index: &HevcAacMp4Index,
) -> Result<(), BackendError> {
    if let Some(parent) = path.parent()
        && !parent.as_os_str().is_empty()
    {
        fs::create_dir_all(parent).map_err(|err| BackendError::Io(err.to_string()))?;
    }
    let file = fs::File::create(path).map_err(|err| BackendError::Io(err.to_string()))?;
    let mut file = BufWriter::with_capacity(1024 * 1024, file);
    file.write_all(DISK_SEGMENT_SIDECAR_MAGIC_V4)
        .map_err(|err| BackendError::Io(err.to_string()))?;
    write_u16(&mut file, index.video_track.width)?;
    write_u16(&mut file, index.video_track.height)?;
    write_color(&mut file, index.video_track.color)?;
    write_codec(&mut file, index.video_track.codec)?;
    write_parameter_sets(&mut file, &index.video_track.parameter_sets)?;
    write_u32(&mut file, index.video_track.timescale)?;
    write_u64(&mut file, index.video_track.duration_ticks)?;
    write_u64(&mut file, index.video_track.samples.len() as u64)?;
    for sample in &index.video_track.samples {
        write_u32(&mut file, sample.duration_ticks)?;
        write_u8(&mut file, u8::from(sample.is_sync))?;
        write_u64(&mut file, sample.offset)?;
        write_u64(&mut file, sample.len)?;
    }
    match &index.audio_track {
        Some(audio) => {
            write_u8(&mut file, 1)?;
            write_u32(&mut file, audio.sample_rate)?;
            write_u16(&mut file, audio.channel_count)?;
            write_u64(&mut file, audio.duration_ticks)?;
            write_u64(&mut file, audio.samples.len() as u64)?;
            for sample in &audio.samples {
                write_u64(&mut file, sample.timestamp_ticks)?;
                write_u32(&mut file, sample.duration_ticks)?;
                write_u64(&mut file, sample.offset)?;
                write_u64(&mut file, sample.len)?;
            }
        }
        None => write_u8(&mut file, 0)?,
    }
    file.flush()
        .map_err(|err| BackendError::Io(err.to_string()))
}

pub(super) fn read_disk_segment_sidecar(path: &Path) -> Result<HevcAacMp4Index, BackendError> {
    let file = fs::File::open(path).map_err(|err| BackendError::Io(err.to_string()))?;
    let mut file = BufReader::with_capacity(1024 * 1024, file);
    let mut magic = [0u8; 8];
    file.read_exact(&mut magic)
        .map_err(|err| BackendError::Io(err.to_string()))?;
    let version = if &magic == DISK_SEGMENT_SIDECAR_MAGIC_V3 {
        SidecarVersion::V3
    } else if &magic == DISK_SEGMENT_SIDECAR_MAGIC_V4 {
        SidecarVersion::V4
    } else {
        return Err(BackendError::unsupported(
            "磁盘循环缓存",
            path.display().to_string(),
            "sidecar 文件头不匹配",
        ));
    };
    let width = read_u16(&mut file)?;
    let height = read_u16(&mut file)?;
    let color = read_color(&mut file)?;
    let codec = read_codec(&mut file)?;
    let parameter_sets = read_parameter_sets(&mut file)?;
    let (timescale, duration_ticks) = match version {
        SidecarVersion::V3 => (VIDEO_CLOCK_HZ as u32, read_u64(&mut file)?),
        SidecarVersion::V4 => {
            let timescale = read_u32(&mut file)?;
            if timescale == 0 {
                return Err(BackendError::unsupported(
                    "磁盘循环缓存",
                    path.display().to_string(),
                    "sidecar 视频 timescale 为 0",
                ));
            }
            (timescale, read_u64(&mut file)?)
        }
    };
    let video_sample_count = read_len(&mut file)?;
    let mut video_samples = Vec::with_capacity(video_sample_count);
    for _ in 0..video_sample_count {
        video_samples.push(HevcIndexedSample {
            duration_ticks: read_u32(&mut file)?,
            is_sync: read_u8(&mut file)? != 0,
            offset: read_u64(&mut file)?,
            len: read_u64(&mut file)?,
        });
    }
    let audio_track = if read_u8(&mut file)? != 0 {
        let sample_rate = read_u32(&mut file)?;
        let channel_count = read_u16(&mut file)?;
        let duration_ticks = read_u64(&mut file)?;
        let sample_count = read_len(&mut file)?;
        let mut samples = Vec::with_capacity(sample_count);
        for _ in 0..sample_count {
            samples.push(AacIndexedSample {
                timestamp_ticks: read_u64(&mut file)?,
                duration_ticks: read_u32(&mut file)?,
                offset: read_u64(&mut file)?,
                len: read_u64(&mut file)?,
            });
        }
        Some(AacIndexedMp4Track {
            sample_rate,
            channel_count,
            duration_ticks,
            samples,
        })
    } else {
        None
    };
    Ok(HevcAacMp4Index {
        video_track: HevcIndexedMp4Track {
            width,
            height,
            timescale,
            duration_ticks,
            color,
            codec,
            parameter_sets,
            samples: video_samples,
        },
        audio_track,
    })
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum SidecarVersion {
    V3,
    V4,
}

pub(super) fn write_color(
    out: &mut impl Write,
    color: NclxColorMetadata,
) -> Result<(), BackendError> {
    write_u16(out, color.colour_primaries)?;
    write_u16(out, color.transfer_characteristics)?;
    write_u16(out, color.matrix_coefficients)?;
    write_u8(out, u8::from(color.full_range))
}

pub(super) fn read_color(input: &mut impl Read) -> Result<NclxColorMetadata, BackendError> {
    Ok(NclxColorMetadata {
        colour_primaries: read_u16(input)?,
        transfer_characteristics: read_u16(input)?,
        matrix_coefficients: read_u16(input)?,
        full_range: read_u8(input)? != 0,
    })
}

pub(super) fn write_codec(
    out: &mut impl Write,
    codec: HevcCodecMetadata,
) -> Result<(), BackendError> {
    write_u8(out, codec.profile_idc)?;
    write_u8(out, codec.chroma_format_idc)?;
    write_u8(out, codec.bit_depth_luma_minus8)?;
    write_u8(out, codec.bit_depth_chroma_minus8)
}

pub(super) fn read_codec(input: &mut impl Read) -> Result<HevcCodecMetadata, BackendError> {
    Ok(HevcCodecMetadata {
        profile_idc: read_u8(input)?,
        chroma_format_idc: read_u8(input)?,
        bit_depth_luma_minus8: read_u8(input)?,
        bit_depth_chroma_minus8: read_u8(input)?,
    })
}

pub(super) fn write_parameter_sets(
    out: &mut impl Write,
    sets: &HevcParameterSets,
) -> Result<(), BackendError> {
    write_nal_array(out, &sets.vps)?;
    write_nal_array(out, &sets.sps)?;
    write_nal_array(out, &sets.pps)
}

pub(super) fn read_parameter_sets(
    input: &mut impl Read,
) -> Result<HevcParameterSets, BackendError> {
    Ok(HevcParameterSets {
        vps: read_nal_array(input)?,
        sps: read_nal_array(input)?,
        pps: read_nal_array(input)?,
    })
}

pub(super) fn write_nal_array(out: &mut impl Write, nals: &[Vec<u8>]) -> Result<(), BackendError> {
    write_u64(out, nals.len() as u64)?;
    for nal in nals {
        write_bytes(out, nal)?;
    }
    Ok(())
}

pub(super) fn read_nal_array(input: &mut impl Read) -> Result<Vec<Vec<u8>>, BackendError> {
    let count = read_len(input)?;
    let mut out = Vec::with_capacity(count);
    for _ in 0..count {
        out.push(read_bytes(input)?);
    }
    Ok(out)
}

pub(super) fn write_bytes(out: &mut impl Write, bytes: &[u8]) -> Result<(), BackendError> {
    write_u64(out, bytes.len() as u64)?;
    out.write_all(bytes)
        .map_err(|err| BackendError::Io(err.to_string()))
}

pub(super) fn read_bytes(input: &mut impl Read) -> Result<Vec<u8>, BackendError> {
    let len = read_len(input)?;
    let mut out = vec![0u8; len];
    input
        .read_exact(&mut out)
        .map_err(|err| BackendError::Io(err.to_string()))?;
    Ok(out)
}

pub(super) fn read_len(input: &mut impl Read) -> Result<usize, BackendError> {
    usize::try_from(read_u64(input)?).map_err(|_| {
        BackendError::unsupported("磁盘循环缓存", "sidecar 长度字段", "长度超过当前平台 usize")
    })
}

pub(super) fn write_u8(out: &mut impl Write, value: u8) -> Result<(), BackendError> {
    out.write_all(&[value])
        .map_err(|err| BackendError::Io(err.to_string()))
}

pub(super) fn read_u8(input: &mut impl Read) -> Result<u8, BackendError> {
    let mut buf = [0u8; 1];
    input
        .read_exact(&mut buf)
        .map_err(|err| BackendError::Io(err.to_string()))?;
    Ok(buf[0])
}

pub(super) fn write_u16(out: &mut impl Write, value: u16) -> Result<(), BackendError> {
    out.write_all(&value.to_le_bytes())
        .map_err(|err| BackendError::Io(err.to_string()))
}

pub(super) fn read_u16(input: &mut impl Read) -> Result<u16, BackendError> {
    let mut buf = [0u8; 2];
    input
        .read_exact(&mut buf)
        .map_err(|err| BackendError::Io(err.to_string()))?;
    Ok(u16::from_le_bytes(buf))
}

pub(super) fn write_u32(out: &mut impl Write, value: u32) -> Result<(), BackendError> {
    out.write_all(&value.to_le_bytes())
        .map_err(|err| BackendError::Io(err.to_string()))
}

pub(super) fn read_u32(input: &mut impl Read) -> Result<u32, BackendError> {
    let mut buf = [0u8; 4];
    input
        .read_exact(&mut buf)
        .map_err(|err| BackendError::Io(err.to_string()))?;
    Ok(u32::from_le_bytes(buf))
}

pub(super) fn write_u64(out: &mut impl Write, value: u64) -> Result<(), BackendError> {
    out.write_all(&value.to_le_bytes())
        .map_err(|err| BackendError::Io(err.to_string()))
}

pub(super) fn read_u64(input: &mut impl Read) -> Result<u64, BackendError> {
    let mut buf = [0u8; 8];
    input
        .read_exact(&mut buf)
        .map_err(|err| BackendError::Io(err.to_string()))?;
    Ok(u64::from_le_bytes(buf))
}

pub(super) fn seconds_to_90k(seconds: f32) -> u64 {
    (f64::from(seconds.max(0.001)) * VIDEO_CLOCK_HZ as f64).round() as u64
}

pub(super) fn scale_90k_to_ns(value: u64) -> u64 {
    ((u128::from(value) * 1_000_000_000u128).div_ceil(u128::from(VIDEO_CLOCK_HZ)))
        .min(u128::from(u64::MAX)) as u64
}

pub(super) fn scale_90k_to_ticks(value: u64, sample_rate: u32) -> u64 {
    ((u128::from(value) * u128::from(sample_rate) + u128::from(VIDEO_CLOCK_HZ / 2))
        / u128::from(VIDEO_CLOCK_HZ))
    .min(u128::from(u64::MAX)) as u64
}

pub(super) fn timestamp_for_filename() -> String {
    let millis = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_millis())
        .unwrap_or_default();
    millis.to_string()
}
