//! Standalone experiment for the legacy Windows NvFBC ABI that is still
//! exported by current NVIDIA display drivers.
//!
//! The default mode only performs read-only status queries and a create probe.
//! Undocumented Sunshine-compatible private data and frame capture require
//! explicit command-line flags. The binary never calls `NvFBC_Enable` or
//! modifies driver files, services, or registry state.

#![cfg_attr(not(windows), allow(dead_code, unused_imports))]

#[cfg(windows)]
use std::{
    ffi::{OsString, c_void},
    mem::size_of,
    path::PathBuf,
    ptr,
    time::{Duration, Instant},
};

#[cfg(windows)]
use libloading::{Library, Symbol};

#[cfg(windows)]
use windows::{
    Win32::{
        Foundation::FILETIME,
        Graphics::Direct3D9::{
            D3D_SDK_VERSION, D3DADAPTER_IDENTIFIER9, D3DCREATE_FPU_PRESERVE,
            D3DCREATE_HARDWARE_VERTEXPROCESSING, D3DCREATE_MULTITHREADED, D3DDEVTYPE_HAL,
            D3DFMT_A2B10G10R10, D3DFMT_X8R8G8B8, D3DLOCK_READONLY, D3DLOCKED_RECT,
            D3DMULTISAMPLE_NONE, D3DPOOL_SYSTEMMEM, D3DPRESENT_INTERVAL_IMMEDIATE,
            D3DPRESENT_PARAMETERS, D3DPRESENTFLAG_VIDEO, D3DSWAPEFFECT_COPY, Direct3DCreate9Ex,
            IDirect3D9Ex, IDirect3DDevice9Ex, IDirect3DSurface9,
        },
        Graphics::Dxgi::{CreateDXGIFactory1, DXGI_ERROR_NOT_FOUND, IDXGIFactory1, IDXGIOutput},
        System::Threading::{GetCurrentProcess, GetProcessTimes},
        UI::WindowsAndMessaging::GetDesktopWindow,
    },
    core::{BOOL, Interface},
};

#[cfg(windows)]
type NvFbcResult = i32;

#[cfg(windows)]
type NvFbcGetSdkVersion = unsafe extern "system" fn(*mut u32) -> NvFbcResult;

#[cfg(windows)]
type NvFbcGetStatusEx = unsafe extern "system" fn(*mut c_void) -> NvFbcResult;

#[cfg(windows)]
type NvFbcCreateEx = unsafe extern "system" fn(*mut c_void) -> NvFbcResult;

#[cfg(windows)]
type NvFbcRelease = unsafe extern "system" fn(*mut c_void) -> NvFbcResult;

#[cfg(windows)]
type NvFbcSetupV3 =
    unsafe extern "system" fn(*mut c_void, *mut NvFbcToDx9VidSetupV3) -> NvFbcResult;

#[cfg(windows)]
type NvFbcGrabV1 = unsafe extern "system" fn(*mut c_void, *mut NvFbcToDx9VidGrabV1) -> NvFbcResult;

#[cfg(windows)]
type NvFbcGpuSleep = unsafe extern "system" fn(*mut c_void, i64) -> NvFbcResult;

#[cfg(windows)]
const NVFBC_SUCCESS: NvFbcResult = 0;

#[cfg(windows)]
const STATUS_FLAG_CAPTURE_POSSIBLE: u32 = 1 << 0;
#[cfg(windows)]
const STATUS_FLAG_CURRENTLY_CAPTURING: u32 = 1 << 1;
#[cfg(windows)]
const STATUS_FLAG_CAN_CREATE_NOW: u32 = 1 << 2;
#[cfg(windows)]
const STATUS_FLAG_MULTI_HEAD: u32 = 1 << 3;
#[cfg(windows)]
const STATUS_FLAG_MULTI_CLIENT: u32 = 1 << 4;

#[cfg(windows)]
const NVFBC_TO_DX9_VID_V2: u32 = 0x2002;
#[cfg(windows)]
const NVFBC_TO_DX9_VID_V3: u32 = 0x2003;

#[cfg(windows)]
const NVFBC_TODX9VID_ARGB10: u32 = 2;

#[cfg(windows)]
const NVFBC_TODX9VID_WAIT_WITH_TIMEOUT: u32 = 0x10;

#[cfg(windows)]
const NVFBC_TODX9VID_NOWAIT: u32 = 0x1;

#[cfg(windows)]
const NVFBC_TODX9VID_SOURCEMODE_FULL: u32 = 0;

#[cfg(windows)]
const NVFBC_SETUP_HDR_REQUEST: u32 = 1 << 4;

#[cfg(windows)]
const NVIDIA_VENDOR_ID: u32 = 0x10de;

#[cfg(windows)]
const SUNSHINE_PRIVATE_DATA: [u32; 4] = [0xaef5_7ac5, 0x401d_1a39, 0x1b85_6bbe, 0x9ed0_ceba];

