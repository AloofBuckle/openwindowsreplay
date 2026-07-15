use super::*;

pub(super) struct DiskSegmentSink {
    pub(super) writer_tx: SyncSender<DiskSegmentWriteJob>,
    pub(super) tx: Sender<ReplayEvent>,
    pub(super) run_index: u64,
    pub(super) stop: Arc<AtomicBool>,
    pub(super) failed: bool,
    pub(super) metadata: Option<EncodedReplayMetadata>,
    pub(super) header_units: Vec<HevcAccessUnit>,
    pub(super) current: Option<DiskSegmentBuilder>,
    pub(super) pending: VecDeque<DiskSegmentBuilder>,
    pub(super) target_duration_90k: u64,
    pub(super) latest_audio_ticks: u64,
    pub(super) live_ring: Arc<Mutex<EncodedReplayRing>>,
}

impl DiskSegmentSink {
    pub(super) fn new(
        writer_tx: SyncSender<DiskSegmentWriteJob>,
        tx: Sender<ReplayEvent>,
        run_index: u64,
        replay_duration: Duration,
        stop: Arc<AtomicBool>,
        live_ring: Arc<Mutex<EncodedReplayRing>>,
    ) -> Self {
        let target_seconds = DISK_SEGMENT_TARGET_SECONDS
            .min((replay_duration.as_secs_f32() / 2.0).max(1.0))
            .max(1.0);
        Self {
            writer_tx,
            tx,
            run_index,
            stop,
            failed: false,
            metadata: None,
            header_units: Vec::new(),
            current: None,
            pending: VecDeque::new(),
            target_duration_90k: seconds_to_90k(target_seconds),
            latest_audio_ticks: 0,
            live_ring,
        }
    }

    pub(super) fn finish_success(&mut self, recorded: &super::vpl::VplOneCopyRecordOutput) {
        if self.failed {
            return;
        }
        if let Some(mut current) = self.current.take() {
            current.end_90k = Some(recorded.video_track.duration_90k);
            self.pending.push_back(current);
        }
        let final_audio_ticks = recorded
            .audio_track
            .as_ref()
            .map(|track| track.duration_ticks)
            .unwrap_or(self.latest_audio_ticks);
        self.latest_audio_ticks = self.latest_audio_ticks.max(final_audio_ticks);
        if let Ok(mut ring) = self.live_ring.lock() {
            ring.finish_segment_with_audio_duration(
                recorded.video_track.duration_90k,
                recorded
                    .audio_track
                    .as_ref()
                    .map(|track| (track.duration_ticks, track.sample_rate)),
            );
        }
        self.flush_ready_pending();
        while let Some(segment) = self.pending.pop_front() {
            self.write_completed_segment(segment);
        }
    }

    pub(super) fn status(&mut self, message: &str) {
        self.send_status(message);
    }

    pub(super) fn video_track_started(&mut self, info: super::vpl::VplOutputTrackInfo) {
        let metadata = EncodedReplayMetadata {
            width: info.width,
            height: info.height,
            color: info.color,
            codec: info.codec,
            audio_sample_rate: crate::backend::audio::TARGET_SAMPLE_RATE,
            audio_channel_count: crate::backend::audio::TARGET_CHANNELS,
        };
        self.metadata = Some(metadata.clone());
        if let Ok(mut ring) = self.live_ring.lock() {
            ring.start_segment(metadata);
        }
        self.send_status("oneVPL 编码器已 Init，磁盘循环分段器已建立。");
    }

