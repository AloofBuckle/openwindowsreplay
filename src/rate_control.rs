#![allow(dead_code)]
//! oneVPL RateControlMethod 的 GUI/配置映射。
//!
//! 注意：oneVPL 的 `mfxInfoMFX` 对多个字段使用 C union。RustReplay 的 GUI
//! 只在选定模式下显示对应字段，避免把同一 union 位置写入错误含义。

use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
pub enum RateControlMethod {
    Cbr,
    Vbr,
    Cqp,
    Avbr,
    La,
    Icq,
    Vcm,
    LaIcq,
    LaHrd,
    Qvbr,
}

impl RateControlMethod {
    pub const fn all() -> [Self; 10] {
        [
            Self::Cbr,
            Self::Vbr,
            Self::Cqp,
            Self::Avbr,
            Self::La,
            Self::Icq,
            Self::Vcm,
            Self::LaIcq,
            Self::LaHrd,
            Self::Qvbr,
        ]
    }

    pub const fn label(self) -> &'static str {
        match self {
            Self::Cbr => "CBR - 恒定码率",
            Self::Vbr => "VBR - 可变码率",
            Self::Cqp => "CQP - 固定 QP",
            Self::Avbr => "AVBR - 平均 VBR",
            Self::La => "LA - 前瞻 VBR",
            Self::Icq => "ICQ - 智能恒定质量",
            Self::Vcm => "VCM - 视频会议模式",
            Self::LaIcq => "LA_ICQ - 前瞻 ICQ",
            Self::LaHrd => "LA_HRD - 前瞻 HRD",
            Self::Qvbr => "QVBR - 质量定义 VBR",
        }
    }

    pub const fn short_name(self) -> &'static str {
        match self {
            Self::Cbr => "CBR",
            Self::Vbr => "VBR",
            Self::Cqp => "CQP",
            Self::Avbr => "AVBR",
            Self::La => "LA",
            Self::Icq => "ICQ",
            Self::Vcm => "VCM",
            Self::LaIcq => "LA_ICQ",
            Self::LaHrd => "LA_HRD",
            Self::Qvbr => "QVBR",
        }
    }

    pub const fn vpl_value(self) -> u16 {
        match self {
            Self::Cbr => 1,
            Self::Vbr => 2,
            Self::Cqp => 3,
            Self::Avbr => 4,
            Self::La => 8,
            Self::Icq => 9,
            Self::Vcm => 10,
            Self::LaIcq => 11,
            Self::LaHrd => 13,
            Self::Qvbr => 14,
        }
    }

    pub const fn from_vpl_value(value: u16) -> Option<Self> {
        match value {
            1 => Some(Self::Cbr),
            2 => Some(Self::Vbr),
            3 => Some(Self::Cqp),
            4 => Some(Self::Avbr),
            8 => Some(Self::La),
            9 => Some(Self::Icq),
            10 => Some(Self::Vcm),
            11 => Some(Self::LaIcq),
            13 => Some(Self::LaHrd),
            14 => Some(Self::Qvbr),
            // MFX_RATECONTROL_LA_EXT 已在当前文档中移除，故这里故意不接受 12。
            _ => None,
        }
    }
}

#[derive(Debug, Default, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
pub enum NvencPreset {
    P1,
    P2,
    P3,
    #[default]
    P4,
    P5,
    P6,
    P7,
}

impl NvencPreset {
    pub const fn all() -> [Self; 7] {
        [
            Self::P1,
            Self::P2,
            Self::P3,
            Self::P4,
            Self::P5,
            Self::P6,
            Self::P7,
        ]
    }

    pub const fn number(self) -> u8 {
        match self {
            Self::P1 => 1,
            Self::P2 => 2,
            Self::P3 => 3,
            Self::P4 => 4,
            Self::P5 => 5,
            Self::P6 => 6,
            Self::P7 => 7,
        }
    }