#[cfg(windows)]
#[derive(Clone, Copy)]
enum GrabStrategy {
    Timeout,
    Event,
    GpuSleep,
    VBlank,
}

#[cfg(windows)]
impl GrabStrategy {
    fn flags(self) -> u32 {
        match self {
            Self::Timeout => NVFBC_TODX9VID_WAIT_WITH_TIMEOUT,
            Self::Event => 0,
            Self::GpuSleep | Self::VBlank => NVFBC_TODX9VID_NOWAIT,
        }
    }

    fn wait_time_ms(self) -> u32 {
        match self {
            Self::Timeout => 100,
            Self::Event | Self::GpuSleep | Self::VBlank => 0,
        }
    }

    fn label(self) -> &'static str {
        match self {
            Self::Timeout => "timeout",
            Self::Event => "event",
            Self::GpuSleep => "gpu-sleep+nowait",
            Self::VBlank => "dxgi-vblank+nowait",
        }
    }
}

#[cfg(windows)]
struct CaptureRunConfig<'a> {
    device: &'a IDirect3DDevice9Ex,
    dxgi_output: Option<&'a IDXGIOutput>,
    width: u32,
    height: u32,
    version: u32,
    duration: Duration,
    strategy: GrabStrategy,
}

/// Public Windows Capture SDK 5.x/7.x `NvFBCStatusEx` layout.
///
/// The driver validates `dwVersion` using this structure's byte size, the
/// structure revision, and the NvFBC DLL ABI byte. Keeping the full reserved
/// tail is therefore required even though the probe only reads the prefix.
#[cfg(windows)]
#[repr(C)]
struct NvFbcStatusEx {
    dw_version: u32,
    flags: u32,
    dw_nvfbc_version: u32,
    dw_adapter_idx: u32,
    private_data: *mut c_void,
    private_data_size: u32,
    reserved: [u32; 59],
    reserved_ptrs: [*mut c_void; 31],
}

#[cfg(windows)]
impl NvFbcStatusEx {
    fn new(adapter: u32, version: u32) -> Self {
        Self {
            dw_version: struct_version::<Self>(version, 2),
            flags: 0,
            dw_nvfbc_version: 0,
            dw_adapter_idx: adapter,
            private_data: ptr::null_mut(),
            private_data_size: 0,
            reserved: [0; 59],
            reserved_ptrs: [ptr::null_mut(); 31],
        }
    }
}

/// Public Windows Capture SDK 7.x `NvFBCCreateParams` layout.
#[cfg(windows)]
#[repr(C)]
struct NvFbcCreateParams {
    dw_version: u32,
    dw_interface_type: u32,
    dw_max_display_width: u32,
    dw_max_display_height: u32,
    device: *mut c_void,
    private_data: *mut c_void,
    private_data_size: u32,
    dw_interface_version: u32,
    nvfbc: *mut c_void,
    dw_adapter_idx: u32,
    dw_nvfbc_version: u32,
    cuda_context: *mut c_void,
    private_data2: *mut c_void,
    private_data2_size: u32,
    reserved: [u32; 55],
    reserved_ptrs: [*mut c_void; 27],
}

#[cfg(windows)]
impl NvFbcCreateParams {
    fn new(
        adapter: u32,
        version: u32,
        interface_type: u32,
        device: &IDirect3DDevice9Ex,
        private_data: Option<&mut [u32; 4]>,
    ) -> Self {
        let (private_data, private_data_size) = private_data
            .map(|data| (data.as_mut_ptr().cast(), size_of_val(data) as u32))
            .unwrap_or((ptr::null_mut(), 0));
        Self {
            dw_version: struct_version::<Self>(version, 2),
            dw_interface_type: interface_type,
            dw_max_display_width: 0,
            dw_max_display_height: 0,
            device: device.as_raw(),
            private_data,
            private_data_size,
            dw_interface_version: 0,
            nvfbc: ptr::null_mut(),
            dw_adapter_idx: adapter,
            dw_nvfbc_version: 0,
            cuda_context: ptr::null_mut(),
            private_data2: ptr::null_mut(),
            private_data2_size: 0,
            reserved: [0; 55],
            reserved_ptrs: [ptr::null_mut(); 27],
        }
    }
}

#[cfg(windows)]
#[repr(C)]
#[derive(Clone, Copy)]
struct NvFbcToDx9VidOutBuf {
    primary: *mut c_void,
    secondary: *mut c_void,
}

#[cfg(windows)]
#[repr(C)]
struct NvFbcToDx9VidSetupV3 {
    dw_version: u32,
    flags: u32,
    mode: u32,
    buffer_count: u32,
    diff_map_block_size: u32,
    stereo_format: u32,
    diff_map_buffer_size: u32,
    classification_map_buffer_size: u32,
    classification_stamp_width: u32,
    classification_stamp_height: u32,
    diff_maps: *mut *mut c_void,
    classification_maps: *mut *mut c_void,
    buffers: *mut NvFbcToDx9VidOutBuf,
    cursor_capture_event: *mut c_void,
    reserved: [u32; 22],
    reserved_ptrs: [*mut c_void; 12],
}

