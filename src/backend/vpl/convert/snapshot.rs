use super::*;

#[cfg(windows)]
pub(in super::super) struct GpuSnapshotConverter {
    pub(in super::super) device: windows::Win32::Graphics::Direct3D11::ID3D11Device,
    pub(in super::super) context: windows::Win32::Graphics::Direct3D11::ID3D11DeviceContext,
    pub(in super::super) source_srv_cache:
        std::sync::Arc<std::sync::Mutex<ShaderResourceViewCache>>,
    pub(in super::super) output: windows::Win32::Graphics::Direct3D11::ID3D11Texture2D,
    pub(in super::super) render_target:
        windows::Win32::Graphics::Direct3D11::ID3D11RenderTargetView,
    pub(in super::super) vertex_shader: windows::Win32::Graphics::Direct3D11::ID3D11VertexShader,
    pub(in super::super) pixel_shader: windows::Win32::Graphics::Direct3D11::ID3D11PixelShader,
    pub(in super::super) full_viewport: windows::Win32::Graphics::Direct3D11::D3D11_VIEWPORT,
}

#[cfg(windows)]
impl GpuSnapshotConverter {
    pub(in super::super) unsafe fn new(
        device: &windows::Win32::Graphics::Direct3D11::ID3D11Device,
        context: &windows::Win32::Graphics::Direct3D11::ID3D11DeviceContext,
        source_desc: &windows::Win32::Graphics::Direct3D11::D3D11_TEXTURE2D_DESC,
    ) -> Result<Self, BackendError> {
        use windows::Win32::Graphics::Direct3D11::{
            D3D11_BIND_RENDER_TARGET, D3D11_BIND_SHADER_RESOURCE, D3D11_TEXTURE2D_DESC,
            D3D11_USAGE_DEFAULT,
        };

        let desc = D3D11_TEXTURE2D_DESC {
            Width: source_desc.Width.max(1),
            Height: source_desc.Height.max(1),
            MipLevels: 1,
            ArraySize: 1,
            Format: source_desc.Format,
            SampleDesc: source_desc.SampleDesc,
            Usage: D3D11_USAGE_DEFAULT,
            BindFlags: (D3D11_BIND_RENDER_TARGET.0 | D3D11_BIND_SHADER_RESOURCE.0) as u32,
            CPUAccessFlags: 0,
            MiscFlags: 0,
        };
        let mut output = None;
        device
            .CreateTexture2D(&desc, None, Some(&mut output))
            .map_err(|err| BackendError::WindowsApi {
                func: "ID3D11Device::CreateTexture2D(shader snapshot)",
                message: err.to_string(),
            })?;
        let output = output.ok_or_else(|| BackendError::WindowsApi {
            func: "CreateTexture2D(shader snapshot)",
            message: "返回空纹理".to_owned(),
        })?;
        let mut render_target = None;
        device
            .CreateRenderTargetView(&output, None, Some(&mut render_target))
            .map_err(|err| BackendError::WindowsApi {
                func: "ID3D11Device::CreateRenderTargetView(shader snapshot)",
                message: err.to_string(),
            })?;
        let render_target = render_target.ok_or_else(|| BackendError::WindowsApi {
            func: "CreateRenderTargetView(shader snapshot)",
            message: "返回空 RTV".to_owned(),
        })?;

        let vs_blob = compile_shader(SNAPSHOT_COPY_HLSL, b"vs_main\0", b"vs_5_0\0")?;
        let ps_blob = compile_shader(SNAPSHOT_COPY_HLSL, b"ps_main\0", b"ps_5_0\0")?;
        let mut vertex_shader = None;
        device
            .CreateVertexShader(&vs_blob, None, Some(&mut vertex_shader))
            .map_err(|err| BackendError::WindowsApi {
                func: "ID3D11Device::CreateVertexShader(shader snapshot)",
                message: err.to_string(),
            })?;
        let mut pixel_shader = None;
        device
            .CreatePixelShader(&ps_blob, None, Some(&mut pixel_shader))
            .map_err(|err| BackendError::WindowsApi {
                func: "ID3D11Device::CreatePixelShader(shader snapshot)",
                message: err.to_string(),
            })?;

        Ok(Self {
            device: device.clone(),
            context: context.clone(),
            source_srv_cache: std::sync::Arc::new(std::sync::Mutex::new(
                ShaderResourceViewCache::retained(),
            )),
            output,
            render_target,
            vertex_shader: vertex_shader.ok_or_else(|| BackendError::WindowsApi {
                func: "CreateVertexShader(shader snapshot)",
                message: "返回空 VS".to_owned(),
            })?,
            pixel_shader: pixel_shader.ok_or_else(|| BackendError::WindowsApi {
                func: "CreatePixelShader(shader snapshot)",
                message: "返回空 PS".to_owned(),
            })?,
            full_viewport: windows::Win32::Graphics::Direct3D11::D3D11_VIEWPORT {
                TopLeftX: 0.0,
                TopLeftY: 0.0,
                Width: source_desc.Width as f32,
                Height: source_desc.Height as f32,
                MinDepth: 0.0,
                MaxDepth: 1.0,
            },
        })
    }

