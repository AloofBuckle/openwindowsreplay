#![deny(unsafe_op_in_unsafe_fn)]

use super::d3d9::{CaptureSurfacePool, D3d9Device};
use crate::error::BackendError;
use std::ffi::c_void;
use std::marker::PhantomData;
use std::ptr::NonNull;
use std::rc::Rc;

#[repr(C)]
struct RrNvFbcHandle {
    _private: [u8; 0],
}

#[repr(C)]
#[derive(Debug, Default, Clone, Copy)]
struct RrNvFbcCreateInfo {
    max_width: u32,
    max_height: u32,
    nvfbc_version: u32,
}

#[repr(C)]
#[derive(Debug, Default, Clone, Copy)]
struct RrNvFbcGrabInfo {
    width: u32,
    height: u32,
    buffer_width: u32,
    must_recreate: u32,
    protected_content: u32,
    driver_internal_error: u32,
    source_pid: u32,
    is_hdr: u32,
    wait_mode_used: u32,
}

unsafe extern "C" {
    fn rr_nvfbc_create(
        d3d9_device: *mut c_void,
        adapter: u32,
        out_handle: *mut *mut RrNvFbcHandle,
        out_info: *mut RrNvFbcCreateInfo,
    ) -> i32;
    fn rr_nvfbc_setup(
        handle: *mut RrNvFbcHandle,
        d3d9_surfaces: *const *mut c_void,
        surface_count: u32,
        hdr: u32,
        cursor: u32,
    ) -> i32;
    fn rr_nvfbc_grab(
        handle: *mut RrNvFbcHandle,
        surface_index: u32,
        out_info: *mut RrNvFbcGrabInfo,
    ) -> i32;
    fn rr_nvfbc_gpu_sleep(handle: *mut RrNvFbcHandle, microseconds: i64) -> i32;
    fn rr_nvfbc_destroy(handle: *mut RrNvFbcHandle) -> i32;
}

#[derive(Debug, Clone, Copy)]
pub(super) struct CreateInfo {
    pub(super) max_width: u32,
    pub(super) max_height: u32,
    pub(super) nvfbc_version: u32,
}

#[derive(Debug, Clone, Copy)]
pub(super) struct GrabInfo {
    pub(super) width: u32,
    pub(super) height: u32,
    pub(super) buffer_width: u32,
    pub(super) must_recreate: bool,
    pub(super) protected_content: bool,
    pub(super) driver_internal_error: u32,
    pub(super) source_pid: u32,
    pub(super) is_hdr: bool,
    pub(super) wait_mode_used: u32,
}

pub(super) struct NvFbcSession {
    handle: Option<NonNull<RrNvFbcHandle>>,
    _not_send_sync: PhantomData<Rc<()>>,
}

impl NvFbcSession {
    pub(super) fn create(device: &D3d9Device) -> Result<(Self, CreateInfo), BackendError> {
        let mut handle = std::ptr::null_mut();
        let mut info = RrNvFbcCreateInfo::default();
        let status = unsafe {
            rr_nvfbc_create(
                device.raw_device().as_ptr(),
                device.adapter_index(),
                &mut handle,
                &mut info,
            )
        };
        check_status("NvFBC_CreateEx", status)?;
        let handle = NonNull::new(handle).ok_or_else(|| {
            BackendError::unsupported(
                "NvFBC FFI",
                "NvFBC_CreateEx",
                "shim 返回成功但 session handle 为空",
            )
        })?;
        if info.max_width == 0 || info.max_height == 0 {
            unsafe {
                let _ = rr_nvfbc_destroy(handle.as_ptr());
            }
            return Err(BackendError::unsupported(
                "NvFBC FFI",
                "NvFBC_CreateEx display dimensions",
                format!("返回无效上限 {}x{}", info.max_width, info.max_height),
            ));
        }
        Ok((
            Self {
                handle: Some(handle),
                _not_send_sync: PhantomData,
            },
            CreateInfo {
                max_width: info.max_width,
                max_height: info.max_height,
                nvfbc_version: info.nvfbc_version,
            },
        ))
    }

