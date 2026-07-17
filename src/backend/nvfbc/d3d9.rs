#![deny(unsafe_op_in_unsafe_fn)]

use crate::error::BackendError;
use std::ffi::c_void;
use std::marker::PhantomData;
use std::ptr::{self, NonNull};
use std::rc::Rc;
use windows::Win32::Graphics::Direct3D9::{
    D3D_SDK_VERSION, D3DADAPTER_IDENTIFIER9, D3DCREATE_FPU_PRESERVE,
    D3DCREATE_HARDWARE_VERTEXPROCESSING, D3DCREATE_MULTITHREADED, D3DDEVTYPE_HAL,
    D3DFMT_A2B10G10R10, D3DFMT_X8R8G8B8, D3DMULTISAMPLE_NONE, D3DPRESENT_INTERVAL_IMMEDIATE,
    D3DPRESENT_PARAMETERS, D3DPRESENTFLAG_VIDEO, D3DSWAPEFFECT_COPY, Direct3DCreate9Ex,
    IDirect3D9Ex, IDirect3DDevice9Ex, IDirect3DSurface9,
};
use windows::Win32::Graphics::Dxgi::{
    CreateDXGIFactory1, DXGI_ERROR_NOT_FOUND, IDXGIFactory1, IDXGIOutput, IDXGIOutput6,
};
use windows::Win32::Graphics::Gdi::{DEVMODEW, ENUM_CURRENT_SETTINGS, EnumDisplaySettingsW};
use windows::Win32::UI::WindowsAndMessaging::GetDesktopWindow;
use windows::core::{BOOL, Interface, PCWSTR};

const NVIDIA_VENDOR_ID: u32 = 0x10de;

#[derive(Debug, Clone)]
pub(super) struct DisplayInfo {
    pub(super) dxgi_adapter_index: u32,
    pub(super) output_index: u32,
    pub(super) device_name: String,
    pub(super) desktop_left: i32,
    pub(super) desktop_top: i32,
    pub(super) desktop_right: i32,
    pub(super) desktop_bottom: i32,
    pub(super) color_space: u32,
    pub(super) bits_per_color: u32,
    pub(super) refresh_numerator: u32,
    pub(super) refresh_denominator: u32,
}

pub(super) struct D3d9Device {
    // The D3D9 object owns adapter enumeration state and outlives its device.
    _d3d: IDirect3D9Ex,
    device: IDirect3DDevice9Ex,
    output: IDXGIOutput,
    adapter_index: u32,
    adapter_name: String,
    display: DisplayInfo,
    _not_send_sync: PhantomData<Rc<()>>,
}

pub(super) struct CaptureSurfacePool {
    surfaces: Vec<IDirect3DSurface9>,
    width: u32,
    height: u32,
    _not_send_sync: PhantomData<Rc<()>>,
}

impl D3d9Device {
    pub(super) fn open(requested_adapter: Option<u32>) -> Result<Self, BackendError> {
        let d3d = unsafe { Direct3DCreate9Ex(D3D_SDK_VERSION) }.map_err(|err| {
            BackendError::WindowsApi {
                func: "Direct3DCreate9Ex(NvFBC)",
                message: err.to_string(),
            }
        })?;
        let (adapter_index, adapter_name) = select_nvidia_adapter(&d3d, requested_adapter)?;
        let device = create_device(&d3d, adapter_index)?;
        let (output, display) = matching_output(&d3d, adapter_index)?;
        Ok(Self {
            _d3d: d3d,
            device,
            output,
            adapter_index,
            adapter_name,
            display,
            _not_send_sync: PhantomData,
        })
    }

    pub(super) fn adapter_index(&self) -> u32 {
        self.adapter_index
    }

    pub(super) fn adapter_name(&self) -> &str {
        &self.adapter_name
    }

    pub(super) fn display(&self) -> &DisplayInfo {
        &self.display
    }

    pub(super) fn device(&self) -> &IDirect3DDevice9Ex {
        &self.device
    }

    pub(super) fn raw_device(&self) -> NonNull<c_void> {
        NonNull::new(self.device.as_raw()).expect("Windows COM interface cannot be null")
    }

