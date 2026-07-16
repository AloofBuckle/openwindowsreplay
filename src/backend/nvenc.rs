#![allow(non_snake_case, dead_code, unsafe_op_in_unsafe_fn)]
//! NVIDIA NVENC 动态 FFI 能力探测。
//!
//! 本模块只做运行时探测：动态加载驱动提供的 `nvEncodeAPI64.dll`，在每个
//! NVIDIA DXGI adapter 上创建同 adapter 的 D3D11 device，然后用
//! `NvEncOpenEncodeSessionEx` 打开 NVENC 会话并枚举 HEVC/profile/input format/caps。
//! 编译不依赖 NVIDIA import lib；生产录制路径后续仍必须保持 DDA/WGC texture、
//! GPU 转换与 NVENC D3D11 registered resource 在同一个 DXGI adapter 上。

use crate::backend::mp4_mux::{HevcAccessUnit, NclxColorMetadata};
use crate::config::ChromaSampling;
use crate::error::BackendError;
use crate::rate_control::{
    NvencPreset, NvencSplitEncodeMode, RateControlConfig, RateControlMethod,
};
use libloading::Library;
use serde::{Deserialize, Serialize};
use std::collections::{BTreeSet, HashMap, VecDeque};
use std::ffi::c_void;
use std::path::PathBuf;
use std::ptr;

#[cfg(windows)]
#[path = "nvenc/cuda.rs"]
mod cuda;

const NVIDIA_VENDOR_ID: u32 = 0x10DE;
const NV_ENC_SUCCESS: i32 = 0;
const NV_ENC_ERR_NEED_MORE_INPUT: i32 = 17;

// Video Codec SDK 13.1 header encoding. The official driver entry point also
// reports its max supported API version; this compiled version is intentionally
// kept explicit so probe logs can show API/header skew.
const NVENCAPI_MAJOR_VERSION: u32 = 13;
const NVENCAPI_MINOR_VERSION: u32 = 1;
const NVENCAPI_VERSION: u32 = NVENCAPI_MAJOR_VERSION | (NVENCAPI_MINOR_VERSION << 24);
const NVENCAPI_DRIVER_VERSION: u32 = (NVENCAPI_MAJOR_VERSION << 4) | NVENCAPI_MINOR_VERSION;
const fn nvencapi_struct_version(version: u32) -> u32 {
    NVENCAPI_VERSION | (version << 16) | (0x7 << 28)
}
const NV_ENCODE_API_FUNCTION_LIST_VER: u32 = nvencapi_struct_version(2);
const NV_ENC_OPEN_ENCODE_SESSION_EX_PARAMS_VER: u32 = nvencapi_struct_version(1);
const NV_ENC_CAPS_PARAM_VER: u32 = nvencapi_struct_version(1);
const NV_ENC_INITIALIZE_PARAMS_VER: u32 = nvencapi_struct_version(7) | (1u32 << 31);
const NV_ENC_CONFIG_VER: u32 = nvencapi_struct_version(9) | (1u32 << 31);
const NV_ENC_PRESET_CONFIG_VER: u32 = nvencapi_struct_version(5) | (1u32 << 31);
const NV_ENC_CREATE_BITSTREAM_BUFFER_VER: u32 = nvencapi_struct_version(1);
const NV_ENC_REGISTER_RESOURCE_VER: u32 = nvencapi_struct_version(5);
const NV_ENC_MAP_INPUT_RESOURCE_VER: u32 = nvencapi_struct_version(4);
const NV_ENC_PIC_PARAMS_VER: u32 = nvencapi_struct_version(7) | (1u32 << 31);
const NV_ENC_LOCK_BITSTREAM_VER: u32 = nvencapi_struct_version(2) | (1u32 << 31);

const NV_ENC_DEVICE_TYPE_DIRECTX: u32 = 0;
const NV_ENC_DEVICE_TYPE_CUDA: u32 = 1;
const NV_ENC_INPUT_RESOURCE_TYPE_DIRECTX: u32 = 0;
const NV_ENC_INPUT_RESOURCE_TYPE_CUDAARRAY: u32 = 2;
const NV_ENC_INPUT_IMAGE: u32 = 0;
const NV_ENC_TUNING_INFO_LOW_LATENCY: u32 = 2;
const NV_ENC_PARAMS_FRAME_FIELD_MODE_FRAME: u32 = 1;
const NV_ENC_BIT_DEPTH_8: u32 = 8;
const NV_ENC_BIT_DEPTH_10: u32 = 10;
const NV_ENC_PIC_FLAG_FORCEIDR: u32 = 0x2;
const NV_ENC_PIC_FLAG_OUTPUT_SPSPPS: u32 = 0x4;
const NV_ENC_PIC_FLAG_EOS: u32 = 0x8;
const NV_ENC_PIC_STRUCT_FRAME: u32 = 0x01;

// SDK 13.x nvEncodeAPI.h layout, verified locally against nv-codec-headers:
// sizeof(NV_ENC_CONFIG)=3584, encodeCodecConfig offset=168,
// NV_ENC_CONFIG_HEVC outputBitDepth/inputBitDepth offsets=200/204.
const NV_ENC_CONFIG_OPAQUE_BYTES: usize = 3584;
const NV_ENC_CONFIG_PROFILE_GUID_OFFSET: usize = 4;
const NV_ENC_CONFIG_GOP_LENGTH_OFFSET: usize = 20;
const NV_ENC_CONFIG_FRAME_INTERVAL_P_OFFSET: usize = 24;
const NV_ENC_CONFIG_FRAME_FIELD_MODE_OFFSET: usize = 32;
const NV_ENC_CONFIG_MV_PRECISION_OFFSET: usize = 36;
const NV_ENC_CONFIG_RC_PARAMS_OFFSET: usize = 40;
const NV_ENC_CONFIG_CODEC_CONFIG_OFFSET: usize = 168;
const NV_ENC_CONFIG_HEVC_FLAGS_OFFSET: usize = NV_ENC_CONFIG_CODEC_CONFIG_OFFSET + 16;
const NV_ENC_CONFIG_HEVC_IDR_PERIOD_OFFSET: usize = NV_ENC_CONFIG_CODEC_CONFIG_OFFSET + 20;
const NV_ENC_CONFIG_HEVC_VUI_OFFSET: usize = NV_ENC_CONFIG_CODEC_CONFIG_OFFSET + 64;
const NV_ENC_CONFIG_HEVC_VUI_VIDEO_SIGNAL_PRESENT_OFFSET: usize = NV_ENC_CONFIG_HEVC_VUI_OFFSET + 8;
const NV_ENC_CONFIG_HEVC_VUI_VIDEO_FORMAT_OFFSET: usize = NV_ENC_CONFIG_HEVC_VUI_OFFSET + 12;
const NV_ENC_CONFIG_HEVC_VUI_FULL_RANGE_OFFSET: usize = NV_ENC_CONFIG_HEVC_VUI_OFFSET + 16;
const NV_ENC_CONFIG_HEVC_VUI_COLOUR_DESCRIPTION_PRESENT_OFFSET: usize =
    NV_ENC_CONFIG_HEVC_VUI_OFFSET + 20;
const NV_ENC_CONFIG_HEVC_VUI_COLOUR_PRIMARIES_OFFSET: usize = NV_ENC_CONFIG_HEVC_VUI_OFFSET + 24;
const NV_ENC_CONFIG_HEVC_VUI_TRANSFER_CHARACTERISTICS_OFFSET: usize =
    NV_ENC_CONFIG_HEVC_VUI_OFFSET + 28;
const NV_ENC_CONFIG_HEVC_VUI_MATRIX_COEFFICIENTS_OFFSET: usize = NV_ENC_CONFIG_HEVC_VUI_OFFSET + 32;
const NV_ENC_CONFIG_HEVC_OUTPUT_BIT_DEPTH_OFFSET: usize = NV_ENC_CONFIG_CODEC_CONFIG_OFFSET + 200;
const NV_ENC_CONFIG_HEVC_INPUT_BIT_DEPTH_OFFSET: usize = NV_ENC_CONFIG_CODEC_CONFIG_OFFSET + 204;
const NV_ENC_VUI_VIDEO_FORMAT_UNSPECIFIED: u32 = 5;

const NV_ENC_RC_PARAMS_VER: u32 = nvencapi_struct_version(1);
const NV_ENC_RC_PARAMS_RATE_CONTROL_MODE_OFFSET: usize = 4;
const NV_ENC_RC_PARAMS_CONST_QP_OFFSET: usize = 8;
const NV_ENC_RC_PARAMS_AVERAGE_BIT_RATE_OFFSET: usize = 20;
const NV_ENC_RC_PARAMS_MAX_BIT_RATE_OFFSET: usize = 24;
const NV_ENC_RC_PARAMS_VBV_BUFFER_SIZE_OFFSET: usize = 28;
const NV_ENC_RC_PARAMS_VBV_INITIAL_DELAY_OFFSET: usize = 32;
const NV_ENC_RC_PARAMS_BITFIELDS_OFFSET: usize = 36;
const NV_ENC_RC_PARAMS_TARGET_QUALITY_OFFSET: usize = 88;
const NV_ENC_RC_PARAMS_TARGET_QUALITY_LSB_OFFSET: usize = 89;
const NV_ENC_RC_PARAMS_LOOKAHEAD_DEPTH_OFFSET: usize = 90;
const NV_ENC_RC_PARAMS_LOW_DELAY_KEY_FRAME_SCALE_OFFSET: usize = 92;
const NV_ENC_RC_PARAMS_MULTI_PASS_OFFSET: usize = 100;
const NV_ENC_RC_PARAMS_BIT_ENABLE_AQ: u32 = 1 << 3;
const NV_ENC_RC_PARAMS_BIT_ENABLE_LOOKAHEAD: u32 = 1 << 5;
const NV_ENC_RC_PARAMS_BIT_ENABLE_TEMPORAL_AQ: u32 = 1 << 8;
const NV_ENC_RC_PARAMS_BIT_ZERO_REORDER_DELAY: u32 = 1 << 9;
const NV_ENC_RC_PARAMS_AQ_STRENGTH_SHIFT: u32 = 12;
const NV_ENC_INITIALIZE_SPLIT_MODE_SHIFT: u32 = 5;
const NV_ENC_INITIALIZE_SPLIT_MODE_MASK: u32 = 0xF << NV_ENC_INITIALIZE_SPLIT_MODE_SHIFT;

const NV_ENC_BUFFER_FORMAT_NV12: u32 = 0x0000_0001;
const NV_ENC_BUFFER_FORMAT_YUV444: u32 = 0x0000_1000;
const NV_ENC_BUFFER_FORMAT_YUV420_10BIT: u32 = 0x0001_0000;
const NV_ENC_BUFFER_FORMAT_YUV444_10BIT: u32 = 0x0010_0000;
const NV_ENC_BUFFER_FORMAT_ARGB: u32 = 0x0100_0000;
const NV_ENC_BUFFER_FORMAT_ARGB10: u32 = 0x0200_0000;
const NV_ENC_BUFFER_FORMAT_AYUV: u32 = 0x0400_0000;
const NV_ENC_BUFFER_FORMAT_ABGR: u32 = 0x1000_0000;
const NV_ENC_BUFFER_FORMAT_ABGR10: u32 = 0x2000_0000;
const NV_ENC_BUFFER_FORMAT_NV16: u32 = 0x4000_0001;
const NV_ENC_BUFFER_FORMAT_P210: u32 = 0x4000_0002;

const NV_ENC_CAPS_SUPPORTED_RATECONTROL_MODES: u32 = 1;
const NV_ENC_CAPS_WIDTH_MAX: u32 = 16;
const NV_ENC_CAPS_HEIGHT_MAX: u32 = 17;
const NV_ENC_CAPS_ASYNC_ENCODE_SUPPORT: u32 = 30;
const NV_ENC_CAPS_SUPPORT_YUV444_ENCODE: u32 = 33;
const NV_ENC_CAPS_SUPPORT_LOOKAHEAD: u32 = 37;
const NV_ENC_CAPS_SUPPORT_TEMPORAL_AQ: u32 = 38;
const NV_ENC_CAPS_SUPPORT_10BIT_ENCODE: u32 = 39;
const NV_ENC_CAPS_NUM_ENCODER_ENGINES: u32 = 49;
const NV_ENC_CAPS_SUPPORT_YUV422_ENCODE: u32 = 59;

const NV_ENC_PARAMS_RC_CONSTQP: u32 = 0;
const NV_ENC_PARAMS_RC_VBR: u32 = 1;
const NV_ENC_PARAMS_RC_CBR: u32 = 2;

const NV_ENC_CODEC_HEVC_GUID: windows::core::GUID = windows::core::GUID::from_values(
    0x790cdc88,
    0x4522,
    0x4d7b,
    [0x94, 0x25, 0xbd, 0xa9, 0x97, 0x5f, 0x76, 0x03],
);
const NV_ENC_HEVC_PROFILE_MAIN_GUID: windows::core::GUID = windows::core::GUID::from_values(
    0xb514c39a,
    0xb55b,
    0x40fa,
    [0x87, 0x8f, 0xf1, 0x25, 0x3b, 0x4d, 0xfd, 0xec],
);
const NV_ENC_HEVC_PROFILE_MAIN10_GUID: windows::core::GUID = windows::core::GUID::from_values(
    0xfa4d2b6c,
    0x3a5b,
    0x411a,
    [0x80, 0x18, 0x0a, 0x3f, 0x5e, 0x3c, 0x9b, 0xe5],
);
const NV_ENC_HEVC_PROFILE_FREXT_GUID: windows::core::GUID = windows::core::GUID::from_values(
    0x51ec32b5,
    0x1b4c,
    0x453c,
    [0x9c, 0xbd, 0xb6, 0x16, 0xbd, 0x62, 0x13, 0x41],
);
const NV_ENC_PRESET_P1_GUID: windows::core::GUID = windows::core::GUID::from_values(
    0xfc0a8d3e,
    0x45f8,
    0x4cf8,
    [0x80, 0xc7, 0x29, 0x88, 0x71, 0x59, 0x0e, 0xbf],
);
const NV_ENC_PRESET_P2_GUID: windows::core::GUID = windows::core::GUID::from_values(
    0xf581cfb8,
    0x88d6,
    0x4381,
    [0x93, 0xf0, 0xdf, 0x13, 0xf9, 0xc2, 0x7d, 0xab],
);
const NV_ENC_PRESET_P3_GUID: windows::core::GUID = windows::core::GUID::from_values(
    0x36850110,
    0x3a07,
    0x441f,
    [0x94, 0xd5, 0x36, 0x70, 0x63, 0x1f, 0x91, 0xf6],
);
const NV_ENC_PRESET_P4_GUID: windows::core::GUID = windows::core::GUID::from_values(
    0x90a7b826,
    0xdf06,
    0x4862,
    [0xb9, 0xd2, 0xcd, 0x6d, 0x73, 0xa0, 0x86, 0x81],
);
const NV_ENC_PRESET_P5_GUID: windows::core::GUID = windows::core::GUID::from_values(
    0x21c6e6b4,
    0x297a,
    0x4cba,
    [0x99, 0x8f, 0xb6, 0xcb, 0xde, 0x72, 0xad, 0xe3],
);
const NV_ENC_PRESET_P6_GUID: windows::core::GUID = windows::core::GUID::from_values(
    0x8e75c279,
    0x6299,
    0x4ab6,
    [0x83, 0x02, 0x0b, 0x21, 0x5a, 0x33, 0x5c, 0xf5],
);
const NV_ENC_PRESET_P7_GUID: windows::core::GUID = windows::core::GUID::from_values(
    0x84848c12,
    0x6f71,
    0x4c13,
    [0x93, 0x1b, 0x53, 0xe2, 0x83, 0xf5, 0x79, 0x74],
);

fn nvenc_preset_guid(preset: NvencPreset) -> windows::core::GUID {
    match preset {
        NvencPreset::P1 => NV_ENC_PRESET_P1_GUID,
        NvencPreset::P2 => NV_ENC_PRESET_P2_GUID,
        NvencPreset::P3 => NV_ENC_PRESET_P3_GUID,
        NvencPreset::P4 => NV_ENC_PRESET_P4_GUID,
        NvencPreset::P5 => NV_ENC_PRESET_P5_GUID,
        NvencPreset::P6 => NV_ENC_PRESET_P6_GUID,
        NvencPreset::P7 => NV_ENC_PRESET_P7_GUID,
    }
}

