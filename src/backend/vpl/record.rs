use super::*;

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct VplOneCopyRecordReport {
    pub adapter_index: u32,
    pub output_index: u32,
    pub adapter_luid: String,
    pub output_path: String,
    pub width: u16,
    pub height: u16,
    pub duration_seconds: f32,
    pub captured_frames: u32,
    pub encoded_samples: u32,
    pub encoded_bytes: u64,
    pub audio_access_units: u32,
    pub audio_encoded_bytes: u64,
    pub dda_timeouts: u32,
    pub input_dxgi_format: u32,
    pub target_dxgi_format: u32,
    pub query_status: i32,
    pub init_status: i32,
    pub close_status: i32,
    pub first_get_surface_status: i32,
    pub video_processor_format_flags_in: u32,
    pub video_processor_format_flags_out: u32,
    pub notes: Vec<String>,
}

#[derive(Debug, Clone)]
pub struct VplOneCopyRecordOutput {
    pub report: VplOneCopyRecordReport,
    pub video_track: crate::backend::mp4_mux::HevcMp4Track,
    pub audio_track: Option<crate::backend::mp4_mux::AacLcMp4Track>,
}

#[derive(Debug, Clone, Copy)]
pub struct VplOutputTrackInfo {
    pub width: u16,
    pub height: u16,
    pub color: crate::backend::mp4_mux::NclxColorMetadata,
    pub codec: crate::backend::mp4_mux::HevcCodecMetadata,
}

pub trait VplOneCopyRecordSink {
    fn status(&mut self, _message: &str) {}
    fn video_track_started(&mut self, info: VplOutputTrackInfo);
    fn hevc_access_unit(&mut self, sample: &crate::backend::mp4_mux::HevcAccessUnit);
    fn aac_access_unit(&mut self, _sample: &crate::backend::mp4_mux::AacAccessUnit) {}
}

#[cfg(windows)]
pub fn record_d3d11_onecopy_mp4(
    adapter_index: u32,
    output: &Path,
    duration_seconds: f32,
    rate_control: &RateControlConfig,
    requested_chroma: ChromaSampling,
) -> Result<VplOneCopyRecordReport, BackendError> {
    record_d3d11_onecopy_mp4_cancelable(
        adapter_index,
        output,
        duration_seconds,
        rate_control,
        requested_chroma,
        None,
    )
}

#[cfg(windows)]
pub fn record_d3d11_onecopy_mp4_cancelable(
    adapter_index: u32,
    output: &Path,
    duration_seconds: f32,
    rate_control: &RateControlConfig,
    requested_chroma: ChromaSampling,
    external_stop: Option<std::sync::Arc<std::sync::atomic::AtomicBool>>,
) -> Result<VplOneCopyRecordReport, BackendError> {
    Ok(record_d3d11_onecopy_mp4_output_cancelable(
        adapter_index,
        output,
        duration_seconds,
        rate_control,
        requested_chroma,
        external_stop,
    )?
    .report)
}

#[cfg(windows)]
pub fn record_d3d11_onecopy_mp4_output_cancelable(
    adapter_index: u32,
    output: &Path,
    duration_seconds: f32,
    rate_control: &RateControlConfig,
    requested_chroma: ChromaSampling,
    external_stop: Option<std::sync::Arc<std::sync::atomic::AtomicBool>>,
) -> Result<VplOneCopyRecordOutput, BackendError> {
    record_d3d11_onecopy_mp4_output_with_route_cancelable(
        adapter_index,
        output,
        duration_seconds,
        rate_control,
        requested_chroma,
        external_stop,
        None,
    )
}

#[cfg(windows)]
#[allow(clippy::too_many_arguments)]
pub fn record_d3d11_onecopy_mp4_output_with_route_cancelable(
    adapter_index: u32,
    output: &Path,
    duration_seconds: f32,
    rate_control: &RateControlConfig,
    requested_chroma: ChromaSampling,
    external_stop: Option<std::sync::Arc<std::sync::atomic::AtomicBool>>,
    route_plan: Option<&VplCurrentDisplayRouteInfo>,
) -> Result<VplOneCopyRecordOutput, BackendError> {
    record_d3d11_onecopy_mp4_impl(
        adapter_index,
        output,
        duration_seconds,
        rate_control,
        requested_chroma,
        RecordCaptureSource::Dda,
        external_stop,
        true,
        None,
        route_plan,
    )
}

#[cfg(windows)]
pub fn record_d3d11_onecopy_memory_output_cancelable(
    adapter_index: u32,
    output: &Path,
    duration_seconds: f32,
    rate_control: &RateControlConfig,
    requested_chroma: ChromaSampling,
    external_stop: Option<std::sync::Arc<std::sync::atomic::AtomicBool>>,
) -> Result<VplOneCopyRecordOutput, BackendError> {
    record_d3d11_onecopy_memory_output_with_sink_cancelable(
        adapter_index,
        output,
        duration_seconds,
        rate_control,
        requested_chroma,
        external_stop,
        None,
        None,
    )
}

