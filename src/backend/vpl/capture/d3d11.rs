use super::*;

#[cfg(windows)]
pub(in super::super) struct KeyedMutexGuard<'a> {
    mutex: &'a windows::Win32::Graphics::Dxgi::IDXGIKeyedMutex,
    release_key: u64,
    held: bool,
}

#[cfg(windows)]
impl<'a> KeyedMutexGuard<'a> {
    pub(in super::super) fn acquire(
        mutex: &'a windows::Win32::Graphics::Dxgi::IDXGIKeyedMutex,
        acquire_key: u64,
        release_key: u64,
        timeout_ms: u32,
        func: &'static str,
    ) -> Result<Self, BackendError> {
        use windows::core::Interface;

        let status = unsafe {
            (Interface::vtable(mutex).AcquireSync)(
                Interface::as_raw(mutex),
                acquire_key,
                timeout_ms,
            )
        };
        match status.0 as u32 {
            0 => Ok(Self {
                mutex,
                release_key,
                held: true,
            }),
            0x0000_0102 => Err(BackendError::WindowsApi {
                func,
                message: format!(
                    "keyed mutex wait timed out after {timeout_ms}ms; resource ownership was not acquired"
                ),
            }),
            0x0000_0080 => Err(BackendError::WindowsApi {
                func,
                message: "keyed mutex was abandoned; shared resource state is invalid".to_owned(),
            }),
            _ => Err(BackendError::WindowsApi {
                func,
                message: windows::core::Error::from_hresult(status).to_string(),
            }),
        }
    }

    pub(in super::super) fn release(mut self, func: &'static str) -> Result<(), BackendError> {
        let status = self.release_raw();
        if status.0 == 0 {
            Ok(())
        } else {
            Err(BackendError::WindowsApi {
                func,
                message: windows::core::Error::from_hresult(status).to_string(),
            })
        }
    }

    fn release_raw(&mut self) -> windows::core::HRESULT {
        use windows::core::Interface;

        if !self.held {
            return windows::core::HRESULT(0);
        }
        self.held = false;
        unsafe {
            (Interface::vtable(self.mutex).ReleaseSync)(
                Interface::as_raw(self.mutex),
                self.release_key,
            )
        }
    }
}

#[cfg(windows)]
impl Drop for KeyedMutexGuard<'_> {
    fn drop(&mut self) {
        let _ = self.release_raw();
    }
}

#[cfg(windows)]
pub(in super::super) struct D3d11MultithreadGuard<'a> {
    pub(in super::super) mt: Option<&'a windows::Win32::Graphics::Direct3D11::ID3D11Multithread>,
}

#[cfg(windows)]
impl<'a> D3d11MultithreadGuard<'a> {
    pub(in super::super) unsafe fn enter(
        mt: &'a Option<windows::Win32::Graphics::Direct3D11::ID3D11Multithread>,
    ) -> Self {
        if let Some(mt) = mt {
            mt.Enter();
            Self { mt: Some(mt) }
        } else {
            Self { mt: None }
        }
    }
}

#[cfg(windows)]
impl Drop for D3d11MultithreadGuard<'_> {
    fn drop(&mut self) {
        unsafe {
            if let Some(mt) = self.mt {
                mt.Leave();
            }
        }
    }
}

#[cfg(windows)]
pub(in super::super) unsafe fn copy_texture_resource(
    context: &windows::Win32::Graphics::Direct3D11::ID3D11DeviceContext,
    source: &windows::Win32::Graphics::Direct3D11::ID3D11Texture2D,
    target: &windows::Win32::Graphics::Direct3D11::ID3D11Texture2D,
) -> Result<(), BackendError> {
    use windows::Win32::Graphics::Direct3D11::ID3D11Resource;
    use windows::core::Interface;

    let src_resource: ID3D11Resource = source.cast().map_err(|err| BackendError::WindowsApi {
        func: "ID3D11Texture2D::cast<ID3D11Resource>(copy source)",
        message: err.to_string(),
    })?;
    let dst_resource: ID3D11Resource = target.cast().map_err(|err| BackendError::WindowsApi {
        func: "ID3D11Texture2D::cast<ID3D11Resource>(copy target)",
        message: err.to_string(),
    })?;
    context.CopyResource(&dst_resource, &src_resource);
    Ok(())
}

