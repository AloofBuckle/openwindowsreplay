//! GPU-only 后端总入口。

pub mod dxgi;
pub mod gpu_smoke;
pub mod pipeline;
pub mod session;
pub mod vpl;

use crate::config::ChromaSampling;
use crate::rate_control::RateControlMethod;
use crate::{config::AppConfig, error::BackendError};
use serde::{Deserialize, Serialize};
use std::collections::BTreeSet;
use std::path::Path;
use std::time::{SystemTime, UNIX_EPOCH};

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ProbeCaps {
    pub probe_time_unix_ms: u128,
    pub dxgi_adapters: Vec<dxgi::DxgiAdapterInfo>,
    pub vpl: vpl::VplProbeInfo,
    /// oneVPL 报告的候选色度采样；这还不是最终可录制路径。
    pub vpl_candidate_chroma: Vec<ChromaSampling>,
    /// 当前完整桌面同步路径可展示的色度采样；不可用时前端直接隐藏字段。
    pub supported_chroma: Vec<ChromaSampling>,
    /// oneVPL Query 验证的码控模式；不可用模式前端直接隐藏。
    pub supported_rate_controls: Vec<RateControlMethod>,
    /// 是否已经形成“当前显示器状态 + DDA/WGC 数据 + GPU 转换 + HEVC 硬编”的完整路径。
    pub desktop_sync_path_available: bool,
    /// oneVPL 是否报告 D3D11 texture 视频内存输入能力；它不是完整路径的充分条件。
    pub d3d11_texture_input_supported: bool,
    pub async_depth_suggested: Option<u16>,
    pub bit_depth_policy: Vec<BitDepthMode>,
    pub capture_cursor_policy: Vec<CaptureCursorPolicy>,
    pub color_fidelity_policy: ColorFidelityPolicy,
    pub audio_policy: AudioPolicy,
    pub package_policy: PackagePolicy,
    pub path_blockers: Vec<String>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum BitDepthMode {
    Internal8Bit,
    Internal10Bit,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct CaptureCursorPolicy {
    pub backend: String,
    pub cursor_recording: bool,
    pub reason: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ColorFidelityPolicy {
    pub strategy: String,
    pub cannot_guarantee: Vec<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct AudioPolicy {
    pub target_format: String,
    pub resampling_allowed: bool,
    pub timestamp_rule: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct PackagePolicy {
    pub single_exe_required: bool,
    pub bundled_dependencies_allowed: bool,
    pub note: String,
}

impl ProbeCaps {
    pub fn short_status(&self) -> String {
        if !self.desktop_sync_path_available {
            format!(
                "未形成完整桌面同步录制路径；DXGI 适配器 {} 个，oneVPL: {}",
                self.dxgi_adapters.len(),
                if self.vpl.available {
                    "已加载"
                } else {
                    "不可用"
                }
            )
        } else {
            format!(
                "已验证色度采样: {}；码控模式: {} 个",
                self.supported_chroma
                    .iter()
                    .map(|c| c.doc_label())
                    .collect::<Vec<_>>()
                    .join("/"),
                self.supported_rate_controls.len()
            )
        }
    }
}

pub fn probe_all() -> ProbeCaps {
    let probe_time_unix_ms = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_millis())
        .unwrap_or_default();

    let (dxgi_adapters, mut reasons) = match dxgi::enumerate_adapters() {
        Ok(adapters) => (adapters, Vec::new()),
        Err(err) => (Vec::new(), vec![format!("DXGI 适配器枚举失败: {err}")]),
    };

    let vpl = vpl::probe_vpl();
    reasons.extend(vpl.warnings.iter().cloned());

    let vpl_candidate_chroma = vpl.chroma_candidates.clone();
    let d3d11_texture_input_supported = vpl.dx11_texture_input_seen;

    if !vpl.hevc_supported {
        reasons.push("没有可用 HEVC 硬件编码实现；禁止软件编码/AVC 降级".to_owned());
    }
    if !d3d11_texture_input_supported {
        reasons
            .push("oneVPL 未报告 D3D11 texture 视频内存输入；不满足 GPU-only 输入前提".to_owned());
    }
    if vpl.rate_controls.is_empty() {
        reasons.push("当前运行时未确认任何可用码率控制模式；前端隐藏码控字段".to_owned());
    }

    // 当前代码还没有完成 DDA/WGC 捕获、GPU 色彩转换 shader、shared D3D11 surface import 的
    // 端到端实测，所以完整桌面同步路径保持关闭；这会让前端直接隐藏相关字段。
    let desktop_sync_path_available = false;
    reasons.push(
        "捕获->GPU色彩转换->shared D3D11 surface import 端到端路径尚未实装验证；不创建录制会话"
            .to_owned(),
    );

    let supported_chroma = if desktop_sync_path_available {
        vpl_candidate_chroma.clone()
    } else {
        Vec::new()
    };

    let mut rate_set = BTreeSet::new();
    for method in &vpl.rate_controls {
        rate_set.insert(*method);
    }

    ProbeCaps {
        probe_time_unix_ms,
        dxgi_adapters,
        supported_chroma,
        vpl_candidate_chroma,
        supported_rate_controls: rate_set.into_iter().collect(),
        desktop_sync_path_available,
        d3d11_texture_input_supported,
        async_depth_suggested: Some(4),
        bit_depth_policy: vec![BitDepthMode::Internal8Bit, BitDepthMode::Internal10Bit],
        capture_cursor_policy: vec![
            CaptureCursorPolicy {
                backend: "DDA".to_owned(),
                cursor_recording: false,
                reason: "DDA 光标合成容易需要 CPU 元数据/合成；按 GPU-only 契约不录光标".to_owned(),
            },
            CaptureCursorPolicy {
                backend: "WGC_BGRA8".to_owned(),
                cursor_recording: true,
                reason: "WGC 路径负责录制光标".to_owned(),
            },
            CaptureCursorPolicy {
                backend: "WGC_FP16".to_owned(),
                cursor_recording: true,
                reason: "WGC 路径负责录制光标".to_owned(),
            },
        ],
        color_fidelity_policy: ColorFidelityPolicy {
            strategy: "按当前显示器状态与 DDA/WGC 实际数据做高保真；无法可靠确定的字段不在前端展示，并写入日志/采用明确推断策略".to_owned(),
            cannot_guarantee: vec![
                "Windows compositor 可能已把源应用颜色转换为桌面合成结果，无法还原源应用原始色彩".to_owned(),
                "DDA/WGC 不总是提供完整逐帧 primaries/transfer/matrix/range 元数据".to_owned(),
                "HDR/SDR 多显示器混合与 scRGB/BT.2020/PQ 转换需依据当前显示器状态推断".to_owned(),
                "HEVC VUI/SEI 颜色标记最终可写字段仍受 oneVPL/驱动支持限制".to_owned(),
            ],
        },
        audio_policy: AudioPolicy {
            target_format: "48kHz stereo float PCM -> AAC LC".to_owned(),
            resampling_allowed: true,
            timestamp_rule: "允许重采样和声道混合，但必须保留/重建绝对时间戳，音画同步禁止按帧号硬凑".to_owned(),
        },
        package_policy: PackagePolicy {
            single_exe_required: false,
            bundled_dependencies_allowed: true,
            note: "发布包可携带 libvpl.dll 等用户态依赖；GPU 驱动、D3D11、Media Foundation 仍是系统/驱动前提".to_owned(),
        },
        path_blockers: reasons,
        vpl,
    }
}

pub fn debug_record_once(
    config: &AppConfig,
    caps: &ProbeCaps,
    output: &Path,
    duration_seconds: f32,
) -> Result<(), BackendError> {
    let chroma = config.chroma.ok_or_else(|| {
        BackendError::unsupported("debug-cli record", "色度采样", "没有选择色度采样")
    })?;
    let request = pipeline::RecordingRequest {
        capture_backend: pipeline::CaptureBackendKind::Dda,
        color_transform: pipeline::ColorTransformKind::HdrPq10ToYuv10,
        chroma_writer: match chroma {
            ChromaSampling::Yuv420 => pipeline::ChromaWriterKind::P010,
            ChromaSampling::Yuv422 => pipeline::ChromaWriterKind::P210,
            ChromaSampling::Yuv444 => pipeline::ChromaWriterKind::Y410,
        },
        rate_control: config.rate_control.clone(),
        replay_minutes: duration_seconds / 60.0,
    };
    pipeline::record_once_gpu_only(&request, caps, output, duration_seconds)
}
