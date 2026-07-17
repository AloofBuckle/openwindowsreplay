use super::*;

#[cfg(windows)]
pub fn record_nvfbc_mp4_output_cancelable(
    output: &Path,
    duration_seconds: f32,
    rate_control: &RateControlConfig,
    requested_chroma: ChromaSampling,
    external_stop: Option<std::sync::Arc<std::sync::atomic::AtomicBool>>,
    probe: &crate::backend::nvfbc::NvFbcProbeInfo,
) -> Result<VplOneCopyRecordOutput, BackendError> {
    record_nvfbc_impl(
        output,
        duration_seconds,
        rate_control,
        requested_chroma,
        external_stop,
        None,
        probe,
        true,
    )
}

#[cfg(windows)]
#[allow(clippy::too_many_arguments)]
pub fn record_nvfbc_memory_output_with_sink_cancelable(
    output: &Path,
    duration_seconds: f32,
    rate_control: &RateControlConfig,
    requested_chroma: ChromaSampling,
    external_stop: Option<std::sync::Arc<std::sync::atomic::AtomicBool>>,
    encoded_sink: Option<&mut dyn VplOneCopyRecordSink>,
    probe: &crate::backend::nvfbc::NvFbcProbeInfo,
) -> Result<VplOneCopyRecordOutput, BackendError> {
    record_nvfbc_impl(
        output,
        duration_seconds,
        rate_control,
        requested_chroma,
        external_stop,
        encoded_sink,
        probe,
        false,
    )
}

#[cfg(windows)]
#[allow(clippy::too_many_arguments)]
fn record_nvfbc_impl(
    output: &Path,
    duration_seconds: f32,
    rate_control: &RateControlConfig,
    requested_chroma: ChromaSampling,
    external_stop: Option<std::sync::Arc<std::sync::atomic::AtomicBool>>,
    encoded_sink: Option<&mut dyn VplOneCopyRecordSink>,
    probe: &crate::backend::nvfbc::NvFbcProbeInfo,
    write_output_mp4: bool,
) -> Result<VplOneCopyRecordOutput, BackendError> {
    let Some(encoded_sink) = encoded_sink else {
        return record_nvfbc_inner(
            output,
            duration_seconds,
            rate_control,
            requested_chroma,
            external_stop,
            None,
            None,
            None,
            probe,
            write_output_mp4,
        );
    };

    std::thread::scope(|scope| {
        let (tx, rx) =
            std::sync::mpsc::sync_channel(super::record_nvfbc_workers::NVFBC_SINK_QUEUE_CAPACITY);
        let failure = std::sync::Arc::new(std::sync::Mutex::new(None));
        let mut channel_sink =
            super::record_nvfbc_workers::NvFbcChannelSink::new(tx, failure.clone());
        let queue_stats = channel_sink.queue_stats();
        let publisher = scope.spawn(move || {
            super::record_nvfbc_workers::run_nvfbc_sink_publisher(rx, encoded_sink, queue_stats)
        });
        let audio_sink = channel_sink.clone();
        let sink_monitor = channel_sink.clone();
        let result = record_nvfbc_inner(
            output,
            duration_seconds,
            rate_control,
            requested_chroma,
            external_stop,
            Some(&mut channel_sink),
            Some(audio_sink),
            Some(sink_monitor),
            probe,
            write_output_mp4,
        );
        let queue_failure = channel_sink.failure();
        drop(channel_sink);
        publisher.join().map_err(|_| {
            BackendError::unsupported(
                "NvFBC encoded sink",
                "publisher thread",
                "有界发布线程 panic",
            )
        })?;
        if let Some(message) = queue_failure {
            return Err(BackendError::unsupported(
                "NvFBC encoded sink",
                "bounded publisher queue",
                message,
            ));
        }
        result
    })
}

