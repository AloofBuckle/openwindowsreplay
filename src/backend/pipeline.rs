#![allow(dead_code)]
//! 录制流水线契约。
//!
//! 本文件定义 DDA/WGC、GPU 色彩转换、色度写入、oneVPL 编码、WASAPI/AAC、MP4
//! 封装之间的 Rust 侧接口。当前构建在无法证明 zero-copy D3D11 import 前不会创建
//! 真实会话，避免引入任何 CPU raw frame 回退。

use crate::config::ChromaSampling;
use crate::error::BackendError;
use crate::rate_control::RateControlConfig;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CaptureBackendKind {
    Dda,
    WgcBgra8,
    WgcFp16,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ColorTransformKind {
    Sdr8ToYuv,
    Sdr10ToYuv10,
    HdrPq10ToYuv10,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ChromaWriterKind {
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
            Self::Nv12 | Self::P010 => ChromaSampling::Yuv420,
            Self::Yuy2 | Self::Y210 | Self::P210 => ChromaSampling::Yuv422,
            Self::Ayuv | Self::Y410 | Self::Rgb4 => ChromaSampling::Yuv444,
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

pub fn unsupported_until_zero_copy_verified(request: &RecordingRequest) -> BackendError {
    BackendError::unsupported(
        "录制流水线",
        format!(
            "{:?} -> {:?} -> {:?} / {:?}",
            request.capture_backend,
            request.color_transform,
            request.chroma_writer,
            request.rate_control.method
        ),
        "尚未完成 shared D3D11 surface import 实测；按文档要求禁止创建会话或改用 CPU 回退",
    )
}
