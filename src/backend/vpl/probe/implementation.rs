use super::*;

pub(in super::super) unsafe fn parse_impl(
    index: u32,
    desc: &MfxImplDescription,
    api: &VplApi,
    loader: MfxLoader,
    current_display_route_keys: &BTreeSet<String>,
    probe_dimensions: VplProbeDimensions,
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
            probe_dimensions,
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
        adapter_luid_low: None,
        adapter_luid_high: None,
        media_adapter_type: desc.Dev.MediaAdapterType,
        hevc_supported,
        hevc_profiles: hevc_profiles.into_iter().collect(),
        input_fourcc: input_fourcc.into_iter().collect(),
        route_candidates,
        rate_controls: rate_controls.into_iter().collect(),
        dx11_texture_input_seen,
    }
}

fn apply_probe_dimensions(param: &mut MfxVideoParam, dimensions: VplProbeDimensions) {
    param.mfx.FrameInfo.Width = dimensions.width;
    param.mfx.FrameInfo.Height = dimensions.height;
    param.mfx.FrameInfo.CropW = dimensions.crop_width;
    param.mfx.FrameInfo.CropH = dimensions.crop_height;
    param.mfx.FrameInfo.FrameRateExtN = VPL_ENCODER_FRAME_RATE_HINT_FALLBACK_N;
    param.mfx.FrameInfo.FrameRateExtD = VPL_ENCODER_FRAME_RATE_HINT_FALLBACK_D;
}

pub(in super::super) unsafe fn query_rate_control_config_supported(
    api: &VplApi,
    session: MfxSession,
    route: VplRecordRoute,
    rate_control: &RateControlConfig,
    probe_dimensions: VplProbeDimensions,
) -> bool {
    let mut input = make_query_param(
        rate_control,
        route.fourcc,
        route.chroma,
        route.bit_depth,
        route.profile,
    );
    apply_probe_dimensions(&mut input, probe_dimensions);
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

pub(in super::super) unsafe fn smoke_rate_control_surface_available(
    api: &VplApi,
    loader: MfxLoader,
    implementation_index: u32,
    route: VplRecordRoute,
    rate_control: &RateControlConfig,
    probe_dimensions: VplProbeDimensions,
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
    // Use the probed primary desktop dimensions rather than a fixed 4K mode.
    apply_probe_dimensions(&mut param, probe_dimensions);
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
    apply_probe_dimensions(&mut param, probe_dimensions);
    param.mfx.FrameInfo.FrameRateExtN = VPL_ENCODER_FRAME_RATE_HINT_FALLBACK_N;
    param.mfx.FrameInfo.FrameRateExtD = VPL_ENCODER_FRAME_RATE_HINT_FALLBACK_D;
    param.mfx.GopPicSize = u16::MAX;
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
                let capacity = initial_bitstream_capacity_bytes(&param, route);
                let mut bitstream_pool = Vec::with_capacity(1);
                let bitstream_storage = vec![0u8; capacity + 31];
                let submitted = submit_encode_async(
                    api,
                    session,
                    AsyncEncodeRequest {
                        surface: first_surface,
                        timestamp_90k: 0,
                        is_sync: true,
                        storage: bitstream_storage,
                        discard: true,
                    },
                    &mut bitstream_pool,
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

pub(in super::super) unsafe fn query_rate_control_features_for_route(
    api: &VplApi,
    session: MfxSession,
    loader: MfxLoader,
    implementation_index: u32,
    route: VplRecordRoute,
    probe_dimensions: VplProbeDimensions,
) -> Vec<VplRateControlFeatureProbe> {
    let mut supported = Vec::new();
    for method in RateControlMethod::all() {
        let mut rate_control = RateControlConfig {
            method,
            ..RateControlConfig::default()
        };
        // 外部 BRC 需要 mfxExtBRC 回调结构；能力探测阶段只验证 oneVPL 内建码控模式。
        rate_control.ext_brc = false;
        if !query_rate_control_config_supported(
            api,
            session,
            route,
            &rate_control,
            probe_dimensions,
        ) {
            continue;
        }

        let mut mbbrc_cfg = rate_control.clone();
        mbbrc_cfg.mbbrc = true;
        let mbbrc =
            query_rate_control_config_supported(api, session, route, &mbbrc_cfg, probe_dimensions);

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
            query_rate_control_config_supported(api, session, route, &cfg, probe_dimensions)
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
            query_rate_control_config_supported(api, session, route, &cfg, probe_dimensions)
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
                query_rate_control_config_supported(api, session, route, &cfg, probe_dimensions)
                    && smoke_rate_control_surface_available(
                        api,
                        loader,
                        implementation_index,
                        route,
                        &cfg,
                        probe_dimensions,
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

pub(in super::super) unsafe fn query_encode_route_candidates(
    api: &VplApi,
    loader: MfxLoader,
    implementation_index: u32,
    input_fourcc: &BTreeSet<String>,
    current_display_route_keys: &BTreeSet<String>,
    probe_dimensions: VplProbeDimensions,
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
        apply_probe_dimensions(&mut input, probe_dimensions);
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
            query_rate_control_features_for_route(
                api,
                session,
                loader,
                implementation_index,
                route,
                probe_dimensions,
            )
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