    pub(in super::super) unsafe fn copy_full(
        &self,
        source: &windows::Win32::Graphics::Direct3D11::ID3D11Texture2D,
    ) -> Result<(), BackendError> {
        self.copy_with_viewports(source, &[self.full_viewport])
    }

    pub(in super::super) unsafe fn copy_dirty(
        &self,
        source: &windows::Win32::Graphics::Direct3D11::ID3D11Texture2D,
        dirty_rects: &[windows::Win32::Foundation::RECT],
    ) -> Result<(), BackendError> {
        let viewports: Vec<_> = dirty_rects.iter().filter_map(snapshot_viewport).collect();
        if viewports.is_empty() {
            self.copy_full(source)
        } else {
            self.copy_with_viewports(source, &viewports)
        }
    }

    pub(in super::super) unsafe fn copy_with_viewports(
        &self,
        source: &windows::Win32::Graphics::Direct3D11::ID3D11Texture2D,
        viewports: &[windows::Win32::Graphics::Direct3D11::D3D11_VIEWPORT],
    ) -> Result<(), BackendError> {
        use windows::Win32::Graphics::Direct3D::D3D_PRIMITIVE_TOPOLOGY_TRIANGLELIST;
        use windows::Win32::Graphics::Direct3D11::{
            ID3D11RenderTargetView, ID3D11ShaderResourceView,
        };
        let srv = lock_srv_cache(&self.source_srv_cache)?.get_or_create(
            &self.device,
            &self.context,
            source,
            "ID3D11Device::CreateShaderResourceView(shader snapshot source)",
        )?;

        self.context
            .IASetPrimitiveTopology(D3D_PRIMITIVE_TOPOLOGY_TRIANGLELIST);
        self.context.VSSetShader(&self.vertex_shader, None);
        self.context.PSSetShader(&self.pixel_shader, None);
        self.context.PSSetShaderResources(0, Some(&[Some(srv)]));
        self.context
            .OMSetRenderTargets(Some(&[Some(self.render_target.clone())]), None);
        for viewport in viewports {
            self.context.RSSetViewports(Some(&[*viewport]));
            self.context.Draw(3, 0);
        }
        let empty_srv: [Option<ID3D11ShaderResourceView>; 1] = [None];
        self.context.PSSetShaderResources(0, Some(&empty_srv));
        let empty_rtv: [Option<ID3D11RenderTargetView>; 1] = [None];
        self.context.OMSetRenderTargets(Some(&empty_rtv), None);
        Ok(())
    }

    pub(in super::super) fn output_texture(
        &self,
    ) -> &windows::Win32::Graphics::Direct3D11::ID3D11Texture2D {
        &self.output
    }
}

#[cfg(windows)]
pub(in super::super) fn snapshot_viewport(
    rect: &windows::Win32::Foundation::RECT,
) -> Option<windows::Win32::Graphics::Direct3D11::D3D11_VIEWPORT> {
    let left = rect.left.max(0) as f32;
    let top = rect.top.max(0) as f32;
    let width = (rect.right - rect.left).max(0) as f32;
    let height = (rect.bottom - rect.top).max(0) as f32;
    if width <= 0.0 || height <= 0.0 {
        return None;
    }
    Some(windows::Win32::Graphics::Direct3D11::D3D11_VIEWPORT {
        TopLeftX: left,
        TopLeftY: top,
        Width: width,
        Height: height,
        MinDepth: 0.0,
        MaxDepth: 1.0,
    })
}

