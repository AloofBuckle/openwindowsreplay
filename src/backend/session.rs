#![allow(dead_code)]
//! 即时回放会话状态机。
//!
//! GUI 会话接到 oneVPL/D3D11 GPU-only 录制后端：后台线程连续产出已编码
//! HEVC/AAC access units，写入内存环形缓存；保存按钮只把环形缓存快照 mux 成 MP4，
//! 不重编码、不复制 raw frame，也不绕开生产编码路径。

use super::{ProbeCaps, pipeline};
use crate::config::{AppConfig, CaptureBackend, ChromaSampling};
use crate::error::BackendError;
use crate::ring::{EncodedReplayMetadata, EncodedReplayRing};
use std::fs;
use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc::{self, Receiver, Sender};
use std::sync::{Arc, Mutex};
use std::thread::{self, JoinHandle};
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

#[derive(Debug, Clone)]
pub enum ReplayState {
    Idle,
    Running { started_at: Instant },
    Stopping { started_at: Instant },
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
    SegmentStarted { index: u64, path: PathBuf },
    SegmentReady { index: u64, clip: CompletedClip },
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
        }
    }
}

impl ReplayController {
    pub fn state(&self) -> &ReplayState {
        &self.state
    }

    pub fn drain_log_messages(&mut self) -> Vec<String> {
        let mut out = Vec::new();
        let mut terminal = false;
        if let Some(rx) = self.event_rx.take() {
            loop {
                match rx.try_recv() {
                    Ok(ReplayEvent::SegmentStarted { index, path }) => out.push(format!(
                        "后台即时回放片段 #{index} 开始录制到内存 encoded ring（调试路径标识：{}）",
                        path.display()
                    )),
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
                        terminal = true;
                    }
                    Ok(ReplayEvent::Stopped) => {
                        out.push("后台即时回放线程已停止。".to_owned());
                        self.state = ReplayState::Idle;
                        self.stop_flag = None;
                        self.live_ring = None;
                        terminal = true;
                    }
                    Err(mpsc::TryRecvError::Empty) => break,
                    Err(mpsc::TryRecvError::Disconnected) => {
                        out.push("后台即时回放事件通道已断开。".to_owned());
                        self.state = ReplayState::Idle;
                        self.stop_flag = None;
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
        let cache_dir = PathBuf::from(&config.cache_dir);
        fs::create_dir_all(&cache_dir).map_err(|err| BackendError::Io(err.to_string()))?;

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
        let segment_seconds = (config.replay_minutes.max(0.1) * 60.0).max(1.0);
        let replay_duration = Duration::from_secs_f32(segment_seconds.max(0.1));
        let live_ring = Arc::new(Mutex::new(EncodedReplayRing::new(
            replay_duration + Duration::from_secs(5),
        )));
        let worker_ring = live_ring.clone();
        let worker = thread::spawn(move || {
            run_segment_worker(
                request,
                caps,
                cache_dir,
                segment_seconds,
                worker_stop,
                tx,
                worker_ring,
            );
        });

        self.latest_clip = None;
        self.stop_flag = Some(stop_flag);
        self.worker = Some(worker);
        self.event_rx = Some(rx);
        self.live_ring = Some(live_ring);
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

fn run_segment_worker(
    request: pipeline::RecordingRequest,
    caps: ProbeCaps,
    cache_dir: PathBuf,
    segment_seconds: f32,
    stop_flag: Arc<AtomicBool>,
    tx: Sender<ReplayEvent>,
    encoded_ring: Arc<Mutex<EncodedReplayRing>>,
) {
    let mut segment_index = 0u64;
    let replay_duration = Duration::from_secs_f32(segment_seconds.max(0.1));
    while !stop_flag.load(Ordering::Relaxed) {
        let output = cache_dir.join(format!(
            "rustreplay_segment_{}_{}.mp4",
            timestamp_for_filename(),
            segment_index
        ));
        let _ = tx.send(ReplayEvent::SegmentStarted {
            index: segment_index,
            path: output.clone(),
        });

        let segment_stop = Arc::new(AtomicBool::new(false));
        let monitor_stop = segment_stop.clone();
        let user_stop = stop_flag.clone();
        let monitor = thread::spawn(move || {
            while !monitor_stop.load(Ordering::Relaxed) && !user_stop.load(Ordering::Relaxed) {
                thread::sleep(Duration::from_millis(20));
            }
            if user_stop.load(Ordering::Relaxed) {
                monitor_stop.store(true, Ordering::Relaxed);
            }
        });

        let (result, sink_started) = {
            let mut sink = SessionRingSink {
                ring: encoded_ring.clone(),
                started: false,
            };
            let result = pipeline::record_once_gpu_only_memory_output_with_sink_cancelable(
                &request,
                &caps,
                &output,
                segment_seconds,
                0,
                Some(segment_stop.clone()),
                Some(&mut sink),
            );
            let sink_started = sink.started;
            (result, sink_started)
        };
        segment_stop.store(true, Ordering::Relaxed);
        let _ = monitor.join();

        match result {
            Ok(recorded) => {
                let report = recorded.report;
                if sink_started {
                    if let Ok(mut ring) = encoded_ring.lock() {
                        ring.finish_segment_with_audio_duration(
                            recorded.video_track.duration_90k,
                            recorded
                                .audio_track
                                .as_ref()
                                .map(|track| (track.duration_ticks, track.sample_rate)),
                        );
                    }
                } else {
                    if let Ok(mut ring) = encoded_ring.lock() {
                        ring.push_tracks(&recorded.video_track, recorded.audio_track.as_ref());
                    }
                }
                let (snapshot, ring_packets, ring_bytes) = encoded_ring
                    .lock()
                    .map(|ring| {
                        (
                            ring.snapshot_recent_tracks(replay_duration),
                            ring.len(),
                            ring.bytes(),
                        )
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
                    index: segment_index,
                    clip,
                });
            }
            Err(err) => {
                if stop_flag.load(Ordering::Relaxed) {
                    break;
                }
                let _ = tx.send(ReplayEvent::Error(err.to_string()));
                return;
            }
        }
        segment_index = segment_index.saturating_add(1);
    }
    let _ = tx.send(ReplayEvent::Stopped);
}

struct SessionRingSink {
    ring: Arc<Mutex<EncodedReplayRing>>,
    started: bool,
}

impl super::vpl::VplOneCopyRecordSink for SessionRingSink {
    fn video_track_started(&mut self, info: super::vpl::VplOutputTrackInfo) {
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

fn timestamp_for_filename() -> String {
    let secs = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or_default();
    secs.to_string()
}