#[cfg(windows)]
#[allow(clippy::too_many_arguments)]
pub fn record_d3d11_onecopy_memory_output_with_sink_cancelable(
    adapter_index: u32,
    output: &Path,
    duration_seconds: f32,
    rate_control: &RateControlConfig,
    requested_chroma: ChromaSampling,
    external_stop: Option<std::sync::Arc<std::sync::atomic::AtomicBool>>,
    encoded_sink: Option<&mut dyn VplOneCopyRecordSink>,
    route_plan: Option<&VplCurrentDisplayRouteInfo>,
) -> Result<VplOneCopyRecordOutput, BackendError> {
    record_d3d11_onecopy_mp4_impl(
        adapter_index,
        output,
        duration_seconds,
        rate_control,
        requested_chroma,
        RecordCaptureSource::Dda,
        external_stop,
        false,
        encoded_sink,
        route_plan,
    )
}

#[cfg(windows)]
pub fn record_wgc_d3d11_onecopy_mp4(
    adapter_index: u32,
    output: &Path,
    duration_seconds: f32,
    rate_control: &RateControlConfig,
    requested_chroma: ChromaSampling,
) -> Result<VplOneCopyRecordReport, BackendError> {
    record_wgc_d3d11_onecopy_mp4_cancelable(
        adapter_index,
        output,
        duration_seconds,
        rate_control,
        requested_chroma,
        None,
    )
}

#[cfg(windows)]
pub fn record_wgc_d3d11_onecopy_mp4_cancelable(
    adapter_index: u32,
    output: &Path,
    duration_seconds: f32,
    rate_control: &RateControlConfig,
    requested_chroma: ChromaSampling,
    external_stop: Option<std::sync::Arc<std::sync::atomic::AtomicBool>>,
) -> Result<VplOneCopyRecordReport, BackendError> {
    Ok(record_wgc_d3d11_onecopy_mp4_output_cancelable(
        adapter_index,
        output,
        duration_seconds,
        rate_control,
        requested_chroma,
        external_stop,
    )?
    .report)
}

#[cfg(windows)]
pub fn record_wgc_d3d11_onecopy_mp4_output_cancelable(
    adapter_index: u32,
    output: &Path,
    duration_seconds: f32,
    rate_control: &RateControlConfig,
    requested_chroma: ChromaSampling,
    external_stop: Option<std::sync::Arc<std::sync::atomic::AtomicBool>>,
) -> Result<VplOneCopyRecordOutput, BackendError> {
    record_wgc_d3d11_onecopy_mp4_output_with_route_cancelable(
        adapter_index,
        output,
        duration_seconds,
        rate_control,
        requested_chroma,
        external_stop,
        None,
    )
}

#[cfg(windows)]
#[allow(clippy::too_many_arguments)]
pub fn record_wgc_d3d11_onecopy_mp4_output_with_route_cancelable(
    adapter_index: u32,
    output: &Path,
    duration_seconds: f32,
    rate_control: &RateControlConfig,
    requested_chroma: ChromaSampling,
    external_stop: Option<std::sync::Arc<std::sync::atomic::AtomicBool>>,
    route_plan: Option<&VplCurrentDisplayRouteInfo>,
) -> Result<VplOneCopyRecordOutput, BackendError> {
    record_d3d11_onecopy_mp4_impl(
        adapter_index,
        output,
        duration_seconds,
        rate_control,
        requested_chroma,
        RecordCaptureSource::Wgc,
        external_stop,
        true,
        None,
        route_plan,
    )
}

#[cfg(windows)]
pub fn record_wgc_d3d11_onecopy_memory_output_cancelable(
    adapter_index: u32,
    output: &Path,
    duration_seconds: f32,
    rate_control: &RateControlConfig,
    requested_chroma: ChromaSampling,
    external_stop: Option<std::sync::Arc<std::sync::atomic::AtomicBool>>,
) -> Result<VplOneCopyRecordOutput, BackendError> {
    record_wgc_d3d11_onecopy_memory_output_with_sink_cancelable(
        adapter_index,
        output,
        duration_seconds,
        rate_control,
        requested_chroma,
        external_stop,
        None,
        None,
    )
}

#[cfg(windows)]
#[allow(clippy::too_many_arguments)]
pub fn record_wgc_d3d11_onecopy_memory_output_with_sink_cancelable(
    adapter_index: u32,
    output: &Path,
    duration_seconds: f32,
    rate_control: &RateControlConfig,
    requested_chroma: ChromaSampling,
    external_stop: Option<std::sync::Arc<std::sync::atomic::AtomicBool>>,
    encoded_sink: Option<&mut dyn VplOneCopyRecordSink>,
    route_plan: Option<&VplCurrentDisplayRouteInfo>,
) -> Result<VplOneCopyRecordOutput, BackendError> {
    record_d3d11_onecopy_mp4_impl(
        adapter_index,
        output,
        duration_seconds,
        rate_control,
        requested_chroma,
        RecordCaptureSource::Wgc,
        external_stop,
        false,
        encoded_sink,
        route_plan,
    )
}

