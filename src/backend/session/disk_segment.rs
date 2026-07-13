use super::*;

pub(super) struct DiskSegmentSink {
    pub(super) writer_tx: Sender<DiskSegmentWriteJob>,
    pub(super) tx: Sender<ReplayEvent>,
    pub(super) run_index: u64,
    pub(super) metadata: Option<EncodedReplayMetadata>,
    pub(super) header_units: Vec<HevcAccessUnit>,
    pub(super) current: Option<DiskSegmentBuilder>,
    pub(super) pending: VecDeque<DiskSegmentBuilder>,
    pub(super) target_duration_90k: u64,
    pub(super) latest_audio_ticks: u64,
}

impl DiskSegmentSink {
    pub(super) fn new(
        writer_tx: Sender<DiskSegmentWriteJob>,
        tx: Sender<ReplayEvent>,
        run_index: u64,
        replay_duration: Duration,
    ) -> Self {
        let target_seconds = DISK_SEGMENT_TARGET_SECONDS
            .min((replay_duration.as_secs_f32() / 2.0).max(1.0))
            .max(1.0);
        Self {
            writer_tx,
            tx,
            run_index,
            metadata: None,
            header_units: Vec::new(),
            current: None,
            pending: VecDeque::new(),
            target_duration_90k: seconds_to_90k(target_seconds),
            latest_audio_ticks: 0,
        }
    }

    pub(super) fn finish_success(&mut self, recorded: &super::vpl::VplOneCopyRecordOutput) {
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
        self.flush_ready_pending();
        while let Some(segment) = self.pending.pop_front() {
            self.write_completed_segment(segment);
        }
    }

    pub(super) fn status(&mut self, message: &str) {
        self.send_status(message);
    }

    pub(super) fn video_track_started(&mut self, info: super::vpl::VplOutputTrackInfo) {
        self.metadata = Some(EncodedReplayMetadata {
            width: info.width,
            height: info.height,
            color: info.color,
            codec: info.codec,
            audio_sample_rate: crate::backend::audio::TARGET_SAMPLE_RATE,
            audio_channel_count: crate::backend::audio::TARGET_CHANNELS,
        });
        self.send_status("oneVPL 编码器已 Init，磁盘循环分段器已建立。");
    }

    pub(super) fn hevc_access_unit(&mut self, sample: &HevcAccessUnit) {
        self.remember_parameter_sets(sample);
        if sample.discard_from_track {
            return;
        }
        let Some(metadata) = self.metadata.clone() else {
            return;
        };
        if self.current.is_none() {
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
            self.current = Some(DiskSegmentBuilder::new(
                metadata,
                sample.timestamp_90k,
                scale_90k_to_ticks(
                    sample.timestamp_90k,
                    crate::backend::audio::TARGET_SAMPLE_RATE,
                ),
            ));
            self.flush_ready_pending();
        }

        if let Some(current) = self.current.as_mut() {
            current.push_video(sample);
        }
    }

    pub(super) fn aac_access_unit(&mut self, sample: &AacAccessUnit) {
        self.latest_audio_ticks = self.latest_audio_ticks.max(
            sample
                .timestamp_ticks
                .saturating_add(u64::from(sample.duration_ticks)),
        );
        self.flush_ready_pending();
        if let Some(segment) = self
            .pending
            .iter_mut()
            .find(|segment| segment.contains_audio_timestamp(sample.timestamp_ticks))
        {
            segment.push_audio(sample);
            return;
        }
        if let Some(current) = self.current.as_mut()
            && sample.timestamp_ticks >= current.start_audio_ticks
        {
            current.push_audio(sample);
        }
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
        if self
            .writer_tx
            .send(DiskSegmentWriteJob {
                run_index: self.run_index,
                segment,
                enqueued_at: Instant::now(),
            })
            .is_err()
        {
            self.send_status("磁盘循环分段写入失败：异步 writer 已停止");
        }
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
    pub(super) video_track: HevcMp4Track,
    pub(super) audio_track: Option<AacLcMp4Track>,
}

#[derive(Debug, Clone)]
pub(super) struct DiskSegmentIndexedTracks {
    pub(super) mp4_path: PathBuf,
    pub(super) index: HevcAacMp4Index,
}

#[derive(Debug, Clone)]
pub(super) struct DiskPreparedReplaySnapshot {
    pub(super) video_track: HevcPreparedMp4Track,
    pub(super) audio_track: Option<AacPreparedMp4Track>,
}

pub(super) fn concat_disk_indexed_segments(
    segments: &[DiskSegmentIndexedTracks],
) -> Option<DiskPreparedReplaySnapshot> {
    let first = segments.first()?;
    let mut video_samples = Vec::new();
    let mut audio_samples = Vec::new();
    let mut parameter_sets = first.index.video_track.parameter_sets.clone();
    let mut video_base_90k = 0u64;
    let mut audio_base_ticks = 0u64;
    let mut audio_sample_rate = 48_000;
    let mut audio_channel_count = 2;
    for segment in segments {
        merge_hevc_parameter_sets(
            &mut parameter_sets,
            &segment.index.video_track.parameter_sets,
        );
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
            audio_sample_rate = audio.sample_rate;
            audio_channel_count = audio.channel_count;
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
        return None;
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
    Some(DiskPreparedReplaySnapshot {
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
    })
}

pub(super) fn merge_hevc_parameter_sets(dst: &mut HevcParameterSets, src: &HevcParameterSets) {
    append_unique_bytes(&mut dst.vps, &src.vps);
    append_unique_bytes(&mut dst.sps, &src.sps);
    append_unique_bytes(&mut dst.pps, &src.pps);
}

pub(super) fn append_unique_bytes(dst: &mut Vec<Vec<u8>>, src: &[Vec<u8>]) {
    for item in src {
        if !dst.iter().any(|seen| seen == item) {
            dst.push(item.clone());
        }
    }
}
