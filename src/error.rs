#![allow(dead_code)]
//! 后端错误类型。
//!
//! 设计原则：只要无法保持“未编码视频帧路径全程 GPU-only”，就明确返回
//! `UnsupportedGpuPath`，不偷偷改用 CPU 回退路径。

use thiserror::Error;

#[derive(Debug, Error, Clone)]
pub enum BackendError {
    #[error("操作已取消: 阶段={stage}")]
    Cancelled { stage: String },

    #[error("录制环境需要重新探测: {reason}")]
    ReconfigureRequired { reason: String },

    #[error("UnsupportedGpuPath: 阶段={stage} 请求={requested} 原因={reason}")]
    UnsupportedGpuPath {
        stage: String,
        requested: String,
        reason: String,
    },

    #[error("oneVPL 调用失败: {func} 返回 status={status}")]
    VplStatus { func: &'static str, status: i32 },

    #[error("Windows API 调用失败: {func}: {message}")]
    WindowsApi { func: &'static str, message: String },

    #[error("音频路径不支持: {reason}")]
    AudioUnsupported { reason: String },

    #[error("IO 错误: {0}")]
    Io(String),
}

impl BackendError {
    pub fn cancelled(stage: impl Into<String>) -> Self {
        Self::Cancelled {
            stage: stage.into(),
        }
    }

    pub const fn is_cancelled(&self) -> bool {
        matches!(self, Self::Cancelled { .. })
    }

    pub const fn is_reconfigure_required(&self) -> bool {
        matches!(self, Self::ReconfigureRequired { .. })
    }

    pub fn reconfigure_required(reason: impl Into<String>) -> Self {
        Self::ReconfigureRequired {
            reason: reason.into(),
        }
    }

    pub fn unsupported(
        stage: impl Into<String>,
        requested: impl Into<String>,
        reason: impl Into<String>,
    ) -> Self {
        Self::UnsupportedGpuPath {
            stage: stage.into(),
            requested: requested.into(),
            reason: reason.into(),
        }
    }
}
