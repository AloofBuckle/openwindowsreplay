#![cfg_attr(windows, windows_subsystem = "windows")]
//! RustReplay 入口。
//!
//! 成品入口只启动 egui GUI；不再保留调试 CLI / 无窗口探测入口。

mod app;
mod backend;
mod config;
mod cpu_placement;
mod embedded_vpl;
mod error;
mod hotkey;
mod indicator_overlay;
mod rate_control;
mod ring;
mod single_instance;
mod tray;

use config::AppConfig;
use eframe::egui;

fn main() {
    if let Err(err) = run() {
        eprintln!("RustReplay 启动失败: {err:#}");
        std::process::exit(1);
    }
}

fn run() -> anyhow::Result<()> {
    wait_for_relaunch_parent();
    if let Some(status) = cpu_placement::initialize_from_env() {
        eprintln!("RustReplay {status}");
    }
    set_process_dpi_awareness();
    let Some(single_instance) =
        single_instance::SingleInstance::acquire().map_err(|err| anyhow::anyhow!(err))?
    else {
        return Ok(());
    };
    embedded_vpl::ensure_installed().map_err(anyhow::Error::msg)?;

    let start_to_tray = AppConfig::load_from_disk()
        .ok()
        .flatten()
        .is_some_and(|config| config.start_minimized_to_tray);

    let native_options = eframe::NativeOptions {
        viewport: egui::ViewportBuilder::default()
            .with_title("RustReplay 即时回放")
            .with_decorations(false)
            .with_inner_size([1280.0, 820.0])
            .with_min_inner_size([980.0, 620.0])
            .with_visible(!start_to_tray)
            .with_taskbar(!start_to_tray),
        ..Default::default()
    };

    eframe::run_native(
        "RustReplay 即时回放",
        native_options,
        Box::new(|cc| Ok(Box::new(app::RustReplayApp::new(cc, single_instance)))),
    )
    .map_err(|err| anyhow::anyhow!(err.to_string()))
}

#[cfg(windows)]
fn wait_for_relaunch_parent() {
    use windows::Win32::Foundation::CloseHandle;
    use windows::Win32::System::Threading::{
        INFINITE, OpenProcess, PROCESS_SYNCHRONIZE, WaitForSingleObject,
    };

    let mut args = std::env::args().skip(1);
    if args.next().as_deref() != Some("--rustreplay-relaunch-after-pid") {
        return;
    }
    let Some(parent_pid) = args.next().and_then(|value| value.parse::<u32>().ok()) else {
        return;
    };
    let Ok(parent) = (unsafe { OpenProcess(PROCESS_SYNCHRONIZE, false, parent_pid) }) else {
        // The old process may already have exited before the helper opened it.
        return;
    };
    unsafe {
        let _ = WaitForSingleObject(parent, INFINITE);
        let _ = CloseHandle(parent);
    }
}

#[cfg(not(windows))]
fn wait_for_relaunch_parent() {}

#[cfg(windows)]
fn set_process_dpi_awareness() {
    unsafe {
        let _ = windows::Win32::UI::HiDpi::SetProcessDpiAwarenessContext(
            windows::Win32::UI::HiDpi::DPI_AWARENESS_CONTEXT_PER_MONITOR_AWARE_V2,
        );
    }
}

#[cfg(not(windows))]
fn set_process_dpi_awareness() {}