    pub const fn raw_name(self) -> &'static str {
        match self {
            Self::P1 => "NV_ENC_PRESET_P1_GUID",
            Self::P2 => "NV_ENC_PRESET_P2_GUID",
            Self::P3 => "NV_ENC_PRESET_P3_GUID",
            Self::P4 => "NV_ENC_PRESET_P4_GUID",
            Self::P5 => "NV_ENC_PRESET_P5_GUID",
            Self::P6 => "NV_ENC_PRESET_P6_GUID",
            Self::P7 => "NV_ENC_PRESET_P7_GUID",
        }
    }

    pub const fn human_name(self) -> &'static str {
        match self {
            Self::P1 => "最快",
            Self::P2 => "很快",
            Self::P3 => "偏速度",
            Self::P4 => "均衡（默认）",
            Self::P5 => "偏质量",
            Self::P6 => "高质量",
            Self::P7 => "最高质量（最慢）",
        }
    }

    pub const fn description(self) -> &'static str {
        match self {
            Self::P1 => "最高编码速度、最低压缩效率，适合编码性能优先。",
            Self::P2 => "编码速度优先，比 P1 提高一些压缩效率。",
            Self::P3 => "偏向编码速度，在性能与压缩效率之间轻度取舍。",
            Self::P4 => "速度与压缩效率均衡，也是本项目保持兼容的默认值。",
            Self::P5 => "偏向压缩质量，编码开销高于 P4。",
            Self::P6 => "高压缩质量，编码开销较高。",
            Self::P7 => "最高压缩效率、编码速度最慢，适合质量优先。",
        }
    }

    pub fn selected_label(self) -> String {
        format!("P{}", self.number())
    }

    pub fn label(self) -> String {
        format!("P{}", self.number())
    }
}

#[derive(Debug, Default, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
#[repr(u8)]
pub enum NvencSplitEncodeMode {
    #[default]
    Auto = 0,
    AutoForced = 1,
    TwoForced = 2,
    ThreeForced = 3,
    FourForced = 4,
    Disabled = 15,
}

impl NvencSplitEncodeMode {
    pub const fn all() -> [Self; 6] {
        [
            Self::Auto,
            Self::AutoForced,
            Self::TwoForced,
            Self::ThreeForced,
            Self::FourForced,
            Self::Disabled,
        ]
    }

    pub const fn raw_value(self) -> u8 {
        self as u8
    }

    pub const fn raw_name(self) -> &'static str {
        match self {
            Self::Auto => "NV_ENC_SPLIT_AUTO_MODE",
            Self::AutoForced => "NV_ENC_SPLIT_AUTO_FORCED_MODE",
            Self::TwoForced => "NV_ENC_SPLIT_TWO_FORCED_MODE",
            Self::ThreeForced => "NV_ENC_SPLIT_THREE_FORCED_MODE",
            Self::FourForced => "NV_ENC_SPLIT_FOUR_FORCED_MODE",
            Self::Disabled => "NV_ENC_SPLIT_DISABLE_MODE",
        }
    }

    pub const fn human_name(self) -> &'static str {
        match self {
            Self::Auto => "自动（驱动决定）",
            Self::AutoForced => "强制分帧（驱动决定条带数）",
            Self::TwoForced => "强制 2 条带",
            Self::ThreeForced => "强制 3 条带",
            Self::FourForced => "强制 4 条带",
            Self::Disabled => "禁用分帧",
        }
    }

    pub fn selected_label(self) -> String {
        format!("{} [{}]", self.human_name(), self.raw_value())
    }

    pub fn label(self) -> String {
        format!(
            "{} | {} = {}",
            self.human_name(),
            self.raw_name(),
            self.raw_value()
        )
    }

    pub fn description(self, encoder_engines: u32) -> String {
        let engines = encoder_engines.max(1);
        match self {
            Self::Auto => format!(
                "由驱动根据预设、低延迟 tuning 和分辨率决定是否分帧。当前 GPU 报告 {engines} 个 NVENC engine。"
            ),
            Self::AutoForced => format!(
                "强制启用分帧，条带数由驱动选择；单 engine 时不会产生实际分帧。当前 GPU 报告 {engines} 个 NVENC engine。"
            ),
            Self::TwoForced | Self::ThreeForced | Self::FourForced => {
                let requested = u32::from(self.raw_value());
                let effective = requested.min(engines);
                format!(
                    "请求 {requested} 条带；硬件 engine 数不足时退化为实际 engine 数。当前 GPU 报告 {engines} 个，最多形成 {effective} 条带。"
                )
            }
            Self::Disabled => "同时关闭自动分帧和强制分帧。".to_owned(),
        }
    }
}

