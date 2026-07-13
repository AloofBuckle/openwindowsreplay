use super::*;

#[cfg(windows)]
pub(in super::super) struct VideoProcessorBlitter {
    pub(in super::super) input_view:
        windows::Win32::Graphics::Direct3D11::ID3D11VideoProcessorInputView,
    pub(in super::super) output_view:
        windows::Win32::Graphics::Direct3D11::ID3D11VideoProcessorOutputView,
}

#[cfg(windows)]
impl VideoProcessorBlitter {
    pub(in super::super) unsafe fn new(
        video_device: &windows::Win32::Graphics::Direct3D11::ID3D11VideoDevice,
        enumerator: &windows::Win32::Graphics::Direct3D11::ID3D11VideoProcessorEnumerator,
        source: &windows::Win32::Graphics::Direct3D11::ID3D11Texture2D,
        target: &windows::Win32::Graphics::Direct3D11::ID3D11Texture2D,
    ) -> Result<Self, BackendError> {
        use windows::Win32::Graphics::Direct3D11::{
            D3D11_TEX2D_VPIV, D3D11_TEX2D_VPOV, D3D11_VIDEO_PROCESSOR_INPUT_VIEW_DESC,
            D3D11_VIDEO_PROCESSOR_INPUT_VIEW_DESC_0, D3D11_VIDEO_PROCESSOR_OUTPUT_VIEW_DESC,
            D3D11_VIDEO_PROCESSOR_OUTPUT_VIEW_DESC_0, D3D11_VPIV_DIMENSION_TEXTURE2D,
            D3D11_VPOV_DIMENSION_TEXTURE2D, ID3D11Resource, ID3D11VideoProcessorInputView,
            ID3D11VideoProcessorOutputView,
        };
        use windows::core::Interface;

        let source_resource: ID3D11Resource =
            source.cast().map_err(|err| BackendError::WindowsApi {
                func: "ID3D11Texture2D::cast<ID3D11Resource>(cached VP source)",
                message: err.to_string(),
            })?;
        let target_resource: ID3D11Resource =
            target.cast().map_err(|err| BackendError::WindowsApi {
                func: "ID3D11Texture2D::cast<ID3D11Resource>(cached VP target)",
                message: err.to_string(),
            })?;

        let input_desc = D3D11_VIDEO_PROCESSOR_INPUT_VIEW_DESC {
            FourCC: 0,
            ViewDimension: D3D11_VPIV_DIMENSION_TEXTURE2D,
            Anonymous: D3D11_VIDEO_PROCESSOR_INPUT_VIEW_DESC_0 {
                Texture2D: D3D11_TEX2D_VPIV {
                    MipSlice: 0,
                    ArraySlice: 0,
                },
            },
        };
        let mut input_view: Option<ID3D11VideoProcessorInputView> = None;
        video_device
            .CreateVideoProcessorInputView(
                &source_resource,
                enumerator,
                &input_desc,
                Some(&mut input_view),
            )
            .map_err(|err| BackendError::WindowsApi {
                func: "ID3D11VideoDevice::CreateVideoProcessorInputView(cached)",
                message: err.to_string(),
            })?;
        let input_view = input_view.ok_or_else(|| BackendError::WindowsApi {
            func: "CreateVideoProcessorInputView(cached)",
            message: "返回空 input view".to_owned(),
        })?;

        let output_desc = D3D11_VIDEO_PROCESSOR_OUTPUT_VIEW_DESC {
            ViewDimension: D3D11_VPOV_DIMENSION_TEXTURE2D,
            Anonymous: D3D11_VIDEO_PROCESSOR_OUTPUT_VIEW_DESC_0 {
                Texture2D: D3D11_TEX2D_VPOV { MipSlice: 0 },
            },
        };
        let mut output_view: Option<ID3D11VideoProcessorOutputView> = None;
        video_device
            .CreateVideoProcessorOutputView(
                &target_resource,
                enumerator,
                &output_desc,
                Some(&mut output_view),
            )
            .map_err(|err| BackendError::WindowsApi {
                func: "ID3D11VideoDevice::CreateVideoProcessorOutputView(cached)",
                message: err.to_string(),
            })?;
        let output_view = output_view.ok_or_else(|| BackendError::WindowsApi {
            func: "CreateVideoProcessorOutputView(cached)",
            message: "返回空 output view".to_owned(),
        })?;

        Ok(Self {
            input_view,
            output_view,
        })
    }

