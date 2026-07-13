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
