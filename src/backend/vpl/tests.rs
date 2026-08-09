use super::*;
use std::time::Duration;

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
fn wgc_transport_ticks_do_not_replace_exact_source_pts() {
    assert_eq!(wgc_timestamp_from_origin_90k(1, 0), 0);
    assert_eq!(wgc_timestamp_from_origin_90k(55, 0), 0);
    assert_eq!(wgc_timestamp_from_origin_90k(56, 0), 1);

    assert_eq!(quantize_wgc_timestamp_90k(1, 0, None), (0, 0));
    assert_eq!(quantize_wgc_timestamp_90k(55, 0, Some(0)), (1, 1));
    assert_eq!(quantize_wgc_timestamp_90k(56, 0, Some(0)), (1, 0));

    let mut timeline = PresentationTimestampTracker::default();
    timeline.remember(0, 0, Some(1), Some(1)).unwrap();
    timeline.remember(1, 1, Some(55), Some(1)).unwrap();
    let mut first = crate::backend::mp4_mux::HevcAccessUnit {
        timestamp_90k: 0,
        presentation_timestamp_100ns: None,
        data: std::sync::Arc::<[u8]>::from([]),
        is_sync: true,
        discard_from_track: false,
    };
    let mut second = crate::backend::mp4_mux::HevcAccessUnit {
        timestamp_90k: 1,
        presentation_timestamp_100ns: None,
        data: std::sync::Arc::<[u8]>::from([]),
        is_sync: false,
        discard_from_track: false,
    };
    timeline.attach(&mut first).unwrap();
    timeline.attach(&mut second).unwrap();
    timeline.finish().unwrap();
    assert_eq!(first.presentation_timestamp_100ns, Some(0));
    assert_eq!(second.presentation_timestamp_100ns, Some(54));
    assert_eq!(timeline.presentation_duration_100ns(), Some(108));
}

