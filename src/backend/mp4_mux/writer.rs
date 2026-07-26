use super::*;

#[allow(dead_code)]
pub fn write_hevc_mp4(path: &Path, track: &HevcMp4Track) -> Result<(), BackendError> {
    write_hevc_aac_mp4(path, track, None)
}

pub fn write_hevc_aac_mp4(
    path: &Path,
    video_track: &HevcMp4Track,
    audio_track: Option<&AacLcMp4Track>,
) -> Result<(), BackendError> {
    write_hevc_aac_mp4_with_index(path, video_track, audio_track).map(|_| ())
}

pub(crate) fn write_hevc_aac_mp4_with_index(
    path: &Path,
    video_track: &HevcMp4Track,
    audio_track: Option<&AacLcMp4Track>,
) -> Result<HevcAacMp4Index, BackendError> {
    if let Some(audio) = audio_track {
        validate_aac_track(audio)?;
    }

    let (converted, parameter_sets, final_video_duration_90k) = prepare_video_track(video_track)?;
    let prepared_audio = audio_track.map(prepare_audio_track).transpose()?;

    let ftyp = make_ftyp();
    let mdat_payload_len: u64 = converted.iter().map(|s| s.data.len()).sum::<u64>()
        + prepared_audio
            .as_ref()
            .map(|audio| audio.iter().map(|s| s.data.len()).sum::<u64>())
            .unwrap_or(0);
    let mdat_header = make_mdat_header(mdat_payload_len);
    let first_sample_offset = ftyp.len() as u64 + mdat_header.len() as u64;
    let mut cursor = first_sample_offset;
    let mut video_offsets = Vec::with_capacity(converted.len());
    for sample in &converted {
        video_offsets.push(cursor);
        cursor += sample.data.len();
    }
    let mut audio_offsets = Vec::new();
    if let Some(audio_samples) = &prepared_audio {
        audio_offsets.reserve(audio_samples.len());
        for sample in audio_samples {
            audio_offsets.push(cursor);
            cursor += sample.data.len();
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
    let file = File::create(path).map_err(|err| BackendError::Io(err.to_string()))?;
    let mut file = BufWriter::with_capacity(1024 * 1024, file);
    file.write_all(&ftyp)
        .map_err(|err| BackendError::Io(err.to_string()))?;
    file.write_all(&mdat_header)
        .map_err(|err| BackendError::Io(err.to_string()))?;
    write_mdat_payload(&mut file, &converted, prepared_audio.as_deref())?;
    file.write_all(&moov)
        .map_err(|err| BackendError::Io(err.to_string()))?;
    file.flush()
        .map_err(|err| BackendError::Io(err.to_string()))?;

    Ok(HevcAacMp4Index {
        video_track: HevcIndexedMp4Track {
            width: video_track.width,
            height: video_track.height,
            duration_90k: final_video_duration_90k,
            color: video_track.color,
            codec: video_track.codec,
            parameter_sets,
            samples: converted
                .into_iter()
                .zip(video_offsets)
                .map(|(sample, offset)| HevcIndexedSample {
                    duration_90k: sample.duration_90k,
                    is_sync: sample.is_sync,
                    offset,
                    len: sample.data.len(),
                })
                .collect(),
        },
        audio_track: match (audio_track, prepared_audio, audio_offsets) {
            (Some(track), Some(samples), offsets) => Some(AacIndexedMp4Track {
                sample_rate: track.sample_rate,
                channel_count: track.channel_count,
                duration_ticks: track.duration_ticks,
                samples: samples
                    .into_iter()
                    .zip(offsets)
                    .map(|(sample, offset)| AacIndexedSample {
                        timestamp_ticks: sample.timestamp_ticks,
                        duration_ticks: sample.duration_ticks,
                        offset,
                        len: sample.data.len(),
                    })
                    .collect(),
            }),
            _ => None,
        },
    })
}

pub(crate) fn write_prepared_hevc_aac_mp4(
    path: &Path,
    video_track: &HevcPreparedMp4Track,
    audio_track: Option<&AacPreparedMp4Track>,
) -> Result<(), BackendError> {
    if let Some(audio) = audio_track
        && audio.sample_rate == 0
    {
        return Err(BackendError::unsupported(
            "MP4 封装",
            "AAC sample_rate=0",
            "音轨 timescale/采样率必须大于 0",
        ));
    }
    if let Some(audio) = audio_track {
        validate_prepared_audio_track(audio)?;
    }

    let converted = video_track
        .samples
        .iter()
        .map(|sample| PreparedSample {
            duration_90k: sample.duration_90k,
            data: SamplePayload::FileRange(sample.data.clone()),
            is_sync: sample.is_sync,
        })
        .collect::<Vec<_>>();
    if converted.is_empty() {
        return Err(BackendError::unsupported(
            "MP4 封装",
            "HEVC prepared video track",
            "没有可封装的视频 sample",
        ));
    }
    let prepared_audio = audio_track.map(|track| {
        track
            .samples
            .iter()
            .map(|sample| PreparedAudioSample {
                timestamp_ticks: sample.timestamp_ticks,
                duration_ticks: sample.duration_ticks,
                data: SamplePayload::FileRange(sample.data.clone()),
            })
            .collect::<Vec<_>>()
    });
    let video_meta = HevcMp4Track {
        width: video_track.width,
        height: video_track.height,
        duration_90k: video_track.duration_90k,
        color: video_track.color,
        codec: video_track.codec,
        samples: Vec::new(),
    };
    let audio_meta = audio_track.map(|track| AacLcMp4Track {
        sample_rate: track.sample_rate,
        channel_count: track.channel_count,
        duration_ticks: track.duration_ticks,
        samples: Vec::new(),
    });

    let ftyp = make_ftyp();
    let mdat_payload_len: u64 = converted.iter().map(|s| s.data.len()).sum::<u64>()
        + prepared_audio
            .as_ref()
            .map(|audio| audio.iter().map(|s| s.data.len()).sum::<u64>())
            .unwrap_or(0);
    let mdat_header = make_mdat_header(mdat_payload_len);
    let first_sample_offset = ftyp.len() as u64 + mdat_header.len() as u64;
    let mut cursor = first_sample_offset;
    let mut video_offsets = Vec::with_capacity(converted.len());
    for sample in &converted {
        video_offsets.push(cursor);
        cursor += sample.data.len();
    }
    let mut audio_offsets = Vec::new();
    if let Some(audio_samples) = &prepared_audio {
        audio_offsets.reserve(audio_samples.len());
        for sample in audio_samples {
            audio_offsets.push(cursor);
            cursor += sample.data.len();
        }
    }
    let audio_ref = match (audio_meta.as_ref(), prepared_audio.as_deref()) {
        (Some(track), Some(samples)) => Some(PreparedAacTrackRef {
            track,
            samples,
            offsets: &audio_offsets,
        }),
        _ => None,
    };
    let moov = make_moov(
        &video_meta,
        video_track.duration_90k,
        &converted,
        &video_offsets,
        &video_track.parameter_sets,
        audio_ref,
    )?;

    if let Some(parent) = path.parent()
        && !parent.as_os_str().is_empty()
    {
        fs::create_dir_all(parent).map_err(|err| BackendError::Io(err.to_string()))?;
    }
    let file = File::create(path).map_err(|err| BackendError::Io(err.to_string()))?;
    let mut file = BufWriter::with_capacity(1024 * 1024, file);
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

pub(super) fn prepare_video_track(
    track: &HevcMp4Track,
) -> Result<(Vec<PreparedSample>, HevcParameterSets, u64), BackendError> {
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

    let mut parameter_sets = HevcParameterSets {
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
            data: SamplePayload::Memory(data.into()),
            is_sync,
        });
    }
    if !converted.first().is_some_and(|sample| sample.is_sync) {
        return Err(BackendError::unsupported(
            "MP4 封装",
            "HEVC video track",
            "首个可播放视频 sample 不是 IDR/CRA 关键帧，拒绝生成不可独立解码的 MP4",
        ));
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

pub(super) fn prepare_audio_track(
    track: &AacLcMp4Track,
) -> Result<Vec<PreparedAudioSample>, BackendError> {
    validate_aac_track(track)?;
    if track.samples.is_empty() {
        return Err(BackendError::unsupported(
            "MP4 封装",
            "AAC audio track",
            "请求写入音轨但没有可封装的 AAC sample",
        ));
    }

    let mut prepared = Vec::with_capacity(track.samples.len());
    let mut expected_timestamp = None;
    for (index, sample) in track.samples.iter().enumerate() {
        if let Some(expected) = expected_timestamp
            && sample.timestamp_ticks != expected
        {
            return Err(BackendError::unsupported(
                "MP4 封装",
                "AAC sample timeline",
                format!(
                    "AAC sample 时间戳不连续：index={index} expected={expected} actual={}",
                    sample.timestamp_ticks
                ),
            ));
        }
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
        expected_timestamp = Some(
            sample
                .timestamp_ticks
                .saturating_add(u64::from(duration_ticks)),
        );
        prepared.push(PreparedAudioSample {
            timestamp_ticks: sample.timestamp_ticks,
            duration_ticks,
            data: SamplePayload::Memory(sample.data.clone()),
        });
    }
    if expected_timestamp.is_some_and(|end| track.duration_ticks < end) {
        return Err(BackendError::unsupported(
            "MP4 封装",
            "AAC track duration",
            format!(
                "duration_ticks={} 小于最后 sample 结束时间 {}",
                track.duration_ticks,
                expected_timestamp.unwrap_or_default()
            ),
        ));
    }
    Ok(prepared)
}

fn validate_prepared_audio_track(track: &AacPreparedMp4Track) -> Result<(), BackendError> {
    if track.samples.is_empty() {
        return Err(BackendError::unsupported(
            "MP4 封装",
            "AAC prepared audio track",
            "请求写入音轨但没有可封装的 AAC sample",
        ));
    }
    let mut expected_timestamp = None;
    for (index, sample) in track.samples.iter().enumerate() {
        if let Some(expected) = expected_timestamp
            && sample.timestamp_ticks != expected
        {
            return Err(BackendError::unsupported(
                "MP4 封装",
                "AAC prepared sample timeline",
                format!(
                    "AAC sample 时间戳不连续：index={index} expected={expected} actual={}",
                    sample.timestamp_ticks
                ),
            ));
        }
        expected_timestamp = Some(
            sample
                .timestamp_ticks
                .saturating_add(u64::from(sample.duration_ticks)),
        );
    }
    if expected_timestamp.is_some_and(|end| track.duration_ticks < end) {
        return Err(BackendError::unsupported(
            "MP4 封装",
            "AAC prepared track duration",
            format!(
                "duration_ticks={} 小于最后 sample 结束时间 {}",
                track.duration_ticks,
                expected_timestamp.unwrap_or_default()
            ),
        ));
    }
    Ok(())
}

pub(super) fn sample_duration(
    samples: &[&HevcAccessUnit],
    track_duration_90k: u64,
    index: usize,
) -> u32 {
    let current = samples[index].timestamp_90k;
    let next = samples
        .get(index + 1)
        .map(|s| s.timestamp_90k)
        .unwrap_or(track_duration_90k);
    next.saturating_sub(current).max(1).min(u32::MAX as u64) as u32
}

pub(super) fn merge_parameter_sets(dst: &mut HevcParameterSets, src: HevcParameterSets) {
    append_unique(&mut dst.vps, src.vps);
    append_unique(&mut dst.sps, src.sps);
    append_unique(&mut dst.pps, src.pps);
}

pub(super) fn append_unique(dst: &mut Vec<Vec<u8>>, src: Vec<Vec<u8>>) {
    let mut seen: BTreeSet<Vec<u8>> = dst.iter().cloned().collect();
    for item in src {
        if seen.insert(item.clone()) {
            dst.push(item);
        }
    }
}

pub(super) fn hevc_annex_b_to_length_prefixed(
    data: &[u8],
) -> Result<(Vec<u8>, bool, HevcParameterSets), BackendError> {
    let mut out = Vec::with_capacity(data.len());
    let mut sets = HevcParameterSets {
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
            16..=21 => is_sync = true,
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

pub(crate) fn hevc_annex_b_has_random_access_nal(data: &[u8]) -> bool {
    let mut pos = 0usize;
    while let Some((start, code_len)) = find_start_code(data, pos) {
        let nal_start = start + code_len;
        let next = find_start_code(data, nal_start)
            .map(|(next_start, _)| next_start)
            .unwrap_or(data.len());
        pos = next;
        if nal_start < next {
            let nal_type = (data[nal_start] >> 1) & 0x3f;
            if matches!(nal_type, 16..=21) {
                return true;
            }
        }
    }
    false
}

#[cfg(test)]
pub(crate) fn hevc_annex_b_parameter_set_access_unit(data: &[u8]) -> Option<Vec<u8>> {
    let sets = super::parameter_sets::extract_hevc_parameter_sets(data);
    (!sets.is_empty()).then(|| super::parameter_sets::canonical_annex_b_header(&sets))
}

pub(super) fn find_start_code(data: &[u8], from: usize) -> Option<(usize, usize)> {
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