    pub(super) fn wait_for_vblank(&self) -> Result<(), BackendError> {
        unsafe { self.output.WaitForVBlank() }.map_err(|err| BackendError::WindowsApi {
            func: "IDXGIOutput::WaitForVBlank(NvFBC)",
            message: err.to_string(),
        })
    }

    pub(super) fn create_surface_pool(
        &self,
        width: u32,
        height: u32,
        count: usize,
    ) -> Result<CaptureSurfacePool, BackendError> {
        if width == 0 || height == 0 || count == 0 || count > 3 {
            return Err(BackendError::unsupported(
                "NvFBC D3D9Ex surface pool",
                format!("{width}x{height} count={count}"),
                "尺寸必须非零，直连 NvFBC 路线最多三个 surface",
            ));
        }
        let mut surfaces = Vec::with_capacity(count);
        for _ in 0..count {
            let mut surface = None;
            unsafe {
                self.device.CreateRenderTarget(
                    width,
                    height,
                    D3DFMT_A2B10G10R10,
                    D3DMULTISAMPLE_NONE,
                    0,
                    false,
                    &mut surface,
                    ptr::null_mut(),
                )
            }
            .map_err(|err| BackendError::WindowsApi {
                func: "IDirect3DDevice9Ex::CreateRenderTarget(NvFBC ARGB10)",
                message: err.to_string(),
            })?;
            surfaces.push(surface.ok_or_else(|| BackendError::WindowsApi {
                func: "IDirect3DDevice9Ex::CreateRenderTarget(NvFBC ARGB10)",
                message: "返回空 surface".to_owned(),
            })?);
        }
        Ok(CaptureSurfacePool {
            surfaces,
            width,
            height,
            _not_send_sync: PhantomData,
        })
    }
}

impl CaptureSurfacePool {
    pub(super) fn surfaces(&self) -> &[IDirect3DSurface9] {
        &self.surfaces
    }

    pub(super) fn raw_surfaces(&self) -> Vec<NonNull<c_void>> {
        self.surfaces
            .iter()
            .map(|surface| {
                NonNull::new(surface.as_raw()).expect("Windows COM interface cannot be null")
            })
            .collect()
    }

    pub(super) fn dimensions(&self) -> (u32, u32) {
        (self.width, self.height)
    }
}

fn select_nvidia_adapter(
    d3d: &IDirect3D9Ex,
    requested_adapter: Option<u32>,
) -> Result<(u32, String), BackendError> {
    let count = unsafe { d3d.GetAdapterCount() };
    let candidates: Box<dyn Iterator<Item = u32>> = if let Some(index) = requested_adapter {
        Box::new(std::iter::once(index))
    } else {
        Box::new(0..count)
    };
    for index in candidates {
        if index >= count {
            continue;
        }
        let mut identifier = D3DADAPTER_IDENTIFIER9::default();
        if unsafe { d3d.GetAdapterIdentifier(index, 0, &mut identifier) }.is_err() {
            continue;
        }
        if identifier.VendorId == NVIDIA_VENDOR_ID {
            return Ok((index, c_string(&identifier.Description)));
        }
    }
    Err(BackendError::unsupported(
        "NvFBC D3D9Ex adapter",
        requested_adapter
            .map(|index| format!("adapter={index}"))
            .unwrap_or_else(|| "NVIDIA adapter".to_owned()),
        "没有找到可创建 D3D9Ex device 的 NVIDIA adapter",
    ))
}

fn create_device(
    d3d: &IDirect3D9Ex,
    adapter_index: u32,
) -> Result<IDirect3DDevice9Ex, BackendError> {
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
            adapter_index,
            D3DDEVTYPE_HAL,
            desktop,
            behavior,
            &mut present,
            ptr::null_mut(),
            &mut device,
        )
    }
    .map_err(|err| BackendError::WindowsApi {
        func: "IDirect3D9Ex::CreateDeviceEx(NvFBC)",
        message: err.to_string(),
    })?;
    device.ok_or_else(|| BackendError::WindowsApi {
        func: "IDirect3D9Ex::CreateDeviceEx(NvFBC)",
        message: "返回空 device".to_owned(),
    })
}

