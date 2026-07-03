#![cfg_attr(windows, windows_subsystem = "windows")]
//! RustReplay 入口。
//!
//! 运行 GUI 时直接启动 egui；用于远程调试时可传入 `--probe-json`，程序只探测
//! 本机 DXGI/oneVPL 能力并把 JSON 写到标准输出，便于通过 `/run` 接口查看。

mod app;
mod backend;
mod cli_debug;
mod config;
mod error;
mod rate_control;
mod ring;

use anyhow::Context;
use eframe::egui;

fn main() {
    if let Err(err) = run() {
        eprintln!("RustReplay 启动失败: {err:#}");
        std::process::exit(1);
    }
}

fn run() -> anyhow::Result<()> {
    let args = std::env::args().skip(1).collect::<Vec<_>>();
    match args.first().map(String::as_str) {
        Some("--probe-json") => {
            let caps = backend::probe_all();
            println!("{}", serde_json::to_string_pretty(&caps)?);
            return Ok(());
        }
        Some("--headless-self-test") => {
            headless_self_test()?;
            return Ok(());
        }
        Some("--debug-cli") => {
            cli_debug::run(&args[1..])?;
            return Ok(());
        }
        Some("--help") | Some("-h") => {
            println!(
                "RustReplay 即时回放\n\n用法:\n  rust_replay.exe              启动 GUI\n  rust_replay.exe --probe-json 仅输出能力探测 JSON\n  rust_replay.exe --headless-self-test 运行无窗口自检\n  rust_replay.exe --debug-cli <命令> 运行可拆除的生产后端 CLI 调试器"
            );
            return Ok(());
        }
        Some(other) => anyhow::bail!("未知参数: {other}"),
        None => {}
    }

    let native_options = eframe::NativeOptions {
        viewport: egui::ViewportBuilder::default()
            .with_title("RustReplay 即时回放")
            .with_inner_size([1280.0, 820.0])
            .with_min_inner_size([980.0, 620.0]),
        ..Default::default()
    };

    eframe::run_native(
        "RustReplay 即时回放",
        native_options,
        Box::new(|cc| Ok(Box::new(app::RustReplayApp::new(cc)))),
    )
    .map_err(|err| anyhow::anyhow!(err.to_string()))
}

fn headless_self_test() -> anyhow::Result<()> {
    let caps = backend::probe_all();
    let mut buffer = ring::EncodedRingBuffer::new(std::time::Duration::from_secs(10));
    buffer.push(ring::EncodedPacket {
        pts_ns: 1_000_000_000,
        dts_ns: 1_000_000_000,
        duration_ns: 33_333_333,
        is_key: true,
        data: vec![0, 0, 1, 0x26],
    });
    let json = serde_json::to_string(&caps).context("序列化能力探测结果失败")?;
    println!(
        "自检完成: caps_json={} bytes, ring_packets={}",
        json.len(),
        buffer.len()
    );
    Ok(())
}