    pub(super) fn setup(
        &mut self,
        surfaces: &CaptureSurfacePool,
        hdr: bool,
        cursor: bool,
    ) -> Result<(), BackendError> {
        let handle = self.handle()?;
        let raw_surfaces = surfaces.raw_surfaces();
        let pointers = raw_surfaces
            .iter()
            .map(|surface| surface.as_ptr())
            .collect::<Vec<_>>();
        let status = unsafe {
            rr_nvfbc_setup(
                handle.as_ptr(),
                pointers.as_ptr(),
                pointers.len() as u32,
                u32::from(hdr),
                u32::from(cursor),
            )
        };
        check_status("NvFBCToDx9VidSetUp(V3 ARGB10)", status)
    }

    pub(super) fn grab(&mut self, surface_index: usize) -> Result<GrabInfo, BackendError> {
        let handle = self.handle()?;
        let index = u32::try_from(surface_index).map_err(|_| {
            BackendError::unsupported(
                "NvFBC FFI",
                format!("surface_index={surface_index}"),
                "surface 索引超出 u32",
            )
        })?;
        let mut info = RrNvFbcGrabInfo::default();
        let status = unsafe { rr_nvfbc_grab(handle.as_ptr(), index, &mut info) };
        if status != 0 || info.must_recreate != 0 {
            if info.must_recreate != 0 || status == -3 {
                return Err(BackendError::reconfigure_required(format!(
                    "NvFBC session invalidated: status={status}({}) driver_internal=0x{:08x}",
                    status_name(status),
                    info.driver_internal_error
                )));
            }
            check_status_with_driver("NvFBCToDx9VidGrabFrame", status, info.driver_internal_error)?;
        }
        Ok(GrabInfo {
            width: info.width,
            height: info.height,
            buffer_width: info.buffer_width,
            must_recreate: info.must_recreate != 0,
            protected_content: info.protected_content != 0,
            driver_internal_error: info.driver_internal_error,
            source_pid: info.source_pid,
            is_hdr: info.is_hdr != 0,
            wait_mode_used: info.wait_mode_used,
        })
    }

    #[allow(dead_code)]
    pub(super) fn gpu_sleep(&mut self, microseconds: i64) -> Result<(), BackendError> {
        let handle = self.handle()?;
        let status = unsafe { rr_nvfbc_gpu_sleep(handle.as_ptr(), microseconds) };
        check_status("NvFBCToDx9Vid::GpuSleep", status)
    }

    pub(super) fn close(mut self) -> Result<(), BackendError> {
        let Some(handle) = self.handle.take() else {
            return Ok(());
        };
        let status = unsafe { rr_nvfbc_destroy(handle.as_ptr()) };
        check_status("NvFBCToDx9VidRelease", status)
    }

    fn handle(&self) -> Result<NonNull<RrNvFbcHandle>, BackendError> {
        self.handle.ok_or_else(|| {
            BackendError::unsupported("NvFBC FFI", "session handle", "session 已关闭")
        })
    }
}

impl Drop for NvFbcSession {
    fn drop(&mut self) {
        if let Some(handle) = self.handle.take() {
            unsafe {
                let _ = rr_nvfbc_destroy(handle.as_ptr());
            }
        }
    }
}

fn check_status(stage: &'static str, status: i32) -> Result<(), BackendError> {
    check_status_with_driver(stage, status, 0)
}

fn check_status_with_driver(
    stage: &'static str,
    status: i32,
    driver_internal_error: u32,
) -> Result<(), BackendError> {
    if status == 0 {
        return Ok(());
    }
    if status == -3 {
        return Err(BackendError::reconfigure_required(format!(
            "{stage}: status={status}({}) driver_internal=0x{driver_internal_error:08x}",
            status_name(status)
        )));
    }
    Err(BackendError::unsupported(
        "NvFBC FFI",
        stage,
        format!(
            "status={status}({}) driver_internal=0x{driver_internal_error:08x}",
            status_name(status)
        ),
    ))
}

fn status_name(status: i32) -> &'static str {
    match status {
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
        -1001 => "RUSTREPLAY_NVFBC_DLL_LOAD_FAILED",
        -1002 => "RUSTREPLAY_NVFBC_EXPORT_MISSING",
        -1003 => "RUSTREPLAY_NVFBC_OUT_OF_MEMORY",
        -1004 => "RUSTREPLAY_NVFBC_CPP_EXCEPTION",
        _ => "NVFBC_ERROR_UNKNOWN",
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn shim_abi_struct_sizes_are_stable() {
        assert_eq!(std::mem::size_of::<RrNvFbcCreateInfo>(), 12);
        assert_eq!(std::mem::size_of::<RrNvFbcGrabInfo>(), 36);
    }
}
