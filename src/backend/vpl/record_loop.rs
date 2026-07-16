use super::*;

#[cfg(windows)]
struct VplRecordRuntime<'a> {
    api: &'a VplApi,
    loader: MfxLoader,
    session: MfxSession,
    encoder_open: bool,
}

#[cfg(windows)]
impl<'a> VplRecordRuntime<'a> {
    unsafe fn new(api: &'a VplApi, loader: MfxLoader) -> Self {
        Self {
            api,
            loader,
            session: ptr::null_mut(),
            encoder_open: false,
        }
    }

    fn set_session(&mut self, session: MfxSession) {
        self.session = session;
    }

    fn mark_encoder_open(&mut self) {
        self.encoder_open = true;
    }

    unsafe fn close_encoder(&mut self) -> i32 {
        if self.encoder_open && !self.session.is_null() {
            self.encoder_open = false;
            (self.api.mfx_video_encode_close)(self.session)
        } else {
            MFX_ERR_NONE
        }
    }

    unsafe fn close_session_and_loader(&mut self) -> i32 {
        let session_status = if !self.session.is_null() {
            let session = std::mem::replace(&mut self.session, ptr::null_mut());
            (self.api.mfx_close)(session)
        } else {
            MFX_ERR_NONE
        };
        if !self.loader.is_null() {
            let loader = std::mem::replace(&mut self.loader, ptr::null_mut());
            (self.api.mfx_unload)(loader);
        }
        session_status
    }

    unsafe fn shutdown(&mut self) -> (i32, i32) {
        let encoder_status = self.close_encoder();
        let session_status = self.close_session_and_loader();
        (encoder_status, session_status)
    }
}

#[cfg(windows)]
impl Drop for VplRecordRuntime<'_> {
    fn drop(&mut self) {
        unsafe {
            let _ = self.shutdown();
        }
    }
}

#[cfg(windows)]
struct MfxSurfaceGuard {
    surface: *mut MfxFrameSurface1,
    interface: *mut MfxFrameSurfaceInterface,
}

#[cfg(windows)]
impl MfxSurfaceGuard {
    unsafe fn new(
        surface: *mut MfxFrameSurface1,
        interface: *mut MfxFrameSurfaceInterface,
    ) -> Self {
        Self { surface, interface }
    }

    unsafe fn release(mut self, func: &'static str) -> Result<(), BackendError> {
        let status = self.release_raw();
        if status == MFX_ERR_NONE {
            Ok(())
        } else {
            Err(BackendError::VplStatus { func, status })
        }
    }

    fn into_raw(mut self) -> *mut MfxFrameSurface1 {
        std::mem::replace(&mut self.surface, ptr::null_mut())
    }

    unsafe fn release_raw(&mut self) -> i32 {
        if self.surface.is_null() || self.interface.is_null() {
            return MFX_ERR_NONE;
        }
        let surface = std::mem::replace(&mut self.surface, ptr::null_mut());
        ((*self.interface).Release)(surface)
    }
}

#[cfg(windows)]
impl Drop for MfxSurfaceGuard {
    fn drop(&mut self) {
        unsafe {
            let _ = self.release_raw();
        }
    }
}

#[cfg(windows)]
struct VplEncodeCleanupGuard<'a> {
    runtime: *mut VplRecordRuntime<'a>,
    pending_surface: *mut Option<*mut MfxFrameSurface1>,
}

#[cfg(windows)]
impl<'a> VplEncodeCleanupGuard<'a> {
    fn new(
        runtime: &mut VplRecordRuntime<'a>,
        pending_surface: &mut Option<*mut MfxFrameSurface1>,
    ) -> Self {
        Self {
            runtime,
            pending_surface,
        }
    }
}

#[cfg(windows)]
impl Drop for VplEncodeCleanupGuard<'_> {
    fn drop(&mut self) {
        unsafe {
            let runtime = &mut *self.runtime;
            if let Some(surface) = (&mut *self.pending_surface).take()
                && !surface.is_null()
            {
                let interface = (*surface).FrameInterface;
                if !interface.is_null() {
                    let _ = ((*interface).Release)(surface);
                }
            }
            let _ = runtime.close_encoder();
            let _ = runtime.close_session_and_loader();
        }
    }
}

