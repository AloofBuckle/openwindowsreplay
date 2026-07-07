#![allow(dead_code)]
//! 即时回放会话状态机。
//!
//! GUI 会话接到 oneVPL/D3D11 GPU-only 录制后端：后台线程连续产出已编码
//! HEVC/AAC access units，写入内存环形缓存；保存按钮只把环形缓存快照 mux 成 MP4，
//! 不重编码、不复制 raw frame，也不绕开生产编码路径。

use super::{ProbeCaps, pipeline};
use crate::backend::mp4_mux::{
    AacAccessUnit, AacLcMp4Track, HevcAccessUnit, HevcCodecMetadata, HevcMp4Track,
    NclxColorMetadata,
};
use crate::config::{AppConfig, CaptureBackend, ChromaSampling, ReplayBufferMode};
use crate::error::BackendError;
use crate::ring::{EncodedReplayMetadata, EncodedReplayRing};
use std::collections::VecDeque;
use std::fs;
use std::io::{Read, Write};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc::{self, Receiver, Sender};
use std::sync::{Arc, Mutex};
use std::thread::{self, JoinHandle};
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

const LIVE_RECORD_SECONDS: f32 = 24.0 * 60.0 * 60.0;
const DISK_SEGMENT_TARGET_SECONDS: f32 = 10.0;
const DISK_SEGMENT_SIDECAR_MAGIC: &[u8; 8] = b"RRSEG001";
const VIDEO_CLOCK_HZ: u64 = 90_000;

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
struct CompletedClip {
    cache_path: PathBuf,
    captured_frames: u32,
    encoded_samples: u32,
    audio_access_units: u32,
    duration_seconds: f32,
    video_track: super::mp4_mux::HevcMp4Track,
    audio_track: Option<super::mp4_mux::AacLcMp4Track>,
    ring_packets: usize,
    ring_bytes: usize,
}

#[derive(Debug)]
enum ReplayEvent {
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
    state: ReplayState,
    stop_flag: Option<Arc<AtomicBool>>,
    worker: Option<JoinHandle<()>>,
    event_rx: Option<Receiver<ReplayEvent>>,
    latest_clip: Option<CompletedClip>,
    live_ring: Option<Arc<Mutex<EncodedReplayRing>>>,
    disk_store: Option<Arc<Mutex<DiskReplayStore>>>,
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
                    super::mp4_mux::write_hevc_aac_mp4(
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

fn validate_config(config: &AppConfig, caps: &ProbeCaps) -> Result<ChromaSampling, BackendError> {
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

fn chroma_writer_for_chroma(chroma: ChromaSampling) -> pipeline::ChromaWriterKind {
    match chroma {
        ChromaSampling::Yuv420 => pipeline::ChromaWriterKind::Auto420,
        ChromaSampling::Yuv422 => pipeline::ChromaWriterKind::Auto422,
        ChromaSampling::Yuv444 => pipeline::ChromaWriterKind::Auto444,
    }
}

fn capture_backend_for_config(backend: CaptureBackend) -> pipeline::CaptureBackendKind {
    match backend {
        CaptureBackend::Dda => pipeline::CaptureBackendKind::Dda,
        CaptureBackend::Wgc => pipeline::CaptureBackendKind::Wgc,
    }
}

#[allow(clippy::too_many_arguments)]
fn run_recording_worker(
    request: pipeline::RecordingRequest,
    caps: ProbeCaps,
    save_dir: PathBuf,
    cache_dir: Option<PathBuf>,
    replay_duration: Duration,
    buffer_mode: ReplayBufferMode,
    stop_flag: Arc<AtomicBool>,
    tx: Sender<ReplayEvent>,
    encoded_ring: Option<Arc<Mutex<EncodedReplayRing>>>,
    disk_store: Option<Arc<Mutex<DiskReplayStore>>>,
) {
    let start_path = cache_dir.clone().unwrap_or_else(|| save_dir.clone());
    let _ = tx.send(ReplayEvent::RecordingStarted {
        mode: buffer_mode,
        path: start_path.clone(),
    });
    let mut run_index = 0u64;
    while !stop_flag.load(Ordering::Relaxed) {
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
                    let Some(store) = disk_store.clone() else {
                        let _ = tx.send(ReplayEvent::Error(
                            "磁盘循环模式缺少 segment store".to_owned(),
                        ));
                        return;
                    };
                    ReplayRecordSink::Disk(DiskSegmentSink::new(
                        store,
                        tx.clone(),
                        run_index,
                        replay_duration,
                    ))
                }
            };
            let result = pipeline::record_once_gpu_only_memory_output_with_sink_cancelable(
                &request,
                &caps,
                &output,
                LIVE_RECORD_SECONDS,
                0,
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
                if stop_flag.load(Ordering::Relaxed) {
                    break;
                }
                let _ = tx.send(ReplayEvent::Error(err.to_string()));
                return;
            }
        }
        run_index = run_index.saturating_add(1);
    }
    let _ = tx.send(ReplayEvent::Stopped);
}