    pub(super) fn hevc_access_unit(&mut self, sample: &HevcAccessUnit) {
        if self.failed {
            return;
        }
        self.remember_parameter_sets(sample);
        if sample.discard_from_track {
            if let Ok(mut ring) = self.live_ring.lock() {
                ring.push_video_au_90k(sample);
            }
            return;
        }
        let Some(metadata) = self.metadata.clone() else {
            return;
        };
        if self.current.is_none() {
            if !sample.is_sync {
                return;
            }
            self.current = Some(DiskSegmentBuilder::new(
                metadata.clone(),
                sample.timestamp_90k,
                scale_90k_to_ticks(
                    sample.timestamp_90k,
                    crate::backend::audio::TARGET_SAMPLE_RATE,
                ),
            ));
        }

        let should_rotate = self.current.as_ref().is_some_and(|current| {
            sample.is_sync
                && current.has_video()
                && sample.timestamp_90k.saturating_sub(current.start_90k)
                    >= self.target_duration_90k
        });
        if should_rotate {
            if let Some(mut current) = self.current.take() {
                current.end_90k = Some(sample.timestamp_90k);
                self.pending.push_back(current);
            }
            if self.pending.len() > DISK_PENDING_SEGMENT_LIMIT {
                self.fail(format!(
                    "磁盘循环音频未能推进，待完成分段超过上限 {}",
                    DISK_PENDING_SEGMENT_LIMIT
                ));
                return;
            }
            self.current = Some(DiskSegmentBuilder::new(
                metadata.clone(),
                sample.timestamp_90k,
                scale_90k_to_ticks(
                    sample.timestamp_90k,
                    crate::backend::audio::TARGET_SAMPLE_RATE,
                ),
            ));
            if let Ok(mut ring) = self.live_ring.lock() {
                ring.restart_timeline(metadata);
            }
            self.flush_ready_pending();
        }

        if let Some(current) = self.current.as_mut() {
            current.push_video(sample);
        }
        if let Ok(mut ring) = self.live_ring.lock() {
            ring.push_video_au_90k(sample);
        }
    }

    pub(super) fn aac_access_unit(&mut self, sample: &AacAccessUnit) {
        if self.failed {
            return;
        }
        if let Some(segment) = self
            .pending
            .iter_mut()
            .find(|segment| segment.contains_audio_timestamp(sample.timestamp_ticks))
        {
            segment.push_audio(sample);
        } else if let Some(current) = self.current.as_mut()
            && sample.timestamp_ticks >= current.start_audio_ticks
        {
            current.push_audio(sample);
        }
        self.latest_audio_ticks = self.latest_audio_ticks.max(
            sample
                .timestamp_ticks
                .saturating_add(u64::from(sample.duration_ticks)),
        );
        if let Ok(mut ring) = self.live_ring.lock() {
            ring.push_audio_au_ticks(sample, crate::backend::audio::TARGET_SAMPLE_RATE);
        }
        self.flush_ready_pending();
    }

    pub(super) fn remember_parameter_sets(&mut self, sample: &HevcAccessUnit) {
        if !sample.discard_from_track && !sample.is_sync && !self.header_units.is_empty() {
            return;
        }
        let Some(data) = super::mp4_mux::hevc_annex_b_parameter_set_access_unit(&sample.data)
        else {
            return;
        };
        if self
            .header_units
            .iter()
            .any(|header| header.data.as_ref() == data.as_slice())
        {
            return;
        }
        self.header_units.push(HevcAccessUnit {
            timestamp_90k: 0,
            data: data.into(),
            is_sync: false,
            discard_from_track: true,
        });
    }

    pub(super) fn flush_ready_pending(&mut self) {
        loop {
            let ready = self
                .pending
                .front()
                .and_then(|segment| segment.end_audio_ticks())
                .is_some_and(|end_ticks| self.latest_audio_ticks >= end_ticks);
            if !ready {
                break;
            }
            if let Some(segment) = self.pending.pop_front() {
                self.write_completed_segment(segment);
            }
        }
    }

    pub(super) fn write_completed_segment(&mut self, segment: DiskSegmentBuilder) {
        let Some(segment) = segment.into_tracks(&self.header_units) else {
            return;
        };
        let job = DiskSegmentWriteJob {
            run_index: self.run_index,
            segment,
            enqueued_at: Instant::now(),
        };
        match self.writer_tx.try_send(job) {
            Ok(()) => {}
            Err(TrySendError::Full(_)) => self.fail(format!(
                "磁盘循环写入队列已满（容量 {}），磁盘速度低于编码分段产生速度",
                DISK_WRITER_QUEUE_CAPACITY
            )),
            Err(TrySendError::Disconnected(_)) => {
                self.fail("磁盘循环异步 writer 已停止".to_owned())
            }
        }
    }

    pub(super) fn fail(&mut self, message: String) {
        if self.failed {
            return;
        }
        self.failed = true;
        self.stop.store(true, Ordering::Relaxed);
        let _ = self.tx.send(ReplayEvent::Error(message));
    }

    pub(super) fn send_status(&mut self, message: &str) {
        let _ = self.tx.send(ReplayEvent::BackendStatus {
            index: self.run_index,
            message: message.to_owned(),
        });
    }
}

#[derive(Debug, Clone)]
pub(super) struct DiskSegmentBuilder {
    pub(super) metadata: EncodedReplayMetadata,
    pub(super) start_90k: u64,
    pub(super) start_audio_ticks: u64,
    pub(super) end_90k: Option<u64>,
    pub(super) video_samples: Vec<HevcAccessUnit>,
    pub(super) audio_samples: Vec<AacAccessUnit>,
}

