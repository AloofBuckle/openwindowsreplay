//! 可拆除 CLI 调试器。
//!
//! 该模块只负责把 GUI 可做的事情映射到命令行，便于通过测试机的 `/run` 接口自动化。
//! 它不引入替代编码器；所有录制/探测动作都走 `backend` 生产后端。

use crate::backend;
use crate::config::{AppConfig, ChromaSampling};
use crate::rate_control::RateControlMethod;
use anyhow::{Context, bail};
use serde::Serialize;
use std::path::PathBuf;

#[derive(Debug, Clone)]
struct DebugOptions {
    command: DebugCommand,
    output: PathBuf,
    duration_seconds: f32,
    adapter_index: u32,
    chroma: Option<ChromaSampling>,
    rate_control: RateControlMethod,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum DebugCommand {
    Help,
    Probe,
    Config,
    SmokeD3d11,
    SmokeEncode,
    Record,
}

impl Default for DebugOptions {
    fn default() -> Self {
        Self {
            command: DebugCommand::Help,
            output: PathBuf::from("replays/debug-cli.mp4"),
            duration_seconds: 3.559,
            adapter_index: 0,
            chroma: Some(ChromaSampling::Yuv420),
            rate_control: RateControlMethod::Cbr,
        }
    }
}

pub fn run(args: &[String]) -> anyhow::Result<()> {
    let options = parse(args)?;
    match options.command {
        DebugCommand::Help => {
            print_help();
        }
        DebugCommand::Probe => {
            let caps = backend::probe_all();
            println!("{}", serde_json::to_string_pretty(&caps)?);
        }
        DebugCommand::Config => {
            let cfg = make_config(&options);
            println!("{}", serde_json::to_string_pretty(&cfg)?);
        }
        DebugCommand::SmokeD3d11 => {
            let result = backend::gpu_smoke::run_d3d11_dda_smoke(options.adapter_index)?;
            println!("{}", serde_json::to_string_pretty(&result)?);
        }
        DebugCommand::SmokeEncode => {
            let result = backend::vpl::run_d3d11_encode_init_smoke(options.adapter_index)?;
            println!("{}", serde_json::to_string_pretty(&result)?);
        }
        DebugCommand::Record => {
            let caps = backend::probe_all();
            let cfg = make_config(&options);
            let request = DebugRecordRequest {
                output: options.output.display().to_string(),
                duration_seconds: options.duration_seconds,
                adapter_index: options.adapter_index,
                config: cfg.clone(),
            };
            eprintln!("{}", serde_json::to_string_pretty(&request)?);
            let report = backend::debug_record_once(
                &cfg,
                &caps,
                &options.output,
                options.duration_seconds,
                options.adapter_index,
            )
            .with_context(|| "生产后端 debug record 失败")?;
            println!("{}", serde_json::to_string_pretty(&report)?);
        }
    }
    Ok(())
}

fn make_config(options: &DebugOptions) -> AppConfig {
    let mut cfg = AppConfig {
        chroma: options.chroma,
        ..AppConfig::default()
    };
    cfg.rate_control.method = options.rate_control;
    cfg.replay_minutes = options.duration_seconds / 60.0;
    cfg.save_dir = options
        .output
        .parent()
        .map(|p| p.display().to_string())
        .unwrap_or_else(|| "replays".to_owned());
    cfg
}

#[derive(Debug, Clone, Serialize)]
struct DebugRecordRequest {
    output: String,
    duration_seconds: f32,
    adapter_index: u32,
    config: AppConfig,
}

fn parse(args: &[String]) -> anyhow::Result<DebugOptions> {
    let mut out = DebugOptions::default();
    let Some(first) = args.first() else {
        return Ok(out);
    };
    out.command = match first.as_str() {
        "probe" => DebugCommand::Probe,
        "config" => DebugCommand::Config,
        "smoke-d3d11" => DebugCommand::SmokeD3d11,
        "smoke-encode" => DebugCommand::SmokeEncode,
        "record" => DebugCommand::Record,
        "help" | "--help" | "-h" => DebugCommand::Help,
        other => bail!("未知 debug-cli 命令: {other}"),
    };

    let mut i = 1;
    while i < args.len() {
        match args[i].as_str() {
            "--output" | "-o" => {
                i += 1;
                out.output = PathBuf::from(args.get(i).context("--output 需要路径")?);
            }
            "--duration" => {
                i += 1;
                let value = args.get(i).context("--duration 需要秒数")?;
                out.duration_seconds = value.parse().context("解析 --duration 失败")?;
            }
            "--adapter" => {
                i += 1;
                let value = args.get(i).context("--adapter 需要索引")?;
                out.adapter_index = value.parse().context("解析 --adapter 失败")?;
            }
            "--chroma" => {
                i += 1;
                out.chroma = Some(parse_chroma(
                    args.get(i).context("--chroma 需要 420/422/444")?,
                )?);
            }
            "--rate" => {
                i += 1;
                out.rate_control = parse_rate(args.get(i).context("--rate 需要码控名")?)?;
            }
            other => bail!("未知 debug-cli 参数: {other}"),
        }
        i += 1;
    }
    Ok(out)
}

fn parse_chroma(value: &str) -> anyhow::Result<ChromaSampling> {
    match value.to_ascii_lowercase().as_str() {
        "420" | "yuv420" => Ok(ChromaSampling::Yuv420),
        "422" | "yuv422" => Ok(ChromaSampling::Yuv422),
        "444" | "yuv444" => Ok(ChromaSampling::Yuv444),
        _ => bail!("未知 chroma: {value}"),
    }
}

fn parse_rate(value: &str) -> anyhow::Result<RateControlMethod> {
    match value.to_ascii_uppercase().as_str() {
        "CBR" => Ok(RateControlMethod::Cbr),
        "VBR" => Ok(RateControlMethod::Vbr),
        "CQP" => Ok(RateControlMethod::Cqp),
        "AVBR" => Ok(RateControlMethod::Avbr),
        "LA" => Ok(RateControlMethod::La),
        "ICQ" => Ok(RateControlMethod::Icq),
        "VCM" => Ok(RateControlMethod::Vcm),
        "LA_ICQ" => Ok(RateControlMethod::LaIcq),
        "LA_HRD" => Ok(RateControlMethod::LaHrd),
        "QVBR" => Ok(RateControlMethod::Qvbr),
        _ => bail!("未知 rate control: {value}"),
    }
}

fn print_help() {
    println!(
        "RustReplay debug-cli（可拆除，全部走生产 backend）\n\n\
         用法:\n\
           rust_replay.exe --debug-cli probe\n\
           rust_replay.exe --debug-cli config [--output out.mp4] [--duration 3.559] [--chroma 420] [--rate CBR]\n\
           rust_replay.exe --debug-cli smoke-d3d11 [--adapter 0]\n\
           rust_replay.exe --debug-cli smoke-encode [--adapter 0]\n\
           rust_replay.exe --debug-cli record --output out.mp4 [--duration 3.559] [--adapter 0] [--chroma 420] [--rate CBR]\n"
    );
}