#[cfg(windows)]
impl NvFbcToDx9VidSetupV3 {
    fn hdr_argb10(buffers: &mut [NvFbcToDx9VidOutBuf], version: u32) -> Self {
        Self {
            dw_version: struct_version::<Self>(version, 3),
            flags: NVFBC_SETUP_HDR_REQUEST,
            mode: NVFBC_TODX9VID_ARGB10,
            buffer_count: buffers.len() as u32,
            diff_map_block_size: 0,
            stereo_format: 0,
            diff_map_buffer_size: 0,
            classification_map_buffer_size: 0,
            classification_stamp_width: 0,
            classification_stamp_height: 0,
            diff_maps: ptr::null_mut(),
            classification_maps: ptr::null_mut(),
            buffers: buffers.as_mut_ptr(),
            cursor_capture_event: ptr::null_mut(),
            reserved: [0; 22],
            reserved_ptrs: [ptr::null_mut(); 12],
        }
    }
}

#[cfg(windows)]
#[repr(C)]
#[derive(Default)]
struct NvFbcFrameGrabInfo {
    width: u32,
    height: u32,
    buffer_width: u32,
    reserved0: u32,
    overlay_active: u32,
    must_recreate: u32,
    first_buffer: u32,
    hardware_mouse_visible: u32,
    protected_content: u32,
    driver_internal_error: u32,
    stereo_on: u32,
    igpu_capture: u32,
    source_pid: u32,
    reserved3: u32,
    hdr_flags: u32,
    wait_mode_used: u32,
    reserved2: [u32; 11],
}

#[cfg(windows)]
#[repr(C)]
struct NvFbcToDx9VidGrabV1 {
    dw_version: u32,
    flags: u32,
    target_width: u32,
    target_height: u32,
    start_x: u32,
    start_y: u32,
    grab_mode: u32,
    buffer_index: u32,
    frame_info: *mut NvFbcFrameGrabInfo,
    wait_time_ms: u32,
    reserved: [u32; 23],
    reserved_ptrs: [*mut c_void; 15],
}

#[cfg(windows)]
impl NvFbcToDx9VidGrabV1 {
    fn blocking(
        buffer_index: u32,
        frame_info: &mut NvFbcFrameGrabInfo,
        version: u32,
        strategy: GrabStrategy,
    ) -> Self {
        Self {
            dw_version: struct_version::<Self>(version, 1),
            flags: strategy.flags(),
            target_width: 0,
            target_height: 0,
            start_x: 0,
            start_y: 0,
            grab_mode: NVFBC_TODX9VID_SOURCEMODE_FULL,
            buffer_index,
            frame_info,
            wait_time_ms: strategy.wait_time_ms(),
            reserved: [0; 23],
            reserved_ptrs: [ptr::null_mut(); 15],
        }
    }
}

#[cfg(windows)]
fn struct_version<T>(dll_version: u32, revision: u32) -> u32 {
    (size_of::<T>() as u32) | (revision << 16) | ((dll_version & 0xff) << 24)
}

#[cfg(windows)]
fn result_name(result: NvFbcResult) -> &'static str {
    match result {
        0 => "NVFBC_SUCCESS",
        -1 => "NVFBC_ERROR_GENERIC",
        -2 => "NVFBC_ERROR_INVALID_PARAM",
        -3 => "NVFBC_ERROR_INVALIDATED_SESSION",
        -4 => "NVFBC_ERROR_PROTECTED_CONTENT",
        -5 => "NVFBC_ERROR_DRIVER_FAILURE",
        -6 => "NVFBC_ERROR_CUDA_FAILURE",
        -7 => "NVFBC_ERROR_UNSUPPORTED",
        -8 => "NVFBC_ERROR_HW_ENC_FAILURE",
        -9 => "NVFBC_ERROR_INCOMPATIBLE_DRIVER",
        -10 => "NVFBC_ERROR_UNSUPPORTED_PLATFORM",
        -11 => "NVFBC_ERROR_OUT_OF_MEMORY",
        -12 => "NVFBC_ERROR_INVALID_PTR",
        -13 => "NVFBC_ERROR_INCOMPATIBLE_VERSION",
        -14 => "NVFBC_ERROR_OPT_CAPTURE_FAILURE",
        -15 => "NVFBC_ERROR_INSUFFICIENT_PRIVILEGES",
        -16 => "NVFBC_ERROR_INVALID_CALL",
        -17 => "NVFBC_ERROR_SYSTEM_ERROR",
        -18 => "NVFBC_ERROR_INVALID_TARGET",
        -19 => "NVFBC_ERROR_NVAPI_FAILURE",
        -20 => "NVFBC_ERROR_DYNAMIC_DISABLE",
        -21 => "NVFBC_ERROR_IPC_FAILURE",
        -22 => "NVFBC_ERROR_CURSOR_CAPTURE_FAILURE",
        _ => "NVFBC_ERROR_UNKNOWN",
    }
}

