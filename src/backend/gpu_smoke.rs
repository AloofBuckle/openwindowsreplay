//! D3D11/DDA GPU-only 冒烟测试。
//!
//! 这个模块不读回像素，只验证同一 DXGI adapter 上能否创建设备、DuplicateOutput
//! 并拿到 `ID3D11Texture2D`。

use crate::error::BackendError;
use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct D3d11DdaSmokeResult {
    pub adapter_index: u32,
    pub adapter_luid: String,
    pub output_description: String,
    pub desktop_width: u32,
    pub desktop_height: u32,
    pub acquired_texture_width: u32,
    pub acquired_texture_height: u32,
    pub acquired_texture_format: u32,
    pub accumulated_frames: u32,
    pub last_present_time_qpc: i64,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct D3d11DdaRateResult {
    pub adapter_index: u32,
    pub adapter_luid: String,
    pub output_description: String,
    pub desktop_width: u32,
    pub desktop_height: u32,
    pub duration_seconds: f32,
    pub acquired_frames: u32,
    pub timeouts: u32,
    pub accumulated_frames_total: u64,
    pub accumulated_frames_max: u32,
    pub acquired_fps: f64,
    pub accumulated_fps: f64,
}

#[cfg(windows)]
pub fn run_d3d11_dda_smoke(adapter_index: u32) -> Result<D3d11DdaSmokeResult, BackendError> {
    use windows::Win32::Foundation::HMODULE;
    use windows::Win32::Graphics::Direct3D::{
        D3D_DRIVER_TYPE_UNKNOWN, D3D_FEATURE_LEVEL, D3D_FEATURE_LEVEL_11_0, D3D_FEATURE_LEVEL_11_1,
    };
    use windows::Win32::Graphics::Direct3D11::{
        D3D11_CREATE_DEVICE_BGRA_SUPPORT, D3D11_CREATE_DEVICE_VIDEO_SUPPORT, D3D11_SDK_VERSION,
        D3D11_TEXTURE2D_DESC, D3D11CreateDevice, ID3D11Device, ID3D11Texture2D,
    };
    use windows::Win32::Graphics::Dxgi::{
        CreateDXGIFactory1, DXGI_ERROR_WAIT_TIMEOUT, DXGI_OUTDUPL_FRAME_INFO, IDXGIAdapter,
        IDXGIFactory1, IDXGIOutput1, IDXGIResource,
    };
    use windows::core::Interface;

    unsafe {
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
        let adapter: IDXGIAdapter = adapter1.cast().map_err(|err| BackendError::WindowsApi {
            func: "IDXGIAdapter1::cast",
            message: err.to_string(),
        })?;

        let mut device: Option<ID3D11Device> = None;
        let mut feature_level = D3D_FEATURE_LEVEL(0);
        let levels = [D3D_FEATURE_LEVEL_11_1, D3D_FEATURE_LEVEL_11_0];
        D3D11CreateDevice(
            Some(&adapter),
            D3D_DRIVER_TYPE_UNKNOWN,
            HMODULE::default(),
            D3D11_CREATE_DEVICE_BGRA_SUPPORT | D3D11_CREATE_DEVICE_VIDEO_SUPPORT,
            Some(&levels),
            D3D11_SDK_VERSION,
            Some(&mut device),
            Some(&mut feature_level),
            None,
        )
        .map_err(|err| BackendError::WindowsApi {
            func: "D3D11CreateDevice",
            message: err.to_string(),
        })?;
        let device = device.ok_or_else(|| BackendError::WindowsApi {
            func: "D3D11CreateDevice",
            message: "返回空 ID3D11Device".to_owned(),
        })?;

        let output = adapter1
            .EnumOutputs(0)
            .map_err(|err| BackendError::WindowsApi {
                func: "IDXGIAdapter1::EnumOutputs(0)",
                message: err.to_string(),
            })?;
        let output_desc = output.GetDesc().map_err(|err| BackendError::WindowsApi {
            func: "IDXGIOutput::GetDesc",
            message: err.to_string(),
        })?;
        let output1: IDXGIOutput1 = output.cast().map_err(|err| BackendError::WindowsApi {
            func: "IDXGIOutput::cast<IDXGIOutput1>",
            message: err.to_string(),
        })?;
        let duplication =
            output1
                .DuplicateOutput(&device)
                .map_err(|err| BackendError::WindowsApi {
                    func: "IDXGIOutput1::DuplicateOutput",
                    message: err.to_string(),
                })?;

        let mut frame_info = DXGI_OUTDUPL_FRAME_INFO::default();
        let mut resource: Option<IDXGIResource> = None;
        match duplication.AcquireNextFrame(2000, &mut frame_info, &mut resource) {
            Ok(()) => {}
            Err(err) if err.code() == DXGI_ERROR_WAIT_TIMEOUT => {
                return Err(BackendError::unsupported(
                    "DDA 冒烟测试",
                    "AcquireNextFrame",
                    "2 秒内没有桌面更新帧；请在测试机桌面产生变化后重试",
                ));
            }
            Err(err) => {
                return Err(BackendError::WindowsApi {
                    func: "IDXGIOutputDuplication::AcquireNextFrame",
                    message: err.to_string(),
                });
            }
        }
        let resource = resource.ok_or_else(|| BackendError::WindowsApi {
            func: "AcquireNextFrame",
            message: "返回空 IDXGIResource".to_owned(),
        })?;
        let texture: ID3D11Texture2D = resource.cast().map_err(|err| BackendError::WindowsApi {
            func: "IDXGIResource::cast<ID3D11Texture2D>",
            message: err.to_string(),
        })?;
        let mut texture_desc = D3D11_TEXTURE2D_DESC::default();
        texture.GetDesc(&mut texture_desc);
        duplication
            .ReleaseFrame()
            .map_err(|err| BackendError::WindowsApi {
                func: "IDXGIOutputDuplication::ReleaseFrame",
                message: err.to_string(),
            })?;

        Ok(D3d11DdaSmokeResult {
            adapter_index,
            adapter_luid,
            output_description: utf16_z_to_string(&output_desc.DeviceName),
            desktop_width: rect_width(output_desc.DesktopCoordinates),
            desktop_height: rect_height(output_desc.DesktopCoordinates),
            acquired_texture_width: texture_desc.Width,
            acquired_texture_height: texture_desc.Height,
            acquired_texture_format: texture_desc.Format.0 as u32,
            accumulated_frames: frame_info.AccumulatedFrames,
            last_present_time_qpc: frame_info.LastPresentTime,
        })
    }
}

#[cfg(windows)]
pub fn measure_d3d11_dda_rate(
    adapter_index: u32,
    duration_seconds: f32,
) -> Result<D3d11DdaRateResult, BackendError> {
    use std::time::{Duration, Instant};
    use windows::Win32::Foundation::HMODULE;
    use windows::Win32::Graphics::Direct3D::{
        D3D_DRIVER_TYPE_UNKNOWN, D3D_FEATURE_LEVEL, D3D_FEATURE_LEVEL_11_0, D3D_FEATURE_LEVEL_11_1,
    };
    use windows::Win32::Graphics::Direct3D11::{
        D3D11_CREATE_DEVICE_BGRA_SUPPORT, D3D11_CREATE_DEVICE_VIDEO_SUPPORT, D3D11_SDK_VERSION,
        D3D11CreateDevice, ID3D11Device,
    };
    use windows::Win32::Graphics::Dxgi::{
        CreateDXGIFactory1, DXGI_ERROR_WAIT_TIMEOUT, DXGI_OUTDUPL_FRAME_INFO, IDXGIAdapter,
        IDXGIFactory1, IDXGIOutput1, IDXGIResource,
    };
    use windows::core::Interface;

    unsafe {
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
        let adapter: IDXGIAdapter = adapter1.cast().map_err(|err| BackendError::WindowsApi {
            func: "IDXGIAdapter1::cast",
            message: err.to_string(),
        })?;

        let mut device: Option<ID3D11Device> = None;
        let mut feature_level = D3D_FEATURE_LEVEL(0);
        let levels = [D3D_FEATURE_LEVEL_11_1, D3D_FEATURE_LEVEL_11_0];
        D3D11CreateDevice(
            Some(&adapter),
            D3D_DRIVER_TYPE_UNKNOWN,
            HMODULE::default(),
            D3D11_CREATE_DEVICE_BGRA_SUPPORT | D3D11_CREATE_DEVICE_VIDEO_SUPPORT,
            Some(&levels),
            D3D11_SDK_VERSION,
            Some(&mut device),
            Some(&mut feature_level),
            None,
        )
        .map_err(|err| BackendError::WindowsApi {
            func: "D3D11CreateDevice",
            message: err.to_string(),
        })?;
        let device = device.ok_or_else(|| BackendError::WindowsApi {
            func: "D3D11CreateDevice",
            message: "返回空 ID3D11Device".to_owned(),
        })?;

        let output = adapter1
            .EnumOutputs(0)
            .map_err(|err| BackendError::WindowsApi {
                func: "IDXGIAdapter1::EnumOutputs(0)",
                message: err.to_string(),
            })?;
        let output_desc = output.GetDesc().map_err(|err| BackendError::WindowsApi {
            func: "IDXGIOutput::GetDesc",
            message: err.to_string(),
        })?;
        let output1: IDXGIOutput1 = output.cast().map_err(|err| BackendError::WindowsApi {
            func: "IDXGIOutput::cast<IDXGIOutput1>",
            message: err.to_string(),
        })?;
        let duplication =
            output1
                .DuplicateOutput(&device)
                .map_err(|err| BackendError::WindowsApi {
                    func: "IDXGIOutput1::DuplicateOutput",
                    message: err.to_string(),
                })?;

        let start = Instant::now();
        let end_at = start + Duration::from_secs_f32(duration_seconds.max(0.1));
        let mut acquired_frames = 0u32;
        let mut timeouts = 0u32;
        let mut accumulated_frames_total = 0u64;
        let mut accumulated_frames_max = 0u32;

        while Instant::now() < end_at {
            let mut frame_info = DXGI_OUTDUPL_FRAME_INFO::default();
            let mut resource: Option<IDXGIResource> = None;
            match duplication.AcquireNextFrame(100, &mut frame_info, &mut resource) {
                Ok(()) => {
                    accumulated_frames_total += frame_info.AccumulatedFrames as u64;
                    accumulated_frames_max =
                        accumulated_frames_max.max(frame_info.AccumulatedFrames);
                    acquired_frames += 1;
                    duplication
                        .ReleaseFrame()
                        .map_err(|err| BackendError::WindowsApi {
                            func: "IDXGIOutputDuplication::ReleaseFrame(rate)",
                            message: err.to_string(),
                        })?;
                }
                Err(err) if err.code() == DXGI_ERROR_WAIT_TIMEOUT => {
                    timeouts += 1;
                }
                Err(err) => {
                    return Err(BackendError::WindowsApi {
                        func: "IDXGIOutputDuplication::AcquireNextFrame(rate)",
                        message: err.to_string(),
                    });
                }
            }
        }

        let elapsed = start.elapsed().as_secs_f64().max(0.001);
        Ok(D3d11DdaRateResult {
            adapter_index,
            adapter_luid,
            output_description: utf16_z_to_string(&output_desc.DeviceName),
            desktop_width: rect_width(output_desc.DesktopCoordinates),
            desktop_height: rect_height(output_desc.DesktopCoordinates),
            duration_seconds,
            acquired_frames,
            timeouts,
            accumulated_frames_total,
            accumulated_frames_max,
            acquired_fps: acquired_frames as f64 / elapsed,
            accumulated_fps: accumulated_frames_total as f64 / elapsed,
        })
    }
}

#[cfg(not(windows))]
pub fn run_d3d11_dda_smoke(adapter_index: u32) -> Result<D3d11DdaSmokeResult, BackendError> {
    let _ = adapter_index;
    Err(BackendError::unsupported(
        "D3D11/DDA 冒烟测试",
        "Windows Desktop Duplication",
        "仅 Windows 可用",
    ))
}

#[cfg(not(windows))]
pub fn measure_d3d11_dda_rate(
    adapter_index: u32,
    duration_seconds: f32,
) -> Result<D3d11DdaRateResult, BackendError> {
    let _ = (adapter_index, duration_seconds);
    Err(BackendError::unsupported(
        "D3D11/DDA rate 测试",
        "Windows Desktop Duplication",
        "仅 Windows 可用",
    ))
}

#[cfg(windows)]
fn rect_width(rect: windows::Win32::Foundation::RECT) -> u32 {
    (rect.right - rect.left).max(0) as u32
}

#[cfg(windows)]
fn rect_height(rect: windows::Win32::Foundation::RECT) -> u32 {
    (rect.bottom - rect.top).max(0) as u32
}

#[cfg(windows)]
fn utf16_z_to_string(buf: &[u16]) -> String {
    let len = buf.iter().position(|&c| c == 0).unwrap_or(buf.len());
    String::from_utf16_lossy(&buf[..len])
}
