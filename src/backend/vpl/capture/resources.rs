use super::*;

#[cfg(windows)]
struct OwnedSharedHandle(windows::Win32::Foundation::HANDLE);

#[cfg(windows)]
impl OwnedSharedHandle {
    fn get(&self) -> windows::Win32::Foundation::HANDLE {
        self.0
    }

    fn into_raw(self) -> windows::Win32::Foundation::HANDLE {
        let raw = self.0;
        std::mem::forget(self);
        raw
    }
}

#[cfg(windows)]
impl Drop for OwnedSharedHandle {
    fn drop(&mut self) {
        unsafe {
            let _ = windows::Win32::Foundation::CloseHandle(self.0);
        }
    }
}

#[cfg(windows)]
struct ThreadDesktopGuard {
    previous: windows::Win32::System::StationsAndDesktops::HDESK,
    input: windows::Win32::System::StationsAndDesktops::HDESK,
}

#[cfg(windows)]
impl ThreadDesktopGuard {
    unsafe fn bind_to_input() -> Result<Self, BackendError> {
        use windows::Win32::System::StationsAndDesktops::{
            DESKTOP_ACCESS_FLAGS, DESKTOP_CONTROL_FLAGS, DESKTOP_READOBJECTS, DESKTOP_WRITEOBJECTS,
            GetThreadDesktop, OpenInputDesktop, SetThreadDesktop,
        };
        use windows::Win32::System::Threading::GetCurrentThreadId;

        let previous =
            GetThreadDesktop(GetCurrentThreadId()).map_err(|err| BackendError::WindowsApi {
                func: "GetThreadDesktop(DDA capture)",
                message: err.to_string(),
            })?;
        let input = OpenInputDesktop(
            DESKTOP_CONTROL_FLAGS(0),
            false,
            DESKTOP_ACCESS_FLAGS(DESKTOP_READOBJECTS.0 | DESKTOP_WRITEOBJECTS.0),
        )
        .map_err(|err| BackendError::WindowsApi {
            func: "OpenInputDesktop(DDA capture)",
            message: err.to_string(),
        })?;
        if let Err(err) = SetThreadDesktop(input) {
            let _ = windows::Win32::System::StationsAndDesktops::CloseDesktop(input);
            return Err(BackendError::WindowsApi {
                func: "SetThreadDesktop(DDA capture)",
                message: err.to_string(),
            });
        }
        Ok(Self { previous, input })
    }
}

#[cfg(windows)]
impl Drop for ThreadDesktopGuard {
    fn drop(&mut self) {
        unsafe {
            let _ = windows::Win32::System::StationsAndDesktops::SetThreadDesktop(self.previous);
            let _ = windows::Win32::System::StationsAndDesktops::CloseDesktop(self.input);
        }
    }
}

#[cfg(windows)]
unsafe fn duplicate_output1_with_retry(
    output: &windows::Win32::Graphics::Dxgi::IDXGIOutput5,
    device: &windows::Win32::Graphics::Direct3D11::ID3D11Device,
    supported_formats: &[windows::Win32::Graphics::Dxgi::Common::DXGI_FORMAT],
) -> windows::core::Result<windows::Win32::Graphics::Dxgi::IDXGIOutputDuplication> {
    let mut last_error = None;
    for attempt in 0..2 {
        let _desktop = ThreadDesktopGuard::bind_to_input().map_err(|err| {
            windows::core::Error::new(
                windows::core::HRESULT(0x80004005u32 as i32),
                err.to_string(),
            )
        })?;
        match output.DuplicateOutput1(device, 0, supported_formats) {
            Ok(duplication) => return Ok(duplication),
            Err(err) => last_error = Some(err),
        }
        if attempt == 0 {
            std::thread::sleep(std::time::Duration::from_millis(200));
        }
    }
    Err(last_error.expect("DuplicateOutput1 retry loop always records an error"))
}