#[cfg(windows)]
fn system_nvfbc_path() -> PathBuf {
    let system_root =
        std::env::var_os("SystemRoot").unwrap_or_else(|| OsString::from(r"C:\Windows"));
    PathBuf::from(system_root)
        .join("System32")
        .join("NvFBC64.dll")
}

#[cfg(windows)]
fn c_string(bytes: &[i8]) -> String {
    let end = bytes
        .iter()
        .position(|byte| *byte == 0)
        .unwrap_or(bytes.len());
    let bytes = bytes[..end]
        .iter()
        .map(|byte| *byte as u8)
        .collect::<Vec<_>>();
    String::from_utf8_lossy(&bytes).into_owned()
}

#[cfg(windows)]
fn create_d3d9_device(
    d3d: &IDirect3D9Ex,
    adapter: u32,
) -> windows::core::Result<IDirect3DDevice9Ex> {
    let desktop = unsafe { GetDesktopWindow() };
    let mut present = D3DPRESENT_PARAMETERS {
        BackBufferWidth: 1,
        BackBufferHeight: 1,
        BackBufferFormat: D3DFMT_X8R8G8B8,
        BackBufferCount: 1,
        SwapEffect: D3DSWAPEFFECT_COPY,
        hDeviceWindow: desktop,
        Windowed: BOOL(1),
        Flags: D3DPRESENTFLAG_VIDEO,
        PresentationInterval: D3DPRESENT_INTERVAL_IMMEDIATE as u32,
        ..Default::default()
    };
    let behavior = (D3DCREATE_FPU_PRESERVE
        | D3DCREATE_MULTITHREADED
        | D3DCREATE_HARDWARE_VERTEXPROCESSING) as u32;
    let mut device = None;
    unsafe {
        d3d.CreateDeviceEx(
            adapter,
            D3DDEVTYPE_HAL,
            desktop,
            behavior,
            &mut present,
            ptr::null_mut(),
            &mut device,
        )?;
    }
    device.ok_or_else(windows::core::Error::from_thread)
}

#[cfg(windows)]
fn matching_dxgi_output(d3d: &IDirect3D9Ex, adapter: u32) -> windows::core::Result<IDXGIOutput> {
    let target_monitor = unsafe { d3d.GetAdapterMonitor(adapter) };
    let factory: IDXGIFactory1 = unsafe { CreateDXGIFactory1()? };
    let mut adapter_index = 0_u32;
    loop {
        let dxgi_adapter = match unsafe { factory.EnumAdapters1(adapter_index) } {
            Ok(adapter) => adapter,
            Err(err) if err.code() == DXGI_ERROR_NOT_FOUND => break,
            Err(err) => return Err(err),
        };
        let mut output_index = 0_u32;
        loop {
            let output = match unsafe { dxgi_adapter.EnumOutputs(output_index) } {
                Ok(output) => output,
                Err(err) if err.code() == DXGI_ERROR_NOT_FOUND => break,
                Err(err) => return Err(err),
            };
            let desc = unsafe { output.GetDesc()? };
            if desc.Monitor == target_monitor {
                return Ok(output);
            }
            output_index += 1;
        }
        adapter_index += 1;
    }
    Err(windows::core::Error::new(
        DXGI_ERROR_NOT_FOUND,
        "No DXGI output matches the D3D9 NvFBC adapter",
    ))
}

#[cfg(windows)]
unsafe fn release_nvfbc_interface(interface: *mut c_void) -> NvFbcResult {
    // `INvFBCToDx9Vid_v2` has no IUnknown prefix. Its fourth vtable entry is Release.
    let vtable = unsafe { *(interface.cast::<*mut *mut c_void>()) };
    let release: NvFbcRelease = unsafe { std::mem::transmute(*vtable.add(3)) };
    unsafe { release(interface) }
}

#[cfg(windows)]
fn create_capture_surfaces(
    device: &IDirect3DDevice9Ex,
    width: u32,
    height: u32,
    count: usize,
) -> windows::core::Result<Vec<IDirect3DSurface9>> {
    let mut surfaces = Vec::with_capacity(count);
    for _ in 0..count {
        let mut surface = None;
        unsafe {
            device.CreateRenderTarget(
                width,
                height,
                D3DFMT_A2B10G10R10,
                D3DMULTISAMPLE_NONE,
                0,
                false,
                &mut surface,
                ptr::null_mut(),
            )?;
        }
        surfaces.push(surface.ok_or_else(windows::core::Error::from_thread)?);
    }
    Ok(surfaces)
}