fn nvenc_preset_from_guid(guid: windows::core::GUID) -> Option<NvencPreset> {
    NvencPreset::all()
        .into_iter()
        .find(|preset| nvenc_preset_guid(*preset) == guid)
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct NvencProbeInfo {
    pub available: bool,
    pub dll_path: Option<String>,
    pub load_error: Option<String>,
    pub compiled_api_version: String,
    pub max_supported_version: Option<String>,
    pub adapters: Vec<NvencAdapterInfo>,
    pub hevc_supported: bool,
    pub hevc_profiles: Vec<String>,
    pub hevc_presets: Vec<NvencPreset>,
    pub input_formats: Vec<String>,
    pub chroma_candidates: Vec<ChromaSampling>,
    pub route_candidates: Vec<NvencRouteProbe>,
    /// 按当前 DXGI output 色彩/位深推导出的 NVENC route。与 oneVPL 一样，这只是
    /// “当前显示器状态 + 编码器能力”的交集；生产链路未接线前不会提升为可录制。
    pub current_display_routes: Vec<NvencCurrentDisplayRouteInfo>,
    pub rate_controls: Vec<RateControlMethod>,
    pub d3d11_texture_input_seen: bool,
    pub warnings: Vec<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct NvencAdapterInfo {
    pub adapter_index: u32,
    pub adapter_name: String,
    pub adapter_luid: String,
    pub vendor_id: u32,
    pub device_id: u32,
    pub d3d11_session_opened: bool,
    pub hevc_supported: bool,
    pub hevc_profiles: Vec<String>,
    pub hevc_presets: Vec<NvencPreset>,
    pub input_formats: Vec<String>,
    pub caps: NvencCapsInfo,
    pub route_candidates: Vec<NvencRouteProbe>,
    pub current_display_routes: Vec<NvencCurrentDisplayRouteInfo>,
    pub rate_controls: Vec<RateControlMethod>,
    pub d3d11_texture_input_seen: bool,
    pub warnings: Vec<String>,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct NvencCapsInfo {
    pub max_width: Option<i32>,
    pub max_height: Option<i32>,
    pub async_encode: Option<bool>,
    pub yuv422: Option<bool>,
    pub yuv444: Option<bool>,
    pub ten_bit: Option<bool>,
    pub lookahead: Option<bool>,
    pub temporal_aq: Option<bool>,
    pub encoder_engines: Option<u32>,
    pub rate_control_mask: Option<i32>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct NvencRouteProbe {
    pub input_format: String,
    pub chroma: ChromaSampling,
    pub bit_depth: u16,
    pub profile: String,
    pub query_supported: bool,
    /// NVENC SDK/驱动能力与当前已实现 GPU writer 均成立时为 true，GUI 只暴露
    /// 这些已经可进入生产录制后端的路线。
    pub production_record_supported: bool,
    pub production_blocker: Option<String>,
    pub rate_controls: Vec<RateControlMethod>,
    pub rate_control_features: Vec<NvencRateControlFeatureProbe>,
    pub note: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct NvencRateControlFeatureProbe {
    pub method: RateControlMethod,
    /// NVENC `enableLookahead`。与 oneVPL LA/LA_ICQ 不是一一等价；这里只表达
    /// 当前 driver/caps 可支持“前瞻分析”字段。
    pub lookahead: bool,
    /// NVENC VBV buffer size/initial delay 可用于 CBR/VBR。
    pub vbv: bool,
    /// NVENC spatial AQ (`enableAQ`)。SDK 没有独立 caps；当前只在 NVENC HEVC 基础模式可见。
    pub spatial_aq: bool,
    /// NVENC temporal AQ (`enableTemporalAQ`)，由 `NV_ENC_CAPS_SUPPORT_TEMPORAL_AQ` 驱动。
    pub temporal_aq: bool,
    /// NVENC VBR targetQuality 字段；只对 VBR 暴露。
    pub target_quality: bool,
    /// 兼容旧日志字段：当前 method 是否有任何 AQ/quality tuning 字段。
    pub aq: bool,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct NvencCurrentDisplayRouteInfo {
    pub adapter_index: u32,
    #[serde(default)]
    pub adapter_luid: String,
    pub output_index: u32,
    pub rotation: u32,
    pub color_space: u32,
    pub bits_per_color: u32,
    pub desktop_left: i32,
    pub desktop_top: i32,
    pub desktop_right: i32,
    pub desktop_bottom: i32,
    pub chroma: ChromaSampling,
    pub input_format: String,
    pub bit_depth: u16,
    pub profile: String,
    pub nclx_colour_primaries: u16,
    pub nclx_transfer_characteristics: u16,
    pub nclx_matrix_coefficients: u16,
    pub nclx_full_range: bool,
    pub route_summary: String,
    pub note: String,
}

pub fn lookahead_depth_max_for_current_display_route(route: &NvencCurrentDisplayRouteInfo) -> u16 {
    const MAX_SLOTS: u128 = 32;
    const MIN_SLOTS: u128 = 8;
    const SLOT_BUDGET_BYTES: u128 = 3 * 1024 * 1024 * 1024;

    if route.input_format.is_empty() {
        return 0;
    }
    let width = u128::from(route.desktop_right.abs_diff(route.desktop_left).max(1));
    let height = u128::from(route.desktop_bottom.abs_diff(route.desktop_top).max(1));
    let pixels = width.saturating_mul(height);
    let texture_bytes = match route.input_format.as_str() {
        "NV12" => pixels.saturating_mul(3) / 2,
        "P010" => pixels.saturating_mul(3),
        "NV16" => pixels.saturating_mul(2),
        "P210" => pixels.saturating_mul(4),
        "AYUV" => pixels.saturating_mul(4),
        "YUV444" => pixels.saturating_mul(3),
        "YUV444_10BIT" => pixels.saturating_mul(6),
        _ => return 0,
    };
    // 原生 D3D11 DDA route 同时持有 capture/encoder 两个 resource view；CUDA
    // planar route 是同设备单 allocation。按较重的 DDA 路径给 GUI 保守上限。
    let allocation_count = if nvenc_route_requires_cuda(&route.input_format) {
        1
    } else {
        2
    };
    let per_slot = texture_bytes.saturating_mul(allocation_count);
    if per_slot == 0 {
        return 0;
    }
    let slots = (SLOT_BUDGET_BYTES / per_slot).clamp(MIN_SLOTS, MAX_SLOTS);
    slots.saturating_sub(1).min(31) as u16
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct NvencD3d11EncodeSmokeReport {
    pub adapter_index: u32,
    pub width: u32,
    pub height: u32,
    pub input_format: String,
    pub output_bytes: usize,
    pub annex_b_start_code_seen: bool,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum NvencD3d11InputFormat {
    Nv12,
    P010,
    Nv16,
    P210,
    Ayuv,
    Yuv444,
    Yuv44410,
}

impl NvencD3d11InputFormat {
    pub const fn label(self) -> &'static str {
        match self {
            Self::Nv12 => "NV12",
            Self::P010 => "P010",
            Self::Nv16 => "NV16",
            Self::P210 => "P210",
            Self::Ayuv => "AYUV",
            Self::Yuv444 => "YUV444",
            Self::Yuv44410 => "YUV444_10BIT",
        }
    }

    const fn buffer_format(self) -> u32 {
        match self {
            Self::Nv12 => NV_ENC_BUFFER_FORMAT_NV12,
            Self::P010 => NV_ENC_BUFFER_FORMAT_YUV420_10BIT,
            Self::Nv16 => NV_ENC_BUFFER_FORMAT_NV16,
            Self::P210 => NV_ENC_BUFFER_FORMAT_P210,
            Self::Ayuv => NV_ENC_BUFFER_FORMAT_AYUV,
            Self::Yuv444 => NV_ENC_BUFFER_FORMAT_YUV444,
            Self::Yuv44410 => NV_ENC_BUFFER_FORMAT_YUV444_10BIT,
        }
    }

    pub(crate) const fn dxgi_pitch_bytes(self, width: u32) -> u32 {
        match self {
            Self::Nv12 | Self::Nv16 | Self::Yuv444 => width,
            Self::P010 | Self::P210 | Self::Yuv44410 => width.saturating_mul(2),
            Self::Ayuv => width.saturating_mul(4),
        }
    }

    pub(crate) const fn texture_height(self, height: u32) -> u32 {
        match self {
            Self::Nv16 | Self::P210 => height.saturating_mul(2),
            Self::Yuv444 | Self::Yuv44410 => height.saturating_mul(3),
            Self::Nv12 | Self::P010 | Self::Ayuv => height,
        }
    }

    pub(crate) const fn frame_size_bytes(self, width: u32, height: u32) -> usize {
        let pitch = self.dxgi_pitch_bytes(width) as usize;
        match self {
            Self::Nv12 | Self::P010 => pitch * height as usize * 3 / 2,
            Self::Nv16 | Self::P210 => pitch * height as usize * 2,
            Self::Yuv444 | Self::Yuv44410 => pitch * height as usize * 3,
            Self::Ayuv => pitch * height as usize,
        }
    }

    #[cfg(windows)]
    pub(crate) const fn dxgi_format(self) -> windows::Win32::Graphics::Dxgi::Common::DXGI_FORMAT {
        use windows::Win32::Graphics::Dxgi::Common::{
            DXGI_FORMAT_AYUV, DXGI_FORMAT_NV12, DXGI_FORMAT_P010, DXGI_FORMAT_R8_UINT,
            DXGI_FORMAT_R16_UINT,
        };
        match self {
            Self::Nv12 => DXGI_FORMAT_NV12,
            Self::P010 => DXGI_FORMAT_P010,
            Self::Nv16 | Self::Yuv444 => DXGI_FORMAT_R8_UINT,
            Self::P210 | Self::Yuv44410 => DXGI_FORMAT_R16_UINT,
            Self::Ayuv => DXGI_FORMAT_AYUV,
        }
    }

    pub const fn requires_cuda_interop(self) -> bool {
        matches!(
            self,
            Self::Nv16 | Self::P210 | Self::Yuv444 | Self::Yuv44410
        )
    }
}

#[cfg(windows)]
pub struct NvencD3d11Encoder {
    registered_inputs: HashMap<usize, NvencRegisteredResource>,
    persistent_registration: bool,
    free_bitstreams: Vec<NvencBitstreamBuffer>,
    pending_frames: VecDeque<NvencPendingFrame>,
    session: NvencSession,
    _api: NvencApi,
    width: u32,
    height: u32,
    input_format: NvencD3d11InputFormat,
    expected_color: NclxColorMetadata,
    vui_verified: bool,
    frame_idx: u32,
    lookahead_depth: u16,
    eos_submitted: bool,
}

#[cfg(windows)]
struct NvencPendingFrame {
    mapped: NvencMappedInputResource,
    transient_registered: Option<NvencRegisteredResource>,
    bitstream: NvencBitstreamBuffer,
    timestamp_90k: u64,
    discard_from_track: bool,
}

#[cfg(windows)]
impl NvencD3d11Encoder {
    pub fn open(
        adapter_index: u32,
        width: u32,
        height: u32,
        input_format: NvencD3d11InputFormat,
        color: NclxColorMetadata,
    ) -> Result<Self, BackendError> {
        let default_rate_control = RateControlConfig {
            // oneVPL 默认值是 40；NVENC HEVC/IP-only lookahead 上限是 31。
            // 裸 encoder smoke 的默认 open 不开启 lookahead，生产路径由 GUI/caps
            // sanitization 后的配置显式传入。
            look_ahead_depth: 0,
            ..RateControlConfig::default()
        };
        Self::open_with_rate_control(
            adapter_index,
            width,
            height,
            input_format,
            color,
            &default_rate_control,
            60,
            1,
        )
    }

    #[allow(clippy::too_many_arguments)]
    pub fn open_with_rate_control(
        adapter_index: u32,
        width: u32,
        height: u32,
        input_format: NvencD3d11InputFormat,
        color: NclxColorMetadata,
        rate_control: &RateControlConfig,
        frame_rate_num: u32,
        frame_rate_den: u32,
    ) -> Result<Self, BackendError> {
        unsafe {
            let (api, _dll_path) = NvencApi::load()
                .map_err(|err| BackendError::unsupported("NVENC", "nvEncodeAPI64.dll", err))?;
            let session = open_d3d11_session_for_adapter(&api, adapter_index)?;
            initialize_low_latency_hevc_encoder(
                &api,
                session.encoder,
                width,
                height,
                input_format,
                color,
                rate_control,
                frame_rate_num,
                frame_rate_den,
            )?;
            let output_buffer_count = usize::from(rate_control.look_ahead_depth).saturating_add(1);
            let mut free_bitstreams = Vec::with_capacity(output_buffer_count);
            for _ in 0..output_buffer_count {
                free_bitstreams.push(create_bitstream_buffer(&api, session.encoder)?);
            }
            Ok(Self {
                registered_inputs: HashMap::new(),
                persistent_registration: std::env::var_os("RUST_REPLAY_NVENC_REGISTER_PER_FRAME")
                    .is_none(),
                free_bitstreams,
                pending_frames: VecDeque::with_capacity(output_buffer_count),
                session,
                _api: api,
                width,
                height,
                input_format,
                expected_color: color,
                vui_verified: false,
                frame_idx: 0,
                lookahead_depth: rate_control.look_ahead_depth,
                eos_submitted: false,
            })
        }
    }

    pub fn device(&self) -> &windows::Win32::Graphics::Direct3D11::ID3D11Device {
        self.session.device()
    }

    pub fn context(&self) -> &windows::Win32::Graphics::Direct3D11::ID3D11DeviceContext {
        self.session.context()
    }

    pub fn input_format(&self) -> NvencD3d11InputFormat {
        self.input_format
    }

    pub fn registration_mode(&self) -> &'static str {
        if self.persistent_registration {
            "persistent"
        } else {
            "per-frame"
        }
    }

    pub fn lookahead_depth(&self) -> u16 {
        self.lookahead_depth
    }

    pub fn pending_frame_count(&self) -> usize {
        self.pending_frames.len()
    }

    pub fn submit_texture(
        &mut self,
        texture: &windows::Win32::Graphics::Direct3D11::ID3D11Texture2D,
        timestamp_90k: u64,
        force_idr: bool,
        discard_from_track: bool,
    ) -> Result<Vec<HevcAccessUnit>, BackendError> {
        use windows::core::Interface;

        if self.eos_submitted {
            return Err(BackendError::unsupported(
                "NVENC encode",
                self.input_format.label(),
                "EOS 已提交，不能继续提交输入帧",
            ));
        }

        unsafe {
            validate_d3d11_input_texture(texture, self.width, self.height, self.input_format)?;
            let bitstream = self.free_bitstreams.pop().ok_or_else(|| {
                BackendError::unsupported(
                    "NVENC delayed output",
                    format!(
                        "LookAheadDepth={} pending={}",
                        self.lookahead_depth,
                        self.pending_frames.len()
                    ),
                    "延迟输出超过已分配 bitstream pool；驱动没有按配置深度返回输出",
                )
            })?;

            let texture_key = texture.as_raw() as usize;
            if self.persistent_registration && !self.registered_inputs.contains_key(&texture_key) {
                let registered = match register_d3d11_input_texture(
                    &self._api,
                    self.session.encoder,
                    texture,
                    self.width,
                    self.height,
                    self.input_format,
                ) {
                    Ok(registered) => registered,
                    Err(err) => {
                        self.free_bitstreams.push(bitstream);
                        return Err(err);
                    }
                };
                self.registered_inputs.insert(texture_key, registered);
            }
            let mut transient_registered = if self.persistent_registration {
                None
            } else {
                let registered = match register_d3d11_input_texture(
                    &self._api,
                    self.session.encoder,
                    texture,
                    self.width,
                    self.height,
                    self.input_format,
                ) {
                    Ok(registered) => registered,
                    Err(err) => {
                        self.free_bitstreams.push(bitstream);
                        return Err(err);
                    }
                };
                Some(registered)
            };
            let registered = if let Some(registered) = transient_registered.as_ref() {
                registered
            } else {
                self.registered_inputs.get(&texture_key).ok_or_else(|| {
                    BackendError::unsupported(
                        "NVENC encode",
                        "persistent D3D11 registered-resource pool",
                        "输入纹理注册后未保留 resource",
                    )
                })?
            };
            let mut mapped = match map_input_resource(&self._api, self.session.encoder, registered)
            {
                Ok(mapped) => mapped,
                Err(err) => {
                    self.free_bitstreams.push(bitstream);
                    return Err(err);
                }
            };
            let status = match encode_one_d3d11_frame(
                &self._api,
                self.session.encoder,
                &mapped,
                &bitstream,
                self.width,
                self.height,
                self.input_format,
                self.frame_idx,
                timestamp_90k,
                force_idr || self.frame_idx == 0,
            ) {
                Ok(status) => status,
                Err(err) => {
                    let _ = mapped.unmap_now();
                    if let Some(registered) = transient_registered.as_mut() {
                        let _ = registered.unregister_now();
                    }
                    self.free_bitstreams.push(bitstream);
                    return Err(err);
                }
            };
            self.pending_frames.push_back(NvencPendingFrame {
                mapped,
                transient_registered,
                bitstream,
                timestamp_90k,
                discard_from_track,
            });
            self.frame_idx = self.frame_idx.wrapping_add(1);

            match status {
                NvencEncodePictureStatus::OutputAvailable => {
                    Ok(vec![self.drain_one_pending_output()?])
                }
                NvencEncodePictureStatus::NeedMoreInput => Ok(Vec::new()),
            }
        }
    }

    pub fn encode_texture(
        &mut self,
        texture: &windows::Win32::Graphics::Direct3D11::ID3D11Texture2D,
        timestamp_90k: u64,
        force_idr: bool,
        discard_from_track: bool,
    ) -> Result<HevcAccessUnit, BackendError> {
        let mut outputs =
            self.submit_texture(texture, timestamp_90k, force_idr, discard_from_track)?;
        if outputs.len() != 1 {
            return Err(BackendError::unsupported(
                "NVENC synchronous encode",
                format!("LookAheadDepth={}", self.lookahead_depth),
                "该调用要求每次提交立即返回一个 AU；启用 Lookahead 时请使用 submit_texture/flush 延迟输出接口",
            ));
        }
        Ok(outputs.remove(0))
    }

    pub fn flush(&mut self) -> Result<Vec<HevcAccessUnit>, BackendError> {
        if self.eos_submitted {
            return Ok(Vec::new());
        }
        unsafe {
            submit_encoder_eos(&self._api, self.session.encoder)?;
            self.eos_submitted = true;
            let mut outputs = Vec::with_capacity(self.pending_frames.len());
            while !self.pending_frames.is_empty() {
                outputs.push(self.drain_one_pending_output()?);
            }
            Ok(outputs)
        }
    }

    unsafe fn drain_one_pending_output(&mut self) -> Result<HevcAccessUnit, BackendError> {
        let pending = self.pending_frames.pop_front().ok_or_else(|| {
            BackendError::unsupported(
                "NVENC delayed output",
                self.input_format.label(),
                "驱动报告有输出，但 pending frame 队列为空",
            )
        })?;
        let NvencPendingFrame {
            mut mapped,
            mut transient_registered,
            bitstream,
            timestamp_90k,
            discard_from_track,
        } = pending;
        let bitstream_result =
            lock_and_copy_bitstream(&self._api, self.session.encoder, &bitstream);
        let unmap_status = mapped.unmap_now();
        let unregister_status = transient_registered
            .as_mut()
            .map(|registered| registered.unregister_now());
        self.free_bitstreams.push(bitstream);

        let output = bitstream_result?;
        nvenc_check("NvEncUnmapInputResource", unmap_status)?;
        if let Some(status) = unregister_status {
            nvenc_check("NvEncUnregisterResource(transient)", status)?;
        }
        if output.output_timestamp_90k != timestamp_90k {
            return Err(BackendError::unsupported(
                "NVENC delayed output timestamp",
                format!(
                    "submitted={} returned={}",
                    timestamp_90k, output.output_timestamp_90k
                ),
                "NVENC 没有按输入 VFR 时间戳返回对应输出，拒绝重写或合成时间戳",
            ));
        }
        if output.bytes.is_empty() {
            return Err(BackendError::unsupported(
                "NVENC encode",
                self.input_format.label(),
                "返回空 HEVC bitstream",
            ));
        }
        if !output.annex_b_start_code_seen {
            return Err(BackendError::unsupported(
                "NVENC encode",
                "HEVC Annex-B bitstream",
                "bitstream 中未发现 Annex-B start code，无法交给当前 MP4 muxer",
            ));
        }
        if !self.vui_verified {
            verify_hevc_vui_matches(&output.bytes, self.expected_color)?;
            self.vui_verified = true;
        }
        let is_sync = crate::backend::mp4_mux::hevc_annex_b_has_random_access_nal(&output.bytes);
        Ok(HevcAccessUnit {
            timestamp_90k,
            data: output.bytes.into(),
            is_sync,
            discard_from_track,
        })
    }

    pub fn shutdown(mut self) -> Result<(), BackendError> {
        if !self.pending_frames.is_empty() {
            return Err(BackendError::unsupported(
                "NVENC shutdown",
                format!("pending_frames={}", self.pending_frames.len()),
                "正常关闭前必须调用 flush 取回全部延迟输出；快速取消由 Drop 中止会话",
            ));
        }
        let mut failures = Vec::new();
        for (_, mut resource) in self.registered_inputs.drain() {
            let status = unsafe { resource.unregister_now() };
            if status != NV_ENC_SUCCESS {
                failures.push(format!("NvEncUnregisterResource status={status}"));
            }
        }
        for mut bitstream in self.free_bitstreams.drain(..) {
            let bitstream_status = unsafe { bitstream.destroy_now() };
            if bitstream_status != NV_ENC_SUCCESS {
                failures.push(format!(
                    "NvEncDestroyBitstreamBuffer status={bitstream_status}"
                ));
            }
        }
        let encoder_status = unsafe { self.session.destroy_now() };
        if encoder_status != NV_ENC_SUCCESS {
            failures.push(format!("NvEncDestroyEncoder status={encoder_status}"));
        }
        if failures.is_empty() {
            Ok(())
        } else {
            Err(BackendError::unsupported(
                "NVENC shutdown",
                self.input_format.label(),
                failures.join(" | "),
            ))
        }
    }
}

#[cfg(windows)]
impl Drop for NvencD3d11Encoder {
    fn drop(&mut self) {
        if self.session.encoder.is_null() {
            return;
        }
        unsafe {
            // Fast cancellation intentionally skips EOS/bitstream draining. Destroying
            // the encoder first makes all child handles invalid; disarm their Drop
            // implementations so they do not call back into a dead NVENC session.
            let _ = self.session.destroy_now();
            for mut pending in self.pending_frames.drain(..) {
                pending.mapped.abandon();
                if let Some(registered) = pending.transient_registered.as_mut() {
                    registered.abandon();
                }
                pending.bitstream.abandon();
            }
            for bitstream in &mut self.free_bitstreams {
                bitstream.abandon();
            }
            for resource in self.registered_inputs.values_mut() {
                resource.abandon();
            }
        }
    }
}

#[cfg(windows)]
pub enum NvencTextureEncoder {
    D3d11(NvencD3d11Encoder),
    Cuda(cuda::NvencCudaInteropEncoder),
}

#[cfg(windows)]
impl NvencTextureEncoder {
    #[allow(clippy::too_many_arguments)]
    pub fn open_with_rate_control(
        adapter_index: u32,
        width: u32,
        height: u32,
        input_format: NvencD3d11InputFormat,
        color: NclxColorMetadata,
        rate_control: &RateControlConfig,
        frame_rate_num: u32,
        frame_rate_den: u32,
    ) -> Result<Self, BackendError> {
        if input_format.requires_cuda_interop() {
            unsafe {
                cuda::NvencCudaInteropEncoder::open_with_rate_control(
                    adapter_index,
                    width,
                    height,
                    input_format,
                    color,
                    rate_control,
                    frame_rate_num,
                    frame_rate_den,
                )
                .map(Self::Cuda)
            }
        } else {
            NvencD3d11Encoder::open_with_rate_control(
                adapter_index,
                width,
                height,
                input_format,
                color,
                rate_control,
                frame_rate_num,
                frame_rate_den,
            )
            .map(Self::D3d11)
        }
    }

    pub fn device(&self) -> &windows::Win32::Graphics::Direct3D11::ID3D11Device {
        match self {
            Self::D3d11(encoder) => encoder.device(),
            Self::Cuda(encoder) => encoder.device(),
        }
    }

    pub fn context(&self) -> &windows::Win32::Graphics::Direct3D11::ID3D11DeviceContext {
        match self {
            Self::D3d11(encoder) => encoder.context(),
            Self::Cuda(encoder) => encoder.context(),
        }
    }

    pub fn input_format(&self) -> NvencD3d11InputFormat {
        match self {
            Self::D3d11(encoder) => encoder.input_format(),
            Self::Cuda(encoder) => encoder.input_format(),
        }
    }

    pub fn registration_mode(&self) -> &'static str {
        match self {
            Self::D3d11(encoder) => encoder.registration_mode(),
            Self::Cuda(encoder) => encoder.registration_mode(),
        }
    }

    pub fn uses_cuda_interop(&self) -> bool {
        matches!(self, Self::Cuda(_))
    }

    pub fn lookahead_depth(&self) -> u16 {
        match self {
            Self::D3d11(encoder) => encoder.lookahead_depth(),
            Self::Cuda(encoder) => encoder.lookahead_depth(),
        }
    }

    pub fn pending_frame_count(&self) -> usize {
        match self {
            Self::D3d11(encoder) => encoder.pending_frame_count(),
            Self::Cuda(encoder) => encoder.pending_frame_count(),
        }
    }

    pub fn submit_texture(
        &mut self,
        texture: &windows::Win32::Graphics::Direct3D11::ID3D11Texture2D,
        timestamp_90k: u64,
        force_idr: bool,
        discard_from_track: bool,
    ) -> Result<Vec<HevcAccessUnit>, BackendError> {
        match self {
            Self::D3d11(encoder) => {
                encoder.submit_texture(texture, timestamp_90k, force_idr, discard_from_track)
            }
            Self::Cuda(encoder) => {
                encoder.submit_texture(texture, timestamp_90k, force_idr, discard_from_track)
            }
        }
    }

    pub fn flush(&mut self) -> Result<Vec<HevcAccessUnit>, BackendError> {
        match self {
            Self::D3d11(encoder) => encoder.flush(),
            Self::Cuda(encoder) => encoder.flush(),
        }
    }

    pub fn shutdown(self) -> Result<(), BackendError> {
        match self {
            Self::D3d11(encoder) => encoder.shutdown(),
            Self::Cuda(encoder) => encoder.shutdown(),
        }
    }
}

#[cfg(not(windows))]
pub struct NvencD3d11Encoder;

#[cfg(not(windows))]
impl NvencD3d11Encoder {
    pub fn open(
        _adapter_index: u32,
        _width: u32,
        _height: u32,
        _input_format: NvencD3d11InputFormat,
        _color: NclxColorMetadata,
    ) -> Result<Self, BackendError> {
        Err(BackendError::unsupported(
            "NVENC D3D11 encoder",
            "Windows D3D11",
            "仅 Windows 支持",
        ))
    }
}

#[derive(Debug, Clone, Copy)]
struct NvencDisplayRouteColor {
    hdr_pq: bool,
    bit_depth: u16,
    mp4_color: NclxColorMetadata,
    note: &'static str,
}

impl NvencProbeInfo {
    fn unavailable(error: String) -> Self {
        Self {
            available: false,
            dll_path: None,
            load_error: Some(error.clone()),
            compiled_api_version: compiled_api_version_string(),
            max_supported_version: None,
            adapters: Vec::new(),
            hevc_supported: false,
            hevc_profiles: Vec::new(),
            hevc_presets: Vec::new(),
            input_formats: Vec::new(),
            chroma_candidates: Vec::new(),
            route_candidates: Vec::new(),
            current_display_routes: Vec::new(),
            rate_controls: Vec::new(),
            d3d11_texture_input_seen: false,
            warnings: vec![error],
        }
    }
}

#[cfg(windows)]
fn probe_current_display_routes_for_adapter(
    adapter: &crate::backend::dxgi::DxgiAdapterInfo,
    route_candidates: &[NvencRouteProbe],
    caps: &NvencCapsInfo,
) -> Result<Vec<NvencCurrentDisplayRouteInfo>, BackendError> {
    use windows::Win32::Graphics::Dxgi::Common::{
        DXGI_MODE_ROTATION_IDENTITY, DXGI_MODE_ROTATION_UNSPECIFIED,
    };
    use windows::Win32::Graphics::Dxgi::{
        CreateDXGIFactory1, DXGI_ERROR_NOT_FOUND, IDXGIFactory1, IDXGIOutput6,
    };
    use windows::core::Interface;

    unsafe {
        let factory: IDXGIFactory1 =
            CreateDXGIFactory1().map_err(|err| BackendError::WindowsApi {
                func: "CreateDXGIFactory1(NVENC current display route)",
                message: err.to_string(),
            })?;
        let adapter1 =
            factory
                .EnumAdapters1(adapter.index)
                .map_err(|err| BackendError::WindowsApi {
                    func: "IDXGIFactory1::EnumAdapters1(NVENC current display route)",
                    message: err.to_string(),
                })?;
        let mut routes = Vec::new();
        let mut output_index = 0u32;
        loop {
            let output = match adapter1.EnumOutputs(output_index) {
                Ok(output) => output,
                Err(err) if err.code() == DXGI_ERROR_NOT_FOUND => break,
                Err(err) => {
                    return Err(BackendError::WindowsApi {
                        func: "IDXGIAdapter1::EnumOutputs(NVENC current display route)",
                        message: err.to_string(),
                    });
                }
            };
            let desc = output.GetDesc().map_err(|err| BackendError::WindowsApi {
                func: "IDXGIOutput::GetDesc(NVENC current display route)",
                message: err.to_string(),
            })?;
            if !desc.AttachedToDesktop.as_bool() {
                output_index = output_index.saturating_add(1);
                continue;
            }
            let rect = desc.DesktopCoordinates;
            let rotation = desc.Rotation.0 as u32;
            if rotation != DXGI_MODE_ROTATION_UNSPECIFIED.0 as u32
                && rotation != DXGI_MODE_ROTATION_IDENTITY.0 as u32
            {
                for chroma in ChromaSampling::all() {
                    routes.push(NvencCurrentDisplayRouteInfo {
                        adapter_index: adapter.index,
                        adapter_luid: adapter.luid_string(),
                        output_index,
                        rotation,
                        color_space: 0,
                        bits_per_color: 0,
                        desktop_left: rect.left,
                        desktop_top: rect.top,
                        desktop_right: rect.right,
                        desktop_bottom: rect.bottom,
                        chroma,
                        input_format: String::new(),
                        bit_depth: 0,
                        profile: String::new(),
                        nclx_colour_primaries: 0,
                        nclx_transfer_characteristics: 0,
                        nclx_matrix_coefficients: 0,
                        nclx_full_range: false,
                        route_summary: String::new(),
                        note: format!(
                            "adapter={} output={} rotation={}：不支持的桌面模式",
                            adapter.index, output_index, rotation
                        ),
                    });
                }
                output_index = output_index.saturating_add(1);
                continue;
            }
            let width = (rect.right - rect.left).max(1) as u32;
            let height = (rect.bottom - rect.top).max(1) as u32;
            let resolution_supported = caps
                .max_width
                .is_none_or(|max| max > 0 && width <= max as u32)
                && caps
                    .max_height
                    .is_none_or(|max| max > 0 && height <= max as u32);
            if !resolution_supported {
                for chroma in ChromaSampling::all() {
                    routes.push(NvencCurrentDisplayRouteInfo {
                        adapter_index: adapter.index,
                        adapter_luid: adapter.luid_string(),
                        output_index,
                        rotation,
                        color_space: 0,
                        bits_per_color: 0,
                        desktop_left: rect.left,
                        desktop_top: rect.top,
                        desktop_right: rect.right,
                        desktop_bottom: rect.bottom,
                        chroma,
                        input_format: String::new(),
                        bit_depth: 0,
                        profile: String::new(),
                        nclx_colour_primaries: 0,
                        nclx_transfer_characteristics: 0,
                        nclx_matrix_coefficients: 0,
                        nclx_full_range: false,
                        route_summary: String::new(),
                        note: format!(
                            "当前输出 {}x{} 超过 NVENC caps max={}x{}",
                            width,
                            height,
                            caps.max_width.unwrap_or(-1),
                            caps.max_height.unwrap_or(-1)
                        ),
                    });
                }
                output_index = output_index.saturating_add(1);
                continue;
            }
            let display = output
                .cast::<IDXGIOutput6>()
                .map_err(|_| {
                    BackendError::unsupported(
                        "NVENC 当前显示器 route",
                        format!(
                            "adapter={} output={} IDXGIOutput6::GetDesc1",
                            adapter.index, output_index
                        ),
                        "不支持的桌面模式",
                    )
                })
                .and_then(|output6| {
                    let desc1 = output6.GetDesc1().map_err(|err| BackendError::WindowsApi {
                        func: "IDXGIOutput6::GetDesc1(NVENC current display route)",
                        message: err.to_string(),
                    })?;
                    let display =
                        nvenc_display_route_color(desc1.ColorSpace.0 as u32, desc1.BitsPerColor)?;
                    Ok((desc1, display))
                });
            match display {
                Ok((desc1, display)) => {
                    for chroma in ChromaSampling::all() {
                        let chroma_alignment_supported = match chroma {
                            ChromaSampling::Yuv420 => {
                                width.is_multiple_of(2) && height.is_multiple_of(2)
                            }
                            ChromaSampling::Yuv422 => width.is_multiple_of(2),
                            ChromaSampling::Yuv444 => true,
                        };
                        let matched = chroma_alignment_supported
                            .then(|| {
                                nvenc_display_route_choices(chroma, display)
                                    .into_iter()
                                    .find_map(|(input_format, bit_depth, profile)| {
                                        route_candidates
                                            .iter()
                                            .find(|route| {
                                                route.production_record_supported
                                                    && route.input_format == input_format
                                                    && route.chroma == chroma
                                                    && route.bit_depth == bit_depth
                                                    && route.profile == profile
                                            })
                                            .map(|route| {
                                                (
                                                    input_format,
                                                    bit_depth,
                                                    profile,
                                                    route.note.clone(),
                                                )
                                            })
                                    })
                            })
                            .flatten();
                        let (input_format, bit_depth, profile, route_summary, note) = if let Some(
                            (input_format, bit_depth, profile, route_note),
                        ) = matched
                        {
                            (
                                input_format.to_owned(),
                                bit_depth,
                                profile.to_owned(),
                                format!(
                                    "NVENC current display: {} -> {} {}-bit {}",
                                    display.note, input_format, bit_depth, profile
                                ),
                                route_note,
                            )
                        } else {
                            (
                                String::new(),
                                display.bit_depth,
                                String::new(),
                                String::new(),
                                if chroma_alignment_supported {
                                    format!(
                                        "当前显示器状态 {} 下没有匹配 NVENC {} route 或 profile/input format",
                                        display.note,
                                        chroma.doc_label()
                                    )
                                } else {
                                    format!(
                                        "当前输出 {}x{} 不满足所选色度采样的尺寸对齐要求",
                                        width, height
                                    )
                                },
                            )
                        };
                        routes.push(NvencCurrentDisplayRouteInfo {
                            adapter_index: adapter.index,
                            adapter_luid: adapter.luid_string(),
                            output_index,
                            rotation,
                            color_space: desc1.ColorSpace.0 as u32,
                            bits_per_color: desc1.BitsPerColor,
                            desktop_left: rect.left,
                            desktop_top: rect.top,
                            desktop_right: rect.right,
                            desktop_bottom: rect.bottom,
                            chroma,
                            input_format,
                            bit_depth,
                            profile,
                            nclx_colour_primaries: display.mp4_color.colour_primaries,
                            nclx_transfer_characteristics: display
                                .mp4_color
                                .transfer_characteristics,
                            nclx_matrix_coefficients: display.mp4_color.matrix_coefficients,
                            nclx_full_range: display.mp4_color.full_range,
                            route_summary,
                            note,
                        });
                    }
                }
                Err(err) => {
                    for chroma in ChromaSampling::all() {
                        routes.push(NvencCurrentDisplayRouteInfo {
                            adapter_index: adapter.index,
                            adapter_luid: adapter.luid_string(),
                            output_index,
                            rotation,
                            color_space: 0,
                            bits_per_color: 0,
                            desktop_left: rect.left,
                            desktop_top: rect.top,
                            desktop_right: rect.right,
                            desktop_bottom: rect.bottom,
                            chroma,
                            input_format: String::new(),
                            bit_depth: 0,
                            profile: String::new(),
                            nclx_colour_primaries: 0,
                            nclx_transfer_characteristics: 0,
                            nclx_matrix_coefficients: 0,
                            nclx_full_range: false,
                            route_summary: String::new(),
                            note: err.to_string(),
                        });
                    }
                }
            }
            output_index = output_index.saturating_add(1);
        }
        routes.sort_by_key(|route| {
            let primary = route.desktop_left <= 0
                && route.desktop_right > 0
                && route.desktop_top <= 0
                && route.desktop_bottom > 0;
            (!primary, route.output_index, route.chroma)
        });
        Ok(routes)
    }
}

#[cfg(windows)]
fn current_display_color_for_adapter(
    adapter_index: u32,
) -> Result<NclxColorMetadata, BackendError> {
    use windows::Win32::Graphics::Dxgi::{CreateDXGIFactory1, IDXGIFactory1, IDXGIOutput6};
    use windows::core::Interface;

    unsafe {
        let factory: IDXGIFactory1 =
            CreateDXGIFactory1().map_err(|err| BackendError::WindowsApi {
                func: "CreateDXGIFactory1(NVENC dynamic color)",
                message: err.to_string(),
            })?;
        let adapter =
            factory
                .EnumAdapters1(adapter_index)
                .map_err(|err| BackendError::WindowsApi {
                    func: "IDXGIFactory1::EnumAdapters1(NVENC dynamic color)",
                    message: err.to_string(),
                })?;
        let output = adapter
            .EnumOutputs(0)
            .map_err(|err| BackendError::WindowsApi {
                func: "IDXGIAdapter1::EnumOutputs(NVENC dynamic color)",
                message: err.to_string(),
            })?;
        let output6 = output.cast::<IDXGIOutput6>().map_err(|_| {
            BackendError::unsupported(
                "NVENC dynamic color",
                "IDXGIOutput6::GetDesc1",
                "当前输出不支持动态色彩状态查询",
            )
        })?;
        let desc = output6.GetDesc1().map_err(|err| BackendError::WindowsApi {
            func: "IDXGIOutput6::GetDesc1(NVENC dynamic color)",
            message: err.to_string(),
        })?;
        Ok(nvenc_display_route_color(desc.ColorSpace.0 as u32, desc.BitsPerColor)?.mp4_color)
    }
}

#[cfg(windows)]
fn nvenc_display_route_color(
    color_space: u32,
    bits_per_color: u32,
) -> Result<NvencDisplayRouteColor, BackendError> {
    use windows::Win32::Graphics::Dxgi::Common::{
        DXGI_COLOR_SPACE_RGB_FULL_G22_NONE_P709, DXGI_COLOR_SPACE_RGB_FULL_G22_NONE_P2020,
        DXGI_COLOR_SPACE_RGB_FULL_G2084_NONE_P2020, DXGI_COLOR_SPACE_RGB_STUDIO_G22_NONE_P709,
        DXGI_COLOR_SPACE_RGB_STUDIO_G22_NONE_P2020, DXGI_COLOR_SPACE_RGB_STUDIO_G24_NONE_P709,
        DXGI_COLOR_SPACE_RGB_STUDIO_G24_NONE_P2020, DXGI_COLOR_SPACE_RGB_STUDIO_G2084_NONE_P2020,
        DXGI_COLOR_SPACE_YCBCR_FULL_G22_LEFT_P601, DXGI_COLOR_SPACE_YCBCR_FULL_G22_LEFT_P709,
        DXGI_COLOR_SPACE_YCBCR_FULL_G22_LEFT_P2020, DXGI_COLOR_SPACE_YCBCR_FULL_G22_NONE_P709_X601,
        DXGI_COLOR_SPACE_YCBCR_FULL_GHLG_TOPLEFT_P2020,
        DXGI_COLOR_SPACE_YCBCR_STUDIO_G22_LEFT_P601, DXGI_COLOR_SPACE_YCBCR_STUDIO_G22_LEFT_P709,
        DXGI_COLOR_SPACE_YCBCR_STUDIO_G22_LEFT_P2020,
        DXGI_COLOR_SPACE_YCBCR_STUDIO_G22_TOPLEFT_P2020,
        DXGI_COLOR_SPACE_YCBCR_STUDIO_G24_LEFT_P709, DXGI_COLOR_SPACE_YCBCR_STUDIO_G24_LEFT_P2020,
        DXGI_COLOR_SPACE_YCBCR_STUDIO_G24_TOPLEFT_P2020,
        DXGI_COLOR_SPACE_YCBCR_STUDIO_G2084_LEFT_P2020,
        DXGI_COLOR_SPACE_YCBCR_STUDIO_G2084_TOPLEFT_P2020,
        DXGI_COLOR_SPACE_YCBCR_STUDIO_GHLG_TOPLEFT_P2020,
    };

    let detected_bits = if bits_per_color == 0 {
        8
    } else {
        bits_per_color as u16
    };
    if detected_bits > 10 {
        return Err(BackendError::unsupported(
            "NVENC 当前显示器 route",
            format!("DXGI ColorSpace={color_space} BitsPerColor={bits_per_color}"),
            "不支持的桌面模式",
        ));
    }
    if color_space == DXGI_COLOR_SPACE_RGB_FULL_G2084_NONE_P2020.0 as u32 {
        Ok(NvencDisplayRouteColor {
            hdr_pq: true,
            bit_depth: 10,
            mp4_color: NclxColorMetadata::bt2020_pq_full(),
            note: "DXGI RGB_FULL_G2084_P2020 -> BT.2020/PQ/full",
        })
    } else if color_space == DXGI_COLOR_SPACE_RGB_STUDIO_G2084_NONE_P2020.0 as u32
        || color_space == DXGI_COLOR_SPACE_YCBCR_STUDIO_G2084_LEFT_P2020.0 as u32
        || color_space == DXGI_COLOR_SPACE_YCBCR_STUDIO_G2084_TOPLEFT_P2020.0 as u32
    {
        Ok(NvencDisplayRouteColor {
            hdr_pq: true,
            bit_depth: 10,
            mp4_color: NclxColorMetadata::bt2020_pq_limited(),
            note: "DXGI *_STUDIO_G2084_P2020 -> BT.2020/PQ/limited",
        })
    } else if color_space == DXGI_COLOR_SPACE_RGB_FULL_G22_NONE_P709.0 as u32
        || color_space == DXGI_COLOR_SPACE_YCBCR_FULL_G22_LEFT_P709.0 as u32
        || color_space == DXGI_COLOR_SPACE_YCBCR_FULL_G22_NONE_P709_X601.0 as u32
    {
        Ok(NvencDisplayRouteColor {
            hdr_pq: false,
            bit_depth: if detected_bits >= 10 { 10 } else { 8 },
            mp4_color: NclxColorMetadata::bt709_full(),
            note: "DXGI *_FULL_G22_P709 -> BT.709/full",
        })
    } else if color_space == DXGI_COLOR_SPACE_RGB_STUDIO_G22_NONE_P709.0 as u32
        || color_space == DXGI_COLOR_SPACE_RGB_STUDIO_G24_NONE_P709.0 as u32
        || color_space == DXGI_COLOR_SPACE_YCBCR_STUDIO_G22_LEFT_P709.0 as u32
        || color_space == DXGI_COLOR_SPACE_YCBCR_STUDIO_G24_LEFT_P709.0 as u32
    {
        Ok(NvencDisplayRouteColor {
            hdr_pq: false,
            bit_depth: if detected_bits >= 10 { 10 } else { 8 },
            mp4_color: NclxColorMetadata::bt709_limited(),
            note: "DXGI *_STUDIO_G22/G24_P709 -> BT.709/limited",
        })
    } else if color_space == DXGI_COLOR_SPACE_RGB_FULL_G22_NONE_P2020.0 as u32
        || color_space == DXGI_COLOR_SPACE_RGB_STUDIO_G22_NONE_P2020.0 as u32
        || color_space == DXGI_COLOR_SPACE_RGB_STUDIO_G24_NONE_P2020.0 as u32
        || color_space == DXGI_COLOR_SPACE_YCBCR_FULL_G22_LEFT_P2020.0 as u32
        || color_space == DXGI_COLOR_SPACE_YCBCR_STUDIO_G22_LEFT_P2020.0 as u32
        || color_space == DXGI_COLOR_SPACE_YCBCR_STUDIO_G22_TOPLEFT_P2020.0 as u32
        || color_space == DXGI_COLOR_SPACE_YCBCR_STUDIO_G24_LEFT_P2020.0 as u32
        || color_space == DXGI_COLOR_SPACE_YCBCR_STUDIO_G24_TOPLEFT_P2020.0 as u32
    {
        let full_range = color_space == DXGI_COLOR_SPACE_RGB_FULL_G22_NONE_P2020.0 as u32
            || color_space == DXGI_COLOR_SPACE_YCBCR_FULL_G22_LEFT_P2020.0 as u32;
        Ok(NvencDisplayRouteColor {
            hdr_pq: false,
            bit_depth: if detected_bits >= 10 { 10 } else { 8 },
            mp4_color: if detected_bits >= 10 {
                NclxColorMetadata::bt2020_sdr_10(full_range)
            } else {
                NclxColorMetadata::bt2020_sdr_8(full_range)
            },
            note: if full_range {
                "DXGI *_FULL_G22_P2020 -> BT.2020 SDR/full"
            } else {
                "DXGI *_STUDIO_G22/G24_P2020 -> BT.2020 SDR/limited"
            },
        })
    } else if color_space == DXGI_COLOR_SPACE_YCBCR_FULL_GHLG_TOPLEFT_P2020.0 as u32
        || color_space == DXGI_COLOR_SPACE_YCBCR_STUDIO_GHLG_TOPLEFT_P2020.0 as u32
        || color_space == DXGI_COLOR_SPACE_YCBCR_FULL_G22_LEFT_P601.0 as u32
        || color_space == DXGI_COLOR_SPACE_YCBCR_STUDIO_G22_LEFT_P601.0 as u32
    {
        Err(BackendError::unsupported(
            "NVENC 当前显示器 route",
            format!("DXGI ColorSpace={color_space}"),
            "不支持的桌面模式",
        ))
    } else {
        Err(BackendError::unsupported(
            "NVENC 当前显示器 route",
            format!("未知/自定义 DXGI ColorSpace={color_space}"),
            "不支持的桌面模式",
        ))
    }
}

fn nvenc_display_route_choices(
    chroma: ChromaSampling,
    display: NvencDisplayRouteColor,
) -> Vec<(&'static str, u16, &'static str)> {
    let ten_bit = display.hdr_pq || display.bit_depth >= 10;
    match (chroma, ten_bit) {
        (ChromaSampling::Yuv420, false) => vec![("NV12", 8, "Main")],
        (ChromaSampling::Yuv420, true) => vec![("P010", 10, "Main10")],
        (ChromaSampling::Yuv422, false) => vec![("NV16", 8, "FRExt")],
        (ChromaSampling::Yuv422, true) => vec![("P210", 10, "FRExt")],
        // SDR 4:4:4 优先 packed AYUV；驱动不暴露 AYUV 时使用已实现的 planar
        // YUV444 CUDA external-memory writer。
        (ChromaSampling::Yuv444, false) => vec![("AYUV", 8, "FRExt"), ("YUV444", 8, "FRExt")],
        (ChromaSampling::Yuv444, true) => vec![("YUV444_10BIT", 10, "FRExt")],
    }
}

pub fn probe_nvenc_adapters(
    dxgi_adapters: &[crate::backend::dxgi::DxgiAdapterInfo],
) -> NvencProbeInfo {
    probe_nvenc_adapters_impl(dxgi_adapters)
}

#[cfg(windows)]
fn probe_nvenc_adapters_impl(
    dxgi_adapters: &[crate::backend::dxgi::DxgiAdapterInfo],
) -> NvencProbeInfo {
    let (api, dll_path) = match NvencApi::load() {
        Ok(pair) => pair,
        Err(error) => return NvencProbeInfo::unavailable(error),
    };

    let mut warnings = Vec::new();
    let mut adapters = Vec::new();
    for adapter in dxgi_adapters
        .iter()
        .filter(|adapter| adapter.vendor_id == NVIDIA_VENDOR_ID && adapter.flags & 0x2 == 0)
    {
        match unsafe { probe_adapter(&api, adapter) } {
            Ok(info) => adapters.push(info),
            Err(err) => warnings.push(format!(
                "NVENC adapter{} {} 探测失败：{err}",
                adapter.index, adapter.description
            )),
        }
    }

    if adapters.is_empty() {
        warnings.push("未枚举到可打开 NVENC D3D11 会话的 NVIDIA 显示 adapter".to_owned());
    }

    let mut hevc_profiles = BTreeSet::new();
    let mut hevc_presets = BTreeSet::new();
    let mut input_formats = BTreeSet::new();
    let mut chroma_candidates = BTreeSet::new();
    let mut rate_controls = BTreeSet::new();
    let mut route_candidates = Vec::new();
    let mut current_display_routes = Vec::new();
    let mut route_keys = BTreeSet::new();
    let mut hevc_supported = false;
    let mut d3d11_texture_input_seen = false;

    for adapter in &adapters {
        hevc_supported |= adapter.hevc_supported;
        d3d11_texture_input_seen |= adapter.d3d11_texture_input_seen;
        for profile in &adapter.hevc_profiles {
            hevc_profiles.insert(profile.clone());
        }
        hevc_presets.extend(adapter.hevc_presets.iter().copied());
        for format in &adapter.input_formats {
            input_formats.insert(format.clone());
        }
        for route in &adapter.route_candidates {
            chroma_candidates.insert(route.chroma);
            let key = format!(
                "{}::{:?}::{}::{}",
                route.input_format, route.chroma, route.bit_depth, route.profile
            );
            if route_keys.insert(key) {
                route_candidates.push(route.clone());
            }
        }
        current_display_routes.extend(adapter.current_display_routes.iter().cloned());
        for method in &adapter.rate_controls {
            rate_controls.insert(*method);
        }
    }

    if hevc_supported && route_candidates.is_empty() {
        warnings.push("NVENC 已支持 HEVC，但未确认任何 D3D11 输入格式 route".to_owned());
    }

    NvencProbeInfo {
        available: true,
        dll_path: Some(dll_path.display().to_string()),
        load_error: None,
        compiled_api_version: compiled_api_version_string(),
        max_supported_version: api.max_supported_version.map(nvenc_driver_version_string),
        adapters,
        hevc_supported,
        hevc_profiles: hevc_profiles.into_iter().collect(),
        hevc_presets: hevc_presets.into_iter().collect(),
        input_formats: input_formats.into_iter().collect(),
        chroma_candidates: chroma_candidates.into_iter().collect(),
        route_candidates,
        current_display_routes,
        rate_controls: rate_controls.into_iter().collect(),
        d3d11_texture_input_seen,
        warnings,
    }
}

#[cfg(not(windows))]
fn probe_nvenc_adapters_impl(
    _dxgi_adapters: &[crate::backend::dxgi::DxgiAdapterInfo],
) -> NvencProbeInfo {
    NvencProbeInfo::unavailable("NVENC D3D11 探测仅在 Windows 上可用".to_owned())
}

/// 本机手动冒烟：初始化 NVENC HEVC，同 adapter 创建 D3D11 NV12 texture，走
/// RegisterResource -> MapInputResource -> EncodePicture -> LockBitstream。
///
/// 这不是生产录制路径；只用合成 NV12 测试图证明 D3D11 registered-resource
/// 编码链路可用。生产路径仍必须接入 DDA/WGC + GPU converter 的真实纹理。
pub fn local_d3d11_encode_smoke(
    adapter_index: u32,
) -> Result<NvencD3d11EncodeSmokeReport, BackendError> {
    local_d3d11_encode_smoke_impl(adapter_index)
}

#[cfg(not(windows))]
fn local_d3d11_encode_smoke_impl(
    _adapter_index: u32,
) -> Result<NvencD3d11EncodeSmokeReport, BackendError> {
    Err(BackendError::unsupported(
        "NVENC D3D11 encode smoke",
        "NvEncodeAPI + D3D11 texture",
        "仅 Windows 支持",
    ))
}

#[cfg(windows)]
fn local_d3d11_encode_smoke_impl(
    adapter_index: u32,
) -> Result<NvencD3d11EncodeSmokeReport, BackendError> {
    unsafe {
        let width = 1280u32;
        let height = 720u32;
        let input_format = NvencD3d11InputFormat::Nv12;
        let color = current_display_color_for_adapter(adapter_index)?;
        let mut encoder =
            NvencD3d11Encoder::open(adapter_index, width, height, input_format, color)?;
        let texture =
            create_synthetic_input_texture(encoder.device(), width, height, input_format)?;
        let sample = encoder.encode_texture(&texture, 0, true, false)?;
        let annex_b_start_code_seen = sample
            .data
            .windows(4)
            .any(|window| window == [0x00, 0x00, 0x00, 0x01]);
        Ok(NvencD3d11EncodeSmokeReport {
            adapter_index,
            width,
            height,
            input_format: input_format.label().to_owned(),
            output_bytes: sample.data.len(),
            annex_b_start_code_seen,
        })
    }
}

#[cfg(windows)]
unsafe fn probe_adapter(
    api: &NvencApi,
    adapter: &crate::backend::dxgi::DxgiAdapterInfo,
) -> Result<NvencAdapterInfo, BackendError> {
    let mut warnings = Vec::new();
    let session = open_d3d11_session_for_adapter(api, adapter.index)?;
    let encoder = session.encoder;

    let encode_guids = query_guid_list(
        "NvEncGetEncodeGUIDCount",
        "NvEncGetEncodeGUIDs",
        encoder,
        api.functions.nvEncGetEncodeGUIDCount,
        |array, array_size, written| {
            let func = api
                .functions
                .nvEncGetEncodeGUIDs
                .ok_or_else(|| nvenc_missing("NvEncGetEncodeGUIDs"))?;
            Ok(func(encoder, array, array_size, written))
        },
    )?;
    let hevc_supported = encode_guids.contains(&NV_ENC_CODEC_HEVC_GUID);

    let mut profiles = Vec::new();
    let mut presets = Vec::new();
    let mut input_formats = Vec::new();
    let mut caps = NvencCapsInfo::default();
    let mut rate_controls = Vec::new();
    let mut routes = Vec::new();
    let mut current_display_routes = Vec::new();

    if hevc_supported {
        profiles = query_hevc_profiles(api, encoder)?;
        presets = query_hevc_presets(api, encoder, &mut warnings)?;
        input_formats = query_input_formats(api, encoder)?;
        caps = query_hevc_caps(api, encoder, &mut warnings);
        rate_controls = caps
            .rate_control_mask
            .map(rate_controls_from_mask)
            .unwrap_or_default();
        routes = build_route_candidates(&input_formats, &profiles, &caps, &rate_controls);
        if let Some(input_format) =
            routes
                .iter()
                .find_map(|route| match route.input_format.as_str() {
                    "NV16" => Some(NvencD3d11InputFormat::Nv16),
                    "P210" => Some(NvencD3d11InputFormat::P210),
                    "YUV444" => Some(NvencD3d11InputFormat::Yuv444),
                    "YUV444_10BIT" => Some(NvencD3d11InputFormat::Yuv44410),
                    _ => None,
                })
        {
            let mut adapter_luid = [0u8; 8];
            adapter_luid[..4].copy_from_slice(&adapter.luid_low.to_ne_bytes());
            adapter_luid[4..].copy_from_slice(&adapter.luid_high.to_ne_bytes());
            if let Err(err) =
                cuda::probe_external_texture_interop(adapter_luid, session.device(), input_format)
            {
                let reason = format!("CUDA external-memory/keyed-mutex 生产前提探测失败：{err}");
                warnings.push(reason.clone());
                for route in &mut routes {
                    if nvenc_route_requires_cuda(&route.input_format) {
                        route.production_record_supported = false;
                        route.production_blocker = Some(reason.clone());
                    }
                }
            }
        }
        match probe_current_display_routes_for_adapter(adapter, &routes, &caps) {
            Ok(display_routes) => current_display_routes = display_routes,
            Err(err) => warnings.push(format!("当前显示器 NVENC route 探测失败：{err}")),
        }
    }

    Ok(NvencAdapterInfo {
        adapter_index: adapter.index,
        adapter_name: adapter.description.clone(),
        adapter_luid: adapter.luid_string(),
        vendor_id: adapter.vendor_id,
        device_id: adapter.device_id,
        d3d11_session_opened: true,
        hevc_supported,
        hevc_profiles: profiles,
        hevc_presets: presets,
        input_formats,
        caps,
        route_candidates: routes,
        current_display_routes,
        rate_controls,
        d3d11_texture_input_seen: hevc_supported,
        warnings,
    })
}

fn nvenc_route_requires_cuda(input_format: &str) -> bool {
    matches!(input_format, "NV16" | "P210" | "YUV444" | "YUV444_10BIT")
}

#[cfg(windows)]
unsafe fn query_hevc_profiles(
    api: &NvencApi,
    encoder: *mut c_void,
) -> Result<Vec<String>, BackendError> {
    let count_func = api
        .functions
        .nvEncGetEncodeProfileGUIDCount
        .ok_or_else(|| nvenc_missing("NvEncGetEncodeProfileGUIDCount"))?;
    let list_func = api
        .functions
        .nvEncGetEncodeProfileGUIDs
        .ok_or_else(|| nvenc_missing("NvEncGetEncodeProfileGUIDs"))?;
    let mut count = 0u32;
    nvenc_check(
        "NvEncGetEncodeProfileGUIDCount(HEVC)",
        count_func(encoder, NV_ENC_CODEC_HEVC_GUID, &mut count),
    )?;
    let mut guids = vec![windows::core::GUID::from_u128(0); count as usize];
    let mut written = 0u32;
    if count > 0 {
        nvenc_check(
            "NvEncGetEncodeProfileGUIDs(HEVC)",
            list_func(
                encoder,
                NV_ENC_CODEC_HEVC_GUID,
                guids.as_mut_ptr(),
                count,
                &mut written,
            ),
        )?;
    }
    let mut out = BTreeSet::new();
    for guid in guids.into_iter().take(written as usize) {
        out.insert(profile_name(guid).to_owned());
    }
    Ok(out.into_iter().collect())
}

#[cfg(windows)]
unsafe fn query_hevc_presets(
    api: &NvencApi,
    encoder: *mut c_void,
    warnings: &mut Vec<String>,
) -> Result<Vec<NvencPreset>, BackendError> {
    let count_func = api
        .functions
        .nvEncGetEncodePresetCount
        .ok_or_else(|| nvenc_missing("NvEncGetEncodePresetCount"))?;
    let list_func = api
        .functions
        .nvEncGetEncodePresetGUIDs
        .ok_or_else(|| nvenc_missing("NvEncGetEncodePresetGUIDs"))?;
    let mut count = 0u32;
    nvenc_check(
        "NvEncGetEncodePresetCount(HEVC)",
        count_func(encoder, NV_ENC_CODEC_HEVC_GUID, &mut count),
    )?;
    let mut guids = vec![windows::core::GUID::from_u128(0); count as usize];
    let mut written = 0u32;
    if count > 0 {
        nvenc_check(
            "NvEncGetEncodePresetGUIDs(HEVC)",
            list_func(
                encoder,
                NV_ENC_CODEC_HEVC_GUID,
                guids.as_mut_ptr(),
                count,
                &mut written,
            ),
        )?;
    }
    let mut out = BTreeSet::new();
    let mut unknown = Vec::new();
    for guid in guids.into_iter().take(written as usize) {
        if let Some(preset) = nvenc_preset_from_guid(guid) {
            out.insert(preset);
        } else {
            unknown.push(format!("{guid:?}"));
        }
    }
    if !unknown.is_empty() {
        warnings.push(format!(
            "NVENC HEVC preset GUID 列表包含 {} 个当前 SDK 未知值，GUI 不暴露但保留原始 GUID 供诊断：[{}]",
            unknown.len(),
            unknown.join(", ")
        ));
    }
    Ok(out.into_iter().collect())
}

#[cfg(windows)]
unsafe fn query_input_formats(
    api: &NvencApi,
    encoder: *mut c_void,
) -> Result<Vec<String>, BackendError> {
    let count_func = api
        .functions
        .nvEncGetInputFormatCount
        .ok_or_else(|| nvenc_missing("NvEncGetInputFormatCount"))?;
    let list_func = api
        .functions
        .nvEncGetInputFormats
        .ok_or_else(|| nvenc_missing("NvEncGetInputFormats"))?;
    let mut count = 0u32;
    nvenc_check(
        "NvEncGetInputFormatCount(HEVC)",
        count_func(encoder, NV_ENC_CODEC_HEVC_GUID, &mut count),
    )?;
    let mut formats = vec![0u32; count as usize];
    let mut written = 0u32;
    if count > 0 {
        nvenc_check(
            "NvEncGetInputFormats(HEVC)",
            list_func(
                encoder,
                NV_ENC_CODEC_HEVC_GUID,
                formats.as_mut_ptr(),
                count,
                &mut written,
            ),
        )?;
    }
    let mut out = BTreeSet::new();
    for format in formats.into_iter().take(written as usize) {
        out.insert(buffer_format_name(format).to_owned());
    }
    Ok(out.into_iter().collect())
}

#[cfg(windows)]
fn query_hevc_caps(
    api: &NvencApi,
    encoder: *mut c_void,
    warnings: &mut Vec<String>,
) -> NvencCapsInfo {
    NvencCapsInfo {
        max_width: query_cap(api, encoder, NV_ENC_CAPS_WIDTH_MAX, warnings),
        max_height: query_cap(api, encoder, NV_ENC_CAPS_HEIGHT_MAX, warnings),
        async_encode: query_cap(api, encoder, NV_ENC_CAPS_ASYNC_ENCODE_SUPPORT, warnings)
            .map(|value| value != 0),
        yuv422: query_cap(api, encoder, NV_ENC_CAPS_SUPPORT_YUV422_ENCODE, warnings)
            .map(|value| value != 0),
        yuv444: query_cap(api, encoder, NV_ENC_CAPS_SUPPORT_YUV444_ENCODE, warnings)
            .map(|value| value != 0),
        ten_bit: query_cap(api, encoder, NV_ENC_CAPS_SUPPORT_10BIT_ENCODE, warnings)
            .map(|value| value != 0),
        lookahead: query_cap(api, encoder, NV_ENC_CAPS_SUPPORT_LOOKAHEAD, warnings)
            .map(|value| value != 0),
        temporal_aq: query_cap(api, encoder, NV_ENC_CAPS_SUPPORT_TEMPORAL_AQ, warnings)
            .map(|value| value != 0),
        encoder_engines: query_cap(api, encoder, NV_ENC_CAPS_NUM_ENCODER_ENGINES, warnings)
            .and_then(|value| u32::try_from(value).ok()),
        rate_control_mask: query_cap(
            api,
            encoder,
            NV_ENC_CAPS_SUPPORTED_RATECONTROL_MODES,
            warnings,
        ),
    }
}

#[cfg(windows)]
fn query_cap(
    api: &NvencApi,
    encoder: *mut c_void,
    cap: u32,
    warnings: &mut Vec<String>,
) -> Option<i32> {
    let Some(func) = api.functions.nvEncGetEncodeCaps else {
        warnings.push("NVENC function list 缺少 NvEncGetEncodeCaps".to_owned());
        return None;
    };
    let mut params = NvEncCapsParam {
        version: NV_ENC_CAPS_PARAM_VER,
        capsToQuery: cap,
        reserved: [0; 62],
    };
    let mut value = 0i32;
    let status = unsafe { func(encoder, NV_ENC_CODEC_HEVC_GUID, &mut params, &mut value) };
    if status == NV_ENC_SUCCESS {
        Some(value)
    } else {
        warnings.push(format!("NvEncGetEncodeCaps({cap}) 返回 status={status}"));
        None
    }
}

fn build_route_candidates(
    input_formats: &[String],
    profiles: &[String],
    caps: &NvencCapsInfo,
    rate_controls: &[RateControlMethod],
) -> Vec<NvencRouteProbe> {
    let formats = input_formats
        .iter()
        .map(String::as_str)
        .collect::<BTreeSet<_>>();
    let profile_set = profiles.iter().map(String::as_str).collect::<BTreeSet<_>>();
    let mut out = Vec::new();
    let mut add = |format: &'static str,
                   chroma: ChromaSampling,
                   bit_depth: u16,
                   profile: &'static str,
                   extra_supported: bool,
                   note: &'static str| {
        let supported =
            formats.contains(format) && profile_set.contains(profile) && extra_supported;
        let production_record_supported = supported
            && matches!(
                format,
                "NV12" | "P010" | "NV16" | "P210" | "AYUV" | "YUV444" | "YUV444_10BIT"
            );
        out.push(NvencRouteProbe {
            input_format: format.to_owned(),
            chroma,
            bit_depth,
            profile: profile.to_owned(),
            query_supported: supported,
            production_record_supported,
            production_blocker: (!production_record_supported)
                .then(|| "NVENC SDK/驱动未同时确认该 input format/profile/caps".to_owned()),
            rate_controls: if supported {
                rate_controls.to_vec()
            } else {
                Vec::new()
            },
            rate_control_features: if supported {
                nvenc_rate_control_features(rate_controls, caps)
            } else {
                Vec::new()
            },
            note: note.to_owned(),
        });
    };

    let ten_bit = caps.ten_bit.unwrap_or(false);
    let yuv422 = caps.yuv422.unwrap_or(false);
    let yuv444 = caps.yuv444.unwrap_or(false);
    add(
        "NV12",
        ChromaSampling::Yuv420,
        8,
        "Main",
        true,
        "NVENC HEVC Main 4:2:0 8-bit D3D11 input",
    );
    add(
        "P010",
        ChromaSampling::Yuv420,
        10,
        "Main10",
        ten_bit,
        "NVENC HEVC Main10 4:2:0 10-bit D3D11 input",
    );
    add(
        "NV16",
        ChromaSampling::Yuv422,
        8,
        "FRExt",
        yuv422,
        "NVENC HEVC RExt 4:2:2 8-bit CUDA-array input",
    );
    add(
        "P210",
        ChromaSampling::Yuv422,
        10,
        "FRExt",
        yuv422 && ten_bit,
        "NVENC HEVC RExt 4:2:2 10-bit CUDA-array input",
    );
    add(
        "AYUV",
        ChromaSampling::Yuv444,
        8,
        "FRExt",
        yuv444,
        "NVENC HEVC RExt 4:4:4 8-bit packed input",
    );
    add(
        "YUV444",
        ChromaSampling::Yuv444,
        8,
        "FRExt",
        yuv444,
        "NVENC HEVC RExt 4:4:4 8-bit planar CUDA-array input",
    );
    add(
        "YUV444_10BIT",
        ChromaSampling::Yuv444,
        10,
        "FRExt",
        yuv444 && ten_bit,
        "NVENC HEVC RExt 4:4:4 10-bit planar CUDA-array input",
    );
    out.into_iter()
        .filter(|route| route.query_supported)
        .collect()
}

fn nvenc_rate_control_features(
    rate_controls: &[RateControlMethod],
    caps: &NvencCapsInfo,
) -> Vec<NvencRateControlFeatureProbe> {
    let lookahead_cap = caps.lookahead.unwrap_or(false);
    let temporal_aq_cap = caps.temporal_aq.unwrap_or(false);
    rate_controls
        .iter()
        .copied()
        .map(|method| NvencRateControlFeatureProbe {
            method,
            lookahead: lookahead_cap
                && matches!(method, RateControlMethod::Cbr | RateControlMethod::Vbr),
            vbv: matches!(method, RateControlMethod::Cbr | RateControlMethod::Vbr),
            spatial_aq: true,
            temporal_aq: temporal_aq_cap,
            target_quality: matches!(method, RateControlMethod::Vbr),
            aq: true,
        })
        .collect()
}

fn rate_controls_from_mask(mask: i32) -> Vec<RateControlMethod> {
    let mut out = Vec::new();
    // NVENC 文档把 NV_ENC_CAPS_SUPPORTED_RATECONTROL_MODES 定义为
    // NV_ENC_PARAMS_RC_MODE 值的 bitmask；这些 enum 值本身就是 0x1/0x2。
    // CONSTQP 的 enum 值是 0，无法由 bitmask 表示；只要 caps query 成功且有
    // HEVC session，就按 NVENC 基础模式暴露 CQP。
    if mask & NV_ENC_PARAMS_RC_CBR as i32 != 0 {
        out.push(RateControlMethod::Cbr);
    }
    if mask & NV_ENC_PARAMS_RC_VBR as i32 != 0 {
        out.push(RateControlMethod::Vbr);
    }
    if mask >= 0 {
        out.push(RateControlMethod::Cqp);
    }
    out
}

#[cfg(windows)]
unsafe fn query_guid_list<F>(
    count_name: &'static str,
    list_name: &'static str,
    encoder: *mut c_void,
    count_func: Option<unsafe extern "system" fn(*mut c_void, *mut u32) -> i32>,
    list_call: F,
) -> Result<Vec<windows::core::GUID>, BackendError>
where
    F: FnOnce(*mut windows::core::GUID, u32, *mut u32) -> Result<i32, BackendError>,
{
    let count_func = count_func.ok_or_else(|| nvenc_missing(count_name))?;
    let mut count = 0u32;
    nvenc_check(count_name, count_func(encoder, &mut count))?;
    let mut guids = vec![windows::core::GUID::from_u128(0); count as usize];
    let mut written = 0u32;
    if count > 0 {
        let status = list_call(guids.as_mut_ptr(), count, &mut written)?;
        nvenc_check(list_name, status)?;
    }
    Ok(guids.into_iter().take(written as usize).collect())
}

#[cfg(windows)]
unsafe fn cached_d3d11_device_for_adapter(
    adapter_index: u32,
) -> Result<
    (
        windows::Win32::Graphics::Direct3D11::ID3D11Device,
        windows::Win32::Graphics::Direct3D11::ID3D11DeviceContext,
        [u8; 8],
    ),
    BackendError,
> {
    use windows::Win32::Foundation::HMODULE;
    use windows::Win32::Graphics::Direct3D::{
        D3D_DRIVER_TYPE_UNKNOWN, D3D_FEATURE_LEVEL, D3D_FEATURE_LEVEL_11_0, D3D_FEATURE_LEVEL_11_1,
    };
    use windows::Win32::Graphics::Direct3D11::{
        D3D11_CREATE_DEVICE_BGRA_SUPPORT, D3D11_CREATE_DEVICE_VIDEO_SUPPORT, D3D11_SDK_VERSION,
        D3D11CreateDevice, ID3D11Device, ID3D11DeviceContext,
    };
    use windows::Win32::Graphics::Dxgi::{CreateDXGIFactory1, IDXGIAdapter, IDXGIFactory1};
    use windows::core::Interface;

    let factory: IDXGIFactory1 = CreateDXGIFactory1().map_err(|err| BackendError::WindowsApi {
        func: "CreateDXGIFactory1(NVENC)",
        message: err.to_string(),
    })?;
    let adapter1 =
        factory
            .EnumAdapters1(adapter_index)
            .map_err(|err| BackendError::WindowsApi {
                func: "IDXGIFactory1::EnumAdapters1(NVENC)",
                message: err.to_string(),
            })?;
    let adapter: IDXGIAdapter = adapter1.cast().map_err(|err| BackendError::WindowsApi {
        func: "IDXGIAdapter1::cast<IDXGIAdapter>(NVENC)",
        message: err.to_string(),
    })?;
    let adapter_desc = adapter1
        .GetDesc1()
        .map_err(|err| BackendError::WindowsApi {
            func: "IDXGIAdapter1::GetDesc1(NVENC device cache)",
            message: err.to_string(),
        })?;
    let adapter_key = ((adapter_desc.AdapterLuid.HighPart as u32 as u64) << 32)
        | u64::from(adapter_desc.AdapterLuid.LowPart);
    static DEVICE_CACHE: std::sync::OnceLock<
        std::sync::Mutex<HashMap<u64, (ID3D11Device, ID3D11DeviceContext)>>,
    > = std::sync::OnceLock::new();
    let (device, context) = {
        let cache = DEVICE_CACHE.get_or_init(|| std::sync::Mutex::new(HashMap::new()));
        let mut cache = cache.lock().map_err(|_| BackendError::WindowsApi {
            func: "NVENC D3D11 device cache",
            message: "device cache mutex poisoned".to_owned(),
        })?;
        let cached = cache
            .get(&adapter_key)
            .map(|(device, context)| (device.clone(), context.clone()));
        if let Some((device, context)) = cached
            && device.GetDeviceRemovedReason().is_ok()
        {
            (device, context)
        } else {
            cache.remove(&adapter_key);
            let mut device: Option<ID3D11Device> = None;
            let mut context: Option<ID3D11DeviceContext> = None;
            let levels = [D3D_FEATURE_LEVEL_11_1, D3D_FEATURE_LEVEL_11_0];
            let mut feature_level = D3D_FEATURE_LEVEL(0);
            D3D11CreateDevice(
                Some(&adapter),
                D3D_DRIVER_TYPE_UNKNOWN,
                HMODULE::default(),
                D3D11_CREATE_DEVICE_BGRA_SUPPORT | D3D11_CREATE_DEVICE_VIDEO_SUPPORT,
                Some(&levels),
                D3D11_SDK_VERSION,
                Some(&mut device),
                Some(&mut feature_level),
                Some(&mut context),
            )
            .map_err(|err| BackendError::WindowsApi {
                func: "D3D11CreateDevice(NVENC)",
                message: err.to_string(),
            })?;
            let device = device.ok_or_else(|| BackendError::WindowsApi {
                func: "D3D11CreateDevice(NVENC)",
                message: "返回空 ID3D11Device".to_owned(),
            })?;
            let context = context.ok_or_else(|| BackendError::WindowsApi {
                func: "D3D11CreateDevice(NVENC)",
                message: "返回空 ID3D11DeviceContext".to_owned(),
            })?;
            cache.insert(adapter_key, (device.clone(), context.clone()));
            (device, context)
        }
    };
    let mut luid = [0u8; 8];
    luid[..4].copy_from_slice(&adapter_desc.AdapterLuid.LowPart.to_ne_bytes());
    luid[4..].copy_from_slice(&adapter_desc.AdapterLuid.HighPart.to_ne_bytes());
    Ok((device, context, luid))
}

#[cfg(windows)]
unsafe fn open_d3d11_session_for_adapter(
    api: &NvencApi,
    adapter_index: u32,
) -> Result<NvencSession, BackendError> {
    use windows::core::Interface;

    let (device, context, _) = cached_d3d11_device_for_adapter(adapter_index)?;

    let open = api
        .functions
        .nvEncOpenEncodeSessionEx
        .ok_or_else(|| nvenc_missing("NvEncOpenEncodeSessionEx"))?;
    let destroy = api
        .functions
        .nvEncDestroyEncoder
        .ok_or_else(|| nvenc_missing("NvEncDestroyEncoder"))?;
    let mut params = NvEncOpenEncodeSessionExParams {
        version: NV_ENC_OPEN_ENCODE_SESSION_EX_PARAMS_VER,
        deviceType: NV_ENC_DEVICE_TYPE_DIRECTX,
        device: device.as_raw(),
        reserved: ptr::null_mut(),
        apiVersion: NVENCAPI_VERSION,
        reserved1: [0; 253],
        reserved2: [ptr::null_mut(); 64],
    };
    let mut encoder = ptr::null_mut();
    nvenc_check(
        "NvEncOpenEncodeSessionEx(D3D11)",
        open(&mut params, &mut encoder),
    )?;
    if encoder.is_null() {
        return Err(BackendError::unsupported(
            "NVENC",
            "NvEncOpenEncodeSessionEx",
            "返回空 encoder handle",
        ));
    }
    Ok(NvencSession {
        encoder,
        destroy: Some(destroy),
        _device: device,
        _context: context,
    })
}

#[cfg(windows)]
#[allow(clippy::too_many_arguments)]
unsafe fn initialize_low_latency_hevc_encoder(
    api: &NvencApi,
    encoder: *mut c_void,
    width: u32,
    height: u32,
    input_format: NvencD3d11InputFormat,
    color: NclxColorMetadata,
    rate_control: &RateControlConfig,
    frame_rate_num: u32,
    frame_rate_den: u32,
) -> Result<(), BackendError> {
    let initialize = api
        .functions
        .nvEncInitializeEncoder
        .ok_or_else(|| nvenc_missing("NvEncInitializeEncoder"))?;
    let preset_guid = nvenc_preset_guid(rate_control.nvenc_preset);
    let mut params: NvEncInitializeParams = std::mem::zeroed();
    params.version = NV_ENC_INITIALIZE_PARAMS_VER;
    params.encodeGUID = NV_ENC_CODEC_HEVC_GUID;
    params.presetGUID = preset_guid;
    params.encodeWidth = width;
    params.encodeHeight = height;
    params.darWidth = width;
    params.darHeight = height;
    params.frameRateNum = frame_rate_num.max(1);
    params.frameRateDen = frame_rate_den.max(1);
    params.enableEncodeAsync = 0;
    params.enablePTD = 1;
    params.bitfields = split_encode_initialize_bitfields(rate_control.nvenc_split_encode_mode);
    let preset_config = query_preset_config(api, encoder, preset_guid)?;
    let mut encode_config =
        make_low_latency_hevc_config_from_base(preset_config, input_format, color, rate_control)?;
    params.encodeConfig = encode_config.as_mut_ptr();
    params.maxEncodeWidth = width;
    params.maxEncodeHeight = height;
    params.tuningInfo = NV_ENC_TUNING_INFO_LOW_LATENCY;
    params.bufferFormat = input_format.buffer_format();
    nvenc_check(
        "NvEncInitializeEncoder(HEVC D3D11 low-latency)",
        initialize(encoder, &mut params),
    )
}

#[cfg(windows)]
fn split_encode_initialize_bitfields(mode: NvencSplitEncodeMode) -> u32 {
    (u32::from(mode.raw_value()) << NV_ENC_INITIALIZE_SPLIT_MODE_SHIFT)
        & NV_ENC_INITIALIZE_SPLIT_MODE_MASK
}

#[cfg(windows)]
unsafe fn query_preset_config(
    api: &NvencApi,
    encoder: *mut c_void,
    preset_guid: windows::core::GUID,
) -> Result<NvEncConfigOpaque, BackendError> {
    let query = api
        .functions
        .nvEncGetEncodePresetConfigEx
        .ok_or_else(|| nvenc_missing("NvEncGetEncodePresetConfigEx"))?;
    let mut preset = NvEncPresetConfig::zeroed();
    nvenc_check(
        "NvEncGetEncodePresetConfigEx(HEVC low-latency)",
        query(
            encoder,
            NV_ENC_CODEC_HEVC_GUID,
            preset_guid,
            NV_ENC_TUNING_INFO_LOW_LATENCY,
            &mut preset,
        ),
    )?;
    Ok(preset.presetCfg)
}

#[cfg(windows)]
#[repr(C, align(8))]
struct NvEncConfigOpaque {
    bytes: [u8; NV_ENC_CONFIG_OPAQUE_BYTES],
}

#[cfg(windows)]
impl NvEncConfigOpaque {
    fn zeroed() -> Self {
        Self {
            bytes: [0; NV_ENC_CONFIG_OPAQUE_BYTES],
        }
    }

    fn as_mut_ptr(&mut self) -> *mut c_void {
        self.bytes.as_mut_ptr() as *mut c_void
    }

    unsafe fn write_u32(&mut self, offset: usize, value: u32) {
        ptr::write_unaligned(self.bytes.as_mut_ptr().add(offset) as *mut u32, value);
    }

    unsafe fn read_u32(&self, offset: usize) -> u32 {
        ptr::read_unaligned(self.bytes.as_ptr().add(offset) as *const u32)
    }

    unsafe fn write_u16(&mut self, offset: usize, value: u16) {
        ptr::write_unaligned(self.bytes.as_mut_ptr().add(offset) as *mut u16, value);
    }

    unsafe fn write_u8(&mut self, offset: usize, value: u8) {
        ptr::write_unaligned(self.bytes.as_mut_ptr().add(offset), value);
    }

    unsafe fn write_i32(&mut self, offset: usize, value: i32) {
        ptr::write_unaligned(self.bytes.as_mut_ptr().add(offset) as *mut i32, value);
    }

    unsafe fn write_guid(&mut self, offset: usize, value: windows::core::GUID) {
        ptr::write_unaligned(
            self.bytes.as_mut_ptr().add(offset) as *mut windows::core::GUID,
            value,
        );
    }
}

#[cfg(windows)]
#[repr(C, align(8))]
struct NvEncPresetConfig {
    version: u32,
    reserved: u32,
    presetCfg: NvEncConfigOpaque,
    reserved1: [u32; 256],
    reserved2: [*mut c_void; 64],
}

#[cfg(windows)]
impl NvEncPresetConfig {
    unsafe fn zeroed() -> Self {
        let mut config: Self = std::mem::zeroed();
        config.version = NV_ENC_PRESET_CONFIG_VER;
        config.presetCfg.write_u32(0, NV_ENC_CONFIG_VER);
        config
    }
}

#[cfg(windows)]
unsafe fn make_low_latency_hevc_config(
    input_format: NvencD3d11InputFormat,
    color: NclxColorMetadata,
    rate_control: &RateControlConfig,
) -> Result<NvEncConfigOpaque, BackendError> {
    make_low_latency_hevc_config_from_base(
        NvEncConfigOpaque::zeroed(),
        input_format,
        color,
        rate_control,
    )
}

#[cfg(windows)]
unsafe fn make_low_latency_hevc_config_from_base(
    mut config: NvEncConfigOpaque,
    input_format: NvencD3d11InputFormat,
    color: NclxColorMetadata,
    rate_control: &RateControlConfig,
) -> Result<NvEncConfigOpaque, BackendError> {
    let (profile, bit_depth, chroma_format_idc) = match input_format {
        NvencD3d11InputFormat::Nv12 => (NV_ENC_HEVC_PROFILE_MAIN_GUID, NV_ENC_BIT_DEPTH_8, 1),
        NvencD3d11InputFormat::P010 => (NV_ENC_HEVC_PROFILE_MAIN10_GUID, NV_ENC_BIT_DEPTH_10, 1),
        NvencD3d11InputFormat::Nv16 => (NV_ENC_HEVC_PROFILE_FREXT_GUID, NV_ENC_BIT_DEPTH_8, 2),
        NvencD3d11InputFormat::P210 => (NV_ENC_HEVC_PROFILE_FREXT_GUID, NV_ENC_BIT_DEPTH_10, 2),
        NvencD3d11InputFormat::Ayuv => (NV_ENC_HEVC_PROFILE_FREXT_GUID, NV_ENC_BIT_DEPTH_8, 3),
        NvencD3d11InputFormat::Yuv444 => (NV_ENC_HEVC_PROFILE_FREXT_GUID, NV_ENC_BIT_DEPTH_8, 3),
        NvencD3d11InputFormat::Yuv44410 => (NV_ENC_HEVC_PROFILE_FREXT_GUID, NV_ENC_BIT_DEPTH_10, 3),
    };
    config.write_u32(0, NV_ENC_CONFIG_VER);
    config.write_guid(NV_ENC_CONFIG_PROFILE_GUID_OFFSET, profile);
    config.write_u32(NV_ENC_CONFIG_GOP_LENGTH_OFFSET, u32::MAX);
    // 1 = IPP... (no B frames), matching low-latency/VFR replay needs.
    config.write_i32(NV_ENC_CONFIG_FRAME_INTERVAL_P_OFFSET, 1);
    config.write_u32(
        NV_ENC_CONFIG_FRAME_FIELD_MODE_OFFSET,
        NV_ENC_PARAMS_FRAME_FIELD_MODE_FRAME,
    );
    config.write_u32(NV_ENC_CONFIG_MV_PRECISION_OFFSET, 0);
    // Preserve preset-owned codec flags while forcing repeatSPSPPS and the exact
    // chromaFormatIDC required by the selected D3D11 input layout.
    let mut hevc_flags = config.read_u32(NV_ENC_CONFIG_HEVC_FLAGS_OFFSET);
    hevc_flags |= 1 << 7;
    hevc_flags &= !(0b11 << 9);
    hevc_flags |= chroma_format_idc << 9;
    config.write_u32(NV_ENC_CONFIG_HEVC_FLAGS_OFFSET, hevc_flags);
    config.write_u32(NV_ENC_CONFIG_HEVC_IDR_PERIOD_OFFSET, u32::MAX);
    config.write_u32(NV_ENC_CONFIG_HEVC_VUI_VIDEO_SIGNAL_PRESENT_OFFSET, 1);
    config.write_u32(
        NV_ENC_CONFIG_HEVC_VUI_VIDEO_FORMAT_OFFSET,
        NV_ENC_VUI_VIDEO_FORMAT_UNSPECIFIED,
    );
    config.write_u32(
        NV_ENC_CONFIG_HEVC_VUI_FULL_RANGE_OFFSET,
        u32::from(color.full_range),
    );
    config.write_u32(NV_ENC_CONFIG_HEVC_VUI_COLOUR_DESCRIPTION_PRESENT_OFFSET, 1);
    config.write_u32(
        NV_ENC_CONFIG_HEVC_VUI_COLOUR_PRIMARIES_OFFSET,
        u32::from(color.colour_primaries),
    );
    config.write_u32(
        NV_ENC_CONFIG_HEVC_VUI_TRANSFER_CHARACTERISTICS_OFFSET,
        u32::from(color.transfer_characteristics),
    );
    config.write_u32(
        NV_ENC_CONFIG_HEVC_VUI_MATRIX_COEFFICIENTS_OFFSET,
        u32::from(color.matrix_coefficients),
    );
    config.write_u32(NV_ENC_CONFIG_HEVC_OUTPUT_BIT_DEPTH_OFFSET, bit_depth);
    config.write_u32(NV_ENC_CONFIG_HEVC_INPUT_BIT_DEPTH_OFFSET, bit_depth);
    apply_rate_control_to_nvenc_config(&mut config, rate_control)?;
    Ok(config)
}

#[cfg(windows)]
unsafe fn apply_rate_control_to_nvenc_config(
    config: &mut NvEncConfigOpaque,
    rate_control: &RateControlConfig,
) -> Result<(), BackendError> {
    let fields = rate_control.to_nvenc_fields().map_err(|err| {
        BackendError::unsupported("NVENC RateControl", rate_control.method.short_name(), err)
    })?;
    let rc = NV_ENC_CONFIG_RC_PARAMS_OFFSET;
    config.write_u32(rc, NV_ENC_RC_PARAMS_VER);
    config.write_u32(
        rc + NV_ENC_RC_PARAMS_RATE_CONTROL_MODE_OFFSET,
        fields.rate_control_mode,
    );
    config.write_u32(
        rc + NV_ENC_RC_PARAMS_AVERAGE_BIT_RATE_OFFSET,
        fields.average_bit_rate,
    );
    config.write_u32(
        rc + NV_ENC_RC_PARAMS_MAX_BIT_RATE_OFFSET,
        fields.max_bit_rate,
    );
    config.write_u32(
        rc + NV_ENC_RC_PARAMS_VBV_BUFFER_SIZE_OFFSET,
        fields.vbv_buffer_size,
    );
    config.write_u32(
        rc + NV_ENC_RC_PARAMS_VBV_INITIAL_DELAY_OFFSET,
        fields.vbv_initial_delay,
    );

    if matches!(rate_control.method, RateControlMethod::Cqp) {
        // NV_ENC_QP 的顺序是 qpInterP/qpInterB/qpIntra；GUI 字段沿用 oneVPL 的
        // QPP/QPB/QPI 命名，因此写入时显式调整顺序。
        config.write_u32(rc + NV_ENC_RC_PARAMS_CONST_QP_OFFSET, fields.const_qp_p);
        config.write_u32(rc + NV_ENC_RC_PARAMS_CONST_QP_OFFSET + 4, fields.const_qp_b);
        config.write_u32(rc + NV_ENC_RC_PARAMS_CONST_QP_OFFSET + 8, fields.const_qp_i);
    }

    let owned_bits = NV_ENC_RC_PARAMS_BIT_ENABLE_AQ
        | NV_ENC_RC_PARAMS_BIT_ENABLE_LOOKAHEAD
        | NV_ENC_RC_PARAMS_BIT_ENABLE_TEMPORAL_AQ
        | NV_ENC_RC_PARAMS_BIT_ZERO_REORDER_DELAY
        | (0xF << NV_ENC_RC_PARAMS_AQ_STRENGTH_SHIFT);
    let mut bitfields = config.read_u32(rc + NV_ENC_RC_PARAMS_BITFIELDS_OFFSET) & !owned_bits;
    if fields.zero_reorder_delay {
        bitfields |= NV_ENC_RC_PARAMS_BIT_ZERO_REORDER_DELAY;
    }
    if fields.enable_lookahead {
        bitfields |= NV_ENC_RC_PARAMS_BIT_ENABLE_LOOKAHEAD;
        config.write_u16(
            rc + NV_ENC_RC_PARAMS_LOOKAHEAD_DEPTH_OFFSET,
            fields.lookahead_depth,
        );
    }
    if fields.enable_spatial_aq {
        bitfields |= NV_ENC_RC_PARAMS_BIT_ENABLE_AQ;
        let strength = u32::from(fields.aq_strength.min(15));
        bitfields |= strength << NV_ENC_RC_PARAMS_AQ_STRENGTH_SHIFT;
    }
    if fields.enable_temporal_aq {
        bitfields |= NV_ENC_RC_PARAMS_BIT_ENABLE_TEMPORAL_AQ;
    }
    config.write_u32(rc + NV_ENC_RC_PARAMS_BITFIELDS_OFFSET, bitfields);
    config.write_u8(
        rc + NV_ENC_RC_PARAMS_TARGET_QUALITY_OFFSET,
        fields.target_quality.min(51),
    );
    config.write_u8(rc + NV_ENC_RC_PARAMS_TARGET_QUALITY_LSB_OFFSET, 0);
    config.write_u32(rc + NV_ENC_RC_PARAMS_MULTI_PASS_OFFSET, fields.multi_pass);
    Ok(())
}

#[cfg(windows)]
unsafe fn create_bitstream_buffer(
    api: &NvencApi,
    encoder: *mut c_void,
) -> Result<NvencBitstreamBuffer, BackendError> {
    let create = api
        .functions
        .nvEncCreateBitstreamBuffer
        .ok_or_else(|| nvenc_missing("NvEncCreateBitstreamBuffer"))?;
    let destroy = api
        .functions
        .nvEncDestroyBitstreamBuffer
        .ok_or_else(|| nvenc_missing("NvEncDestroyBitstreamBuffer"))?;
    let mut params: NvEncCreateBitstreamBuffer = std::mem::zeroed();
    params.version = NV_ENC_CREATE_BITSTREAM_BUFFER_VER;
    nvenc_check("NvEncCreateBitstreamBuffer", create(encoder, &mut params))?;
    if params.bitstreamBuffer.is_null() {
        return Err(BackendError::unsupported(
            "NVENC encode smoke",
            "NvEncCreateBitstreamBuffer",
            "返回空 output buffer",
        ));
    }
    Ok(NvencBitstreamBuffer {
        encoder,
        buffer: params.bitstreamBuffer,
        destroy: Some(destroy),
    })
}

#[cfg(windows)]
unsafe fn create_synthetic_input_texture(
    device: &windows::Win32::Graphics::Direct3D11::ID3D11Device,
    width: u32,
    height: u32,
    input_format: NvencD3d11InputFormat,
) -> Result<windows::Win32::Graphics::Direct3D11::ID3D11Texture2D, BackendError> {
    use windows::Win32::Graphics::Direct3D11::{
        D3D11_SUBRESOURCE_DATA, D3D11_TEXTURE2D_DESC, D3D11_USAGE_DEFAULT,
    };
    use windows::Win32::Graphics::Dxgi::Common::DXGI_SAMPLE_DESC;

    let width_usize = width as usize;
    let height_usize = height as usize;
    let pitch = input_format.dxgi_pitch_bytes(width) as usize;
    let mut pixels = vec![0u8; input_format.frame_size_bytes(width, height)];
    match input_format {
        NvencD3d11InputFormat::Nv12 | NvencD3d11InputFormat::Nv16 => {
            for y in 0..height_usize {
                let row = &mut pixels[y * pitch..y * pitch + width_usize];
                for (x, value) in row.iter_mut().enumerate() {
                    *value =
                        (((x * 255) / width_usize.max(1)) as u8).saturating_add((y % 64) as u8);
                }
            }
            let chroma_start = pitch * height_usize;
            let chroma_rows = if input_format == NvencD3d11InputFormat::Nv12 {
                height_usize / 2
            } else {
                height_usize
            };
            for y in 0..chroma_rows {
                let row =
                    &mut pixels[chroma_start + y * pitch..chroma_start + y * pitch + width_usize];
                for pair in row.chunks_exact_mut(2) {
                    pair[0] = 128;
                    pair[1] = 128;
                }
            }
        }
        NvencD3d11InputFormat::P010 | NvencD3d11InputFormat::P210 => {
            for y in 0..height_usize {
                let row = &mut pixels[y * pitch..y * pitch + width_usize * 2];
                for (x, px) in row.chunks_exact_mut(2).enumerate() {
                    let ten_bit = (((x * 1023) / width_usize.max(1)) as u16)
                        .saturating_add(((y % 64) as u16) << 2)
                        .min(1023);
                    let value = ten_bit << 6;
                    px.copy_from_slice(&value.to_le_bytes());
                }
            }
            let chroma_start = pitch * height_usize;
            let chroma_rows = if input_format == NvencD3d11InputFormat::P010 {
                height_usize / 2
            } else {
                height_usize
            };
            for y in 0..chroma_rows {
                let row = &mut pixels
                    [chroma_start + y * pitch..chroma_start + y * pitch + width_usize * 2];
                for pair in row.chunks_exact_mut(4) {
                    let neutral = (512u16 << 6).to_le_bytes();
                    pair[0..2].copy_from_slice(&neutral);
                    pair[2..4].copy_from_slice(&neutral);
                }
            }
        }
        NvencD3d11InputFormat::Yuv444 => {
            for y in 0..height_usize {
                let row = &mut pixels[y * pitch..(y + 1) * pitch];
                for (x, value) in row.iter_mut().enumerate() {
                    *value = ((x * 255) / width_usize.max(1)) as u8;
                }
            }
            let u_start = pitch * height_usize;
            let v_start = u_start + pitch * height_usize;
            pixels[u_start..v_start].fill(128);
            pixels[v_start..v_start + pitch * height_usize].fill(128);
        }
        NvencD3d11InputFormat::Yuv44410 => {
            for y in 0..height_usize {
                let row = &mut pixels[y * pitch..(y + 1) * pitch];
                for (x, value) in row.chunks_exact_mut(2).enumerate() {
                    let sample = (((x * 1023) / width_usize.max(1)) as u16) << 6;
                    value.copy_from_slice(&sample.to_le_bytes());
                }
            }
            let plane_bytes = pitch * height_usize;
            let neutral = (512u16 << 6).to_le_bytes();
            for plane in 1..=2 {
                let start = plane * plane_bytes;
                for value in pixels[start..start + plane_bytes].chunks_exact_mut(2) {
                    value.copy_from_slice(&neutral);
                }
            }
        }
        NvencD3d11InputFormat::Ayuv => {
            for y in 0..height_usize {
                let row = &mut pixels[y * pitch..(y + 1) * pitch];
                for (x, px) in row.chunks_exact_mut(4).enumerate() {
                    let luma = ((x * 255) / width_usize.max(1)) as u8;
                    let word = (0xffu32 << 24) | (u32::from(luma) << 16) | (128u32 << 8) | 128u32;
                    px.copy_from_slice(&word.to_le_bytes());
                }
            }
        }
    }

    let desc = D3D11_TEXTURE2D_DESC {
        Width: width,
        Height: input_format.texture_height(height),
        MipLevels: 1,
        ArraySize: 1,
        Format: input_format.dxgi_format(),
        SampleDesc: DXGI_SAMPLE_DESC {
            Count: 1,
            Quality: 0,
        },
        Usage: D3D11_USAGE_DEFAULT,
        BindFlags: 0,
        CPUAccessFlags: 0,
        MiscFlags: 0,
    };
    let initial = D3D11_SUBRESOURCE_DATA {
        pSysMem: pixels.as_ptr() as *const c_void,
        SysMemPitch: input_format.dxgi_pitch_bytes(width),
        SysMemSlicePitch: 0,
    };
    let mut texture = None;
    device
        .CreateTexture2D(&desc, Some(&initial), Some(&mut texture))
        .map_err(|err| BackendError::WindowsApi {
            func: "ID3D11Device::CreateTexture2D(NVENC synthetic input)",
            message: err.to_string(),
        })?;
    texture.ok_or_else(|| BackendError::WindowsApi {
        func: "CreateTexture2D(NVENC synthetic input)",
        message: "返回空纹理".to_owned(),
    })
}

#[cfg(windows)]
unsafe fn open_cuda_session(
    api: &NvencApi,
    context: *mut c_void,
) -> Result<NvencCudaSession, BackendError> {
    let open = api
        .functions
        .nvEncOpenEncodeSessionEx
        .ok_or_else(|| nvenc_missing("NvEncOpenEncodeSessionEx"))?;
    let destroy = api
        .functions
        .nvEncDestroyEncoder
        .ok_or_else(|| nvenc_missing("NvEncDestroyEncoder"))?;
    let mut params = NvEncOpenEncodeSessionExParams {
        version: NV_ENC_OPEN_ENCODE_SESSION_EX_PARAMS_VER,
        deviceType: NV_ENC_DEVICE_TYPE_CUDA,
        device: context,
        reserved: ptr::null_mut(),
        apiVersion: NVENCAPI_VERSION,
        reserved1: [0; 253],
        reserved2: [ptr::null_mut(); 64],
    };
    let mut encoder = ptr::null_mut();
    nvenc_check(
        "NvEncOpenEncodeSessionEx(CUDA)",
        open(&mut params, &mut encoder),
    )?;
    if encoder.is_null() {
        return Err(BackendError::unsupported(
            "NVENC CUDA",
            "NvEncOpenEncodeSessionEx",
            "返回空 encoder handle",
        ));
    }
    Ok(NvencCudaSession {
        encoder,
        destroy: Some(destroy),
    })
}

#[cfg(windows)]
unsafe fn create_synthetic_input_buffer(
    device: &windows::Win32::Graphics::Direct3D11::ID3D11Device,
    width: u32,
    height: u32,
    input_format: NvencD3d11InputFormat,
) -> Result<windows::Win32::Graphics::Direct3D11::ID3D11Buffer, BackendError> {
    use windows::Win32::Graphics::Direct3D11::{
        D3D11_BUFFER_DESC, D3D11_SUBRESOURCE_DATA, D3D11_USAGE_DEFAULT,
    };

    let byte_width = input_format.frame_size_bytes(width, height);
    let byte_width_u32 = u32::try_from(byte_width).map_err(|_| {
        BackendError::unsupported(
            "NVENC synthetic D3D11 buffer",
            format!("{} bytes", byte_width),
            "输入资源超过 D3D11 buffer 的 u32 ByteWidth",
        )
    })?;
    let pixels = vec![0u8; byte_width];
    let desc = D3D11_BUFFER_DESC {
        ByteWidth: byte_width_u32,
        Usage: D3D11_USAGE_DEFAULT,
        BindFlags: 0,
        CPUAccessFlags: 0,
        MiscFlags: 0,
        StructureByteStride: 0,
    };
    let initial = D3D11_SUBRESOURCE_DATA {
        pSysMem: pixels.as_ptr() as *const c_void,
        SysMemPitch: 0,
        SysMemSlicePitch: 0,
    };
    let mut buffer = None;
    device
        .CreateBuffer(&desc, Some(&initial), Some(&mut buffer))
        .map_err(|err| BackendError::WindowsApi {
            func: "ID3D11Device::CreateBuffer(NVENC synthetic planar input)",
            message: err.to_string(),
        })?;
    buffer.ok_or_else(|| BackendError::WindowsApi {
        func: "CreateBuffer(NVENC synthetic planar input)",
        message: "返回空 buffer".to_owned(),
    })
}

#[cfg(windows)]
unsafe fn create_synthetic_external_input_texture(
    device: &windows::Win32::Graphics::Direct3D11::ID3D11Device,
    width: u32,
    height: u32,
    input_format: NvencD3d11InputFormat,
) -> Result<
    (
        windows::Win32::Graphics::Direct3D11::ID3D11Texture2D,
        windows::Win32::Foundation::HANDLE,
    ),
    BackendError,
> {
    use windows::Win32::Foundation::GENERIC_ALL;
    use windows::Win32::Graphics::Direct3D11::{
        D3D11_BIND_UNORDERED_ACCESS, D3D11_RESOURCE_MISC_SHARED_KEYEDMUTEX,
        D3D11_RESOURCE_MISC_SHARED_NTHANDLE, D3D11_SUBRESOURCE_DATA, D3D11_TEXTURE2D_DESC,
        D3D11_USAGE_DEFAULT,
    };
    use windows::Win32::Graphics::Dxgi::Common::DXGI_SAMPLE_DESC;
    use windows::Win32::Graphics::Dxgi::IDXGIResource1;
    use windows::core::{Interface, PCWSTR};

    let pixels = vec![0u8; input_format.frame_size_bytes(width, height)];
    let desc = D3D11_TEXTURE2D_DESC {
        Width: width,
        Height: input_format.texture_height(height),
        MipLevels: 1,
        ArraySize: 1,
        Format: input_format.dxgi_format(),
        SampleDesc: DXGI_SAMPLE_DESC {
            Count: 1,
            Quality: 0,
        },
        Usage: D3D11_USAGE_DEFAULT,
        BindFlags: D3D11_BIND_UNORDERED_ACCESS.0 as u32,
        CPUAccessFlags: 0,
        MiscFlags: (D3D11_RESOURCE_MISC_SHARED_NTHANDLE.0 | D3D11_RESOURCE_MISC_SHARED_KEYEDMUTEX.0)
            as u32,
    };
    let initial = D3D11_SUBRESOURCE_DATA {
        pSysMem: pixels.as_ptr() as *const c_void,
        SysMemPitch: input_format.dxgi_pitch_bytes(width),
        SysMemSlicePitch: 0,
    };
    let mut texture = None;
    device
        .CreateTexture2D(&desc, Some(&initial), Some(&mut texture))
        .map_err(|err| BackendError::WindowsApi {
            func: "ID3D11Device::CreateTexture2D(NVENC CUDA external memory)",
            message: err.to_string(),
        })?;
    let texture = texture.ok_or_else(|| BackendError::WindowsApi {
        func: "CreateTexture2D(NVENC CUDA external memory)",
        message: "返回空 texture".to_owned(),
    })?;
    let resource: IDXGIResource1 = texture.cast().map_err(|err| BackendError::WindowsApi {
        func: "ID3D11Texture2D::cast<IDXGIResource1>(NVENC CUDA external memory)",
        message: err.to_string(),
    })?;
    let handle = resource
        .CreateSharedHandle(None, GENERIC_ALL.0, PCWSTR::null())
        .map_err(|err| BackendError::WindowsApi {
            func: "IDXGIResource1::CreateSharedHandle(NVENC CUDA external memory)",
            message: err.to_string(),
        })?;
    Ok((texture, handle))
}

#[cfg(windows)]
unsafe fn validate_d3d11_input_texture(
    texture: &windows::Win32::Graphics::Direct3D11::ID3D11Texture2D,
    width: u32,
    height: u32,
    input_format: NvencD3d11InputFormat,
) -> Result<(), BackendError> {
    let mut desc = windows::Win32::Graphics::Direct3D11::D3D11_TEXTURE2D_DESC::default();
    texture.GetDesc(&mut desc);
    if desc.Width != width
        || desc.Height != input_format.texture_height(height)
        || desc.Format != input_format.dxgi_format()
    {
        return Err(BackendError::unsupported(
            "NVENC D3D11 input texture",
            format!(
                "{}x{} DXGI_FORMAT({})",
                desc.Width, desc.Height, desc.Format.0
            ),
            format!(
                "需要 storage={}x{} {}，encode={}x{}，禁止 CPU/staging/raw-frame fallback",
                width,
                input_format.texture_height(height),
                input_format.label(),
                width,
                height
            ),
        ));
    }
    Ok(())
}

#[cfg(windows)]
unsafe fn register_d3d11_input_texture(
    api: &NvencApi,
    encoder: *mut c_void,
    texture: &windows::Win32::Graphics::Direct3D11::ID3D11Texture2D,
    width: u32,
    height: u32,
    input_format: NvencD3d11InputFormat,
) -> Result<NvencRegisteredResource, BackendError> {
    use windows::core::Interface;

    let resource = texture
        .cast::<windows::Win32::Graphics::Direct3D11::ID3D11Resource>()
        .map_err(|err| BackendError::WindowsApi {
            func: "ID3D11Texture2D::cast<ID3D11Resource>(NVENC input)",
            message: err.to_string(),
        })?;
    register_d3d11_input_resource(api, encoder, &resource, width, height, input_format)
}

#[cfg(windows)]
unsafe fn register_d3d11_input_resource(
    api: &NvencApi,
    encoder: *mut c_void,
    resource: &windows::Win32::Graphics::Direct3D11::ID3D11Resource,
    width: u32,
    height: u32,
    input_format: NvencD3d11InputFormat,
) -> Result<NvencRegisteredResource, BackendError> {
    use windows::core::Interface;

    let register = api
        .functions
        .nvEncRegisterResource
        .ok_or_else(|| nvenc_missing("NvEncRegisterResource"))?;
    let unregister = api
        .functions
        .nvEncUnregisterResource
        .ok_or_else(|| nvenc_missing("NvEncUnregisterResource"))?;
    let mut params: NvEncRegisterResource = std::mem::zeroed();
    params.version = NV_ENC_REGISTER_RESOURCE_VER;
    params.resourceType = NV_ENC_INPUT_RESOURCE_TYPE_DIRECTX;
    params.width = width;
    params.height = height;
    // D3D11 texture 资源由驱动按 subresource layout 管理；CPU pitch 仅适用于
    // host/pitched buffer。NVENC D3D11 registered-resource 冒烟在 NVIDIA 驱动上
    // 需要这里保持 0，否则部分格式（尤其 P010）会被判为 invalid param。
    params.pitch = 0;
    params.subResourceIndex = 0;
    params.resourceToRegister = resource.as_raw();
    params.bufferFormat = input_format.buffer_format();
    params.bufferUsage = NV_ENC_INPUT_IMAGE;
    nvenc_check(
        "NvEncRegisterResource(D3D11 texture)",
        register(encoder, &mut params),
    )?;
    if params.registeredResource.is_null() {
        return Err(BackendError::unsupported(
            "NVENC encode smoke",
            "NvEncRegisterResource",
            "返回空 registeredResource",
        ));
    }
    Ok(NvencRegisteredResource {
        encoder,
        resource: params.registeredResource,
        unregister: Some(unregister),
        _resource: Some(resource.clone()),
    })
}

#[cfg(windows)]
unsafe fn register_cuda_array_input(
    api: &NvencApi,
    encoder: *mut c_void,
    array: *mut c_void,
    width: u32,
    height: u32,
    input_format: NvencD3d11InputFormat,
) -> Result<NvencRegisteredResource, BackendError> {
    let register = api
        .functions
        .nvEncRegisterResource
        .ok_or_else(|| nvenc_missing("NvEncRegisterResource"))?;
    let unregister = api
        .functions
        .nvEncUnregisterResource
        .ok_or_else(|| nvenc_missing("NvEncUnregisterResource"))?;
    let mut params: NvEncRegisterResource = std::mem::zeroed();
    params.version = NV_ENC_REGISTER_RESOURCE_VER;
    params.resourceType = NV_ENC_INPUT_RESOURCE_TYPE_CUDAARRAY;
    params.width = width;
    params.height = height;
    params.pitch = input_format.dxgi_pitch_bytes(width);
    params.subResourceIndex = 0;
    params.resourceToRegister = array;
    params.bufferFormat = input_format.buffer_format();
    params.bufferUsage = NV_ENC_INPUT_IMAGE;
    nvenc_check(
        "NvEncRegisterResource(CUDA array)",
        register(encoder, &mut params),
    )?;
    if params.registeredResource.is_null() {
        return Err(BackendError::unsupported(
            "NVENC CUDA encode",
            "NvEncRegisterResource",
            "返回空 registeredResource",
        ));
    }
    Ok(NvencRegisteredResource {
        encoder,
        resource: params.registeredResource,
        unregister: Some(unregister),
        _resource: None,
    })
}

#[cfg(windows)]
unsafe fn map_input_resource(
    api: &NvencApi,
    encoder: *mut c_void,
    registered: &NvencRegisteredResource,
) -> Result<NvencMappedInputResource, BackendError> {
    let map = api
        .functions
        .nvEncMapInputResource
        .ok_or_else(|| nvenc_missing("NvEncMapInputResource"))?;
    let unmap = api
        .functions
        .nvEncUnmapInputResource
        .ok_or_else(|| nvenc_missing("NvEncUnmapInputResource"))?;
    let mut params: NvEncMapInputResource = std::mem::zeroed();
    params.version = NV_ENC_MAP_INPUT_RESOURCE_VER;
    params.registeredResource = registered.resource;
    nvenc_check(
        "NvEncMapInputResource(D3D11 texture)",
        map(encoder, &mut params),
    )?;
    if params.mappedResource.is_null() {
        return Err(BackendError::unsupported(
            "NVENC encode smoke",
            "NvEncMapInputResource",
            "返回空 mappedResource",
        ));
    }
    Ok(NvencMappedInputResource {
        encoder,
        mapped: params.mappedResource,
        format: params.mappedBufferFmt,
        unmap: Some(unmap),
    })
}

#[cfg(windows)]
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum NvencEncodePictureStatus {
    OutputAvailable,
    NeedMoreInput,
}

#[cfg(windows)]
#[allow(clippy::too_many_arguments)]
unsafe fn encode_one_d3d11_frame(
    api: &NvencApi,
    encoder: *mut c_void,
    mapped: &NvencMappedInputResource,
    bitstream: &NvencBitstreamBuffer,
    width: u32,
    height: u32,
    input_format: NvencD3d11InputFormat,
    frame_idx: u32,
    timestamp_90k: u64,
    force_idr: bool,
) -> Result<NvencEncodePictureStatus, BackendError> {
    let encode = api
        .functions
        .nvEncEncodePicture
        .ok_or_else(|| nvenc_missing("NvEncEncodePicture"))?;
    let mut params: NvEncPicParams = std::mem::zeroed();
    params.version = NV_ENC_PIC_PARAMS_VER;
    params.inputWidth = width;
    params.inputHeight = height;
    params.inputPitch = input_format.dxgi_pitch_bytes(width);
    params.encodePicFlags = if force_idr {
        NV_ENC_PIC_FLAG_FORCEIDR | NV_ENC_PIC_FLAG_OUTPUT_SPSPPS
    } else {
        0
    };
    params.frameIdx = frame_idx;
    params.inputTimeStamp = timestamp_90k;
    params.inputDuration = 1;
    params.inputBuffer = mapped.mapped;
    params.outputBitstream = bitstream.buffer;
    params.bufferFmt = if mapped.format == 0 {
        input_format.buffer_format()
    } else {
        mapped.format
    };
    params.pictureStruct = NV_ENC_PIC_STRUCT_FRAME;
    match encode(encoder, &mut params) {
        NV_ENC_SUCCESS => Ok(NvencEncodePictureStatus::OutputAvailable),
        NV_ENC_ERR_NEED_MORE_INPUT => Ok(NvencEncodePictureStatus::NeedMoreInput),
        status => {
            nvenc_check("NvEncEncodePicture(D3D11 texture)", status)?;
            unreachable!()
        }
    }
}

#[cfg(windows)]
unsafe fn submit_encoder_eos(api: &NvencApi, encoder: *mut c_void) -> Result<(), BackendError> {
    let encode = api
        .functions
        .nvEncEncodePicture
        .ok_or_else(|| nvenc_missing("NvEncEncodePicture"))?;
    let mut params: NvEncPicParams = std::mem::zeroed();
    params.version = NV_ENC_PIC_PARAMS_VER;
    params.encodePicFlags = NV_ENC_PIC_FLAG_EOS;
    nvenc_check("NvEncEncodePicture(EOS)", encode(encoder, &mut params))
}

#[cfg(windows)]
struct NvencLockedOutput {
    bytes: Vec<u8>,
    annex_b_start_code_seen: bool,
    output_timestamp_90k: u64,
}

#[cfg(windows)]
unsafe fn lock_and_copy_bitstream(
    api: &NvencApi,
    encoder: *mut c_void,
    bitstream: &NvencBitstreamBuffer,
) -> Result<NvencLockedOutput, BackendError> {
    let lock = api
        .functions
        .nvEncLockBitstream
        .ok_or_else(|| nvenc_missing("NvEncLockBitstream"))?;
    let unlock = api
        .functions
        .nvEncUnlockBitstream
        .ok_or_else(|| nvenc_missing("NvEncUnlockBitstream"))?;
    let mut params: NvEncLockBitstream = std::mem::zeroed();
    params.version = NV_ENC_LOCK_BITSTREAM_VER;
    params.outputBitstream = bitstream.buffer;
    nvenc_check("NvEncLockBitstream", lock(encoder, &mut params))?;
    let mut locked = NvencLockedBitstream {
        encoder,
        buffer: bitstream,
        unlock: Some(unlock),
    };
    if params.bitstreamBufferPtr.is_null() {
        return Err(BackendError::unsupported(
            "NVENC encode smoke",
            "NvEncLockBitstream",
            "返回空 bitstreamBufferPtr",
        ));
    }
    let bytes = std::slice::from_raw_parts(
        params.bitstreamBufferPtr as *const u8,
        params.bitstreamSizeInBytes as usize,
    )
    .to_vec();
    let annex_b_start_code_seen = bytes
        .windows(4)
        .any(|window| window == [0x00, 0x00, 0x00, 0x01]);
    nvenc_check("NvEncUnlockBitstream", locked.unlock_now())?;
    Ok(NvencLockedOutput {
        bytes,
        annex_b_start_code_seen,
        output_timestamp_90k: params.outputTimeStamp,
    })
}

#[cfg(windows)]
struct NvencSession {
    encoder: *mut c_void,
    destroy: Option<unsafe extern "system" fn(*mut c_void) -> i32>,
    _device: windows::Win32::Graphics::Direct3D11::ID3D11Device,
    _context: windows::Win32::Graphics::Direct3D11::ID3D11DeviceContext,
}

#[cfg(windows)]
struct NvencCudaSession {
    encoder: *mut c_void,
    destroy: Option<unsafe extern "system" fn(*mut c_void) -> i32>,
}

#[cfg(windows)]
impl NvencCudaSession {
    unsafe fn destroy_now(&mut self) -> i32 {
        if self.encoder.is_null() {
            return NV_ENC_SUCCESS;
        }
        let encoder = std::mem::replace(&mut self.encoder, ptr::null_mut());
        self.destroy
            .map(|destroy| destroy(encoder))
            .unwrap_or(NV_ENC_SUCCESS)
    }
}

#[cfg(windows)]
impl Drop for NvencCudaSession {
    fn drop(&mut self) {
        unsafe {
            let _ = self.destroy_now();
        }
    }
}

#[cfg(windows)]
impl NvencSession {
    fn device(&self) -> &windows::Win32::Graphics::Direct3D11::ID3D11Device {
        &self._device
    }

    fn context(&self) -> &windows::Win32::Graphics::Direct3D11::ID3D11DeviceContext {
        &self._context
    }

    unsafe fn destroy_now(&mut self) -> i32 {
        if self.encoder.is_null() {
            return NV_ENC_SUCCESS;
        }
        let encoder = std::mem::replace(&mut self.encoder, ptr::null_mut());
        self.destroy
            .map(|destroy| destroy(encoder))
            .unwrap_or(NV_ENC_SUCCESS)
    }
}

#[cfg(windows)]
impl Drop for NvencSession {
    fn drop(&mut self) {
        unsafe {
            let _ = self.destroy_now();
        }
    }
}

#[cfg(windows)]
struct NvencBitstreamBuffer {
    encoder: *mut c_void,
    buffer: *mut c_void,
    destroy: Option<unsafe extern "system" fn(*mut c_void, *mut c_void) -> i32>,
}

#[cfg(windows)]
impl NvencBitstreamBuffer {
    unsafe fn destroy_now(&mut self) -> i32 {
        if self.encoder.is_null() || self.buffer.is_null() {
            return NV_ENC_SUCCESS;
        }
        let buffer = std::mem::replace(&mut self.buffer, ptr::null_mut());
        self.destroy
            .map(|destroy| destroy(self.encoder, buffer))
            .unwrap_or(NV_ENC_SUCCESS)
    }

    unsafe fn abandon(&mut self) {
        self.encoder = ptr::null_mut();
        self.buffer = ptr::null_mut();
        self.destroy = None;
    }
}

#[cfg(windows)]
impl Drop for NvencBitstreamBuffer {
    fn drop(&mut self) {
        unsafe {
            let _ = self.destroy_now();
        }
    }
}

#[cfg(windows)]
struct NvencRegisteredResource {
    encoder: *mut c_void,
    resource: *mut c_void,
    unregister: Option<unsafe extern "system" fn(*mut c_void, *mut c_void) -> i32>,
    // NVENC requires the external D3D11 resource to outlive unregister.
    _resource: Option<windows::Win32::Graphics::Direct3D11::ID3D11Resource>,
}

#[cfg(windows)]
impl NvencRegisteredResource {
    unsafe fn unregister_now(&mut self) -> i32 {
        if self.encoder.is_null() || self.resource.is_null() {
            return NV_ENC_SUCCESS;
        }
        let resource = std::mem::replace(&mut self.resource, ptr::null_mut());
        self.unregister
            .map(|unregister| unregister(self.encoder, resource))
            .unwrap_or(NV_ENC_SUCCESS)
    }

    unsafe fn abandon(&mut self) {
        self.encoder = ptr::null_mut();
        self.resource = ptr::null_mut();
        self.unregister = None;
    }
}

#[cfg(windows)]
impl Drop for NvencRegisteredResource {
    fn drop(&mut self) {
        unsafe {
            let _ = self.unregister_now();
        }
    }
}

#[cfg(windows)]
struct NvencMappedInputResource {
    encoder: *mut c_void,
    mapped: *mut c_void,
    format: u32,
    unmap: Option<unsafe extern "system" fn(*mut c_void, *mut c_void) -> i32>,
}

#[cfg(windows)]
impl NvencMappedInputResource {
    unsafe fn unmap_now(&mut self) -> i32 {
        if self.encoder.is_null() || self.mapped.is_null() {
            return NV_ENC_SUCCESS;
        }
        let mapped = std::mem::replace(&mut self.mapped, ptr::null_mut());
        self.unmap
            .map(|unmap| unmap(self.encoder, mapped))
            .unwrap_or(NV_ENC_SUCCESS)
    }

    unsafe fn abandon(&mut self) {
        self.encoder = ptr::null_mut();
        self.mapped = ptr::null_mut();
        self.unmap = None;
    }
}

#[cfg(windows)]
impl Drop for NvencMappedInputResource {
    fn drop(&mut self) {
        unsafe {
            let _ = self.unmap_now();
        }
    }
}

#[cfg(windows)]
struct NvencLockedBitstream<'a> {
    encoder: *mut c_void,
    buffer: &'a NvencBitstreamBuffer,
    unlock: Option<unsafe extern "system" fn(*mut c_void, *mut c_void) -> i32>,
}

#[cfg(windows)]
impl NvencLockedBitstream<'_> {
    unsafe fn unlock_now(&mut self) -> i32 {
        if self.encoder.is_null() || self.buffer.buffer.is_null() {
            return NV_ENC_SUCCESS;
        }
        let encoder = std::mem::replace(&mut self.encoder, ptr::null_mut());
        self.unlock
            .map(|unlock| unlock(encoder, self.buffer.buffer))
            .unwrap_or(NV_ENC_SUCCESS)
    }
}

#[cfg(windows)]
impl Drop for NvencLockedBitstream<'_> {
    fn drop(&mut self) {
        unsafe {
            let _ = self.unlock_now();
        }
    }
}

#[cfg(windows)]
struct NvencApi {
    _lib: Library,
    _dll_path: PathBuf,
    max_supported_version: Option<u32>,
    functions: NvEncodeApiFunctionList,
}

#[cfg(windows)]
impl NvencApi {
    fn load() -> Result<(Self, PathBuf), String> {
        let mut errors = Vec::new();
        for candidate in candidate_dlls() {
            let lib = match unsafe { Library::new(&candidate) } {
                Ok(lib) => lib,
                Err(err) => {
                    errors.push(format!("{}: {err}", candidate.display()));
                    continue;
                }
            };
            let max_supported_version = unsafe {
                let func = lib
                    .get::<unsafe extern "system" fn(*mut u32) -> i32>(
                        b"NvEncodeAPIGetMaxSupportedVersion\0",
                    )
                    .ok();
                if let Some(func) = func {
                    let mut version = 0u32;
                    if func(&mut version) == NV_ENC_SUCCESS {
                        Some(version)
                    } else {
                        None
                    }
                } else {
                    None
                }
            };
            if let Some(version) = max_supported_version
                && version < NVENCAPI_DRIVER_VERSION
            {
                errors.push(format!(
                    "{}: NVIDIA driver NVENC API {}.{} 低于本程序编译所需 {}.{}",
                    candidate.display(),
                    version >> 4,
                    version & 0xF,
                    NVENCAPI_MAJOR_VERSION,
                    NVENCAPI_MINOR_VERSION
                ));
                continue;
            }
            let create = unsafe {
                lib.get::<unsafe extern "system" fn(*mut NvEncodeApiFunctionList) -> i32>(
                    b"NvEncodeAPICreateInstance\0",
                )
            };
            let create = match create {
                Ok(func) => func,
                Err(err) => {
                    errors.push(format!(
                        "{}: 缺少 NvEncodeAPICreateInstance: {err}",
                        candidate.display()
                    ));
                    continue;
                }
            };
            let mut functions = NvEncodeApiFunctionList::zeroed();
            functions.version = NV_ENCODE_API_FUNCTION_LIST_VER;
            let status = unsafe { create(&mut functions) };
            if status != NV_ENC_SUCCESS {
                errors.push(format!(
                    "{}: NvEncodeAPICreateInstance status={status}",
                    candidate.display()
                ));
                continue;
            }
            return Ok((
                Self {
                    _lib: lib,
                    _dll_path: candidate.clone(),
                    max_supported_version,
                    functions,
                },
                candidate,
            ));
        }
        Err(format!(
            "无法加载 nvEncodeAPI64.dll；尝试路径：{}",
            errors.join("；")
        ))
    }
}

#[cfg(windows)]
fn candidate_dlls() -> Vec<PathBuf> {
    let mut out = Vec::new();
    if let Some(path) = std::env::var_os("RUSTREPLAY_NVENC_DLL") {
        out.push(PathBuf::from(path));
    }
    if let Some(system_root) = std::env::var_os("SystemRoot") {
        out.push(
            PathBuf::from(system_root)
                .join("System32")
                .join("nvEncodeAPI64.dll"),
        );
    }
    out.push(PathBuf::from("nvEncodeAPI64.dll"));
    out
}

#[cfg(windows)]
#[repr(C)]
struct NvEncCapsParam {
    version: u32,
    capsToQuery: u32,
    reserved: [u32; 62],
}

#[cfg(windows)]
#[repr(C)]
struct NvEncOpenEncodeSessionExParams {
    version: u32,
    deviceType: u32,
    device: *mut c_void,
    reserved: *mut c_void,
    apiVersion: u32,
    reserved1: [u32; 253],
    reserved2: [*mut c_void; 64],
}

#[cfg(windows)]
#[repr(C)]
#[derive(Clone, Copy)]
struct NvencExternalMeHintCountsPerBlock {
    bitfields: u32,
    reserved1: [u32; 3],
}

#[cfg(windows)]
#[repr(C)]
struct NvEncInitializeParams {
    version: u32,
    encodeGUID: windows::core::GUID,
    presetGUID: windows::core::GUID,
    encodeWidth: u32,
    encodeHeight: u32,
    darWidth: u32,
    darHeight: u32,
    frameRateNum: u32,
    frameRateDen: u32,
    enableEncodeAsync: u32,
    enablePTD: u32,
    bitfields: u32,
    privDataSize: u32,
    reserved: u32,
    privData: *mut c_void,
    encodeConfig: *mut c_void,
    maxEncodeWidth: u32,
    maxEncodeHeight: u32,
    maxMEHintCountsPerBlock: [NvencExternalMeHintCountsPerBlock; 2],
    tuningInfo: u32,
    bufferFormat: u32,
    numStateBuffers: u32,
    outputStatsLevel: u32,
    reserved1: [u32; 284],
    reserved2: [*mut c_void; 64],
}

#[cfg(windows)]
#[repr(C)]
struct NvEncCreateBitstreamBuffer {
    version: u32,
    size: u32,
    memoryHeap: u32,
    reserved: u32,
    bitstreamBuffer: *mut c_void,
    bitstreamBufferPtr: *mut c_void,
    reserved1: [u32; 58],
    reserved2: [*mut c_void; 64],
}

#[cfg(windows)]
#[repr(C)]
struct NvEncRegisterResource {
    version: u32,
    resourceType: u32,
    width: u32,
    height: u32,
    pitch: u32,
    subResourceIndex: u32,
    resourceToRegister: *mut c_void,
    registeredResource: *mut c_void,
    bufferFormat: u32,
    bufferUsage: u32,
    pInputFencePoint: *mut c_void,
    chromaOffset: [u32; 2],
    chromaOffsetIn: [u32; 2],
    reserved1: [u32; 244],
    reserved2: [*mut c_void; 61],
}

#[cfg(windows)]
#[repr(C)]
struct NvEncMapInputResource {
    version: u32,
    subResourceIndex: u32,
    inputResource: *mut c_void,
    registeredResource: *mut c_void,
    mappedResource: *mut c_void,
    mappedBufferFmt: u32,
    // 足够覆盖 reserved1/reserved2；只读取 mappedResource/mappedBufferFmt。
    opaque_tail: [usize; 512],
}

#[cfg(windows)]
#[repr(C)]
struct NvEncPicParams {
    version: u32,
    inputWidth: u32,
    inputHeight: u32,
    inputPitch: u32,
    encodePicFlags: u32,
    frameIdx: u32,
    inputTimeStamp: u64,
    inputDuration: u64,
    inputBuffer: *mut c_void,
    outputBitstream: *mut c_void,
    completionEvent: *mut c_void,
    bufferFmt: u32,
    pictureStruct: u32,
    pictureType: u32,
    // codecPicParams 及后续 reserved/meHint/qp/recon 字段保持 0。用 8-byte 对齐
    // tail 覆盖真实 SDK 结构剩余部分，避免手写不相关 codec union。
    opaque_tail: [usize; 768],
}

#[cfg(windows)]
#[repr(C)]
struct NvEncLockBitstream {
    version: u32,
    bitfields: u32,
    outputBitstream: *mut c_void,
    sliceOffsets: *mut u32,
    frameIdx: u32,
    hwEncodeStatus: u32,
    numSlices: u32,
    bitstreamSizeInBytes: u32,
    outputTimeStamp: u64,
    outputDuration: u64,
    bitstreamBufferPtr: *mut c_void,
    // 只读取 bitstreamSizeInBytes/bitstreamBufferPtr，后续输出统计字段保持空间。
    opaque_tail: [usize; 512],
}

#[cfg(windows)]
#[repr(C)]
struct NvEncodeApiFunctionList {
    version: u32,
    reserved: u32,
    nvEncOpenEncodeSession: *const c_void,
    nvEncGetEncodeGUIDCount: Option<unsafe extern "system" fn(*mut c_void, *mut u32) -> i32>,
    nvEncGetEncodeProfileGUIDCount:
        Option<unsafe extern "system" fn(*mut c_void, windows::core::GUID, *mut u32) -> i32>,
    nvEncGetEncodeProfileGUIDs: Option<
        unsafe extern "system" fn(
            *mut c_void,
            windows::core::GUID,
            *mut windows::core::GUID,
            u32,
            *mut u32,
        ) -> i32,
    >,
    nvEncGetEncodeGUIDs: Option<
        unsafe extern "system" fn(*mut c_void, *mut windows::core::GUID, u32, *mut u32) -> i32,
    >,
    nvEncGetInputFormatCount:
        Option<unsafe extern "system" fn(*mut c_void, windows::core::GUID, *mut u32) -> i32>,
    nvEncGetInputFormats: Option<
        unsafe extern "system" fn(*mut c_void, windows::core::GUID, *mut u32, u32, *mut u32) -> i32,
    >,
    nvEncGetEncodeCaps: Option<
        unsafe extern "system" fn(
            *mut c_void,
            windows::core::GUID,
            *mut NvEncCapsParam,
            *mut i32,
        ) -> i32,
    >,
    nvEncGetEncodePresetCount:
        Option<unsafe extern "system" fn(*mut c_void, windows::core::GUID, *mut u32) -> i32>,
    nvEncGetEncodePresetGUIDs: Option<
        unsafe extern "system" fn(
            *mut c_void,
            windows::core::GUID,
            *mut windows::core::GUID,
            u32,
            *mut u32,
        ) -> i32,
    >,
    nvEncGetEncodePresetConfig: *const c_void,
    nvEncInitializeEncoder:
        Option<unsafe extern "system" fn(*mut c_void, *mut NvEncInitializeParams) -> i32>,
    nvEncCreateInputBuffer: *const c_void,
    nvEncDestroyInputBuffer: *const c_void,
    nvEncCreateBitstreamBuffer:
        Option<unsafe extern "system" fn(*mut c_void, *mut NvEncCreateBitstreamBuffer) -> i32>,
    nvEncDestroyBitstreamBuffer: Option<unsafe extern "system" fn(*mut c_void, *mut c_void) -> i32>,
    nvEncEncodePicture: Option<unsafe extern "system" fn(*mut c_void, *mut NvEncPicParams) -> i32>,
    nvEncLockBitstream:
        Option<unsafe extern "system" fn(*mut c_void, *mut NvEncLockBitstream) -> i32>,
    nvEncUnlockBitstream: Option<unsafe extern "system" fn(*mut c_void, *mut c_void) -> i32>,
    nvEncLockInputBuffer: *const c_void,
    nvEncUnlockInputBuffer: *const c_void,
    nvEncGetEncodeStats: *const c_void,
    nvEncGetSequenceParams: *const c_void,
    nvEncRegisterAsyncEvent: *const c_void,
    nvEncUnregisterAsyncEvent: *const c_void,
    nvEncMapInputResource:
        Option<unsafe extern "system" fn(*mut c_void, *mut NvEncMapInputResource) -> i32>,
    nvEncUnmapInputResource: Option<unsafe extern "system" fn(*mut c_void, *mut c_void) -> i32>,
    nvEncDestroyEncoder: Option<unsafe extern "system" fn(*mut c_void) -> i32>,
    nvEncInvalidateRefFrames: *const c_void,
    nvEncOpenEncodeSessionEx: Option<
        unsafe extern "system" fn(*mut NvEncOpenEncodeSessionExParams, *mut *mut c_void) -> i32,
    >,
    nvEncRegisterResource:
        Option<unsafe extern "system" fn(*mut c_void, *mut NvEncRegisterResource) -> i32>,
    nvEncUnregisterResource: Option<unsafe extern "system" fn(*mut c_void, *mut c_void) -> i32>,
    nvEncReconfigureEncoder: *const c_void,
    reserved1: *const c_void,
    nvEncCreateMVBuffer: *const c_void,
    nvEncDestroyMVBuffer: *const c_void,
    nvEncRunMotionEstimationOnly: *const c_void,
    nvEncGetLastErrorString: *const c_void,
    nvEncSetIOCudaStreams: *const c_void,
    nvEncGetEncodePresetConfigEx: Option<
        unsafe extern "system" fn(
            *mut c_void,
            windows::core::GUID,
            windows::core::GUID,
            u32,
            *mut NvEncPresetConfig,
        ) -> i32,
    >,
    nvEncGetSequenceParamEx: *const c_void,
    nvEncRestoreEncoderState: *const c_void,
    nvEncLookaheadPicture: *const c_void,
    reserved2: [*const c_void; 275],
}

#[cfg(windows)]
impl NvEncodeApiFunctionList {
    fn zeroed() -> Self {
        unsafe { std::mem::zeroed() }
    }
}

#[cfg(windows)]
fn nvenc_check(func: &'static str, status: i32) -> Result<(), BackendError> {
    if status == NV_ENC_SUCCESS {
        Ok(())
    } else {
        Err(BackendError::unsupported(
            "NVENC FFI",
            func,
            format!("NVENCSTATUS={status}"),
        ))
    }
}

#[cfg(windows)]
fn nvenc_missing(func: &'static str) -> BackendError {
    BackendError::unsupported("NVENC FFI", func, "function list 未返回该函数指针")
}

fn buffer_format_name(format: u32) -> &'static str {
    match format {
        NV_ENC_BUFFER_FORMAT_NV12 => "NV12",
        NV_ENC_BUFFER_FORMAT_YUV420_10BIT => "P010",
        NV_ENC_BUFFER_FORMAT_NV16 => "NV16",
        NV_ENC_BUFFER_FORMAT_P210 => "P210",
        NV_ENC_BUFFER_FORMAT_YUV444 => "YUV444",
        NV_ENC_BUFFER_FORMAT_YUV444_10BIT => "YUV444_10BIT",
        NV_ENC_BUFFER_FORMAT_ARGB => "ARGB",
        NV_ENC_BUFFER_FORMAT_ARGB10 => "ARGB10",
        NV_ENC_BUFFER_FORMAT_AYUV => "AYUV",
        NV_ENC_BUFFER_FORMAT_ABGR => "ABGR",
        NV_ENC_BUFFER_FORMAT_ABGR10 => "ABGR10",
        _ => "UNKNOWN",
    }
}

