use super::*;

pub(in super::super) fn route_candidates_from_fourcc(
    input_fourcc: &BTreeSet<String>,
) -> Vec<VplRecordRoute> {
    VplRecordRoute::query_candidates()
        .into_iter()
        .filter(|route| input_fourcc.contains(&fourcc_to_string(route.fourcc)))
        .collect()
}

pub(in super::super) fn route_probe_key(route: VplRecordRoute) -> String {
    route_probe_key_parts(
        &fourcc_to_string(route.fourcc),
        chroma_from_vpl(route.chroma).unwrap_or(ChromaSampling::Yuv420),
        route.bit_depth,
        hevc_profile_name(u32::from(route.profile)),
    )
}

pub(in super::super) fn route_probe_key_parts(
    fourcc: &str,
    chroma: ChromaSampling,
    bit_depth: u16,
    profile: &str,
) -> String {
    format!("{fourcc}::{chroma:?}::{bit_depth}::{profile}")
}

pub(in super::super) fn choose_query_format(
    input_fourcc: &BTreeSet<String>,
) -> (u32, u16, u16, u16) {
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

pub(in super::super) fn make_query_param(
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
    param.mfx.GopPicSize = u16::MAX;
    param.mfx.GopRefDist = 1;
    param.mfx.IdrInterval = 1;
    apply_rate_control_config_to_param(&mut param, rate_control);
    param
}

pub(in super::super) fn validate_rate_control_config(
    rate_control: &RateControlConfig,
) -> Result<(), BackendError> {
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

pub(in super::super) fn validate_scaled_field(
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

pub(in super::super) fn apply_record_route_to_param(
    param: &mut MfxVideoParam,
    route: VplRecordRoute,
) {
    param.mfx.FrameInfo.FourCC = route.fourcc;
    param.mfx.FrameInfo.ChromaFormat = route.chroma;
    param.mfx.FrameInfo.BitDepthLuma = route.bit_depth;
    param.mfx.FrameInfo.BitDepthChroma = route.bit_depth;
    param.mfx.CodecProfile = route.profile;
}

pub(in super::super) fn query_output_preserves_record_route(
    param: &MfxVideoParam,
    route: VplRecordRoute,
) -> bool {
    param.mfx.FrameInfo.FourCC == route.fourcc
        && param.mfx.FrameInfo.ChromaFormat == route.chroma
        && param.mfx.FrameInfo.BitDepthLuma == route.bit_depth
        && param.mfx.FrameInfo.BitDepthChroma == route.bit_depth
        && param.mfx.CodecProfile == route.profile
}

pub(in super::super) fn apply_rate_control_config_to_param(
    param: &mut MfxVideoParam,
    cfg: &RateControlConfig,
) {
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

pub(in super::super) fn apply_rate_control_config_to_ext_buffers(
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

pub(in super::super) fn attach_rate_control_ext_params(
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

pub(in super::super) fn scale_kb_field_to_u16(value: u32, multiplier: u16) -> u16 {
    if value == 0 {
        0
    } else {
        value
            .div_ceil(u32::from(multiplier.max(1)))
            .min(u32::from(u16::MAX)) as u16
    }
}

pub(in super::super) unsafe fn bounded_slice<'a, T>(
    ptr: *const T,
    len: u16,
    max: usize,
) -> &'a [T] {
    if ptr.is_null() || len == 0 {
        &[]
    } else {
        std::slice::from_raw_parts(ptr, usize::from(len).min(max))
    }
}

pub(in super::super) fn c_char_array_to_string<const N: usize>(buf: &[c_char; N]) -> String {
    let bytes: Vec<u8> = buf
        .iter()
        .take_while(|&&c| c != 0)
        .map(|&c| c as u8)
        .collect();
    String::from_utf8_lossy(&bytes).trim().to_owned()
}

pub(in super::super) fn version_to_string(version: u32) -> String {
    let minor = version & 0xFFFF;
    let major = (version >> 16) & 0xFFFF;
    format!("{major}.{minor}")
}

pub(in super::super) const fn struct_version(major: u16, minor: u16) -> u16 {
    major * 256 + minor
}

pub(in super::super) fn acceleration_to_string(
    default_mode: u32,
    desc: &MfxAccelerationModeDescription,
) -> String {
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

pub(in super::super) fn acceleration_mode_name(mode: u32) -> &'static str {
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

pub(in super::super) fn hevc_profile_name(profile: u32) -> &'static str {
    match profile {
        MFX_PROFILE_HEVC_MAIN => "HEVC Main",
        MFX_PROFILE_HEVC_MAIN10 => "HEVC Main10",
        MFX_PROFILE_HEVC_MAINSP => "HEVC Main Still Picture",
        MFX_PROFILE_HEVC_REXT => "HEVC RExt(含 422/444)",
        MFX_PROFILE_HEVC_SCC => "HEVC SCC",
        _ => "HEVC Unknown Profile",
    }
}

pub(in super::super) fn fourcc_to_string(value: u32) -> String {
    let bytes = value.to_le_bytes();
    if bytes.iter().all(|b| b.is_ascii_graphic() || *b == b' ') {
        String::from_utf8_lossy(&bytes).to_string()
    } else {
        format!("0x{value:08X}")
    }
}

pub(in super::super) fn fourcc_from_name(name: &str) -> Option<u32> {
    let bytes = name.as_bytes();
    if bytes.len() != 4 {
        return None;
    }
    Some(make_fourcc(bytes[0], bytes[1], bytes[2], bytes[3]))
}

pub(in super::super) fn chroma_from_fourcc_name(name: &str) -> Option<ChromaSampling> {
    match name {
        "NV12" | "P010" => Some(ChromaSampling::Yuv420),
        "YUY2" | "Y210" | "P210" => Some(ChromaSampling::Yuv422),
        "AYUV" | "Y410" | "RGB4" => Some(ChromaSampling::Yuv444),
        _ => None,
    }
}

pub(in super::super) fn chroma_from_vpl(value: u16) -> Option<ChromaSampling> {
    match value {
        1 => Some(ChromaSampling::Yuv420),
        2 => Some(ChromaSampling::Yuv422),
        3 => Some(ChromaSampling::Yuv444),
        _ => None,
    }
}
