use super::*;

#[derive(Debug, Clone)]
pub enum ReplayState {
    Idle,
    Running { started_at: Instant },
    Stopping { started_at: Instant },
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ReplaySaveReadiness {
    Idle,
    Starting,
    WaitingForAudio,
    Ready,
}

impl ReplaySaveReadiness {
    pub const fn can_save(self) -> bool {
        matches!(self, Self::Ready)
    }
}

#[derive(Debug, Clone)]
pub(super) struct CompletedClip {
    pub(super) cache_path: PathBuf,
    pub(super) captured_frames: u32,
    pub(super) encoded_samples: u32,
    pub(super) audio_access_units: u32,
    pub(super) duration_seconds: f32,
    pub(super) video_track: super::mp4_mux::HevcMp4Track,
    pub(super) audio_track: Option<super::mp4_mux::AacLcMp4Track>,
    pub(super) ring_packets: usize,
    pub(super) ring_bytes: usize,
}

#[derive(Debug)]
pub(super) enum ReplayEvent {
    RecordingStarted {
        mode: ReplayBufferMode,
        path: PathBuf,
    },
    BackendStatus {
        index: u64,
        message: String,
    },
    SegmentReady {
        index: u64,
        clip: CompletedClip,
    },
    Error(String),
    Stopped,
}

#[derive(Debug)]
pub struct ReplayController {
    pub(super) state: ReplayState,
    pub(super) stop_flag: Option<Arc<AtomicBool>>,
    pub(super) worker: Option<JoinHandle<()>>,
    pub(super) event_rx: Option<Receiver<ReplayEvent>>,
    pub(super) latest_clip: Option<CompletedClip>,
    pub(super) live_ring: Option<Arc<Mutex<EncodedReplayRing>>>,
    pub(super) disk_store: Option<Arc<Mutex<DiskReplayStore>>>,
}

impl Default for ReplayController {
    fn default() -> Self {
        Self {
            state: ReplayState::Idle,
            stop_flag: None,
            worker: None,
            event_rx: None,
            latest_clip: None,
            live_ring: None,
            disk_store: None,
        }
    }
}

impl ReplayController {
    pub fn state(&self) -> &ReplayState {
        &self.state
    }

    pub fn save_readiness(&self) -> ReplaySaveReadiness {
        if self.latest_clip.is_some() {
            return ReplaySaveReadiness::Ready;
        }
        if !matches!(
            self.state,
            ReplayState::Running { .. } | ReplayState::Stopping { .. }
        ) {
            return ReplaySaveReadiness::Idle;
        }
        if let Some(ring) = &self.live_ring {
            let Ok(ring) = ring.lock() else {
                return ReplaySaveReadiness::Starting;
            };
            let availability = ring.availability();
            if availability.video_packets > 0 && availability.audio_packets > 0 {
                return ReplaySaveReadiness::Ready;
            }
            if availability.video_packets > 0 || availability.pending_video_packet {
                return ReplaySaveReadiness::WaitingForAudio;
            }
            return ReplaySaveReadiness::Starting;
        }
        if let Some(store) = &self.disk_store {
            let Ok(store) = store.lock() else {
                return ReplaySaveReadiness::Starting;
            };
            if store.has_saveable_segments() {
                ReplaySaveReadiness::Ready
            } else {
                ReplaySaveReadiness::Starting
            }
        } else {
            ReplaySaveReadiness::Starting
        }
    }

