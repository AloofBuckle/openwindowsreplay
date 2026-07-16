//! GPU-only 后端总入口。

pub mod aac_mf;
pub mod audio;
pub mod dxgi;
pub mod mp4_mux;
pub mod nvenc;
pub mod pipeline;
pub mod session;
pub mod vpl;
pub mod wasapi;

use crate::config::ChromaSampling;
use crate::error::BackendError;
use crate::rate_control::{
    NvencMultiPass, NvencPreset, NvencSplitEncodeMode, RateControlConfig, RateControlMethod,
};
use serde::{Deserialize, Serialize};
use std::collections::{BTreeMap, BTreeSet};
use std::time::{SystemTime, UNIX_EPOCH};

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ProbeCaps {
    pub probe_time_unix_ms: u128,
    pub dxgi_adapters: Vec<dxgi::DxgiAdapterInfo>,
    pub vpl: vpl::VplProbeInfo,
    /// NVIDIA NVENC 驱动/SDK 运行时探测。NVENC fork 保留 oneVPL 探测，同时
    /// 独立枚举 NVENC D3D11 HEVC 能力，后续由自动后端选择器按 adapter/route 决定。
    pub nvenc: nvenc::NvencProbeInfo,
    /// 自动视频编码器选择结果。oneVPL 生产路径仍优先；当 oneVPL 不能形成当前
    /// 显示器 GPU-only HEVC 路径而 NVENC NV12/P010 或 SDR AYUV route 可用时，
    /// NVENC 会作为 production-ready fallback 被选中。
    pub video_encoder_selection: VideoEncoderSelection,
    /// oneVPL 报告的候选色度采样；这还不是最终可录制路径。
    pub vpl_candidate_chroma: Vec<ChromaSampling>,
    /// 当前完整桌面同步路径可展示的色度采样；不可用时前端直接隐藏字段。
    pub supported_chroma: Vec<ChromaSampling>,
    /// 当前 active 编码后端验证的码控模式；不可用模式前端直接隐藏。
    pub supported_rate_controls: Vec<RateControlMethod>,
    /// 按可生产录制色度采样聚合后的码控可用性。前端先选色度，再只展示该色度
    /// 下至少一条当前显示器生产 route 逐项 Query 通过的 RateControlMethod。
    pub rate_controls_by_chroma: Vec<RateControlChromaSupport>,
    /// 按可生产录制色度采样/码控模式聚合后的可选码控字段可见性。GUI 隐藏
    /// 所有当前显示器生产 route 都不支持的字段；录制时仍逐 route Query/Init 验证。
    pub rate_control_features_by_chroma: Vec<RateControlChromaFeatureSupport>,
    /// 是否已经形成“当前显示器状态 + DDA/WGC 数据 + GPU 转换 + HEVC 硬编”的完整路径。
    pub desktop_sync_path_available: bool,
    /// oneVPL 或 NVENC 是否报告 D3D11 texture 视频内存输入能力；它不是完整路径的充分条件。
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

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum VideoEncoderBackend {
    OneVpl,
    Nvenc,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct RecordTarget {
    pub adapter_index: u32,
    pub output_index: u32,
}

impl VideoEncoderBackend {
    pub const fn label(self) -> &'static str {
        match self {
            Self::OneVpl => "oneVPL",
            Self::Nvenc => "NVENC",
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct VideoEncoderCandidate {
    pub backend: VideoEncoderBackend,
    pub available: bool,
    pub hevc_supported: bool,
    pub d3d11_texture_input_supported: bool,
    pub current_display_route_count: usize,
    pub production_ready: bool,
    pub reason: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct VideoEncoderSelection {
    pub active: Option<VideoEncoderBackend>,
    pub candidates: Vec<VideoEncoderCandidate>,
    pub reason: String,
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

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct NvencTuningSupport {
    pub presets: Vec<NvencPreset>,
    pub split_encode_modes: Vec<NvencSplitEncodeMode>,
    pub multi_pass_modes: Vec<NvencMultiPass>,
    pub spatial_aq: bool,
    pub encoder_engines: u32,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct RateControlFeatureSupport {
    pub method: RateControlMethod,
    pub brc_param_multiplier: bool,
    pub look_ahead_depth: bool,
    pub look_ahead_depth_max: u16,
    pub win_brc: bool,
    pub low_delay_brc: bool,
    pub max_frame_size: bool,
    pub mbbrc: bool,
    pub nvenc_spatial_aq: bool,
    pub nvenc_temporal_aq: bool,
    pub nvenc_target_quality: bool,
}

impl RateControlFeatureSupport {
    pub const fn hidden(method: RateControlMethod) -> Self {
        Self {
            method,
            brc_param_multiplier: false,
            look_ahead_depth: false,
            look_ahead_depth_max: 0,
            win_brc: false,
            low_delay_brc: false,
            max_frame_size: false,
            mbbrc: false,
            nvenc_spatial_aq: false,
            nvenc_temporal_aq: false,
            nvenc_target_quality: false,
        }
    }

    fn from_probe(probe: &vpl::VplRateControlFeatureProbe) -> Self {
        Self {
            method: probe.method,
            brc_param_multiplier: true,
            look_ahead_depth: probe.look_ahead_depth,
            look_ahead_depth_max: if probe.look_ahead_depth { 100 } else { 0 },
            win_brc: probe.win_brc,
            low_delay_brc: probe.low_delay_brc,
            max_frame_size: probe.max_frame_size,
            mbbrc: probe.mbbrc,
            nvenc_spatial_aq: false,
            nvenc_temporal_aq: false,
            nvenc_target_quality: false,
        }
    }

    fn from_nvenc_probe(
        probe: &nvenc::NvencRateControlFeatureProbe,
        look_ahead_depth_max: u16,
    ) -> Self {
        Self {
            method: probe.method,
            // BRCParamMultiplier 是 oneVPL mfxInfoMFX 字段；NVENC bit/s 字段为 u32，
            // 不向 GUI 暴露该 oneVPL 专用倍率。
            brc_param_multiplier: false,
            look_ahead_depth: probe.lookahead,
            look_ahead_depth_max: if probe.lookahead {
                look_ahead_depth_max.min(31)
            } else {
                0
            },
            win_brc: false,
            low_delay_brc: false,
            max_frame_size: false,
            mbbrc: false,
            nvenc_spatial_aq: probe.spatial_aq,
            nvenc_temporal_aq: probe.temporal_aq,
            nvenc_target_quality: probe.target_quality,
        }
    }

    fn union(method: RateControlMethod, probes: &[Self]) -> Self {
        let Some(first) = probes.first().copied() else {
            return Self::hidden(method);
        };
        let mut out = probes.iter().skip(1).fold(first, |mut acc, item| {
            acc.brc_param_multiplier |= item.brc_param_multiplier;
            acc.look_ahead_depth |= item.look_ahead_depth;
            acc.look_ahead_depth_max = match (acc.look_ahead_depth_max, item.look_ahead_depth_max) {
                (0, value) | (value, 0) => value,
                (left, right) => left.min(right),
            };
            acc.win_brc |= item.win_brc;
            acc.low_delay_brc |= item.low_delay_brc;
            acc.max_frame_size |= item.max_frame_size;
            acc.mbbrc |= item.mbbrc;
            acc.nvenc_spatial_aq |= item.nvenc_spatial_aq;
            acc.nvenc_temporal_aq |= item.nvenc_temporal_aq;
            acc.nvenc_target_quality |= item.nvenc_target_quality;
            acc
        });
        out.method = method;
        out
    }
}

impl ProbeCaps {
    pub fn preferred_vpl_route_for_chroma(
        &self,
        chroma: ChromaSampling,
    ) -> Option<&vpl::VplCurrentDisplayRouteInfo> {
        self.vpl
            .current_display_routes
            .iter()
            .filter(|route| {
                route.chroma == chroma
                    && !route.fourcc.is_empty()
                    && route.implementation_index != u32::MAX
            })
            .min_by_key(|route| {
                display_route_priority(
                    route.desktop_left,
                    route.desktop_top,
                    route.desktop_right,
                    route.desktop_bottom,
                    route.adapter_index,
                    route.output_index,
                )
            })
    }

    pub fn preferred_nvenc_route_for_chroma(
        &self,
        chroma: ChromaSampling,
    ) -> Option<&nvenc::NvencCurrentDisplayRouteInfo> {
        self.nvenc
            .current_display_routes
            .iter()
            .filter(|route| route.chroma == chroma && !route.input_format.is_empty())
            .min_by_key(|route| {
                display_route_priority(
                    route.desktop_left,
                    route.desktop_top,
                    route.desktop_right,
                    route.desktop_bottom,
                    route.adapter_index,
                    route.output_index,
                )
            })
    }

    pub fn record_target_for_chroma(&self, chroma: ChromaSampling) -> Option<RecordTarget> {
        match self.video_encoder_selection.active {
            Some(VideoEncoderBackend::OneVpl) => {
                self.preferred_vpl_route_for_chroma(chroma)
                    .map(|route| RecordTarget {
                        adapter_index: route.adapter_index,
                        output_index: route.output_index,
                    })
            }
            Some(VideoEncoderBackend::Nvenc) => {
                self.preferred_nvenc_route_for_chroma(chroma)
                    .map(|route| RecordTarget {
                        adapter_index: route.adapter_index,
                        output_index: route.output_index,
                    })
            }
            None => None,
        }
    }

    pub fn route_blocker_summary_for_chroma(&self, chroma: ChromaSampling) -> Option<String> {
        let mut blockers = BTreeSet::new();
        let include_vpl = !matches!(
            self.video_encoder_selection.active,
            Some(VideoEncoderBackend::Nvenc)
        );
        let include_nvenc = !matches!(
            self.video_encoder_selection.active,
            Some(VideoEncoderBackend::OneVpl)
        );

        if include_vpl {
            blockers.extend(
                self.vpl
                    .current_display_routes
                    .iter()
                    .filter(|route| route.chroma == chroma && route.fourcc.is_empty())
                    .map(|route| format!("oneVPL 当前显示器 route：{}", route.note)),
            );
            if self.vpl.current_display_routes.is_empty() {
                blockers.insert("oneVPL 当前显示器自动 route 探测为空".to_owned());
            }
            blockers.extend(
                self.vpl
                    .route_candidates
                    .iter()
                    .filter(|route| route.chroma == chroma && !route.production_record_supported)
                    .map(|route| {
                        format!(
                            "oneVPL FourCC={} bit_depth={} profile={}：{}",
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
        }

        if include_nvenc {
            blockers.extend(
                self.nvenc
                    .current_display_routes
                    .iter()
                    .filter(|route| route.chroma == chroma && route.input_format.is_empty())
                    .map(|route| format!("NVENC 当前显示器 route：{}", route.note)),
            );
            if self.nvenc.current_display_routes.is_empty() {
                blockers.insert("NVENC 当前显示器自动 route 探测为空".to_owned());
            }
            blockers.extend(
                self.nvenc
                    .route_candidates
                    .iter()
                    .filter(|route| route.chroma == chroma && !route.production_record_supported)
                    .map(|route| {
                        format!(
                            "NVENC input={} bit_depth={} profile={}：{}",
                            route.input_format,
                            route.bit_depth,
                            route.profile,
                            route
                                .production_blocker
                                .as_deref()
                                .unwrap_or(route.note.as_str())
                        )
                    }),
            );
        }

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

    pub fn nvenc_tuning_support_for_chroma(
        &self,
        chroma: ChromaSampling,
    ) -> Option<NvencTuningSupport> {
        if !matches!(
            self.video_encoder_selection.active,
            Some(VideoEncoderBackend::Nvenc)
        ) {
            return None;
        }
        let route = self
            .preferred_nvenc_route_for_chroma(chroma)
            .filter(|route| {
                self.nvenc
                    .adapters
                    .iter()
                    .find(|adapter| adapter.adapter_index == route.adapter_index)
                    .is_some_and(|adapter| {
                        adapter.route_candidates.iter().any(|candidate| {
                            candidate.production_record_supported
                                && candidate.chroma == route.chroma
                                && candidate.input_format == route.input_format
                                && candidate.bit_depth == route.bit_depth
                                && candidate.profile == route.profile
                        })
                    })
            })?;
        let adapter = self
            .nvenc
            .adapters
            .iter()
            .find(|adapter| adapter.adapter_index == route.adapter_index)?;
        let encoder_engines = adapter.caps.encoder_engines.unwrap_or(1).max(1);
        let split_encode_modes = NvencSplitEncodeMode::all().to_vec();
        Some(NvencTuningSupport {
            presets: adapter.hevc_presets.clone(),
            split_encode_modes,
            multi_pass_modes: NvencMultiPass::all().to_vec(),
            spatial_aq: true,
            encoder_engines,
        })
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
                "active 编码后端能力探测未确认该 RateControlMethod 在当前色度生产 route 上可用",
            ));
        }
        if matches!(
            self.video_encoder_selection.active,
            Some(VideoEncoderBackend::Nvenc)
        ) {
            let tuning = self
                .nvenc_tuning_support_for_chroma(chroma)
                .ok_or_else(|| {
                    BackendError::unsupported(
                        context,
                        "NVENC 原始调参",
                        "当前显示器生产 route 没有可对应的 NVENC preset/engine 能力",
                    )
                })?;
            if cfg.brc_param_multiplier != 1 {
                return Err(BackendError::unsupported(
                    context,
                    "BRCParamMultiplier",
                    "BRCParamMultiplier 是 oneVPL 专用字段；NVENC 使用 u32 bit/s 字段，前端应隐藏该倍率",
                ));
            }
            if !tuning.presets.contains(&cfg.nvenc_preset) {
                return Err(BackendError::unsupported(
                    context,
                    cfg.nvenc_preset.raw_name(),
                    "当前 NVENC HEVC session 未枚举到该 preset GUID；前端应只展示驱动返回的 P1..P7",
                ));
            }
            if !tuning
                .split_encode_modes
                .contains(&cfg.nvenc_split_encode_mode)
            {
                return Err(BackendError::unsupported(
                    context,
                    cfg.nvenc_split_encode_mode.raw_name(),
                    format!(
                        "该 splitEncodeMode 不在当前 SDK 暴露的原始值集合中；GPU 报告 {} 个 NVENC engine",
                        tuning.encoder_engines,
                    ),
                ));
            }
            if !tuning.multi_pass_modes.contains(&cfg.nvenc_multi_pass) {
                return Err(BackendError::unsupported(
                    context,
                    cfg.nvenc_multi_pass.raw_name(),
                    "当前 HEVC production route 未确认该 NV_ENC_MULTI_PASS 原始值",
                ));
            }
            if cfg.nvenc_spatial_aq && !tuning.spatial_aq {
                return Err(BackendError::unsupported(
                    context,
                    "NVENC Spatial AQ",
                    "当前 NVENC route 未确认空间 AQ 可用",
                ));
            }
            if cfg.nvenc_temporal_aq || cfg.nvenc_aq_strength != 0 {
                return Err(BackendError::unsupported(
                    context,
                    "NVENC extra tuning",
                    "允许用户修改的额外调参仅包括 preset、splitEncodeMode、multiPass 与 Spatial AQ；Temporal AQ/AQ strength 必须保持默认",
                ));
            }
            cfg.to_nvenc_fields()
                .map_err(|err| BackendError::unsupported(context, "NVENC RateControl", err))?;
        }
        let features = self.rate_control_features_for(chroma, cfg.method);
        if matches!(
            self.video_encoder_selection.active,
            Some(VideoEncoderBackend::Nvenc)
        ) {
            if cfg.nvenc_spatial_aq && !features.nvenc_spatial_aq {
                return Err(BackendError::unsupported(
                    context,
                    "NVENC Spatial AQ",
                    "当前 NVENC route/method 未确认支持空间 AQ；前端应隐藏该字段",
                ));
            }
            if cfg.nvenc_temporal_aq && !features.nvenc_temporal_aq {
                return Err(BackendError::unsupported(
                    context,
                    "NVENC Temporal AQ",
                    "当前 NVENC caps 未确认支持时间 AQ；前端应隐藏该字段",
                ));
            }
            if cfg.nvenc_vbr_target_quality > 0 && !features.nvenc_target_quality {
                return Err(BackendError::unsupported(
                    context,
                    "NVENC targetQuality",
                    "当前 NVENC method 未确认支持 targetQuality；前端应隐藏该字段",
                ));
            }
        }
        if cfg.ext_brc {
            return Err(BackendError::unsupported(
                context,
                "ExtBRC",
                "External BRC 需要 mfxExtBRC 回调；当前生产后端未接入，GUI 隐藏该字段",
            ));
        }
        let uses_lookahead = match self.video_encoder_selection.active {
            Some(VideoEncoderBackend::Nvenc) => {
                matches!(cfg.method, RateControlMethod::Cbr | RateControlMethod::Vbr)
                    && cfg.look_ahead_depth > 0
            }
            _ => {
                matches!(
                    cfg.method,
                    RateControlMethod::La | RateControlMethod::LaIcq | RateControlMethod::LaHrd
                ) && cfg.look_ahead_depth > 0
            }
        };
        if uses_lookahead && !features.look_ahead_depth {
            return Err(BackendError::unsupported(
                context,
                "LookAheadDepth",
                "当前 route/method 未确认支持 LookAheadDepth；前端应隐藏该字段",
            ));
        }
        if uses_lookahead && cfg.look_ahead_depth > features.look_ahead_depth_max {
            return Err(BackendError::unsupported(
                context,
                format!("LookAheadDepth={}", cfg.look_ahead_depth),
                format!(
                    "当前 route 的 GPU surface 池最多允许 LookAheadDepth={}；前端应按能力上限限制该字段",
                    features.look_ahead_depth_max
                ),
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

fn display_route_priority(
    left: i32,
    top: i32,
    right: i32,
    bottom: i32,
    adapter_index: u32,
    output_index: u32,
) -> (bool, u32, u32) {
    let contains_desktop_origin = left <= 0 && right > 0 && top <= 0 && bottom > 0;
    (!contains_desktop_origin, adapter_index, output_index)
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
            ) + &format!(
                "，NVENC: {}；自动选择：{}",
                if self.nvenc.available {
                    if self.nvenc.hevc_supported {
                        "HEVC 可见"
                    } else {
                        "已加载但无 HEVC"
                    }
                } else {
                    "不可用"
                },
                self.video_encoder_selection.reason
            )
        } else {
            format!(
                "已验证色度采样: {}；码控模式: {} 个；active={}；oneVPL={} NVENC={}",
                self.supported_chroma
                    .iter()
                    .map(|c| c.doc_label())
                    .collect::<Vec<_>>()
                    .join("/"),
                self.supported_rate_controls.len(),
                self.video_encoder_selection
                    .active
                    .map(VideoEncoderBackend::label)
                    .unwrap_or("无"),
                if self.vpl.hevc_supported {
                    "HEVC"
                } else {
                    "无"
                },
                if self.nvenc.hevc_supported {
                    "HEVC"
                } else {
                    "无"
                }
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
    let nvenc = nvenc::probe_nvenc_adapters(&dxgi_adapters);
    reasons.extend(vpl.warnings.iter().cloned());
    reasons.extend(
        nvenc
            .warnings
            .iter()
            .map(|warning| format!("NVENC: {warning}")),
    );

    let vpl_candidate_chroma = vpl.chroma_candidates.clone();
    let d3d11_texture_input_supported =
        vpl.dx11_texture_input_seen || nvenc.d3d11_texture_input_seen;

    if !vpl.hevc_supported && !nvenc.hevc_supported {
        reasons.push("没有可用 HEVC 硬件编码实现；禁止软件编码/AVC 降级".to_owned());
    }
    if !d3d11_texture_input_supported {
        reasons
            .push("oneVPL/NVENC 均未报告 D3D11 texture 输入；不满足 GPU-only 输入前提".to_owned());
    }
    if vpl.rate_controls.is_empty() && nvenc.rate_controls.is_empty() {
        reasons.push("当前运行时未确认任何可用码率控制模式；前端隐藏码控字段".to_owned());
    }

    let has_display_adapter = dxgi_adapters.iter().any(|adapter| adapter.flags & 0x2 == 0);
    if !has_display_adapter {
        reasons.push("DXGI 未枚举到可输出桌面的硬件 adapter".to_owned());
    }

    let mut onevpl_production_chroma_set = BTreeSet::new();
    for route in &vpl.current_display_routes {
        if !route.fourcc.is_empty() && route.implementation_index != u32::MAX {
            onevpl_production_chroma_set.insert(route.chroma);
        }
    }

    let mut nvenc_production_chroma_set = BTreeSet::new();
    for route in &nvenc.current_display_routes {
        if !route.input_format.is_empty() {
            nvenc_production_chroma_set.insert(route.chroma);
        }
    }

    if onevpl_production_chroma_set.is_empty() && nvenc_production_chroma_set.is_empty() {
        reasons.push(
            "oneVPL/NVENC 均未确认任何可生产录制的 GPU-only 色度路线；前端隐藏色度字段".to_owned(),
        );
    }
    if vpl.current_display_routes.is_empty() && nvenc.current_display_routes.is_empty() {
        reasons.push("当前显示器自动 route 探测为空；不能按未知显示状态展示录制字段".to_owned());
    }
    for route in &vpl.current_display_routes {
        if route.fourcc.is_empty() {
            reasons.push(format!(
                "当前显示器 oneVPL {} route 不可用：{}",
                route.chroma.doc_label(),
                route.note
            ));
        }
    }
    for route in &nvenc.current_display_routes {
        if route.input_format.is_empty() {
            reasons.push(format!(
                "当前显示器 NVENC {} route 不可用：{}",
                route.chroma.doc_label(),
                route.note
            ));
        }
    }

    // 自动匹配只展示当前生产后端真正接线的色度类别；Query 可见但 writer/DXGI
    // 尚未接通的 route 仍只放在候选列表，不让前端展示成可录制。
    let onevpl_desktop_sync_path_available = has_display_adapter
        && vpl.hevc_supported
        && vpl.dx11_texture_input_seen
        && !onevpl_production_chroma_set.is_empty();
    let nvenc_desktop_sync_path_available = has_display_adapter
        && nvenc.hevc_supported
        && nvenc.d3d11_texture_input_seen
        && !nvenc_production_chroma_set.is_empty();
    let video_encoder_selection = select_video_encoder(
        &vpl,
        &nvenc,
        onevpl_desktop_sync_path_available,
        nvenc_desktop_sync_path_available,
    );
    let desktop_sync_path_available = video_encoder_selection.active.is_some();

    let supported_chroma = match video_encoder_selection.active {
        Some(VideoEncoderBackend::OneVpl) => onevpl_production_chroma_set.iter().copied().collect(),
        Some(VideoEncoderBackend::Nvenc) => nvenc_production_chroma_set.iter().copied().collect(),
        None => Vec::new(),
    };

    let mut rate_map: BTreeMap<ChromaSampling, Vec<BTreeSet<RateControlMethod>>> = BTreeMap::new();
    let mut feature_map: BTreeMap<
        ChromaSampling,
        BTreeMap<RateControlMethod, Vec<RateControlFeatureSupport>>,
    > = BTreeMap::new();
    if desktop_sync_path_available {
        match video_encoder_selection.active {
            Some(VideoEncoderBackend::OneVpl) => {
                for display_route in vpl.current_display_routes.iter().filter(|route| {
                    !route.fourcc.is_empty() && route.implementation_index != u32::MAX
                }) {
                    let Some(route) = vpl
                        .implementations
                        .iter()
                        .find(|implementation| {
                            implementation.index == display_route.implementation_index
                        })
                        .and_then(|implementation| {
                            implementation.route_candidates.iter().find(|candidate| {
                                candidate.production_record_supported
                                    && candidate.fourcc == display_route.fourcc
                                    && candidate.chroma == display_route.chroma
                                    && candidate.bit_depth == display_route.bit_depth
                                    && candidate.profile == display_route.profile
                            })
                        })
                    else {
                        continue;
                    };
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
            Some(VideoEncoderBackend::Nvenc) => {
                for display_route in nvenc
                    .current_display_routes
                    .iter()
                    .filter(|route| !route.input_format.is_empty())
                {
                    let look_ahead_depth_max =
                        nvenc::lookahead_depth_max_for_current_display_route(display_route);
                    let Some(route) = nvenc
                        .adapters
                        .iter()
                        .find(|adapter| adapter.adapter_index == display_route.adapter_index)
                        .and_then(|adapter| {
                            adapter.route_candidates.iter().find(|candidate| {
                                candidate.production_record_supported
                                    && candidate.input_format == display_route.input_format
                                    && candidate.chroma == display_route.chroma
                                    && candidate.bit_depth == display_route.bit_depth
                                    && candidate.profile == display_route.profile
                            })
                        })
                    else {
                        continue;
                    };
                    rate_map
                        .entry(route.chroma)
                        .or_default()
                        .push(route.rate_controls.iter().copied().collect());
                    let by_method = feature_map.entry(route.chroma).or_default();
                    for method in &route.rate_controls {
                        if let Some(feature) = route
                            .rate_control_features
                            .iter()
                            .find(|feature| feature.method == *method)
                        {
                            by_method.entry(*method).or_default().push(
                                RateControlFeatureSupport::from_nvenc_probe(
                                    feature,
                                    look_ahead_depth_max,
                                ),
                            );
                        } else {
                            by_method
                                .entry(*method)
                                .or_default()
                                .push(RateControlFeatureSupport::hidden(*method));
                        }
                    }
                }
            }
            None => {}
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
    let async_depth_suggested = if matches!(
        video_encoder_selection.active,
        Some(VideoEncoderBackend::OneVpl)
    ) {
        let route_suggestions = vpl
            .current_display_routes
            .iter()
            .filter(|route| !route.fourcc.is_empty() && route.implementation_index != u32::MAX)
            .filter_map(|display_route| {
                vpl.implementations
                    .iter()
                    .find(|implementation| {
                        implementation.index == display_route.implementation_index
                    })
                    .and_then(|implementation| {
                        implementation.route_candidates.iter().find(|route| {
                            route.production_record_supported
                                && route.fourcc == display_route.fourcc
                                && route.chroma == display_route.chroma
                                && route.bit_depth == display_route.bit_depth
                                && route.profile == display_route.profile
                        })
                    })
                    .and_then(|route| {
                        (route.num_frame_suggested > 0).then_some(route.num_frame_suggested)
                    })
            });
        // oneVPL QueryIOSurf 返回的是所请求 AsyncDepth/GOP/route 下建议的 surface 数；
        // 这里向前端暴露一个保守可用值，而不是硬编码常数。实际录制仍会在 Init 前
        // 根据 DDA/WGC 固定路线设置 AsyncDepth 并再次 Query/Init 验证。
        route_suggestions
            .min()
            .map(|surfaces| surfaces.clamp(2, 16))
    } else if matches!(
        video_encoder_selection.active,
        Some(VideoEncoderBackend::Nvenc)
    ) {
        // 当前 NVENC 接线为同步 registered-resource 编码，捕获 snapshot pool 仍独立固定；
        // 不向前端暴露 oneVPL AsyncDepth 概念。
        None
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
        nvenc,
        video_encoder_selection,
        vpl,
    }
}

fn select_video_encoder(
    vpl: &vpl::VplProbeInfo,
    nvenc: &nvenc::NvencProbeInfo,
    onevpl_desktop_sync_ready: bool,
    nvenc_desktop_sync_ready: bool,
) -> VideoEncoderSelection {
    let onevpl = VideoEncoderCandidate {
        backend: VideoEncoderBackend::OneVpl,
        available: vpl.available,
        hevc_supported: vpl.hevc_supported,
        d3d11_texture_input_supported: vpl.dx11_texture_input_seen,
        current_display_route_count: vpl
            .current_display_routes
            .iter()
            .filter(|route| !route.fourcc.is_empty() && route.implementation_index != u32::MAX)
            .count(),
        production_ready: onevpl_desktop_sync_ready,
        reason: if onevpl_desktop_sync_ready {
            "oneVPL 已形成当前显示器 D3D11 GPU-only HEVC 生产路径".to_owned()
        } else if !vpl.available {
            vpl.load_error
                .clone()
                .unwrap_or_else(|| "oneVPL dispatcher 不可用".to_owned())
        } else if !vpl.hevc_supported {
            "oneVPL 未报告 HEVC 硬件编码".to_owned()
        } else if !vpl.dx11_texture_input_seen {
            "oneVPL 未报告 D3D11 texture 视频内存输入".to_owned()
        } else {
            "oneVPL 未形成当前显示器完整生产 route".to_owned()
        },
    };
    let nvenc_candidate = VideoEncoderCandidate {
        backend: VideoEncoderBackend::Nvenc,
        available: nvenc.available,
        hevc_supported: nvenc.hevc_supported,
        d3d11_texture_input_supported: nvenc.d3d11_texture_input_seen,
        current_display_route_count: nvenc
            .current_display_routes
            .iter()
            .filter(|route| !route.input_format.is_empty())
            .count(),
        production_ready: nvenc_desktop_sync_ready,
        reason: if nvenc_desktop_sync_ready {
            "NVENC 已形成当前显示器 D3D11 GPU-only HEVC 生产路径（NV12/P010 或 SDR AYUV）"
                .to_owned()
        } else if !nvenc.available {
            nvenc
                .load_error
                .clone()
                .unwrap_or_else(|| "nvEncodeAPI64.dll 不可用".to_owned())
        } else if !nvenc.hevc_supported {
            "NVENC 未报告 HEVC encode GUID".to_owned()
        } else if !nvenc.d3d11_texture_input_seen {
            "NVENC D3D11 会话或输入格式不可用".to_owned()
        } else if nvenc.current_display_routes.is_empty() {
            "NVENC D3D11/HEVC 可用，但当前显示器没有可匹配 route 或不在 NVIDIA 输出上".to_owned()
        } else {
            "NVENC D3D11/HEVC/current-display route 已探测，但没有匹配 NV12/P010/SDR AYUV 的可生产路线"
                .to_owned()
        },
    };

    let candidates = vec![onevpl, nvenc_candidate];
    let active = candidates
        .iter()
        .find(|candidate| candidate.production_ready)
        .map(|candidate| candidate.backend);
    let reason = active
        .map(|backend| format!("选择 {}", backend.label()))
        .unwrap_or_else(|| {
            candidates
                .iter()
                .map(|candidate| format!("{}: {}", candidate.backend.label(), candidate.reason))
                .collect::<Vec<_>>()
                .join("；")
        });

    VideoEncoderSelection {
        active,
        candidates,
        reason,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn empty_vpl_probe() -> vpl::VplProbeInfo {
        vpl::VplProbeInfo {
            available: true,
            dll_path: None,
            load_error: None,
            implementations: Vec::new(),
            hevc_supported: true,
            hevc_profiles: Vec::new(),
            input_fourcc: Vec::new(),
            chroma_candidates: Vec::new(),
            route_candidates: Vec::new(),
            current_display_routes: Vec::new(),
            rate_controls: Vec::new(),
            dx11_texture_input_seen: true,
            warnings: Vec::new(),
        }
    }

    fn empty_nvenc_probe() -> nvenc::NvencProbeInfo {
        nvenc::NvencProbeInfo {
            available: true,
            dll_path: None,
            load_error: None,
            compiled_api_version: String::new(),
            max_supported_version: None,
            adapters: Vec::new(),
            hevc_supported: true,
            hevc_profiles: Vec::new(),
            hevc_presets: Vec::new(),
            input_formats: Vec::new(),
            chroma_candidates: Vec::new(),
            route_candidates: Vec::new(),
            current_display_routes: Vec::new(),
            rate_controls: Vec::new(),
            d3d11_texture_input_seen: true,
            warnings: Vec::new(),
        }
    }

    #[test]
    fn automatic_encoder_selection_prioritizes_onevpl_then_falls_back_to_nvenc() {
        let vpl = empty_vpl_probe();
        let nvenc = empty_nvenc_probe();

        let both_ready = select_video_encoder(&vpl, &nvenc, true, true);
        assert_eq!(both_ready.active, Some(VideoEncoderBackend::OneVpl));

        let nvenc_fallback = select_video_encoder(&vpl, &nvenc, false, true);
        assert_eq!(nvenc_fallback.active, Some(VideoEncoderBackend::Nvenc));

        let neither_ready = select_video_encoder(&vpl, &nvenc, false, false);
        assert_eq!(neither_ready.active, None);
    }

    #[test]
    fn display_route_priority_prefers_the_output_containing_the_desktop_origin() {
        let secondary = display_route_priority(3840, 0, 7680, 2160, 0, 0);
        let primary = display_route_priority(0, 0, 3840, 2160, 1, 2);
        assert!(primary < secondary);
        assert_eq!(
            display_route_priority(0, 0, 1920, 1080, 0, 3),
            (false, 0, 3)
        );
    }

    #[test]
    fn split_encode_modes_expose_every_sdk_value() {
        assert_eq!(
            NvencSplitEncodeMode::all(),
            [
                NvencSplitEncodeMode::Auto,
                NvencSplitEncodeMode::AutoForced,
                NvencSplitEncodeMode::TwoForced,
                NvencSplitEncodeMode::ThreeForced,
                NvencSplitEncodeMode::FourForced,
                NvencSplitEncodeMode::Disabled,
            ]
        );
    }

    #[test]
    #[ignore = "需要本机 DXGI/oneVPL/NVENC 运行时；手动验证完整自动探测与后端选择"]
    fn local_automatic_encoder_selection_smoke() {
        let caps = probe_all();
        println!(
            "{}",
            serde_json::to_string_pretty(&caps.video_encoder_selection).unwrap()
        );
        assert_eq!(
            caps.video_encoder_selection
                .candidates
                .iter()
                .map(|candidate| candidate.backend)
                .collect::<Vec<_>>(),
            vec![VideoEncoderBackend::OneVpl, VideoEncoderBackend::Nvenc]
        );
        let first_ready = caps
            .video_encoder_selection
            .candidates
            .iter()
            .find(|candidate| candidate.production_ready)
            .map(|candidate| candidate.backend);
        assert_eq!(caps.video_encoder_selection.active, first_ready);
        assert!(
            caps.video_encoder_selection.active.is_some(),
            "本机应至少形成一条 oneVPL/NVENC GPU-only HEVC 生产路径"
        );
    }
}