#[cfg(windows)]
pub(in super::super) unsafe fn copy_texture_subresource_region(
    context: &windows::Win32::Graphics::Direct3D11::ID3D11DeviceContext,
    source: &windows::Win32::Graphics::Direct3D11::ID3D11Texture2D,
    target: &windows::Win32::Graphics::Direct3D11::ID3D11Texture2D,
    width: u32,
    height: u32,
) -> Result<(), BackendError> {
    use windows::Win32::Graphics::Direct3D11::{D3D11_BOX, ID3D11Resource};
    use windows::core::Interface;

    let src_resource: ID3D11Resource = source.cast().map_err(|err| BackendError::WindowsApi {
        func: "ID3D11Texture2D::cast<ID3D11Resource>(region copy source)",
        message: err.to_string(),
    })?;
    let dst_resource: ID3D11Resource = target.cast().map_err(|err| BackendError::WindowsApi {
        func: "ID3D11Texture2D::cast<ID3D11Resource>(region copy target)",
        message: err.to_string(),
    })?;
    let src_box = D3D11_BOX {
        left: 0,
        top: 0,
        front: 0,
        right: width.max(1),
        bottom: height.max(1),
        back: 1,
    };
    context.CopySubresourceRegion(&dst_resource, 0, 0, 0, 0, &src_resource, 0, Some(&src_box));
    Ok(())
}

#[cfg(windows)]
pub(in super::super) struct CachedFallbackShaderResourceView {
    texture: windows::Win32::Graphics::Direct3D11::ID3D11Texture2D,
    srv: windows::Win32::Graphics::Direct3D11::ID3D11ShaderResourceView,
}

#[cfg(windows)]
pub(in super::super) struct ShaderResourceViewCache {
    pub(in super::super) direct:
        HashMap<usize, windows::Win32::Graphics::Direct3D11::ID3D11ShaderResourceView>,
    pub(in super::super) fallback: HashMap<usize, CachedFallbackShaderResourceView>,
    pub(in super::super) retain_views: bool,
}

#[cfg(windows)]
impl ShaderResourceViewCache {
    pub(in super::super) fn retained() -> Self {
        Self {
            direct: HashMap::new(),
            fallback: HashMap::new(),
            retain_views: true,
        }
    }

    pub(in super::super) fn transient() -> Self {
        Self {
            direct: HashMap::new(),
            fallback: HashMap::new(),
            retain_views: false,
        }
    }

    pub(in super::super) unsafe fn get_or_create(
        &mut self,
        device: &windows::Win32::Graphics::Direct3D11::ID3D11Device,
        context: &windows::Win32::Graphics::Direct3D11::ID3D11DeviceContext,
        source: &windows::Win32::Graphics::Direct3D11::ID3D11Texture2D,
        label: &'static str,
    ) -> Result<windows::Win32::Graphics::Direct3D11::ID3D11ShaderResourceView, BackendError> {
        if !self.retain_views {
            return create_shader_resource_view_with_gpu_copy_fallback(
                device, context, source, label,
            );
        }
        let key = windows::core::Interface::as_raw(source) as usize;
        if let Some(srv) = self.direct.get(&key) {
            return Ok(srv.clone());
        }
        if let Some(cached) = self.fallback.get(&key) {
            copy_texture_resource(context, source, &cached.texture)?;
            return Ok(cached.srv.clone());
        }
        if let Ok(srv) = create_direct_shader_resource_view(device, source, label) {
            if self.direct.len() + self.fallback.len() >= 64 {
                self.direct.clear();
                self.fallback.clear();
            }
            self.direct.insert(key, srv.clone());
            return Ok(srv);
        }
        let (texture, srv) =
            create_gpu_copy_fallback_shader_resource_view(device, context, source, label)?;
        if self.direct.len() + self.fallback.len() >= 64 {
            self.direct.clear();
            self.fallback.clear();
        }
        self.fallback.insert(
            key,
            CachedFallbackShaderResourceView {
                texture,
                srv: srv.clone(),
            },
        );
        Ok(srv)
    }
}

#[cfg(windows)]
pub(in super::super) fn lock_srv_cache(
    cache: &std::sync::Mutex<ShaderResourceViewCache>,
) -> Result<std::sync::MutexGuard<'_, ShaderResourceViewCache>, BackendError> {
    cache
        .lock()
        .map_err(|_| BackendError::unsupported("D3D11 SRV cache", "mutex", "SRV cache 锁已中毒"))
}

