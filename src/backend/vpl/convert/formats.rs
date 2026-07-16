use super::*;

#[cfg(windows)]
pub(in super::super) enum GpuRecordConverter {
    P010(GpuP010Converter),
    Nv12(GpuNv12Converter),
    NvencPlanar(GpuNvencPlanarConverter),
    Packed(GpuPackedConverter),
    Rgb4(GpuRgb4Converter),
}

#[cfg(windows)]
impl GpuRecordConverter {
    pub(in super::super) unsafe fn new(
        route: VplRecordRoute,
        device: &windows::Win32::Graphics::Direct3D11::ID3D11Device,
        context: &windows::Win32::Graphics::Direct3D11::ID3D11DeviceContext,
        output: &windows::Win32::Graphics::Direct3D11::ID3D11Texture2D,
        width: u32,
        height: u32,
        enable_compute: bool,
    ) -> Result<Self, BackendError> {
        Self::new_with_source_cache(
            route,
            device,
            context,
            output,
            width,
            height,
            enable_compute,
            std::sync::Arc::new(std::sync::Mutex::new(ShaderResourceViewCache::retained())),
        )
    }

    #[allow(clippy::too_many_arguments)]
    pub(in super::super) unsafe fn new_with_source_cache(
        route: VplRecordRoute,
        device: &windows::Win32::Graphics::Direct3D11::ID3D11Device,
        context: &windows::Win32::Graphics::Direct3D11::ID3D11DeviceContext,
        output: &windows::Win32::Graphics::Direct3D11::ID3D11Texture2D,
        width: u32,
        height: u32,
        enable_compute: bool,
        source_srv_cache: std::sync::Arc<std::sync::Mutex<ShaderResourceViewCache>>,
    ) -> Result<Self, BackendError> {
        match route.fourcc {
            MFX_FOURCC_NV12 => Ok(Self::Nv12(GpuNv12Converter::new(
                route,
                device,
                context,
                output,
                width,
                height,
                source_srv_cache,
            )?)),
            MFX_FOURCC_P010 => Ok(Self::P010(GpuP010Converter::new(
                route,
                device,
                context,
                output,
                width,
                height,
                enable_compute,
                source_srv_cache,
            )?)),
            NVENC_FOURCC_NV16 | NVENC_FOURCC_P210 | NVENC_FOURCC_Y444 | NVENC_FOURCC_Y4P0 => {
                Ok(Self::NvencPlanar(GpuNvencPlanarConverter::new(
                    route,
                    device,
                    context,
                    output,
                    width,
                    height,
                    source_srv_cache,
                )?))
            }
            MFX_FOURCC_YUY2 | MFX_FOURCC_Y210 | MFX_FOURCC_AYUV | MFX_FOURCC_Y410 => {
                Ok(Self::Packed(GpuPackedConverter::new(
                    route,
                    device,
                    context,
                    output,
                    width,
                    height,
                    source_srv_cache,
                )?))
            }
            MFX_FOURCC_RGB4 => Ok(Self::Rgb4(GpuRgb4Converter::new(
                device,
                context,
                output,
                width,
                height,
                source_srv_cache,
            )?)),
            _ => Err(BackendError::unsupported(
                "GPU ChromaWriter",
                fourcc_to_string(route.fourcc),
                "该 oneVPL FourCC 仍只参与 Query，尚无生产 GPU writer",
            )),
        }
    }

    pub(in super::super) unsafe fn convert(
        &self,
        source: &windows::Win32::Graphics::Direct3D11::ID3D11Texture2D,
    ) -> Result<(), BackendError> {
        match self {
            Self::P010(converter) => converter.convert(source),
            Self::Nv12(converter) => converter.convert(source),
            Self::NvencPlanar(converter) => converter.convert(source),
            Self::Packed(converter) => converter.convert(source),
            Self::Rgb4(converter) => converter.convert(source),
        }
    }
}

#[cfg(windows)]
pub(in super::super) struct GpuNv12Converter {
    pub(in super::super) device: windows::Win32::Graphics::Direct3D11::ID3D11Device,
    pub(in super::super) context: windows::Win32::Graphics::Direct3D11::ID3D11DeviceContext,
    pub(in super::super) source_srv_cache:
        std::sync::Arc<std::sync::Mutex<ShaderResourceViewCache>>,
    pub(in super::super) luma_uav: windows::Win32::Graphics::Direct3D11::ID3D11UnorderedAccessView,
    pub(in super::super) chroma_uav:
        windows::Win32::Graphics::Direct3D11::ID3D11UnorderedAccessView,
    pub(in super::super) compute_shader: windows::Win32::Graphics::Direct3D11::ID3D11ComputeShader,
    pub(in super::super) chroma_width: u32,
    pub(in super::super) chroma_height: u32,
}