#[cfg(windows)]
#[allow(clippy::too_many_arguments)]
fn record_nvfbc_inner(
    output: &Path,
    duration_seconds: f32,
    rate_control: &RateControlConfig,
    requested_chroma: ChromaSampling,
    external_stop: Option<std::sync::Arc<std::sync::atomic::AtomicBool>>,
    mut encoded_sink: Option<&mut dyn VplOneCopyRecordSink>,
    audio_sink: Option<super::record_nvfbc_workers::NvFbcChannelSink>,
    sink_monitor: Option<super::record_nvfbc_workers::NvFbcChannelSink>,
    probe: &crate::backend::nvfbc::NvFbcProbeInfo,
    write_output_mp4: bool,
) -> Result<VplOneCopyRecordOutput, BackendError> {
    use crate::backend::mp4_mux::{HevcCodecMetadata, HevcMp4Track, write_hevc_aac_mp4};
    use crate::backend::nvfbc::{NvFbcOptions, NvFbcRecorder};
    use std::sync::atomic::{AtomicBool, Ordering};
    use std::time::{Duration, Instant};

    if !probe.available
        || !probe
            .routes
            .iter()
            .any(|route| route.chroma == requested_chroma && route.supported)
    {
        return Err(BackendError::unsupported(
            "NvFBC 录制初始化",
            requested_chroma.doc_label(),
            probe
                .error
                .clone()
                .unwrap_or_else(|| "本次启动探测未确认该专用捕获 route".to_owned()),
        ));
    }
    if rate_control.look_ahead_depth != 0 {
        return Err(BackendError::unsupported(
            "NvFBC 录制初始化",
            format!("LookAheadDepth={}", rate_control.look_ahead_depth),
            "NvFBC 三表面直连路线不支持 Lookahead",
        ));
    }

    let record_started = Instant::now();
    let stop = external_stop.unwrap_or_else(|| std::sync::Arc::new(AtomicBool::new(false)));
    let capture_duration = Duration::from_secs_f32(duration_seconds.max(0.1));
    let requested_duration_90k =
        (duration_seconds.max(0.1) as f64 * VIDEO_CLOCK_HZ as f64).round() as u64;
    let retain_output_samples = write_output_mp4 || encoded_sink.is_none();
    let mut notes = vec![
        "NvFBC 专用路线：NvFBC V3 直接写三个 D3D9Ex ARGB10 surface，NVENC 直接注册同一 surface；应用不执行额外 GPU 内容复制".to_owned(),
        format!(
            "NvFBC 本次启动能力：version={} capture={}x{} chroma={} lookahead_policy={}",
            probe.nvfbc_version,
            probe.capture_width,
            probe.capture_height,
            requested_chroma.doc_label(),
            probe.lookahead_policy
        ),
    ];
    sink_status(
        &mut encoded_sink,
        format!(
            "NvFBC 录制后端初始化开始：chroma={} rc={}",
            requested_chroma.doc_label(),
            rate_control.method.short_name()
        ),
    );

    let adapter_index = probe
        .adapter
        .as_ref()
        .map(|adapter| adapter.d3d9_adapter_index);
    let mut recorder = NvFbcRecorder::open(NvFbcOptions {
        adapter_index,
        chroma: requested_chroma,
        capture_cursor: true,
        rate_control: rate_control.clone(),
    })?;
    let (width_u32, height_u32) = recorder.dimensions();
    let width = u16::try_from(width_u32).map_err(|_| {
        BackendError::unsupported(
            "NvFBC MP4 track",
            format!("width={width_u32}"),
            "MP4 轨道模型当前只接受 u16 尺寸",
        )
    })?;
    let height = u16::try_from(height_u32).map_err(|_| {
        BackendError::unsupported(
            "NvFBC MP4 track",
            format!("height={height_u32}"),
            "MP4 轨道模型当前只接受 u16 尺寸",
        )
    })?;
    let color = recorder.color();
    let codec = match requested_chroma {
        ChromaSampling::Yuv420 => HevcCodecMetadata::main10_420_10(),
        ChromaSampling::Yuv422 => HevcCodecMetadata::rext(2, 10),
        ChromaSampling::Yuv444 => HevcCodecMetadata::rext(3, 10),
    };
    if let Some(sink) = encoded_sink.as_deref_mut() {
        sink.video_track_started(VplOutputTrackInfo {
            width,
            height,
            color,
            codec,
        });
    }
    sink_status(
        &mut encoded_sink,
        format!(
            "NvFBC/NVENC D3D9Ex session 已建立：{}x{}，累计 {:.1}ms",
            width,
            height,
            record_started.elapsed().as_secs_f64() * 1000.0
        ),
    );

    let audio_worker = super::record_nvfbc_workers::NvFbcAudioWorker::spawn(
        capture_duration + Duration::from_secs(5),
        retain_output_samples,
        audio_sink,
    )?;
    let mut audio_worker = Some(audio_worker);
    let mut samples = Vec::new();
    let mut encoded_stats = RecordHevcStats::default();
    let mut captured_frames = 0u32;
    let mut first_video_timestamp_100ns = None;
    let mut last_capture_timestamp_90k = None;
    let mut last_capture_interval_90k = None;
    let mut last_forced_idr_timestamp_90k = None;
    let mut last_source_pid = 0u32;
    let mut last_wait_mode = 0u32;
    let mut vblank_skips = 0u64;
    let mut max_vblank_ticks = 0u64;
    let mut cadence_one = 0u64;
    let mut cadence_two = 0u64;
    let mut cadence_three_plus = 0u64;
    let expected_interval_90k = probe
        .display
        .as_ref()
        .filter(|display| display.refresh_numerator > 0)
        .map(|display| {
            (VIDEO_CLOCK_HZ * u64::from(display.refresh_denominator.max(1)))
                .div_ceil(u64::from(display.refresh_numerator))
                .max(1)
        })
        .unwrap_or(1);

    while !stop.load(Ordering::Relaxed) {
        let force_idr = last_capture_timestamp_90k.is_none_or(|timestamp| {
            should_force_source_timed_idr(last_forced_idr_timestamp_90k, timestamp)
        });
        let frame = recorder.capture_next_with_force_idr(force_idr)?;
        if force_idr {
            last_forced_idr_timestamp_90k = Some(frame.capture_timestamp_90k);
        }
        if first_video_timestamp_100ns.is_none() {
            first_video_timestamp_100ns = Some(frame.capture_timestamp_100ns);
            if let Some(worker) = audio_worker.as_ref() {
                worker.set_video_start(frame.capture_timestamp_100ns)?;
            }
            sink_status(
                &mut encoded_sink,
                format!(
                    "初始化阶段结束：首个正式 NvFBC 帧进入 NVENC，累计 {:.1}ms",
                    record_started.elapsed().as_secs_f64() * 1000.0
                ),
            );
        }
        if let Some(previous) = last_capture_timestamp_90k {
            let delta = frame.capture_timestamp_90k.saturating_sub(previous);
            last_capture_interval_90k = Some(delta.max(1));
            let multiple = delta
                .saturating_add(expected_interval_90k / 2)
                .checked_div(expected_interval_90k)
                .unwrap_or(1)
                .max(1);
            match multiple {
                1 => cadence_one = cadence_one.saturating_add(1),
                2 => cadence_two = cadence_two.saturating_add(1),
                _ => cadence_three_plus = cadence_three_plus.saturating_add(1),
            }
        }
        last_capture_timestamp_90k = Some(frame.capture_timestamp_90k);
        vblank_skips = vblank_skips.saturating_add(frame.vblank_ticks.saturating_sub(1));
        max_vblank_ticks = max_vblank_ticks.max(frame.vblank_ticks);
        last_source_pid = frame.source_pid;
        last_wait_mode = frame.wait_mode_used;
        captured_frames = captured_frames.saturating_add(1);
        for sample in frame.access_units {
            push_record_hevc_sample(
                &mut samples,
                sample,
                &mut encoded_sink,
                retain_output_samples,
                &mut encoded_stats,
            );
        }
        if let Some(message) = sink_monitor.as_ref().and_then(|sink| sink.failure()) {
            return Err(BackendError::unsupported(
                "NvFBC encoded sink",
                "bounded publisher queue",
                message,
            ));
        }
        if let Some(worker) = audio_worker.as_ref() {
            worker.check()?;
        }
        if frame.capture_timestamp_90k >= requested_duration_90k {
            break;
        }
    }

    if stop.load(Ordering::Relaxed) {
        if let Some(worker) = audio_worker.take() {
            worker.abort();
        }
        drop(recorder);
        sink_status(
            &mut encoded_sink,
            format!(
                "NvFBC 录制快速停止：captured_frames={} total={:.1}ms",
                captured_frames,
                record_started.elapsed().as_secs_f64() * 1000.0
            ),
        );
        return Err(BackendError::cancelled("NvFBC 录制循环"));
    }

    let (finish_samples, nvenc_stats) = recorder.finish_with_stats()?;
    for sample in finish_samples {
        push_record_hevc_sample(
            &mut samples,
            sample,
            &mut encoded_sink,
            retain_output_samples,
            &mut encoded_stats,
        );
    }
    if encoded_stats.encoded_samples == 0 {
        return Err(BackendError::unsupported(
            "NvFBC NVENC encode",
            requested_chroma.doc_label(),
            "录制结束后没有可封装 HEVC access unit",
        ));
    }
    // NvFBC supplies arrival timestamps but no explicit duration for the final
    // frame. Reuse the last observed source interval for that tail instead of
    // manufacturing a one-tick (1/90000s) sample or extending to a CFR target.
    let duration_90k = encoded_stats
        .last_timestamp_90k
        .map(|timestamp| {
            timestamp.saturating_add(
                last_capture_interval_90k
                    .unwrap_or(expected_interval_90k)
                    .max(1),
            )
        })
        .unwrap_or(requested_duration_90k.max(1));
    let audio_output = audio_worker
        .take()
        .expect("NvFBC audio worker is present until normal finish")
        .finish(duration_90k)?;
    notes.extend(audio_output.notes);
    let audio_track = audio_output.track;
    let audio_access_units = audio_output.access_units;
    let audio_encoded_bytes = audio_output.encoded_bytes;
    if let Some(message) = sink_monitor.as_ref().and_then(|sink| sink.failure()) {
        return Err(BackendError::unsupported(
            "NvFBC encoded sink",
            "bounded publisher queue",
            message,
        ));
    }
    if let Some(sink) = sink_monitor.as_ref() {
        notes.push(format!(
            "NvFBC encoded sink：bounded_capacity={} queue_high_water={}",
            super::record_nvfbc_workers::NVFBC_SINK_QUEUE_CAPACITY,
            sink.queue_high_water()
        ));
    }

    let video_track = HevcMp4Track {
        width,
        height,
        duration_90k,
        color,
        codec,
        samples,
    };
    if write_output_mp4 {
        write_hevc_aac_mp4(output, &video_track, audio_track.as_ref())?;
    } else {
        notes.push(format!(
            "NvFBC 生产会话以内存 encoded ring 为主，跳过临时 MP4 写出：{}",
            output.display()
        ));
    }
    notes.push(format!(
        "NvFBC VFR 时间线使用独立 DXGI vblank waiter 的绝对 QPC，同时派生 90k 相对时间戳；source_pid={} wait_mode={} periodic_idr={}ms",
        last_source_pid,
        last_wait_mode,
        REPLAY_IDR_INTERVAL_90K * 1000 / VIDEO_CLOCK_HZ
    ));
    let completion_latency_avg_us = nvenc_stats
        .completion_latency_us
        .checked_div(nvenc_stats.completed_frames)
        .unwrap_or(0);
    let completion_wait_avg_us = nvenc_stats
        .completion_wait_us
        .checked_div(nvenc_stats.completion_waits)
        .unwrap_or(0);
    notes.push(format!(
        "NvFBC cadence audit：expected_interval_90k={} 1x={} 2x={} 3x+={} vblank_skips={} max_vblank_ticks={}",
        expected_interval_90k,
        cadence_one,
        cadence_two,
        cadence_three_plus,
        vblank_skips,
        max_vblank_ticks
    ));
    notes.push(format!(
        "NvFBC NVENC completion：async={} fallback={:?} frames={} latency_avg={}us latency_max={}us blocking_waits={} wait_avg={}us wait_max={}us",
        nvenc_stats.async_encode,
        nvenc_stats.async_fallback,
        nvenc_stats.completed_frames,
        completion_latency_avg_us,
        nvenc_stats.completion_latency_max_us,
        nvenc_stats.completion_waits,
        completion_wait_avg_us,
        nvenc_stats.completion_wait_max_us
    ));

    let display = probe.display.as_ref();
    let report = VplOneCopyRecordReport {
        adapter_index: display
            .map(|value| value.dxgi_adapter_index)
            .unwrap_or_default(),
        output_index: display.map(|value| value.output_index).unwrap_or_default(),
        adapter_luid: probe
            .adapter
            .as_ref()
            .map(|value| format!("NvFBC:D3D9:{}", value.d3d9_adapter_index))
            .unwrap_or_else(|| "NvFBC:D3D9".to_owned()),
        output_path: output.display().to_string(),
        width,
        height,
        duration_seconds: duration_90k as f32 / VIDEO_CLOCK_HZ as f32,
        captured_frames,
        encoded_samples: encoded_stats.encoded_samples,
        encoded_bytes: encoded_stats.encoded_bytes,
        audio_access_units,
        audio_encoded_bytes,
        dda_timeouts: 0,
        input_dxgi_format: 0,
        target_dxgi_format: 0,
        query_status: 0,
        init_status: 0,
        close_status: 0,
        first_get_surface_status: 0,
        video_processor_format_flags_in: 0,
        video_processor_format_flags_out: 0,
        notes,
    };
    sink_status(
        &mut encoded_sink,
        format!(
            "NvFBC 录制段正常结束：captured_frames={} video_au={} audio_au={} duration={:.3}s total={:.1}ms",
            report.captured_frames,
            report.encoded_samples,
            report.audio_access_units,
            report.duration_seconds,
            record_started.elapsed().as_secs_f64() * 1000.0
        ),
    );
    Ok(VplOneCopyRecordOutput {
        report,
        video_track,
        audio_track,
    })
}