#[cfg(windows)]
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum RecordCaptureSource {
    Dda,
    Wgc,
}

#[cfg(windows)]
impl RecordCaptureSource {
    pub(super) fn is_wgc(self) -> bool {
        matches!(self, Self::Wgc)
    }

    pub(super) const fn label(self) -> &'static str {
        match self {
            Self::Dda => "DDA",
            Self::Wgc => "WGC",
        }
    }
}

#[cfg(windows)]
pub(super) type RecordAudioCaptureResult = (
    crate::backend::audio::AudioSourceKind,
    Result<crate::backend::wasapi::WasapiCaptureStats, BackendError>,
);

#[cfg(windows)]
pub(super) type RecordAudioCaptureHandle = std::thread::JoinHandle<RecordAudioCaptureResult>;

#[cfg(windows)]
pub(super) struct RecordAudioCapture {
    pub(super) stop: std::sync::Arc<std::sync::atomic::AtomicBool>,
    pub(super) handles: Vec<RecordAudioCaptureHandle>,
    pub(super) rx: std::sync::mpsc::Receiver<crate::backend::audio::PcmFrame>,
    pub(super) frames: Vec<crate::backend::audio::PcmFrame>,
    pub(super) live_encoder: Option<crate::backend::aac_mf::MfAacLcEncoder>,
    pub(super) live_blocker: crate::backend::audio::AacBlocker,
    pub(super) live_submitted_until_ticks: u64,
    pub(super) live_pushed_until_ticks: u64,
    pub(super) live_failed: bool,
    pub(super) retain_full_pcm: bool,
    pub(super) live_access_units: u32,
    pub(super) live_encoded_bytes: u64,
    pub(super) audio_end_abs_100ns: Option<i64>,
}

#[cfg(windows)]
impl RecordAudioCapture {
    pub(super) const LIVE_SAFETY_100NS: i64 = 1_000_000; // 100ms，避免麦克风/loopback 较晚 packet 改写已推送 AAC。
    const PRE_VIDEO_PCM_100NS: i64 = 50_000_000; // 首个正式视频帧之前只需保留最近 5 秒音频。

    pub(super) fn start(
        duration: std::time::Duration,
        retain_full_pcm: bool,
        notes: &mut Vec<String>,
    ) -> Option<Self> {
        if std::env::var("RUST_REPLAY_AUDIO")
            .ok()
            .is_some_and(|value| value == "0" || value.eq_ignore_ascii_case("false"))
        {
            notes.push("音频采集被 RUST_REPLAY_AUDIO=0 显式关闭".to_owned());
            return None;
        }
        let stop = std::sync::Arc::new(std::sync::atomic::AtomicBool::new(false));
        let (tx, rx) = std::sync::mpsc::channel();
        let mut handles = Vec::new();
        for source in [
            crate::backend::audio::AudioSourceKind::Loopback,
            crate::backend::audio::AudioSourceKind::Microphone,
        ] {
            let stop_for_thread = stop.clone();
            let tx_for_thread = tx.clone();
            handles.push(std::thread::spawn(move || {
                let result = crate::backend::wasapi::capture_default_streaming(
                    source,
                    duration,
                    Some(stop_for_thread),
                    tx_for_thread,
                );
                (source, result)
            }));
        }
        drop(tx);
        notes.push(
            "音频路径已并行启动：WASAPI loopback + 默认麦克风，按 QPC/100ns 绝对时间戳流式采集；录制后端内部按首个正式视频源时间戳裁剪/重基准并实时推送 AAC 到 encoded ring"
                .to_owned(),
        );
        Some(Self {
            stop,
            handles,
            rx,
            frames: Vec::new(),
            live_encoder: None,
            live_blocker: crate::backend::audio::AacBlocker::default(),
            live_submitted_until_ticks: 0,
            live_pushed_until_ticks: 0,
            live_failed: false,
            retain_full_pcm,
            live_access_units: 0,
            live_encoded_bytes: 0,
            audio_end_abs_100ns: None,
        })
    }

    pub(super) fn drain_incoming(&mut self) {
        while let Ok(frame) = self.rx.try_recv() {
            let end = frame.end_time_100ns();
            self.audio_end_abs_100ns = Some(
                self.audio_end_abs_100ns
                    .map(|current| current.max(end))
                    .unwrap_or(end),
            );
            self.frames.push(frame);
        }
        if !self.retain_full_pcm && self.live_submitted_until_ticks == 0 {
            let keep_from = self
                .audio_end_abs_100ns
                .unwrap_or(0)
                .saturating_sub(Self::PRE_VIDEO_PCM_100NS);
            self.frames
                .retain(|frame| frame.end_time_100ns() >= keep_from);
        }
    }