fn profile_name(guid: windows::core::GUID) -> &'static str {
    if guid == NV_ENC_HEVC_PROFILE_MAIN_GUID {
        "Main"
    } else if guid == NV_ENC_HEVC_PROFILE_MAIN10_GUID {
        "Main10"
    } else if guid == NV_ENC_HEVC_PROFILE_FREXT_GUID {
        "FRExt"
    } else {
        "Unknown"
    }
}

fn compiled_api_version_string() -> String {
    format!("{NVENCAPI_MAJOR_VERSION}.{NVENCAPI_MINOR_VERSION}")
}

fn nvenc_driver_version_string(value: u32) -> String {
    // NvEncodeAPIGetMaxSupportedVersion returns (major << 4) | minor.
    format!("{}.{}", value >> 4, value & 0x0f)
}

fn verify_hevc_vui_matches(
    annex_b: &[u8],
    expected: NclxColorMetadata,
) -> Result<(), BackendError> {
    let sps = find_hevc_annex_b_nal(annex_b, 33).ok_or_else(|| {
        BackendError::unsupported(
            "NVENC HEVC VUI",
            "首个 IDR access unit",
            "码流没有 SPS，无法确认动态色彩 range",
        )
    })?;
    let actual = parse_hevc_sps_vui(sps).map_err(|err| {
        BackendError::unsupported(
            "NVENC HEVC VUI",
            "SPS video_signal_type",
            format!("无法解析编码器输出的 VUI：{err}"),
        )
    })?;
    let actual = actual.ok_or_else(|| {
        BackendError::unsupported(
            "NVENC HEVC VUI",
            "SPS video_signal_type",
            "编码器输出未包含完整 range/colour_description，已阻止不一致 route",
        )
    })?;
    if actual != expected {
        return Err(BackendError::unsupported(
            "NVENC HEVC VUI",
            "动态 route 色彩元数据",
            format!(
                "编码器未保留请求值：requested={}/{}/{} range={} actual={}/{}/{} range={}，已阻止不一致 route",
                expected.colour_primaries,
                expected.transfer_characteristics,
                expected.matrix_coefficients,
                if expected.full_range {
                    "full"
                } else {
                    "limited"
                },
                actual.colour_primaries,
                actual.transfer_characteristics,
                actual.matrix_coefficients,
                if actual.full_range { "full" } else { "limited" },
            ),
        ));
    }
    Ok(())
}