#[cfg(windows)]
#[allow(clippy::too_many_arguments)]
pub(super) fn record_d3d11_onecopy_mp4_impl(
    adapter_index: u32,
    output: &Path,
    duration_seconds: f32,
    rate_control: &RateControlConfig,
    requested_chroma: ChromaSampling,
    capture_source: RecordCaptureSource,
    external_stop: Option<std::sync::Arc<std::sync::atomic::AtomicBool>>,
    write_output_mp4: bool,
    mut encoded_sink: Option<&mut dyn VplOneCopyRecordSink>,
    route_plan: Option<&VplCurrentDisplayRouteInfo>,
) -> Result<VplOneCopyRecordOutput, BackendError> {
    use crate::backend::mp4_mux::{HevcMp4Track, write_hevc_aac_mp4};
    use std::time::{Duration, Instant};
    use windows::Win32::Graphics::Direct3D11::{
        D3D11_TEXTURE2D_DESC, ID3D11Device, ID3D11DeviceContext, ID3D11Texture2D,
    };
    use windows::Win32::Graphics::Dxgi::{CreateDXGIFactory1, IDXGIFactory1};
    use windows::core::Interface;

    validate_rate_control_config(rate_control)?;

    let record_started = Instant::now();
    let record_stop = external_stop
        .clone()
        .unwrap_or_else(|| std::sync::Arc::new(std::sync::atomic::AtomicBool::new(false)));
    sink_status(
        &mut encoded_sink,
        format!(
            "录制后端初始化开始：capture={} chroma={} rc={}",
            capture_source.label(),
            requested_chroma.doc_label(),
            rate_control.method.short_name()
        ),
    );
    if record_stop.load(std::sync::atomic::Ordering::Relaxed) {
        return Err(BackendError::cancelled("oneVPL/D3D11 初始化前"));
    }

    let phase_started = Instant::now();
    let (api, dll_path) =
        VplApi::load().map_err(|err| BackendError::unsupported("oneVPL", "DLL", err))?;
    sink_status(
        &mut encoded_sink,
        format!(
            "初始化阶段：加载 oneVPL DLL 完成，用时 {:.1}ms",
            phase_started.elapsed().as_secs_f64() * 1000.0
        ),
    );
    let mut notes = vec![format!("oneVPL DLL: {}", dll_path.display())];

    unsafe {
        let _thread_priority = match capture_source {
            RecordCaptureSource::Dda => RecordThreadPriorityGuard::raise(&mut notes),
            RecordCaptureSource::Wgc => {
                notes.push("WGC 录制主线程/持久 MTA 捕获服务固定保持普通 CPU 优先级".to_owned());
                None
            }
        };
        let factory: IDXGIFactory1 =
            CreateDXGIFactory1().map_err(|err| BackendError::WindowsApi {
                func: "CreateDXGIFactory1",
                message: err.to_string(),
            })?;
        let adapter1 =
            factory
                .EnumAdapters1(adapter_index)
                .map_err(|err| BackendError::WindowsApi {
                    func: "IDXGIFactory1::EnumAdapters1",
                    message: err.to_string(),
                })?;
        let desc = adapter1
            .GetDesc1()
            .map_err(|err| BackendError::WindowsApi {
                func: "IDXGIAdapter1::GetDesc1",
                message: err.to_string(),
            })?;
        let adapter_luid = format!(
            "{:08X}:{:08X}",
            desc.AdapterLuid.HighPart as u32, desc.AdapterLuid.LowPart
        );
        let selected_output_index = route_plan.map(|plan| plan.output_index).unwrap_or(0);
        let selected_output = adapter1.EnumOutputs(selected_output_index).map_err(|err| {
            BackendError::WindowsApi {
                func: "IDXGIAdapter1::EnumOutputs(record target)",
                message: err.to_string(),
            }
        })?;
        let output_desc = selected_output
            .GetDesc()
            .map_err(|err| BackendError::WindowsApi {
                func: "IDXGIOutput::GetDesc",
                message: err.to_string(),
            })?;
        let (capture_width, capture_height, capture_dimensions_note) =
            capture_dimensions_from_output(&output_desc);
        let aligned_width = align16(capture_width);
        let aligned_height = align16(capture_height);
        notes.push(format!(
            "oneVPL capture target: adapter={} output={} {}; aligned={}x{}",
            adapter_index,
            selected_output_index,
            capture_dimensions_note,
            aligned_width,
            aligned_height
        ));
        let (encoder_frame_rate_n, encoder_frame_rate_d, encoder_frame_rate_note) =
            encoder_frame_rate_hint_from_output(&output_desc);
        notes.push(format!(
            "oneVPL FrameRateExt 仅作为编码器码控/HRD 提示：{}/{}；来源={}；正式 MP4 时间戳保持 DDA/WGC 源 VFR 节奏",
            encoder_frame_rate_n, encoder_frame_rate_d, encoder_frame_rate_note
        ));
        // 自动同步路线集中在录制后端内部：前端只表达目标色度采样；
        // 能力探测阶段已经把当前显示器状态解析为 RoutePlan。启动时只做
        // 轻量一致性校验，避免重复完整 route 现场探测。
        let route_probe_started = Instant::now();
        let record_route_candidates = if let Some(plan) = route_plan {
            validate_current_display_route_plan(
                plan,
                adapter_index,
                selected_output_index,
                &output_desc,
                &selected_output,
                requested_chroma,
            )?;
            let route = record_route_from_display_plan(plan)?;
            notes.push(format!(
                "record RoutePlan accepted: adapter={} output={} ColorSpace={} BitsPerColor={} rect={},{},{},{} route={}",
                plan.adapter_index,
                plan.output_index,
                plan.color_space,
                plan.bits_per_color,
                plan.desktop_left,
                plan.desktop_top,
                plan.desktop_right,
                plan.desktop_bottom,
                route.summary()
            ));
            vec![route]
        } else {
            notes.push(
                "record RoutePlan missing; falling back to startup-time display route probing"
                    .to_owned(),
            );
            select_record_route_candidates_for_output(
                &selected_output,
                requested_chroma,
                &mut notes,
            )?
        };
        sink_status(
            &mut encoded_sink,
            format!(
                "初始化阶段：DXGI 输出/桌面模式 RoutePlan 准备完成，候选={}，累计 {:.1}ms，本阶段 {:.1}ms",
                record_route_candidates.len(),
                record_started.elapsed().as_secs_f64() * 1000.0,
                route_probe_started.elapsed().as_secs_f64() * 1000.0
            ),
        );
        if record_route_candidates
            .iter()
            .any(|route| !route.supports_requested_chroma(requested_chroma))
        {
            return Err(BackendError::unsupported(
                "录制路线选择",
                requested_chroma.doc_label(),
                "内部选择出的候选路线不匹配请求色度",
            ));
        }

        let loader = (api.mfx_load)();
        if loader.is_null() {
            return Err(BackendError::unsupported(
                "oneVPL record",
                "MFXLoad",
                "返回空 loader",
            ));
        }
        let mut runtime = VplRecordRuntime::new(&api, loader);
        if record_stop.load(std::sync::atomic::Ordering::Relaxed) {
            return Err(BackendError::cancelled("oneVPL loader 创建后"));
        }
        let session_started = Instant::now();
        let implementation_index = route_plan
            .map(|plan| plan.implementation_index)
            .filter(|index| *index != u32::MAX)
            .unwrap_or(0);
        notes.push(format!(
            "oneVPL implementation target from capability probe: {}",
            implementation_index
        ));
        let mut session: MfxSession = ptr::null_mut();
        let create_status = (api.mfx_create_session)(loader, implementation_index, &mut session);
        runtime.set_session(session);
        if create_status != MFX_ERR_NONE || session.is_null() {
            return Err(BackendError::VplStatus {
                func: "MFXCreateSession",
                status: create_status,
            });
        }
        sink_status(
            &mut encoded_sink,
            format!(
                "初始化阶段：MFXCreateSession 完成，累计 {:.1}ms，本阶段 {:.1}ms",
                record_started.elapsed().as_secs_f64() * 1000.0,
                session_started.elapsed().as_secs_f64() * 1000.0
            ),
        );
        let record_async_depth = std::env::var("RUST_REPLAY_VPL_ASYNC_DEPTH")
            .ok()
            .and_then(|value| value.parse::<u16>().ok())
            .unwrap_or(if capture_source.is_wgc() {
                2
            } else {
                VPL_RECORD_ASYNC_DEPTH
            })
            .clamp(2, VPL_RECORD_ASYNC_DEPTH);

        let record_gop_pic_size = std::env::var("RUST_REPLAY_VPL_GOP_PIC_SIZE")
            .ok()
            .and_then(|value| value.parse::<u16>().ok())
            .unwrap_or(u16::MAX)
            .clamp(1, u16::MAX);
        let record_gop_ref_dist = std::env::var("RUST_REPLAY_VPL_GOP_REF_DIST")
            .ok()
            .and_then(|value| value.parse::<u16>().ok())
            .unwrap_or(1)
            .clamp(1, 16);
        let record_idr_interval = std::env::var("RUST_REPLAY_VPL_IDR_INTERVAL")
            .ok()
            .and_then(|value| value.parse::<u16>().ok())
            .unwrap_or(1);

        let mut query_failures = Vec::new();
        let mut selected_route: Option<(VplRecordRoute, MfxVideoParam, i32)> = None;
        if route_plan.is_some() {
            let candidate = record_route_candidates[0];
            let mut param = make_query_param(
                rate_control,
                candidate.fourcc,
                candidate.chroma,
                candidate.bit_depth,
                candidate.profile,
            );
            param.AsyncDepth = record_async_depth;
            notes.push(format!(
                "record RoutePlan: 跳过启动阶段 MFXVideoENCODE_Query/QueryIOSurf；使用能力探测阶段已验证 route={} rc={}",
                candidate.summary(),
                rate_control.method.short_name()
            ));
            selected_route = Some((candidate, param, MFX_ERR_NONE));
        }
        for candidate in record_route_candidates
            .iter()
            .filter(|_| route_plan.is_none())
        {
            let mut query_param = make_query_param(
                rate_control,
                candidate.fourcc,
                candidate.chroma,
                candidate.bit_depth,
                candidate.profile,
            );
            query_param.AsyncDepth = record_async_depth;
            query_param.mfx.FrameInfo.Width = aligned_width;
            query_param.mfx.FrameInfo.Height = aligned_height;
            query_param.mfx.FrameInfo.CropW = capture_width;
            query_param.mfx.FrameInfo.CropH = capture_height;
            query_param.mfx.FrameInfo.FrameRateExtN = encoder_frame_rate_n;
            query_param.mfx.FrameInfo.FrameRateExtD = encoder_frame_rate_d;
            query_param.mfx.GopPicSize = record_gop_pic_size;
            query_param.mfx.GopRefDist = record_gop_ref_dist;
            query_param.mfx.IdrInterval = record_idr_interval;
            let mut query_ext_buffers = VplEncodeExtBuffers::for_route(*candidate, rate_control);
            query_ext_buffers.attach(&mut query_param);

            let mut queried = query_param;
            let query_status =
                (api.mfx_video_encode_query)(session, &mut query_param, &mut queried);
            if query_status < MFX_ERR_NONE {
                query_failures.push(format!(
                    "{}: Query status={query_status}",
                    candidate.summary()
                ));
                continue;
            }
            if query_status == MFX_WRN_PARTIAL_ACCELERATION {
                query_failures.push(format!(
                    "{}: Query 返回 MFX_WRN_PARTIAL_ACCELERATION",
                    candidate.summary()
                ));
                continue;
            }
            if !query_output_preserves_record_route(&queried, *candidate)
                || queried.mfx.RateControlMethod != rate_control.method.vpl_value()
            {
                query_failures.push(format!(
                    "{}: Query 改写关键参数 FourCC {} -> {}, chroma {} -> {}, bit_depth {}/{} -> {}/{}, profile {} -> {}, rc {} -> {}",
                    candidate.summary(),
                    fourcc_to_string(query_param.mfx.FrameInfo.FourCC),
                    fourcc_to_string(queried.mfx.FrameInfo.FourCC),
                    query_param.mfx.FrameInfo.ChromaFormat,
                    queried.mfx.FrameInfo.ChromaFormat,
                    query_param.mfx.FrameInfo.BitDepthLuma,
                    query_param.mfx.FrameInfo.BitDepthChroma,
                    queried.mfx.FrameInfo.BitDepthLuma,
                    queried.mfx.FrameInfo.BitDepthChroma,
                    hevc_profile_name(u32::from(query_param.mfx.CodecProfile)),
                    hevc_profile_name(u32::from(queried.mfx.CodecProfile)),
                    rate_control.method.short_name(),
                    queried.mfx.RateControlMethod
                ));
                continue;
            }
            let mut iosurf_param = queried;
            let mut alloc_request: MfxFrameAllocRequest = std::mem::zeroed();
            let query_iosurf_status =
                (api.mfx_video_encode_query_iosurf)(session, &mut iosurf_param, &mut alloc_request);
            if query_iosurf_status < MFX_ERR_NONE {
                query_failures.push(format!(
                    "{}: QueryIOSurf status={}（该具体码控字段组合不能创建 video-memory surface）",
                    candidate.summary(),
                    query_iosurf_status
                ));
                continue;
            }
            notes.push(format!(
                "oneVPL record route QueryIOSurf: route={} status={} min={} suggested={}",
                candidate.summary(),
                query_iosurf_status,
                alloc_request.NumFrameMin,
                alloc_request.NumFrameSuggested
            ));
            selected_route = Some((*candidate, queried, query_status));
            if !query_failures.is_empty() {
                notes.push(format!(
                    "oneVPL route fallback: skipped candidates=[{}]",
                    query_failures.join(" | ")
                ));
            }
            break;
        }
        let Some((record_route, queried, query_status)) = selected_route else {
            return Err(BackendError::unsupported(
                "oneVPL record route Query",
                requested_chroma.doc_label(),
                format!(
                    "当前显示状态候选 route 均未通过 oneVPL/D3D11 GPU-only Query；{}",
                    query_failures.join(" | ")
                ),
            ));
        };
        notes.push(format!(
            "oneVPL record route selected: {}",
            record_route.summary()
        ));
        sink_status(
            &mut encoded_sink,
            format!(
                "初始化阶段：oneVPL Query/QueryIOSurf 选路完成：{}，累计 {:.1}ms",
                record_route.summary(),
                record_started.elapsed().as_secs_f64() * 1000.0
            ),
        );
        let mut param = queried;
        apply_record_route_to_param(&mut param, record_route);
        apply_rate_control_config_to_param(&mut param, rate_control);
        let mut ext_buffers = VplEncodeExtBuffers::for_route(record_route, rate_control);
        ext_buffers.attach(&mut param);
        param.AsyncDepth = record_async_depth;
        param.mfx.FrameInfo.FrameRateExtN = encoder_frame_rate_n;
        param.mfx.FrameInfo.FrameRateExtD = encoder_frame_rate_d;
        param.mfx.FrameInfo.Width = aligned_width;
        param.mfx.FrameInfo.Height = aligned_height;
        param.mfx.FrameInfo.CropW = capture_width;
        param.mfx.FrameInfo.CropH = capture_height;
        param.mfx.GopPicSize = record_gop_pic_size;
        param.mfx.GopRefDist = record_gop_ref_dist;
        param.mfx.IdrInterval = record_idr_interval;
        param.mfx.LowPower = MFX_CODINGOPTION_ON;
        param.mfx.TargetUsage = 7;
        if record_stop.load(std::sync::atomic::Ordering::Relaxed) {
            return Err(BackendError::cancelled("MFXVideoENCODE_Init 前"));
        }
        let init_started = Instant::now();
        let init_status = (api.mfx_video_encode_init)(session, &mut param);
        if init_status < MFX_ERR_NONE {
            return Err(BackendError::VplStatus {
                func: "MFXVideoENCODE_Init",
                status: init_status,
            });
        }
        sink_status(
            &mut encoded_sink,
            format!(
                "初始化阶段：MFXVideoENCODE_Init 完成 status={}，累计 {:.1}ms，本阶段 {:.1}ms",
                init_status,
                record_started.elapsed().as_secs_f64() * 1000.0,
                init_started.elapsed().as_secs_f64() * 1000.0
            ),
        );
        runtime.mark_encoder_open();
        if let Some(sink) = encoded_sink.as_deref_mut() {
            sink.video_track_started(VplOutputTrackInfo {
                width: capture_width,
                height: capture_height,
                color: record_route.mp4_color,
                codec: record_route.mp4_codec,
            });
        }

        let mut first_surface: *mut MfxFrameSurface1 = ptr::null_mut();
        let first_get_surface_status =
            (api.mfx_memory_get_surface_for_encode)(session, &mut first_surface);
        if first_get_surface_status != MFX_ERR_NONE || first_surface.is_null() {
            return Err(BackendError::unsupported(
                "oneVPL D3D11 surface import",
                format!(
                    "{} + {}",
                    record_route.summary(),
                    rate_control.method.short_name()
                ),
                format!(
                    "MFXMemory_GetSurfaceForEncode 返回 status={}；该具体 route/码控字段组合无法提供 video-memory surface，禁止 CPU fallback",
                    first_get_surface_status
                ),
            ));
        }

        let first_interface = (*first_surface).FrameInterface;
        if first_interface.is_null() {
            return Err(BackendError::unsupported(
                "oneVPL record",
                "FrameInterface",
                "oneVPL surface 没有 FrameInterface",
            ));
        }
        let surface_release = (*first_interface).Release;
        let first_surface_guard = MfxSurfaceGuard::new(first_surface, first_interface);
        let mut first_native: MfxHDL = ptr::null_mut();
        let mut first_native_type = 0u32;
        let surface_started = Instant::now();
        let native_status = ((*first_interface).GetNativeHandle)(
            first_surface,
            &mut first_native,
            &mut first_native_type,
        );
        if native_status != MFX_ERR_NONE || first_native_type != MFX_RESOURCE_DX11_TEXTURE {
            return Err(BackendError::VplStatus {
                func: "mfxFrameSurfaceInterface::GetNativeHandle",
                status: native_status,
            });
        }
        let Some(first_target) = <ID3D11Texture2D as Interface>::from_raw_borrowed(&first_native)
        else {
            return Err(BackendError::unsupported(
                "oneVPL record",
                "native texture",
                "GetNativeHandle 返回值不是 ID3D11Texture2D",
            ));
        };
        let mut target_desc = D3D11_TEXTURE2D_DESC::default();
        first_target.GetDesc(&mut target_desc);
        let surface_cache_enabled = std::env::var("RUSTREPLAY_SURFACE_CACHE")
            .map(|value| value != "0")
            .unwrap_or(true);
        let mut surface_texture_cache: HashMap<usize, ID3D11Texture2D> = HashMap::new();
        if surface_cache_enabled {
            surface_texture_cache.insert(first_surface as usize, first_target.clone());
        }

        let mut device_handle: MfxHDL = ptr::null_mut();
        let mut device_type = 0u32;
        let device_status = ((*first_interface).GetDeviceHandle)(
            first_surface,
            &mut device_handle,
            &mut device_type,
        );
        if device_status != MFX_ERR_NONE || device_type != MFX_HANDLE_D3D11_DEVICE {
            return Err(BackendError::VplStatus {
                func: "mfxFrameSurfaceInterface::GetDeviceHandle",
                status: device_status,
            });
        }
        let Some(vpl_device) = <ID3D11Device as Interface>::from_raw_borrowed(&device_handle)
        else {
            return Err(BackendError::unsupported(
                "oneVPL record",
                "device handle",
                "GetDeviceHandle 返回值不是 ID3D11Device",
            ));
        };
        let (vpl_luid_low, vpl_luid_high) = d3d11_device_adapter_luid(vpl_device)?;
        if vpl_luid_low != desc.AdapterLuid.LowPart || vpl_luid_high != desc.AdapterLuid.HighPart {
            return Err(BackendError::unsupported(
                "oneVPL D3D11 device",
                format!(
                    "session LUID={:08X}:{:08X}, desktop adapter LUID={:08X}:{:08X}",
                    vpl_luid_high as u32,
                    vpl_luid_low,
                    desc.AdapterLuid.HighPart as u32,
                    desc.AdapterLuid.LowPart
                ),
                "不支持的桌面模式：oneVPL session 与捕获输出不在同一 DXGI adapter",
            ));
        }
        sink_status(
            &mut encoded_sink,
            format!(
                "初始化阶段：oneVPL video-memory surface/native D3D11 device 就绪，累计 {:.1}ms，本阶段 {:.1}ms",
                record_started.elapsed().as_secs_f64() * 1000.0,
                surface_started.elapsed().as_secs_f64() * 1000.0
            ),
        );

        let immediate: ID3D11DeviceContext =
            vpl_device
                .GetImmediateContext()
                .map_err(|err| BackendError::WindowsApi {
                    func: "ID3D11Device::GetImmediateContext",
                    message: err.to_string(),
                })?;
        if capture_source.is_wgc() {
            let priority = WGC_GPU_THREAD_PRIORITY;
            match set_d3d11_gpu_thread_priority(vpl_device, priority) {
                Ok(()) => notes.push(format!(
                    "WGC encoder D3D11 device GPU thread priority set to {priority}"
                )),
                Err(err) => notes.push(format!(
                    "WGC encoder D3D11 device GPU thread priority {priority} failed: {err}"
                )),
            }
        }
        notes.push(format!(
            "oneVPL record encode params: async_depth={}, rc={}, brc_multiplier={}, target_field={}kbps(effective≈{}kbps), max_field={}kbps(effective≈{}kbps), buffer_field={}KB, gop_pic_size={}, gop_ref_dist={}, idr_interval={}, ext_buffers={}",
            record_async_depth,
            rate_control.method.short_name(),
            param.mfx.BRCParamMultiplier,
            param.mfx.TargetKbps,
            u32::from(param.mfx.TargetKbps) * u32::from(param.mfx.BRCParamMultiplier.max(1)),
            param.mfx.MaxKbps,
            u32::from(param.mfx.MaxKbps) * u32::from(param.mfx.BRCParamMultiplier.max(1)),
            param.mfx.BufferSizeInKB,
            param.mfx.GopPicSize,
            param.mfx.GopRefDist,
            param.mfx.IdrInterval,
            param.NumExtParam
        ));
        notes.push(format!(
            "record backend route selected internally: {}",
            record_route.summary()
        ));
        notes.push(format!(
            "oneVPL surface native handle cache enabled={surface_cache_enabled}"
        ));
        notes.push(
            "首个正式 AU 通过 mfxEncodeCtrl FrameType=I|REF|IDR 强制为关键帧；编码预热 AU 仅用于驱动/表面预热和参数集提取，不写入正式时间线"
                .to_owned(),
        );
        let record_route_dxgi_format = record_route.try_dxgi_format()?;
        let gpu_route_started = Instant::now();
        let route_intermediate =
            create_route_intermediate(vpl_device, &target_desc, record_route, true)?;
        sink_status(
            &mut encoded_sink,
            format!(
                "初始化阶段：GPU route intermediate texture 就绪，累计 {:.1}ms，本阶段 {:.1}ms",
                record_started.elapsed().as_secs_f64() * 1000.0,
                gpu_route_started.elapsed().as_secs_f64() * 1000.0
            ),
        );
        match capture_source {
            RecordCaptureSource::Dda => notes.push(
                "固定 DDA 路线：独立 D3D11 capture device 获取 DDA 帧，捕获线程按自动 route GPU shader 写目标 FourCC keyed shared snapshot 后立即 ReleaseFrame，编码线程复制目标 FourCC snapshot 到 oneVPL surface"
                    .to_owned(),
            ),
            RecordCaptureSource::Wgc => notes.push(
                "固定 WGC 路线：使用 oneVPL native D3D11 device 创建 WGC capture，捕获端 GPU shader 写目标 FourCC ordinary ring texture、WGC 负责录制光标，编码线程仅 CopyResource 到 oneVPL surface"
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
        let mut route_converter: Option<GpuRecordConverter> = None;
        let mut conversion_ready = false;
        let format_flags_in = 0u32;
        let format_flags_out = 0u32;
        let mut pending_surface = Some(first_surface_guard.into_raw());
        let mut in_flight: VecDeque<Box<AsyncEncode>> =
            VecDeque::with_capacity(record_async_depth as usize);
        let bitstream_capacity_bytes = initial_bitstream_capacity_bytes(&param, record_route);
        let mut bitstream_pool: Vec<Vec<u8>> = Vec::with_capacity(record_async_depth as usize);
        for _ in 0..record_async_depth {
            bitstream_pool.push(vec![0u8; bitstream_capacity_bytes + 31]);
        }
        let _encode_cleanup_guard = VplEncodeCleanupGuard::new(&mut runtime, &mut pending_surface);
        notes.push(format!(
            "oneVPL bitstream pool：initial_per_buffer={} bytes async_depth={} total={} bytes；仅在 MFX_ERR_NOT_ENOUGH_BUFFER 时按需倍增并复用",
            bitstream_capacity_bytes,
            record_async_depth,
            bitstream_capacity_bytes.saturating_mul(record_async_depth as usize)
        ));
        let async_depth = record_async_depth as usize;
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
        // DDA/WGC are intentionally VFR: stop from the accepted source
        // timeline itself, without subtracting or synthesizing a nominal FPS
        // frame duration.
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
                target_desc.Width,
                target_desc.Height,
                record_route,
                matches!(capture_source, RecordCaptureSource::Dda),
            );
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
                        selected_output_index,
                        capture_device,
                        capture_context,
                        DdaOutputMode::SharedToEncoder(vpl_device.clone()),
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
                    vpl_device.clone(),
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
                "capture snapshot pool: textures={}, queue={}",
                capture_pool_size, capture_queue_size
            ));
            notes.push(if capture_source.is_wgc() {
                "WGC same-device ordinary slots are returned after encoder CopyResource submission; shared immediate-context command ordering prevents reuse before the copy"
                    .to_owned()
            } else {
                "capture thread: encoder-side shared snapshot slots are returned asynchronously after GPU event queries confirm route shader/copy consumed them"
                    .to_owned()
            });

            let mut capture_stats: Option<CaptureStats> = None;
            let mut capture_error: Option<CaptureFailure> = None;
            let mut active_source_desc: Option<D3D11_TEXTURE2D_DESC> = None;
            let mut pending_free_slots: Vec<CaptureFrameSlot> = Vec::new();
            let mut next_route_validation = Instant::now() + Duration::from_secs(1);

            loop {
                if record_stop.load(std::sync::atomic::Ordering::Relaxed) {
                    stop.store(true, std::sync::atomic::Ordering::Relaxed);
                    break;
                }
                if Instant::now() >= next_route_validation {
                    if let Some(plan) = route_plan {
                        let current_desc = selected_output.GetDesc().map_err(|err| {
                            BackendError::reconfigure_required(format!(
                                "读取当前输出状态失败：{err}"
                            ))
                        })?;
                        validate_current_display_route_plan(
                            plan,
                            adapter_index,
                            selected_output_index,
                            &current_desc,
                            &selected_output,
                            requested_chroma,
                        )
                        .map_err(|err| BackendError::reconfigure_required(err.to_string()))?;
                    }
                    next_route_validation = Instant::now() + Duration::from_secs(1);
                }
                return_ready_snapshot_slots(&mut pending_free_slots, &immediate, &free_tx, false)?;
                let msg = match frame_rx.recv_timeout(Duration::from_millis(2)) {
                    Ok(msg) => msg,
                    Err(std::sync::mpsc::RecvTimeoutError::Timeout) => {
                        return_ready_snapshot_slots(
                            &mut pending_free_slots,
                            &immediate,
                            &free_tx,
                            false,
                        )?;
                        let sync_started = Instant::now();
                        loop {
                            match try_sync_one_async_encode(
                                &api,
                                session,
                                &mut in_flight,
                                &mut bitstream_pool,
                                0,
                            )? {
                                TrySyncResult::Ready(Some(sample)) => push_record_hevc_sample(
                                    &mut samples,
                                    sample,
                                    &mut encoded_sink,
                                    retain_output_samples,
                                    &mut encoded_stats,
                                ),
                                TrySyncResult::Ready(None) => break,
                                TrySyncResult::NotReady => break,
                            }
                        }
                        if let Some(capture) = audio_capture.as_mut() {
                            capture.poll_live_aac(
                                first_video_timestamp_100ns,
                                last_submitted_sample_timestamp_90k,
                                &mut encoded_sink,
                                &mut notes,
                            )?;
                        }
                        perf.sync.add(sync_started.elapsed());
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
                                conversion_ready = false;
                                route_converter = None;
                                notes.push(format!(
                                    "capture format changed; route shader converter rebuilt for {}x{} DXGI_FORMAT({})",
                                    source_desc.Width, source_desc.Height, source_desc.Format.0
                                ));
                            }
                            active_source_desc = Some(source_desc);

                            let init_started = Instant::now();
                            let same_route_format =
                                source_desc.Format.0 == record_route_dxgi_format.0;
                            if !same_route_format
                                && !record_route
                                    .accepts_unconverted_capture_format(source_desc.Format)
                            {
                                return Err(BackendError::unsupported(
                                    "Capture -> ColorTransform",
                                    format!(
                                        "{} route 收到 DXGI_FORMAT({})",
                                        record_route.summary(),
                                        source_desc.Format.0
                                    ),
                                    "不支持的桌面模式",
                                ));
                            }
                            let direct_route_snapshot = same_route_format
                                && source_desc.Width == target_desc.Width
                                && source_desc.Height == target_desc.Height;
                            if !conversion_ready && same_route_format {
                                conversion_ready = true;
                                if direct_route_snapshot {
                                    notes.push(
                                        "捕获线程已产出目标格式/尺寸 shared snapshot；编码线程固定仅执行 GPU copy"
                                            .to_owned(),
                                    );
                                } else {
                                    notes.push(format!(
                                        "捕获线程已产出目标格式 shared snapshot，但尺寸 {}x{} != oneVPL surface {}x{}；编码线程固定用 GPU CopySubresourceRegion 填充有效区域",
                                        source_desc.Width,
                                        source_desc.Height,
                                        target_desc.Width,
                                        target_desc.Height
                                    ));
                                }
                            }
                            if !conversion_ready {
                                route_converter = Some(GpuRecordConverter::new(
                                    record_route,
                                    vpl_device,
                                    &immediate,
                                    &route_intermediate,
                                    source_desc.Width,
                                    source_desc.Height,
                                    true,
                                )?);
                                notes.push(format!(
                                    "输入 DXGI_FORMAT({}) 固定使用 GPU shader 全帧写入目标 YUV plane",
                                    source_desc.Format.0
                                ));
                                conversion_ready = true;
                            }
                            perf.init.add(init_started.elapsed());

                            if !dirty_rects.is_empty() {
                                dirty_metadata_frames += 1;
                                dirty_area_total += dirty_rect_area(&dirty_rects);
                            }
                            if move_rect_bytes > 0 {
                                move_metadata_frames += 1;
                            }
                            let surface_started = Instant::now();
                            let surface = if let Some(surface) = pending_surface.take() {
                                surface
                            } else {
                                let mut next_surface: *mut MfxFrameSurface1 = ptr::null_mut();
                                let status = (api.mfx_memory_get_surface_for_encode)(
                                    session,
                                    &mut next_surface,
                                );
                                if status != MFX_ERR_NONE || next_surface.is_null() {
                                    return Err(BackendError::unsupported(
                                        "oneVPL D3D11 surface import",
                                        format!(
                                            "{} + {}",
                                            record_route.summary(),
                                            rate_control.method.short_name()
                                        ),
                                        format!(
                                            "MFXMemory_GetSurfaceForEncode 返回 status={status}；该具体 route/码控字段组合无法继续提供 video-memory surface，禁止 CPU fallback"
                                        ),
                                    ));
                                }
                                next_surface
                            };

                            let frame_interface = (*surface).FrameInterface;
                            if frame_interface.is_null() {
                                let _ = surface_release(surface);
                                return Err(BackendError::unsupported(
                                    "oneVPL record",
                                    "FrameInterface",
                                    "oneVPL surface 没有 FrameInterface",
                                ));
                            }
                            let surface_guard = MfxSurfaceGuard::new(surface, frame_interface);
                            let (target, surface_cache_hit) = cached_vpl_surface_texture(
                                surface,
                                &mut surface_texture_cache,
                                surface_cache_enabled,
                            )?;
                            if surface_cache_hit {
                                perf.surface_cache_hits = perf.surface_cache_hits.saturating_add(1);
                            } else {
                                perf.surface_cache_misses =
                                    perf.surface_cache_misses.saturating_add(1);
                            }
                            perf.surface.add(surface_started.elapsed());

                            let mut keyed_mutex_guard = None;
                            let conversion_source = match &slot {
                                CaptureFrameSlot::Shared(shared) => {
                                    keyed_mutex_guard = Some(KeyedMutexGuard::acquire(
                                        &shared.encoder_mutex,
                                        1,
                                        0,
                                        1_000,
                                        "IDXGIKeyedMutex::AcquireSync(encoder snapshot)",
                                    )?);
                                    shared.encoder_texture.clone()
                                }
                                CaptureFrameSlot::FenceShared(_) => {
                                    return Err(BackendError::unsupported(
                                        "oneVPL capture slot",
                                        "D3D11 shared-fence NVENC slot",
                                        "oneVPL 路线不接受 NVENC 专用 shared-fence slot",
                                    ));
                                }
                                CaptureFrameSlot::Local(local) => local.texture.clone(),
                            };
                            let conversion_result = (|| -> Result<(), BackendError> {
                                if direct_route_snapshot {
                                    let copy_started = Instant::now();
                                    copy_texture_resource(&immediate, &conversion_source, &target)?;
                                    perf.copy.add(copy_started.elapsed());
                                    full_convert_frames += 1;
                                } else if same_route_format {
                                    let copy_started = Instant::now();
                                    copy_texture_subresource_region(
                                        &immediate,
                                        &conversion_source,
                                        &target,
                                        source_desc.Width.min(target_desc.Width),
                                        source_desc.Height.min(target_desc.Height),
                                    )?;
                                    perf.copy.add(copy_started.elapsed());
                                    full_convert_frames += 1;
                                } else if let Some(converter) = &route_converter {
                                    let convert_started = Instant::now();
                                    converter.convert(&conversion_source)?;
                                    perf.convert.add(convert_started.elapsed());
                                    full_convert_frames += 1;
                                } else {
                                    return Err(BackendError::unsupported(
                                        "GPU shader -> route YUV plane",
                                        format!("DXGI_FORMAT({}) 输入", source_desc.Format.0),
                                        "固定路线未初始化目标 YUV 转换器",
                                    ));
                                }
                                Ok(())
                            })();

                            let return_wgc_slot_immediately =
                                matches!(&slot, CaptureFrameSlot::Local(_));
                            let fence_started = Instant::now();
                            match &slot {
                                CaptureFrameSlot::Shared(shared) => {
                                    shared.encoder_fence.mark(&immediate);
                                    keyed_mutex_guard
                                        .take()
                                        .expect("shared snapshot owns keyed mutex guard")
                                        .release(
                                            "IDXGIKeyedMutex::ReleaseSync(encoder snapshot)",
                                        )?;
                                }
                                CaptureFrameSlot::FenceShared(_) => {}
                                CaptureFrameSlot::Local(_) => {}
                            }
                            perf.source_fence.add(fence_started.elapsed());
                            drop(keyed_mutex_guard);
                            conversion_result?;
                            if return_wgc_slot_immediately {
                                // WGC producer and encoder consumer share this immediate context.
                                // A reused slot's next conversion is ordered after this copy, so an
                                // event-query round trip would only delay slot recycling.
                                let _ = free_tx.send(slot);
                            } else {
                                pending_free_slots.push(slot);
                            }

                            if !same_route_format {
                                let copy_started = Instant::now();
                                copy_texture_resource(&immediate, &route_intermediate, &target)?;
                                perf.copy.add(copy_started.elapsed());
                            }

                            if warmup {
                                let warmup_ts90 = u64::from(warmup_encoded_frames)
                                    .saturating_mul(ENCODER_WARMUP_TIMESTAMP_STEP_90K);
                                warmup_encoded_frames = warmup_encoded_frames.saturating_add(1);
                                (*surface).Data.TimeStamp = warmup_ts90;
                                (*surface).Data.FrameOrder = warmup_encoded_frames;
                                let warmup_submit_started = Instant::now();
                                if bitstream_pool.is_empty() {
                                    while bitstream_pool.is_empty() && !in_flight.is_empty() {
                                        if let Some(sample) = sync_one_async_encode(
                                            &api,
                                            session,
                                            &mut in_flight,
                                            &mut bitstream_pool,
                                        )? {
                                            push_record_hevc_sample(
                                                &mut samples,
                                                sample,
                                                &mut encoded_sink,
                                                retain_output_samples,
                                                &mut encoded_stats,
                                            );
                                        }
                                    }
                                }
                                let bitstream_storage = bitstream_pool.pop().ok_or_else(|| {
                                    BackendError::unsupported(
                                        "oneVPL record",
                                        "bitstream pool",
                                        "没有可用于 warmup encode 的 bitstream 缓冲",
                                    )
                                })?;
                                let submitted = submit_encode_async(
                                    &api,
                                    session,
                                    AsyncEncodeRequest {
                                        surface,
                                        timestamp_90k: warmup_ts90,
                                        is_sync: warmup_encoded_frames == 1,
                                        storage: bitstream_storage,
                                        discard: true,
                                    },
                                    &mut bitstream_pool,
                                )?;
                                perf.submit.add(warmup_submit_started.elapsed());
                                surface_guard
                                    .release("mfxFrameSurfaceInterface::Release(warmup)")?;
                                if let Some(submitted) = submitted {
                                    in_flight.push_back(submitted);
                                }
                                let warmup_sync_started = Instant::now();
                                if rate_control.low_delay_brc {
                                    while !in_flight.is_empty() {
                                        if let Some(sample) = sync_one_async_encode(
                                            &api,
                                            session,
                                            &mut in_flight,
                                            &mut bitstream_pool,
                                        )? {
                                            push_record_hevc_sample(
                                                &mut samples,
                                                sample,
                                                &mut encoded_sink,
                                                retain_output_samples,
                                                &mut encoded_stats,
                                            );
                                        }
                                    }
                                } else {
                                    while in_flight.len() >= async_depth {
                                        match try_sync_one_async_encode(
                                            &api,
                                            session,
                                            &mut in_flight,
                                            &mut bitstream_pool,
                                            1,
                                        )? {
                                            TrySyncResult::Ready(Some(sample)) => {
                                                push_record_hevc_sample(
                                                    &mut samples,
                                                    sample,
                                                    &mut encoded_sink,
                                                    retain_output_samples,
                                                    &mut encoded_stats,
                                                )
                                            }
                                            TrySyncResult::Ready(None) => break,
                                            TrySyncResult::NotReady => break,
                                        }
                                    }
                                }
                                perf.sync.add(warmup_sync_started.elapsed());
                                perf.frame.add(frame_started.elapsed());
                                return Ok(());
                            }

                            if first_video_timestamp_100ns.is_none() {
                                sink_status(
                                    &mut encoded_sink,
                                    format!(
                                        "初始化阶段结束：首个正式源视频帧进入编码，累计 {:.1}ms；此前耗时属于 oneVPL/D3D11 初始化 + {} capture warmup",
                                        record_started.elapsed().as_secs_f64() * 1000.0,
                                        capture_source.label()
                                    ),
                                );
                                first_video_timestamp_100ns = timestamp_100ns;
                            }
                            let first_ts = *first_sample_timestamp_90k.get_or_insert(timestamp_90k);
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
                            (*surface).Data.TimeStamp = sample_ts90;
                            (*surface).Data.FrameOrder = captured_frames;
                            let submit_started = Instant::now();
                            if bitstream_pool.is_empty() {
                                while bitstream_pool.is_empty() && !in_flight.is_empty() {
                                    if let Some(sample) = sync_one_async_encode(
                                        &api,
                                        session,
                                        &mut in_flight,
                                        &mut bitstream_pool,
                                    )? {
                                        push_record_hevc_sample(
                                            &mut samples,
                                            sample,
                                            &mut encoded_sink,
                                            retain_output_samples,
                                            &mut encoded_stats,
                                        );
                                    }
                                }
                            }
                            let bitstream_storage = bitstream_pool.pop().ok_or_else(|| {
                                BackendError::unsupported(
                                    "oneVPL record",
                                    "bitstream pool",
                                    "没有可用 bitstream 缓冲，且无可同步的 in-flight encode",
                                )
                            })?;
                            let submitted = submit_encode_async(
                                &api,
                                session,
                                AsyncEncodeRequest {
                                    surface,
                                    timestamp_90k: sample_ts90,
                                    is_sync: force_idr,
                                    storage: bitstream_storage,
                                    discard: false,
                                },
                                &mut bitstream_pool,
                            )?;
                            perf.submit.add(submit_started.elapsed());
                            surface_guard.release("mfxFrameSurfaceInterface::Release")?;
                            if let Some(submitted) = submitted {
                                in_flight.push_back(submitted);
                            }
                            let sync_started = Instant::now();
                            while in_flight.len() >= async_depth {
                                match try_sync_one_async_encode(
                                    &api,
                                    session,
                                    &mut in_flight,
                                    &mut bitstream_pool,
                                    1,
                                )? {
                                    TrySyncResult::Ready(Some(sample)) => push_record_hevc_sample(
                                        &mut samples,
                                        sample,
                                        &mut encoded_sink,
                                        retain_output_samples,
                                        &mut encoded_stats,
                                    ),
                                    TrySyncResult::Ready(None) => break,
                                    TrySyncResult::NotReady => break,
                                }
                            }
                            while in_flight.len() >= VPL_RECORD_MAX_IN_FLIGHT {
                                if let Some(sample) = sync_one_async_encode(
                                    &api,
                                    session,
                                    &mut in_flight,
                                    &mut bitstream_pool,
                                )? {
                                    push_record_hevc_sample(
                                        &mut samples,
                                        sample,
                                        &mut encoded_sink,
                                        retain_output_samples,
                                        &mut encoded_stats,
                                    );
                                }
                            }
                            perf.sync.add(sync_started.elapsed());

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
            return_ready_snapshot_slots(&mut pending_free_slots, &immediate, &free_tx, true)?;
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
                    "停止排查：capture thread 已退出，开始快速释放；跳过段尾 60s 同步等待、encoder flush 和完整 AAC 重建，in_flight={}",
                    in_flight.len()
                ),
            );
            if let Some(surface) = pending_surface.take() {
                let frame_interface = (*surface).FrameInterface;
                if !frame_interface.is_null() {
                    let _ = ((*frame_interface).Release)(surface);
                }
            }
            let short_drain_deadline = Instant::now() + Duration::from_millis(200);
            while !in_flight.is_empty() && Instant::now() < short_drain_deadline {
                match try_sync_one_async_encode(
                    &api,
                    session,
                    &mut in_flight,
                    &mut bitstream_pool,
                    0,
                )? {
                    TrySyncResult::Ready(Some(sample)) => push_record_hevc_sample(
                        &mut samples,
                        sample,
                        &mut encoded_sink,
                        retain_output_samples,
                        &mut encoded_stats,
                    ),
                    TrySyncResult::Ready(None) => break,
                    TrySyncResult::NotReady => std::thread::sleep(Duration::from_millis(1)),
                }
            }
            if let Some(capture) = audio_capture.as_mut() {
                capture.stop_without_reencode(&mut notes);
            }
            surface_texture_cache.clear();
            let (close_status, mfx_close_status) = runtime.shutdown();
            sink_status(
                &mut encoded_sink,
                format!(
                    "停止排查：快速释放完成，close_status={} mfx_close_status={} leftover_in_flight={} cleanup={:.1}ms total={:.1}ms",
                    close_status,
                    mfx_close_status,
                    in_flight.len(),
                    stop_cleanup_started.elapsed().as_secs_f64() * 1000.0,
                    record_started.elapsed().as_secs_f64() * 1000.0
                ),
            );
            return Err(BackendError::cancelled("oneVPL 录制循环"));
        }

        if let Some(surface) = pending_surface.take() {
            let frame_interface = (*surface).FrameInterface;
            if !frame_interface.is_null() {
                MfxSurfaceGuard::new(surface, frame_interface)
                    .release("mfxFrameSurfaceInterface::Release(unused pending surface)")?;
            }
        }

        while !in_flight.is_empty() {
            if let Some(sample) =
                sync_one_async_encode(&api, session, &mut in_flight, &mut bitstream_pool)?
            {
                push_record_hevc_sample(
                    &mut samples,
                    sample,
                    &mut encoded_sink,
                    retain_output_samples,
                    &mut encoded_stats,
                );
            }
        }
        let skipped_flush_samples = flush_encoder(
            &api,
            session,
            &mut samples,
            &mut encoded_sink,
            retain_output_samples,
            &mut encoded_stats,
            &mut bitstream_pool,
        )?;
        let duration_90k = encoded_timeline_duration_90k(
            &samples,
            encoded_stats.last_timestamp_90k,
            requested_duration_90k,
            !capture_source.is_wgc(),
        );
        surface_texture_cache.clear();
        let (close_status, mfx_close_status) = runtime.shutdown();
        if close_status != MFX_ERR_NONE {
            return Err(BackendError::VplStatus {
                func: "MFXVideoENCODE_Close",
                status: close_status,
            });
        }
        if mfx_close_status != MFX_ERR_NONE {
            return Err(BackendError::VplStatus {
                func: "MFXClose",
                status: mfx_close_status,
            });
        }

        let encoded_samples = encoded_stats.encoded_samples;
        let encoded_bytes = encoded_stats.encoded_bytes;
        let discarded_header_units = encoded_stats.discarded_header_units;
        if encoded_samples == 0 {
            return Err(BackendError::unsupported(
                "oneVPL encode",
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
        if write_output_mp4 {
            write_hevc_aac_mp4(output, &video_track, audio_track.as_ref())?;
        } else {
            notes.push(format!(
                "生产会话以内存 encoded ring 为主，跳过临时 MP4 写出：{}；保存时再从已编码 HEVC/AAC 快照 mux",
                output.display()
            ));
        }

        notes.push("视频路径固定为 DDA texture/WGC BGRA8/FP16 -> keyed/shared GPU snapshot；DDA 在编码线程按自动 route 全帧 GPU shader 写目标 FourCC，WGC 在捕获线程按自动 route GPU shader 写目标 FourCC；随后一次 CopyResource 到 oneVPL surface -> HEVC -> MP4；未做 raw frame CPU 回读".to_owned());
        notes.push(format!(
            "route 色彩元数据同步写入 MP4 nclx 与 oneVPL mfxExtVideoSignalInfo(VUI)：当前 route nclx={}/{}/{} range={}；10-bit/HDR route 要求 FP16 capture 或已转换目标格式，HDR FP16 按 scRGB 线性 80nit/1.0 转 BT.2020 ST2084 PQ，SDR route 按当前显示色彩写 BT.709 或 BT.2020 SDR YUV",
            record_route.mp4_color.colour_primaries,
            record_route.mp4_color.transfer_characteristics,
            record_route.mp4_color.matrix_coefficients,
            if record_route.mp4_color.full_range {
                "full"
            } else {
                "limited"
            }
        ));
        if skipped_flush_samples > 0 {
            notes.push(format!(
                "oneVPL flush 返回了 {skipped_flush_samples} 个重复/非单调时间戳 AU；为保持 WGC/DDA 源 VFR 时间戳，未按外部 CFR 伪造时间戳，已跳过这些不可封装为正时长 sample 的 AU"
            ));
        }
        if discarded_header_units > 0 {
            notes.push(format!(
                "MP4 muxer 已从 {discarded_header_units} 个编码预热 AU 中提取 VPS/SPS/PPS，但这些预热 AU 不写入正式视频时间线"
            ));
        }
        if capture_source.is_wgc() {
            notes.push(
                "WGC timestamp policy: MP4 sample timestamps are WGC SystemRelativeTime relative to the first accepted source frame; source-to-source gaps are preserved as VFR sample-duration gaps, and no external CFR clock is used to synthesize missing timestamps"
                    .to_owned(),
            );
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
            "DDA dirty rect 元数据统计（固定路线不做 partial dirty-rect 转换）: partial={} full={} dirty_metadata_frames={} move_metadata_frames={} avg_dirty_area={:.0}px",
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
            query_status,
            init_status,
            close_status,
            first_get_surface_status,
            video_processor_format_flags_in: format_flags_in,
            video_processor_format_flags_out: format_flags_out,
            notes,
        };

        sink_status(
            &mut encoded_sink,
            format!(
                "录制段正常结束并完成封装准备：captured_frames={} video_au={} audio_au={} total={:.1}ms",
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
