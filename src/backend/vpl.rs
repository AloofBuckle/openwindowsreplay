#![allow(non_snake_case, dead_code, unsafe_op_in_unsafe_fn)]
//! oneVPL 动态 FFI 能力探测。
//!
//! 不依赖封装 crate，运行时尝试加载系统/oneAPI/MSYS2 中的 libvpl。探测只读取
//! dispatcher 暴露的 `mfxImplDescription`。真正编码阶段使用
//! `MFXVideoENCODE_Query/Init`、`MFXMemory_GetSurfaceForEncode` 和一次 GPU
//! `CopyResource` 写入 oneVPL 内部分配的 D3D11 surface，不保留旧外部 surface 导入或 0 拷贝分支。

use crate::backend::mp4_mux::{HevcCodecMetadata, NclxColorMetadata};
use crate::config::ChromaSampling;
use crate::error::BackendError;
use crate::rate_control::{RateControlConfig, RateControlMethod};
use libloading::Library;
use serde::{Deserialize, Serialize};
use std::collections::{BTreeSet, HashMap, VecDeque};
use std::ffi::{c_char, c_void};
use std::path::{Path, PathBuf};
use std::ptr;

const MFX_ERR_NONE: i32 = 0;
const MFX_ERR_NOT_FOUND: i32 = -9;
const MFX_ERR_MORE_DATA: i32 = -10;
const MFX_ERR_MORE_SURFACE: i32 = -11;
const MFX_ERR_NOT_IMPLEMENTED: i32 = -24;
const MFX_WRN_IN_EXECUTION: i32 = 1;
const MFX_WRN_DEVICE_BUSY: i32 = 2;
const MFX_WRN_PARTIAL_ACCELERATION: i32 = 4;
const MFX_IMPLCAPS_IMPLDESCSTRUCTURE: u32 = 1;
const MFX_IMPL_TYPE_HARDWARE: u32 = 0x0002;
const MFX_ACCEL_MODE_VIA_D3D11: u32 = 0x0300;
const MFX_RESOURCE_DX11_TEXTURE: u32 = 5;
const MFX_IOPATTERN_IN_VIDEO_MEMORY: u16 = 0x01;
const MFX_PICSTRUCT_PROGRESSIVE: u16 = 0x01;
const MFX_HANDLE_D3D11_DEVICE: u32 = 3;
const VIDEO_CLOCK_HZ: u64 = 90_000;
const VPL_RECORD_ASYNC_DEPTH: u16 = 16;
const VPL_RECORD_MAX_IN_FLIGHT: usize = 64;
const VPL_BITSTREAM_BYTES: usize = 64 * 1024 * 1024;
const MFX_CODINGOPTION_ON: u16 = 0x10;
const MFX_CODINGOPTION_OFF: u16 = 0x20;
const MFX_FRAMETYPE_I: u16 = 0x0001;
const MFX_FRAMETYPE_REF: u16 = 0x0040;
const MFX_FRAMETYPE_IDR: u16 = 0x0080;
const MFX_EXTBUFF_CODING_OPTION2: u32 = make_fourcc(b'C', b'D', b'O', b'2');
const MFX_EXTBUFF_CODING_OPTION3: u32 = make_fourcc(b'C', b'D', b'O', b'3');
const MFX_EXTBUFF_VIDEO_SIGNAL_INFO: u32 = make_fourcc(b'V', b'S', b'I', b'N');
const VPL_ENCODER_FRAME_RATE_HINT_FALLBACK_N: u32 = 60;
const VPL_ENCODER_FRAME_RATE_HINT_FALLBACK_D: u32 = 1;
const ENCODER_WARMUP_TIMESTAMP_STEP_90K: u64 = 1;

const MFX_CODEC_HEVC: u32 = make_fourcc(b'H', b'E', b'V', b'C');
const MFX_FOURCC_NV12: u32 = make_fourcc(b'N', b'V', b'1', b'2');
const MFX_FOURCC_YUY2: u32 = make_fourcc(b'Y', b'U', b'Y', b'2');
const MFX_FOURCC_RGB4: u32 = make_fourcc(b'R', b'G', b'B', b'4');
const MFX_FOURCC_P010: u32 = make_fourcc(b'P', b'0', b'1', b'0');
const MFX_FOURCC_P210: u32 = make_fourcc(b'P', b'2', b'1', b'0');
const MFX_FOURCC_AYUV: u32 = make_fourcc(b'A', b'Y', b'U', b'V');
const MFX_FOURCC_Y210: u32 = make_fourcc(b'Y', b'2', b'1', b'0');
const MFX_FOURCC_Y410: u32 = make_fourcc(b'Y', b'4', b'1', b'0');

const MFX_PROFILE_HEVC_MAIN: u32 = 1;
const MFX_PROFILE_HEVC_MAIN10: u32 = 2;
const MFX_PROFILE_HEVC_MAINSP: u32 = 3;
const MFX_PROFILE_HEVC_REXT: u32 = 4;
const MFX_PROFILE_HEVC_SCC: u32 = 9;

#[derive(Debug, Clone, Copy)]
struct VplRecordRoute {
    label: &'static str,
    fourcc: u32,
    chroma: u16,
    bit_depth: u16,
    profile: u16,
    mp4_color: NclxColorMetadata,
    mp4_codec: HevcCodecMetadata,
}

#[derive(Debug, Clone, Copy)]
struct DisplayRouteColor {
    hdr_pq: bool,
    bit_depth: u16,
    full_range: bool,
    mp4_color: NclxColorMetadata,
    note: &'static str,
}

impl VplRecordRoute {
    const fn sdr8_nv12() -> Self {
        Self {
            label: "SDR 8-bit -> NV12 -> HEVC Main",
            fourcc: MFX_FOURCC_NV12,
            chroma: 1,
            bit_depth: 8,
            profile: MFX_PROFILE_HEVC_MAIN as u16,
            mp4_color: NclxColorMetadata::bt709_full(),
            mp4_codec: HevcCodecMetadata::main_420_8(),
        }
    }

    const fn sdr8_yuy2() -> Self {
        Self {
            label: "SDR 8-bit -> YUY2 -> HEVC RExt 422",
            fourcc: MFX_FOURCC_YUY2,
            chroma: 2,
            bit_depth: 8,
            profile: MFX_PROFILE_HEVC_REXT as u16,
            mp4_color: NclxColorMetadata::bt709_full(),
            mp4_codec: HevcCodecMetadata::rext(2, 8),
        }
    }

    const fn sdr10_y210() -> Self {
        Self {
            label: "SDR 10-bit -> Y210 -> HEVC RExt 422",
            fourcc: MFX_FOURCC_Y210,
            chroma: 2,
            bit_depth: 10,
            profile: MFX_PROFILE_HEVC_REXT as u16,
            mp4_color: NclxColorMetadata::bt709_full(),
            mp4_codec: HevcCodecMetadata::rext(2, 10),
        }
    }

    const fn hdr_pq_y210() -> Self {
        Self {
            label: "HDR PQ 10-bit -> Y210 -> HEVC RExt 422",
            fourcc: MFX_FOURCC_Y210,
            chroma: 2,
            bit_depth: 10,
            profile: MFX_PROFILE_HEVC_REXT as u16,
            mp4_color: NclxColorMetadata::bt2020_pq_full(),
            mp4_codec: HevcCodecMetadata::rext(2, 10),
        }
    }

    const fn sdr10_p010() -> Self {
        Self {
            label: "SDR 10-bit -> P010 -> HEVC Main10",
            fourcc: MFX_FOURCC_P010,
            chroma: 1,
            bit_depth: 10,
            profile: MFX_PROFILE_HEVC_MAIN10 as u16,
            mp4_color: NclxColorMetadata::bt709_full(),
            mp4_codec: HevcCodecMetadata::main10_420_10(),
        }
    }

    const fn sdr10_p210() -> Self {
        Self {
            label: "SDR 10-bit -> P210 -> HEVC RExt 422",
            fourcc: MFX_FOURCC_P210,
            chroma: 2,
            bit_depth: 10,
            profile: MFX_PROFILE_HEVC_REXT as u16,
            mp4_color: NclxColorMetadata::bt709_full(),
            mp4_codec: HevcCodecMetadata::rext(2, 10),
        }
    }

    const fn sdr8_ayuv() -> Self {
        Self {
            label: "SDR 8-bit -> AYUV -> HEVC RExt 444",
            fourcc: MFX_FOURCC_AYUV,
            chroma: 3,
            bit_depth: 8,
            profile: MFX_PROFILE_HEVC_REXT as u16,
            mp4_color: NclxColorMetadata::bt709_full(),
            mp4_codec: HevcCodecMetadata::rext(3, 8),
        }
    }

    const fn sdr10_y410() -> Self {
        Self {
            label: "SDR 10-bit -> Y410 -> HEVC RExt 444",
            fourcc: MFX_FOURCC_Y410,
            chroma: 3,
            bit_depth: 10,
            profile: MFX_PROFILE_HEVC_REXT as u16,
            mp4_color: NclxColorMetadata::bt709_full(),
            mp4_codec: HevcCodecMetadata::rext(3, 10),
        }
    }

    const fn hdr_pq_y410() -> Self {
        Self {
            label: "HDR PQ 10-bit -> Y410 -> HEVC RExt 444",
            fourcc: MFX_FOURCC_Y410,
            chroma: 3,
            bit_depth: 10,
            profile: MFX_PROFILE_HEVC_REXT as u16,
            mp4_color: NclxColorMetadata::bt2020_pq_full(),
            mp4_codec: HevcCodecMetadata::rext(3, 10),
        }
    }

    const fn sdr8_rgb4() -> Self {
        Self {
            label: "SDR 8-bit -> RGB4 -> HEVC RExt 444",
            fourcc: MFX_FOURCC_RGB4,
            chroma: 3,
            bit_depth: 8,
            profile: MFX_PROFILE_HEVC_REXT as u16,
            mp4_color: NclxColorMetadata::bt709_full(),
            mp4_codec: HevcCodecMetadata::rext(3, 8),
        }
    }

    fn query_candidates() -> [Self; 8] {
        [
            Self::sdr8_nv12(),
            Self::sdr10_p010(),
            Self::sdr8_yuy2(),
            Self::sdr10_y210(),
            Self::sdr10_p210(),
            Self::sdr8_ayuv(),
            Self::sdr10_y410(),
            Self::sdr8_rgb4(),
        ]
    }

    const fn hdr_pq_p010() -> Self {
        Self {
            label: "HDR PQ 10-bit -> P010 -> HEVC Main10",
            fourcc: MFX_FOURCC_P010,
            chroma: 1,
            bit_depth: 10,
            profile: MFX_PROFILE_HEVC_MAIN10 as u16,
            mp4_color: NclxColorMetadata::bt2020_pq_full(),
            mp4_codec: HevcCodecMetadata::main10_420_10(),
        }
    }

    fn summary(self) -> String {
        format!(
            "{} / FourCC={} chroma={} bit_depth={} profile={} nclx={}/{}/{} range={}",
            self.label,
            fourcc_to_string(self.fourcc),
            self.chroma,
            self.bit_depth,
            hevc_profile_name(u32::from(self.profile)),
            self.mp4_color.colour_primaries,
            self.mp4_color.transfer_characteristics,
            self.mp4_color.matrix_coefficients,
            if self.mp4_color.full_range {
                "full"
            } else {
                "limited"
            }
        )
    }

    fn is_hdr_pq(self) -> bool {
        self.mp4_color.transfer_characteristics == 16
    }

    fn is_bt2020_sdr(self) -> bool {
        self.mp4_color.colour_primaries == 9
            && matches!(self.mp4_color.transfer_characteristics, 1 | 14)
            && self.mp4_color.matrix_coefficients == 9
    }

    fn requires_fp16_capture(self) -> bool {
        self.bit_depth >= 10
    }

    fn supports_requested_chroma(self, requested: ChromaSampling) -> bool {
        matches!(
            (requested, self.chroma),
            (ChromaSampling::Yuv420, 1) | (ChromaSampling::Yuv422, 2) | (ChromaSampling::Yuv444, 3)
        )
    }

    fn candidates_for_requested_chroma_and_display(
        requested: ChromaSampling,
        display_color: DisplayRouteColor,
    ) -> Vec<Self> {
        let mut routes = match (requested, display_color.hdr_pq) {
            (ChromaSampling::Yuv420, false) if display_color.bit_depth >= 10 => {
                vec![Self::sdr10_p010()]
            }
            (ChromaSampling::Yuv420, false) => vec![Self::sdr8_nv12()],
            (ChromaSampling::Yuv420, true) => vec![Self::hdr_pq_p010()],
            (ChromaSampling::Yuv422, false) if display_color.bit_depth >= 10 => {
                vec![Self::sdr10_y210()]
            }
            (ChromaSampling::Yuv422, false) => vec![Self::sdr8_yuy2()],
            (ChromaSampling::Yuv422, true) => vec![Self::hdr_pq_y210()],
            (ChromaSampling::Yuv444, false) if display_color.bit_depth >= 10 => {
                vec![Self::sdr10_y410()]
            }
            // 8-bit 444 有两个已接线 GPU writer：优先 AYUV；若具体 oneVPL/驱动只
            // Query/Init RGB4，录制阶段会在同一源色彩契约下自动落到 RGB4。
            (ChromaSampling::Yuv444, false) => vec![Self::sdr8_ayuv(), Self::sdr8_rgb4()],
            (ChromaSampling::Yuv444, true) => vec![Self::hdr_pq_y410()],
        };
        routes.retain(|route| route.production_gpu_writer_available());
        routes
            .into_iter()
            .map(|route| route.with_color(display_color.mp4_color))
            .collect()
    }

    fn production_gpu_writer_available(self) -> bool {
        matches!(
            self.fourcc,
            MFX_FOURCC_NV12
                | MFX_FOURCC_P010
                | MFX_FOURCC_YUY2
                | MFX_FOURCC_Y210
                | MFX_FOURCC_AYUV
                | MFX_FOURCC_Y410
                | MFX_FOURCC_RGB4
        )
    }

    fn production_gpu_writer_blocker(self) -> Option<&'static str> {
        if self.production_gpu_writer_available() {
            None
        } else if self.fourcc == MFX_FOURCC_P210 {
            Some(
                "P210 在 oneVPL Query 中可见，但 DXGI/D3D11 没有可直接创建/绑定的 P210 texture format；用 P016/P010 代替会改变 4:2:2 平面布局，无法保证 GPU-only 高保真",
            )
        } else {
            Some("该 FourCC 尚无生产 GPU ChromaWriter 或 DXGI texture layout 证明")
        }
    }

    const fn with_color(mut self, color: NclxColorMetadata) -> Self {
        self.mp4_color = color;
        self
    }
}

#[cfg(windows)]
fn select_record_route_candidates_for_output(
    output: &windows::Win32::Graphics::Dxgi::IDXGIOutput,
    requested_chroma: ChromaSampling,
    notes: &mut Vec<String>,
) -> Result<Vec<VplRecordRoute>, BackendError> {
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
    use windows::Win32::Graphics::Dxgi::IDXGIOutput6;
    use windows::core::Interface;

    let Ok(output6) = output.cast::<IDXGIOutput6>() else {
        return Err(BackendError::unsupported(
            "录制路线选择",
            "IDXGIOutput6::GetDesc1 不可用，无法可靠读取当前显示器色彩空间",
            "不支持的桌面模式",
        ));
    };
    let desc1 = unsafe {
        output6.GetDesc1().map_err(|err| BackendError::WindowsApi {
            func: "IDXGIOutput6::GetDesc1(record route)",
            message: err.to_string(),
        })?
    };
    let color_space = desc1.ColorSpace;
    let detected_bits = if desc1.BitsPerColor == 0 {
        8
    } else {
        desc1.BitsPerColor as u16
    };
    if detected_bits > 10 {
        return Err(BackendError::unsupported(
            "录制路线选择",
            format!(
                "DXGI ColorSpace={} BitsPerColor={}",
                color_space.0, desc1.BitsPerColor
            ),
            "不支持的桌面模式",
        ));
    }
    let display_color = if color_space == DXGI_COLOR_SPACE_RGB_FULL_G2084_NONE_P2020 {
        DisplayRouteColor {
            hdr_pq: true,
            bit_depth: 10,
            full_range: true,
            mp4_color: NclxColorMetadata::bt2020_pq_full(),
            note: "DXGI RGB_FULL_G2084_P2020 -> BT.2020/PQ/full",
        }
    } else if color_space == DXGI_COLOR_SPACE_RGB_STUDIO_G2084_NONE_P2020
        || color_space == DXGI_COLOR_SPACE_YCBCR_STUDIO_G2084_LEFT_P2020
        || color_space == DXGI_COLOR_SPACE_YCBCR_STUDIO_G2084_TOPLEFT_P2020
    {
        DisplayRouteColor {
            hdr_pq: true,
            bit_depth: 10,
            full_range: false,
            mp4_color: NclxColorMetadata::bt2020_pq_limited(),
            note: "DXGI *_STUDIO_G2084_P2020 -> BT.2020/PQ/limited",
        }
    } else if color_space == DXGI_COLOR_SPACE_RGB_FULL_G22_NONE_P709
        || color_space == DXGI_COLOR_SPACE_YCBCR_FULL_G22_LEFT_P709
        || color_space == DXGI_COLOR_SPACE_YCBCR_FULL_G22_NONE_P709_X601
    {
        DisplayRouteColor {
            hdr_pq: false,
            bit_depth: if detected_bits >= 10 { 10 } else { 8 },
            full_range: true,
            mp4_color: NclxColorMetadata::bt709_full(),
            note: "DXGI *_FULL_G22_P709 -> BT.709/full",
        }
    } else if color_space == DXGI_COLOR_SPACE_RGB_STUDIO_G22_NONE_P709
        || color_space == DXGI_COLOR_SPACE_RGB_STUDIO_G24_NONE_P709
        || color_space == DXGI_COLOR_SPACE_YCBCR_STUDIO_G22_LEFT_P709
        || color_space == DXGI_COLOR_SPACE_YCBCR_STUDIO_G24_LEFT_P709
    {
        DisplayRouteColor {
            hdr_pq: false,
            bit_depth: if detected_bits >= 10 { 10 } else { 8 },
            full_range: false,
            mp4_color: NclxColorMetadata::bt709_limited(),
            note: "DXGI *_STUDIO_G22/G24_P709 -> BT.709/limited",
        }
    } else if color_space == DXGI_COLOR_SPACE_YCBCR_FULL_GHLG_TOPLEFT_P2020
        || color_space == DXGI_COLOR_SPACE_YCBCR_STUDIO_GHLG_TOPLEFT_P2020
    {
        return Err(BackendError::unsupported(
            "录制路线选择",
            format!("DXGI ColorSpace={}", color_space.0),
            "不支持的桌面模式",
        ));
    } else if color_space == DXGI_COLOR_SPACE_RGB_FULL_G22_NONE_P2020
        || color_space == DXGI_COLOR_SPACE_RGB_STUDIO_G22_NONE_P2020
        || color_space == DXGI_COLOR_SPACE_RGB_STUDIO_G24_NONE_P2020
        || color_space == DXGI_COLOR_SPACE_YCBCR_FULL_G22_LEFT_P2020
        || color_space == DXGI_COLOR_SPACE_YCBCR_STUDIO_G22_LEFT_P2020
        || color_space == DXGI_COLOR_SPACE_YCBCR_STUDIO_G22_TOPLEFT_P2020
        || color_space == DXGI_COLOR_SPACE_YCBCR_STUDIO_G24_LEFT_P2020
        || color_space == DXGI_COLOR_SPACE_YCBCR_STUDIO_G24_TOPLEFT_P2020
    {
        let full_range = color_space == DXGI_COLOR_SPACE_RGB_FULL_G22_NONE_P2020
            || color_space == DXGI_COLOR_SPACE_YCBCR_FULL_G22_LEFT_P2020;
        DisplayRouteColor {
            hdr_pq: false,
            bit_depth: if detected_bits >= 10 { 10 } else { 8 },
            full_range,
            mp4_color: if detected_bits >= 10 {
                NclxColorMetadata::bt2020_sdr_10(full_range)
            } else {
                NclxColorMetadata::bt2020_sdr_8(full_range)
            },
            note: match (detected_bits >= 10, full_range) {
                (true, true) => "DXGI *_FULL_G22_P2020 -> BT.2020 SDR10/full",
                (true, false) => "DXGI *_STUDIO_G22/G24_P2020 -> BT.2020 SDR10/limited",
                (false, true) => "DXGI *_FULL_G22_P2020 -> BT.2020 SDR8/full",
                (false, false) => "DXGI *_STUDIO_G22/G24_P2020 -> BT.2020 SDR8/limited",
            },
        }
    } else if color_space == DXGI_COLOR_SPACE_YCBCR_FULL_G22_LEFT_P601
        || color_space == DXGI_COLOR_SPACE_YCBCR_STUDIO_G22_LEFT_P601
    {
        return Err(BackendError::unsupported(
            "录制路线选择",
            format!("BT.601 DXGI ColorSpace={}", color_space.0),
            "不支持的桌面模式",
        ));
    } else {
        return Err(BackendError::unsupported(
            "录制路线选择",
            format!("未知/自定义 DXGI ColorSpace={}", color_space.0),
            "不支持的桌面模式",
        ));
    };
    let routes = VplRecordRoute::candidates_for_requested_chroma_and_display(
        requested_chroma,
        display_color,
    );
    if routes.is_empty() {
        return Err(BackendError::unsupported(
            "录制路线选择",
            requested_chroma.doc_label(),
            "不支持的桌面模式",
        ));
    }
    notes.push(format!(
        "display route probe: ColorSpace={} BitsPerColor={} detected_color={} -> candidates=[{}]",
        color_space.0,
        desc1.BitsPerColor,
        display_color.note,
        routes
            .iter()
            .map(|route| route.summary())
            .collect::<Vec<_>>()
            .join("；")
    ));
    Ok(routes)
}

#[cfg(windows)]
fn select_record_route_for_output(
    output: &windows::Win32::Graphics::Dxgi::IDXGIOutput,
    requested_chroma: ChromaSampling,
    notes: &mut Vec<String>,
) -> Result<VplRecordRoute, BackendError> {
    select_record_route_candidates_for_output(output, requested_chroma, notes)?
        .into_iter()
        .next()
        .ok_or_else(|| {
            BackendError::unsupported(
                "录制路线选择",
                requested_chroma.doc_label(),
                "不支持的桌面模式",
            )
        })
}

fn record_route_from_display_plan(
    plan: &VplCurrentDisplayRouteInfo,
) -> Result<VplRecordRoute, BackendError> {
    let fourcc = fourcc_from_name(&plan.fourcc).ok_or_else(|| {
        BackendError::unsupported(
            "录制 RoutePlan",
            format!("FourCC={}", plan.fourcc),
            "不支持的桌面模式",
        )
    })?;
    if plan.vpl_chroma == 0 || plan.vpl_profile == 0 || plan.bit_depth == 0 {
        return Err(BackendError::unsupported(
            "录制 RoutePlan",
            plan.route_summary.clone(),
            "能力探测没有提供完整 route 元数据，请重新探测能力",
        ));
    }
    Ok(VplRecordRoute {
        label: "Probe RoutePlan",
        fourcc,
        chroma: plan.vpl_chroma,
        bit_depth: plan.bit_depth,
        profile: plan.vpl_profile,
        mp4_color: NclxColorMetadata {
            colour_primaries: plan.nclx_colour_primaries,
            transfer_characteristics: plan.nclx_transfer_characteristics,
            matrix_coefficients: plan.nclx_matrix_coefficients,
            full_range: plan.nclx_full_range,
        },
        mp4_codec: HevcCodecMetadata {
            profile_idc: plan.codec_profile_idc,
            chroma_format_idc: plan.codec_chroma_format_idc,
            bit_depth_luma_minus8: plan.codec_bit_depth_luma_minus8,
            bit_depth_chroma_minus8: plan.codec_bit_depth_chroma_minus8,
        },
    })
}

#[cfg(windows)]
fn validate_current_display_route_plan(
    plan: &VplCurrentDisplayRouteInfo,
    adapter_index: u32,
    output_desc: &windows::Win32::Graphics::Dxgi::DXGI_OUTPUT_DESC,
    output: &windows::Win32::Graphics::Dxgi::IDXGIOutput,
    requested_chroma: ChromaSampling,
) -> Result<(), BackendError> {
    use windows::Win32::Graphics::Dxgi::IDXGIOutput6;
    use windows::core::Interface;

    if plan.chroma != requested_chroma || plan.fourcc.is_empty() {
        return Err(BackendError::unsupported(
            "录制 RoutePlan",
            format!(
                "requested={} plan_chroma={} fourcc={}",
                requested_chroma.doc_label(),
                plan.chroma.doc_label(),
                plan.fourcc
            ),
            "能力探测没有当前色度的可录制 route，请重新探测能力",
        ));
    }
    if plan.adapter_index != adapter_index || plan.output_index != 0 {
        return Err(BackendError::unsupported(
            "录制 RoutePlan",
            format!(
                "plan adapter/output={}/{} current adapter/output={}/0",
                plan.adapter_index, plan.output_index, adapter_index
            ),
            "显示输出已变化，请重新探测能力",
        ));
    }
    let rect = output_desc.DesktopCoordinates;
    if plan.desktop_left != rect.left
        || plan.desktop_top != rect.top
        || plan.desktop_right != rect.right
        || plan.desktop_bottom != rect.bottom
    {
        return Err(BackendError::unsupported(
            "录制 RoutePlan",
            format!(
                "plan rect={},{},{},{} current rect={},{},{},{}",
                plan.desktop_left,
                plan.desktop_top,
                plan.desktop_right,
                plan.desktop_bottom,
                rect.left,
                rect.top,
                rect.right,
                rect.bottom
            ),
            "显示输出区域已变化，请重新探测能力",
        ));
    }
    let output6 = output.cast::<IDXGIOutput6>().map_err(|_| {
        BackendError::unsupported(
            "录制 RoutePlan",
            "IDXGIOutput6::GetDesc1",
            "不支持的桌面模式",
        )
    })?;
    let desc1 = unsafe {
        output6.GetDesc1().map_err(|err| BackendError::WindowsApi {
            func: "IDXGIOutput6::GetDesc1(record RoutePlan guard)",
            message: err.to_string(),
        })?
    };
    if plan.color_space != desc1.ColorSpace.0 as u32 || plan.bits_per_color != desc1.BitsPerColor {
        return Err(BackendError::unsupported(
            "录制 RoutePlan",
            format!(
                "plan ColorSpace={} BitsPerColor={} current ColorSpace={} BitsPerColor={}",
                plan.color_space, plan.bits_per_color, desc1.ColorSpace.0, desc1.BitsPerColor
            ),
            "显示色彩状态已变化，请重新探测能力",
        ));
    }
    Ok(())
}

#[cfg(windows)]
impl VplRecordRoute {
    fn try_dxgi_format(
        self,
    ) -> Result<windows::Win32::Graphics::Dxgi::Common::DXGI_FORMAT, BackendError> {
        use windows::Win32::Graphics::Dxgi::Common::{
            DXGI_FORMAT_AYUV, DXGI_FORMAT_B8G8R8A8_UNORM, DXGI_FORMAT_NV12, DXGI_FORMAT_P010,
            DXGI_FORMAT_Y210, DXGI_FORMAT_Y410, DXGI_FORMAT_YUY2,
        };
        let format = match self.fourcc {
            MFX_FOURCC_NV12 => DXGI_FORMAT_NV12,
            MFX_FOURCC_P010 => DXGI_FORMAT_P010,
            MFX_FOURCC_YUY2 => DXGI_FORMAT_YUY2,
            MFX_FOURCC_Y210 => DXGI_FORMAT_Y210,
            // DXGI 没有独立 P210 enum。不能把 P210 静默 alias 到 P016/P010：
            // P210 是 4:2:2 planar，P016/P010 是 4:2:0 planar，布局语义不同。
            MFX_FOURCC_P210 => {
                return Err(BackendError::unsupported(
                    "录制 route DXGI format",
                    self.summary(),
                    "不支持的桌面模式",
                ));
            }
            MFX_FOURCC_AYUV => DXGI_FORMAT_AYUV,
            MFX_FOURCC_Y410 => DXGI_FORMAT_Y410,
            MFX_FOURCC_RGB4 => DXGI_FORMAT_B8G8R8A8_UNORM,
            _ => {
                return Err(BackendError::unsupported(
                    "录制 route DXGI format",
                    self.summary(),
                    "不支持的桌面模式",
                ));
            }
        };
        Ok(format)
    }

    fn wgc_input_format(
        self,
    ) -> (
        windows::Graphics::DirectX::DirectXPixelFormat,
        windows::Win32::Graphics::Dxgi::Common::DXGI_FORMAT,
    ) {
        use windows::Graphics::DirectX::DirectXPixelFormat;
        use windows::Win32::Graphics::Dxgi::Common::{
            DXGI_FORMAT_B8G8R8A8_UNORM, DXGI_FORMAT_R16G16B16A16_FLOAT,
        };
        if self.requires_fp16_capture() {
            (
                DirectXPixelFormat::R16G16B16A16Float,
                DXGI_FORMAT_R16G16B16A16_FLOAT,
            )
        } else {
            (
                DirectXPixelFormat::B8G8R8A8UIntNormalized,
                DXGI_FORMAT_B8G8R8A8_UNORM,
            )
        }
    }

    fn accepts_unconverted_capture_format(
        self,
        format: windows::Win32::Graphics::Dxgi::Common::DXGI_FORMAT,
    ) -> bool {
        use windows::Win32::Graphics::Dxgi::Common::DXGI_FORMAT_R16G16B16A16_FLOAT;
        !self.requires_fp16_capture() || format == DXGI_FORMAT_R16G16B16A16_FLOAT
    }
}

const fn make_fourcc(a: u8, b: u8, c: u8, d: u8) -> u32 {
    (a as u32) | ((b as u32) << 8) | ((c as u32) << 16) | ((d as u32) << 24)
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct VplProbeInfo {
    pub available: bool,
    pub dll_path: Option<String>,
    pub load_error: Option<String>,
    pub implementations: Vec<VplImplementationInfo>,
    pub hevc_supported: bool,
    pub hevc_profiles: Vec<String>,
    pub input_fourcc: Vec<String>,
    pub chroma_candidates: Vec<ChromaSampling>,
    pub route_candidates: Vec<VplRouteProbe>,
    /// 根据当前 adapter0/output0 显示状态推导出的生产录制 route。GUI 只应展示
    /// 同时满足 oneVPL Query 和当前显示状态自动 route 的色度/码控。
    pub current_display_routes: Vec<VplCurrentDisplayRouteInfo>,
    pub rate_controls: Vec<RateControlMethod>,
    pub dx11_texture_input_seen: bool,
    pub warnings: Vec<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct VplImplementationInfo {
    pub index: u32,
    pub impl_name: String,
    pub api_version: String,
    pub implementation: String,
    pub acceleration_mode: String,
    pub vendor_id: u32,
    pub vendor_impl_id: u32,
    pub device_id: String,
    pub media_adapter_type: u16,
    pub hevc_supported: bool,
    pub hevc_profiles: Vec<String>,
    pub input_fourcc: Vec<String>,
    pub route_candidates: Vec<VplRouteProbe>,
    pub rate_controls: Vec<RateControlMethod>,
    pub dx11_texture_input_seen: bool,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct VplRouteProbe {
    pub fourcc: String,
    pub chroma: ChromaSampling,
    pub bit_depth: u16,
    pub profile: String,
    pub query_status: i32,
    pub query_iosurf_status: i32,
    pub num_frame_min: u16,
    pub num_frame_suggested: u16,
    pub query_supported: bool,
    pub query_iosurf_supported: bool,
    pub query_preserved_route: bool,
    pub production_record_supported: bool,
    /// production_record_supported=false 时给前端/日志展示的明确阻断原因。
    /// Query 可见不等于生产可录制；例如 P210 缺少 DXGI texture 格式表达。
    pub production_blocker: Option<String>,
    /// 在这条 FourCC/Chroma/Profile 生产 route 上逐项 MFXVideoENCODE_Query
    /// 确认可用的 oneVPL 内建码控模式。前端按当前 route/chroma 隐藏不可用模式。
    pub rate_controls: Vec<RateControlMethod>,
    /// 该 route 上额外码控字段的可见性探测。主 union 字段随 RateControlMethod
    /// 固定可见；这里仅描述 mfxExtCodingOption2/3 或可选开关。
    pub rate_control_features: Vec<VplRateControlFeatureProbe>,
    pub note: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct VplRateControlFeatureProbe {
    pub method: RateControlMethod,
    pub look_ahead_depth: bool,
    pub win_brc: bool,
    pub low_delay_brc: bool,
    pub max_frame_size: bool,
    pub mbbrc: bool,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct VplCurrentDisplayRouteInfo {
    pub adapter_index: u32,
    pub output_index: u32,
    pub color_space: u32,
    pub bits_per_color: u32,
    pub desktop_left: i32,
    pub desktop_top: i32,
    pub desktop_right: i32,
    pub desktop_bottom: i32,
    pub chroma: ChromaSampling,
    pub fourcc: String,
    pub bit_depth: u16,
    pub vpl_chroma: u16,
    pub vpl_profile: u16,
    pub profile: String,
    pub nclx_colour_primaries: u16,
    pub nclx_transfer_characteristics: u16,
    pub nclx_matrix_coefficients: u16,
    pub nclx_full_range: bool,
    pub codec_profile_idc: u8,
    pub codec_chroma_format_idc: u8,
    pub codec_bit_depth_luma_minus8: u8,
    pub codec_bit_depth_chroma_minus8: u8,
    pub route_summary: String,
    pub note: String,
}

impl VplProbeInfo {
    fn unavailable(error: String) -> Self {
        Self {
            available: false,
            dll_path: None,
            load_error: Some(error.clone()),
            implementations: Vec::new(),
            hevc_supported: false,
            hevc_profiles: Vec::new(),
            input_fourcc: Vec::new(),
            chroma_candidates: Vec::new(),
            route_candidates: Vec::new(),
            current_display_routes: Vec::new(),
            rate_controls: Vec::new(),
            dx11_texture_input_seen: false,
            warnings: vec![error],
        }
    }
}

pub fn probe_vpl() -> VplProbeInfo {
    let (api, dll_path) = match VplApi::load() {
        Ok(pair) => pair,
        Err(error) => return VplProbeInfo::unavailable(error),
    };

    let mut warnings = Vec::new();
    let mut implementations = Vec::new();
    let current_display_routes = match probe_current_display_record_routes(0) {
        Ok(routes) => routes,
        Err(err) => {
            warnings.push(format!(
                "当前显示器自动 route 探测失败；前端不按显示状态展示录制字段: {err}"
            ));
            Vec::new()
        }
    };
    let current_display_route_keys = current_display_routes
        .iter()
        .filter(|route| !route.fourcc.is_empty())
        .map(|route| {
            route_probe_key_parts(&route.fourcc, route.chroma, route.bit_depth, &route.profile)
        })
        .collect::<BTreeSet<_>>();

    unsafe {
        let loader = (api.mfx_load)();
        if loader.is_null() {
            return VplProbeInfo::unavailable(format!(
                "MFXLoad 返回空句柄，DLL={}",
                dll_path.display()
            ));
        }

        let mut index = 0u32;
        loop {
            let mut handle: MfxHDL = ptr::null_mut();
            let status = (api.mfx_enum_implementations)(
                loader,
                index,
                MFX_IMPLCAPS_IMPLDESCSTRUCTURE,
                &mut handle,
            );
            if status == MFX_ERR_NOT_FOUND {
                break;
            }
            if status != MFX_ERR_NONE {
                warnings.push(format!(
                    "MFXEnumImplementations({index}) 返回 status={status}"
                ));
                break;
            }
            if handle.is_null() {
                warnings.push(format!("MFXEnumImplementations({index}) 返回空描述"));
                index += 1;
                continue;
            }

            let desc = &*(handle as *const MfxImplDescription);
            implementations.push(parse_impl(
                index,
                desc,
                &api,
                loader,
                &current_display_route_keys,
                &mut warnings,
            ));

            let release_status = (api.mfx_release_impl_description)(loader, handle);
            if release_status != MFX_ERR_NONE {
                warnings.push(format!(
                    "MFXDispReleaseImplDescription({index}) 返回 status={release_status}"
                ));
            }
            index += 1;
        }
        (api.mfx_unload)(loader);
    }

    if implementations.is_empty() {
        warnings.push("oneVPL dispatcher 未枚举到任何实现".to_owned());
    }

    let mut hevc_profiles = BTreeSet::new();
    let mut input_fourcc = BTreeSet::new();
    let mut rate_controls = BTreeSet::new();
    let mut chroma_candidates = BTreeSet::new();
    let mut route_candidates = Vec::new();
    let mut route_candidate_keys = BTreeSet::new();
    let mut hevc_supported = false;
    let mut dx11_texture_input_seen = false;

    for imp in &implementations {
        if imp.hevc_supported {
            hevc_supported = true;
        }
        if imp.dx11_texture_input_seen {
            dx11_texture_input_seen = true;
        }
        for profile in &imp.hevc_profiles {
            hevc_profiles.insert(profile.clone());
        }
        for fourcc in &imp.input_fourcc {
            input_fourcc.insert(fourcc.clone());
            if let Some(chroma) = chroma_from_fourcc_name(fourcc) {
                chroma_candidates.insert(chroma);
            }
        }
        for method in &imp.rate_controls {
            rate_controls.insert(*method);
        }
        for route in &imp.route_candidates {
            let key = format!(
                "{}::{:?}::{}::{}",
                route.fourcc, route.chroma, route.bit_depth, route.profile
            );
            if route_candidate_keys.insert(key) {
                route_candidates.push(route.clone());
            }
        }
    }

    if hevc_supported && rate_controls.is_empty() {
        warnings.push("oneVPL 未经 MFXVideoENCODE_Query 确认任何可用 RateControlMethod；为避免展示不支持模式，GUI 将隐藏码控模式".to_owned());
    }
    if hevc_supported && !dx11_texture_input_seen {
        warnings.push(
            "HEVC 实现未报告 MFX_RESOURCE_DX11_TEXTURE 输入；不满足 D3D11 video-memory + 一次 GPU CopyResource 前提"
                .to_owned(),
        );
    }

    VplProbeInfo {
        available: true,
        dll_path: Some(dll_path.display().to_string()),
        load_error: None,
        implementations,
        hevc_supported,
        hevc_profiles: hevc_profiles.into_iter().collect(),
        input_fourcc: input_fourcc.into_iter().collect(),
        chroma_candidates: chroma_candidates.into_iter().collect(),
        route_candidates,
        current_display_routes,
        rate_controls: rate_controls.into_iter().collect(),
        dx11_texture_input_seen,
        warnings,
    }
}

#[cfg(windows)]
fn probe_current_display_record_routes(
    adapter_index: u32,
) -> Result<Vec<VplCurrentDisplayRouteInfo>, BackendError> {
    use windows::Win32::Graphics::Dxgi::{CreateDXGIFactory1, IDXGIFactory1, IDXGIOutput6};
    use windows::core::Interface;

    unsafe {
        let factory: IDXGIFactory1 =
            CreateDXGIFactory1().map_err(|err| BackendError::WindowsApi {
                func: "CreateDXGIFactory1(current display route)",
                message: err.to_string(),
            })?;
        let adapter =
            factory
                .EnumAdapters1(adapter_index)
                .map_err(|err| BackendError::WindowsApi {
                    func: "IDXGIFactory1::EnumAdapters1(current display route)",
                    message: err.to_string(),
                })?;
        let output = adapter
            .EnumOutputs(0)
            .map_err(|err| BackendError::WindowsApi {
                func: "IDXGIAdapter1::EnumOutputs(0 current display route)",
                message: err.to_string(),
            })?;
        let output_desc = output.GetDesc().map_err(|err| BackendError::WindowsApi {
            func: "IDXGIOutput::GetDesc(current display route)",
            message: err.to_string(),
        })?;
        let output6 = output.cast::<IDXGIOutput6>().map_err(|_| {
            BackendError::unsupported(
                "当前显示器 route 探测",
                "IDXGIOutput6::GetDesc1",
                "不支持的桌面模式",
            )
        })?;
        let desc1 = output6.GetDesc1().map_err(|err| BackendError::WindowsApi {
            func: "IDXGIOutput6::GetDesc1(current display route)",
            message: err.to_string(),
        })?;

        let mut routes = Vec::new();
        for chroma in [
            ChromaSampling::Yuv420,
            ChromaSampling::Yuv422,
            ChromaSampling::Yuv444,
        ] {
            let mut notes = Vec::new();
            match select_record_route_candidates_for_output(&output, chroma, &mut notes) {
                Ok(candidates) => {
                    for (index, route) in candidates.into_iter().enumerate() {
                        routes.push(VplCurrentDisplayRouteInfo {
                            adapter_index,
                            output_index: 0,
                            color_space: desc1.ColorSpace.0 as u32,
                            bits_per_color: desc1.BitsPerColor,
                            desktop_left: output_desc.DesktopCoordinates.left,
                            desktop_top: output_desc.DesktopCoordinates.top,
                            desktop_right: output_desc.DesktopCoordinates.right,
                            desktop_bottom: output_desc.DesktopCoordinates.bottom,
                            chroma,
                            fourcc: fourcc_to_string(route.fourcc),
                            bit_depth: route.bit_depth,
                            vpl_chroma: route.chroma,
                            vpl_profile: route.profile,
                            profile: hevc_profile_name(u32::from(route.profile)).to_owned(),
                            nclx_colour_primaries: route.mp4_color.colour_primaries,
                            nclx_transfer_characteristics: route.mp4_color.transfer_characteristics,
                            nclx_matrix_coefficients: route.mp4_color.matrix_coefficients,
                            nclx_full_range: route.mp4_color.full_range,
                            codec_profile_idc: route.mp4_codec.profile_idc,
                            codec_chroma_format_idc: route.mp4_codec.chroma_format_idc,
                            codec_bit_depth_luma_minus8: route.mp4_codec.bit_depth_luma_minus8,
                            codec_bit_depth_chroma_minus8: route.mp4_codec.bit_depth_chroma_minus8,
                            route_summary: route.summary(),
                            note: format!("{}；candidate_order={}", notes.join("；"), index + 1),
                        });
                    }
                }
                Err(err) => routes.push(VplCurrentDisplayRouteInfo {
                    adapter_index,
                    output_index: 0,
                    color_space: desc1.ColorSpace.0 as u32,
                    bits_per_color: desc1.BitsPerColor,
                    desktop_left: output_desc.DesktopCoordinates.left,
                    desktop_top: output_desc.DesktopCoordinates.top,
                    desktop_right: output_desc.DesktopCoordinates.right,
                    desktop_bottom: output_desc.DesktopCoordinates.bottom,
                    chroma,
                    fourcc: String::new(),
                    bit_depth: 0,
                    vpl_chroma: 0,
                    vpl_profile: 0,
                    profile: String::new(),
                    nclx_colour_primaries: 0,
                    nclx_transfer_characteristics: 0,
                    nclx_matrix_coefficients: 0,
                    nclx_full_range: false,
                    codec_profile_idc: 0,
                    codec_chroma_format_idc: 0,
                    codec_bit_depth_luma_minus8: 0,
                    codec_bit_depth_chroma_minus8: 0,
                    route_summary: format!("{} 当前显示状态无可用 route", chroma.doc_label()),
                    note: err.to_string(),
                }),
            }
        }
        Ok(routes)
    }
}

#[cfg(not(windows))]
fn probe_current_display_record_routes(
    _adapter_index: u32,
) -> Result<Vec<VplCurrentDisplayRouteInfo>, BackendError> {
    Ok(Vec::new())
}

struct VplApi {
    _library: Library,
    mfx_load: unsafe extern "C" fn() -> MfxLoader,
    mfx_unload: unsafe extern "C" fn(MfxLoader),
    mfx_enum_implementations: unsafe extern "C" fn(MfxLoader, u32, u32, *mut MfxHDL) -> i32,
    mfx_release_impl_description: unsafe extern "C" fn(MfxLoader, MfxHDL) -> i32,
    mfx_create_session: unsafe extern "C" fn(MfxLoader, u32, *mut MfxSession) -> i32,
    mfx_close: unsafe extern "C" fn(MfxSession) -> i32,
    mfx_video_encode_query:
        unsafe extern "C" fn(MfxSession, *mut MfxVideoParam, *mut MfxVideoParam) -> i32,
    mfx_video_encode_query_iosurf:
        unsafe extern "C" fn(MfxSession, *mut MfxVideoParam, *mut MfxFrameAllocRequest) -> i32,
    mfx_video_encode_init: unsafe extern "C" fn(MfxSession, *mut MfxVideoParam) -> i32,
    mfx_memory_get_surface_for_encode:
        unsafe extern "C" fn(MfxSession, *mut *mut MfxFrameSurface1) -> i32,
    mfx_video_encode_frame_async: unsafe extern "C" fn(
        MfxSession,
        *mut c_void,
        *mut MfxFrameSurface1,
        *mut MfxBitstream,
        *mut MfxSyncPoint,
    ) -> i32,
    mfx_video_core_sync_operation: unsafe extern "C" fn(MfxSession, MfxSyncPoint, u32) -> i32,
    mfx_video_encode_close: unsafe extern "C" fn(MfxSession) -> i32,
}

impl VplApi {
    fn load() -> Result<(Self, PathBuf), String> {
        let mut attempts = Vec::new();
        for path in candidate_dlls() {
            let result = unsafe { Library::new(&path) };
            match result {
                Ok(library) => {
                    let api = unsafe {
                        let mfx_load = *library
                            .get::<unsafe extern "C" fn() -> MfxLoader>(b"MFXLoad\0")
                            .map_err(|e| e.to_string())?;
                        let mfx_unload = *library
                            .get::<unsafe extern "C" fn(MfxLoader)>(b"MFXUnload\0")
                            .map_err(|e| e.to_string())?;
                        let mfx_enum_implementations = *library
                            .get::<unsafe extern "C" fn(MfxLoader, u32, u32, *mut MfxHDL) -> i32>(
                                b"MFXEnumImplementations\0",
                            )
                            .map_err(|e| e.to_string())?;
                        let mfx_release_impl_description = *library
                            .get::<unsafe extern "C" fn(MfxLoader, MfxHDL) -> i32>(
                                b"MFXDispReleaseImplDescription\0",
                            )
                            .map_err(|e| e.to_string())?;
                        let mfx_create_session = *library
                            .get::<unsafe extern "C" fn(MfxLoader, u32, *mut MfxSession) -> i32>(
                                b"MFXCreateSession\0",
                            )
                            .map_err(|e| e.to_string())?;
                        let mfx_close = *library
                            .get::<unsafe extern "C" fn(MfxSession) -> i32>(b"MFXClose\0")
                            .map_err(|e| e.to_string())?;
                        let mfx_video_encode_query = *library
                            .get::<unsafe extern "C" fn(
                                MfxSession,
                                *mut MfxVideoParam,
                                *mut MfxVideoParam,
                            ) -> i32>(b"MFXVideoENCODE_Query\0")
                            .map_err(|e| e.to_string())?;
                        let mfx_video_encode_query_iosurf = *library
                            .get::<unsafe extern "C" fn(
                                MfxSession,
                                *mut MfxVideoParam,
                                *mut MfxFrameAllocRequest,
                            ) -> i32>(b"MFXVideoENCODE_QueryIOSurf\0")
                            .map_err(|e| e.to_string())?;
                        let mfx_video_encode_init = *library
                            .get::<unsafe extern "C" fn(MfxSession, *mut MfxVideoParam) -> i32>(
                                b"MFXVideoENCODE_Init\0",
                            )
                            .map_err(|e| e.to_string())?;
                        let mfx_memory_get_surface_for_encode = *library
                            .get::<unsafe extern "C" fn(
                                MfxSession,
                                *mut *mut MfxFrameSurface1,
                            ) -> i32>(b"MFXMemory_GetSurfaceForEncode\0")
                            .map_err(|e| e.to_string())?;
                        let mfx_video_encode_frame_async = *library
                            .get::<unsafe extern "C" fn(
                                MfxSession,
                                *mut c_void,
                                *mut MfxFrameSurface1,
                                *mut MfxBitstream,
                                *mut MfxSyncPoint,
                            ) -> i32>(
                                b"MFXVideoENCODE_EncodeFrameAsync\0"
                            )
                            .map_err(|e| e.to_string())?;
                        let mfx_video_core_sync_operation = *library
                            .get::<unsafe extern "C" fn(MfxSession, MfxSyncPoint, u32) -> i32>(
                                b"MFXVideoCORE_SyncOperation\0",
                            )
                            .map_err(|e| e.to_string())?;
                        let mfx_video_encode_close = *library
                            .get::<unsafe extern "C" fn(MfxSession) -> i32>(
                                b"MFXVideoENCODE_Close\0",
                            )
                            .map_err(|e| e.to_string())?;
                        Self {
                            _library: library,
                            mfx_load,
                            mfx_unload,
                            mfx_enum_implementations,
                            mfx_release_impl_description,
                            mfx_create_session,
                            mfx_close,
                            mfx_video_encode_query,
                            mfx_video_encode_query_iosurf,
                            mfx_video_encode_init,
                            mfx_memory_get_surface_for_encode,
                            mfx_video_encode_frame_async,
                            mfx_video_core_sync_operation,
                            mfx_video_encode_close,
                        }
                    };
                    return Ok((api, path));
                }
                Err(err) => attempts.push(format!("{}: {err}", path.display())),
            }
        }
        Err(format!(
            "未能加载 oneVPL DLL；尝试路径: {}",
            attempts.join(" | ")
        ))
    }
}

fn candidate_dlls() -> Vec<PathBuf> {
    let mut out = Vec::new();
    if let Ok(path) = std::env::var("RUSTREPLAY_VPL_DLL") {
        out.push(PathBuf::from(path));
    }
    out.extend([
        PathBuf::from("libvpl-2.dll"),
        PathBuf::from("libvpl.dll"),
        PathBuf::from("vpl.dll"),
        PathBuf::from("onevpl.dll"),
        PathBuf::from(r"C:\Program Files (x86)\Intel\oneAPI\vpl\latest\bin\libvpl.dll"),
        PathBuf::from(r"C:\Program Files\Intel\oneAPI\vpl\latest\bin\libvpl.dll"),
        PathBuf::from(r"C:\msys64\ucrt64\bin\libvpl-2.dll"),
    ]);
    out
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct VplOneCopyRecordReport {
    pub adapter_index: u32,
    pub adapter_luid: String,
    pub output_path: String,
    pub width: u16,
    pub height: u16,
    pub duration_seconds: f32,
    pub captured_frames: u32,
    pub encoded_samples: u32,
    pub encoded_bytes: u64,
    pub audio_access_units: u32,
    pub audio_encoded_bytes: u64,
    pub dda_timeouts: u32,
    pub input_dxgi_format: u32,
    pub target_dxgi_format: u32,
    pub query_status: i32,
    pub init_status: i32,
    pub close_status: i32,
    pub first_get_surface_status: i32,
    pub video_processor_format_flags_in: u32,
    pub video_processor_format_flags_out: u32,
    pub notes: Vec<String>,
}

#[derive(Debug, Clone)]
pub struct VplOneCopyRecordOutput {
    pub report: VplOneCopyRecordReport,
    pub video_track: crate::backend::mp4_mux::HevcMp4Track,
    pub audio_track: Option<crate::backend::mp4_mux::AacLcMp4Track>,
}

#[derive(Debug, Clone, Copy)]
pub struct VplOutputTrackInfo {
    pub width: u16,
    pub height: u16,
    pub color: crate::backend::mp4_mux::NclxColorMetadata,
    pub codec: crate::backend::mp4_mux::HevcCodecMetadata,
}

pub trait VplOneCopyRecordSink {
    fn status(&mut self, _message: &str) {}
    fn video_track_started(&mut self, info: VplOutputTrackInfo);
    fn hevc_access_unit(&mut self, sample: &crate::backend::mp4_mux::HevcAccessUnit);
    fn aac_access_unit(&mut self, _sample: &crate::backend::mp4_mux::AacAccessUnit) {}
}

#[cfg(windows)]
pub fn record_d3d11_onecopy_mp4(
    adapter_index: u32,
    output: &Path,
    duration_seconds: f32,
    rate_control: &RateControlConfig,
    requested_chroma: ChromaSampling,
) -> Result<VplOneCopyRecordReport, BackendError> {
    record_d3d11_onecopy_mp4_cancelable(
        adapter_index,
        output,
        duration_seconds,
        rate_control,
        requested_chroma,
        None,
    )
}

#[cfg(windows)]
pub fn record_d3d11_onecopy_mp4_cancelable(
    adapter_index: u32,
    output: &Path,
    duration_seconds: f32,
    rate_control: &RateControlConfig,
    requested_chroma: ChromaSampling,
    external_stop: Option<std::sync::Arc<std::sync::atomic::AtomicBool>>,
) -> Result<VplOneCopyRecordReport, BackendError> {
    Ok(record_d3d11_onecopy_mp4_output_cancelable(
        adapter_index,
        output,
        duration_seconds,
        rate_control,
        requested_chroma,
        external_stop,
    )?
    .report)
}

#[cfg(windows)]
pub fn record_d3d11_onecopy_mp4_output_cancelable(
    adapter_index: u32,
    output: &Path,
    duration_seconds: f32,
    rate_control: &RateControlConfig,
    requested_chroma: ChromaSampling,
    external_stop: Option<std::sync::Arc<std::sync::atomic::AtomicBool>>,
) -> Result<VplOneCopyRecordOutput, BackendError> {
    record_d3d11_onecopy_mp4_impl(
        adapter_index,
        output,
        duration_seconds,
        rate_control,
        requested_chroma,
        RecordCaptureSource::Dda,
        external_stop,
        true,
        None,
        None,
    )
}

#[cfg(windows)]
pub fn record_d3d11_onecopy_memory_output_cancelable(
    adapter_index: u32,
    output: &Path,
    duration_seconds: f32,
    rate_control: &RateControlConfig,
    requested_chroma: ChromaSampling,
    external_stop: Option<std::sync::Arc<std::sync::atomic::AtomicBool>>,
) -> Result<VplOneCopyRecordOutput, BackendError> {
    record_d3d11_onecopy_memory_output_with_sink_cancelable(
        adapter_index,
        output,
        duration_seconds,
        rate_control,
        requested_chroma,
        external_stop,
        None,
        None,
    )
}

#[cfg(windows)]
#[allow(clippy::too_many_arguments)]
pub fn record_d3d11_onecopy_memory_output_with_sink_cancelable(
    adapter_index: u32,
    output: &Path,
    duration_seconds: f32,
    rate_control: &RateControlConfig,
    requested_chroma: ChromaSampling,
    external_stop: Option<std::sync::Arc<std::sync::atomic::AtomicBool>>,
    encoded_sink: Option<&mut dyn VplOneCopyRecordSink>,
    route_plan: Option<&VplCurrentDisplayRouteInfo>,
) -> Result<VplOneCopyRecordOutput, BackendError> {
    record_d3d11_onecopy_mp4_impl(
        adapter_index,
        output,
        duration_seconds,
        rate_control,
        requested_chroma,
        RecordCaptureSource::Dda,
        external_stop,
        false,
        encoded_sink,
        route_plan,
    )
}

#[cfg(windows)]
pub fn record_wgc_d3d11_onecopy_mp4(
    adapter_index: u32,
    output: &Path,
    duration_seconds: f32,
    rate_control: &RateControlConfig,
    requested_chroma: ChromaSampling,
) -> Result<VplOneCopyRecordReport, BackendError> {
    record_wgc_d3d11_onecopy_mp4_cancelable(
        adapter_index,
        output,
        duration_seconds,
        rate_control,
        requested_chroma,
        None,
    )
}

#[cfg(windows)]
pub fn record_wgc_d3d11_onecopy_mp4_cancelable(
    adapter_index: u32,
    output: &Path,
    duration_seconds: f32,
    rate_control: &RateControlConfig,
    requested_chroma: ChromaSampling,
    external_stop: Option<std::sync::Arc<std::sync::atomic::AtomicBool>>,
) -> Result<VplOneCopyRecordReport, BackendError> {
    Ok(record_wgc_d3d11_onecopy_mp4_output_cancelable(
        adapter_index,
        output,
        duration_seconds,
        rate_control,
        requested_chroma,
        external_stop,
    )?
    .report)
}

#[cfg(windows)]
pub fn record_wgc_d3d11_onecopy_mp4_output_cancelable(
    adapter_index: u32,
    output: &Path,
    duration_seconds: f32,
    rate_control: &RateControlConfig,
    requested_chroma: ChromaSampling,
    external_stop: Option<std::sync::Arc<std::sync::atomic::AtomicBool>>,
) -> Result<VplOneCopyRecordOutput, BackendError> {
    record_d3d11_onecopy_mp4_impl(
        adapter_index,
        output,
        duration_seconds,
        rate_control,
        requested_chroma,
        RecordCaptureSource::Wgc,
        external_stop,
        true,
        None,
        None,
    )
}

#[cfg(windows)]
pub fn record_wgc_d3d11_onecopy_memory_output_cancelable(
    adapter_index: u32,
    output: &Path,
    duration_seconds: f32,
    rate_control: &RateControlConfig,
    requested_chroma: ChromaSampling,
    external_stop: Option<std::sync::Arc<std::sync::atomic::AtomicBool>>,
) -> Result<VplOneCopyRecordOutput, BackendError> {
    record_wgc_d3d11_onecopy_memory_output_with_sink_cancelable(
        adapter_index,
        output,
        duration_seconds,
        rate_control,
        requested_chroma,
        external_stop,
        None,
        None,
    )
}

#[cfg(windows)]
#[allow(clippy::too_many_arguments)]
pub fn record_wgc_d3d11_onecopy_memory_output_with_sink_cancelable(
    adapter_index: u32,
    output: &Path,
    duration_seconds: f32,
    rate_control: &RateControlConfig,
    requested_chroma: ChromaSampling,
    external_stop: Option<std::sync::Arc<std::sync::atomic::AtomicBool>>,
    encoded_sink: Option<&mut dyn VplOneCopyRecordSink>,
    route_plan: Option<&VplCurrentDisplayRouteInfo>,
) -> Result<VplOneCopyRecordOutput, BackendError> {
    record_d3d11_onecopy_mp4_impl(
        adapter_index,
        output,
        duration_seconds,
        rate_control,
        requested_chroma,
        RecordCaptureSource::Wgc,
        external_stop,
        false,
        encoded_sink,
        route_plan,
    )
}

#[cfg(windows)]
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum RecordCaptureSource {
    Dda,
    Wgc,
}

#[cfg(windows)]
impl RecordCaptureSource {
    fn is_wgc(self) -> bool {
        matches!(self, Self::Wgc)
    }

    const fn label(self) -> &'static str {
        match self {
            Self::Dda => "DDA",
            Self::Wgc => "WGC",
        }
    }
}

#[cfg(windows)]
type RecordAudioCaptureResult = (
    crate::backend::audio::AudioSourceKind,
    Result<crate::backend::wasapi::WasapiCaptureStats, BackendError>,
);

#[cfg(windows)]
type RecordAudioCaptureHandle = std::thread::JoinHandle<RecordAudioCaptureResult>;

#[cfg(windows)]
struct RecordAudioCapture {
    stop: std::sync::Arc<std::sync::atomic::AtomicBool>,
    handles: Vec<RecordAudioCaptureHandle>,
    rx: std::sync::mpsc::Receiver<crate::backend::audio::PcmFrame>,
    frames: Vec<crate::backend::audio::PcmFrame>,
    live_encoder: Option<crate::backend::aac_mf::MfAacLcEncoder>,
    live_blocker: crate::backend::audio::AacBlocker,
    live_submitted_until_ticks: u64,
    live_pushed_until_ticks: u64,
    live_failed: bool,
    audio_end_abs_100ns: Option<i64>,
}

#[cfg(windows)]
impl RecordAudioCapture {
    const LIVE_SAFETY_100NS: i64 = 1_000_000; // 100ms，避免麦克风/loopback 较晚 packet 改写已推送 AAC。

    fn start(duration: std::time::Duration, notes: &mut Vec<String>) -> Option<Self> {
        if std::env::var("RUST_REPLAY_AUDIO")
            .ok()
            .is_some_and(|value| value == "0" || value.eq_ignore_ascii_case("false"))
        {
            notes.push("音频采集被 RUST_REPLAY_AUDIO=0 显式关闭".to_owned());
            return None;
        }
        let stop = std::sync::Arc::new(std::sync::atomic::AtomicBool::new(false));
        let (tx, rx) = std::sync::mpsc::channel();
        let mut handles = Vec::new();
        for source in [
            crate::backend::audio::AudioSourceKind::Loopback,
            crate::backend::audio::AudioSourceKind::Microphone,
        ] {
            let stop_for_thread = stop.clone();
            let tx_for_thread = tx.clone();
            handles.push(std::thread::spawn(move || {
                let result = crate::backend::wasapi::capture_default_streaming(
                    source,
                    duration,
                    Some(stop_for_thread),
                    tx_for_thread,
                );
                (source, result)
            }));
        }
        drop(tx);
        notes.push(
            "音频路径已并行启动：WASAPI loopback + 默认麦克风，按 QPC/100ns 绝对时间戳流式采集；录制后端内部按首个正式视频源时间戳裁剪/重基准并实时推送 AAC 到 encoded ring"
                .to_owned(),
        );
        Some(Self {
            stop,
            handles,
            rx,
            frames: Vec::new(),
            live_encoder: None,
            live_blocker: crate::backend::audio::AacBlocker::default(),
            live_submitted_until_ticks: 0,
            live_pushed_until_ticks: 0,
            live_failed: false,
            audio_end_abs_100ns: None,
        })
    }

    fn drain_incoming(&mut self) {
        while let Ok(frame) = self.rx.try_recv() {
            let end = frame.end_time_100ns();
            self.audio_end_abs_100ns = Some(
                self.audio_end_abs_100ns
                    .map(|current| current.max(end))
                    .unwrap_or(end),
            );
            self.frames.push(frame);
        }
    }

    fn poll_live_aac(
        &mut self,
        first_video_timestamp_100ns: Option<i64>,
        current_video_timestamp_90k: Option<u64>,
        encoded_sink: &mut Option<&mut dyn VplOneCopyRecordSink>,
        notes: &mut Vec<String>,
    ) {
        self.drain_incoming();
        if encoded_sink.is_none() || self.live_failed {
            return;
        }
        if let Err(err) = self.poll_live_aac_inner(
            first_video_timestamp_100ns,
            current_video_timestamp_90k,
            encoded_sink,
        ) {
            self.live_failed = true;
            notes.push(format!(
                "实时 AAC ring 推送不可用：{err}；最终 MP4 音轨仍在段结束时由完整 WASAPI PCM 重新编码"
            ));
        }
    }

    fn poll_live_aac_inner(
        &mut self,
        first_video_timestamp_100ns: Option<i64>,
        current_video_timestamp_90k: Option<u64>,
        encoded_sink: &mut Option<&mut dyn VplOneCopyRecordSink>,
    ) -> Result<(), BackendError> {
        use crate::backend::audio::{AAC_LC_FRAME_SAMPLES, mix_window_samples_to_stereo_48k};

        let Some(video_start_100ns) = first_video_timestamp_100ns else {
            return Ok(());
        };
        let Some(video_timestamp_90k) = current_video_timestamp_90k else {
            return Ok(());
        };
        let Some(audio_end_abs_100ns) = self.audio_end_abs_100ns else {
            return Ok(());
        };
        let audio_ready_100ns = audio_end_abs_100ns
            .saturating_sub(video_start_100ns)
            .saturating_sub(Self::LIVE_SAFETY_100NS)
            .max(0);
        let video_ready_100ns = video_90k_to_100ns(video_timestamp_90k).max(0);
        let ready_100ns = audio_ready_100ns.min(video_ready_100ns);
        let ready_ticks = audio_100ns_to_ticks(ready_100ns);
        let encode_until_ticks =
            (ready_ticks / AAC_LC_FRAME_SAMPLES as u64) * AAC_LC_FRAME_SAMPLES as u64;
        if encode_until_ticks <= self.live_submitted_until_ticks {
            return Ok(());
        }

        let from_ticks = self.live_submitted_until_ticks;
        let window_ticks = encode_until_ticks.saturating_sub(from_ticks);
        if window_ticks == 0 {
            return Ok(());
        }
        let window_start_offset_100ns = audio_ticks_to_100ns(from_ticks);
        let window_start_abs_100ns = video_start_100ns.saturating_add(window_start_offset_100ns);
        let mut mixed = mix_window_samples_to_stereo_48k(
            &self.frames,
            window_start_abs_100ns,
            window_ticks as usize,
        )?;
        mixed.start_time_100ns = window_start_offset_100ns;
        let blocks = self.live_blocker.push(&mixed);
        if self.live_encoder.is_none() {
            self.live_encoder = Some(crate::backend::aac_mf::MfAacLcEncoder::new()?);
        }
        let encoder = self.live_encoder.as_mut().expect("created above");
        let mut submitted_until_ticks = self.live_submitted_until_ticks;
        for block in &blocks {
            if block.timestamp_ticks < submitted_until_ticks
                || block.timestamp_ticks >= encode_until_ticks
            {
                continue;
            }
            for sample in encoder.encode_block(block)? {
                if let Some(sink) = encoded_sink.as_deref_mut() {
                    sink.aac_access_unit(&sample);
                }
                self.live_pushed_until_ticks = self.live_pushed_until_ticks.max(
                    sample
                        .timestamp_ticks
                        .saturating_add(u64::from(sample.duration_ticks)),
                );
            }
            submitted_until_ticks = block
                .timestamp_ticks
                .saturating_add(AAC_LC_FRAME_SAMPLES as u64);
        }
        self.live_submitted_until_ticks = submitted_until_ticks;
        self.prune_live_pcm_frames(video_start_100ns);
        Ok(())
    }

    fn prune_live_pcm_frames(&mut self, video_start_100ns: i64) {
        let keep_from_ticks = self
            .live_submitted_until_ticks
            .saturating_sub(crate::backend::audio::TARGET_SAMPLE_RATE as u64);
        let keep_from_100ns =
            video_start_100ns.saturating_add(audio_ticks_to_100ns(keep_from_ticks));
        self.frames
            .retain(|frame| frame.end_time_100ns() >= keep_from_100ns);
    }

    fn finish_live_aac(
        &mut self,
        encoded_sink: &mut Option<&mut dyn VplOneCopyRecordSink>,
        notes: &mut Vec<String>,
    ) {
        if let Some(encoder) = self.live_encoder.as_mut()
            && let Some(block) = self.live_blocker.flush_padded()
        {
            match encoder.encode_block(&block) {
                Ok(samples) => {
                    for sample in samples {
                        if let Some(sink) = encoded_sink.as_deref_mut() {
                            sink.aac_access_unit(&sample);
                        }
                        self.live_pushed_until_ticks = self.live_pushed_until_ticks.max(
                            sample
                                .timestamp_ticks
                                .saturating_add(u64::from(sample.duration_ticks)),
                        );
                    }
                }
                Err(err) => notes.push(format!("实时 AAC ring 尾块编码失败：{err}")),
            }
        }
        if let Some(encoder) = self.live_encoder.take() {
            match encoder.finish() {
                Ok(samples) => {
                    let mut pushed = 0usize;
                    for sample in samples {
                        if let Some(sink) = encoded_sink.as_deref_mut() {
                            sink.aac_access_unit(&sample);
                        }
                        self.live_pushed_until_ticks = self.live_pushed_until_ticks.max(
                            sample
                                .timestamp_ticks
                                .saturating_add(u64::from(sample.duration_ticks)),
                        );
                        pushed += 1;
                    }
                    if pushed > 0 {
                        notes.push(format!(
                            "实时 AAC ring flush 推送 access_units={} pushed_until_ticks={}",
                            pushed, self.live_pushed_until_ticks
                        ));
                    }
                }
                Err(err) => notes.push(format!("实时 AAC ring flush 失败：{err}")),
            }
        }
    }

    fn live_pushed_until_ticks(&self) -> u64 {
        self.live_pushed_until_ticks
    }

    fn finish(&mut self, notes: &mut Vec<String>) -> Vec<crate::backend::audio::PcmFrame> {
        self.drain_incoming();
        self.stop.store(true, std::sync::atomic::Ordering::Relaxed);
        while let Some(handle) = self.handles.pop() {
            match handle.join() {
                Ok((source, Ok(stats))) => {
                    notes.push(format!(
                        "WASAPI {:?} 捕获 packet={} pcm_frames={}",
                        source, stats.packet_count, stats.pcm_frames
                    ));
                }
                Ok((source, Err(err))) => {
                    notes.push(format!("WASAPI {:?} 捕获不可用：{err}", source));
                }
                Err(_) => notes.push("WASAPI 捕获线程 panic；该音源被跳过".to_owned()),
            }
            self.drain_incoming();
        }
        self.drain_incoming();
        std::mem::take(&mut self.frames)
    }

    fn stop_without_reencode(&mut self, notes: &mut Vec<String>) {
        let started = std::time::Instant::now();
        self.stop.store(true, std::sync::atomic::Ordering::Relaxed);
        while let Some(handle) = self.handles.pop() {
            match handle.join() {
                Ok((source, Ok(stats))) => notes.push(format!(
                    "WASAPI {:?} 快速停止：packet={} pcm_frames={}",
                    source, stats.packet_count, stats.pcm_frames
                )),
                Ok((source, Err(err))) => {
                    notes.push(format!("WASAPI {:?} 快速停止时不可用：{err}", source));
                }
                Err(_) => notes.push("WASAPI 快速停止时捕获线程 panic；该音源被跳过".to_owned()),
            }
        }
        self.drain_incoming();
        self.frames.clear();
        notes.push(format!(
            "音频快速停止完成：跳过停止时完整 AAC 重建，耗时 {:.1}ms",
            started.elapsed().as_secs_f64() * 1000.0
        ));
    }
}

#[cfg(windows)]
impl Drop for RecordAudioCapture {
    fn drop(&mut self) {
        self.stop.store(true, std::sync::atomic::Ordering::Relaxed);
        while let Some(handle) = self.handles.pop() {
            let _ = handle.join();
        }
    }
}

#[cfg(windows)]
fn build_record_aac_track(
    audio_frames: Vec<crate::backend::audio::PcmFrame>,
    first_video_timestamp_100ns: Option<i64>,
    video_duration_90k: u64,
    notes: &mut Vec<String>,
    encoded_sink: &mut Option<&mut dyn VplOneCopyRecordSink>,
    sink_push_from_ticks: u64,
) -> Result<Option<crate::backend::mp4_mux::AacLcMp4Track>, BackendError> {
    use crate::backend::audio::{
        AacBlocker, TARGET_CHANNELS, TARGET_SAMPLE_RATE, mix_window_samples_to_stereo_48k,
    };
    use crate::backend::mp4_mux::AacLcMp4Track;

    let Some(video_start_100ns) = first_video_timestamp_100ns else {
        notes.push("音频封装跳过：视频路径没有可与 WASAPI QPC 对齐的首帧绝对时间戳".to_owned());
        return Ok(None);
    };
    let audio_duration_ticks = video_90k_to_audio_ticks(video_duration_90k).max(1);
    let audio_duration_100ns = video_90k_to_100ns(video_duration_90k).max(1);
    let audio_end_100ns = video_start_100ns.saturating_add(audio_duration_100ns);
    let clipped_packets = audio_frames
        .iter()
        .filter(|frame| {
            frame.end_time_100ns() > video_start_100ns && frame.start_time_100ns < audio_end_100ns
        })
        .count();
    let clipped_pcm_frames = audio_frames
        .iter()
        .filter(|frame| {
            frame.end_time_100ns() > video_start_100ns && frame.start_time_100ns < audio_end_100ns
        })
        .map(|frame| frame.frame_count())
        .sum::<usize>();
    let mut encoder = crate::backend::aac_mf::MfAacLcEncoder::new()?;
    let mut blocker = AacBlocker::default();
    let mut samples = Vec::new();
    let mut mixed_48k_frames = 0u64;
    let mut aac_blocks = 0usize;
    let mut cursor_ticks = 0u64;
    const AAC_MIX_CHUNK_TICKS: u64 = TARGET_SAMPLE_RATE as u64;
    while cursor_ticks < audio_duration_ticks {
        let chunk_ticks = (audio_duration_ticks - cursor_ticks).min(AAC_MIX_CHUNK_TICKS);
        let window_start_abs_100ns =
            video_start_100ns.saturating_add(audio_ticks_to_100ns(cursor_ticks));
        let mut mixed = mix_window_samples_to_stereo_48k(
            &audio_frames,
            window_start_abs_100ns,
            chunk_ticks as usize,
        )?;
        mixed.start_time_100ns = audio_ticks_to_100ns(cursor_ticks);
        mixed_48k_frames = mixed_48k_frames.saturating_add(mixed.samples.len() as u64);
        for block in blocker.push(&mixed) {
            aac_blocks += 1;
            for sample in encoder.encode_block(&block)? {
                push_record_aac_sample(&mut samples, sample, encoded_sink, sink_push_from_ticks);
            }
        }
        cursor_ticks = cursor_ticks.saturating_add(chunk_ticks);
    }
    if let Some(block) = blocker.flush_padded() {
        aac_blocks += 1;
        for sample in encoder.encode_block(&block)? {
            push_record_aac_sample(&mut samples, sample, encoded_sink, sink_push_from_ticks);
        }
    }
    for sample in encoder.finish()? {
        push_record_aac_sample(&mut samples, sample, encoded_sink, sink_push_from_ticks);
    }
    if samples.is_empty() {
        notes.push("音频封装跳过：AAC encoder 没有输出 access unit".to_owned());
        return Ok(None);
    }
    let final_duration_ticks = samples
        .iter()
        .map(|sample| u64::from(sample.duration_ticks))
        .sum::<u64>()
        .max(1);
    let aac_padding_ticks = final_duration_ticks.saturating_sub(audio_duration_ticks);
    notes.push(format!(
        "音频同步：video_start_qpc100ns={} video_duration_90k={} requested_audio_ticks={} final_aac_ticks={} aac_padding_ticks={} clipped_packets={} clipped_pcm_frames={} mixed_48k_frames={} aac_blocks={}",
        video_start_100ns,
        video_duration_90k,
        audio_duration_ticks,
        final_duration_ticks,
        aac_padding_ticks,
        clipped_packets,
        clipped_pcm_frames,
        mixed_48k_frames,
        aac_blocks
    ));
    if sink_push_from_ticks > 0 {
        notes.push(format!(
            "音频 ring 去重：段结束完整 AAC 重新编码后，只把 timestamp_ticks>={sink_push_from_ticks} 的尾部 AU 推给 encoded ring，避免与实时 AAC 重复"
        ));
    }
    Ok(Some(AacLcMp4Track {
        sample_rate: TARGET_SAMPLE_RATE,
        channel_count: TARGET_CHANNELS,
        duration_ticks: final_duration_ticks,
        samples,
    }))
}

#[cfg(windows)]
fn push_record_aac_sample(
    samples: &mut Vec<crate::backend::mp4_mux::AacAccessUnit>,
    sample: crate::backend::mp4_mux::AacAccessUnit,
    encoded_sink: &mut Option<&mut dyn VplOneCopyRecordSink>,
    sink_push_from_ticks: u64,
) {
    if sample.timestamp_ticks >= sink_push_from_ticks
        && let Some(sink) = encoded_sink.as_deref_mut()
    {
        sink.aac_access_unit(&sample);
    }
    samples.push(sample);
}

#[cfg(windows)]
fn encoder_frame_rate_hint_from_output(
    output_desc: &windows::Win32::Graphics::Dxgi::DXGI_OUTPUT_DESC,
) -> (u32, u32, String) {
    use windows::Win32::Graphics::Gdi::{DEVMODEW, ENUM_CURRENT_SETTINGS, EnumDisplaySettingsW};
    use windows::core::PCWSTR;

    let mut devmode = DEVMODEW::default();
    devmode.dmSize = std::mem::size_of::<DEVMODEW>() as u16;
    let ok = unsafe {
        EnumDisplaySettingsW(
            PCWSTR(output_desc.DeviceName.as_ptr()),
            ENUM_CURRENT_SETTINGS,
            &mut devmode,
        )
        .as_bool()
    };
    if ok && devmode.dmDisplayFrequency > 0 {
        let hz = devmode.dmDisplayFrequency as u32;
        (
            hz,
            1,
            format!("EnumDisplaySettingsW current dmDisplayFrequency={}Hz", hz),
        )
    } else {
        (
            VPL_ENCODER_FRAME_RATE_HINT_FALLBACK_N,
            VPL_ENCODER_FRAME_RATE_HINT_FALLBACK_D,
            "EnumDisplaySettingsW 未返回有效刷新率，使用 oneVPL 码控提示 fallback 60/1".to_owned(),
        )
    }
}

#[cfg(windows)]
#[allow(clippy::too_many_arguments)]
fn record_d3d11_onecopy_mp4_impl(
    adapter_index: u32,
    output: &Path,
    duration_seconds: f32,
    rate_control: &RateControlConfig,
    requested_chroma: ChromaSampling,
    capture_source: RecordCaptureSource,
    external_stop: Option<std::sync::Arc<std::sync::atomic::AtomicBool>>,
    write_output_mp4: bool,
    mut encoded_sink: Option<&mut dyn VplOneCopyRecordSink>,
    route_plan: Option<&VplCurrentDisplayRouteInfo>,
) -> Result<VplOneCopyRecordOutput, BackendError> {
    use crate::backend::mp4_mux::{HevcMp4Track, write_hevc_aac_mp4};
    use std::time::{Duration, Instant};
    use windows::Win32::Graphics::Direct3D11::{
        D3D11_TEXTURE2D_DESC, ID3D11Device, ID3D11DeviceContext, ID3D11Texture2D,
    };
    use windows::Win32::Graphics::Dxgi::{CreateDXGIFactory1, IDXGIFactory1};
    use windows::core::Interface;

    validate_rate_control_config(rate_control)?;

    let record_started = Instant::now();
    let record_stop = external_stop
        .clone()
        .unwrap_or_else(|| std::sync::Arc::new(std::sync::atomic::AtomicBool::new(false)));
    sink_status(
        &mut encoded_sink,
        format!(
            "录制后端初始化开始：capture={} chroma={} rc={}",
            capture_source.label(),
            requested_chroma.doc_label(),
            rate_control.method.short_name()
        ),
    );
    if record_stop.load(std::sync::atomic::Ordering::Relaxed) {
        return Err(BackendError::unsupported(
            "录制后端初始化",
            "用户停止请求",
            "停止请求发生在 oneVPL/D3D11 初始化前，已中止当前片段",
        ));
    }

    let phase_started = Instant::now();
    let (api, dll_path) =
        VplApi::load().map_err(|err| BackendError::unsupported("oneVPL", "DLL", err))?;
    sink_status(
        &mut encoded_sink,
        format!(
            "初始化阶段：加载 oneVPL DLL 完成，用时 {:.1}ms",
            phase_started.elapsed().as_secs_f64() * 1000.0
        ),
    );
    let mut notes = vec![format!("oneVPL DLL: {}", dll_path.display())];

    unsafe {
        let _thread_priority = match capture_source {
            RecordCaptureSource::Dda => RecordThreadPriorityGuard::raise(&mut notes),
            RecordCaptureSource::Wgc => {
                notes.push(
                    "WGC 录制主线程/FrameArrived 回调/捕获线程固定保持普通 CPU 优先级".to_owned(),
                );
                None
            }
        };
        let factory: IDXGIFactory1 =
            CreateDXGIFactory1().map_err(|err| BackendError::WindowsApi {
                func: "CreateDXGIFactory1",
                message: err.to_string(),
            })?;
        let adapter1 =
            factory
                .EnumAdapters1(adapter_index)
                .map_err(|err| BackendError::WindowsApi {
                    func: "IDXGIFactory1::EnumAdapters1",
                    message: err.to_string(),
                })?;
        let desc = adapter1
            .GetDesc1()
            .map_err(|err| BackendError::WindowsApi {
                func: "IDXGIAdapter1::GetDesc1",
                message: err.to_string(),
            })?;
        let adapter_luid = format!(
            "{:08X}:{:08X}",
            desc.AdapterLuid.HighPart as u32, desc.AdapterLuid.LowPart
        );
        let output0 = adapter1
            .EnumOutputs(0)
            .map_err(|err| BackendError::WindowsApi {
                func: "IDXGIAdapter1::EnumOutputs(0)",
                message: err.to_string(),
            })?;
        let output_desc = output0.GetDesc().map_err(|err| BackendError::WindowsApi {
            func: "IDXGIOutput::GetDesc",
            message: err.to_string(),
        })?;
        let capture_width = (output_desc.DesktopCoordinates.right
            - output_desc.DesktopCoordinates.left)
            .max(1) as u16;
        let capture_height = (output_desc.DesktopCoordinates.bottom
            - output_desc.DesktopCoordinates.top)
            .max(1) as u16;
        let aligned_width = align16(capture_width);
        let aligned_height = align16(capture_height);
        let (encoder_frame_rate_n, encoder_frame_rate_d, encoder_frame_rate_note) =
            encoder_frame_rate_hint_from_output(&output_desc);
        notes.push(format!(
            "oneVPL FrameRateExt 仅作为编码器码控/HRD 提示：{}/{}；来源={}；正式 MP4 时间戳保持 DDA/WGC 源 VFR 节奏",
            encoder_frame_rate_n, encoder_frame_rate_d, encoder_frame_rate_note
        ));
        // 自动同步路线集中在录制后端内部：前端只表达目标色度采样；
        // 能力探测阶段已经把当前显示器状态解析为 RoutePlan。启动时只做
        // 轻量一致性校验，避免重复完整 route 现场探测。
        let route_probe_started = Instant::now();
        let record_route_candidates = if let Some(plan) = route_plan {
            validate_current_display_route_plan(
                plan,
                adapter_index,
                &output_desc,
                &output0,
                requested_chroma,
            )?;
            let route = record_route_from_display_plan(plan)?;
            notes.push(format!(
                "record RoutePlan accepted: adapter={} output={} ColorSpace={} BitsPerColor={} rect={},{},{},{} route={}",
                plan.adapter_index,
                plan.output_index,
                plan.color_space,
                plan.bits_per_color,
                plan.desktop_left,
                plan.desktop_top,
                plan.desktop_right,
                plan.desktop_bottom,
                route.summary()
            ));
            vec![route]
        } else {
            notes.push(
                "record RoutePlan missing; falling back to startup-time display route probing"
                    .to_owned(),
            );
            select_record_route_candidates_for_output(&output0, requested_chroma, &mut notes)?
        };
        sink_status(
            &mut encoded_sink,
            format!(
                "初始化阶段：DXGI 输出/桌面模式 RoutePlan 准备完成，候选={}，累计 {:.1}ms，本阶段 {:.1}ms",
                record_route_candidates.len(),
                record_started.elapsed().as_secs_f64() * 1000.0,
                route_probe_started.elapsed().as_secs_f64() * 1000.0
            ),
        );
        if record_route_candidates
            .iter()
            .any(|route| !route.supports_requested_chroma(requested_chroma))
        {
            return Err(BackendError::unsupported(
                "录制路线选择",
                requested_chroma.doc_label(),
                "内部选择出的候选路线不匹配请求色度",
            ));
        }

        let loader = (api.mfx_load)();
        if loader.is_null() {
            return Err(BackendError::unsupported(
                "oneVPL record",
                "MFXLoad",
                "返回空 loader",
            ));
        }
        if record_stop.load(std::sync::atomic::Ordering::Relaxed) {
            (api.mfx_unload)(loader);
            return Err(BackendError::unsupported(
                "录制后端初始化",
                "用户停止请求",
                "停止请求发生在 oneVPL loader 创建后，已中止当前片段",
            ));
        }
        let session_started = Instant::now();
        let mut session: MfxSession = ptr::null_mut();
        let create_status = (api.mfx_create_session)(loader, 0, &mut session);
        if create_status != MFX_ERR_NONE || session.is_null() {
            (api.mfx_unload)(loader);
            return Err(BackendError::VplStatus {
                func: "MFXCreateSession",
                status: create_status,
            });
        }
        sink_status(
            &mut encoded_sink,
            format!(
                "初始化阶段：MFXCreateSession 完成，累计 {:.1}ms，本阶段 {:.1}ms",
                record_started.elapsed().as_secs_f64() * 1000.0,
                session_started.elapsed().as_secs_f64() * 1000.0
            ),
        );
        let record_async_depth = std::env::var("RUST_REPLAY_VPL_ASYNC_DEPTH")
            .ok()
            .and_then(|value| value.parse::<u16>().ok())
            .unwrap_or(if capture_source.is_wgc() {
                2
            } else {
                VPL_RECORD_ASYNC_DEPTH
            })
            .clamp(2, VPL_RECORD_ASYNC_DEPTH);

        let record_gop_pic_size = std::env::var("RUST_REPLAY_VPL_GOP_PIC_SIZE")
            .ok()
            .and_then(|value| value.parse::<u16>().ok())
            .unwrap_or(60)
            .clamp(1, u16::MAX);
        let record_gop_ref_dist = std::env::var("RUST_REPLAY_VPL_GOP_REF_DIST")
            .ok()
            .and_then(|value| value.parse::<u16>().ok())
            .unwrap_or(1)
            .clamp(1, 16);
        let record_idr_interval = std::env::var("RUST_REPLAY_VPL_IDR_INTERVAL")
            .ok()
            .and_then(|value| value.parse::<u16>().ok())
            .unwrap_or(1);

        let mut query_failures = Vec::new();
        let mut selected_route: Option<(VplRecordRoute, MfxVideoParam, i32)> = None;
        if route_plan.is_some() {
            let candidate = record_route_candidates[0];
            let mut param = make_query_param(
                rate_control,
                candidate.fourcc,
                candidate.chroma,
                candidate.bit_depth,
                candidate.profile,
            );
            param.AsyncDepth = record_async_depth;
            notes.push(format!(
                "record RoutePlan: 跳过启动阶段 MFXVideoENCODE_Query/QueryIOSurf；使用能力探测阶段已验证 route={} rc={}",
                candidate.summary(),
                rate_control.method.short_name()
            ));
            selected_route = Some((candidate, param, MFX_ERR_NONE));
        }
        for candidate in record_route_candidates
            .iter()
            .filter(|_| route_plan.is_none())
        {
            let mut query_param = make_query_param(
                rate_control,
                candidate.fourcc,
                candidate.chroma,
                candidate.bit_depth,
                candidate.profile,
            );
            query_param.AsyncDepth = record_async_depth;
            query_param.mfx.FrameInfo.Width = aligned_width;
            query_param.mfx.FrameInfo.Height = aligned_height;
            query_param.mfx.FrameInfo.CropW = capture_width;
            query_param.mfx.FrameInfo.CropH = capture_height;
            query_param.mfx.FrameInfo.FrameRateExtN = encoder_frame_rate_n;
            query_param.mfx.FrameInfo.FrameRateExtD = encoder_frame_rate_d;
            query_param.mfx.GopPicSize = record_gop_pic_size;
            query_param.mfx.GopRefDist = record_gop_ref_dist;
            query_param.mfx.IdrInterval = record_idr_interval;
            let mut query_ext_buffers = VplEncodeExtBuffers::for_route(*candidate, rate_control);
            query_ext_buffers.attach(&mut query_param);

            let mut queried = query_param;
            let query_status =
                (api.mfx_video_encode_query)(session, &mut query_param, &mut queried);
            if query_status < MFX_ERR_NONE {
                query_failures.push(format!(
                    "{}: Query status={query_status}",
                    candidate.summary()
                ));
                continue;
            }
            if query_status == MFX_WRN_PARTIAL_ACCELERATION {
                query_failures.push(format!(
                    "{}: Query 返回 MFX_WRN_PARTIAL_ACCELERATION",
                    candidate.summary()
                ));
                continue;
            }
            if !query_output_preserves_record_route(&queried, *candidate)
                || queried.mfx.RateControlMethod != rate_control.method.vpl_value()
            {
                query_failures.push(format!(
                    "{}: Query 改写关键参数 FourCC {} -> {}, chroma {} -> {}, bit_depth {}/{} -> {}/{}, profile {} -> {}, rc {} -> {}",
                    candidate.summary(),
                    fourcc_to_string(query_param.mfx.FrameInfo.FourCC),
                    fourcc_to_string(queried.mfx.FrameInfo.FourCC),
                    query_param.mfx.FrameInfo.ChromaFormat,
                    queried.mfx.FrameInfo.ChromaFormat,
                    query_param.mfx.FrameInfo.BitDepthLuma,
                    query_param.mfx.FrameInfo.BitDepthChroma,
                    queried.mfx.FrameInfo.BitDepthLuma,
                    queried.mfx.FrameInfo.BitDepthChroma,
                    hevc_profile_name(u32::from(query_param.mfx.CodecProfile)),
                    hevc_profile_name(u32::from(queried.mfx.CodecProfile)),
                    rate_control.method.short_name(),
                    queried.mfx.RateControlMethod
                ));
                continue;
            }
            let mut iosurf_param = queried;
            let mut alloc_request: MfxFrameAllocRequest = std::mem::zeroed();
            let query_iosurf_status =
                (api.mfx_video_encode_query_iosurf)(session, &mut iosurf_param, &mut alloc_request);
            if query_iosurf_status < MFX_ERR_NONE {
                query_failures.push(format!(
                    "{}: QueryIOSurf status={}（该具体码控字段组合不能创建 video-memory surface）",
                    candidate.summary(),
                    query_iosurf_status
                ));
                continue;
            }
            notes.push(format!(
                "oneVPL record route QueryIOSurf: route={} status={} min={} suggested={}",
                candidate.summary(),
                query_iosurf_status,
                alloc_request.NumFrameMin,
                alloc_request.NumFrameSuggested
            ));
            selected_route = Some((*candidate, queried, query_status));
            if !query_failures.is_empty() {
                notes.push(format!(
                    "oneVPL route fallback: skipped candidates=[{}]",
                    query_failures.join(" | ")
                ));
            }
            break;
        }
        let Some((record_route, queried, query_status)) = selected_route else {
            let _ = (api.mfx_close)(session);
            (api.mfx_unload)(loader);
            return Err(BackendError::unsupported(
                "oneVPL record route Query",
                requested_chroma.doc_label(),
                format!(
                    "当前显示状态候选 route 均未通过 oneVPL/D3D11 GPU-only Query；{}",
                    query_failures.join(" | ")
                ),
            ));
        };
        notes.push(format!(
            "oneVPL record route selected: {}",
            record_route.summary()
        ));
        sink_status(
            &mut encoded_sink,
            format!(
                "初始化阶段：oneVPL Query/QueryIOSurf 选路完成：{}，累计 {:.1}ms",
                record_route.summary(),
                record_started.elapsed().as_secs_f64() * 1000.0
            ),
        );
        let mut param = queried;
        apply_record_route_to_param(&mut param, record_route);
        apply_rate_control_config_to_param(&mut param, rate_control);
        let mut ext_buffers = VplEncodeExtBuffers::for_route(record_route, rate_control);
        ext_buffers.attach(&mut param);
        param.AsyncDepth = record_async_depth;
        param.mfx.FrameInfo.FrameRateExtN = encoder_frame_rate_n;
        param.mfx.FrameInfo.FrameRateExtD = encoder_frame_rate_d;
        param.mfx.FrameInfo.Width = aligned_width;
        param.mfx.FrameInfo.Height = aligned_height;
        param.mfx.FrameInfo.CropW = capture_width;
        param.mfx.FrameInfo.CropH = capture_height;
        param.mfx.GopPicSize = record_gop_pic_size;
        param.mfx.GopRefDist = record_gop_ref_dist;
        param.mfx.IdrInterval = record_idr_interval;
        param.mfx.LowPower = MFX_CODINGOPTION_ON;
        param.mfx.TargetUsage = 7;
        if record_stop.load(std::sync::atomic::Ordering::Relaxed) {
            let _ = (api.mfx_close)(session);
            (api.mfx_unload)(loader);
            return Err(BackendError::unsupported(
                "录制后端初始化",
                "用户停止请求",
                "停止请求发生在 MFXVideoENCODE_Init 前，已中止当前片段",
            ));
        }
        let init_started = Instant::now();
        let init_status = (api.mfx_video_encode_init)(session, &mut param);
        if init_status < MFX_ERR_NONE {
            let _ = (api.mfx_close)(session);
            (api.mfx_unload)(loader);
            return Err(BackendError::VplStatus {
                func: "MFXVideoENCODE_Init",
                status: init_status,
            });
        }
        sink_status(
            &mut encoded_sink,
            format!(
                "初始化阶段：MFXVideoENCODE_Init 完成 status={}，累计 {:.1}ms，本阶段 {:.1}ms",
                init_status,
                record_started.elapsed().as_secs_f64() * 1000.0,
                init_started.elapsed().as_secs_f64() * 1000.0
            ),
        );
        if let Some(sink) = encoded_sink.as_deref_mut() {
            sink.video_track_started(VplOutputTrackInfo {
                width: capture_width,
                height: capture_height,
                color: record_route.mp4_color,
                codec: record_route.mp4_codec,
            });
        }

        let mut first_surface: *mut MfxFrameSurface1 = ptr::null_mut();
        let first_get_surface_status =
            (api.mfx_memory_get_surface_for_encode)(session, &mut first_surface);
        if first_get_surface_status != MFX_ERR_NONE || first_surface.is_null() {
            let _ = (api.mfx_video_encode_close)(session);
            let _ = (api.mfx_close)(session);
            (api.mfx_unload)(loader);
            return Err(BackendError::unsupported(
                "oneVPL D3D11 surface import",
                format!(
                    "{} + {}",
                    record_route.summary(),
                    rate_control.method.short_name()
                ),
                format!(
                    "MFXMemory_GetSurfaceForEncode 返回 status={}；该具体 route/码控字段组合无法提供 video-memory surface，禁止 CPU fallback",
                    first_get_surface_status
                ),
            ));
        }

        let first_interface = (*first_surface).FrameInterface;
        if first_interface.is_null() {
            let _ = (api.mfx_video_encode_close)(session);
            let _ = (api.mfx_close)(session);
            (api.mfx_unload)(loader);
            return Err(BackendError::unsupported(
                "oneVPL record",
                "FrameInterface",
                "oneVPL surface 没有 FrameInterface",
            ));
        }
        let mut first_native: MfxHDL = ptr::null_mut();
        let mut first_native_type = 0u32;
        let surface_started = Instant::now();
        let native_status = ((*first_interface).GetNativeHandle)(
            first_surface,
            &mut first_native,
            &mut first_native_type,
        );
        if native_status != MFX_ERR_NONE || first_native_type != MFX_RESOURCE_DX11_TEXTURE {
            let _ = ((*first_interface).Release)(first_surface);
            let _ = (api.mfx_video_encode_close)(session);
            let _ = (api.mfx_close)(session);
            (api.mfx_unload)(loader);
            return Err(BackendError::VplStatus {
                func: "mfxFrameSurfaceInterface::GetNativeHandle",
                status: native_status,
            });
        }
        let Some(first_target) = <ID3D11Texture2D as Interface>::from_raw_borrowed(&first_native)
        else {
            let _ = ((*first_interface).Release)(first_surface);
            let _ = (api.mfx_video_encode_close)(session);
            let _ = (api.mfx_close)(session);
            (api.mfx_unload)(loader);
            return Err(BackendError::unsupported(
                "oneVPL record",
                "native texture",
                "GetNativeHandle 返回值不是 ID3D11Texture2D",
            ));
        };
        let mut target_desc = D3D11_TEXTURE2D_DESC::default();
        first_target.GetDesc(&mut target_desc);
        let surface_cache_enabled = std::env::var("RUSTREPLAY_SURFACE_CACHE")
            .map(|value| value != "0")
            .unwrap_or(true);
        let mut surface_texture_cache: HashMap<usize, ID3D11Texture2D> = HashMap::new();
        if surface_cache_enabled {
            surface_texture_cache.insert(first_surface as usize, first_target.clone());
        }

        let mut device_handle: MfxHDL = ptr::null_mut();
        let mut device_type = 0u32;
        let device_status = ((*first_interface).GetDeviceHandle)(
            first_surface,
            &mut device_handle,
            &mut device_type,
        );
        if device_status != MFX_ERR_NONE || device_type != MFX_HANDLE_D3D11_DEVICE {
            let _ = ((*first_interface).Release)(first_surface);
            let _ = (api.mfx_video_encode_close)(session);
            let _ = (api.mfx_close)(session);
            (api.mfx_unload)(loader);
            return Err(BackendError::VplStatus {
                func: "mfxFrameSurfaceInterface::GetDeviceHandle",
                status: device_status,
            });
        }
        let Some(vpl_device) = <ID3D11Device as Interface>::from_raw_borrowed(&device_handle)
        else {
            let _ = ((*first_interface).Release)(first_surface);
            let _ = (api.mfx_video_encode_close)(session);
            let _ = (api.mfx_close)(session);
            (api.mfx_unload)(loader);
            return Err(BackendError::unsupported(
                "oneVPL record",
                "device handle",
                "GetDeviceHandle 返回值不是 ID3D11Device",
            ));
        };
        sink_status(
            &mut encoded_sink,
            format!(
                "初始化阶段：oneVPL video-memory surface/native D3D11 device 就绪，累计 {:.1}ms，本阶段 {:.1}ms",
                record_started.elapsed().as_secs_f64() * 1000.0,
                surface_started.elapsed().as_secs_f64() * 1000.0
            ),
        );

        let immediate: ID3D11DeviceContext =
            vpl_device
                .GetImmediateContext()
                .map_err(|err| BackendError::WindowsApi {
                    func: "ID3D11Device::GetImmediateContext",
                    message: err.to_string(),
                })?;
        if capture_source.is_wgc() {
            let priority = -7;
            match set_d3d11_gpu_thread_priority(vpl_device, priority) {
                Ok(()) => notes.push(format!(
                    "WGC encoder D3D11 device GPU thread priority set to {priority}"
                )),
                Err(err) => notes.push(format!(
                    "WGC encoder D3D11 device GPU thread priority {priority} failed: {err}"
                )),
            }
        }
        notes.push(format!(
            "oneVPL record encode params: async_depth={}, rc={}, brc_multiplier={}, target_field={}kbps(effective≈{}kbps), max_field={}kbps(effective≈{}kbps), buffer_field={}KB, gop_pic_size={}, gop_ref_dist={}, idr_interval={}, ext_buffers={}",
            record_async_depth,
            rate_control.method.short_name(),
            param.mfx.BRCParamMultiplier,
            param.mfx.TargetKbps,
            u32::from(param.mfx.TargetKbps) * u32::from(param.mfx.BRCParamMultiplier.max(1)),
            param.mfx.MaxKbps,
            u32::from(param.mfx.MaxKbps) * u32::from(param.mfx.BRCParamMultiplier.max(1)),
            param.mfx.BufferSizeInKB,
            param.mfx.GopPicSize,
            param.mfx.GopRefDist,
            param.mfx.IdrInterval,
            param.NumExtParam
        ));
        notes.push(format!(
            "record backend route selected internally: {}",
            record_route.summary()
        ));
        notes.push(format!(
            "oneVPL surface native handle cache enabled={surface_cache_enabled}"
        ));
        notes.push(
            "首个正式 AU 通过 mfxEncodeCtrl FrameType=I|REF|IDR 强制为关键帧；编码预热 AU 仅用于驱动/表面预热和参数集提取，不写入正式时间线"
                .to_owned(),
        );
        let record_route_dxgi_format = record_route.try_dxgi_format()?;
        let gpu_route_started = Instant::now();
        let route_intermediate =
            create_route_intermediate(vpl_device, &target_desc, record_route, true)?;
        sink_status(
            &mut encoded_sink,
            format!(
                "初始化阶段：GPU route intermediate texture 就绪，累计 {:.1}ms，本阶段 {:.1}ms",
                record_started.elapsed().as_secs_f64() * 1000.0,
                gpu_route_started.elapsed().as_secs_f64() * 1000.0
            ),
        );
        match capture_source {
            RecordCaptureSource::Dda => notes.push(
                "固定 DDA 路线：独立 D3D11 capture device 获取 DDA 帧，捕获线程按自动 route GPU shader 写目标 FourCC keyed shared snapshot 后立即 ReleaseFrame，编码线程复制目标 FourCC snapshot 到 oneVPL surface"
                    .to_owned(),
            ),
            RecordCaptureSource::Wgc => notes.push(
                "固定 WGC 路线：使用 oneVPL native D3D11 device 创建 WGC capture，捕获端 GPU shader 写目标 FourCC ordinary ring texture、WGC 负责录制光标，编码线程仅 CopyResource 到 oneVPL surface"
                    .to_owned(),
            ),
        }

        let mut samples = Vec::new();
        let retain_output_samples = write_output_mp4 || encoded_sink.is_none();
        let mut captured_frames = 0u32;
        let mut warmup_encoded_frames = 0u32;
        let mut dda_timeouts = 0u32;
        let mut input_dxgi_format = 0u32;
        let mut route_converter: Option<GpuRecordConverter> = None;
        let mut conversion_ready = false;
        let format_flags_in = 0u32;
        let format_flags_out = 0u32;
        let mut pending_surface = Some(first_surface);
        let mut in_flight: VecDeque<Box<AsyncEncode>> =
            VecDeque::with_capacity(record_async_depth as usize);
        let mut bitstream_pool: Vec<Vec<u8>> = Vec::with_capacity(record_async_depth as usize);
        for _ in 0..record_async_depth {
            bitstream_pool.push(vec![0u8; VPL_BITSTREAM_BYTES + 31]);
        }
        let async_depth = record_async_depth as usize;
        let mut perf = RecordPerf::default();
        let mut dirty_metadata_frames = 0u32;
        let partial_convert_frames = 0u32;
        let mut full_convert_frames = 0u32;
        let mut move_metadata_frames = 0u32;
        let mut dirty_area_total = 0u64;
        let mut first_sample_timestamp_90k: Option<u64> = None;
        let mut first_video_timestamp_100ns: Option<i64> = None;
        let mut last_submitted_sample_timestamp_90k: Option<u64> = None;
        let capture_duration = Duration::from_secs_f32(duration_seconds.max(0.1));
        let requested_duration_90k =
            (duration_seconds.max(0.1) as f64 * VIDEO_CLOCK_HZ as f64).round() as u64;
        // DDA/WGC are intentionally VFR: stop from the accepted source
        // timeline itself, without subtracting or synthesizing a nominal FPS
        // frame duration.
        let source_stop_90k = requested_duration_90k;
        let start = Instant::now();
        let end_at = start + capture_duration;
        let qpc_frequency = query_performance_frequency().unwrap_or(0);
        let audio_started = Instant::now();
        let mut audio_capture =
            RecordAudioCapture::start(capture_duration + Duration::from_secs(5), &mut notes);
        sink_status(
            &mut encoded_sink,
            format!(
                "初始化阶段：音频线程启动阶段完成 enabled={}，累计 {:.1}ms，本阶段 {:.1}ms",
                audio_capture.is_some(),
                record_started.elapsed().as_secs_f64() * 1000.0,
                audio_started.elapsed().as_secs_f64() * 1000.0
            ),
        );

        {
            let capture_pool_size = 32usize;
            let capture_queue_size = capture_pool_size;
            let (frame_tx, frame_rx) = std::sync::mpsc::channel::<CaptureMsg>();
            let (free_tx, free_rx) = std::sync::mpsc::channel::<CaptureFrameSlot>();
            let capture_stop = std::sync::Arc::new(std::sync::atomic::AtomicBool::new(false));
            let stop = capture_stop.clone();
            let capture_thread_started = Instant::now();
            let capture_handle = match capture_source {
                RecordCaptureSource::Dda => {
                    let (capture_device, capture_context) =
                        create_d3d11_device_for_adapter(&adapter1)?;
                    spawn_dda_capture_thread(
                        adapter1.clone(),
                        capture_device,
                        capture_context,
                        vpl_device.clone(),
                        None,
                        start,
                        end_at,
                        source_stop_90k,
                        qpc_frequency,
                        record_route,
                        target_desc.Width,
                        target_desc.Height,
                        capture_pool_size,
                        stop.clone(),
                        frame_tx,
                        free_rx,
                    )
                }
                RecordCaptureSource::Wgc => spawn_wgc_capture_thread(
                    adapter1.clone(),
                    vpl_device.clone(),
                    start,
                    end_at,
                    source_stop_90k,
                    record_route,
                    target_desc.Width,
                    target_desc.Height,
                    capture_pool_size,
                    stop.clone(),
                    frame_tx,
                    free_rx,
                ),
            };
            sink_status(
                &mut encoded_sink,
                format!(
                    "初始化阶段：{} capture thread 已启动，累计 {:.1}ms，本阶段 {:.1}ms；后续若仍无正式帧，多半处于 capture/warmup 阶段",
                    capture_source.label(),
                    record_started.elapsed().as_secs_f64() * 1000.0,
                    capture_thread_started.elapsed().as_secs_f64() * 1000.0
                ),
            );
            notes.push(format!(
                "capture snapshot pool: textures={}, queue={}",
                capture_pool_size, capture_queue_size
            ));
            notes.push(
                "capture thread: encoder-side snapshot slots are returned asynchronously after GPU event queries confirm route shader/copy consumed them"
                    .to_owned(),
            );

            let mut capture_stats: Option<CaptureStats> = None;
            let mut capture_error: Option<String> = None;
            let mut active_source_desc: Option<D3D11_TEXTURE2D_DESC> = None;
            let mut pending_free_slots: Vec<CaptureFrameSlot> = Vec::new();

            loop {
                if record_stop.load(std::sync::atomic::Ordering::Relaxed) {
                    stop.store(true, std::sync::atomic::Ordering::Relaxed);
                    break;
                }
                return_ready_snapshot_slots(&mut pending_free_slots, &immediate, &free_tx, false)?;
                let msg = match frame_rx.recv_timeout(Duration::from_millis(2)) {
                    Ok(msg) => msg,
                    Err(std::sync::mpsc::RecvTimeoutError::Timeout) => {
                        return_ready_snapshot_slots(
                            &mut pending_free_slots,
                            &immediate,
                            &free_tx,
                            false,
                        )?;
                        let sync_started = Instant::now();
                        loop {
                            match try_sync_one_async_encode(
                                &api,
                                session,
                                &mut in_flight,
                                &mut bitstream_pool,
                                0,
                            )? {
                                TrySyncResult::Ready(Some(sample)) => push_record_hevc_sample(
                                    &mut samples,
                                    sample,
                                    &mut encoded_sink,
                                    retain_output_samples,
                                ),
                                TrySyncResult::Ready(None) => break,
                                TrySyncResult::NotReady => break,
                            }
                        }
                        if let Some(capture) = audio_capture.as_mut() {
                            capture.poll_live_aac(
                                first_video_timestamp_100ns,
                                last_submitted_sample_timestamp_90k,
                                &mut encoded_sink,
                                &mut notes,
                            );
                        }
                        perf.sync.add(sync_started.elapsed());
                        continue;
                    }
                    Err(std::sync::mpsc::RecvTimeoutError::Disconnected) => break,
                };

                match msg {
                    CaptureMsg::Frame(captured) => {
                        let frame_result = (|| -> Result<(), BackendError> {
                            let frame_started = Instant::now();
                            let CapturedSnapshot {
                                slot,
                                source_desc,
                                move_rect_bytes,
                                dirty_rects,
                                timestamp_90k,
                                timestamp_100ns,
                                capture_index: _,
                                accumulated_frames,
                                warmup,
                            } = captured;
                            if input_dxgi_format == 0 {
                                input_dxgi_format = source_desc.Format.0 as u32;
                            }
                            perf.dda_accumulated_frames_total += u64::from(accumulated_frames);
                            perf.dda_accumulated_frames_max =
                                perf.dda_accumulated_frames_max.max(accumulated_frames);

                            let source_changed = active_source_desc.is_some_and(|prev| {
                                prev.Width != source_desc.Width
                                    || prev.Height != source_desc.Height
                                    || prev.Format.0 != source_desc.Format.0
                            });
                            if source_changed {
                                conversion_ready = false;
                                route_converter = None;
                                notes.push(format!(
                                    "capture format changed; route shader converter rebuilt for {}x{} DXGI_FORMAT({})",
                                    source_desc.Width, source_desc.Height, source_desc.Format.0
                                ));
                            }
                            active_source_desc = Some(source_desc);

                            let init_started = Instant::now();
                            let same_route_format =
                                source_desc.Format.0 == record_route_dxgi_format.0;
                            if !same_route_format
                                && !record_route
                                    .accepts_unconverted_capture_format(source_desc.Format)
                            {
                                return Err(BackendError::unsupported(
                                    "Capture -> ColorTransform",
                                    format!(
                                        "{} route 收到 DXGI_FORMAT({})",
                                        record_route.summary(),
                                        source_desc.Format.0
                                    ),
                                    "不支持的桌面模式",
                                ));
                            }
                            let direct_route_snapshot = same_route_format
                                && source_desc.Width == target_desc.Width
                                && source_desc.Height == target_desc.Height;
                            if !conversion_ready && same_route_format {
                                conversion_ready = true;
                                if direct_route_snapshot {
                                    notes.push(
                                        "捕获线程已产出目标格式/尺寸 shared snapshot；编码线程固定仅执行 GPU copy"
                                            .to_owned(),
                                    );
                                } else {
                                    notes.push(format!(
                                        "捕获线程已产出目标格式 shared snapshot，但尺寸 {}x{} != oneVPL surface {}x{}；编码线程固定用 GPU CopySubresourceRegion 填充有效区域",
                                        source_desc.Width,
                                        source_desc.Height,
                                        target_desc.Width,
                                        target_desc.Height
                                    ));
                                }
                            }
                            if !conversion_ready {
                                route_converter = Some(GpuRecordConverter::new(
                                    record_route,
                                    vpl_device,
                                    &immediate,
                                    &route_intermediate,
                                    source_desc.Width,
                                    source_desc.Height,
                                    true,
                                )?);
                                notes.push(format!(
                                    "输入 DXGI_FORMAT({}) 固定使用 GPU shader 全帧写入目标 YUV plane",
                                    source_desc.Format.0
                                ));
                                conversion_ready = true;
                            }
                            perf.init.add(init_started.elapsed());

                            if !dirty_rects.is_empty() {
                                dirty_metadata_frames += 1;
                                dirty_area_total += dirty_rect_area(&dirty_rects);
                            }
                            if move_rect_bytes > 0 {
                                move_metadata_frames += 1;
                            }
                            let surface_started = Instant::now();
                            let surface = if let Some(surface) = pending_surface.take() {
                                surface
                            } else {
                                let mut next_surface: *mut MfxFrameSurface1 = ptr::null_mut();
                                let status = (api.mfx_memory_get_surface_for_encode)(
                                    session,
                                    &mut next_surface,
                                );
                                if status != MFX_ERR_NONE || next_surface.is_null() {
                                    return Err(BackendError::unsupported(
                                        "oneVPL D3D11 surface import",
                                        format!(
                                            "{} + {}",
                                            record_route.summary(),
                                            rate_control.method.short_name()
                                        ),
                                        format!(
                                            "MFXMemory_GetSurfaceForEncode 返回 status={status}；该具体 route/码控字段组合无法继续提供 video-memory surface，禁止 CPU fallback"
                                        ),
                                    ));
                                }
                                next_surface
                            };

                            let frame_interface = (*surface).FrameInterface;
                            if frame_interface.is_null() {
                                return Err(BackendError::unsupported(
                                    "oneVPL record",
                                    "FrameInterface",
                                    "oneVPL surface 没有 FrameInterface",
                                ));
                            }
                            let (target, surface_cache_hit) = match cached_vpl_surface_texture(
                                surface,
                                &mut surface_texture_cache,
                                surface_cache_enabled,
                            ) {
                                Ok(texture) => texture,
                                Err(err) => {
                                    let _ = ((*frame_interface).Release)(surface);
                                    return Err(err);
                                }
                            };
                            if surface_cache_hit {
                                perf.surface_cache_hits = perf.surface_cache_hits.saturating_add(1);
                            } else {
                                perf.surface_cache_misses =
                                    perf.surface_cache_misses.saturating_add(1);
                            }
                            perf.surface.add(surface_started.elapsed());

                            let conversion_source = match &slot {
                                CaptureFrameSlot::Shared(shared) => {
                                    shared.encoder_mutex.AcquireSync(1, 1_000).map_err(|err| {
                                        BackendError::WindowsApi {
                                            func: "IDXGIKeyedMutex::AcquireSync(encoder snapshot)",
                                            message: err.to_string(),
                                        }
                                    })?;
                                    shared.encoder_texture.clone()
                                }
                                CaptureFrameSlot::WgcLocal(local) => local.texture.clone(),
                            };
                            let conversion_result = (|| -> Result<(), BackendError> {
                                if direct_route_snapshot {
                                    let copy_started = Instant::now();
                                    copy_texture_resource(&immediate, &conversion_source, &target)?;
                                    perf.copy.add(copy_started.elapsed());
                                    full_convert_frames += 1;
                                } else if same_route_format {
                                    let copy_started = Instant::now();
                                    copy_texture_subresource_region(
                                        &immediate,
                                        &conversion_source,
                                        &target,
                                        source_desc.Width.min(target_desc.Width),
                                        source_desc.Height.min(target_desc.Height),
                                    )?;
                                    perf.copy.add(copy_started.elapsed());
                                    full_convert_frames += 1;
                                } else if let Some(converter) = &route_converter {
                                    let convert_started = Instant::now();
                                    converter.convert(&conversion_source)?;
                                    perf.convert.add(convert_started.elapsed());
                                    full_convert_frames += 1;
                                } else {
                                    return Err(BackendError::unsupported(
                                        "GPU shader -> route YUV plane",
                                        format!("DXGI_FORMAT({}) 输入", source_desc.Format.0),
                                        "固定路线未初始化目标 YUV 转换器",
                                    ));
                                }
                                Ok(())
                            })();

                            let fence_started = Instant::now();
                            match &slot {
                                CaptureFrameSlot::Shared(shared) => {
                                    shared.encoder_fence.mark(&immediate);
                                    shared.encoder_mutex.ReleaseSync(0).map_err(|err| {
                                        BackendError::WindowsApi {
                                            func: "IDXGIKeyedMutex::ReleaseSync(encoder snapshot)",
                                            message: err.to_string(),
                                        }
                                    })?;
                                }
                                CaptureFrameSlot::WgcLocal(local) => {
                                    local.fence.mark(&immediate);
                                }
                            }
                            perf.source_fence.add(fence_started.elapsed());
                            conversion_result?;
                            // The free slot is returned after encoder-side conversion/copy commands
                            // have been submitted, the keyed mutex has been released, and a D3D11
                            // event query later confirms the GPU has consumed the snapshot.
                            pending_free_slots.push(slot);

                            if !same_route_format {
                                let copy_started = Instant::now();
                                copy_texture_resource(&immediate, &route_intermediate, &target)?;
                                perf.copy.add(copy_started.elapsed());
                            }

                            if warmup {
                                let warmup_ts90 = u64::from(warmup_encoded_frames)
                                    .saturating_mul(ENCODER_WARMUP_TIMESTAMP_STEP_90K);
                                warmup_encoded_frames = warmup_encoded_frames.saturating_add(1);
                                (*surface).Data.TimeStamp = warmup_ts90;
                                (*surface).Data.FrameOrder = warmup_encoded_frames;
                                let warmup_submit_started = Instant::now();
                                if bitstream_pool.is_empty() {
                                    while bitstream_pool.is_empty() && !in_flight.is_empty() {
                                        if let Some(sample) = sync_one_async_encode(
                                            &api,
                                            session,
                                            &mut in_flight,
                                            &mut bitstream_pool,
                                        )? {
                                            push_record_hevc_sample(
                                                &mut samples,
                                                sample,
                                                &mut encoded_sink,
                                                retain_output_samples,
                                            );
                                        }
                                    }
                                }
                                let submitted = submit_encode_async(
                                    &api,
                                    session,
                                    surface,
                                    warmup_ts90,
                                    warmup_encoded_frames == 1,
                                    bitstream_pool.pop().ok_or_else(|| {
                                        BackendError::unsupported(
                                            "oneVPL record",
                                            "bitstream pool",
                                            "没有可用于 warmup encode 的 bitstream 缓冲",
                                        )
                                    })?,
                                    true,
                                )?;
                                perf.submit.add(warmup_submit_started.elapsed());
                                let release_status = ((*frame_interface).Release)(surface);
                                if release_status != MFX_ERR_NONE {
                                    return Err(BackendError::VplStatus {
                                        func: "mfxFrameSurfaceInterface::Release(warmup)",
                                        status: release_status,
                                    });
                                }
                                if let Some(submitted) = submitted {
                                    in_flight.push_back(submitted);
                                }
                                let warmup_sync_started = Instant::now();
                                if rate_control.low_delay_brc {
                                    while !in_flight.is_empty() {
                                        if let Some(sample) = sync_one_async_encode(
                                            &api,
                                            session,
                                            &mut in_flight,
                                            &mut bitstream_pool,
                                        )? {
                                            push_record_hevc_sample(
                                                &mut samples,
                                                sample,
                                                &mut encoded_sink,
                                                retain_output_samples,
                                            );
                                        }
                                    }
                                } else {
                                    while in_flight.len() >= async_depth {
                                        match try_sync_one_async_encode(
                                            &api,
                                            session,
                                            &mut in_flight,
                                            &mut bitstream_pool,
                                            1,
                                        )? {
                                            TrySyncResult::Ready(Some(sample)) => {
                                                push_record_hevc_sample(
                                                    &mut samples,
                                                    sample,
                                                    &mut encoded_sink,
                                                    retain_output_samples,
                                                )
                                            }
                                            TrySyncResult::Ready(None) => break,
                                            TrySyncResult::NotReady => break,
                                        }
                                    }
                                }
                                perf.sync.add(warmup_sync_started.elapsed());
                                perf.frame.add(frame_started.elapsed());
                                return Ok(());
                            }

                            if first_video_timestamp_100ns.is_none() {
                                sink_status(
                                    &mut encoded_sink,
                                    format!(
                                        "初始化阶段结束：首个正式源视频帧进入编码，累计 {:.1}ms；此前耗时属于 oneVPL/D3D11 初始化 + {} capture warmup",
                                        record_started.elapsed().as_secs_f64() * 1000.0,
                                        capture_source.label()
                                    ),
                                );
                                first_video_timestamp_100ns = timestamp_100ns;
                            }
                            let first_ts = *first_sample_timestamp_90k.get_or_insert(timestamp_90k);
                            let sample_ts90 = timestamp_90k.saturating_sub(first_ts);
                            last_submitted_sample_timestamp_90k = Some(sample_ts90);
                            (*surface).Data.TimeStamp = sample_ts90;
                            (*surface).Data.FrameOrder = captured_frames;
                            let submit_started = Instant::now();
                            if bitstream_pool.is_empty() {
                                while bitstream_pool.is_empty() && !in_flight.is_empty() {
                                    if let Some(sample) = sync_one_async_encode(
                                        &api,
                                        session,
                                        &mut in_flight,
                                        &mut bitstream_pool,
                                    )? {
                                        push_record_hevc_sample(
                                            &mut samples,
                                            sample,
                                            &mut encoded_sink,
                                            retain_output_samples,
                                        );
                                    }
                                }
                            }
                            let submitted = submit_encode_async(
                                &api,
                                session,
                                surface,
                                sample_ts90,
                                captured_frames == 0,
                                bitstream_pool.pop().ok_or_else(|| {
                                    BackendError::unsupported(
                                        "oneVPL record",
                                        "bitstream pool",
                                        "没有可用 bitstream 缓冲，且无可同步的 in-flight encode",
                                    )
                                })?,
                                false,
                            )?;
                            perf.submit.add(submit_started.elapsed());
                            let release_status = ((*frame_interface).Release)(surface);
                            if release_status != MFX_ERR_NONE {
                                return Err(BackendError::VplStatus {
                                    func: "mfxFrameSurfaceInterface::Release",
                                    status: release_status,
                                });
                            }
                            if let Some(submitted) = submitted {
                                in_flight.push_back(submitted);
                            }
                            let sync_started = Instant::now();
                            while in_flight.len() >= async_depth {
                                match try_sync_one_async_encode(
                                    &api,
                                    session,
                                    &mut in_flight,
                                    &mut bitstream_pool,
                                    1,
                                )? {
                                    TrySyncResult::Ready(Some(sample)) => push_record_hevc_sample(
                                        &mut samples,
                                        sample,
                                        &mut encoded_sink,
                                        retain_output_samples,
                                    ),
                                    TrySyncResult::Ready(None) => break,
                                    TrySyncResult::NotReady => break,
                                }
                            }
                            while in_flight.len() >= VPL_RECORD_MAX_IN_FLIGHT {
                                if let Some(sample) = sync_one_async_encode(
                                    &api,
                                    session,
                                    &mut in_flight,
                                    &mut bitstream_pool,
                                )? {
                                    push_record_hevc_sample(
                                        &mut samples,
                                        sample,
                                        &mut encoded_sink,
                                        retain_output_samples,
                                    );
                                }
                            }
                            perf.sync.add(sync_started.elapsed());

                            captured_frames += 1;
                            if let Some(capture) = audio_capture.as_mut() {
                                capture.poll_live_aac(
                                    first_video_timestamp_100ns,
                                    last_submitted_sample_timestamp_90k,
                                    &mut encoded_sink,
                                    &mut notes,
                                );
                            }
                            perf.frame.add(frame_started.elapsed());
                            Ok(())
                        })();
                        if let Err(err) = frame_result {
                            stop.store(true, std::sync::atomic::Ordering::Relaxed);
                            let _ = capture_handle.join();
                            return Err(err);
                        }
                    }
                    CaptureMsg::Done(stats) => {
                        capture_stats = Some(stats);
                        break;
                    }
                    CaptureMsg::Error(message) => {
                        capture_error = Some(message);
                        break;
                    }
                }
            }

            stop.store(true, std::sync::atomic::Ordering::Relaxed);
            return_ready_snapshot_slots(&mut pending_free_slots, &immediate, &free_tx, true)?;
            let _ = capture_handle.join();
            if let Some(message) = capture_error {
                return Err(BackendError::unsupported(
                    "DDA capture thread",
                    "Acquire/CopyResource",
                    message,
                ));
            }
            if let Some(stats) = capture_stats {
                dda_timeouts = stats.dda_timeouts.min(u64::from(u32::MAX)) as u32;
                perf.dda_accumulated_frames_total = stats.accumulated_frames_total;
                perf.dda_accumulated_frames_max = stats.accumulated_frames_max;
                notes.push(stats.summary());
            }
        }

        if record_stop.load(std::sync::atomic::Ordering::Relaxed) {
            let stop_cleanup_started = Instant::now();
            sink_status(
                &mut encoded_sink,
                format!(
                    "停止排查：capture thread 已退出，开始快速释放；跳过段尾 60s 同步等待、encoder flush 和完整 AAC 重建，in_flight={}",
                    in_flight.len()
                ),
            );
            if let Some(surface) = pending_surface.take() {
                let frame_interface = (*surface).FrameInterface;
                if !frame_interface.is_null() {
                    let _ = ((*frame_interface).Release)(surface);
                }
            }
            let short_drain_deadline = Instant::now() + Duration::from_millis(200);
            while !in_flight.is_empty() && Instant::now() < short_drain_deadline {
                match try_sync_one_async_encode(
                    &api,
                    session,
                    &mut in_flight,
                    &mut bitstream_pool,
                    0,
                )? {
                    TrySyncResult::Ready(Some(sample)) => push_record_hevc_sample(
                        &mut samples,
                        sample,
                        &mut encoded_sink,
                        retain_output_samples,
                    ),
                    TrySyncResult::Ready(None) => break,
                    TrySyncResult::NotReady => std::thread::sleep(Duration::from_millis(1)),
                }
            }
            if let Some(capture) = audio_capture.as_mut() {
                capture.stop_without_reencode(&mut notes);
            }
            surface_texture_cache.clear();
            let close_status = (api.mfx_video_encode_close)(session);
            let mfx_close_status = (api.mfx_close)(session);
            (api.mfx_unload)(loader);
            sink_status(
                &mut encoded_sink,
                format!(
                    "停止排查：快速释放完成，close_status={} mfx_close_status={} leftover_in_flight={} cleanup={:.1}ms total={:.1}ms",
                    close_status,
                    mfx_close_status,
                    in_flight.len(),
                    stop_cleanup_started.elapsed().as_secs_f64() * 1000.0,
                    record_started.elapsed().as_secs_f64() * 1000.0
                ),
            );
            return Err(BackendError::unsupported(
                "停止即时回放",
                "用户停止请求",
                "已快速中止当前录制片段；停止路径不再等待正常段结束的 flush/AAC 完整重建",
            ));
        }

        while !in_flight.is_empty() {
            if let Some(sample) =
                sync_one_async_encode(&api, session, &mut in_flight, &mut bitstream_pool)?
            {
                push_record_hevc_sample(
                    &mut samples,
                    sample,
                    &mut encoded_sink,
                    retain_output_samples,
                );
            }
        }
        let skipped_flush_samples = flush_encoder(
            &api,
            session,
            &mut samples,
            &mut encoded_sink,
            retain_output_samples,
        )?;
        let duration_90k = encoded_timeline_duration_90k(
            &samples,
            requested_duration_90k,
            !capture_source.is_wgc(),
        );
        surface_texture_cache.clear();
        let close_status = (api.mfx_video_encode_close)(session);
        let mfx_close_status = (api.mfx_close)(session);
        (api.mfx_unload)(loader);
        if mfx_close_status != MFX_ERR_NONE {
            return Err(BackendError::VplStatus {
                func: "MFXClose",
                status: mfx_close_status,
            });
        }

        let encoded_samples = samples
            .iter()
            .filter(|sample| !sample.discard_from_track)
            .count()
            .min(u32::MAX as usize) as u32;
        let encoded_bytes = samples
            .iter()
            .filter(|sample| !sample.discard_from_track)
            .map(|s| s.data.len() as u64)
            .sum();
        let discarded_header_units = samples
            .iter()
            .filter(|sample| sample.discard_from_track)
            .count();
        if encoded_samples == 0 {
            return Err(BackendError::unsupported(
                "oneVPL encode",
                record_route.summary(),
                format!(
                    "录制结束后没有任何可封装 AU；captured_frames={captured_frames}, target_dxgi_format={}, input_dxgi_format={}, notes={}",
                    target_desc.Format.0,
                    input_dxgi_format,
                    notes.join(" | ")
                ),
            ));
        }
        let audio_track = if let Some(capture) = audio_capture.as_mut() {
            capture.finish_live_aac(&mut encoded_sink, &mut notes);
            let sink_push_from_ticks = capture.live_pushed_until_ticks();
            let audio_frames = capture.finish(&mut notes);
            build_record_aac_track(
                audio_frames,
                first_video_timestamp_100ns,
                duration_90k,
                &mut notes,
                &mut encoded_sink,
                sink_push_from_ticks,
            )?
        } else {
            None
        };
        let audio_access_units = audio_track
            .as_ref()
            .map(|track| track.samples.len().min(u32::MAX as usize) as u32)
            .unwrap_or(0);
        let audio_encoded_bytes = audio_track
            .as_ref()
            .map(|track| {
                track
                    .samples
                    .iter()
                    .map(|sample| sample.data.len() as u64)
                    .sum()
            })
            .unwrap_or(0);
        let video_track = HevcMp4Track {
            width: capture_width,
            height: capture_height,
            duration_90k,
            color: record_route.mp4_color,
            codec: record_route.mp4_codec,
            samples,
        };
        if write_output_mp4 {
            write_hevc_aac_mp4(output, &video_track, audio_track.as_ref())?;
        } else {
            notes.push(format!(
                "生产会话以内存 encoded ring 为主，跳过临时 MP4 写出：{}；保存时再从已编码 HEVC/AAC 快照 mux",
                output.display()
            ));
        }

        notes.push("视频路径固定为 DDA texture/WGC BGRA8/FP16 -> keyed/shared GPU snapshot；DDA 在编码线程按自动 route 全帧 GPU shader 写目标 FourCC，WGC 在捕获线程按自动 route GPU shader 写目标 FourCC；随后一次 CopyResource 到 oneVPL surface -> HEVC -> MP4；未做 raw frame CPU 回读".to_owned());
        notes.push(format!(
            "route 色彩元数据同步写入 MP4 nclx 与 oneVPL mfxExtVideoSignalInfo(VUI)：当前 route nclx={}/{}/{} range={}；10-bit/HDR route 要求 FP16 capture 或已转换目标格式，HDR FP16 按 scRGB 线性 80nit/1.0 转 BT.2020 ST2084 PQ，SDR route 按当前显示色彩写 BT.709 或 BT.2020 SDR YUV",
            record_route.mp4_color.colour_primaries,
            record_route.mp4_color.transfer_characteristics,
            record_route.mp4_color.matrix_coefficients,
            if record_route.mp4_color.full_range {
                "full"
            } else {
                "limited"
            }
        ));
        if skipped_flush_samples > 0 {
            notes.push(format!(
                "oneVPL flush 返回了 {skipped_flush_samples} 个重复/非单调时间戳 AU；为保持 WGC/DDA 源 VFR 时间戳，未按外部 CFR 伪造时间戳，已跳过这些不可封装为正时长 sample 的 AU"
            ));
        }
        if discarded_header_units > 0 {
            notes.push(format!(
                "MP4 muxer 已从 {discarded_header_units} 个编码预热 AU 中提取 VPS/SPS/PPS，但这些预热 AU 不写入正式视频时间线"
            ));
        }
        if capture_source.is_wgc() {
            notes.push(
                "WGC timestamp policy: MP4 sample timestamps are WGC SystemRelativeTime relative to the first accepted source frame; source-to-source gaps are preserved as VFR sample-duration gaps, and no external CFR clock is used to synthesize missing timestamps"
                    .to_owned(),
            );
        }
        if audio_access_units > 0 {
            notes.push(format!(
                "音频轨已接入：AAC LC access_units={} bytes={}，按首个正式视频源时间戳和视频 track duration 裁剪/补静音",
                audio_access_units, audio_encoded_bytes
            ));
        } else {
            notes.push("音频轨未写入：未能获得视频绝对时间戳或音频被显式关闭".to_owned());
        }
        notes.push(format!(
            "DDA dirty rect 元数据统计（固定路线不做 partial dirty-rect 转换）: partial={} full={} dirty_metadata_frames={} move_metadata_frames={} avg_dirty_area={:.0}px",
            partial_convert_frames,
            full_convert_frames,
            dirty_metadata_frames,
            move_metadata_frames,
            if dirty_metadata_frames == 0 {
                0.0
            } else {
                dirty_area_total as f64 / f64::from(dirty_metadata_frames)
            }
        ));
        notes.push(perf.summary(captured_frames));

        let report = VplOneCopyRecordReport {
            adapter_index,
            adapter_luid,
            output_path: output.display().to_string(),
            width: capture_width,
            height: capture_height,
            duration_seconds,
            captured_frames,
            encoded_samples,
            encoded_bytes,
            audio_access_units,
            audio_encoded_bytes,
            dda_timeouts,
            input_dxgi_format,
            target_dxgi_format: target_desc.Format.0 as u32,
            query_status,
            init_status,
            close_status,
            first_get_surface_status,
            video_processor_format_flags_in: format_flags_in,
            video_processor_format_flags_out: format_flags_out,
            notes,
        };

        sink_status(
            &mut encoded_sink,
            format!(
                "录制段正常结束并完成封装准备：captured_frames={} video_au={} audio_au={} total={:.1}ms",
                captured_frames,
                encoded_samples,
                audio_access_units,
                record_started.elapsed().as_secs_f64() * 1000.0
            ),
        );

        Ok(VplOneCopyRecordOutput {
            report,
            video_track,
            audio_track,
        })
    }
}

#[cfg(not(windows))]
pub fn record_d3d11_onecopy_mp4(
    adapter_index: u32,
    output: &Path,
    duration_seconds: f32,
    rate_control: &RateControlConfig,
    requested_chroma: ChromaSampling,
) -> Result<VplOneCopyRecordReport, BackendError> {
    let _ = (
        adapter_index,
        output,
        duration_seconds,
        rate_control,
        requested_chroma,
    );
    Err(BackendError::unsupported(
        "oneVPL D3D11 one-copy record",
        "Windows D3D11",
        "仅 Windows 可用",
    ))
}

#[cfg(not(windows))]
pub fn record_d3d11_onecopy_mp4_cancelable(
    adapter_index: u32,
    output: &Path,
    duration_seconds: f32,
    rate_control: &RateControlConfig,
    requested_chroma: ChromaSampling,
    external_stop: Option<std::sync::Arc<std::sync::atomic::AtomicBool>>,
) -> Result<VplOneCopyRecordReport, BackendError> {
    let _ = external_stop;
    record_d3d11_onecopy_mp4(
        adapter_index,
        output,
        duration_seconds,
        rate_control,
        requested_chroma,
    )
}

#[cfg(not(windows))]
pub fn record_wgc_d3d11_onecopy_mp4(
    adapter_index: u32,
    output: &Path,
    duration_seconds: f32,
    rate_control: &RateControlConfig,
    requested_chroma: ChromaSampling,
) -> Result<VplOneCopyRecordReport, BackendError> {
    let _ = (
        adapter_index,
        output,
        duration_seconds,
        rate_control,
        requested_chroma,
    );
    Err(BackendError::unsupported(
        "oneVPL WGC D3D11 one-copy record",
        "Windows.Graphics.Capture + Windows D3D11",
        "仅 Windows 可用",
    ))
}

#[cfg(not(windows))]
pub fn record_wgc_d3d11_onecopy_mp4_cancelable(
    adapter_index: u32,
    output: &Path,
    duration_seconds: f32,
    rate_control: &RateControlConfig,
    requested_chroma: ChromaSampling,
    external_stop: Option<std::sync::Arc<std::sync::atomic::AtomicBool>>,
) -> Result<VplOneCopyRecordReport, BackendError> {
    let _ = external_stop;
    record_wgc_d3d11_onecopy_mp4(
        adapter_index,
        output,
        duration_seconds,
        rate_control,
        requested_chroma,
    )
}

#[derive(Debug)]
struct EncodedSurfaceBytes {
    encode_status: i32,
    sync_status: i32,
    timestamp_90k: u64,
    frame_type: u16,
    bytes: Vec<u8>,
}

#[derive(Default)]
struct StageTiming {
    calls: u64,
    total_ns: u128,
    max_ns: u128,
}

impl StageTiming {
    fn add(&mut self, duration: std::time::Duration) {
        let ns = duration.as_nanos();
        self.calls += 1;
        self.total_ns += ns;
        self.max_ns = self.max_ns.max(ns);
    }

    fn avg_ms(&self) -> f64 {
        if self.calls == 0 {
            0.0
        } else {
            self.total_ns as f64 / self.calls as f64 / 1_000_000.0
        }
    }

    fn max_ms(&self) -> f64 {
        self.max_ns as f64 / 1_000_000.0
    }
}

#[derive(Default)]
struct RecordPerf {
    acquire: StageTiming,
    init: StageTiming,
    snapshot: StageTiming,
    source_fence: StageTiming,
    surface: StageTiming,
    convert: StageTiming,
    copy: StageTiming,
    submit: StageTiming,
    sync: StageTiming,
    release_frame: StageTiming,
    frame: StageTiming,
    surface_cache_hits: u64,
    surface_cache_misses: u64,
    dda_accumulated_frames_total: u64,
    dda_accumulated_frames_max: u32,
}

impl RecordPerf {
    fn summary(&self, captured_frames: u32) -> String {
        let accumulated_denominator = if self.acquire.calls == 0 {
            u64::from(captured_frames)
        } else {
            self.acquire.calls
        };
        let avg_accumulated = if accumulated_denominator == 0 {
            0.0
        } else {
            self.dda_accumulated_frames_total as f64 / accumulated_denominator as f64
        };
        format!(
            concat!(
                "perf(cpu ms avg/max): acquire={:.3}/{:.3}, init={:.3}/{:.3}, ",
                "dda_snapshot={:.3}/{:.3}, source_fence={:.3}/{:.3}, surface+native={:.3}/{:.3}, ",
                "convert={:.3}/{:.3}, copy={:.3}/{:.3}, ",
                "encode_submit={:.3}/{:.3}, encode_sync={:.3}/{:.3}, ",
                "release_frame={:.3}/{:.3}, frame_body={:.3}/{:.3}; ",
                "surface_cache hit/miss={}/{}, dda_accumulated avg/max={:.2}/{}, captured={}"
            ),
            self.acquire.avg_ms(),
            self.acquire.max_ms(),
            self.init.avg_ms(),
            self.init.max_ms(),
            self.snapshot.avg_ms(),
            self.snapshot.max_ms(),
            self.source_fence.avg_ms(),
            self.source_fence.max_ms(),
            self.surface.avg_ms(),
            self.surface.max_ms(),
            self.convert.avg_ms(),
            self.convert.max_ms(),
            self.copy.avg_ms(),
            self.copy.max_ms(),
            self.submit.avg_ms(),
            self.submit.max_ms(),
            self.sync.avg_ms(),
            self.sync.max_ms(),
            self.release_frame.avg_ms(),
            self.release_frame.max_ms(),
            self.frame.avg_ms(),
            self.frame.max_ms(),
            self.surface_cache_hits,
            self.surface_cache_misses,
            avg_accumulated,
            self.dda_accumulated_frames_max,
            captured_frames
        )
    }
}

struct AsyncEncode {
    bitstream: MfxBitstream,
    storage: Vec<u8>,
    syncp: MfxSyncPoint,
    _ctrl: Option<Box<MfxEncodeCtrl>>,
    timestamp_90k: u64,
    is_sync: bool,
    discard: bool,
}

enum TrySyncResult {
    Ready(Option<crate::backend::mp4_mux::HevcAccessUnit>),
    NotReady,
}

#[cfg(windows)]
unsafe fn cached_vpl_surface_texture(
    surface: *mut MfxFrameSurface1,
    cache: &mut HashMap<usize, windows::Win32::Graphics::Direct3D11::ID3D11Texture2D>,
    cache_enabled: bool,
) -> Result<(windows::Win32::Graphics::Direct3D11::ID3D11Texture2D, bool), BackendError> {
    use windows::Win32::Graphics::Direct3D11::ID3D11Texture2D;
    use windows::core::Interface;

    let key = surface as usize;
    if cache_enabled && let Some(texture) = cache.get(&key) {
        return Ok((texture.clone(), true));
    }
    let frame_interface = (*surface).FrameInterface;
    if frame_interface.is_null() {
        return Err(BackendError::unsupported(
            "oneVPL record",
            "FrameInterface",
            "oneVPL surface 没有 FrameInterface",
        ));
    }
    let mut native: MfxHDL = ptr::null_mut();
    let mut native_type = 0u32;
    let status = ((*frame_interface).GetNativeHandle)(surface, &mut native, &mut native_type);
    if status != MFX_ERR_NONE || native_type != MFX_RESOURCE_DX11_TEXTURE {
        return Err(BackendError::VplStatus {
            func: "mfxFrameSurfaceInterface::GetNativeHandle",
            status,
        });
    }
    let Some(target) = <ID3D11Texture2D as Interface>::from_raw_borrowed(&native) else {
        return Err(BackendError::unsupported(
            "oneVPL record",
            "native texture",
            "GetNativeHandle 返回值不是 ID3D11Texture2D",
        ));
    };
    let texture = target.clone();
    if cache_enabled {
        cache.insert(key, texture.clone());
    }
    Ok((texture, false))
}

fn sink_status(encoded_sink: &mut Option<&mut dyn VplOneCopyRecordSink>, message: impl AsRef<str>) {
    if let Some(sink) = encoded_sink.as_deref_mut() {
        sink.status(message.as_ref());
    }
}

fn push_record_hevc_sample(
    samples: &mut Vec<crate::backend::mp4_mux::HevcAccessUnit>,
    sample: crate::backend::mp4_mux::HevcAccessUnit,
    encoded_sink: &mut Option<&mut dyn VplOneCopyRecordSink>,
    retain_sample: bool,
) {
    if let Some(sink) = encoded_sink.as_deref_mut() {
        sink.hevc_access_unit(&sample);
    }
    if retain_sample || sample.discard_from_track {
        samples.push(sample);
    }
}

fn mfx_frame_type_is_sync(frame_type: u16) -> bool {
    frame_type & (MFX_FRAMETYPE_IDR | MFX_FRAMETYPE_I) != 0
}

unsafe fn submit_encode_async(
    api: &VplApi,
    session: MfxSession,
    surface: *mut MfxFrameSurface1,
    timestamp_90k: u64,
    is_sync: bool,
    mut storage: Vec<u8>,
    discard: bool,
) -> Result<Option<Box<AsyncEncode>>, BackendError> {
    storage.resize(VPL_BITSTREAM_BYTES + 31, 0);
    let aligned_offset = (32 - (storage.as_ptr() as usize & 31)) & 31;
    let aligned = storage.as_mut_ptr().add(aligned_offset);
    let mut ctrl = if is_sync {
        Some(Box::new(MfxEncodeCtrl {
            FrameType: MFX_FRAMETYPE_I | MFX_FRAMETYPE_REF | MFX_FRAMETYPE_IDR,
            ..unsafe { std::mem::zeroed() }
        }))
    } else {
        None
    };
    let ctrl_ptr = ctrl
        .as_deref_mut()
        .map(|ctrl| ctrl as *mut MfxEncodeCtrl as *mut c_void)
        .unwrap_or(ptr::null_mut());
    let mut flight = Box::new(AsyncEncode {
        bitstream: MfxBitstream {
            CodecId: MFX_CODEC_HEVC,
            Data: aligned,
            MaxLength: VPL_BITSTREAM_BYTES as u32,
            TimeStamp: timestamp_90k,
            ..std::mem::zeroed()
        },
        storage,
        syncp: ptr::null_mut(),
        _ctrl: ctrl,
        timestamp_90k,
        is_sync,
        discard,
    });

    let mut status = (api.mfx_video_encode_frame_async)(
        session,
        ctrl_ptr,
        surface,
        &mut flight.bitstream,
        &mut flight.syncp,
    );
    let mut busy_retries = 0u32;
    while status == MFX_WRN_DEVICE_BUSY && busy_retries < 10 {
        std::thread::sleep(std::time::Duration::from_millis(1));
        flight.syncp = ptr::null_mut();
        flight.bitstream.DataLength = 0;
        flight.bitstream.DataOffset = 0;
        let ctrl_ptr = flight
            ._ctrl
            .as_deref_mut()
            .map(|ctrl| ctrl as *mut MfxEncodeCtrl as *mut c_void)
            .unwrap_or(ptr::null_mut());
        status = (api.mfx_video_encode_frame_async)(
            session,
            ctrl_ptr,
            surface,
            &mut flight.bitstream,
            &mut flight.syncp,
        );
        busy_retries += 1;
    }

    if matches!(status, MFX_ERR_MORE_DATA | MFX_ERR_MORE_SURFACE) {
        return Ok(None);
    }
    if status < MFX_ERR_NONE {
        return Err(BackendError::VplStatus {
            func: "MFXVideoENCODE_EncodeFrameAsync",
            status,
        });
    }
    if flight.syncp.is_null() {
        return Ok(None);
    }
    Ok(Some(flight))
}

unsafe fn sync_one_async_encode(
    api: &VplApi,
    session: MfxSession,
    in_flight: &mut VecDeque<Box<AsyncEncode>>,
    bitstream_pool: &mut Vec<Vec<u8>>,
) -> Result<Option<crate::backend::mp4_mux::HevcAccessUnit>, BackendError> {
    let Some(flight) = in_flight.pop_front() else {
        return Ok(None);
    };
    let sync_status = (api.mfx_video_core_sync_operation)(session, flight.syncp, 60_000);
    if sync_status < MFX_ERR_NONE {
        return Err(BackendError::VplStatus {
            func: "MFXVideoCORE_SyncOperation",
            status: sync_status,
        });
    }
    finish_synced_async_encode(*flight, bitstream_pool)
}

unsafe fn try_sync_one_async_encode(
    api: &VplApi,
    session: MfxSession,
    in_flight: &mut VecDeque<Box<AsyncEncode>>,
    bitstream_pool: &mut Vec<Vec<u8>>,
    timeout_ms: u32,
) -> Result<TrySyncResult, BackendError> {
    let Some(front) = in_flight.front() else {
        return Ok(TrySyncResult::Ready(None));
    };
    let sync_status = (api.mfx_video_core_sync_operation)(session, front.syncp, timeout_ms);
    if matches!(sync_status, MFX_WRN_IN_EXECUTION | MFX_WRN_DEVICE_BUSY) {
        return Ok(TrySyncResult::NotReady);
    }
    if sync_status < MFX_ERR_NONE {
        return Err(BackendError::VplStatus {
            func: "MFXVideoCORE_SyncOperation(short)",
            status: sync_status,
        });
    }
    let flight = in_flight
        .pop_front()
        .expect("front existed before successful short sync");
    finish_synced_async_encode(*flight, bitstream_pool).map(TrySyncResult::Ready)
}

unsafe fn poll_completed_async_encodes(
    api: &VplApi,
    session: MfxSession,
    in_flight: &mut VecDeque<Box<AsyncEncode>>,
    bitstream_pool: &mut Vec<Vec<u8>>,
    samples: &mut Vec<crate::backend::mp4_mux::HevcAccessUnit>,
) -> Result<(), BackendError> {
    while let Some(front) = in_flight.front() {
        let sync_status = (api.mfx_video_core_sync_operation)(session, front.syncp, 0);
        if matches!(sync_status, MFX_WRN_IN_EXECUTION | MFX_WRN_DEVICE_BUSY) {
            break;
        }
        if sync_status < MFX_ERR_NONE {
            return Err(BackendError::VplStatus {
                func: "MFXVideoCORE_SyncOperation(poll)",
                status: sync_status,
            });
        }
        let flight = in_flight
            .pop_front()
            .expect("front existed before successful poll");
        if let Some(sample) = finish_synced_async_encode(*flight, bitstream_pool)? {
            samples.push(sample);
        }
    }
    Ok(())
}

unsafe fn finish_synced_async_encode(
    flight: AsyncEncode,
    bitstream_pool: &mut Vec<Vec<u8>>,
) -> Result<Option<crate::backend::mp4_mux::HevcAccessUnit>, BackendError> {
    let len = flight.bitstream.DataLength as usize;
    if len == 0 {
        bitstream_pool.push(flight.storage);
        return Ok(None);
    }
    let start = flight
        .bitstream
        .Data
        .add(flight.bitstream.DataOffset as usize);
    let data = std::slice::from_raw_parts(start, len).to_vec();
    let is_sync = flight.is_sync || mfx_frame_type_is_sync(flight.bitstream.FrameType);
    bitstream_pool.push(flight.storage);
    Ok(Some(crate::backend::mp4_mux::HevcAccessUnit {
        timestamp_90k: flight.timestamp_90k,
        data,
        is_sync,
        discard_from_track: flight.discard,
    }))
}

unsafe fn encode_surface_bytes(
    api: &VplApi,
    session: MfxSession,
    surface: *mut MfxFrameSurface1,
) -> Result<EncodedSurfaceBytes, BackendError> {
    encode_surface_or_flush_bytes(api, session, surface)
}

unsafe fn encode_surface_or_flush_bytes(
    api: &VplApi,
    session: MfxSession,
    surface: *mut MfxFrameSurface1,
) -> Result<EncodedSurfaceBytes, BackendError> {
    const BITSTREAM_BYTES: usize = 128 * 1024 * 1024;

    let mut storage = vec![0u8; BITSTREAM_BYTES + 31];
    let aligned_offset = (32 - (storage.as_ptr() as usize & 31)) & 31;
    let aligned = storage.as_mut_ptr().add(aligned_offset);
    let mut bitstream: MfxBitstream = std::mem::zeroed();
    bitstream.CodecId = MFX_CODEC_HEVC;
    bitstream.Data = aligned;
    bitstream.MaxLength = BITSTREAM_BYTES as u32;

    let mut syncp: MfxSyncPoint = ptr::null_mut();
    let mut encode_status = (api.mfx_video_encode_frame_async)(
        session,
        ptr::null_mut(),
        surface,
        &mut bitstream,
        &mut syncp,
    );
    let mut busy_retries = 0u32;
    while encode_status == MFX_WRN_DEVICE_BUSY && busy_retries < 50 {
        std::thread::sleep(std::time::Duration::from_millis(2));
        syncp = ptr::null_mut();
        bitstream.DataLength = 0;
        bitstream.DataOffset = 0;
        encode_status = (api.mfx_video_encode_frame_async)(
            session,
            ptr::null_mut(),
            surface,
            &mut bitstream,
            &mut syncp,
        );
        busy_retries += 1;
    }

    if matches!(encode_status, MFX_ERR_MORE_DATA | MFX_ERR_MORE_SURFACE) {
        return Ok(EncodedSurfaceBytes {
            encode_status,
            sync_status: i32::MIN,
            timestamp_90k: 0,
            frame_type: 0,
            bytes: Vec::new(),
        });
    }
    if encode_status < MFX_ERR_NONE {
        return Err(BackendError::VplStatus {
            func: "MFXVideoENCODE_EncodeFrameAsync",
            status: encode_status,
        });
    }

    let sync_status = if !syncp.is_null() {
        (api.mfx_video_core_sync_operation)(session, syncp, 60_000)
    } else {
        i32::MIN
    };
    if sync_status < MFX_ERR_NONE && sync_status != i32::MIN {
        return Err(BackendError::VplStatus {
            func: "MFXVideoCORE_SyncOperation",
            status: sync_status,
        });
    }

    let start = bitstream.Data.add(bitstream.DataOffset as usize);
    let len = bitstream.DataLength as usize;
    let bytes = if len == 0 {
        Vec::new()
    } else {
        std::slice::from_raw_parts(start, len).to_vec()
    };
    Ok(EncodedSurfaceBytes {
        encode_status,
        sync_status,
        timestamp_90k: bitstream.TimeStamp,
        frame_type: bitstream.FrameType,
        bytes,
    })
}

unsafe fn flush_encoder(
    api: &VplApi,
    session: MfxSession,
    samples: &mut Vec<crate::backend::mp4_mux::HevcAccessUnit>,
    encoded_sink: &mut Option<&mut dyn VplOneCopyRecordSink>,
    retain_output_samples: bool,
) -> Result<u32, BackendError> {
    let mut skipped_non_monotonic = 0u32;
    loop {
        let encoded = encode_surface_or_flush_bytes(api, session, ptr::null_mut())?;
        if encoded.encode_status == MFX_ERR_MORE_DATA {
            break;
        }
        if !encoded.bytes.is_empty() {
            let last_timestamp = samples.last().map(|sample| sample.timestamp_90k);
            let last_timestamp = last_track_timestamp_90k(samples).or(last_timestamp);
            let timestamp_90k = encoded.timestamp_90k;
            if let Some(last) = last_timestamp
                && timestamp_90k <= last
            {
                skipped_non_monotonic = skipped_non_monotonic.saturating_add(1);
                continue;
            }
            let is_sync = mfx_frame_type_is_sync(encoded.frame_type);
            push_record_hevc_sample(
                samples,
                crate::backend::mp4_mux::HevcAccessUnit {
                    timestamp_90k,
                    data: encoded.bytes,
                    is_sync,
                    discard_from_track: false,
                },
                encoded_sink,
                retain_output_samples,
            );
        } else {
            break;
        }
    }
    Ok(skipped_non_monotonic)
}

fn last_track_timestamp_90k(samples: &[crate::backend::mp4_mux::HevcAccessUnit]) -> Option<u64> {
    samples
        .iter()
        .rev()
        .find(|sample| !sample.discard_from_track)
        .map(|sample| sample.timestamp_90k)
}

#[cfg(windows)]
unsafe fn create_duplication_on_device(
    adapter1: &windows::Win32::Graphics::Dxgi::IDXGIAdapter1,
    device: &windows::Win32::Graphics::Direct3D11::ID3D11Device,
    route: VplRecordRoute,
) -> Result<windows::Win32::Graphics::Dxgi::IDXGIOutputDuplication, BackendError> {
    use windows::Win32::Graphics::Dxgi::Common::{
        DXGI_FORMAT_B8G8R8A8_UNORM, DXGI_FORMAT_B8G8R8X8_UNORM, DXGI_FORMAT_R8G8B8A8_UNORM,
        DXGI_FORMAT_R16G16B16A16_FLOAT,
    };
    use windows::Win32::Graphics::Dxgi::{IDXGIOutput1, IDXGIOutput5};
    use windows::core::Interface;

    let output = adapter1
        .EnumOutputs(0)
        .map_err(|err| BackendError::WindowsApi {
            func: "IDXGIAdapter1::EnumOutputs(0)",
            message: err.to_string(),
        })?;
    let output1: IDXGIOutput1 = output.cast().map_err(|err| BackendError::WindowsApi {
        func: "IDXGIOutput::cast<IDXGIOutput1>",
        message: err.to_string(),
    })?;
    if let Ok(output5) = output.cast::<IDXGIOutput5>() {
        if route.requires_fp16_capture() {
            // 10-bit/HDR 路线不能退到 BGRA8：那会在 capture 阶段丢失源位深，
            // 与“源是什么位深，输出就是什么位深”的后端契约冲突。
            let supported_formats = [DXGI_FORMAT_R16G16B16A16_FLOAT];
            match output5.DuplicateOutput1(device, 0, &supported_formats) {
                Ok(duplication) => return Ok(duplication),
                Err(err) => {
                    return Err(BackendError::unsupported(
                        "DDA capture",
                        format!("{} DuplicateOutput1 FP16 失败: {err}", route.summary()),
                        "不支持的桌面模式",
                    ));
                }
            }
        } else {
            let supported_formats = [
                // SDR8 route keeps the desktop in ordinary 8-bit RGB before
                // the SDR BT.709 GPU conversion. FP16 stays last as a diagnostic
                // fallback rather than the preferred output for an 8-bit route.
                DXGI_FORMAT_B8G8R8A8_UNORM,
                DXGI_FORMAT_B8G8R8X8_UNORM,
                DXGI_FORMAT_R8G8B8A8_UNORM,
                DXGI_FORMAT_R16G16B16A16_FLOAT,
            ];
            match output5.DuplicateOutput1(device, 0, &supported_formats) {
                Ok(duplication) => return Ok(duplication),
                Err(err) => {
                    // 某些 WDDM/驱动组合能枚举 IDXGIOutput5，但 DuplicateOutput1
                    // 对格式列表返回 UNSUPPORTED。只有 SDR8 路线可退回普通 DDA。
                    let _ = err;
                }
            }
        }
    }

    if route.requires_fp16_capture() {
        return Err(BackendError::unsupported(
            "DDA capture",
            route.summary(),
            "不支持的桌面模式",
        ));
    }

    output1
        .DuplicateOutput(device)
        .map_err(|err| BackendError::WindowsApi {
            func: "IDXGIOutput1::DuplicateOutput(oneVPL device)",
            message: err.to_string(),
        })
}

#[cfg(windows)]
unsafe fn create_route_intermediate(
    device: &windows::Win32::Graphics::Direct3D11::ID3D11Device,
    target_desc: &windows::Win32::Graphics::Direct3D11::D3D11_TEXTURE2D_DESC,
    route: VplRecordRoute,
    allow_uav: bool,
) -> Result<windows::Win32::Graphics::Direct3D11::ID3D11Texture2D, BackendError> {
    use windows::Win32::Graphics::Direct3D11::{
        D3D11_BIND_RENDER_TARGET, D3D11_BIND_UNORDERED_ACCESS, D3D11_TEXTURE2D_DESC,
        D3D11_USAGE_DEFAULT,
    };

    let mut bind_flags = if matches!(route.fourcc, MFX_FOURCC_P010 | MFX_FOURCC_RGB4) {
        D3D11_BIND_RENDER_TARGET.0 as u32
    } else {
        0
    };
    if allow_uav {
        bind_flags |= D3D11_BIND_UNORDERED_ACCESS.0 as u32;
    }
    let desc = D3D11_TEXTURE2D_DESC {
        Width: target_desc.Width,
        Height: target_desc.Height,
        MipLevels: 1,
        ArraySize: 1,
        Format: route.try_dxgi_format()?,
        SampleDesc: target_desc.SampleDesc,
        Usage: D3D11_USAGE_DEFAULT,
        BindFlags: bind_flags,
        CPUAccessFlags: 0,
        MiscFlags: 0,
    };
    let mut texture = None;
    device
        .CreateTexture2D(&desc, None, Some(&mut texture))
        .map_err(|err| BackendError::WindowsApi {
            func: "ID3D11Device::CreateTexture2D(route intermediate)",
            message: err.to_string(),
        })?;
    texture.ok_or_else(|| BackendError::WindowsApi {
        func: "CreateTexture2D(route intermediate)",
        message: "返回空纹理".to_owned(),
    })
}

#[cfg(windows)]
unsafe fn create_dda_snapshot_texture(
    device: &windows::Win32::Graphics::Direct3D11::ID3D11Device,
    source_desc: &windows::Win32::Graphics::Direct3D11::D3D11_TEXTURE2D_DESC,
) -> Result<windows::Win32::Graphics::Direct3D11::ID3D11Texture2D, BackendError> {
    use windows::Win32::Graphics::Direct3D11::{
        D3D11_BIND_SHADER_RESOURCE, D3D11_TEXTURE2D_DESC, D3D11_USAGE_DEFAULT,
    };

    let desc = D3D11_TEXTURE2D_DESC {
        Width: source_desc.Width.max(1),
        Height: source_desc.Height.max(1),
        MipLevels: 1,
        ArraySize: 1,
        Format: source_desc.Format,
        SampleDesc: source_desc.SampleDesc,
        Usage: D3D11_USAGE_DEFAULT,
        BindFlags: D3D11_BIND_SHADER_RESOURCE.0 as u32,
        CPUAccessFlags: 0,
        MiscFlags: 0,
    };
    let mut texture = None;
    device
        .CreateTexture2D(&desc, None, Some(&mut texture))
        .map_err(|err| BackendError::WindowsApi {
            func: "ID3D11Device::CreateTexture2D(DDA snapshot)",
            message: err.to_string(),
        })?;
    texture.ok_or_else(|| BackendError::WindowsApi {
        func: "CreateTexture2D(DDA snapshot)",
        message: "返回空纹理".to_owned(),
    })
}

#[cfg(windows)]
unsafe fn create_d3d11_device_for_adapter(
    adapter1: &windows::Win32::Graphics::Dxgi::IDXGIAdapter1,
) -> Result<
    (
        windows::Win32::Graphics::Direct3D11::ID3D11Device,
        windows::Win32::Graphics::Direct3D11::ID3D11DeviceContext,
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
    use windows::Win32::Graphics::Dxgi::IDXGIAdapter;
    use windows::core::Interface;

    let adapter: IDXGIAdapter = adapter1.cast().map_err(|err| BackendError::WindowsApi {
        func: "IDXGIAdapter1::cast<IDXGIAdapter>(capture device)",
        message: err.to_string(),
    })?;
    let mut device: Option<ID3D11Device> = None;
    let mut context: Option<ID3D11DeviceContext> = None;
    let mut feature_level = D3D_FEATURE_LEVEL(0);
    let levels = [D3D_FEATURE_LEVEL_11_1, D3D_FEATURE_LEVEL_11_0];
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
        func: "D3D11CreateDevice(capture)",
        message: err.to_string(),
    })?;
    let device = device.ok_or_else(|| BackendError::WindowsApi {
        func: "D3D11CreateDevice(capture)",
        message: "返回空 ID3D11Device".to_owned(),
    })?;
    let context = context.ok_or_else(|| BackendError::WindowsApi {
        func: "D3D11CreateDevice(capture)",
        message: "返回空 ID3D11DeviceContext".to_owned(),
    })?;
    Ok((device, context))
}

#[cfg(windows)]
unsafe fn create_shared_snapshot_slot(
    id: usize,
    capture_device: &windows::Win32::Graphics::Direct3D11::ID3D11Device,
    encoder_device: &windows::Win32::Graphics::Direct3D11::ID3D11Device,
    source_desc: &windows::Win32::Graphics::Direct3D11::D3D11_TEXTURE2D_DESC,
) -> Result<SnapshotSlot, BackendError> {
    use windows::Win32::Graphics::Direct3D11::{
        D3D11_BIND_SHADER_RESOURCE, D3D11_RESOURCE_MISC_SHARED_KEYEDMUTEX,
        D3D11_RESOURCE_MISC_SHARED_NTHANDLE, D3D11_TEXTURE2D_DESC, D3D11_USAGE_DEFAULT,
        ID3D11Device1, ID3D11Texture2D,
    };
    use windows::Win32::Graphics::Dxgi::{
        DXGI_SHARED_RESOURCE_READ, IDXGIKeyedMutex, IDXGIResource1,
    };
    use windows::core::{Interface, PCWSTR};

    let desc = D3D11_TEXTURE2D_DESC {
        Width: source_desc.Width.max(1),
        Height: source_desc.Height.max(1),
        MipLevels: 1,
        ArraySize: 1,
        Format: source_desc.Format,
        SampleDesc: source_desc.SampleDesc,
        Usage: D3D11_USAGE_DEFAULT,
        BindFlags: D3D11_BIND_SHADER_RESOURCE.0 as u32,
        CPUAccessFlags: 0,
        MiscFlags: (D3D11_RESOURCE_MISC_SHARED_NTHANDLE.0 | D3D11_RESOURCE_MISC_SHARED_KEYEDMUTEX.0)
            as u32,
    };
    let mut capture_texture = None;
    capture_device
        .CreateTexture2D(&desc, None, Some(&mut capture_texture))
        .map_err(|err| BackendError::WindowsApi {
            func: "ID3D11Device::CreateTexture2D(shared DDA snapshot)",
            message: err.to_string(),
        })?;
    let capture_texture = capture_texture.ok_or_else(|| BackendError::WindowsApi {
        func: "CreateTexture2D(shared DDA snapshot)",
        message: "返回空 capture texture".to_owned(),
    })?;
    let capture_mutex: IDXGIKeyedMutex =
        capture_texture
            .cast()
            .map_err(|err| BackendError::WindowsApi {
                func: "ID3D11Texture2D::cast<IDXGIKeyedMutex>(capture)",
                message: err.to_string(),
            })?;
    let resource1: IDXGIResource1 =
        capture_texture
            .cast()
            .map_err(|err| BackendError::WindowsApi {
                func: "ID3D11Texture2D::cast<IDXGIResource1>(shared DDA snapshot)",
                message: err.to_string(),
            })?;
    let shared_handle = resource1
        .CreateSharedHandle(None, DXGI_SHARED_RESOURCE_READ.0, PCWSTR::null())
        .map_err(|err| BackendError::WindowsApi {
            func: "IDXGIResource1::CreateSharedHandle(shared DDA snapshot)",
            message: err.to_string(),
        })?;
    let encoder_device1: ID3D11Device1 =
        encoder_device
            .cast()
            .map_err(|err| BackendError::WindowsApi {
                func: "ID3D11Device::cast<ID3D11Device1>(encoder)",
                message: err.to_string(),
            })?;
    let encoder_texture: ID3D11Texture2D = encoder_device1
        .OpenSharedResource1(shared_handle)
        .map_err(|err| BackendError::WindowsApi {
            func: "ID3D11Device1::OpenSharedResource1(shared DDA snapshot)",
            message: err.to_string(),
        })?;
    let encoder_mutex: IDXGIKeyedMutex =
        encoder_texture
            .cast()
            .map_err(|err| BackendError::WindowsApi {
                func: "ID3D11Texture2D::cast<IDXGIKeyedMutex>(encoder)",
                message: err.to_string(),
            })?;
    Ok(SnapshotSlot {
        inner: std::sync::Arc::new(SnapshotSlotInner {
            id,
            capture_texture,
            encoder_texture,
            capture_mutex,
            encoder_mutex,
            encoder_fence: GpuCompletionFence::new(encoder_device)?,
            shared_handle,
        }),
    })
}

#[cfg(windows)]
#[derive(Clone)]
struct SnapshotSlot {
    inner: std::sync::Arc<SnapshotSlotInner>,
}

#[cfg(windows)]
impl std::ops::Deref for SnapshotSlot {
    type Target = SnapshotSlotInner;

    fn deref(&self) -> &Self::Target {
        &self.inner
    }
}

#[cfg(windows)]
struct SnapshotSlotInner {
    id: usize,
    capture_texture: windows::Win32::Graphics::Direct3D11::ID3D11Texture2D,
    encoder_texture: windows::Win32::Graphics::Direct3D11::ID3D11Texture2D,
    capture_mutex: windows::Win32::Graphics::Dxgi::IDXGIKeyedMutex,
    encoder_mutex: windows::Win32::Graphics::Dxgi::IDXGIKeyedMutex,
    encoder_fence: GpuCompletionFence,
    shared_handle: windows::Win32::Foundation::HANDLE,
}

#[cfg(windows)]
unsafe impl Send for SnapshotSlot {}
#[cfg(windows)]
unsafe impl Sync for SnapshotSlot {}
#[cfg(windows)]
unsafe impl Send for SnapshotSlotInner {}
#[cfg(windows)]
unsafe impl Sync for SnapshotSlotInner {}

#[cfg(windows)]
impl Drop for SnapshotSlotInner {
    fn drop(&mut self) {
        unsafe {
            let _ = windows::Win32::Foundation::CloseHandle(self.shared_handle);
        }
    }
}

#[cfg(windows)]
struct CapturedSnapshot {
    slot: CaptureFrameSlot,
    source_desc: windows::Win32::Graphics::Direct3D11::D3D11_TEXTURE2D_DESC,
    move_rect_bytes: u32,
    dirty_rects: Vec<windows::Win32::Foundation::RECT>,
    timestamp_90k: u64,
    timestamp_100ns: Option<i64>,
    capture_index: u64,
    accumulated_frames: u32,
    warmup: bool,
}

#[cfg(windows)]
struct WgcLocalSlot {
    id: usize,
    texture: windows::Win32::Graphics::Direct3D11::ID3D11Texture2D,
    fence: GpuCompletionFence,
}

#[cfg(windows)]
unsafe impl Send for WgcLocalSlot {}
#[cfg(windows)]
unsafe impl Sync for WgcLocalSlot {}

#[cfg(windows)]
enum CaptureFrameSlot {
    Shared(SnapshotSlot),
    WgcLocal(WgcLocalSlot),
}

#[cfg(windows)]
#[derive(Debug, Clone)]
struct CaptureStats {
    acquired: u64,
    copied: u64,
    dropped_no_slot: u64,
    dropped_queue_full: u64,
    dropped_duplicate_timestamp: u64,
    dropped_warmup: u64,
    dda_timeouts: u64,
    accumulated_frames_total: u64,
    accumulated_frames_max: u32,
    source_interval_count: u64,
    source_interval_min_90k: u64,
    source_interval_max_90k: u64,
    source_interval_above_7_5ms: u64,
    source_interval_below_6_5ms: u64,
    source_interval_bad_examples: Vec<(u64, u64)>,
    callback_frame_count: u64,
    callback_frame_cpu_total_us: u64,
    callback_frame_cpu_max_us: u64,
    callback_copy_cpu_max_us: u64,
    wgc_frame_queue_count: u64,
    wgc_frame_queue_total_us: u64,
    wgc_frame_queue_max_us: u64,
    wgc_input_queue_max: u64,
}

#[cfg(windows)]
impl CaptureStats {
    fn new() -> Self {
        Self {
            acquired: 0,
            copied: 0,
            dropped_no_slot: 0,
            dropped_queue_full: 0,
            dropped_duplicate_timestamp: 0,
            dropped_warmup: 0,
            dda_timeouts: 0,
            accumulated_frames_total: 0,
            accumulated_frames_max: 0,
            source_interval_count: 0,
            source_interval_min_90k: u64::MAX,
            source_interval_max_90k: 0,
            source_interval_above_7_5ms: 0,
            source_interval_below_6_5ms: 0,
            source_interval_bad_examples: Vec::new(),
            callback_frame_count: 0,
            callback_frame_cpu_total_us: 0,
            callback_frame_cpu_max_us: 0,
            callback_copy_cpu_max_us: 0,
            wgc_frame_queue_count: 0,
            wgc_frame_queue_total_us: 0,
            wgc_frame_queue_max_us: 0,
            wgc_input_queue_max: 0,
        }
    }

    fn observe_source_interval(&mut self, frame_index: u64, delta_90k: u64) {
        self.source_interval_count = self.source_interval_count.saturating_add(1);
        self.source_interval_min_90k = self.source_interval_min_90k.min(delta_90k);
        self.source_interval_max_90k = self.source_interval_max_90k.max(delta_90k);
        let above_threshold = (7.5f64 * VIDEO_CLOCK_HZ as f64 / 1000.0).round() as u64;
        let below_threshold = (6.5f64 * VIDEO_CLOCK_HZ as f64 / 1000.0).round() as u64;
        if delta_90k > above_threshold {
            self.source_interval_above_7_5ms = self.source_interval_above_7_5ms.saturating_add(1);
            if self.source_interval_bad_examples.len() < 8 {
                self.source_interval_bad_examples
                    .push((frame_index, delta_90k));
            }
        }
        if delta_90k < below_threshold {
            self.source_interval_below_6_5ms = self.source_interval_below_6_5ms.saturating_add(1);
            if self.source_interval_bad_examples.len() < 8 {
                self.source_interval_bad_examples
                    .push((frame_index, delta_90k));
            }
        }
    }

    fn observe_callback_frame_cpu(
        &mut self,
        frame_duration: std::time::Duration,
        copy_duration: std::time::Duration,
    ) {
        let frame_us = frame_duration.as_micros().min(u128::from(u64::MAX)) as u64;
        let copy_us = copy_duration.as_micros().min(u128::from(u64::MAX)) as u64;
        self.callback_frame_count = self.callback_frame_count.saturating_add(1);
        self.callback_frame_cpu_total_us =
            self.callback_frame_cpu_total_us.saturating_add(frame_us);
        self.callback_frame_cpu_max_us = self.callback_frame_cpu_max_us.max(frame_us);
        self.callback_copy_cpu_max_us = self.callback_copy_cpu_max_us.max(copy_us);
    }

    fn observe_wgc_frame_queue_delay(&mut self, delay: std::time::Duration) {
        let delay_us = delay.as_micros().min(u128::from(u64::MAX)) as u64;
        self.wgc_frame_queue_count = self.wgc_frame_queue_count.saturating_add(1);
        self.wgc_frame_queue_total_us = self.wgc_frame_queue_total_us.saturating_add(delay_us);
        self.wgc_frame_queue_max_us = self.wgc_frame_queue_max_us.max(delay_us);
    }

    fn summary(&self) -> String {
        let avg_accumulated = if self.acquired == 0 {
            0.0
        } else {
            self.accumulated_frames_total as f64 / self.acquired as f64
        };
        let min_interval_ms = if self.source_interval_min_90k == u64::MAX {
            0.0
        } else {
            self.source_interval_min_90k as f64 * 1000.0 / VIDEO_CLOCK_HZ as f64
        };
        let max_interval_ms = self.source_interval_max_90k as f64 * 1000.0 / VIDEO_CLOCK_HZ as f64;
        let bad_examples = self
            .source_interval_bad_examples
            .iter()
            .map(|(index, delta)| {
                format!(
                    "{}:{:.3}ms",
                    index,
                    *delta as f64 * 1000.0 / VIDEO_CLOCK_HZ as f64
                )
            })
            .collect::<Vec<_>>()
            .join(",");
        let callback_avg_ms = if self.callback_frame_count == 0 {
            0.0
        } else {
            self.callback_frame_cpu_total_us as f64 / self.callback_frame_count as f64 / 1000.0
        };
        let wgc_queue_avg_ms = if self.wgc_frame_queue_count == 0 {
            0.0
        } else {
            self.wgc_frame_queue_total_us as f64 / self.wgc_frame_queue_count as f64 / 1000.0
        };
        format!(
            "capture-thread: acquired={}, copied={}, dropped_no_slot={}, dropped_queue_full={}, dropped_duplicate_timestamp={}, dropped_warmup={}, dda_timeouts={}, dda_accumulated avg/max={:.2}/{}, source_interval count={} min/max={:.3}/{:.3}ms above7.5={} below6.5={} bad_examples=[{}], callback_cpu avg/max={:.3}/{:.3}ms copy_max={:.3}ms, wgc_frame_queue avg/max={:.3}/{:.3}ms input_queue_max={}",
            self.acquired,
            self.copied,
            self.dropped_no_slot,
            self.dropped_queue_full,
            self.dropped_duplicate_timestamp,
            self.dropped_warmup,
            self.dda_timeouts,
            avg_accumulated,
            self.accumulated_frames_max,
            self.source_interval_count,
            min_interval_ms,
            max_interval_ms,
            self.source_interval_above_7_5ms,
            self.source_interval_below_6_5ms,
            bad_examples,
            callback_avg_ms,
            self.callback_frame_cpu_max_us as f64 / 1000.0,
            self.callback_copy_cpu_max_us as f64 / 1000.0,
            wgc_queue_avg_ms,
            self.wgc_frame_queue_max_us as f64 / 1000.0,
            self.wgc_input_queue_max,
        )
    }
}

#[cfg(windows)]
fn snapshot_slot_matches(
    slot: &SnapshotSlot,
    desc: &windows::Win32::Graphics::Direct3D11::D3D11_TEXTURE2D_DESC,
) -> bool {
    let mut slot_desc = windows::Win32::Graphics::Direct3D11::D3D11_TEXTURE2D_DESC::default();
    unsafe {
        slot.capture_texture.GetDesc(&mut slot_desc);
    }
    slot_desc.Width == desc.Width
        && slot_desc.Height == desc.Height
        && slot_desc.Format.0 == desc.Format.0
}

#[cfg(windows)]
fn wgc_local_slot_matches(
    slot: &WgcLocalSlot,
    desc: &windows::Win32::Graphics::Direct3D11::D3D11_TEXTURE2D_DESC,
) -> bool {
    let mut slot_desc = windows::Win32::Graphics::Direct3D11::D3D11_TEXTURE2D_DESC::default();
    unsafe {
        slot.texture.GetDesc(&mut slot_desc);
    }
    slot_desc.Width == desc.Width
        && slot_desc.Height == desc.Height
        && slot_desc.Format.0 == desc.Format.0
}

#[cfg(windows)]
unsafe fn create_wgc_local_slot(
    id: usize,
    device: &windows::Win32::Graphics::Direct3D11::ID3D11Device,
    source_desc: &windows::Win32::Graphics::Direct3D11::D3D11_TEXTURE2D_DESC,
) -> Result<WgcLocalSlot, BackendError> {
    use windows::Win32::Graphics::Direct3D11::{D3D11_TEXTURE2D_DESC, D3D11_USAGE_DEFAULT};

    let desc = D3D11_TEXTURE2D_DESC {
        Width: source_desc.Width.max(1),
        Height: source_desc.Height.max(1),
        MipLevels: 1,
        ArraySize: 1,
        Format: source_desc.Format,
        SampleDesc: source_desc.SampleDesc,
        Usage: D3D11_USAGE_DEFAULT,
        BindFlags: 0,
        CPUAccessFlags: 0,
        MiscFlags: 0,
    };
    let mut texture = None;
    device
        .CreateTexture2D(&desc, None, Some(&mut texture))
        .map_err(|err| BackendError::WindowsApi {
            func: "ID3D11Device::CreateTexture2D(WGC local snapshot)",
            message: err.to_string(),
        })?;
    let texture = texture.ok_or_else(|| BackendError::WindowsApi {
        func: "CreateTexture2D(WGC local snapshot)",
        message: "返回空纹理".to_owned(),
    })?;
    Ok(WgcLocalSlot {
        id,
        texture,
        fence: GpuCompletionFence::new(device)?,
    })
}

#[cfg(windows)]
enum CaptureMsg {
    Frame(CapturedSnapshot),
    Done(CaptureStats),
    Error(String),
}

#[cfg(windows)]
#[allow(clippy::too_many_arguments)]
fn spawn_dda_capture_thread(
    adapter1: windows::Win32::Graphics::Dxgi::IDXGIAdapter1,
    device: windows::Win32::Graphics::Direct3D11::ID3D11Device,
    context: windows::Win32::Graphics::Direct3D11::ID3D11DeviceContext,
    encoder_device: windows::Win32::Graphics::Direct3D11::ID3D11Device,
    d3d_multithread: Option<windows::Win32::Graphics::Direct3D11::ID3D11Multithread>,
    start: std::time::Instant,
    end_at: std::time::Instant,
    source_stop_90k: u64,
    qpc_frequency: i64,
    route: VplRecordRoute,
    target_width: u32,
    target_height: u32,
    pool_size: usize,
    stop: std::sync::Arc<std::sync::atomic::AtomicBool>,
    frame_tx: std::sync::mpsc::Sender<CaptureMsg>,
    free_rx: std::sync::mpsc::Receiver<CaptureFrameSlot>,
) -> std::thread::JoinHandle<()> {
    std::thread::spawn(move || {
        let result = unsafe {
            run_dda_capture_thread(
                adapter1,
                device,
                context,
                encoder_device,
                d3d_multithread,
                start,
                end_at,
                source_stop_90k,
                qpc_frequency,
                route,
                target_width,
                target_height,
                pool_size,
                stop,
                &frame_tx,
                free_rx,
            )
        };
        match result {
            Ok(stats) => {
                let _ = frame_tx.send(CaptureMsg::Done(stats));
            }
            Err(message) => {
                let _ = frame_tx.send(CaptureMsg::Error(message));
            }
        }
    })
}

#[cfg(windows)]
#[allow(clippy::too_many_arguments)]
unsafe fn run_dda_capture_thread(
    adapter1: windows::Win32::Graphics::Dxgi::IDXGIAdapter1,
    device: windows::Win32::Graphics::Direct3D11::ID3D11Device,
    context: windows::Win32::Graphics::Direct3D11::ID3D11DeviceContext,
    encoder_device: windows::Win32::Graphics::Direct3D11::ID3D11Device,
    d3d_multithread: Option<windows::Win32::Graphics::Direct3D11::ID3D11Multithread>,
    start: std::time::Instant,
    end_at: std::time::Instant,
    source_stop_90k: u64,
    qpc_frequency: i64,
    route: VplRecordRoute,
    target_width: u32,
    target_height: u32,
    pool_size: usize,
    stop: std::sync::Arc<std::sync::atomic::AtomicBool>,
    frame_tx: &std::sync::mpsc::Sender<CaptureMsg>,
    free_rx: std::sync::mpsc::Receiver<CaptureFrameSlot>,
) -> Result<CaptureStats, String> {
    use std::collections::VecDeque;
    use std::sync::atomic::Ordering;
    use windows::Win32::Graphics::Direct3D11::D3D11_TEXTURE2D_DESC;
    use windows::Win32::Graphics::Dxgi::{
        DXGI_ERROR_WAIT_TIMEOUT, DXGI_OUTDUPL_FRAME_INFO, IDXGIResource,
    };
    use windows::core::Interface;

    let _thread_priority = RecordThreadPriorityGuard::raise_capture_thread();
    let duplication =
        create_duplication_on_device(&adapter1, &device, route).map_err(|err| err.to_string())?;
    let mut stats = CaptureStats::new();
    let mut free_slots: VecDeque<SnapshotSlot> = VecDeque::new();
    let mut source_desc0: Option<D3D11_TEXTURE2D_DESC> = None;
    let mut snapshot_desc0: Option<D3D11_TEXTURE2D_DESC> = None;
    let mut route_intermediate: Option<windows::Win32::Graphics::Direct3D11::ID3D11Texture2D> =
        None;
    let mut route_converter: Option<GpuRecordConverter> = None;
    let mut capture_index = 0u64;
    let mut timestamp_origin_qpc: Option<i64> = None;
    let mut last_timestamp_90k: Option<u64> = None;
    let mut last_accepted_present_qpc: Option<i64> = None;
    let mut encoder_warmup_pending = false;
    let mut encoder_warmup_done = false;
    let dda_pipeline_warmup_frames = 4u32;
    // Keep only a short frame-count warmup after encoder warmup. The official
    // timeline starts from DDA LastPresentTime and does not require a fixed
    // refresh-rate interval to become "stable".
    let dda_pipeline_warmup_stable_intervals_required = 0u32;
    let mut dda_pipeline_warmup_remaining = 0u32;
    let dda_pipeline_warmup_stable_intervals = 0u32;
    let max_end_at = end_at + std::time::Duration::from_secs(3);

    while !stop.load(Ordering::Relaxed) && std::time::Instant::now() < max_end_at {
        while let Ok(slot) = free_rx.try_recv() {
            if encoder_warmup_pending {
                encoder_warmup_pending = false;
                encoder_warmup_done = true;
                dda_pipeline_warmup_remaining = dda_pipeline_warmup_frames;
                timestamp_origin_qpc = None;
                last_timestamp_90k = None;
                last_accepted_present_qpc = None;
            }
            let CaptureFrameSlot::Shared(slot) = slot else {
                continue;
            };
            if snapshot_desc0
                .as_ref()
                .is_none_or(|desc| snapshot_slot_matches(&slot, desc))
            {
                free_slots.push_back(slot);
            }
        }

        let mut frame_info = DXGI_OUTDUPL_FRAME_INFO::default();
        let mut resource: Option<IDXGIResource> = None;
        match duplication.AcquireNextFrame(8, &mut frame_info, &mut resource) {
            Ok(()) => {}
            Err(err) if err.code() == DXGI_ERROR_WAIT_TIMEOUT => {
                stats.dda_timeouts += 1;
                continue;
            }
            Err(err) => {
                return Err(format!(
                    "IDXGIOutputDuplication::AcquireNextFrame(capture): {err}"
                ));
            }
        }

        let mut reached_source_end = false;
        let frame_result = (|| -> Result<(), String> {
            stats.acquired += 1;
            stats.accumulated_frames_total += u64::from(frame_info.AccumulatedFrames);
            stats.accumulated_frames_max = stats
                .accumulated_frames_max
                .max(frame_info.AccumulatedFrames);

            let resource =
                resource.ok_or_else(|| "AcquireNextFrame returned null resource".to_owned())?;
            let source: windows::Win32::Graphics::Direct3D11::ID3D11Texture2D = resource
                .cast()
                .map_err(|err| format!("IDXGIResource::cast<ID3D11Texture2D>(capture): {err}"))?;
            let mut source_desc = D3D11_TEXTURE2D_DESC::default();
            source.GetDesc(&mut source_desc);
            let frame_metadata =
                read_dda_frame_metadata(&duplication, frame_info.TotalMetadataBufferSize)
                    .map_err(|err| err.to_string())?;

            if frame_info.LastPresentTime > 0
                && last_accepted_present_qpc.is_some_and(|last| frame_info.LastPresentTime <= last)
            {
                stats.dropped_duplicate_timestamp += 1;
                return Ok(());
            }
            if qpc_frequency > 0
                && timestamp_origin_qpc.is_none()
                && frame_info.LastPresentTime <= 0
            {
                stats.dropped_duplicate_timestamp += 1;
                return Ok(());
            }

            let source_changed = source_desc0.is_none_or(|first| {
                first.Width != source_desc.Width
                    || first.Height != source_desc.Height
                    || first.Format.0 != source_desc.Format.0
            });
            if source_changed {
                let first_source = source_desc0.is_none();
                source_desc0 = Some(source_desc);
                let snapshot_desc = D3D11_TEXTURE2D_DESC {
                    Width: target_width.max(1),
                    Height: target_height.max(1),
                    MipLevels: 1,
                    ArraySize: 1,
                    Format: route.try_dxgi_format().map_err(|err| err.to_string())?,
                    SampleDesc: source_desc.SampleDesc,
                    Usage: source_desc.Usage,
                    BindFlags: 0,
                    CPUAccessFlags: 0,
                    MiscFlags: 0,
                };
                route_intermediate = Some(
                    create_route_intermediate(&device, &snapshot_desc, route, true)
                        .map_err(|err| err.to_string())?,
                );
                let intermediate = route_intermediate
                    .as_ref()
                    .ok_or_else(|| "DDA route intermediate missing after create".to_owned())?;
                route_converter = Some(
                    GpuRecordConverter::new(
                        route,
                        &device,
                        &context,
                        intermediate,
                        source_desc.Width,
                        source_desc.Height,
                        true,
                    )
                    .map_err(|err| err.to_string())?,
                );
                snapshot_desc0 = Some(snapshot_desc);
                free_slots.clear();
                for id in 0..pool_size {
                    let slot =
                        create_shared_snapshot_slot(id, &device, &encoder_device, &snapshot_desc)
                            .map_err(|err| err.to_string())?;
                    free_slots.push_back(slot);
                }
                if first_source {
                    stats.dropped_warmup += 1;
                    return Ok(());
                }
            }
            let snapshot_desc =
                snapshot_desc0.ok_or_else(|| "DDA route snapshot desc missing".to_owned())?;

            if encoder_warmup_pending {
                if frame_info.LastPresentTime > 0 {
                    last_accepted_present_qpc = Some(frame_info.LastPresentTime);
                }
                stats.dropped_warmup += 1;
                return Ok(());
            }

            let Some(slot) = free_slots.pop_front() else {
                stats.dropped_no_slot += 1;
                return Ok(());
            };

            {
                let _guard = D3d11MultithreadGuard::enter(&d3d_multithread);
                slot.capture_mutex
                    .AcquireSync(0, 1_000)
                    .map_err(|err| format!("IDXGIKeyedMutex::AcquireSync(capture): {err}"))?;
                let intermediate = route_intermediate
                    .as_ref()
                    .ok_or_else(|| "DDA route intermediate missing".to_owned())?;
                let converter = route_converter
                    .as_ref()
                    .ok_or_else(|| "DDA route converter missing".to_owned())?;
                converter
                    .convert(&source)
                    .and_then(|()| {
                        copy_texture_resource(&context, intermediate, &slot.capture_texture)
                    })
                    .map_err(|err| err.to_string())?;
                slot.capture_mutex
                    .ReleaseSync(1)
                    .map_err(|err| format!("IDXGIKeyedMutex::ReleaseSync(capture): {err}"))?;
            }
            stats.copied += 1;
            if frame_info.LastPresentTime > 0 {
                last_accepted_present_qpc = Some(frame_info.LastPresentTime);
            }
            if !encoder_warmup_done {
                let captured = CapturedSnapshot {
                    slot: CaptureFrameSlot::Shared(slot),
                    source_desc: snapshot_desc,
                    move_rect_bytes: frame_metadata.move_rect_bytes,
                    dirty_rects: frame_metadata.dirty_rects,
                    timestamp_90k: 0,
                    timestamp_100ns: qpc_counter_to_100ns(
                        frame_info.LastPresentTime,
                        qpc_frequency,
                    ),
                    capture_index,
                    accumulated_frames: frame_info.AccumulatedFrames,
                    warmup: true,
                };
                encoder_warmup_pending = true;
                stats.dropped_warmup += 1;
                if frame_tx.send(CaptureMsg::Frame(captured)).is_err() {
                    stop.store(true, Ordering::Relaxed);
                }
                return Ok(());
            }
            let pipeline_warmup_frame = dda_pipeline_warmup_remaining > 0
                || dda_pipeline_warmup_stable_intervals
                    < dda_pipeline_warmup_stable_intervals_required;
            if pipeline_warmup_frame {
                dda_pipeline_warmup_remaining = dda_pipeline_warmup_remaining.saturating_sub(1);
                stats.dropped_warmup += 1;
                timestamp_origin_qpc = None;
                last_timestamp_90k = None;
                let captured = CapturedSnapshot {
                    slot: CaptureFrameSlot::Shared(slot),
                    source_desc: snapshot_desc,
                    move_rect_bytes: frame_metadata.move_rect_bytes,
                    dirty_rects: frame_metadata.dirty_rects,
                    timestamp_90k: 0,
                    timestamp_100ns: qpc_counter_to_100ns(
                        frame_info.LastPresentTime,
                        qpc_frequency,
                    ),
                    capture_index,
                    accumulated_frames: frame_info.AccumulatedFrames,
                    warmup: true,
                };
                if frame_tx.send(CaptureMsg::Frame(captured)).is_err() {
                    stop.store(true, Ordering::Relaxed);
                }
                return Ok(());
            }
            let previous_timestamp_90k = last_timestamp_90k;
            let mut timestamp_90k = dda_relative_timestamp_90k(
                frame_info.LastPresentTime,
                qpc_frequency,
                start,
                &mut timestamp_origin_qpc,
                &mut last_timestamp_90k,
            );
            if let Some(previous) = previous_timestamp_90k {
                if timestamp_90k <= previous {
                    timestamp_90k = previous.saturating_add(1);
                    last_timestamp_90k = Some(timestamp_90k);
                }
                stats
                    .observe_source_interval(capture_index, timestamp_90k.saturating_sub(previous));
            }
            reached_source_end = timestamp_90k >= source_stop_90k;
            let captured = CapturedSnapshot {
                slot: CaptureFrameSlot::Shared(slot),
                source_desc: snapshot_desc,
                move_rect_bytes: frame_metadata.move_rect_bytes,
                dirty_rects: frame_metadata.dirty_rects,
                timestamp_90k,
                timestamp_100ns: qpc_counter_to_100ns(frame_info.LastPresentTime, qpc_frequency),
                capture_index,
                accumulated_frames: frame_info.AccumulatedFrames,
                warmup: false,
            };
            capture_index += 1;

            if frame_tx.send(CaptureMsg::Frame(captured)).is_err() {
                stop.store(true, Ordering::Relaxed);
            }
            Ok(())
        })();

        duplication
            .ReleaseFrame()
            .map_err(|err| format!("IDXGIOutputDuplication::ReleaseFrame(capture): {err}"))?;
        frame_result?;
        if reached_source_end {
            break;
        }
    }

    Ok(stats)
}

#[cfg(windows)]
#[allow(clippy::too_many_arguments)]
fn spawn_wgc_capture_thread(
    adapter1: windows::Win32::Graphics::Dxgi::IDXGIAdapter1,
    encoder_device: windows::Win32::Graphics::Direct3D11::ID3D11Device,
    start: std::time::Instant,
    end_at: std::time::Instant,
    source_stop_90k: u64,
    route: VplRecordRoute,
    target_width: u32,
    target_height: u32,
    pool_size: usize,
    stop: std::sync::Arc<std::sync::atomic::AtomicBool>,
    frame_tx: std::sync::mpsc::Sender<CaptureMsg>,
    free_rx: std::sync::mpsc::Receiver<CaptureFrameSlot>,
) -> std::thread::JoinHandle<()> {
    std::thread::spawn(move || {
        let result = unsafe {
            run_wgc_capture_thread(
                adapter1,
                encoder_device,
                start,
                end_at,
                source_stop_90k,
                route,
                target_width,
                target_height,
                pool_size,
                stop,
                frame_tx.clone(),
                free_rx,
            )
        };
        match result {
            Ok(stats) => {
                let _ = frame_tx.send(CaptureMsg::Done(stats));
            }
            Err(message) => {
                let _ = frame_tx.send(CaptureMsg::Error(message));
            }
        }
        std::thread::sleep(std::time::Duration::from_millis(100));
    })
}

#[cfg(windows)]
#[allow(clippy::too_many_arguments)]
unsafe fn run_wgc_capture_thread(
    adapter1: windows::Win32::Graphics::Dxgi::IDXGIAdapter1,
    encoder_device: windows::Win32::Graphics::Direct3D11::ID3D11Device,
    start: std::time::Instant,
    end_at: std::time::Instant,
    source_stop_90k: u64,
    route: VplRecordRoute,
    target_width: u32,
    target_height: u32,
    pool_size: usize,
    stop: std::sync::Arc<std::sync::atomic::AtomicBool>,
    frame_tx: std::sync::mpsc::Sender<CaptureMsg>,
    free_rx: std::sync::mpsc::Receiver<CaptureFrameSlot>,
) -> Result<CaptureStats, String> {
    use std::collections::VecDeque;
    use std::sync::atomic::{AtomicUsize, Ordering};
    use std::sync::{Arc, Mutex};
    use windows::Foundation::{TimeSpan, TypedEventHandler};
    use windows::Graphics::Capture::{
        Direct3D11CaptureFrame, Direct3D11CaptureFramePool, GraphicsCaptureItem,
        GraphicsCaptureSession,
    };
    use windows::Graphics::DirectX::Direct3D11::IDirect3DDevice;
    use windows::Win32::Foundation::RPC_E_CHANGED_MODE;
    use windows::Win32::Graphics::Direct3D11::{
        D3D11_TEXTURE2D_DESC, ID3D11Multithread, ID3D11Texture2D,
    };
    use windows::Win32::Graphics::Dxgi::Common::DXGI_SAMPLE_DESC;
    use windows::Win32::Graphics::Dxgi::IDXGIDevice;
    use windows::Win32::System::WinRT::Direct3D11::{
        CreateDirect3D11DeviceFromDXGIDevice, IDirect3DDxgiInterfaceAccess,
    };
    use windows::Win32::System::WinRT::Graphics::Capture::IGraphicsCaptureItemInterop;
    use windows::Win32::System::WinRT::{RO_INIT_MULTITHREADED, RoInitialize, RoUninitialize};
    use windows::core::{IInspectable, Interface};

    struct RoGuard(bool);
    impl Drop for RoGuard {
        fn drop(&mut self) {
            if self.0 {
                unsafe {
                    RoUninitialize();
                }
            }
        }
    }

    struct WgcCaptureState {
        stats: CaptureStats,
        free_slots: VecDeque<WgcLocalSlot>,
        source_desc: Option<D3D11_TEXTURE2D_DESC>,
        capture_index: u64,
        timestamp_origin_100ns: Option<i64>,
        warmup_last_timestamp_100ns: Option<i64>,
        warmup_stable_intervals: u32,
        warmup_done: bool,
        encoder_warmup_pending: bool,
        post_warmup_discard_remaining: u32,
        pipeline_warmup_remaining: u32,
        pipeline_warmup_stable_intervals: u32,
        last_timestamp_100ns: Option<i64>,
        last_timestamp_90k: Option<u64>,
        record_wall_deadline: Option<std::time::Instant>,
        error: Option<String>,
    }

    impl WgcCaptureState {
        fn new() -> Self {
            Self {
                stats: CaptureStats::new(),
                free_slots: VecDeque::new(),
                source_desc: None,
                capture_index: 0,
                timestamp_origin_100ns: None,
                warmup_last_timestamp_100ns: None,
                warmup_stable_intervals: 0,
                warmup_done: false,
                encoder_warmup_pending: false,
                post_warmup_discard_remaining: 0,
                pipeline_warmup_remaining: 0,
                pipeline_warmup_stable_intervals: 0,
                last_timestamp_100ns: None,
                last_timestamp_90k: None,
                record_wall_deadline: None,
                error: None,
            }
        }
    }

    struct WgcFrameCloseGuard(Direct3D11CaptureFrame);
    impl Drop for WgcFrameCloseGuard {
        fn drop(&mut self) {
            let _ = self.0.Close();
        }
    }

    struct WgcQueuedFrame {
        frame: Direct3D11CaptureFrame,
        enqueued_at: std::time::Instant,
    }

    fn update_atomic_max(target: &AtomicUsize, value: usize) {
        let mut current = target.load(Ordering::Relaxed);
        while value > current {
            match target.compare_exchange_weak(current, value, Ordering::Relaxed, Ordering::Relaxed)
            {
                Ok(_) => break,
                Err(next) => current = next,
            }
        }
    }

    fn win_err(label: &str, err: windows::core::Error) -> String {
        format!("{label}: {err}")
    }

    const WGC_WARMUP_STABLE_INTERVALS: u32 = 0;

    let capture_duration = end_at.saturating_duration_since(start);
    let ro_guard = match RoInitialize(RO_INIT_MULTITHREADED) {
        Ok(()) => RoGuard(true),
        Err(err) if err.code() == RPC_E_CHANGED_MODE => RoGuard(false),
        Err(err) => return Err(win_err("RoInitialize(WGC record)", err)),
    };
    if !GraphicsCaptureSession::IsSupported()
        .map_err(|err| win_err("GraphicsCaptureSession::IsSupported(WGC record)", err))?
    {
        return Err("GraphicsCaptureSession::IsSupported 返回 false".to_owned());
    }

    let device = encoder_device.clone();
    let context = device.GetImmediateContext().map_err(|err| {
        win_err(
            "oneVPL native ID3D11Device::GetImmediateContext(WGC record)",
            err,
        )
    })?;
    let wgc_multithread: Option<ID3D11Multithread> = context.cast().ok();
    if let Some(mt) = &wgc_multithread {
        let _ = mt.SetMultithreadProtected(true);
    }
    let output = adapter1
        .EnumOutputs(0)
        .map_err(|err| win_err("IDXGIAdapter1::EnumOutputs(WGC record)", err))?;
    let output_desc = output
        .GetDesc()
        .map_err(|err| win_err("IDXGIOutput::GetDesc(WGC record)", err))?;
    let dxgi_device: IDXGIDevice = device
        .cast()
        .map_err(|err| win_err("ID3D11Device::cast<IDXGIDevice>(WGC record)", err))?;
    let inspectable = CreateDirect3D11DeviceFromDXGIDevice(&dxgi_device)
        .map_err(|err| win_err("CreateDirect3D11DeviceFromDXGIDevice(WGC record)", err))?;
    let winrt_device: IDirect3DDevice = inspectable
        .cast()
        .map_err(|err| win_err("IInspectable::cast<IDirect3DDevice>(WGC record)", err))?;
    let item_interop: IGraphicsCaptureItemInterop =
        windows::core::factory::<GraphicsCaptureItem, IGraphicsCaptureItemInterop>().map_err(
            |err| {
                win_err(
                    "factory<GraphicsCaptureItem, IGraphicsCaptureItemInterop>(WGC record)",
                    err,
                )
            },
        )?;
    let item: GraphicsCaptureItem =
        item_interop
            .CreateForMonitor(output_desc.Monitor)
            .map_err(|err| {
                win_err(
                    "IGraphicsCaptureItemInterop::CreateForMonitor(WGC record)",
                    err,
                )
            })?;
    let item_size = item
        .Size()
        .map_err(|err| win_err("GraphicsCaptureItem::Size(WGC record)", err))?;
    let (pixel_format, source_dxgi_format) = route.wgc_input_format();
    // 固定 WGC 路线：按后端选择的 SDR/HDR route 捕获为 BGRA8/FP16，
    // 捕获线程 GPU shader 写目标 FourCC surface；编码线程仅 CopyResource 到 oneVPL surface。
    let wgc_frame_pool_size = 4;
    let post_warmup_discard_frames = 24;
    let pipeline_warmup_frames = 48;
    let pipeline_warmup_stable_intervals_required = 0;
    let frame_pool = Direct3D11CaptureFramePool::CreateFreeThreaded(
        &winrt_device,
        pixel_format,
        wgc_frame_pool_size,
        item_size,
    )
    .map_err(|err| {
        win_err(
            "Direct3D11CaptureFramePool::CreateFreeThreaded(WGC record)",
            err,
        )
    })?;
    let session = frame_pool.CreateCaptureSession(&item).map_err(|err| {
        win_err(
            "Direct3D11CaptureFramePool::CreateCaptureSession(WGC record)",
            err,
        )
    })?;
    let _ = session.SetIsBorderRequired(false);
    let _ = session.SetIsCursorCaptureEnabled(true);
    let _ = session.SetMinUpdateInterval(TimeSpan { Duration: 0 });

    let mut initial_state = WgcCaptureState::new();
    let initial_source_desc = D3D11_TEXTURE2D_DESC {
        Width: item_size.Width.max(1) as u32,
        Height: item_size.Height.max(1) as u32,
        MipLevels: 1,
        ArraySize: 1,
        Format: source_dxgi_format,
        SampleDesc: DXGI_SAMPLE_DESC {
            Count: 1,
            Quality: 0,
        },
        Usage: windows::Win32::Graphics::Direct3D11::D3D11_USAGE_DEFAULT,
        BindFlags: 0,
        CPUAccessFlags: 0,
        MiscFlags: 0,
    };
    let route_dxgi_format = route.try_dxgi_format().map_err(|err| err.to_string())?;
    let initial_snapshot_desc = D3D11_TEXTURE2D_DESC {
        Width: target_width.max(item_size.Width.max(1) as u32),
        Height: target_height.max(item_size.Height.max(1) as u32),
        Format: route_dxgi_format,
        ..initial_source_desc
    };
    let route_intermediate =
        create_route_intermediate(&device, &initial_snapshot_desc, route, true)
            .map_err(|err| err.to_string())?;
    let route_converter = GpuRecordConverter::new(
        route,
        &device,
        &context,
        &route_intermediate,
        initial_source_desc.Width,
        initial_source_desc.Height,
        true,
    )
    .map_err(|err| err.to_string())?;
    let capture_route_path = (route_intermediate, route_converter);
    initial_state.source_desc = Some(initial_snapshot_desc);
    for id in 0..pool_size {
        initial_state.free_slots.push_back(
            create_wgc_local_slot(id, &device, &initial_snapshot_desc)
                .map_err(|err| err.to_string())?,
        );
    }
    let callback_state = Arc::new(Mutex::new(initial_state));
    let (wgc_frame_tx, wgc_frame_rx) = std::sync::mpsc::channel::<WgcQueuedFrame>();
    let wgc_frame_queue_depth = Arc::new(AtomicUsize::new(0));
    let wgc_frame_queue_depth_max = Arc::new(AtomicUsize::new(0));
    let handler_stop = Arc::clone(&stop);
    let handler_state = Arc::clone(&callback_state);
    let handler_wgc_frame_tx = wgc_frame_tx.clone();
    let handler_wgc_frame_queue_depth = Arc::clone(&wgc_frame_queue_depth);
    let handler_wgc_frame_queue_depth_max = Arc::clone(&wgc_frame_queue_depth_max);
    let handler =
        TypedEventHandler::<Direct3D11CaptureFramePool, IInspectable>::new(move |sender, _| {
            if handler_stop.load(Ordering::Relaxed) {
                return Ok(());
            }
            if let Some(pool) = sender.as_ref() {
                loop {
                    let frame = match pool.TryGetNextFrame() {
                        Ok(frame) => frame,
                        Err(err) if err.code().0 == 0 => break,
                        Err(err) => {
                            if let Ok(mut state) = handler_state.lock() {
                                state.error = Some(win_err("TryGetNextFrame(WGC callback)", err));
                            }
                            handler_stop.store(true, Ordering::Relaxed);
                            break;
                        }
                    };

                    let queued = WgcQueuedFrame {
                        frame,
                        enqueued_at: std::time::Instant::now(),
                    };
                    if handler_wgc_frame_tx.send(queued).is_err() {
                        handler_stop.store(true, Ordering::Relaxed);
                        break;
                    }
                    let depth = handler_wgc_frame_queue_depth.fetch_add(1, Ordering::Relaxed) + 1;
                    update_atomic_max(&handler_wgc_frame_queue_depth_max, depth);
                    if handler_stop.load(Ordering::Relaxed) {
                        break;
                    }
                }
            } else if let Ok(mut state) = handler_state.lock() {
                state.error = Some("FrameArrived(WGC callback) sender 为空".to_owned());
                handler_stop.store(true, Ordering::Relaxed);
            }
            Ok(())
        });

    let token = frame_pool
        .FrameArrived(&handler)
        .map_err(|err| win_err("Direct3D11CaptureFramePool::FrameArrived(WGC record)", err))?;
    session
        .StartCapture()
        .map_err(|err| win_err("GraphicsCaptureSession::StartCapture(WGC record)", err))?;

    let process_wgc_frame = |queued: WgcQueuedFrame| -> Result<(), String> {
        let queue_delay = queued.enqueued_at.elapsed();
        let frame = queued.frame;
        let _close_guard = WgcFrameCloseGuard(frame.clone());
        let callback_frame_started = std::time::Instant::now();
        let timestamp_100ns = frame
            .SystemRelativeTime()
            .map_err(|err| {
                win_err(
                    "Direct3D11CaptureFrame::SystemRelativeTime(WGC capture thread)",
                    err,
                )
            })?
            .Duration;
        let mut state = callback_state
            .lock()
            .map_err(|_| "WGC callback state mutex poisoned".to_owned())?;
        if state.error.is_some() {
            return Ok(());
        }
        state.stats.acquired += 1;
        state.stats.observe_wgc_frame_queue_delay(queue_delay);
        if timestamp_100ns <= 0
            || state
                .last_timestamp_100ns
                .is_some_and(|last| timestamp_100ns <= last)
        {
            state.stats.dropped_duplicate_timestamp += 1;
            return Ok(());
        }

        let mut drop_warmup_frame = false;
        let mut encoder_warmup_frame = false;
        if state.post_warmup_discard_remaining > 0 {
            state.post_warmup_discard_remaining =
                state.post_warmup_discard_remaining.saturating_sub(1);
            state.stats.dropped_warmup += 1;
            state.last_timestamp_100ns = Some(timestamp_100ns);
            drop_warmup_frame = true;
            if state.post_warmup_discard_remaining == 0 {
                state.warmup_done = true;
                state.pipeline_warmup_remaining = pipeline_warmup_frames;
                state.pipeline_warmup_stable_intervals = 0;
                state.last_timestamp_100ns = None;
                state.timestamp_origin_100ns = None;
                state.last_timestamp_90k = None;
                state.record_wall_deadline = None;
            }
        }
        if !drop_warmup_frame && !encoder_warmup_frame && !state.warmup_done {
            if state.warmup_last_timestamp_100ns.is_some() {
                state.warmup_stable_intervals = state.warmup_stable_intervals.saturating_add(1);
            }
            state.warmup_last_timestamp_100ns = Some(timestamp_100ns);
            state.stats.dropped_warmup += 1;
            state.last_timestamp_100ns = Some(timestamp_100ns);
            drop_warmup_frame = true;
            if state.warmup_stable_intervals >= WGC_WARMUP_STABLE_INTERVALS
                && !state.encoder_warmup_pending
            {
                encoder_warmup_frame = true;
                drop_warmup_frame = false;
                state.encoder_warmup_pending = true;
            }
        }
        if drop_warmup_frame {
            return Ok(());
        }
        let pipeline_warmup_frame = state.warmup_done
            && (state.pipeline_warmup_remaining > 0
                || state.pipeline_warmup_stable_intervals
                    < pipeline_warmup_stable_intervals_required);

        let surface = frame
            .Surface()
            .map_err(|err| win_err("Direct3D11CaptureFrame::Surface(WGC capture thread)", err))?;
        let access: IDirect3DDxgiInterfaceAccess = surface
            .cast()
            .map_err(|err| win_err("IDirect3DSurface::cast<IDirect3DDxgiInterfaceAccess>", err))?;
        let source: ID3D11Texture2D = access.GetInterface().map_err(|err| {
            win_err(
                "IDirect3DDxgiInterfaceAccess::GetInterface<ID3D11Texture2D>",
                err,
            )
        })?;
        let mut source_desc = D3D11_TEXTURE2D_DESC::default();
        source.GetDesc(&mut source_desc);
        let snapshot_desc = initial_snapshot_desc;

        if source_desc.Width != initial_source_desc.Width
            || source_desc.Height != initial_source_desc.Height
            || source_desc.Format.0 != initial_source_desc.Format.0
        {
            return Err(format!(
                "不支持的桌面模式: WGC input desc changed during callback capture: {}x{} fmt {} -> {}x{} fmt {}; route snapshot stays {}x{} fmt {}",
                initial_source_desc.Width,
                initial_source_desc.Height,
                initial_source_desc.Format.0,
                source_desc.Width,
                source_desc.Height,
                source_desc.Format.0,
                snapshot_desc.Width,
                snapshot_desc.Height,
                snapshot_desc.Format.0,
            ));
        }
        if state.source_desc.is_none() {
            state.source_desc = Some(snapshot_desc);
        }

        let Some(slot) = state.free_slots.pop_front() else {
            state.stats.dropped_no_slot += 1;
            return Ok(());
        };
        let _guard = D3d11MultithreadGuard::enter(&wgc_multithread);
        let copy_started = std::time::Instant::now();
        let (route_intermediate, route_converter) = &capture_route_path;
        route_converter
            .convert(&source)
            .and_then(|()| {
                copy_texture_resource(&context, route_intermediate, &slot.texture)
            })
            .map_err(|err| {
                format!(
                    "WGC capture route conversion failed: input_tex_format={} route_tex_format={} target={}x{}; {err}",
                    initial_source_desc.Format.0,
                    initial_snapshot_desc.Format.0,
                    initial_snapshot_desc.Width,
                    initial_snapshot_desc.Height
                )
            })?;
        let copy_duration = copy_started.elapsed();
        state.stats.copied += 1;
        let callback_frame_duration = callback_frame_started.elapsed();
        state
            .stats
            .observe_callback_frame_cpu(callback_frame_duration, copy_duration);

        if encoder_warmup_frame || pipeline_warmup_frame {
            if pipeline_warmup_frame {
                if state.last_timestamp_100ns.is_some() {
                    state.pipeline_warmup_stable_intervals =
                        state.pipeline_warmup_stable_intervals.saturating_add(1);
                }
                state.pipeline_warmup_remaining = state.pipeline_warmup_remaining.saturating_sub(1);
                state.last_timestamp_100ns = Some(timestamp_100ns);
                if state.pipeline_warmup_remaining == 0
                    && state.pipeline_warmup_stable_intervals
                        >= pipeline_warmup_stable_intervals_required
                {
                    state.last_timestamp_100ns = None;
                    state.timestamp_origin_100ns = None;
                    state.last_timestamp_90k = None;
                    state.record_wall_deadline = None;
                }
            }
            let captured = CapturedSnapshot {
                slot: CaptureFrameSlot::WgcLocal(slot),
                source_desc: snapshot_desc,
                move_rect_bytes: 0,
                dirty_rects: Vec::new(),
                timestamp_90k: 0,
                timestamp_100ns: Some(timestamp_100ns),
                capture_index: state.capture_index,
                accumulated_frames: 1,
                warmup: true,
            };
            if frame_tx.send(CaptureMsg::Frame(captured)).is_err() {
                stop.store(true, Ordering::Relaxed);
            }
            return Ok(());
        }

        state.last_timestamp_100ns = Some(timestamp_100ns);
        let previous_timestamp_90k = state.last_timestamp_90k;
        let mut timestamp_origin_100ns = state.timestamp_origin_100ns;
        let mut last_timestamp_90k = state.last_timestamp_90k;
        let timestamp_90k = wgc_relative_timestamp_90k(
            timestamp_100ns,
            &mut timestamp_origin_100ns,
            &mut last_timestamp_90k,
        );
        state.timestamp_origin_100ns = timestamp_origin_100ns;
        state.last_timestamp_90k = last_timestamp_90k;
        let capture_index = state.capture_index;
        if state.record_wall_deadline.is_none() {
            state.record_wall_deadline = Some(
                std::time::Instant::now() + capture_duration + std::time::Duration::from_secs(2),
            );
        }
        if let Some(previous) = previous_timestamp_90k {
            state
                .stats
                .observe_source_interval(capture_index, timestamp_90k.saturating_sub(previous));
        }
        let captured = CapturedSnapshot {
            slot: CaptureFrameSlot::WgcLocal(slot),
            source_desc: snapshot_desc,
            move_rect_bytes: 0,
            dirty_rects: Vec::new(),
            timestamp_90k,
            timestamp_100ns: Some(timestamp_100ns),
            capture_index,
            accumulated_frames: 1,
            warmup: false,
        };
        state.capture_index = state.capture_index.saturating_add(1);
        if frame_tx.send(CaptureMsg::Frame(captured)).is_err() {
            stop.store(true, Ordering::Relaxed);
        }
        if timestamp_90k >= source_stop_90k {
            stop.store(true, Ordering::Relaxed);
        }
        Ok(())
    };
    let return_slot = |slot: CaptureFrameSlot| -> Result<(), String> {
        let CaptureFrameSlot::WgcLocal(slot) = slot else {
            return Ok(());
        };
        let mut state = callback_state
            .lock()
            .map_err(|_| "WGC callback state mutex poisoned while returning slot".to_owned())?;
        if state.encoder_warmup_pending {
            state.encoder_warmup_pending = false;
            state.post_warmup_discard_remaining = post_warmup_discard_frames;
            if state.post_warmup_discard_remaining == 0 {
                state.warmup_done = true;
                state.pipeline_warmup_remaining = pipeline_warmup_frames;
                state.pipeline_warmup_stable_intervals = 0;
            }
            state.last_timestamp_100ns = None;
            state.timestamp_origin_100ns = None;
            state.last_timestamp_90k = None;
            state.record_wall_deadline = None;
        }
        if state
            .source_desc
            .as_ref()
            .is_none_or(|desc| wgc_local_slot_matches(&slot, desc))
        {
            state.free_slots.push_back(slot);
        }
        Ok(())
    };

    let startup_deadline =
        std::time::Instant::now() + capture_duration + std::time::Duration::from_secs(10);
    while !stop.load(Ordering::Relaxed) {
        while let Ok(slot) = free_rx.try_recv() {
            return_slot(slot)?;
        }
        while let Ok(frame) = wgc_frame_rx.try_recv() {
            wgc_frame_queue_depth.fetch_sub(1, Ordering::Relaxed);
            let frame_result = process_wgc_frame(frame);
            if let Err(message) = frame_result {
                if let Ok(mut state) = callback_state.lock() {
                    state.error = Some(message);
                }
                stop.store(true, Ordering::Relaxed);
                break;
            }
            if stop.load(Ordering::Relaxed) {
                break;
            }
        }
        let now = std::time::Instant::now();
        let deadline_reached = {
            let state = callback_state.lock().map_err(|_| {
                "WGC callback state mutex poisoned while checking deadline".to_owned()
            })?;
            state
                .record_wall_deadline
                .map(|deadline| now >= deadline)
                .unwrap_or(now >= startup_deadline)
        };
        if deadline_reached {
            break;
        }
        match wgc_frame_rx.recv_timeout(std::time::Duration::from_millis(1)) {
            Ok(frame) => {
                wgc_frame_queue_depth.fetch_sub(1, Ordering::Relaxed);
                let frame_result = process_wgc_frame(frame);
                if let Err(message) = frame_result {
                    if let Ok(mut state) = callback_state.lock() {
                        state.error = Some(message);
                    }
                    stop.store(true, Ordering::Relaxed);
                    break;
                }
            }
            Err(std::sync::mpsc::RecvTimeoutError::Timeout) => {}
            Err(std::sync::mpsc::RecvTimeoutError::Disconnected) => break,
        }
        while let Ok(slot) = free_rx.try_recv() {
            return_slot(slot)?;
        }
        if callback_state
            .lock()
            .map_err(|_| "WGC callback state mutex poisoned while checking error".to_owned())?
            .error
            .is_some()
        {
            stop.store(true, Ordering::Relaxed);
            break;
        }
    }

    let drain_deadline = std::time::Instant::now() + std::time::Duration::from_secs(3);
    loop {
        while let Ok(slot) = free_rx.try_recv() {
            return_slot(slot)?;
        }
        let free_len = callback_state
            .lock()
            .map_err(|_| "WGC callback state mutex poisoned while draining".to_owned())?
            .free_slots
            .len();
        if free_len >= pool_size || std::time::Instant::now() >= drain_deadline {
            break;
        }
        match free_rx.recv_timeout(std::time::Duration::from_millis(1)) {
            Ok(slot) => return_slot(slot)?,
            Err(std::sync::mpsc::RecvTimeoutError::Timeout) => {}
            Err(std::sync::mpsc::RecvTimeoutError::Disconnected) => break,
        }
    }
    let _ = frame_pool.RemoveFrameArrived(token);
    let _ = session.Close();
    let _ = frame_pool.Close();
    drop(handler);
    std::mem::forget(ro_guard);

    let (error, mut stats) = {
        let state = callback_state
            .lock()
            .map_err(|_| "WGC callback state mutex poisoned while finalizing".to_owned())?;
        (state.error.clone(), state.stats.clone())
    };
    stats.wgc_input_queue_max = wgc_frame_queue_depth_max.load(Ordering::Relaxed) as u64;
    // Some WGC/WinRT wrappers have shown access violations when released
    // immediately on the capture thread after a high-rate recording. Keep only
    // the WinRT capture graph alive; snapshot slots and shared handles still drop.
    std::mem::forget(session);
    std::mem::forget(frame_pool);
    std::mem::forget(item);
    std::mem::forget(item_interop);
    std::mem::forget(winrt_device);
    std::mem::forget(inspectable);
    if let Some(error) = error {
        return Err(error);
    }
    stats.dda_timeouts = 0;
    Ok(stats)
}

#[cfg(windows)]
struct D3d11MultithreadGuard<'a> {
    mt: Option<&'a windows::Win32::Graphics::Direct3D11::ID3D11Multithread>,
}

#[cfg(windows)]
impl<'a> D3d11MultithreadGuard<'a> {
    unsafe fn enter(
        mt: &'a Option<windows::Win32::Graphics::Direct3D11::ID3D11Multithread>,
    ) -> Self {
        if let Some(mt) = mt {
            mt.Enter();
            Self { mt: Some(mt) }
        } else {
            Self { mt: None }
        }
    }
}

#[cfg(windows)]
impl Drop for D3d11MultithreadGuard<'_> {
    fn drop(&mut self) {
        unsafe {
            if let Some(mt) = self.mt {
                mt.Leave();
            }
        }
    }
}

#[cfg(windows)]
unsafe fn copy_texture_resource(
    context: &windows::Win32::Graphics::Direct3D11::ID3D11DeviceContext,
    source: &windows::Win32::Graphics::Direct3D11::ID3D11Texture2D,
    target: &windows::Win32::Graphics::Direct3D11::ID3D11Texture2D,
) -> Result<(), BackendError> {
    use windows::Win32::Graphics::Direct3D11::ID3D11Resource;
    use windows::core::Interface;

    let src_resource: ID3D11Resource = source.cast().map_err(|err| BackendError::WindowsApi {
        func: "ID3D11Texture2D::cast<ID3D11Resource>(copy source)",
        message: err.to_string(),
    })?;
    let dst_resource: ID3D11Resource = target.cast().map_err(|err| BackendError::WindowsApi {
        func: "ID3D11Texture2D::cast<ID3D11Resource>(copy target)",
        message: err.to_string(),
    })?;
    context.CopyResource(&dst_resource, &src_resource);
    Ok(())
}

#[cfg(windows)]
unsafe fn copy_texture_subresource_region(
    context: &windows::Win32::Graphics::Direct3D11::ID3D11DeviceContext,
    source: &windows::Win32::Graphics::Direct3D11::ID3D11Texture2D,
    target: &windows::Win32::Graphics::Direct3D11::ID3D11Texture2D,
    width: u32,
    height: u32,
) -> Result<(), BackendError> {
    use windows::Win32::Graphics::Direct3D11::{D3D11_BOX, ID3D11Resource};
    use windows::core::Interface;

    let src_resource: ID3D11Resource = source.cast().map_err(|err| BackendError::WindowsApi {
        func: "ID3D11Texture2D::cast<ID3D11Resource>(region copy source)",
        message: err.to_string(),
    })?;
    let dst_resource: ID3D11Resource = target.cast().map_err(|err| BackendError::WindowsApi {
        func: "ID3D11Texture2D::cast<ID3D11Resource>(region copy target)",
        message: err.to_string(),
    })?;
    let src_box = D3D11_BOX {
        left: 0,
        top: 0,
        front: 0,
        right: width.max(1),
        bottom: height.max(1),
        back: 1,
    };
    context.CopySubresourceRegion(&dst_resource, 0, 0, 0, 0, &src_resource, 0, Some(&src_box));
    Ok(())
}

#[cfg(windows)]
unsafe fn create_shader_resource_view_with_gpu_copy_fallback(
    device: &windows::Win32::Graphics::Direct3D11::ID3D11Device,
    context: &windows::Win32::Graphics::Direct3D11::ID3D11DeviceContext,
    source: &windows::Win32::Graphics::Direct3D11::ID3D11Texture2D,
    label: &'static str,
) -> Result<windows::Win32::Graphics::Direct3D11::ID3D11ShaderResourceView, BackendError> {
    use windows::Win32::Graphics::Direct3D::D3D11_SRV_DIMENSION_TEXTURE2D;
    use windows::Win32::Graphics::Direct3D11::{
        D3D11_BIND_SHADER_RESOURCE, D3D11_SHADER_RESOURCE_VIEW_DESC,
        D3D11_SHADER_RESOURCE_VIEW_DESC_0, D3D11_TEX2D_SRV, D3D11_TEXTURE2D_DESC,
        D3D11_USAGE_DEFAULT, ID3D11Resource,
    };
    use windows::core::Interface;

    let source_resource: ID3D11Resource =
        source.cast().map_err(|err| BackendError::WindowsApi {
            func: "ID3D11Texture2D::cast<ID3D11Resource>(SRV source)",
            message: err.to_string(),
        })?;
    let mut srv = None;
    if device
        .CreateShaderResourceView(&source_resource, None, Some(&mut srv))
        .is_ok()
    {
        return srv.ok_or_else(|| BackendError::WindowsApi {
            func: label,
            message: "CreateShaderResourceView 返回空 SRV".to_owned(),
        });
    }

    let mut desc = D3D11_TEXTURE2D_DESC::default();
    source.GetDesc(&mut desc);
    let explicit_srv_desc = D3D11_SHADER_RESOURCE_VIEW_DESC {
        Format: desc.Format,
        ViewDimension: D3D11_SRV_DIMENSION_TEXTURE2D,
        Anonymous: D3D11_SHADER_RESOURCE_VIEW_DESC_0 {
            Texture2D: D3D11_TEX2D_SRV {
                MostDetailedMip: 0,
                MipLevels: 1,
            },
        },
    };
    let mut explicit_srv = None;
    if device
        .CreateShaderResourceView(
            &source_resource,
            Some(&explicit_srv_desc),
            Some(&mut explicit_srv),
        )
        .is_ok()
    {
        return explicit_srv.ok_or_else(|| BackendError::WindowsApi {
            func: label,
            message: format!(
                "显式 SRV desc 返回空 SRV，format={}, bind_flags=0x{:X}",
                desc.Format.0, desc.BindFlags
            ),
        });
    }

    let copy_desc = D3D11_TEXTURE2D_DESC {
        Width: desc.Width.max(1),
        Height: desc.Height.max(1),
        MipLevels: 1,
        ArraySize: 1,
        Format: desc.Format,
        SampleDesc: desc.SampleDesc,
        Usage: D3D11_USAGE_DEFAULT,
        BindFlags: D3D11_BIND_SHADER_RESOURCE.0 as u32,
        CPUAccessFlags: 0,
        MiscFlags: 0,
    };
    let mut shader_readable = None;
    device
        .CreateTexture2D(&copy_desc, None, Some(&mut shader_readable))
        .map_err(|err| BackendError::WindowsApi {
            func: "ID3D11Device::CreateTexture2D(SRV fallback copy)",
            message: err.to_string(),
        })?;
    let shader_readable = shader_readable.ok_or_else(|| BackendError::WindowsApi {
        func: "CreateTexture2D(SRV fallback copy)",
        message: "返回空纹理".to_owned(),
    })?;
    copy_texture_resource(context, source, &shader_readable)?;
    let shader_resource: ID3D11Resource =
        shader_readable
            .cast()
            .map_err(|err| BackendError::WindowsApi {
                func: "ID3D11Texture2D::cast<ID3D11Resource>(SRV fallback copy)",
                message: err.to_string(),
            })?;
    let mut fallback_srv = None;
    let fallback_srv_desc = D3D11_SHADER_RESOURCE_VIEW_DESC {
        Format: copy_desc.Format,
        ViewDimension: D3D11_SRV_DIMENSION_TEXTURE2D,
        Anonymous: D3D11_SHADER_RESOURCE_VIEW_DESC_0 {
            Texture2D: D3D11_TEX2D_SRV {
                MostDetailedMip: 0,
                MipLevels: 1,
            },
        },
    };
    device
        .CreateShaderResourceView(&shader_resource, Some(&fallback_srv_desc), Some(&mut fallback_srv))
        .map_err(|err| BackendError::WindowsApi {
            func: label,
            message: format!(
                "CreateShaderResourceView 直接/显式/一次 GPU copy fallback 均失败: {err}; source_format={}, source_bind=0x{:X}, copy_format={}",
                desc.Format.0, desc.BindFlags, copy_desc.Format.0
            ),
        })?;
    fallback_srv.ok_or_else(|| BackendError::WindowsApi {
        func: label,
        message: "fallback 返回空 SRV".to_owned(),
    })
}

#[cfg(windows)]
unsafe fn set_d3d11_gpu_thread_priority(
    device: &windows::Win32::Graphics::Direct3D11::ID3D11Device,
    priority: i32,
) -> Result<(), BackendError> {
    use windows::Win32::Graphics::Dxgi::IDXGIDevice;
    use windows::core::Interface;

    let dxgi_device: IDXGIDevice = device.cast().map_err(|err| BackendError::WindowsApi {
        func: "ID3D11Device::cast<IDXGIDevice>(GPU priority)",
        message: err.to_string(),
    })?;
    dxgi_device
        .SetGPUThreadPriority(priority)
        .map_err(|err| BackendError::WindowsApi {
            func: "IDXGIDevice::SetGPUThreadPriority",
            message: err.to_string(),
        })
}

#[cfg(windows)]
struct DdaFrameMetadata {
    move_rect_bytes: u32,
    dirty_rects: Vec<windows::Win32::Foundation::RECT>,
}

#[cfg(windows)]
unsafe fn read_dda_frame_metadata(
    duplication: &windows::Win32::Graphics::Dxgi::IDXGIOutputDuplication,
    total_metadata_bytes: u32,
) -> Result<DdaFrameMetadata, BackendError> {
    use windows::Win32::Graphics::Dxgi::DXGI_OUTDUPL_MOVE_RECT;

    if total_metadata_bytes == 0 {
        return Ok(DdaFrameMetadata {
            move_rect_bytes: 0,
            dirty_rects: Vec::new(),
        });
    }

    let move_capacity =
        (total_metadata_bytes as usize / std::mem::size_of::<DXGI_OUTDUPL_MOVE_RECT>()).max(1);
    let mut move_rects = vec![DXGI_OUTDUPL_MOVE_RECT::default(); move_capacity];
    let mut move_rect_bytes = 0u32;
    duplication
        .GetFrameMoveRects(
            total_metadata_bytes,
            move_rects.as_mut_ptr(),
            &mut move_rect_bytes,
        )
        .map_err(|err| BackendError::WindowsApi {
            func: "IDXGIOutputDuplication::GetFrameMoveRects",
            message: err.to_string(),
        })?;

    let dirty_capacity = (total_metadata_bytes as usize
        / std::mem::size_of::<windows::Win32::Foundation::RECT>())
    .max(1);
    let mut dirty_rects = vec![windows::Win32::Foundation::RECT::default(); dirty_capacity];
    let mut dirty_rect_bytes = 0u32;
    duplication
        .GetFrameDirtyRects(
            total_metadata_bytes,
            dirty_rects.as_mut_ptr(),
            &mut dirty_rect_bytes,
        )
        .map_err(|err| BackendError::WindowsApi {
            func: "IDXGIOutputDuplication::GetFrameDirtyRects",
            message: err.to_string(),
        })?;
    let dirty_len = (dirty_rect_bytes as usize
        / std::mem::size_of::<windows::Win32::Foundation::RECT>())
    .min(dirty_rects.len());
    dirty_rects.truncate(dirty_len);

    Ok(DdaFrameMetadata {
        move_rect_bytes,
        dirty_rects,
    })
}

#[cfg(windows)]
fn dirty_rect_area(rects: &[windows::Win32::Foundation::RECT]) -> u64 {
    rects
        .iter()
        .map(|rect| {
            let width = (rect.right - rect.left).max(0) as u64;
            let height = (rect.bottom - rect.top).max(0) as u64;
            width * height
        })
        .sum()
}

#[cfg(windows)]
struct GpuCompletionFence {
    asynchronous: windows::Win32::Graphics::Direct3D11::ID3D11Asynchronous,
}

#[cfg(windows)]
impl GpuCompletionFence {
    unsafe fn new(
        device: &windows::Win32::Graphics::Direct3D11::ID3D11Device,
    ) -> Result<Self, BackendError> {
        use windows::Win32::Graphics::Direct3D11::{
            D3D11_QUERY_DESC, D3D11_QUERY_EVENT, ID3D11Asynchronous, ID3D11Query,
        };
        use windows::core::Interface;

        let desc = D3D11_QUERY_DESC {
            Query: D3D11_QUERY_EVENT,
            MiscFlags: 0,
        };
        let mut query: Option<ID3D11Query> = None;
        device
            .CreateQuery(&desc, Some(&mut query))
            .map_err(|err| BackendError::WindowsApi {
                func: "ID3D11Device::CreateQuery(DDA source fence)",
                message: err.to_string(),
            })?;
        let asynchronous: ID3D11Asynchronous = query
            .ok_or_else(|| BackendError::WindowsApi {
                func: "CreateQuery(DDA source fence)",
                message: "返回空 query".to_owned(),
            })?
            .cast()
            .map_err(|err| BackendError::WindowsApi {
                func: "ID3D11Query::cast<ID3D11Asynchronous>",
                message: err.to_string(),
            })?;
        Ok(Self { asynchronous })
    }

    unsafe fn mark(&self, context: &windows::Win32::Graphics::Direct3D11::ID3D11DeviceContext) {
        context.End(&self.asynchronous);
    }

    unsafe fn is_ready(
        &self,
        context: &windows::Win32::Graphics::Direct3D11::ID3D11DeviceContext,
    ) -> Result<bool, BackendError> {
        use windows::Win32::Foundation::{S_FALSE, S_OK};

        let hr = (windows::core::Interface::vtable(context).GetData)(
            windows::core::Interface::as_raw(context),
            windows::core::Interface::as_raw(&self.asynchronous),
            std::ptr::null_mut(),
            0,
            0,
        );
        if hr == S_OK {
            Ok(true)
        } else if hr == S_FALSE {
            Ok(false)
        } else {
            Err(BackendError::WindowsApi {
                func: "ID3D11DeviceContext::GetData(DDA source fence)",
                message: format!("HRESULT 0x{:08X}", hr.0 as u32),
            })
        }
    }

    unsafe fn wait_ready(
        &self,
        context: &windows::Win32::Graphics::Direct3D11::ID3D11DeviceContext,
    ) -> Result<(), BackendError> {
        let mut polls = 0u32;
        while !self.is_ready(context)? {
            polls = polls.wrapping_add(1);
            if polls.is_multiple_of(64) {
            } else {
                std::hint::spin_loop();
            }
        }
        Ok(())
    }

    unsafe fn wait(
        &self,
        context: &windows::Win32::Graphics::Direct3D11::ID3D11DeviceContext,
    ) -> Result<(), BackendError> {
        self.mark(context);
        self.wait_ready(context)
    }
}

#[cfg(windows)]
unsafe fn return_ready_snapshot_slots(
    pending: &mut Vec<CaptureFrameSlot>,
    context: &windows::Win32::Graphics::Direct3D11::ID3D11DeviceContext,
    free_tx: &std::sync::mpsc::Sender<CaptureFrameSlot>,
    wait_all: bool,
) -> Result<(), BackendError> {
    let mut index = 0usize;
    while index < pending.len() {
        let ready = match &pending[index] {
            CaptureFrameSlot::Shared(slot) => {
                if wait_all {
                    slot.encoder_fence.wait_ready(context)?;
                    true
                } else {
                    slot.encoder_fence.is_ready(context)?
                }
            }
            CaptureFrameSlot::WgcLocal(slot) => {
                if wait_all {
                    slot.fence.wait_ready(context)?;
                    true
                } else {
                    slot.fence.is_ready(context)?
                }
            }
        };
        if ready {
            let slot = pending.swap_remove(index);
            let _ = free_tx.send(slot);
        } else {
            index += 1;
        }
    }
    Ok(())
}

#[cfg(windows)]
struct GpuSnapshotConverter {
    device: windows::Win32::Graphics::Direct3D11::ID3D11Device,
    context: windows::Win32::Graphics::Direct3D11::ID3D11DeviceContext,
    output: windows::Win32::Graphics::Direct3D11::ID3D11Texture2D,
    render_target: windows::Win32::Graphics::Direct3D11::ID3D11RenderTargetView,
    vertex_shader: windows::Win32::Graphics::Direct3D11::ID3D11VertexShader,
    pixel_shader: windows::Win32::Graphics::Direct3D11::ID3D11PixelShader,
    full_viewport: windows::Win32::Graphics::Direct3D11::D3D11_VIEWPORT,
}

#[cfg(windows)]
impl GpuSnapshotConverter {
    unsafe fn new(
        device: &windows::Win32::Graphics::Direct3D11::ID3D11Device,
        context: &windows::Win32::Graphics::Direct3D11::ID3D11DeviceContext,
        source_desc: &windows::Win32::Graphics::Direct3D11::D3D11_TEXTURE2D_DESC,
    ) -> Result<Self, BackendError> {
        use windows::Win32::Graphics::Direct3D11::{
            D3D11_BIND_RENDER_TARGET, D3D11_BIND_SHADER_RESOURCE, D3D11_TEXTURE2D_DESC,
            D3D11_USAGE_DEFAULT,
        };

        let desc = D3D11_TEXTURE2D_DESC {
            Width: source_desc.Width.max(1),
            Height: source_desc.Height.max(1),
            MipLevels: 1,
            ArraySize: 1,
            Format: source_desc.Format,
            SampleDesc: source_desc.SampleDesc,
            Usage: D3D11_USAGE_DEFAULT,
            BindFlags: (D3D11_BIND_RENDER_TARGET.0 | D3D11_BIND_SHADER_RESOURCE.0) as u32,
            CPUAccessFlags: 0,
            MiscFlags: 0,
        };
        let mut output = None;
        device
            .CreateTexture2D(&desc, None, Some(&mut output))
            .map_err(|err| BackendError::WindowsApi {
                func: "ID3D11Device::CreateTexture2D(shader snapshot)",
                message: err.to_string(),
            })?;
        let output = output.ok_or_else(|| BackendError::WindowsApi {
            func: "CreateTexture2D(shader snapshot)",
            message: "返回空纹理".to_owned(),
        })?;
        let mut render_target = None;
        device
            .CreateRenderTargetView(&output, None, Some(&mut render_target))
            .map_err(|err| BackendError::WindowsApi {
                func: "ID3D11Device::CreateRenderTargetView(shader snapshot)",
                message: err.to_string(),
            })?;
        let render_target = render_target.ok_or_else(|| BackendError::WindowsApi {
            func: "CreateRenderTargetView(shader snapshot)",
            message: "返回空 RTV".to_owned(),
        })?;

        let vs_blob = compile_shader(SNAPSHOT_COPY_HLSL, b"vs_main\0", b"vs_5_0\0")?;
        let ps_blob = compile_shader(SNAPSHOT_COPY_HLSL, b"ps_main\0", b"ps_5_0\0")?;
        let mut vertex_shader = None;
        device
            .CreateVertexShader(&vs_blob, None, Some(&mut vertex_shader))
            .map_err(|err| BackendError::WindowsApi {
                func: "ID3D11Device::CreateVertexShader(shader snapshot)",
                message: err.to_string(),
            })?;
        let mut pixel_shader = None;
        device
            .CreatePixelShader(&ps_blob, None, Some(&mut pixel_shader))
            .map_err(|err| BackendError::WindowsApi {
                func: "ID3D11Device::CreatePixelShader(shader snapshot)",
                message: err.to_string(),
            })?;

        Ok(Self {
            device: device.clone(),
            context: context.clone(),
            output,
            render_target,
            vertex_shader: vertex_shader.ok_or_else(|| BackendError::WindowsApi {
                func: "CreateVertexShader(shader snapshot)",
                message: "返回空 VS".to_owned(),
            })?,
            pixel_shader: pixel_shader.ok_or_else(|| BackendError::WindowsApi {
                func: "CreatePixelShader(shader snapshot)",
                message: "返回空 PS".to_owned(),
            })?,
            full_viewport: windows::Win32::Graphics::Direct3D11::D3D11_VIEWPORT {
                TopLeftX: 0.0,
                TopLeftY: 0.0,
                Width: source_desc.Width as f32,
                Height: source_desc.Height as f32,
                MinDepth: 0.0,
                MaxDepth: 1.0,
            },
        })
    }

    unsafe fn copy_full(
        &self,
        source: &windows::Win32::Graphics::Direct3D11::ID3D11Texture2D,
    ) -> Result<(), BackendError> {
        self.copy_with_viewports(source, &[self.full_viewport])
    }

    unsafe fn copy_dirty(
        &self,
        source: &windows::Win32::Graphics::Direct3D11::ID3D11Texture2D,
        dirty_rects: &[windows::Win32::Foundation::RECT],
    ) -> Result<(), BackendError> {
        let viewports: Vec<_> = dirty_rects.iter().filter_map(snapshot_viewport).collect();
        if viewports.is_empty() {
            self.copy_full(source)
        } else {
            self.copy_with_viewports(source, &viewports)
        }
    }

    unsafe fn copy_with_viewports(
        &self,
        source: &windows::Win32::Graphics::Direct3D11::ID3D11Texture2D,
        viewports: &[windows::Win32::Graphics::Direct3D11::D3D11_VIEWPORT],
    ) -> Result<(), BackendError> {
        use windows::Win32::Graphics::Direct3D::D3D_PRIMITIVE_TOPOLOGY_TRIANGLELIST;
        use windows::Win32::Graphics::Direct3D11::{
            ID3D11RenderTargetView, ID3D11Resource, ID3D11ShaderResourceView,
        };
        use windows::core::Interface;

        let source_resource: ID3D11Resource =
            source.cast().map_err(|err| BackendError::WindowsApi {
                func: "ID3D11Texture2D::cast<ID3D11Resource>(shader snapshot source)",
                message: err.to_string(),
            })?;
        let mut srv = None;
        self.device
            .CreateShaderResourceView(&source_resource, None, Some(&mut srv))
            .map_err(|err| BackendError::WindowsApi {
                func: "ID3D11Device::CreateShaderResourceView(shader snapshot source)",
                message: err.to_string(),
            })?;
        let srv = srv.ok_or_else(|| BackendError::WindowsApi {
            func: "CreateShaderResourceView(shader snapshot source)",
            message: "返回空 SRV".to_owned(),
        })?;

        self.context
            .IASetPrimitiveTopology(D3D_PRIMITIVE_TOPOLOGY_TRIANGLELIST);
        self.context.VSSetShader(&self.vertex_shader, None);
        self.context.PSSetShader(&self.pixel_shader, None);
        self.context.PSSetShaderResources(0, Some(&[Some(srv)]));
        self.context
            .OMSetRenderTargets(Some(&[Some(self.render_target.clone())]), None);
        for viewport in viewports {
            self.context.RSSetViewports(Some(&[*viewport]));
            self.context.Draw(3, 0);
        }
        let empty_srv: [Option<ID3D11ShaderResourceView>; 1] = [None];
        self.context.PSSetShaderResources(0, Some(&empty_srv));
        let empty_rtv: [Option<ID3D11RenderTargetView>; 1] = [None];
        self.context.OMSetRenderTargets(Some(&empty_rtv), None);
        Ok(())
    }

    fn output_texture(&self) -> &windows::Win32::Graphics::Direct3D11::ID3D11Texture2D {
        &self.output
    }
}

#[cfg(windows)]
fn snapshot_viewport(
    rect: &windows::Win32::Foundation::RECT,
) -> Option<windows::Win32::Graphics::Direct3D11::D3D11_VIEWPORT> {
    let left = rect.left.max(0) as f32;
    let top = rect.top.max(0) as f32;
    let width = (rect.right - rect.left).max(0) as f32;
    let height = (rect.bottom - rect.top).max(0) as f32;
    if width <= 0.0 || height <= 0.0 {
        return None;
    }
    Some(windows::Win32::Graphics::Direct3D11::D3D11_VIEWPORT {
        TopLeftX: left,
        TopLeftY: top,
        Width: width,
        Height: height,
        MinDepth: 0.0,
        MaxDepth: 1.0,
    })
}

#[cfg(windows)]
struct GpuRgbaConverter {
    device: windows::Win32::Graphics::Direct3D11::ID3D11Device,
    context: windows::Win32::Graphics::Direct3D11::ID3D11DeviceContext,
    output: windows::Win32::Graphics::Direct3D11::ID3D11Texture2D,
    render_target: windows::Win32::Graphics::Direct3D11::ID3D11RenderTargetView,
    vertex_shader: windows::Win32::Graphics::Direct3D11::ID3D11VertexShader,
    pixel_shader: windows::Win32::Graphics::Direct3D11::ID3D11PixelShader,
    viewport: windows::Win32::Graphics::Direct3D11::D3D11_VIEWPORT,
}

#[cfg(windows)]
impl GpuRgbaConverter {
    unsafe fn new(
        device: &windows::Win32::Graphics::Direct3D11::ID3D11Device,
        context: &windows::Win32::Graphics::Direct3D11::ID3D11DeviceContext,
        width: u32,
        height: u32,
    ) -> Result<Self, BackendError> {
        use windows::Win32::Graphics::Direct3D11::{
            D3D11_BIND_RENDER_TARGET, D3D11_BIND_SHADER_RESOURCE, D3D11_TEXTURE2D_DESC,
            D3D11_USAGE_DEFAULT,
        };
        use windows::Win32::Graphics::Dxgi::Common::{
            DXGI_FORMAT_R8G8B8A8_UNORM, DXGI_SAMPLE_DESC,
        };

        let desc = D3D11_TEXTURE2D_DESC {
            Width: width.max(1),
            Height: height.max(1),
            MipLevels: 1,
            ArraySize: 1,
            Format: DXGI_FORMAT_R8G8B8A8_UNORM,
            SampleDesc: DXGI_SAMPLE_DESC {
                Count: 1,
                Quality: 0,
            },
            Usage: D3D11_USAGE_DEFAULT,
            BindFlags: (D3D11_BIND_RENDER_TARGET.0 | D3D11_BIND_SHADER_RESOURCE.0) as u32,
            CPUAccessFlags: 0,
            MiscFlags: 0,
        };
        let mut output = None;
        device
            .CreateTexture2D(&desc, None, Some(&mut output))
            .map_err(|err| BackendError::WindowsApi {
                func: "ID3D11Device::CreateTexture2D(RGBA converter)",
                message: err.to_string(),
            })?;
        let output = output.ok_or_else(|| BackendError::WindowsApi {
            func: "CreateTexture2D(RGBA converter)",
            message: "返回空纹理".to_owned(),
        })?;
        let mut render_target = None;
        device
            .CreateRenderTargetView(&output, None, Some(&mut render_target))
            .map_err(|err| BackendError::WindowsApi {
                func: "ID3D11Device::CreateRenderTargetView(RGBA converter)",
                message: err.to_string(),
            })?;
        let render_target = render_target.ok_or_else(|| BackendError::WindowsApi {
            func: "CreateRenderTargetView(RGBA converter)",
            message: "返回空 RTV".to_owned(),
        })?;

        let vs_blob = compile_shader(RGBA_CONVERT_HLSL, b"vs_main\0", b"vs_5_0\0")?;
        let ps_blob = compile_shader(RGBA_CONVERT_HLSL, b"ps_main\0", b"ps_5_0\0")?;
        let mut vertex_shader = None;
        device
            .CreateVertexShader(&vs_blob, None, Some(&mut vertex_shader))
            .map_err(|err| BackendError::WindowsApi {
                func: "ID3D11Device::CreateVertexShader(RGBA converter)",
                message: err.to_string(),
            })?;
        let mut pixel_shader = None;
        device
            .CreatePixelShader(&ps_blob, None, Some(&mut pixel_shader))
            .map_err(|err| BackendError::WindowsApi {
                func: "ID3D11Device::CreatePixelShader(RGBA converter)",
                message: err.to_string(),
            })?;

        Ok(Self {
            device: device.clone(),
            context: context.clone(),
            output,
            render_target,
            vertex_shader: vertex_shader.ok_or_else(|| BackendError::WindowsApi {
                func: "CreateVertexShader(RGBA converter)",
                message: "返回空 VS".to_owned(),
            })?,
            pixel_shader: pixel_shader.ok_or_else(|| BackendError::WindowsApi {
                func: "CreatePixelShader(RGBA converter)",
                message: "返回空 PS".to_owned(),
            })?,
            viewport: windows::Win32::Graphics::Direct3D11::D3D11_VIEWPORT {
                TopLeftX: 0.0,
                TopLeftY: 0.0,
                Width: width as f32,
                Height: height as f32,
                MinDepth: 0.0,
                MaxDepth: 1.0,
            },
        })
    }

    unsafe fn convert(
        &self,
        source: &windows::Win32::Graphics::Direct3D11::ID3D11Texture2D,
    ) -> Result<windows::Win32::Graphics::Direct3D11::ID3D11Texture2D, BackendError> {
        use windows::Win32::Graphics::Direct3D::D3D_PRIMITIVE_TOPOLOGY_TRIANGLELIST;
        use windows::Win32::Graphics::Direct3D11::{
            ID3D11RenderTargetView, ID3D11Resource, ID3D11ShaderResourceView,
        };
        use windows::core::Interface;

        let source_resource: ID3D11Resource =
            source.cast().map_err(|err| BackendError::WindowsApi {
                func: "ID3D11Texture2D::cast<ID3D11Resource>(RGBA source)",
                message: err.to_string(),
            })?;
        let mut srv = None;
        self.device
            .CreateShaderResourceView(&source_resource, None, Some(&mut srv))
            .map_err(|err| BackendError::WindowsApi {
                func: "ID3D11Device::CreateShaderResourceView(RGBA source)",
                message: err.to_string(),
            })?;
        let srv = srv.ok_or_else(|| BackendError::WindowsApi {
            func: "CreateShaderResourceView(RGBA source)",
            message: "返回空 SRV".to_owned(),
        })?;

        self.context.RSSetViewports(Some(&[self.viewport]));
        self.context
            .IASetPrimitiveTopology(D3D_PRIMITIVE_TOPOLOGY_TRIANGLELIST);
        self.context.VSSetShader(&self.vertex_shader, None);
        self.context.PSSetShader(&self.pixel_shader, None);
        self.context.PSSetShaderResources(0, Some(&[Some(srv)]));
        self.context
            .OMSetRenderTargets(Some(&[Some(self.render_target.clone())]), None);
        self.context.Draw(3, 0);
        let empty_srv: [Option<ID3D11ShaderResourceView>; 1] = [None];
        self.context.PSSetShaderResources(0, Some(&empty_srv));
        let empty_rtv: [Option<ID3D11RenderTargetView>; 1] = [None];
        self.context.OMSetRenderTargets(Some(&empty_rtv), None);
        Ok(self.output.clone())
    }

    fn output_texture(&self) -> &windows::Win32::Graphics::Direct3D11::ID3D11Texture2D {
        &self.output
    }
}

#[cfg(windows)]
struct GpuRgb4Converter {
    device: windows::Win32::Graphics::Direct3D11::ID3D11Device,
    context: windows::Win32::Graphics::Direct3D11::ID3D11DeviceContext,
    render_target: windows::Win32::Graphics::Direct3D11::ID3D11RenderTargetView,
    vertex_shader: windows::Win32::Graphics::Direct3D11::ID3D11VertexShader,
    pixel_shader: windows::Win32::Graphics::Direct3D11::ID3D11PixelShader,
    viewport: windows::Win32::Graphics::Direct3D11::D3D11_VIEWPORT,
}

#[cfg(windows)]
impl GpuRgb4Converter {
    unsafe fn new(
        device: &windows::Win32::Graphics::Direct3D11::ID3D11Device,
        context: &windows::Win32::Graphics::Direct3D11::ID3D11DeviceContext,
        output: &windows::Win32::Graphics::Direct3D11::ID3D11Texture2D,
        width: u32,
        height: u32,
    ) -> Result<Self, BackendError> {
        use windows::Win32::Graphics::Direct3D11::{
            D3D11_RENDER_TARGET_VIEW_DESC, D3D11_RENDER_TARGET_VIEW_DESC_0,
            D3D11_RTV_DIMENSION_TEXTURE2D, D3D11_TEX2D_RTV,
        };
        use windows::Win32::Graphics::Dxgi::Common::DXGI_FORMAT_B8G8R8A8_UNORM;

        let rtv_desc = D3D11_RENDER_TARGET_VIEW_DESC {
            Format: DXGI_FORMAT_B8G8R8A8_UNORM,
            ViewDimension: D3D11_RTV_DIMENSION_TEXTURE2D,
            Anonymous: D3D11_RENDER_TARGET_VIEW_DESC_0 {
                Texture2D: D3D11_TEX2D_RTV { MipSlice: 0 },
            },
        };
        let mut render_target = None;
        device
            .CreateRenderTargetView(output, Some(&rtv_desc), Some(&mut render_target))
            .map_err(|err| BackendError::WindowsApi {
                func: "ID3D11Device::CreateRenderTargetView(RGB4 writer)",
                message: err.to_string(),
            })?;

        let vs_blob = compile_shader(RGBA_CONVERT_HLSL, b"vs_main\0", b"vs_5_0\0")?;
        let ps_blob = compile_shader(RGBA_CONVERT_HLSL, b"ps_main\0", b"ps_5_0\0")?;
        let mut vertex_shader = None;
        device
            .CreateVertexShader(&vs_blob, None, Some(&mut vertex_shader))
            .map_err(|err| BackendError::WindowsApi {
                func: "ID3D11Device::CreateVertexShader(RGB4 writer)",
                message: err.to_string(),
            })?;
        let mut pixel_shader = None;
        device
            .CreatePixelShader(&ps_blob, None, Some(&mut pixel_shader))
            .map_err(|err| BackendError::WindowsApi {
                func: "ID3D11Device::CreatePixelShader(RGB4 writer)",
                message: err.to_string(),
            })?;

        Ok(Self {
            device: device.clone(),
            context: context.clone(),
            render_target: render_target.ok_or_else(|| BackendError::WindowsApi {
                func: "CreateRenderTargetView(RGB4 writer)",
                message: "返回空 RTV".to_owned(),
            })?,
            vertex_shader: vertex_shader.ok_or_else(|| BackendError::WindowsApi {
                func: "CreateVertexShader(RGB4 writer)",
                message: "返回空 VS".to_owned(),
            })?,
            pixel_shader: pixel_shader.ok_or_else(|| BackendError::WindowsApi {
                func: "CreatePixelShader(RGB4 writer)",
                message: "返回空 PS".to_owned(),
            })?,
            viewport: windows::Win32::Graphics::Direct3D11::D3D11_VIEWPORT {
                TopLeftX: 0.0,
                TopLeftY: 0.0,
                Width: width as f32,
                Height: height as f32,
                MinDepth: 0.0,
                MaxDepth: 1.0,
            },
        })
    }

    unsafe fn convert(
        &self,
        source: &windows::Win32::Graphics::Direct3D11::ID3D11Texture2D,
    ) -> Result<(), BackendError> {
        use windows::Win32::Graphics::Direct3D::D3D_PRIMITIVE_TOPOLOGY_TRIANGLELIST;
        use windows::Win32::Graphics::Direct3D11::{
            ID3D11RenderTargetView, ID3D11ShaderResourceView,
        };

        let srv = create_shader_resource_view_with_gpu_copy_fallback(
            &self.device,
            &self.context,
            source,
            "ID3D11Device::CreateShaderResourceView(RGB4 writer source)",
        )?;

        self.context.RSSetViewports(Some(&[self.viewport]));
        self.context
            .IASetPrimitiveTopology(D3D_PRIMITIVE_TOPOLOGY_TRIANGLELIST);
        self.context.VSSetShader(&self.vertex_shader, None);
        self.context.PSSetShader(&self.pixel_shader, None);
        self.context.PSSetShaderResources(0, Some(&[Some(srv)]));
        self.context
            .OMSetRenderTargets(Some(&[Some(self.render_target.clone())]), None);
        self.context.Draw(3, 0);
        let empty_srv: [Option<ID3D11ShaderResourceView>; 1] = [None];
        self.context.PSSetShaderResources(0, Some(&empty_srv));
        let empty_rtv: [Option<ID3D11RenderTargetView>; 1] = [None];
        self.context.OMSetRenderTargets(Some(&empty_rtv), None);
        Ok(())
    }
}

#[cfg(windows)]
enum GpuRecordConverter {
    P010(GpuP010Converter),
    Nv12(GpuNv12Converter),
    Packed(GpuPackedConverter),
    Rgb4(GpuRgb4Converter),
}

#[cfg(windows)]
impl GpuRecordConverter {
    unsafe fn new(
        route: VplRecordRoute,
        device: &windows::Win32::Graphics::Direct3D11::ID3D11Device,
        context: &windows::Win32::Graphics::Direct3D11::ID3D11DeviceContext,
        output: &windows::Win32::Graphics::Direct3D11::ID3D11Texture2D,
        width: u32,
        height: u32,
        enable_compute: bool,
    ) -> Result<Self, BackendError> {
        match route.fourcc {
            MFX_FOURCC_NV12 => Ok(Self::Nv12(GpuNv12Converter::new(
                route, device, context, output, width, height,
            )?)),
            MFX_FOURCC_P010 => Ok(Self::P010(GpuP010Converter::new(
                route,
                device,
                context,
                output,
                width,
                height,
                enable_compute,
            )?)),
            MFX_FOURCC_YUY2 | MFX_FOURCC_Y210 | MFX_FOURCC_AYUV | MFX_FOURCC_Y410 => {
                Ok(Self::Packed(GpuPackedConverter::new(
                    route, device, context, output, width, height,
                )?))
            }
            MFX_FOURCC_RGB4 => Ok(Self::Rgb4(GpuRgb4Converter::new(
                device, context, output, width, height,
            )?)),
            _ => Err(BackendError::unsupported(
                "GPU ChromaWriter",
                fourcc_to_string(route.fourcc),
                "该 oneVPL FourCC 仍只参与 Query，尚无生产 GPU writer",
            )),
        }
    }

    unsafe fn convert(
        &self,
        source: &windows::Win32::Graphics::Direct3D11::ID3D11Texture2D,
    ) -> Result<(), BackendError> {
        match self {
            Self::P010(converter) => converter.convert(source),
            Self::Nv12(converter) => converter.convert(source),
            Self::Packed(converter) => converter.convert(source),
            Self::Rgb4(converter) => converter.convert(source),
        }
    }
}

#[cfg(windows)]
struct GpuNv12Converter {
    device: windows::Win32::Graphics::Direct3D11::ID3D11Device,
    context: windows::Win32::Graphics::Direct3D11::ID3D11DeviceContext,
    luma_uav: windows::Win32::Graphics::Direct3D11::ID3D11UnorderedAccessView,
    chroma_uav: windows::Win32::Graphics::Direct3D11::ID3D11UnorderedAccessView,
    compute_shader: windows::Win32::Graphics::Direct3D11::ID3D11ComputeShader,
    chroma_width: u32,
    chroma_height: u32,
}

#[cfg(windows)]
impl GpuNv12Converter {
    unsafe fn new(
        route: VplRecordRoute,
        device: &windows::Win32::Graphics::Direct3D11::ID3D11Device,
        context: &windows::Win32::Graphics::Direct3D11::ID3D11DeviceContext,
        output: &windows::Win32::Graphics::Direct3D11::ID3D11Texture2D,
        width: u32,
        height: u32,
    ) -> Result<Self, BackendError> {
        use windows::Win32::Graphics::Direct3D11::{
            D3D11_TEX2D_UAV1, D3D11_UAV_DIMENSION_TEXTURE2D, D3D11_UNORDERED_ACCESS_VIEW_DESC1,
            D3D11_UNORDERED_ACCESS_VIEW_DESC1_0, ID3D11Device3, ID3D11Resource,
            ID3D11UnorderedAccessView1,
        };
        use windows::Win32::Graphics::Dxgi::Common::{
            DXGI_FORMAT_R8_UNORM, DXGI_FORMAT_R8G8_UNORM,
        };
        use windows::core::Interface;

        let device3: ID3D11Device3 = device.cast().map_err(|err| BackendError::WindowsApi {
            func: "ID3D11Device::cast<ID3D11Device3>(NV12 converter)",
            message: err.to_string(),
        })?;
        let output_resource: ID3D11Resource =
            output.cast().map_err(|err| BackendError::WindowsApi {
                func: "ID3D11Texture2D::cast<ID3D11Resource>(NV12 output)",
                message: err.to_string(),
            })?;

        let luma_uav_desc = D3D11_UNORDERED_ACCESS_VIEW_DESC1 {
            Format: DXGI_FORMAT_R8_UNORM,
            ViewDimension: D3D11_UAV_DIMENSION_TEXTURE2D,
            Anonymous: D3D11_UNORDERED_ACCESS_VIEW_DESC1_0 {
                Texture2D: D3D11_TEX2D_UAV1 {
                    MipSlice: 0,
                    PlaneSlice: 0,
                },
            },
        };
        let mut luma_uav1: Option<ID3D11UnorderedAccessView1> = None;
        device3
            .CreateUnorderedAccessView1(
                &output_resource,
                Some(&luma_uav_desc),
                Some(&mut luma_uav1),
            )
            .map_err(|err| BackendError::WindowsApi {
                func: "ID3D11Device3::CreateUnorderedAccessView1(NV12 luma)",
                message: err.to_string(),
            })?;
        let luma_uav = luma_uav1
            .ok_or_else(|| BackendError::WindowsApi {
                func: "CreateUnorderedAccessView1(NV12 luma)",
                message: "返回空 luma UAV".to_owned(),
            })?
            .cast()
            .map_err(|err| BackendError::WindowsApi {
                func: "ID3D11UnorderedAccessView1::cast(NV12 luma)",
                message: err.to_string(),
            })?;

        let chroma_uav_desc = D3D11_UNORDERED_ACCESS_VIEW_DESC1 {
            Format: DXGI_FORMAT_R8G8_UNORM,
            ViewDimension: D3D11_UAV_DIMENSION_TEXTURE2D,
            Anonymous: D3D11_UNORDERED_ACCESS_VIEW_DESC1_0 {
                Texture2D: D3D11_TEX2D_UAV1 {
                    MipSlice: 0,
                    PlaneSlice: 1,
                },
            },
        };
        let mut chroma_uav1: Option<ID3D11UnorderedAccessView1> = None;
        device3
            .CreateUnorderedAccessView1(
                &output_resource,
                Some(&chroma_uav_desc),
                Some(&mut chroma_uav1),
            )
            .map_err(|err| BackendError::WindowsApi {
                func: "ID3D11Device3::CreateUnorderedAccessView1(NV12 chroma)",
                message: err.to_string(),
            })?;
        let chroma_uav = chroma_uav1
            .ok_or_else(|| BackendError::WindowsApi {
                func: "CreateUnorderedAccessView1(NV12 chroma)",
                message: "返回空 chroma UAV".to_owned(),
            })?
            .cast()
            .map_err(|err| BackendError::WindowsApi {
                func: "ID3D11UnorderedAccessView1::cast(NV12 chroma)",
                message: err.to_string(),
            })?;

        let shader_template = if route.is_bt2020_sdr() {
            NV12_BT2020_CONVERT_HLSL
        } else {
            NV12_CONVERT_HLSL
        };
        let shader_source = shader_source_with_range(shader_template, route.mp4_color.full_range);
        let cs_blob = compile_shader(&shader_source, b"cs_main\0", b"cs_5_0\0")?;
        let mut compute_shader = None;
        device
            .CreateComputeShader(&cs_blob, None, Some(&mut compute_shader))
            .map_err(|err| BackendError::WindowsApi {
                func: "ID3D11Device::CreateComputeShader(NV12 compute)",
                message: err.to_string(),
            })?;

        Ok(Self {
            device: device.clone(),
            context: context.clone(),
            luma_uav,
            chroma_uav,
            compute_shader: compute_shader.ok_or_else(|| BackendError::WindowsApi {
                func: "CreateComputeShader(NV12 compute)",
                message: "返回空 CS".to_owned(),
            })?,
            chroma_width: (width / 2).max(1),
            chroma_height: (height / 2).max(1),
        })
    }

    unsafe fn convert(
        &self,
        source: &windows::Win32::Graphics::Direct3D11::ID3D11Texture2D,
    ) -> Result<(), BackendError> {
        use windows::Win32::Graphics::Direct3D11::{
            ID3D11ComputeShader, ID3D11ShaderResourceView, ID3D11UnorderedAccessView,
        };

        let srv = create_shader_resource_view_with_gpu_copy_fallback(
            &self.device,
            &self.context,
            source,
            "ID3D11Device::CreateShaderResourceView(NV12 source)",
        )?;

        self.context.CSSetShader(&self.compute_shader, None);
        self.context.CSSetShaderResources(0, Some(&[Some(srv)]));
        let uavs = [Some(self.luma_uav.clone()), Some(self.chroma_uav.clone())];
        self.context
            .CSSetUnorderedAccessViews(0, 2, Some(uavs.as_ptr()), None);
        self.context.Dispatch(
            self.chroma_width.div_ceil(8),
            self.chroma_height.div_ceil(8),
            1,
        );
        let empty_srv: [Option<ID3D11ShaderResourceView>; 1] = [None];
        self.context.CSSetShaderResources(0, Some(&empty_srv));
        let empty_uav: [Option<ID3D11UnorderedAccessView>; 2] = [None, None];
        self.context
            .CSSetUnorderedAccessViews(0, 2, Some(empty_uav.as_ptr()), None);
        self.context.CSSetShader(None::<&ID3D11ComputeShader>, None);
        Ok(())
    }
}

#[cfg(windows)]
struct GpuPackedConverter {
    device: windows::Win32::Graphics::Direct3D11::ID3D11Device,
    context: windows::Win32::Graphics::Direct3D11::ID3D11DeviceContext,
    output_uav: windows::Win32::Graphics::Direct3D11::ID3D11UnorderedAccessView,
    compute_shader: windows::Win32::Graphics::Direct3D11::ID3D11ComputeShader,
    dispatch_width: u32,
    dispatch_height: u32,
    label: &'static str,
}

#[cfg(windows)]
impl GpuPackedConverter {
    unsafe fn new(
        route: VplRecordRoute,
        device: &windows::Win32::Graphics::Direct3D11::ID3D11Device,
        context: &windows::Win32::Graphics::Direct3D11::ID3D11DeviceContext,
        output: &windows::Win32::Graphics::Direct3D11::ID3D11Texture2D,
        width: u32,
        height: u32,
    ) -> Result<Self, BackendError> {
        use windows::Win32::Graphics::Direct3D11::{
            D3D11_TEX2D_UAV1, D3D11_UAV_DIMENSION_TEXTURE2D, D3D11_UNORDERED_ACCESS_VIEW_DESC1,
            D3D11_UNORDERED_ACCESS_VIEW_DESC1_0, ID3D11Device3, ID3D11Resource,
            ID3D11UnorderedAccessView1,
        };
        use windows::Win32::Graphics::Dxgi::Common::{
            DXGI_FORMAT_R16G16B16A16_UINT, DXGI_FORMAT_R32_UINT,
        };
        use windows::core::Interface;

        let (view_format, shader_template, label, dispatch_width) = match route.fourcc {
            MFX_FOURCC_YUY2 => (
                DXGI_FORMAT_R32_UINT,
                if route.is_bt2020_sdr() {
                    YUY2_BT2020_CONVERT_HLSL
                } else {
                    YUY2_CONVERT_HLSL
                },
                "YUY2 8-bit 4:2:2",
                width.div_ceil(2),
            ),
            MFX_FOURCC_Y210 => (
                DXGI_FORMAT_R16G16B16A16_UINT,
                if route.is_hdr_pq() {
                    Y210_CONVERT_HLSL
                } else if route.is_bt2020_sdr() {
                    Y210_SDR_BT2020_CONVERT_HLSL
                } else {
                    Y210_SDR10_CONVERT_HLSL
                },
                "Y210 10-bit 4:2:2",
                width.div_ceil(2),
            ),
            MFX_FOURCC_AYUV => (
                DXGI_FORMAT_R32_UINT,
                if route.is_bt2020_sdr() {
                    AYUV_BT2020_CONVERT_HLSL
                } else {
                    AYUV_CONVERT_HLSL
                },
                "AYUV 8-bit 4:4:4",
                width,
            ),
            MFX_FOURCC_Y410 => (
                DXGI_FORMAT_R32_UINT,
                if route.is_hdr_pq() {
                    Y410_CONVERT_HLSL
                } else if route.is_bt2020_sdr() {
                    Y410_SDR_BT2020_CONVERT_HLSL
                } else {
                    Y410_SDR10_CONVERT_HLSL
                },
                "Y410 10-bit 4:4:4",
                width,
            ),
            _ => {
                return Err(BackendError::unsupported(
                    "GPU packed ChromaWriter",
                    fourcc_to_string(route.fourcc),
                    "没有对应 packed writer",
                ));
            }
        };

        let device3: ID3D11Device3 = device.cast().map_err(|err| BackendError::WindowsApi {
            func: "ID3D11Device::cast<ID3D11Device3>(packed converter)",
            message: err.to_string(),
        })?;
        let output_resource: ID3D11Resource =
            output.cast().map_err(|err| BackendError::WindowsApi {
                func: "ID3D11Texture2D::cast<ID3D11Resource>(packed output)",
                message: err.to_string(),
            })?;
        let uav_desc = D3D11_UNORDERED_ACCESS_VIEW_DESC1 {
            Format: view_format,
            ViewDimension: D3D11_UAV_DIMENSION_TEXTURE2D,
            Anonymous: D3D11_UNORDERED_ACCESS_VIEW_DESC1_0 {
                Texture2D: D3D11_TEX2D_UAV1 {
                    MipSlice: 0,
                    PlaneSlice: 0,
                },
            },
        };
        let mut output_uav1: Option<ID3D11UnorderedAccessView1> = None;
        device3
            .CreateUnorderedAccessView1(&output_resource, Some(&uav_desc), Some(&mut output_uav1))
            .map_err(|err| BackendError::WindowsApi {
                func: "ID3D11Device3::CreateUnorderedAccessView1(packed output)",
                message: err.to_string(),
            })?;
        let output_uav = output_uav1
            .ok_or_else(|| BackendError::WindowsApi {
                func: "CreateUnorderedAccessView1(packed output)",
                message: "返回空 UAV".to_owned(),
            })?
            .cast()
            .map_err(|err| BackendError::WindowsApi {
                func: "ID3D11UnorderedAccessView1::cast(packed output)",
                message: err.to_string(),
            })?;

        let shader_source = shader_source_with_range(shader_template, route.mp4_color.full_range);
        let cs_blob = compile_shader(&shader_source, b"cs_main\0", b"cs_5_0\0")?;
        let mut compute_shader = None;
        device
            .CreateComputeShader(&cs_blob, None, Some(&mut compute_shader))
            .map_err(|err| BackendError::WindowsApi {
                func: "ID3D11Device::CreateComputeShader(packed converter)",
                message: err.to_string(),
            })?;

        Ok(Self {
            device: device.clone(),
            context: context.clone(),
            output_uav,
            compute_shader: compute_shader.ok_or_else(|| BackendError::WindowsApi {
                func: "CreateComputeShader(packed converter)",
                message: "返回空 CS".to_owned(),
            })?,
            dispatch_width: dispatch_width.max(1),
            dispatch_height: height.max(1),
            label,
        })
    }

    unsafe fn convert(
        &self,
        source: &windows::Win32::Graphics::Direct3D11::ID3D11Texture2D,
    ) -> Result<(), BackendError> {
        use windows::Win32::Graphics::Direct3D11::{
            ID3D11ComputeShader, ID3D11ShaderResourceView, ID3D11UnorderedAccessView,
        };

        let srv = create_shader_resource_view_with_gpu_copy_fallback(
            &self.device,
            &self.context,
            source,
            "ID3D11Device::CreateShaderResourceView(packed source)",
        )?;

        self.context.CSSetShader(&self.compute_shader, None);
        self.context.CSSetShaderResources(0, Some(&[Some(srv)]));
        let uavs = [Some(self.output_uav.clone())];
        self.context
            .CSSetUnorderedAccessViews(0, 1, Some(uavs.as_ptr()), None);
        self.context.Dispatch(
            self.dispatch_width.div_ceil(16),
            self.dispatch_height.div_ceil(8),
            1,
        );
        let empty_srv: [Option<ID3D11ShaderResourceView>; 1] = [None];
        self.context.CSSetShaderResources(0, Some(&empty_srv));
        let empty_uav: [Option<ID3D11UnorderedAccessView>; 1] = [None];
        self.context
            .CSSetUnorderedAccessViews(0, 1, Some(empty_uav.as_ptr()), None);
        self.context.CSSetShader(None::<&ID3D11ComputeShader>, None);
        Ok(())
    }
}

#[cfg(windows)]
struct GpuP010Converter {
    device: windows::Win32::Graphics::Direct3D11::ID3D11Device,
    context: windows::Win32::Graphics::Direct3D11::ID3D11DeviceContext,
    luma_target: windows::Win32::Graphics::Direct3D11::ID3D11RenderTargetView,
    chroma_target: windows::Win32::Graphics::Direct3D11::ID3D11RenderTargetView,
    pq_lut: Option<windows::Win32::Graphics::Direct3D11::ID3D11ShaderResourceView>,
    luma_uav: Option<windows::Win32::Graphics::Direct3D11::ID3D11UnorderedAccessView>,
    chroma_uav: Option<windows::Win32::Graphics::Direct3D11::ID3D11UnorderedAccessView>,
    vertex_shader: windows::Win32::Graphics::Direct3D11::ID3D11VertexShader,
    luma_shader: windows::Win32::Graphics::Direct3D11::ID3D11PixelShader,
    chroma_shader: windows::Win32::Graphics::Direct3D11::ID3D11PixelShader,
    compute_shader: Option<windows::Win32::Graphics::Direct3D11::ID3D11ComputeShader>,
    luma_viewport: windows::Win32::Graphics::Direct3D11::D3D11_VIEWPORT,
    chroma_viewport: windows::Win32::Graphics::Direct3D11::D3D11_VIEWPORT,
    chroma_width: u32,
    chroma_height: u32,
}

#[cfg(windows)]
impl GpuP010Converter {
    unsafe fn new(
        route: VplRecordRoute,
        device: &windows::Win32::Graphics::Direct3D11::ID3D11Device,
        context: &windows::Win32::Graphics::Direct3D11::ID3D11DeviceContext,
        output: &windows::Win32::Graphics::Direct3D11::ID3D11Texture2D,
        width: u32,
        height: u32,
        enable_compute: bool,
    ) -> Result<Self, BackendError> {
        use windows::Win32::Graphics::Direct3D11::{
            D3D11_RENDER_TARGET_VIEW_DESC1, D3D11_RENDER_TARGET_VIEW_DESC1_0,
            D3D11_RTV_DIMENSION_TEXTURE2D, D3D11_TEX2D_RTV1, D3D11_TEX2D_UAV1,
            D3D11_UAV_DIMENSION_TEXTURE2D, D3D11_UNORDERED_ACCESS_VIEW_DESC1,
            D3D11_UNORDERED_ACCESS_VIEW_DESC1_0, ID3D11Device3, ID3D11RenderTargetView,
            ID3D11RenderTargetView1, ID3D11Resource, ID3D11UnorderedAccessView1,
        };
        use windows::Win32::Graphics::Dxgi::Common::{
            DXGI_FORMAT_R16_UNORM, DXGI_FORMAT_R16G16_UNORM,
        };
        use windows::core::Interface;

        let device3: ID3D11Device3 = device.cast().map_err(|err| BackendError::WindowsApi {
            func: "ID3D11Device::cast<ID3D11Device3>(P010 plane converter)",
            message: err.to_string(),
        })?;
        let output_resource: ID3D11Resource =
            output.cast().map_err(|err| BackendError::WindowsApi {
                func: "ID3D11Texture2D::cast<ID3D11Resource>(P010 output)",
                message: err.to_string(),
            })?;

        let luma_desc = D3D11_RENDER_TARGET_VIEW_DESC1 {
            Format: DXGI_FORMAT_R16_UNORM,
            ViewDimension: D3D11_RTV_DIMENSION_TEXTURE2D,
            Anonymous: D3D11_RENDER_TARGET_VIEW_DESC1_0 {
                Texture2D: D3D11_TEX2D_RTV1 {
                    MipSlice: 0,
                    PlaneSlice: 0,
                },
            },
        };
        let mut luma_target1: Option<ID3D11RenderTargetView1> = None;
        device3
            .CreateRenderTargetView1(&output_resource, Some(&luma_desc), Some(&mut luma_target1))
            .map_err(|err| BackendError::WindowsApi {
                func: "ID3D11Device3::CreateRenderTargetView1(P010 luma)",
                message: err.to_string(),
            })?;
        let luma_target: ID3D11RenderTargetView = luma_target1
            .ok_or_else(|| BackendError::WindowsApi {
                func: "CreateRenderTargetView1(P010 luma)",
                message: "返回空 luma RTV".to_owned(),
            })?
            .cast()
            .map_err(|err| BackendError::WindowsApi {
                func: "ID3D11RenderTargetView1::cast(P010 luma)",
                message: err.to_string(),
            })?;

        let chroma_desc = D3D11_RENDER_TARGET_VIEW_DESC1 {
            Format: DXGI_FORMAT_R16G16_UNORM,
            ViewDimension: D3D11_RTV_DIMENSION_TEXTURE2D,
            Anonymous: D3D11_RENDER_TARGET_VIEW_DESC1_0 {
                Texture2D: D3D11_TEX2D_RTV1 {
                    MipSlice: 0,
                    PlaneSlice: 1,
                },
            },
        };
        let mut chroma_target1: Option<ID3D11RenderTargetView1> = None;
        device3
            .CreateRenderTargetView1(
                &output_resource,
                Some(&chroma_desc),
                Some(&mut chroma_target1),
            )
            .map_err(|err| BackendError::WindowsApi {
                func: "ID3D11Device3::CreateRenderTargetView1(P010 chroma)",
                message: err.to_string(),
            })?;
        let chroma_target: ID3D11RenderTargetView = chroma_target1
            .ok_or_else(|| BackendError::WindowsApi {
                func: "CreateRenderTargetView1(P010 chroma)",
                message: "返回空 chroma RTV".to_owned(),
            })?
            .cast()
            .map_err(|err| BackendError::WindowsApi {
                func: "ID3D11RenderTargetView1::cast(P010 chroma)",
                message: err.to_string(),
            })?;

        let transfer_lut = if route.is_hdr_pq() {
            Some(create_st2084_pq_lut_srv(device)?)
        } else {
            None
        };

        let shader_template = if route.is_hdr_pq() {
            P010_CONVERT_HLSL
        } else if route.is_bt2020_sdr() {
            P010_SDR_BT2020_CONVERT_HLSL
        } else {
            P010_SDR10_CONVERT_HLSL
        };
        let shader_source = shader_source_with_range(shader_template, route.mp4_color.full_range);
        let vs_blob = compile_shader(&shader_source, b"vs_main\0", b"vs_5_0\0")?;
        let luma_blob = compile_shader(&shader_source, b"ps_luma\0", b"ps_5_0\0")?;
        let chroma_blob = compile_shader(&shader_source, b"ps_chroma\0", b"ps_5_0\0")?;
        let compute_blob = if enable_compute {
            Some(compile_shader(&shader_source, b"cs_main\0", b"cs_5_0\0")?)
        } else {
            None
        };
        let mut vertex_shader = None;
        device
            .CreateVertexShader(&vs_blob, None, Some(&mut vertex_shader))
            .map_err(|err| BackendError::WindowsApi {
                func: "ID3D11Device::CreateVertexShader(P010 converter)",
                message: err.to_string(),
            })?;
        let mut luma_shader = None;
        device
            .CreatePixelShader(&luma_blob, None, Some(&mut luma_shader))
            .map_err(|err| BackendError::WindowsApi {
                func: "ID3D11Device::CreatePixelShader(P010 luma)",
                message: err.to_string(),
            })?;
        let mut chroma_shader = None;
        device
            .CreatePixelShader(&chroma_blob, None, Some(&mut chroma_shader))
            .map_err(|err| BackendError::WindowsApi {
                func: "ID3D11Device::CreatePixelShader(P010 chroma)",
                message: err.to_string(),
            })?;
        let mut compute_shader = None;
        if let Some(blob) = compute_blob {
            device
                .CreateComputeShader(&blob, None, Some(&mut compute_shader))
                .map_err(|err| BackendError::WindowsApi {
                    func: "ID3D11Device::CreateComputeShader(P010 compute)",
                    message: err.to_string(),
                })?;
        }

        let mut luma_uav = None;
        let mut chroma_uav = None;
        if enable_compute {
            let luma_uav_desc = D3D11_UNORDERED_ACCESS_VIEW_DESC1 {
                Format: DXGI_FORMAT_R16_UNORM,
                ViewDimension: D3D11_UAV_DIMENSION_TEXTURE2D,
                Anonymous: D3D11_UNORDERED_ACCESS_VIEW_DESC1_0 {
                    Texture2D: D3D11_TEX2D_UAV1 {
                        MipSlice: 0,
                        PlaneSlice: 0,
                    },
                },
            };
            let mut luma_uav1: Option<ID3D11UnorderedAccessView1> = None;
            device3
                .CreateUnorderedAccessView1(
                    &output_resource,
                    Some(&luma_uav_desc),
                    Some(&mut luma_uav1),
                )
                .map_err(|err| BackendError::WindowsApi {
                    func: "ID3D11Device3::CreateUnorderedAccessView1(P010 luma)",
                    message: err.to_string(),
                })?;
            luma_uav = Some(
                luma_uav1
                    .ok_or_else(|| BackendError::WindowsApi {
                        func: "CreateUnorderedAccessView1(P010 luma)",
                        message: "返回空 luma UAV".to_owned(),
                    })?
                    .cast()
                    .map_err(|err| BackendError::WindowsApi {
                        func: "ID3D11UnorderedAccessView1::cast(P010 luma)",
                        message: err.to_string(),
                    })?,
            );

            let chroma_uav_desc = D3D11_UNORDERED_ACCESS_VIEW_DESC1 {
                Format: DXGI_FORMAT_R16G16_UNORM,
                ViewDimension: D3D11_UAV_DIMENSION_TEXTURE2D,
                Anonymous: D3D11_UNORDERED_ACCESS_VIEW_DESC1_0 {
                    Texture2D: D3D11_TEX2D_UAV1 {
                        MipSlice: 0,
                        PlaneSlice: 1,
                    },
                },
            };
            let mut chroma_uav1: Option<ID3D11UnorderedAccessView1> = None;
            device3
                .CreateUnorderedAccessView1(
                    &output_resource,
                    Some(&chroma_uav_desc),
                    Some(&mut chroma_uav1),
                )
                .map_err(|err| BackendError::WindowsApi {
                    func: "ID3D11Device3::CreateUnorderedAccessView1(P010 chroma)",
                    message: err.to_string(),
                })?;
            chroma_uav = Some(
                chroma_uav1
                    .ok_or_else(|| BackendError::WindowsApi {
                        func: "CreateUnorderedAccessView1(P010 chroma)",
                        message: "返回空 chroma UAV".to_owned(),
                    })?
                    .cast()
                    .map_err(|err| BackendError::WindowsApi {
                        func: "ID3D11UnorderedAccessView1::cast(P010 chroma)",
                        message: err.to_string(),
                    })?,
            );
        }

        let chroma_width = (width / 2).max(1);
        let chroma_height = (height / 2).max(1);

        Ok(Self {
            device: device.clone(),
            context: context.clone(),
            luma_target,
            chroma_target,
            pq_lut: transfer_lut,
            luma_uav,
            chroma_uav,
            vertex_shader: vertex_shader.ok_or_else(|| BackendError::WindowsApi {
                func: "CreateVertexShader(P010 converter)",
                message: "返回空 VS".to_owned(),
            })?,
            luma_shader: luma_shader.ok_or_else(|| BackendError::WindowsApi {
                func: "CreatePixelShader(P010 luma)",
                message: "返回空 luma PS".to_owned(),
            })?,
            chroma_shader: chroma_shader.ok_or_else(|| BackendError::WindowsApi {
                func: "CreatePixelShader(P010 chroma)",
                message: "返回空 chroma PS".to_owned(),
            })?,
            compute_shader,
            luma_viewport: windows::Win32::Graphics::Direct3D11::D3D11_VIEWPORT {
                TopLeftX: 0.0,
                TopLeftY: 0.0,
                Width: width as f32,
                Height: height as f32,
                MinDepth: 0.0,
                MaxDepth: 1.0,
            },
            chroma_viewport: windows::Win32::Graphics::Direct3D11::D3D11_VIEWPORT {
                TopLeftX: 0.0,
                TopLeftY: 0.0,
                Width: (width / 2).max(1) as f32,
                Height: (height / 2).max(1) as f32,
                MinDepth: 0.0,
                MaxDepth: 1.0,
            },
            chroma_width,
            chroma_height,
        })
    }

    unsafe fn convert(
        &self,
        source: &windows::Win32::Graphics::Direct3D11::ID3D11Texture2D,
    ) -> Result<(), BackendError> {
        use windows::Win32::Graphics::Direct3D::D3D_PRIMITIVE_TOPOLOGY_TRIANGLELIST;
        use windows::Win32::Graphics::Direct3D11::{
            ID3D11ComputeShader, ID3D11RenderTargetView, ID3D11ShaderResourceView,
            ID3D11UnorderedAccessView,
        };

        let srv = create_shader_resource_view_with_gpu_copy_fallback(
            &self.device,
            &self.context,
            source,
            "ID3D11Device::CreateShaderResourceView(P010 source)",
        )?;

        if let (Some(compute_shader), Some(luma_uav), Some(chroma_uav)) =
            (&self.compute_shader, &self.luma_uav, &self.chroma_uav)
        {
            self.context.CSSetShader(compute_shader, None);
            if let Some(pq_lut) = &self.pq_lut {
                self.context
                    .CSSetShaderResources(0, Some(&[Some(srv), Some(pq_lut.clone())]));
            } else {
                self.context.CSSetShaderResources(0, Some(&[Some(srv)]));
            }
            let uavs = [Some(luma_uav.clone()), Some(chroma_uav.clone())];
            self.context
                .CSSetUnorderedAccessViews(0, 2, Some(uavs.as_ptr()), None);
            self.context.Dispatch(
                self.chroma_width.div_ceil(8),
                self.chroma_height.div_ceil(8),
                1,
            );
            let empty_srv: [Option<ID3D11ShaderResourceView>; 2] = [None, None];
            self.context.CSSetShaderResources(0, Some(&empty_srv));
            let empty_uav: [Option<ID3D11UnorderedAccessView>; 2] = [None, None];
            self.context
                .CSSetUnorderedAccessViews(0, 2, Some(empty_uav.as_ptr()), None);
            self.context.CSSetShader(None::<&ID3D11ComputeShader>, None);
            return Ok(());
        }

        self.context
            .IASetPrimitiveTopology(D3D_PRIMITIVE_TOPOLOGY_TRIANGLELIST);
        self.context.VSSetShader(&self.vertex_shader, None);
        if let Some(pq_lut) = &self.pq_lut {
            self.context
                .PSSetShaderResources(0, Some(&[Some(srv), Some(pq_lut.clone())]));
        } else {
            self.context.PSSetShaderResources(0, Some(&[Some(srv)]));
        }

        self.context.RSSetViewports(Some(&[self.luma_viewport]));
        self.context.PSSetShader(&self.luma_shader, None);
        self.context
            .OMSetRenderTargets(Some(&[Some(self.luma_target.clone())]), None);
        self.context.Draw(3, 0);

        self.context.RSSetViewports(Some(&[self.chroma_viewport]));
        self.context.PSSetShader(&self.chroma_shader, None);
        self.context
            .OMSetRenderTargets(Some(&[Some(self.chroma_target.clone())]), None);
        self.context.Draw(3, 0);

        let empty_srv: [Option<ID3D11ShaderResourceView>; 2] = [None, None];
        self.context.PSSetShaderResources(0, Some(&empty_srv));
        let empty_rtv: [Option<ID3D11RenderTargetView>; 1] = [None];
        self.context.OMSetRenderTargets(Some(&empty_rtv), None);
        Ok(())
    }

    unsafe fn convert_dirty(
        &self,
        source: &windows::Win32::Graphics::Direct3D11::ID3D11Texture2D,
        dirty_rects: &[windows::Win32::Foundation::RECT],
    ) -> Result<(), BackendError> {
        if self.compute_shader.is_some() || dirty_rects.is_empty() {
            return self.convert(source);
        }

        use windows::Win32::Graphics::Direct3D::D3D_PRIMITIVE_TOPOLOGY_TRIANGLELIST;
        use windows::Win32::Graphics::Direct3D11::{
            D3D11_VIEWPORT, ID3D11RenderTargetView, ID3D11Resource, ID3D11ShaderResourceView,
        };
        use windows::core::Interface;

        let source_resource: ID3D11Resource =
            source.cast().map_err(|err| BackendError::WindowsApi {
                func: "ID3D11Texture2D::cast<ID3D11Resource>(P010 dirty source)",
                message: err.to_string(),
            })?;
        let mut srv = None;
        self.device
            .CreateShaderResourceView(&source_resource, None, Some(&mut srv))
            .map_err(|err| BackendError::WindowsApi {
                func: "ID3D11Device::CreateShaderResourceView(P010 dirty source)",
                message: err.to_string(),
            })?;
        let srv = srv.ok_or_else(|| BackendError::WindowsApi {
            func: "CreateShaderResourceView(P010 dirty source)",
            message: "返回空 SRV".to_owned(),
        })?;

        self.context
            .IASetPrimitiveTopology(D3D_PRIMITIVE_TOPOLOGY_TRIANGLELIST);
        self.context.VSSetShader(&self.vertex_shader, None);
        if let Some(pq_lut) = &self.pq_lut {
            self.context
                .PSSetShaderResources(0, Some(&[Some(srv), Some(pq_lut.clone())]));
        } else {
            self.context.PSSetShaderResources(0, Some(&[Some(srv)]));
        }

        self.context.PSSetShader(&self.luma_shader, None);
        self.context
            .OMSetRenderTargets(Some(&[Some(self.luma_target.clone())]), None);
        for rect in dirty_rects {
            if let Some(viewport) = luma_dirty_viewport(rect) {
                self.context.RSSetViewports(Some(&[viewport]));
                self.context.Draw(3, 0);
            }
        }

        self.context.PSSetShader(&self.chroma_shader, None);
        self.context
            .OMSetRenderTargets(Some(&[Some(self.chroma_target.clone())]), None);
        for rect in dirty_rects {
            if let Some(viewport) = chroma_dirty_viewport(rect) {
                self.context.RSSetViewports(Some(&[viewport]));
                self.context.Draw(3, 0);
            }
        }

        let empty_srv: [Option<ID3D11ShaderResourceView>; 2] = [None, None];
        self.context.PSSetShaderResources(0, Some(&empty_srv));
        let empty_rtv: [Option<ID3D11RenderTargetView>; 1] = [None];
        self.context.OMSetRenderTargets(Some(&empty_rtv), None);

        fn luma_dirty_viewport(rect: &windows::Win32::Foundation::RECT) -> Option<D3D11_VIEWPORT> {
            let left = rect.left.max(0) as f32;
            let top = rect.top.max(0) as f32;
            let width = (rect.right - rect.left).max(0) as f32;
            let height = (rect.bottom - rect.top).max(0) as f32;
            if width <= 0.0 || height <= 0.0 {
                return None;
            }
            Some(D3D11_VIEWPORT {
                TopLeftX: left,
                TopLeftY: top,
                Width: width,
                Height: height,
                MinDepth: 0.0,
                MaxDepth: 1.0,
            })
        }

        fn chroma_dirty_viewport(
            rect: &windows::Win32::Foundation::RECT,
        ) -> Option<D3D11_VIEWPORT> {
            let left = rect.left.max(0) & !1;
            let top = rect.top.max(0) & !1;
            let right = (rect.right.max(left + 1) + 1) & !1;
            let bottom = (rect.bottom.max(top + 1) + 1) & !1;
            let width = ((right - left) / 2).max(0) as f32;
            let height = ((bottom - top) / 2).max(0) as f32;
            if width <= 0.0 || height <= 0.0 {
                return None;
            }
            Some(D3D11_VIEWPORT {
                TopLeftX: (left / 2) as f32,
                TopLeftY: (top / 2) as f32,
                Width: width,
                Height: height,
                MinDepth: 0.0,
                MaxDepth: 1.0,
            })
        }

        Ok(())
    }
}

#[cfg(windows)]
unsafe fn create_st2084_pq_lut_srv(
    device: &windows::Win32::Graphics::Direct3D11::ID3D11Device,
) -> Result<windows::Win32::Graphics::Direct3D11::ID3D11ShaderResourceView, BackendError> {
    use windows::Win32::Graphics::Direct3D11::{
        D3D11_BIND_SHADER_RESOURCE, D3D11_SUBRESOURCE_DATA, D3D11_TEXTURE1D_DESC,
        D3D11_USAGE_IMMUTABLE, ID3D11Resource,
    };
    use windows::Win32::Graphics::Dxgi::Common::DXGI_FORMAT_R16_UNORM;
    use windows::core::Interface;

    const LUT_SIZE: usize = 4096;
    let mut data = [0u16; LUT_SIZE];
    for (i, value) in data.iter_mut().enumerate() {
        let normalized_luminance = i as f64 / (LUT_SIZE - 1) as f64;
        let pq = st2084_pq_oetf_scalar(normalized_luminance);
        *value = (pq.clamp(0.0, 1.0) * 65535.0).round() as u16;
    }

    let desc = D3D11_TEXTURE1D_DESC {
        Width: LUT_SIZE as u32,
        MipLevels: 1,
        ArraySize: 1,
        Format: DXGI_FORMAT_R16_UNORM,
        Usage: D3D11_USAGE_IMMUTABLE,
        BindFlags: D3D11_BIND_SHADER_RESOURCE.0 as u32,
        CPUAccessFlags: 0,
        MiscFlags: 0,
    };
    let initial = D3D11_SUBRESOURCE_DATA {
        pSysMem: data.as_ptr() as *const c_void,
        SysMemPitch: (LUT_SIZE * std::mem::size_of::<u16>()) as u32,
        SysMemSlicePitch: 0,
    };
    let mut texture = None;
    device
        .CreateTexture1D(&desc, Some(&initial), Some(&mut texture))
        .map_err(|err| BackendError::WindowsApi {
            func: "ID3D11Device::CreateTexture1D(PQ LUT)",
            message: err.to_string(),
        })?;
    let texture = texture.ok_or_else(|| BackendError::WindowsApi {
        func: "CreateTexture1D(PQ LUT)",
        message: "返回空纹理".to_owned(),
    })?;
    let resource: ID3D11Resource = texture.cast().map_err(|err| BackendError::WindowsApi {
        func: "ID3D11Texture1D::cast<ID3D11Resource>(PQ LUT)",
        message: err.to_string(),
    })?;
    let mut srv = None;
    device
        .CreateShaderResourceView(&resource, None, Some(&mut srv))
        .map_err(|err| BackendError::WindowsApi {
            func: "ID3D11Device::CreateShaderResourceView(PQ LUT)",
            message: err.to_string(),
        })?;
    srv.ok_or_else(|| BackendError::WindowsApi {
        func: "CreateShaderResourceView(PQ LUT)",
        message: "返回空 SRV".to_owned(),
    })
}

fn st2084_pq_oetf_scalar(normalized_luminance: f64) -> f64 {
    let n = normalized_luminance.clamp(0.0, 1.0);
    let m1 = 2610.0 / 16_384.0;
    let m2 = 2523.0 / 32.0;
    let c1 = 3424.0 / 4096.0;
    let c2 = 2413.0 / 128.0;
    let c3 = 2392.0 / 128.0;
    let n_pow = n.powf(m1);
    ((c1 + c2 * n_pow) / (1.0 + c3 * n_pow)).powf(m2)
}

#[cfg(windows)]
const P010_CONVERT_HLSL: &str = r#"
Texture2D<float4> src_tex : register(t0);
Texture1D<float> transfer_lut : register(t1);
static const bool RR_FULL_RANGE = RR_FULL_RANGE_PLACEHOLDER;

float4 vs_main(uint id : SV_VertexID) : SV_Position {
    float2 pos[3] = {
        float2(-1.0,  1.0),
        float2( 3.0,  1.0),
        float2(-1.0, -3.0)
    };
    return float4(pos[id], 0.0, 1.0);
}

float pq_oetf(float normalized_luminance) {
    uint idx = (uint)(saturate(normalized_luminance) * 4095.0 + 0.5);
    return transfer_lut.Load(int2(idx, 0));
}

float3 rec709_linear_to_bt2020_linear(float3 rgb709) {
    return float3(
        0.6274039 * rgb709.r + 0.3292830 * rgb709.g + 0.0433131 * rgb709.b,
        0.0690973 * rgb709.r + 0.9195404 * rgb709.g + 0.0113623 * rgb709.b,
        0.0163914 * rgb709.r + 0.0880133 * rgb709.g + 0.8955953 * rgb709.b
    );
}

float3 sc_rgb_to_pq2020(float3 sc_rgb) {
    // Windows HDR desktop capture is scRGB linear with Rec.709/sRGB primaries
    // and 1.0 == 80 cd/m^2. Convert that display-referred signal to BT.2020
    // linear light, then encode each component with ST 2084 over 0..10000 nits.
    float3 bt2020_linear = max(rec709_linear_to_bt2020_linear(max(sc_rgb, 0.0)), 0.0);
    float3 normalized_nits = bt2020_linear * (80.0 / 10000.0);
    return float3(
        pq_oetf(normalized_nits.r),
        pq_oetf(normalized_nits.g),
        pq_oetf(normalized_nits.b)
    );
}

float3 pq2020_to_full_ycbcr(float3 rgb) {
    const float kr = 0.2627;
    const float kb = 0.0593;
    const float kg = 1.0 - kr - kb;
    float y = kr * rgb.r + kg * rgb.g + kb * rgb.b;
    float cb = (rgb.b - y) / (2.0 * (1.0 - kb));
    float cr = (rgb.r - y) / (2.0 * (1.0 - kr));
    return float3(
        saturate(y),
        saturate(cb + 0.5),
        saturate(cr + 0.5)
    );
}

float3 apply_yuv10_range(float3 ycbcr) {
    ycbcr = saturate(ycbcr);
    if (RR_FULL_RANGE) {
        return ycbcr;
    }
    float cb = ycbcr.y - 0.5;
    float cr = ycbcr.z - 0.5;
    return float3(
        (64.0 / 1023.0) + ycbcr.x * (876.0 / 1023.0),
        (512.0 / 1023.0) + cb * (896.0 / 1023.0),
        (512.0 / 1023.0) + cr * (896.0 / 1023.0)
    );
}

float3 load_ycbcr(uint2 pixel) {
    uint width;
    uint height;
    src_tex.GetDimensions(width, height);
    pixel = min(pixel, uint2(width - 1, height - 1));
    return apply_yuv10_range(pq2020_to_full_ycbcr(sc_rgb_to_pq2020(src_tex.Load(int3(pixel, 0)).rgb)));
}

float4 ps_luma(float4 pos : SV_Position) : SV_Target {
    return load_ycbcr(uint2(pos.xy)).xxxx;
}

float4 ps_chroma(float4 pos : SV_Position) : SV_Target {
    uint2 base_pixel = uint2(pos.xy) * 2;
    float3 c = load_ycbcr(base_pixel + uint2(1, 1));
    return float4(c.yz, 0.0, 1.0);
}

RWTexture2D<float> y_plane : register(u0);
RWTexture2D<float2> uv_plane : register(u1);

[numthreads(8, 8, 1)]
void cs_main(uint3 tid : SV_DispatchThreadID) {
    uint width;
    uint height;
    src_tex.GetDimensions(width, height);
    uint2 base_pixel = tid.xy * 2;
    if (base_pixel.x >= width || base_pixel.y >= height) {
        return;
    }

    float2 chroma_sum = float2(0.0, 0.0);
    float chroma_count = 0.0;
    [unroll]
    for (uint dy = 0; dy < 2; dy++) {
        [unroll]
        for (uint dx = 0; dx < 2; dx++) {
            uint2 pixel = base_pixel + uint2(dx, dy);
            if (pixel.x < width && pixel.y < height) {
                float3 ycbcr = load_ycbcr(pixel);
                y_plane[pixel] = ycbcr.x;
                chroma_sum += ycbcr.yz;
                chroma_count += 1.0;
            }
        }
    }
    uv_plane[tid.xy] = chroma_sum / max(chroma_count, 1.0);
}
"#;

#[cfg(windows)]
const P010_SDR10_CONVERT_HLSL: &str = r#"
Texture2D<float4> src_tex : register(t0);
static const bool RR_FULL_RANGE = RR_FULL_RANGE_PLACEHOLDER;

float4 vs_main(uint id : SV_VertexID) : SV_Position {
    float2 pos[3] = {
        float2(-1.0,  1.0),
        float2( 3.0,  1.0),
        float2(-1.0, -3.0)
    };
    return float4(pos[id], 0.0, 1.0);
}

float3 rgb_to_bt709_ycbcr(float3 rgb) {
    const float kr = 0.2126;
    const float kb = 0.0722;
    const float kg = 1.0 - kr - kb;
    float y = kr * rgb.r + kg * rgb.g + kb * rgb.b;
    float cb = (rgb.b - y) / (2.0 * (1.0 - kb));
    float cr = (rgb.r - y) / (2.0 * (1.0 - kr));
    return float3(saturate(y), saturate(cb + 0.5), saturate(cr + 0.5));
}

float bt709_oetf(float linear_value) {
    float v = max(linear_value, 0.0);
    return (v < 0.018) ? (4.5 * v) : (1.099 * pow(v, 0.45) - 0.099);
}

float3 sc_rgb_to_bt709_signal(float3 sc_rgb) {
    return saturate(float3(
        bt709_oetf(sc_rgb.r),
        bt709_oetf(sc_rgb.g),
        bt709_oetf(sc_rgb.b)
    ));
}

float3 apply_yuv10_range(float3 ycbcr) {
    ycbcr = saturate(ycbcr);
    if (RR_FULL_RANGE) {
        return ycbcr;
    }
    float cb = ycbcr.y - 0.5;
    float cr = ycbcr.z - 0.5;
    return float3(
        (64.0 / 1023.0) + ycbcr.x * (876.0 / 1023.0),
        (512.0 / 1023.0) + cb * (896.0 / 1023.0),
        (512.0 / 1023.0) + cr * (896.0 / 1023.0)
    );
}

float3 load_ycbcr(uint2 pixel) {
    uint width;
    uint height;
    src_tex.GetDimensions(width, height);
    pixel = min(pixel, uint2(width - 1, height - 1));
    return apply_yuv10_range(rgb_to_bt709_ycbcr(sc_rgb_to_bt709_signal(src_tex.Load(int3(pixel, 0)).rgb)));
}

float4 ps_luma(float4 pos : SV_Position) : SV_Target {
    return load_ycbcr(uint2(pos.xy)).xxxx;
}

float4 ps_chroma(float4 pos : SV_Position) : SV_Target {
    uint2 base_pixel = uint2(pos.xy) * 2;
    float3 c = load_ycbcr(base_pixel + uint2(1, 1));
    return float4(c.yz, 0.0, 1.0);
}

RWTexture2D<float> y_plane : register(u0);
RWTexture2D<float2> uv_plane : register(u1);

[numthreads(8, 8, 1)]
void cs_main(uint3 tid : SV_DispatchThreadID) {
    uint width;
    uint height;
    src_tex.GetDimensions(width, height);
    uint2 base_pixel = tid.xy * 2;
    if (base_pixel.x >= width || base_pixel.y >= height) {
        return;
    }

    float2 chroma_sum = float2(0.0, 0.0);
    float chroma_count = 0.0;
    [unroll]
    for (uint dy = 0; dy < 2; dy++) {
        [unroll]
        for (uint dx = 0; dx < 2; dx++) {
            uint2 pixel = base_pixel + uint2(dx, dy);
            if (pixel.x < width && pixel.y < height) {
                float3 ycbcr = load_ycbcr(pixel);
                y_plane[pixel] = ycbcr.x;
                chroma_sum += ycbcr.yz;
                chroma_count += 1.0;
            }
        }
    }
    uv_plane[tid.xy] = chroma_sum / max(chroma_count, 1.0);
}
"#;

#[cfg(windows)]
const P010_SDR_BT2020_CONVERT_HLSL: &str = r#"
Texture2D<float4> src_tex : register(t0);
static const bool RR_FULL_RANGE = RR_FULL_RANGE_PLACEHOLDER;

float4 vs_main(uint id : SV_VertexID) : SV_Position {
    float2 pos[3] = {
        float2(-1.0,  1.0),
        float2( 3.0,  1.0),
        float2(-1.0, -3.0)
    };
    return float4(pos[id], 0.0, 1.0);
}

float3 rec709_linear_to_bt2020_linear(float3 rgb709) {
    return float3(
        0.6274039 * rgb709.r + 0.3292830 * rgb709.g + 0.0433131 * rgb709.b,
        0.0690973 * rgb709.r + 0.9195404 * rgb709.g + 0.0113623 * rgb709.b,
        0.0163914 * rgb709.r + 0.0880133 * rgb709.g + 0.8955953 * rgb709.b
    );
}

float bt2020_oetf(float linear_value) {
    float v = max(linear_value, 0.0);
    return (v < 0.018) ? (4.5 * v) : (1.099 * pow(v, 0.45) - 0.099);
}

float3 sc_rgb_to_bt2020_signal(float3 sc_rgb) {
    float3 bt2020_linear = max(rec709_linear_to_bt2020_linear(max(sc_rgb, 0.0)), 0.0);
    return saturate(float3(
        bt2020_oetf(bt2020_linear.r),
        bt2020_oetf(bt2020_linear.g),
        bt2020_oetf(bt2020_linear.b)
    ));
}

float3 rgb_to_bt2020_ycbcr(float3 rgb) {
    const float kr = 0.2627;
    const float kb = 0.0593;
    const float kg = 1.0 - kr - kb;
    float y = kr * rgb.r + kg * rgb.g + kb * rgb.b;
    float cb = (rgb.b - y) / (2.0 * (1.0 - kb));
    float cr = (rgb.r - y) / (2.0 * (1.0 - kr));
    return float3(saturate(y), saturate(cb + 0.5), saturate(cr + 0.5));
}

float3 apply_yuv10_range(float3 ycbcr) {
    ycbcr = saturate(ycbcr);
    if (RR_FULL_RANGE) {
        return ycbcr;
    }
    float cb = ycbcr.y - 0.5;
    float cr = ycbcr.z - 0.5;
    return float3(
        (64.0 / 1023.0) + ycbcr.x * (876.0 / 1023.0),
        (512.0 / 1023.0) + cb * (896.0 / 1023.0),
        (512.0 / 1023.0) + cr * (896.0 / 1023.0)
    );
}

float3 load_ycbcr(uint2 pixel) {
    uint width;
    uint height;
    src_tex.GetDimensions(width, height);
    pixel = min(pixel, uint2(width - 1, height - 1));
    return apply_yuv10_range(rgb_to_bt2020_ycbcr(sc_rgb_to_bt2020_signal(src_tex.Load(int3(pixel, 0)).rgb)));
}

float4 ps_luma(float4 pos : SV_Position) : SV_Target {
    return load_ycbcr(uint2(pos.xy)).xxxx;
}

float4 ps_chroma(float4 pos : SV_Position) : SV_Target {
    uint2 base_pixel = uint2(pos.xy) * 2;
    float3 c = load_ycbcr(base_pixel + uint2(1, 1));
    return float4(c.yz, 0.0, 1.0);
}

RWTexture2D<float> y_plane : register(u0);
RWTexture2D<float2> uv_plane : register(u1);

[numthreads(8, 8, 1)]
void cs_main(uint3 tid : SV_DispatchThreadID) {
    uint width;
    uint height;
    src_tex.GetDimensions(width, height);
    uint2 base_pixel = tid.xy * 2;
    if (base_pixel.x >= width || base_pixel.y >= height) {
        return;
    }

    float2 chroma_sum = float2(0.0, 0.0);
    float chroma_count = 0.0;
    [unroll]
    for (uint dy = 0; dy < 2; dy++) {
        [unroll]
        for (uint dx = 0; dx < 2; dx++) {
            uint2 pixel = base_pixel + uint2(dx, dy);
            if (pixel.x < width && pixel.y < height) {
                float3 ycbcr = load_ycbcr(pixel);
                y_plane[pixel] = ycbcr.x;
                chroma_sum += ycbcr.yz;
                chroma_count += 1.0;
            }
        }
    }
    uv_plane[tid.xy] = chroma_sum / max(chroma_count, 1.0);
}
"#;

#[cfg(windows)]
const NV12_CONVERT_HLSL: &str = r#"
Texture2D<float4> src_tex : register(t0);
RWTexture2D<float> y_plane : register(u0);
RWTexture2D<float2> uv_plane : register(u1);
static const bool RR_FULL_RANGE = RR_FULL_RANGE_PLACEHOLDER;

float3 rgb_to_bt709_full_ycbcr(float3 rgb) {
    const float kr = 0.2126;
    const float kb = 0.0722;
    const float kg = 1.0 - kr - kb;
    float y = kr * rgb.r + kg * rgb.g + kb * rgb.b;
    float cb = (rgb.b - y) / (2.0 * (1.0 - kb));
    float cr = (rgb.r - y) / (2.0 * (1.0 - kr));
    return float3(
        saturate(y),
        saturate(cb + 0.5),
        saturate(cr + 0.5)
    );
}

float3 apply_yuv8_range(float3 ycbcr) {
    ycbcr = saturate(ycbcr);
    if (RR_FULL_RANGE) {
        return ycbcr;
    }
    float cb = ycbcr.y - 0.5;
    float cr = ycbcr.z - 0.5;
    return float3(
        (16.0 / 255.0) + ycbcr.x * (219.0 / 255.0),
        (128.0 / 255.0) + cb * (224.0 / 255.0),
        (128.0 / 255.0) + cr * (224.0 / 255.0)
    );
}

float3 load_ycbcr(uint2 pixel) {
    uint width;
    uint height;
    src_tex.GetDimensions(width, height);
    pixel = min(pixel, uint2(width - 1, height - 1));
    return apply_yuv8_range(rgb_to_bt709_full_ycbcr(saturate(src_tex.Load(int3(pixel, 0)).rgb)));
}

[numthreads(8, 8, 1)]
void cs_main(uint3 tid : SV_DispatchThreadID) {
    uint width;
    uint height;
    src_tex.GetDimensions(width, height);
    uint2 base_pixel = tid.xy * 2;
    if (base_pixel.x >= width || base_pixel.y >= height) {
        return;
    }

    float2 chroma_sum = float2(0.0, 0.0);
    float chroma_count = 0.0;
    [unroll]
    for (uint dy = 0; dy < 2; dy++) {
        [unroll]
        for (uint dx = 0; dx < 2; dx++) {
            uint2 pixel = base_pixel + uint2(dx, dy);
            if (pixel.x < width && pixel.y < height) {
                float3 ycbcr = load_ycbcr(pixel);
                y_plane[pixel] = ycbcr.x;
                chroma_sum += ycbcr.yz;
                chroma_count += 1.0;
            }
        }
    }
    uv_plane[tid.xy] = chroma_sum / max(chroma_count, 1.0);
}
"#;

#[cfg(windows)]
const NV12_BT2020_CONVERT_HLSL: &str = r#"
Texture2D<float4> src_tex : register(t0);
RWTexture2D<float> y_plane : register(u0);
RWTexture2D<float2> uv_plane : register(u1);
static const bool RR_FULL_RANGE = RR_FULL_RANGE_PLACEHOLDER;

float3 rgb_to_bt2020_ycbcr(float3 rgb) {
    const float kr = 0.2627;
    const float kb = 0.0593;
    const float kg = 1.0 - kr - kb;
    float y = kr * rgb.r + kg * rgb.g + kb * rgb.b;
    float cb = (rgb.b - y) / (2.0 * (1.0 - kb));
    float cr = (rgb.r - y) / (2.0 * (1.0 - kr));
    return float3(saturate(y), saturate(cb + 0.5), saturate(cr + 0.5));
}

float3 apply_yuv8_range(float3 ycbcr) {
    ycbcr = saturate(ycbcr);
    if (RR_FULL_RANGE) {
        return ycbcr;
    }
    float cb = ycbcr.y - 0.5;
    float cr = ycbcr.z - 0.5;
    return float3(
        (16.0 / 255.0) + ycbcr.x * (219.0 / 255.0),
        (128.0 / 255.0) + cb * (224.0 / 255.0),
        (128.0 / 255.0) + cr * (224.0 / 255.0)
    );
}

float3 load_ycbcr(uint2 pixel) {
    uint width;
    uint height;
    src_tex.GetDimensions(width, height);
    pixel = min(pixel, uint2(width - 1, height - 1));
    return apply_yuv8_range(rgb_to_bt2020_ycbcr(saturate(src_tex.Load(int3(pixel, 0)).rgb)));
}

[numthreads(8, 8, 1)]
void cs_main(uint3 tid : SV_DispatchThreadID) {
    uint width;
    uint height;
    src_tex.GetDimensions(width, height);
    uint2 base_pixel = tid.xy * 2;
    if (base_pixel.x >= width || base_pixel.y >= height) {
        return;
    }

    float2 chroma_sum = float2(0.0, 0.0);
    float chroma_count = 0.0;
    [unroll]
    for (uint dy = 0; dy < 2; dy++) {
        [unroll]
        for (uint dx = 0; dx < 2; dx++) {
            uint2 pixel = base_pixel + uint2(dx, dy);
            if (pixel.x < width && pixel.y < height) {
                float3 ycbcr = load_ycbcr(pixel);
                y_plane[pixel] = ycbcr.x;
                chroma_sum += ycbcr.yz;
                chroma_count += 1.0;
            }
        }
    }
    uv_plane[tid.xy] = chroma_sum / max(chroma_count, 1.0);
}
"#;

#[cfg(windows)]
const YUY2_CONVERT_HLSL: &str = r#"
Texture2D<float4> src_tex : register(t0);
RWTexture2D<uint> yuy2_tex : register(u0);
static const bool RR_FULL_RANGE = RR_FULL_RANGE_PLACEHOLDER;

float3 rgb_to_bt709_full_ycbcr(float3 rgb) {
    const float kr = 0.2126;
    const float kb = 0.0722;
    const float kg = 1.0 - kr - kb;
    float y = kr * rgb.r + kg * rgb.g + kb * rgb.b;
    float cb = (rgb.b - y) / (2.0 * (1.0 - kb));
    float cr = (rgb.r - y) / (2.0 * (1.0 - kr));
    return float3(saturate(y), saturate(cb + 0.5), saturate(cr + 0.5));
}

float3 apply_yuv8_range(float3 ycbcr) {
    ycbcr = saturate(ycbcr);
    if (RR_FULL_RANGE) {
        return ycbcr;
    }
    float cb = ycbcr.y - 0.5;
    float cr = ycbcr.z - 0.5;
    return float3(
        (16.0 / 255.0) + ycbcr.x * (219.0 / 255.0),
        (128.0 / 255.0) + cb * (224.0 / 255.0),
        (128.0 / 255.0) + cr * (224.0 / 255.0)
    );
}

float3 load_ycbcr(uint2 pixel) {
    uint width;
    uint height;
    src_tex.GetDimensions(width, height);
    pixel = min(pixel, uint2(width - 1, height - 1));
    return apply_yuv8_range(rgb_to_bt709_full_ycbcr(saturate(src_tex.Load(int3(pixel, 0)).rgb)));
}

uint u8(float value) {
    return (uint)round(saturate(value) * 255.0);
}

[numthreads(16, 8, 1)]
void cs_main(uint3 tid : SV_DispatchThreadID) {
    uint width;
    uint height;
    src_tex.GetDimensions(width, height);
    uint2 base_pixel = uint2(tid.x * 2, tid.y);
    if (base_pixel.x >= width || base_pixel.y >= height) {
        return;
    }
    float3 c0 = load_ycbcr(base_pixel);
    float3 c1 = load_ycbcr(uint2(min(base_pixel.x + 1, width - 1), base_pixel.y));
    float2 uv = (c0.yz + c1.yz) * 0.5;
    uint y0 = u8(c0.x);
    uint u = u8(uv.x);
    uint y1 = u8(c1.x);
    uint v = u8(uv.y);
    yuy2_tex[tid.xy] = y0 | (u << 8) | (y1 << 16) | (v << 24);
}
"#;

#[cfg(windows)]
const YUY2_BT2020_CONVERT_HLSL: &str = r#"
Texture2D<float4> src_tex : register(t0);
RWTexture2D<uint> yuy2_tex : register(u0);
static const bool RR_FULL_RANGE = RR_FULL_RANGE_PLACEHOLDER;

float3 rgb_to_bt2020_ycbcr(float3 rgb) {
    const float kr = 0.2627;
    const float kb = 0.0593;
    const float kg = 1.0 - kr - kb;
    float y = kr * rgb.r + kg * rgb.g + kb * rgb.b;
    float cb = (rgb.b - y) / (2.0 * (1.0 - kb));
    float cr = (rgb.r - y) / (2.0 * (1.0 - kr));
    return float3(saturate(y), saturate(cb + 0.5), saturate(cr + 0.5));
}

float3 apply_yuv8_range(float3 ycbcr) {
    ycbcr = saturate(ycbcr);
    if (RR_FULL_RANGE) {
        return ycbcr;
    }
    float cb = ycbcr.y - 0.5;
    float cr = ycbcr.z - 0.5;
    return float3(
        (16.0 / 255.0) + ycbcr.x * (219.0 / 255.0),
        (128.0 / 255.0) + cb * (224.0 / 255.0),
        (128.0 / 255.0) + cr * (224.0 / 255.0)
    );
}

float3 load_ycbcr(uint2 pixel) {
    uint width;
    uint height;
    src_tex.GetDimensions(width, height);
    pixel = min(pixel, uint2(width - 1, height - 1));
    return apply_yuv8_range(rgb_to_bt2020_ycbcr(saturate(src_tex.Load(int3(pixel, 0)).rgb)));
}

uint u8(float value) {
    return (uint)round(saturate(value) * 255.0);
}

[numthreads(16, 8, 1)]
void cs_main(uint3 tid : SV_DispatchThreadID) {
    uint width;
    uint height;
    src_tex.GetDimensions(width, height);
    uint2 base_pixel = uint2(tid.x * 2, tid.y);
    if (base_pixel.x >= width || base_pixel.y >= height) {
        return;
    }
    float3 c0 = load_ycbcr(base_pixel);
    float3 c1 = load_ycbcr(uint2(min(base_pixel.x + 1, width - 1), base_pixel.y));
    float2 uv = (c0.yz + c1.yz) * 0.5;
    uint y0 = u8(c0.x);
    uint u = u8(uv.x);
    uint y1 = u8(c1.x);
    uint v = u8(uv.y);
    yuy2_tex[tid.xy] = y0 | (u << 8) | (y1 << 16) | (v << 24);
}
"#;

#[cfg(windows)]
const AYUV_CONVERT_HLSL: &str = r#"
Texture2D<float4> src_tex : register(t0);
RWTexture2D<uint> ayuv_tex : register(u0);
static const bool RR_FULL_RANGE = RR_FULL_RANGE_PLACEHOLDER;

float3 rgb_to_bt709_full_ycbcr(float3 rgb) {
    const float kr = 0.2126;
    const float kb = 0.0722;
    const float kg = 1.0 - kr - kb;
    float y = kr * rgb.r + kg * rgb.g + kb * rgb.b;
    float cb = (rgb.b - y) / (2.0 * (1.0 - kb));
    float cr = (rgb.r - y) / (2.0 * (1.0 - kr));
    return float3(saturate(y), saturate(cb + 0.5), saturate(cr + 0.5));
}

float3 apply_yuv8_range(float3 ycbcr) {
    ycbcr = saturate(ycbcr);
    if (RR_FULL_RANGE) {
        return ycbcr;
    }
    float cb = ycbcr.y - 0.5;
    float cr = ycbcr.z - 0.5;
    return float3(
        (16.0 / 255.0) + ycbcr.x * (219.0 / 255.0),
        (128.0 / 255.0) + cb * (224.0 / 255.0),
        (128.0 / 255.0) + cr * (224.0 / 255.0)
    );
}

uint u8(float value) {
    return (uint)round(saturate(value) * 255.0);
}

[numthreads(16, 8, 1)]
void cs_main(uint3 tid : SV_DispatchThreadID) {
    uint width;
    uint height;
    src_tex.GetDimensions(width, height);
    if (tid.x >= width || tid.y >= height) {
        return;
    }
    float3 ycbcr = apply_yuv8_range(rgb_to_bt709_full_ycbcr(saturate(src_tex.Load(int3(tid.xy, 0)).rgb)));
    uint y = u8(ycbcr.x);
    uint u = u8(ycbcr.y);
    uint v = u8(ycbcr.z);
    // DXGI AYUV 的 R32_UINT UAV 视图按 V/U/Y/A 字节直写。
    ayuv_tex[tid.xy] = v | (u << 8) | (y << 16) | (255u << 24);
}
"#;

#[cfg(windows)]
const AYUV_BT2020_CONVERT_HLSL: &str = r#"
Texture2D<float4> src_tex : register(t0);
RWTexture2D<uint> ayuv_tex : register(u0);
static const bool RR_FULL_RANGE = RR_FULL_RANGE_PLACEHOLDER;

float3 rgb_to_bt2020_ycbcr(float3 rgb) {
    const float kr = 0.2627;
    const float kb = 0.0593;
    const float kg = 1.0 - kr - kb;
    float y = kr * rgb.r + kg * rgb.g + kb * rgb.b;
    float cb = (rgb.b - y) / (2.0 * (1.0 - kb));
    float cr = (rgb.r - y) / (2.0 * (1.0 - kr));
    return float3(saturate(y), saturate(cb + 0.5), saturate(cr + 0.5));
}

float3 apply_yuv8_range(float3 ycbcr) {
    ycbcr = saturate(ycbcr);
    if (RR_FULL_RANGE) {
        return ycbcr;
    }
    float cb = ycbcr.y - 0.5;
    float cr = ycbcr.z - 0.5;
    return float3(
        (16.0 / 255.0) + ycbcr.x * (219.0 / 255.0),
        (128.0 / 255.0) + cb * (224.0 / 255.0),
        (128.0 / 255.0) + cr * (224.0 / 255.0)
    );
}

uint u8(float value) {
    return (uint)round(saturate(value) * 255.0);
}

[numthreads(16, 8, 1)]
void cs_main(uint3 tid : SV_DispatchThreadID) {
    uint width;
    uint height;
    src_tex.GetDimensions(width, height);
    if (tid.x >= width || tid.y >= height) {
        return;
    }
    float3 ycbcr = apply_yuv8_range(rgb_to_bt2020_ycbcr(saturate(src_tex.Load(int3(tid.xy, 0)).rgb)));
    uint y = u8(ycbcr.x);
    uint u = u8(ycbcr.y);
    uint v = u8(ycbcr.z);
    ayuv_tex[tid.xy] = v | (u << 8) | (y << 16) | (255u << 24);
}
"#;

#[cfg(windows)]
const Y210_CONVERT_HLSL: &str = r#"
Texture2D<float4> src_tex : register(t0);
RWTexture2D<uint4> y210_tex : register(u0);
static const bool RR_FULL_RANGE = RR_FULL_RANGE_PLACEHOLDER;

float pq_oetf(float normalized_luminance) {
    const float m1 = 2610.0 / 16384.0;
    const float m2 = 2523.0 / 32.0;
    const float c1 = 3424.0 / 4096.0;
    const float c2 = 2413.0 / 128.0;
    const float c3 = 2392.0 / 128.0;
    float n = saturate(normalized_luminance);
    float p = pow(n, m1);
    return pow((c1 + c2 * p) / (1.0 + c3 * p), m2);
}

float3 rec709_linear_to_bt2020_linear(float3 rgb709) {
    return float3(
        0.6274039 * rgb709.r + 0.3292830 * rgb709.g + 0.0433131 * rgb709.b,
        0.0690973 * rgb709.r + 0.9195404 * rgb709.g + 0.0113623 * rgb709.b,
        0.0163914 * rgb709.r + 0.0880133 * rgb709.g + 0.8955953 * rgb709.b
    );
}

float3 sc_rgb_to_pq2020(float3 sc_rgb) {
    float3 bt2020_linear = max(rec709_linear_to_bt2020_linear(max(sc_rgb, 0.0)), 0.0);
    float3 normalized_nits = bt2020_linear * (80.0 / 10000.0);
    return float3(pq_oetf(normalized_nits.r), pq_oetf(normalized_nits.g), pq_oetf(normalized_nits.b));
}

float3 pq2020_to_full_ycbcr(float3 rgb) {
    const float kr = 0.2627;
    const float kb = 0.0593;
    const float kg = 1.0 - kr - kb;
    float y = kr * rgb.r + kg * rgb.g + kb * rgb.b;
    float cb = (rgb.b - y) / (2.0 * (1.0 - kb));
    float cr = (rgb.r - y) / (2.0 * (1.0 - kr));
    return float3(saturate(y), saturate(cb + 0.5), saturate(cr + 0.5));
}

float3 apply_yuv10_range(float3 ycbcr) {
    ycbcr = saturate(ycbcr);
    if (RR_FULL_RANGE) {
        return ycbcr;
    }
    float cb = ycbcr.y - 0.5;
    float cr = ycbcr.z - 0.5;
    return float3(
        (64.0 / 1023.0) + ycbcr.x * (876.0 / 1023.0),
        (512.0 / 1023.0) + cb * (896.0 / 1023.0),
        (512.0 / 1023.0) + cr * (896.0 / 1023.0)
    );
}

float3 load_ycbcr(uint2 pixel) {
    uint width;
    uint height;
    src_tex.GetDimensions(width, height);
    pixel = min(pixel, uint2(width - 1, height - 1));
    return apply_yuv10_range(pq2020_to_full_ycbcr(sc_rgb_to_pq2020(src_tex.Load(int3(pixel, 0)).rgb)));
}

uint u10_to_msb16(float value) {
    return ((uint)round(saturate(value) * 1023.0)) << 6;
}

[numthreads(16, 8, 1)]
void cs_main(uint3 tid : SV_DispatchThreadID) {
    uint width;
    uint height;
    src_tex.GetDimensions(width, height);
    uint2 base_pixel = uint2(tid.x * 2, tid.y);
    if (base_pixel.x >= width || base_pixel.y >= height) {
        return;
    }
    float3 c0 = load_ycbcr(base_pixel);
    float3 c1 = load_ycbcr(uint2(min(base_pixel.x + 1, width - 1), base_pixel.y));
    float2 uv = (c0.yz + c1.yz) * 0.5;
    y210_tex[tid.xy] = uint4(u10_to_msb16(c0.x), u10_to_msb16(uv.x), u10_to_msb16(c1.x), u10_to_msb16(uv.y));
}
"#;

#[cfg(windows)]
const Y410_CONVERT_HLSL: &str = r#"
Texture2D<float4> src_tex : register(t0);
RWTexture2D<uint> y410_tex : register(u0);
static const bool RR_FULL_RANGE = RR_FULL_RANGE_PLACEHOLDER;

float pq_oetf(float normalized_luminance) {
    const float m1 = 2610.0 / 16384.0;
    const float m2 = 2523.0 / 32.0;
    const float c1 = 3424.0 / 4096.0;
    const float c2 = 2413.0 / 128.0;
    const float c3 = 2392.0 / 128.0;
    float n = saturate(normalized_luminance);
    float p = pow(n, m1);
    return pow((c1 + c2 * p) / (1.0 + c3 * p), m2);
}

float3 rec709_linear_to_bt2020_linear(float3 rgb709) {
    return float3(
        0.6274039 * rgb709.r + 0.3292830 * rgb709.g + 0.0433131 * rgb709.b,
        0.0690973 * rgb709.r + 0.9195404 * rgb709.g + 0.0113623 * rgb709.b,
        0.0163914 * rgb709.r + 0.0880133 * rgb709.g + 0.8955953 * rgb709.b
    );
}

float3 sc_rgb_to_pq2020(float3 sc_rgb) {
    float3 bt2020_linear = max(rec709_linear_to_bt2020_linear(max(sc_rgb, 0.0)), 0.0);
    float3 normalized_nits = bt2020_linear * (80.0 / 10000.0);
    return float3(pq_oetf(normalized_nits.r), pq_oetf(normalized_nits.g), pq_oetf(normalized_nits.b));
}

float3 pq2020_to_full_ycbcr(float3 rgb) {
    const float kr = 0.2627;
    const float kb = 0.0593;
    const float kg = 1.0 - kr - kb;
    float y = kr * rgb.r + kg * rgb.g + kb * rgb.b;
    float cb = (rgb.b - y) / (2.0 * (1.0 - kb));
    float cr = (rgb.r - y) / (2.0 * (1.0 - kr));
    return float3(saturate(y), saturate(cb + 0.5), saturate(cr + 0.5));
}

float3 apply_yuv10_range(float3 ycbcr) {
    ycbcr = saturate(ycbcr);
    if (RR_FULL_RANGE) {
        return ycbcr;
    }
    float cb = ycbcr.y - 0.5;
    float cr = ycbcr.z - 0.5;
    return float3(
        (64.0 / 1023.0) + ycbcr.x * (876.0 / 1023.0),
        (512.0 / 1023.0) + cb * (896.0 / 1023.0),
        (512.0 / 1023.0) + cr * (896.0 / 1023.0)
    );
}

uint u10(float value) {
    return (uint)round(saturate(value) * 1023.0);
}

[numthreads(16, 8, 1)]
void cs_main(uint3 tid : SV_DispatchThreadID) {
    uint width;
    uint height;
    src_tex.GetDimensions(width, height);
    if (tid.x >= width || tid.y >= height) {
        return;
    }
    float3 ycbcr = apply_yuv10_range(pq2020_to_full_ycbcr(sc_rgb_to_pq2020(src_tex.Load(int3(tid.xy, 0)).rgb)));
    uint y = u10(ycbcr.x);
    uint u = u10(ycbcr.y);
    uint v = u10(ycbcr.z);
    // DXGI Y410 的 R32_UINT UAV 视图按 U/Y/V/A 位域直写。
    y410_tex[tid.xy] = u | (y << 10) | (v << 20) | (3u << 30);
}
"#;

#[cfg(windows)]
const Y210_SDR10_CONVERT_HLSL: &str = r#"
Texture2D<float4> src_tex : register(t0);
RWTexture2D<uint4> y210_tex : register(u0);
static const bool RR_FULL_RANGE = RR_FULL_RANGE_PLACEHOLDER;

float3 rgb_to_bt709_ycbcr(float3 rgb) {
    const float kr = 0.2126;
    const float kb = 0.0722;
    const float kg = 1.0 - kr - kb;
    float y = kr * rgb.r + kg * rgb.g + kb * rgb.b;
    float cb = (rgb.b - y) / (2.0 * (1.0 - kb));
    float cr = (rgb.r - y) / (2.0 * (1.0 - kr));
    return float3(saturate(y), saturate(cb + 0.5), saturate(cr + 0.5));
}

float bt709_oetf(float linear_value) {
    float v = max(linear_value, 0.0);
    return (v < 0.018) ? (4.5 * v) : (1.099 * pow(v, 0.45) - 0.099);
}

float3 sc_rgb_to_bt709_signal(float3 sc_rgb) {
    return saturate(float3(
        bt709_oetf(sc_rgb.r),
        bt709_oetf(sc_rgb.g),
        bt709_oetf(sc_rgb.b)
    ));
}

float3 apply_yuv10_range(float3 ycbcr) {
    ycbcr = saturate(ycbcr);
    if (RR_FULL_RANGE) {
        return ycbcr;
    }
    float cb = ycbcr.y - 0.5;
    float cr = ycbcr.z - 0.5;
    return float3(
        (64.0 / 1023.0) + ycbcr.x * (876.0 / 1023.0),
        (512.0 / 1023.0) + cb * (896.0 / 1023.0),
        (512.0 / 1023.0) + cr * (896.0 / 1023.0)
    );
}

float3 load_ycbcr(uint2 pixel) {
    uint width;
    uint height;
    src_tex.GetDimensions(width, height);
    pixel = min(pixel, uint2(width - 1, height - 1));
    return apply_yuv10_range(rgb_to_bt709_ycbcr(sc_rgb_to_bt709_signal(src_tex.Load(int3(pixel, 0)).rgb)));
}

uint u10_to_msb16(float value) {
    return ((uint)round(saturate(value) * 1023.0)) << 6;
}

[numthreads(16, 8, 1)]
void cs_main(uint3 tid : SV_DispatchThreadID) {
    uint width;
    uint height;
    src_tex.GetDimensions(width, height);
    uint2 base_pixel = uint2(tid.x * 2, tid.y);
    if (base_pixel.x >= width || base_pixel.y >= height) {
        return;
    }
    float3 c0 = load_ycbcr(base_pixel);
    float3 c1 = load_ycbcr(uint2(min(base_pixel.x + 1, width - 1), base_pixel.y));
    float2 uv = (c0.yz + c1.yz) * 0.5;
    y210_tex[tid.xy] = uint4(u10_to_msb16(c0.x), u10_to_msb16(uv.x), u10_to_msb16(c1.x), u10_to_msb16(uv.y));
}
"#;

#[cfg(windows)]
const Y210_SDR_BT2020_CONVERT_HLSL: &str = r#"
Texture2D<float4> src_tex : register(t0);
RWTexture2D<uint4> y210_tex : register(u0);
static const bool RR_FULL_RANGE = RR_FULL_RANGE_PLACEHOLDER;

float3 rec709_linear_to_bt2020_linear(float3 rgb709) {
    return float3(
        0.6274039 * rgb709.r + 0.3292830 * rgb709.g + 0.0433131 * rgb709.b,
        0.0690973 * rgb709.r + 0.9195404 * rgb709.g + 0.0113623 * rgb709.b,
        0.0163914 * rgb709.r + 0.0880133 * rgb709.g + 0.8955953 * rgb709.b
    );
}

float bt2020_oetf(float linear_value) {
    float v = max(linear_value, 0.0);
    return (v < 0.018) ? (4.5 * v) : (1.099 * pow(v, 0.45) - 0.099);
}

float3 sc_rgb_to_bt2020_signal(float3 sc_rgb) {
    float3 bt2020_linear = max(rec709_linear_to_bt2020_linear(max(sc_rgb, 0.0)), 0.0);
    return saturate(float3(
        bt2020_oetf(bt2020_linear.r),
        bt2020_oetf(bt2020_linear.g),
        bt2020_oetf(bt2020_linear.b)
    ));
}

float3 rgb_to_bt2020_ycbcr(float3 rgb) {
    const float kr = 0.2627;
    const float kb = 0.0593;
    const float kg = 1.0 - kr - kb;
    float y = kr * rgb.r + kg * rgb.g + kb * rgb.b;
    float cb = (rgb.b - y) / (2.0 * (1.0 - kb));
    float cr = (rgb.r - y) / (2.0 * (1.0 - kr));
    return float3(saturate(y), saturate(cb + 0.5), saturate(cr + 0.5));
}

float3 apply_yuv10_range(float3 ycbcr) {
    ycbcr = saturate(ycbcr);
    if (RR_FULL_RANGE) {
        return ycbcr;
    }
    float cb = ycbcr.y - 0.5;
    float cr = ycbcr.z - 0.5;
    return float3(
        (64.0 / 1023.0) + ycbcr.x * (876.0 / 1023.0),
        (512.0 / 1023.0) + cb * (896.0 / 1023.0),
        (512.0 / 1023.0) + cr * (896.0 / 1023.0)
    );
}

float3 load_ycbcr(uint2 pixel) {
    uint width;
    uint height;
    src_tex.GetDimensions(width, height);
    pixel = min(pixel, uint2(width - 1, height - 1));
    return apply_yuv10_range(rgb_to_bt2020_ycbcr(sc_rgb_to_bt2020_signal(src_tex.Load(int3(pixel, 0)).rgb)));
}

uint u10_to_msb16(float value) {
    return ((uint)round(saturate(value) * 1023.0)) << 6;
}

[numthreads(16, 8, 1)]
void cs_main(uint3 tid : SV_DispatchThreadID) {
    uint width;
    uint height;
    src_tex.GetDimensions(width, height);
    uint2 base_pixel = uint2(tid.x * 2, tid.y);
    if (base_pixel.x >= width || base_pixel.y >= height) {
        return;
    }
    float3 c0 = load_ycbcr(base_pixel);
    float3 c1 = load_ycbcr(uint2(min(base_pixel.x + 1, width - 1), base_pixel.y));
    float2 uv = (c0.yz + c1.yz) * 0.5;
    y210_tex[tid.xy] = uint4(u10_to_msb16(c0.x), u10_to_msb16(uv.x), u10_to_msb16(c1.x), u10_to_msb16(uv.y));
}
"#;

#[cfg(windows)]
const Y410_SDR10_CONVERT_HLSL: &str = r#"
Texture2D<float4> src_tex : register(t0);
RWTexture2D<uint> y410_tex : register(u0);
static const bool RR_FULL_RANGE = RR_FULL_RANGE_PLACEHOLDER;

float3 rgb_to_bt709_ycbcr(float3 rgb) {
    const float kr = 0.2126;
    const float kb = 0.0722;
    const float kg = 1.0 - kr - kb;
    float y = kr * rgb.r + kg * rgb.g + kb * rgb.b;
    float cb = (rgb.b - y) / (2.0 * (1.0 - kb));
    float cr = (rgb.r - y) / (2.0 * (1.0 - kr));
    return float3(saturate(y), saturate(cb + 0.5), saturate(cr + 0.5));
}

float bt709_oetf(float linear_value) {
    float v = max(linear_value, 0.0);
    return (v < 0.018) ? (4.5 * v) : (1.099 * pow(v, 0.45) - 0.099);
}

float3 sc_rgb_to_bt709_signal(float3 sc_rgb) {
    return saturate(float3(
        bt709_oetf(sc_rgb.r),
        bt709_oetf(sc_rgb.g),
        bt709_oetf(sc_rgb.b)
    ));
}

float3 apply_yuv10_range(float3 ycbcr) {
    ycbcr = saturate(ycbcr);
    if (RR_FULL_RANGE) {
        return ycbcr;
    }
    float cb = ycbcr.y - 0.5;
    float cr = ycbcr.z - 0.5;
    return float3(
        (64.0 / 1023.0) + ycbcr.x * (876.0 / 1023.0),
        (512.0 / 1023.0) + cb * (896.0 / 1023.0),
        (512.0 / 1023.0) + cr * (896.0 / 1023.0)
    );
}

uint u10(float value) {
    return (uint)round(saturate(value) * 1023.0);
}

[numthreads(16, 8, 1)]
void cs_main(uint3 tid : SV_DispatchThreadID) {
    uint width;
    uint height;
    src_tex.GetDimensions(width, height);
    if (tid.x >= width || tid.y >= height) {
        return;
    }
    float3 ycbcr = apply_yuv10_range(rgb_to_bt709_ycbcr(sc_rgb_to_bt709_signal(src_tex.Load(int3(tid.xy, 0)).rgb)));
    uint y = u10(ycbcr.x);
    uint u = u10(ycbcr.y);
    uint v = u10(ycbcr.z);
    y410_tex[tid.xy] = u | (y << 10) | (v << 20) | (3u << 30);
}
"#;

#[cfg(windows)]
const Y410_SDR_BT2020_CONVERT_HLSL: &str = r#"
Texture2D<float4> src_tex : register(t0);
RWTexture2D<uint> y410_tex : register(u0);
static const bool RR_FULL_RANGE = RR_FULL_RANGE_PLACEHOLDER;

float3 rec709_linear_to_bt2020_linear(float3 rgb709) {
    return float3(
        0.6274039 * rgb709.r + 0.3292830 * rgb709.g + 0.0433131 * rgb709.b,
        0.0690973 * rgb709.r + 0.9195404 * rgb709.g + 0.0113623 * rgb709.b,
        0.0163914 * rgb709.r + 0.0880133 * rgb709.g + 0.8955953 * rgb709.b
    );
}

float bt2020_oetf(float linear_value) {
    float v = max(linear_value, 0.0);
    return (v < 0.018) ? (4.5 * v) : (1.099 * pow(v, 0.45) - 0.099);
}

float3 sc_rgb_to_bt2020_signal(float3 sc_rgb) {
    float3 bt2020_linear = max(rec709_linear_to_bt2020_linear(max(sc_rgb, 0.0)), 0.0);
    return saturate(float3(
        bt2020_oetf(bt2020_linear.r),
        bt2020_oetf(bt2020_linear.g),
        bt2020_oetf(bt2020_linear.b)
    ));
}

float3 rgb_to_bt2020_ycbcr(float3 rgb) {
    const float kr = 0.2627;
    const float kb = 0.0593;
    const float kg = 1.0 - kr - kb;
    float y = kr * rgb.r + kg * rgb.g + kb * rgb.b;
    float cb = (rgb.b - y) / (2.0 * (1.0 - kb));
    float cr = (rgb.r - y) / (2.0 * (1.0 - kr));
    return float3(saturate(y), saturate(cb + 0.5), saturate(cr + 0.5));
}

float3 apply_yuv10_range(float3 ycbcr) {
    ycbcr = saturate(ycbcr);
    if (RR_FULL_RANGE) {
        return ycbcr;
    }
    float cb = ycbcr.y - 0.5;
    float cr = ycbcr.z - 0.5;
    return float3(
        (64.0 / 1023.0) + ycbcr.x * (876.0 / 1023.0),
        (512.0 / 1023.0) + cb * (896.0 / 1023.0),
        (512.0 / 1023.0) + cr * (896.0 / 1023.0)
    );
}

uint u10(float value) {
    return (uint)round(saturate(value) * 1023.0);
}

[numthreads(16, 8, 1)]
void cs_main(uint3 tid : SV_DispatchThreadID) {
    uint width;
    uint height;
    src_tex.GetDimensions(width, height);
    if (tid.x >= width || tid.y >= height) {
        return;
    }
    float3 ycbcr = apply_yuv10_range(rgb_to_bt2020_ycbcr(sc_rgb_to_bt2020_signal(src_tex.Load(int3(tid.xy, 0)).rgb)));
    uint y = u10(ycbcr.x);
    uint u = u10(ycbcr.y);
    uint v = u10(ycbcr.z);
    y410_tex[tid.xy] = u | (y << 10) | (v << 20) | (3u << 30);
}
"#;

#[cfg(windows)]
struct VideoProcessorBlitter {
    input_view: windows::Win32::Graphics::Direct3D11::ID3D11VideoProcessorInputView,
    output_view: windows::Win32::Graphics::Direct3D11::ID3D11VideoProcessorOutputView,
}

#[cfg(windows)]
impl VideoProcessorBlitter {
    unsafe fn new(
        video_device: &windows::Win32::Graphics::Direct3D11::ID3D11VideoDevice,
        enumerator: &windows::Win32::Graphics::Direct3D11::ID3D11VideoProcessorEnumerator,
        source: &windows::Win32::Graphics::Direct3D11::ID3D11Texture2D,
        target: &windows::Win32::Graphics::Direct3D11::ID3D11Texture2D,
    ) -> Result<Self, BackendError> {
        use windows::Win32::Graphics::Direct3D11::{
            D3D11_TEX2D_VPIV, D3D11_TEX2D_VPOV, D3D11_VIDEO_PROCESSOR_INPUT_VIEW_DESC,
            D3D11_VIDEO_PROCESSOR_INPUT_VIEW_DESC_0, D3D11_VIDEO_PROCESSOR_OUTPUT_VIEW_DESC,
            D3D11_VIDEO_PROCESSOR_OUTPUT_VIEW_DESC_0, D3D11_VPIV_DIMENSION_TEXTURE2D,
            D3D11_VPOV_DIMENSION_TEXTURE2D, ID3D11Resource, ID3D11VideoProcessorInputView,
            ID3D11VideoProcessorOutputView,
        };
        use windows::core::Interface;

        let source_resource: ID3D11Resource =
            source.cast().map_err(|err| BackendError::WindowsApi {
                func: "ID3D11Texture2D::cast<ID3D11Resource>(cached VP source)",
                message: err.to_string(),
            })?;
        let target_resource: ID3D11Resource =
            target.cast().map_err(|err| BackendError::WindowsApi {
                func: "ID3D11Texture2D::cast<ID3D11Resource>(cached VP target)",
                message: err.to_string(),
            })?;

        let input_desc = D3D11_VIDEO_PROCESSOR_INPUT_VIEW_DESC {
            FourCC: 0,
            ViewDimension: D3D11_VPIV_DIMENSION_TEXTURE2D,
            Anonymous: D3D11_VIDEO_PROCESSOR_INPUT_VIEW_DESC_0 {
                Texture2D: D3D11_TEX2D_VPIV {
                    MipSlice: 0,
                    ArraySlice: 0,
                },
            },
        };
        let mut input_view: Option<ID3D11VideoProcessorInputView> = None;
        video_device
            .CreateVideoProcessorInputView(
                &source_resource,
                enumerator,
                &input_desc,
                Some(&mut input_view),
            )
            .map_err(|err| BackendError::WindowsApi {
                func: "ID3D11VideoDevice::CreateVideoProcessorInputView(cached)",
                message: err.to_string(),
            })?;
        let input_view = input_view.ok_or_else(|| BackendError::WindowsApi {
            func: "CreateVideoProcessorInputView(cached)",
            message: "返回空 input view".to_owned(),
        })?;

        let output_desc = D3D11_VIDEO_PROCESSOR_OUTPUT_VIEW_DESC {
            ViewDimension: D3D11_VPOV_DIMENSION_TEXTURE2D,
            Anonymous: D3D11_VIDEO_PROCESSOR_OUTPUT_VIEW_DESC_0 {
                Texture2D: D3D11_TEX2D_VPOV { MipSlice: 0 },
            },
        };
        let mut output_view: Option<ID3D11VideoProcessorOutputView> = None;
        video_device
            .CreateVideoProcessorOutputView(
                &target_resource,
                enumerator,
                &output_desc,
                Some(&mut output_view),
            )
            .map_err(|err| BackendError::WindowsApi {
                func: "ID3D11VideoDevice::CreateVideoProcessorOutputView(cached)",
                message: err.to_string(),
            })?;
        let output_view = output_view.ok_or_else(|| BackendError::WindowsApi {
            func: "CreateVideoProcessorOutputView(cached)",
            message: "返回空 output view".to_owned(),
        })?;

        Ok(Self {
            input_view,
            output_view,
        })
    }

    unsafe fn blit(
        &self,
        video_context: &windows::Win32::Graphics::Direct3D11::ID3D11VideoContext,
        processor: &windows::Win32::Graphics::Direct3D11::ID3D11VideoProcessor,
    ) -> Result<(), BackendError> {
        use std::mem::ManuallyDrop;
        use windows::Win32::Graphics::Direct3D11::D3D11_VIDEO_PROCESSOR_STREAM;

        let mut stream = D3D11_VIDEO_PROCESSOR_STREAM {
            Enable: windows::core::BOOL(1),
            OutputIndex: 0,
            InputFrameOrField: 0,
            PastFrames: 0,
            FutureFrames: 0,
            ppPastSurfaces: ptr::null_mut(),
            pInputSurface: ManuallyDrop::new(Some(self.input_view.clone())),
            ppFutureSurfaces: ptr::null_mut(),
            ppPastSurfacesRight: ptr::null_mut(),
            pInputSurfaceRight: ManuallyDrop::new(None),
            ppFutureSurfacesRight: ptr::null_mut(),
        };
        let blt_result = video_context.VideoProcessorBlt(
            processor,
            &self.output_view,
            0,
            std::slice::from_ref(&stream),
        );
        ManuallyDrop::drop(&mut stream.pInputSurface);
        blt_result.map_err(|err| BackendError::WindowsApi {
            func: "ID3D11VideoContext::VideoProcessorBlt(cached)",
            message: err.to_string(),
        })
    }
}

#[cfg(windows)]
const SNAPSHOT_COPY_HLSL: &str = r#"
Texture2D<float4> src_tex : register(t0);

float4 vs_main(uint id : SV_VertexID) : SV_Position {
    float2 pos[3] = {
        float2(-1.0,  1.0),
        float2( 3.0,  1.0),
        float2(-1.0, -3.0)
    };
    return float4(pos[id], 0.0, 1.0);
}

float4 ps_main(float4 pos : SV_Position) : SV_Target {
    return src_tex.Load(int3(uint2(pos.xy), 0));
}
"#;

#[cfg(windows)]
const RGBA_CONVERT_HLSL: &str = r#"
Texture2D<float4> src_tex : register(t0);

float4 vs_main(uint id : SV_VertexID) : SV_Position {
    float2 pos[3] = {
        float2(-1.0,  1.0),
        float2( 3.0,  1.0),
        float2(-1.0, -3.0)
    };
    return float4(pos[id], 0.0, 1.0);
}

float4 ps_main(float4 pos : SV_Position) : SV_Target {
    return saturate(src_tex.Load(int3(uint2(pos.xy), 0)));
}
"#;

#[cfg(windows)]
unsafe fn compile_shader(
    source: &str,
    entry: &[u8],
    target: &[u8],
) -> Result<Vec<u8>, BackendError> {
    use windows::Win32::Graphics::Direct3D::Fxc::D3DCompile;
    use windows::Win32::Graphics::Direct3D::{ID3DBlob, ID3DInclude};
    use windows::core::PCSTR;

    let mut code: Option<ID3DBlob> = None;
    let mut errors: Option<ID3DBlob> = None;
    let result = D3DCompile(
        source.as_ptr() as *const c_void,
        source.len(),
        PCSTR::null(),
        None,
        Option::<&ID3DInclude>::None,
        PCSTR(entry.as_ptr()),
        PCSTR(target.as_ptr()),
        0,
        0,
        &mut code,
        Some(&mut errors),
    );
    if let Err(err) = result {
        let message = if let Some(errors) = errors {
            let ptr = errors.GetBufferPointer() as *const u8;
            let len = errors.GetBufferSize();
            String::from_utf8_lossy(std::slice::from_raw_parts(ptr, len)).to_string()
        } else {
            err.to_string()
        };
        return Err(BackendError::WindowsApi {
            func: "D3DCompile",
            message,
        });
    }
    let code = code.ok_or_else(|| BackendError::WindowsApi {
        func: "D3DCompile",
        message: "返回空 shader blob".to_owned(),
    })?;
    let ptr = code.GetBufferPointer() as *const u8;
    let len = code.GetBufferSize();
    Ok(std::slice::from_raw_parts(ptr, len).to_vec())
}

#[cfg(windows)]
fn shader_source_with_range(template: &str, full_range: bool) -> String {
    template.replace(
        "RR_FULL_RANGE_PLACEHOLDER",
        if full_range { "true" } else { "false" },
    )
}

#[cfg(windows)]
unsafe fn process_with_video_processor(
    video_device: &windows::Win32::Graphics::Direct3D11::ID3D11VideoDevice,
    video_context: &windows::Win32::Graphics::Direct3D11::ID3D11VideoContext,
    enumerator: &windows::Win32::Graphics::Direct3D11::ID3D11VideoProcessorEnumerator,
    processor: &windows::Win32::Graphics::Direct3D11::ID3D11VideoProcessor,
    source: &windows::Win32::Graphics::Direct3D11::ID3D11Texture2D,
    target: &windows::Win32::Graphics::Direct3D11::ID3D11Texture2D,
) -> Result<(), BackendError> {
    use std::mem::ManuallyDrop;
    use windows::Win32::Graphics::Direct3D11::{
        D3D11_TEX2D_VPIV, D3D11_TEX2D_VPOV, D3D11_VIDEO_PROCESSOR_INPUT_VIEW_DESC,
        D3D11_VIDEO_PROCESSOR_INPUT_VIEW_DESC_0, D3D11_VIDEO_PROCESSOR_OUTPUT_VIEW_DESC,
        D3D11_VIDEO_PROCESSOR_OUTPUT_VIEW_DESC_0, D3D11_VIDEO_PROCESSOR_STREAM,
        D3D11_VPIV_DIMENSION_TEXTURE2D, D3D11_VPOV_DIMENSION_TEXTURE2D, ID3D11Resource,
        ID3D11VideoProcessorInputView, ID3D11VideoProcessorOutputView,
    };
    use windows::core::Interface;

    let source_resource: ID3D11Resource =
        source.cast().map_err(|err| BackendError::WindowsApi {
            func: "ID3D11Texture2D::cast<ID3D11Resource>(source)",
            message: err.to_string(),
        })?;
    let target_resource: ID3D11Resource =
        target.cast().map_err(|err| BackendError::WindowsApi {
            func: "ID3D11Texture2D::cast<ID3D11Resource>(target)",
            message: err.to_string(),
        })?;

    let input_desc = D3D11_VIDEO_PROCESSOR_INPUT_VIEW_DESC {
        FourCC: 0,
        ViewDimension: D3D11_VPIV_DIMENSION_TEXTURE2D,
        Anonymous: D3D11_VIDEO_PROCESSOR_INPUT_VIEW_DESC_0 {
            Texture2D: D3D11_TEX2D_VPIV {
                MipSlice: 0,
                ArraySlice: 0,
            },
        },
    };
    let mut input_view: Option<ID3D11VideoProcessorInputView> = None;
    video_device
        .CreateVideoProcessorInputView(
            &source_resource,
            enumerator,
            &input_desc,
            Some(&mut input_view),
        )
        .map_err(|err| BackendError::WindowsApi {
            func: "ID3D11VideoDevice::CreateVideoProcessorInputView",
            message: err.to_string(),
        })?;
    let input_view = input_view.ok_or_else(|| BackendError::WindowsApi {
        func: "CreateVideoProcessorInputView",
        message: "返回空 input view".to_owned(),
    })?;

    let output_desc = D3D11_VIDEO_PROCESSOR_OUTPUT_VIEW_DESC {
        ViewDimension: D3D11_VPOV_DIMENSION_TEXTURE2D,
        Anonymous: D3D11_VIDEO_PROCESSOR_OUTPUT_VIEW_DESC_0 {
            Texture2D: D3D11_TEX2D_VPOV { MipSlice: 0 },
        },
    };
    let mut output_view: Option<ID3D11VideoProcessorOutputView> = None;
    video_device
        .CreateVideoProcessorOutputView(
            &target_resource,
            enumerator,
            &output_desc,
            Some(&mut output_view),
        )
        .map_err(|err| BackendError::WindowsApi {
            func: "ID3D11VideoDevice::CreateVideoProcessorOutputView",
            message: err.to_string(),
        })?;
    let output_view = output_view.ok_or_else(|| BackendError::WindowsApi {
        func: "CreateVideoProcessorOutputView",
        message: "返回空 output view".to_owned(),
    })?;

    let mut stream = D3D11_VIDEO_PROCESSOR_STREAM {
        Enable: windows::core::BOOL(1),
        OutputIndex: 0,
        InputFrameOrField: 0,
        PastFrames: 0,
        FutureFrames: 0,
        ppPastSurfaces: ptr::null_mut(),
        pInputSurface: ManuallyDrop::new(Some(input_view)),
        ppFutureSurfaces: ptr::null_mut(),
        ppPastSurfacesRight: ptr::null_mut(),
        pInputSurfaceRight: ManuallyDrop::new(None),
        ppFutureSurfacesRight: ptr::null_mut(),
    };
    let blt_result =
        video_context.VideoProcessorBlt(processor, &output_view, 0, std::slice::from_ref(&stream));
    ManuallyDrop::drop(&mut stream.pInputSurface);
    blt_result.map_err(|err| BackendError::WindowsApi {
        func: "ID3D11VideoContext::VideoProcessorBlt",
        message: err.to_string(),
    })
}

unsafe fn release_pending_surface(surface: Option<*mut MfxFrameSurface1>) {
    if let Some(surface) = surface
        && !surface.is_null()
        && !(*surface).FrameInterface.is_null()
    {
        let _ = ((*(*surface).FrameInterface).Release)(surface);
    }
}

#[cfg(windows)]
struct RecordThreadPriorityGuard {
    handle: windows::Win32::Foundation::HANDLE,
    previous: i32,
}

#[cfg(windows)]
impl RecordThreadPriorityGuard {
    unsafe fn raise(notes: &mut Vec<String>) -> Option<Self> {
        use windows::Win32::System::Threading::{
            GetCurrentThread, GetThreadPriority, SetThreadPriority, THREAD_PRIORITY_HIGHEST,
        };

        let handle = GetCurrentThread();
        let previous = GetThreadPriority(handle);
        match SetThreadPriority(handle, THREAD_PRIORITY_HIGHEST) {
            Ok(()) => {
                notes.push(
                    "录制线程临时提升到 THREAD_PRIORITY_HIGHEST 以降低 DDA 高刷新采集抖动"
                        .to_owned(),
                );
                Some(Self { handle, previous })
            }
            Err(err) => {
                notes.push(format!("录制线程提权失败，继续使用当前优先级：{}", err));
                None
            }
        }
    }

    unsafe fn raise_capture_thread() -> Option<Self> {
        use windows::Win32::System::Threading::{
            GetCurrentThread, GetThreadPriority, SetThreadPriority, THREAD_PRIORITY_TIME_CRITICAL,
        };

        let handle = GetCurrentThread();
        let previous = GetThreadPriority(handle);
        SetThreadPriority(handle, THREAD_PRIORITY_TIME_CRITICAL)
            .ok()
            .map(|()| Self { handle, previous })
    }
}

#[cfg(windows)]
impl Drop for RecordThreadPriorityGuard {
    fn drop(&mut self) {
        unsafe {
            let _ = windows::Win32::System::Threading::SetThreadPriority(
                self.handle,
                windows::Win32::System::Threading::THREAD_PRIORITY(self.previous),
            );
        }
    }
}

fn duration_to_90k(duration: std::time::Duration) -> u64 {
    (duration.as_secs_f64() * VIDEO_CLOCK_HZ as f64).round() as u64
}

#[cfg(windows)]
fn query_performance_frequency() -> Option<i64> {
    let mut frequency = 0i64;
    unsafe {
        windows::Win32::System::Performance::QueryPerformanceFrequency(&mut frequency)
            .ok()
            .filter(|_| frequency > 0)
            .map(|_| frequency)
    }
}

#[cfg(windows)]
fn dda_timestamp_90k(
    last_present_time_qpc: i64,
    qpc_frequency: i64,
    fallback_start: std::time::Instant,
) -> u64 {
    if last_present_time_qpc > 0 && qpc_frequency > 0 {
        qpc_delta_to_90k(last_present_time_qpc, qpc_frequency)
    } else {
        duration_to_90k(std::time::Instant::now().saturating_duration_since(fallback_start))
    }
}

#[cfg(windows)]
fn dda_relative_timestamp_90k(
    last_present_time_qpc: i64,
    qpc_frequency: i64,
    fallback_start: std::time::Instant,
    origin_qpc: &mut Option<i64>,
    last_timestamp_90k: &mut Option<u64>,
) -> u64 {
    let mut timestamp = if last_present_time_qpc > 0 && qpc_frequency > 0 {
        let origin = *origin_qpc.get_or_insert(last_present_time_qpc);
        qpc_delta_to_90k(last_present_time_qpc.saturating_sub(origin), qpc_frequency)
    } else {
        duration_to_90k(std::time::Instant::now().saturating_duration_since(fallback_start))
    };
    if let Some(last) = *last_timestamp_90k
        && timestamp <= last
    {
        timestamp = last.saturating_add(1);
    }
    *last_timestamp_90k = Some(timestamp);
    timestamp
}

#[cfg(windows)]
fn qpc_delta_to_90k(delta_qpc: i64, qpc_frequency: i64) -> u64 {
    ((delta_qpc.max(0) as i128 * VIDEO_CLOCK_HZ as i128 + (qpc_frequency as i128 / 2))
        / qpc_frequency as i128) as u64
}

#[cfg(windows)]
fn qpc_counter_to_100ns(qpc_value: i64, qpc_frequency: i64) -> Option<i64> {
    if qpc_value <= 0 || qpc_frequency <= 0 {
        return None;
    }
    Some(
        ((qpc_value as i128 * 10_000_000i128 + (qpc_frequency as i128 / 2)) / qpc_frequency as i128)
            as i64,
    )
}

#[cfg(windows)]
fn video_90k_to_100ns(duration_90k: u64) -> i64 {
    ((duration_90k as u128 * 10_000_000u128).div_ceil(VIDEO_CLOCK_HZ as u128)).min(i64::MAX as u128)
        as i64
}

#[cfg(windows)]
fn video_90k_to_audio_ticks(duration_90k: u64) -> u64 {
    ((duration_90k as u128 * crate::backend::audio::TARGET_SAMPLE_RATE as u128)
        .div_ceil(VIDEO_CLOCK_HZ as u128))
    .min(u64::MAX as u128) as u64
}

#[cfg(windows)]
fn audio_100ns_to_ticks(duration_100ns: i64) -> u64 {
    if duration_100ns <= 0 {
        return 0;
    }
    ((duration_100ns as i128 * crate::backend::audio::TARGET_SAMPLE_RATE as i128 + 5_000_000i128)
        / 10_000_000i128)
        .max(0) as u64
}

#[cfg(windows)]
fn audio_ticks_to_100ns(ticks: u64) -> i64 {
    ((ticks as u128 * 10_000_000u128).div_ceil(crate::backend::audio::TARGET_SAMPLE_RATE as u128))
        .min(i64::MAX as u128) as i64
}

#[cfg(windows)]
fn wgc_relative_timestamp_90k(
    timestamp_100ns: i64,
    origin_100ns: &mut Option<i64>,
    last_timestamp_90k: &mut Option<u64>,
) -> u64 {
    // WGC is intentionally VFR.  This conversion is the only timestamp
    // transform for accepted WGC frames: SystemRelativeTime is made relative to
    // the first accepted WGC source frame and scaled to the 90 kHz MP4/video
    // timebase.  Do not add an external CFR clock here; source gaps must
    // remain visible as longer sample durations.
    let origin = *origin_100ns.get_or_insert(timestamp_100ns);
    let delta_100ns = timestamp_100ns.saturating_sub(origin).max(0) as i128;
    let timestamp =
        (((delta_100ns * VIDEO_CLOCK_HZ as i128) + 5_000_000i128) / 10_000_000i128).max(0) as u64;
    *last_timestamp_90k = Some(timestamp);
    timestamp
}

fn encoded_timeline_duration_90k(
    samples: &[crate::backend::mp4_mux::HevcAccessUnit],
    fallback_90k: u64,
    extend_to_fallback: bool,
) -> u64 {
    // DDA historically extends the final sample to the requested recording
    // duration to keep a strict whole-duration timeline. WGC must not do that:
    // its VFR timeline is defined by accepted WGC SystemRelativeTime samples,
    // so an early stop should produce a shorter source-derived track instead
    // of stretching the last sample to an external wall-clock/CFR target.
    let source_end_90k = samples
        .iter()
        .rev()
        .find(|sample| !sample.discard_from_track)
        .map(|sample| sample.timestamp_90k.saturating_add(1))
        .unwrap_or(fallback_90k);
    if extend_to_fallback {
        source_end_90k.max(fallback_90k).max(1)
    } else {
        source_end_90k.max(1)
    }
}

const fn align16(value: u16) -> u16 {
    value.div_ceil(16) * 16
}

unsafe fn parse_impl(
    index: u32,
    desc: &MfxImplDescription,
    api: &VplApi,
    loader: MfxLoader,
    current_display_route_keys: &BTreeSet<String>,
    warnings: &mut Vec<String>,
) -> VplImplementationInfo {
    let mut hevc_supported = false;
    let mut hevc_profiles = BTreeSet::new();
    let mut input_fourcc = BTreeSet::new();
    let mut rate_controls = BTreeSet::new();
    let mut dx11_texture_input_seen = false;

    let codecs = bounded_slice(desc.Enc.Codecs, desc.Enc.NumCodecs, 128);
    for codec in codecs {
        if codec.CodecID != MFX_CODEC_HEVC {
            continue;
        }
        hevc_supported = true;
        if desc.Enc.Version.version >= struct_version(1, 1) && !codec.EncExtDesc.is_null() {
            let ext = &*codec.EncExtDesc;
            let methods = bounded_slice(ext.RateControlMethods, ext.NumRateControlMethods, 128);
            for &raw in methods {
                if let Some(method) = RateControlMethod::from_vpl_value(raw) {
                    rate_controls.insert(method);
                }
            }
        }

        let profiles = bounded_slice(codec.Profiles, codec.NumProfiles, 128);
        for profile in profiles {
            hevc_profiles.insert(hevc_profile_name(profile.Profile).to_owned());
            let mem_descs = bounded_slice(profile.MemDesc, profile.NumMemTypes, 128);
            for mem in mem_descs {
                if mem.MemHandleType == MFX_RESOURCE_DX11_TEXTURE {
                    dx11_texture_input_seen = true;
                }
                let color_formats = bounded_slice(mem.ColorFormats, mem.NumColorFormats, 256);
                for &fourcc in color_formats {
                    input_fourcc.insert(fourcc_to_string(fourcc));
                }
                if desc.Enc.Version.version >= struct_version(1, 1) && !mem.MemExtDesc.is_null() {
                    let mem_ext = &*mem.MemExtDesc;
                    let chromas = bounded_slice(
                        mem_ext.TargetChromaSubsamplings,
                        mem_ext.NumTargetChromaSubsamplings,
                        32,
                    );
                    for &chroma in chromas {
                        if let Some(mapped) = chroma_from_vpl(chroma) {
                            input_fourcc.insert(format!("ChromaFormat:{:?}", mapped));
                        }
                    }
                }
            }
        }
    }

    let route_candidates = if hevc_supported {
        query_encode_route_candidates(
            api,
            loader,
            index,
            &input_fourcc,
            current_display_route_keys,
            warnings,
        )
    } else {
        Vec::new()
    };

    if hevc_supported {
        let descriptor_rate_controls = rate_controls.clone();
        rate_controls.clear();
        for method in route_candidates
            .iter()
            .filter(|route| route.production_record_supported)
            .flat_map(|route| route.rate_controls.iter().copied())
        {
            rate_controls.insert(method);
        }
        for method in descriptor_rate_controls.difference(&rate_controls) {
            warnings.push(format!(
                "实现 {index} 的 mfxImplDescription 暴露了 RateControlMethod={}，但当前 HEVC/D3D11 production query 未确认支持，前端隐藏该模式",
                method.short_name()
            ));
        }
        if rate_controls.is_empty() && !descriptor_rate_controls.is_empty() {
            warnings.push(format!(
                "实现 {index} 的 RateControlMethod 描述列表非空，但逐项 MFXVideoENCODE_Query 均未通过；按能力隐藏策略不展示码控模式"
            ));
        }
    }

    if desc.Enc.NumCodecs > 128 {
        warnings.push(format!("实现 {index} 编码器数量异常，已截断读取"));
    }

    VplImplementationInfo {
        index,
        impl_name: c_char_array_to_string(&desc.ImplName),
        api_version: version_to_string(desc.ApiVersion.version),
        implementation: match desc.Impl {
            MFX_IMPL_TYPE_HARDWARE => "硬件".to_owned(),
            1 => "软件".to_owned(),
            other => format!("未知({other})"),
        },
        acceleration_mode: acceleration_to_string(
            desc.AccelerationMode,
            &desc.AccelerationModeDescription,
        ),
        vendor_id: desc.VendorID,
        vendor_impl_id: desc.VendorImplID,
        device_id: c_char_array_to_string(&desc.Dev.DeviceID),
        media_adapter_type: desc.Dev.MediaAdapterType,
        hevc_supported,
        hevc_profiles: hevc_profiles.into_iter().collect(),
        input_fourcc: input_fourcc.into_iter().collect(),
        route_candidates,
        rate_controls: rate_controls.into_iter().collect(),
        dx11_texture_input_seen,
    }
}

unsafe fn query_rate_control_config_supported(
    api: &VplApi,
    session: MfxSession,
    route: VplRecordRoute,
    rate_control: &RateControlConfig,
) -> bool {
    let mut input = make_query_param(
        rate_control,
        route.fourcc,
        route.chroma,
        route.bit_depth,
        route.profile,
    );
    let mut ext_buffers = VplEncodeExtBuffers::for_route(route, rate_control);
    ext_buffers.attach(&mut input);
    let mut output = input;
    let status = (api.mfx_video_encode_query)(session, &mut input, &mut output);
    if status < MFX_ERR_NONE
        || status == MFX_WRN_PARTIAL_ACCELERATION
        || !query_output_preserves_record_route(&output, route)
        || output.mfx.RateControlMethod != rate_control.method.vpl_value()
    {
        return false;
    }
    let mut request: MfxFrameAllocRequest = std::mem::zeroed();
    let mut iosurf_param = output;
    (api.mfx_video_encode_query_iosurf)(session, &mut iosurf_param, &mut request) >= MFX_ERR_NONE
}

unsafe fn smoke_rate_control_surface_available(
    api: &VplApi,
    loader: MfxLoader,
    implementation_index: u32,
    route: VplRecordRoute,
    rate_control: &RateControlConfig,
) -> bool {
    let mut session: MfxSession = ptr::null_mut();
    let create_status = (api.mfx_create_session)(loader, implementation_index, &mut session);
    if create_status != MFX_ERR_NONE || session.is_null() {
        return false;
    }
    let mut param = make_query_param(
        rate_control,
        route.fourcc,
        route.chroma,
        route.bit_depth,
        route.profile,
    );
    // 用生产目标的 4K 桌面尺寸 + WGC async_depth=2 参数做 Query/Init smoke，避免某个
    // 码控字段在默认低分辨率 Query/Init 通过、实际桌面录制却拿不到 surface。
    param.mfx.FrameInfo.Width = 3840;
    param.mfx.FrameInfo.Height = 2160;
    param.mfx.FrameInfo.CropW = 3840;
    param.mfx.FrameInfo.CropH = 2160;
    param.mfx.FrameInfo.FrameRateExtN = VPL_ENCODER_FRAME_RATE_HINT_FALLBACK_N;
    param.mfx.FrameInfo.FrameRateExtD = VPL_ENCODER_FRAME_RATE_HINT_FALLBACK_D;
    param.mfx.GopRefDist = 1;
    param.mfx.LowPower = MFX_CODINGOPTION_ON;
    param.mfx.TargetUsage = 7;
    // GUI/即时回放生产路径默认走 WGC，当前低延迟/稳定性路线使用 AsyncDepth=2；
    // 因此可见性 smoke 必须按这个生产约束判断，而不是按 DDA 压测用深队列放宽。
    param.AsyncDepth = 2;
    let mut ext_buffers = VplEncodeExtBuffers::for_route(route, rate_control);
    ext_buffers.attach(&mut param);
    let mut queried = param;
    let query_status = (api.mfx_video_encode_query)(session, &mut param, &mut queried);
    if query_status < MFX_ERR_NONE
        || query_status == MFX_WRN_PARTIAL_ACCELERATION
        || !query_output_preserves_record_route(&queried, route)
        || queried.mfx.RateControlMethod != rate_control.method.vpl_value()
    {
        let _ = (api.mfx_close)(session);
        return false;
    }
    let mut param = queried;
    apply_record_route_to_param(&mut param, route);
    apply_rate_control_config_to_param(&mut param, rate_control);
    param.mfx.FrameInfo.Width = 3840;
    param.mfx.FrameInfo.Height = 2160;
    param.mfx.FrameInfo.CropW = 3840;
    param.mfx.FrameInfo.CropH = 2160;
    param.mfx.FrameInfo.FrameRateExtN = VPL_ENCODER_FRAME_RATE_HINT_FALLBACK_N;
    param.mfx.FrameInfo.FrameRateExtD = VPL_ENCODER_FRAME_RATE_HINT_FALLBACK_D;
    param.mfx.GopPicSize = 60;
    param.mfx.GopRefDist = 1;
    param.mfx.IdrInterval = 1;
    param.mfx.LowPower = MFX_CODINGOPTION_ON;
    param.mfx.TargetUsage = 7;
    param.AsyncDepth = 2;
    let mut ext_buffers = VplEncodeExtBuffers::for_route(route, rate_control);
    ext_buffers.attach(&mut param);
    let init_status = (api.mfx_video_encode_init)(session, &mut param);
    if init_status < MFX_ERR_NONE || init_status == MFX_WRN_PARTIAL_ACCELERATION {
        let _ = (api.mfx_close)(session);
        return false;
    }
    // 与生产录制一致，LowDelayBRC 等字段不能只看 Query/Init/首个 surface：
    // 某些驱动会在首帧 warmup encode 后无法继续提供 video-memory surface。
    let warmup_copies = std::env::var("RUST_REPLAY_VPL_SURFACE_WARMUP_COPIES")
        .ok()
        .and_then(|value| value.parse::<usize>().ok())
        .unwrap_or(if rate_control.low_delay_brc { 1 } else { 4 })
        .clamp(1, param.AsyncDepth as usize);
    let mut ok = true;
    let mut first_surface: *mut MfxFrameSurface1 = ptr::null_mut();
    let first_status = (api.mfx_memory_get_surface_for_encode)(session, &mut first_surface);
    if first_status != MFX_ERR_NONE || first_surface.is_null() {
        ok = false;
    }
    if ok {
        let interface = (*first_surface).FrameInterface;
        if interface.is_null() {
            ok = false;
        } else {
            let mut native: MfxHDL = ptr::null_mut();
            let mut native_type = 0u32;
            let native_status =
                ((*interface).GetNativeHandle)(first_surface, &mut native, &mut native_type);
            if native_status != MFX_ERR_NONE || native_type != MFX_RESOURCE_DX11_TEXTURE {
                ok = false;
            }
            if ok {
                let mut device_handle: MfxHDL = ptr::null_mut();
                let mut device_type = 0u32;
                let device_status = ((*interface).GetDeviceHandle)(
                    first_surface,
                    &mut device_handle,
                    &mut device_type,
                );
                if device_status != MFX_ERR_NONE
                    || device_type != MFX_HANDLE_D3D11_DEVICE
                    || device_handle.is_null()
                {
                    ok = false;
                }
                if ok {
                    let Some(target_texture) =
                        <windows::Win32::Graphics::Direct3D11::ID3D11Texture2D as windows::core::Interface>::from_raw_borrowed(&native)
                    else {
                        let _ = ((*interface).Release)(first_surface);
                        let _ = (api.mfx_video_encode_close)(session);
                        let _ = (api.mfx_close)(session);
                        return false;
                    };
                    let Some(device) =
                        <windows::Win32::Graphics::Direct3D11::ID3D11Device as windows::core::Interface>::from_raw_borrowed(&device_handle)
                    else {
                        let _ = ((*interface).Release)(first_surface);
                        let _ = (api.mfx_video_encode_close)(session);
                        let _ = (api.mfx_close)(session);
                        return false;
                    };
                    let mut target_desc =
                        windows::Win32::Graphics::Direct3D11::D3D11_TEXTURE2D_DESC::default();
                    target_texture.GetDesc(&mut target_desc);
                    target_desc.BindFlags = Default::default();
                    target_desc.MiscFlags = Default::default();
                    target_desc.CPUAccessFlags = Default::default();
                    target_desc.Usage = windows::Win32::Graphics::Direct3D11::D3D11_USAGE_DEFAULT;
                    let mut source_texture = None;
                    if device
                        .CreateTexture2D(&target_desc, None, Some(&mut source_texture))
                        .is_err()
                    {
                        ok = false;
                    }
                    if let Some(source_texture) = source_texture.as_ref() {
                        match device.GetImmediateContext() {
                            Ok(context) => {
                                if copy_texture_resource(&context, source_texture, target_texture)
                                    .is_err()
                                {
                                    ok = false;
                                }
                            }
                            Err(_) => ok = false,
                        }
                    } else {
                        ok = false;
                    }
                }
            }
            if ok {
                (*first_surface).Data.TimeStamp = 0;
                (*first_surface).Data.FrameOrder = 1;
                let mut bitstream_pool = vec![vec![0u8; VPL_BITSTREAM_BYTES + 31]];
                let submitted = submit_encode_async(
                    api,
                    session,
                    first_surface,
                    0,
                    true,
                    bitstream_pool.pop().unwrap_or_default(),
                    true,
                );
                let release_status = ((*interface).Release)(first_surface);
                if release_status != MFX_ERR_NONE {
                    ok = false;
                }
                match submitted {
                    Ok(Some(flight)) if ok => {
                        let mut in_flight = VecDeque::new();
                        in_flight.push_back(flight);
                        if sync_one_async_encode(api, session, &mut in_flight, &mut bitstream_pool)
                            .is_err()
                        {
                            ok = false;
                        }
                    }
                    Ok(_) => {}
                    Err(_) => ok = false,
                }
            } else {
                let _ = ((*interface).Release)(first_surface);
            }
        }
    }
    if ok {
        for _ in 1..warmup_copies {
            let mut surface: *mut MfxFrameSurface1 = ptr::null_mut();
            let surface_status = (api.mfx_memory_get_surface_for_encode)(session, &mut surface);
            if surface_status != MFX_ERR_NONE || surface.is_null() {
                ok = false;
                break;
            }
            let interface = (*surface).FrameInterface;
            if interface.is_null() {
                ok = false;
                break;
            }
            let mut native: MfxHDL = ptr::null_mut();
            let mut native_type = 0u32;
            let native_status =
                ((*interface).GetNativeHandle)(surface, &mut native, &mut native_type);
            let release_status = ((*interface).Release)(surface);
            if native_status != MFX_ERR_NONE
                || native_type != MFX_RESOURCE_DX11_TEXTURE
                || release_status != MFX_ERR_NONE
            {
                ok = false;
                break;
            }
        }
    }
    if ok {
        // 再模拟一帧真实编码循环的 surface 获取；当前测试机上 LowDelayBRC
        // 不是卡在首个 warmup surface，而是 warmup 后的下一次获取返回 -4。
        let mut surface: *mut MfxFrameSurface1 = ptr::null_mut();
        let surface_status = (api.mfx_memory_get_surface_for_encode)(session, &mut surface);
        if surface_status != MFX_ERR_NONE || surface.is_null() {
            ok = false;
        } else {
            let interface = (*surface).FrameInterface;
            if interface.is_null() {
                ok = false;
            } else {
                let mut native: MfxHDL = ptr::null_mut();
                let mut native_type = 0u32;
                let native_status =
                    ((*interface).GetNativeHandle)(surface, &mut native, &mut native_type);
                let release_status = ((*interface).Release)(surface);
                if native_status != MFX_ERR_NONE
                    || native_type != MFX_RESOURCE_DX11_TEXTURE
                    || release_status != MFX_ERR_NONE
                {
                    ok = false;
                }
            }
        }
    }
    let _ = (api.mfx_video_encode_close)(session);
    let _ = (api.mfx_close)(session);
    ok
}

unsafe fn query_rate_control_features_for_route(
    api: &VplApi,
    session: MfxSession,
    loader: MfxLoader,
    implementation_index: u32,
    route: VplRecordRoute,
) -> Vec<VplRateControlFeatureProbe> {
    let mut supported = Vec::new();
    for method in RateControlMethod::all() {
        let mut rate_control = RateControlConfig {
            method,
            ..RateControlConfig::default()
        };
        // 外部 BRC 需要 mfxExtBRC 回调结构；能力探测阶段只验证 oneVPL 内建码控模式。
        rate_control.ext_brc = false;
        if !query_rate_control_config_supported(api, session, route, &rate_control) {
            continue;
        }

        let mut mbbrc_cfg = rate_control.clone();
        mbbrc_cfg.mbbrc = true;
        let mbbrc = query_rate_control_config_supported(api, session, route, &mbbrc_cfg);

        let win_brc = if matches!(
            method,
            RateControlMethod::Cbr
                | RateControlMethod::Vbr
                | RateControlMethod::La
                | RateControlMethod::LaHrd
                | RateControlMethod::Qvbr
        ) {
            let mut cfg = rate_control.clone();
            cfg.win_brc_max_avg_kbps = cfg.max_kbps.max(cfg.target_kbps).max(1);
            cfg.win_brc_size = 60;
            query_rate_control_config_supported(api, session, route, &cfg)
        } else {
            false
        };

        let max_frame_size = if matches!(
            method,
            RateControlMethod::Vbr
                | RateControlMethod::La
                | RateControlMethod::Vcm
                | RateControlMethod::LaHrd
                | RateControlMethod::Qvbr
        ) {
            let mut cfg = rate_control.clone();
            cfg.max_frame_size = 1_048_576;
            query_rate_control_config_supported(api, session, route, &cfg)
        } else {
            false
        };

        // LowDelayBRC 在当前 mfx-gen/D3D11/WGC 生产循环中已观察到
        // Query/Init/近似 surface smoke 通过、但真实帧循环随后
        // MFXMemory_GetSurfaceForEncode 返回 -4。启动探测不能安全启动完整
        // 录制循环，因此默认按“不可用字段隐藏”处理；需要硬件 bring-up
        // 时可显式打开实验环境变量重新暴露。
        let low_delay_brc = if matches!(
            method,
            RateControlMethod::Vbr | RateControlMethod::Vcm | RateControlMethod::Qvbr
        ) {
            if std::env::var_os("RUST_REPLAY_EXPERIMENTAL_LOW_DELAY_BRC_PROBE").is_some() {
                let mut cfg = rate_control.clone();
                cfg.low_delay_brc = true;
                query_rate_control_config_supported(api, session, route, &cfg)
                    && smoke_rate_control_surface_available(
                        api,
                        loader,
                        implementation_index,
                        route,
                        &cfg,
                    )
            } else {
                false
            }
        } else {
            false
        };

        supported.push(VplRateControlFeatureProbe {
            method,
            look_ahead_depth: matches!(
                method,
                RateControlMethod::La | RateControlMethod::LaIcq | RateControlMethod::LaHrd
            ),
            win_brc,
            low_delay_brc,
            max_frame_size,
            mbbrc,
        });
    }
    supported
}

unsafe fn query_encode_route_candidates(
    api: &VplApi,
    loader: MfxLoader,
    implementation_index: u32,
    input_fourcc: &BTreeSet<String>,
    current_display_route_keys: &BTreeSet<String>,
    warnings: &mut Vec<String>,
) -> Vec<VplRouteProbe> {
    let mut session: MfxSession = ptr::null_mut();
    let create_status = (api.mfx_create_session)(loader, implementation_index, &mut session);
    if create_status != MFX_ERR_NONE || session.is_null() {
        warnings.push(format!(
            "MFXCreateSession({implementation_index}) 失败，无法用 MFXVideoENCODE_Query 探测 route matrix: status={create_status}"
        ));
        return Vec::new();
    }

    let mut probes = Vec::new();
    for route in route_candidates_from_fourcc(input_fourcc)
        .into_iter()
        .filter(|route| current_display_route_keys.contains(&route_probe_key(*route)))
    {
        let cfg = RateControlConfig::default();
        let mut input = make_query_param(
            &cfg,
            route.fourcc,
            route.chroma,
            route.bit_depth,
            route.profile,
        );
        let mut ext_buffers = VplEncodeExtBuffers::for_route(route, &cfg);
        ext_buffers.attach(&mut input);
        let mut output = input;
        let status = (api.mfx_video_encode_query)(session, &mut input, &mut output);
        let preserved = query_output_preserves_record_route(&output, route);
        let query_supported = status >= MFX_ERR_NONE && status != MFX_WRN_PARTIAL_ACCELERATION;
        let mut request: MfxFrameAllocRequest = std::mem::zeroed();
        let query_iosurf_status = if query_supported && preserved {
            let mut iosurf_param = output;
            (api.mfx_video_encode_query_iosurf)(session, &mut iosurf_param, &mut request)
        } else {
            MFX_ERR_NOT_FOUND
        };
        let query_iosurf_supported = query_iosurf_status >= MFX_ERR_NONE;
        let production_record_supported = query_supported
            && query_iosurf_supported
            && preserved
            && route.production_gpu_writer_available();
        let production_blocker = if production_record_supported {
            None
        } else if query_supported && preserved && !route.production_gpu_writer_available() {
            route.production_gpu_writer_blocker().map(str::to_owned)
        } else if query_supported && preserved && !query_iosurf_supported {
            Some(format!(
                "MFXVideoENCODE_QueryIOSurf 未通过：status={query_iosurf_status}"
            ))
        } else if query_supported && !preserved {
            Some(
                "oneVPL Query 改写了 FourCC/Chroma/BitDepth/Profile，不能视作该 route 可用"
                    .to_owned(),
            )
        } else {
            Some(format!(
                "MFXVideoENCODE_Query 未通过或部分加速：status={status}"
            ))
        };
        let rate_control_features = if production_record_supported {
            query_rate_control_features_for_route(api, session, loader, implementation_index, route)
        } else {
            Vec::new()
        };
        let rate_controls = rate_control_features
            .iter()
            .map(|feature| feature.method)
            .collect::<Vec<_>>();
        let note = if production_record_supported {
            format!(
                "Query/QueryIOSurf 通过；生产路线已接入 GPU writer/MP4 metadata；该 route 逐项 Query 确认码控模式=[{}]",
                rate_controls
                    .iter()
                    .map(|method| method.short_name())
                    .collect::<Vec<_>>()
                    .join(", ")
            )
        } else if query_supported && preserved {
            format!(
                "Query 通过；QueryIOSurf status={query_iosurf_status}；生产录制保持 Unsupported：{}",
                production_blocker.as_deref().unwrap_or("未知生产阻断")
            )
        } else if query_supported {
            "Query 返回成功但 oneVPL 改写了 FourCC/Chroma/BitDepth/Profile，不能视作该 route 可用"
                .to_owned()
        } else {
            format!("Query 未通过或部分加速，status={status}")
        };
        probes.push(VplRouteProbe {
            fourcc: fourcc_to_string(route.fourcc),
            chroma: chroma_from_vpl(route.chroma).unwrap_or(ChromaSampling::Yuv420),
            bit_depth: route.bit_depth,
            profile: hevc_profile_name(u32::from(route.profile)).to_owned(),
            query_status: status,
            query_iosurf_status,
            num_frame_min: request.NumFrameMin,
            num_frame_suggested: request.NumFrameSuggested,
            query_supported,
            query_iosurf_supported,
            query_preserved_route: preserved,
            production_record_supported,
            production_blocker,
            rate_controls,
            rate_control_features,
            note,
        });
    }

    let close_status = (api.mfx_close)(session);
    if close_status != MFX_ERR_NONE {
        warnings.push(format!(
            "MFXClose({implementation_index}) route matrix 返回 status={close_status}"
        ));
    }
    probes
}

fn route_candidates_from_fourcc(input_fourcc: &BTreeSet<String>) -> Vec<VplRecordRoute> {
    VplRecordRoute::query_candidates()
        .into_iter()
        .filter(|route| input_fourcc.contains(&fourcc_to_string(route.fourcc)))
        .collect()
}

fn route_probe_key(route: VplRecordRoute) -> String {
    route_probe_key_parts(
        &fourcc_to_string(route.fourcc),
        chroma_from_vpl(route.chroma).unwrap_or(ChromaSampling::Yuv420),
        route.bit_depth,
        hevc_profile_name(u32::from(route.profile)),
    )
}

fn route_probe_key_parts(
    fourcc: &str,
    chroma: ChromaSampling,
    bit_depth: u16,
    profile: &str,
) -> String {
    format!("{fourcc}::{chroma:?}::{bit_depth}::{profile}")
}

fn choose_query_format(input_fourcc: &BTreeSet<String>) -> (u32, u16, u16, u16) {
    if input_fourcc.contains("NV12") {
        (MFX_FOURCC_NV12, 1, 8, MFX_PROFILE_HEVC_MAIN as u16)
    } else if input_fourcc.contains("P010") {
        (MFX_FOURCC_P010, 1, 10, MFX_PROFILE_HEVC_MAIN10 as u16)
    } else if input_fourcc.contains("YUY2") {
        (MFX_FOURCC_YUY2, 2, 8, MFX_PROFILE_HEVC_REXT as u16)
    } else if input_fourcc.contains("Y210") {
        (MFX_FOURCC_Y210, 2, 10, MFX_PROFILE_HEVC_REXT as u16)
    } else if input_fourcc.contains("P210") {
        (MFX_FOURCC_P210, 2, 10, MFX_PROFILE_HEVC_REXT as u16)
    } else if input_fourcc.contains("AYUV") {
        (MFX_FOURCC_AYUV, 3, 8, MFX_PROFILE_HEVC_REXT as u16)
    } else if input_fourcc.contains("Y410") {
        (MFX_FOURCC_Y410, 3, 10, MFX_PROFILE_HEVC_REXT as u16)
    } else if input_fourcc.contains("RGB4") {
        (MFX_FOURCC_RGB4, 3, 8, MFX_PROFILE_HEVC_REXT as u16)
    } else {
        (MFX_FOURCC_NV12, 1, 8, MFX_PROFILE_HEVC_MAIN as u16)
    }
}

fn make_query_param(
    rate_control: &RateControlConfig,
    fourcc: u32,
    chroma: u16,
    bit_depth: u16,
    profile: u16,
) -> MfxVideoParam {
    let mut param: MfxVideoParam = unsafe { std::mem::zeroed() };
    param.AsyncDepth = VPL_RECORD_ASYNC_DEPTH;
    param.IOPattern = MFX_IOPATTERN_IN_VIDEO_MEMORY;
    param.mfx.BRCParamMultiplier = 1;
    param.mfx.FrameInfo.FourCC = fourcc;
    param.mfx.FrameInfo.Width = 1920;
    param.mfx.FrameInfo.Height = 1088;
    param.mfx.FrameInfo.CropW = 1920;
    param.mfx.FrameInfo.CropH = 1080;
    param.mfx.FrameInfo.FrameRateExtN = VPL_ENCODER_FRAME_RATE_HINT_FALLBACK_N;
    param.mfx.FrameInfo.FrameRateExtD = VPL_ENCODER_FRAME_RATE_HINT_FALLBACK_D;
    param.mfx.FrameInfo.PicStruct = MFX_PICSTRUCT_PROGRESSIVE;
    param.mfx.FrameInfo.ChromaFormat = chroma;
    param.mfx.FrameInfo.BitDepthLuma = bit_depth;
    param.mfx.FrameInfo.BitDepthChroma = bit_depth;
    param.mfx.CodecId = MFX_CODEC_HEVC;
    param.mfx.CodecProfile = profile;
    param.mfx.LowPower = MFX_CODINGOPTION_ON;
    param.mfx.TargetUsage = 7;
    param.mfx.GopPicSize = 60;
    param.mfx.GopRefDist = 1;
    param.mfx.IdrInterval = 1;
    apply_rate_control_config_to_param(&mut param, rate_control);
    param
}

fn validate_rate_control_config(rate_control: &RateControlConfig) -> Result<(), BackendError> {
    if rate_control.ext_brc {
        return Err(BackendError::unsupported(
            "oneVPL RateControl",
            "ExtBRC=ON",
            "mfxExtCodingOption2::ExtBRC 需要同时提供 mfxExtBRC 回调结构；当前 GPU-only 生产后端尚未实现外部 BRC 回调，不能假装启用",
        ));
    }
    if matches!(
        rate_control.method,
        RateControlMethod::La | RateControlMethod::LaIcq | RateControlMethod::LaHrd
    ) && rate_control.look_ahead_depth != 0
        && !(10..=100).contains(&rate_control.look_ahead_depth)
    {
        return Err(BackendError::unsupported(
            "oneVPL RateControl",
            format!("LookAheadDepth={}", rate_control.look_ahead_depth),
            "LookAheadDepth 只能为 0(库默认) 或 10..=100；前端应隐藏/限制不可用值",
        ));
    }
    let fields = rate_control.to_vpl_fields();
    if (fields.win_brc_max_avg_kbps == 0) != (fields.win_brc_size == 0) {
        return Err(BackendError::unsupported(
            "oneVPL RateControl",
            format!(
                "WinBRCMaxAvgKbps={} WinBRCSize={}",
                fields.win_brc_max_avg_kbps, fields.win_brc_size
            ),
            "sliding window BRC 必须两个字段同时为 0 才关闭，或两个字段同时非 0 才启用",
        ));
    }
    let multiplier = fields.brc_param_multiplier.max(1);
    validate_scaled_field("InitialDelayInKB", fields.initial_delay_in_kb, multiplier)?;
    validate_scaled_field("BufferSizeInKB", fields.buffer_size_in_kb, multiplier)?;
    validate_scaled_field("TargetKbps", fields.target_kbps, multiplier)?;
    validate_scaled_field("MaxKbps", fields.max_kbps, multiplier)?;
    validate_scaled_field("WinBRCMaxAvgKbps", fields.win_brc_max_avg_kbps, multiplier)?;
    Ok(())
}

fn validate_scaled_field(
    name: &'static str,
    value: u32,
    multiplier: u16,
) -> Result<(), BackendError> {
    if value == 0 {
        return Ok(());
    }
    let max_effective = u32::from(u16::MAX) * u32::from(multiplier.max(1));
    if value > max_effective {
        return Err(BackendError::unsupported(
            "oneVPL RateControl",
            format!("{name}={value}, BRCParamMultiplier={multiplier}"),
            format!(
                "{name} 按当前 BRCParamMultiplier 写入 oneVPL 16-bit 字段会被截断；请提高 BRCParamMultiplier 或降低该值，上限约 {max_effective}"
            ),
        ));
    }
    Ok(())
}

fn apply_record_route_to_param(param: &mut MfxVideoParam, route: VplRecordRoute) {
    param.mfx.FrameInfo.FourCC = route.fourcc;
    param.mfx.FrameInfo.ChromaFormat = route.chroma;
    param.mfx.FrameInfo.BitDepthLuma = route.bit_depth;
    param.mfx.FrameInfo.BitDepthChroma = route.bit_depth;
    param.mfx.CodecProfile = route.profile;
}

fn query_output_preserves_record_route(param: &MfxVideoParam, route: VplRecordRoute) -> bool {
    param.mfx.FrameInfo.FourCC == route.fourcc
        && param.mfx.FrameInfo.ChromaFormat == route.chroma
        && param.mfx.FrameInfo.BitDepthLuma == route.bit_depth
        && param.mfx.FrameInfo.BitDepthChroma == route.bit_depth
        && param.mfx.CodecProfile == route.profile
}

fn apply_rate_control_config_to_param(param: &mut MfxVideoParam, cfg: &RateControlConfig) {
    let fields = cfg.to_vpl_fields();
    let multiplier = fields.brc_param_multiplier.max(1);
    param.mfx.BRCParamMultiplier = multiplier;
    param.mfx.RateControlMethod = fields.rate_control_method;

    // oneVPL 的这些位置是 union：按当前 RateControlMethod 写入对应含义。
    param.mfx.InitialDelayInKB = 0;
    param.mfx.BufferSizeInKB = scale_kb_field_to_u16(fields.buffer_size_in_kb, multiplier);
    param.mfx.TargetKbps = 0;
    param.mfx.MaxKbps = 0;

    match cfg.method {
        RateControlMethod::Cbr => {
            param.mfx.InitialDelayInKB =
                scale_kb_field_to_u16(fields.initial_delay_in_kb, multiplier);
            param.mfx.TargetKbps = scale_kb_field_to_u16(fields.target_kbps, multiplier);
        }
        RateControlMethod::Vbr
        | RateControlMethod::Vcm
        | RateControlMethod::LaHrd
        | RateControlMethod::Qvbr => {
            param.mfx.InitialDelayInKB =
                scale_kb_field_to_u16(fields.initial_delay_in_kb, multiplier);
            param.mfx.TargetKbps = scale_kb_field_to_u16(fields.target_kbps, multiplier);
            param.mfx.MaxKbps = scale_kb_field_to_u16(fields.max_kbps, multiplier);
        }
        RateControlMethod::Cqp => {
            param.mfx.InitialDelayInKB = fields.qpi;
            param.mfx.TargetKbps = fields.qpp;
            param.mfx.MaxKbps = fields.qpb;
            param.mfx.BufferSizeInKB = 0;
        }
        RateControlMethod::Avbr => {
            param.mfx.InitialDelayInKB = fields.accuracy;
            param.mfx.TargetKbps = scale_kb_field_to_u16(fields.target_kbps, multiplier);
            param.mfx.MaxKbps = fields.convergence;
            param.mfx.BufferSizeInKB = 0;
        }
        RateControlMethod::La => {
            param.mfx.TargetKbps = scale_kb_field_to_u16(fields.target_kbps, multiplier);
            param.mfx.BufferSizeInKB = 0;
        }
        RateControlMethod::Icq | RateControlMethod::LaIcq => {
            param.mfx.TargetKbps = fields.icq_quality;
            param.mfx.BufferSizeInKB = 0;
        }
    }
}

fn apply_rate_control_config_to_ext_buffers(
    coding2: &mut MfxExtCodingOption2,
    coding3: &mut MfxExtCodingOption3,
    cfg: &RateControlConfig,
) {
    let fields = cfg.to_vpl_fields();
    let multiplier = fields.brc_param_multiplier.max(1);

    coding2.MaxFrameSize = fields.max_frame_size;
    coding2.MBBRC = if fields.mbbrc { MFX_CODINGOPTION_ON } else { 0 };
    coding2.ExtBRC = if fields.ext_brc {
        MFX_CODINGOPTION_ON
    } else {
        0
    };
    coding2.LookAheadDepth = fields.look_ahead_depth;

    coding3.WinBRCMaxAvgKbps = scale_kb_field_to_u16(fields.win_brc_max_avg_kbps, multiplier);
    coding3.WinBRCSize = fields.win_brc_size;
    coding3.QVBRQuality = fields.qvbr_quality;
    coding3.LowDelayBRC = if fields.low_delay_brc {
        MFX_CODINGOPTION_ON
    } else {
        0
    };
}

fn attach_rate_control_ext_params(
    param: &mut MfxVideoParam,
    coding2: &mut MfxExtCodingOption2,
    coding3: &mut MfxExtCodingOption3,
    ext_params: &mut [*mut c_void; 2],
) {
    let mut count = 0usize;
    if coding2.has_rate_control_overrides() {
        ext_params[count] = coding2 as *mut MfxExtCodingOption2 as *mut c_void;
        count += 1;
    }
    if coding3.has_rate_control_overrides() {
        ext_params[count] = coding3 as *mut MfxExtCodingOption3 as *mut c_void;
        count += 1;
    }
    if count > 0 {
        param.ExtParam = ext_params.as_mut_ptr();
        param.NumExtParam = count as u16;
    }
}

fn scale_kb_field_to_u16(value: u32, multiplier: u16) -> u16 {
    if value == 0 {
        0
    } else {
        value
            .div_ceil(u32::from(multiplier.max(1)))
            .min(u32::from(u16::MAX)) as u16
    }
}

unsafe fn bounded_slice<'a, T>(ptr: *const T, len: u16, max: usize) -> &'a [T] {
    if ptr.is_null() || len == 0 {
        &[]
    } else {
        std::slice::from_raw_parts(ptr, usize::from(len).min(max))
    }
}

fn c_char_array_to_string<const N: usize>(buf: &[c_char; N]) -> String {
    let bytes: Vec<u8> = buf
        .iter()
        .take_while(|&&c| c != 0)
        .map(|&c| c as u8)
        .collect();
    String::from_utf8_lossy(&bytes).trim().to_owned()
}

fn version_to_string(version: u32) -> String {
    let minor = version & 0xFFFF;
    let major = (version >> 16) & 0xFFFF;
    format!("{major}.{minor}")
}

const fn struct_version(major: u16, minor: u16) -> u16 {
    major * 256 + minor
}

fn acceleration_to_string(default_mode: u32, desc: &MfxAccelerationModeDescription) -> String {
    let mut modes = vec![acceleration_mode_name(default_mode).to_owned()];
    unsafe {
        for mode in bounded_slice(desc.Mode, desc.NumAccelerationModes, 32) {
            let name = acceleration_mode_name(*mode).to_owned();
            if !modes.contains(&name) {
                modes.push(name);
            }
        }
    }
    modes.join(", ")
}

fn acceleration_mode_name(mode: u32) -> &'static str {
    match mode {
        0 => "NA",
        0x0200 => "D3D9",
        MFX_ACCEL_MODE_VIA_D3D11 => "D3D11",
        0x0400 => "VAAPI",
        0x0401 => "VAAPI_DRM_MODESET",
        0x0402 => "VAAPI_GLX",
        0x0403 => "VAAPI_X11",
        0x0404 => "VAAPI_WAYLAND",
        0x0500 => "HDDLUNITE",
        _ => "未知",
    }
}

fn hevc_profile_name(profile: u32) -> &'static str {
    match profile {
        MFX_PROFILE_HEVC_MAIN => "HEVC Main",
        MFX_PROFILE_HEVC_MAIN10 => "HEVC Main10",
        MFX_PROFILE_HEVC_MAINSP => "HEVC Main Still Picture",
        MFX_PROFILE_HEVC_REXT => "HEVC RExt(含 422/444)",
        MFX_PROFILE_HEVC_SCC => "HEVC SCC",
        _ => "HEVC Unknown Profile",
    }
}

fn fourcc_to_string(value: u32) -> String {
    let bytes = value.to_le_bytes();
    if bytes.iter().all(|b| b.is_ascii_graphic() || *b == b' ') {
        String::from_utf8_lossy(&bytes).to_string()
    } else {
        format!("0x{value:08X}")
    }
}

fn fourcc_from_name(name: &str) -> Option<u32> {
    let bytes = name.as_bytes();
    if bytes.len() != 4 {
        return None;
    }
    Some(make_fourcc(bytes[0], bytes[1], bytes[2], bytes[3]))
}

fn chroma_from_fourcc_name(name: &str) -> Option<ChromaSampling> {
    match name {
        "NV12" | "P010" => Some(ChromaSampling::Yuv420),
        "YUY2" | "Y210" | "P210" => Some(ChromaSampling::Yuv422),
        "AYUV" | "Y410" | "RGB4" => Some(ChromaSampling::Yuv444),
        _ => None,
    }
}

fn chroma_from_vpl(value: u16) -> Option<ChromaSampling> {
    match value {
        1 => Some(ChromaSampling::Yuv420),
        2 => Some(ChromaSampling::Yuv422),
        3 => Some(ChromaSampling::Yuv444),
        _ => None,
    }
}

type MfxLoader = *mut c_void;
type MfxSession = *mut c_void;
type MfxHDL = *mut c_void;
type MfxSyncPoint = *mut c_void;

#[repr(C)]
#[derive(Debug, Clone, Copy)]
struct MfxStructVersion {
    version: u16,
}

#[repr(C)]
#[derive(Debug, Clone, Copy)]
struct MfxExtBuffer {
    BufferId: u32,
    BufferSz: u32,
}

#[repr(C)]
#[derive(Debug, Clone, Copy)]
struct MfxEncodeCtrl {
    Header: MfxExtBuffer,
    reserved: [u32; 4],
    reserved1: u16,
    MfxNalUnitType: u16,
    SkipFrame: u16,
    QP: u16,
    FrameType: u16,
    NumExtParam: u16,
    NumPayload: u16,
    reserved2: u16,
    ExtParam: *mut *mut c_void,
    Payload: *mut *mut c_void,
}

#[repr(C)]
#[derive(Debug, Clone, Copy)]
struct MfxExtVideoSignalInfo {
    Header: MfxExtBuffer,
    VideoFormat: u16,
    VideoFullRange: u16,
    ColourDescriptionPresent: u16,
    ColourPrimaries: u16,
    TransferCharacteristics: u16,
    MatrixCoefficients: u16,
}

impl MfxExtVideoSignalInfo {
    fn bt2020_pq_full() -> Self {
        Self::from_nclx(NclxColorMetadata::bt2020_pq_full())
    }

    fn from_nclx(color: NclxColorMetadata) -> Self {
        Self {
            Header: MfxExtBuffer {
                BufferId: MFX_EXTBUFF_VIDEO_SIGNAL_INFO,
                BufferSz: std::mem::size_of::<Self>() as u32,
            },
            // ITU-T H.265 video_format value 5 means "unspecified"; colour
            // description below carries the normative HDR signal identity.
            VideoFormat: 5,
            VideoFullRange: u16::from(color.full_range),
            ColourDescriptionPresent: 1,
            ColourPrimaries: color.colour_primaries,
            TransferCharacteristics: color.transfer_characteristics,
            MatrixCoefficients: color.matrix_coefficients,
        }
    }
}

#[repr(C)]
#[derive(Debug, Clone, Copy)]
struct MfxExtCodingOption2 {
    Header: MfxExtBuffer,
    IntRefType: u16,
    IntRefCycleSize: u16,
    IntRefQPDelta: i16,
    MaxFrameSize: u32,
    MaxSliceSize: u32,
    BitrateLimit: u16,
    MBBRC: u16,
    ExtBRC: u16,
    LookAheadDepth: u16,
    Trellis: u16,
    RepeatPPS: u16,
    BRefType: u16,
    AdaptiveI: u16,
    AdaptiveB: u16,
    LookAheadDS: u16,
    NumMbPerSlice: u16,
    SkipFrame: u16,
    MinQPI: u8,
    MaxQPI: u8,
    MinQPP: u8,
    MaxQPP: u8,
    MinQPB: u8,
    MaxQPB: u8,
    FixedFrameRate: u16,
    DisableDeblockingIdc: u16,
    DisableVUI: u16,
    BufferingPeriodSEI: u16,
    EnableMAD: u16,
    UseRawRef: u16,
}

impl MfxExtCodingOption2 {
    fn for_rate_control(rate_control: &RateControlConfig) -> Self {
        let mut out: Self = unsafe { std::mem::zeroed() };
        out.Header = MfxExtBuffer {
            BufferId: MFX_EXTBUFF_CODING_OPTION2,
            BufferSz: std::mem::size_of::<Self>() as u32,
        };
        let mut coding3 = MfxExtCodingOption3::empty();
        apply_rate_control_config_to_ext_buffers(&mut out, &mut coding3, rate_control);
        out
    }

    fn has_rate_control_overrides(&self) -> bool {
        self.MaxFrameSize != 0
            || self.MBBRC != 0
            || self.ExtBRC != 0
            || self.LookAheadDepth != 0
            || self.RepeatPPS != 0
    }
}

#[repr(C)]
#[derive(Debug, Clone, Copy)]
struct MfxExtCodingOption3 {
    Header: MfxExtBuffer,
    NumSliceI: u16,
    NumSliceP: u16,
    NumSliceB: u16,
    WinBRCMaxAvgKbps: u16,
    WinBRCSize: u16,
    QVBRQuality: u16,
    EnableMBQP: u16,
    IntRefCycleDist: u16,
    DirectBiasAdjustment: u16,
    GlobalMotionBiasAdjustment: u16,
    MVCostScalingFactor: u16,
    MBDisableSkipMap: u16,
    WeightedPred: u16,
    WeightedBiPred: u16,
    AspectRatioInfoPresent: u16,
    OverscanInfoPresent: u16,
    OverscanAppropriate: u16,
    TimingInfoPresent: u16,
    BitstreamRestriction: u16,
    LowDelayHrd: u16,
    MotionVectorsOverPicBoundaries: u16,
    reserved1: [u16; 2],
    ScenarioInfo: u16,
    ContentInfo: u16,
    PRefType: u16,
    FadeDetection: u16,
    reserved2: [u16; 2],
    GPB: u16,
    MaxFrameSizeI: u32,
    MaxFrameSizeP: u32,
    reserved3: [u32; 3],
    EnableQPOffset: u16,
    QPOffset: [i16; 8],
    NumRefActiveP: [u16; 8],
    NumRefActiveBL0: [u16; 8],
    NumRefActiveBL1: [u16; 8],
    reserved6: u16,
    TransformSkip: u16,
    TargetChromaFormatPlus1: u16,
    TargetBitDepthLuma: u16,
    TargetBitDepthChroma: u16,
    BRCPanicMode: u16,
    LowDelayBRC: u16,
    EnableMBForceIntra: u16,
    AdaptiveMaxFrameSize: u16,
    RepartitionCheckEnable: u16,
    reserved5: [u16; 3],
    EncodedUnitsInfo: u16,
    EnableNalUnitType: u16,
    AdaptiveLTR: u16,
    AdaptiveCQM: u16,
    AdaptiveRef: u16,
    reserved: [u16; 161],
}

impl MfxExtCodingOption3 {
    fn empty() -> Self {
        let mut out: Self = unsafe { std::mem::zeroed() };
        out.Header = MfxExtBuffer {
            BufferId: MFX_EXTBUFF_CODING_OPTION3,
            BufferSz: std::mem::size_of::<Self>() as u32,
        };
        out
    }

    fn for_rate_control(rate_control: &RateControlConfig) -> Self {
        let mut coding2 = MfxExtCodingOption2 {
            Header: MfxExtBuffer {
                BufferId: MFX_EXTBUFF_CODING_OPTION2,
                BufferSz: std::mem::size_of::<MfxExtCodingOption2>() as u32,
            },
            ..unsafe { std::mem::zeroed() }
        };
        let mut out = Self::empty();
        apply_rate_control_config_to_ext_buffers(&mut coding2, &mut out, rate_control);
        out
    }

    fn apply_route(&mut self, route: VplRecordRoute) {
        if u32::from(route.profile) == MFX_PROFILE_HEVC_REXT {
            self.TargetChromaFormatPlus1 = route.chroma.saturating_add(1);
            self.TargetBitDepthLuma = route.bit_depth;
            self.TargetBitDepthChroma = route.bit_depth;
        } else {
            self.TargetChromaFormatPlus1 = 0;
            self.TargetBitDepthLuma = 0;
            self.TargetBitDepthChroma = 0;
        }
    }

    fn has_rate_control_overrides(&self) -> bool {
        self.WinBRCMaxAvgKbps != 0
            || self.WinBRCSize != 0
            || self.QVBRQuality != 0
            || self.LowDelayBRC != 0
            || self.TargetChromaFormatPlus1 != 0
            || self.TargetBitDepthLuma != 0
            || self.TargetBitDepthChroma != 0
    }
}

struct VplEncodeExtBuffers {
    video_signal: MfxExtVideoSignalInfo,
    coding2: MfxExtCodingOption2,
    coding3: MfxExtCodingOption3,
    ext_params: [*mut c_void; 3],
}

impl VplEncodeExtBuffers {
    fn hdr_pq_full(rate_control: &RateControlConfig) -> Self {
        Self::for_route(VplRecordRoute::hdr_pq_p010(), rate_control)
    }

    fn for_route(route: VplRecordRoute, rate_control: &RateControlConfig) -> Self {
        let mut out = Self {
            video_signal: MfxExtVideoSignalInfo::from_nclx(route.mp4_color),
            coding2: MfxExtCodingOption2::for_rate_control(rate_control),
            coding3: MfxExtCodingOption3::for_rate_control(rate_control),
            ext_params: [ptr::null_mut(); 3],
        };
        out.coding2.RepeatPPS = MFX_CODINGOPTION_ON;
        out.coding3.apply_route(route);
        out
    }

    fn refresh(&mut self, route: VplRecordRoute, rate_control: &RateControlConfig) {
        self.video_signal = MfxExtVideoSignalInfo::from_nclx(route.mp4_color);
        apply_rate_control_config_to_ext_buffers(
            &mut self.coding2,
            &mut self.coding3,
            rate_control,
        );
        self.coding2.RepeatPPS = MFX_CODINGOPTION_ON;
        self.coding3.apply_route(route);
    }

    fn attach(&mut self, param: &mut MfxVideoParam) {
        let mut count = 0usize;
        self.ext_params[count] =
            &mut self.video_signal as *mut MfxExtVideoSignalInfo as *mut c_void;
        count += 1;
        if self.coding2.has_rate_control_overrides() {
            self.ext_params[count] = &mut self.coding2 as *mut MfxExtCodingOption2 as *mut c_void;
            count += 1;
        }
        if self.coding3.has_rate_control_overrides() {
            self.ext_params[count] = &mut self.coding3 as *mut MfxExtCodingOption3 as *mut c_void;
            count += 1;
        }
        param.ExtParam = self.ext_params.as_mut_ptr();
        param.NumExtParam = count as u16;
    }
}

#[repr(C)]
#[derive(Debug, Clone, Copy)]
struct MfxVersion {
    version: u32,
}

#[repr(C)]
#[derive(Debug, Clone, Copy)]
struct MfxRange32U {
    Min: u32,
    Max: u32,
    Step: u32,
}

#[repr(C)]
#[derive(Debug, Clone, Copy)]
struct MfxFrameId {
    TemporalId: u16,
    PriorityId: u16,
    DependencyId: u16,
    QualityId: u16,
}

#[repr(C)]
#[derive(Debug, Clone, Copy)]
struct MfxFrameInfo {
    reserved: [u32; 4],
    ChannelId: u16,
    BitDepthLuma: u16,
    BitDepthChroma: u16,
    Shift: u16,
    FrameId: MfxFrameId,
    FourCC: u32,
    Width: u16,
    Height: u16,
    CropX: u16,
    CropY: u16,
    CropW: u16,
    CropH: u16,
    FrameRateExtN: u32,
    FrameRateExtD: u32,
    reserved3: u16,
    AspectRatioW: u16,
    AspectRatioH: u16,
    PicStruct: u16,
    ChromaFormat: u16,
    reserved2: u16,
}

#[repr(C)]
#[derive(Debug, Clone, Copy)]
struct MfxInfoMFX {
    reserved: [u32; 7],
    LowPower: u16,
    BRCParamMultiplier: u16,
    FrameInfo: MfxFrameInfo,
    CodecId: u32,
    CodecProfile: u16,
    CodecLevel: u16,
    NumThread: u16,
    TargetUsage: u16,
    GopPicSize: u16,
    GopRefDist: u16,
    GopOptFlag: u16,
    IdrInterval: u16,
    RateControlMethod: u16,
    InitialDelayInKB: u16,
    BufferSizeInKB: u16,
    TargetKbps: u16,
    MaxKbps: u16,
    NumSlice: u16,
    NumRefFrame: u16,
    EncodedOrder: u16,
}

#[repr(C)]
#[derive(Debug, Clone, Copy)]
struct MfxVideoParam {
    AllocId: u32,
    reserved: [u32; 2],
    reserved3: u16,
    AsyncDepth: u16,
    mfx: MfxInfoMFX,
    union_padding: [u8; 32],
    Protected: u16,
    IOPattern: u16,
    ExtParam: *mut *mut c_void,
    NumExtParam: u16,
    reserved2: u16,
}

#[repr(C)]
#[derive(Debug, Clone, Copy)]
struct MfxFrameAllocRequest {
    AllocId: u32,
    reserved3: [u32; 3],
    Info: MfxFrameInfo,
    Type: u16,
    NumFrameMin: u16,
    NumFrameSuggested: u16,
    reserved2: u16,
}

#[repr(C)]
#[derive(Debug, Clone, Copy)]
struct MfxFrameData {
    ExtParam: *mut *mut c_void,
    NumExtParam: u16,
    reserved: [u16; 9],
    MemType: u16,
    PitchHigh: u16,
    TimeStamp: u64,
    FrameOrder: u32,
    Locked: u16,
    Pitch: u16,
    Y: *mut u8,
    UV: *mut u8,
    V: *mut u8,
    A: *mut u8,
    MemId: MfxHDL,
    Corrupted: u16,
    DataFlag: u16,
    reserved4: [u16; 2],
}

#[repr(C)]
#[derive(Debug, Clone, Copy)]
struct MfxFrameSurface1 {
    FrameInterface: *mut MfxFrameSurfaceInterface,
    Version: MfxStructVersion,
    reserved1: [u16; 3],
    Info: MfxFrameInfo,
    Data: MfxFrameData,
}

#[repr(C)]
#[derive(Clone, Copy)]
struct MfxFrameSurfaceInterface {
    Context: MfxHDL,
    Version: MfxStructVersion,
    reserved1: [u16; 3],
    AddRef: unsafe extern "C" fn(*mut MfxFrameSurface1) -> i32,
    Release: unsafe extern "C" fn(*mut MfxFrameSurface1) -> i32,
    GetRefCounter: unsafe extern "C" fn(*mut MfxFrameSurface1, *mut u32) -> i32,
    Map: unsafe extern "C" fn(*mut MfxFrameSurface1, u32) -> i32,
    Unmap: unsafe extern "C" fn(*mut MfxFrameSurface1) -> i32,
    GetNativeHandle: unsafe extern "C" fn(*mut MfxFrameSurface1, *mut MfxHDL, *mut u32) -> i32,
    GetDeviceHandle: unsafe extern "C" fn(*mut MfxFrameSurface1, *mut MfxHDL, *mut u32) -> i32,
    Synchronize: unsafe extern "C" fn(*mut MfxFrameSurface1, u32) -> i32,
    OnComplete: unsafe extern "C" fn(i32),
    QueryInterface: unsafe extern "C" fn(*mut MfxFrameSurface1, MfxGuid, *mut MfxHDL) -> i32,
    reserved2: [MfxHDL; 2],
}

impl std::fmt::Debug for MfxFrameSurfaceInterface {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("MfxFrameSurfaceInterface")
            .field("Context", &self.Context)
            .field("Version", &self.Version.version)
            .finish_non_exhaustive()
    }
}

#[repr(C)]
#[derive(Debug, Clone, Copy)]
struct MfxGuid {
    Data: [u8; 16],
}

#[repr(C)]
#[derive(Debug, Clone, Copy)]
struct MfxBitstream {
    EncryptedData: *mut c_void,
    ExtParam: *mut *mut c_void,
    NumExtParam: u16,
    reserved1: u16,
    CodecId: u32,
    DecodeTimeStamp: i64,
    TimeStamp: u64,
    Data: *mut u8,
    DataOffset: u32,
    DataLength: u32,
    MaxLength: u32,
    PicStruct: u16,
    FrameType: u16,
    DataFlag: u16,
    reserved2: u16,
}

#[repr(C)]
#[derive(Debug)]
struct MfxEncExtDescription {
    Version: MfxStructVersion,
    reserved: [u16; 10],
    NumRateControlMethods: u16,
    RateControlMethods: *const u16,
    reserved2: [u16; 11],
    NumExtBufferIDs: u16,
    ExtBufferIDs: *const u32,
}

#[repr(C)]
#[derive(Debug)]
struct MfxEncMemExtDescription {
    Version: MfxStructVersion,
    reserved: [u16; 13],
    TargetMaxBitDepth: u16,
    NumTargetChromaSubsamplings: u16,
    TargetChromaSubsamplings: *const u16,
}

#[repr(C)]
#[derive(Debug)]
struct MfxEncoderMemDesc {
    MemHandleType: u32,
    Width: MfxRange32U,
    Height: MfxRange32U,
    reserved: [u16; 2],
    MemExtDesc: *const MfxEncMemExtDescription,
    reserved3: u16,
    NumColorFormats: u16,
    ColorFormats: *const u32,
}

#[repr(C)]
#[derive(Debug)]
struct MfxEncoderProfile {
    Profile: u32,
    reserved: [u16; 7],
    NumMemTypes: u16,
    MemDesc: *const MfxEncoderMemDesc,
}

#[repr(C)]
#[derive(Debug)]
struct MfxEncoderCodec {
    CodecID: u32,
    MaxcodecLevel: u16,
    BiDirectionalPrediction: u16,
    EncExtDesc: *const MfxEncExtDescription,
    reserved: [u16; 3],
    NumProfiles: u16,
    Profiles: *const MfxEncoderProfile,
}

#[repr(C)]
#[derive(Debug)]
struct MfxEncoderDescription {
    Version: MfxStructVersion,
    reserved: [u16; 7],
    NumCodecs: u16,
    Codecs: *const MfxEncoderCodec,
}

#[repr(C)]
#[derive(Debug)]
struct MfxDecoderDescription {
    Version: MfxStructVersion,
    reserved: [u16; 7],
    NumCodecs: u16,
    Codecs: *const c_void,
}

#[repr(C)]
#[derive(Debug)]
struct MfxVppDescription {
    Version: MfxStructVersion,
    reserved: [u16; 7],
    NumFilters: u16,
    Filters: *const c_void,
}

#[repr(C)]
#[derive(Debug)]
struct MfxDeviceDescription {
    Version: MfxStructVersion,
    reserved: [u16; 6],
    MediaAdapterType: u16,
    DeviceID: [c_char; 128],
    NumSubDevices: u16,
    SubDevices: *const c_void,
}

#[repr(C)]
#[derive(Debug)]
struct MfxAccelerationModeDescription {
    Version: MfxStructVersion,
    reserved: [u16; 2],
    NumAccelerationModes: u16,
    Mode: *const u32,
}

#[repr(C)]
#[derive(Debug)]
struct MfxPoolPolicyDescription {
    Version: MfxStructVersion,
    reserved: [u16; 2],
    NumPoolPolicies: u16,
    Policy: *const u32,
}

#[repr(C)]
#[derive(Debug)]
struct MfxImplDescription {
    Version: MfxStructVersion,
    Impl: u32,
    AccelerationMode: u32,
    ApiVersion: MfxVersion,
    ImplName: [c_char; 32],
    License: [c_char; 128],
    Keywords: [c_char; 128],
    VendorID: u32,
    VendorImplID: u32,
    Dev: MfxDeviceDescription,
    Dec: MfxDecoderDescription,
    Enc: MfxEncoderDescription,
    VPP: MfxVppDescription,
    AccelerationModeDescription: MfxAccelerationModeDescription,
    PoolPolicies: MfxPoolPolicyDescription,
    reserved: [u32; 8],
    NumExtParam: u32,
    ExtParam: *const c_void,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn fourcc_roundtrip() {
        assert_eq!(fourcc_to_string(MFX_FOURCC_NV12), "NV12");
        assert_eq!(fourcc_to_string(MFX_FOURCC_P010), "P010");
        assert_eq!(fourcc_to_string(MFX_FOURCC_YUY2), "YUY2");
        assert_eq!(fourcc_to_string(MFX_FOURCC_Y210), "Y210");
        assert_eq!(fourcc_to_string(MFX_FOURCC_P210), "P210");
        assert_eq!(fourcc_to_string(MFX_FOURCC_AYUV), "AYUV");
        assert_eq!(fourcc_to_string(MFX_FOURCC_Y410), "Y410");
        assert_eq!(fourcc_to_string(MFX_FOURCC_RGB4), "RGB4");
    }

    #[test]
    fn struct_version_matches_header_macro() {
        assert_eq!(struct_version(1, 2), 258);
    }

    #[test]
    fn ffi_struct_sizes_match_onevpl_headers() {
        assert_eq!(std::mem::size_of::<MfxFrameInfo>(), 68);
        assert_eq!(std::mem::size_of::<MfxInfoMFX>(), 136);
        assert_eq!(std::mem::size_of::<MfxVideoParam>(), 208);
        assert_eq!(std::mem::size_of::<MfxBitstream>(), 72);
        assert_eq!(std::mem::size_of::<MfxFrameData>(), 96);
        assert_eq!(std::mem::size_of::<MfxFrameSurface1>(), 184);
        assert_eq!(std::mem::size_of::<MfxEncodeCtrl>(), 56);
        assert_eq!(std::mem::size_of::<MfxExtVideoSignalInfo>(), 20);
        assert_eq!(std::mem::size_of::<MfxExtCodingOption2>(), 68);
        assert_eq!(std::mem::size_of::<MfxExtCodingOption3>(), 512);
        assert_eq!(std::mem::offset_of!(MfxEncodeCtrl, FrameType), 32);
        assert_eq!(std::mem::offset_of!(MfxEncodeCtrl, ExtParam), 40);
        assert_eq!(std::mem::offset_of!(MfxExtCodingOption2, MaxFrameSize), 16);
        assert_eq!(std::mem::offset_of!(MfxExtCodingOption2, MBBRC), 26);
        assert_eq!(std::mem::offset_of!(MfxExtCodingOption2, ExtBRC), 28);
        assert_eq!(
            std::mem::offset_of!(MfxExtCodingOption2, LookAheadDepth),
            30
        );
        assert_eq!(
            std::mem::offset_of!(MfxExtCodingOption3, WinBRCMaxAvgKbps),
            14
        );
        assert_eq!(std::mem::offset_of!(MfxExtCodingOption3, WinBRCSize), 16);
        assert_eq!(std::mem::offset_of!(MfxExtCodingOption3, QVBRQuality), 18);
        assert_eq!(std::mem::offset_of!(MfxExtCodingOption3, LowDelayBRC), 166);
    }

    #[test]
    fn dll_candidates_include_env_first() {
        assert!(candidate_dlls().iter().any(|p| p.ends_with("libvpl-2.dll")));
    }

    #[test]
    fn rate_control_config_writes_union_and_ext_fields() {
        let cfg = RateControlConfig {
            method: RateControlMethod::Qvbr,
            brc_param_multiplier: 2,
            target_kbps: 100_000,
            max_kbps: 120_000,
            buffer_size_kb: 60_000,
            initial_delay_kb: 10_000,
            qvbr_quality: 17,
            win_brc_max_avg_kbps: 90_000,
            win_brc_size: 144,
            low_delay_brc: true,
            max_frame_size: 200_000,
            mbbrc: true,
            ..RateControlConfig::default()
        };
        let param = make_query_param(&cfg, MFX_FOURCC_P010, 1, 10, 2);
        assert_eq!(
            param.mfx.RateControlMethod,
            RateControlMethod::Qvbr.vpl_value()
        );
        assert_eq!(param.mfx.BRCParamMultiplier, 2);
        assert_eq!(param.mfx.TargetKbps, 50_000);
        assert_eq!(param.mfx.MaxKbps, 60_000);
        assert_eq!(param.mfx.BufferSizeInKB, 30_000);
        assert_eq!(param.mfx.InitialDelayInKB, 5_000);

        let mut co2 = MfxExtCodingOption2::for_rate_control(&cfg);
        let mut co3 = MfxExtCodingOption3::for_rate_control(&cfg);
        apply_rate_control_config_to_ext_buffers(&mut co2, &mut co3, &cfg);
        assert_eq!(co2.MaxFrameSize, 200_000);
        assert_eq!(co2.MBBRC, MFX_CODINGOPTION_ON);
        assert_eq!(co3.WinBRCMaxAvgKbps, 45_000);
        assert_eq!(co3.WinBRCSize, 144);
        assert_eq!(co3.QVBRQuality, 17);
        assert_eq!(co3.LowDelayBRC, MFX_CODINGOPTION_ON);
    }
}