fn find_hevc_annex_b_nal(data: &[u8], wanted_type: u8) -> Option<&[u8]> {
    let mut pos = 0usize;
    while let Some((start, code_len)) = find_annex_b_start_code(data, pos) {
        let nal_start = start + code_len;
        let next = find_annex_b_start_code(data, nal_start)
            .map(|(next_start, _)| next_start)
            .unwrap_or(data.len());
        pos = next;
        if nal_start + 2 <= next && ((data[nal_start] >> 1) & 0x3f) == wanted_type {
            let mut nal = &data[nal_start..next];
            while nal.last().copied() == Some(0) {
                nal = &nal[..nal.len() - 1];
            }
            return Some(nal);
        }
    }
    None
}

fn find_annex_b_start_code(data: &[u8], from: usize) -> Option<(usize, usize)> {
    let mut i = from;
    while i + 3 <= data.len() {
        if data[i..].starts_with(&[0, 0, 1]) {
            return Some((i, 3));
        }
        if data[i..].starts_with(&[0, 0, 0, 1]) {
            return Some((i, 4));
        }
        i += 1;
    }
    None
}

fn parse_hevc_sps_vui(sps_nal: &[u8]) -> Result<Option<NclxColorMetadata>, String> {
    if sps_nal.len() < 3 || ((sps_nal[0] >> 1) & 0x3f) != 33 {
        return Err("NAL unit 不是 HEVC SPS".to_owned());
    }
    let rbsp = hevc_ebsp_to_rbsp(&sps_nal[2..]);
    let mut bits = HevcBitReader::new(&rbsp);
    bits.skip(4)?; // sps_video_parameter_set_id
    let max_sub_layers_minus1 = bits.read_bits(3)? as usize;
    bits.skip(1)?; // sps_temporal_id_nesting_flag
    skip_hevc_profile_tier_level(&mut bits, max_sub_layers_minus1)?;
    bits.read_ue()?; // sps_seq_parameter_set_id
    let chroma_format_idc = bits.read_ue()?;
    if chroma_format_idc == 3 {
        bits.skip(1)?; // separate_colour_plane_flag
    }
    bits.read_ue()?; // pic_width_in_luma_samples
    bits.read_ue()?; // pic_height_in_luma_samples
    if bits.read_bit()? {
        for _ in 0..4 {
            bits.read_ue()?;
        }
    }
    bits.read_ue()?; // bit_depth_luma_minus8
    bits.read_ue()?; // bit_depth_chroma_minus8
    let log2_max_pic_order_cnt_lsb = bits.read_ue()?.saturating_add(4) as usize;
    let sub_layer_ordering_info_present = bits.read_bit()?;
    let first_layer = if sub_layer_ordering_info_present {
        0
    } else {
        max_sub_layers_minus1
    };
    for _ in first_layer..=max_sub_layers_minus1 {
        bits.read_ue()?;
        bits.read_ue()?;
        bits.read_ue()?;
    }
    for _ in 0..6 {
        bits.read_ue()?;
    }
    if bits.read_bit()? && bits.read_bit()? {
        skip_hevc_scaling_list_data(&mut bits)?;
    }
    bits.skip(1)?; // amp_enabled_flag
    bits.skip(1)?; // sample_adaptive_offset_enabled_flag
    if bits.read_bit()? {
        bits.skip(4 + 4)?;
        bits.read_ue()?;
        bits.read_ue()?;
        bits.skip(1)?;
    }
    let num_short_term_ref_pic_sets = bits.read_ue()? as usize;
    let mut num_delta_pocs = Vec::with_capacity(num_short_term_ref_pic_sets);
    for st_rps_idx in 0..num_short_term_ref_pic_sets {
        let count = parse_hevc_short_term_ref_pic_set(
            &mut bits,
            st_rps_idx,
            num_short_term_ref_pic_sets,
            &num_delta_pocs,
        )?;
        num_delta_pocs.push(count);
    }
    if bits.read_bit()? {
        let num_long_term_ref_pics_sps = bits.read_ue()? as usize;
        for _ in 0..num_long_term_ref_pics_sps {
            bits.skip(log2_max_pic_order_cnt_lsb)?;
            bits.skip(1)?;
        }
    }
    bits.skip(1)?; // sps_temporal_mvp_enabled_flag
    bits.skip(1)?; // strong_intra_smoothing_enabled_flag
    if !bits.read_bit()? {
        return Ok(None);
    }
    parse_hevc_vui_parameters(&mut bits)
}

