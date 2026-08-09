use super::*;

use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::sync::mpsc::{Receiver, RecvTimeoutError, SyncSender, TrySendError};
use std::sync::{Arc, Mutex};
use std::thread::JoinHandle;
use std::time::Duration;

pub(super) const NVFBC_SINK_QUEUE_CAPACITY: usize = 96;

pub(super) enum NvFbcSinkMessage {
    Status(String),
    VideoTrack(VplOutputTrackInfo),
    Hevc(crate::backend::mp4_mux::HevcAccessUnit),
    Aac(crate::backend::mp4_mux::AacAccessUnit),
}

#[derive(Clone)]
pub(super) struct NvFbcChannelSink {
    tx: SyncSender<NvFbcSinkMessage>,
    failure: Arc<Mutex<Option<String>>>,
    queue: NvFbcSinkQueueStats,
}

#[derive(Clone)]
pub(super) struct NvFbcSinkQueueStats {
    queued: Arc<AtomicUsize>,
    high_water: Arc<AtomicUsize>,
}

impl NvFbcChannelSink {
    pub(super) fn new(
        tx: SyncSender<NvFbcSinkMessage>,
        failure: Arc<Mutex<Option<String>>>,
    ) -> Self {
        Self {
            tx,
            failure,
            queue: NvFbcSinkQueueStats {
                queued: Arc::new(AtomicUsize::new(0)),
                high_water: Arc::new(AtomicUsize::new(0)),
            },
        }
    }

    pub(super) fn failure(&self) -> Option<String> {
        self.failure
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
            .clone()
    }

    pub(super) fn queue_stats(&self) -> NvFbcSinkQueueStats {
        self.queue.clone()
    }

    pub(super) fn queue_high_water(&self) -> usize {
        self.queue.high_water.load(Ordering::Acquire)
    }

    fn publish_required(&self, message: NvFbcSinkMessage, label: &'static str) {
        if self.failure().is_some() {
            return;
        }
        let queued = self.queue.queued.fetch_add(1, Ordering::AcqRel) + 1;
        match self.tx.try_send(message) {
            Ok(()) => {
                self.queue.high_water.fetch_max(queued, Ordering::AcqRel);
            }
            Err(TrySendError::Full(_)) => {
                self.queue.queued.fetch_sub(1, Ordering::AcqRel);
                self.fail(format!(
                    "NvFBC encoded sink 有界队列已满：capacity={} packet={label}；拒绝静默丢包",
                    NVFBC_SINK_QUEUE_CAPACITY
                ));
            }
            Err(TrySendError::Disconnected(_)) => {
                self.queue.queued.fetch_sub(1, Ordering::AcqRel);
                self.fail(format!("NvFBC encoded sink 发布线程已退出：packet={label}"))
            }
        }
    }

    fn publish_status(&self, message: String) {
        let queued = self.queue.queued.fetch_add(1, Ordering::AcqRel) + 1;
        if self.tx.try_send(NvFbcSinkMessage::Status(message)).is_err() {
            self.queue.queued.fetch_sub(1, Ordering::AcqRel);
        } else {
            self.queue.high_water.fetch_max(queued, Ordering::AcqRel);
        }
    }

    fn fail(&self, message: String) {
        let mut failure = self
            .failure
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        if failure.is_none() {
            *failure = Some(message);
        }
    }
}

impl VplOneCopyRecordSink for NvFbcChannelSink {
    fn status(&mut self, message: &str) {
        self.publish_status(message.to_owned());
    }

    fn video_track_started(&mut self, info: VplOutputTrackInfo) {
        self.publish_required(NvFbcSinkMessage::VideoTrack(info), "video_track");
    }

    fn hevc_access_unit(&mut self, sample: &crate::backend::mp4_mux::HevcAccessUnit) {
        self.publish_required(NvFbcSinkMessage::Hevc(sample.clone()), "HEVC");
    }

    fn aac_access_unit(&mut self, sample: &crate::backend::mp4_mux::AacAccessUnit) {
        self.publish_required(NvFbcSinkMessage::Aac(sample.clone()), "AAC");
    }
}

