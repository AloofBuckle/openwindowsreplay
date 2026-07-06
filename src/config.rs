#![allow(dead_code)]
//! 全局配置模型。
//!
//! 文档要求 GUI 为引导式填参，因此这里把“可显示的配置”和“后端探测出的
//! 可用能力”分开：配置可以保存用户意图，真正开始录制前仍必须由后端再次验证。

use crate::rate_control::RateControlConfig;
use serde::{Deserialize, Serialize};
use std::path::PathBuf;

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(default)]
pub struct AppConfig {
    pub capture_backend: CaptureBackend,
    pub chroma: Option<ChromaSampling>,
    pub rate_control: RateControlConfig,
    pub cache_dir: String,
    pub save_dir: String,
    pub replay_minutes: f32,
}

impl Default for AppConfig {
    fn default() -> Self {
        Self {
            capture_backend: CaptureBackend::Wgc,
            chroma: None,
            rate_control: RateControlConfig::default(),
            cache_dir: "cache".to_owned(),
            save_dir: "replays".to_owned(),
            replay_minutes: 3.0,
        }
    }
}

impl AppConfig {
    pub fn config_path() -> PathBuf {
        let base = std::env::var_os("APPDATA")
            .map(PathBuf::from)
            .or_else(|| std::env::current_dir().ok())
            .unwrap_or_else(|| PathBuf::from("."));
        base.join("RustReplay").join("config.json")
    }

    pub fn load_from_disk() -> Result<Option<Self>, String> {
        let path = Self::config_path();
        if !path.exists() {
            return Ok(None);
        }
        let text = std::fs::read_to_string(&path)
            .map_err(|err| format!("读取配置文件 {} 失败：{err}", path.display()))?;
        serde_json::from_str::<Self>(&text)
            .map(Some)
            .map_err(|err| format!("解析配置文件 {} 失败：{err}", path.display()))
    }

    pub fn save_to_disk(&self) -> Result<(), String> {
        let path = Self::config_path();
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent)
                .map_err(|err| format!("创建配置目录 {} 失败：{err}", parent.display()))?;
        }
        let text =
            serde_json::to_string_pretty(self).map_err(|err| format!("序列化配置失败：{err}"))?;
        std::fs::write(&path, text)
            .map_err(|err| format!("写入配置文件 {} 失败：{err}", path.display()))
    }

    pub fn stable_json(&self) -> String {
        serde_json::to_string(self).unwrap_or_default()
    }
}

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
pub enum CaptureBackend {
    Dda,
    #[default]
    Wgc,
}

impl CaptureBackend {
    pub const fn all() -> [Self; 2] {
        [Self::Wgc, Self::Dda]
    }

    pub const fn label(self) -> &'static str {
        match self {
            Self::Dda => "DDA（不录光标）",
            Self::Wgc => "WGC（录制光标）",
        }
    }

    pub const fn short_name(self) -> &'static str {
        match self {
            Self::Dda => "DDA",
            Self::Wgc => "WGC",
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