fn parse_hevc_vui_parameters(
    bits: &mut HevcBitReader<'_>,
) -> Result<Option<NclxColorMetadata>, String> {
    if bits.read_bit()? {
        let aspect_ratio_idc = bits.read_bits(8)?;
        if aspect_ratio_idc == 255 {
            bits.skip(16 + 16)?;
        }
    }
    if bits.read_bit()? {
        bits.skip(1)?;
    }
    if !bits.read_bit()? {
        return Ok(None);
    }
    bits.skip(3)?; // video_format
    let full_range = bits.read_bit()?;
    if !bits.read_bit()? {
        return Ok(None);
    }
    Ok(Some(NclxColorMetadata {
        colour_primaries: bits.read_bits(8)? as u16,
        transfer_characteristics: bits.read_bits(8)? as u16,
        matrix_coefficients: bits.read_bits(8)? as u16,
        full_range,
    }))
}

fn skip_hevc_profile_tier_level(
    bits: &mut HevcBitReader<'_>,
    max_sub_layers_minus1: usize,
) -> Result<(), String> {
    bits.skip(96)?; // general profile/compatibility/constraints/level
    let mut profile_present = Vec::with_capacity(max_sub_layers_minus1);
    let mut level_present = Vec::with_capacity(max_sub_layers_minus1);
    for _ in 0..max_sub_layers_minus1 {
        profile_present.push(bits.read_bit()?);
        level_present.push(bits.read_bit()?);
    }
    if max_sub_layers_minus1 > 0 {
        bits.skip(2 * (8 - max_sub_layers_minus1))?;
    }
    for index in 0..max_sub_layers_minus1 {
        if profile_present[index] {
            bits.skip(88)?;
        }
        if level_present[index] {
            bits.skip(8)?;
        }
    }
    Ok(())
}

