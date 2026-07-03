#![allow(dead_code)]
//! 即时回放会话状态机。

use super::{ProbeCaps, pipeline};
use crate::config::AppConfig;
use crate::error::BackendError;
use std::time::Instant;

#[derive(Debug, Clone)]
pub enum ReplayState {
    Idle,
    Running { started_at: Instant },
}

#[derive(Debug, Clone)]
pub struct ReplayController {
    state: ReplayState,
}

impl Default for ReplayController {
    fn default() -> Self {
        Self {
            state: ReplayState::Idle,
        }
    }
}

impl ReplayController {
    pub fn state(&self) -> &ReplayState {
        &self.state
    }

    pub fn start(&mut self, config: &AppConfig, caps: &ProbeCaps) -> Result<(), BackendError> {
        let chroma = config.chroma.ok_or_else(|| {
            BackendError::unsupported("GUI 参数", "色度采样", "没有已验证的色度采样可选项")
        })?;

        if !caps.supported_chroma.contains(&chroma) {
            return Err(BackendError::unsupported(
                "创建录制会话",
                format!("色度采样 {}", chroma.doc_label()),
                "后端未报告该色度采样存在完整 GPU-only 路径，GUI 正常情况下会隐藏它",
            ));
        }

        if !caps
            .supported_rate_controls
            .contains(&config.rate_control.method)
        {
            return Err(BackendError::unsupported(
                "创建录制会话",
                format!("码控模式 {}", config.rate_control.method.short_name()),
                "oneVPL 能力探测未确认该 RateControlMethod 可用",
            ));
        }

        if !caps.desktop_sync_path_available {
            return Err(BackendError::unsupported(
                "创建录制会话",
                "当前显示器状态 + 捕获 + GPU 转换 + HEVC 硬编完整路径",
                caps.path_blockers
                    .first()
                    .cloned()
                    .unwrap_or_else(|| "没有可用桌面同步录制路径".to_owned()),
            ));
        }

        let request = pipeline::RecordingRequest {
            capture_backend: pipeline::CaptureBackendKind::WgcBgra8,
            color_transform: pipeline::ColorTransformKind::Sdr8ToYuv,
            chroma_writer: match chroma {
                crate::config::ChromaSampling::Yuv420 => pipeline::ChromaWriterKind::Nv12,
                crate::config::ChromaSampling::Yuv422 => pipeline::ChromaWriterKind::Yuy2,
                crate::config::ChromaSampling::Yuv444 => pipeline::ChromaWriterKind::Ayuv,
            },
            rate_control: config.rate_control.clone(),
            replay_minutes: config.replay_minutes,
        };

        // 防御性保留：即便未来 supported_chroma 被放开，当前流水线仍必须拒绝，直到真正接入。
        Err(pipeline::unsupported_until_zero_copy_verified(&request))
    }

    pub fn stop(&mut self) -> Result<(), BackendError> {
        match self.state {
            ReplayState::Idle => Err(BackendError::unsupported(
                "停止即时回放",
                "当前会话",
                "当前没有正在运行的录制会话",
            )),
            ReplayState::Running { .. } => {
                self.state = ReplayState::Idle;
                Ok(())
            }
        }
    }

    pub fn save(&mut self, _config: &AppConfig) -> Result<(), BackendError> {
        match self.state {
            ReplayState::Idle => Err(BackendError::unsupported(
                "保存即时回放",
                "MP4 输出",
                "当前没有正在运行的录制会话，也没有可保存的编码环形缓存",
            )),
            ReplayState::Running { .. } => Err(BackendError::unsupported(
                "保存即时回放",
                "HEVC/AAC -> MP4",
                "MP4 muxer 只允许消费已编码码流；当前尚未产生可封装的 HEVC/AAC 包",
            )),
        }
    }
}