    pub(super) fn poll_live_aac(
        &mut self,
        first_video_timestamp_100ns: Option<i64>,
        _current_video_timestamp_90k: Option<u64>,
        encoded_sink: &mut Option<&mut dyn VplOneCopyRecordSink>,
        _notes: &mut Vec<String>,
    ) -> Result<(), BackendError> {
        self.drain_incoming();
        if encoded_sink.is_none() {
            return Ok(());
        }
        if self.live_failed {
            return Err(BackendError::unsupported(
                "实时 AAC",
                "encoded replay sink",
                "此前 AAC 编码已失败，已阻止继续积累未编码 PCM",
            ));
        }
        let result = self.poll_live_aac_inner(first_video_timestamp_100ns, encoded_sink);
        self.live_failed = result.is_err();
        result
    }

    pub(super) fn poll_live_aac_inner(
        &mut self,
        first_video_timestamp_100ns: Option<i64>,
        encoded_sink: &mut Option<&mut dyn VplOneCopyRecordSink>,
    ) -> Result<(), BackendError> {
        let Some(video_start_100ns) = first_video_timestamp_100ns else {
            return Ok(());
        };
        let Some(audio_end_abs_100ns) = self.audio_end_abs_100ns else {
            return Ok(());
        };
        let audio_ready_100ns = audio_end_abs_100ns
            .saturating_sub(video_start_100ns)
            .saturating_sub(Self::LIVE_SAFETY_100NS)
            .max(0);
        let ready_ticks = audio_100ns_to_ticks(audio_ready_100ns);
        let frame_ticks = crate::backend::audio::AAC_LC_FRAME_SAMPLES as u64;
        let encode_until_ticks = (ready_ticks / frame_ticks) * frame_ticks;
        self.encode_live_until_ticks(video_start_100ns, encode_until_ticks, encoded_sink)?;
        self.prune_live_pcm_frames(video_start_100ns);
        Ok(())
    }

    fn encode_live_until_ticks(
        &mut self,
        video_start_100ns: i64,
        encode_until_ticks: u64,
        encoded_sink: &mut Option<&mut dyn VplOneCopyRecordSink>,
    ) -> Result<(), BackendError> {
        use crate::backend::audio::{
            AAC_LC_FRAME_SAMPLES, TARGET_SAMPLE_RATE, mix_window_samples_to_stereo_48k,
        };

        if encode_until_ticks <= self.live_submitted_until_ticks {
            return Ok(());
        }
        if self.live_encoder.is_none() {
            self.live_encoder = Some(crate::backend::aac_mf::MfAacLcEncoder::new()?);
        }
        let chunk_ticks = u64::from(TARGET_SAMPLE_RATE);
        while self.live_submitted_until_ticks < encode_until_ticks {
            let from_ticks = self.live_submitted_until_ticks;
            let until_ticks = from_ticks
                .saturating_add(chunk_ticks)
                .min(encode_until_ticks);
            let window_ticks = until_ticks.saturating_sub(from_ticks);
            let window_start_offset_100ns = audio_ticks_to_100ns(from_ticks);
            let window_start_abs_100ns =
                video_start_100ns.saturating_add(window_start_offset_100ns);
            let mut mixed = mix_window_samples_to_stereo_48k(
                &self.frames,
                window_start_abs_100ns,
                window_ticks as usize,
            )?;
            mixed.start_time_100ns = window_start_offset_100ns;
            let blocks = self.live_blocker.push(&mixed);
            let encoder = self.live_encoder.as_mut().expect("created above");
            let mut submitted_until_ticks = from_ticks;
            for block in &blocks {
                if block.timestamp_ticks < submitted_until_ticks
                    || block.timestamp_ticks >= until_ticks
                {
                    continue;
                }
                let samples = encoder.encode_block(block)?;
                for sample in samples {
                    Self::push_live_aac_sample(
                        &mut self.live_pushed_until_ticks,
                        &mut self.live_access_units,
                        &mut self.live_encoded_bytes,
                        sample,
                        encoded_sink,
                    );
                }
                submitted_until_ticks = block
                    .timestamp_ticks
                    .saturating_add(AAC_LC_FRAME_SAMPLES as u64);
            }
            self.live_submitted_until_ticks = submitted_until_ticks.max(until_ticks);
            self.prune_live_pcm_frames(video_start_100ns);
        }
        Ok(())
    }

    fn push_live_aac_sample(
        pushed_until_ticks: &mut u64,
        access_units: &mut u32,
        encoded_bytes: &mut u64,
        sample: crate::backend::mp4_mux::AacAccessUnit,
        encoded_sink: &mut Option<&mut dyn VplOneCopyRecordSink>,
    ) {
        if let Some(sink) = encoded_sink.as_deref_mut() {
            sink.aac_access_unit(&sample);
        }
        *pushed_until_ticks = (*pushed_until_ticks).max(
            sample
                .timestamp_ticks
                .saturating_add(u64::from(sample.duration_ticks)),
        );
        *access_units = access_units.saturating_add(1);
        *encoded_bytes = encoded_bytes.saturating_add(sample.data.len() as u64);
    }

