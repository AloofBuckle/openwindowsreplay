//! DXGI 适配器枚举。
//!
//! 这里仅读取适配器名称和 LUID，不做跨 GPU 复制。后续创建 D3D11 device、DDA/WGC
//! capture 和 oneVPL session 时必须使用同一个 LUID。

use crate::error::BackendError;
use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct DxgiAdapterInfo {
    pub index: u32,
    pub description: String,
    pub vendor_id: u32,
    pub device_id: u32,
    pub subsystem_id: u32,
    pub revision: u32,
    pub dedicated_video_memory: u64,
    pub dedicated_system_memory: u64,
    pub shared_system_memory: u64,
    pub luid_low: u32,
    pub luid_high: i32,
    pub flags: u32,
}

impl DxgiAdapterInfo {
    pub fn luid_string(&self) -> String {
        format!("{:08X}:{:08X}", self.luid_high as u32, self.luid_low)
    }
}

#[cfg(windows)]
pub fn enumerate_adapters() -> Result<Vec<DxgiAdapterInfo>, BackendError> {
    use windows::Win32::Graphics::Dxgi::{CreateDXGIFactory1, DXGI_ERROR_NOT_FOUND, IDXGIFactory1};

    unsafe {
        let factory: IDXGIFactory1 =
            CreateDXGIFactory1().map_err(|err| BackendError::WindowsApi {
                func: "CreateDXGIFactory1",
                message: err.to_string(),
            })?;

        let mut adapters = Vec::new();
        let mut index = 0u32;
        loop {
            match factory.EnumAdapters1(index) {
                Ok(adapter) => {
                    let desc = adapter.GetDesc1().map_err(|err| BackendError::WindowsApi {
                        func: "IDXGIAdapter1::GetDesc1",
                        message: err.to_string(),
                    })?;
                    adapters.push(DxgiAdapterInfo {
                        index,
                        description: utf16_z_to_string(&desc.Description),
                        vendor_id: desc.VendorId,
                        device_id: desc.DeviceId,
                        subsystem_id: desc.SubSysId,
                        revision: desc.Revision,
                        dedicated_video_memory: desc.DedicatedVideoMemory as u64,
                        dedicated_system_memory: desc.DedicatedSystemMemory as u64,
                        shared_system_memory: desc.SharedSystemMemory as u64,
                        luid_low: desc.AdapterLuid.LowPart,
                        luid_high: desc.AdapterLuid.HighPart,
                        flags: desc.Flags,
                    });
                    index += 1;
                }
                Err(err) if err.code() == DXGI_ERROR_NOT_FOUND => break,
                Err(err) => {
                    return Err(BackendError::WindowsApi {
                        func: "IDXGIFactory1::EnumAdapters1",
                        message: err.to_string(),
                    });
                }
            }
        }
        Ok(adapters)
    }
}

#[cfg(not(windows))]
pub fn enumerate_adapters() -> Result<Vec<DxgiAdapterInfo>, BackendError> {
    Err(BackendError::unsupported(
        "DXGI",
        "adapter LUID",
        "DXGI 只在 Windows 上可用",
    ))
}

fn utf16_z_to_string(buf: &[u16]) -> String {
    let len = buf.iter().position(|&c| c == 0).unwrap_or(buf.len());
    String::from_utf16_lossy(&buf[..len])
}