fn skip_hevc_scaling_list_data(bits: &mut HevcBitReader<'_>) -> Result<(), String> {
    for size_id in 0..4usize {
        let step = if size_id == 3 { 3 } else { 1 };
        for _matrix_id in (0..6usize).step_by(step) {
            if !bits.read_bit()? {
                bits.read_ue()?;
                continue;
            }
            let coef_num = 64usize.min(1usize << (4 + (size_id << 1)));
            if size_id > 1 {
                bits.read_se()?;
            }
            for _ in 0..coef_num {
                bits.read_se()?;
            }
        }
    }
    Ok(())
}

fn parse_hevc_short_term_ref_pic_set(
    bits: &mut HevcBitReader<'_>,
    st_rps_idx: usize,
    num_short_term_ref_pic_sets: usize,
    num_delta_pocs: &[usize],
) -> Result<usize, String> {
    let inter_ref_pic_set_prediction_flag = st_rps_idx != 0 && bits.read_bit()?;
    if inter_ref_pic_set_prediction_flag {
        let delta_idx_minus1 = if st_rps_idx == num_short_term_ref_pic_sets {
            bits.read_ue()? as usize
        } else {
            0
        };
        if delta_idx_minus1 >= st_rps_idx {
            return Err("SPS short-term RPS 引用了无效索引".to_owned());
        }
        let ref_rps_idx = st_rps_idx - (delta_idx_minus1 + 1);
        bits.skip(1)?; // delta_rps_sign
        bits.read_ue()?; // abs_delta_rps_minus1
        let mut count = 0usize;
        for _ in 0..=num_delta_pocs[ref_rps_idx] {
            let used_by_curr_pic_flag = bits.read_bit()?;
            let use_delta_flag = used_by_curr_pic_flag || bits.read_bit()?;
            if use_delta_flag {
                count += 1;
            }
        }
        Ok(count)
    } else {
        let num_negative_pics = bits.read_ue()? as usize;
        let num_positive_pics = bits.read_ue()? as usize;
        for _ in 0..num_negative_pics {
            bits.read_ue()?;
            bits.skip(1)?;
        }
        for _ in 0..num_positive_pics {
            bits.read_ue()?;
            bits.skip(1)?;
        }
        Ok(num_negative_pics + num_positive_pics)
    }
}

fn hevc_ebsp_to_rbsp(ebsp: &[u8]) -> Vec<u8> {
    let mut rbsp = Vec::with_capacity(ebsp.len());
    let mut zero_count = 0usize;
    for &byte in ebsp {
        if zero_count >= 2 && byte == 0x03 {
            continue;
        }
        rbsp.push(byte);
        zero_count = if byte == 0 { zero_count + 1 } else { 0 };
    }
    rbsp
}

struct HevcBitReader<'a> {
    data: &'a [u8],
    bit_offset: usize,
}

impl<'a> HevcBitReader<'a> {
    fn new(data: &'a [u8]) -> Self {
        Self {
            data,
            bit_offset: 0,
        }
    }

    fn read_bit(&mut self) -> Result<bool, String> {
        if self.bit_offset >= self.data.len().saturating_mul(8) {
            return Err("SPS RBSP 提前结束".to_owned());
        }
        let byte = self.data[self.bit_offset / 8];
        let shift = 7 - (self.bit_offset % 8);
        self.bit_offset += 1;
        Ok(((byte >> shift) & 1) != 0)
    }

    fn read_bits(&mut self, count: usize) -> Result<u32, String> {
        if count > 32 {
            return Err(format!("一次读取的 SPS 位数过大：{count}"));
        }
        let mut value = 0u32;
        for _ in 0..count {
            value = (value << 1) | u32::from(self.read_bit()?);
        }
        Ok(value)
    }

    fn skip(&mut self, count: usize) -> Result<(), String> {
        let end = self
            .bit_offset
            .checked_add(count)
            .ok_or_else(|| "SPS 位偏移溢出".to_owned())?;
        if end > self.data.len().saturating_mul(8) {
            return Err("SPS RBSP 提前结束".to_owned());
        }
        self.bit_offset = end;
        Ok(())
    }

    fn read_ue(&mut self) -> Result<u32, String> {
        let mut leading_zero_bits = 0usize;
        while !self.read_bit()? {
            leading_zero_bits += 1;
            if leading_zero_bits > 31 {
                return Err("SPS Exp-Golomb 值过大".to_owned());
            }
        }
        if leading_zero_bits == 0 {
            return Ok(0);
        }
        let suffix = self.read_bits(leading_zero_bits)?;
        Ok(((1u32 << leading_zero_bits) - 1).saturating_add(suffix))
    }