#[test]
fn wgc_coalesce_window_tracks_display_refresh() {
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
fn delayed_warmup_output_cannot_enter_the_formal_source_timeline() {
    let mut timeline = PresentationTimestampTracker::default();
    timeline.remember_discard(0).unwrap();
    timeline.remember_discard(1).unwrap();
    timeline.remember(2, 0, Some(100), Some(100)).unwrap();

    let mut delayed_warmup = crate::backend::mp4_mux::HevcAccessUnit {
        timestamp_90k: 1,
        presentation_timestamp_100ns: None,
        data: std::sync::Arc::<[u8]>::from([]),
        is_sync: true,
        discard_from_track: false,
    };
    timeline.attach(&mut delayed_warmup).unwrap();
    assert!(delayed_warmup.discard_from_track);

    let mut first_formal = crate::backend::mp4_mux::HevcAccessUnit {
        timestamp_90k: 2,
        presentation_timestamp_100ns: None,
        data: std::sync::Arc::<[u8]>::from([]),
        is_sync: true,
        discard_from_track: true,
    };
    timeline.attach(&mut first_formal).unwrap();
    assert!(!first_formal.discard_from_track);
    assert_eq!(first_formal.timestamp_90k, 0);
    assert_eq!(first_formal.presentation_timestamp_100ns, Some(0));
    assert_eq!(timeline.presentation_duration_100ns(), Some(1));
    timeline.finish().unwrap();
}

#[test]
fn wgc_event_wait_honors_coalesce_deadline_and_idle_cap() {
    assert_eq!(
        wgc_event_wait_timeout(None, Duration::from_secs(1)),
        Duration::from_millis(50)
    );
    assert_eq!(
        wgc_event_wait_timeout(Some(Duration::from_millis(3)), Duration::from_secs(1)),
        Duration::from_millis(3)
    );
    assert_eq!(
        wgc_event_wait_timeout(Some(Duration::from_millis(30)), Duration::from_millis(2)),
        Duration::from_millis(2)
    );
}

#[test]
fn pq_lut_quartic_domain_stays_within_one_10bit_code() {
    for sample in 0..=1_000_000u32 {
        let normalized_luminance = f64::from(sample) / 1_000_000.0;
        let coordinate = normalized_luminance.sqrt().sqrt();
        let index = (coordinate * (ST2084_PQ_LUT_SIZE - 1) as f64)
            .round()
            .clamp(0.0, (ST2084_PQ_LUT_SIZE - 1) as f64) as usize;
        let exact = (st2084_pq_oetf_scalar(normalized_luminance) * 1023.0).round() as i32;
        let approximated = (st2084_pq_oetf_scalar(st2084_pq_lut_normalized_luminance(index))
            * 1023.0)
            .round() as i32;
        assert!(
            (exact - approximated).abs() <= 1,
            "sample={sample} normalized_luminance={normalized_luminance} exact={exact} approximated={approximated}"
        );
    }
}

#[test]
fn signed_scrgb_components_survive_bt2020_primary_conversion() {
    fn rec709_to_bt2020(rgb: [f64; 3]) -> [f64; 3] {
        [
            0.6274039 * rgb[0] + 0.3292830 * rgb[1] + 0.0433131 * rgb[2],
            0.0690973 * rgb[0] + 0.9195404 * rgb[1] + 0.0113623 * rgb[2],
            0.0163914 * rgb[0] + 0.0880133 * rgb[1] + 0.8955953 * rgb[2],
        ]
    }

    let bt2020_red_in_scrgb = [1.660491, -0.124550, -0.018151];
    let bt2020_green_in_scrgb = [-0.587641, 1.132900, -0.100579];
    let red = rec709_to_bt2020(bt2020_red_in_scrgb);
    let green = rec709_to_bt2020(bt2020_green_in_scrgb);
    assert!((red[0] - 1.0).abs() < 0.000_01);
    assert!(red[1].abs() < 0.000_01 && red[2].abs() < 0.000_01);
    assert!((green[1] - 1.0).abs() < 0.000_01);
    assert!(green[0].abs() < 0.000_01 && green[2].abs() < 0.000_01);

    let clipped_red = rec709_to_bt2020([
        bt2020_red_in_scrgb[0].max(0.0),
        bt2020_red_in_scrgb[1].max(0.0),
        bt2020_red_in_scrgb[2].max(0.0),
    ]);
    let clipped_green = rec709_to_bt2020([
        bt2020_green_in_scrgb[0].max(0.0),
        bt2020_green_in_scrgb[1].max(0.0),
        bt2020_green_in_scrgb[2].max(0.0),
    ]);
    assert!(clipped_red[1] > 0.1, "pre-clipped red becomes orange");
    assert!(
        clipped_green[0] > 0.3,
        "pre-clipped green becomes yellow-green"
    );

    for (name, shader) in [
        ("P010 HDR", P010_CONVERT_HLSL),
        ("P010 BT.2020 SDR", P010_SDR_BT2020_CONVERT_HLSL),
        ("Y210 HDR", Y210_CONVERT_HLSL),
        ("Y210 BT.2020 SDR", Y210_SDR_BT2020_CONVERT_HLSL),
        ("Y410 HDR", Y410_CONVERT_HLSL),
        ("Y410 BT.2020 SDR", Y410_SDR_BT2020_CONVERT_HLSL),
    ] {
        assert!(
            !shader.contains("rec709_linear_to_bt2020_linear(max("),
            "{name} must preserve signed scRGB through the primary conversion"
        );
    }
    assert!(
        NVENC_PLANAR_CONVERT_HLSL.contains("rec709_linear_to_bt2020_linear(source)"),
        "NVENC planar 422/444 conversion must preserve signed scRGB"
    );
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
fn delayed_encoder_uses_the_latest_completed_or_requested_idr_timestamp() {
    assert_eq!(
        latest_idr_timestamp_90k(Some(0), Some(450_000)),
        Some(450_000)
    );
    assert_eq!(
        latest_idr_timestamp_90k(Some(450_000), Some(0)),
        Some(450_000)
    );
    assert_eq!(latest_idr_timestamp_90k(None, Some(450_000)), Some(450_000));
    assert!(!should_force_source_timed_idr(
        latest_idr_timestamp_90k(Some(0), Some(450_000)),
        450_001,
    ));

    let mut scheduler = SourceTimedIdrScheduler::default();
    assert!(scheduler.should_force(None, 0));
    assert!(scheduler.should_force(Some(0), REPLAY_IDR_INTERVAL_90K));
    assert!(!scheduler.should_force(Some(0), REPLAY_IDR_INTERVAL_90K + 1));
    assert!(scheduler.should_force(Some(0), REPLAY_IDR_INTERVAL_90K * 2));
}

#[test]
fn delayed_idr_scheduler_does_not_repeat_request_while_output_is_in_flight() {
    let mut scheduler = SourceTimedIdrScheduler::default();
    let mut completed_sync = None;
    let timestamps = [
        0,
        REPLAY_IDR_INTERVAL_90K,
        REPLAY_IDR_INTERVAL_90K + 1,
        REPLAY_IDR_INTERVAL_90K + 2,
        REPLAY_IDR_INTERVAL_90K + 3,
        REPLAY_IDR_INTERVAL_90K * 2,
    ];
    let mut requests = Vec::new();
    for (index, timestamp) in timestamps.into_iter().enumerate() {
        let force = scheduler.should_force(completed_sync, timestamp);
        if force {
            requests.push(timestamp);
        }
        // Simulate an async encoder that does not publish the second IDR until
        // after several later source frames have already been submitted.
        if index == 4 {
            completed_sync = Some(REPLAY_IDR_INTERVAL_90K);
        }
    }
    assert_eq!(
        requests,
        vec![0, REPLAY_IDR_INTERVAL_90K, REPLAY_IDR_INTERVAL_90K * 2]
    );
}

#[test]
fn aligned_live_audio_chunks_keep_aac_timestamps_contiguous() {
    let mut blocker = crate::backend::audio::AacBlocker::default();
    let mut timestamps = Vec::new();
    for _ in 0..3 {
        let frame = crate::backend::audio::StereoPcmFrame {
            start_time_100ns: 0,
            samples: vec![[0.0, 0.0]; LIVE_AUDIO_MIX_CHUNK_TICKS as usize],
        };
        timestamps.extend(
            blocker
                .push(&frame)
                .into_iter()
                .map(|block| block.timestamp_ticks),
        );
    }
    assert!(
        timestamps.windows(2).all(|pair| {
            pair[1] == pair[0] + crate::backend::audio::AAC_LC_FRAME_SAMPLES as u64
        })
    );
    assert_eq!(
        timestamps.len() as u64 * crate::backend::audio::AAC_LC_FRAME_SAMPLES as u64,
        LIVE_AUDIO_MIX_CHUNK_TICKS * 3
    );
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
fn direct_arc_async_output_survives_bitstream_pool_reuse() {
    let expected = [0, 0, 0, 1, 0x26, 1, 2, 3, 4];
    let data_offset = 7usize;
    let mut storage = vec![0u8; 96];
    storage[data_offset..data_offset + expected.len()].copy_from_slice(&expected);
    let bitstream = MfxBitstream {
        Data: storage.as_mut_ptr(),
        DataOffset: data_offset as u32,
        DataLength: expected.len() as u32,
        TimeStamp: u64::MAX,
        ..unsafe { std::mem::zeroed() }
    };
    let flight = AsyncEncode {
        bitstream,
        storage,
        syncp: ptr::null_mut(),
        _ctrl: None,
        timestamp_90k: 123,
        is_sync: false,
        discard: false,
    };
    let mut bitstream_pool = Vec::new();

    let sample = unsafe { finish_synced_async_encode(flight, &mut bitstream_pool) }
        .unwrap()
        .unwrap();
    assert_eq!(bitstream_pool.len(), 1);
    bitstream_pool[0].fill(0xff);

    assert_eq!(&*sample.data, expected.as_slice());
    assert_eq!(sample.timestamp_90k, 123);
}

#[test]
fn direct_arc_empty_async_output_returns_storage_without_a_sample() {
    let storage = vec![0u8; 96];
    let flight = AsyncEncode {
        bitstream: unsafe { std::mem::zeroed() },
        storage,
        syncp: ptr::null_mut(),
        _ctrl: None,
        timestamp_90k: 123,
        is_sync: false,
        discard: false,
    };
    let mut bitstream_pool = Vec::new();

    let sample = unsafe { finish_synced_async_encode(flight, &mut bitstream_pool) }.unwrap();

    assert!(sample.is_none());
    assert_eq!(bitstream_pool.len(), 1);
}

#[test]
#[ignore = "需要 Intel oneVPL、D3D11 桌面会话、可捕获桌面、WASAPI 与 Media Foundation AAC"]
fn local_onevpl_dda_record_smoke() {
    unsafe {
        let _ = windows::Win32::UI::HiDpi::SetProcessDpiAwarenessContext(
            windows::Win32::UI::HiDpi::DPI_AWARENESS_CONTEXT_PER_MONITOR_AWARE_V2,
        );
    }
    let probe = probe_vpl();
    let route = probe
        .current_display_routes
        .iter()
        .find(|route| route.chroma == ChromaSampling::Yuv420 && !route.fourcc.is_empty())
        .expect("本机需要当前显示器对应的 oneVPL YUV420 production route");
    let output_override = std::env::var_os("RUST_REPLAY_VPL_SMOKE_OUTPUT");
    let keep_output = output_override.is_some();
    let output = output_override
        .map(PathBuf::from)
        .unwrap_or_else(|| std::env::temp_dir().join("rustreplay_onevpl_dda_record_smoke.mp4"));
    let duration_seconds = std::env::var("RUST_REPLAY_VPL_SMOKE_SECONDS")
        .ok()
        .and_then(|value| value.parse::<f32>().ok())
        .filter(|value| value.is_finite() && *value >= 0.1)
        .unwrap_or(5.0);
    let rate_control = RateControlConfig {
        method: RateControlMethod::Cbr,
        target_kbps: 130_000,
        brc_param_multiplier: 2,
        ..RateControlConfig::default()
    };

    let recorded = record_d3d11_onecopy_mp4_output_with_route_cancelable(
        route.adapter_index,
        &output,
        duration_seconds,
        &rate_control,
        ChromaSampling::Yuv420,
        None,
        Some(route),
    )
    .unwrap();

    println!(
        "{}",
        serde_json::to_string_pretty(&recorded.report).unwrap()
    );
    println!("onevpl_smoke_output={}", output.display());
    assert!(recorded.report.captured_frames > 0);
    assert!(recorded.report.encoded_samples > 0);
    assert!(recorded.report.encoded_bytes > 0);
    assert!(recorded.report.audio_access_units > 0);
    assert!(recorded.report.audio_encoded_bytes > 0);
    assert!(
        output.metadata().expect("oneVPL smoke MP4 metadata").len() > 0,
        "应写出非空 oneVPL HEVC/AAC MP4"
    );
    if !keep_output {
        let _ = std::fs::remove_file(output);
    }
}

#[cfg(windows)]
struct OneVplWgcRingSmokeSink {
    ring: crate::ring::EncodedReplayRing,
    started: bool,
}

#[cfg(windows)]
impl OneVplWgcRingSmokeSink {
    fn new(retention: std::time::Duration) -> Self {
        Self {
            ring: crate::ring::EncodedReplayRing::new(retention),
            started: false,
        }
    }
}

#[cfg(windows)]
impl VplOneCopyRecordSink for OneVplWgcRingSmokeSink {
    fn status(&mut self, message: &str) {
        println!("wgc production smoke status: {message}");
    }

    fn video_track_started(&mut self, info: VplOutputTrackInfo) {
        self.ring.start_segment(crate::ring::EncodedReplayMetadata {
            width: info.width,
            height: info.height,
            color: info.color,
            codec: info.codec,
            audio_sample_rate: crate::backend::audio::TARGET_SAMPLE_RATE,
            audio_channel_count: crate::backend::audio::TARGET_CHANNELS,
        });
        self.started = true;
    }

    fn hevc_access_unit(&mut self, sample: &crate::backend::mp4_mux::HevcAccessUnit) {
        if self.started {
            self.ring.push_video_au_90k(sample);
        }
    }

    fn aac_access_unit(&mut self, sample: &crate::backend::mp4_mux::AacAccessUnit) {
        if self.started {
            self.ring
                .push_audio_au_ticks(sample, crate::backend::audio::TARGET_SAMPLE_RATE);
        }
    }
}

#[test]
#[ignore = "需要 Intel oneVPL、WGC 桌面会话、可捕获桌面、WASAPI 与 Media Foundation AAC；使用当前磁盘配置验证生产 Memory ring 路线"]
fn local_onevpl_wgc_production_record_smoke() {
    unsafe {
        let _ = windows::Win32::UI::HiDpi::SetProcessDpiAwarenessContext(
            windows::Win32::UI::HiDpi::DPI_AWARENESS_CONTEXT_PER_MONITOR_AWARE_V2,
        );
    }
    for name in [
        "RUST_REPLAY_WGC_POST_WARMUP_DISCARD_FRAMES",
        "RUST_REPLAY_WGC_PIPELINE_WARMUP_FRAMES",
        "RUST_REPLAY_WGC_PIPELINE_WARMUP_STABLE_INTERVALS",
        "RUST_REPLAY_VPL_ASYNC_DEPTH",
    ] {
        assert!(
            std::env::var_os(name).is_none(),
            "production WGC smoke forbids override {name}"
        );
    }

    let config = AppConfig::load_from_disk()
        .expect("读取当前用户配置")
        .expect("当前用户配置必须存在");
    assert_eq!(
        config.capture_backend,
        crate::config::CaptureBackend::Wgc,
        "当前配置必须选择 WGC"
    );
    assert_eq!(
        config.replay_buffer_mode,
        crate::config::ReplayBufferMode::Memory,
        "当前配置必须选择 Memory encoded ring"
    );
    let requested_chroma = config.chroma.unwrap_or(ChromaSampling::Yuv420);
    let probe = probe_vpl();
    let route = probe
        .current_display_routes
        .iter()
        .find(|route| route.chroma == requested_chroma && !route.fourcc.is_empty())
        .expect("本机需要当前显示器对应的 oneVPL production route");
    let output_override = std::env::var_os("RUST_REPLAY_VPL_WGC_SMOKE_OUTPUT");
    let keep_output = output_override.is_some();
    let output = output_override.map(PathBuf::from).unwrap_or_else(|| {
        std::env::temp_dir().join("rustreplay_onevpl_wgc_production_record_smoke.mp4")
    });
    let duration_seconds = std::env::var("RUST_REPLAY_VPL_WGC_SMOKE_SECONDS")
        .ok()
        .and_then(|value| value.parse::<f32>().ok())
        .filter(|value| value.is_finite() && *value >= 0.1)
        .unwrap_or(5.0);
    let retention = std::time::Duration::from_secs_f32(duration_seconds + 30.0);
    let mut sink = OneVplWgcRingSmokeSink::new(retention);

    let recorded = record_wgc_d3d11_onecopy_memory_output_with_sink_cancelable(
        route.adapter_index,
        &output,
        duration_seconds,
        &config.rate_control,
        requested_chroma,
        None,
        Some(&mut sink),
        Some(route),
    )
    .unwrap();
    assert!(
        sink.started,
        "WGC production sink must receive track metadata"
    );
    sink.ring.finish_segment_with_audio_duration(
        recorded.video_track.duration_90k,
        recorded
            .audio_track
            .as_ref()
            .map(|track| (track.duration_ticks, track.sample_rate)),
    );
    let availability = sink.ring.availability();
    println!("wgc production ring availability: {availability:?}");
    let snapshot = sink
        .ring
        .snapshot_recent_tracks(retention)
        .expect("Memory encoded ring 必须可生成包含关键帧的快照");
    crate::backend::mp4_mux::write_hevc_aac_mp4(
        &output,
        &snapshot.video_track,
        snapshot.audio_track.as_ref(),
    )
    .unwrap();

    println!(
        "{}",
        serde_json::to_string_pretty(&recorded.report).unwrap()
    );
    println!("onevpl_wgc_smoke_output={}", output.display());
    assert!(recorded.report.captured_frames > 0);
    assert!(recorded.report.encoded_samples > 0);
    assert!(recorded.report.encoded_bytes > 0);
    assert!(recorded.report.audio_access_units > 0);
    assert!(recorded.report.audio_encoded_bytes > 0);
    assert!(availability.parameter_sets_ready);
    assert!(availability.video_key_packets > 0);
    assert!(availability.audio_packets > 0);
    assert!(
        output.metadata().expect("WGC smoke MP4 metadata").len() > 0,
        "应从 Memory encoded ring 写出非空 oneVPL HEVC/AAC MP4"
    );
    if !keep_output {
        let _ = std::fs::remove_file(output);
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
