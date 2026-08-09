use super::*;

const WGC_EVENT_WAIT_MAX: std::time::Duration = std::time::Duration::from_millis(50);

pub(super) fn wgc_event_wait_timeout(
    coalesce_remaining: Option<std::time::Duration>,
    deadline_remaining: std::time::Duration,
) -> std::time::Duration {
    let timeout = WGC_EVENT_WAIT_MAX.min(deadline_remaining);
    coalesce_remaining.map_or(timeout, |remaining| timeout.min(remaining))
}

#[cfg(windows)]
struct WgcFrameArrivedRegistration {
    frame_pool: windows::Graphics::Capture::Direct3D11CaptureFramePool,
    token: i64,
}

#[cfg(windows)]
impl Drop for WgcFrameArrivedRegistration {
    fn drop(&mut self) {
        let _ = self.frame_pool.RemoveFrameArrived(self.token);
    }
}

#[cfg(windows)]
struct WgcCaptureJob {
    adapter1: windows::Win32::Graphics::Dxgi::IDXGIAdapter1,
    output_index: u32,
    encoder_device: windows::Win32::Graphics::Direct3D11::ID3D11Device,
    start: std::time::Instant,
    end_at: std::time::Instant,
    source_stop_90k: u64,
    route: VplRecordRoute,
    target_width: u32,
    target_height: u32,
    pool_size: usize,
    stop: std::sync::Arc<std::sync::atomic::AtomicBool>,
    frame_tx: std::sync::mpsc::Sender<CaptureMsg>,
    free_rx: std::sync::mpsc::Receiver<CaptureFrameSlot>,
    result_tx: std::sync::mpsc::Sender<Result<CaptureStats, String>>,
}

#[cfg(windows)]
std::thread_local! {
    static WGC_WINRT_DEVICE_CACHE: std::cell::RefCell<std::collections::HashMap<usize, windows::Graphics::DirectX::Direct3D11::IDirect3DDevice>> =
        std::cell::RefCell::new(std::collections::HashMap::new());
}

#[cfg(windows)]
fn spawn_wgc_capture_service() -> Result<std::sync::mpsc::Sender<WgcCaptureJob>, std::io::Error> {
    let (tx, rx) = std::sync::mpsc::channel::<WgcCaptureJob>();
    std::thread::Builder::new()
        .name("rustreplay-wgc-mta".to_owned())
        .spawn(move || {
            use windows::Win32::Foundation::RPC_E_CHANGED_MODE;
            use windows::Win32::System::WinRT::{
                RO_INIT_MULTITHREADED, RoInitialize, RoUninitialize,
            };

            let initialized = unsafe {
                match RoInitialize(RO_INIT_MULTITHREADED) {
                    Ok(()) => Ok(true),
                    Err(err) if err.code() == RPC_E_CHANGED_MODE => Ok(false),
                    Err(err) => Err(format!("RoInitialize(WGC service): {err}")),
                }
            };
            while let Ok(job) = rx.recv() {
                let result = match &initialized {
                    Ok(_) => std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| unsafe {
                        run_wgc_capture_thread(
                            job.adapter1,
                            job.output_index,
                            job.encoder_device,
                            job.start,
                            job.end_at,
                            job.source_stop_90k,
                            job.route,
                            job.target_width,
                            job.target_height,
                            job.pool_size,
                            job.stop,
                            job.frame_tx,
                            job.free_rx,
                        )
                    }))
                    .unwrap_or_else(|_| Err("persistent WGC capture job panicked".to_owned())),
                    Err(message) => Err(message.clone()),
                };
                let _ = job.result_tx.send(result);
            }
            if matches!(initialized, Ok(true)) {
                unsafe { RoUninitialize() };
            }
        })?;
    Ok(tx)
}

#[cfg(windows)]
fn dispatch_wgc_capture_job(mut job: WgcCaptureJob) -> Result<(), String> {
    static SERVICE: std::sync::OnceLock<
        std::sync::Mutex<Option<std::sync::mpsc::Sender<WgcCaptureJob>>>,
    > = std::sync::OnceLock::new();
    let service = SERVICE.get_or_init(|| std::sync::Mutex::new(None));
    let mut service = service
        .lock()
        .map_err(|_| "persistent WGC service mutex poisoned".to_owned())?;
    for _ in 0..2 {
        if service.is_none() {
            *service = Some(
                spawn_wgc_capture_service()
                    .map_err(|err| format!("spawn persistent WGC service: {err}"))?,
            );
        }
        let sender = service.as_ref().expect("initialized above").clone();
        match sender.send(job) {
            Ok(()) => return Ok(()),
            Err(err) => {
                job = err.0;
                *service = None;
            }
        }
    }
    Err("persistent WGC capture service is unavailable after restart".to_owned())
}

