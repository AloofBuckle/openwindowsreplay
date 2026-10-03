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

    let prepared_video = prepare_video_track(video_track)?;
    let converted = prepared_video.samples;
    let parameter_sets = prepared_video.parameter_sets;
    let final_video_duration_ticks = prepared_video.duration_ticks;
    let video_timescale = prepared_video.timescale;
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
        video_timescale,
        final_video_duration_ticks,
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
            timescale: video_timescale,
            duration_ticks: final_video_duration_ticks,
            color: video_track.color,
            codec: video_track.codec,
            parameter_sets,
            samples: converted
                .into_iter()
                .zip(video_offsets)
                .map(|(sample, offset)| HevcIndexedSample {
                    duration_ticks: sample.duration_ticks,
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
            duration_ticks: sample.duration_ticks,
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
        duration_90k: 0,
        presentation_duration_100ns: None,
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
        video_track.timescale,
        video_track.duration_ticks,
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

#[derive(Debug)]
pub(super) struct PreparedVideoTrack {
    pub(super) timescale: u32,
    pub(super) duration_ticks: u64,
    pub(super) samples: Vec<PreparedSample>,
    pub(super) parameter_sets: HevcParameterSets,
}

pub(super) fn prepare_video_track(
    track: &HevcMp4Track,
) -> Result<PreparedVideoTrack, BackendError> {
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

    let exact_timestamp_count = playable_samples
        .iter()
        .filter(|sample| sample.presentation_timestamp_100ns.is_some())
        .count();
    if exact_timestamp_count != 0 && exact_timestamp_count != playable_samples.len() {
        return Err(BackendError::unsupported(
            "MP4 封装",
            "HEVC presentation timestamp",
            "视频 sample 混用了 90 kHz transport PTS 与 100 ns source PTS",
        ));
    }
    let exact_timeline = exact_timestamp_count == playable_samples.len();
    if track.presentation_duration_100ns.is_some() && !exact_timeline {
        return Err(BackendError::unsupported(
            "MP4 封装",
            "HEVC presentation duration",
            "存在 100 ns track duration，但视频 sample 没有完整的 100 ns source PTS",
        ));
    }
    let timescale = if exact_timeline {
        VIDEO_TIMESCALE_100NS
    } else {
        VIDEO_TIMESCALE_90K
    };
    let timestamps = playable_samples
        .iter()
        .map(|sample| {
            if exact_timeline {
                sample.presentation_timestamp_100ns.unwrap_or(0)
            } else {
                sample.timestamp_90k
            }
        })
        .collect::<Vec<_>>();
    for pair in timestamps.windows(2) {
        if pair[1] <= pair[0] {
            return Err(BackendError::unsupported(
                "MP4 封装",
                format!("video timescale={timescale}"),
                format!(
                    "视频 source PTS 非严格递增：previous={} current={}",
                    pair[0], pair[1]
                ),
            ));
        }
    }
    let requested_duration_ticks = if exact_timeline {
        track
            .presentation_duration_100ns
            .unwrap_or_else(|| infer_video_duration(&timestamps))
    } else {
        track.duration_90k
    }
    .max(1);

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
        let duration_ticks =
            sample_duration(&timestamps, requested_duration_ticks, playable_index)?;
        playable_index += 1;
        converted.push(PreparedSample {
            duration_ticks,
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
    let prepared_duration_ticks = converted
        .iter()
        .map(|sample| u64::from(sample.duration_ticks))
        .sum::<u64>();
    if let Some(last) = converted.last_mut()
        && prepared_duration_ticks < requested_duration_ticks
    {
        let extra = requested_duration_ticks.saturating_sub(prepared_duration_ticks);
        last.duration_ticks = u64::from(last.duration_ticks)
            .saturating_add(extra)
            .try_into()
            .map_err(|_| {
                BackendError::unsupported(
                    "MP4 封装",
                    format!("video timescale={timescale}"),
                    "末个视频 sample duration 超过 MP4 stts 的 u32 上限",
                )
            })?;
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

    let final_duration_ticks = converted
        .iter()
        .map(|sample| u64::from(sample.duration_ticks))
        .sum::<u64>()
        .max(1);
    Ok(PreparedVideoTrack {
        timescale,
        duration_ticks: final_duration_ticks,
        samples: converted,
        parameter_sets,
    })
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
    timestamps: &[u64],
    track_duration_ticks: u64,
    index: usize,
) -> Result<u32, BackendError> {
    let current = timestamps[index];
    let next = timestamps
        .get(index + 1)
        .copied()
        .unwrap_or(track_duration_ticks);
    let duration = next.saturating_sub(current).max(1);
    u32::try_from(duration).map_err(|_| {
        BackendError::unsupported(
            "MP4 封装",
            format!("sample_index={index} duration_ticks={duration}"),
            "单个视频 sample duration 超过 MP4 stts 的 u32 上限",
        )
    })
}

fn infer_video_duration(timestamps: &[u64]) -> u64 {
    let Some(&last) = timestamps.last() else {
        return 1;
    };
    let tail = timestamps
        .iter()
        .rev()
        .copied()
        .find(|timestamp| *timestamp < last)
        .map(|previous| last.saturating_sub(previous).max(1))
        .unwrap_or(1);
    last.saturating_add(tail).max(1)
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
