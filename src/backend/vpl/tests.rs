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
    }
    impl VplOneCopyRecordSink for SmokeStatusSink {
        fn status(&mut self, message: &str) {
            println!("smoke status: {message}");
        }

        fn video_track_started(&mut self, _info: VplOutputTrackInfo) {}
        fn hevc_access_unit(&mut self, sample: &crate::backend::mp4_mux::HevcAccessUnit) {
            self.video.push(sample.clone());
        }
        fn aac_access_unit(&mut self, _sample: &crate::backend::mp4_mux::AacAccessUnit) {}
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
    let route_plan = nvenc_probe
        .current_display_routes
        .iter()
        .find(|route| {
            route.chroma == ChromaSampling::Yuv420
                && matches!(route.input_format.as_str(), "NV12" | "P010")
        })
        .expect("本机需要当前显示器 NVENC NV12/P010 route");
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
    let rate_control = RateControlConfig {
        method: RateControlMethod::Cbr,
        ..RateControlConfig::default()
    };
    let mut sink = SmokeStatusSink::default();
    let record_result = match capture_source {
        RecordCaptureSource::Dda => record_nvenc_d3d11_onecopy_memory_output_with_sink_cancelable(
            route_plan.adapter_index,
            &path,
            duration_seconds,
            &rate_control,
            ChromaSampling::Yuv420,
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
                ChromaSampling::Yuv420,
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
