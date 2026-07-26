use super::ui::capture_mode_switch_target;
use super::*;
use crate::rate_control::{NvencMultiPass, NvencPreset, NvencSplitEncodeMode};

#[test]
fn capture_mode_controls_hide_unavailable_dedicated_switch_and_preserve_return_path() {
    assert_eq!(
        capture_mode_switch_target(CaptureMode::Generic, CaptureMode::Generic, false),
        None
    );
    assert_eq!(
        capture_mode_switch_target(CaptureMode::Generic, CaptureMode::Generic, true),
        Some(CaptureMode::DedicatedNvFbc)
    );
    assert_eq!(
        capture_mode_switch_target(
            CaptureMode::DedicatedNvFbc,
            CaptureMode::DedicatedNvFbc,
            true
        ),
        Some(CaptureMode::Generic)
    );
    assert_eq!(
        capture_mode_switch_target(CaptureMode::DedicatedNvFbc, CaptureMode::Generic, false),
        Some(CaptureMode::Generic)
    );
}

#[test]
fn switching_from_nvenc_clears_nvenc_only_rate_control_fields() {
    let mut cfg = RateControlConfig {
        nvenc_spatial_aq: true,
        nvenc_temporal_aq: true,
        nvenc_aq_strength: 9,
        nvenc_vbr_target_quality: 27,
        ..Default::default()
    };
    let mut features = RateControlFeatureSupport::hidden(cfg.method);
    features.nvenc_spatial_aq = true;
    features.nvenc_temporal_aq = true;
    features.nvenc_target_quality = true;

    sanitize_hidden_rate_control_fields(&mut cfg, &features, Some(VideoEncoderBackend::OneVpl));

    assert!(!cfg.nvenc_spatial_aq);
    assert!(!cfg.nvenc_temporal_aq);
    assert_eq!(cfg.nvenc_aq_strength, 0);
    assert_eq!(cfg.nvenc_vbr_target_quality, 0);
}

#[test]
fn nvenc_rate_control_sanitization_obeys_feature_visibility() {
    let mut cfg = RateControlConfig {
        method: RateControlMethod::Vbr,
        look_ahead_depth: 31,
        nvenc_spatial_aq: true,
        nvenc_temporal_aq: true,
        nvenc_aq_strength: 255,
        nvenc_vbr_target_quality: 255,
        ..Default::default()
    };
    let mut features = RateControlFeatureSupport::hidden(cfg.method);
    features.nvenc_spatial_aq = true;
    features.nvenc_target_quality = true;

    sanitize_hidden_rate_control_fields(&mut cfg, &features, Some(VideoEncoderBackend::Nvenc));

    assert!(cfg.nvenc_spatial_aq);
    assert_eq!(cfg.look_ahead_depth, 0);
    assert!(!cfg.nvenc_temporal_aq);
    assert_eq!(cfg.nvenc_aq_strength, 0);
    assert_eq!(cfg.nvenc_vbr_target_quality, 51);
}

#[test]
fn nvenc_lookahead_is_clamped_to_route_surface_budget() {
    let mut cfg = RateControlConfig {
        method: RateControlMethod::Cbr,
        look_ahead_depth: 31,
        ..Default::default()
    };
    let mut features = RateControlFeatureSupport::hidden(cfg.method);
    features.look_ahead_depth = true;
    features.look_ahead_depth_max = 15;

    sanitize_hidden_rate_control_fields(&mut cfg, &features, Some(VideoEncoderBackend::Nvenc));

    assert_eq!(cfg.look_ahead_depth, 15);
}

#[test]
fn nvenc_tuning_sanitization_uses_only_reported_raw_values() {
    let support = NvencTuningSupport {
        presets: vec![NvencPreset::P2, NvencPreset::P5],
        split_encode_modes: vec![
            NvencSplitEncodeMode::Auto,
            NvencSplitEncodeMode::TwoForced,
            NvencSplitEncodeMode::Disabled,
        ],
        multi_pass_modes: vec![NvencMultiPass::Disabled, NvencMultiPass::FullResolution],
        spatial_aq: false,
        encoder_engines: 2,
    };
    let mut cfg = RateControlConfig {
        nvenc_preset: NvencPreset::P4,
        nvenc_split_encode_mode: NvencSplitEncodeMode::FourForced,
        nvenc_multi_pass: NvencMultiPass::QuarterResolution,
        nvenc_spatial_aq: true,
        nvenc_temporal_aq: true,
        nvenc_aq_strength: 15,
        ..Default::default()
    };

    sanitize_nvenc_tuning_fields(&mut cfg, Some(&support));

    assert_eq!(cfg.nvenc_preset, NvencPreset::P2);
    assert_eq!(cfg.nvenc_split_encode_mode, NvencSplitEncodeMode::Auto);
    assert_eq!(cfg.nvenc_multi_pass, NvencMultiPass::Disabled);
    assert!(!cfg.nvenc_spatial_aq);
    assert!(!cfg.nvenc_temporal_aq);
    assert_eq!(cfg.nvenc_aq_strength, 0);

    cfg.nvenc_preset = NvencPreset::P5;
    cfg.nvenc_split_encode_mode = NvencSplitEncodeMode::Disabled;
    cfg.nvenc_multi_pass = NvencMultiPass::FullResolution;
    sanitize_nvenc_tuning_fields(&mut cfg, Some(&support));
    assert_eq!(cfg.nvenc_preset, NvencPreset::P5);
    assert_eq!(cfg.nvenc_split_encode_mode, NvencSplitEncodeMode::Disabled);
    assert_eq!(cfg.nvenc_multi_pass, NvencMultiPass::FullResolution);
}