#[cfg(windows)]
impl GpuNv12Converter {
    pub(in super::super) unsafe fn new(
        route: VplRecordRoute,
        device: &windows::Win32::Graphics::Direct3D11::ID3D11Device,
        context: &windows::Win32::Graphics::Direct3D11::ID3D11DeviceContext,
        output: &windows::Win32::Graphics::Direct3D11::ID3D11Texture2D,
        width: u32,
        height: u32,
        source_srv_cache: std::sync::Arc<std::sync::Mutex<ShaderResourceViewCache>>,
    ) -> Result<Self, BackendError> {
        use windows::Win32::Graphics::Direct3D11::{
            D3D11_TEX2D_UAV1, D3D11_UAV_DIMENSION_TEXTURE2D, D3D11_UNORDERED_ACCESS_VIEW_DESC1,
            D3D11_UNORDERED_ACCESS_VIEW_DESC1_0, ID3D11Device3, ID3D11Resource,
            ID3D11UnorderedAccessView1,
        };
        use windows::Win32::Graphics::Dxgi::Common::{
            DXGI_FORMAT_R8_UNORM, DXGI_FORMAT_R8G8_UNORM,
        };
        use windows::core::Interface;

        let device3: ID3D11Device3 = device.cast().map_err(|err| BackendError::WindowsApi {
            func: "ID3D11Device::cast<ID3D11Device3>(NV12 converter)",
            message: err.to_string(),
        })?;
        let output_resource: ID3D11Resource =
            output.cast().map_err(|err| BackendError::WindowsApi {
                func: "ID3D11Texture2D::cast<ID3D11Resource>(NV12 output)",
                message: err.to_string(),
            })?;

        let luma_uav_desc = D3D11_UNORDERED_ACCESS_VIEW_DESC1 {
            Format: DXGI_FORMAT_R8_UNORM,
            ViewDimension: D3D11_UAV_DIMENSION_TEXTURE2D,
            Anonymous: D3D11_UNORDERED_ACCESS_VIEW_DESC1_0 {
                Texture2D: D3D11_TEX2D_UAV1 {
                    MipSlice: 0,
                    PlaneSlice: 0,
                },
            },
        };
        let mut luma_uav1: Option<ID3D11UnorderedAccessView1> = None;
        device3
            .CreateUnorderedAccessView1(
                &output_resource,
                Some(&luma_uav_desc),
                Some(&mut luma_uav1),
            )
            .map_err(|err| BackendError::WindowsApi {
                func: "ID3D11Device3::CreateUnorderedAccessView1(NV12 luma)",
                message: err.to_string(),
            })?;
        let luma_uav = luma_uav1
            .ok_or_else(|| BackendError::WindowsApi {
                func: "CreateUnorderedAccessView1(NV12 luma)",
                message: "返回空 luma UAV".to_owned(),
            })?
            .cast()
            .map_err(|err| BackendError::WindowsApi {
                func: "ID3D11UnorderedAccessView1::cast(NV12 luma)",
                message: err.to_string(),
            })?;

        let chroma_uav_desc = D3D11_UNORDERED_ACCESS_VIEW_DESC1 {
            Format: DXGI_FORMAT_R8G8_UNORM,
            ViewDimension: D3D11_UAV_DIMENSION_TEXTURE2D,
            Anonymous: D3D11_UNORDERED_ACCESS_VIEW_DESC1_0 {
                Texture2D: D3D11_TEX2D_UAV1 {
                    MipSlice: 0,
                    PlaneSlice: 1,
                },
            },
        };
        let mut chroma_uav1: Option<ID3D11UnorderedAccessView1> = None;
        device3
            .CreateUnorderedAccessView1(
                &output_resource,
                Some(&chroma_uav_desc),
                Some(&mut chroma_uav1),
            )
            .map_err(|err| BackendError::WindowsApi {
                func: "ID3D11Device3::CreateUnorderedAccessView1(NV12 chroma)",
                message: err.to_string(),
            })?;
        let chroma_uav = chroma_uav1
            .ok_or_else(|| BackendError::WindowsApi {
                func: "CreateUnorderedAccessView1(NV12 chroma)",
                message: "返回空 chroma UAV".to_owned(),
            })?
            .cast()
            .map_err(|err| BackendError::WindowsApi {
                func: "ID3D11UnorderedAccessView1::cast(NV12 chroma)",
                message: err.to_string(),
            })?;

        let shader_template = if route.is_bt2020_sdr() {
            NV12_BT2020_CONVERT_HLSL
        } else {
            NV12_CONVERT_HLSL
        };
        let shader_source = shader_source_with_range(shader_template, route.mp4_color.full_range);
        let cs_blob = compile_shader(&shader_source, b"cs_main\0", b"cs_5_0\0")?;
        let mut compute_shader = None;
        device
            .CreateComputeShader(&cs_blob, None, Some(&mut compute_shader))
            .map_err(|err| BackendError::WindowsApi {
                func: "ID3D11Device::CreateComputeShader(NV12 compute)",
                message: err.to_string(),
            })?;

        Ok(Self {
            device: device.clone(),
            context: context.clone(),
            source_srv_cache,
            luma_uav,
            chroma_uav,
            compute_shader: compute_shader.ok_or_else(|| BackendError::WindowsApi {
                func: "CreateComputeShader(NV12 compute)",
                message: "返回空 CS".to_owned(),
            })?,
            chroma_width: (width / 2).max(1),
            chroma_height: (height / 2).max(1),
        })
    }

    pub(in super::super) unsafe fn convert(
        &self,
        source: &windows::Win32::Graphics::Direct3D11::ID3D11Texture2D,
    ) -> Result<(), BackendError> {
        use windows::Win32::Graphics::Direct3D11::{
            ID3D11ComputeShader, ID3D11ShaderResourceView, ID3D11UnorderedAccessView,
        };

        let srv = lock_srv_cache(&self.source_srv_cache)?.get_or_create(
            &self.device,
            &self.context,
            source,
            "ID3D11Device::CreateShaderResourceView(NV12 source)",
        )?;

        self.context.CSSetShader(&self.compute_shader, None);
        self.context.CSSetShaderResources(0, Some(&[Some(srv)]));
        let uavs = [Some(self.luma_uav.clone()), Some(self.chroma_uav.clone())];
        self.context
            .CSSetUnorderedAccessViews(0, 2, Some(uavs.as_ptr()), None);
        self.context.Dispatch(
            self.chroma_width.div_ceil(8),
            self.chroma_height.div_ceil(8),
            1,
        );
        let empty_srv: [Option<ID3D11ShaderResourceView>; 1] = [None];
        self.context.CSSetShaderResources(0, Some(&empty_srv));
        let empty_uav: [Option<ID3D11UnorderedAccessView>; 2] = [None, None];
        self.context
            .CSSetUnorderedAccessViews(0, 2, Some(empty_uav.as_ptr()), None);
        self.context.CSSetShader(None::<&ID3D11ComputeShader>, None);
        Ok(())
    }
}

#[cfg(windows)]
pub(in super::super) struct GpuNvencPlanarConverter {
    device: windows::Win32::Graphics::Direct3D11::ID3D11Device,
    context: windows::Win32::Graphics::Direct3D11::ID3D11DeviceContext,
    source_srv_cache: std::sync::Arc<std::sync::Mutex<ShaderResourceViewCache>>,
    output_uav: windows::Win32::Graphics::Direct3D11::ID3D11UnorderedAccessView,
    compute_shader: windows::Win32::Graphics::Direct3D11::ID3D11ComputeShader,
    dispatch_width: u32,
    dispatch_height: u32,
}

