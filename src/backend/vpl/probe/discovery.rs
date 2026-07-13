use super::*;

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
    pub(in super::super) fn unavailable(error: String) -> Self {
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
pub(in super::super) fn probe_current_display_record_routes(
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
pub(in super::super) fn probe_current_display_record_routes(
    _adapter_index: u32,
) -> Result<Vec<VplCurrentDisplayRouteInfo>, BackendError> {
    Ok(Vec::new())
}
