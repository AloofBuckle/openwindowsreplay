use super::*;

#[cfg(windows)]
pub(super) enum CaptureMsg {
    Frame(CapturedSnapshot),
    Done(CaptureStats),
    Error(CaptureFailure),
}

#[cfg(windows)]
pub(super) enum DdaOutputMode {
    SharedToEncoder(windows::Win32::Graphics::Direct3D11::ID3D11Device),
    SharedFenceToEncoder(windows::Win32::Graphics::Direct3D11::ID3D11Device),
}

#[cfg(windows)]
const DDA_RECONFIGURE_PREFIX: &str = "DDA_RECONFIGURE_REQUIRED:";

#[cfg(windows)]
#[allow(clippy::too_many_arguments)]
pub(super) fn spawn_dda_capture_thread(
    adapter1: windows::Win32::Graphics::Dxgi::IDXGIAdapter1,
    output_index: u32,
    device: windows::Win32::Graphics::Direct3D11::ID3D11Device,
    context: windows::Win32::Graphics::Direct3D11::ID3D11DeviceContext,
    output_mode: DdaOutputMode,
    start: std::time::Instant,
    end_at: std::time::Instant,
    source_stop_90k: u64,
    qpc_frequency: i64,
    route: VplRecordRoute,
    target_width: u32,
    target_height: u32,
    pool_size: usize,
    stop: std::sync::Arc<std::sync::atomic::AtomicBool>,
    frame_tx: std::sync::mpsc::Sender<CaptureMsg>,
    free_rx: std::sync::mpsc::Receiver<CaptureFrameSlot>,
) -> std::thread::JoinHandle<()> {
    std::thread::spawn(move || {
        let result = unsafe {
            run_dda_capture_thread(
                adapter1,
                output_index,
                device,
                context,
                output_mode,
                start,
                end_at,
                source_stop_90k,
                qpc_frequency,
                route,
                target_width,
                target_height,
                pool_size,
                stop,
                &frame_tx,
                free_rx,
            )
        };
        match result {
            Ok(stats) => {
                let _ = frame_tx.send(CaptureMsg::Done(stats));
            }
            Err(message) => {
                let failure = message
                    .strip_prefix(DDA_RECONFIGURE_PREFIX)
                    .map(|message| CaptureFailure::Reconfigure(message.trim().to_owned()))
                    .unwrap_or(CaptureFailure::Fatal(message));
                let _ = frame_tx.send(CaptureMsg::Error(failure));
            }
        }
    })
}