    pub(in super::super) unsafe fn blit(
        &self,
        video_context: &windows::Win32::Graphics::Direct3D11::ID3D11VideoContext,
        processor: &windows::Win32::Graphics::Direct3D11::ID3D11VideoProcessor,
    ) -> Result<(), BackendError> {
        use std::mem::ManuallyDrop;
        use windows::Win32::Graphics::Direct3D11::D3D11_VIDEO_PROCESSOR_STREAM;

        let mut stream = D3D11_VIDEO_PROCESSOR_STREAM {
            Enable: windows::core::BOOL(1),
            OutputIndex: 0,
            InputFrameOrField: 0,
            PastFrames: 0,
            FutureFrames: 0,
            ppPastSurfaces: ptr::null_mut(),
            pInputSurface: ManuallyDrop::new(Some(self.input_view.clone())),
            ppFutureSurfaces: ptr::null_mut(),
            ppPastSurfacesRight: ptr::null_mut(),
            pInputSurfaceRight: ManuallyDrop::new(None),
            ppFutureSurfacesRight: ptr::null_mut(),
        };
        let blt_result = video_context.VideoProcessorBlt(
            processor,
            &self.output_view,
            0,
            std::slice::from_ref(&stream),
        );
        ManuallyDrop::drop(&mut stream.pInputSurface);
        blt_result.map_err(|err| BackendError::WindowsApi {
            func: "ID3D11VideoContext::VideoProcessorBlt(cached)",
            message: err.to_string(),
        })
    }
}

#[cfg(windows)]
#[derive(Clone, Hash, PartialEq, Eq)]
pub(in super::super) struct ShaderCompileKey {
    pub(in super::super) source: String,
    pub(in super::super) entry: Vec<u8>,
    pub(in super::super) target: Vec<u8>,
}

#[cfg(windows)]
pub(in super::super) static SHADER_BYTECODE_CACHE: std::sync::OnceLock<
    std::sync::Mutex<HashMap<ShaderCompileKey, std::sync::Arc<[u8]>>>,
> = std::sync::OnceLock::new();

#[cfg(windows)]
pub(in super::super) unsafe fn compile_shader(
    source: &str,
    entry: &[u8],
    target: &[u8],
) -> Result<std::sync::Arc<[u8]>, BackendError> {
    use windows::Win32::Graphics::Direct3D::Fxc::D3DCompile;
    use windows::Win32::Graphics::Direct3D::{ID3DBlob, ID3DInclude};
    use windows::core::PCSTR;

    let key = ShaderCompileKey {
        source: source.to_owned(),
        entry: entry.to_vec(),
        target: target.to_vec(),
    };
    let cache = SHADER_BYTECODE_CACHE.get_or_init(|| std::sync::Mutex::new(HashMap::new()));
    if let Ok(cache) = cache.lock()
        && let Some(code) = cache.get(&key)
    {
        return Ok(code.clone());
    }

    let mut code: Option<ID3DBlob> = None;
    let mut errors: Option<ID3DBlob> = None;
    let result = D3DCompile(
        source.as_ptr() as *const c_void,
        source.len(),
        PCSTR::null(),
        None,
        Option::<&ID3DInclude>::None,
        PCSTR(entry.as_ptr()),
        PCSTR(target.as_ptr()),
        0,
        0,
        &mut code,
        Some(&mut errors),
    );
    if let Err(err) = result {
        let message = if let Some(errors) = errors {
            let ptr = errors.GetBufferPointer() as *const u8;
            let len = errors.GetBufferSize();
            String::from_utf8_lossy(std::slice::from_raw_parts(ptr, len)).to_string()
        } else {
            err.to_string()
        };
        return Err(BackendError::WindowsApi {
            func: "D3DCompile",
            message,
        });
    }
    let code = code.ok_or_else(|| BackendError::WindowsApi {
        func: "D3DCompile",
        message: "返回空 shader blob".to_owned(),
    })?;
    let ptr = code.GetBufferPointer() as *const u8;
    let len = code.GetBufferSize();
    let bytes: std::sync::Arc<[u8]> = std::slice::from_raw_parts(ptr, len).to_vec().into();
    if let Ok(mut cache) = cache.lock() {
        cache.insert(key, bytes.clone());
    }
    Ok(bytes)
}

#[cfg(windows)]
pub(in super::super) fn shader_source_with_range(template: &str, full_range: bool) -> String {
    template.replace(
        "RR_FULL_RANGE_PLACEHOLDER",
        if full_range { "true" } else { "false" },
    )
}