    pub(super) fn prune_live_pcm_frames(&mut self, video_start_100ns: i64) {
        let keep_from_ticks = self
            .live_submitted_until_ticks
            .saturating_sub(crate::backend::audio::TARGET_SAMPLE_RATE as u64);
        let keep_from_100ns =
            video_start_100ns.saturating_add(audio_ticks_to_100ns(keep_from_ticks));
        self.frames
            .retain(|frame| frame.end_time_100ns() >= keep_from_100ns);
    }

    pub(super) fn finish_live_aac(
        &mut self,
        encoded_sink: &mut Option<&mut dyn VplOneCopyRecordSink>,
        notes: &mut Vec<String>,
    ) -> Result<(), BackendError> {
        if let Some(encoder) = self.live_encoder.as_mut()
            && let Some(block) = self.live_blocker.flush_padded()
        {
            let samples = encoder.encode_block(&block)?;
            for sample in samples {
                Self::push_live_aac_sample(
                    &mut self.live_pushed_until_ticks,
                    &mut self.live_access_units,
                    &mut self.live_encoded_bytes,
                    sample,
                    encoded_sink,
                );
            }
        }
        if let Some(encoder) = self.live_encoder.take() {
            let samples = encoder.finish()?;
            let pushed = samples.len();
            for sample in samples {
                Self::push_live_aac_sample(
                    &mut self.live_pushed_until_ticks,
                    &mut self.live_access_units,
                    &mut self.live_encoded_bytes,
                    sample,
                    encoded_sink,
                );
            }
            if pushed > 0 {
                notes.push(format!(
                    "实时 AAC ring flush 推送 access_units={} pushed_until_ticks={}",
                    pushed, self.live_pushed_until_ticks
                ));
            }
        }
        Ok(())
    }

    pub(super) fn live_pushed_until_ticks(&self) -> u64 {
        self.live_pushed_until_ticks
    }

    pub(super) fn live_access_units(&self) -> u32 {
        self.live_access_units
    }

    pub(super) fn live_encoded_bytes(&self) -> u64 {
        self.live_encoded_bytes
    }

    pub(super) fn finish_streaming(
        &mut self,
        first_video_timestamp_100ns: Option<i64>,
        video_duration_90k: u64,
        encoded_sink: &mut Option<&mut dyn VplOneCopyRecordSink>,
        notes: &mut Vec<String>,
    ) -> Result<Option<crate::backend::mp4_mux::AacLcMp4Track>, BackendError> {
        self.stop_and_join(notes, "流式结束");
        let Some(video_start_100ns) = first_video_timestamp_100ns else {
            self.frames.clear();
            notes.push("流式 AAC 结束跳过：没有首个正式视频绝对时间戳".to_owned());
            return Ok(None);
        };
        let target_ticks = video_90k_to_audio_ticks(video_duration_90k).max(1);
        self.encode_live_until_ticks(video_start_100ns, target_ticks, encoded_sink)?;
        self.finish_live_aac(encoded_sink, notes)?;
        self.frames.clear();
        if self.live_access_units == 0 {
            notes.push("流式 AAC 结束跳过：编码器没有输出 access unit".to_owned());
            return Ok(None);
        }
        let duration_ticks = self.live_pushed_until_ticks.max(target_ticks).max(1);
        notes.push(format!(
            "流式 AAC 完成：access_units={} encoded_bytes={} duration_ticks={}，未保留整段 PCM/AU 副本",
            self.live_access_units, self.live_encoded_bytes, duration_ticks
        ));
        Ok(Some(crate::backend::mp4_mux::AacLcMp4Track {
            sample_rate: crate::backend::audio::TARGET_SAMPLE_RATE,
            channel_count: crate::backend::audio::TARGET_CHANNELS,
            duration_ticks,
            samples: Vec::new(),
        }))
    }

    pub(super) fn finish(
        &mut self,
        notes: &mut Vec<String>,
    ) -> Vec<crate::backend::audio::PcmFrame> {
        self.stop_and_join(notes, "捕获结束");
        std::mem::take(&mut self.frames)
    }

    fn stop_and_join(&mut self, notes: &mut Vec<String>, phase: &str) {
        self.drain_incoming();
        self.stop.store(true, std::sync::atomic::Ordering::Relaxed);
        while let Some(handle) = self.handles.pop() {
            match handle.join() {
                Ok((source, Ok(stats))) => {
                    notes.push(format!(
                        "WASAPI {:?} {}：packet={} pcm_frames={}",
                        source, phase, stats.packet_count, stats.pcm_frames
                    ));
                }
                Ok((source, Err(err))) => {
                    notes.push(format!("WASAPI {:?} {}不可用：{err}", source, phase));
                }
                Err(_) => notes.push(format!("WASAPI {phase}线程 panic；该音源被跳过")),
            }
            self.drain_incoming();
        }
        self.drain_incoming();
    }

