#![allow(dead_code)]
//! 录制流水线契约。
//!
//! 本文件定义 DDA/WGC、GPU 色彩转换、色度写入、oneVPL 编码、WASAPI/AAC、MP4
//! 封装之间的 Rust 侧接口。当前成品路径允许一次 GPU CopyResource 送入 oneVPL
//! 内部分配 surface，但仍禁止任何 raw frame CPU 回读/Map/Staging 回退。

use super::ProbeCaps;
use crate::config::ChromaSampling;
use crate::error::BackendError;
use crate::rate_control::RateControlConfig;
use std::path::Path;
use std::sync::Arc;
use std::sync::atomic::AtomicBool;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CaptureBackendKind {
    Dda,
    Wgc,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ColorTransformKind {
    /// 后端根据当前显示器状态和 DDA/WGC 实际输入自动选择 SDR8/SDR10/HDRPQ10 路线。
    AutoFromDisplay,
    Sdr8ToYuv,
    Sdr10ToYuv10,
    HdrPq10ToYuv10,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ChromaWriterKind {
    /// 只表达用户请求 4:2:0；具体 NV12/P010 由录制后端 RoutePlan 自动选择。
    Auto420,
    /// 只表达用户请求 4:2:2；具体 YUY2/Y210/P210 可用性由后端 RoutePlan/Query 决定。
    Auto422,
    /// 只表达用户请求 4:4:4；具体 AYUV/Y410/RGB4 由后端 RoutePlan/Query 决定。
    Auto444,
    Nv12,
    P010,
    Yuy2,
    Y210,
    P210,
    Ayuv,
    Y410,
    Rgb4,
}

impl ChromaWriterKind {
    pub const fn chroma(self) -> ChromaSampling {
        match self {
            Self::Auto420 | Self::Nv12 | Self::P010 => ChromaSampling::Yuv420,
            Self::Auto422 | Self::Yuy2 | Self::Y210 | Self::P210 => ChromaSampling::Yuv422,
            Self::Auto444 | Self::Ayuv | Self::Y410 | Self::Rgb4 => ChromaSampling::Yuv444,
        }
    }
}

#[derive(Debug, Clone)]
pub struct RecordingRequest {
    pub capture_backend: CaptureBackendKind,
    pub color_transform: ColorTransformKind,
    pub chroma_writer: ChromaWriterKind,
    pub rate_control: RateControlConfig,
    pub replay_minutes: f32,
}

#[derive(Debug, Clone)]
pub struct GpuFrameSurface {
    /// 绝对时间戳，单位纳秒。VFR/音画同步禁止用帧号替代。
    pub timestamp_ns: u64,
    /// DXGI adapter LUID 的文本表示。
    pub adapter_luid: String,
    /// 真实实现中这里必须是 ID3D11Texture2D 指针；当前原型不持有 COM 指针。
    pub d3d11_texture_debug: String,
}

pub trait CaptureStage {
    fn next_frame(&mut self) -> Result<GpuFrameSurface, BackendError>;
}

pub trait ColorTransformStage {
    fn transform(&mut self, input: &GpuFrameSurface) -> Result<GpuFrameSurface, BackendError>;
}

pub trait ChromaWriterStage {
    fn write_chroma(&mut self, input: &GpuFrameSurface) -> Result<GpuFrameSurface, BackendError>;
}

pub trait VplEncoderStage {
    fn encode_gpu_surface(&mut self, input: &GpuFrameSurface) -> Result<(), BackendError>;
}

pub fn record_once_gpu_only(
    request: &RecordingRequest,
    caps: &ProbeCaps,
    output: &Path,
    duration_seconds: f32,
    adapter_index: u32,
) -> Result<super::vpl::VplOneCopyRecordReport, BackendError> {
    record_once_gpu_only_cancelable(request, caps, output, duration_seconds, adapter_index, None)
}

pub fn record_once_gpu_only_cancelable(
    request: &RecordingRequest,
    caps: &ProbeCaps,
    output: &Path,
    duration_seconds: f32,
    adapter_index: u32,
    external_stop: Option<Arc<AtomicBool>>,
) -> Result<super::vpl::VplOneCopyRecordReport, BackendError> {
    #[cfg(windows)]
    {
        Ok(record_once_gpu_only_output_cancelable(
            request,
            caps,
            output,
            duration_seconds,
            adapter_index,
            external_stop,
        )?
        .report)
    }

    #[cfg(not(windows))]
    {
        if !caps.d3d11_texture_input_supported {
            return Err(BackendError::unsupported(
                "oneVPL 编码",
                "D3D11 texture 输入",
                "oneVPL 能力探测未确认 MFX_RESOURCE_DX11_TEXTURE",
            ));
        }
        let requested_chroma = request.chroma_writer.chroma();
        if !caps.supported_chroma.contains(&requested_chroma) {
            let reason = caps
                .route_blocker_summary_for_chroma(requested_chroma)
                .unwrap_or_else(|| {
                    "能力探测未确认该色度采样存在完整 GPU-only 生产 route".to_owned()
                });
            return Err(BackendError::unsupported(
                "录制流水线",
                requested_chroma.doc_label(),
                reason,
            ));
        }
        caps.validate_rate_control_config(requested_chroma, &request.rate_control, "录制流水线")?;
        match request.capture_backend {
            CaptureBackendKind::Dda => super::vpl::record_d3d11_onecopy_mp4_cancelable(
                adapter_index,
                output,
                duration_seconds,
                &request.rate_control,
                requested_chroma,
                external_stop,
            ),
            CaptureBackendKind::Wgc => super::vpl::record_wgc_d3d11_onecopy_mp4_cancelable(
                adapter_index,
                output,
                duration_seconds,
                &request.rate_control,
                requested_chroma,
                external_stop,
            ),
        }
    }
}

#[cfg(windows)]
pub fn record_once_gpu_only_output_cancelable(
    request: &RecordingRequest,
    caps: &ProbeCaps,
    output: &Path,
    duration_seconds: f32,
    adapter_index: u32,
    external_stop: Option<Arc<AtomicBool>>,
) -> Result<super::vpl::VplOneCopyRecordOutput, BackendError> {
    validate_record_request(request, caps)?;
    let requested_chroma = request.chroma_writer.chroma();
    match caps.video_encoder_selection.active {
        Some(super::VideoEncoderBackend::Nvenc) => {
            let route_plan = caps
                .preferred_nvenc_route_for_chroma(requested_chroma)
                .filter(|route| route.adapter_index == adapter_index);
            match request.capture_backend {
                CaptureBackendKind::Dda => {
                    super::vpl::record_nvenc_d3d11_onecopy_mp4_output_cancelable(
                        adapter_index,
                        output,
                        duration_seconds,
                        &request.rate_control,
                        requested_chroma,
                        external_stop,
                        route_plan,
                    )
                }
                CaptureBackendKind::Wgc => {
                    super::vpl::record_nvenc_wgc_d3d11_onecopy_mp4_output_cancelable(
                        adapter_index,
                        output,
                        duration_seconds,
                        &request.rate_control,
                        requested_chroma,
                        external_stop,
                        route_plan,
                    )
                }
            }
        }
        Some(super::VideoEncoderBackend::OneVpl) => {
            let route_plan = caps
                .preferred_vpl_route_for_chroma(requested_chroma)
                .filter(|route| route.adapter_index == adapter_index);
            match request.capture_backend {
                CaptureBackendKind::Dda => {
                    super::vpl::record_d3d11_onecopy_mp4_output_with_route_cancelable(
                        adapter_index,
                        output,
                        duration_seconds,
                        &request.rate_control,
                        requested_chroma,
                        external_stop,
                        route_plan,
                    )
                }
                CaptureBackendKind::Wgc => {
                    super::vpl::record_wgc_d3d11_onecopy_mp4_output_with_route_cancelable(
                        adapter_index,
                        output,
                        duration_seconds,
                        &request.rate_control,
                        requested_chroma,
                        external_stop,
                        route_plan,
                    )
                }
            }
        }
        None => Err(BackendError::unsupported(
            "录制流水线",
            "视频编码器自动选择",
            "能力探测没有选出 production_ready 的 oneVPL/NVENC 后端",
        )),
    }
}

#[cfg(windows)]
pub fn record_once_gpu_only_memory_output_cancelable(
    request: &RecordingRequest,
    caps: &ProbeCaps,
    output: &Path,
    duration_seconds: f32,
    adapter_index: u32,
    external_stop: Option<Arc<AtomicBool>>,
) -> Result<super::vpl::VplOneCopyRecordOutput, BackendError> {
    record_once_gpu_only_memory_output_with_sink_cancelable(
        request,
        caps,
        output,
        duration_seconds,
        adapter_index,
        external_stop,
        None,
    )
}

#[cfg(windows)]
pub fn record_once_gpu_only_memory_output_with_sink_cancelable(
    request: &RecordingRequest,
    caps: &ProbeCaps,
    output: &Path,
    duration_seconds: f32,
    adapter_index: u32,
    external_stop: Option<Arc<AtomicBool>>,
    encoded_sink: Option<&mut dyn super::vpl::VplOneCopyRecordSink>,
) -> Result<super::vpl::VplOneCopyRecordOutput, BackendError> {
    validate_record_request(request, caps)?;
    let requested_chroma = request.chroma_writer.chroma();
    match caps.video_encoder_selection.active {
        Some(super::VideoEncoderBackend::Nvenc) => {
            let route_plan = caps
                .preferred_nvenc_route_for_chroma(requested_chroma)
                .filter(|route| route.adapter_index == adapter_index);
            match request.capture_backend {
                CaptureBackendKind::Dda => {
                    super::vpl::record_nvenc_d3d11_onecopy_memory_output_with_sink_cancelable(
                        adapter_index,
                        output,
                        duration_seconds,
                        &request.rate_control,
                        requested_chroma,
                        external_stop,
                        encoded_sink,
                        route_plan,
                    )
                }
                CaptureBackendKind::Wgc => {
                    super::vpl::record_nvenc_wgc_d3d11_onecopy_memory_output_with_sink_cancelable(
                        adapter_index,
                        output,
                        duration_seconds,
                        &request.rate_control,
                        requested_chroma,
                        external_stop,
                        encoded_sink,
                        route_plan,
                    )
                }
            }
        }
        Some(super::VideoEncoderBackend::OneVpl) => {
            let route_plan = caps
                .preferred_vpl_route_for_chroma(requested_chroma)
                .filter(|route| route.adapter_index == adapter_index);
            match request.capture_backend {
                CaptureBackendKind::Dda => {
                    super::vpl::record_d3d11_onecopy_memory_output_with_sink_cancelable(
                        adapter_index,
                        output,
                        duration_seconds,
                        &request.rate_control,
                        requested_chroma,
                        external_stop,
                        encoded_sink,
                        route_plan,
                    )
                }
                CaptureBackendKind::Wgc => {
                    super::vpl::record_wgc_d3d11_onecopy_memory_output_with_sink_cancelable(
                        adapter_index,
                        output,
                        duration_seconds,
                        &request.rate_control,
                        requested_chroma,
                        external_stop,
                        encoded_sink,
                        route_plan,
                    )
                }
            }
        }
        None => Err(BackendError::unsupported(
            "录制流水线",
            "视频编码器自动选择",
            "能力探测没有选出 production_ready 的 oneVPL/NVENC 后端",
        )),
    }
}

fn validate_record_request(
    request: &RecordingRequest,
    caps: &ProbeCaps,
) -> Result<(), BackendError> {
    if !caps.d3d11_texture_input_supported {
        return Err(BackendError::unsupported(
            "GPU 视频编码",
            "D3D11 texture 输入",
            "active oneVPL/NVENC 后端能力探测未确认 D3D11 texture 输入",
        ));
    }
    let requested_chroma = request.chroma_writer.chroma();
    if !caps.supported_chroma.contains(&requested_chroma) {
        let reason = caps
            .route_blocker_summary_for_chroma(requested_chroma)
            .unwrap_or_else(|| "能力探测未确认该色度采样存在完整 GPU-only 生产 route".to_owned());
        return Err(BackendError::unsupported(
            "录制流水线",
            requested_chroma.doc_label(),
            reason,
        ));
    }
    caps.validate_rate_control_config(requested_chroma, &request.rate_control, "录制流水线")?;
    Ok(())
}