#[cfg(windows)]
impl GpuNvencPlanarConverter {
    #[allow(clippy::too_many_arguments)]
    unsafe fn new(
        route: VplRecordRoute,
        device: &windows::Win32::Graphics::Direct3D11::ID3D11Device,
        context: &windows::Win32::Graphics::Direct3D11::ID3D11DeviceContext,
        output: &windows::Win32::Graphics::Direct3D11::ID3D11Texture2D,
        width: u32,
        height: u32,
        source_srv_cache: std::sync::Arc<std::sync::Mutex<ShaderResourceViewCache>>,
    ) -> Result<Self, BackendError> {
        use windows::Win32::Graphics::Direct3D11::{
            D3D11_TEX2D_UAV1, D3D11_UAV_DIMENSION_TEXTURE2D, D3D11_UNORDERED_ACCESS_VIEW_DESC1,
            D3D11_UNORDERED_ACCESS_VIEW_DESC1_0, ID3D11Device3, ID3D11Resource,
            ID3D11UnorderedAccessView1,
        };
        use windows::core::Interface;

        let chroma_422 = matches!(route.fourcc, NVENC_FOURCC_NV16 | NVENC_FOURCC_P210);
        if chroma_422 && !width.is_multiple_of(2) {
            return Err(BackendError::unsupported(
                "NVENC planar GPU writer",
                format!("{}x{}", width, height),
                "4:2:2 平面路线要求偶数宽度",
            ));
        }
        let bit_depth_10 = matches!(route.fourcc, NVENC_FOURCC_P210 | NVENC_FOURCC_Y4P0);
        let color_mode = if route.is_hdr_pq() {
            4
        } else if bit_depth_10 && route.is_bt2020_sdr() {
            3
        } else if bit_depth_10 {
            2
        } else if route.is_bt2020_sdr() {
            1
        } else {
            0
        };

        let device3: ID3D11Device3 = device.cast().map_err(|err| BackendError::WindowsApi {
            func: "ID3D11Device::cast<ID3D11Device3>(NVENC planar converter)",
            message: err.to_string(),
        })?;
        let output_resource: ID3D11Resource =
            output.cast().map_err(|err| BackendError::WindowsApi {
                func: "ID3D11Texture2D::cast<ID3D11Resource>(NVENC planar output)",
                message: err.to_string(),
            })?;
        let uav_desc = D3D11_UNORDERED_ACCESS_VIEW_DESC1 {
            Format: route.try_dxgi_format()?,
            ViewDimension: D3D11_UAV_DIMENSION_TEXTURE2D,
            Anonymous: D3D11_UNORDERED_ACCESS_VIEW_DESC1_0 {
                Texture2D: D3D11_TEX2D_UAV1 {
                    MipSlice: 0,
                    PlaneSlice: 0,
                },
            },
        };
        let mut output_uav1: Option<ID3D11UnorderedAccessView1> = None;
        device3
            .CreateUnorderedAccessView1(&output_resource, Some(&uav_desc), Some(&mut output_uav1))
            .map_err(|err| BackendError::WindowsApi {
                func: "ID3D11Device3::CreateUnorderedAccessView1(NVENC planar output)",
                message: err.to_string(),
            })?;
        let output_uav = output_uav1
            .ok_or_else(|| BackendError::WindowsApi {
                func: "CreateUnorderedAccessView1(NVENC planar output)",
                message: "返回空 UAV".to_owned(),
            })?
            .cast()
            .map_err(|err| BackendError::WindowsApi {
                func: "ID3D11UnorderedAccessView1::cast(NVENC planar output)",
                message: err.to_string(),
            })?;

        let shader_source =
            shader_source_with_range(NVENC_PLANAR_CONVERT_HLSL, route.mp4_color.full_range)
                .replace(
                    "RR_BIT_DEPTH_10_PLACEHOLDER",
                    if bit_depth_10 { "true" } else { "false" },
                )
                .replace(
                    "RR_CHROMA_422_PLACEHOLDER",
                    if chroma_422 { "true" } else { "false" },
                )
                .replace("RR_COLOR_MODE_PLACEHOLDER", &color_mode.to_string());
        let cs_blob = compile_shader(&shader_source, b"cs_main\0", b"cs_5_0\0")?;
        let mut compute_shader = None;
        device
            .CreateComputeShader(&cs_blob, None, Some(&mut compute_shader))
            .map_err(|err| BackendError::WindowsApi {
                func: "ID3D11Device::CreateComputeShader(NVENC planar converter)",
                message: err.to_string(),
            })?;

        Ok(Self {
            device: device.clone(),
            context: context.clone(),
            source_srv_cache,
            output_uav,
            compute_shader: compute_shader.ok_or_else(|| BackendError::WindowsApi {
                func: "CreateComputeShader(NVENC planar converter)",
                message: "返回空 CS".to_owned(),
            })?,
            dispatch_width: if chroma_422 { width.div_ceil(2) } else { width }.max(1),
            dispatch_height: height.max(1),
        })
    }

    unsafe fn convert(
        &self,
        source: &windows::Win32::Graphics::Direct3D11::ID3D11Texture2D,
    ) -> Result<(), BackendError> {
        use windows::Win32::Graphics::Direct3D11::{
            ID3D11ComputeShader, ID3D11ShaderResourceView, ID3D11UnorderedAccessView,
        };

        let srv = lock_srv_cache(&self.source_srv_cache)?.get_or_create(
            &self.device,
            &self.context,
            source,
            "ID3D11Device::CreateShaderResourceView(NVENC planar source)",
        )?;
        self.context.CSSetShader(&self.compute_shader, None);
        self.context.CSSetShaderResources(0, Some(&[Some(srv)]));
        let uavs = [Some(self.output_uav.clone())];
        self.context
            .CSSetUnorderedAccessViews(0, 1, Some(uavs.as_ptr()), None);
        self.context.Dispatch(
            self.dispatch_width.div_ceil(16),
            self.dispatch_height.div_ceil(8),
            1,
        );
        let empty_srv: [Option<ID3D11ShaderResourceView>; 1] = [None];
        self.context.CSSetShaderResources(0, Some(&empty_srv));
        let empty_uav: [Option<ID3D11UnorderedAccessView>; 1] = [None];
        self.context
            .CSSetUnorderedAccessViews(0, 1, Some(empty_uav.as_ptr()), None);
        self.context.CSSetShader(None::<&ID3D11ComputeShader>, None);
        Ok(())
    }
}

#[cfg(windows)]
pub(in super::super) struct GpuPackedConverter {
    pub(in super::super) device: windows::Win32::Graphics::Direct3D11::ID3D11Device,
    pub(in super::super) context: windows::Win32::Graphics::Direct3D11::ID3D11DeviceContext,
    pub(in super::super) source_srv_cache:
        std::sync::Arc<std::sync::Mutex<ShaderResourceViewCache>>,
    pub(in super::super) output_uav:
        windows::Win32::Graphics::Direct3D11::ID3D11UnorderedAccessView,
    pub(in super::super) compute_shader: windows::Win32::Graphics::Direct3D11::ID3D11ComputeShader,
    pub(in super::super) dispatch_width: u32,
    pub(in super::super) dispatch_height: u32,
    pub(in super::super) label: &'static str,
}