#[cfg(windows)]
#[allow(clippy::too_many_arguments)]
pub(super) fn spawn_wgc_capture_thread(
    adapter1: windows::Win32::Graphics::Dxgi::IDXGIAdapter1,
    output_index: u32,
    encoder_device: windows::Win32::Graphics::Direct3D11::ID3D11Device,
    start: std::time::Instant,
    end_at: std::time::Instant,
    source_stop_90k: u64,
    route: VplRecordRoute,
    target_width: u32,
    target_height: u32,
    pool_size: usize,
    stop: std::sync::Arc<std::sync::atomic::AtomicBool>,
    frame_tx: std::sync::mpsc::Sender<CaptureMsg>,
    free_rx: std::sync::mpsc::Receiver<CaptureFrameSlot>,
) -> std::thread::JoinHandle<()> {
    let (result_tx, result_rx) = std::sync::mpsc::channel();
    let job = WgcCaptureJob {
        adapter1,
        output_index,
        encoder_device,
        start,
        end_at,
        source_stop_90k,
        route,
        target_width,
        target_height,
        pool_size,
        stop,
        frame_tx: frame_tx.clone(),
        free_rx,
        result_tx,
    };
    std::thread::spawn(move || {
        let result = if let Err(err) = dispatch_wgc_capture_job(job) {
            Err(err)
        } else {
            result_rx
                .recv()
                .unwrap_or_else(|_| Err("persistent WGC capture service disconnected".to_owned()))
        };
        match result {
            Ok(stats) => {
                let _ = frame_tx.send(CaptureMsg::Done(stats));
            }
            Err(message) => {
                let failure = if message.contains("device was removed")
                    || message.contains("DXGI_ERROR_DEVICE_REMOVED")
                    || message.contains("DXGI_ERROR_DEVICE_RESET")
                    || message.contains("RO_E_CLOSED")
                {
                    CaptureFailure::Reconfigure(message)
                } else {
                    CaptureFailure::Fatal(message)
                };
                let _ = frame_tx.send(CaptureMsg::Error(failure));
            }
        }
        std::thread::sleep(std::time::Duration::from_millis(100));
    })
}

