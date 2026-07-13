//! egui application shell.
//!
//! State ownership, rendering, platform integration, indicator assets, rate
//! control controls, and log interaction are separated into focused modules.

use crate::backend::session::{ReplayController, ReplaySaveReadiness, ReplayState};
use crate::backend::{
    NvencTuningSupport, ProbeCaps, RateControlFeatureSupport, VideoEncoderBackend,
};
use crate::config::{AppConfig, CaptureBackend, HotkeyConfig, HotkeyKey, ReplayBufferMode};
use crate::hotkey::{HotkeyEvent, HotkeyRuntime};
use crate::indicator_overlay::{IndicatorOverlayRuntime, NativeIndicatorImage};
use crate::rate_control::{RateControlConfig, RateControlMethod};
use crate::single_instance::SingleInstance;
use crate::tray::{TrayEvent, TrayRuntime};
use eframe::egui;
use std::path::Path;
use std::sync::Arc;
use std::sync::mpsc::{self, Receiver};
use std::time::{Duration, Instant};

mod indicator;
mod log;
mod model;
mod platform;
mod rate_control;
mod ui;

use indicator::*;
use log::*;
use model::*;
use platform::*;
use rate_control::*;

pub use model::RustReplayApp;

#[cfg(test)]
mod tests;