    pub fn drain_log_messages(&mut self) -> Vec<String> {
        let mut out = Vec::new();
        let mut terminal = false;
        if let Some(rx) = self.event_rx.take() {
            loop {
                match rx.try_recv() {
                    Ok(ReplayEvent::RecordingStarted { mode, path }) => match mode {
                        ReplayBufferMode::Memory => out.push(format!(
                            "后台即时回放以内存循环模式开始；保存将直接写入输出目录：{}",
                            path.display()
                        )),
                        ReplayBufferMode::Disk => out.push(format!(
                            "后台即时回放以磁盘循环模式开始；分段缓存目录：{}",
                            path.display()
                        )),
                    },
                    Ok(ReplayEvent::BackendStatus { index, message }) => {
                        out.push(format!("后台即时回放片段 #{index}：{message}"));
                    }
                    Ok(ReplayEvent::SegmentReady { index, clip }) => {
                        out.push(format!(
                            "后台即时回放片段 #{index} 已完成：{}，frames={} video_au={} audio_au={} duration={:.3}s，encoded_ring packets={} bytes={}",
                            clip.cache_path.display(),
                            clip.captured_frames,
                            clip.encoded_samples,
                            clip.audio_access_units,
                            clip.duration_seconds,
                            clip.ring_packets,
                            clip.ring_bytes
                        ));
                        self.latest_clip = Some(clip);
                    }
                    Ok(ReplayEvent::Error(message)) => {
                        out.push(format!("后台即时回放录制失败：{message}"));
                        self.state = ReplayState::Idle;
                        self.stop_flag = None;
                        self.live_ring = None;
                        self.disk_store = None;
                        terminal = true;
                    }
                    Ok(ReplayEvent::Stopped) => {
                        out.push("后台即时回放线程已停止。".to_owned());
                        self.state = ReplayState::Idle;
                        self.stop_flag = None;
                        self.live_ring = None;
                        self.disk_store = None;
                        terminal = true;
                    }
                    Err(mpsc::TryRecvError::Empty) => break,
                    Err(mpsc::TryRecvError::Disconnected) => {
                        out.push("后台即时回放事件通道已断开。".to_owned());
                        self.state = ReplayState::Idle;
                        self.stop_flag = None;
                        self.live_ring = None;
                        self.disk_store = None;
                        terminal = true;
                        break;
                    }
                }
            }
            if !terminal {
                self.event_rx = Some(rx);
            }
        }
        if terminal {
            if let Some(worker) = self.worker.take() {
                let _ = worker.join();
            }
            self.event_rx = None;
        }
        out
    }

    pub fn start(&mut self, config: &AppConfig, caps: &ProbeCaps) -> Result<(), BackendError> {
        for _ in self.drain_log_messages() {}
        if !matches!(self.state, ReplayState::Idle) {
            return Err(BackendError::unsupported(
                "开始即时回放",
                "当前会话",
                "已有后台录制会话正在运行或停止中",
            ));
        }

        let chroma = validate_config(config, caps)?;
        let save_dir = PathBuf::from(&config.save_dir);
        fs::create_dir_all(&save_dir).map_err(|err| BackendError::Io(err.to_string()))?;
        let replay_duration =
            Duration::from_secs_f32((config.replay_minutes.max(0.1) * 60.0).max(1.0));
        let buffer_mode = config.replay_buffer_mode;
        let cache_dir = if buffer_mode.is_disk() {
            let cache_dir = PathBuf::from(&config.cache_dir);
            fs::create_dir_all(&cache_dir).map_err(|err| BackendError::Io(err.to_string()))?;
            Some(cache_dir)
        } else {
            None
        };

        let request = pipeline::RecordingRequest {
            capture_backend: capture_backend_for_config(config.capture_backend),
            color_transform: pipeline::ColorTransformKind::AutoFromDisplay,
            chroma_writer: chroma_writer_for_chroma(chroma),
            rate_control: config.rate_control.clone(),
            replay_minutes: config.replay_minutes,
        };
        let caps = caps.clone();
        let stop_flag = Arc::new(AtomicBool::new(false));
        let worker_stop = stop_flag.clone();
        let (tx, rx) = mpsc::channel();
        let live_ring = (buffer_mode == ReplayBufferMode::Memory).then(|| {
            Arc::new(Mutex::new(EncodedReplayRing::new(
                replay_duration + Duration::from_secs(5),
            )))
        });
        let disk_store = cache_dir.clone().map(|cache_dir| {
            Arc::new(Mutex::new(DiskReplayStore::new(
                cache_dir,
                replay_duration,
                Duration::from_secs_f32(DISK_SEGMENT_TARGET_SECONDS),
            )))
        });
        let worker_ring = live_ring.clone();
        let worker_disk_store = disk_store.clone();
        let worker = thread::spawn(move || {
            run_recording_worker(
                request,
                caps,
                save_dir,
                cache_dir,
                replay_duration,
                buffer_mode,
                worker_stop,
                tx,
                worker_ring,
                worker_disk_store,
            );
        });

        self.latest_clip = None;
        self.stop_flag = Some(stop_flag);
        self.worker = Some(worker);
        self.event_rx = Some(rx);
        self.live_ring = live_ring;
        self.disk_store = disk_store;
        self.state = ReplayState::Running {
            started_at: Instant::now(),
        };
        Ok(())
    }

    pub fn stop(&mut self) -> Result<(), BackendError> {
        self.drain_log_messages();
        match self.state {
            ReplayState::Idle => Err(BackendError::unsupported(
                "停止即时回放",
                "当前会话",
                "当前没有正在运行的录制会话",
            )),
            ReplayState::Running { started_at } | ReplayState::Stopping { started_at } => {
                if let Some(flag) = &self.stop_flag {
                    flag.store(true, Ordering::Relaxed);
                }
                self.state = ReplayState::Stopping { started_at };
                Ok(())
            }
        }
    }