enum ReplayRecordSink {
    Memory(SessionRingSink),
    Disk(DiskSegmentSink),
}

impl ReplayRecordSink {
    fn finish_success(&mut self, recorded: &super::vpl::VplOneCopyRecordOutput) {
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

struct SessionRingSink {
    ring: Arc<Mutex<EncodedReplayRing>>,
    tx: Sender<ReplayEvent>,
    segment_index: u64,
    started: bool,
}

impl SessionRingSink {
    fn finish_success(&mut self, recorded: &super::vpl::VplOneCopyRecordOutput) {
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
    fn send_status(&mut self, message: &str) {
        let _ = self.tx.send(ReplayEvent::BackendStatus {
            index: self.segment_index,
            message: message.to_owned(),
        });
    }
}

#[derive(Debug, Clone)]
struct DiskSegmentMeta {
    index: u64,
    mp4_path: PathBuf,
    sidecar_path: PathBuf,
    duration_90k: u64,
    audio_access_units: usize,
    bytes: u64,
}

impl DiskSegmentMeta {
    fn duration(&self) -> Duration {
        Duration::from_nanos(scale_90k_to_ns(self.duration_90k))
    }
}

#[derive(Debug)]
struct DiskReplayStore {
    dir: PathBuf,
    retention: Duration,
    segment_slop: Duration,
    segments: VecDeque<DiskSegmentMeta>,
    next_index: u64,
}

impl DiskReplayStore {
    fn new(dir: PathBuf, retention: Duration, segment_slop: Duration) -> Self {
        Self {
            dir,
            retention,
            segment_slop,
            segments: VecDeque::new(),
            next_index: 0,
        }
    }

    fn has_saveable_segments(&self) -> bool {
        self.segments
            .iter()
            .any(|segment| segment.audio_access_units > 0)
    }

    fn write_segment(
        &mut self,
        segment: DiskSegmentTracks,
    ) -> Result<DiskSegmentMeta, BackendError> {
        fs::create_dir_all(&self.dir).map_err(|err| BackendError::Io(err.to_string()))?;
        let index = self.next_index;
        self.next_index = self.next_index.saturating_add(1);
        let stem = format!("rustreplay_segment_{}_{}", timestamp_for_filename(), index);
        let mp4_path = self.dir.join(format!("{stem}.mp4"));
        let sidecar_path = self.dir.join(format!("{stem}.rrseg"));
        super::mp4_mux::write_hevc_aac_mp4(
            &mp4_path,
            &segment.video_track,
            segment.audio_track.as_ref(),
        )?;
        write_disk_segment_sidecar(&sidecar_path, &segment)?;
        let bytes = fs::metadata(&mp4_path).map(|meta| meta.len()).unwrap_or(0)
            + fs::metadata(&sidecar_path)
                .map(|meta| meta.len())
                .unwrap_or(0);
        let meta = DiskSegmentMeta {
            index,
            mp4_path,
            sidecar_path,
            duration_90k: segment.video_track.duration_90k,
            audio_access_units: segment
                .audio_track
                .as_ref()
                .map(|track| track.samples.len())
                .unwrap_or(0),
            bytes,
        };
        self.segments.push_back(meta.clone());
        self.prune_old_segments();
        Ok(meta)
    }

    fn snapshot_recent_tracks(
        &self,
        duration: Duration,
    ) -> Result<Option<crate::ring::EncodedReplaySnapshot>, BackendError> {
        let selected = self.select_recent_segments(duration);
        if selected.is_empty() {
            return Ok(None);
        }
        let mut segments = Vec::with_capacity(selected.len());
        for meta in selected {
            segments.push(read_disk_segment_sidecar(&meta.sidecar_path)?);
        }
        Ok(concat_disk_segments(&segments))
    }

    fn select_recent_segments(&self, duration: Duration) -> Vec<DiskSegmentMeta> {
        let target_ns = duration.as_nanos().min(u128::from(u64::MAX)) as u64;
        let mut selected = VecDeque::new();
        let mut accumulated_ns = 0u64;
        for segment in self.segments.iter().rev() {
            selected.push_front(segment.clone());
            accumulated_ns = accumulated_ns.saturating_add(scale_90k_to_ns(segment.duration_90k));
            if accumulated_ns >= target_ns {
                break;
            }
        }
        selected.into_iter().collect()
    }

    fn prune_old_segments(&mut self) {
        let keep_for = self.retention.saturating_add(self.segment_slop);
        let keep_ns = keep_for.as_nanos().min(u128::from(u64::MAX)) as u64;
        let mut accumulated_ns = 0u64;
        let mut keep_from = self.segments.len();
        for (idx, segment) in self.segments.iter().enumerate().rev() {
            accumulated_ns = accumulated_ns.saturating_add(scale_90k_to_ns(segment.duration_90k));
            keep_from = idx;
            if accumulated_ns >= keep_ns {
                break;
            }
        }
        for segment in self.segments.drain(..keep_from) {
            let _ = fs::remove_file(&segment.mp4_path);
            let _ = fs::remove_file(&segment.sidecar_path);
        }
    }
}

struct DiskSegmentSink {
    store: Arc<Mutex<DiskReplayStore>>,
    tx: Sender<ReplayEvent>,
    run_index: u64,
    metadata: Option<EncodedReplayMetadata>,
    header_units: Vec<HevcAccessUnit>,
    current: Option<DiskSegmentBuilder>,
    pending: VecDeque<DiskSegmentBuilder>,
    target_duration_90k: u64,
    latest_audio_ticks: u64,
}

impl DiskSegmentSink {
    fn new(
        store: Arc<Mutex<DiskReplayStore>>,
        tx: Sender<ReplayEvent>,
        run_index: u64,
        replay_duration: Duration,
    ) -> Self {
        let target_seconds = DISK_SEGMENT_TARGET_SECONDS
            .min((replay_duration.as_secs_f32() / 2.0).max(1.0))
            .max(1.0);
        Self {
            store,
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

    fn finish_success(&mut self, recorded: &super::vpl::VplOneCopyRecordOutput) {
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

    fn status(&mut self, message: &str) {
        self.send_status(message);
    }

    fn video_track_started(&mut self, info: super::vpl::VplOutputTrackInfo) {
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

    fn hevc_access_unit(&mut self, sample: &HevcAccessUnit) {
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

    fn aac_access_unit(&mut self, sample: &AacAccessUnit) {
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

    fn remember_parameter_sets(&mut self, sample: &HevcAccessUnit) {
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
            .any(|header| header.data.as_slice() == data.as_slice())
        {
            return;
        }
        self.header_units.push(HevcAccessUnit {
            timestamp_90k: 0,
            data,
            is_sync: false,
            discard_from_track: true,
        });
    }

    fn flush_ready_pending(&mut self) {
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

    fn write_completed_segment(&mut self, segment: DiskSegmentBuilder) {
        let Some(segment) = segment.into_tracks(&self.header_units) else {
            return;
        };
        match self
            .store
            .lock()
            .map_err(|_| BackendError::Io("磁盘循环缓存锁已中毒".to_owned()))
            .and_then(|mut store| store.write_segment(segment))
        {
            Ok(meta) => self.send_status(&format!(
                "磁盘循环分段已写入 #{}：{}，duration={:.3}s，audio_au={}，bytes={}",
                meta.index,
                meta.mp4_path.display(),
                meta.duration().as_secs_f64(),
                meta.audio_access_units,
                meta.bytes
            )),
            Err(err) => self.send_status(&format!("磁盘循环分段写入失败：{err}")),
        }
    }

    fn send_status(&mut self, message: &str) {
        let _ = self.tx.send(ReplayEvent::BackendStatus {
            index: self.run_index,
            message: message.to_owned(),
        });
    }
}

#[derive(Debug, Clone)]
struct DiskSegmentBuilder {
    metadata: EncodedReplayMetadata,
    start_90k: u64,
    start_audio_ticks: u64,
    end_90k: Option<u64>,
    video_samples: Vec<HevcAccessUnit>,
    audio_samples: Vec<AacAccessUnit>,
}

impl DiskSegmentBuilder {
    fn new(metadata: EncodedReplayMetadata, start_90k: u64, start_audio_ticks: u64) -> Self {
        Self {
            metadata,
            start_90k,
            start_audio_ticks,
            end_90k: None,
            video_samples: Vec::new(),
            audio_samples: Vec::new(),
        }
    }

    fn has_video(&self) -> bool {
        self.video_samples
            .iter()
            .any(|sample| !sample.discard_from_track)
    }

    fn push_video(&mut self, sample: &HevcAccessUnit) {
        let mut sample = sample.clone();
        sample.timestamp_90k = sample.timestamp_90k.saturating_sub(self.start_90k);
        self.video_samples.push(sample);
    }

    fn push_audio(&mut self, sample: &AacAccessUnit) {
        let mut sample = sample.clone();
        sample.timestamp_ticks = sample
            .timestamp_ticks
            .saturating_sub(self.start_audio_ticks);
        self.audio_samples.push(sample);
    }

    fn end_audio_ticks(&self) -> Option<u64> {
        self.end_90k.map(|end| {
            scale_90k_to_ticks(end, self.metadata.audio_sample_rate).max(self.start_audio_ticks)
        })
    }

    fn contains_audio_timestamp(&self, timestamp_ticks: u64) -> bool {
        timestamp_ticks >= self.start_audio_ticks
            && self
                .end_audio_ticks()
                .is_some_and(|end_ticks| timestamp_ticks < end_ticks)
    }

    fn into_tracks(self, header_units: &[HevcAccessUnit]) -> Option<DiskSegmentTracks> {
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
struct DiskSegmentTracks {
    video_track: HevcMp4Track,
    audio_track: Option<AacLcMp4Track>,
}

fn concat_disk_segments(
    segments: &[DiskSegmentTracks],
) -> Option<crate::ring::EncodedReplaySnapshot> {
    let first = segments.first()?;
    let mut video_samples = Vec::new();
    let mut audio_samples = Vec::new();
    let mut video_base_90k = 0u64;
    let mut audio_base_ticks = 0u64;
    let mut audio_sample_rate = 48_000;
    let mut audio_channel_count = 2;
    for segment in segments {
        video_samples.extend(
            segment
                .video_track
                .samples
                .iter()
                .cloned()
                .map(|mut sample| {
                    sample.timestamp_90k = video_base_90k.saturating_add(sample.timestamp_90k);
                    sample
                }),
        );
        video_base_90k = video_base_90k.saturating_add(segment.video_track.duration_90k);
        if let Some(audio) = &segment.audio_track {
            audio_sample_rate = audio.sample_rate;
            audio_channel_count = audio.channel_count;
            audio_samples.extend(audio.samples.iter().cloned().map(|mut sample| {
                sample.timestamp_ticks = audio_base_ticks.saturating_add(sample.timestamp_ticks);
                sample
            }));
            audio_base_ticks = audio_base_ticks.saturating_add(audio.duration_ticks);
        } else {
            audio_base_ticks = audio_base_ticks.saturating_add(scale_90k_to_ticks(
                segment.video_track.duration_90k,
                audio_sample_rate,
            ));
        }
    }
    if video_samples.iter().all(|sample| sample.discard_from_track) {
        return None;
    }
    let audio_track = if audio_samples.is_empty() {
        None
    } else {
        let sample_duration_ticks = audio_samples
            .iter()
            .map(|sample| {
                sample
                    .timestamp_ticks
                    .saturating_add(u64::from(sample.duration_ticks))
            })
            .max()
            .unwrap_or(1)
            .max(1);
        Some(AacLcMp4Track {
            sample_rate: audio_sample_rate,
            channel_count: audio_channel_count,
            duration_ticks: sample_duration_ticks.max(audio_base_ticks).max(1),
            samples: audio_samples,
        })
    };
    Some(crate::ring::EncodedReplaySnapshot {
        video_track: HevcMp4Track {
            width: first.video_track.width,
            height: first.video_track.height,
            duration_90k: video_base_90k.max(1),
            color: first.video_track.color,
            codec: first.video_track.codec,
            samples: video_samples,
        },
        audio_track,
    })
}

fn write_disk_segment_sidecar(
    path: &Path,
    segment: &DiskSegmentTracks,
) -> Result<(), BackendError> {
    if let Some(parent) = path.parent()
        && !parent.as_os_str().is_empty()
    {
        fs::create_dir_all(parent).map_err(|err| BackendError::Io(err.to_string()))?;
    }
    let mut file = fs::File::create(path).map_err(|err| BackendError::Io(err.to_string()))?;
    file.write_all(DISK_SEGMENT_SIDECAR_MAGIC)
        .map_err(|err| BackendError::Io(err.to_string()))?;
    write_u16(&mut file, segment.video_track.width)?;
    write_u16(&mut file, segment.video_track.height)?;
    write_color(&mut file, segment.video_track.color)?;
    write_codec(&mut file, segment.video_track.codec)?;
    write_u64(&mut file, segment.video_track.duration_90k)?;
    match &segment.audio_track {
        Some(audio) => {
            write_u8(&mut file, 1)?;
            write_u32(&mut file, audio.sample_rate)?;
            write_u16(&mut file, audio.channel_count)?;
            write_u64(&mut file, audio.duration_ticks)?;
            write_u64(&mut file, audio.samples.len() as u64)?;
            for sample in &audio.samples {
                write_u64(&mut file, sample.timestamp_ticks)?;
                write_u32(&mut file, sample.duration_ticks)?;
                write_bytes(&mut file, &sample.data)?;
            }
        }
        None => write_u8(&mut file, 0)?,
    }
    write_u64(&mut file, segment.video_track.samples.len() as u64)?;
    for sample in &segment.video_track.samples {
        write_u64(&mut file, sample.timestamp_90k)?;
        write_u8(&mut file, u8::from(sample.is_sync))?;
        write_u8(&mut file, u8::from(sample.discard_from_track))?;
        write_bytes(&mut file, &sample.data)?;
    }
    file.flush()
        .map_err(|err| BackendError::Io(err.to_string()))
}

fn read_disk_segment_sidecar(path: &Path) -> Result<DiskSegmentTracks, BackendError> {
    let mut file = fs::File::open(path).map_err(|err| BackendError::Io(err.to_string()))?;
    let mut magic = [0u8; 8];
    file.read_exact(&mut magic)
        .map_err(|err| BackendError::Io(err.to_string()))?;
    if &magic != DISK_SEGMENT_SIDECAR_MAGIC {
        return Err(BackendError::unsupported(
            "磁盘循环缓存",
            path.display().to_string(),
            "sidecar 文件头不匹配",
        ));
    }
    let width = read_u16(&mut file)?;
    let height = read_u16(&mut file)?;
    let color = read_color(&mut file)?;
    let codec = read_codec(&mut file)?;
    let duration_90k = read_u64(&mut file)?;
    let audio_track = if read_u8(&mut file)? != 0 {
        let sample_rate = read_u32(&mut file)?;
        let channel_count = read_u16(&mut file)?;
        let duration_ticks = read_u64(&mut file)?;
        let sample_count = read_len(&mut file)?;
        let mut samples = Vec::with_capacity(sample_count);
        for _ in 0..sample_count {
            samples.push(AacAccessUnit {
                timestamp_ticks: read_u64(&mut file)?,
                duration_ticks: read_u32(&mut file)?,
                data: read_bytes(&mut file)?,
            });
        }
        Some(AacLcMp4Track {
            sample_rate,
            channel_count,
            duration_ticks,
            samples,
        })
    } else {
        None
    };
    let sample_count = read_len(&mut file)?;
    let mut samples = Vec::with_capacity(sample_count);
    for _ in 0..sample_count {
        samples.push(HevcAccessUnit {
            timestamp_90k: read_u64(&mut file)?,
            is_sync: read_u8(&mut file)? != 0,
            discard_from_track: read_u8(&mut file)? != 0,
            data: read_bytes(&mut file)?,
        });
    }
    Ok(DiskSegmentTracks {
        video_track: HevcMp4Track {
            width,
            height,
            duration_90k,
            color,
            codec,
            samples,
        },
        audio_track,
    })
}

fn write_color(out: &mut impl Write, color: NclxColorMetadata) -> Result<(), BackendError> {
    write_u16(out, color.colour_primaries)?;
    write_u16(out, color.transfer_characteristics)?;
    write_u16(out, color.matrix_coefficients)?;
    write_u8(out, u8::from(color.full_range))
}

fn read_color(input: &mut impl Read) -> Result<NclxColorMetadata, BackendError> {
    Ok(NclxColorMetadata {
        colour_primaries: read_u16(input)?,
        transfer_characteristics: read_u16(input)?,
        matrix_coefficients: read_u16(input)?,
        full_range: read_u8(input)? != 0,
    })
}

fn write_codec(out: &mut impl Write, codec: HevcCodecMetadata) -> Result<(), BackendError> {
    write_u8(out, codec.profile_idc)?;
    write_u8(out, codec.chroma_format_idc)?;
    write_u8(out, codec.bit_depth_luma_minus8)?;
    write_u8(out, codec.bit_depth_chroma_minus8)
}

fn read_codec(input: &mut impl Read) -> Result<HevcCodecMetadata, BackendError> {
    Ok(HevcCodecMetadata {
        profile_idc: read_u8(input)?,
        chroma_format_idc: read_u8(input)?,
        bit_depth_luma_minus8: read_u8(input)?,
        bit_depth_chroma_minus8: read_u8(input)?,
    })
}

fn write_bytes(out: &mut impl Write, bytes: &[u8]) -> Result<(), BackendError> {
    write_u64(out, bytes.len() as u64)?;
    out.write_all(bytes)
        .map_err(|err| BackendError::Io(err.to_string()))
}

fn read_bytes(input: &mut impl Read) -> Result<Vec<u8>, BackendError> {
    let len = read_len(input)?;
    let mut out = vec![0u8; len];
    input
        .read_exact(&mut out)
        .map_err(|err| BackendError::Io(err.to_string()))?;
    Ok(out)
}

fn read_len(input: &mut impl Read) -> Result<usize, BackendError> {
    usize::try_from(read_u64(input)?).map_err(|_| {
        BackendError::unsupported("磁盘循环缓存", "sidecar 长度字段", "长度超过当前平台 usize")
    })
}

fn write_u8(out: &mut impl Write, value: u8) -> Result<(), BackendError> {
    out.write_all(&[value])
        .map_err(|err| BackendError::Io(err.to_string()))
}

fn read_u8(input: &mut impl Read) -> Result<u8, BackendError> {
    let mut buf = [0u8; 1];
    input
        .read_exact(&mut buf)
        .map_err(|err| BackendError::Io(err.to_string()))?;
    Ok(buf[0])
}

fn write_u16(out: &mut impl Write, value: u16) -> Result<(), BackendError> {
    out.write_all(&value.to_le_bytes())
        .map_err(|err| BackendError::Io(err.to_string()))
}

fn read_u16(input: &mut impl Read) -> Result<u16, BackendError> {
    let mut buf = [0u8; 2];
    input
        .read_exact(&mut buf)
        .map_err(|err| BackendError::Io(err.to_string()))?;
    Ok(u16::from_le_bytes(buf))
}

fn write_u32(out: &mut impl Write, value: u32) -> Result<(), BackendError> {
    out.write_all(&value.to_le_bytes())
        .map_err(|err| BackendError::Io(err.to_string()))
}

fn read_u32(input: &mut impl Read) -> Result<u32, BackendError> {
    let mut buf = [0u8; 4];
    input
        .read_exact(&mut buf)
        .map_err(|err| BackendError::Io(err.to_string()))?;
    Ok(u32::from_le_bytes(buf))
}

fn write_u64(out: &mut impl Write, value: u64) -> Result<(), BackendError> {
    out.write_all(&value.to_le_bytes())
        .map_err(|err| BackendError::Io(err.to_string()))
}

fn read_u64(input: &mut impl Read) -> Result<u64, BackendError> {
    let mut buf = [0u8; 8];
    input
        .read_exact(&mut buf)
        .map_err(|err| BackendError::Io(err.to_string()))?;
    Ok(u64::from_le_bytes(buf))
}

fn seconds_to_90k(seconds: f32) -> u64 {
    (f64::from(seconds.max(0.001)) * VIDEO_CLOCK_HZ as f64).round() as u64
}

fn scale_90k_to_ns(value: u64) -> u64 {
    ((u128::from(value) * 1_000_000_000u128).div_ceil(u128::from(VIDEO_CLOCK_HZ)))
        .min(u128::from(u64::MAX)) as u64
}

fn scale_90k_to_ticks(value: u64, sample_rate: u32) -> u64 {
    ((u128::from(value) * u128::from(sample_rate) + u128::from(VIDEO_CLOCK_HZ / 2))
        / u128::from(VIDEO_CLOCK_HZ))
    .min(u128::from(u64::MAX)) as u64
}

fn timestamp_for_filename() -> String {
    let secs = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or_default();
    secs.to_string()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn disk_segment_sidecar_roundtrips_tracks() {
        let dir = unique_temp_dir("sidecar_roundtrip");
        fs::create_dir_all(&dir).unwrap();
        let path = dir.join("segment.rrseg");
        let segment = segment(0, 90_000, 0, 48_000);

        write_disk_segment_sidecar(&path, &segment).unwrap();
        let read = read_disk_segment_sidecar(&path).unwrap();

        assert_eq!(read.video_track.width, segment.video_track.width);
        assert_eq!(read.video_track.duration_90k, 90_000);
        assert_eq!(read.video_track.samples.len(), 2);
        assert_eq!(
            read.audio_track.as_ref().unwrap().duration_ticks,
            segment.audio_track.as_ref().unwrap().duration_ticks
        );
        let _ = fs::remove_dir_all(dir);
    }

    #[test]
    fn disk_segment_concat_rebases_timestamps() {
        let first = segment(0, 90_000, 0, 48_000);
        let second = segment(0, 45_000, 0, 24_000);

        let snapshot = concat_disk_segments(&[first, second]).unwrap();

        assert_eq!(snapshot.video_track.duration_90k, 135_000);
        assert_eq!(snapshot.video_track.samples[0].timestamp_90k, 0);
        assert_eq!(snapshot.video_track.samples[2].timestamp_90k, 90_000);
        let audio = snapshot.audio_track.unwrap();
        assert_eq!(audio.duration_ticks, 72_000);
        assert_eq!(audio.samples[2].timestamp_ticks, 48_000);
    }

    #[test]
    fn disk_store_selects_whole_recent_segments() {
        let mut store = DiskReplayStore::new(
            unique_temp_dir("select_recent"),
            Duration::from_secs(25),
            Duration::from_secs(10),
        );
        for index in 0..5 {
            store.segments.push_back(DiskSegmentMeta {
                index,
                mp4_path: PathBuf::from(format!("{index}.mp4")),
                sidecar_path: PathBuf::from(format!("{index}.rrseg")),
                duration_90k: 10 * VIDEO_CLOCK_HZ,
                audio_access_units: 1,
                bytes: 1,
            });
        }

        let selected = store.select_recent_segments(Duration::from_secs(25));

        assert_eq!(
            selected
                .iter()
                .map(|segment| segment.index)
                .collect::<Vec<_>>(),
            vec![2, 3, 4]
        );
    }

    #[test]
    fn disk_segment_builder_keeps_audio_duration_aligned_to_video_segment() {
        let metadata = EncodedReplayMetadata {
            width: 16,
            height: 16,
            color: NclxColorMetadata::bt709_full(),
            codec: HevcCodecMetadata::main_420_8(),
            audio_sample_rate: 48_000,
            audio_channel_count: 2,
        };
        let mut builder = DiskSegmentBuilder::new(metadata, 90_000, 48_000);
        builder.end_90k = Some(180_000);
        builder.push_video(&HevcAccessUnit {
            timestamp_90k: 90_000,
            data: vec![0, 0, 1, 38, 1],
            is_sync: true,
            discard_from_track: false,
        });
        builder.push_audio(&AacAccessUnit {
            timestamp_ticks: 48_000,
            duration_ticks: 1024,
            data: vec![0x21, 0x10],
        });

        let segment = builder.into_tracks(&[]).unwrap();

        assert_eq!(segment.audio_track.unwrap().duration_ticks, 48_000);
    }

    fn segment(
        video_start_90k: u64,
        duration_90k: u64,
        audio_start_ticks: u64,
        audio_duration_ticks: u64,
    ) -> DiskSegmentTracks {
        DiskSegmentTracks {
            video_track: HevcMp4Track {
                width: 16,
                height: 16,
                duration_90k,
                color: NclxColorMetadata::bt709_full(),
                codec: HevcCodecMetadata::main_420_8(),
                samples: vec![
                    HevcAccessUnit {
                        timestamp_90k: video_start_90k,
                        data: vec![0, 0, 1, 32, 1],
                        is_sync: false,
                        discard_from_track: true,
                    },
                    HevcAccessUnit {
                        timestamp_90k: video_start_90k,
                        data: vec![0, 0, 1, 38, 1],
                        is_sync: true,
                        discard_from_track: false,
                    },
                ],
            },
            audio_track: Some(AacLcMp4Track {
                sample_rate: 48_000,
                channel_count: 2,
                duration_ticks: audio_duration_ticks,
                samples: vec![
                    AacAccessUnit {
                        timestamp_ticks: audio_start_ticks,
                        duration_ticks: 1024,
                        data: vec![0x21, 0x10],
                    },
                    AacAccessUnit {
                        timestamp_ticks: audio_start_ticks + audio_duration_ticks / 2,
                        duration_ticks: 1024,
                        data: vec![0x21, 0x10],
                    },
                ],
            }),
        }
    }

    fn unique_temp_dir(name: &str) -> PathBuf {
        std::env::temp_dir().join(format!(
            "rustreplay_{name}_{}",
            SystemTime::now()
                .duration_since(UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ))
    }
}