#[cfg(windows)]
impl GpuPackedConverter {
    pub(in super::super) unsafe fn new(
        route: VplRecordRoute,
        device: &windows::Win32::Graphics::Direct3D11::ID3D11Device,
        context: &windows::Win32::Graphics::Direct3D11::ID3D11DeviceContext,
        output: &windows::Win32::Graphics::Direct3D11::ID3D11Texture2D,
        width: u32,
        height: u32,
        source_srv_cache: std::sync::Arc<std::sync::Mutex<ShaderResourceViewCache>>,
    ) -> Result<Self, BackendError> {
        use windows::Win32::Graphics::Direct3D11::{
            D3D11_TEX2D_UAV1, D3D11_UAV_DIMENSION_TEXTURE2D, D3D11_UNORDERED_ACCESS_VIEW_DESC1,
            D3D11_UNORDERED_ACCESS_VIEW_DESC1_0, ID3D11Device3, ID3D11Resource,
            ID3D11UnorderedAccessView1,
        };
        use windows::Win32::Graphics::Dxgi::Common::{
            DXGI_FORMAT_R16G16B16A16_UINT, DXGI_FORMAT_R32_UINT,
        };
        use windows::core::Interface;

        let (view_format, shader_template, label, dispatch_width) = match route.fourcc {
            MFX_FOURCC_YUY2 => (
                DXGI_FORMAT_R32_UINT,
                if route.is_bt2020_sdr() {
                    YUY2_BT2020_CONVERT_HLSL
                } else {
                    YUY2_CONVERT_HLSL
                },
                "YUY2 8-bit 4:2:2",
                width.div_ceil(2),
            ),
            MFX_FOURCC_Y210 => (
                DXGI_FORMAT_R16G16B16A16_UINT,
                if route.is_hdr_pq() {
                    Y210_CONVERT_HLSL
                } else if route.is_bt2020_sdr() {
                    Y210_SDR_BT2020_CONVERT_HLSL
                } else {
                    Y210_SDR10_CONVERT_HLSL
                },
                "Y210 10-bit 4:2:2",
                width.div_ceil(2),
            ),
            MFX_FOURCC_AYUV => (
                DXGI_FORMAT_R32_UINT,
                if route.is_bt2020_sdr() {
                    AYUV_BT2020_CONVERT_HLSL
                } else {
                    AYUV_CONVERT_HLSL
                },
                "AYUV 8-bit 4:4:4",
                width,
            ),
            MFX_FOURCC_Y410 => (
                DXGI_FORMAT_R32_UINT,
                if route.is_hdr_pq() {
                    Y410_CONVERT_HLSL
                } else if route.is_bt2020_sdr() {
                    Y410_SDR_BT2020_CONVERT_HLSL
                } else {
                    Y410_SDR10_CONVERT_HLSL
                },
                "Y410 10-bit 4:4:4",
                width,
            ),
            _ => {
                return Err(BackendError::unsupported(
                    "GPU packed ChromaWriter",
                    fourcc_to_string(route.fourcc),
                    "没有对应 packed writer",
                ));
            }
        };

        let device3: ID3D11Device3 = device.cast().map_err(|err| BackendError::WindowsApi {
            func: "ID3D11Device::cast<ID3D11Device3>(packed converter)",
            message: err.to_string(),
        })?;
        let output_resource: ID3D11Resource =
            output.cast().map_err(|err| BackendError::WindowsApi {
                func: "ID3D11Texture2D::cast<ID3D11Resource>(packed output)",
                message: err.to_string(),
            })?;
        let uav_desc = D3D11_UNORDERED_ACCESS_VIEW_DESC1 {
            Format: view_format,
            ViewDimension: D3D11_UAV_DIMENSION_TEXTURE2D,
            Anonymous: D3D11_UNORDERED_ACCESS_VIEW_DESC1_0 {
                Texture2D: D3D11_TEX2D_UAV1 {
                    MipSlice: 0,
                    PlaneSlice: 0,
                },
            },
        };
        let mut output_uav1: Option<ID3D11UnorderedAccessView1> = None;
        device3
            .CreateUnorderedAccessView1(&output_resource, Some(&uav_desc), Some(&mut output_uav1))
            .map_err(|err| BackendError::WindowsApi {
                func: "ID3D11Device3::CreateUnorderedAccessView1(packed output)",
                message: err.to_string(),
            })?;
        let output_uav = output_uav1
            .ok_or_else(|| BackendError::WindowsApi {
                func: "CreateUnorderedAccessView1(packed output)",
                message: "返回空 UAV".to_owned(),
            })?
            .cast()
            .map_err(|err| BackendError::WindowsApi {
                func: "ID3D11UnorderedAccessView1::cast(packed output)",
                message: err.to_string(),
            })?;

        let shader_source = shader_source_with_range(shader_template, route.mp4_color.full_range);
        let cs_blob = compile_shader(&shader_source, b"cs_main\0", b"cs_5_0\0")?;
        let mut compute_shader = None;
        device
            .CreateComputeShader(&cs_blob, None, Some(&mut compute_shader))
            .map_err(|err| BackendError::WindowsApi {
                func: "ID3D11Device::CreateComputeShader(packed converter)",
                message: err.to_string(),
            })?;

        Ok(Self {
            device: device.clone(),
            context: context.clone(),
            source_srv_cache,
            output_uav,
            compute_shader: compute_shader.ok_or_else(|| BackendError::WindowsApi {
                func: "CreateComputeShader(packed converter)",
                message: "返回空 CS".to_owned(),
            })?,
            dispatch_width: dispatch_width.max(1),
            dispatch_height: height.max(1),
            label,
        })
    }