#[cfg(windows)]
fn readback_hash(
    device: &IDirect3DDevice9Ex,
    surface: &IDirect3DSurface9,
    width: u32,
    height: u32,
) -> windows::core::Result<(u64, u64, u32, u32)> {
    let mut staging = None;
    unsafe {
        device.CreateOffscreenPlainSurface(
            width,
            height,
            D3DFMT_A2B10G10R10,
            D3DPOOL_SYSTEMMEM,
            &mut staging,
            ptr::null_mut(),
        )?;
    }
    let staging = staging.ok_or_else(windows::core::Error::from_thread)?;
    unsafe { device.GetRenderTargetData(surface, &staging)? };

    let mut locked = D3DLOCKED_RECT::default();
    unsafe {
        staging.LockRect(&mut locked, ptr::null(), D3DLOCK_READONLY as u32)?;
    }

    let result = (|| {
        let pitch = locked.Pitch.max(0) as usize;
        let row_bytes = width as usize * size_of::<u32>();
        if locked.pBits.is_null() || pitch < row_bytes {
            return (0, 0, u32::MAX, 0);
        }

        let mut hash = 0xcbf2_9ce4_8422_2325_u64;
        let mut nonzero_words = 0_u64;
        let mut minimum = u32::MAX;
        let mut maximum = 0_u32;
        for y in 0..height as usize {
            let row = unsafe { (locked.pBits as *const u8).add(y * pitch) };
            let words = unsafe { std::slice::from_raw_parts(row.cast::<u32>(), width as usize) };
            for &word in words {
                hash ^= u64::from(word);
                hash = hash.wrapping_mul(0x0000_0100_0000_01b3);
                nonzero_words += u64::from(word != 0);
                minimum = minimum.min(word);
                maximum = maximum.max(word);
            }
        }
        (hash, nonzero_words, minimum, maximum)
    })();

    unsafe { staging.UnlockRect()? };
    Ok(result)
}

#[cfg(windows)]
fn percentile_us(values: &[u64], percentile: f64) -> u64 {
    if values.is_empty() {
        return 0;
    }
    let mut sorted = values.to_vec();
    sorted.sort_unstable();
    let index = ((sorted.len() - 1) as f64 * percentile).round() as usize;
    sorted[index]
}

#[cfg(windows)]
fn filetime_ticks(value: FILETIME) -> u64 {
    (u64::from(value.dwHighDateTime) << 32) | u64::from(value.dwLowDateTime)
}

#[cfg(windows)]
fn process_cpu_time_100ns() -> windows::core::Result<u64> {
    let mut creation = FILETIME::default();
    let mut exit = FILETIME::default();
    let mut kernel = FILETIME::default();
    let mut user = FILETIME::default();
    unsafe {
        GetProcessTimes(
            GetCurrentProcess(),
            &mut creation,
            &mut exit,
            &mut kernel,
            &mut user,
        )?;
    }
    Ok(filetime_ticks(kernel) + filetime_ticks(user))
}