    fn read_se(&mut self) -> Result<i32, String> {
        let code_num = self.read_ue()?;
        if code_num & 1 == 0 {
            Ok(-((code_num / 2) as i32))
        } else {
            Ok(code_num.div_ceil(2) as i32)
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    struct TestBitWriter {
        bytes: Vec<u8>,
        current: u8,
        used: u8,
    }

    impl TestBitWriter {
        fn new() -> Self {
            Self {
                bytes: Vec::new(),
                current: 0,
                used: 0,
            }
        }

        fn bit(&mut self, value: bool) {
            self.current = (self.current << 1) | u8::from(value);
            self.used += 1;
            if self.used == 8 {
                self.bytes.push(self.current);
                self.current = 0;
                self.used = 0;
            }
        }

        fn bits(&mut self, value: u32, count: usize) {
            for shift in (0..count).rev() {
                self.bit(((value >> shift) & 1) != 0);
            }
        }

        fn ue(&mut self, value: u32) {
            let code_num = value + 1;
            let bits = 32 - code_num.leading_zeros();
            for _ in 1..bits {
                self.bit(false);
            }
            self.bits(code_num, bits as usize);
        }

        fn finish(mut self) -> Vec<u8> {
            if self.used != 0 {
                self.current <<= 8 - self.used;
                self.bytes.push(self.current);
            }
            self.bytes
        }
    }

    fn synthetic_hevc_sps(color: NclxColorMetadata) -> Vec<u8> {
        let mut bits = TestBitWriter::new();
        bits.bits(0, 4); // sps_video_parameter_set_id
        bits.bits(0, 3); // sps_max_sub_layers_minus1
        bits.bit(true); // sps_temporal_id_nesting_flag
        bits.bits(0, 32);
        bits.bits(0, 32);
        bits.bits(0, 32); // profile_tier_level
        bits.ue(0); // sps_seq_parameter_set_id
        bits.ue(1); // chroma_format_idc 4:2:0
        bits.ue(1280);
        bits.ue(720);
        bits.bit(false); // conformance_window_flag
        bits.ue(2); // 10-bit luma
        bits.ue(2); // 10-bit chroma
        bits.ue(4); // log2_max_pic_order_cnt_lsb_minus4
        bits.bit(false); // sps_sub_layer_ordering_info_present_flag
        bits.ue(0);
        bits.ue(0);
        bits.ue(0);
        for _ in 0..6 {
            bits.ue(0);
        }
        bits.bit(false); // scaling_list_enabled_flag
        bits.bit(false); // amp_enabled_flag
        bits.bit(false); // sample_adaptive_offset_enabled_flag
        bits.bit(false); // pcm_enabled_flag
        bits.ue(0); // num_short_term_ref_pic_sets
        bits.bit(false); // long_term_ref_pics_present_flag
        bits.bit(false); // sps_temporal_mvp_enabled_flag
        bits.bit(false); // strong_intra_smoothing_enabled_flag
        bits.bit(true); // vui_parameters_present_flag
        bits.bit(false); // aspect_ratio_info_present_flag
        bits.bit(false); // overscan_info_present_flag
        bits.bit(true); // video_signal_type_present_flag
        bits.bits(NV_ENC_VUI_VIDEO_FORMAT_UNSPECIFIED, 3);
        bits.bit(color.full_range);
        bits.bit(true); // colour_description_present_flag
        bits.bits(u32::from(color.colour_primaries), 8);
        bits.bits(u32::from(color.transfer_characteristics), 8);
        bits.bits(u32::from(color.matrix_coefficients), 8);
        bits.bit(true); // rbsp_stop_one_bit
        let rbsp = bits.finish();

        let mut nal = vec![0x42, 0x01]; // nal_unit_type=33, temporal_id_plus1=1
        let mut zero_count = 0usize;
        for byte in rbsp {
            if zero_count >= 2 && byte <= 3 {
                nal.push(3);
                zero_count = 0;
            }
            nal.push(byte);
            zero_count = if byte == 0 { zero_count + 1 } else { 0 };
        }
        nal
    }

    #[cfg(windows)]
    unsafe fn read_u32(config: &NvEncConfigOpaque, offset: usize) -> u32 {
        ptr::read_unaligned(config.bytes.as_ptr().add(offset) as *const u32)
    }

    #[cfg(windows)]
    unsafe fn read_u16(config: &NvEncConfigOpaque, offset: usize) -> u16 {
        ptr::read_unaligned(config.bytes.as_ptr().add(offset) as *const u16)
    }

    #[cfg(windows)]
    unsafe fn read_u8(config: &NvEncConfigOpaque, offset: usize) -> u8 {
        ptr::read_unaligned(config.bytes.as_ptr().add(offset))
    }

    #[test]
    fn driver_api_version_encoding_is_distinct_from_struct_version_encoding() {
        assert_eq!(NVENCAPI_DRIVER_VERSION, (13 << 4) | 1);
        assert_ne!(NVENCAPI_DRIVER_VERSION, NVENCAPI_VERSION);
    }

    #[test]
    fn hevc_sps_vui_parser_preserves_dynamic_range() {
        for full_range in [false, true] {
            let color = NclxColorMetadata::bt2020_pq(full_range);
            let sps = synthetic_hevc_sps(color);
            assert_eq!(parse_hevc_sps_vui(&sps).unwrap(), Some(color));

            let mut annex_b = vec![0, 0, 0, 1];
            annex_b.extend_from_slice(&sps);
            verify_hevc_vui_matches(&annex_b, color).unwrap();
            assert!(
                verify_hevc_vui_matches(&annex_b, NclxColorMetadata::bt2020_pq(!full_range))
                    .is_err()
            );
        }
    }

    #[test]
    fn rc_mask_maps_to_shared_gui_methods() {
        let methods = rate_controls_from_mask(
            NV_ENC_PARAMS_RC_CONSTQP as i32
                | NV_ENC_PARAMS_RC_VBR as i32
                | NV_ENC_PARAMS_RC_CBR as i32,
        );
        assert_eq!(
            methods,
            vec![
                RateControlMethod::Cbr,
                RateControlMethod::Vbr,
                RateControlMethod::Cqp
            ]
        );
        assert_eq!(rate_controls_from_mask(0), vec![RateControlMethod::Cqp]);
    }

    #[test]
    #[cfg(windows)]
    fn nvenc_config_writes_cbr_rate_control_fields() {
        let cfg = RateControlConfig {
            method: RateControlMethod::Cbr,
            target_kbps: 50_000,
            buffer_size_kb: 4_096,
            initial_delay_kb: 2_048,
            look_ahead_depth: 12,
            ..Default::default()
        };
        let config = unsafe {
            make_low_latency_hevc_config(
                NvencD3d11InputFormat::P010,
                NclxColorMetadata::bt2020_pq(true),
                &cfg,
            )
            .unwrap()
        };
        let rc = NV_ENC_CONFIG_RC_PARAMS_OFFSET;
        unsafe {
            assert_eq!(read_u32(&config, rc), NV_ENC_RC_PARAMS_VER);
            assert_eq!(
                read_u32(&config, rc + NV_ENC_RC_PARAMS_RATE_CONTROL_MODE_OFFSET),
                NV_ENC_PARAMS_RC_CBR
            );
            assert_eq!(
                read_u32(&config, rc + NV_ENC_RC_PARAMS_AVERAGE_BIT_RATE_OFFSET),
                50_000_000
            );
            assert_eq!(
                read_u32(&config, rc + NV_ENC_RC_PARAMS_VBV_BUFFER_SIZE_OFFSET),
                4_096 * 1024 * 8
            );
            assert_eq!(
                read_u32(&config, rc + NV_ENC_RC_PARAMS_VBV_INITIAL_DELAY_OFFSET),
                2_048 * 1024 * 8
            );
            let bitfields = read_u32(&config, rc + NV_ENC_RC_PARAMS_BITFIELDS_OFFSET);
            assert_ne!(bitfields & NV_ENC_RC_PARAMS_BIT_ENABLE_LOOKAHEAD, 0);
            assert_ne!(bitfields & NV_ENC_RC_PARAMS_BIT_ZERO_REORDER_DELAY, 0);
            assert_eq!(
                read_u16(&config, rc + NV_ENC_RC_PARAMS_LOOKAHEAD_DEPTH_OFFSET),
                12
            );
        }
    }

    #[test]
    #[cfg(windows)]
    fn nvenc_config_writes_dynamic_full_and_limited_vui() {
        let rate_control = RateControlConfig::default();
        let full_color = NclxColorMetadata::bt2020_pq(true);
        let limited_color = NclxColorMetadata::bt2020_pq(false);
        let full = unsafe {
            make_low_latency_hevc_config(NvencD3d11InputFormat::P010, full_color, &rate_control)
                .unwrap()
        };
        let limited = unsafe {
            make_low_latency_hevc_config(NvencD3d11InputFormat::P010, limited_color, &rate_control)
                .unwrap()
        };

        unsafe {
            for config in [&full, &limited] {
                assert_eq!(
                    read_u32(config, NV_ENC_CONFIG_HEVC_VUI_VIDEO_SIGNAL_PRESENT_OFFSET),
                    1
                );
                assert_eq!(
                    read_u32(config, NV_ENC_CONFIG_HEVC_VUI_VIDEO_FORMAT_OFFSET),
                    NV_ENC_VUI_VIDEO_FORMAT_UNSPECIFIED
                );
                assert_eq!(
                    read_u32(
                        config,
                        NV_ENC_CONFIG_HEVC_VUI_COLOUR_DESCRIPTION_PRESENT_OFFSET
                    ),
                    1
                );
                assert_eq!(
                    read_u32(config, NV_ENC_CONFIG_HEVC_VUI_COLOUR_PRIMARIES_OFFSET),
                    9
                );
                assert_eq!(
                    read_u32(
                        config,
                        NV_ENC_CONFIG_HEVC_VUI_TRANSFER_CHARACTERISTICS_OFFSET
                    ),
                    16
                );
                assert_eq!(
                    read_u32(config, NV_ENC_CONFIG_HEVC_VUI_MATRIX_COEFFICIENTS_OFFSET),
                    9
                );
            }
            assert_eq!(read_u32(&full, NV_ENC_CONFIG_HEVC_VUI_FULL_RANGE_OFFSET), 1);
            assert_eq!(
                read_u32(&limited, NV_ENC_CONFIG_HEVC_VUI_FULL_RANGE_OFFSET),
                0
            );
        }
    }

    #[test]
    #[cfg(windows)]
    fn nvenc_config_writes_cqp_qp_order() {
        let cfg = RateControlConfig {
            method: RateControlMethod::Cqp,
            qpi: 21,
            qpp: 23,
            qpb: 25,
            ..Default::default()
        };
        let config = unsafe {
            make_low_latency_hevc_config(
                NvencD3d11InputFormat::Nv12,
                NclxColorMetadata::bt709(false),
                &cfg,
            )
            .unwrap()
        };
        let rc = NV_ENC_CONFIG_RC_PARAMS_OFFSET + NV_ENC_RC_PARAMS_CONST_QP_OFFSET;
        unsafe {
            assert_eq!(read_u32(&config, rc), 23);
            assert_eq!(read_u32(&config, rc + 4), 25);
            assert_eq!(read_u32(&config, rc + 8), 21);
        }
    }

    #[test]
    #[cfg(windows)]
    fn nvenc_config_writes_ayuv_rext_chroma() {
        let config = unsafe {
            make_low_latency_hevc_config(
                NvencD3d11InputFormat::Ayuv,
                NclxColorMetadata::bt709(true),
                &RateControlConfig::default(),
            )
            .unwrap()
        };
        unsafe {
            let flags = read_u32(&config, NV_ENC_CONFIG_HEVC_FLAGS_OFFSET);
            assert_eq!((flags >> 9) & 0x03, 3);
            assert_eq!(
                read_u32(&config, NV_ENC_CONFIG_HEVC_OUTPUT_BIT_DEPTH_OFFSET),
                NV_ENC_BIT_DEPTH_8
            );
        }
    }

    #[test]
    #[cfg(windows)]
    fn nvenc_config_writes_spatial_aq_multipass_and_vbr_target_quality() {
        let cfg = RateControlConfig {
            method: RateControlMethod::Vbr,
            nvenc_spatial_aq: true,
            nvenc_temporal_aq: true,
            nvenc_aq_strength: 9,
            nvenc_vbr_target_quality: 27,
            nvenc_multi_pass: crate::rate_control::NvencMultiPass::FullResolution,
            ..Default::default()
        };
        let config = unsafe {
            make_low_latency_hevc_config(
                NvencD3d11InputFormat::P010,
                NclxColorMetadata::bt2020_pq(false),
                &cfg,
            )
            .unwrap()
        };
        let rc = NV_ENC_CONFIG_RC_PARAMS_OFFSET;
        unsafe {
            let bitfields = read_u32(&config, rc + NV_ENC_RC_PARAMS_BITFIELDS_OFFSET);
            assert_ne!(bitfields & NV_ENC_RC_PARAMS_BIT_ENABLE_AQ, 0);
            assert_eq!(bitfields & NV_ENC_RC_PARAMS_BIT_ENABLE_TEMPORAL_AQ, 0);
            assert_eq!((bitfields >> NV_ENC_RC_PARAMS_AQ_STRENGTH_SHIFT) & 0x0f, 0);
            assert_eq!(
                read_u8(&config, rc + NV_ENC_RC_PARAMS_TARGET_QUALITY_OFFSET),
                27
            );
            assert_eq!(
                read_u32(&config, rc + NV_ENC_RC_PARAMS_MULTI_PASS_OFFSET),
                2
            );
        }
    }

    #[test]
    #[cfg(windows)]
    fn nvenc_rate_control_preserves_preset_owned_fields() {
        let rc = NV_ENC_CONFIG_RC_PARAMS_OFFSET;
        let mut base = NvEncConfigOpaque::zeroed();
        unsafe {
            base.write_u8(rc + NV_ENC_RC_PARAMS_LOW_DELAY_KEY_FRAME_SCALE_OFFSET, 7);
            base.write_u8(rc + NV_ENC_RC_PARAMS_TARGET_QUALITY_LSB_OFFSET, 255);
            base.write_u32(
                rc + NV_ENC_RC_PARAMS_BITFIELDS_OFFSET,
                1 << 11, // strictGOPTarget: not owned by the GUI mapping.
            );
        }
        let cfg = RateControlConfig {
            method: RateControlMethod::Vbr,
            nvenc_vbr_target_quality: 23,
            ..Default::default()
        };
        let config = unsafe {
            make_low_latency_hevc_config_from_base(
                base,
                NvencD3d11InputFormat::P010,
                NclxColorMetadata::bt2020_pq(false),
                &cfg,
            )
            .unwrap()
        };

        unsafe {
            assert_eq!(
                read_u8(
                    &config,
                    rc + NV_ENC_RC_PARAMS_LOW_DELAY_KEY_FRAME_SCALE_OFFSET
                ),
                7
            );
            assert_eq!(
                read_u8(&config, rc + NV_ENC_RC_PARAMS_TARGET_QUALITY_LSB_OFFSET),
                0
            );
            assert_ne!(
                read_u32(&config, rc + NV_ENC_RC_PARAMS_BITFIELDS_OFFSET) & (1 << 11),
                0
            );
        }
    }

    #[test]
    fn nvenc_rate_control_feature_matrix_exposes_implemented_lookahead() {
        let caps = NvencCapsInfo {
            lookahead: Some(true),
            temporal_aq: Some(true),
            ..Default::default()
        };
        let features = nvenc_rate_control_features(
            &[
                RateControlMethod::Cbr,
                RateControlMethod::Vbr,
                RateControlMethod::Cqp,
            ],
            &caps,
        );
        let cbr = features
            .iter()
            .find(|feature| feature.method == RateControlMethod::Cbr)
            .unwrap();
        assert!(cbr.lookahead);
        assert!(cbr.vbv);
        assert!(cbr.spatial_aq);
        assert!(!cbr.target_quality);

        let vbr = features
            .iter()
            .find(|feature| feature.method == RateControlMethod::Vbr)
            .unwrap();
        assert!(vbr.lookahead);
        assert!(vbr.vbv);
        assert!(vbr.target_quality);

        let cqp = features
            .iter()
            .find(|feature| feature.method == RateControlMethod::Cqp)
            .unwrap();
        assert!(!cqp.lookahead);
        assert!(!cqp.vbv);
        assert!(!cqp.target_quality);
    }

    #[test]
    fn nvenc_preset_guids_roundtrip() {
        for preset in NvencPreset::all() {
            assert_eq!(
                nvenc_preset_from_guid(nvenc_preset_guid(preset)),
                Some(preset)
            );
        }
        assert_eq!(
            nvenc_preset_from_guid(windows::core::GUID::from_u128(0)),
            None
        );
    }

    #[test]
    #[cfg(windows)]
    fn split_encode_mode_raw_values_land_in_initialize_bitfield() {
        for (mode, raw) in [
            (NvencSplitEncodeMode::Auto, 0),
            (NvencSplitEncodeMode::AutoForced, 1),
            (NvencSplitEncodeMode::TwoForced, 2),
            (NvencSplitEncodeMode::ThreeForced, 3),
            (NvencSplitEncodeMode::FourForced, 4),
            (NvencSplitEncodeMode::Disabled, 15),
        ] {
            let bitfields = split_encode_initialize_bitfields(mode);
            assert_eq!(
                (bitfields & NV_ENC_INITIALIZE_SPLIT_MODE_MASK)
                    >> NV_ENC_INITIALIZE_SPLIT_MODE_SHIFT,
                raw
            );
            assert_eq!(bitfields & !NV_ENC_INITIALIZE_SPLIT_MODE_MASK, 0);
        }
    }

    #[test]
    #[cfg(windows)]
    fn nvenc_tuning_ffi_layout_matches_sdk_13_1() {
        assert_eq!(std::mem::size_of::<NvEncConfigOpaque>(), 3_584);
        assert_eq!(std::mem::align_of::<NvEncConfigOpaque>(), 8);
        assert_eq!(std::mem::size_of::<NvEncPresetConfig>(), 5_128);
        assert_eq!(std::mem::offset_of!(NvEncPresetConfig, presetCfg), 8);
        assert_eq!(std::mem::offset_of!(NvEncPresetConfig, reserved1), 3_592);
        assert_eq!(std::mem::offset_of!(NvEncPresetConfig, reserved2), 4_616);

        assert_eq!(std::mem::size_of::<NvEncInitializeParams>(), 1_800);
        assert_eq!(std::mem::offset_of!(NvEncInitializeParams, bitfields), 68);
        assert_eq!(
            std::mem::offset_of!(NvEncInitializeParams, encodeConfig),
            88
        );
        assert_eq!(std::mem::offset_of!(NvEncInitializeParams, tuningInfo), 136);
        assert_eq!(std::mem::offset_of!(NvEncInitializeParams, reserved1), 152);
    }

    #[test]
    fn known_formats_are_named_for_route_probe() {
        assert_eq!(buffer_format_name(NV_ENC_BUFFER_FORMAT_NV12), "NV12");
        assert_eq!(
            buffer_format_name(NV_ENC_BUFFER_FORMAT_YUV420_10BIT),
            "P010"
        );
        assert_eq!(buffer_format_name(NV_ENC_BUFFER_FORMAT_NV16), "NV16");
        assert_eq!(buffer_format_name(NV_ENC_BUFFER_FORMAT_P210), "P210");
    }

    #[test]
    fn nvenc_lookahead_depth_uses_route_surface_budget() {
        let route = |width: i32, height: i32, input_format: &str| NvencCurrentDisplayRouteInfo {
            adapter_index: 0,
            adapter_luid: String::new(),
            output_index: 0,
            rotation: 1,
            color_space: 0,
            bits_per_color: 10,
            desktop_left: 0,
            desktop_top: 0,
            desktop_right: width,
            desktop_bottom: height,
            chroma: ChromaSampling::Yuv444,
            input_format: input_format.to_owned(),
            bit_depth: 10,
            profile: "FRExt".to_owned(),
            nclx_colour_primaries: 9,
            nclx_transfer_characteristics: 16,
            nclx_matrix_coefficients: 9,
            nclx_full_range: true,
            route_summary: String::new(),
            note: String::new(),
        };

        assert_eq!(
            lookahead_depth_max_for_current_display_route(&route(3_840, 2_160, "YUV444_10BIT")),
            31
        );
        assert_eq!(
            lookahead_depth_max_for_current_display_route(&route(7_680, 4_320, "YUV444_10BIT")),
            15
        );
        assert_eq!(
            lookahead_depth_max_for_current_display_route(&route(7_680, 4_320, "P010")),
            15
        );
    }

    #[test]
    #[ignore = "需要本机 NVIDIA 驱动和 D3D11 桌面会话；手动用于验证 NVENC FFI 探测"]
    fn local_nvenc_probe_smoke() {
        let adapters = crate::backend::dxgi::enumerate_adapters().unwrap_or_default();
        let probe = probe_nvenc_adapters(&adapters);
        println!("{}", serde_json::to_string_pretty(&probe).unwrap());
        assert!(probe.available, "本机应能加载 nvEncodeAPI64.dll");
        assert!(probe.hevc_supported, "本机应能报告 NVENC HEVC");
    }

    #[test]
    #[ignore = "需要本机 NVIDIA 驱动和 D3D11 桌面会话；手动用于验证 NVENC registered-resource 编码链路"]
    fn local_nvenc_d3d11_encode_smoke() {
        let adapters = crate::backend::dxgi::enumerate_adapters().unwrap_or_default();
        let adapter_index = adapters
            .iter()
            .find(|adapter| adapter.vendor_id == NVIDIA_VENDOR_ID && adapter.flags & 0x2 == 0)
            .map(|adapter| adapter.index)
            .expect("本机需要至少一个 NVIDIA 显示 adapter");
        let report = local_d3d11_encode_smoke(adapter_index).unwrap();
        println!("{}", serde_json::to_string_pretty(&report).unwrap());
        assert!(report.output_bytes > 0, "NVENC 应输出非空 HEVC bitstream");
        assert!(
            report.annex_b_start_code_seen,
            "NVENC HEVC 输出应包含 Annex-B start code"
        );
    }

    #[test]
    #[ignore = "需要本机 NVIDIA 驱动和 D3D11 桌面会话；手动用于验证 NVENC P010 registered-resource 编码链路"]
    fn local_nvenc_d3d11_p010_encode_smoke() {
        let adapters = crate::backend::dxgi::enumerate_adapters().unwrap_or_default();
        let adapter_index = adapters
            .iter()
            .find(|adapter| adapter.vendor_id == NVIDIA_VENDOR_ID && adapter.flags & 0x2 == 0)
            .map(|adapter| adapter.index)
            .expect("本机需要至少一个 NVIDIA 显示 adapter");
        unsafe {
            let width = 1280u32;
            let height = 720u32;
            let input_format = NvencD3d11InputFormat::P010;
            let color = current_display_color_for_adapter(adapter_index).unwrap();
            let mut encoder =
                NvencD3d11Encoder::open(adapter_index, width, height, input_format, color).unwrap();
            let texture =
                create_synthetic_input_texture(encoder.device(), width, height, input_format)
                    .unwrap();
            let sample = encoder.encode_texture(&texture, 0, true, false).unwrap();
            assert_eq!(encoder.registered_inputs.len(), 1);
            let second = encoder.encode_texture(&texture, 375, false, false).unwrap();
            assert!(!second.data.is_empty());
            assert_eq!(encoder.registered_inputs.len(), 1);
            let second_texture =
                create_synthetic_input_texture(encoder.device(), width, height, input_format)
                    .unwrap();
            let third = encoder
                .encode_texture(&second_texture, 750, false, false)
                .unwrap();
            assert!(!third.data.is_empty());
            assert_eq!(encoder.registered_inputs.len(), 2);
            println!(
                "{}",
                serde_json::to_string_pretty(&NvencD3d11EncodeSmokeReport {
                    adapter_index,
                    width,
                    height,
                    input_format: input_format.label().to_owned(),
                    output_bytes: sample.data.len(),
                    annex_b_start_code_seen: sample
                        .data
                        .windows(4)
                        .any(|window| window == [0x00, 0x00, 0x00, 0x01]),
                })
                .unwrap()
            );
            assert!(!sample.data.is_empty());
        }
    }

    #[test]
    #[ignore = "需要本机支持 Lookahead 的 NVIDIA 驱动；验证 NEED_MORE_INPUT、EOS drain 与 VFR 时间戳顺序"]
    fn local_nvenc_lookahead_delayed_output_smoke() {
        let adapters = crate::backend::dxgi::enumerate_adapters().unwrap_or_default();
        let adapter_index = adapters
            .iter()
            .find(|adapter| adapter.vendor_id == NVIDIA_VENDOR_ID && adapter.flags & 0x2 == 0)
            .map(|adapter| adapter.index)
            .expect("本机需要至少一个 NVIDIA 显示 adapter");

        unsafe {
            let width = 1_280;
            let height = 720;
            let input_format = NvencD3d11InputFormat::P010;
            let color = current_display_color_for_adapter(adapter_index).unwrap();
            let rate_control = RateControlConfig {
                method: RateControlMethod::Cbr,
                target_kbps: 20_000,
                buffer_size_kb: 2_000,
                initial_delay_kb: 1_000,
                look_ahead_depth: 8,
                ..Default::default()
            };
            let mut encoder = NvencD3d11Encoder::open_with_rate_control(
                adapter_index,
                width,
                height,
                input_format,
                color,
                &rate_control,
                240,
                1,
            )
            .unwrap();
            let textures = (0..40)
                .map(|_| {
                    create_synthetic_input_texture(encoder.device(), width, height, input_format)
                        .unwrap()
                })
                .collect::<Vec<_>>();
            let timestamps = (0..textures.len())
                .map(|index| (index as u64).saturating_mul(563))
                .collect::<Vec<_>>();
            let mut outputs = Vec::new();
            let mut delayed_submits = 0usize;
            for (index, texture) in textures.iter().enumerate() {
                let ready = encoder
                    .submit_texture(texture, timestamps[index], index == 0, false)
                    .unwrap();
                delayed_submits += usize::from(ready.is_empty());
                outputs.extend(ready);
            }
            outputs.extend(encoder.flush().unwrap());

            assert!(delayed_submits >= usize::from(rate_control.look_ahead_depth));
            assert_eq!(outputs.len(), timestamps.len());
            assert_eq!(encoder.pending_frame_count(), 0);
            assert_eq!(
                outputs
                    .iter()
                    .map(|sample| sample.timestamp_90k)
                    .collect::<Vec<_>>(),
                timestamps
            );
            assert!(outputs.iter().all(|sample| !sample.data.is_empty()));
            encoder.shutdown().unwrap();
        }
    }

    #[test]
    #[ignore = "需要支持 HEVC 4:2:2/4:4:4 的 NVIDIA GPU；验证 D3D11-CUDA array 零拷贝注册和编码"]
    fn local_nvenc_cuda_array_formats_smoke() {
        use windows::Win32::Graphics::Dxgi::{CreateDXGIFactory1, IDXGIFactory1};
        use windows::core::Interface;

        let adapters = crate::backend::dxgi::enumerate_adapters().unwrap_or_default();
        let adapter_index = adapters
            .iter()
            .find(|adapter| adapter.vendor_id == NVIDIA_VENDOR_ID && adapter.flags & 0x2 == 0)
            .map(|adapter| adapter.index)
            .expect("本机需要至少一个 NVIDIA 显示 adapter");

        unsafe {
            let (api, _) = NvencApi::load().unwrap();
            let d3d_session = open_d3d11_session_for_adapter(&api, adapter_index).unwrap();
            let factory: IDXGIFactory1 = CreateDXGIFactory1().unwrap();
            let adapter = factory.EnumAdapters1(adapter_index).unwrap();
            let desc = adapter.GetDesc1().unwrap();
            let mut luid = [0u8; 8];
            luid[..4].copy_from_slice(&desc.AdapterLuid.LowPart.to_ne_bytes());
            luid[4..].copy_from_slice(&desc.AdapterLuid.HighPart.to_ne_bytes());
            let cuda_context = cuda::CudaPrimaryContext::for_adapter_luid(luid).unwrap();
            let width = 1_280;
            let height = 720;
            for input_format in [
                NvencD3d11InputFormat::Nv16,
                NvencD3d11InputFormat::P210,
                NvencD3d11InputFormat::Yuv444,
                NvencD3d11InputFormat::Yuv44410,
            ] {
                let color = if matches!(
                    input_format,
                    NvencD3d11InputFormat::P210 | NvencD3d11InputFormat::Yuv44410
                ) {
                    NclxColorMetadata::bt2020_pq(true)
                } else {
                    NclxColorMetadata::bt709(true)
                };
                let texture = create_synthetic_input_texture(
                    d3d_session.device(),
                    width,
                    height,
                    input_format,
                )
                .unwrap();
                let resource = texture
                    .cast::<windows::Win32::Graphics::Direct3D11::ID3D11Resource>()
                    .unwrap();
                let mut cuda_resource = cuda_context.register_d3d11_resource(&resource).unwrap();
                let array = cuda_resource.map_array().unwrap();
                let mut encoder = open_cuda_session(&api, cuda_context.raw_context()).unwrap();
                let rate_control = RateControlConfig {
                    look_ahead_depth: 0,
                    ..RateControlConfig::default()
                };
                initialize_low_latency_hevc_encoder(
                    &api,
                    encoder.encoder,
                    width,
                    height,
                    input_format,
                    color,
                    &rate_control,
                    60,
                    1,
                )
                .unwrap_or_else(|err| {
                    panic!("{} CUDA NVENC init failed: {err}", input_format.label())
                });
                let mut bitstream = create_bitstream_buffer(&api, encoder.encoder).unwrap();
                let mut registered = register_cuda_array_input(
                    &api,
                    encoder.encoder,
                    array,
                    width,
                    height,
                    input_format,
                )
                .unwrap_or_else(|err| {
                    panic!("{} CUDA array register failed: {err}", input_format.label())
                });
                let mut mapped = map_input_resource(&api, encoder.encoder, &registered).unwrap();
                let status = encode_one_d3d11_frame(
                    &api,
                    encoder.encoder,
                    &mapped,
                    &bitstream,
                    width,
                    height,
                    input_format,
                    0,
                    0,
                    true,
                )
                .unwrap();
                assert_eq!(status, NvencEncodePictureStatus::OutputAvailable);
                let output = lock_and_copy_bitstream(&api, encoder.encoder, &bitstream).unwrap();
                assert!(!output.bytes.is_empty(), "{} output", input_format.label());
                nvenc_check("NvEncUnmapInputResource", mapped.unmap_now()).unwrap();
                nvenc_check("NvEncUnregisterResource", registered.unregister_now()).unwrap();
                nvenc_check("NvEncDestroyBitstreamBuffer", bitstream.destroy_now()).unwrap();
                nvenc_check("NvEncDestroyEncoder", encoder.destroy_now()).unwrap();
                cuda_resource.unmap().unwrap();
                println!(
                    "D3D11-CUDA array format passed: {} pitch={} frame_bytes={} output_bytes={}",
                    input_format.label(),
                    input_format.dxgi_pitch_bytes(width),
                    input_format.frame_size_bytes(width, height),
                    output.bytes.len()
                );
            }
        }
    }

    #[test]
    #[ignore = "需要 NVIDIA CUDA external-memory interop；验证持久 D3D11 allocation -> CUDA array -> NVENC 性能"]
    fn local_nvenc_cuda_external_memory_p210_smoke() {
        use std::time::Instant;
        use windows::Win32::Graphics::Dxgi::{CreateDXGIFactory1, IDXGIFactory1};

        let adapters = crate::backend::dxgi::enumerate_adapters().unwrap_or_default();
        let adapter_index = adapters
            .iter()
            .find(|adapter| adapter.vendor_id == NVIDIA_VENDOR_ID && adapter.flags & 0x2 == 0)
            .map(|adapter| adapter.index)
            .expect("本机需要至少一个 NVIDIA 显示 adapter");
        unsafe {
            let (api, _) = NvencApi::load().unwrap();
            let (device, _immediate, _) = cached_d3d11_device_for_adapter(adapter_index).unwrap();
            let factory: IDXGIFactory1 = CreateDXGIFactory1().unwrap();
            let adapter = factory.EnumAdapters1(adapter_index).unwrap();
            let desc = adapter.GetDesc1().unwrap();
            let mut luid = [0u8; 8];
            luid[..4].copy_from_slice(&desc.AdapterLuid.LowPart.to_ne_bytes());
            luid[4..].copy_from_slice(&desc.AdapterLuid.HighPart.to_ne_bytes());
            let cuda_context = cuda::CudaPrimaryContext::for_adapter_luid(luid).unwrap();
            let width = 1_920;
            let height = 1_080;
            let input_format = NvencD3d11InputFormat::P210;
            let color = NclxColorMetadata::bt2020_pq(true);
            let (_texture, handle) =
                create_synthetic_external_input_texture(&device, width, height, input_format)
                    .unwrap();
            let external = cuda_context
                .import_external_texture(
                    handle,
                    width,
                    input_format.texture_height(height),
                    input_format,
                )
                .unwrap();
            let mut encoder = open_cuda_session(&api, cuda_context.raw_context()).unwrap();
            let rate_control = RateControlConfig {
                look_ahead_depth: 0,
                ..RateControlConfig::default()
            };
            initialize_low_latency_hevc_encoder(
                &api,
                encoder.encoder,
                width,
                height,
                input_format,
                color,
                &rate_control,
                240,
                1,
            )
            .unwrap();
            let mut bitstream = create_bitstream_buffer(&api, encoder.encoder).unwrap();
            let mut registered = register_cuda_array_input(
                &api,
                encoder.encoder,
                external.array(),
                width,
                height,
                input_format,
            )
            .unwrap();
            let mut mapped = map_input_resource(&api, encoder.encoder, &registered).unwrap();
            let frame_count = 120u32;
            let started = Instant::now();
            for frame in 0..frame_count {
                assert_eq!(
                    encode_one_d3d11_frame(
                        &api,
                        encoder.encoder,
                        &mapped,
                        &bitstream,
                        width,
                        height,
                        input_format,
                        frame,
                        u64::from(frame) * 375,
                        frame == 0,
                    )
                    .unwrap(),
                    NvencEncodePictureStatus::OutputAvailable
                );
                let output = lock_and_copy_bitstream(&api, encoder.encoder, &bitstream).unwrap();
                assert!(!output.bytes.is_empty());
            }
            let elapsed = started.elapsed();
            println!(
                "CUDA external-memory P210 persistent input: frames={} total_ms={:.3} ms_per_frame={:.3}",
                frame_count,
                elapsed.as_secs_f64() * 1000.0,
                elapsed.as_secs_f64() * 1000.0 / f64::from(frame_count)
            );
            nvenc_check("NvEncUnmapInputResource", mapped.unmap_now()).unwrap();
            nvenc_check("NvEncUnregisterResource", registered.unregister_now()).unwrap();
            nvenc_check("NvEncDestroyBitstreamBuffer", bitstream.destroy_now()).unwrap();
            nvenc_check("NvEncDestroyEncoder", encoder.destroy_now()).unwrap();
        }
    }

    #[test]
    #[ignore = "需要 NVIDIA CUDA external-memory interop 与 HEVC RExt；验证四种 planar 输入和 Lookahead 持久注册"]
    fn local_nvenc_cuda_external_formats_lookahead_smoke() {
        use windows::Win32::Graphics::Dxgi::IDXGIKeyedMutex;
        use windows::core::Interface;

        let adapters = crate::backend::dxgi::enumerate_adapters().unwrap_or_default();
        let adapter_index = adapters
            .iter()
            .find(|adapter| adapter.vendor_id == NVIDIA_VENDOR_ID && adapter.flags & 0x2 == 0)
            .map(|adapter| adapter.index)
            .expect("本机需要至少一个 NVIDIA 显示 adapter");
        unsafe {
            for input_format in [
                NvencD3d11InputFormat::Nv16,
                NvencD3d11InputFormat::P210,
                NvencD3d11InputFormat::Yuv444,
                NvencD3d11InputFormat::Yuv44410,
            ] {
                let color = if matches!(
                    input_format,
                    NvencD3d11InputFormat::P210 | NvencD3d11InputFormat::Yuv44410
                ) {
                    NclxColorMetadata::bt2020_pq_full()
                } else {
                    NclxColorMetadata::bt709_full()
                };
                let rate_control = RateControlConfig {
                    method: RateControlMethod::Cbr,
                    target_kbps: 20_000,
                    buffer_size_kb: 2_000,
                    initial_delay_kb: 1_000,
                    look_ahead_depth: 4,
                    ..RateControlConfig::default()
                };
                let mut encoder = cuda::NvencCudaInteropEncoder::open_with_rate_control(
                    adapter_index,
                    1_280,
                    720,
                    input_format,
                    color,
                    &rate_control,
                    120,
                    1,
                )
                .unwrap_or_else(|err| panic!("{} 初始化失败：{err}", input_format.label()));
                let textures = (0..12)
                    .map(|_| {
                        let (texture, unused_handle) = create_synthetic_external_input_texture(
                            encoder.device(),
                            1_280,
                            720,
                            input_format,
                        )
                        .unwrap();
                        windows::Win32::Foundation::CloseHandle(unused_handle).unwrap();
                        texture
                    })
                    .collect::<Vec<_>>();
                let mutexes = textures
                    .iter()
                    .map(|texture| texture.cast::<IDXGIKeyedMutex>().unwrap())
                    .collect::<Vec<_>>();
                let timestamps = (0..textures.len())
                    .map(|index| (index as u64).saturating_mul(750))
                    .collect::<Vec<_>>();
                let mut outputs = Vec::new();
                for (index, (texture, mutex)) in textures.iter().zip(mutexes.iter()).enumerate() {
                    mutex.AcquireSync(0, 1_000).unwrap();
                    mutex.ReleaseSync(1).unwrap();
                    outputs.extend(
                        encoder
                            .submit_texture(texture, timestamps[index], index == 0, false)
                            .unwrap(),
                    );
                }
                outputs.extend(encoder.flush().unwrap());
                assert_eq!(outputs.len(), timestamps.len(), "{}", input_format.label());
                assert_eq!(
                    outputs
                        .iter()
                        .map(|sample| sample.timestamp_90k)
                        .collect::<Vec<_>>(),
                    timestamps,
                    "{}",
                    input_format.label()
                );
                encoder.shutdown().unwrap();
                println!(
                    "CUDA external-memory Lookahead passed: {} frames={}",
                    input_format.label(),
                    outputs.len()
                );
            }
        }
    }

    #[test]
    #[ignore = "需要 NVIDIA CUDA external-memory interop；验证快速中止释放 pending keyed mutex"]
    fn local_nvenc_cuda_external_pending_drop_releases_keys() {
        use windows::Win32::Graphics::Dxgi::IDXGIKeyedMutex;
        use windows::core::Interface;

        let adapters = crate::backend::dxgi::enumerate_adapters().unwrap_or_default();
        let adapter_index = adapters
            .iter()
            .find(|adapter| adapter.vendor_id == NVIDIA_VENDOR_ID && adapter.flags & 0x2 == 0)
            .map(|adapter| adapter.index)
            .expect("本机需要至少一个 NVIDIA 显示 adapter");
        unsafe {
            let rate_control = RateControlConfig {
                look_ahead_depth: 8,
                ..RateControlConfig::default()
            };
            let mut encoder = cuda::NvencCudaInteropEncoder::open_with_rate_control(
                adapter_index,
                1_280,
                720,
                NvencD3d11InputFormat::P210,
                NclxColorMetadata::bt2020_pq_full(),
                &rate_control,
                120,
                1,
            )
            .unwrap();
            let mut textures = Vec::new();
            let mut mutexes = Vec::new();
            for index in 0..4u64 {
                let (texture, unused_handle) = create_synthetic_external_input_texture(
                    encoder.device(),
                    1_280,
                    720,
                    NvencD3d11InputFormat::P210,
                )
                .unwrap();
                windows::Win32::Foundation::CloseHandle(unused_handle).unwrap();
                let mutex = texture.cast::<IDXGIKeyedMutex>().unwrap();
                mutex.AcquireSync(0, 1_000).unwrap();
                mutex.ReleaseSync(1).unwrap();
                assert!(
                    encoder
                        .submit_texture(&texture, index.saturating_mul(750), index == 0, false)
                        .unwrap()
                        .is_empty()
                );
                textures.push(texture);
                mutexes.push(mutex);
            }
            assert_eq!(encoder.pending_frame_count(), 4);
            drop(encoder);
            for mutex in &mutexes {
                mutex.AcquireSync(0, 1_000).unwrap();
                mutex.ReleaseSync(0).unwrap();
            }
            drop(textures);
        }
    }

    #[test]
    #[ignore = "需要本机 NVIDIA 驱动；手动验证 CBR/VBR/CQP 三种 NVENC 码控生产初始化和首帧编码"]
    fn local_nvenc_rate_control_modes_smoke() {
        let adapters = crate::backend::dxgi::enumerate_adapters().unwrap_or_default();
        let adapter_index = adapters
            .iter()
            .find(|adapter| adapter.vendor_id == NVIDIA_VENDOR_ID && adapter.flags & 0x2 == 0)
            .map(|adapter| adapter.index)
            .expect("本机需要至少一个 NVIDIA 显示 adapter");
        let configs = [
            RateControlConfig {
                method: RateControlMethod::Cbr,
                target_kbps: 20_000,
                buffer_size_kb: 2_000,
                initial_delay_kb: 1_000,
                look_ahead_depth: 0,
                nvenc_spatial_aq: true,
                nvenc_multi_pass: crate::rate_control::NvencMultiPass::QuarterResolution,
                ..Default::default()
            },
            RateControlConfig {
                method: RateControlMethod::Vbr,
                target_kbps: 16_000,
                max_kbps: 30_000,
                buffer_size_kb: 2_000,
                initial_delay_kb: 1_000,
                look_ahead_depth: 0,
                nvenc_vbr_target_quality: 24,
                nvenc_spatial_aq: true,
                nvenc_multi_pass: crate::rate_control::NvencMultiPass::QuarterResolution,
                ..Default::default()
            },
            RateControlConfig {
                method: RateControlMethod::Cqp,
                qpi: 20,
                qpp: 22,
                qpb: 24,
                nvenc_spatial_aq: true,
                nvenc_multi_pass: crate::rate_control::NvencMultiPass::FullResolution,
                ..Default::default()
            },
        ];

        unsafe {
            let width = 1_280;
            let height = 720;
            let input_format = NvencD3d11InputFormat::P010;
            let color = current_display_color_for_adapter(adapter_index).unwrap();
            for config in configs {
                let mut encoder = NvencD3d11Encoder::open_with_rate_control(
                    adapter_index,
                    width,
                    height,
                    input_format,
                    color,
                    &config,
                    240,
                    1,
                )
                .unwrap();
                let texture =
                    create_synthetic_input_texture(encoder.device(), width, height, input_format)
                        .unwrap();
                let sample = encoder.encode_texture(&texture, 0, true, false).unwrap();
                println!(
                    "{}",
                    serde_json::to_string_pretty(&config.to_nvenc_fields().unwrap()).unwrap()
                );
                assert!(!sample.data.is_empty());
            }
        }
    }

    #[test]
    #[ignore = "需要本机多 NVENC engine NVIDIA GPU；手动验证非默认 preset/split/multiPass/Spatial AQ 原始值"]
    fn local_nvenc_non_default_tuning_smoke() {
        let adapters = crate::backend::dxgi::enumerate_adapters().unwrap_or_default();
        let probe = probe_nvenc_adapters(&adapters);
        let adapter = probe
            .adapters
            .iter()
            .find(|adapter| {
                !adapter.current_display_routes.is_empty()
                    && adapter.caps.encoder_engines.unwrap_or_default() >= 2
                    && adapter.hevc_presets.contains(&NvencPreset::P7)
            })
            .expect("本机需要当前显示器所在的多 engine NVIDIA adapter，并支持 P7");
        unsafe {
            let width = 3_840;
            let height = 2_160;
            let input_format = NvencD3d11InputFormat::P010;
            let color = current_display_color_for_adapter(adapter.adapter_index).unwrap();
            for split_mode in NvencSplitEncodeMode::all() {
                let rate_control = RateControlConfig {
                    method: RateControlMethod::Cbr,
                    target_kbps: 20_000,
                    look_ahead_depth: 0,
                    nvenc_preset: NvencPreset::P7,
                    nvenc_split_encode_mode: split_mode,
                    nvenc_multi_pass: crate::rate_control::NvencMultiPass::FullResolution,
                    nvenc_spatial_aq: true,
                    ..Default::default()
                };
                let mut encoder = NvencD3d11Encoder::open_with_rate_control(
                    adapter.adapter_index,
                    width,
                    height,
                    input_format,
                    color,
                    &rate_control,
                    240,
                    1,
                )
                .unwrap();
                let texture =
                    create_synthetic_input_texture(encoder.device(), width, height, input_format)
                        .unwrap();
                let first = encoder.encode_texture(&texture, 0, true, false).unwrap();
                println!(
                    "{}",
                    serde_json::to_string_pretty(&rate_control.to_nvenc_tuning_fields()).unwrap()
                );
                assert!(!first.data.is_empty());
            }
            for multi_pass in crate::rate_control::NvencMultiPass::all() {
                let rate_control = RateControlConfig {
                    method: RateControlMethod::Cbr,
                    target_kbps: 20_000,
                    look_ahead_depth: 0,
                    nvenc_preset: NvencPreset::P7,
                    nvenc_split_encode_mode: NvencSplitEncodeMode::Auto,
                    nvenc_multi_pass: multi_pass,
                    nvenc_spatial_aq: true,
                    ..Default::default()
                };
                let mut encoder = NvencD3d11Encoder::open_with_rate_control(
                    adapter.adapter_index,
                    width,
                    height,
                    input_format,
                    color,
                    &rate_control,
                    240,
                    1,
                )
                .unwrap();
                let texture =
                    create_synthetic_input_texture(encoder.device(), width, height, input_format)
                        .unwrap();
                let first = encoder.encode_texture(&texture, 0, true, false).unwrap();
                println!(
                    "{}",
                    serde_json::to_string_pretty(&rate_control.to_nvenc_tuning_fields()).unwrap()
                );
                assert!(!first.data.is_empty());
            }
        }
    }

    #[test]
    #[ignore = "需要本机 NVIDIA 驱动；手动验证 32 个持久注册 P010 surface 的匀速运动编码"]
    fn local_nvenc_p010_synthetic_motion_smoke() {
        use windows::Win32::Graphics::Direct3D11::ID3D11Resource;
        use windows::core::Interface;

        let adapters = crate::backend::dxgi::enumerate_adapters().unwrap_or_default();
        let adapter_index = adapters
            .iter()
            .find(|adapter| adapter.vendor_id == NVIDIA_VENDOR_ID && adapter.flags & 0x2 == 0)
            .map(|adapter| adapter.index)
            .expect("本机需要至少一个 NVIDIA 显示 adapter");

        unsafe {
            let width = 3840u32;
            let height = 2160u32;
            let frame_count = std::env::var("RUST_REPLAY_NVENC_SYNTHETIC_FRAMES")
                .ok()
                .and_then(|value| value.parse::<u32>().ok())
                .unwrap_or(1200);
            let input_format = NvencD3d11InputFormat::P010;
            let color = current_display_color_for_adapter(adapter_index).unwrap();
            let rate_control = RateControlConfig {
                method: RateControlMethod::Cbr,
                target_kbps: 20_000,
                look_ahead_depth: 0,
                ..Default::default()
            };
            let mut encoder = NvencD3d11Encoder::open_with_rate_control(
                adapter_index,
                width,
                height,
                input_format,
                color,
                &rate_control,
                240,
                1,
            )
            .unwrap();
            let context = encoder.context().clone();
            let textures = (0..32)
                .map(|_| {
                    create_synthetic_input_texture(encoder.device(), width, height, input_format)
                        .unwrap()
                })
                .collect::<Vec<_>>();

            let pitch = input_format.dxgi_pitch_bytes(width) as usize;
            let mut pixels = vec![0u8; input_format.frame_size_bytes(width, height)];
            let black = (64u16 << 6).to_le_bytes();
            for row in pixels[..pitch * height as usize].chunks_exact_mut(pitch) {
                for value in row.chunks_exact_mut(2) {
                    value.copy_from_slice(&black);
                }
            }
            let chroma_start = pitch * height as usize;
            let neutral = (512u16 << 6).to_le_bytes();
            for row in pixels[chroma_start..].chunks_exact_mut(pitch) {
                for pair in row.chunks_exact_mut(4) {
                    pair[0..2].copy_from_slice(&neutral);
                    pair[2..4].copy_from_slice(&neutral);
                }
            }

            let object_width = 192usize;
            let object_height = 96usize;
            let object_y = 1000usize;
            let travel = width as usize - object_width - 512;
            let bright = (900u16 << 6).to_le_bytes();
            let mut previous_x = None;
            let mut samples = Vec::with_capacity(frame_count as usize);
            for frame_index in 0..frame_count {
                if let Some(x) = previous_x {
                    for y in object_y..object_y + object_height {
                        let row = &mut pixels[y * pitch..(y + 1) * pitch];
                        for value in row[x * 2..(x + object_width) * 2].chunks_exact_mut(2) {
                            value.copy_from_slice(&black);
                        }
                    }
                }
                let x = 256 + (frame_index as usize * 8) % travel;
                for y in object_y..object_y + object_height {
                    let row = &mut pixels[y * pitch..(y + 1) * pitch];
                    for value in row[x * 2..(x + object_width) * 2].chunks_exact_mut(2) {
                        value.copy_from_slice(&bright);
                    }
                }
                previous_x = Some(x);

                let texture = &textures[frame_index as usize % textures.len()];
                let resource: ID3D11Resource = texture.cast().unwrap();
                context.UpdateSubresource(
                    &resource,
                    0,
                    None,
                    pixels.as_ptr() as *const c_void,
                    pitch as u32,
                    0,
                );
                samples.push(
                    encoder
                        .encode_texture(
                            texture,
                            u64::from(frame_index) * 375,
                            frame_index == 0,
                            false,
                        )
                        .unwrap(),
                );
            }

            let path = std::env::var_os("RUST_REPLAY_NVENC_SYNTHETIC_OUTPUT")
                .map(PathBuf::from)
                .unwrap_or_else(|| std::env::temp_dir().join("rustreplay_nvenc_motion.mp4"));
            let encoded_bytes = samples
                .iter()
                .map(|sample| sample.data.len())
                .sum::<usize>();
            crate::backend::mp4_mux::write_hevc_aac_mp4(
                &path,
                &crate::backend::mp4_mux::HevcMp4Track {
                    width: width as u16,
                    height: height as u16,
                    duration_90k: u64::from(frame_count) * 375,
                    color,
                    codec: crate::backend::mp4_mux::HevcCodecMetadata::main10_420_10(),
                    samples,
                },
                None,
            )
            .unwrap();
            println!(
                "synthetic_motion path={} frames={} bytes={} registration={} registered_inputs={}",
                path.display(),
                frame_count,
                encoded_bytes,
                encoder.registration_mode(),
                encoder.registered_inputs.len()
            );
        }
    }

    #[test]
    #[ignore = "需要本机 NVIDIA 驱动、D3D11 桌面会话和 ffprobe；手动验证 NVENC SPS 动态 full/limited VUI"]
    fn local_nvenc_dynamic_vui_range_smoke() {
        let adapters = crate::backend::dxgi::enumerate_adapters().unwrap_or_default();
        let adapter_index = adapters
            .iter()
            .find(|adapter| adapter.vendor_id == NVIDIA_VENDOR_ID && adapter.flags & 0x2 == 0)
            .map(|adapter| adapter.index)
            .expect("本机需要至少一个 NVIDIA 显示 adapter");

        unsafe {
            let width = 1280u32;
            let height = 720u32;
            let input_format = NvencD3d11InputFormat::P010;
            for full_range in [false, true] {
                let color = NclxColorMetadata::bt2020_pq(full_range);
                let mut encoder =
                    NvencD3d11Encoder::open(adapter_index, width, height, input_format, color)
                        .unwrap();
                let texture =
                    create_synthetic_input_texture(encoder.device(), width, height, input_format)
                        .unwrap();
                let sample = encoder.encode_texture(&texture, 0, true, false).unwrap();
                let label = if full_range { "full" } else { "limited" };
                let path = std::env::temp_dir().join(format!(
                    "rustreplay_nvenc_vui_{label}_{}.mp4",
                    std::process::id()
                ));
                crate::backend::mp4_mux::write_hevc_aac_mp4(
                    &path,
                    &crate::backend::mp4_mux::HevcMp4Track {
                        width: width as u16,
                        height: height as u16,
                        duration_90k: 3_000,
                        color,
                        codec: crate::backend::mp4_mux::HevcCodecMetadata::main10_420_10(),
                        samples: vec![sample],
                    },
                    None,
                )
                .unwrap();
                let output = std::process::Command::new("ffprobe")
                    .args([
                        "-v",
                        "error",
                        "-select_streams",
                        "v:0",
                        "-show_entries",
                        "stream=color_range,color_space,color_transfer,color_primaries:frame=color_range,color_space,color_transfer,color_primaries",
                        "-of",
                        "json",
                    ])
                    .arg(&path)
                    .output()
                    .unwrap();
                let _ = std::fs::remove_file(&path);
                assert!(output.status.success(), "ffprobe failed for {label}");
                let stdout = String::from_utf8(output.stdout).unwrap();
                let expected_range = if full_range { "pc" } else { "tv" };
                let metadata: serde_json::Value = serde_json::from_str(&stdout).unwrap();
                for section in ["streams", "frames"] {
                    let entry = metadata[section]
                        .as_array()
                        .and_then(|entries| entries.first())
                        .unwrap_or_else(|| {
                            panic!("ffprobe missing {section} for {label}: {stdout}")
                        });
                    assert_eq!(entry["color_range"], expected_range, "{section}: {stdout}");
                    assert_eq!(entry["color_space"], "bt2020nc", "{section}: {stdout}");
                    assert_eq!(entry["color_transfer"], "smpte2084", "{section}: {stdout}");
                    assert_eq!(entry["color_primaries"], "bt2020", "{section}: {stdout}");
                }
                println!("NVENC {label} MP4 stream/frame metadata:\n{stdout}");
            }
        }
    }

    #[test]
    #[ignore = "需要本机 NVIDIA 驱动和 D3D11 桌面会话；手动用于验证 NVENC AYUV registered-resource 编码链路"]
    fn local_nvenc_d3d11_ayuv_encode_smoke() {
        let adapters = crate::backend::dxgi::enumerate_adapters().unwrap_or_default();
        let adapter_index = adapters
            .iter()
            .find(|adapter| adapter.vendor_id == NVIDIA_VENDOR_ID && adapter.flags & 0x2 == 0)
            .map(|adapter| adapter.index)
            .expect("本机需要至少一个 NVIDIA 显示 adapter");
        unsafe {
            let width = 1280u32;
            let height = 720u32;
            let input_format = NvencD3d11InputFormat::Ayuv;
            let color = current_display_color_for_adapter(adapter_index).unwrap();
            let mut encoder =
                NvencD3d11Encoder::open(adapter_index, width, height, input_format, color).unwrap();
            let texture =
                create_synthetic_input_texture(encoder.device(), width, height, input_format)
                    .unwrap();
            let sample = encoder.encode_texture(&texture, 0, true, false).unwrap();
            println!(
                "{}",
                serde_json::to_string_pretty(&NvencD3d11EncodeSmokeReport {
                    adapter_index,
                    width,
                    height,
                    input_format: input_format.label().to_owned(),
                    output_bytes: sample.data.len(),
                    annex_b_start_code_seen: sample
                        .data
                        .windows(4)
                        .any(|window| window == [0x00, 0x00, 0x00, 0x01]),
                })
                .unwrap()
            );
            assert!(!sample.data.is_empty());
        }
    }
}