#[cfg(windows)]
pub(in super::super) unsafe fn create_direct_shader_resource_view(
    device: &windows::Win32::Graphics::Direct3D11::ID3D11Device,
    source: &windows::Win32::Graphics::Direct3D11::ID3D11Texture2D,
    label: &'static str,
) -> Result<windows::Win32::Graphics::Direct3D11::ID3D11ShaderResourceView, BackendError> {
    use windows::Win32::Graphics::Direct3D::D3D11_SRV_DIMENSION_TEXTURE2D;
    use windows::Win32::Graphics::Direct3D11::{
        D3D11_SHADER_RESOURCE_VIEW_DESC, D3D11_SHADER_RESOURCE_VIEW_DESC_0, D3D11_TEX2D_SRV,
        D3D11_TEXTURE2D_DESC, ID3D11Resource,
    };
    use windows::core::Interface;

    let source_resource: ID3D11Resource =
        source.cast().map_err(|err| BackendError::WindowsApi {
            func: "ID3D11Texture2D::cast<ID3D11Resource>(SRV source)",
            message: err.to_string(),
        })?;
    let mut srv = None;
    if device
        .CreateShaderResourceView(&source_resource, None, Some(&mut srv))
        .is_ok()
    {
        return srv.ok_or_else(|| BackendError::WindowsApi {
            func: label,
            message: "CreateShaderResourceView 返回空 SRV".to_owned(),
        });
    }

    let mut desc = D3D11_TEXTURE2D_DESC::default();
    source.GetDesc(&mut desc);
    let explicit_srv_desc = D3D11_SHADER_RESOURCE_VIEW_DESC {
        Format: desc.Format,
        ViewDimension: D3D11_SRV_DIMENSION_TEXTURE2D,
        Anonymous: D3D11_SHADER_RESOURCE_VIEW_DESC_0 {
            Texture2D: D3D11_TEX2D_SRV {
                MostDetailedMip: 0,
                MipLevels: 1,
            },
        },
    };
    let mut explicit_srv = None;
    device
        .CreateShaderResourceView(
            &source_resource,
            Some(&explicit_srv_desc),
            Some(&mut explicit_srv),
        )
        .map_err(|err| BackendError::WindowsApi {
            func: label,
            message: format!(
                "CreateShaderResourceView direct failed: {err}; format={}, bind_flags=0x{:X}",
                desc.Format.0, desc.BindFlags
            ),
        })?;
    explicit_srv.ok_or_else(|| BackendError::WindowsApi {
        func: label,
        message: format!(
            "显式 SRV desc 返回空 SRV，format={}, bind_flags=0x{:X}",
            desc.Format.0, desc.BindFlags
        ),
    })
}

#[cfg(windows)]
pub(in super::super) unsafe fn create_shader_resource_view_with_gpu_copy_fallback(
    device: &windows::Win32::Graphics::Direct3D11::ID3D11Device,
    context: &windows::Win32::Graphics::Direct3D11::ID3D11DeviceContext,
    source: &windows::Win32::Graphics::Direct3D11::ID3D11Texture2D,
    label: &'static str,
) -> Result<windows::Win32::Graphics::Direct3D11::ID3D11ShaderResourceView, BackendError> {
    if let Ok(srv) = create_direct_shader_resource_view(device, source, label) {
        return Ok(srv);
    }

    create_gpu_copy_fallback_shader_resource_view(device, context, source, label)
        .map(|(_, srv)| srv)
}

#[cfg(windows)]
unsafe fn create_gpu_copy_fallback_shader_resource_view(
    device: &windows::Win32::Graphics::Direct3D11::ID3D11Device,
    context: &windows::Win32::Graphics::Direct3D11::ID3D11DeviceContext,
    source: &windows::Win32::Graphics::Direct3D11::ID3D11Texture2D,
    label: &'static str,
) -> Result<
    (
        windows::Win32::Graphics::Direct3D11::ID3D11Texture2D,
        windows::Win32::Graphics::Direct3D11::ID3D11ShaderResourceView,
    ),
    BackendError,
