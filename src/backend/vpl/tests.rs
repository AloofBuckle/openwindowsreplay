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
    assert_eq!(std::mem::size_of::<MfxExtendedDeviceId>(), 196);
    assert_eq!(std::mem::offset_of!(MfxEncodeCtrl, FrameType), 32);
    assert_eq!(std::mem::offset_of!(MfxEncodeCtrl, ExtParam), 40);
    assert_eq!(std::mem::offset_of!(MfxExtCodingOption2, MaxFrameSize), 16);
    assert_eq!(std::mem::offset_of!(MfxExtCodingOption2, MBBRC), 26);
    assert_eq!(std::mem::offset_of!(MfxExtCodingOption2, ExtBRC), 28);
    assert_eq!(std::mem::offset_of!(MfxExtCodingOption2, DisableVUI), 58);
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
fn vpl_route_forces_vui_enabled_without_forcing_range() {
    let rate_control = RateControlConfig::default();
    let full = VplEncodeExtBuffers::for_route(VplRecordRoute::hdr_pq_p010(), &rate_control);
    let limited = VplEncodeExtBuffers::for_route(
        VplRecordRoute::hdr_pq_p010().with_color(NclxColorMetadata::bt2020_pq(false)),
        &rate_control,
    );

    assert_eq!(full.coding2.DisableVUI, MFX_CODINGOPTION_OFF);
    assert_eq!(limited.coding2.DisableVUI, MFX_CODINGOPTION_OFF);
    assert_eq!(full.video_signal.VideoFullRange, 1);
    assert_eq!(limited.video_signal.VideoFullRange, 0);
}

#[test]
fn dll_candidates_include_env_first() {
    let candidates = candidate_dlls();
    let expected_config_index = if let Ok(path) = std::env::var("RUSTREPLAY_VPL_DLL") {
        assert_eq!(candidates[0], PathBuf::from(path));
        1
    } else {
        0
    };
    assert_eq!(candidates[expected_config_index], AppConfig::vpl_dll_path());
    assert!(candidates.iter().any(|p| p.ends_with("libvpl-2.dll")));
}

