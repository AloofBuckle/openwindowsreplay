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
    /// 只允许用户修改“保存即时回放/重放”这一项热键。
    pub save_hotkey: HotkeyConfig,
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
            save_hotkey: HotkeyConfig::default(),
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

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub struct HotkeyConfig {
    pub ctrl: bool,
    pub alt: bool,
    pub shift: bool,
    pub key: HotkeyKey,
}

impl Default for HotkeyConfig {
    fn default() -> Self {
        Self {
            ctrl: false,
            alt: false,
            shift: false,
            key: HotkeyKey::F9,
        }
    }
}

impl HotkeyConfig {
    pub fn label(self) -> String {
        let mut parts = Vec::with_capacity(4);
        if self.ctrl {
            parts.push("Ctrl");
        }
        if self.alt {
            parts.push("Alt");
        }
        if self.shift {
            parts.push("Shift");
        }
        parts.push(self.key.label());
        parts.join("+")
    }

    pub const fn has_modifier(self) -> bool {
        self.ctrl || self.alt || self.shift
    }

    pub const fn is_safe_global_binding(self) -> bool {
        self.key.is_function_key() || self.has_modifier()
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum HotkeyKey {
    Num0,
    Num1,
    Num2,
    Num3,
    Num4,
    Num5,
    Num6,
    Num7,
    Num8,
    Num9,
    A,
    B,
    C,
    D,
    E,
    F,
    G,
    H,
    I,
    J,
    K,
    L,
    M,
    N,
    O,
    P,
    Q,
    R,
    S,
    T,
    U,
    V,
    W,
    X,
    Y,
    Z,
    F1,
    F2,
    F3,
    F4,
    F5,
    F6,
    F7,
    F8,
    F9,
    F10,
    F11,
    F12,
    F13,
    F14,
    F15,
    F16,
    F17,
    F18,
    F19,
    F20,
    F21,
    F22,
    F23,
    F24,
}

impl HotkeyKey {
    pub const fn label(self) -> &'static str {
        match self {
            Self::Num0 => "0",
            Self::Num1 => "1",
            Self::Num2 => "2",
            Self::Num3 => "3",
            Self::Num4 => "4",
            Self::Num5 => "5",
            Self::Num6 => "6",
            Self::Num7 => "7",
            Self::Num8 => "8",
            Self::Num9 => "9",
            Self::A => "A",
            Self::B => "B",
            Self::C => "C",
            Self::D => "D",
            Self::E => "E",
            Self::F => "F",
            Self::G => "G",
            Self::H => "H",
            Self::I => "I",
            Self::J => "J",
            Self::K => "K",
            Self::L => "L",
            Self::M => "M",
            Self::N => "N",
            Self::O => "O",
            Self::P => "P",
            Self::Q => "Q",
            Self::R => "R",
            Self::S => "S",
            Self::T => "T",
            Self::U => "U",
            Self::V => "V",
            Self::W => "W",
            Self::X => "X",
            Self::Y => "Y",
            Self::Z => "Z",
            Self::F1 => "F1",
            Self::F2 => "F2",
            Self::F3 => "F3",
            Self::F4 => "F4",
            Self::F5 => "F5",
            Self::F6 => "F6",
            Self::F7 => "F7",
            Self::F8 => "F8",
            Self::F9 => "F9",
            Self::F10 => "F10",
            Self::F11 => "F11",
            Self::F12 => "F12",
            Self::F13 => "F13",
            Self::F14 => "F14",
            Self::F15 => "F15",
            Self::F16 => "F16",
            Self::F17 => "F17",
            Self::F18 => "F18",
            Self::F19 => "F19",
            Self::F20 => "F20",
            Self::F21 => "F21",
            Self::F22 => "F22",
            Self::F23 => "F23",
            Self::F24 => "F24",
        }
    }

    pub const fn is_function_key(self) -> bool {
        matches!(
            self,
            Self::F1
                | Self::F2
                | Self::F3
                | Self::F4
                | Self::F5
                | Self::F6
                | Self::F7
                | Self::F8
                | Self::F9
                | Self::F10
                | Self::F11
                | Self::F12
                | Self::F13
                | Self::F14
                | Self::F15
                | Self::F16
                | Self::F17
                | Self::F18
                | Self::F19
                | Self::F20
                | Self::F21
                | Self::F22
                | Self::F23
                | Self::F24
        )
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