pub(super) fn run_nvfbc_sink_publisher(
    rx: Receiver<NvFbcSinkMessage>,
    sink: &mut dyn VplOneCopyRecordSink,
    queue: NvFbcSinkQueueStats,
) {
    while let Ok(message) = rx.recv() {
        queue.queued.fetch_sub(1, Ordering::AcqRel);
        match message {
            NvFbcSinkMessage::Status(message) => sink.status(&message),
            NvFbcSinkMessage::VideoTrack(info) => sink.video_track_started(info),
            NvFbcSinkMessage::Hevc(sample) => sink.hevc_access_unit(&sample),
            NvFbcSinkMessage::Aac(sample) => sink.aac_access_unit(&sample),
        }
    }
}

enum NvFbcAudioCommand {
    VideoStart(i64),
    Finish(u64),
    Abort,
}

pub(super) struct NvFbcAudioOutput {
    pub(super) track: Option<crate::backend::mp4_mux::AacLcMp4Track>,
    pub(super) access_units: u32,
    pub(super) encoded_bytes: u64,
    pub(super) notes: Vec<String>,
}

pub(super) struct NvFbcAudioWorker {
    command_tx: SyncSender<NvFbcAudioCommand>,
    thread: Option<JoinHandle<Result<NvFbcAudioOutput, BackendError>>>,
    failed: Arc<AtomicBool>,
    failure: Arc<Mutex<Option<String>>>,
}

impl NvFbcAudioWorker {
    pub(super) fn spawn(
        duration: Duration,
        retain_output_samples: bool,
        sink: Option<NvFbcChannelSink>,
    ) -> Result<Self, BackendError> {
        let (command_tx, command_rx) = std::sync::mpsc::sync_channel(4);
        let failed = Arc::new(AtomicBool::new(false));
        let failure = Arc::new(Mutex::new(None));
        let failed_for_thread = failed.clone();
        let failure_for_thread = failure.clone();
        let thread = std::thread::Builder::new()
            .name("rustreplay-nvfbc-audio".to_owned())
            .spawn(move || {
                let result = run_audio_worker(command_rx, duration, retain_output_samples, sink);
                if let Err(err) = &result {
                    failed_for_thread.store(true, Ordering::Release);
                    let mut failure = failure_for_thread
                        .lock()
                        .unwrap_or_else(|poisoned| poisoned.into_inner());
                    *failure = Some(err.to_string());
                }
                result
            })
            .map_err(|err| BackendError::Io(format!("创建 NvFBC 音频工作线程失败：{err}")))?;
        Ok(Self {
            command_tx,
            thread: Some(thread),
            failed,
            failure,
        })
    }

    pub(super) fn set_video_start(&self, timestamp_100ns: i64) -> Result<(), BackendError> {
        self.command_tx
            .send(NvFbcAudioCommand::VideoStart(timestamp_100ns))
            .map_err(|err| {
                BackendError::unsupported(
                    "NvFBC audio worker",
                    "video start timestamp",
                    format!("音频工作线程已退出：{err}"),
                )
            })
    }

    pub(super) fn check(&self) -> Result<(), BackendError> {
        if self.failed.load(Ordering::Acquire) {
            return Err(BackendError::unsupported(
                "NvFBC audio worker",
                "incremental mixer/AAC",
                self.failure
                    .lock()
                    .unwrap_or_else(|poisoned| poisoned.into_inner())
                    .clone()
                    .unwrap_or_else(|| "音频工作线程失败".to_owned()),
            ));
        }
        Ok(())
    }

    pub(super) fn finish(mut self, duration_90k: u64) -> Result<NvFbcAudioOutput, BackendError> {
        self.command_tx
            .send(NvFbcAudioCommand::Finish(duration_90k))
            .map_err(|err| BackendError::Io(format!("结束 NvFBC 音频线程失败：{err}")))?;
        self.join()
    }

    pub(super) fn abort(mut self) {
        let _ = self.command_tx.try_send(NvFbcAudioCommand::Abort);
        if let Some(thread) = self.thread.take() {
            let _ = thread.join();
        }
    }