    pub fn save(&mut self, config: &AppConfig) -> Result<(), BackendError> {
        self.drain_log_messages();
        let save_dir = PathBuf::from(&config.save_dir);
        fs::create_dir_all(&save_dir).map_err(|err| BackendError::Io(err.to_string()))?;
        let file_name = format!("RustReplay_{}.mp4", timestamp_for_filename());
        let dst = save_dir.join(file_name);

        let replay_duration =
            Duration::from_secs_f32((config.replay_minutes.max(0.1) * 60.0).max(1.0));
        if let Some(ring) = &self.live_ring {
            let snapshot = ring
                .lock()
                .map_err(|_| BackendError::Io("encoded ring 锁已中毒".to_owned()))?
                .snapshot_recent_tracks(replay_duration);
            if let Some(snapshot) = snapshot {
                if snapshot.audio_track.is_some() {
                    super::mp4_mux::write_hevc_aac_mp4(
                        &dst,
                        &snapshot.video_track,
                        snapshot.audio_track.as_ref(),
                    )?;
                    return Ok(());
                }
                if self.latest_clip.is_none() {
                    return Err(BackendError::unsupported(
                        "保存即时回放",
                        "内存 encoded ring AAC",
                        "视频已进入 encoded ring，但音频 access unit 尚未进入；请稍后重试或等待一个完整片段完成，避免静默保存 video-only 文件",
                    ));
                }
            }
        }

        if let Some(store) = &self.disk_store {
            let snapshot = store
                .lock()
                .map_err(|_| BackendError::Io("磁盘循环缓存锁已中毒".to_owned()))?
                .snapshot_recent_tracks(replay_duration)?;
            if let Some(snapshot) = snapshot {
                if snapshot.audio_track.is_some() {
                    super::mp4_mux::write_prepared_hevc_aac_mp4(
                        &dst,
                        &snapshot.video_track,
                        snapshot.audio_track.as_ref(),
                    )?;
                    return Ok(());
                }
                return Err(BackendError::unsupported(
                    "保存即时回放",
                    "磁盘循环缓存 AAC",
                    "磁盘分段中没有可封装的音频 access unit",
                ));
            }
        }

        let Some(clip) = &self.latest_clip else {
            return Err(BackendError::unsupported(
                "保存即时回放",
                "内存 encoded ring",
                "后台录制尚未产生可保存的已编码 HEVC/AAC access unit",
            ));
        };
        super::mp4_mux::write_hevc_aac_mp4(&dst, &clip.video_track, clip.audio_track.as_ref())?;
        Ok(())
    }
}

pub(super) fn validate_config(
    config: &AppConfig,
    caps: &ProbeCaps,
) -> Result<ChromaSampling, BackendError> {
    let chroma = config.chroma.ok_or_else(|| {
        BackendError::unsupported("GUI 参数", "色度采样", "没有已验证的色度采样可选项")
    })?;

    if !caps.supported_chroma.contains(&chroma) {
        return Err(BackendError::unsupported(
            "创建录制会话",
            format!("色度采样 {}", chroma.doc_label()),
            "后端未报告该色度采样存在完整 GPU-only 路径，GUI 正常情况下会隐藏它",
        ));
    }

    caps.validate_rate_control_config(chroma, &config.rate_control, "创建录制会话")?;

    if !caps.desktop_sync_path_available {
        return Err(BackendError::unsupported(
            "创建录制会话",
            "当前显示器状态 + 捕获 + GPU 转换 + HEVC 硬编完整路径",
            caps.path_blockers
                .first()
                .cloned()
                .unwrap_or_else(|| "没有可用桌面同步录制路径".to_owned()),
        ));
    }
    Ok(chroma)
}

pub(super) fn chroma_writer_for_chroma(chroma: ChromaSampling) -> pipeline::ChromaWriterKind {
    match chroma {
        ChromaSampling::Yuv420 => pipeline::ChromaWriterKind::Auto420,
        ChromaSampling::Yuv422 => pipeline::ChromaWriterKind::Auto422,
        ChromaSampling::Yuv444 => pipeline::ChromaWriterKind::Auto444,
    }
}

pub(super) fn capture_backend_for_config(backend: CaptureBackend) -> pipeline::CaptureBackendKind {
    match backend {
        CaptureBackend::Dda => pipeline::CaptureBackendKind::Dda,
        CaptureBackend::Wgc => pipeline::CaptureBackendKind::Wgc,
    }
}