#[cfg(windows)]
#[allow(clippy::too_many_arguments)]
pub(super) unsafe fn run_wgc_capture_thread(
    adapter1: windows::Win32::Graphics::Dxgi::IDXGIAdapter1,
    output_index: u32,
    encoder_device: windows::Win32::Graphics::Direct3D11::ID3D11Device,
    start: std::time::Instant,
    end_at: std::time::Instant,
    source_stop_90k: u64,
    route: VplRecordRoute,
    target_width: u32,
    target_height: u32,
    pool_size: usize,
    stop: std::sync::Arc<std::sync::atomic::AtomicBool>,
    frame_tx: std::sync::mpsc::Sender<CaptureMsg>,
    free_rx: std::sync::mpsc::Receiver<CaptureFrameSlot>,
) -> Result<CaptureStats, String> {
    use std::collections::VecDeque;
    use std::sync::atomic::Ordering;
    use std::sync::{Arc, Mutex};
    use windows::Foundation::{TimeSpan, TypedEventHandler};
    use windows::Graphics::Capture::{
        Direct3D11CaptureFrame, Direct3D11CaptureFramePool, GraphicsCaptureItem,
        GraphicsCaptureSession,
    };
    use windows::Graphics::DirectX::Direct3D11::IDirect3DDevice;
    use windows::Win32::Graphics::Direct3D11::{
        D3D11_TEXTURE2D_DESC, ID3D11Multithread, ID3D11Texture2D,
    };
    use windows::Win32::Graphics::Dxgi::Common::DXGI_SAMPLE_DESC;
    use windows::Win32::Graphics::Dxgi::IDXGIDevice;
    use windows::Win32::System::WinRT::Direct3D11::{
        CreateDirect3D11DeviceFromDXGIDevice, IDirect3DDxgiInterfaceAccess,
    };
    use windows::Win32::System::WinRT::Graphics::Capture::IGraphicsCaptureItemInterop;
    use windows::core::{IInspectable, Interface};

    struct WgcCaptureState {
        stats: CaptureStats,
        free_slots: VecDeque<LocalRouteSlot>,
        source_desc: Option<D3D11_TEXTURE2D_DESC>,
        capture_index: u64,
        timestamp_origin_100ns: Option<i64>,
        warmup_last_timestamp_100ns: Option<i64>,
        warmup_stable_intervals: u32,
        warmup_done: bool,
        encoder_warmup_pending: bool,
        post_warmup_discard_remaining: u32,
        pipeline_warmup_remaining: u32,
        pipeline_warmup_stable_intervals: u32,
        last_timestamp_100ns: Option<i64>,
        last_timestamp_90k: Option<u64>,
        record_wall_deadline: Option<std::time::Instant>,
        error: Option<String>,
    }

    impl WgcCaptureState {
        fn new() -> Self {
            Self {
                stats: CaptureStats::new(),
                free_slots: VecDeque::new(),
                source_desc: None,
                capture_index: 0,
                timestamp_origin_100ns: None,
                warmup_last_timestamp_100ns: None,
                warmup_stable_intervals: 0,
                warmup_done: false,
                encoder_warmup_pending: false,
                post_warmup_discard_remaining: 0,
                pipeline_warmup_remaining: 0,
                pipeline_warmup_stable_intervals: 0,
                last_timestamp_100ns: None,
                last_timestamp_90k: None,
                record_wall_deadline: None,
                error: None,
            }
        }
    }

    struct WgcFrameCloseGuard(Direct3D11CaptureFrame);
    impl Drop for WgcFrameCloseGuard {
        fn drop(&mut self) {
            let _ = self.0.Close();
        }
    }

    struct WgcQueuedFrame {
        frame: Direct3D11CaptureFrame,
        timestamp_100ns: i64,
        enqueued_at: std::time::Instant,
        coalesce_origin_100ns: i64,
        coalesce_started_at: std::time::Instant,
    }

    fn win_err(label: &str, err: windows::core::Error) -> String {
        format!("{label}: {err}")
    }

    const WGC_WARMUP_STABLE_INTERVALS: u32 = 0;

    let capture_duration = end_at.saturating_duration_since(start);
    if !GraphicsCaptureSession::IsSupported()
        .map_err(|err| win_err("GraphicsCaptureSession::IsSupported(WGC record)", err))?
    {
        return Err("GraphicsCaptureSession::IsSupported 返回 false".to_owned());
    }

    let device = encoder_device.clone();
    let context = device.GetImmediateContext().map_err(|err| {
        win_err(
            "oneVPL native ID3D11Device::GetImmediateContext(WGC record)",
            err,
        )
    })?;
    let wgc_multithread: Option<ID3D11Multithread> = context.cast().ok();
    if let Some(mt) = &wgc_multithread {
        let _ = mt.SetMultithreadProtected(true);
    }
    let output = adapter1
        .EnumOutputs(output_index)
        .map_err(|err| win_err("IDXGIAdapter1::EnumOutputs(WGC record)", err))?;
    let output_desc = output
        .GetDesc()
        .map_err(|err| win_err("IDXGIOutput::GetDesc(WGC record)", err))?;
    let display_refresh_hz = display_frequency_hz_from_output(&output_desc);
    let wgc_coalesce_window_100ns = display_refresh_hz
        .map(wgc_coalesce_window_100ns_for_refresh)
        .unwrap_or(0);
    let device_key = device.as_raw() as usize;
    if let Err(err) = device.GetDeviceRemovedReason() {
        WGC_WINRT_DEVICE_CACHE.with(|cache| {
            cache.borrow_mut().remove(&device_key);
        });
        return Err(format!(
            "WGC D3D11 device was removed before capture start: {err}"
        ));
    }
    let dxgi_device: IDXGIDevice = device
        .cast()
        .map_err(|err| win_err("ID3D11Device::cast<IDXGIDevice>(WGC record)", err))?;
    let winrt_device = WGC_WINRT_DEVICE_CACHE.with(|cache| {
        let mut cache = cache.borrow_mut();
        if let Some(device) = cache.get(&device_key) {
            return Ok(device.clone());
        }
        let inspectable = CreateDirect3D11DeviceFromDXGIDevice(&dxgi_device)
            .map_err(|err| win_err("CreateDirect3D11DeviceFromDXGIDevice(WGC record)", err))?;
        let device: IDirect3DDevice = inspectable
            .cast()
            .map_err(|err| win_err("IInspectable::cast<IDirect3DDevice>(WGC record)", err))?;
        if cache.len() >= 8 {
            cache.clear();
        }
        cache.insert(device_key, device.clone());
        Ok::<_, String>(device)
    })?;
    let item_interop: IGraphicsCaptureItemInterop =
        windows::core::factory::<GraphicsCaptureItem, IGraphicsCaptureItemInterop>().map_err(
            |err| {
                win_err(
                    "factory<GraphicsCaptureItem, IGraphicsCaptureItemInterop>(WGC record)",
                    err,
                )
            },
        )?;
    let item: GraphicsCaptureItem =
        item_interop
            .CreateForMonitor(output_desc.Monitor)
            .map_err(|err| {
                win_err(
                    "IGraphicsCaptureItemInterop::CreateForMonitor(WGC record)",
                    err,
                )
            })?;
    let item_size = item
        .Size()
        .map_err(|err| win_err("GraphicsCaptureItem::Size(WGC record)", err))?;
    let (pixel_format, source_dxgi_format) = route.wgc_input_format();
    // 固定 WGC 路线：按后端选择的 SDR/HDR route 捕获为 BGRA8/FP16，
    // 捕获线程 GPU shader 写目标 FourCC surface；编码线程仅 CopyResource 到 oneVPL surface。
    let wgc_frame_pool_size = 4;
    let post_warmup_discard_frames = std::env::var("RUST_REPLAY_WGC_POST_WARMUP_DISCARD_FRAMES")
        .ok()
        .and_then(|value| value.parse::<u32>().ok())
        .unwrap_or(24);
    let pipeline_warmup_frames = std::env::var("RUST_REPLAY_WGC_PIPELINE_WARMUP_FRAMES")
        .ok()
        .and_then(|value| value.parse::<u32>().ok())
        .unwrap_or(48);
    let pipeline_warmup_stable_intervals_required =
        std::env::var("RUST_REPLAY_WGC_PIPELINE_WARMUP_STABLE_INTERVALS")
            .ok()
            .and_then(|value| value.parse::<u32>().ok())
            .unwrap_or(0);
    let frame_pool = Direct3D11CaptureFramePool::CreateFreeThreaded(
        &winrt_device,
        pixel_format,
        wgc_frame_pool_size,
        item_size,
    )
    .map_err(|err| {
        win_err(
            "Direct3D11CaptureFramePool::CreateFreeThreaded(WGC record)",
            err,
        )
    })?;
    let session = frame_pool.CreateCaptureSession(&item).map_err(|err| {
        win_err(
            "Direct3D11CaptureFramePool::CreateCaptureSession(WGC record)",
            err,
        )
    })?;
    let _ = session.SetIsBorderRequired(false);
    let _ = session.SetIsCursorCaptureEnabled(true);
    let _ = session.SetMinUpdateInterval(TimeSpan { Duration: 0 });

    let mut initial_state = WgcCaptureState::new();
    initial_state.stats.wgc_coalesce_window_100ns = wgc_coalesce_window_100ns;
    let initial_source_desc = D3D11_TEXTURE2D_DESC {
        Width: item_size.Width.max(1) as u32,
        Height: item_size.Height.max(1) as u32,
        MipLevels: 1,
        ArraySize: 1,
        Format: source_dxgi_format,
        SampleDesc: DXGI_SAMPLE_DESC {
            Count: 1,
            Quality: 0,
        },
        Usage: windows::Win32::Graphics::Direct3D11::D3D11_USAGE_DEFAULT,
        BindFlags: 0,
        CPUAccessFlags: 0,
        MiscFlags: 0,
    };
    let route_dxgi_format = route.try_dxgi_format().map_err(|err| err.to_string())?;
    let initial_snapshot_desc = D3D11_TEXTURE2D_DESC {
        Width: target_width.max(item_size.Width.max(1) as u32),
        Height: target_height.max(item_size.Height.max(1) as u32),
        Format: route_dxgi_format,
        ..initial_source_desc
    };
    initial_state.source_desc = Some(initial_snapshot_desc);
    // WGC frame-pool surfaces must not be retained by cached SRVs; holding a view
    // pins the source buffer and prevents the small WinRT frame pool from recycling.
    let source_srv_cache =
        std::sync::Arc::new(std::sync::Mutex::new(ShaderResourceViewCache::transient()));
    for id in 0..pool_size {
        initial_state.free_slots.push_back(
            create_local_route_slot(
                id,
                &device,
                &context,
                &initial_snapshot_desc,
                route,
                initial_source_desc.Width,
                initial_source_desc.Height,
                // WGC P010 直接注册给 NVENC 时，compute/UAV 双平面写会在部分
                // NVIDIA 驱动上产生跨帧色度损坏；平面 RTV 写保持零复制且稳定。
                false,
                source_srv_cache.clone(),
            )
            .map_err(|err| err.to_string())?,
        );
    }
    let callback_state = Arc::new(Mutex::new(initial_state));
    let (frame_arrived_tx, frame_arrived_rx) = std::sync::mpsc::sync_channel(1);
    let frame_arrived_handler =
        TypedEventHandler::<Direct3D11CaptureFramePool, IInspectable>::new(move |_, _| {
            let _ = frame_arrived_tx.try_send(());
            Ok(())
        });
    let frame_arrived_token = frame_pool
        .FrameArrived(&frame_arrived_handler)
        .map_err(|err| win_err("Direct3D11CaptureFramePool::FrameArrived(WGC record)", err))?;
    let frame_arrived_registration = WgcFrameArrivedRegistration {
        frame_pool: frame_pool.clone(),
        token: frame_arrived_token,
    };
    session
        .StartCapture()
        .map_err(|err| win_err("GraphicsCaptureSession::StartCapture(WGC record)", err))?;

    let process_wgc_frame = |queued: WgcQueuedFrame| -> Result<(), String> {
        let queue_delay = queued.enqueued_at.elapsed();
        let frame = queued.frame;
        let _close_guard = WgcFrameCloseGuard(frame.clone());
        let callback_frame_started = std::time::Instant::now();
        let timestamp_100ns = queued.timestamp_100ns;
        let mut state = callback_state
            .lock()
            .map_err(|_| "WGC callback state mutex poisoned".to_owned())?;
        if state.error.is_some() {
            return Ok(());
        }
        state.stats.acquired += 1;
        state.stats.observe_wgc_frame_queue_delay(queue_delay);
        if timestamp_100ns <= 0
            || state
                .last_timestamp_100ns
                .is_some_and(|last| timestamp_100ns <= last)
        {
            state.stats.dropped_duplicate_timestamp += 1;
            return Ok(());
        }

        let mut drop_warmup_frame = false;
        let mut encoder_warmup_frame = false;
        if state.post_warmup_discard_remaining > 0 {
            state.post_warmup_discard_remaining =
                state.post_warmup_discard_remaining.saturating_sub(1);
            state.stats.dropped_warmup += 1;
            state.last_timestamp_100ns = Some(timestamp_100ns);
            drop_warmup_frame = true;
            if state.post_warmup_discard_remaining == 0 {
                state.warmup_done = true;
                state.pipeline_warmup_remaining = pipeline_warmup_frames;
                state.pipeline_warmup_stable_intervals = 0;
                state.last_timestamp_100ns = None;
                state.timestamp_origin_100ns = None;
                state.last_timestamp_90k = None;
                state.record_wall_deadline = None;
            }
        }
        if !drop_warmup_frame && !encoder_warmup_frame && !state.warmup_done {
            if state.warmup_last_timestamp_100ns.is_some() {
                state.warmup_stable_intervals = state.warmup_stable_intervals.saturating_add(1);
            }
            state.warmup_last_timestamp_100ns = Some(timestamp_100ns);
            state.stats.dropped_warmup += 1;
            state.last_timestamp_100ns = Some(timestamp_100ns);
            drop_warmup_frame = true;
            if !state.encoder_warmup_pending {
                encoder_warmup_frame = true;
                drop_warmup_frame = false;
                state.encoder_warmup_pending = true;
            }
        }
        if drop_warmup_frame {
            return Ok(());
        }
        let pipeline_warmup_frame = state.warmup_done
            && (state.pipeline_warmup_remaining > 0
                || state.pipeline_warmup_stable_intervals
                    < pipeline_warmup_stable_intervals_required);
        let surface = frame
            .Surface()
            .map_err(|err| win_err("Direct3D11CaptureFrame::Surface(WGC capture thread)", err))?;
        let access: IDirect3DDxgiInterfaceAccess = surface
            .cast()
            .map_err(|err| win_err("IDirect3DSurface::cast<IDirect3DDxgiInterfaceAccess>", err))?;
        let source: ID3D11Texture2D = access.GetInterface().map_err(|err| {
            win_err(
                "IDirect3DDxgiInterfaceAccess::GetInterface<ID3D11Texture2D>",
                err,
            )
        })?;
        let mut source_desc = D3D11_TEXTURE2D_DESC::default();
        source.GetDesc(&mut source_desc);
        let snapshot_desc = initial_snapshot_desc;

        if source_desc.Width != initial_source_desc.Width
            || source_desc.Height != initial_source_desc.Height
            || source_desc.Format.0 != initial_source_desc.Format.0
        {
            return Err(format!(
                "不支持的桌面模式: WGC input desc changed during callback capture: {}x{} fmt {} -> {}x{} fmt {}; route snapshot stays {}x{} fmt {}",
                initial_source_desc.Width,
                initial_source_desc.Height,
                initial_source_desc.Format.0,
                source_desc.Width,
                source_desc.Height,
                source_desc.Format.0,
                snapshot_desc.Width,
                snapshot_desc.Height,
                snapshot_desc.Format.0,
            ));
        }
        if state.source_desc.is_none() {
            state.source_desc = Some(snapshot_desc);
        }

        let Some(slot) = state.free_slots.pop_front() else {
            state.stats.dropped_no_slot += 1;
            return Ok(());
        };
        let _guard = D3d11MultithreadGuard::enter(&wgc_multithread);
        let keyed_mutex = slot.keyed_mutex.clone();
        let keyed_mutex_guard = if let Some(mutex) = keyed_mutex.as_ref() {
            Some(
                KeyedMutexGuard::acquire(
                    mutex,
                    0,
                    1,
                    1_000,
                    "IDXGIKeyedMutex::AcquireSync(WGC CUDA planar capture)",
                )
                .map_err(|err| err.to_string())?,
            )
        } else {
            None
        };
        let copy_started = std::time::Instant::now();
        let convert_result = slot.converter.convert(&source).map_err(|err| {
            format!(
                "WGC capture route conversion failed: input_tex_format={} route_tex_format={} target={}x{}; {err}",
                initial_source_desc.Format.0,
                initial_snapshot_desc.Format.0,
                initial_snapshot_desc.Width,
                initial_snapshot_desc.Height
            )
        });
        if let Some(guard) = keyed_mutex_guard {
            guard
                .release("IDXGIKeyedMutex::ReleaseSync(WGC CUDA planar capture)")
                .map_err(|err| err.to_string())?;
        } else {
            slot.capture_fence.mark(&context);
        }
        convert_result?;
        let copy_duration = copy_started.elapsed();
        state.stats.copied += 1;
        let callback_frame_duration = callback_frame_started.elapsed();
        state
            .stats
            .observe_callback_frame_cpu(callback_frame_duration, copy_duration);

        if encoder_warmup_frame || pipeline_warmup_frame {
            if pipeline_warmup_frame {
                if state.last_timestamp_100ns.is_some() {
                    state.pipeline_warmup_stable_intervals =
                        state.pipeline_warmup_stable_intervals.saturating_add(1);
                }
                state.pipeline_warmup_remaining = state.pipeline_warmup_remaining.saturating_sub(1);
                state.last_timestamp_100ns = Some(timestamp_100ns);
                if state.pipeline_warmup_remaining == 0
                    && state.pipeline_warmup_stable_intervals
                        >= pipeline_warmup_stable_intervals_required
                {
                    state.last_timestamp_100ns = None;
                    state.timestamp_origin_100ns = None;
                    state.last_timestamp_90k = None;
                    state.record_wall_deadline = None;
                }
            }
            let captured = CapturedSnapshot {
                slot: CaptureFrameSlot::Local(slot),
                source_desc: snapshot_desc,
                move_rect_bytes: 0,
                dirty_rects: Vec::new(),
                timestamp_90k: 0,
                timestamp_100ns: Some(timestamp_100ns),
                capture_index: state.capture_index,
                accumulated_frames: 1,
                warmup: true,
            };
            if frame_tx.send(CaptureMsg::Frame(captured)).is_err() {
                stop.store(true, Ordering::Relaxed);
            } else if encoder_warmup_frame {
                // Delayed encoders (NVENC Lookahead) retain the warmup texture until
                // a later output is ready. Capture-side warmup must advance when the
                // frame is handed to the encoder thread, not when that texture slot is
                // eventually recycled, otherwise no further frames can be submitted.
                state.encoder_warmup_pending = false;
                state.post_warmup_discard_remaining = post_warmup_discard_frames;
                if state.post_warmup_discard_remaining == 0 {
                    state.warmup_done = true;
                    state.pipeline_warmup_remaining = pipeline_warmup_frames;
                    state.pipeline_warmup_stable_intervals = 0;
                }
                state.last_timestamp_100ns = None;
                state.timestamp_origin_100ns = None;
                state.last_timestamp_90k = None;
                state.record_wall_deadline = None;
            }
            return Ok(());
        }

        state.last_timestamp_100ns = Some(timestamp_100ns);
        let previous_timestamp_90k = state.last_timestamp_90k;
        let timestamp_origin_100ns = state.timestamp_origin_100ns.unwrap_or(timestamp_100ns);
        let (timestamp_90k, _transport_adjustment_90k) = quantize_wgc_timestamp_90k(
            timestamp_100ns,
            timestamp_origin_100ns,
            previous_timestamp_90k,
        );
        state.timestamp_origin_100ns = Some(timestamp_origin_100ns);
        state.last_timestamp_90k = Some(timestamp_90k);
        let capture_index = state.capture_index;
        if state.record_wall_deadline.is_none() {
            state.record_wall_deadline = Some(
                std::time::Instant::now() + capture_duration + std::time::Duration::from_secs(2),
            );
        }
        if let Some(previous) = previous_timestamp_90k {
            state
                .stats
                .observe_source_interval(capture_index, timestamp_90k.saturating_sub(previous));
        }
        let captured = CapturedSnapshot {
            slot: CaptureFrameSlot::Local(slot),
            source_desc: snapshot_desc,
            move_rect_bytes: 0,
            dirty_rects: Vec::new(),
            timestamp_90k,
            timestamp_100ns: Some(timestamp_100ns),
            capture_index,
            accumulated_frames: 1,
            warmup: false,
        };
        state.capture_index = state.capture_index.saturating_add(1);
        if frame_tx.send(CaptureMsg::Frame(captured)).is_err() {
            stop.store(true, Ordering::Relaxed);
        }
        if timestamp_90k >= source_stop_90k {
            stop.store(true, Ordering::Relaxed);
        }
        Ok(())
    };
    let return_slot = |slot: CaptureFrameSlot| -> Result<(), String> {
        let CaptureFrameSlot::Local(slot) = slot else {
            return Ok(());
        };
        let mut state = callback_state
            .lock()
            .map_err(|_| "WGC callback state mutex poisoned while returning slot".to_owned())?;
        if state.encoder_warmup_pending {
            state.encoder_warmup_pending = false;
            state.post_warmup_discard_remaining = post_warmup_discard_frames;
            if state.post_warmup_discard_remaining == 0 {
                state.warmup_done = true;
                state.pipeline_warmup_remaining = pipeline_warmup_frames;
                state.pipeline_warmup_stable_intervals = 0;
            }
            state.last_timestamp_100ns = None;
            state.timestamp_origin_100ns = None;
            state.last_timestamp_90k = None;
            state.record_wall_deadline = None;
        }
        if state
            .source_desc
            .as_ref()
            .is_none_or(|desc| local_route_slot_matches(&slot, desc))
        {
            state.free_slots.push_back(slot);
        }
        Ok(())
    };

    let startup_deadline =
        std::time::Instant::now() + capture_duration + std::time::Duration::from_secs(10);
    let wgc_coalesce_window = std::time::Duration::from_nanos(
        u64::try_from(wgc_coalesce_window_100ns.max(0))
            .unwrap_or(u64::MAX)
            .saturating_mul(100),
    );
    let mut pending_wgc_frame: Option<WgcQueuedFrame> = None;
    let mut wgc_coalesced_frames = 0u64;
    let mut enqueue_wgc_frame = |pending: &mut Option<WgcQueuedFrame>,
                                 mut frame: WgcQueuedFrame| {
        let Some(current) = pending.take() else {
            *pending = Some(frame);
            return None;
        };
        if should_coalesce_wgc_timestamps(
            current.coalesce_origin_100ns,
            frame.timestamp_100ns,
            wgc_coalesce_window_100ns,
        ) {
            frame.coalesce_origin_100ns = current.coalesce_origin_100ns;
            frame.coalesce_started_at = current.coalesce_started_at;
            let _ = current.frame.Close();
            wgc_coalesced_frames = wgc_coalesced_frames.saturating_add(1);
            *pending = Some(frame);
            None
        } else {
            *pending = Some(frame);
            Some(current)
        }
    };
    let poll_next_wgc_frame = || -> Result<Option<WgcQueuedFrame>, String> {
        let frame = match frame_pool.TryGetNextFrame() {
            Ok(frame) => frame,
            Err(err) if err.code().0 == 0 => return Ok(None),
            Err(err) => return Err(win_err("TryGetNextFrame(WGC polling)", err)),
        };
        let timestamp_100ns = frame
            .SystemRelativeTime()
            .map_err(|err| {
                win_err(
                    "Direct3D11CaptureFrame::SystemRelativeTime(WGC polling)",
                    err,
                )
            })?
            .Duration;
        let enqueued_at = std::time::Instant::now();
        Ok(Some(WgcQueuedFrame {
            frame,
            timestamp_100ns,
            enqueued_at,
            coalesce_origin_100ns: timestamp_100ns,
            coalesce_started_at: enqueued_at,
        }))
    };
    while !stop.load(Ordering::Relaxed) {
        while let Ok(slot) = free_rx.try_recv() {
            return_slot(slot)?;
        }
        let mut polled_frame = false;
        loop {
            let Some(frame) = poll_next_wgc_frame()? else {
                break;
            };
            polled_frame = true;
            while let Ok(slot) = free_rx.try_recv() {
                return_slot(slot)?;
            }
            if let Some(frame) = enqueue_wgc_frame(&mut pending_wgc_frame, frame) {
                let frame_result = process_wgc_frame(frame);
                if let Err(message) = frame_result {
                    if let Ok(mut state) = callback_state.lock() {
                        state.error = Some(message);
                    }
                    stop.store(true, Ordering::Relaxed);
                    break;
                }
            }
            if stop.load(Ordering::Relaxed) {
                break;
            }
        }
        if pending_wgc_frame
            .as_ref()
            .is_some_and(|frame| frame.coalesce_started_at.elapsed() >= wgc_coalesce_window)
        {
            let frame = pending_wgc_frame.take().expect("pending WGC frame");
            if let Err(message) = process_wgc_frame(frame) {
                if let Ok(mut state) = callback_state.lock() {
                    state.error = Some(message);
                }
                stop.store(true, Ordering::Relaxed);
                break;
            }
        }
        let now = std::time::Instant::now();
        let record_deadline = {
            let state = callback_state.lock().map_err(|_| {
                "WGC callback state mutex poisoned while checking deadline".to_owned()
            })?;
            state.record_wall_deadline.unwrap_or(startup_deadline)
        };
        if now >= record_deadline {
            break;
        }
        if !polled_frame {
            let coalesce_remaining = pending_wgc_frame.as_ref().map(|frame| {
                wgc_coalesce_window.saturating_sub(frame.coalesce_started_at.elapsed())
            });
            let wait_timeout = wgc_event_wait_timeout(
                coalesce_remaining,
                record_deadline.saturating_duration_since(std::time::Instant::now()),
            );
            if !wait_timeout.is_zero() {
                match frame_arrived_rx.recv_timeout(wait_timeout) {
                    Ok(()) | Err(std::sync::mpsc::RecvTimeoutError::Timeout) => {}
                    Err(std::sync::mpsc::RecvTimeoutError::Disconnected) => {
                        if let Ok(mut state) = callback_state.lock() {
                            state.error =
                                Some("WGC FrameArrived event channel disconnected".to_owned());
                        }
                        stop.store(true, Ordering::Relaxed);
                        break;
                    }
                }
            }
        }
        while let Ok(slot) = free_rx.try_recv() {
            return_slot(slot)?;
        }
        if callback_state
            .lock()
            .map_err(|_| "WGC callback state mutex poisoned while checking error".to_owned())?
            .error
            .is_some()
        {
            stop.store(true, Ordering::Relaxed);
            break;
        }
    }
    if let Some(frame) = pending_wgc_frame.take() {
        let _ = frame.frame.Close();
    }

    // Lookahead 合法持有输入槽位直到 recorder 随后的 EOS flush。这里等待全部槽位
    // 会与“先停止捕获线程、再 flush 编码器”的关闭顺序形成固定超时等待。
    let _ = session.Close();
    drop(frame_arrived_registration);
    drop(frame_arrived_handler);
    let _ = frame_pool.Close();

    let (error, mut stats) = {
        let state = callback_state
            .lock()
            .map_err(|_| "WGC callback state mutex poisoned while finalizing".to_owned())?;
        (state.error.clone(), state.stats.clone())
    };
    stats.wgc_input_queue_max = 0;
    stats.wgc_coalesced_frames = wgc_coalesced_frames;
    drop(session);
    drop(frame_pool);
    drop(item);
    drop(item_interop);
    drop(winrt_device);
    if let Some(error) = error {
        return Err(error);
    }
    stats.dda_timeouts = 0;
    Ok(stats)
}