fn matching_output(
    d3d: &IDirect3D9Ex,
    d3d_adapter_index: u32,
) -> Result<(IDXGIOutput, DisplayInfo), BackendError> {
    let target_monitor = unsafe { d3d.GetAdapterMonitor(d3d_adapter_index) };
    let factory: IDXGIFactory1 =
        unsafe { CreateDXGIFactory1() }.map_err(|err| BackendError::WindowsApi {
            func: "CreateDXGIFactory1(NvFBC D3D9 monitor match)",
            message: err.to_string(),
        })?;
    let mut adapter_index = 0;
    loop {
        let adapter = match unsafe { factory.EnumAdapters1(adapter_index) } {
            Ok(adapter) => adapter,
            Err(err) if err.code() == DXGI_ERROR_NOT_FOUND => break,
            Err(err) => {
                return Err(BackendError::WindowsApi {
                    func: "IDXGIFactory1::EnumAdapters1(NvFBC)",
                    message: err.to_string(),
                });
            }
        };
        let mut output_index = 0;
        loop {
            let output = match unsafe { adapter.EnumOutputs(output_index) } {
                Ok(output) => output,
                Err(err) if err.code() == DXGI_ERROR_NOT_FOUND => break,
                Err(err) => {
                    return Err(BackendError::WindowsApi {
                        func: "IDXGIAdapter1::EnumOutputs(NvFBC)",
                        message: err.to_string(),
                    });
                }
            };
            let desc = unsafe { output.GetDesc() }.map_err(|err| BackendError::WindowsApi {
                func: "IDXGIOutput::GetDesc(NvFBC)",
                message: err.to_string(),
            })?;
            if desc.Monitor == target_monitor {
                let output6 = output.cast::<IDXGIOutput6>().map_err(|_| {
                    BackendError::unsupported(
                        "NvFBC display color",
                        "IDXGIOutput6::GetDesc1",
                        "当前输出不能提供桌面色彩状态",
                    )
                })?;
                let desc1 =
                    unsafe { output6.GetDesc1() }.map_err(|err| BackendError::WindowsApi {
                        func: "IDXGIOutput6::GetDesc1(NvFBC)",
                        message: err.to_string(),
                    })?;
                let (refresh_numerator, refresh_denominator) =
                    current_refresh_rate(&desc.DeviceName)?;
                return Ok((
                    output,
                    DisplayInfo {
                        dxgi_adapter_index: adapter_index,
                        output_index,
                        device_name: wide_string(&desc.DeviceName),
                        desktop_left: desc.DesktopCoordinates.left,
                        desktop_top: desc.DesktopCoordinates.top,
                        desktop_right: desc.DesktopCoordinates.right,
                        desktop_bottom: desc.DesktopCoordinates.bottom,
                        color_space: desc1.ColorSpace.0 as u32,
                        bits_per_color: desc1.BitsPerColor,
                        refresh_numerator,
                        refresh_denominator,
                    },
                ));
            }
            output_index += 1;
        }
        adapter_index += 1;
    }
    Err(BackendError::unsupported(
        "NvFBC D3D9Ex output",
        format!("D3D9 adapter={d3d_adapter_index}"),
        "没有找到 monitor handle 一致的 DXGI output",
    ))
}

fn current_refresh_rate(device_name: &[u16; 32]) -> Result<(u32, u32), BackendError> {
    let mut mode = DEVMODEW {
        dmSize: std::mem::size_of::<DEVMODEW>() as u16,
        ..Default::default()
    };
    let ok = unsafe {
        EnumDisplaySettingsW(
            PCWSTR(device_name.as_ptr()),
            ENUM_CURRENT_SETTINGS,
            &mut mode,
        )
    };
    if !ok.as_bool() || mode.dmDisplayFrequency <= 1 {
        return Err(BackendError::unsupported(
            "NvFBC display refresh",
            wide_string(device_name),
            "EnumDisplaySettingsW 未返回有效当前刷新率",
        ));
    }
    Ok((mode.dmDisplayFrequency, 1))
}

fn c_string(value: &[i8]) -> String {
    let end = value
        .iter()
        .position(|byte| *byte == 0)
        .unwrap_or(value.len());
    let bytes = value[..end]
        .iter()
        .map(|byte| *byte as u8)
        .collect::<Vec<_>>();
    String::from_utf8_lossy(&bytes).into_owned()
}

fn wide_string(value: &[u16]) -> String {
    let end = value
        .iter()
        .position(|code| *code == 0)
        .unwrap_or(value.len());
    String::from_utf16_lossy(&value[..end])
}
