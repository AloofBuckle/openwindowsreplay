use super::*;

#[cfg(windows)]
pub(in super::super) unsafe fn create_duplication_on_device(
    adapter1: &windows::Win32::Graphics::Dxgi::IDXGIAdapter1,
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
        .EnumOutputs(0)
        .map_err(|err| BackendError::WindowsApi {
            func: "IDXGIAdapter1::EnumOutputs(0)",
            message: err.to_string(),
        })?;
    let output1: IDXGIOutput1 = output.cast().map_err(|err| BackendError::WindowsApi {
        func: "IDXGIOutput::cast<IDXGIOutput1>",
        message: err.to_string(),
    })?;
    if let Ok(output5) = output.cast::<IDXGIOutput5>() {
        if route.requires_fp16_capture() {
            // 10-bit/HDR 路线不能退到 BGRA8：那会在 capture 阶段丢失源位深，
            // 与“源是什么位深，输出就是什么位深”的后端契约冲突。
            let supported_formats = [DXGI_FORMAT_R16G16B16A16_FLOAT];
            match output5.DuplicateOutput1(device, 0, &supported_formats) {
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
            match output5.DuplicateOutput1(device, 0, &supported_formats) {
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
        D3D11_BIND_RENDER_TARGET, D3D11_BIND_UNORDERED_ACCESS, D3D11_TEXTURE2D_DESC,
        D3D11_USAGE_DEFAULT,
    };

    let mut bind_flags = if matches!(route.fourcc, MFX_FOURCC_P010 | MFX_FOURCC_RGB4) {
        D3D11_BIND_RENDER_TARGET.0 as u32
    } else {
        0
    };
    if allow_uav {
        bind_flags |= D3D11_BIND_UNORDERED_ACCESS.0 as u32;
    }
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
        MiscFlags: 0,
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
    let shared_handle = resource1
        .CreateSharedHandle(None, DXGI_SHARED_RESOURCE_READ.0, PCWSTR::null())
        .map_err(|err| BackendError::WindowsApi {
            func: "IDXGIResource1::CreateSharedHandle(shared DDA snapshot)",
            message: err.to_string(),
        })?;
    let encoder_device1: ID3D11Device1 =
        encoder_device
            .cast()
            .map_err(|err| BackendError::WindowsApi {
                func: "ID3D11Device::cast<ID3D11Device1>(encoder)",
                message: err.to_string(),
            })?;
    let encoder_texture: ID3D11Texture2D = encoder_device1
        .OpenSharedResource1(shared_handle)
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
            shared_handle,
        }),
    })
}