#[cfg(windows)]
unsafe fn run_hdr_capture(
    interface: *mut c_void,
    config: CaptureRunConfig<'_>,
) -> anyhow::Result<()> {
    let CaptureRunConfig {
        device,
        dxgi_output,
        width,
        height,
        version,
        duration,
        strategy,
    } = config;
    let surfaces = create_capture_surfaces(device, width, height, 3)?;
    let mut output_buffers = surfaces
        .iter()
        .map(|surface| NvFbcToDx9VidOutBuf {
            primary: surface.as_raw(),
            secondary: ptr::null_mut(),
        })
        .collect::<Vec<_>>();

    let mut setup = NvFbcToDx9VidSetupV3::hdr_argb10(&mut output_buffers, version);
    let vtable = unsafe { *(interface.cast::<*mut *mut c_void>()) };
    let setup_method: NvFbcSetupV3 = unsafe { std::mem::transmute(*vtable.add(0)) };
    let grab_method: NvFbcGrabV1 = unsafe { std::mem::transmute(*vtable.add(1)) };
    let gpu_sleep_method: NvFbcGpuSleep = unsafe { std::mem::transmute(*vtable.add(2)) };

    println!(
        concat!(
            "capture_setup width={width} height={height} buffers={buffers} format=ARGB10 hdr_request=true ",
            "setup_struct_size={setup_size} setup_version=0x{setup_version:08x} ",
            "grab_struct_size={grab_size} grab_wait={grab_wait}"
        ),
        width = width,
        height = height,
        buffers = output_buffers.len(),
        setup_size = size_of::<NvFbcToDx9VidSetupV3>(),
        setup_version = setup.dw_version,
        grab_size = size_of::<NvFbcToDx9VidGrabV1>(),
        grab_wait = strategy.label(),
    );
    let setup_result = unsafe { setup_method(interface, &mut setup) };
    println!(
        "capture_setup_result={setup_result}({})",
        result_name(setup_result)
    );
    if setup_result != NVFBC_SUCCESS {
        anyhow::bail!("NvFBCToDx9VidSetUp failed with {setup_result}");
    }

    let capture_cpu_start = process_cpu_time_100ns()?;
    let started = Instant::now();
    let mut call_us = Vec::new();
    let mut return_interval_us = Vec::new();
    let mut last_return = None;
    let mut success_count = 0_u64;
    let mut last_buffer_index = 0_usize;
    let mut hdr_count = 0_u64;
    let mut first_info = None;
    let mut last_info = None;

    while started.elapsed() < duration {
        if matches!(strategy, GrabStrategy::VBlank) {
            let output = dxgi_output.ok_or_else(|| anyhow::anyhow!("missing DXGI output"))?;
            unsafe { output.WaitForVBlank()? };
        }
        if matches!(strategy, GrabStrategy::GpuSleep) && success_count > 0 {
            let sleep_result = unsafe { gpu_sleep_method(interface, 4_166) };
            if sleep_result != NVFBC_SUCCESS {
                println!(
                    "capture_gpu_sleep_error result={sleep_result}({})",
                    result_name(sleep_result)
                );
                break;
            }
        }
        let buffer_index = success_count as usize % surfaces.len();
        let mut info = NvFbcFrameGrabInfo::default();
        let mut grab =
            NvFbcToDx9VidGrabV1::blocking(buffer_index as u32, &mut info, version, strategy);
        let before = Instant::now();
        let result = unsafe { grab_method(interface, &mut grab) };
        let returned = Instant::now();
        call_us.push(returned.duration_since(before).as_micros() as u64);
        if let Some(previous) = last_return.replace(returned) {
            return_interval_us.push(returned.duration_since(previous).as_micros() as u64);
        }

        if result != NVFBC_SUCCESS {
            println!(
                concat!(
                    "capture_grab_error frame={success_count} result={result}({result_name}) ",
                    "driver_internal=0x{driver_internal:08x} must_recreate={must_recreate} protected={protected}"
                ),
                success_count = success_count,
                result = result,
                must_recreate = info.must_recreate,
                protected = info.protected_content,
                result_name = result_name(result),
                driver_internal = info.driver_internal_error,
            );
            break;
        }

        success_count += 1;
        last_buffer_index = buffer_index;
        hdr_count += u64::from(info.hdr_flags & 1 != 0);
        if first_info.is_none() {
            first_info = Some((
                info.width,
                info.height,
                info.buffer_width,
                info.hdr_flags,
                info.wait_mode_used,
                info.source_pid,
            ));
        }
        last_info = Some((
            info.width,
            info.height,
            info.buffer_width,
            info.hdr_flags,
            info.wait_mode_used,
            info.source_pid,
        ));
        if success_count <= 5 || success_count.is_multiple_of(300) {
            println!(
                concat!(
                    "capture_frame={success_count} buffer={buffer_index} call_us={call_us} ",
                    "width={width} height={height} buffer_width={buffer_width} hdr={hdr} ",
                    "wait_mode={wait_mode} source_pid={source_pid}"
                ),
                success_count = success_count,
                buffer_index = buffer_index,
                call_us = call_us.last().copied().unwrap_or_default(),
                width = info.width,
                height = info.height,
                buffer_width = info.buffer_width,
                hdr = info.hdr_flags & 1 != 0,
                wait_mode = info.wait_mode_used,
                source_pid = info.source_pid,
            );
        }
    }

    let elapsed = started.elapsed();
    let capture_cpu_end = process_cpu_time_100ns()?;
    let capture_cpu_ms = (capture_cpu_end.saturating_sub(capture_cpu_start)) as f64 / 10_000.0;
    let capture_one_core_percent = if elapsed.is_zero() {
        0.0
    } else {
        capture_cpu_ms / elapsed.as_secs_f64() / 10.0
    };
    let average_fps = if elapsed.is_zero() {
        0.0
    } else {
        success_count as f64 / elapsed.as_secs_f64()
    };
    println!(
        concat!(
            "capture_summary elapsed_ms={elapsed_ms:.3} frames={success_count} fps={average_fps:.3} ",
            "hdr_frames={hdr_count} call_p50_us={call_p50} call_p95_us={call_p95} ",
            "call_p99_us={call_p99} call_max_us={call_max} ",
            "return_interval_p50_us={interval_p50} return_interval_p95_us={interval_p95} ",
            "return_interval_p99_us={interval_p99} return_interval_max_us={interval_max} ",
            "capture_cpu_ms={capture_cpu_ms:.3} capture_one_core_percent={capture_one_core_percent:.3} ",
            "first_info={first_info:?} last_info={last_info:?}"
        ),
        elapsed_ms = elapsed.as_secs_f64() * 1000.0,
        success_count = success_count,
        average_fps = average_fps,
        hdr_count = hdr_count,
        call_p50 = percentile_us(&call_us, 0.50),
        call_p95 = percentile_us(&call_us, 0.95),
        call_p99 = percentile_us(&call_us, 0.99),
        call_max = call_us.iter().copied().max().unwrap_or_default(),
        interval_p50 = percentile_us(&return_interval_us, 0.50),
        interval_p95 = percentile_us(&return_interval_us, 0.95),
        interval_p99 = percentile_us(&return_interval_us, 0.99),
        interval_max = return_interval_us.iter().copied().max().unwrap_or_default(),
        capture_cpu_ms = capture_cpu_ms,
        capture_one_core_percent = capture_one_core_percent,
        first_info = first_info,
        last_info = last_info,
    );

    if success_count > 0 {
        let readback_started = Instant::now();
        let (hash, nonzero_words, minimum, maximum) =
            readback_hash(device, &surfaces[last_buffer_index], width, height)?;
        println!(
            concat!(
                "capture_readback buffer={last_buffer_index} hash=0x{hash:016x} ",
                "nonzero_words={nonzero_words} min=0x{minimum:08x} max=0x{maximum:08x} ",
                "readback_ms={readback_ms:.3}"
            ),
            last_buffer_index = last_buffer_index,
            hash = hash,
            nonzero_words = nonzero_words,
            minimum = minimum,
            maximum = maximum,
            readback_ms = readback_started.elapsed().as_secs_f64() * 1000.0,
        );
    }

    Ok(())
}