> {
    use windows::Win32::Graphics::Direct3D::D3D11_SRV_DIMENSION_TEXTURE2D;
    use windows::Win32::Graphics::Direct3D11::{
        D3D11_BIND_SHADER_RESOURCE, D3D11_SHADER_RESOURCE_VIEW_DESC,
        D3D11_SHADER_RESOURCE_VIEW_DESC_0, D3D11_TEX2D_SRV, D3D11_TEXTURE2D_DESC,
        D3D11_USAGE_DEFAULT, ID3D11Resource,
    };
    use windows::core::Interface;

    let mut desc = D3D11_TEXTURE2D_DESC::default();
    source.GetDesc(&mut desc);
    let copy_desc = D3D11_TEXTURE2D_DESC {
        Width: desc.Width.max(1),
        Height: desc.Height.max(1),
        MipLevels: 1,
        ArraySize: 1,
        Format: desc.Format,
        SampleDesc: desc.SampleDesc,
        Usage: D3D11_USAGE_DEFAULT,
        BindFlags: D3D11_BIND_SHADER_RESOURCE.0 as u32,
        CPUAccessFlags: 0,
        MiscFlags: 0,
    };
    let mut shader_readable = None;
    device
        .CreateTexture2D(&copy_desc, None, Some(&mut shader_readable))
        .map_err(|err| BackendError::WindowsApi {
            func: "ID3D11Device::CreateTexture2D(SRV fallback copy)",
            message: err.to_string(),
        })?;
    let shader_readable = shader_readable.ok_or_else(|| BackendError::WindowsApi {
        func: "CreateTexture2D(SRV fallback copy)",
        message: "返回空纹理".to_owned(),
    })?;
    copy_texture_resource(context, source, &shader_readable)?;
    let shader_resource: ID3D11Resource =
        shader_readable
            .cast()
            .map_err(|err| BackendError::WindowsApi {
                func: "ID3D11Texture2D::cast<ID3D11Resource>(SRV fallback copy)",
                message: err.to_string(),
            })?;
    let mut fallback_srv = None;
    let fallback_srv_desc = D3D11_SHADER_RESOURCE_VIEW_DESC {
        Format: copy_desc.Format,
        ViewDimension: D3D11_SRV_DIMENSION_TEXTURE2D,
        Anonymous: D3D11_SHADER_RESOURCE_VIEW_DESC_0 {
            Texture2D: D3D11_TEX2D_SRV {
                MostDetailedMip: 0,
                MipLevels: 1,
            },
        },
    };
    device
        .CreateShaderResourceView(&shader_resource, Some(&fallback_srv_desc), Some(&mut fallback_srv))
        .map_err(|err| BackendError::WindowsApi {
            func: label,
            message: format!(
                "CreateShaderResourceView 直接/显式/一次 GPU copy fallback 均失败: {err}; source_format={}, source_bind=0x{:X}, copy_format={}",
                desc.Format.0, desc.BindFlags, copy_desc.Format.0
            ),
        })?;
    let fallback_srv = fallback_srv.ok_or_else(|| BackendError::WindowsApi {
        func: label,
        message: "fallback 返回空 SRV".to_owned(),
    })?;
    Ok((shader_readable, fallback_srv))
}

#[cfg(windows)]
pub(in super::super) unsafe fn set_d3d11_gpu_thread_priority(
    device: &windows::Win32::Graphics::Direct3D11::ID3D11Device,
    priority: i32,
) -> Result<(), BackendError> {
    use windows::Win32::Graphics::Dxgi::IDXGIDevice;
    use windows::core::Interface;

    let dxgi_device: IDXGIDevice = device.cast().map_err(|err| BackendError::WindowsApi {
        func: "ID3D11Device::cast<IDXGIDevice>(GPU priority)",
        message: err.to_string(),
    })?;
    dxgi_device
        .SetGPUThreadPriority(priority)
        .map_err(|err| BackendError::WindowsApi {
            func: "IDXGIDevice::SetGPUThreadPriority",
            message: err.to_string(),
        })
}