    pub(in super::super) unsafe fn convert(
        &self,
        source: &windows::Win32::Graphics::Direct3D11::ID3D11Texture2D,
    ) -> Result<(), BackendError> {
        use windows::Win32::Graphics::Direct3D11::{
            ID3D11ComputeShader, ID3D11ShaderResourceView, ID3D11UnorderedAccessView,
        };

        let srv = lock_srv_cache(&self.source_srv_cache)?.get_or_create(
            &self.device,
            &self.context,
            source,
            "ID3D11Device::CreateShaderResourceView(packed source)",
        )?;

        self.context.CSSetShader(&self.compute_shader, None);
        self.context.CSSetShaderResources(0, Some(&[Some(srv)]));
        let uavs = [Some(self.output_uav.clone())];
        self.context
            .CSSetUnorderedAccessViews(0, 1, Some(uavs.as_ptr()), None);
        self.context.Dispatch(
            self.dispatch_width.div_ceil(16),
            self.dispatch_height.div_ceil(8),
            1,
        );
        let empty_srv: [Option<ID3D11ShaderResourceView>; 1] = [None];
        self.context.CSSetShaderResources(0, Some(&empty_srv));
        let empty_uav: [Option<ID3D11UnorderedAccessView>; 1] = [None];
        self.context
            .CSSetUnorderedAccessViews(0, 1, Some(empty_uav.as_ptr()), None);
        self.context.CSSetShader(None::<&ID3D11ComputeShader>, None);
        Ok(())
    }
}

#[cfg(windows)]
pub(in super::super) struct GpuP010Converter {
    pub(in super::super) device: windows::Win32::Graphics::Direct3D11::ID3D11Device,
    pub(in super::super) context: windows::Win32::Graphics::Direct3D11::ID3D11DeviceContext,
    pub(in super::super) source_srv_cache:
        std::sync::Arc<std::sync::Mutex<ShaderResourceViewCache>>,
    pub(in super::super) luma_target: windows::Win32::Graphics::Direct3D11::ID3D11RenderTargetView,
    pub(in super::super) chroma_target:
        windows::Win32::Graphics::Direct3D11::ID3D11RenderTargetView,
    pub(in super::super) pq_lut:
        Option<windows::Win32::Graphics::Direct3D11::ID3D11ShaderResourceView>,
    pub(in super::super) luma_uav:
        Option<windows::Win32::Graphics::Direct3D11::ID3D11UnorderedAccessView>,
    pub(in super::super) chroma_uav:
        Option<windows::Win32::Graphics::Direct3D11::ID3D11UnorderedAccessView>,
    pub(in super::super) vertex_shader: windows::Win32::Graphics::Direct3D11::ID3D11VertexShader,
    pub(in super::super) luma_shader: windows::Win32::Graphics::Direct3D11::ID3D11PixelShader,
    pub(in super::super) chroma_shader: windows::Win32::Graphics::Direct3D11::ID3D11PixelShader,
    pub(in super::super) compute_shader:
        Option<windows::Win32::Graphics::Direct3D11::ID3D11ComputeShader>,
    pub(in super::super) luma_viewport: windows::Win32::Graphics::Direct3D11::D3D11_VIEWPORT,
    pub(in super::super) chroma_viewport: windows::Win32::Graphics::Direct3D11::D3D11_VIEWPORT,
    pub(in super::super) chroma_width: u32,
    pub(in super::super) chroma_height: u32,
}