#[derive(Debug, Default, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
#[repr(u8)]
pub enum NvencMultiPass {
    #[default]
    Disabled = 0,
    QuarterResolution = 1,
    FullResolution = 2,
}

impl NvencMultiPass {
    pub const fn all() -> [Self; 3] {
        [
            Self::Disabled,
            Self::QuarterResolution,
            Self::FullResolution,
        ]
    }

    pub const fn raw_value(self) -> u8 {
        self as u8
    }

    pub const fn raw_name(self) -> &'static str {
        match self {
            Self::Disabled => "NV_ENC_MULTI_PASS_DISABLED",
            Self::QuarterResolution => "NV_ENC_TWO_PASS_QUARTER_RESOLUTION",
            Self::FullResolution => "NV_ENC_TWO_PASS_FULL_RESOLUTION",
        }
    }

    pub const fn human_name(self) -> &'static str {
        match self {
            Self::Disabled => "单遍（最快）",
            Self::QuarterResolution => "二遍：1/4 分辨率首遍",
            Self::FullResolution => "二遍：全分辨率首遍",
        }
    }

    pub const fn description(self) -> &'static str {
        match self {
            Self::Disabled => "只编码一遍，额外分析开销最低。",
            Self::QuarterResolution => {
                "先以 1/4 分辨率分析，再完整编码；开销较低，并保留较大的运动搜索范围。"
            }
            Self::FullResolution => {
                "先以完整分辨率分析，再完整编码；统计更精细，但编码与显存带宽开销更高。"
            }
        }
    }

    pub fn selected_label(self) -> String {
        format!("{} [{}]", self.human_name(), self.raw_value())
    }

    pub fn label(self) -> String {
        format!(
            "{} | {} = {}",
            self.human_name(),
            self.raw_name(),
            self.raw_value()
        )
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RateControlConfig {
    pub method: RateControlMethod,
    pub brc_param_multiplier: u16,
    pub target_kbps: u32,
    pub max_kbps: u32,
    pub buffer_size_kb: u32,
    pub initial_delay_kb: u32,
    pub qpi: u16,
    pub qpp: u16,
    pub qpb: u16,
    pub accuracy: u16,
    pub convergence: u16,
    pub look_ahead_depth: u16,
    pub icq_quality: u16,
    pub qvbr_quality: u16,
    pub win_brc_max_avg_kbps: u32,
    pub win_brc_size: u16,
    pub low_delay_brc: bool,
    pub max_frame_size: u32,
    pub mbbrc: bool,
    pub ext_brc: bool,
    /// NVENC preset GUID。GUI 仅展示当前 HEVC session 枚举到的 P1..P7。
    #[serde(default)]
    pub nvenc_preset: NvencPreset,
    /// NVENC `NV_ENC_INITIALIZE_PARAMS::splitEncodeMode` 原始枚举值。
    #[serde(default)]
    pub nvenc_split_encode_mode: NvencSplitEncodeMode,
    /// NVENC `NV_ENC_RC_PARAMS::multiPass` 原始枚举值。
    #[serde(default)]
    pub nvenc_multi_pass: NvencMultiPass,
    /// NVENC 空间 AQ（`NV_ENC_RC_PARAMS::enableAQ`）。仅 NVENC active backend 下显示/写入。
    #[serde(default)]
    pub nvenc_spatial_aq: bool,
    /// 兼容旧配置；不再作为允许用户修改的 NVENC 额外调参。
    #[serde(default)]
    pub nvenc_temporal_aq: bool,
    /// 兼容旧配置；Spatial AQ 使用驱动默认 strength，不再暴露显式强度。
    #[serde(default)]
    pub nvenc_aq_strength: u8,
    /// NVENC VBR target quality，0 表示自动；1..=51 越小质量越高。
    #[serde(default)]
    pub nvenc_vbr_target_quality: u8,
}

impl Default for RateControlConfig {
    fn default() -> Self {
        Self {
            method: RateControlMethod::Cbr,
            brc_param_multiplier: 1,
            target_kbps: 20_000,
            max_kbps: 30_000,
            buffer_size_kb: 0,
            initial_delay_kb: 0,
            qpi: 23,
            qpp: 25,
            qpb: 27,
            accuracy: 100,
            convergence: 100,
            look_ahead_depth: 0,
            icq_quality: 23,
            qvbr_quality: 23,
            win_brc_max_avg_kbps: 0,
            win_brc_size: 0,
            low_delay_brc: false,
            max_frame_size: 0,
            mbbrc: false,
            ext_brc: false,
            nvenc_preset: NvencPreset::default(),
            nvenc_split_encode_mode: NvencSplitEncodeMode::default(),
            nvenc_multi_pass: NvencMultiPass::default(),
            nvenc_spatial_aq: false,
            nvenc_temporal_aq: false,
            nvenc_aq_strength: 0,
            nvenc_vbr_target_quality: 0,
        }
    }
}

impl RateControlConfig {
    /// 生成接近 oneVPL `mfxInfoMFX`/扩展 buffer 的扁平字段，供后端写 FFI 时使用。
    pub fn to_vpl_fields(&self) -> VplRateControlFields {
        let mut out = VplRateControlFields {
            rate_control_method: self.method.vpl_value(),
            brc_param_multiplier: self.brc_param_multiplier.max(1),
            ..VplRateControlFields::default()
        };

        match self.method {
            RateControlMethod::Cbr => {
                out.target_kbps = self.target_kbps;
                out.buffer_size_in_kb = self.buffer_size_kb;
                out.initial_delay_in_kb = self.initial_delay_kb;
                out.win_brc_max_avg_kbps = self.win_brc_max_avg_kbps;
                out.win_brc_size = self.win_brc_size;
            }
            RateControlMethod::Vbr => {
                out.target_kbps = self.target_kbps;
                out.max_kbps = self.max_kbps;
                out.buffer_size_in_kb = self.buffer_size_kb;
                out.initial_delay_in_kb = self.initial_delay_kb;
                out.win_brc_max_avg_kbps = self.win_brc_max_avg_kbps;
                out.win_brc_size = self.win_brc_size;
                out.low_delay_brc = self.low_delay_brc;
                out.max_frame_size = self.max_frame_size;
            }
            RateControlMethod::Cqp => {
                out.qpi = self.qpi;
                out.qpp = self.qpp;
                out.qpb = self.qpb;
            }
            RateControlMethod::Avbr => {
                out.target_kbps = self.target_kbps;
                out.accuracy = self.accuracy;
                out.convergence = self.convergence;
            }
            RateControlMethod::La => {
                out.target_kbps = self.target_kbps;
                out.look_ahead_depth = self.look_ahead_depth;
                out.win_brc_max_avg_kbps = self.win_brc_max_avg_kbps;
                out.win_brc_size = self.win_brc_size;
                out.max_frame_size = self.max_frame_size;
            }
            RateControlMethod::Icq => {
                out.icq_quality = self.icq_quality.clamp(1, 51);
            }
            RateControlMethod::Vcm => {
                out.target_kbps = self.target_kbps;
                out.max_kbps = self.max_kbps;
                out.buffer_size_in_kb = self.buffer_size_kb;
                out.initial_delay_in_kb = self.initial_delay_kb;
                out.low_delay_brc = self.low_delay_brc;
                out.max_frame_size = self.max_frame_size;
            }
            RateControlMethod::LaIcq => {
                out.icq_quality = self.icq_quality.clamp(1, 51);
                out.look_ahead_depth = self.look_ahead_depth;
            }
            RateControlMethod::LaHrd => {
                out.target_kbps = self.target_kbps;
                out.max_kbps = self.max_kbps;
                out.buffer_size_in_kb = self.buffer_size_kb;
                out.initial_delay_in_kb = self.initial_delay_kb;
                out.look_ahead_depth = self.look_ahead_depth;
                out.win_brc_max_avg_kbps = self.win_brc_max_avg_kbps;
                out.win_brc_size = self.win_brc_size;
                out.max_frame_size = self.max_frame_size;
            }
            RateControlMethod::Qvbr => {
                out.target_kbps = self.target_kbps;
                out.max_kbps = self.max_kbps;
                out.buffer_size_in_kb = self.buffer_size_kb;
                out.initial_delay_in_kb = self.initial_delay_kb;
                out.qvbr_quality = self.qvbr_quality.clamp(1, 51);
                out.win_brc_max_avg_kbps = self.win_brc_max_avg_kbps;
                out.win_brc_size = self.win_brc_size;
                out.low_delay_brc = self.low_delay_brc;
                out.max_frame_size = self.max_frame_size;
            }
        }

        out.mbbrc = self.mbbrc;
        out.ext_brc = self.ext_brc;
        out
    }

    /// 生成 NVENC `NV_ENC_RC_PARAMS` 的等价扁平字段。
    ///
    /// NVENC SDK 13.x 的公开码控模式只有 CBR/VBR/CONSTQP；oneVPL 的 AVBR/LA/ICQ/VCM/
    /// LA_ICQ/LA_HRD/QVBR 不在这里伪装映射，前端会按 active backend 隐藏这些模式。
    pub fn to_nvenc_fields(&self) -> Result<NvencRateControlFields, String> {
        let mut out = NvencRateControlFields {
            rate_control_mode: match self.method {
                RateControlMethod::Cqp => 0,
                RateControlMethod::Vbr => 1,
                RateControlMethod::Cbr => 2,
                other => {
                    return Err(format!(
                        "NVENC SDK 当前只支持 CBR/VBR/CQP，不能映射 {}",
                        other.short_name()
                    ));
                }
            },
            rate_control_mode_name: match self.method {
                RateControlMethod::Cqp => "NV_ENC_PARAMS_RC_CONSTQP",
                RateControlMethod::Vbr => "NV_ENC_PARAMS_RC_VBR",
                RateControlMethod::Cbr => "NV_ENC_PARAMS_RC_CBR",
                _ => unreachable!(),
            }
            .to_owned(),
            zero_reorder_delay: true,
            ..NvencRateControlFields::default()
        };

        match self.method {
            RateControlMethod::Cbr => {
                out.average_bit_rate = checked_kbps_to_bits("TargetKbps", self.target_kbps)?;
                out.vbv_buffer_size = checked_kib_to_bits("BufferSizeInKB", self.buffer_size_kb)?;
                out.vbv_initial_delay =
                    checked_kib_to_bits("InitialDelayInKB", self.initial_delay_kb)?;
                apply_nvenc_lookahead(self.look_ahead_depth, &mut out)?;
            }
            RateControlMethod::Vbr => {
                out.average_bit_rate = checked_kbps_to_bits("TargetKbps", self.target_kbps)?;
                out.max_bit_rate = checked_kbps_to_bits_allow_zero("MaxKbps", self.max_kbps)?;
                out.vbv_buffer_size = checked_kib_to_bits("BufferSizeInKB", self.buffer_size_kb)?;
                out.vbv_initial_delay =
                    checked_kib_to_bits("InitialDelayInKB", self.initial_delay_kb)?;
                apply_nvenc_lookahead(self.look_ahead_depth, &mut out)?;
                out.target_quality = self.nvenc_vbr_target_quality.min(51);
            }
            RateControlMethod::Cqp => {
                out.const_qp_i = u32::from(self.qpi.min(51));
                out.const_qp_p = u32::from(self.qpp.min(51));
                out.const_qp_b = u32::from(self.qpb.min(51));
            }
            _ => unreachable!(),
        }
        out.enable_spatial_aq = self.nvenc_spatial_aq;
        out.enable_temporal_aq = false;
        out.aq_strength = 0;
        out.multi_pass = u32::from(self.nvenc_multi_pass.raw_value());

        Ok(out)
    }

    pub fn to_nvenc_tuning_fields(&self) -> NvencTuningFields {
        NvencTuningFields {
            preset: self.nvenc_preset.raw_name().to_owned(),
            preset_number: self.nvenc_preset.number(),
            split_encode_mode: u32::from(self.nvenc_split_encode_mode.raw_value()),
            split_encode_mode_name: self.nvenc_split_encode_mode.raw_name().to_owned(),
            multi_pass: u32::from(self.nvenc_multi_pass.raw_value()),
            multi_pass_name: self.nvenc_multi_pass.raw_name().to_owned(),
            spatial_aq: self.nvenc_spatial_aq,
        }
    }
}

#[derive(Debug, Default, Clone, Serialize, Deserialize)]
pub struct VplRateControlFields {
    pub rate_control_method: u16,
    pub brc_param_multiplier: u16,
    pub initial_delay_in_kb: u32,
    pub buffer_size_in_kb: u32,
    pub target_kbps: u32,
    pub max_kbps: u32,
    pub qpi: u16,
    pub qpp: u16,
    pub qpb: u16,
    pub accuracy: u16,
    pub convergence: u16,
    pub look_ahead_depth: u16,
    pub icq_quality: u16,
    pub qvbr_quality: u16,
    pub win_brc_max_avg_kbps: u32,
    pub win_brc_size: u16,
    pub low_delay_brc: bool,
    pub max_frame_size: u32,
    pub mbbrc: bool,
    pub ext_brc: bool,
}

#[derive(Debug, Default, Clone, Serialize)]
pub struct NvencRateControlFields {
    pub rate_control_mode: u32,
    pub rate_control_mode_name: String,
    pub average_bit_rate: u32,
    pub max_bit_rate: u32,
    pub vbv_buffer_size: u32,
    pub vbv_initial_delay: u32,
    pub const_qp_i: u32,
    pub const_qp_p: u32,
    pub const_qp_b: u32,
    pub enable_lookahead: bool,
    pub lookahead_depth: u16,
    pub enable_spatial_aq: bool,
    pub enable_temporal_aq: bool,
    pub aq_strength: u8,
    pub target_quality: u8,
    pub zero_reorder_delay: bool,
    pub multi_pass: u32,
}

#[derive(Debug, Clone, Serialize)]
pub struct NvencTuningFields {
    pub preset: String,
    pub preset_number: u8,
    pub split_encode_mode: u32,
    pub split_encode_mode_name: String,
    pub multi_pass: u32,
    pub multi_pass_name: String,
    pub spatial_aq: bool,
}

fn checked_kbps_to_bits(name: &str, kbps: u32) -> Result<u32, String> {
    if kbps == 0 {
        return Err(format!("{name} 对 NVENC 必须大于 0"));
    }
    checked_kbps_to_bits_allow_zero(name, kbps)
}

fn checked_kbps_to_bits_allow_zero(name: &str, kbps: u32) -> Result<u32, String> {
    kbps.checked_mul(1000)
        .ok_or_else(|| format!("{name}={kbps} Kbps 超出 NVENC u32 bit/s 字段范围"))
}

fn checked_kib_to_bits(name: &str, kib: u32) -> Result<u32, String> {
    kib.checked_mul(1024)
        .and_then(|bytes| bytes.checked_mul(8))
        .ok_or_else(|| format!("{name}={kib} KiB 超出 NVENC u32 bit 字段范围"))
}

fn apply_nvenc_lookahead(
    look_ahead_depth: u16,
    out: &mut NvencRateControlFields,
) -> Result<(), String> {
    if look_ahead_depth == 0 {
        return Ok(());
    }
    // NVENC SDK 对 HEVC/IP-only 路径的 lookaheadDepth 范围是 0..=31（无 B 帧时）。
    if look_ahead_depth > 31 {
        return Err(format!(
            "NVENC LookAheadDepth={} 超出 0..=31；前端应按 NVENC active backend 限制该字段",
            look_ahead_depth
        ));
    }
    out.enable_lookahead = true;
    out.lookahead_depth = look_ahead_depth;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn removed_la_ext_is_not_mapped() {
        assert_eq!(RateControlMethod::from_vpl_value(12), None);
    }

    #[test]
    fn qvbr_maps_all_expected_fields() {
        let cfg = RateControlConfig {
            method: RateControlMethod::Qvbr,
            qvbr_quality: 99,
            ..Default::default()
        };
        let fields = cfg.to_vpl_fields();
        assert_eq!(fields.rate_control_method, 14);
        assert_eq!(fields.qvbr_quality, 51);
        assert!(fields.target_kbps > 0);
    }

    #[test]
    fn nvenc_cbr_maps_bitrate_vbv_lookahead_and_spatial_aq() {
        let cfg = RateControlConfig {
            method: RateControlMethod::Cbr,
            target_kbps: 40_000,
            buffer_size_kb: 2_000,
            initial_delay_kb: 1_000,
            look_ahead_depth: 16,
            nvenc_spatial_aq: true,
            nvenc_aq_strength: 9,
            nvenc_multi_pass: NvencMultiPass::FullResolution,
            ..Default::default()
        };
        let fields = cfg.to_nvenc_fields().unwrap();
        assert_eq!(fields.rate_control_mode, 2);
        assert_eq!(fields.average_bit_rate, 40_000_000);
        assert_eq!(fields.vbv_buffer_size, 2_000 * 1024 * 8);
        assert_eq!(fields.vbv_initial_delay, 1_000 * 1024 * 8);
        assert!(fields.enable_lookahead);
        assert_eq!(fields.lookahead_depth, 16);
        assert!(fields.enable_spatial_aq);
        assert!(!fields.enable_temporal_aq);
        assert_eq!(fields.aq_strength, 0);
        assert_eq!(fields.multi_pass, 2);
    }

    #[test]
    fn nvenc_cqp_maps_qp_order() {
        let cfg = RateControlConfig {
            method: RateControlMethod::Cqp,
            qpi: 21,
            qpp: 23,
            qpb: 25,
            ..Default::default()
        };
        let fields = cfg.to_nvenc_fields().unwrap();
        assert_eq!(fields.rate_control_mode, 0);
        assert_eq!(fields.const_qp_i, 21);
        assert_eq!(fields.const_qp_p, 23);
        assert_eq!(fields.const_qp_b, 25);
    }

    #[test]
    fn nvenc_rejects_onevpl_only_methods_and_overwide_lookahead() {
        let unsupported = RateControlConfig {
            method: RateControlMethod::Qvbr,
            ..Default::default()
        };
        assert!(unsupported.to_nvenc_fields().is_err());

        let too_deep = RateControlConfig {
            method: RateControlMethod::Vbr,
            look_ahead_depth: 40,
            ..Default::default()
        };
        assert!(too_deep.to_nvenc_fields().is_err());
    }

    #[test]
    fn nvenc_vbr_maps_target_quality_and_ignores_legacy_aq_controls() {
        let cfg = RateControlConfig {
            method: RateControlMethod::Vbr,
            target_kbps: 24_000,
            max_kbps: 36_000,
            buffer_size_kb: 3_000,
            initial_delay_kb: 1_500,
            look_ahead_depth: 8,
            nvenc_temporal_aq: true,
            nvenc_aq_strength: 15,
            nvenc_vbr_target_quality: 27,
            ..Default::default()
        };
        let fields = cfg.to_nvenc_fields().unwrap();
        assert_eq!(fields.rate_control_mode, 1);
        assert_eq!(fields.average_bit_rate, 24_000_000);
        assert_eq!(fields.max_bit_rate, 36_000_000);
        assert_eq!(fields.vbv_buffer_size, 3_000 * 1024 * 8);
        assert_eq!(fields.vbv_initial_delay, 1_500 * 1024 * 8);
        assert!(fields.enable_lookahead);
        assert_eq!(fields.lookahead_depth, 8);
        assert!(!fields.enable_temporal_aq);
        assert_eq!(fields.aq_strength, 0);
        assert_eq!(fields.target_quality, 27);
    }

    #[test]
    fn nvenc_tuning_defaults_preserve_existing_encoder_behavior() {
        let cfg = RateControlConfig::default();
        assert_eq!(cfg.nvenc_preset, NvencPreset::P4);
        assert_eq!(cfg.nvenc_split_encode_mode, NvencSplitEncodeMode::Auto);
        assert_eq!(cfg.nvenc_multi_pass, NvencMultiPass::Disabled);
        assert!(!cfg.nvenc_spatial_aq);

        let fields = cfg.to_nvenc_tuning_fields();
        assert_eq!(fields.preset, "NV_ENC_PRESET_P4_GUID");
        assert_eq!(fields.preset_number, 4);
        assert_eq!(fields.split_encode_mode, 0);
        assert_eq!(fields.multi_pass, 0);
        assert!(!fields.spatial_aq);
    }

    #[test]
    fn nvenc_tuning_raw_values_match_sdk_enums() {
        assert_eq!(
            NvencPreset::all().map(NvencPreset::number),
            [1, 2, 3, 4, 5, 6, 7]
        );
        assert_eq!(NvencSplitEncodeMode::Auto.raw_value(), 0);
        assert_eq!(NvencSplitEncodeMode::AutoForced.raw_value(), 1);
        assert_eq!(NvencSplitEncodeMode::TwoForced.raw_value(), 2);
        assert_eq!(NvencSplitEncodeMode::ThreeForced.raw_value(), 3);
        assert_eq!(NvencSplitEncodeMode::FourForced.raw_value(), 4);
        assert_eq!(NvencSplitEncodeMode::Disabled.raw_value(), 15);
        assert_eq!(
            NvencSplitEncodeMode::all().map(NvencSplitEncodeMode::raw_value),
            [0, 1, 2, 3, 4, 15]
        );
        assert_eq!(
            NvencMultiPass::all().map(NvencMultiPass::raw_value),
            [0, 1, 2]
        );
        assert!(NvencSplitEncodeMode::Disabled.label().contains("禁用分帧"));
        assert!(NvencSplitEncodeMode::Disabled.label().contains("= 15"));
        assert!(
            NvencSplitEncodeMode::FourForced
                .description(3)
                .contains("最多形成 3 条带")
        );
        assert!(NvencMultiPass::FullResolution.label().contains("全分辨率"));
        assert!(NvencMultiPass::FullResolution.label().contains("= 2"));
        assert_eq!(NvencPreset::P1.human_name(), "最快");
        assert_eq!(NvencPreset::P7.human_name(), "最高质量（最慢）");
        assert_eq!(NvencPreset::P1.label(), "P1");
        assert_eq!(NvencPreset::P7.selected_label(), "P7");
    }
}
