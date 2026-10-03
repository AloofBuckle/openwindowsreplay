use super::*;

pub(super) fn make_moov(
    video_track: &HevcMp4Track,
    video_timescale: u32,
    video_duration_ticks: u64,
    video_samples: &[PreparedSample],
    video_offsets: &[u64],
    sets: &HevcParameterSets,
    audio: Option<PreparedAacTrackRef<'_>>,
) -> Result<Vec<u8>, BackendError> {
    let video_duration_ms = scale_duration(video_duration_ticks, video_timescale, MOVIE_TIMESCALE);
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
        video_timescale,
        video_duration_ticks,
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

pub(super) fn make_mvhd(duration_ms: u32) -> Vec<u8> {
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

pub(super) fn make_video_trak(
    track: &HevcMp4Track,
    timescale: u32,
    duration_ticks: u64,
    samples: &[PreparedSample],
    offsets: &[u64],
    sets: &HevcParameterSets,
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
    p.extend(make_mdia(
        track,
        timescale,
        duration_ticks,
        samples,
        offsets,
        sets,
    )?);
    Ok(mp4_box(*b"trak", p))
}

pub(super) fn make_audio_trak(
    track: &AacLcMp4Track,
    samples: &[PreparedAudioSample],
    offsets: &[u64],
    track_duration_ms: u32,
) -> Result<Vec<u8>, BackendError> {
    let media_duration_ticks = samples
        .iter()
        .map(|sample| u64::from(sample.duration_ticks))
        .sum::<u64>()
        .max(1);
    let start_offset_ticks = samples
        .first()
        .map(|sample| sample.timestamp_ticks)
        .unwrap_or(0);
    let sample_end_ticks = start_offset_ticks.saturating_add(media_duration_ticks);
    let timeline_duration_ticks = track.duration_ticks.max(sample_end_ticks);
    let trailing_empty_ticks = timeline_duration_ticks.saturating_sub(sample_end_ticks);
    let mut p = Vec::new();
    p.extend(make_tkhd(2, track_duration_ms, 0x0100, 0, 0));
    if start_offset_ticks > 0 || trailing_empty_ticks > 0 {
        p.extend(make_audio_edts(
            start_offset_ticks,
            media_duration_ticks,
            timeline_duration_ticks,
            track.sample_rate,
        ));
    }
    p.extend(make_audio_mdia(
        track,
        media_duration_ticks,
        samples,
        offsets,
    )?);
    Ok(mp4_box(*b"trak", p))
}

pub(super) fn make_audio_edts(
    start_offset_ticks: u64,
    media_duration_ticks: u64,
    timeline_duration_ticks: u64,
    sample_rate: u32,
) -> Vec<u8> {
    let start_offset_ms = scale_duration(start_offset_ticks, sample_rate, MOVIE_TIMESCALE);
    let media_end_ms = scale_duration(
        start_offset_ticks.saturating_add(media_duration_ticks),
        sample_rate,
        MOVIE_TIMESCALE,
    );
    let timeline_duration_ms =
        scale_duration(timeline_duration_ticks, sample_rate, MOVIE_TIMESCALE);
    let media_duration_ms = media_end_ms.saturating_sub(start_offset_ms).max(1);
    let trailing_empty_ms = timeline_duration_ms.saturating_sub(media_end_ms);
    let mut entries = Vec::new();
    if start_offset_ticks > 0 {
        entries.push((start_offset_ms, u32::MAX));
    }
    entries.push((media_duration_ms, 0));
    if trailing_empty_ms > 0 {
        entries.push((trailing_empty_ms, u32::MAX));
    }

    let mut p = Vec::new();
    be32(&mut p, entries.len() as u32);
    for (segment_duration, media_time) in entries {
        be32(&mut p, segment_duration);
        be32(&mut p, media_time);
        be16(&mut p, 1);
        be16(&mut p, 0);
    }
    mp4_box(*b"edts", full_box(*b"elst", 0, 0, p))
}

pub(super) fn make_tkhd(
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

pub(super) fn make_mdia(
    track: &HevcMp4Track,
    timescale: u32,
    duration_ticks: u64,
    samples: &[PreparedSample],
    offsets: &[u64],
    sets: &HevcParameterSets,
) -> Result<Vec<u8>, BackendError> {
    let mut p = Vec::new();
    p.extend(make_mdhd(timescale, duration_ticks));
    p.extend(make_hdlr(*b"vide", "VideoHandler"));
    p.extend(make_minf(track, samples, offsets, sets)?);
    Ok(mp4_box(*b"mdia", p))
}

pub(super) fn make_audio_mdia(
    track: &AacLcMp4Track,
    media_duration_ticks: u64,
    samples: &[PreparedAudioSample],
    offsets: &[u64],
) -> Result<Vec<u8>, BackendError> {
    let mut p = Vec::new();
    p.extend(make_mdhd(track.sample_rate, media_duration_ticks));
    p.extend(make_hdlr(*b"soun", "SoundHandler"));
    p.extend(make_audio_minf(track, samples, offsets)?);
    Ok(mp4_box(*b"mdia", p))
}

pub(super) fn make_mdhd(timescale: u32, duration: u64) -> Vec<u8> {
    let mut p = Vec::new();
    let version = if duration > u64::from(u32::MAX) { 1 } else { 0 };
    if version == 1 {
        be64(&mut p, 0);
        be64(&mut p, 0);
        be32(&mut p, timescale);
        be64(&mut p, duration);
    } else {
        be32(&mut p, 0);
        be32(&mut p, 0);
        be32(&mut p, timescale);
        be32(&mut p, duration as u32);
    }
    be16(&mut p, 0x55c4); // "und"
    be16(&mut p, 0);
    full_box(*b"mdhd", version, 0, p)
}

pub(super) fn make_hdlr(handler_type: [u8; 4], name: &str) -> Vec<u8> {
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

pub(super) fn make_minf(
    track: &HevcMp4Track,
    samples: &[PreparedSample],
    offsets: &[u64],
    sets: &HevcParameterSets,
) -> Result<Vec<u8>, BackendError> {
    let mut p = Vec::new();
    p.extend(make_vmhd());
    p.extend(make_dinf());
    p.extend(make_stbl(track, samples, offsets, sets)?);
    Ok(mp4_box(*b"minf", p))
}

pub(super) fn make_audio_minf(
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

pub(super) fn make_vmhd() -> Vec<u8> {
    let mut p = Vec::new();
    be16(&mut p, 0);
    be16(&mut p, 0);
    be16(&mut p, 0);
    be16(&mut p, 0);
    full_box(*b"vmhd", 0, 1, p)
}

pub(super) fn make_smhd() -> Vec<u8> {
    let mut p = Vec::new();
    be16(&mut p, 0); // balance
    be16(&mut p, 0);
    full_box(*b"smhd", 0, 0, p)
}

pub(super) fn make_dinf() -> Vec<u8> {
    let url = full_box(*b"url ", 0, 1, Vec::new());
    let mut dref_payload = Vec::new();
    be32(&mut dref_payload, 1);
    dref_payload.extend(url);
    mp4_box(*b"dinf", full_box(*b"dref", 0, 0, dref_payload))
}

pub(super) fn make_stbl(
    track: &HevcMp4Track,
    samples: &[PreparedSample],
    offsets: &[u64],
    sets: &HevcParameterSets,
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

pub(super) fn make_audio_stbl(
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

pub(super) fn make_stsd(
    track: &HevcMp4Track,
    sets: &HevcParameterSets,
) -> Result<Vec<u8>, BackendError> {
    let hvc1 = make_hvc1_sample_entry(track, sets)?;
    let mut p = Vec::new();
    be32(&mut p, 1);
    p.extend(hvc1);
    Ok(full_box(*b"stsd", 0, 0, p))
}

pub(super) fn make_audio_stsd(track: &AacLcMp4Track) -> Result<Vec<u8>, BackendError> {
    let mp4a = make_mp4a_sample_entry(track)?;
    let mut p = Vec::new();
    be32(&mut p, 1);
    p.extend(mp4a);
    Ok(full_box(*b"stsd", 0, 0, p))
}

pub(super) fn make_hvc1_sample_entry(
    track: &HevcMp4Track,
    sets: &HevcParameterSets,
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
    let name = b"RustReplay HEVC";
    compressor[0] = name.len() as u8;
    compressor[1..=name.len()].copy_from_slice(name);
    p.extend_from_slice(&compressor);
    be16(&mut p, 0x0018);
    be16(&mut p, 0xffff);
    p.extend(make_hvcc_with_codec(sets, track.codec)?);
    p.extend(make_colr_nclx(track.color));
    Ok(mp4_box(*b"hvc1", p))
}

pub(super) fn make_mp4a_sample_entry(track: &AacLcMp4Track) -> Result<Vec<u8>, BackendError> {
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

pub(super) fn make_esds_aac_lc(track: &AacLcMp4Track) -> Result<Vec<u8>, BackendError> {
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

pub(super) fn descriptor(tag: u8, payload: Vec<u8>) -> Vec<u8> {
    let mut out = Vec::new();
    out.push(tag);
    write_descriptor_len(&mut out, payload.len() as u32);
    out.extend(payload);
    out
}

pub(super) fn write_descriptor_len(out: &mut Vec<u8>, len: u32) {
    out.push(((len >> 21) as u8 & 0x7f) | 0x80);
    out.push(((len >> 14) as u8 & 0x7f) | 0x80);
    out.push(((len >> 7) as u8 & 0x7f) | 0x80);
    out.push((len as u8) & 0x7f);
}

pub(super) fn aac_lc_audio_specific_config(
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

pub(super) fn aac_sample_rate_index(sample_rate: u32) -> Option<u8> {
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

pub(super) fn make_hvcc_with_codec(
    sets: &HevcParameterSets,
    codec: HevcCodecMetadata,
) -> Result<Vec<u8>, BackendError> {
    let config = decoder_configuration_from_parameter_sets(sets, codec)?;
    let mut p = vec![
        1, // configurationVersion
        ((config.profile_space & 0x03) << 6)
            | (u8::from(config.tier_flag) << 5)
            | (config.profile_idc & 0x1f),
    ];
    p.extend_from_slice(&config.profile_compatibility_flags.to_be_bytes());
    p.extend_from_slice(&config.constraint_indicator_flags);
    p.push(config.level_idc);
    be16(&mut p, 0xf000);
    p.push(0xfc);
    p.push(0xfc | (config.chroma_format_idc & 0x03));
    p.push(0xf8 | (config.bit_depth_luma_minus8 & 0x07));
    p.push(0xf8 | (config.bit_depth_chroma_minus8 & 0x07));
    be16(&mut p, 0);
    p.push(
        ((config.num_temporal_layers & 0x07) << 3)
            | (u8::from(config.temporal_id_nested) << 2)
            | 0x03,
    );
    p.push(3);
    append_hvcc_array(&mut p, 32, &sets.vps)?;
    append_hvcc_array(&mut p, 33, &sets.sps)?;
    append_hvcc_array(&mut p, 34, &sets.pps)?;
    Ok(mp4_box(*b"hvcC", p))
}

pub(super) fn append_hvcc_array(
    p: &mut Vec<u8>,
    nal_type: u8,
    nals: &[Vec<u8>],
) -> Result<(), BackendError> {
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

pub(super) fn make_colr_nclx(color: NclxColorMetadata) -> Vec<u8> {
    let mut p = Vec::new();
    p.extend_from_slice(b"nclx");
    be16(&mut p, color.colour_primaries);
    be16(&mut p, color.transfer_characteristics);
    be16(&mut p, color.matrix_coefficients);
    p.push(if color.full_range { 0x80 } else { 0x00 });
    mp4_box(*b"colr", p)
}

pub(super) fn make_stts(samples: &[PreparedSample]) -> Vec<u8> {
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

pub(super) fn make_audio_stts(samples: &[PreparedAudioSample]) -> Vec<u8> {
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

pub(super) fn make_stss(samples: &[PreparedSample]) -> Vec<u8> {
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

pub(super) fn make_stsc() -> Vec<u8> {
    let mut p = Vec::new();
    be32(&mut p, 1);
    be32(&mut p, 1);
    be32(&mut p, 1);
    be32(&mut p, 1);
    full_box(*b"stsc", 0, 0, p)
}

pub(super) fn make_stsz(samples: &[PreparedSample]) -> Vec<u8> {
    let mut p = Vec::new();
    be32(&mut p, 0);
    be32(&mut p, samples.len() as u32);
    for sample in samples {
        be32(&mut p, sample.data.len() as u32);
    }
    full_box(*b"stsz", 0, 0, p)
}

pub(super) fn make_audio_stsz(samples: &[PreparedAudioSample]) -> Vec<u8> {
    let mut p = Vec::new();
    be32(&mut p, 0);
    be32(&mut p, samples.len() as u32);
    for sample in samples {
        be32(&mut p, sample.data.len() as u32);
    }
    full_box(*b"stsz", 0, 0, p)
}

pub(super) fn make_chunk_offsets(offsets: &[u64]) -> Result<Vec<u8>, BackendError> {
    if offsets.iter().any(|&offset| offset > u32::MAX as u64) {
        return make_co64(offsets);
    }
    make_stco(offsets)
}

pub(super) fn make_stco(offsets: &[u64]) -> Result<Vec<u8>, BackendError> {
    let mut p = Vec::new();
    be32(&mut p, offsets.len() as u32);
    for &offset in offsets {
        be32(&mut p, offset as u32);
    }
    Ok(full_box(*b"stco", 0, 0, p))
}

pub(super) fn make_co64(offsets: &[u64]) -> Result<Vec<u8>, BackendError> {
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

pub(super) fn validate_aac_track(track: &AacLcMp4Track) -> Result<(), BackendError> {
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

pub(super) fn scale_duration(duration: u64, source_timescale: u32, target_timescale: u32) -> u32 {
    if source_timescale == 0 {
        return 0;
    }
    ((duration * u64::from(target_timescale)).div_ceil(u64::from(source_timescale)))
        .min(u32::MAX as u64) as u32
}

pub(super) fn mp4_box(name: [u8; 4], payload: Vec<u8>) -> Vec<u8> {
    let size = (payload.len() + 8) as u32;
    let mut out = Vec::with_capacity(size as usize);
    out.extend_from_slice(&size.to_be_bytes());
    out.extend_from_slice(&name);
    out.extend(payload);
    out
}

pub(super) fn full_box(name: [u8; 4], version: u8, flags: u32, payload: Vec<u8>) -> Vec<u8> {
    let mut p = Vec::with_capacity(payload.len() + 4);
    p.push(version);
    p.push(((flags >> 16) & 0xff) as u8);
    p.push(((flags >> 8) & 0xff) as u8);
    p.push((flags & 0xff) as u8);
    p.extend(payload);
    mp4_box(name, p)
}

pub(super) fn unity_matrix(p: &mut Vec<u8>) {
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

pub(super) fn be16(p: &mut Vec<u8>, value: u16) {
    p.extend_from_slice(&value.to_be_bytes());
}

pub(super) fn be32(p: &mut Vec<u8>, value: u32) {
    p.extend_from_slice(&value.to_be_bytes());
}

pub(super) fn be64(p: &mut Vec<u8>, value: u64) {
    p.extend_from_slice(&value.to_be_bytes());
}