#[cfg(windows)]
#[allow(clippy::too_many_arguments)]
pub(super) unsafe fn run_dda_capture_thread(
    adapter1: windows::Win32::Graphics::Dxgi::IDXGIAdapter1,
    output_index: u32,
    device: windows::Win32::Graphics::Direct3D11::ID3D11Device,
    context: windows::Win32::Graphics::Direct3D11::ID3D11DeviceContext,
    output_mode: DdaOutputMode,
    start: std::time::Instant,
    end_at: std::time::Instant,
    source_stop_90k: u64,
    qpc_frequency: i64,
    route: VplRecordRoute,
    target_width: u32,
    target_height: u32,
    pool_size: usize,
    stop: std::sync::Arc<std::sync::atomic::AtomicBool>,
    frame_tx: &std::sync::mpsc::Sender<CaptureMsg>,
    free_rx: std::sync::mpsc::Receiver<CaptureFrameSlot>,
) -> Result<CaptureStats, String> {
    use std::collections::VecDeque;
    use std::sync::atomic::Ordering;
    use windows::Win32::Graphics::Direct3D11::{D3D11_TEXTURE2D_DESC, ID3D11DeviceContext4};
    use windows::Win32::Graphics::Dxgi::{
        DXGI_ERROR_ACCESS_LOST, DXGI_ERROR_DEVICE_REMOVED, DXGI_ERROR_DEVICE_RESET,
        DXGI_ERROR_SESSION_DISCONNECTED, DXGI_ERROR_WAIT_TIMEOUT, DXGI_OUTDUPL_FRAME_INFO,
        IDXGIResource,
    };
    use windows::core::Interface;

    let _thread_priority = RecordThreadPriorityGuard::raise_capture_thread();
    let capture_context4: Option<ID3D11DeviceContext4> =
        if matches!(&output_mode, DdaOutputMode::SharedFenceToEncoder(_)) {
            Some(context.cast().map_err(|err| {
                format!("DDA shared-fence capture requires ID3D11DeviceContext4: {err}")
            })?)
        } else {
            None
        };
    let duplication = create_duplication_on_device(&adapter1, output_index, &device, route)
        .map_err(|err| err.to_string())?;
    let mut stats = CaptureStats::new();
    let mut free_slots: VecDeque<CaptureFrameSlot> = VecDeque::new();
    let mut source_desc0: Option<D3D11_TEXTURE2D_DESC> = None;
    let mut snapshot_desc0: Option<D3D11_TEXTURE2D_DESC> = None;
    let mut route_intermediate: Option<windows::Win32::Graphics::Direct3D11::ID3D11Texture2D> =
        None;
    let mut route_converter: Option<GpuRecordConverter> = None;
    let mut capture_index = 0u64;
    let mut timestamp_origin_qpc: Option<i64> = None;
    let mut last_timestamp_90k: Option<u64> = None;
    let mut last_accepted_present_qpc: Option<i64> = None;
    let max_end_at = end_at + std::time::Duration::from_secs(3);

    while !stop.load(Ordering::Relaxed) && std::time::Instant::now() < max_end_at {
        while let Ok(slot) = free_rx.try_recv() {
            let slot_matches = match &slot {
                CaptureFrameSlot::Shared(shared) => {
                    matches!(&output_mode, DdaOutputMode::SharedToEncoder(_))
                        && snapshot_desc0
                            .as_ref()
                            .is_none_or(|desc| snapshot_slot_matches(shared, desc))
                }
                CaptureFrameSlot::FenceShared(shared) => {
                    matches!(&output_mode, DdaOutputMode::SharedFenceToEncoder(_))
                        && snapshot_desc0
                            .as_ref()
                            .is_none_or(|desc| shared_fence_slot_matches(shared, desc))
                }
                CaptureFrameSlot::Local(_) => false,
            };
            if slot_matches {
                free_slots.push_back(slot);
            }
        }

        let mut frame_info = DXGI_OUTDUPL_FRAME_INFO::default();
        let mut resource: Option<IDXGIResource> = None;
        match duplication.AcquireNextFrame(8, &mut frame_info, &mut resource) {
            Ok(()) => {}
            Err(err) if err.code() == DXGI_ERROR_WAIT_TIMEOUT => {
                stats.dda_timeouts += 1;
                continue;
            }
            Err(err)
                if matches!(
                    err.code(),
                    DXGI_ERROR_ACCESS_LOST
                        | DXGI_ERROR_DEVICE_REMOVED
                        | DXGI_ERROR_DEVICE_RESET
                        | DXGI_ERROR_SESSION_DISCONNECTED
                ) =>
            {
                return Err(format!(
                    "{DDA_RECONFIGURE_PREFIX} IDXGIOutputDuplication 失效 ({err})；桌面会话、显示模式、设备或输出所有权已变化"
                ));
            }
            Err(err) => {
                return Err(format!(
                    "IDXGIOutputDuplication::AcquireNextFrame(capture): {err}"
                ));
            }
        }

        let mut reached_source_end = false;
        let frame_result = (|| -> Result<(), String> {
            stats.acquired += 1;
            stats.accumulated_frames_total += u64::from(frame_info.AccumulatedFrames);
            stats.accumulated_frames_max = stats
                .accumulated_frames_max
                .max(frame_info.AccumulatedFrames);

            let resource =
                resource.ok_or_else(|| "AcquireNextFrame returned null resource".to_owned())?;
            let source: windows::Win32::Graphics::Direct3D11::ID3D11Texture2D = resource
                .cast()
                .map_err(|err| format!("IDXGIResource::cast<ID3D11Texture2D>(capture): {err}"))?;
            let mut source_desc = D3D11_TEXTURE2D_DESC::default();
            source.GetDesc(&mut source_desc);
            let frame_metadata =
                read_dda_frame_metadata(&duplication, frame_info.TotalMetadataBufferSize)
                    .map_err(|err| err.to_string())?;

            if frame_info.LastPresentTime > 0
                && last_accepted_present_qpc.is_some_and(|last| frame_info.LastPresentTime <= last)
            {
                stats.dropped_duplicate_timestamp += 1;
                return Ok(());
            }
            if qpc_frequency > 0
                && timestamp_origin_qpc.is_none()
                && frame_info.LastPresentTime <= 0
            {
                stats.dropped_duplicate_timestamp += 1;
                return Ok(());
            }

            if let Some(first) = source_desc0
                && (first.Width != source_desc.Width
                    || first.Height != source_desc.Height
                    || first.Format.0 != source_desc.Format.0)
            {
                return Err(format!(
                    "{DDA_RECONFIGURE_PREFIX} DDA source desc changed during capture: {}x{} fmt {} -> {}x{} fmt {}",
                    first.Width,
                    first.Height,
                    first.Format.0,
                    source_desc.Width,
                    source_desc.Height,
                    source_desc.Format.0
                ));
            }
            if source_desc0.is_none() {
                if !route.accepts_unconverted_capture_format(source_desc.Format) {
                    return Err(format!(
                        "DDA DuplicateOutput1 returned DXGI_FORMAT({}) for {}; the route requires FP16 source data and refuses an 8-bit HDR downgrade",
                        source_desc.Format.0,
                        route.summary()
                    ));
                }
                source_desc0 = Some(source_desc);
                let snapshot_desc = D3D11_TEXTURE2D_DESC {
                    Width: target_width.max(1),
                    Height: target_height.max(1),
                    MipLevels: 1,
                    ArraySize: 1,
                    Format: route.try_dxgi_format().map_err(|err| err.to_string())?,
                    SampleDesc: source_desc.SampleDesc,
                    Usage: source_desc.Usage,
                    BindFlags: 0,
                    CPUAccessFlags: 0,
                    MiscFlags: 0,
                };
                snapshot_desc0 = Some(snapshot_desc);
                free_slots.clear();
                match &output_mode {
                    DdaOutputMode::SharedToEncoder(encoder_device) => {
                        route_intermediate = Some(
                            create_route_intermediate(&device, &snapshot_desc, route, true)
                                .map_err(|err| err.to_string())?,
                        );
                        let intermediate = route_intermediate.as_ref().ok_or_else(|| {
                            "DDA route intermediate missing after create".to_owned()
                        })?;
                        route_converter = Some(
                            GpuRecordConverter::new(
                                route,
                                &device,
                                &context,
                                intermediate,
                                source_desc.Width,
                                source_desc.Height,
                                true,
                            )
                            .map_err(|err| err.to_string())?,
                        );
                        for id in 0..pool_size {
                            let slot = create_shared_snapshot_slot(
                                id,
                                &device,
                                encoder_device,
                                &snapshot_desc,
                            )
                            .map_err(|err| err.to_string())?;
                            free_slots.push_back(CaptureFrameSlot::Shared(slot));
                        }
                    }
                    DdaOutputMode::SharedFenceToEncoder(encoder_device) => {
                        let source_srv_cache = std::sync::Arc::new(std::sync::Mutex::new(
                            ShaderResourceViewCache::retained(),
                        ));
                        for id in 0..pool_size {
                            let slot = create_shared_fence_route_slot(
                                id,
                                &device,
                                &context,
                                encoder_device,
                                &snapshot_desc,
                                route,
                                source_desc.Width,
                                source_desc.Height,
                                source_srv_cache.clone(),
                            )
                            .map_err(|err| err.to_string())?;
                            free_slots.push_back(CaptureFrameSlot::FenceShared(slot));
                        }
                    }
                }
            }
            let snapshot_desc =
                snapshot_desc0.ok_or_else(|| "DDA route snapshot desc missing".to_owned())?;

            let Some(mut slot) = free_slots.pop_front() else {
                stats.dropped_no_slot += 1;
                return Ok(());
            };

            {
                match &mut slot {
                    CaptureFrameSlot::Shared(shared) => {
                        let mutex_guard = KeyedMutexGuard::acquire(
                            &shared.capture_mutex,
                            0,
                            1,
                            1_000,
                            "IDXGIKeyedMutex::AcquireSync(capture)",
                        )
                        .map_err(|err| err.to_string())?;
                        let convert_result = (|| -> Result<(), BackendError> {
                            let intermediate = route_intermediate.as_ref().ok_or_else(|| {
                                BackendError::unsupported(
                                    "DDA route",
                                    "shared intermediate",
                                    "DDA route intermediate missing",
                                )
                            })?;
                            let converter = route_converter.as_ref().ok_or_else(|| {
                                BackendError::unsupported(
                                    "DDA route",
                                    "shared converter",
                                    "DDA route converter missing",
                                )
                            })?;
                            converter.convert(&source)?;
                            copy_texture_resource(&context, intermediate, &shared.capture_texture)
                        })();
                        mutex_guard
                            .release("IDXGIKeyedMutex::ReleaseSync(capture)")
                            .map_err(|err| err.to_string())?;
                        convert_result.map_err(|err| err.to_string())?;
                    }
                    CaptureFrameSlot::FenceShared(shared) => {
                        shared.converter.convert(&source).map_err(|err| {
                            format!(
                                "DDA shared-fence route conversion failed: input_tex_format={} route_tex_format={} target={}x{}; {err}",
                                source_desc.Format.0,
                                snapshot_desc.Format.0,
                                snapshot_desc.Width,
                                snapshot_desc.Height
                            )
                        })?;
                        shared.fence_value = shared.fence_value.saturating_add(1);
                        capture_context4
                            .as_ref()
                            .ok_or_else(|| {
                                "DDA shared-fence capture context is unavailable".to_owned()
                            })?
                            .Signal(&shared.capture_fence, shared.fence_value)
                            .map_err(|err| {
                                format!("ID3D11DeviceContext4::Signal(DDA capture): {err}")
                            })?;
                    }
                    CaptureFrameSlot::Local(_) => {
                        return Err("DDA received unexpected local route slot".to_owned());
                    }
                }
            }
            stats.copied += 1;
            if frame_info.LastPresentTime > 0 {
                last_accepted_present_qpc = Some(frame_info.LastPresentTime);
            }
            let previous_timestamp_90k = last_timestamp_90k;
            let mut timestamp_90k = dda_relative_timestamp_90k(
                frame_info.LastPresentTime,
                qpc_frequency,
                start,
                &mut timestamp_origin_qpc,
                &mut last_timestamp_90k,
            );
            if let Some(previous) = previous_timestamp_90k {
                if timestamp_90k <= previous {
                    timestamp_90k = previous.saturating_add(1);
                    last_timestamp_90k = Some(timestamp_90k);
                }
                stats
                    .observe_source_interval(capture_index, timestamp_90k.saturating_sub(previous));
            }
            reached_source_end = timestamp_90k >= source_stop_90k;
            let captured = CapturedSnapshot {
                slot,
                source_desc: snapshot_desc,
                move_rect_bytes: frame_metadata.move_rect_bytes,
                dirty_rects: frame_metadata.dirty_rects,
                timestamp_90k,
                timestamp_100ns: qpc_counter_to_100ns(frame_info.LastPresentTime, qpc_frequency),
                capture_index,
                accumulated_frames: frame_info.AccumulatedFrames,
                warmup: false,
            };
            capture_index += 1;

            if frame_tx.send(CaptureMsg::Frame(captured)).is_err() {
                stop.store(true, Ordering::Relaxed);
            }
            Ok(())
        })();

        duplication
            .ReleaseFrame()
            .map_err(|err| format!("IDXGIOutputDuplication::ReleaseFrame(capture): {err}"))?;
        frame_result?;
        if reached_source_end {
            break;
        }
    }

    Ok(stats)
}
