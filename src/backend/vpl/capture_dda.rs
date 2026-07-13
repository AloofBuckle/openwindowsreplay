use super::*;

#[cfg(windows)]
pub(super) enum CaptureMsg {
    Frame(CapturedSnapshot),
    Done(CaptureStats),
    Error(String),
}

#[cfg(windows)]
#[allow(clippy::too_many_arguments)]
pub(super) fn spawn_dda_capture_thread(
    adapter1: windows::Win32::Graphics::Dxgi::IDXGIAdapter1,
    device: windows::Win32::Graphics::Direct3D11::ID3D11Device,
    context: windows::Win32::Graphics::Direct3D11::ID3D11DeviceContext,
    encoder_device: windows::Win32::Graphics::Direct3D11::ID3D11Device,
    d3d_multithread: Option<windows::Win32::Graphics::Direct3D11::ID3D11Multithread>,
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
                device,
                context,
                encoder_device,
                d3d_multithread,
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
                let _ = frame_tx.send(CaptureMsg::Error(message));
            }
        }
    })
}

#[cfg(windows)]
#[allow(clippy::too_many_arguments)]
pub(super) unsafe fn run_dda_capture_thread(
    adapter1: windows::Win32::Graphics::Dxgi::IDXGIAdapter1,
    device: windows::Win32::Graphics::Direct3D11::ID3D11Device,
    context: windows::Win32::Graphics::Direct3D11::ID3D11DeviceContext,
    encoder_device: windows::Win32::Graphics::Direct3D11::ID3D11Device,
    d3d_multithread: Option<windows::Win32::Graphics::Direct3D11::ID3D11Multithread>,
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
    use windows::Win32::Graphics::Direct3D11::D3D11_TEXTURE2D_DESC;
    use windows::Win32::Graphics::Dxgi::{
        DXGI_ERROR_WAIT_TIMEOUT, DXGI_OUTDUPL_FRAME_INFO, IDXGIResource,
    };
    use windows::core::Interface;

    let _thread_priority = RecordThreadPriorityGuard::raise_capture_thread();
    let duplication =
        create_duplication_on_device(&adapter1, &device, route).map_err(|err| err.to_string())?;
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
    let mut encoder_warmup_pending = false;
    let mut encoder_warmup_done = false;
    let dda_pipeline_warmup_frames = 4u32;
    // Keep only a short frame-count warmup after encoder warmup. The official
    // timeline starts from DDA LastPresentTime and does not require a fixed
    // refresh-rate interval to become "stable".
    let dda_pipeline_warmup_stable_intervals_required = 0u32;
    let mut dda_pipeline_warmup_remaining = 0u32;
    let dda_pipeline_warmup_stable_intervals = 0u32;
    let max_end_at = end_at + std::time::Duration::from_secs(3);

    while !stop.load(Ordering::Relaxed) && std::time::Instant::now() < max_end_at {
        while let Ok(slot) = free_rx.try_recv() {
            if encoder_warmup_pending {
                encoder_warmup_pending = false;
                encoder_warmup_done = true;
                dda_pipeline_warmup_remaining = dda_pipeline_warmup_frames;
                timestamp_origin_qpc = None;
                last_timestamp_90k = None;
                last_accepted_present_qpc = None;
            }
            let slot_matches = match &slot {
                CaptureFrameSlot::Shared(shared) => snapshot_desc0
                    .as_ref()
                    .is_none_or(|desc| snapshot_slot_matches(shared, desc)),
                CaptureFrameSlot::WgcLocal(_) => false,
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

            let source_changed = source_desc0.is_none_or(|first| {
                first.Width != source_desc.Width
                    || first.Height != source_desc.Height
                    || first.Format.0 != source_desc.Format.0
            });
            if source_changed {
                let first_source = source_desc0.is_none();
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
                route_intermediate = Some(
                    create_route_intermediate(&device, &snapshot_desc, route, true)
                        .map_err(|err| err.to_string())?,
                );
                let intermediate = route_intermediate
                    .as_ref()
                    .ok_or_else(|| "DDA route intermediate missing after create".to_owned())?;
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
                snapshot_desc0 = Some(snapshot_desc);
                free_slots.clear();
                for id in 0..pool_size {
                    let slot =
                        create_shared_snapshot_slot(id, &device, &encoder_device, &snapshot_desc)
                            .map_err(|err| err.to_string())?;
                    free_slots.push_back(CaptureFrameSlot::Shared(slot));
                }
                if first_source {
                    stats.dropped_warmup += 1;
                    return Ok(());
                }
            }
            let snapshot_desc =
                snapshot_desc0.ok_or_else(|| "DDA route snapshot desc missing".to_owned())?;

            if encoder_warmup_pending {
                if frame_info.LastPresentTime > 0 {
                    last_accepted_present_qpc = Some(frame_info.LastPresentTime);
                }
                stats.dropped_warmup += 1;
                return Ok(());
            }

            let Some(slot) = free_slots.pop_front() else {
                stats.dropped_no_slot += 1;
                return Ok(());
            };

            {
                let _guard = D3d11MultithreadGuard::enter(&d3d_multithread);
                match &slot {
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
                    CaptureFrameSlot::WgcLocal(_) => {
                        return Err("DDA received unexpected WGC-local slot".to_owned());
                    }
                }
            }
            stats.copied += 1;
            if frame_info.LastPresentTime > 0 {
                last_accepted_present_qpc = Some(frame_info.LastPresentTime);
            }
            if !encoder_warmup_done {
                let captured = CapturedSnapshot {
                    slot,
                    source_desc: snapshot_desc,
                    move_rect_bytes: frame_metadata.move_rect_bytes,
                    dirty_rects: frame_metadata.dirty_rects,
                    timestamp_90k: 0,
                    timestamp_100ns: qpc_counter_to_100ns(
                        frame_info.LastPresentTime,
                        qpc_frequency,
                    ),
                    capture_index,
                    accumulated_frames: frame_info.AccumulatedFrames,
                    warmup: true,
                };
                encoder_warmup_pending = true;
                stats.dropped_warmup += 1;
                if frame_tx.send(CaptureMsg::Frame(captured)).is_err() {
                    stop.store(true, Ordering::Relaxed);
                }
                return Ok(());
            }
            let pipeline_warmup_frame = dda_pipeline_warmup_remaining > 0
                || dda_pipeline_warmup_stable_intervals
                    < dda_pipeline_warmup_stable_intervals_required;
            if pipeline_warmup_frame {
                dda_pipeline_warmup_remaining = dda_pipeline_warmup_remaining.saturating_sub(1);
                stats.dropped_warmup += 1;
                timestamp_origin_qpc = None;
                last_timestamp_90k = None;
                let captured = CapturedSnapshot {
                    slot,
                    source_desc: snapshot_desc,
                    move_rect_bytes: frame_metadata.move_rect_bytes,
                    dirty_rects: frame_metadata.dirty_rects,
                    timestamp_90k: 0,
                    timestamp_100ns: qpc_counter_to_100ns(
                        frame_info.LastPresentTime,
                        qpc_frequency,
                    ),
                    capture_index,
                    accumulated_frames: frame_info.AccumulatedFrames,
                    warmup: true,
                };
                if frame_tx.send(CaptureMsg::Frame(captured)).is_err() {
                    stop.store(true, Ordering::Relaxed);
                }
                return Ok(());
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
