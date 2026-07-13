//! GPU-only 后端总入口。

pub mod aac_mf;
pub mod audio;
pub mod dxgi;
pub mod mp4_mux;
pub mod pipeline;
pub mod session;
pub mod vpl;
pub mod wasapi;

use crate::config::ChromaSampling;
use crate::error::BackendError;
use crate::rate_control::{RateControlConfig, RateControlMethod};
use serde::{Deserialize, Serialize};
use std::collections::{BTreeMap, BTreeSet};
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
    /// 按可生产录制色度采样聚合后的码控可用性。前端先选色度，再只展示该色度
    /// 下至少一条当前显示器生产 route 逐项 Query 通过的 RateControlMethod。
    pub rate_controls_by_chroma: Vec<RateControlChromaSupport>,
    /// 按可生产录制色度采样/码控模式聚合后的可选码控字段可见性。GUI 隐藏
    /// 所有当前显示器生产 route 都不支持的字段；录制时仍逐 route Query/Init 验证。
    pub rate_control_features_by_chroma: Vec<RateControlChromaFeatureSupport>,
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

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct RateControlChromaSupport {
    pub chroma: ChromaSampling,
    pub methods: Vec<RateControlMethod>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct RateControlChromaFeatureSupport {
    pub chroma: ChromaSampling,
    pub features: Vec<RateControlFeatureSupport>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct RateControlFeatureSupport {
    pub method: RateControlMethod,
    pub look_ahead_depth: bool,
    pub win_brc: bool,
    pub low_delay_brc: bool,
    pub max_frame_size: bool,
    pub mbbrc: bool,
}

impl RateControlFeatureSupport {
    pub const fn hidden(method: RateControlMethod) -> Self {
        Self {
            method,
            look_ahead_depth: false,
            win_brc: false,
            low_delay_brc: false,
            max_frame_size: false,
            mbbrc: false,
        }
    }

    fn from_probe(probe: &vpl::VplRateControlFeatureProbe) -> Self {
        Self {
            method: probe.method,
            look_ahead_depth: probe.look_ahead_depth,
            win_brc: probe.win_brc,
            low_delay_brc: probe.low_delay_brc,
            max_frame_size: probe.max_frame_size,
            mbbrc: probe.mbbrc,
        }
    }

    fn union(method: RateControlMethod, probes: &[Self]) -> Self {
        let Some(first) = probes.first().copied() else {
            return Self::hidden(method);
        };
        let mut out = probes.iter().skip(1).fold(first, |mut acc, item| {
            acc.look_ahead_depth |= item.look_ahead_depth;
            acc.win_brc |= item.win_brc;
            acc.low_delay_brc |= item.low_delay_brc;
            acc.max_frame_size |= item.max_frame_size;
            acc.mbbrc |= item.mbbrc;
            acc
        });
        out.method = method;
        out
    }
}

impl ProbeCaps {
    pub fn route_blocker_summary_for_chroma(&self, chroma: ChromaSampling) -> Option<String> {
        let mut blockers = self
            .vpl
            .current_display_routes
            .iter()
            .filter(|route| route.chroma == chroma && route.fourcc.is_empty())
            .map(|route| format!("当前显示器 route：{}", route.note))
            .collect::<BTreeSet<_>>();
        if self.vpl.current_display_routes.is_empty() {
            blockers.insert("当前显示器自动 route 探测为空，不能按未知显示状态录制".to_owned());
        }
        blockers.extend(
            self.vpl
                .route_candidates
                .iter()
                .filter(|route| route.chroma == chroma && !route.production_record_supported)
                .map(|route| {
                    format!(
                        "FourCC={} bit_depth={} profile={}：{}",
                        route.fourcc,
                        route.bit_depth,
                        route.profile,
                        route
                            .production_blocker
                            .as_deref()
                            .unwrap_or(route.note.as_str())
                    )
                }),
        );
        (!blockers.is_empty()).then(|| blockers.into_iter().collect::<Vec<_>>().join("；"))
    }

    pub fn rate_controls_for_chroma(&self, chroma: ChromaSampling) -> &[RateControlMethod] {
        self.rate_controls_by_chroma
            .iter()
            .find(|item| item.chroma == chroma)
            .map(|item| item.methods.as_slice())
            .unwrap_or(&[])
    }

    pub fn rate_control_features_for(
        &self,
        chroma: ChromaSampling,
        method: RateControlMethod,
    ) -> RateControlFeatureSupport {
        self.rate_control_features_by_chroma
            .iter()
            .find(|item| item.chroma == chroma)
            .and_then(|item| {
                item.features
                    .iter()
                    .find(|feature| feature.method == method)
            })
            .copied()
            .unwrap_or_else(|| RateControlFeatureSupport::hidden(method))
    }

    pub fn validate_rate_control_config(
        &self,
        chroma: ChromaSampling,
        cfg: &RateControlConfig,
        context: &'static str,
    ) -> Result<(), BackendError> {
        if !self.rate_controls_for_chroma(chroma).contains(&cfg.method) {
            return Err(BackendError::unsupported(
                context,
                format!("{} + {}", chroma.doc_label(), cfg.method.short_name()),
                "oneVPL 能力探测未确认该 RateControlMethod 在当前色度生产 route 上可用",
            ));
        }
        let features = self.rate_control_features_for(chroma, cfg.method);
        if cfg.ext_brc {
            return Err(BackendError::unsupported(
                context,
                "ExtBRC",
                "External BRC 需要 mfxExtBRC 回调；当前生产后端未接入，GUI 隐藏该字段",
            ));
        }
        let uses_lookahead = matches!(
            cfg.method,
            RateControlMethod::La | RateControlMethod::LaIcq | RateControlMethod::LaHrd
        ) && cfg.look_ahead_depth > 0;
        if uses_lookahead && !features.look_ahead_depth {
            return Err(BackendError::unsupported(
                context,
                "LookAheadDepth",
                "当前 route/method 未确认支持 LookAheadDepth；前端应隐藏该字段",
            ));
        }
        if (cfg.win_brc_max_avg_kbps > 0 || cfg.win_brc_size > 0) && !features.win_brc {
            return Err(BackendError::unsupported(
                context,
                "WinBRCMaxAvgKbps/WinBRCSize",
                "当前 route/method 未确认支持滑动窗口码控；前端应隐藏该字段",
            ));
        }
        if cfg.low_delay_brc && !features.low_delay_brc {
            return Err(BackendError::unsupported(
                context,
                "LowDelayBRC",
                "当前 route/method 未通过 Query + D3D11 surface 冒烟；前端应隐藏该字段",
            ));
        }
        if cfg.max_frame_size > 0 && !features.max_frame_size {
            return Err(BackendError::unsupported(
                context,
                "MaxFrameSize",
                "当前 route/method 未确认支持 MaxFrameSize；前端应隐藏该字段",
            ));
        }
        if cfg.mbbrc && !features.mbbrc {
            return Err(BackendError::unsupported(
                context,
                "MBBRC",
                "当前 route/method 未确认支持宏块级码控；前端应隐藏该字段",
            ));
        }
        Ok(())
    }
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

    let has_display_adapter = dxgi_adapters.iter().any(|adapter| adapter.flags & 0x2 == 0);
    if !has_display_adapter {
        reasons.push("DXGI 未枚举到可输出桌面的硬件 adapter".to_owned());
    }
    let mut production_chroma_set = BTreeSet::new();
    let current_display_route_keys = vpl
        .current_display_routes
        .iter()
        .filter(|route| !route.fourcc.is_empty())
        .map(|route| {
            format!(
                "{}::{:?}::{}::{}",
                route.fourcc, route.chroma, route.bit_depth, route.profile
            )
        })
        .collect::<BTreeSet<_>>();
    for route in &vpl.route_candidates {
        let route_key = format!(
            "{}::{:?}::{}::{}",
            route.fourcc, route.chroma, route.bit_depth, route.profile
        );
        if route.production_record_supported && current_display_route_keys.contains(&route_key) {
            production_chroma_set.insert(route.chroma);
        }
    }
    if production_chroma_set.is_empty() {
        reasons
            .push("oneVPL 未确认任何可生产录制的 GPU-only 色度路线；前端隐藏色度字段".to_owned());
    }
    if vpl.current_display_routes.is_empty() {
        reasons.push("当前显示器自动 route 探测为空；不能按未知显示状态展示录制字段".to_owned());
    }
    for route in &vpl.current_display_routes {
        if route.fourcc.is_empty() {
            reasons.push(format!(
                "当前显示器 {} route 不可用：{}",
                route.chroma.doc_label(),
                route.note
            ));
        }
    }

    // 自动匹配只展示当前生产后端真正接线的色度类别；Query 可见但 writer/DXGI
    // 尚未接通的 route 仍只放在 vpl_candidate_chroma，不让前端展示成可录制。
    let desktop_sync_path_available = has_display_adapter
        && vpl.hevc_supported
        && d3d11_texture_input_supported
        && !production_chroma_set.is_empty();

    let supported_chroma = if desktop_sync_path_available {
        production_chroma_set.into_iter().collect()
    } else {
        Vec::new()
    };

    let mut rate_map: BTreeMap<ChromaSampling, Vec<BTreeSet<RateControlMethod>>> = BTreeMap::new();
    let mut feature_map: BTreeMap<
        ChromaSampling,
        BTreeMap<RateControlMethod, Vec<RateControlFeatureSupport>>,
    > = BTreeMap::new();
    if desktop_sync_path_available {
        for route in &vpl.route_candidates {
            let route_key = format!(
                "{}::{:?}::{}::{}",
                route.fourcc, route.chroma, route.bit_depth, route.profile
            );
            if route.production_record_supported && current_display_route_keys.contains(&route_key)
            {
                rate_map
                    .entry(route.chroma)
                    .or_default()
                    .push(route.rate_controls.iter().copied().collect());
                let by_method = feature_map.entry(route.chroma).or_default();
                for feature in &route.rate_control_features {
                    by_method
                        .entry(feature.method)
                        .or_default()
                        .push(RateControlFeatureSupport::from_probe(feature));
                }
            }
        }
    }
    let mut rate_controls_by_chroma = Vec::new();
    let mut rate_set = BTreeSet::new();
    for chroma in &supported_chroma {
        let Some(route_sets) = rate_map.get(chroma) else {
            continue;
        };
        let mut methods_set = BTreeSet::new();
        for route_set in route_sets {
            methods_set.extend(route_set.iter().copied());
        }
        let methods = methods_set.into_iter().collect::<Vec<_>>();
        for method in &methods {
            rate_set.insert(*method);
        }
        rate_controls_by_chroma.push(RateControlChromaSupport {
            chroma: *chroma,
            methods,
        });
    }
    let mut rate_control_features_by_chroma = Vec::new();
    for item in &rate_controls_by_chroma {
        let by_method = feature_map.get(&item.chroma);
        let mut features = Vec::new();
        for method in &item.methods {
            let route_features = by_method
                .and_then(|map| map.get(method))
                .map(Vec::as_slice)
                .unwrap_or(&[]);
            features.push(RateControlFeatureSupport::union(*method, route_features));
        }
        rate_control_features_by_chroma.push(RateControlChromaFeatureSupport {
            chroma: item.chroma,
            features,
        });
    }
    let async_depth_suggested = if desktop_sync_path_available {
        let route_suggestions = vpl
            .route_candidates
            .iter()
            .filter(|route| {
                let route_key = format!(
                    "{}::{:?}::{}::{}",
                    route.fourcc, route.chroma, route.bit_depth, route.profile
                );
                route.production_record_supported && current_display_route_keys.contains(&route_key)
            })
            .filter_map(|route| {
                (route.num_frame_suggested > 0).then_some(route.num_frame_suggested)
            });
        // oneVPL QueryIOSurf 返回的是所请求 AsyncDepth/GOP/route 下建议的 surface 数；
        // 这里向前端暴露一个保守可用值，而不是硬编码常数。实际录制仍会在 Init 前
        // 根据 DDA/WGC 固定路线设置 AsyncDepth 并再次 Query/Init 验证。
        route_suggestions
            .min()
            .map(|surfaces| surfaces.clamp(2, 16))
    } else {
        None
    };

    ProbeCaps {
        probe_time_unix_ms,
        dxgi_adapters,
        supported_chroma,
        vpl_candidate_chroma,
        supported_rate_controls: rate_set.into_iter().collect(),
        rate_controls_by_chroma,
        rate_control_features_by_chroma,
        desktop_sync_path_available,
        d3d11_texture_input_supported,
        async_depth_suggested,
        bit_depth_policy: vec![BitDepthMode::Internal8Bit, BitDepthMode::Internal10Bit],
        capture_cursor_policy: vec![
            CaptureCursorPolicy {
                backend: "DDA".to_owned(),
                cursor_recording: false,
                reason: "DDA 光标合成容易需要 CPU 元数据/合成；按 GPU-only 契约不录光标".to_owned(),
            },
            CaptureCursorPolicy {
                backend: "WGC".to_owned(),
                cursor_recording: true,
                reason: "WGC 按后端自动 route 使用 BGRA8/FP16 输入，并负责录制光标".to_owned(),
            },
        ],
        color_fidelity_policy: ColorFidelityPolicy {
            strategy: "按当前显示器状态与 DDA/WGC 实际数据做高保真；无法可靠确定的字段不在前端展示，并写入日志/采用明确推断策略".to_owned(),
            cannot_guarantee: vec![
                "Windows compositor 可能已把源应用颜色转换为桌面合成结果，无法还原源应用原始色彩".to_owned(),
                "DDA/WGC 不总是提供完整逐帧 primaries/transfer/matrix/range 元数据".to_owned(),
                "HDR/SDR 多显示器混合与 scRGB/BT.2020/PQ 转换需依据当前显示器状态推断".to_owned(),
                "HEVC VUI/SEI 颜色标记最终可写字段仍受 oneVPL/驱动支持限制".to_owned(),
                "当前生产 route 对 HLG、BT.601、未知/自定义/P3 类显示色彩空间保持 UnsupportedGpuPath，不改写成 BT.709 或 PQ 假装高保真".to_owned(),
            ],
        },
        audio_policy: AudioPolicy {
            target_format: "48kHz stereo float PCM -> AAC LC".to_owned(),
            resampling_allowed: true,
            timestamp_rule: "允许重采样和声道混合，但必须保留/重建绝对时间戳，音画同步禁止按帧号硬凑".to_owned(),
        },
        package_policy: PackagePolicy {
            single_exe_required: true,
            bundled_dependencies_allowed: true,
            note: "oneVPL dispatcher 及其用户态运行库内嵌于 EXE，启动时校验并释放到 ProgramData 配置目录；发布目录不携带 DLL。GPU 驱动、D3D11、Media Foundation 仍是系统/驱动前提".to_owned(),
        },
        path_blockers: reasons,
        vpl,
    }
}