#[cfg(windows)]
impl GpuP010Converter {
    #[allow(clippy::too_many_arguments)]
    pub(in super::super) unsafe fn new(
        route: VplRecordRoute,
        device: &windows::Win32::Graphics::Direct3D11::ID3D11Device,
        context: &windows::Win32::Graphics::Direct3D11::ID3D11DeviceContext,
        output: &windows::Win32::Graphics::Direct3D11::ID3D11Texture2D,
        width: u32,
        height: u32,
        enable_compute: bool,
        source_srv_cache: std::sync::Arc<std::sync::Mutex<ShaderResourceViewCache>>,
    ) -> Result<Self, BackendError> {
        use windows::Win32::Graphics::Direct3D11::{
            D3D11_RENDER_TARGET_VIEW_DESC1, D3D11_RENDER_TARGET_VIEW_DESC1_0,
            D3D11_RTV_DIMENSION_TEXTURE2D, D3D11_TEX2D_RTV1, D3D11_TEX2D_UAV1,
            D3D11_UAV_DIMENSION_TEXTURE2D, D3D11_UNORDERED_ACCESS_VIEW_DESC1,
            D3D11_UNORDERED_ACCESS_VIEW_DESC1_0, ID3D11Device3, ID3D11RenderTargetView,
            ID3D11RenderTargetView1, ID3D11Resource, ID3D11UnorderedAccessView1,
        };
        use windows::Win32::Graphics::Dxgi::Common::{
            DXGI_FORMAT_R16_UNORM, DXGI_FORMAT_R16G16_UNORM,
        };
        use windows::core::Interface;

        let device3: ID3D11Device3 = device.cast().map_err(|err| BackendError::WindowsApi {
            func: "ID3D11Device::cast<ID3D11Device3>(P010 plane converter)",
            message: err.to_string(),
        })?;
        let output_resource: ID3D11Resource =
            output.cast().map_err(|err| BackendError::WindowsApi {
                func: "ID3D11Texture2D::cast<ID3D11Resource>(P010 output)",
                message: err.to_string(),
            })?;

        let luma_desc = D3D11_RENDER_TARGET_VIEW_DESC1 {
            Format: DXGI_FORMAT_R16_UNORM,
            ViewDimension: D3D11_RTV_DIMENSION_TEXTURE2D,
            Anonymous: D3D11_RENDER_TARGET_VIEW_DESC1_0 {
                Texture2D: D3D11_TEX2D_RTV1 {
                    MipSlice: 0,
                    PlaneSlice: 0,
                },
            },
        };
        let mut luma_target1: Option<ID3D11RenderTargetView1> = None;
        device3
            .CreateRenderTargetView1(&output_resource, Some(&luma_desc), Some(&mut luma_target1))
            .map_err(|err| BackendError::WindowsApi {
                func: "ID3D11Device3::CreateRenderTargetView1(P010 luma)",
                message: err.to_string(),
            })?;
        let luma_target: ID3D11RenderTargetView = luma_target1
            .ok_or_else(|| BackendError::WindowsApi {
                func: "CreateRenderTargetView1(P010 luma)",
                message: "返回空 luma RTV".to_owned(),
            })?
            .cast()
            .map_err(|err| BackendError::WindowsApi {
                func: "ID3D11RenderTargetView1::cast(P010 luma)",
                message: err.to_string(),
            })?;

        let chroma_desc = D3D11_RENDER_TARGET_VIEW_DESC1 {
            Format: DXGI_FORMAT_R16G16_UNORM,
            ViewDimension: D3D11_RTV_DIMENSION_TEXTURE2D,
            Anonymous: D3D11_RENDER_TARGET_VIEW_DESC1_0 {
                Texture2D: D3D11_TEX2D_RTV1 {
                    MipSlice: 0,
                    PlaneSlice: 1,
                },
            },
        };
        let mut chroma_target1: Option<ID3D11RenderTargetView1> = None;
        device3
            .CreateRenderTargetView1(
                &output_resource,
                Some(&chroma_desc),
                Some(&mut chroma_target1),
            )
            .map_err(|err| BackendError::WindowsApi {
                func: "ID3D11Device3::CreateRenderTargetView1(P010 chroma)",
                message: err.to_string(),
            })?;
        let chroma_target: ID3D11RenderTargetView = chroma_target1
            .ok_or_else(|| BackendError::WindowsApi {
                func: "CreateRenderTargetView1(P010 chroma)",
                message: "返回空 chroma RTV".to_owned(),
            })?
            .cast()
            .map_err(|err| BackendError::WindowsApi {
                func: "ID3D11RenderTargetView1::cast(P010 chroma)",
                message: err.to_string(),
            })?;

        let transfer_lut = if route.is_hdr_pq() {
            Some(create_st2084_pq_lut_srv(device)?)
        } else {
            None
        };

        let shader_template = if route.is_hdr_pq() {
            P010_CONVERT_HLSL
        } else if route.is_bt2020_sdr() {
            P010_SDR_BT2020_CONVERT_HLSL
        } else {
            P010_SDR10_CONVERT_HLSL
        };
        let shader_source = shader_source_with_range(shader_template, route.mp4_color.full_range);
        let vs_blob = compile_shader(&shader_source, b"vs_main\0", b"vs_5_0\0")?;
        let luma_blob = compile_shader(&shader_source, b"ps_luma\0", b"ps_5_0\0")?;
        let chroma_blob = compile_shader(&shader_source, b"ps_chroma\0", b"ps_5_0\0")?;
        let compute_blob = if enable_compute {
            Some(compile_shader(&shader_source, b"cs_main\0", b"cs_5_0\0")?)
        } else {
            None
        };
        let mut vertex_shader = None;
        device
            .CreateVertexShader(&vs_blob, None, Some(&mut vertex_shader))
            .map_err(|err| BackendError::WindowsApi {
                func: "ID3D11Device::CreateVertexShader(P010 converter)",
                message: err.to_string(),
            })?;
        let mut luma_shader = None;
        device
            .CreatePixelShader(&luma_blob, None, Some(&mut luma_shader))
            .map_err(|err| BackendError::WindowsApi {
                func: "ID3D11Device::CreatePixelShader(P010 luma)",
                message: err.to_string(),
            })?;
        let mut chroma_shader = None;
        device
            .CreatePixelShader(&chroma_blob, None, Some(&mut chroma_shader))
            .map_err(|err| BackendError::WindowsApi {
                func: "ID3D11Device::CreatePixelShader(P010 chroma)",
                message: err.to_string(),
            })?;
        let mut compute_shader = None;
        if let Some(blob) = compute_blob {
            device
                .CreateComputeShader(&blob, None, Some(&mut compute_shader))
                .map_err(|err| BackendError::WindowsApi {
                    func: "ID3D11Device::CreateComputeShader(P010 compute)",
                    message: err.to_string(),
                })?;
        }

        let mut luma_uav = None;
        let mut chroma_uav = None;
        if enable_compute {
            let luma_uav_desc = D3D11_UNORDERED_ACCESS_VIEW_DESC1 {
                Format: DXGI_FORMAT_R16_UNORM,
                ViewDimension: D3D11_UAV_DIMENSION_TEXTURE2D,
                Anonymous: D3D11_UNORDERED_ACCESS_VIEW_DESC1_0 {
                    Texture2D: D3D11_TEX2D_UAV1 {
                        MipSlice: 0,
                        PlaneSlice: 0,
                    },
                },
            };
            let mut luma_uav1: Option<ID3D11UnorderedAccessView1> = None;
            device3
                .CreateUnorderedAccessView1(
                    &output_resource,
                    Some(&luma_uav_desc),
                    Some(&mut luma_uav1),
                )
                .map_err(|err| BackendError::WindowsApi {
                    func: "ID3D11Device3::CreateUnorderedAccessView1(P010 luma)",
                    message: err.to_string(),
                })?;
            luma_uav = Some(
                luma_uav1
                    .ok_or_else(|| BackendError::WindowsApi {
                        func: "CreateUnorderedAccessView1(P010 luma)",
                        message: "返回空 luma UAV".to_owned(),
                    })?
                    .cast()
                    .map_err(|err| BackendError::WindowsApi {
                        func: "ID3D11UnorderedAccessView1::cast(P010 luma)",
                        message: err.to_string(),
                    })?,
            );

            let chroma_uav_desc = D3D11_UNORDERED_ACCESS_VIEW_DESC1 {
                Format: DXGI_FORMAT_R16G16_UNORM,
                ViewDimension: D3D11_UAV_DIMENSION_TEXTURE2D,
                Anonymous: D3D11_UNORDERED_ACCESS_VIEW_DESC1_0 {
                    Texture2D: D3D11_TEX2D_UAV1 {
                        MipSlice: 0,
                        PlaneSlice: 1,
                    },
                },
            };
            let mut chroma_uav1: Option<ID3D11UnorderedAccessView1> = None;
            device3
                .CreateUnorderedAccessView1(
                    &output_resource,
                    Some(&chroma_uav_desc),
                    Some(&mut chroma_uav1),
                )
                .map_err(|err| BackendError::WindowsApi {
                    func: "ID3D11Device3::CreateUnorderedAccessView1(P010 chroma)",
                    message: err.to_string(),
                })?;
            chroma_uav = Some(
                chroma_uav1
                    .ok_or_else(|| BackendError::WindowsApi {
                        func: "CreateUnorderedAccessView1(P010 chroma)",
                        message: "返回空 chroma UAV".to_owned(),
                    })?
                    .cast()
                    .map_err(|err| BackendError::WindowsApi {
                        func: "ID3D11UnorderedAccessView1::cast(P010 chroma)",
                        message: err.to_string(),
                    })?,
            );
        }

        let chroma_width = (width / 2).max(1);
        let chroma_height = (height / 2).max(1);

        Ok(Self {
            device: device.clone(),
            context: context.clone(),
            source_srv_cache,
            luma_target,
            chroma_target,
            pq_lut: transfer_lut,
            luma_uav,
            chroma_uav,
            vertex_shader: vertex_shader.ok_or_else(|| BackendError::WindowsApi {
                func: "CreateVertexShader(P010 converter)",
                message: "返回空 VS".to_owned(),
            })?,
            luma_shader: luma_shader.ok_or_else(|| BackendError::WindowsApi {
                func: "CreatePixelShader(P010 luma)",
                message: "返回空 luma PS".to_owned(),
            })?,
            chroma_shader: chroma_shader.ok_or_else(|| BackendError::WindowsApi {
                func: "CreatePixelShader(P010 chroma)",
                message: "返回空 chroma PS".to_owned(),
            })?,
            compute_shader,
            luma_viewport: windows::Win32::Graphics::Direct3D11::D3D11_VIEWPORT {
                TopLeftX: 0.0,
                TopLeftY: 0.0,
                Width: width as f32,
                Height: height as f32,
                MinDepth: 0.0,
                MaxDepth: 1.0,
            },
            chroma_viewport: windows::Win32::Graphics::Direct3D11::D3D11_VIEWPORT {
                TopLeftX: 0.0,
                TopLeftY: 0.0,
                Width: (width / 2).max(1) as f32,
                Height: (height / 2).max(1) as f32,
                MinDepth: 0.0,
                MaxDepth: 1.0,
            },
            chroma_width,
            chroma_height,
        })
    }