#[cfg(windows)]
pub(in super::super) struct GpuRgbaConverter {
    pub(in super::super) device: windows::Win32::Graphics::Direct3D11::ID3D11Device,
    pub(in super::super) context: windows::Win32::Graphics::Direct3D11::ID3D11DeviceContext,
    pub(in super::super) source_srv_cache:
        std::sync::Arc<std::sync::Mutex<ShaderResourceViewCache>>,
    pub(in super::super) output: windows::Win32::Graphics::Direct3D11::ID3D11Texture2D,
    pub(in super::super) render_target:
        windows::Win32::Graphics::Direct3D11::ID3D11RenderTargetView,
    pub(in super::super) vertex_shader: windows::Win32::Graphics::Direct3D11::ID3D11VertexShader,
    pub(in super::super) pixel_shader: windows::Win32::Graphics::Direct3D11::ID3D11PixelShader,
    pub(in super::super) viewport: windows::Win32::Graphics::Direct3D11::D3D11_VIEWPORT,
}

#[cfg(windows)]
impl GpuRgbaConverter {
    pub(in super::super) unsafe fn new(
        device: &windows::Win32::Graphics::Direct3D11::ID3D11Device,
        context: &windows::Win32::Graphics::Direct3D11::ID3D11DeviceContext,
        width: u32,
        height: u32,
    ) -> Result<Self, BackendError> {
        use windows::Win32::Graphics::Direct3D11::{
            D3D11_BIND_RENDER_TARGET, D3D11_BIND_SHADER_RESOURCE, D3D11_TEXTURE2D_DESC,
            D3D11_USAGE_DEFAULT,
        };
        use windows::Win32::Graphics::Dxgi::Common::{
            DXGI_FORMAT_R8G8B8A8_UNORM, DXGI_SAMPLE_DESC,
        };

        let desc = D3D11_TEXTURE2D_DESC {
            Width: width.max(1),
            Height: height.max(1),
            MipLevels: 1,
            ArraySize: 1,
            Format: DXGI_FORMAT_R8G8B8A8_UNORM,
            SampleDesc: DXGI_SAMPLE_DESC {
                Count: 1,
                Quality: 0,
            },
            Usage: D3D11_USAGE_DEFAULT,
            BindFlags: (D3D11_BIND_RENDER_TARGET.0 | D3D11_BIND_SHADER_RESOURCE.0) as u32,
            CPUAccessFlags: 0,
            MiscFlags: 0,
        };
        let mut output = None;
        device
            .CreateTexture2D(&desc, None, Some(&mut output))
            .map_err(|err| BackendError::WindowsApi {
                func: "ID3D11Device::CreateTexture2D(RGBA converter)",
                message: err.to_string(),
            })?;
        let output = output.ok_or_else(|| BackendError::WindowsApi {
            func: "CreateTexture2D(RGBA converter)",
            message: "返回空纹理".to_owned(),
        })?;
        let mut render_target = None;
        device
            .CreateRenderTargetView(&output, None, Some(&mut render_target))
            .map_err(|err| BackendError::WindowsApi {
                func: "ID3D11Device::CreateRenderTargetView(RGBA converter)",
                message: err.to_string(),
            })?;
        let render_target = render_target.ok_or_else(|| BackendError::WindowsApi {
            func: "CreateRenderTargetView(RGBA converter)",
            message: "返回空 RTV".to_owned(),
        })?;

        let vs_blob = compile_shader(RGBA_CONVERT_HLSL, b"vs_main\0", b"vs_5_0\0")?;
        let ps_blob = compile_shader(RGBA_CONVERT_HLSL, b"ps_main\0", b"ps_5_0\0")?;
        let mut vertex_shader = None;
        device
            .CreateVertexShader(&vs_blob, None, Some(&mut vertex_shader))
            .map_err(|err| BackendError::WindowsApi {
                func: "ID3D11Device::CreateVertexShader(RGBA converter)",
                message: err.to_string(),
            })?;
        let mut pixel_shader = None;
        device
            .CreatePixelShader(&ps_blob, None, Some(&mut pixel_shader))
            .map_err(|err| BackendError::WindowsApi {
                func: "ID3D11Device::CreatePixelShader(RGBA converter)",
                message: err.to_string(),
            })?;

        Ok(Self {
            device: device.clone(),
            context: context.clone(),
            source_srv_cache: std::sync::Arc::new(std::sync::Mutex::new(
                ShaderResourceViewCache::retained(),
            )),
            output,
            render_target,
            vertex_shader: vertex_shader.ok_or_else(|| BackendError::WindowsApi {
                func: "CreateVertexShader(RGBA converter)",
                message: "返回空 VS".to_owned(),
            })?,
            pixel_shader: pixel_shader.ok_or_else(|| BackendError::WindowsApi {
                func: "CreatePixelShader(RGBA converter)",
                message: "返回空 PS".to_owned(),
            })?,
            viewport: windows::Win32::Graphics::Direct3D11::D3D11_VIEWPORT {
                TopLeftX: 0.0,
                TopLeftY: 0.0,
                Width: width as f32,
                Height: height as f32,
                MinDepth: 0.0,
                MaxDepth: 1.0,
            },
        })
    }

