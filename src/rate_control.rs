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
            look_ahead_depth: 40,
            icq_quality: 23,
            qvbr_quality: 23,
            win_brc_max_avg_kbps: 0,
            win_brc_size: 0,
            low_delay_brc: false,
            max_frame_size: 0,
            mbbrc: false,
            ext_brc: false,
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
}