#[cfg(windows)]
pub(in super::super) unsafe fn create_duplication_on_device(
    adapter1: &windows::Win32::Graphics::Dxgi::IDXGIAdapter1,
    output_index: u32,
    device: &windows::Win32::Graphics::Direct3D11::ID3D11Device,
    route: VplRecordRoute,
) -> Result<windows::Win32::Graphics::Dxgi::IDXGIOutputDuplication, BackendError> {
    use windows::Win32::Graphics::Dxgi::Common::{
        DXGI_FORMAT_B8G8R8A8_UNORM, DXGI_FORMAT_B8G8R8X8_UNORM, DXGI_FORMAT_R8G8B8A8_UNORM,
        DXGI_FORMAT_R16G16B16A16_FLOAT,
    };
    use windows::Win32::Graphics::Dxgi::{IDXGIOutput1, IDXGIOutput5};
    use windows::core::Interface;

    let output = adapter1
        .EnumOutputs(output_index)
        .map_err(|err| BackendError::WindowsApi {
            func: "IDXGIAdapter1::EnumOutputs(DDA target)",
            message: err.to_string(),
        })?;
    let output1: IDXGIOutput1 = output.cast().map_err(|err| BackendError::WindowsApi {
        func: "IDXGIOutput::cast<IDXGIOutput1>",
        message: err.to_string(),
    })?;
    if let Ok(output5) = output.cast::<IDXGIOutput5>() {
        if route.requires_fp16_capture() {
            // DuplicateOutput1 expects the caller's complete acceptable scan-out
            // format list. Keep FP16 first, but include the mandatory 8-bit formats
            // so drivers that validate the list as a whole can still create the
            // duplication. The capture loop verifies the actual returned format and
            // rejects 8-bit output for this route, so this cannot silently downgrade
            // HDR/10-bit capture.
            let supported_formats = [
                DXGI_FORMAT_R16G16B16A16_FLOAT,
                DXGI_FORMAT_B8G8R8A8_UNORM,
                DXGI_FORMAT_B8G8R8X8_UNORM,
                DXGI_FORMAT_R8G8B8A8_UNORM,
            ];
            match duplicate_output1_with_retry(&output5, device, &supported_formats) {
                Ok(duplication) => return Ok(duplication),
                Err(err) => {
                    return Err(BackendError::unsupported(
                        "DDA capture",
                        format!("{} DuplicateOutput1 FP16 失败: {err}", route.summary()),
                        "不支持的桌面模式",
                    ));
                }
            }
        } else {
            let supported_formats = [
                // SDR8 route keeps the desktop in ordinary 8-bit RGB before
                // the SDR BT.709 GPU conversion. FP16 stays last as a diagnostic
                // fallback rather than the preferred output for an 8-bit route.
                DXGI_FORMAT_B8G8R8A8_UNORM,
                DXGI_FORMAT_B8G8R8X8_UNORM,
                DXGI_FORMAT_R8G8B8A8_UNORM,
                DXGI_FORMAT_R16G16B16A16_FLOAT,
            ];
            match duplicate_output1_with_retry(&output5, device, &supported_formats) {
                Ok(duplication) => return Ok(duplication),
                Err(err) => {
                    // 某些 WDDM/驱动组合能枚举 IDXGIOutput5，但 DuplicateOutput1
                    // 对格式列表返回 UNSUPPORTED。只有 SDR8 路线可退回普通 DDA。
                    let _ = err;
                }
            }
        }
    }

    if route.requires_fp16_capture() {
        return Err(BackendError::unsupported(
            "DDA capture",
            route.summary(),
            "不支持的桌面模式",
        ));
    }

    output1
        .DuplicateOutput(device)
        .map_err(|err| BackendError::WindowsApi {
            func: "IDXGIOutput1::DuplicateOutput(oneVPL device)",
            message: err.to_string(),
        })
}

