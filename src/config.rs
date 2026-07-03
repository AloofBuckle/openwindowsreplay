#![allow(dead_code)]
//! 全局配置模型。
//!
//! 文档要求 GUI 为引导式填参，因此这里把“可显示的配置”和“后端探测出的
//! 可用能力”分开：配置可以保存用户意图，真正开始录制前仍必须由后端再次验证。

use crate::rate_control::RateControlConfig;
use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AppConfig {
    pub chroma: Option<ChromaSampling>,
    pub rate_control: RateControlConfig,
    pub cache_dir: String,
    pub save_dir: String,
    pub replay_minutes: f32,
}

impl Default for AppConfig {
    fn default() -> Self {
        Self {
            chroma: None,
            rate_control: RateControlConfig::default(),
            cache_dir: "cache".to_owned(),
            save_dir: "replays".to_owned(),
            replay_minutes: 3.0,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
pub enum ChromaSampling {
    Yuv420,
    Yuv422,
    Yuv444,
}

impl ChromaSampling {
    pub const fn all() -> [Self; 3] {
        [Self::Yuv420, Self::Yuv422, Self::Yuv444]
    }

    pub const fn label(self) -> &'static str {
        match self {
            Self::Yuv420 => "4:2:0",
            Self::Yuv422 => "4:2:2",
            Self::Yuv444 => "4:4:4",
        }
    }

    pub const fn doc_label(self) -> &'static str {
        match self {
            Self::Yuv420 => "420",
            Self::Yuv422 => "422",
            Self::Yuv444 => "444",
        }
    }

    pub const fn expected_fourcc(self) -> &'static [&'static str] {
        match self {
            Self::Yuv420 => &["NV12", "P010"],
            Self::Yuv422 => &["YUY2", "Y210", "P210"],
            Self::Yuv444 => &["AYUV", "Y410", "RGB4"],
        }
    }
}