    pub(in super::super) unsafe fn convert(
        &self,
        source: &windows::Win32::Graphics::Direct3D11::ID3D11Texture2D,
    ) -> Result<windows::Win32::Graphics::Direct3D11::ID3D11Texture2D, BackendError> {
        use windows::Win32::Graphics::Direct3D::D3D_PRIMITIVE_TOPOLOGY_TRIANGLELIST;
        use windows::Win32::Graphics::Direct3D11::{
            ID3D11RenderTargetView, ID3D11ShaderResourceView,
        };
        let srv = lock_srv_cache(&self.source_srv_cache)?.get_or_create(
            &self.device,
            &self.context,
            source,
            "ID3D11Device::CreateShaderResourceView(RGBA source)",
        )?;

        self.context.RSSetViewports(Some(&[self.viewport]));
        self.context
            .IASetPrimitiveTopology(D3D_PRIMITIVE_TOPOLOGY_TRIANGLELIST);
        self.context.VSSetShader(&self.vertex_shader, None);
        self.context.PSSetShader(&self.pixel_shader, None);
        self.context.PSSetShaderResources(0, Some(&[Some(srv)]));
        self.context
            .OMSetRenderTargets(Some(&[Some(self.render_target.clone())]), None);
        self.context.Draw(3, 0);
        let empty_srv: [Option<ID3D11ShaderResourceView>; 1] = [None];
        self.context.PSSetShaderResources(0, Some(&empty_srv));
        let empty_rtv: [Option<ID3D11RenderTargetView>; 1] = [None];
        self.context.OMSetRenderTargets(Some(&empty_rtv), None);
        Ok(self.output.clone())
    }

    pub(in super::super) fn output_texture(
        &self,
    ) -> &windows::Win32::Graphics::Direct3D11::ID3D11Texture2D {
        &self.output
    }
}

#[cfg(windows)]
pub(in super::super) struct GpuRgb4Converter {
    pub(in super::super) device: windows::Win32::Graphics::Direct3D11::ID3D11Device,
    pub(in super::super) context: windows::Win32::Graphics::Direct3D11::ID3D11DeviceContext,
    pub(in super::super) source_srv_cache:
        std::sync::Arc<std::sync::Mutex<ShaderResourceViewCache>>,
    pub(in super::super) render_target:
        windows::Win32::Graphics::Direct3D11::ID3D11RenderTargetView,
    pub(in super::super) vertex_shader: windows::Win32::Graphics::Direct3D11::ID3D11VertexShader,
    pub(in super::super) pixel_shader: windows::Win32::Graphics::Direct3D11::ID3D11PixelShader,
    pub(in super::super) viewport: windows::Win32::Graphics::Direct3D11::D3D11_VIEWPORT,
}