#[cfg(windows)]
pub(in super::super) unsafe fn d3d11_device_adapter_luid(
    device: &windows::Win32::Graphics::Direct3D11::ID3D11Device,
) -> Result<(u32, i32), BackendError> {
    use windows::Win32::Graphics::Dxgi::IDXGIDevice;
    use windows::core::Interface;

    let dxgi_device: IDXGIDevice = device.cast().map_err(|err| BackendError::WindowsApi {
        func: "ID3D11Device::cast<IDXGIDevice>(adapter LUID)",
        message: err.to_string(),
    })?;
    let adapter = dxgi_device
        .GetAdapter()
        .map_err(|err| BackendError::WindowsApi {
            func: "IDXGIDevice::GetAdapter(adapter LUID)",
            message: err.to_string(),
        })?;
    let desc = adapter.GetDesc().map_err(|err| BackendError::WindowsApi {
        func: "IDXGIAdapter::GetDesc(adapter LUID)",
        message: err.to_string(),
    })?;
    Ok((desc.AdapterLuid.LowPart, desc.AdapterLuid.HighPart))
}

#[cfg(windows)]
pub(in super::super) struct DdaFrameMetadata {
    pub(in super::super) move_rect_bytes: u32,
    pub(in super::super) dirty_rects: Vec<windows::Win32::Foundation::RECT>,
}

#[cfg(windows)]
pub(in super::super) unsafe fn read_dda_frame_metadata(
    duplication: &windows::Win32::Graphics::Dxgi::IDXGIOutputDuplication,
    total_metadata_bytes: u32,
) -> Result<DdaFrameMetadata, BackendError> {
    use windows::Win32::Graphics::Dxgi::DXGI_OUTDUPL_MOVE_RECT;

    if total_metadata_bytes == 0 {
        return Ok(DdaFrameMetadata {
            move_rect_bytes: 0,
            dirty_rects: Vec::new(),
        });
    }

    let move_capacity =
        (total_metadata_bytes as usize / std::mem::size_of::<DXGI_OUTDUPL_MOVE_RECT>()).max(1);
    let mut move_rects = vec![DXGI_OUTDUPL_MOVE_RECT::default(); move_capacity];
    let mut move_rect_bytes = 0u32;
    duplication
        .GetFrameMoveRects(
            total_metadata_bytes,
            move_rects.as_mut_ptr(),
            &mut move_rect_bytes,
        )
        .map_err(|err| BackendError::WindowsApi {
            func: "IDXGIOutputDuplication::GetFrameMoveRects",
            message: err.to_string(),
        })?;

    let dirty_capacity = (total_metadata_bytes as usize
        / std::mem::size_of::<windows::Win32::Foundation::RECT>())
    .max(1);
    let mut dirty_rects = vec![windows::Win32::Foundation::RECT::default(); dirty_capacity];
    let mut dirty_rect_bytes = 0u32;
    duplication
        .GetFrameDirtyRects(
            total_metadata_bytes,
            dirty_rects.as_mut_ptr(),
            &mut dirty_rect_bytes,
        )
        .map_err(|err| BackendError::WindowsApi {
            func: "IDXGIOutputDuplication::GetFrameDirtyRects",
            message: err.to_string(),
        })?;
    let dirty_len = (dirty_rect_bytes as usize
        / std::mem::size_of::<windows::Win32::Foundation::RECT>())
    .min(dirty_rects.len());
    dirty_rects.truncate(dirty_len);

    Ok(DdaFrameMetadata {
        move_rect_bytes,
        dirty_rects,
    })
}

#[cfg(windows)]
pub(in super::super) fn dirty_rect_area(rects: &[windows::Win32::Foundation::RECT]) -> u64 {
    rects
        .iter()
        .map(|rect| {
            let width = (rect.right - rect.left).max(0) as u64;
            let height = (rect.bottom - rect.top).max(0) as u64;
            width * height
        })
        .sum()
}

#[cfg(windows)]
pub(in super::super) struct GpuCompletionFence {
    pub(in super::super) asynchronous: windows::Win32::Graphics::Direct3D11::ID3D11Asynchronous,
}

#[cfg(windows)]
impl GpuCompletionFence {
    pub(in super::super) unsafe fn new(
        device: &windows::Win32::Graphics::Direct3D11::ID3D11Device,
    ) -> Result<Self, BackendError> {
        use windows::Win32::Graphics::Direct3D11::{
            D3D11_QUERY_DESC, D3D11_QUERY_EVENT, ID3D11Asynchronous, ID3D11Query,
        };
        use windows::core::Interface;

        let desc = D3D11_QUERY_DESC {
            Query: D3D11_QUERY_EVENT,
            MiscFlags: 0,
        };
        let mut query: Option<ID3D11Query> = None;
        device
            .CreateQuery(&desc, Some(&mut query))
            .map_err(|err| BackendError::WindowsApi {
                func: "ID3D11Device::CreateQuery(DDA source fence)",
                message: err.to_string(),
            })?;
        let asynchronous: ID3D11Asynchronous = query
            .ok_or_else(|| BackendError::WindowsApi {
                func: "CreateQuery(DDA source fence)",
                message: "返回空 query".to_owned(),
            })?
            .cast()
            .map_err(|err| BackendError::WindowsApi {
                func: "ID3D11Query::cast<ID3D11Asynchronous>",
                message: err.to_string(),
            })?;
        Ok(Self { asynchronous })
    }