impl DiskSegmentBuilder {
    pub(super) fn new(
        metadata: EncodedReplayMetadata,
        start_90k: u64,
        start_audio_ticks: u64,
    ) -> Self {
        Self {
            metadata,
            start_90k,
            start_audio_ticks,
            end_90k: None,
            video_samples: Vec::new(),
            audio_samples: Vec::new(),
        }
    }

    pub(super) fn has_video(&self) -> bool {
        self.video_samples
            .iter()
            .any(|sample| !sample.discard_from_track)
    }

    pub(super) fn push_video(&mut self, sample: &HevcAccessUnit) {
        let mut sample = sample.clone();
        sample.timestamp_90k = sample.timestamp_90k.saturating_sub(self.start_90k);
        self.video_samples.push(sample);
    }

    pub(super) fn push_audio(&mut self, sample: &AacAccessUnit) {
        let mut sample = sample.clone();
        sample.timestamp_ticks = sample
            .timestamp_ticks
            .saturating_sub(self.start_audio_ticks);
        self.audio_samples.push(sample);
    }

    pub(super) fn end_audio_ticks(&self) -> Option<u64> {
        self.end_90k.map(|end| {
            scale_90k_to_ticks(end, self.metadata.audio_sample_rate).max(self.start_audio_ticks)
        })
    }

    pub(super) fn contains_audio_timestamp(&self, timestamp_ticks: u64) -> bool {
        timestamp_ticks >= self.start_audio_ticks
            && self
                .end_audio_ticks()
                .is_some_and(|end_ticks| timestamp_ticks < end_ticks)
    }

    pub(super) fn into_tracks(self, header_units: &[HevcAccessUnit]) -> Option<DiskSegmentTracks> {
        let end_90k = self.end_90k?;
        let duration_90k = end_90k.saturating_sub(self.start_90k).max(1);
        if !self.has_video() {
            return None;
        }
        if !self
            .video_samples
            .first()
            .is_some_and(|sample| sample.is_sync)
        {
            return None;
        }
        let mut samples = header_units
            .iter()
            .map(|sample| {
                let mut sample = sample.clone();
                sample.timestamp_90k = 0;
                sample.discard_from_track = true;
                sample
            })
            .collect::<Vec<_>>();
        samples.extend(self.video_samples);
        let audio_track = if self.audio_samples.is_empty() {
            None
        } else {
            let sample_duration_ticks = self
                .audio_samples
                .iter()
                .map(|sample| {
                    sample
                        .timestamp_ticks
                        .saturating_add(u64::from(sample.duration_ticks))
                })
                .max()
                .unwrap_or(1)
                .max(1);
            let segment_duration_ticks =
                scale_90k_to_ticks(duration_90k, self.metadata.audio_sample_rate);
            Some(AacLcMp4Track {
                sample_rate: self.metadata.audio_sample_rate,
                channel_count: self.metadata.audio_channel_count,
                duration_ticks: sample_duration_ticks.max(segment_duration_ticks).max(1),
                samples: self.audio_samples,
            })
        };
        Some(DiskSegmentTracks {
            source_start_90k: self.start_90k,
            source_end_90k: end_90k,
            video_track: HevcMp4Track {
                width: self.metadata.width,
                height: self.metadata.height,
                duration_90k,
                color: self.metadata.color,
                codec: self.metadata.codec,
                samples,
            },
            audio_track,
        })
    }
}

#[derive(Debug, Clone)]
pub(super) struct DiskSegmentTracks {
    pub(super) source_start_90k: u64,
    pub(super) source_end_90k: u64,
    pub(super) video_track: HevcMp4Track,
    pub(super) audio_track: Option<AacLcMp4Track>,
}

#[derive(Debug, Clone)]
pub(super) struct DiskSegmentIndexedTracks {
    pub(super) mp4_path: PathBuf,
    pub(super) index: HevcAacMp4Index,
    pub(super) lease: Arc<()>,
}

#[derive(Debug, Clone)]
pub(super) struct DiskPreparedReplaySnapshot {
    pub(super) video_track: HevcPreparedMp4Track,
    pub(super) audio_track: Option<AacPreparedMp4Track>,
    pub(super) _leases: Vec<Arc<()>>,
}