#[test]
fn wgc_timestamp_quantization_exposes_sub_tick_duplicates() {
    assert_eq!(wgc_timestamp_from_origin_90k(1, 0), 0);
    assert_eq!(wgc_timestamp_from_origin_90k(55, 0), 0);
    assert_eq!(wgc_timestamp_from_origin_90k(56, 0), 1);
    assert_eq!(minimum_source_interval_90k_for_refresh(240), 187);
    assert_eq!(minimum_source_interval_90k_for_refresh(60), 750);
    assert_eq!(minimum_source_interval_90k_for_refresh(0), 1);
    assert_eq!(wgc_coalesce_window_100ns_for_refresh(240), 35_000);
    assert_eq!(wgc_coalesce_window_100ns_for_refresh(60), 140_000);
    assert_eq!(wgc_coalesce_window_100ns_for_refresh(0), 0);
    assert!(should_coalesce_wgc_timestamps(1_000_000, 1_034_999, 35_000));
    assert!(!should_coalesce_wgc_timestamps(
        1_000_000, 1_035_000, 35_000
    ));
    assert!(!should_coalesce_wgc_timestamps(1_000_000, 999_999, 35_000));
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

#[cfg(windows)]
fn run_local_nvenc_d3d11_record_smoke(capture_source: RecordCaptureSource) {
    #[derive(Default)]
    struct SmokeStatusSink {
        video: Vec<crate::backend::mp4_mux::HevcAccessUnit>,
        audio: Vec<crate::backend::mp4_mux::AacAccessUnit>,
    }
    impl VplOneCopyRecordSink for SmokeStatusSink {
        fn status(&mut self, message: &str) {
            println!("smoke status: {message}");
        }

        fn video_track_started(&mut self, _info: VplOutputTrackInfo) {}
        fn hevc_access_unit(&mut self, sample: &crate::backend::mp4_mux::HevcAccessUnit) {
            self.video.push(sample.clone());
        }
        fn aac_access_unit(&mut self, sample: &crate::backend::mp4_mux::AacAccessUnit) {
            self.audio.push(sample.clone());
        }
    }

    unsafe {
        let _ = windows::Win32::UI::HiDpi::SetProcessDpiAwarenessContext(
            windows::Win32::UI::HiDpi::DPI_AWARENESS_CONTEXT_PER_MONITOR_AWARE_V2,
        );
    }
    // 手动短测时桌面可能完全静止，WGC 只在变化时产帧；把 warmup 缩短到 0
    // 只影响本 ignored 测试，生产默认仍使用较保守的 warmup。
    unsafe {
        std::env::set_var("RUST_REPLAY_WGC_POST_WARMUP_DISCARD_FRAMES", "0");
        std::env::set_var("RUST_REPLAY_WGC_PIPELINE_WARMUP_FRAMES", "0");
        std::env::set_var("RUST_REPLAY_WGC_PIPELINE_WARMUP_STABLE_INTERVALS", "0");
    }
    let adapters = crate::backend::dxgi::enumerate_adapters().unwrap_or_default();
    let nvenc_probe = crate::backend::nvenc::probe_nvenc_adapters(&adapters);
    let requested_chroma = match std::env::var("RUST_REPLAY_NVENC_SMOKE_CHROMA")
        .ok()
        .as_deref()
    {
        Some("422") => ChromaSampling::Yuv422,
        Some("444") => ChromaSampling::Yuv444,
        _ => ChromaSampling::Yuv420,
    };
    let route_plan = nvenc_probe
        .current_display_routes
        .iter()
        .find(|route| route.chroma == requested_chroma && !route.input_format.is_empty())
        .expect("本机需要当前显示器对应色度的 NVENC production route");
    let move_cursor = std::env::var_os("RUST_REPLAY_NVENC_SMOKE_MOVE_CURSOR").is_some();
    let cursor_stop = std::sync::Arc::new(std::sync::atomic::AtomicBool::new(false));
    let cursor_thread = move_cursor.then(|| {
        let cursor_stop = cursor_stop.clone();
        std::thread::spawn(move || {
            use std::sync::atomic::Ordering;
            use std::time::Duration;
            use windows::Win32::Foundation::POINT;
            use windows::Win32::UI::WindowsAndMessaging::{GetCursorPos, SetCursorPos};

            let mut origin = POINT::default();
            if unsafe { GetCursorPos(&mut origin) }.is_err() {
                return;
            }
            let mut tick = 0i32;
            while !cursor_stop.load(Ordering::Relaxed) {
                let dx = match tick.rem_euclid(4) {
                    0 => 0,
                    1 => 8,
                    2 => 0,
                    _ => -8,
                };
                let dy = if tick.rem_euclid(2) == 0 { 0 } else { 4 };
                let _ = unsafe { SetCursorPos(origin.x + dx, origin.y + dy) };
                tick = tick.wrapping_add(1);
                std::thread::sleep(Duration::from_millis(50));
            }
            let _ = unsafe { SetCursorPos(origin.x, origin.y) };
        })
    });
    let output_override = std::env::var_os("RUST_REPLAY_NVENC_SMOKE_OUTPUT");
    let keep_output = output_override.is_some();
    let path = output_override.map(PathBuf::from).unwrap_or_else(|| {
        std::env::temp_dir().join(format!(
            "rustreplay_nvenc_{}_record_smoke.mp4",
            capture_source.label().to_ascii_lowercase()
        ))
    });
    let duration_seconds = std::env::var("RUST_REPLAY_NVENC_SMOKE_SECONDS")
        .ok()
        .and_then(|value| value.parse::<f32>().ok())
        .filter(|value| value.is_finite() && *value >= 0.1)
        .unwrap_or(5.0);
    let look_ahead_depth = std::env::var("RUST_REPLAY_NVENC_SMOKE_LOOKAHEAD")
        .ok()
        .and_then(|value| value.parse::<u16>().ok())
        .unwrap_or(0)
        .min(31);
    let nvenc_split_encode_mode = match std::env::var("RUST_REPLAY_NVENC_SMOKE_SPLIT")
        .ok()
        .as_deref()
    {
        Some("disabled") => crate::rate_control::NvencSplitEncodeMode::Disabled,
        Some("three") => crate::rate_control::NvencSplitEncodeMode::ThreeForced,
        _ => crate::rate_control::NvencSplitEncodeMode::Auto,
    };
    let rate_control = RateControlConfig {
        method: RateControlMethod::Cbr,
        look_ahead_depth,
        nvenc_split_encode_mode,
        ..RateControlConfig::default()
    };
    let mut sink = SmokeStatusSink::default();
    let record_result = match capture_source {
        RecordCaptureSource::Dda => record_nvenc_d3d11_onecopy_memory_output_with_sink_cancelable(
            route_plan.adapter_index,
            &path,
            duration_seconds,
            &rate_control,
            requested_chroma,
            None,
            Some(&mut sink),
            Some(route_plan),
        ),
        RecordCaptureSource::Wgc => {
            record_nvenc_wgc_d3d11_onecopy_memory_output_with_sink_cancelable(
                route_plan.adapter_index,
                &path,
                duration_seconds,
                &rate_control,
                requested_chroma,
                None,
                Some(&mut sink),
                Some(route_plan),
            )
        }
    };
    cursor_stop.store(true, std::sync::atomic::Ordering::Relaxed);
    if let Some(cursor_thread) = cursor_thread {
        let _ = cursor_thread.join();
    }
    let mut recorded = record_result.unwrap();
    if !recorded
        .video_track
        .samples
        .iter()
        .any(|sample| !sample.discard_from_track)
    {
        recorded.video_track.samples = std::mem::take(&mut sink.video);
    }
    if recorded
        .audio_track
        .as_ref()
        .is_some_and(|track| track.samples.is_empty())
        && !sink.audio.is_empty()
    {
        let duration_ticks = sink
            .audio
            .iter()
            .map(|sample| {
                sample
                    .timestamp_ticks
                    .saturating_add(u64::from(sample.duration_ticks))
            })
            .max()
            .unwrap_or(1);
        recorded.audio_track = Some(crate::backend::mp4_mux::AacLcMp4Track {
            sample_rate: crate::backend::audio::TARGET_SAMPLE_RATE,
            channel_count: crate::backend::audio::TARGET_CHANNELS,
            duration_ticks,
            samples: std::mem::take(&mut sink.audio),
        });
    }
    crate::backend::mp4_mux::write_hevc_aac_mp4(
        &path,
        &recorded.video_track,
        recorded.audio_track.as_ref(),
    )
    .unwrap();
    println!(
        "{}",
        serde_json::to_string_pretty(&recorded.report).unwrap()
    );
    assert!(recorded.report.captured_frames > 0);
    assert!(recorded.report.encoded_samples > 0);
    assert!(path.exists(), "应写出可 mux 的 NVENC MP4");
    if !keep_output {
        let _ = std::fs::remove_file(path);
    }
}

#[test]
#[ignore = "需要本机 NVIDIA 驱动、D3D11 桌面会话和可捕获桌面；手动验证 NVENC WGC 生产录制路径"]
fn local_nvenc_wgc_d3d11_record_smoke() {
    let repeats = std::env::var("RUST_REPLAY_NVENC_SMOKE_REPEATS")
        .ok()
        .and_then(|value| value.parse::<usize>().ok())
        .unwrap_or(1)
        .clamp(1, 100);
    for repeat in 0..repeats {
        println!("NVENC WGC repeat {}/{} start", repeat + 1, repeats);
        run_local_nvenc_d3d11_record_smoke(RecordCaptureSource::Wgc);
        println!("NVENC WGC repeat {}/{} done", repeat + 1, repeats);
    }
}

#[test]
fn replay_idr_requests_are_bounded_by_source_pts_not_frame_count() {
    assert!(should_force_source_timed_idr(None, 0));
    assert!(!should_force_source_timed_idr(
        Some(0),
        REPLAY_IDR_INTERVAL_90K - 1
    ));
    assert!(should_force_source_timed_idr(
        Some(0),
        REPLAY_IDR_INTERVAL_90K
    ));
    assert!(!should_force_source_timed_idr(
        Some(10 * VIDEO_CLOCK_HZ),
        9 * VIDEO_CLOCK_HZ
    ));
}

#[test]
fn bitstream_capacity_is_derived_from_route_dimensions_and_hrd_buffer() {
    let mut param: MfxVideoParam = unsafe { std::mem::zeroed() };
    param.mfx.FrameInfo.CropW = 3_840;
    param.mfx.FrameInfo.CropH = 2_160;
    let p010 = initial_bitstream_capacity_bytes(&param, VplRecordRoute::hdr_pq_p010());
    assert!(p010 >= 11 * 1024 * 1024);
    assert!(p010 < 16 * 1024 * 1024);

    param.mfx.BufferSizeInKB = 30_000;
    param.mfx.BRCParamMultiplier = 2;
    let hrd_limited = initial_bitstream_capacity_bytes(&param, VplRecordRoute::hdr_pq_p010());
    assert!(hrd_limited >= 60_000 * 1024);
    assert!(hrd_limited <= VPL_BITSTREAM_INITIAL_MAX_BYTES);
}

#[test]
fn bitstream_capacity_has_bounded_initial_allocation_for_large_routes() {
    let mut param: MfxVideoParam = unsafe { std::mem::zeroed() };
    param.mfx.FrameInfo.Width = u16::MAX;
    param.mfx.FrameInfo.Height = u16::MAX;
    param.mfx.BufferSizeInKB = u16::MAX;
    param.mfx.BRCParamMultiplier = u16::MAX;

    assert_eq!(
        initial_bitstream_capacity_bytes(&param, VplRecordRoute::hdr_pq_y410()),
        VPL_BITSTREAM_INITIAL_MAX_BYTES
    );
}

#[test]
fn capture_pool_preserves_32_slots_for_4k_and_bounds_8k_cross_device_vram() {
    assert_eq!(
        capture_pool_size_for_route(3_840, 2_160, VplRecordRoute::hdr_pq_p010(), true),
        32
    );
    assert_eq!(
        capture_pool_size_for_route(3_840, 2_160, VplRecordRoute::hdr_pq_y410(), true),
        32
    );
    let eight_k_shared =
        capture_pool_size_for_route(7_680, 4_320, VplRecordRoute::hdr_pq_y410(), true);
    assert!((8..32).contains(&eight_k_shared));
    assert!(
        capture_pool_size_for_route(7_680, 4_320, VplRecordRoute::hdr_pq_y410(), false)
            > eight_k_shared
    );
}

#[test]
#[ignore = "需要本机 NVIDIA 驱动、D3D11 桌面会话和可捕获桌面；手动验证 NVENC DDA 生产录制路径"]
fn local_nvenc_dda_d3d11_record_smoke() {
    let repeats = std::env::var("RUST_REPLAY_NVENC_SMOKE_REPEATS")
        .ok()
        .and_then(|value| value.parse::<usize>().ok())
        .unwrap_or(1)
        .clamp(1, 100);
    for repeat in 0..repeats {
        println!("NVENC DDA repeat {}/{} start", repeat + 1, repeats);
        run_local_nvenc_d3d11_record_smoke(RecordCaptureSource::Dda);
        println!("NVENC DDA repeat {}/{} done", repeat + 1, repeats);
    }
}

#[test]
#[ignore = "需要本机 NVIDIA 驱动和 D3D11 shared-fence 支持；手动验证 NV12/P010/AYUV 零拷贝共享输入"]
fn local_nvenc_shared_fence_route_formats_smoke() {
    use windows::Win32::Graphics::Direct3D11::{
        D3D11_TEXTURE2D_DESC, D3D11_USAGE_DEFAULT, ID3D11DeviceContext4,
    };
    use windows::Win32::Graphics::Dxgi::Common::DXGI_SAMPLE_DESC;
    use windows::Win32::Graphics::Dxgi::{CreateDXGIFactory1, IDXGIFactory1};
    use windows::core::Interface;

    let adapters = crate::backend::dxgi::enumerate_adapters().unwrap_or_default();
    let nvenc_probe = crate::backend::nvenc::probe_nvenc_adapters(&adapters);
    let adapter_index = nvenc_probe
        .current_display_routes
        .first()
        .expect("本机需要至少一个 NVENC current-display route")
        .adapter_index;
    let factory: IDXGIFactory1 = unsafe { CreateDXGIFactory1() }.unwrap();
    let adapter1 = unsafe { factory.EnumAdapters1(adapter_index) }.unwrap();
    let width = 1280;
    let height = 720;

    for route in [
        VplRecordRoute::sdr8_nv12(),
        VplRecordRoute::hdr_pq_p010(),
        VplRecordRoute::sdr8_ayuv(),
    ] {
        let input_format = nvenc_input_format_from_route(route).unwrap();
        let mut encoder = crate::backend::nvenc::NvencD3d11Encoder::open(
            adapter_index,
            width,
            height,
            input_format,
            route.mp4_color,
        )
        .unwrap();
        let (capture_device, capture_context) =
            unsafe { create_d3d11_device_for_adapter(&adapter1) }.unwrap();
        let target_desc = D3D11_TEXTURE2D_DESC {
            Width: width,
            Height: height,
            MipLevels: 1,
            ArraySize: 1,
            Format: route.try_dxgi_format().unwrap(),
            SampleDesc: DXGI_SAMPLE_DESC {
                Count: 1,
                Quality: 0,
            },
            Usage: D3D11_USAGE_DEFAULT,
            BindFlags: 0,
            CPUAccessFlags: 0,
            MiscFlags: 0,
        };
        let mut slot = unsafe {
            create_shared_fence_route_slot(
                0,
                &capture_device,
                &capture_context,
                encoder.device(),
                &target_desc,
                route,
                width,
                height,
                std::sync::Arc::new(std::sync::Mutex::new(ShaderResourceViewCache::retained())),
            )
        }
        .unwrap();
        let capture_context4: ID3D11DeviceContext4 = capture_context.cast().unwrap();
        let encoder_context4: ID3D11DeviceContext4 = encoder.context().cast().unwrap();
        slot.fence_value = 1;
        unsafe {
            capture_context4
                .Signal(&slot.capture_fence, slot.fence_value)
                .unwrap();
            encoder_context4
                .Wait(&slot.encoder_fence, slot.fence_value)
                .unwrap();
        }
        let sample = encoder
            .encode_texture(&slot.encoder_texture, 0, true, false)
            .unwrap();
        assert!(!sample.data.is_empty(), "{} shared input", route.summary());
        encoder.shutdown().unwrap();
        println!("shared-fence route passed: {}", route.summary());
    }
}
