#![allow(dead_code)]
//! 全局配置模型。
//!
//! 文档要求 GUI 为引导式填参，因此这里把“可显示的配置”和“后端探测出的
//! 可用能力”分开：配置可以保存用户意图，真正开始录制前仍必须由后端再次验证。

use crate::rate_control::RateControlConfig;
use serde::{Deserialize, Serialize};
use std::fs::{File, OpenOptions};
use std::io::{self, Write};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};

static CONFIG_TEMP_SEQUENCE: AtomicU64 = AtomicU64::new(0);

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(default)]
pub struct AppConfig {
    pub capture_mode: CaptureMode,
    pub capture_backend: CaptureBackend,
    pub replay_buffer_mode: ReplayBufferMode,
    pub chroma: Option<ChromaSampling>,
    pub rate_control: RateControlConfig,
    pub cache_dir: String,
    pub save_dir: String,
    pub replay_minutes: f32,
    pub start_recording_on_launch: bool,
    pub start_minimized_to_tray: bool,
    /// 只允许用户修改“保存即时回放/重放”这一项热键。
    pub save_hotkey: HotkeyConfig,
    pub indicator: IndicatorConfig,
}

impl Default for AppConfig {
    fn default() -> Self {
        Self {
            capture_mode: CaptureMode::Generic,
            capture_backend: CaptureBackend::Wgc,
            replay_buffer_mode: ReplayBufferMode::Memory,
            chroma: None,
            rate_control: RateControlConfig::default(),
            cache_dir: "cache".to_owned(),
            save_dir: "replays".to_owned(),
            replay_minutes: 3.0,
            start_recording_on_launch: false,
            start_minimized_to_tray: false,
            save_hotkey: HotkeyConfig::default(),
            indicator: IndicatorConfig::default(),
        }
    }
}

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
pub enum CaptureMode {
    #[default]
    Generic,
    DedicatedNvFbc,
}

impl CaptureMode {
    pub const fn label(self) -> &'static str {
        match self {
            Self::Generic => "通用捕获",
            Self::DedicatedNvFbc => "NvFBC 专用捕获",
        }
    }
}

impl AppConfig {
    pub fn config_dir() -> PathBuf {
        let base = std::env::var_os("ProgramData")
            .map(PathBuf::from)
            .unwrap_or_else(|| PathBuf::from(r"C:\ProgramData"));
        base.join("OneVPL Replay")
    }

    pub fn config_path() -> PathBuf {
        Self::config_dir().join("config.json")
    }

    pub fn indicator_dir() -> PathBuf {
        Self::config_dir().join("indicator")
    }

    pub fn vpl_dll_path() -> PathBuf {
        Self::config_dir().join("libvpl-2.dll")
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
        write_file_atomically(&path, text.as_bytes())
            .map_err(|err| format!("写入配置文件 {} 失败：{err}", path.display()))
    }

    pub fn stable_json(&self) -> String {
        serde_json::to_string(self).unwrap_or_default()
    }

    pub fn clear_global_entry() -> Result<(), String> {
        let path = Self::config_path();
        match std::fs::remove_file(&path) {
            Ok(()) => Ok(()),
            Err(err) if err.kind() == std::io::ErrorKind::NotFound => Ok(()),
            Err(err) => Err(format!("删除配置文件 {} 失败：{err}", path.display())),
        }
    }
}

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
pub enum ReplayBufferMode {
    #[default]
    Memory,
    Disk,
}

impl ReplayBufferMode {
    pub const fn is_disk(self) -> bool {
        matches!(self, Self::Disk)
    }

