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
        D3D11_TEXTURE2D_DESC, D3D11_USAGE_DEFAULT, ID3D11DeviceContext4, ID3D11Texture2D,
    };
    use windows::Win32::Graphics::Dxgi::Common::DXGI_SAMPLE_DESC;
    use windows::Win32::Graphics::Dxgi::{CreateDXGIFactory1, IDXGIFactory1};
    use windows::core::Interface;

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
        return Err(BackendError::cancelled("NVENC/D3D11 初始化前"));
    }

    let mut notes = vec![
        "NVENC production path: native D3D11 formats use WGC local inputs or DDA shared-fence capture plus one encoder-local safety copy; planar 422/444 formats use same-device CUDA external-memory inputs with keyed mutex; no CPU Map/staging/raw-frame fallback".to_owned(),
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
        let selected_output_index = route_plan.map(|plan| plan.output_index).unwrap_or(0);
        let selected_output = adapter1.EnumOutputs(selected_output_index).map_err(|err| {
            BackendError::WindowsApi {
                func: "IDXGIAdapter1::EnumOutputs(NVENC record target)",
                message: err.to_string(),
            }
        })?;
        let output_desc = selected_output
            .GetDesc()
            .map_err(|err| BackendError::WindowsApi {
                func: "IDXGIOutput::GetDesc(NVENC record)",
                message: err.to_string(),
            })?;
        let (capture_width, capture_height, capture_dimensions_note) =
            capture_dimensions_from_output(&output_desc);
        if requested_chroma == ChromaSampling::Yuv420
            && (capture_width % 2 != 0 || capture_height % 2 != 0)
        {
            return Err(BackendError::unsupported(
                "NVENC 录制尺寸",
                format!("{}x{} 4:2:0", capture_width, capture_height),
                "不支持的桌面模式",
            ));
        }
        if requested_chroma == ChromaSampling::Yuv422 && capture_width % 2 != 0 {
            return Err(BackendError::unsupported(
                "NVENC 录制尺寸",
                format!("{}x{} 4:2:2", capture_width, capture_height),
                "不支持的桌面模式",
            ));
        }
        let encode_width = u32::from(capture_width);
        let encode_height = u32::from(capture_height);
        notes.push(format!(
            "NVENC direct-input dimensions: {capture_dimensions_note}; encode={}x{} (no implicit padding)",
            encode_width, encode_height
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
            &adapter_luid,
            selected_output_index,
            &output_desc,
            &selected_output,
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
        let mut nvenc_encoder = crate::backend::nvenc::NvencTextureEncoder::open_with_rate_control(
            adapter_index,
            encode_width,
            encode_height,
            nvenc_input_format,
            record_route.mp4_color,
            rate_control,
            encoder_frame_rate_n,
            encoder_frame_rate_d,
        )?;
        let encoder_device = nvenc_encoder.device().clone();
        let immediate = nvenc_encoder.context().clone();
        let encoder_context4: Option<ID3D11DeviceContext4> =
            if capture_source == RecordCaptureSource::Dda && !nvenc_encoder.uses_cuda_interop() {
                Some(immediate.cast().map_err(|err| BackendError::WindowsApi {
                    func: "ID3D11DeviceContext::cast<ID3D11DeviceContext4>(NVENC DDA safety copy)",
                    message: err.to_string(),
                })?)
            } else {
                None
            };
        let (encoder_luid_low, encoder_luid_high) = d3d11_device_adapter_luid(&encoder_device)?;
        if encoder_luid_low != desc.AdapterLuid.LowPart
            || encoder_luid_high != desc.AdapterLuid.HighPart
        {
            return Err(BackendError::unsupported(
                "NVENC D3D11 device",
                format!(
                    "encoder LUID={:08X}:{:08X}, desktop adapter LUID={:08X}:{:08X}",
                    encoder_luid_high as u32,
                    encoder_luid_low,
                    desc.AdapterLuid.HighPart as u32,
                    desc.AdapterLuid.LowPart
                ),
                "不支持的桌面模式：NVENC session 与捕获输出不在同一 DXGI adapter",
            ));
        }
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
                "初始化阶段：NVENC encoder/session 完成 input={} transport={}，累计 {:.1}ms，本阶段 {:.1}ms",
                nvenc_input_format.label(),
                if nvenc_encoder.uses_cuda_interop() {
                    "CUDA external-memory array"
                } else {
                    "D3D11 direct"
                },
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

        let record_route_dxgi_format = nvenc_input_format.dxgi_format();
        let target_desc = D3D11_TEXTURE2D_DESC {
            Width: encode_width,
            Height: nvenc_input_format.texture_height(encode_height),
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
        sink_status(
            &mut encoded_sink,
            format!(
                "初始化阶段：NVENC 输入槽位格式就绪 input={} storage={}x{} encode={}x{} zero_copy_capture_slots={}，累计 {:.1}ms",
                nvenc_input_format.label(),
                target_desc.Width,
                target_desc.Height,
                encode_width,
                encode_height,
                capture_source != RecordCaptureSource::Dda || nvenc_encoder.uses_cuda_interop(),
                record_started.elapsed().as_secs_f64() * 1000.0
            ),
        );
        match (capture_source, nvenc_encoder.uses_cuda_interop()) {
            (RecordCaptureSource::Dda, false) => notes.push(
                "NVENC DDA 安全路线：独立 capture device 的 shader 写普通共享 YUV texture；encoder device 等待 shared fence 后复制一次到本地纹理，再交给 NVENC split engines"
                    .to_owned(),
            ),
            (RecordCaptureSource::Wgc, false) => notes.push(
                "NVENC WGC 路线：使用 NVENC D3D11 device 创建 WGC capture，每个本地 YUV 槽位自带 route converter；shader 直接写最终槽位，等待 GPU fence 后直接注册为 NVENC input，全程不再复制"
                    .to_owned(),
            ),
            (RecordCaptureSource::Dda, true) => notes.push(
                "NVENC DDA CUDA-array 路线：DDA 与 shader 使用同一 D3D11 device 写带 keyed mutex 的连续平面 texture；共享 NT handle 只导入一次 CUDA external memory 并持久注册 NVENC，无 GPU 内容拷贝"
                    .to_owned(),
            ),
            (RecordCaptureSource::Wgc, true) => notes.push(
                "NVENC WGC CUDA-array 路线：WGC 与 shader 使用同一 D3D11 device 写带 keyed mutex 的连续平面 texture；共享 NT handle 只导入一次 CUDA external memory 并持久注册 NVENC，无 GPU 内容拷贝"
                    .to_owned(),
            ),
        }

        struct DdaCopyInput {
            texture: ID3D11Texture2D,
        }

        struct PendingCaptureInput {
            capture_slot: CaptureFrameSlot,
            dda_copy_input: Option<DdaCopyInput>,
        }

        let mut samples = Vec::new();
        let mut pending_slots = std::collections::VecDeque::<PendingCaptureInput>::new();
        let mut dda_copy_free = Vec::<DdaCopyInput>::new();
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
        let mut last_forced_idr_timestamp_90k: Option<u64> = None;
        let capture_duration = Duration::from_secs_f32(duration_seconds.max(0.1));
        let requested_duration_90k =
            (duration_seconds.max(0.1) as f64 * VIDEO_CLOCK_HZ as f64).round() as u64;
        let source_stop_90k = requested_duration_90k;
        let start = Instant::now();
        let end_at = start + capture_duration;
        let qpc_frequency = query_performance_frequency().unwrap_or(0);
        let audio_started = Instant::now();
        let mut audio_capture = RecordAudioCapture::start(
            capture_duration + Duration::from_secs(5),
            retain_output_samples,
            &mut notes,
        );
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
            let capture_pool_size = capture_pool_size_for_route(
                encode_width,
                encode_height,
                record_route,
                matches!(capture_source, RecordCaptureSource::Dda)
                    && !nvenc_encoder.uses_cuda_interop(),
            );
            let required_lookahead_slots =
                usize::from(rate_control.look_ahead_depth).saturating_add(1);
            if capture_pool_size < required_lookahead_slots {
                return Err(BackendError::unsupported(
                    "NVENC Lookahead capture pool",
                    format!(
                        "depth={} available_slots={} required_slots={}",
                        rate_control.look_ahead_depth, capture_pool_size, required_lookahead_slots
                    ),
                    "当前分辨率/跨设备路线的 GPU surface 内存预算不足以保留全部延迟输入",
                ));
            }
            if capture_source == RecordCaptureSource::Dda && !nvenc_encoder.uses_cuda_interop() {
                dda_copy_free.reserve(required_lookahead_slots);
                for _ in 0..required_lookahead_slots {
                    let mut texture = None;
                    encoder_device
                        .CreateTexture2D(&target_desc, None, Some(&mut texture))
                        .map_err(|err| BackendError::WindowsApi {
                            func: "ID3D11Device::CreateTexture2D(NVENC DDA safety copy)",
                            message: err.to_string(),
                        })?;
                    dda_copy_free.push(DdaCopyInput {
                        texture: texture.ok_or_else(|| BackendError::WindowsApi {
                            func: "CreateTexture2D(NVENC DDA safety copy)",
                            message: "返回空 encoder-local texture".to_owned(),
                        })?,
                    });
                }
                notes.push(format!(
                    "NVENC DDA encoder-local safety-copy pool: textures={} (LookaheadDepth+1)",
                    dda_copy_free.len()
                ));
            }
            let capture_queue_size = capture_pool_size;
            let (frame_tx, frame_rx) = std::sync::mpsc::channel::<CaptureMsg>();
            let (free_tx, free_rx) = std::sync::mpsc::channel::<CaptureFrameSlot>();
            let capture_stop = std::sync::Arc::new(std::sync::atomic::AtomicBool::new(false));
            let stop = capture_stop.clone();
            let capture_thread_started = Instant::now();
            let capture_handle = match capture_source {
                RecordCaptureSource::Dda => {
                    let (capture_device, capture_context) = if nvenc_encoder.uses_cuda_interop() {
                        (encoder_device.clone(), immediate.clone())
                    } else {
                        create_d3d11_device_for_adapter(&adapter1)?
                    };
                    let output_mode = if nvenc_encoder.uses_cuda_interop() {
                        DdaOutputMode::Local
                    } else {
                        DdaOutputMode::SharedFenceToEncoder(encoder_device.clone())
                    };
                    spawn_dda_capture_thread(
                        adapter1.clone(),
                        selected_output_index,
                        capture_device,
                        capture_context,
                        output_mode,
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
                    selected_output_index,
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
                "NVENC capture input pool: textures={}, queue={}",
                capture_pool_size, capture_queue_size
            ));

            let mut capture_stats: Option<CaptureStats> = None;
            let mut capture_error: Option<CaptureFailure> = None;
            let mut active_source_desc: Option<D3D11_TEXTURE2D_DESC> = None;
            let mut next_route_validation = Instant::now() + Duration::from_secs(1);

            loop {
                if record_stop.load(std::sync::atomic::Ordering::Relaxed) {
                    stop.store(true, std::sync::atomic::Ordering::Relaxed);
                    break;
                }
                if Instant::now() >= next_route_validation {
                    let current_desc = selected_output.GetDesc().map_err(|err| {
                        BackendError::reconfigure_required(format!(
                            "读取当前 NVENC 输出状态失败：{err}"
                        ))
                    })?;
                    validate_current_nvenc_route_plan(
                        plan,
                        adapter_index,
                        &adapter_luid,
                        selected_output_index,
                        &current_desc,
                        &selected_output,
                        requested_chroma,
                    )
                    .map_err(|err| BackendError::reconfigure_required(err.to_string()))?;
                    next_route_validation = Instant::now() + Duration::from_secs(1);
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
                            )?;
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
                            let source_input = match &slot {
                                CaptureFrameSlot::Shared(_) => {
                                    return Err(BackendError::unsupported(
                                        "NVENC direct DDA input",
                                        "keyed shared capture slot",
                                        "NVENC DDA 直写路线不接受 shared snapshot 或兼容复制回退",
                                    ));
                                }
                                CaptureFrameSlot::FenceShared(shared) => {
                                    encoder_context4
                                        .as_ref()
                                        .ok_or_else(|| {
                                            BackendError::unsupported(
                                                "NVENC shared-fence DDA input",
                                                "ID3D11DeviceContext4",
                                                "encoder 缺少 GPU wait context",
                                            )
                                        })?
                                        .Wait(&shared.encoder_fence, shared.fence_value)
                                        .map_err(|err| BackendError::WindowsApi {
                                            func: "ID3D11DeviceContext4::Wait(NVENC DDA safety copy)",
                                            message: err.to_string(),
                                        })?;
                                    shared.encoder_texture.clone()
                                }
                                CaptureFrameSlot::Local(local) => {
                                    if local.keyed_mutex.is_none() {
                                        local.capture_fence.wait_ready(&immediate)?;
                                    }
                                    local.texture.clone()
                                }
                            };
                            perf.source_fence.add(fence_started.elapsed());

                            let mut dda_copy_input = None;
                            let direct_input = if matches!(&slot, CaptureFrameSlot::FenceShared(_))
                            {
                                let copy_started = Instant::now();
                                let copy_input = dda_copy_free.pop().ok_or_else(|| {
                                    BackendError::unsupported(
                                        "NVENC DDA safety copy",
                                        format!(
                                            "LookaheadDepth={} pending={}",
                                            rate_control.look_ahead_depth,
                                            pending_slots.len()
                                        ),
                                        "encoder-local input pool exhausted before NVENC returned an output",
                                    )
                                })?;
                                copy_texture_resource(
                                    &immediate,
                                    &source_input,
                                    &copy_input.texture,
                                )?;
                                let texture = copy_input.texture.clone();
                                dda_copy_input = Some(copy_input);
                                perf.copy.add(copy_started.elapsed());
                                texture
                            } else {
                                source_input
                            };

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
                                let idr_reference = encoded_stats
                                    .last_sync_timestamp_90k
                                    .or(last_forced_idr_timestamp_90k);
                                let force_idr =
                                    should_force_source_timed_idr(idr_reference, sample_ts90);
                                if force_idr {
                                    last_forced_idr_timestamp_90k = Some(sample_ts90);
                                }
                                (sample_ts90, force_idr, false)
                            };

                            let submit_started = Instant::now();
                            let encode_result = nvenc_encoder.submit_texture(
                                &direct_input,
                                sample_ts90,
                                force_idr,
                                discard_from_track,
                            );
                            perf.submit.add(submit_started.elapsed());
                            let outputs = match encode_result {
                                Ok(outputs) => outputs,
                                Err(err) => {
                                    if let Some(copy_input) = dda_copy_input.take() {
                                        dda_copy_free.push(copy_input);
                                    }
                                    let _ = free_tx.send(slot);
                                    return Err(err);
                                }
                            };
                            pending_slots.push_back(PendingCaptureInput {
                                capture_slot: slot,
                                dda_copy_input,
                            });
                            for sample in outputs {
                                let completed = pending_slots.pop_front().ok_or_else(|| {
                                    BackendError::unsupported(
                                        "NVENC delayed output",
                                        "capture slot queue",
                                        "编码器返回 AU 时没有对应的 pending capture slot",
                                    )
                                })?;
                                if let Some(copy_input) = completed.dda_copy_input {
                                    dda_copy_free.push(copy_input);
                                }
                                let _ = free_tx.send(completed.capture_slot);
                                push_record_hevc_sample(
                                    &mut samples,
                                    sample,
                                    &mut encoded_sink,
                                    retain_output_samples,
                                    &mut encoded_stats,
                                );
                            }
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
                                )?;
                            }
                            perf.frame.add(frame_started.elapsed());
                            Ok(())
                        })();
                        if let Err(err) = frame_result {
                            stop.store(true, std::sync::atomic::Ordering::Relaxed);
                            let _ = capture_thread.stop_and_join();
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
            capture_thread.stop_and_join()?;
            if let Some(failure) = capture_error {
                match failure {
                    CaptureFailure::Reconfigure(reason) => {
                        return Err(BackendError::reconfigure_required(reason));
                    }
                    CaptureFailure::Fatal(message) => {
                        return Err(BackendError::unsupported(
                            format!("{} capture thread", capture_source.label()),
                            "Acquire/CopyResource",
                            message,
                        ));
                    }
                }
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
            return Err(BackendError::cancelled("NVENC 录制循环"));
        }

        let flush_started = Instant::now();
        let flushed_outputs = nvenc_encoder.flush()?;
        for sample in flushed_outputs {
            let completed = pending_slots.pop_front().ok_or_else(|| {
                BackendError::unsupported(
                    "NVENC delayed output flush",
                    "capture slot queue",
                    "EOS flush 返回 AU 时没有对应的 pending capture slot",
                )
            })?;
            if let Some(copy_input) = completed.dda_copy_input {
                dda_copy_free.push(copy_input);
            }
            push_record_hevc_sample(
                &mut samples,
                sample,
                &mut encoded_sink,
                retain_output_samples,
                &mut encoded_stats,
            );
        }
        if !pending_slots.is_empty() || nvenc_encoder.pending_frame_count() != 0 {
            return Err(BackendError::unsupported(
                "NVENC delayed output flush",
                format!(
                    "capture_slots={} encoder_pending={}",
                    pending_slots.len(),
                    nvenc_encoder.pending_frame_count()
                ),
                "EOS 后仍有未释放输入，拒绝生成不完整 MP4",
            ));
        }
        notes.push(format!(
            "NVENC delayed-output flush 完成：lookahead_depth={} elapsed={:.1}ms",
            nvenc_encoder.lookahead_depth(),
            flush_started.elapsed().as_secs_f64() * 1000.0
        ));

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
                    let encoded_bytes = track
                        .as_ref()
                        .map(|track| {
                            track
                                .samples
                                .iter()
                                .map(|sample| sample.data.len() as u64)
                                .sum()
                        })
                        .unwrap_or(0);
                    (track, access_units, encoded_bytes)
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
            width: capture_width,
            height: capture_height,
            duration_90k,
            color: record_route.mp4_color,
            codec: record_route.mp4_codec,
            samples,
        };
        nvenc_encoder.shutdown()?;
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
            output_index: selected_output_index,
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