    pub(in super::super) unsafe fn convert(
        &self,
        source: &windows::Win32::Graphics::Direct3D11::ID3D11Texture2D,
    ) -> Result<(), BackendError> {
        use windows::Win32::Graphics::Direct3D::D3D_PRIMITIVE_TOPOLOGY_TRIANGLELIST;
        use windows::Win32::Graphics::Direct3D11::{
            ID3D11ComputeShader, ID3D11RenderTargetView, ID3D11ShaderResourceView,
            ID3D11UnorderedAccessView,
        };

        let srv = lock_srv_cache(&self.source_srv_cache)?.get_or_create(
            &self.device,
            &self.context,
            source,
            "ID3D11Device::CreateShaderResourceView(P010 source)",
        )?;

        if let (Some(compute_shader), Some(luma_uav), Some(chroma_uav)) =
            (&self.compute_shader, &self.luma_uav, &self.chroma_uav)
        {
            self.context.CSSetShader(compute_shader, None);
            if let Some(pq_lut) = &self.pq_lut {
                self.context
                    .CSSetShaderResources(0, Some(&[Some(srv), Some(pq_lut.clone())]));
            } else {
                self.context.CSSetShaderResources(0, Some(&[Some(srv)]));
            }
            let uavs = [Some(luma_uav.clone()), Some(chroma_uav.clone())];
            self.context
                .CSSetUnorderedAccessViews(0, 2, Some(uavs.as_ptr()), None);
            self.context.Dispatch(
                self.chroma_width.div_ceil(8),
                self.chroma_height.div_ceil(8),
                1,
            );
            let empty_srv: [Option<ID3D11ShaderResourceView>; 2] = [None, None];
            self.context.CSSetShaderResources(0, Some(&empty_srv));
            let empty_uav: [Option<ID3D11UnorderedAccessView>; 2] = [None, None];
            self.context
                .CSSetUnorderedAccessViews(0, 2, Some(empty_uav.as_ptr()), None);
            self.context.CSSetShader(None::<&ID3D11ComputeShader>, None);
            return Ok(());
        }

        self.context
            .IASetPrimitiveTopology(D3D_PRIMITIVE_TOPOLOGY_TRIANGLELIST);
        self.context.VSSetShader(&self.vertex_shader, None);
        if let Some(pq_lut) = &self.pq_lut {
            self.context
                .PSSetShaderResources(0, Some(&[Some(srv), Some(pq_lut.clone())]));
        } else {
            self.context.PSSetShaderResources(0, Some(&[Some(srv)]));
        }

        self.context.RSSetViewports(Some(&[self.luma_viewport]));
        self.context.PSSetShader(&self.luma_shader, None);
        self.context
            .OMSetRenderTargets(Some(&[Some(self.luma_target.clone())]), None);
        self.context.Draw(3, 0);

        self.context.RSSetViewports(Some(&[self.chroma_viewport]));
        self.context.PSSetShader(&self.chroma_shader, None);
        self.context
            .OMSetRenderTargets(Some(&[Some(self.chroma_target.clone())]), None);
        self.context.Draw(3, 0);

        let empty_srv: [Option<ID3D11ShaderResourceView>; 2] = [None, None];
        self.context.PSSetShaderResources(0, Some(&empty_srv));
        let empty_rtv: [Option<ID3D11RenderTargetView>; 1] = [None];
        self.context.OMSetRenderTargets(Some(&empty_rtv), None);
        Ok(())
    }