    pub(in super::super) unsafe fn mark(
        &self,
        context: &windows::Win32::Graphics::Direct3D11::ID3D11DeviceContext,
    ) {
        context.End(&self.asynchronous);
    }

    pub(in super::super) unsafe fn is_ready(
        &self,
        context: &windows::Win32::Graphics::Direct3D11::ID3D11DeviceContext,
    ) -> Result<bool, BackendError> {
        use windows::Win32::Foundation::{S_FALSE, S_OK};

        let hr = (windows::core::Interface::vtable(context).GetData)(
            windows::core::Interface::as_raw(context),
            windows::core::Interface::as_raw(&self.asynchronous),
            std::ptr::null_mut(),
            0,
            0,
        );
        if hr == S_OK {
            Ok(true)
        } else if hr == S_FALSE {
            Ok(false)
        } else {
            Err(BackendError::WindowsApi {
                func: "ID3D11DeviceContext::GetData(DDA source fence)",
                message: format!("HRESULT 0x{:08X}", hr.0 as u32),
            })
        }
    }

    pub(in super::super) unsafe fn wait_ready(
        &self,
        context: &windows::Win32::Graphics::Direct3D11::ID3D11DeviceContext,
    ) -> Result<(), BackendError> {
        let started = std::time::Instant::now();
        let deadline = started + std::time::Duration::from_secs(2);
        let mut polls = 0u32;
        while !self.is_ready(context)? {
            polls = polls.wrapping_add(1);
            if polls.is_multiple_of(256) {
                let device = context
                    .GetDevice()
                    .map_err(|err| BackendError::WindowsApi {
                        func: "ID3D11DeviceContext::GetDevice(GPU fence wait)",
                        message: err.to_string(),
                    })?;
                device
                    .GetDeviceRemovedReason()
                    .map_err(|err| BackendError::WindowsApi {
                        func: "ID3D11Device::GetDeviceRemovedReason(GPU fence wait)",
                        message: err.to_string(),
                    })?;
            }
            if std::time::Instant::now() >= deadline {
                return Err(BackendError::WindowsApi {
                    func: "ID3D11DeviceContext::GetData(GPU fence wait)",
                    message: format!(
                        "GPU event query did not complete within {:.1}ms",
                        started.elapsed().as_secs_f64() * 1000.0
                    ),
                });
            }
            if polls < 64 {
                std::hint::spin_loop();
            } else if polls < 512 {
                std::thread::yield_now();
            } else {
                std::thread::sleep(std::time::Duration::from_micros(50));
            }
        }
        Ok(())
    }

    pub(in super::super) unsafe fn wait(
        &self,
        context: &windows::Win32::Graphics::Direct3D11::ID3D11DeviceContext,
    ) -> Result<(), BackendError> {
        self.mark(context);
        self.wait_ready(context)
    }
}

#[cfg(windows)]
pub(in super::super) unsafe fn return_ready_snapshot_slots(
    pending: &mut Vec<CaptureFrameSlot>,
    context: &windows::Win32::Graphics::Direct3D11::ID3D11DeviceContext,
    free_tx: &std::sync::mpsc::Sender<CaptureFrameSlot>,
    wait_all: bool,
) -> Result<(), BackendError> {
    let mut index = 0usize;
    while index < pending.len() {
        let ready = match &pending[index] {
            CaptureFrameSlot::Shared(slot) => {
                if wait_all {
                    slot.encoder_fence.wait_ready(context)?;
                    true
                } else {
                    slot.encoder_fence.is_ready(context)?
                }
            }
            CaptureFrameSlot::WgcLocal(_) => true,
        };
        if ready {
            let slot = pending.swap_remove(index);
            let _ = free_tx.send(slot);
        } else {
            index += 1;
        }
    }
    Ok(())
}