#[cfg(windows)]
impl GpuRgb4Converter {
    pub(in super::super) unsafe fn new(
        device: &windows::Win32::Graphics::Direct3D11::ID3D11Device,
        context: &windows::Win32::Graphics::Direct3D11::ID3D11DeviceContext,
        output: &windows::Win32::Graphics::Direct3D11::ID3D11Texture2D,
        width: u32,
        height: u32,
        source_srv_cache: std::sync::Arc<std::sync::Mutex<ShaderResourceViewCache>>,
    ) -> Result<Self, BackendError> {
        use windows::Win32::Graphics::Direct3D11::{
            D3D11_RENDER_TARGET_VIEW_DESC, D3D11_RENDER_TARGET_VIEW_DESC_0,
            D3D11_RTV_DIMENSION_TEXTURE2D, D3D11_TEX2D_RTV,
        };
        use windows::Win32::Graphics::Dxgi::Common::DXGI_FORMAT_B8G8R8A8_UNORM;

        let rtv_desc = D3D11_RENDER_TARGET_VIEW_DESC {
            Format: DXGI_FORMAT_B8G8R8A8_UNORM,
            ViewDimension: D3D11_RTV_DIMENSION_TEXTURE2D,
            Anonymous: D3D11_RENDER_TARGET_VIEW_DESC_0 {
                Texture2D: D3D11_TEX2D_RTV { MipSlice: 0 },
            },
        };
        let mut render_target = None;
        device
            .CreateRenderTargetView(output, Some(&rtv_desc), Some(&mut render_target))
            .map_err(|err| BackendError::WindowsApi {
                func: "ID3D11Device::CreateRenderTargetView(RGB4 writer)",
                message: err.to_string(),
            })?;

        let vs_blob = compile_shader(RGBA_CONVERT_HLSL, b"vs_main\0", b"vs_5_0\0")?;
        let ps_blob = compile_shader(RGBA_CONVERT_HLSL, b"ps_main\0", b"ps_5_0\0")?;
        let mut vertex_shader = None;
        device
            .CreateVertexShader(&vs_blob, None, Some(&mut vertex_shader))
            .map_err(|err| BackendError::WindowsApi {
                func: "ID3D11Device::CreateVertexShader(RGB4 writer)",
                message: err.to_string(),
            })?;
        let mut pixel_shader = None;
        device
            .CreatePixelShader(&ps_blob, None, Some(&mut pixel_shader))
            .map_err(|err| BackendError::WindowsApi {
                func: "ID3D11Device::CreatePixelShader(RGB4 writer)",
                message: err.to_string(),
            })?;

        Ok(Self {
            device: device.clone(),
            context: context.clone(),
            source_srv_cache,
            render_target: render_target.ok_or_else(|| BackendError::WindowsApi {
                func: "CreateRenderTargetView(RGB4 writer)",
                message: "返回空 RTV".to_owned(),
            })?,
            vertex_shader: vertex_shader.ok_or_else(|| BackendError::WindowsApi {
                func: "CreateVertexShader(RGB4 writer)",
                message: "返回空 VS".to_owned(),
            })?,
            pixel_shader: pixel_shader.ok_or_else(|| BackendError::WindowsApi {
                func: "CreatePixelShader(RGB4 writer)",
                message: "返回空 PS".to_owned(),
            })?,
            viewport: windows::Win32::Graphics::Direct3D11::D3D11_VIEWPORT {
                TopLeftX: 0.0,
                TopLeftY: 0.0,
                Width: width as f32,
                Height: height as f32,
                MinDepth: 0.0,
                MaxDepth: 1.0,
            },
        })
    }

    pub(in super::super) unsafe fn convert(
        &self,
        source: &windows::Win32::Graphics::Direct3D11::ID3D11Texture2D,
    ) -> Result<(), BackendError> {
        use windows::Win32::Graphics::Direct3D::D3D_PRIMITIVE_TOPOLOGY_TRIANGLELIST;
        use windows::Win32::Graphics::Direct3D11::{
            ID3D11RenderTargetView, ID3D11ShaderResourceView,
        };

        let srv = lock_srv_cache(&self.source_srv_cache)?.get_or_create(
            &self.device,
            &self.context,
            source,
            "ID3D11Device::CreateShaderResourceView(RGB4 writer source)",
        )?;

        self.context.RSSetViewports(Some(&[self.viewport]));
        self.context
            .IASetPrimitiveTopology(D3D_PRIMITIVE_TOPOLOGY_TRIANGLELIST);
        self.context.VSSetShader(&self.vertex_shader, None);
        self.context.PSSetShader(&self.pixel_shader, None);
        self.context.PSSetShaderResources(0, Some(&[Some(srv)]));
        self.context
            .OMSetRenderTargets(Some(&[Some(self.render_target.clone())]), None);
        self.context.Draw(3, 0);
        let empty_srv: [Option<ID3D11ShaderResourceView>; 1] = [None];
        self.context.PSSetShaderResources(0, Some(&empty_srv));
        let empty_rtv: [Option<ID3D11RenderTargetView>; 1] = [None];
        self.context.OMSetRenderTargets(Some(&empty_rtv), None);
        Ok(())
    }
}