#[cfg(windows)]
pub(in super::super) unsafe fn process_with_video_processor(
    video_device: &windows::Win32::Graphics::Direct3D11::ID3D11VideoDevice,
    video_context: &windows::Win32::Graphics::Direct3D11::ID3D11VideoContext,
    enumerator: &windows::Win32::Graphics::Direct3D11::ID3D11VideoProcessorEnumerator,
    processor: &windows::Win32::Graphics::Direct3D11::ID3D11VideoProcessor,
    source: &windows::Win32::Graphics::Direct3D11::ID3D11Texture2D,
    target: &windows::Win32::Graphics::Direct3D11::ID3D11Texture2D,
) -> Result<(), BackendError> {
    use std::mem::ManuallyDrop;
    use windows::Win32::Graphics::Direct3D11::{
        D3D11_TEX2D_VPIV, D3D11_TEX2D_VPOV, D3D11_VIDEO_PROCESSOR_INPUT_VIEW_DESC,
        D3D11_VIDEO_PROCESSOR_INPUT_VIEW_DESC_0, D3D11_VIDEO_PROCESSOR_OUTPUT_VIEW_DESC,
        D3D11_VIDEO_PROCESSOR_OUTPUT_VIEW_DESC_0, D3D11_VIDEO_PROCESSOR_STREAM,
        D3D11_VPIV_DIMENSION_TEXTURE2D, D3D11_VPOV_DIMENSION_TEXTURE2D, ID3D11Resource,
        ID3D11VideoProcessorInputView, ID3D11VideoProcessorOutputView,
    };
    use windows::core::Interface;

    let source_resource: ID3D11Resource =
        source.cast().map_err(|err| BackendError::WindowsApi {
            func: "ID3D11Texture2D::cast<ID3D11Resource>(source)",
            message: err.to_string(),
        })?;
    let target_resource: ID3D11Resource =
        target.cast().map_err(|err| BackendError::WindowsApi {
            func: "ID3D11Texture2D::cast<ID3D11Resource>(target)",
            message: err.to_string(),
        })?;

    let input_desc = D3D11_VIDEO_PROCESSOR_INPUT_VIEW_DESC {
        FourCC: 0,
        ViewDimension: D3D11_VPIV_DIMENSION_TEXTURE2D,
        Anonymous: D3D11_VIDEO_PROCESSOR_INPUT_VIEW_DESC_0 {
            Texture2D: D3D11_TEX2D_VPIV {
                MipSlice: 0,
                ArraySlice: 0,
            },
        },
    };
    let mut input_view: Option<ID3D11VideoProcessorInputView> = None;
    video_device
        .CreateVideoProcessorInputView(
            &source_resource,
            enumerator,
            &input_desc,
            Some(&mut input_view),
        )
        .map_err(|err| BackendError::WindowsApi {
            func: "ID3D11VideoDevice::CreateVideoProcessorInputView",
            message: err.to_string(),
        })?;
    let input_view = input_view.ok_or_else(|| BackendError::WindowsApi {
        func: "CreateVideoProcessorInputView",
        message: "返回空 input view".to_owned(),
    })?;

    let output_desc = D3D11_VIDEO_PROCESSOR_OUTPUT_VIEW_DESC {
        ViewDimension: D3D11_VPOV_DIMENSION_TEXTURE2D,
        Anonymous: D3D11_VIDEO_PROCESSOR_OUTPUT_VIEW_DESC_0 {
            Texture2D: D3D11_TEX2D_VPOV { MipSlice: 0 },
        },
    };
    let mut output_view: Option<ID3D11VideoProcessorOutputView> = None;
    video_device
        .CreateVideoProcessorOutputView(
            &target_resource,
            enumerator,
            &output_desc,
            Some(&mut output_view),
        )
        .map_err(|err| BackendError::WindowsApi {
            func: "ID3D11VideoDevice::CreateVideoProcessorOutputView",
            message: err.to_string(),
        })?;
    let output_view = output_view.ok_or_else(|| BackendError::WindowsApi {
        func: "CreateVideoProcessorOutputView",
        message: "返回空 output view".to_owned(),
    })?;

    let mut stream = D3D11_VIDEO_PROCESSOR_STREAM {
        Enable: windows::core::BOOL(1),
        OutputIndex: 0,
        InputFrameOrField: 0,
        PastFrames: 0,
        FutureFrames: 0,
        ppPastSurfaces: ptr::null_mut(),
        pInputSurface: ManuallyDrop::new(Some(input_view)),
        ppFutureSurfaces: ptr::null_mut(),
        ppPastSurfacesRight: ptr::null_mut(),
        pInputSurfaceRight: ManuallyDrop::new(None),
        ppFutureSurfacesRight: ptr::null_mut(),
    };
    let blt_result =
        video_context.VideoProcessorBlt(processor, &output_view, 0, std::slice::from_ref(&stream));
    ManuallyDrop::drop(&mut stream.pInputSurface);
    blt_result.map_err(|err| BackendError::WindowsApi {
        func: "ID3D11VideoContext::VideoProcessorBlt",
        message: err.to_string(),
    })
}