#[cfg(windows)]
pub(in super::super) unsafe fn create_route_intermediate(
    device: &windows::Win32::Graphics::Direct3D11::ID3D11Device,
    target_desc: &windows::Win32::Graphics::Direct3D11::D3D11_TEXTURE2D_DESC,
    route: VplRecordRoute,
    allow_uav: bool,
) -> Result<windows::Win32::Graphics::Direct3D11::ID3D11Texture2D, BackendError> {
    use windows::Win32::Graphics::Direct3D11::{
        D3D11_BIND_RENDER_TARGET, D3D11_BIND_UNORDERED_ACCESS,
        D3D11_RESOURCE_MISC_SHARED_KEYEDMUTEX, D3D11_RESOURCE_MISC_SHARED_NTHANDLE,
        D3D11_TEXTURE2D_DESC, D3D11_USAGE_DEFAULT,
    };

    let mut bind_flags = if matches!(route.fourcc, MFX_FOURCC_P010 | MFX_FOURCC_RGB4) {
        D3D11_BIND_RENDER_TARGET.0 as u32
    } else {
        0
    };
    if allow_uav {
        bind_flags |= D3D11_BIND_UNORDERED_ACCESS.0 as u32;
    }
    let misc_flags = if route.is_nvenc_cuda_planar() {
        (D3D11_RESOURCE_MISC_SHARED_NTHANDLE.0 | D3D11_RESOURCE_MISC_SHARED_KEYEDMUTEX.0) as u32
    } else {
        0
    };
    let desc = D3D11_TEXTURE2D_DESC {
        Width: target_desc.Width,
        Height: target_desc.Height,
        MipLevels: 1,
        ArraySize: 1,
        Format: route.try_dxgi_format()?,
        SampleDesc: target_desc.SampleDesc,
        Usage: D3D11_USAGE_DEFAULT,
        BindFlags: bind_flags,
        CPUAccessFlags: 0,
        MiscFlags: misc_flags,
    };
    let mut texture = None;
    device
        .CreateTexture2D(&desc, None, Some(&mut texture))
        .map_err(|err| BackendError::WindowsApi {
            func: "ID3D11Device::CreateTexture2D(route intermediate)",
            message: err.to_string(),
        })?;
    texture.ok_or_else(|| BackendError::WindowsApi {
        func: "CreateTexture2D(route intermediate)",
        message: "返回空纹理".to_owned(),
    })
}

#[cfg(windows)]
pub(in super::super) unsafe fn create_dda_snapshot_texture(
    device: &windows::Win32::Graphics::Direct3D11::ID3D11Device,
    source_desc: &windows::Win32::Graphics::Direct3D11::D3D11_TEXTURE2D_DESC,
) -> Result<windows::Win32::Graphics::Direct3D11::ID3D11Texture2D, BackendError> {
    use windows::Win32::Graphics::Direct3D11::{
        D3D11_BIND_SHADER_RESOURCE, D3D11_TEXTURE2D_DESC, D3D11_USAGE_DEFAULT,
    };

    let desc = D3D11_TEXTURE2D_DESC {
        Width: source_desc.Width.max(1),
        Height: source_desc.Height.max(1),
        MipLevels: 1,
        ArraySize: 1,
        Format: source_desc.Format,
        SampleDesc: source_desc.SampleDesc,
        Usage: D3D11_USAGE_DEFAULT,
        BindFlags: D3D11_BIND_SHADER_RESOURCE.0 as u32,
        CPUAccessFlags: 0,
        MiscFlags: 0,
    };
    let mut texture = None;
    device
        .CreateTexture2D(&desc, None, Some(&mut texture))
        .map_err(|err| BackendError::WindowsApi {
            func: "ID3D11Device::CreateTexture2D(DDA snapshot)",
            message: err.to_string(),
        })?;
    texture.ok_or_else(|| BackendError::WindowsApi {
        func: "CreateTexture2D(DDA snapshot)",
        message: "返回空纹理".to_owned(),
    })
}

#[cfg(windows)]
pub(in super::super) unsafe fn create_d3d11_device_for_adapter(
    adapter1: &windows::Win32::Graphics::Dxgi::IDXGIAdapter1,
) -> Result<
    (
        windows::Win32::Graphics::Direct3D11::ID3D11Device,
        windows::Win32::Graphics::Direct3D11::ID3D11DeviceContext,
    ),
    BackendError,