    pub(in super::super) unsafe fn convert_dirty(
        &self,
        source: &windows::Win32::Graphics::Direct3D11::ID3D11Texture2D,
        dirty_rects: &[windows::Win32::Foundation::RECT],
    ) -> Result<(), BackendError> {
        if self.compute_shader.is_some() || dirty_rects.is_empty() {
            return self.convert(source);
        }

        use windows::Win32::Graphics::Direct3D::D3D_PRIMITIVE_TOPOLOGY_TRIANGLELIST;
        use windows::Win32::Graphics::Direct3D11::{
            D3D11_VIEWPORT, ID3D11RenderTargetView, ID3D11ShaderResourceView,
        };
        let srv = lock_srv_cache(&self.source_srv_cache)?.get_or_create(
            &self.device,
            &self.context,
            source,
            "ID3D11Device::CreateShaderResourceView(P010 dirty source)",
        )?;

        self.context
            .IASetPrimitiveTopology(D3D_PRIMITIVE_TOPOLOGY_TRIANGLELIST);
        self.context.VSSetShader(&self.vertex_shader, None);
        if let Some(pq_lut) = &self.pq_lut {
            self.context
                .PSSetShaderResources(0, Some(&[Some(srv), Some(pq_lut.clone())]));
        } else {
            self.context.PSSetShaderResources(0, Some(&[Some(srv)]));
        }

        self.context.PSSetShader(&self.luma_shader, None);
        self.context
            .OMSetRenderTargets(Some(&[Some(self.luma_target.clone())]), None);
        for rect in dirty_rects {
            if let Some(viewport) = luma_dirty_viewport(rect) {
                self.context.RSSetViewports(Some(&[viewport]));
                self.context.Draw(3, 0);
            }
        }

        self.context.PSSetShader(&self.chroma_shader, None);
        self.context
            .OMSetRenderTargets(Some(&[Some(self.chroma_target.clone())]), None);
        for rect in dirty_rects {
            if let Some(viewport) = chroma_dirty_viewport(rect) {
                self.context.RSSetViewports(Some(&[viewport]));
                self.context.Draw(3, 0);
            }
        }

        let empty_srv: [Option<ID3D11ShaderResourceView>; 2] = [None, None];
        self.context.PSSetShaderResources(0, Some(&empty_srv));
        let empty_rtv: [Option<ID3D11RenderTargetView>; 1] = [None];
        self.context.OMSetRenderTargets(Some(&empty_rtv), None);

        fn luma_dirty_viewport(rect: &windows::Win32::Foundation::RECT) -> Option<D3D11_VIEWPORT> {
            let left = rect.left.max(0) as f32;
            let top = rect.top.max(0) as f32;
            let width = (rect.right - rect.left).max(0) as f32;
            let height = (rect.bottom - rect.top).max(0) as f32;
            if width <= 0.0 || height <= 0.0 {
                return None;
            }
            Some(D3D11_VIEWPORT {
                TopLeftX: left,
                TopLeftY: top,
                Width: width,
                Height: height,
                MinDepth: 0.0,
                MaxDepth: 1.0,
            })
        }

        fn chroma_dirty_viewport(
            rect: &windows::Win32::Foundation::RECT,
        ) -> Option<D3D11_VIEWPORT> {
            let left = rect.left.max(0) & !1;
            let top = rect.top.max(0) & !1;
            let right = (rect.right.max(left + 1) + 1) & !1;
            let bottom = (rect.bottom.max(top + 1) + 1) & !1;
            let width = ((right - left) / 2).max(0) as f32;
            let height = ((bottom - top) / 2).max(0) as f32;
            if width <= 0.0 || height <= 0.0 {
                return None;
            }
            Some(D3D11_VIEWPORT {
                TopLeftX: (left / 2) as f32,
                TopLeftY: (top / 2) as f32,
                Width: width,
                Height: height,
                MinDepth: 0.0,
                MaxDepth: 1.0,
            })
        }

        Ok(())
    }
}

#[cfg(windows)]
pub(in super::super) unsafe fn create_st2084_pq_lut_srv(
    device: &windows::Win32::Graphics::Direct3D11::ID3D11Device,
) -> Result<windows::Win32::Graphics::Direct3D11::ID3D11ShaderResourceView, BackendError> {
    use windows::Win32::Graphics::Direct3D11::{
        D3D11_BIND_SHADER_RESOURCE, D3D11_SUBRESOURCE_DATA, D3D11_TEXTURE1D_DESC,
        D3D11_USAGE_IMMUTABLE, ID3D11Resource,
    };
    use windows::Win32::Graphics::Dxgi::Common::DXGI_FORMAT_R16_UNORM;
    use windows::core::Interface;

    let mut data = [0u16; ST2084_PQ_LUT_SIZE];
    for (i, value) in data.iter_mut().enumerate() {
        let normalized_luminance = st2084_pq_lut_normalized_luminance(i);
        let pq = st2084_pq_oetf_scalar(normalized_luminance);
        *value = (pq.clamp(0.0, 1.0) * 65535.0).round() as u16;
    }

    let desc = D3D11_TEXTURE1D_DESC {
        Width: ST2084_PQ_LUT_SIZE as u32,
        MipLevels: 1,
        ArraySize: 1,
        Format: DXGI_FORMAT_R16_UNORM,
        Usage: D3D11_USAGE_IMMUTABLE,
        BindFlags: D3D11_BIND_SHADER_RESOURCE.0 as u32,
        CPUAccessFlags: 0,
        MiscFlags: 0,
    };
    let initial = D3D11_SUBRESOURCE_DATA {
        pSysMem: data.as_ptr() as *const c_void,
        SysMemPitch: (ST2084_PQ_LUT_SIZE * std::mem::size_of::<u16>()) as u32,
        SysMemSlicePitch: 0,
    };
    let mut texture = None;
    device
        .CreateTexture1D(&desc, Some(&initial), Some(&mut texture))
        .map_err(|err| BackendError::WindowsApi {
            func: "ID3D11Device::CreateTexture1D(PQ LUT)",
            message: err.to_string(),
        })?;
    let texture = texture.ok_or_else(|| BackendError::WindowsApi {
        func: "CreateTexture1D(PQ LUT)",
        message: "返回空纹理".to_owned(),
    })?;
    let resource: ID3D11Resource = texture.cast().map_err(|err| BackendError::WindowsApi {
        func: "ID3D11Texture1D::cast<ID3D11Resource>(PQ LUT)",
        message: err.to_string(),
    })?;
    let mut srv = None;
    device
        .CreateShaderResourceView(&resource, None, Some(&mut srv))
        .map_err(|err| BackendError::WindowsApi {
            func: "ID3D11Device::CreateShaderResourceView(PQ LUT)",
            message: err.to_string(),
        })?;
    srv.ok_or_else(|| BackendError::WindowsApi {
        func: "CreateShaderResourceView(PQ LUT)",
        message: "返回空 SRV".to_owned(),
    })
}

pub(in super::super) const ST2084_PQ_LUT_SIZE: usize = 4096;

pub(in super::super) fn st2084_pq_lut_normalized_luminance(index: usize) -> f64 {
    let coordinate = index.min(ST2084_PQ_LUT_SIZE - 1) as f64 / (ST2084_PQ_LUT_SIZE - 1) as f64;
    coordinate.powi(4)
}

pub(in super::super) fn st2084_pq_oetf_scalar(normalized_luminance: f64) -> f64 {
    let n = normalized_luminance.clamp(0.0, 1.0);
    let m1 = 2610.0 / 16_384.0;
    let m2 = 2523.0 / 32.0;
    let c1 = 3424.0 / 4096.0;
    let c2 = 2413.0 / 128.0;
    let c3 = 2392.0 / 128.0;
    let n_pow = n.powf(m1);
    ((c1 + c2 * n_pow) / (1.0 + c3 * n_pow)).powf(m2)
}
