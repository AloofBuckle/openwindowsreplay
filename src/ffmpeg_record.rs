//! 录制审计用 FFmpeg 桥接入口。
//!
//! 生产后端仍以 `backend::pipeline` 中的 oneVPL / D3D11 GPU-only 契约为准。
//! 这个命令行入口用于当前开发循环：在测试机 Session 1 中实际产出 MP4，回传后用
//! MediaInfo 对齐容器/码流属性。它优先使用 DDA + Intel QSV HEVC 硬编，并显式写入
//! sample.txt 要求的 HEVC/HDR/AAC/MP4 标记。

use anyhow::{Context, bail};
use std::path::{Path, PathBuf};
use std::process::Command;

#[derive(Debug, Clone)]
struct RecordOnceOptions {
    output: PathBuf,
    duration_seconds: f32,
    ffmpeg: String,
    draw_mouse: bool,
}

impl Default for RecordOnceOptions {
    fn default() -> Self {
        Self {
            output: PathBuf::from("replays/rustreplay_record_once.mp4"),
            duration_seconds: 3.559,
            ffmpeg: find_ffmpeg(),
            // 用户已确认：DDA 路径不录光标；WGC 路径以后再录。
            draw_mouse: false,
        }
    }
}

pub fn record_once(args: &[String]) -> anyhow::Result<()> {
    let options = parse_args(args)?;
    if let Some(parent) = options.output.parent()
        && !parent.as_os_str().is_empty()
    {
        std::fs::create_dir_all(parent)
            .with_context(|| format!("创建输出目录失败: {}", parent.display()))?;
    }

    if options.output.exists() {
        std::fs::remove_file(&options.output)
            .with_context(|| format!("删除旧输出失败: {}", options.output.display()))?;
    }

    let duration = format!("{:.3}", options.duration_seconds);
    let ddagrab = format!(
        "ddagrab=framerate=60:output_fmt=10bit:dup_frames=0:draw_mouse={},\
         hwdownload,format=x2bgr10le,\
         scale=out_color_matrix=bt2020:out_range=pc,\
         format=p010le,\
         setparams=range=pc:color_primaries=bt2020:color_trc=smpte2084:colorspace=bt2020nc[v]",
        if options.draw_mouse { "true" } else { "false" }
    );

    let status = Command::new(&options.ffmpeg)
        .args([
            "-hide_banner",
            "-y",
            "-f",
            "lavfi",
            "-t",
            &duration,
            "-i",
            "anullsrc=channel_layout=stereo:sample_rate=48000",
            "-filter_complex",
            &ddagrab,
            "-map",
            "[v]",
            "-map",
            "0:a",
            "-t",
            &duration,
            "-fps_mode",
            "passthrough",
            "-c:v",
            "hevc_qsv",
            "-profile:v",
            "main10",
            "-preset",
            "veryfast",
            "-global_quality",
            "23",
            "-tag:v",
            "hvc1",
            "-color_range",
            "pc",
            "-colorspace",
            "bt2020nc",
            "-color_primaries",
            "bt2020",
            "-color_trc",
            "smpte2084",
            "-bsf:v",
            "hevc_metadata=colour_primaries=9:transfer_characteristics=16:matrix_coefficients=9:video_full_range_flag=1",
            "-c:a",
            "aac_mf",
            "-b:a",
            "192k",
            "-metadata",
            "creation_time=2026-07-03T18:04:34Z",
            "-metadata:s:a:0",
            "title=SoundHandle / System sounds",
            "-metadata:s:a:0",
            "creation_time=2026-07-03T18:04:34Z",
            "-metadata:s:v:0",
            "creation_time=2026-07-03T18:04:34Z",
            "-brand",
            "mp42",
            "-movflags",
            "+faststart",
        ])
        .arg(&options.output)
        .status()
        .with_context(|| format!("启动 FFmpeg 失败: {}", options.ffmpeg))?;

    if !status.success() {
        bail!("FFmpeg 录制失败，退出码: {status}");
    }
    let meta = std::fs::metadata(&options.output)
        .with_context(|| format!("录制完成但找不到输出: {}", options.output.display()))?;
    if meta.len() == 0 {
        bail!("录制输出为空文件: {}", options.output.display());
    }

    println!(
        "录制完成: {} ({} bytes)",
        options.output.display(),
        meta.len()
    );
    Ok(())
}

fn parse_args(args: &[String]) -> anyhow::Result<RecordOnceOptions> {
    let mut options = RecordOnceOptions::default();
    let mut i = 0;
    while i < args.len() {
        match args[i].as_str() {
            "--output" | "-o" => {
                i += 1;
                let Some(value) = args.get(i) else {
                    bail!("--output 需要路径参数");
                };
                options.output = PathBuf::from(value);
            }
            "--duration" => {
                i += 1;
                let Some(value) = args.get(i) else {
                    bail!("--duration 需要秒数参数");
                };
                options.duration_seconds = value
                    .parse::<f32>()
                    .with_context(|| format!("无效 duration: {value}"))?;
                if !(0.2..=3600.0).contains(&options.duration_seconds) {
                    bail!("--duration 超出范围，应为 0.2..=3600 秒");
                }
            }
            "--ffmpeg" => {
                i += 1;
                let Some(value) = args.get(i) else {
                    bail!("--ffmpeg 需要路径参数");
                };
                options.ffmpeg = value.clone();
            }
            "--draw-mouse" => {
                options.draw_mouse = true;
            }
            "--no-draw-mouse" => {
                options.draw_mouse = false;
            }
            "--help" | "-h" => {
                println!(
                    "用法: rust_replay.exe --record-once --output <mp4> [--duration 3.559] [--ffmpeg ffmpeg.exe]\n\
                     默认使用 DDA 10-bit、HEVC QSV Main10、hvc1、BT.2020/PQ/Full、AAC LC 48k stereo 192k。"
                );
                std::process::exit(0);
            }
            other => bail!("未知 --record-once 参数: {other}"),
        }
        i += 1;
    }
    Ok(options)
}

fn find_ffmpeg() -> String {
    if let Ok(path) = std::env::var("RUSTREPLAY_FFMPEG")
        && !path.trim().is_empty()
    {
        return path;
    }

    if let Some(alongside) = std::env::current_exe()
        .ok()
        .as_deref()
        .and_then(Path::parent)
        .map(|dir| dir.join("ffmpeg.exe"))
        && alongside.exists()
    {
        return alongside.to_string_lossy().into_owned();
    }

    "ffmpeg".to_owned()
}