> {
    use windows::Win32::Foundation::HMODULE;
    use windows::Win32::Graphics::Direct3D::{
        D3D_DRIVER_TYPE_UNKNOWN, D3D_FEATURE_LEVEL, D3D_FEATURE_LEVEL_11_0, D3D_FEATURE_LEVEL_11_1,
    };
    use windows::Win32::Graphics::Direct3D11::{
        D3D11_CREATE_DEVICE_BGRA_SUPPORT, D3D11_CREATE_DEVICE_VIDEO_SUPPORT, D3D11_SDK_VERSION,
        D3D11CreateDevice, ID3D11Device, ID3D11DeviceContext,
    };
    use windows::Win32::Graphics::Dxgi::IDXGIAdapter;
    use windows::core::Interface;

    let adapter: IDXGIAdapter = adapter1.cast().map_err(|err| BackendError::WindowsApi {
        func: "IDXGIAdapter1::cast<IDXGIAdapter>(capture device)",
        message: err.to_string(),
    })?;
    let mut device: Option<ID3D11Device> = None;
    let mut context: Option<ID3D11DeviceContext> = None;
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
        Some(&mut context),
    )
    .map_err(|err| BackendError::WindowsApi {
        func: "D3D11CreateDevice(capture)",
        message: err.to_string(),
    })?;
    let device = device.ok_or_else(|| BackendError::WindowsApi {
        func: "D3D11CreateDevice(capture)",
        message: "返回空 ID3D11Device".to_owned(),
    })?;
    let context = context.ok_or_else(|| BackendError::WindowsApi {
        func: "D3D11CreateDevice(capture)",
        message: "返回空 ID3D11DeviceContext".to_owned(),
    })?;
    Ok((device, context))
}

#[cfg(windows)]
pub(in super::super) unsafe fn create_shared_snapshot_slot(
    id: usize,
    capture_device: &windows::Win32::Graphics::Direct3D11::ID3D11Device,
    encoder_device: &windows::Win32::Graphics::Direct3D11::ID3D11Device,
    source_desc: &windows::Win32::Graphics::Direct3D11::D3D11_TEXTURE2D_DESC,
) -> Result<SnapshotSlot, BackendError> {
    use windows::Win32::Graphics::Direct3D11::{
        D3D11_BIND_SHADER_RESOURCE, D3D11_RESOURCE_MISC_SHARED_KEYEDMUTEX,
        D3D11_RESOURCE_MISC_SHARED_NTHANDLE, D3D11_TEXTURE2D_DESC, D3D11_USAGE_DEFAULT,
        ID3D11Device1, ID3D11Texture2D,
    };
    use windows::Win32::Graphics::Dxgi::{
        DXGI_SHARED_RESOURCE_READ, IDXGIKeyedMutex, IDXGIResource1,
    };
    use windows::core::{Interface, PCWSTR};

    let desc = D3D11_TEXTURE2D_DESC {
        Width: source_desc.Width.max(1),
        Height: source_desc.Height.max(1),
        MipLevels: 1,
        ArraySize: 1,
        Format: source_desc.Format,
        SampleDesc: source_desc.SampleDesc,
        Usage: D3D11_USAGE_DEFAULT,
        BindFlags: D3D11_BIND_SHADER_RESOURCE.0 as u32,
        CPUAccessFlags: 0,
        MiscFlags: (D3D11_RESOURCE_MISC_SHARED_NTHANDLE.0 | D3D11_RESOURCE_MISC_SHARED_KEYEDMUTEX.0)
            as u32,
    };
    let mut capture_texture = None;
    capture_device
        .CreateTexture2D(&desc, None, Some(&mut capture_texture))
        .map_err(|err| BackendError::WindowsApi {
            func: "ID3D11Device::CreateTexture2D(shared DDA snapshot)",
            message: err.to_string(),
        })?;
    let capture_texture = capture_texture.ok_or_else(|| BackendError::WindowsApi {
        func: "CreateTexture2D(shared DDA snapshot)",
        message: "返回空 capture texture".to_owned(),
    })?;
    let capture_mutex: IDXGIKeyedMutex =
        capture_texture
            .cast()
            .map_err(|err| BackendError::WindowsApi {
                func: "ID3D11Texture2D::cast<IDXGIKeyedMutex>(capture)",
                message: err.to_string(),
            })?;
    let resource1: IDXGIResource1 =
        capture_texture
            .cast()
            .map_err(|err| BackendError::WindowsApi {
                func: "ID3D11Texture2D::cast<IDXGIResource1>(shared DDA snapshot)",
                message: err.to_string(),
            })?;
    let shared_handle = OwnedSharedHandle(
        resource1
            .CreateSharedHandle(None, DXGI_SHARED_RESOURCE_READ.0, PCWSTR::null())
            .map_err(|err| BackendError::WindowsApi {
                func: "IDXGIResource1::CreateSharedHandle(shared DDA snapshot)",
                message: err.to_string(),
            })?,
    );
    let encoder_device1: ID3D11Device1 =
        encoder_device
            .cast()
            .map_err(|err| BackendError::WindowsApi {
                func: "ID3D11Device::cast<ID3D11Device1>(encoder)",
                message: err.to_string(),
            })?;
    let encoder_texture: ID3D11Texture2D = encoder_device1
        .OpenSharedResource1(shared_handle.get())
        .map_err(|err| BackendError::WindowsApi {
            func: "ID3D11Device1::OpenSharedResource1(shared DDA snapshot)",
            message: err.to_string(),
        })?;
    let encoder_mutex: IDXGIKeyedMutex =
        encoder_texture
            .cast()
            .map_err(|err| BackendError::WindowsApi {
                func: "ID3D11Texture2D::cast<IDXGIKeyedMutex>(encoder)",
                message: err.to_string(),
            })?;
    Ok(SnapshotSlot {
        inner: std::sync::Arc::new(SnapshotSlotInner {
            id,
            capture_texture,
            encoder_texture,
            capture_mutex,
            encoder_mutex,
            encoder_fence: GpuCompletionFence::new(encoder_device)?,
            shared_handle: shared_handle.into_raw(),
        }),
    })
}

