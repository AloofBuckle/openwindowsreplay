use super::*;

#[allow(clippy::too_many_arguments)]
pub(super) fn run_recording_worker(
    request: pipeline::RecordingRequest,
    mut caps: ProbeCaps,
    save_dir: PathBuf,
    cache_dir: Option<PathBuf>,
    replay_duration: Duration,
    buffer_mode: ReplayBufferMode,
    stop_flag: Arc<AtomicBool>,
    tx: Sender<ReplayEvent>,
    encoded_ring: Option<Arc<Mutex<EncodedReplayRing>>>,
    disk_store: Option<Arc<Mutex<DiskReplayStore>>>,
    disk_live_ring: Option<Arc<Mutex<EncodedReplayRing>>>,
) {
    let start_path = cache_dir.clone().unwrap_or_else(|| save_dir.clone());
    let _ = tx.send(ReplayEvent::RecordingStarted {
        mode: buffer_mode,
        path: start_path.clone(),
    });
    let mut disk_writer = disk_store
        .clone()
        .map(|store| DiskSegmentWriter::spawn(store, tx.clone(), stop_flag.clone()));
    let requested_chroma = request.chroma_writer.chroma();
    let mut run_index = 0u64;
    while !stop_flag.load(Ordering::Relaxed) {
        let Some(record_target) = caps.record_target_for_chroma(requested_chroma) else {
            let _ = tx.send(ReplayEvent::Error(format!(
                "能力探测没有为 active 编码后端选出 {} 的 adapter/output 录制目标",
                requested_chroma.doc_label()
            )));
            return;
        };
        let _ = tx.send(ReplayEvent::BackendStatus {
            index: run_index,
            message: format!(
                "录制目标来自能力探测：adapter={} output={}",
                record_target.adapter_index, record_target.output_index
            ),
        });
        let output_dir = cache_dir.as_ref().unwrap_or(&save_dir);
        let output = output_dir.join(format!(
            "rustreplay_live_{}_{}.mp4",
            timestamp_for_filename(),
            run_index
        ));

        let (result, mut sink) = {
            let mut sink = match buffer_mode {
                ReplayBufferMode::Memory => {
                    let Some(ring) = encoded_ring.clone() else {
                        let _ = tx.send(ReplayEvent::Error(
                            "内存循环模式缺少 encoded ring".to_owned(),
                        ));
                        return;
                    };
                    ReplayRecordSink::Memory(SessionRingSink {
                        ring,
                        tx: tx.clone(),
                        segment_index: run_index,
                        started: false,
                    })
                }
                ReplayBufferMode::Disk => {
                    let Some(writer_tx) = disk_writer.as_ref().and_then(DiskSegmentWriter::sender)
                    else {
                        let _ = tx.send(ReplayEvent::Error(
                            "磁盘循环模式缺少异步 segment writer".to_owned(),
                        ));
                        return;
                    };
                    let Some(live_ring) = disk_live_ring.clone() else {
                        let _ = tx.send(ReplayEvent::Error(
                            "磁盘循环模式缺少开放分段 encoded ring".to_owned(),
                        ));
                        return;
                    };
                    ReplayRecordSink::Disk(Box::new(DiskSegmentSink::new(
                        writer_tx,
                        tx.clone(),
                        run_index,
                        replay_duration,
                        stop_flag.clone(),
                        live_ring,
                    )))
                }
            };
            let result = pipeline::record_once_gpu_only_memory_output_with_sink_cancelable(
                &request,
                &caps,
                &output,
                LIVE_RECORD_SECONDS,
                record_target.adapter_index,
                Some(stop_flag.clone()),
                Some(&mut sink),
            );
            (result, sink)
        };

        match result {
            Ok(recorded) => {
                sink.finish_success(&recorded);
                if let ReplayRecordSink::Memory(_) = sink {
                    let report = recorded.report;
                    let (snapshot, ring_packets, ring_bytes) = encoded_ring
                        .as_ref()
                        .and_then(|ring| {
                            ring.lock().ok().map(|ring| {
                                (
                                    ring.snapshot_recent_tracks(replay_duration),
                                    ring.len(),
                                    ring.bytes(),
                                )
                            })
                        })
                        .unwrap_or((None, 0, 0));
                    let snapshot = snapshot.unwrap_or_else(|| crate::ring::EncodedReplaySnapshot {
                        video_track: recorded.video_track.clone(),
                        audio_track: recorded.audio_track.clone(),
                    });
                    let clip = CompletedClip {
                        cache_path: output,
                        captured_frames: report.captured_frames,
                        encoded_samples: report.encoded_samples,
                        audio_access_units: report.audio_access_units,
                        duration_seconds: report.duration_seconds,
                        video_track: snapshot.video_track,
                        audio_track: snapshot.audio_track,
                        ring_packets,
                        ring_bytes,
                    };
                    let _ = tx.send(ReplayEvent::SegmentReady {
                        index: run_index,
                        clip,
                    });
                }
            }
            Err(err) => {
                if err.is_cancelled() {
                    break;
                }
                if err.is_reconfigure_required() {
                    let _ = tx.send(ReplayEvent::BackendStatus {
                        index: run_index,
                        message: format!("显示环境变化，重新执行能力探测后重启录制段：{err}"),
                    });
                    for _ in 0..20 {
                        if stop_flag.load(Ordering::Relaxed) {
                            break;
                        }
                        std::thread::sleep(Duration::from_millis(10));
                    }
                    if stop_flag.load(Ordering::Relaxed) {
                        break;
                    }
                    caps = crate::backend::probe_all();
                    run_index = run_index.saturating_add(1);
                    continue;
                }
                drop(sink);
                if let Some(writer) = disk_writer.take() {
                    writer.shutdown();
                }
                if let Some(store) = &disk_store
                    && let Ok(mut store) = store.lock()
                {
                    let _ = store.clear_segments();
                }
                let _ = tx.send(ReplayEvent::Error(err.to_string()));
                return;
            }
        }
        run_index = run_index.saturating_add(1);
    }
    if let Some(writer) = disk_writer.take() {
        writer.shutdown();
    }
    if let Some(store) = &disk_store {
        match store.lock() {
            Ok(mut store) => {
                if let Err(err) = store.clear_segments() {
                    let _ = tx.send(ReplayEvent::BackendStatus {
                        index: run_index,
                        message: format!("停止后清理磁盘循环缓存失败：{err}"),
                    });
                }
            }
            Err(_) => {
                let _ = tx.send(ReplayEvent::BackendStatus {
                    index: run_index,
                    message: "停止后无法清理磁盘循环缓存：store 锁已中毒".to_owned(),
                });
            }
        }
    }
    let _ = tx.send(ReplayEvent::Stopped);
}