    pub(super) fn stop_without_reencode(&mut self, notes: &mut Vec<String>) {
        let started = std::time::Instant::now();
        self.stop.store(true, std::sync::atomic::Ordering::Relaxed);
        while let Some(handle) = self.handles.pop() {
            match handle.join() {
                Ok((source, Ok(stats))) => notes.push(format!(
                    "WASAPI {:?} 快速停止：packet={} pcm_frames={}",
                    source, stats.packet_count, stats.pcm_frames
                )),
                Ok((source, Err(err))) => {
                    notes.push(format!("WASAPI {:?} 快速停止时不可用：{err}", source));
                }
                Err(_) => notes.push("WASAPI 快速停止时捕获线程 panic；该音源被跳过".to_owned()),
            }
        }
        self.drain_incoming();
        self.frames.clear();
        notes.push(format!(
            "音频快速停止完成：跳过停止时完整 AAC 重建，耗时 {:.1}ms",
            started.elapsed().as_secs_f64() * 1000.0
        ));
    }
}

#[cfg(windows)]
impl Drop for RecordAudioCapture {
    fn drop(&mut self) {
        self.stop.store(true, std::sync::atomic::Ordering::Relaxed);
        while let Some(handle) = self.handles.pop() {
            let _ = handle.join();
        }
    }
}

#[cfg(windows)]
pub(super) fn build_record_aac_track(
    audio_frames: Vec<crate::backend::audio::PcmFrame>,
    first_video_timestamp_100ns: Option<i64>,
    video_duration_90k: u64,
    notes: &mut Vec<String>,
    encoded_sink: &mut Option<&mut dyn VplOneCopyRecordSink>,
    sink_push_from_ticks: u64,
) -> Result<Option<crate::backend::mp4_mux::AacLcMp4Track>, BackendError> {
    use crate::backend::audio::{
        AacBlocker, TARGET_CHANNELS, TARGET_SAMPLE_RATE, mix_window_samples_to_stereo_48k,
    };
    use crate::backend::mp4_mux::AacLcMp4Track;

    let Some(video_start_100ns) = first_video_timestamp_100ns else {
        notes.push("音频封装跳过：视频路径没有可与 WASAPI QPC 对齐的首帧绝对时间戳".to_owned());
        return Ok(None);
    };
    let audio_duration_ticks = video_90k_to_audio_ticks(video_duration_90k).max(1);
    let audio_duration_100ns = video_90k_to_100ns(video_duration_90k).max(1);
    let audio_end_100ns = video_start_100ns.saturating_add(audio_duration_100ns);
    let clipped_packets = audio_frames
        .iter()
        .filter(|frame| {
            frame.end_time_100ns() > video_start_100ns && frame.start_time_100ns < audio_end_100ns
        })
        .count();
    let clipped_pcm_frames = audio_frames
        .iter()
        .filter(|frame| {
            frame.end_time_100ns() > video_start_100ns && frame.start_time_100ns < audio_end_100ns
        })
        .map(|frame| frame.frame_count())
        .sum::<usize>();
    let mut encoder = crate::backend::aac_mf::MfAacLcEncoder::new()?;
    let mut blocker = AacBlocker::default();
    let mut samples = Vec::new();
    let mut mixed_48k_frames = 0u64;
    let mut aac_blocks = 0usize;
    let mut cursor_ticks = 0u64;
    const AAC_MIX_CHUNK_TICKS: u64 = TARGET_SAMPLE_RATE as u64;
    while cursor_ticks < audio_duration_ticks {
        let chunk_ticks = (audio_duration_ticks - cursor_ticks).min(AAC_MIX_CHUNK_TICKS);
        let window_start_abs_100ns =
            video_start_100ns.saturating_add(audio_ticks_to_100ns(cursor_ticks));
        let mut mixed = mix_window_samples_to_stereo_48k(
            &audio_frames,
            window_start_abs_100ns,
            chunk_ticks as usize,
        )?;
        mixed.start_time_100ns = audio_ticks_to_100ns(cursor_ticks);
        mixed_48k_frames = mixed_48k_frames.saturating_add(mixed.samples.len() as u64);
        for block in blocker.push(&mixed) {
            aac_blocks += 1;
            for sample in encoder.encode_block(&block)? {
                push_record_aac_sample(&mut samples, sample, encoded_sink, sink_push_from_ticks);
            }
        }
        cursor_ticks = cursor_ticks.saturating_add(chunk_ticks);
    }
    if let Some(block) = blocker.flush_padded() {
        aac_blocks += 1;
        for sample in encoder.encode_block(&block)? {
            push_record_aac_sample(&mut samples, sample, encoded_sink, sink_push_from_ticks);
        }
    }
    for sample in encoder.finish()? {
        push_record_aac_sample(&mut samples, sample, encoded_sink, sink_push_from_ticks);
    }
    if samples.is_empty() {
        notes.push("音频封装跳过：AAC encoder 没有输出 access unit".to_owned());
        return Ok(None);
    }
    let final_duration_ticks = samples
        .iter()
        .map(|sample| u64::from(sample.duration_ticks))
        .sum::<u64>()
        .max(1);
    let aac_padding_ticks = final_duration_ticks.saturating_sub(audio_duration_ticks);
    notes.push(format!(
        "音频同步：video_start_qpc100ns={} video_duration_90k={} requested_audio_ticks={} final_aac_ticks={} aac_padding_ticks={} clipped_packets={} clipped_pcm_frames={} mixed_48k_frames={} aac_blocks={}",
        video_start_100ns,
        video_duration_90k,
        audio_duration_ticks,
        final_duration_ticks,
        aac_padding_ticks,
        clipped_packets,
        clipped_pcm_frames,
        mixed_48k_frames,
        aac_blocks
    ));
    if sink_push_from_ticks > 0 {
        notes.push(format!(
            "音频 ring 去重：段结束完整 AAC 重新编码后，只把 timestamp_ticks>={sink_push_from_ticks} 的尾部 AU 推给 encoded ring，避免与实时 AAC 重复"
        ));
    }
    Ok(Some(AacLcMp4Track {
        sample_rate: TARGET_SAMPLE_RATE,
        channel_count: TARGET_CHANNELS,
        duration_ticks: final_duration_ticks,
        samples,
    }))
}