    fn join(&mut self) -> Result<NvFbcAudioOutput, BackendError> {
        let thread = self.thread.take().ok_or_else(|| {
            BackendError::unsupported("NvFBC audio worker", "join", "音频工作线程已被回收")
        })?;
        thread.join().map_err(|_| {
            BackendError::unsupported("NvFBC audio worker", "join", "音频工作线程 panic")
        })?
    }
}

impl Drop for NvFbcAudioWorker {
    fn drop(&mut self) {
        if self.thread.is_some() {
            let _ = self.command_tx.try_send(NvFbcAudioCommand::Abort);
            if let Some(thread) = self.thread.take() {
                let _ = thread.join();
            }
        }
    }
}

struct NvFbcAudioSink {
    channel: Option<NvFbcChannelSink>,
    retain_samples: bool,
    samples: Vec<crate::backend::mp4_mux::AacAccessUnit>,
}

impl VplOneCopyRecordSink for NvFbcAudioSink {
    fn video_track_started(&mut self, _info: VplOutputTrackInfo) {}

    fn hevc_access_unit(&mut self, _sample: &crate::backend::mp4_mux::HevcAccessUnit) {}

    fn aac_access_unit(&mut self, sample: &crate::backend::mp4_mux::AacAccessUnit) {
        if let Some(channel) = self.channel.as_mut() {
            channel.aac_access_unit(sample);
        }
        if self.retain_samples {
            self.samples.push(sample.clone());
        }
    }
}

fn run_audio_worker(
    command_rx: Receiver<NvFbcAudioCommand>,
    duration: Duration,
    retain_output_samples: bool,
    channel_sink: Option<NvFbcChannelSink>,
) -> Result<NvFbcAudioOutput, BackendError> {
    let mut notes = Vec::new();
    let mut capture = RecordAudioCapture::start(duration, false, &mut notes);
    let mut audio_sink = NvFbcAudioSink {
        channel: channel_sink,
        retain_samples: retain_output_samples,
        samples: Vec::new(),
    };
    let mut video_start_100ns = None;
    loop {
        match command_rx.recv_timeout(Duration::from_millis(4)) {
            Ok(NvFbcAudioCommand::VideoStart(timestamp)) => {
                video_start_100ns.get_or_insert(timestamp);
            }
            Ok(NvFbcAudioCommand::Finish(duration_90k)) => {
                return finish_audio_worker(
                    capture.as_mut(),
                    video_start_100ns,
                    duration_90k,
                    &mut audio_sink,
                    notes,
                );
            }
            Ok(NvFbcAudioCommand::Abort) | Err(RecvTimeoutError::Disconnected) => {
                if let Some(capture) = capture.as_mut() {
                    capture.stop_without_reencode(&mut notes);
                }
                return Ok(NvFbcAudioOutput {
                    track: None,
                    access_units: 0,
                    encoded_bytes: 0,
                    notes,
                });
            }
            Err(RecvTimeoutError::Timeout) => {}
        }
        if let Some(capture) = capture.as_mut() {
            let mut sink = Some(&mut audio_sink as &mut dyn VplOneCopyRecordSink);
            capture.poll_live_aac(video_start_100ns, None, &mut sink, &mut notes)?;
        }
    }
}

fn finish_audio_worker(
    capture: Option<&mut RecordAudioCapture>,
    video_start_100ns: Option<i64>,
    duration_90k: u64,
    audio_sink: &mut NvFbcAudioSink,
    mut notes: Vec<String>,
) -> Result<NvFbcAudioOutput, BackendError> {
    let Some(capture) = capture else {
        return Ok(NvFbcAudioOutput {
            track: None,
            access_units: 0,
            encoded_bytes: 0,
            notes,
        });
    };
    let mut sink = Some(audio_sink as &mut dyn VplOneCopyRecordSink);
    let mut track =
        capture.finish_streaming(video_start_100ns, duration_90k, None, &mut sink, &mut notes)?;
    if let Some(track) = track.as_mut()
        && audio_sink.retain_samples
    {
        track.samples = std::mem::take(&mut audio_sink.samples);
    }
    let access_units = capture.live_access_units();
    let encoded_bytes = capture.live_encoded_bytes();
    Ok(NvFbcAudioOutput {
        track,
        access_units,
        encoded_bytes,
        notes,
    })
}
