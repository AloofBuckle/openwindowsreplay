use super::*;

#[cfg(windows)]
#[allow(clippy::too_many_arguments)]
pub fn record_nvenc_d3d11_onecopy_memory_output_with_sink_cancelable(
    adapter_index: u32,
    output: &Path,
    duration_seconds: f32,
    rate_control: &RateControlConfig,
    requested_chroma: ChromaSampling,
    external_stop: Option<std::sync::Arc<std::sync::atomic::AtomicBool>>,
    encoded_sink: Option<&mut dyn VplOneCopyRecordSink>,
    route_plan: Option<&crate::backend::nvenc::NvencCurrentDisplayRouteInfo>,
) -> Result<VplOneCopyRecordOutput, BackendError> {
    record_nvenc_d3d11_onecopy_mp4_impl(
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
#[allow(clippy::too_many_arguments)]
pub fn record_nvenc_d3d11_onecopy_mp4_output_cancelable(
    adapter_index: u32,
    output: &Path,
    duration_seconds: f32,
    rate_control: &RateControlConfig,
    requested_chroma: ChromaSampling,
    external_stop: Option<std::sync::Arc<std::sync::atomic::AtomicBool>>,
    route_plan: Option<&crate::backend::nvenc::NvencCurrentDisplayRouteInfo>,
) -> Result<VplOneCopyRecordOutput, BackendError> {
    record_nvenc_d3d11_onecopy_mp4_impl(
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
#[allow(clippy::too_many_arguments)]
pub fn record_nvenc_wgc_d3d11_onecopy_memory_output_with_sink_cancelable(
    adapter_index: u32,
    output: &Path,
    duration_seconds: f32,
    rate_control: &RateControlConfig,
    requested_chroma: ChromaSampling,
    external_stop: Option<std::sync::Arc<std::sync::atomic::AtomicBool>>,
    encoded_sink: Option<&mut dyn VplOneCopyRecordSink>,
    route_plan: Option<&crate::backend::nvenc::NvencCurrentDisplayRouteInfo>,
) -> Result<VplOneCopyRecordOutput, BackendError> {
    record_nvenc_d3d11_onecopy_mp4_impl(
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
#[allow(clippy::too_many_arguments)]
pub fn record_nvenc_wgc_d3d11_onecopy_mp4_output_cancelable(
    adapter_index: u32,
    output: &Path,
    duration_seconds: f32,
    rate_control: &RateControlConfig,
    requested_chroma: ChromaSampling,
    external_stop: Option<std::sync::Arc<std::sync::atomic::AtomicBool>>,
    route_plan: Option<&crate::backend::nvenc::NvencCurrentDisplayRouteInfo>,
) -> Result<VplOneCopyRecordOutput, BackendError> {
    record_nvenc_d3d11_onecopy_mp4_impl(
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
#[allow(clippy::too_many_arguments)]
pub(super) fn record_nvenc_d3d11_onecopy_mp4_impl(
    adapter_index: u32,
    output: &Path,
    duration_seconds: f32,
    rate_control: &RateControlConfig,
    requested_chroma: ChromaSampling,
    capture_source: RecordCaptureSource,
    external_stop: Option<std::sync::Arc<std::sync::atomic::AtomicBool>>,
    write_output_mp4: bool,
    mut encoded_sink: Option<&mut dyn VplOneCopyRecordSink>,
    route_plan: Option<&crate::backend::nvenc::NvencCurrentDisplayRouteInfo>,
) -> Result<VplOneCopyRecordOutput, BackendError> {
    use crate::backend::mp4_mux::{HevcMp4Track, write_hevc_aac_mp4};
    use std::time::{Duration, Instant};
    use windows::Win32::Graphics::Direct3D11::{
        D3D11_TEXTURE2D_DESC, D3D11_USAGE_DEFAULT, ID3D11Texture2D,
    };
    use windows::Win32::Graphics::Dxgi::Common::DXGI_SAMPLE_DESC;
    use windows::Win32::Graphics::Dxgi::{CreateDXGIFactory1, IDXGIFactory1};

    if requested_chroma == ChromaSampling::Yuv422 {
        return Err(BackendError::unsupported(
            "NVENC 录制",
            requested_chroma.doc_label(),
            "NV16/P210 没有可直接注册且保持平面语义的原生 DXGI texture layout，4:2:2 继续保持 probe-only",
        ));
    }
    if let Err(err) = rate_control.to_nvenc_fields() {
        return Err(BackendError::unsupported(
            "NVENC 码控",
            rate_control.method.short_name(),
            err,
        ));
    }

    let record_started = Instant::now();
    let record_stop = external_stop
        .clone()
        .unwrap_or_else(|| std::sync::Arc::new(std::sync::atomic::AtomicBool::new(false)));
    sink_status(
        &mut encoded_sink,
        format!(
            "NVENC 录制后端初始化开始：capture={} chroma={} rc={}",
            capture_source.label(),
            requested_chroma.doc_label(),
            rate_control.method.short_name()
        ),
    );
    if record_stop.load(std::sync::atomic::Ordering::Relaxed) {
        return Err(BackendError::unsupported(
            "NVENC 录制后端初始化",
            "用户停止请求",
            "停止请求发生在 NVENC/D3D11 初始化前，已中止当前片段",
        ));
    }

    let mut notes = vec![
        "NVENC production path: WGC converts directly into pooled registered inputs; DDA uses keyed shared snapshots plus one GPU compatibility copy because NVENC rejects keyed shared input; no CPU Map/staging/raw-frame fallback".to_owned(),
        format!(
            "NVENC rate-control request accepted and written to NV_ENC_RC_PARAMS: {}",
            rate_control.method.short_name()
        ),
    ];
    unsafe {
        let _thread_priority = match capture_source {
            RecordCaptureSource::Dda => RecordThreadPriorityGuard::raise(&mut notes),
            RecordCaptureSource::Wgc => {
                notes.push(
                    "WGC/NVENC 录制主线程/持久 MTA 捕获服务固定保持普通 CPU 优先级".to_owned(),
                );
                None
            }
        };
        let factory: IDXGIFactory1 =
            CreateDXGIFactory1().map_err(|err| BackendError::WindowsApi {
                func: "CreateDXGIFactory1(NVENC record)",
                message: err.to_string(),
            })?;
        let adapter1 =
            factory
                .EnumAdapters1(adapter_index)
                .map_err(|err| BackendError::WindowsApi {
                    func: "IDXGIFactory1::EnumAdapters1(NVENC record)",
                    message: err.to_string(),
                })?;
        let desc = adapter1
            .GetDesc1()
            .map_err(|err| BackendError::WindowsApi {
                func: "IDXGIAdapter1::GetDesc1(NVENC record)",
                message: err.to_string(),
            })?;
        let adapter_luid = format!(
            "{:08X}:{:08X}",
            desc.AdapterLuid.HighPart as u32, desc.AdapterLuid.LowPart
        );
        let output0 = adapter1
            .EnumOutputs(0)
            .map_err(|err| BackendError::WindowsApi {
                func: "IDXGIAdapter1::EnumOutputs(0 NVENC record)",
                message: err.to_string(),
            })?;
        let output_desc = output0.GetDesc().map_err(|err| BackendError::WindowsApi {
            func: "IDXGIOutput::GetDesc(NVENC record)",
            message: err.to_string(),
        })?;
        let (capture_width, capture_height, capture_dimensions_note) =
            capture_dimensions_from_output(&output_desc);
        let aligned_width = align16(capture_width);
        let aligned_height = align16(capture_height);
        notes.push(format!(
            "NVENC direct-input dimensions: {capture_dimensions_note}; aligned={}x{}",
            aligned_width, aligned_height
        ));
        let (encoder_frame_rate_n, encoder_frame_rate_d, encoder_frame_rate_note) =
            encoder_frame_rate_hint_from_output(&output_desc);
        notes.push(format!(
            "NVENC frameRate 仅作为编码器码控提示：{}/{}；来源={}；正式 MP4 时间戳保持 DDA/WGC 源 VFR 节奏",
            encoder_frame_rate_n, encoder_frame_rate_d, encoder_frame_rate_note
        ));

        let Some(plan) = route_plan else {
            return Err(BackendError::unsupported(
                "NVENC 录制 RoutePlan",
                requested_chroma.doc_label(),
                "缺少能力探测阶段的当前显示器 NVENC route；请重新探测能力",
            ));
        };
        validate_current_nvenc_route_plan(
            plan,
            adapter_index,
            &output_desc,
            &output0,
            requested_chroma,
        )?;
        let record_route = record_route_from_nvenc_display_plan(plan)?;
        let nvenc_input_format = nvenc_input_format_from_route(record_route)?;
        notes.push(format!(
            "NVENC record RoutePlan accepted: adapter={} output={} ColorSpace={} BitsPerColor={} rect={},{},{},{} input={} route={}",
            plan.adapter_index,
            plan.output_index,
            plan.color_space,
            plan.bits_per_color,
            plan.desktop_left,
            plan.desktop_top,
            plan.desktop_right,
            plan.desktop_bottom,
            plan.input_format,
            record_route.summary()
        ));

        let init_started = Instant::now();
        let mut nvenc_encoder = crate::backend::nvenc::NvencD3d11Encoder::open_with_rate_control(
            adapter_index,
            aligned_width as u32,
            aligned_height as u32,
            nvenc_input_format,
            record_route.mp4_color,
            rate_control,
            encoder_frame_rate_n,
            encoder_frame_rate_d,
        )?;
        let encoder_device = nvenc_encoder.device().clone();
        let immediate = nvenc_encoder.context().clone();
        notes.push(format!(
            "NVENC D3D11 registration mode: {}",
            nvenc_encoder.registration_mode()
        ));
        if capture_source.is_wgc() {
            // Lowest priority (-7) can starve capture conversion/NVENC behind a 240 Hz
            // browser or game workload and exhaust the snapshot pool. Normal priority keeps
            // capture schedulable without promoting it above the foreground application.
            let priority = WGC_GPU_THREAD_PRIORITY;
            match set_d3d11_gpu_thread_priority(&encoder_device, priority) {
                Ok(()) => notes.push(format!(
                    "WGC/NVENC D3D11 device GPU thread priority set to {priority}"
                )),
                Err(err) => notes.push(format!(
                    "WGC/NVENC D3D11 device GPU thread priority {priority} failed: {err}"
                )),
            }
        }
        sink_status(
            &mut encoded_sink,
            format!(
                "初始化阶段：NVENC D3D11 encoder/session 完成 input={}，累计 {:.1}ms，本阶段 {:.1}ms",
                nvenc_input_format.label(),
                record_started.elapsed().as_secs_f64() * 1000.0,
                init_started.elapsed().as_secs_f64() * 1000.0
            ),
        );
        if let Some(sink) = encoded_sink.as_deref_mut() {
            sink.video_track_started(VplOutputTrackInfo {
                width: capture_width,
                height: capture_height,
                color: record_route.mp4_color,
                codec: record_route.mp4_codec,
            });
        }

        let record_route_dxgi_format = record_route.try_dxgi_format()?;
        let target_desc = D3D11_TEXTURE2D_DESC {
            Width: aligned_width as u32,
            Height: aligned_height as u32,
            MipLevels: 1,
            ArraySize: 1,
            Format: record_route_dxgi_format,
            SampleDesc: DXGI_SAMPLE_DESC {
                Count: 1,
                Quality: 0,
            },
            Usage: D3D11_USAGE_DEFAULT,
            BindFlags: 0,
            CPUAccessFlags: 0,
            MiscFlags: 0,
        };
        let dda_target_texture = if capture_source == RecordCaptureSource::Dda {
            let mut texture: Option<ID3D11Texture2D> = None;
            encoder_device
                .CreateTexture2D(&target_desc, None, Some(&mut texture))
                .map_err(|err| BackendError::WindowsApi {
                    func: "ID3D11Device::CreateTexture2D(NVENC DDA compatibility input)",
                    message: err.to_string(),
                })?;
            Some(texture.ok_or_else(|| BackendError::WindowsApi {
                func: "CreateTexture2D(NVENC DDA compatibility input)",
                message: "返回空纹理".to_owned(),
            })?)
        } else {
            None
        };
        sink_status(
            &mut encoded_sink,
            format!(
                "初始化阶段：NVENC 输入槽位格式就绪 input={} {}x{} dda_copy_target={}，累计 {:.1}ms",
                nvenc_input_format.label(),
                target_desc.Width,
                target_desc.Height,
                dda_target_texture.is_some(),
                record_started.elapsed().as_secs_f64() * 1000.0
            ),
        );
        match capture_source {
            RecordCaptureSource::Dda => notes.push(
                "NVENC DDA 兼容路线：独立 D3D11 capture device 获取并转换到 keyed shared YUV snapshot；NVIDIA 驱动不接受 keyed shared texture 直接编码，编码端保留一次 GPU CopyResource 到普通 NVENC input"
                    .to_owned(),
            ),
            RecordCaptureSource::Wgc => notes.push(
                "NVENC WGC 路线：使用 NVENC D3D11 device 创建 WGC capture，每个本地 YUV 槽位自带 route converter；shader 直接写最终槽位，等待 GPU fence 后直接注册为 NVENC input，全程不再复制"
                    .to_owned(),
            ),
        }

        let mut samples = Vec::new();
        let mut encoded_stats = RecordHevcStats::default();
        let retain_output_samples = write_output_mp4 || encoded_sink.is_none();
        let mut captured_frames = 0u32;
        let mut warmup_encoded_frames = 0u32;
        let mut dda_timeouts = 0u32;
        let mut input_dxgi_format = 0u32;
        let format_flags_in = 0u32;
        let format_flags_out = 0u32;
        let mut perf = RecordPerf::default();
        let mut dirty_metadata_frames = 0u32;
        let partial_convert_frames = 0u32;
        let mut full_convert_frames = 0u32;
        let mut move_metadata_frames = 0u32;
        let mut dirty_area_total = 0u64;
        let mut first_sample_timestamp_90k: Option<u64> = None;
        let mut first_video_timestamp_100ns: Option<i64> = None;
        let mut last_submitted_sample_timestamp_90k: Option<u64> = None;
        let capture_duration = Duration::from_secs_f32(duration_seconds.max(0.1));
        let requested_duration_90k =
            (duration_seconds.max(0.1) as f64 * VIDEO_CLOCK_HZ as f64).round() as u64;
        let source_stop_90k = requested_duration_90k;
        let start = Instant::now();
        let end_at = start + capture_duration;
        let qpc_frequency = query_performance_frequency().unwrap_or(0);
        let audio_started = Instant::now();
        let mut audio_capture =
            RecordAudioCapture::start(capture_duration + Duration::from_secs(5), &mut notes);
        sink_status(
            &mut encoded_sink,
            format!(
                "初始化阶段：音频线程启动阶段完成 enabled={}，累计 {:.1}ms，本阶段 {:.1}ms",
                audio_capture.is_some(),
                record_started.elapsed().as_secs_f64() * 1000.0,
                audio_started.elapsed().as_secs_f64() * 1000.0
            ),
        );

        {
            let capture_pool_size = 32usize;
            let capture_queue_size = capture_pool_size;
            let (frame_tx, frame_rx) = std::sync::mpsc::channel::<CaptureMsg>();
            let (free_tx, free_rx) = std::sync::mpsc::channel::<CaptureFrameSlot>();
            let capture_stop = std::sync::Arc::new(std::sync::atomic::AtomicBool::new(false));
            let stop = capture_stop.clone();
            let capture_thread_started = Instant::now();
            let capture_handle = match capture_source {
                RecordCaptureSource::Dda => {
                    let (capture_device, capture_context) =
                        create_d3d11_device_for_adapter(&adapter1)?;
                    spawn_dda_capture_thread(
                        adapter1.clone(),
                        capture_device,
                        capture_context,
                        encoder_device.clone(),
                        None,
                        start,
                        end_at,
                        source_stop_90k,
                        qpc_frequency,
                        record_route,
                        target_desc.Width,
                        target_desc.Height,
                        capture_pool_size,
                        stop.clone(),
                        frame_tx,
                        free_rx,
                    )
                }
                RecordCaptureSource::Wgc => spawn_wgc_capture_thread(
                    adapter1.clone(),
                    encoder_device.clone(),
                    start,
                    end_at,
                    source_stop_90k,
                    record_route,
                    target_desc.Width,
                    target_desc.Height,
                    capture_pool_size,
                    stop.clone(),
                    frame_tx,
                    free_rx,
                ),
            };
            let mut capture_thread = CaptureThreadGuard::new(stop.clone(), capture_handle);
            sink_status(
                &mut encoded_sink,
                format!(
                    "初始化阶段：{} capture thread 已启动，累计 {:.1}ms，本阶段 {:.1}ms；后续若仍无正式帧，多半处于 capture/warmup 阶段",
                    capture_source.label(),
                    record_started.elapsed().as_secs_f64() * 1000.0,
                    capture_thread_started.elapsed().as_secs_f64() * 1000.0
                ),
            );
            notes.push(format!(
                "NVENC capture snapshot pool: textures={}, queue={}",
                capture_pool_size, capture_queue_size
            ));

            let mut capture_stats: Option<CaptureStats> = None;
            let mut capture_error: Option<String> = None;
            let mut active_source_desc: Option<D3D11_TEXTURE2D_DESC> = None;

            loop {
                if record_stop.load(std::sync::atomic::Ordering::Relaxed) {
                    stop.store(true, std::sync::atomic::Ordering::Relaxed);
                    break;
                }
                let msg = match frame_rx.recv_timeout(Duration::from_millis(2)) {
                    Ok(msg) => msg,
                    Err(std::sync::mpsc::RecvTimeoutError::Timeout) => {
                        if let Some(capture) = audio_capture.as_mut() {
                            capture.poll_live_aac(
                                first_video_timestamp_100ns,
                                last_submitted_sample_timestamp_90k,
                                &mut encoded_sink,
                                &mut notes,
                            );
                        }
                        continue;
                    }
                    Err(std::sync::mpsc::RecvTimeoutError::Disconnected) => break,
                };

                match msg {
                    CaptureMsg::Frame(captured) => {
                        let frame_result = (|| -> Result<(), BackendError> {
                            let frame_started = Instant::now();
                            let CapturedSnapshot {
                                slot,
                                source_desc,
                                move_rect_bytes,
                                dirty_rects,
                                timestamp_90k,
                                timestamp_100ns,
                                capture_index: _,
                                accumulated_frames,
                                warmup,
                            } = captured;
                            if input_dxgi_format == 0 {
                                input_dxgi_format = source_desc.Format.0 as u32;
                            }
                            perf.dda_accumulated_frames_total += u64::from(accumulated_frames);
                            perf.dda_accumulated_frames_max =
                                perf.dda_accumulated_frames_max.max(accumulated_frames);

                            let source_changed = active_source_desc.is_some_and(|prev| {
                                prev.Width != source_desc.Width
                                    || prev.Height != source_desc.Height
                                    || prev.Format.0 != source_desc.Format.0
                            });
                            if source_changed {
                                notes.push(format!(
                                    "NVENC direct-input surface pool changed to {}x{} DXGI_FORMAT({})",
                                    source_desc.Width, source_desc.Height, source_desc.Format.0
                                ));
                            }
                            active_source_desc = Some(source_desc);

                            let init_started = Instant::now();
                            let direct_route_surface = source_desc.Format.0
                                == record_route_dxgi_format.0
                                && source_desc.Width == target_desc.Width
                                && source_desc.Height == target_desc.Height;
                            if !direct_route_surface {
                                return Err(BackendError::unsupported(
                                    "NVENC direct registered input",
                                    format!(
                                        "capture={}x{} DXGI_FORMAT({}), expected={}x{} DXGI_FORMAT({})",
                                        source_desc.Width,
                                        source_desc.Height,
                                        source_desc.Format.0,
                                        target_desc.Width,
                                        target_desc.Height,
                                        record_route_dxgi_format.0,
                                    ),
                                    "捕获线程必须产出与 NVENC 输入完全一致的最终 route YUV surface",
                                ));
                            }
                            perf.init.add(init_started.elapsed());

                            if !dirty_rects.is_empty() {
                                dirty_metadata_frames += 1;
                                dirty_area_total += dirty_rect_area(&dirty_rects);
                            }
                            if move_rect_bytes > 0 {
                                move_metadata_frames += 1;
                            }
                            full_convert_frames = full_convert_frames.saturating_add(1);

                            let fence_started = Instant::now();
                            let mut keyed_mutex_guard = None;
                            let direct_input = match &slot {
                                CaptureFrameSlot::Shared(shared) => {
                                    keyed_mutex_guard = Some(KeyedMutexGuard::acquire(
                                        &shared.encoder_mutex,
                                        1,
                                        0,
                                        1_000,
                                        "IDXGIKeyedMutex::AcquireSync(NVENC DDA snapshot)",
                                    )?);
                                    let target = dda_target_texture.as_ref().ok_or_else(|| {
                                        BackendError::unsupported(
                                            "NVENC DDA compatibility copy",
                                            "missing ordinary input texture",
                                            "DDA shared snapshot cannot be submitted directly to this NVENC driver",
                                        )
                                    })?;
                                    let copy_started = Instant::now();
                                    let copy_result = copy_texture_resource(
                                        &immediate,
                                        &shared.encoder_texture,
                                        target,
                                    );
                                    perf.copy.add(copy_started.elapsed());
                                    copy_result?;
                                    target.clone()
                                }
                                CaptureFrameSlot::WgcLocal(local) => {
                                    local.capture_fence.wait_ready(&immediate)?;
                                    local.texture.clone()
                                }
                            };
                            perf.source_fence.add(fence_started.elapsed());

                            let (sample_ts90, force_idr, discard_from_track) = if warmup {
                                let warmup_ts90 = u64::from(warmup_encoded_frames)
                                    .saturating_mul(ENCODER_WARMUP_TIMESTAMP_STEP_90K);
                                warmup_encoded_frames = warmup_encoded_frames.saturating_add(1);
                                (warmup_ts90, warmup_encoded_frames == 1, true)
                            } else {
                                if first_video_timestamp_100ns.is_none() {
                                    sink_status(
                                        &mut encoded_sink,
                                        format!(
                                            "初始化阶段结束：首个正式源视频帧进入 NVENC，累计 {:.1}ms；此前耗时属于 NVENC/D3D11 初始化 + {} capture warmup",
                                            record_started.elapsed().as_secs_f64() * 1000.0,
                                            capture_source.label()
                                        ),
                                    );
                                    first_video_timestamp_100ns = timestamp_100ns;
                                }
                                let first_ts =
                                    *first_sample_timestamp_90k.get_or_insert(timestamp_90k);
                                let sample_ts90 = timestamp_90k.saturating_sub(first_ts);
                                last_submitted_sample_timestamp_90k = Some(sample_ts90);
                                (sample_ts90, captured_frames == 0, false)
                            };

                            let submit_started = Instant::now();
                            let encode_result = nvenc_encoder.encode_texture(
                                &direct_input,
                                sample_ts90,
                                force_idr,
                                discard_from_track,
                            );
                            perf.submit.add(submit_started.elapsed());

                            let release_result = match &slot {
                                CaptureFrameSlot::Shared(_) => keyed_mutex_guard
                                    .take()
                                    .expect("shared snapshot owns keyed mutex guard")
                                    .release("IDXGIKeyedMutex::ReleaseSync(NVENC DDA snapshot)"),
                                CaptureFrameSlot::WgcLocal(_) => Ok(()),
                            };
                            release_result?;
                            drop(keyed_mutex_guard);
                            let sample = encode_result?;
                            let _ = free_tx.send(slot);

                            push_record_hevc_sample(
                                &mut samples,
                                sample,
                                &mut encoded_sink,
                                retain_output_samples,
                                &mut encoded_stats,
                            );
                            if warmup {
                                perf.frame.add(frame_started.elapsed());
                                return Ok(());
                            }

                            captured_frames += 1;
                            if let Some(capture) = audio_capture.as_mut() {
                                capture.poll_live_aac(
                                    first_video_timestamp_100ns,
                                    last_submitted_sample_timestamp_90k,
                                    &mut encoded_sink,
                                    &mut notes,
                                );
                            }
                            perf.frame.add(frame_started.elapsed());
                            Ok(())
                        })();
                        if let Err(err) = frame_result {
                            stop.store(true, std::sync::atomic::Ordering::Relaxed);
                            capture_thread.stop_and_join();
                            return Err(err);
                        }
                    }
                    CaptureMsg::Done(stats) => {
                        capture_stats = Some(stats);
                        break;
                    }
                    CaptureMsg::Error(message) => {
                        capture_error = Some(message);
                        break;
                    }
                }
            }

            stop.store(true, std::sync::atomic::Ordering::Relaxed);
            capture_thread.stop_and_join();
            if let Some(message) = capture_error {
                return Err(BackendError::unsupported(
                    format!("{} capture thread", capture_source.label()),
                    "Acquire/CopyResource",
                    message,
                ));
            }
            if let Some(stats) = capture_stats {
                dda_timeouts = stats.dda_timeouts.min(u64::from(u32::MAX)) as u32;
                perf.dda_accumulated_frames_total = stats.accumulated_frames_total;
                perf.dda_accumulated_frames_max = stats.accumulated_frames_max;
                notes.push(stats.summary());
            }
        }

        if record_stop.load(std::sync::atomic::Ordering::Relaxed) {
            let stop_cleanup_started = Instant::now();
            sink_status(
                &mut encoded_sink,
                format!(
                    "停止排查：NVENC capture thread 已退出，开始快速释放；captured_frames={}",
                    captured_frames
                ),
            );
            if let Some(capture) = audio_capture.as_mut() {
                capture.stop_without_reencode(&mut notes);
            }
            sink_status(
                &mut encoded_sink,
                format!(
                    "停止排查：NVENC 快速释放完成 cleanup={:.1}ms total={:.1}ms",
                    stop_cleanup_started.elapsed().as_secs_f64() * 1000.0,
                    record_started.elapsed().as_secs_f64() * 1000.0
                ),
            );
            return Err(BackendError::unsupported(
                "停止即时回放",
                "用户停止请求",
                "已快速中止当前 NVENC 录制片段",
            ));
        }

        let duration_90k = encoded_timeline_duration_90k(
            &samples,
            encoded_stats.last_timestamp_90k,
            requested_duration_90k,
            !capture_source.is_wgc(),
        );
        let encoded_samples = encoded_stats.encoded_samples;
        let encoded_bytes = encoded_stats.encoded_bytes;
        let discarded_header_units = encoded_stats.discarded_header_units;
        if encoded_samples == 0 {
            return Err(BackendError::unsupported(
                "NVENC encode",
                record_route.summary(),
                format!(
                    "录制结束后没有任何可封装 AU；captured_frames={captured_frames}, target_dxgi_format={}, input_dxgi_format={}, notes={}",
                    target_desc.Format.0,
                    input_dxgi_format,
                    notes.join(" | ")
                ),
            ));
        }
        let audio_track = if let Some(capture) = audio_capture.as_mut() {
            capture.finish_live_aac(&mut encoded_sink, &mut notes);
            let sink_push_from_ticks = capture.live_pushed_until_ticks();
            let audio_frames = capture.finish(&mut notes);
            build_record_aac_track(
                audio_frames,
                first_video_timestamp_100ns,
                duration_90k,
                &mut notes,
                &mut encoded_sink,
                sink_push_from_ticks,
            )?
        } else {
            None
        };
        let audio_access_units = audio_track
            .as_ref()
            .map(|track| track.samples.len().min(u32::MAX as usize) as u32)
            .unwrap_or(0);
        let audio_encoded_bytes = audio_track
            .as_ref()
            .map(|track| {
                track
                    .samples
                    .iter()
                    .map(|sample| sample.data.len() as u64)
                    .sum()
            })
            .unwrap_or(0);
        let video_track = HevcMp4Track {
            width: capture_width,
            height: capture_height,
            duration_90k,
            color: record_route.mp4_color,
            codec: record_route.mp4_codec,
            samples,
        };
        if write_output_mp4 {
            write_hevc_aac_mp4(output, &video_track, audio_track.as_ref())?;
        } else {
            notes.push(format!(
                "NVENC 生产会话以内存 encoded ring 为主，跳过临时 MP4 写出：{}；保存时再从已编码 HEVC/AAC 快照 mux",
                output.display()
            ));
        }

        notes.push(format!(
            "NVENC route 色彩元数据由当前 DXGI 状态动态同步到 GPU shader、HEVC SPS VUI 与 MP4 nclx，并已校验首个 SPS：当前 route nclx={}/{}/{} range={} input={}",
            record_route.mp4_color.colour_primaries,
            record_route.mp4_color.transfer_characteristics,
            record_route.mp4_color.matrix_coefficients,
            if record_route.mp4_color.full_range {
                "full"
            } else {
                "limited"
            },
            nvenc_input_format.label()
        ));
        if discarded_header_units > 0 {
            notes.push(format!(
                "MP4 muxer 已从 {discarded_header_units} 个 NVENC warmup AU 中提取 VPS/SPS/PPS，但这些预热 AU 不写入正式视频时间线"
            ));
        }
        if audio_access_units > 0 {
            notes.push(format!(
                "音频轨已接入：AAC LC access_units={} bytes={}，按首个正式视频源时间戳和视频 track duration 裁剪/补静音",
                audio_access_units, audio_encoded_bytes
            ));
        } else {
            notes.push("音频轨未写入：未能获得视频绝对时间戳或音频被显式关闭".to_owned());
        }
        notes.push(format!(
            "NVENC dirty rect 元数据统计（固定路线不做 partial dirty-rect 转换）: partial={} full={} dirty_metadata_frames={} move_metadata_frames={} avg_dirty_area={:.0}px",
            partial_convert_frames,
            full_convert_frames,
            dirty_metadata_frames,
            move_metadata_frames,
            if dirty_metadata_frames == 0 {
                0.0
            } else {
                dirty_area_total as f64 / f64::from(dirty_metadata_frames)
            }
        ));
        notes.push(perf.summary(captured_frames));

        let report = VplOneCopyRecordReport {
            adapter_index,
            adapter_luid,
            output_path: output.display().to_string(),
            width: capture_width,
            height: capture_height,
            duration_seconds,
            captured_frames,
            encoded_samples,
            encoded_bytes,
            audio_access_units,
            audio_encoded_bytes,
            dda_timeouts,
            input_dxgi_format,
            target_dxgi_format: target_desc.Format.0 as u32,
            query_status: 0,
            init_status: 0,
            close_status: 0,
            first_get_surface_status: 0,
            video_processor_format_flags_in: format_flags_in,
            video_processor_format_flags_out: format_flags_out,
            notes,
        };

        sink_status(
            &mut encoded_sink,
            format!(
                "NVENC 录制段正常结束并完成封装准备：captured_frames={} video_au={} audio_au={} total={:.1}ms",
                captured_frames,
                encoded_samples,
                audio_access_units,
                record_started.elapsed().as_secs_f64() * 1000.0
            ),
        );

        Ok(VplOneCopyRecordOutput {
            report,
            video_track,
            audio_track,
        })
    }
}