pub(super) enum ReplayRecordSink {
    Memory(SessionRingSink),
    Disk(Box<DiskSegmentSink>),
}

impl ReplayRecordSink {
    pub(super) fn finish_success(&mut self, recorded: &super::vpl::VplOneCopyRecordOutput) {
        match self {
            Self::Memory(sink) => sink.finish_success(recorded),
            Self::Disk(sink) => sink.finish_success(recorded),
        }
    }
}

impl super::vpl::VplOneCopyRecordSink for ReplayRecordSink {
    fn status(&mut self, message: &str) {
        match self {
            Self::Memory(sink) => sink.status(message),
            Self::Disk(sink) => sink.status(message),
        }
    }

    fn video_track_started(&mut self, info: super::vpl::VplOutputTrackInfo) {
        match self {
            Self::Memory(sink) => sink.video_track_started(info),
            Self::Disk(sink) => sink.video_track_started(info),
        }
    }

    fn hevc_access_unit(&mut self, sample: &HevcAccessUnit) {
        match self {
            Self::Memory(sink) => sink.hevc_access_unit(sample),
            Self::Disk(sink) => sink.hevc_access_unit(sample),
        }
    }

    fn aac_access_unit(&mut self, sample: &AacAccessUnit) {
        match self {
            Self::Memory(sink) => sink.aac_access_unit(sample),
            Self::Disk(sink) => sink.aac_access_unit(sample),
        }
    }
}

pub(super) struct SessionRingSink {
    pub(super) ring: Arc<Mutex<EncodedReplayRing>>,
    pub(super) tx: Sender<ReplayEvent>,
    pub(super) segment_index: u64,
    pub(super) started: bool,
}

impl SessionRingSink {
    pub(super) fn finish_success(&mut self, recorded: &super::vpl::VplOneCopyRecordOutput) {
        if self.started {
            if let Ok(mut ring) = self.ring.lock() {
                ring.finish_segment_with_audio_duration(
                    recorded.video_track.duration_90k,
                    recorded
                        .audio_track
                        .as_ref()
                        .map(|track| (track.duration_ticks, track.sample_rate)),
                );
            }
        } else if let Ok(mut ring) = self.ring.lock() {
            ring.push_tracks(&recorded.video_track, recorded.audio_track.as_ref());
        }
    }
}

impl super::vpl::VplOneCopyRecordSink for SessionRingSink {
    fn status(&mut self, message: &str) {
        self.send_status(message);
    }

    fn video_track_started(&mut self, info: super::vpl::VplOutputTrackInfo) {
        self.status("oneVPL 编码器已 Init，encoded ring 元数据已建立。");
        if let Ok(mut ring) = self.ring.lock() {
            ring.start_segment(EncodedReplayMetadata {
                width: info.width,
                height: info.height,
                color: info.color,
                codec: info.codec,
                audio_sample_rate: crate::backend::audio::TARGET_SAMPLE_RATE,
                audio_channel_count: crate::backend::audio::TARGET_CHANNELS,
            });
            self.started = true;
        }
    }

    fn hevc_access_unit(&mut self, sample: &super::mp4_mux::HevcAccessUnit) {
        if self.started
            && let Ok(mut ring) = self.ring.lock()
        {
            ring.push_video_au_90k(sample);
        }
    }

    fn aac_access_unit(&mut self, sample: &super::mp4_mux::AacAccessUnit) {
        if self.started
            && let Ok(mut ring) = self.ring.lock()
        {
            ring.push_audio_au_ticks(sample, crate::backend::audio::TARGET_SAMPLE_RATE);
        }
    }
}

impl SessionRingSink {
    pub(super) fn send_status(&mut self, message: &str) {
        let _ = self.tx.send(ReplayEvent::BackendStatus {
            index: self.segment_index,
            message: message.to_owned(),
        });
    }
}
