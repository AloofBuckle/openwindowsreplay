use super::*;
use std::collections::BTreeMap;

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
    pub adapter_luid_low: Option<u32>,
    pub adapter_luid_high: Option<i32>,
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
    /// oneVPL implementation index verified during capability probing.
    pub implementation_index: u32,
    pub adapter_index: u32,
    pub output_index: u32,
    pub rotation: u32,
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

#[derive(Debug, Clone, Copy)]
pub(in super::super) struct VplProbeDimensions {
    pub width: u16,
    pub height: u16,
    pub crop_width: u16,
    pub crop_height: u16,
}

impl VplProbeDimensions {
    fn from_display_routes(routes: &[VplCurrentDisplayRouteInfo]) -> Self {
        let route = routes.iter().min_by_key(|route| {
            let primary = route.desktop_left <= 0
                && route.desktop_right > 0
                && route.desktop_top <= 0
                && route.desktop_bottom > 0;
            (!primary, route.adapter_index, route.output_index)
        });
        let crop_width = route
            .map(|route| (route.desktop_right - route.desktop_left).max(1) as u32)
            .unwrap_or(1920)
            .min(u32::from(u16::MAX)) as u16;
        let crop_height = route
            .map(|route| (route.desktop_bottom - route.desktop_top).max(1) as u32)
            .unwrap_or(1080)
            .min(u32::from(u16::MAX)) as u16;
        Self {
            width: (u32::from(crop_width).div_ceil(16) * 16).min(u32::from(u16::MAX) & !15) as u16,
            height: (u32::from(crop_height).div_ceil(16) * 16).min(u32::from(u16::MAX) & !15)
                as u16,
            crop_width,
            crop_height,
        }
    }
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
    let dxgi_adapters = match crate::backend::dxgi::enumerate_adapters() {
        Ok(adapters) => adapters,
        Err(err) => {
            warnings.push(format!("oneVPL DXGI adapter 枚举失败：{err}"));
            Vec::new()
        }
    };
    let mut display_route_candidates = Vec::new();
    for adapter in dxgi_adapters
        .iter()
        .filter(|adapter| adapter.flags & 0x2 == 0)
    {
        match probe_current_display_record_routes(adapter.index) {
            Ok(mut routes) => display_route_candidates.append(&mut routes),
            Err(err) => warnings.push(format!(
                "adapter={} 当前显示器自动 route 探测失败：{err}",
                adapter.index
            )),
        }
    }
    let current_display_route_keys = display_route_candidates
        .iter()
        .filter(|route| !route.fourcc.is_empty())
        .map(|route| {
            route_probe_key_parts(&route.fourcc, route.chroma, route.bit_depth, &route.profile)
        })
        .collect::<BTreeSet<_>>();
    let probe_dimensions = VplProbeDimensions::from_display_routes(&display_route_candidates);

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
            let adapter_luid =
                query_implementation_adapter_luid(&api, loader, index, &mut warnings);
            let mut implementation = parse_impl(
                index,
                desc,
                &api,
                loader,
                &current_display_route_keys,
                probe_dimensions,
                &mut warnings,
            );
            implementation.adapter_luid_low = adapter_luid.map(|(low, _)| low);
            implementation.adapter_luid_high = adapter_luid.map(|(_, high)| high);
            implementations.push(implementation);

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
    let current_display_routes = resolve_display_routes_for_implementations(
        &display_route_candidates,
        &dxgi_adapters,
        &implementations,
        &mut warnings,
    );

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
        let mut routes = Vec::new();
        let mut output_index = 0u32;
        loop {
            let output = match adapter.EnumOutputs(output_index) {
                Ok(output) => output,
                Err(err) if err.code() == DXGI_ERROR_NOT_FOUND => break,
                Err(err) => {
                    return Err(BackendError::WindowsApi {
                        func: "IDXGIAdapter1::EnumOutputs(current display route)",
                        message: err.to_string(),
                    });
                }
            };
            let output_desc = output.GetDesc().map_err(|err| BackendError::WindowsApi {
                func: "IDXGIOutput::GetDesc(current display route)",
                message: err.to_string(),
            })?;
            if !output_desc.AttachedToDesktop.as_bool() {
                output_index = output_index.saturating_add(1);
                continue;
            }
            let output6 = output.cast::<IDXGIOutput6>();
            let desc1 = output6.and_then(|output6| output6.GetDesc1());
            let rotation = output_desc.Rotation.0 as u32;
            let rotation_supported = rotation == DXGI_MODE_ROTATION_UNSPECIFIED.0 as u32
                || rotation == DXGI_MODE_ROTATION_IDENTITY.0 as u32;
            for chroma in ChromaSampling::all() {
                let result = if !rotation_supported {
                    Err(BackendError::unsupported(
                        "当前显示器 route 探测",
                        format!(
                            "adapter={} output={} rotation={}",
                            adapter_index, output_index, rotation
                        ),
                        "不支持的桌面模式",
                    ))
                } else if let Err(err) = &desc1 {
                    Err(BackendError::WindowsApi {
                        func: "IDXGIOutput6::GetDesc1(current display route)",
                        message: err.to_string(),
                    })
                } else {
                    let mut notes = Vec::new();
                    select_record_route_candidates_for_output(&output, chroma, &mut notes)
                        .map(|candidates| (candidates, notes))
                };
                match result {
                    Ok((candidates, notes)) => {
                        let desc1 = desc1.as_ref().expect("successful route has output6 desc");
                        for (index, route) in candidates.into_iter().enumerate() {
                            routes.push(VplCurrentDisplayRouteInfo {
                                implementation_index: u32::MAX,
                                adapter_index,
                                output_index,
                                rotation,
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
                                nclx_transfer_characteristics: route
                                    .mp4_color
                                    .transfer_characteristics,
                                nclx_matrix_coefficients: route.mp4_color.matrix_coefficients,
                                nclx_full_range: route.mp4_color.full_range,
                                codec_profile_idc: route.mp4_codec.profile_idc,
                                codec_chroma_format_idc: route.mp4_codec.chroma_format_idc,
                                codec_bit_depth_luma_minus8: route.mp4_codec.bit_depth_luma_minus8,
                                codec_bit_depth_chroma_minus8: route
                                    .mp4_codec
                                    .bit_depth_chroma_minus8,
                                route_summary: route.summary(),
                                note: format!(
                                    "{}；candidate_order={}",
                                    notes.join("；"),
                                    index + 1
                                ),
                            });
                        }
                    }
                    Err(err) => {
                        let (color_space, bits_per_color) = desc1
                            .as_ref()
                            .map(|desc| (desc.ColorSpace.0 as u32, desc.BitsPerColor))
                            .unwrap_or((0, 0));
                        routes.push(VplCurrentDisplayRouteInfo {
                            implementation_index: u32::MAX,
                            adapter_index,
                            output_index,
                            rotation,
                            color_space,
                            bits_per_color,
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
                            route_summary: format!(
                                "{} 当前显示状态无可用 route",
                                chroma.doc_label()
                            ),
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

unsafe fn query_implementation_adapter_luid(
    api: &VplApi,
    loader: MfxLoader,
    implementation_index: u32,
    warnings: &mut Vec<String>,
) -> Option<(u32, i32)> {
    let mut handle: MfxHDL = ptr::null_mut();
    let status = (api.mfx_enum_implementations)(
        loader,
        implementation_index,
        MFX_IMPLCAPS_DEVICE_ID_EXTENDED,
        &mut handle,
    );
    if status != MFX_ERR_NONE || handle.is_null() {
        if !handle.is_null() {
            let _ = (api.mfx_release_impl_description)(loader, handle);
        }
        warnings.push(format!(
            "MFXEnumImplementations({implementation_index}, DEVICE_ID_EXTENDED) 未提供 DXGI LUID：status={status}"
        ));
        return None;
    }

    let extended = &*(handle as *const MfxExtendedDeviceId);
    let luid = if extended.LUIDValid != 0 {
        Some((
            u32::from_ne_bytes(
                extended.DeviceLUID[0..4]
                    .try_into()
                    .expect("LUID low bytes"),
            ),
            i32::from_ne_bytes(
                extended.DeviceLUID[4..8]
                    .try_into()
                    .expect("LUID high bytes"),
            ),
        ))
    } else {
        warnings.push(format!(
            "oneVPL implementation {implementation_index} 的 extended device ID 未标记有效 DXGI LUID"
        ));
        None
    };
    let release_status = (api.mfx_release_impl_description)(loader, handle);
    if release_status != MFX_ERR_NONE {
        warnings.push(format!(
            "MFXDispReleaseImplDescription({implementation_index}, DEVICE_ID_EXTENDED) 返回 status={release_status}"
        ));
    }
    luid
}

fn resolve_display_routes_for_implementations(
    display_routes: &[VplCurrentDisplayRouteInfo],
    adapters: &[crate::backend::dxgi::DxgiAdapterInfo],
    implementations: &[VplImplementationInfo],
    warnings: &mut Vec<String>,
) -> Vec<VplCurrentDisplayRouteInfo> {
    let mut grouped =
        BTreeMap::<(u32, u32, ChromaSampling), Vec<&VplCurrentDisplayRouteInfo>>::new();
    for route in display_routes {
        grouped
            .entry((route.adapter_index, route.output_index, route.chroma))
            .or_default()
            .push(route);
    }

    let mut resolved = Vec::new();
    let mut fallback_warned = BTreeSet::new();
    for ((adapter_index, output_index, chroma), routes) in grouped {
        let adapter = adapters
            .iter()
            .find(|adapter| adapter.index == adapter_index);
        let matched = routes.iter().find_map(|display_route| {
            if display_route.fourcc.is_empty() {
                return None;
            }
            implementations.iter().find_map(|implementation| {
                let same_adapter = adapter.is_some_and(|adapter| {
                    implementation_matches_adapter(adapter, adapters, implementation)
                });
                if !same_adapter
                    || implementation.implementation != "硬件"
                    || !implementation.dx11_texture_input_seen
                {
                    return None;
                }
                implementation
                    .route_candidates
                    .iter()
                    .any(|candidate| {
                        candidate.production_record_supported
                            && candidate.fourcc == display_route.fourcc
                            && candidate.chroma == display_route.chroma
                            && candidate.bit_depth == display_route.bit_depth
                            && candidate.profile == display_route.profile
                    })
                    .then_some((*display_route, implementation.index))
            })
        });

        if let Some((route, implementation_index)) = matched {
            let mut route = route.clone();
            route.implementation_index = implementation_index;
            route.note = format!("implementation={}；{}", implementation_index, route.note);
            if implementations
                .iter()
                .find(|implementation| implementation.index == implementation_index)
                .is_some_and(|implementation| implementation.adapter_luid_low.is_none())
                && fallback_warned.insert(implementation_index)
            {
                warnings.push(format!(
                    "oneVPL implementation {implementation_index} 未提供 DXGI LUID；当前同厂商仅有一个 DXGI adapter，使用唯一厂商匹配兼容路径"
                ));
            }
            resolved.push(route);
        } else if let Some(route) = routes.first() {
            let mut unavailable = (*route).clone();
            unavailable.implementation_index = u32::MAX;
            unavailable.fourcc.clear();
            unavailable.bit_depth = 0;
            unavailable.vpl_chroma = 0;
            unavailable.vpl_profile = 0;
            unavailable.profile.clear();
            unavailable.route_summary = format!(
                "{} adapter={} output={} 无同设备 oneVPL implementation",
                chroma.doc_label(),
                adapter_index,
                output_index
            );
            unavailable.note = format!(
                "{}；oneVPL 硬件 implementation 的 DXGI LUID 与该 output adapter 不匹配，或旧 dispatcher 下存在同厂商多适配器歧义，禁止跨 GPU 桌面同步录制",
                unavailable.note
            );
            warnings.push(unavailable.note.clone());
            resolved.push(unavailable);
        }
    }
    resolved.sort_by_key(|route| {
        let primary = route.desktop_left <= 0
            && route.desktop_right > 0
            && route.desktop_top <= 0
            && route.desktop_bottom > 0;
        (
            !primary,
            route.adapter_index,
            route.output_index,
            route.chroma,
        )
    });
    resolved
}

fn implementation_matches_adapter(
    adapter: &crate::backend::dxgi::DxgiAdapterInfo,
    adapters: &[crate::backend::dxgi::DxgiAdapterInfo],
    implementation: &VplImplementationInfo,
) -> bool {
    match (
        implementation.adapter_luid_low,
        implementation.adapter_luid_high,
    ) {
        (Some(low), Some(high)) => adapter.luid_low == low && adapter.luid_high == high,
        (None, None) => {
            adapter.vendor_id != 0
                && adapter.vendor_id == implementation.vendor_id
                && adapters
                    .iter()
                    .filter(|candidate| {
                        candidate.flags & 0x2 == 0
                            && candidate.vendor_id == implementation.vendor_id
                    })
                    .count()
                    == 1
        }
        _ => false,
    }
}

#[cfg(not(windows))]
pub(in super::super) fn probe_current_display_record_routes(
    _adapter_index: u32,
) -> Result<Vec<VplCurrentDisplayRouteInfo>, BackendError> {
    Ok(Vec::new())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn implementation_adapter_match_uses_luid_for_same_vendor_gpus() {
        let first = adapter(0, 0x8086, 10, 1);
        let second = adapter(1, 0x8086, 20, 2);
        let adapters = vec![first.clone(), second.clone()];
        let implementation = implementation(Some((20, 2)));

        assert!(!implementation_matches_adapter(
            &first,
            &adapters,
            &implementation
        ));
        assert!(implementation_matches_adapter(
            &second,
            &adapters,
            &implementation
        ));
    }

    #[test]
    fn implementation_adapter_match_rejects_ambiguous_vendor_fallback() {
        let first = adapter(0, 0x8086, 10, 1);
        let second = adapter(1, 0x8086, 20, 2);
        let adapters = vec![first.clone(), second];
        let implementation = implementation(None);

        assert!(!implementation_matches_adapter(
            &first,
            &adapters,
            &implementation
        ));
    }

    #[test]
    fn implementation_adapter_match_allows_unique_vendor_fallback() {
        let intel = adapter(0, 0x8086, 10, 1);
        let nvidia = adapter(1, 0x10DE, 20, 2);
        let adapters = vec![intel.clone(), nvidia];
        let implementation = implementation(None);

        assert!(implementation_matches_adapter(
            &intel,
            &adapters,
            &implementation
        ));
    }

    fn adapter(
        index: u32,
        vendor_id: u32,
        luid_low: u32,
        luid_high: i32,
    ) -> crate::backend::dxgi::DxgiAdapterInfo {
        crate::backend::dxgi::DxgiAdapterInfo {
            index,
            description: format!("adapter-{index}"),
            vendor_id,
            device_id: 1,
            subsystem_id: 0,
            revision: 0,
            dedicated_video_memory: 0,
            dedicated_system_memory: 0,
            shared_system_memory: 0,
            luid_low,
            luid_high,
            flags: 0,
        }
    }

    fn implementation(adapter_luid: Option<(u32, i32)>) -> VplImplementationInfo {
        VplImplementationInfo {
            index: 0,
            impl_name: "test".to_owned(),
            api_version: "2.0".to_owned(),
            implementation: "硬件".to_owned(),
            acceleration_mode: "D3D11".to_owned(),
            vendor_id: 0x8086,
            vendor_impl_id: 0,
            device_id: "test".to_owned(),
            adapter_luid_low: adapter_luid.map(|(low, _)| low),
            adapter_luid_high: adapter_luid.map(|(_, high)| high),
            media_adapter_type: 0,
            hevc_supported: true,
            hevc_profiles: Vec::new(),
            input_fourcc: Vec::new(),
            route_candidates: Vec::new(),
            rate_controls: Vec::new(),
            dx11_texture_input_seen: true,
        }
    }
}