#[cfg(windows)]
fn main() -> anyhow::Result<()> {
    let use_sunshine_private_data = std::env::args_os().any(|arg| arg == "--sunshine-private-data");
    let capture_mode = std::env::args_os().any(|arg| arg == "--capture");
    let event_blocking_grab = std::env::args_os().any(|arg| arg == "--event-blocking-grab");
    let gpu_sleep_grab = std::env::args_os().any(|arg| arg == "--gpu-sleep-grab");
    let vblank_grab = std::env::args_os().any(|arg| arg == "--vblank-grab");
    let grab_strategy = if vblank_grab {
        GrabStrategy::VBlank
    } else if gpu_sleep_grab {
        GrabStrategy::GpuSleep
    } else if event_blocking_grab {
        GrabStrategy::Event
    } else {
        GrabStrategy::Timeout
    };
    let dll_path = system_nvfbc_path();
    println!("nvfbc_probe=status_and_create");
    println!("sunshine_private_data={use_sunshine_private_data}");
    println!("capture_mode={capture_mode}");
    println!("grab_strategy={}", grab_strategy.label());
    println!("dll={}", dll_path.display());
    println!("status_struct_size={}", size_of::<NvFbcStatusEx>());

    let library = unsafe { Library::new(&dll_path) }?;
    let get_sdk_version: Symbol<NvFbcGetSdkVersion> =
        unsafe { library.get(b"NvFBC_GetSDKVersion\0") }?;
    let get_status_ex: Symbol<NvFbcGetStatusEx> = unsafe { library.get(b"NvFBC_GetStatusEx\0") }?;
    let create_ex: Symbol<NvFbcCreateEx> = unsafe { library.get(b"NvFBC_CreateEx\0") }?;

    let mut sdk_version = 0_u32;
    let sdk_result = unsafe { get_sdk_version(&mut sdk_version) };
    println!(
        "get_sdk_version_result={}({}) sdk_version=0x{sdk_version:08x}",
        sdk_result,
        result_name(sdk_result)
    );

    if sdk_result != NVFBC_SUCCESS {
        anyhow::bail!("NvFBC_GetSDKVersion failed with {sdk_result}");
    }

    // Existing Windows SDK releases use the low byte as the DLL ABI byte
    // (0x50 for SDK 5.x and 0x70 for SDK 7.x). If the driver returns a packed
    // value, also try its low byte and the known compatible ABI values.
    let mut abi_versions = vec![sdk_version & 0xff, sdk_version, 0x70, 0x50];
    abi_versions.retain(|version| *version != 0);
    abi_versions.sort_unstable();
    abi_versions.dedup();
    abi_versions.reverse();

    for abi_version in abi_versions {
        println!("abi_candidate=0x{abi_version:08x}");
        for adapter in 0..8_u32 {
            let mut status = NvFbcStatusEx::new(adapter, abi_version);
            let result = unsafe { get_status_ex((&mut status as *mut NvFbcStatusEx).cast()) };
            println!(
                concat!(
                    "adapter={adapter} result={result}({result_name}) ",
                    "struct_version=0x{struct_version:08x} driver_version=0x{driver_version:08x} ",
                    "capture_possible={capture_possible} currently_capturing={currently_capturing} ",
                    "can_create_now={can_create_now} multi_head={multi_head} multi_client={multi_client} ",
                    "raw_flags=0x{flags:08x}"
                ),
                adapter = adapter,
                result = result,
                result_name = result_name(result),
                struct_version = status.dw_version,
                driver_version = status.dw_nvfbc_version,
                capture_possible = status.flags & STATUS_FLAG_CAPTURE_POSSIBLE != 0,
                currently_capturing = status.flags & STATUS_FLAG_CURRENTLY_CAPTURING != 0,
                can_create_now = status.flags & STATUS_FLAG_CAN_CREATE_NOW != 0,
                multi_head = status.flags & STATUS_FLAG_MULTI_HEAD != 0,
                multi_client = status.flags & STATUS_FLAG_MULTI_CLIENT != 0,
                flags = status.flags,
            );
        }
    }

    println!("create_probe_begin=true");
    println!("create_struct_size={}", size_of::<NvFbcCreateParams>());
    let d3d = unsafe { Direct3DCreate9Ex(D3D_SDK_VERSION) }?;
    let adapter_count = unsafe { d3d.GetAdapterCount() };
    println!("d3d9_adapter_count={adapter_count}");
    for adapter in 0..adapter_count {
        let mut identifier = D3DADAPTER_IDENTIFIER9::default();
        let identify_result = unsafe { d3d.GetAdapterIdentifier(adapter, 0, &mut identifier) };
        if let Err(err) = identify_result {
            println!("d3d9_adapter={adapter} identify_error={err}");
            continue;
        }

        let description = c_string(&identifier.Description);
        println!(
            "d3d9_adapter={adapter} vendor=0x{:04x} device=0x{:04x} description={description:?}",
            identifier.VendorId, identifier.DeviceId
        );
        if identifier.VendorId != NVIDIA_VENDOR_ID {
            continue;
        }

        let device = match create_d3d9_device(&d3d, adapter) {
            Ok(device) => device,
            Err(err) => {
                println!("d3d9_adapter={adapter} create_device_error={err}");
                continue;
            }
        };
        println!("d3d9_adapter={adapter} create_device_success=true");
        let dxgi_output = if capture_mode && matches!(grab_strategy, GrabStrategy::VBlank) {
            let output = matching_dxgi_output(&d3d, adapter)?;
            println!("d3d9_adapter={adapter} matching_dxgi_output=true");
            Some(output)
        } else {
            None
        };

        for interface_type in [NVFBC_TO_DX9_VID_V2, NVFBC_TO_DX9_VID_V3] {
            let mut sunshine_private_data = SUNSHINE_PRIVATE_DATA;
            let private_data = use_sunshine_private_data.then_some(&mut sunshine_private_data);
            let mut create = NvFbcCreateParams::new(
                adapter,
                sdk_version & 0xff,
                interface_type,
                &device,
                private_data,
            );
            let result = unsafe { create_ex((&mut create as *mut NvFbcCreateParams).cast()) };
            println!(
                concat!(
                    "d3d9_adapter={adapter} requested_interface=0x{requested_interface:04x} ",
                    "create_ex_result={result}({result_name}) struct_version=0x{struct_version:08x} ",
                    "max_width={max_width} max_height={max_height} driver_version=0x{driver_version:08x} ",
                    "interface=0x{interface:x}"
                ),
                adapter = adapter,
                requested_interface = interface_type,
                result = result,
                result_name = result_name(result),
                struct_version = create.dw_version,
                max_width = create.dw_max_display_width,
                max_height = create.dw_max_display_height,
                driver_version = create.dw_nvfbc_version,
                interface = create.nvfbc as usize,
            );

            if result == NVFBC_SUCCESS && !create.nvfbc.is_null() {
                let capture_result = if capture_mode && interface_type == NVFBC_TO_DX9_VID_V3 {
                    unsafe {
                        run_hdr_capture(
                            create.nvfbc,
                            CaptureRunConfig {
                                device: &device,
                                dxgi_output: dxgi_output.as_ref(),
                                width: create.dw_max_display_width,
                                height: create.dw_max_display_height,
                                version: sdk_version & 0xff,
                                duration: Duration::from_secs(10),
                                strategy: grab_strategy,
                            },
                        )
                    }
                } else {
                    Ok(())
                };
                let release_result = unsafe { release_nvfbc_interface(create.nvfbc) };
                println!(
                    concat!(
                        "d3d9_adapter={adapter} requested_interface=0x{requested_interface:04x} ",
                        "release_result={release_result}({release_name})"
                    ),
                    adapter = adapter,
                    requested_interface = interface_type,
                    release_result = release_result,
                    release_name = result_name(release_result),
                );
                capture_result?;
            }
        }
    }

    Ok(())
}

#[cfg(not(windows))]
fn main() {
    eprintln!("nvfbc_probe is only available on Windows");
}