pub(super) fn concat_disk_indexed_segments(
    segments: &[DiskSegmentIndexedTracks],
) -> Result<Option<DiskPreparedReplaySnapshot>, BackendError> {
    let Some(first) = segments.first() else {
        return Ok(None);
    };
    validate_disk_segment_compatibility(first, first)?;
    let mut video_samples = Vec::new();
    let mut audio_samples = Vec::new();
    let parameter_sets = first.index.video_track.parameter_sets.clone();
    let mut video_base_90k = 0u64;
    let mut audio_base_ticks = 0u64;
    let audio_sample_rate = first
        .index
        .audio_track
        .as_ref()
        .map(|audio| audio.sample_rate)
        .unwrap_or(48_000);
    let audio_channel_count = first
        .index
        .audio_track
        .as_ref()
        .map(|audio| audio.channel_count)
        .unwrap_or(2);
    for segment in segments {
        validate_disk_segment_compatibility(first, segment)?;
        video_samples.extend(segment.index.video_track.samples.iter().map(|sample| {
            HevcPreparedSample {
                duration_90k: sample.duration_90k,
                is_sync: sample.is_sync,
                data: Mp4SampleFileRange {
                    path: segment.mp4_path.clone(),
                    offset: sample.offset,
                    len: sample.len,
                },
            }
        }));
        video_base_90k = video_base_90k.saturating_add(segment.index.video_track.duration_90k);
        if let Some(audio) = &segment.index.audio_track {
            audio_samples.extend(audio.samples.iter().map(|sample| AacPreparedSample {
                duration_ticks: sample.duration_ticks,
                data: Mp4SampleFileRange {
                    path: segment.mp4_path.clone(),
                    offset: sample.offset,
                    len: sample.len,
                },
            }));
            audio_base_ticks = audio_base_ticks.saturating_add(audio.duration_ticks);
        } else {
            audio_base_ticks = audio_base_ticks.saturating_add(scale_90k_to_ticks(
                segment.index.video_track.duration_90k,
                audio_sample_rate,
            ));
        }
    }
    if video_samples.is_empty() {
        return Ok(None);
    }
    let audio_track = if audio_samples.is_empty() {
        None
    } else {
        Some(AacPreparedMp4Track {
            sample_rate: audio_sample_rate,
            channel_count: audio_channel_count,
            duration_ticks: audio_base_ticks.max(1),
            samples: audio_samples,
        })
    };
    Ok(Some(DiskPreparedReplaySnapshot {
        video_track: HevcPreparedMp4Track {
            width: first.index.video_track.width,
            height: first.index.video_track.height,
            duration_90k: video_base_90k.max(1),
            color: first.index.video_track.color,
            codec: first.index.video_track.codec,
            parameter_sets,
            samples: video_samples,
        },
        audio_track,
        _leases: segments
            .iter()
            .map(|segment| segment.lease.clone())
            .collect(),
    }))
}

pub(super) fn validate_disk_segment_compatibility(
    expected: &DiskSegmentIndexedTracks,
    actual: &DiskSegmentIndexedTracks,
) -> Result<(), BackendError> {
    let expected_video = &expected.index.video_track;
    let actual_video = &actual.index.video_track;
    let incompatible_video = expected_video.width != actual_video.width
        || expected_video.height != actual_video.height
        || expected_video.color != actual_video.color
        || expected_video.codec != actual_video.codec
        || expected_video.parameter_sets != actual_video.parameter_sets;
    if incompatible_video {
        return Err(BackendError::unsupported(
            "磁盘循环缓存拼接",
            actual.mp4_path.display().to_string(),
            "分段的分辨率、色彩、HEVC profile/位深或 VPS/SPS/PPS 与当前保存 epoch 不一致",
        ));
    }
    if !actual_video
        .samples
        .first()
        .is_some_and(|sample| sample.is_sync)
    {
        return Err(BackendError::unsupported(
            "磁盘循环缓存拼接",
            actual.mp4_path.display().to_string(),
            "分段首个视频 sample 不是关键帧",
        ));
    }
    match (&expected.index.audio_track, &actual.index.audio_track) {
        (Some(expected), Some(actual))
            if expected.sample_rate == actual.sample_rate
                && expected.channel_count == actual.channel_count => {}
        (None, None) => {}
        _ => {
            return Err(BackendError::unsupported(
                "磁盘循环缓存拼接",
                actual.mp4_path.display().to_string(),
                "分段 AAC 轨道是否存在、采样率或声道数与当前保存 epoch 不一致",
            ));
        }
    }
    Ok(())
}
