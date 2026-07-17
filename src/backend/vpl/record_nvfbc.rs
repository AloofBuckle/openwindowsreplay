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
    mut encoded_sink: Option<&mut dyn VplOneCopyRecordSink>,
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

    let mut audio_capture = RecordAudioCapture::start(
        capture_duration + Duration::from_secs(5),
        retain_output_samples,
        &mut notes,
    );
    let mut samples = Vec::new();
    let mut encoded_stats = RecordHevcStats::default();
    let mut captured_frames = 0u32;
    let mut first_video_timestamp_100ns = None;
    let mut last_capture_timestamp_90k = None;
    let mut last_forced_idr_timestamp_90k = None;
    let mut last_source_pid = 0u32;
    let mut last_wait_mode = 0u32;

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
            sink_status(
                &mut encoded_sink,
                format!(
                    "初始化阶段结束：首个正式 NvFBC 帧进入 NVENC，累计 {:.1}ms",
                    record_started.elapsed().as_secs_f64() * 1000.0
                ),
            );
        }
        last_capture_timestamp_90k = Some(frame.capture_timestamp_90k);
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
        if let Some(capture) = audio_capture.as_mut() {
            capture.poll_live_aac(
                first_video_timestamp_100ns,
                last_capture_timestamp_90k,
                &mut encoded_sink,
                &mut notes,
            )?;
        }
        if frame.capture_timestamp_90k >= requested_duration_90k {
            break;
        }
    }

    if stop.load(Ordering::Relaxed) {
        if let Some(capture) = audio_capture.as_mut() {
            capture.stop_without_reencode(&mut notes);
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

    for sample in recorder.finish()? {
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
    let duration_90k = encoded_timeline_duration_90k(
        &samples,
        encoded_stats.last_timestamp_90k,
        requested_duration_90k,
        false,
    );
    let (audio_track, audio_access_units, audio_encoded_bytes) =
        if let Some(capture) = audio_capture.as_mut() {
            if retain_output_samples {
                capture.finish_live_aac(&mut encoded_sink, &mut notes)?;
                let sink_push_from_ticks = capture.live_pushed_until_ticks();
                let audio_frames = capture.finish(&mut notes);
                let track = build_record_aac_track(
                    audio_frames,
                    first_video_timestamp_100ns,
                    duration_90k,
                    &mut notes,
                    &mut encoded_sink,
                    sink_push_from_ticks,
                )?;
                let access_units = track
                    .as_ref()
                    .map(|track| track.samples.len().min(u32::MAX as usize) as u32)
                    .unwrap_or(0);
                let bytes = track
                    .as_ref()
                    .map(|track| {
                        track
                            .samples
                            .iter()
                            .map(|sample| sample.data.len() as u64)
                            .sum()
                    })
                    .unwrap_or(0);
                (track, access_units, bytes)
            } else {
                let track = capture.finish_streaming(
                    first_video_timestamp_100ns,
                    duration_90k,
                    &mut encoded_sink,
                    &mut notes,
                )?;
                (
                    track,
                    capture.live_access_units(),
                    capture.live_encoded_bytes(),
                )
            }
        } else {
            (None, 0, 0)
        };

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
        "NvFBC VFR 时间线使用成功 Grab 后的绝对 QPC，同时派生 90k 相对时间戳；source_pid={} wait_mode={} periodic_idr={}ms",
        last_source_pid,
        last_wait_mode,
        REPLAY_IDR_INTERVAL_90K * 1000 / VIDEO_CLOCK_HZ
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
