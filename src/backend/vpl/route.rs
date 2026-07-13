use super::*;

pub(super) const MFX_ERR_NONE: i32 = 0;
pub(super) const MFX_ERR_NOT_FOUND: i32 = -9;
pub(super) const MFX_ERR_MORE_DATA: i32 = -10;
pub(super) const MFX_ERR_MORE_SURFACE: i32 = -11;
pub(super) const MFX_ERR_NOT_IMPLEMENTED: i32 = -24;
pub(super) const MFX_WRN_IN_EXECUTION: i32 = 1;
pub(super) const MFX_WRN_DEVICE_BUSY: i32 = 2;
pub(super) const MFX_WRN_PARTIAL_ACCELERATION: i32 = 4;
pub(super) const MFX_IMPLCAPS_IMPLDESCSTRUCTURE: u32 = 1;
pub(super) const MFX_IMPL_TYPE_HARDWARE: u32 = 0x0002;
pub(super) const MFX_ACCEL_MODE_VIA_D3D11: u32 = 0x0300;
pub(super) const MFX_RESOURCE_DX11_TEXTURE: u32 = 5;
pub(super) const MFX_IOPATTERN_IN_VIDEO_MEMORY: u16 = 0x01;
pub(super) const MFX_PICSTRUCT_PROGRESSIVE: u16 = 0x01;
pub(super) const MFX_HANDLE_D3D11_DEVICE: u32 = 3;
pub(super) const VIDEO_CLOCK_HZ: u64 = 90_000;
pub(super) const VPL_RECORD_ASYNC_DEPTH: u16 = 16;
pub(super) const VPL_RECORD_MAX_IN_FLIGHT: usize = 64;
pub(super) const VPL_BITSTREAM_BYTES: usize = 64 * 1024 * 1024;
pub(super) const MFX_CODINGOPTION_ON: u16 = 0x10;
pub(super) const MFX_CODINGOPTION_OFF: u16 = 0x20;
pub(super) const MFX_FRAMETYPE_I: u16 = 0x0001;
pub(super) const MFX_FRAMETYPE_REF: u16 = 0x0040;
pub(super) const MFX_FRAMETYPE_IDR: u16 = 0x0080;
pub(super) const MFX_EXTBUFF_CODING_OPTION2: u32 = make_fourcc(b'C', b'D', b'O', b'2');
pub(super) const MFX_EXTBUFF_CODING_OPTION3: u32 = make_fourcc(b'C', b'D', b'O', b'3');
pub(super) const MFX_EXTBUFF_VIDEO_SIGNAL_INFO: u32 = make_fourcc(b'V', b'S', b'I', b'N');
pub(super) const VPL_ENCODER_FRAME_RATE_HINT_FALLBACK_N: u32 = 60;
pub(super) const VPL_ENCODER_FRAME_RATE_HINT_FALLBACK_D: u32 = 1;
pub(super) const ENCODER_WARMUP_TIMESTAMP_STEP_90K: u64 = 1;

pub(super) const MFX_CODEC_HEVC: u32 = make_fourcc(b'H', b'E', b'V', b'C');
pub(super) const MFX_FOURCC_NV12: u32 = make_fourcc(b'N', b'V', b'1', b'2');
pub(super) const MFX_FOURCC_YUY2: u32 = make_fourcc(b'Y', b'U', b'Y', b'2');
pub(super) const MFX_FOURCC_RGB4: u32 = make_fourcc(b'R', b'G', b'B', b'4');
pub(super) const MFX_FOURCC_P010: u32 = make_fourcc(b'P', b'0', b'1', b'0');
pub(super) const MFX_FOURCC_P210: u32 = make_fourcc(b'P', b'2', b'1', b'0');
pub(super) const MFX_FOURCC_AYUV: u32 = make_fourcc(b'A', b'Y', b'U', b'V');
pub(super) const MFX_FOURCC_Y210: u32 = make_fourcc(b'Y', b'2', b'1', b'0');
pub(super) const MFX_FOURCC_Y410: u32 = make_fourcc(b'Y', b'4', b'1', b'0');