#[cfg(windows)]
pub(super) fn push_record_aac_sample(
    samples: &mut Vec<crate::backend::mp4_mux::AacAccessUnit>,
    sample: crate::backend::mp4_mux::AacAccessUnit,
    encoded_sink: &mut Option<&mut dyn VplOneCopyRecordSink>,
    sink_push_from_ticks: u64,
) {
    if sample.timestamp_ticks >= sink_push_from_ticks
        && let Some(sink) = encoded_sink.as_deref_mut()
    {
        sink.aac_access_unit(&sample);
    }
    samples.push(sample);
}

#[cfg(windows)]
pub(super) fn capture_dimensions_from_output(
    output_desc: &windows::Win32::Graphics::Dxgi::DXGI_OUTPUT_DESC,
) -> (u16, u16, String) {
    use windows::Win32::Graphics::Gdi::{DEVMODEW, ENUM_CURRENT_SETTINGS, EnumDisplaySettingsW};
    use windows::core::PCWSTR;

    let desktop_width =
        (output_desc.DesktopCoordinates.right - output_desc.DesktopCoordinates.left).max(1) as u32;
    let desktop_height =
        (output_desc.DesktopCoordinates.bottom - output_desc.DesktopCoordinates.top).max(1) as u32;
    let mut devmode = DEVMODEW {
        dmSize: std::mem::size_of::<DEVMODEW>() as u16,
        ..Default::default()
    };
    let mode_available = unsafe {
        EnumDisplaySettingsW(
            PCWSTR(output_desc.DeviceName.as_ptr()),
            ENUM_CURRENT_SETTINGS,
            &mut devmode,
        )
        .as_bool()
    };
    let (width, height, source) =
        if mode_available && devmode.dmPelsWidth > 0 && devmode.dmPelsHeight > 0 {
            (
                devmode.dmPelsWidth,
                devmode.dmPelsHeight,
                "EnumDisplaySettingsW physical mode",
            )
        } else {
            (
                desktop_width,
                desktop_height,
                "DXGI DesktopCoordinates fallback",
            )
        };
    (
        width.min(u32::from(u16::MAX)) as u16,
        height.min(u32::from(u16::MAX)) as u16,
        format!(
            "{source}={}x{}; DXGI DesktopCoordinates={}x{}",
            width, height, desktop_width, desktop_height
        ),
    )
}

#[cfg(windows)]
pub(super) fn display_frequency_hz_from_output(
    output_desc: &windows::Win32::Graphics::Dxgi::DXGI_OUTPUT_DESC,
) -> Option<u32> {
    use windows::Win32::Graphics::Gdi::{DEVMODEW, ENUM_CURRENT_SETTINGS, EnumDisplaySettingsW};
    use windows::core::PCWSTR;

    let mut devmode = DEVMODEW {
        dmSize: std::mem::size_of::<DEVMODEW>() as u16,
        ..Default::default()
    };
    let available = unsafe {
        EnumDisplaySettingsW(
            PCWSTR(output_desc.DeviceName.as_ptr()),
            ENUM_CURRENT_SETTINGS,
            &mut devmode,
        )
        .as_bool()
    };
    (available && devmode.dmDisplayFrequency > 0).then_some(devmode.dmDisplayFrequency)
}

pub(super) fn minimum_source_interval_90k_for_refresh(refresh_hz: u32) -> u64 {
    if refresh_hz == 0 {
        1
    } else {
        (VIDEO_CLOCK_HZ / (u64::from(refresh_hz) * 2)).max(1)
    }
}

pub(super) fn should_force_source_timed_idr(
    last_sync_or_request_90k: Option<u64>,
    current_timestamp_90k: u64,
) -> bool {
    last_sync_or_request_90k
        .is_none_or(|last| current_timestamp_90k.saturating_sub(last) >= REPLAY_IDR_INTERVAL_90K)
}

