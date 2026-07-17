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
    pub(super) disk_live_ring: Option<Arc<Mutex<EncodedReplayRing>>>,
    pub(super) disk_live_run_index: Option<Arc<AtomicU64>>,
    pub(super) memory_save_after_ns: Option<u64>,
    pub(super) memory_save_codec_epoch: Option<u64>,
    pub(super) disk_save_after: Option<DiskSaveCursor>,
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
            disk_live_ring: None,
            disk_live_run_index: None,
            memory_save_after_ns: None,
            memory_save_codec_epoch: None,
            disk_save_after: None,
        }
    }
}

impl Drop for ReplayController {
    fn drop(&mut self) {
        if let Some(stop) = &self.stop_flag {
            stop.store(true, Ordering::Relaxed);
        }
        if let Some(worker) = self.worker.take() {
            let _ = worker.join();
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
            let video_available = availability.parameter_sets_ready
                && (availability.video_key_packets > 0 || availability.pending_video_key);
            if video_available && availability.audio_packets > 0 {
                return ReplaySaveReadiness::Ready;
            }
            if video_available {
                return ReplaySaveReadiness::WaitingForAudio;
            }
            return ReplaySaveReadiness::Starting;
        }
        if let Some(store) = &self.disk_store {
            let Ok(store) = store.lock() else {
                return ReplaySaveReadiness::Starting;
            };
            if store.has_saveable_segments() {
                return ReplaySaveReadiness::Ready;
            }
        }
        if let Some(ring) = &self.disk_live_ring {
            let Ok(ring) = ring.lock() else {
                return ReplaySaveReadiness::Starting;
            };
            let availability = ring.availability();
            let video_available = availability.parameter_sets_ready
                && (availability.video_key_packets > 0 || availability.pending_video_key);
            if video_available && availability.audio_packets > 0 {
                ReplaySaveReadiness::Ready
            } else if video_available {
                ReplaySaveReadiness::WaitingForAudio
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
                        self.disk_live_ring = None;
                        self.disk_live_run_index = None;
                        terminal = true;
                    }
                    Ok(ReplayEvent::Stopped) => {
                        out.push("后台即时回放线程已停止。".to_owned());
                        self.state = ReplayState::Idle;
                        self.stop_flag = None;
                        self.live_ring = None;
                        self.disk_store = None;
                        self.disk_live_ring = None;
                        self.disk_live_run_index = None;
                        terminal = true;
                    }
                    Err(mpsc::TryRecvError::Empty) => break,
                    Err(mpsc::TryRecvError::Disconnected) => {
                        out.push("后台即时回放事件通道已断开。".to_owned());
                        self.state = ReplayState::Idle;
                        self.stop_flag = None;
                        self.live_ring = None;
                        self.disk_store = None;
                        self.disk_live_ring = None;
                        self.disk_live_run_index = None;
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

        let capture_mode = caps.effective_capture_mode(config.capture_mode);
        let chroma = validate_config(config, caps, capture_mode)?;
        let save_dir = PathBuf::from(&config.save_dir);
        fs::create_dir_all(&save_dir).map_err(|err| BackendError::Io(err.to_string()))?;
        cleanup_stale_replay_parts(&save_dir)?;
        let replay_duration =
            Duration::from_secs_f32((config.replay_minutes.max(0.1) * 60.0).max(1.0));
        let buffer_mode = config.replay_buffer_mode;
        let cache_dir = if buffer_mode.is_disk() {
            let cache_dir = PathBuf::from(&config.cache_dir);
            DiskReplayStore::prepare_directory(&cache_dir)?;
            Some(cache_dir)
        } else {
            None
        };

        let request = pipeline::RecordingRequest {
            capture_mode,
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
        let disk_live_ring = buffer_mode.is_disk().then(|| {
            Arc::new(Mutex::new(EncodedReplayRing::new(
                replay_duration + Duration::from_secs(5),
            )))
        });
        let disk_live_run_index = buffer_mode
            .is_disk()
            .then(|| Arc::new(AtomicU64::new(u64::MAX)));
        let worker_ring = live_ring.clone();
        let worker_disk_store = disk_store.clone();
        let worker_disk_live_ring = disk_live_ring.clone();
        let worker_disk_live_run_index = disk_live_run_index.clone();
        let worker = thread::Builder::new()
            .name("rustreplay-recording-worker".to_owned())
            .spawn(move || {
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
                    worker_disk_live_ring,
                    worker_disk_live_run_index,
                );
            })
            .map_err(|err| BackendError::Io(format!("创建录制工作线程失败：{err}")))?;

        self.latest_clip = None;
        self.stop_flag = Some(stop_flag);
        self.worker = Some(worker);
        self.event_rx = Some(rx);
        self.live_ring = live_ring;
        self.disk_store = disk_store;
        self.disk_live_ring = disk_live_ring;
        self.disk_live_run_index = disk_live_run_index;
        self.memory_save_after_ns = None;
        self.memory_save_codec_epoch = None;
        self.disk_save_after = None;
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
            let ring = ring
                .lock()
                .map_err(|_| BackendError::Io("encoded ring 锁已中毒".to_owned()))?;
            let not_before_pts_ns = (self.memory_save_codec_epoch == Some(ring.codec_epoch()))
                .then_some(self.memory_save_after_ns)
                .flatten();
            let packet_snapshot =
                ring.snapshot_recent_packets_after(replay_duration, not_before_pts_ns);
            drop(ring);
            if let Some(packet_snapshot) = packet_snapshot {
                let save_codec_epoch = packet_snapshot.codec_epoch();
                let save_after_ns = packet_snapshot.save_cursor_pts_ns();
                // Materializing packet bytes is intentionally outside the ring mutex so capture
                // and NVENC can continue pushing access units while a replay is being saved.
                let snapshot = packet_snapshot.into_tracks();
                if snapshot.audio_track.is_some() {
                    write_replay_output_atomically(&dst, |path| {
                        super::mp4_mux::write_hevc_aac_mp4(
                            path,
                            &snapshot.video_track,
                            snapshot.audio_track.as_ref(),
                        )
                    })?;
                    self.memory_save_codec_epoch = Some(save_codec_epoch);
                    self.memory_save_after_ns = Some(save_after_ns);
                    return Ok(());
                }
                return Err(BackendError::unsupported(
                    "保存即时回放",
                    "内存 encoded ring AAC",
                    "视频已进入 encoded ring，但本次新时间窗口内尚无音频 access unit；请稍后重试",
                ));
            } else if self.memory_save_after_ns.is_some() {
                return Err(BackendError::unsupported(
                    "保存即时回放",
                    "上次成功保存后的 encoded ring",
                    "尚无新的关键帧和音频可保存；请稍后重试",
                ));
            }
        }

        if let Some(store) = &self.disk_store {
            let snapshot_with_cursor = store
                .lock()
                .map_err(|_| BackendError::Io("磁盘循环缓存锁已中毒".to_owned()))?
                .snapshot_recent_tracks_after(replay_duration, self.disk_save_after)?;
            if let Some((snapshot, last_cursor)) = snapshot_with_cursor {
                if snapshot.audio_track.is_some() {
                    write_replay_output_atomically(&dst, |path| {
                        super::mp4_mux::write_prepared_hevc_aac_mp4(
                            path,
                            &snapshot.video_track,
                            snapshot.audio_track.as_ref(),
                        )
                    })?;
                    self.disk_save_after = Some(last_cursor);
                    return Ok(());
                }
                return Err(BackendError::unsupported(
                    "保存即时回放",
                    "磁盘循环缓存 AAC",
                    "磁盘分段中没有可封装的音频 access unit",
                ));
            }
        }

        if self.try_save_open_disk_segment(&dst, replay_duration)? {
            return Ok(());
        }
        if self.disk_save_after.is_some() {
            return Err(BackendError::unsupported(
                "保存即时回放",
                "上次成功保存后的磁盘循环时间线",
                "尚无新的完整分段或开放分段可保存；请稍后重试",
            ));
        }

        if self.memory_save_after_ns.is_some() || self.disk_save_after.is_some() {
            return Err(BackendError::unsupported(
                "保存即时回放",
                "上次成功保存后的时间窗口",
                "尚无新内容可保存，不会重复写入首次保存前的画面",
            ));
        }

        let Some(clip) = &self.latest_clip else {
            return Err(BackendError::unsupported(
                "保存即时回放",
                "内存 encoded ring",
                "后台录制尚未产生可保存的已编码 HEVC/AAC access unit",
            ));
        };
        write_replay_output_atomically(&dst, |path| {
            super::mp4_mux::write_hevc_aac_mp4(path, &clip.video_track, clip.audio_track.as_ref())
        })?;
        Ok(())
    }

    fn try_save_open_disk_segment(
        &mut self,
        dst: &Path,
        replay_duration: Duration,
    ) -> Result<bool, BackendError> {
        let Some(ring) = &self.disk_live_ring else {
            return Ok(false);
        };
        let Some(live_run_index) = &self.disk_live_run_index else {
            return Ok(false);
        };
        let ring = ring
            .lock()
            .map_err(|_| BackendError::Io("开放磁盘分段 ring 锁已中毒".to_owned()))?;
        let run_index = live_run_index.load(Ordering::Acquire);
        if run_index == u64::MAX {
            return Ok(false);
        }
        let not_before_pts_ns = match self.disk_save_after {
            Some(cursor) if cursor.run_index == run_index => Some(cursor.source_pts_ns),
            Some(cursor) if cursor.run_index > run_index => return Ok(false),
            _ => None,
        };
        let packet_snapshot =
            ring.snapshot_recent_packets_after(replay_duration, not_before_pts_ns);
        drop(ring);
        let Some(packet_snapshot) = packet_snapshot else {
            return Ok(false);
        };
        let save_after_ns = packet_snapshot.save_cursor_pts_ns();
        let snapshot = packet_snapshot.into_tracks();
        if snapshot.audio_track.is_none() {
            return Ok(false);
        }
        write_replay_output_atomically(dst, |path| {
            super::mp4_mux::write_hevc_aac_mp4(
                path,
                &snapshot.video_track,
                snapshot.audio_track.as_ref(),
            )
        })?;
        self.disk_save_after = Some(DiskSaveCursor {
            run_index,
            source_pts_ns: save_after_ns,
        });
        Ok(true)
    }
}

fn write_replay_output_atomically(
    dst: &Path,
    write: impl FnOnce(&Path) -> Result<(), BackendError>,
) -> Result<(), BackendError> {
    let part = dst.with_extension("mp4.part");
    let _ = fs::remove_file(&part);
    let result = write(&part)
        .and_then(|()| fs::rename(&part, dst).map_err(|err| BackendError::Io(err.to_string())));
    if result.is_err() {
        let _ = fs::remove_file(&part);
    }
    result
}

fn cleanup_stale_replay_parts(save_dir: &Path) -> Result<(), BackendError> {
    for entry in fs::read_dir(save_dir).map_err(|err| BackendError::Io(err.to_string()))? {
        let entry = entry.map_err(|err| BackendError::Io(err.to_string()))?;
        let name = entry.file_name();
        let name = name.to_string_lossy();
        if name.starts_with("RustReplay_") && name.ends_with(".mp4.part") {
            fs::remove_file(entry.path()).map_err(|err| {
                BackendError::Io(format!(
                    "清理残留即时回放临时文件 {} 失败：{err}",
                    entry.path().display()
                ))
            })?;
        }
    }
    Ok(())
}

pub(super) fn validate_config(
    config: &AppConfig,
    caps: &ProbeCaps,
    capture_mode: CaptureMode,
) -> Result<ChromaSampling, BackendError> {
    let chroma = config.chroma.ok_or_else(|| {
        BackendError::unsupported("GUI 参数", "色度采样", "没有已验证的色度采样可选项")
    })?;

    if !caps
        .supported_chroma_for_capture_mode(capture_mode)
        .contains(&chroma)
    {
        return Err(BackendError::unsupported(
            "创建录制会话",
            format!("色度采样 {}", chroma.doc_label()),
            format!(
                "{}后端未报告该色度采样存在完整生产路径，GUI 正常情况下会隐藏它",
                capture_mode.label()
            ),
        ));
    }

    caps.validate_rate_control_config_for_capture_mode(
        capture_mode,
        chroma,
        &config.rate_control,
        "创建录制会话",
    )?;

    if capture_mode == CaptureMode::Generic && !caps.desktop_sync_path_available {
        return Err(BackendError::unsupported(
            "创建录制会话",
            "当前显示器状态 + 捕获 + GPU 转换 + HEVC 硬编完整路径",
            caps.path_blockers
                .first()
                .cloned()
                .unwrap_or_else(|| "没有可用桌面同步录制路径".to_owned()),
        ));
    }
    if capture_mode == CaptureMode::DedicatedNvFbc && !caps.nvfbc_usable() {
        return Err(BackendError::unsupported(
            "创建录制会话",
            "NvFBC 专用捕获",
            "本次启动未探测到可用 NvFBC route；应回退通用捕获且隐藏专用切换按钮",
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