#[cfg(windows)]
#[allow(clippy::too_many_arguments)]
pub(in super::super) unsafe fn create_shared_fence_route_slot(
    id: usize,
    capture_device: &windows::Win32::Graphics::Direct3D11::ID3D11Device,
    capture_context: &windows::Win32::Graphics::Direct3D11::ID3D11DeviceContext,
    encoder_device: &windows::Win32::Graphics::Direct3D11::ID3D11Device,
    target_desc: &windows::Win32::Graphics::Direct3D11::D3D11_TEXTURE2D_DESC,
    route: VplRecordRoute,
    input_width: u32,
    input_height: u32,
    source_srv_cache: std::sync::Arc<std::sync::Mutex<ShaderResourceViewCache>>,
) -> Result<SharedFenceSlot, BackendError> {
    use windows::Win32::Foundation::GENERIC_ALL;
    use windows::Win32::Graphics::Direct3D11::{
        D3D11_BIND_RENDER_TARGET, D3D11_BIND_UNORDERED_ACCESS, D3D11_FENCE_FLAG_SHARED,
        D3D11_RESOURCE_MISC_SHARED, D3D11_TEXTURE2D_DESC, D3D11_USAGE_DEFAULT, ID3D11Device5,
        ID3D11Fence, ID3D11Texture2D,
    };
    use windows::Win32::Graphics::Dxgi::IDXGIResource;
    use windows::core::{Interface, PCWSTR};

    let use_render_target = matches!(route.fourcc, MFX_FOURCC_P010 | MFX_FOURCC_RGB4);
    let bind_flags = if use_render_target {
        D3D11_BIND_RENDER_TARGET.0 as u32
    } else {
        D3D11_BIND_UNORDERED_ACCESS.0 as u32
    };
    let desc = D3D11_TEXTURE2D_DESC {
        Width: target_desc.Width.max(1),
        Height: target_desc.Height.max(1),
        MipLevels: 1,
        ArraySize: 1,
        Format: target_desc.Format,
        SampleDesc: target_desc.SampleDesc,
        Usage: D3D11_USAGE_DEFAULT,
        BindFlags: bind_flags,
        CPUAccessFlags: 0,
        MiscFlags: D3D11_RESOURCE_MISC_SHARED.0 as u32,
    };
    let mut capture_texture = None;
    capture_device
        .CreateTexture2D(&desc, None, Some(&mut capture_texture))
        .map_err(|err| BackendError::WindowsApi {
            func: "ID3D11Device::CreateTexture2D(shared-fence DDA route)",
            message: err.to_string(),
        })?;
    let capture_texture = capture_texture.ok_or_else(|| BackendError::WindowsApi {
        func: "CreateTexture2D(shared-fence DDA route)",
        message: "返回空 capture texture".to_owned(),
    })?;
    let resource: IDXGIResource =
        capture_texture
            .cast()
            .map_err(|err| BackendError::WindowsApi {
                func: "ID3D11Texture2D::cast<IDXGIResource>(shared-fence DDA route)",
                message: err.to_string(),
            })?;
    let texture_handle = resource
        .GetSharedHandle()
        .map_err(|err| BackendError::WindowsApi {
            func: "IDXGIResource::GetSharedHandle(shared-fence DDA route)",
            message: err.to_string(),
        })?;
    let mut encoder_texture: Option<ID3D11Texture2D> = None;
    encoder_device
        .OpenSharedResource(texture_handle, &mut encoder_texture)
        .map_err(|err| BackendError::WindowsApi {
            func: "ID3D11Device::OpenSharedResource(shared-fence DDA route)",
            message: err.to_string(),
        })?;
    let encoder_texture = encoder_texture.ok_or_else(|| BackendError::WindowsApi {
        func: "ID3D11Device::OpenSharedResource(shared-fence DDA route)",
        message: "返回空 encoder texture".to_owned(),
    })?;

    let capture_device5: ID3D11Device5 =
        capture_device
            .cast()
            .map_err(|err| BackendError::WindowsApi {
                func: "ID3D11Device::cast<ID3D11Device5>(shared-fence DDA capture)",
                message: err.to_string(),
            })?;
    let mut capture_fence: Option<ID3D11Fence> = None;
    capture_device5
        .CreateFence(0, D3D11_FENCE_FLAG_SHARED, &mut capture_fence)
        .map_err(|err| BackendError::WindowsApi {
            func: "ID3D11Device5::CreateFence(shared-fence DDA route)",
            message: err.to_string(),
        })?;
    let capture_fence = capture_fence.ok_or_else(|| BackendError::WindowsApi {
        func: "ID3D11Device5::CreateFence(shared-fence DDA route)",
        message: "返回空 capture fence".to_owned(),
    })?;
    let fence_handle = OwnedSharedHandle(
        capture_fence
            .CreateSharedHandle(None, GENERIC_ALL.0, PCWSTR::null())
            .map_err(|err| BackendError::WindowsApi {
                func: "ID3D11Fence::CreateSharedHandle(shared-fence DDA route)",
                message: err.to_string(),
            })?,
    );
    let encoder_device5: ID3D11Device5 =
        encoder_device
            .cast()
            .map_err(|err| BackendError::WindowsApi {
                func: "ID3D11Device::cast<ID3D11Device5>(shared-fence DDA encoder)",
                message: err.to_string(),
            })?;
    let mut encoder_fence: Option<ID3D11Fence> = None;
    encoder_device5
        .OpenSharedFence(fence_handle.get(), &mut encoder_fence)
        .map_err(|err| BackendError::WindowsApi {
            func: "ID3D11Device5::OpenSharedFence(shared-fence DDA route)",
            message: err.to_string(),
        })?;
    let encoder_fence = encoder_fence.ok_or_else(|| BackendError::WindowsApi {
        func: "ID3D11Device5::OpenSharedFence(shared-fence DDA route)",
        message: "返回空 encoder fence".to_owned(),
    })?;
    let converter = GpuRecordConverter::new_with_source_cache(
        route,
        capture_device,
        capture_context,
        &capture_texture,
        input_width,
        input_height,
        !use_render_target,
        source_srv_cache,
    )?;

    Ok(SharedFenceSlot {
        id,
        capture_texture,
        encoder_texture,
        converter,
        capture_fence,
        encoder_fence,
        fence_value: 0,
        fence_shared_handle: fence_handle.into_raw(),
    })
}