pub(super) fn capture_pool_size_for_route(
    width: u32,
    height: u32,
    route: VplRecordRoute,
    shared_cross_device: bool,
) -> usize {
    const MAX_SLOTS: usize = 32;
    const MIN_SLOTS: usize = 8;
    const SLOT_BUDGET_BYTES: u128 = 3 * 1024 * 1024 * 1024;

    let pixels = u128::from(width.max(1)).saturating_mul(u128::from(height.max(1)));
    let texture_bytes = match route.fourcc {
        MFX_FOURCC_NV12 => pixels.saturating_mul(3) / 2,
        MFX_FOURCC_P010 => pixels.saturating_mul(3),
        MFX_FOURCC_YUY2 => pixels.saturating_mul(2),
        MFX_FOURCC_Y210 => pixels.saturating_mul(4),
        MFX_FOURCC_AYUV | MFX_FOURCC_Y410 | MFX_FOURCC_RGB4 => pixels.saturating_mul(4),
        _ => pixels.saturating_mul(4),
    };
    let per_slot = texture_bytes.saturating_mul(if shared_cross_device { 2 } else { 1 });
    if per_slot == 0 {
        return MAX_SLOTS;
    }
    let budget_slots = (SLOT_BUDGET_BYTES / per_slot).min(usize::MAX as u128) as usize;
    budget_slots.clamp(MIN_SLOTS, MAX_SLOTS)
}

pub(super) fn wgc_coalesce_window_100ns_for_refresh(refresh_hz: u32) -> i64 {
    if refresh_hz == 0 {
        return 0;
    }

    // A display present remains outside this window, while cursor/window updates emitted
    // between presents are grouped and only the newest compositor image is retained.
    let nominal = 10_000_000u64 / u64::from(refresh_hz);
    nominal.saturating_mul(21).div_ceil(25).min(i64::MAX as u64) as i64
}

pub(super) fn should_coalesce_wgc_timestamps(
    previous_100ns: i64,
    current_100ns: i64,
    window: i64,
) -> bool {
    window > 0
        && current_100ns >= previous_100ns
        && current_100ns.saturating_sub(previous_100ns) < window
}

#[cfg(windows)]
pub(super) fn encoder_frame_rate_hint_from_output(
    output_desc: &windows::Win32::Graphics::Dxgi::DXGI_OUTPUT_DESC,
) -> (u32, u32, String) {
    if let Some(hz) = display_frequency_hz_from_output(output_desc) {
        (
            hz,
            1,
            format!("EnumDisplaySettingsW current dmDisplayFrequency={}Hz", hz),
        )
    } else {
        (
            VPL_ENCODER_FRAME_RATE_HINT_FALLBACK_N,
            VPL_ENCODER_FRAME_RATE_HINT_FALLBACK_D,
            "EnumDisplaySettingsW 未返回有效刷新率，使用 oneVPL 码控提示 fallback 60/1".to_owned(),
        )
    }
}

#[cfg(not(windows))]
pub fn record_d3d11_onecopy_mp4(
    adapter_index: u32,
    output: &Path,
    duration_seconds: f32,
    rate_control: &RateControlConfig,
    requested_chroma: ChromaSampling,
) -> Result<VplOneCopyRecordReport, BackendError> {
    let _ = (
        adapter_index,
        output,
        duration_seconds,
        rate_control,
        requested_chroma,
    );
    Err(BackendError::unsupported(
        "oneVPL D3D11 one-copy record",
        "Windows D3D11",
        "仅 Windows 可用",
    ))
}

#[cfg(not(windows))]
pub fn record_d3d11_onecopy_mp4_cancelable(
    adapter_index: u32,
    output: &Path,
    duration_seconds: f32,
    rate_control: &RateControlConfig,
    requested_chroma: ChromaSampling,
    external_stop: Option<std::sync::Arc<std::sync::atomic::AtomicBool>>,
) -> Result<VplOneCopyRecordReport, BackendError> {
    let _ = external_stop;
    record_d3d11_onecopy_mp4(
        adapter_index,
        output,
        duration_seconds,
        rate_control,
        requested_chroma,
    )
}

#[cfg(not(windows))]
pub fn record_wgc_d3d11_onecopy_mp4(
    adapter_index: u32,
    output: &Path,
    duration_seconds: f32,
    rate_control: &RateControlConfig,
    requested_chroma: ChromaSampling,
) -> Result<VplOneCopyRecordReport, BackendError> {
    let _ = (
        adapter_index,
        output,
        duration_seconds,
        rate_control,
        requested_chroma,
    );
    Err(BackendError::unsupported(
        "oneVPL WGC D3D11 one-copy record",
        "Windows.Graphics.Capture + Windows D3D11",
        "仅 Windows 可用",
    ))
}

#[cfg(not(windows))]
pub fn record_wgc_d3d11_onecopy_mp4_cancelable(
    adapter_index: u32,
    output: &Path,
    duration_seconds: f32,
    rate_control: &RateControlConfig,
    requested_chroma: ChromaSampling,
    external_stop: Option<std::sync::Arc<std::sync::atomic::AtomicBool>>,
) -> Result<VplOneCopyRecordReport, BackendError> {
    let _ = external_stop;
    record_wgc_d3d11_onecopy_mp4(
        adapter_index,
        output,
        duration_seconds,
        rate_control,
        requested_chroma,
    )
}