pub(super) const MFX_PROFILE_HEVC_MAIN: u32 = 1;
pub(super) const MFX_PROFILE_HEVC_MAIN10: u32 = 2;
pub(super) const MFX_PROFILE_HEVC_MAINSP: u32 = 3;
pub(super) const MFX_PROFILE_HEVC_REXT: u32 = 4;
pub(super) const MFX_PROFILE_HEVC_SCC: u32 = 9;

#[derive(Debug, Clone, Copy)]
pub(super) struct VplRecordRoute {
    pub(super) label: &'static str,
    pub(super) fourcc: u32,
    pub(super) chroma: u16,
    pub(super) bit_depth: u16,
    pub(super) profile: u16,
    pub(super) mp4_color: NclxColorMetadata,
    pub(super) mp4_codec: HevcCodecMetadata,
}

#[derive(Debug, Clone, Copy)]
pub(super) struct DisplayRouteColor {
    pub(super) hdr_pq: bool,
    pub(super) bit_depth: u16,
    pub(super) full_range: bool,
    pub(super) mp4_color: NclxColorMetadata,
    pub(super) note: &'static str,
}

impl VplRecordRoute {
    pub(super) const fn sdr8_nv12() -> Self {
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

    pub(super) const fn sdr8_yuy2() -> Self {
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

    pub(super) const fn sdr10_y210() -> Self {
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

    pub(super) const fn hdr_pq_y210() -> Self {
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

    pub(super) const fn sdr10_p010() -> Self {
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

    pub(super) const fn sdr10_p210() -> Self {
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

    pub(super) const fn sdr8_ayuv() -> Self {
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

    pub(super) const fn sdr10_y410() -> Self {
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

    pub(super) const fn hdr_pq_y410() -> Self {
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

    pub(super) const fn sdr8_rgb4() -> Self {
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

    pub(super) fn query_candidates() -> [Self; 8] {
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

    pub(super) const fn hdr_pq_p010() -> Self {
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

    pub(super) fn summary(self) -> String {
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

    pub(super) fn is_hdr_pq(self) -> bool {
        self.mp4_color.transfer_characteristics == 16
    }

    pub(super) fn is_bt2020_sdr(self) -> bool {
        self.mp4_color.colour_primaries == 9
            && matches!(self.mp4_color.transfer_characteristics, 1 | 14)
            && self.mp4_color.matrix_coefficients == 9
    }

    pub(super) fn requires_fp16_capture(self) -> bool {
        self.bit_depth >= 10
    }

    pub(super) fn supports_requested_chroma(self, requested: ChromaSampling) -> bool {
        matches!(
            (requested, self.chroma),
            (ChromaSampling::Yuv420, 1) | (ChromaSampling::Yuv422, 2) | (ChromaSampling::Yuv444, 3)
        )
    }

    pub(super) fn candidates_for_requested_chroma_and_display(
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

    pub(super) fn production_gpu_writer_available(self) -> bool {
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

    pub(super) fn production_gpu_writer_blocker(self) -> Option<&'static str> {
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

    pub(super) const fn with_color(mut self, color: NclxColorMetadata) -> Self {
        self.mp4_color = color;
        self
    }
}

#[cfg(windows)]
pub(super) fn select_record_route_candidates_for_output(
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
pub(super) fn select_record_route_for_output(
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

pub(super) fn record_route_from_display_plan(
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
pub(super) fn validate_current_display_route_plan(
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
    pub(super) fn try_dxgi_format(
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

    pub(super) fn wgc_input_format(
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

    pub(super) fn accepts_unconverted_capture_format(
        self,
        format: windows::Win32::Graphics::Dxgi::Common::DXGI_FORMAT,
    ) -> bool {
        use windows::Win32::Graphics::Dxgi::Common::DXGI_FORMAT_R16G16B16A16_FLOAT;
        !self.requires_fp16_capture() || format == DXGI_FORMAT_R16G16B16A16_FLOAT
    }
}

pub(super) const fn make_fourcc(a: u8, b: u8, c: u8, d: u8) -> u32 {
    (a as u32) | ((b as u32) << 8) | ((c as u32) << 16) | ((d as u32) << 24)
}