    pub const fn label(self) -> &'static str {
        match self {
            Self::Memory => "内存循环",
            Self::Disk => "磁盘循环",
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(default)]
pub struct IndicatorConfig {
    /// 指示器左上角屏幕位置，单位为 Win32 物理屏幕像素。
    pub position_x: f32,
    pub position_y: f32,
    /// 用户界面以 px 展示；渲染时按当前 DPI 折算到逻辑点。
    pub diameter_px: u32,
    pub image_path: Option<String>,
    pub text_enabled: bool,
}

impl Default for IndicatorConfig {
    fn default() -> Self {
        Self {
            position_x: 32.0,
            position_y: 32.0,
            diameter_px: 32,
            image_path: None,
            text_enabled: false,
        }
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

fn write_file_atomically(path: &Path, contents: &[u8]) -> io::Result<()> {
    write_file_atomically_with_replace(path, contents, replace_file_atomically)
}

fn write_file_atomically_with_replace(
    path: &Path,
    contents: &[u8],
    replace: impl FnOnce(&Path, &Path) -> io::Result<()>,
) -> io::Result<()> {
    let (temp_path, mut temp_file) = create_atomic_temp_file(path)?;
    let write_result = temp_file
        .write_all(contents)
        .and_then(|()| temp_file.flush())
        .and_then(|()| temp_file.sync_all());
    drop(temp_file);
    if let Err(err) = write_result {
        let _ = std::fs::remove_file(&temp_path);
        return Err(err);
    }
    if let Err(err) = replace(&temp_path, path) {
        let _ = std::fs::remove_file(&temp_path);
        return Err(err);
    }
    Ok(())
}

fn create_atomic_temp_file(path: &Path) -> io::Result<(PathBuf, File)> {
    let parent = path.parent().unwrap_or_else(|| Path::new("."));
    let file_name = path
        .file_name()
        .and_then(|name| name.to_str())
        .unwrap_or("config.json");
    for _ in 0..100 {
        let sequence = CONFIG_TEMP_SEQUENCE.fetch_add(1, Ordering::Relaxed);
        let temp_path = parent.join(format!(
            ".{file_name}.{}.{}.tmp",
            std::process::id(),
            sequence
        ));
        match OpenOptions::new()
            .write(true)
            .create_new(true)
            .open(&temp_path)
        {
            Ok(file) => return Ok((temp_path, file)),
            Err(err) if err.kind() == io::ErrorKind::AlreadyExists => continue,
            Err(err) => return Err(err),
        }
    }
    Err(io::Error::new(
        io::ErrorKind::AlreadyExists,
        "无法为配置文件创建唯一临时文件",
    ))
}

#[cfg(windows)]
fn replace_file_atomically(source: &Path, destination: &Path) -> io::Result<()> {
    use std::os::windows::ffi::OsStrExt;
    use windows::Win32::Storage::FileSystem::{
        MOVEFILE_REPLACE_EXISTING, MOVEFILE_WRITE_THROUGH, MoveFileExW,
    };
    use windows::core::PCWSTR;

    let source_wide: Vec<u16> = source.as_os_str().encode_wide().chain(Some(0)).collect();
    let destination_wide: Vec<u16> = destination
        .as_os_str()
        .encode_wide()
        .chain(Some(0))
        .collect();
    unsafe {
        MoveFileExW(
            PCWSTR(source_wide.as_ptr()),
            PCWSTR(destination_wide.as_ptr()),
            MOVEFILE_REPLACE_EXISTING | MOVEFILE_WRITE_THROUGH,
        )
    }
    .map_err(|err| io::Error::other(format!("MoveFileExW 原子替换失败：{err}")))
}

#[cfg(not(windows))]
fn replace_file_atomically(source: &Path, destination: &Path) -> io::Result<()> {
    std::fs::rename(source, destination)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn legacy_config_without_capture_mode_defaults_to_generic_and_keeps_backend() {
        let config: AppConfig = serde_json::from_str(
            r#"{
                "capture_backend": "Dda",
                "cache_dir": "legacy-cache",
                "save_dir": "legacy-save"
            }"#,
        )
        .expect("deserialize legacy config");

        assert_eq!(config.capture_mode, CaptureMode::Generic);
        assert_eq!(config.capture_backend, CaptureBackend::Dda);
        assert_eq!(config.cache_dir, "legacy-cache");
        assert_eq!(config.save_dir, "legacy-save");
    }

    #[test]
    fn dedicated_capture_preference_is_serialized_independently_from_generic_backend() {
        let config = AppConfig {
            capture_mode: CaptureMode::DedicatedNvFbc,
            capture_backend: CaptureBackend::Dda,
            ..AppConfig::default()
        };
        let restored: AppConfig =
            serde_json::from_str(&config.stable_json()).expect("round-trip config");

        assert_eq!(restored.capture_mode, CaptureMode::DedicatedNvFbc);
        assert_eq!(restored.capture_backend, CaptureBackend::Dda);
    }

    #[test]
    fn atomic_config_write_replaces_existing_file() {
        let dir = config_test_dir("replace");
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("config.json");
        std::fs::write(&path, b"old config").unwrap();

        write_file_atomically(&path, b"new config").unwrap();

        assert_eq!(std::fs::read(&path).unwrap(), b"new config");
        assert_eq!(std::fs::read_dir(&dir).unwrap().count(), 1);
        let _ = std::fs::remove_dir_all(dir);
    }

    #[test]
    fn failed_atomic_config_replace_preserves_old_file_and_removes_temp() {
        let dir = config_test_dir("replace_failure");
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("config.json");
        std::fs::write(&path, b"known-good config").unwrap();

        let error = write_file_atomically_with_replace(&path, b"partial new config", |_, _| {
            Err(io::Error::other("injected replacement failure"))
        })
        .unwrap_err();

        assert!(error.to_string().contains("injected replacement failure"));
        assert_eq!(std::fs::read(&path).unwrap(), b"known-good config");
        assert_eq!(std::fs::read_dir(&dir).unwrap().count(), 1);
        let _ = std::fs::remove_dir_all(dir);
    }

    fn config_test_dir(name: &str) -> PathBuf {
        std::env::temp_dir().join(format!(
            "rustreplay_config_{name}_{}_{}",
            std::process::id(),
            CONFIG_TEMP_SEQUENCE.fetch_add(1, Ordering::Relaxed)
        ))
    }
}
