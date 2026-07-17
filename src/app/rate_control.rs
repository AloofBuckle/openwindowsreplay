use super::*;

pub(super) fn rate_control_fields(
    ui: &mut egui::Ui,
    cfg: &mut RateControlConfig,
    features: &RateControlFeatureSupport,
    backend: Option<VideoEncoderBackend>,
) {
    sanitize_hidden_rate_control_fields(cfg, features, backend);
    if features.brc_param_multiplier {
        ui.horizontal(|ui| {
            ui.label("BRCParamMultiplier");
            ui.add(egui::DragValue::new(&mut cfg.brc_param_multiplier).range(1..=65535));
        });
    }

    match cfg.method {
        RateControlMethod::Cbr => {
            kbps_fields(ui, cfg, false, backend);
            if features.look_ahead_depth {
                lookahead_field(ui, cfg, backend, features.look_ahead_depth_max);
            }
            if features.win_brc {
                sliding_window_fields(ui, cfg);
            }
        }
        RateControlMethod::Vbr => {
            kbps_fields(ui, cfg, true, backend);
            if features.look_ahead_depth {
                lookahead_field(ui, cfg, backend, features.look_ahead_depth_max);
            }
            if features.win_brc {
                sliding_window_fields(ui, cfg);
            }
            if features.low_delay_brc {
                low_delay_brc_field(ui, cfg);
            }
            if features.max_frame_size {
                max_frame_size_field(ui, cfg);
            }
        }
        RateControlMethod::Cqp => {
            qp_fields(ui, cfg);
        }
        RateControlMethod::Avbr => {
            target_field(ui, cfg);
            ui.horizontal(|ui| {
                ui.label("Accuracy (十分之一百分点)");
                ui.add(egui::DragValue::new(&mut cfg.accuracy).range(0..=65535));
            });
            ui.horizontal(|ui| {
                ui.label("Convergence (100 frames)");
                ui.add(egui::DragValue::new(&mut cfg.convergence).range(0..=65535));
            });
        }
        RateControlMethod::La => {
            target_field(ui, cfg);
            if features.look_ahead_depth {
                lookahead_field(ui, cfg, backend, features.look_ahead_depth_max);
            }
            if features.win_brc {
                sliding_window_fields(ui, cfg);
            }
            if features.max_frame_size {
                max_frame_size_field(ui, cfg);
            }
        }
        RateControlMethod::Icq => {
            icq_field(ui, cfg);
        }
        RateControlMethod::Vcm => {
            kbps_fields(ui, cfg, true, backend);
            if features.low_delay_brc {
                low_delay_brc_field(ui, cfg);
            }
            if features.max_frame_size {
                max_frame_size_field(ui, cfg);
            }
        }
        RateControlMethod::LaIcq => {
            icq_field(ui, cfg);
            if features.look_ahead_depth {
                lookahead_field(ui, cfg, backend, features.look_ahead_depth_max);
            }
        }
        RateControlMethod::LaHrd => {
            kbps_fields(ui, cfg, true, backend);
            if features.look_ahead_depth {
                lookahead_field(ui, cfg, backend, features.look_ahead_depth_max);
            }
            if features.win_brc {
                sliding_window_fields(ui, cfg);
            }
            if features.max_frame_size {
                max_frame_size_field(ui, cfg);
            }
        }
        RateControlMethod::Qvbr => {
            kbps_fields(ui, cfg, true, backend);
            ui.horizontal(|ui| {
                ui.label("QVBRQuality (1 最好，51 最差)");
                ui.add(egui::DragValue::new(&mut cfg.qvbr_quality).range(1..=51));
            });
            if features.win_brc {
                sliding_window_fields(ui, cfg);
            }
            if features.low_delay_brc {
                low_delay_brc_field(ui, cfg);
            }
            if features.max_frame_size {
                max_frame_size_field(ui, cfg);
            }
        }
    }

    if features.mbbrc {
        ui.separator();
        ui.checkbox(&mut cfg.mbbrc, "MBBRC（宏块级码率控制，可提升主观质量）");
    }
    cfg.ext_brc = false;

    if matches!(backend, Some(VideoEncoderBackend::Nvenc)) && features.nvenc_target_quality {
        nvenc_quality_fields(ui, cfg, features);
    }

    match backend {
        Some(VideoEncoderBackend::Nvenc) => {
            ui.collapsing("将写入 NVENC NV_ENC_RC_PARAMS 的字段预览", |ui| match cfg
                .to_nvenc_fields()
            {
                Ok(fields) => ui.monospace(
                    serde_json::to_string_pretty(&serde_json::json!({
                        "tuning": cfg.to_nvenc_tuning_fields(),
                        "rate_control": fields,
                    }))
                    .unwrap_or_else(|_| "<序列化失败>".to_owned()),
                ),
                Err(err) => ui.monospace(format!("当前字段不能写入 NVENC：{err}")),
            });
        }
        _ => {
            let fields = cfg.to_vpl_fields();
            ui.collapsing("将写入 oneVPL 的字段预览", |ui| {
                ui.monospace(
                    serde_json::to_string_pretty(&fields)
                        .unwrap_or_else(|_| "<序列化失败>".to_owned()),
                );
            });
        }
    }
}

pub(super) fn sanitize_config_against_caps(
    config: &mut AppConfig,
    caps: &ProbeCaps,
    capture_mode: CaptureMode,
) {
    let supported_chroma = caps.supported_chroma_for_capture_mode(capture_mode);
    if config.chroma.is_none()
        || config
            .chroma
            .is_some_and(|chroma| !supported_chroma.contains(&chroma))
    {
        config.chroma = supported_chroma.first().copied();
    }
    let supported_rate_controls = config
        .chroma
        .map(|chroma| caps.rate_controls_for_capture_mode(capture_mode, chroma))
        .unwrap_or(&[]);
    if !supported_rate_controls.is_empty()
        && !supported_rate_controls.contains(&config.rate_control.method)
    {
        config.rate_control.method = supported_rate_controls[0];
    }
    if let Some(chroma) = config.chroma {
        let features = caps.rate_control_features_for_capture_mode(
            capture_mode,
            chroma,
            config.rate_control.method,
        );
        sanitize_hidden_rate_control_fields(
            &mut config.rate_control,
            &features,
            caps.video_encoder_backend_for_capture_mode(capture_mode),
        );
        let tuning = caps.nvenc_tuning_support_for_capture_mode(capture_mode, chroma);
        sanitize_nvenc_tuning_fields(&mut config.rate_control, tuning.as_ref());
    } else {
        sanitize_nvenc_tuning_fields(&mut config.rate_control, None);
    }
}

pub(super) fn sanitize_hidden_rate_control_fields(
    cfg: &mut RateControlConfig,
    features: &RateControlFeatureSupport,
    backend: Option<VideoEncoderBackend>,
) {
    if !features.brc_param_multiplier {
        cfg.brc_param_multiplier = 1;
    }
    if !features.look_ahead_depth || features.look_ahead_depth_max == 0 {
        cfg.look_ahead_depth = 0;
    } else if matches!(backend, Some(VideoEncoderBackend::Nvenc)) {
        cfg.look_ahead_depth = cfg
            .look_ahead_depth
            .min(features.look_ahead_depth_max.min(31));
    } else {
        if (1..10).contains(&cfg.look_ahead_depth) {
            cfg.look_ahead_depth = 10;
        }
        cfg.look_ahead_depth = cfg
            .look_ahead_depth
            .min(features.look_ahead_depth_max.min(100));
    }
    if matches!(backend, Some(VideoEncoderBackend::Nvenc)) {
        cfg.buffer_size_kb = cfg.buffer_size_kb.min(524_287);
        cfg.initial_delay_kb = cfg.initial_delay_kb.min(524_287);
    }
    if !features.win_brc {
        cfg.win_brc_max_avg_kbps = 0;
        cfg.win_brc_size = 0;
    }
    if !features.low_delay_brc {
        cfg.low_delay_brc = false;
    }
    if !features.max_frame_size {
        cfg.max_frame_size = 0;
    }
    if !features.mbbrc {
        cfg.mbbrc = false;
    }
    if !matches!(backend, Some(VideoEncoderBackend::Nvenc)) {
        cfg.nvenc_spatial_aq = false;
        cfg.nvenc_vbr_target_quality = 0;
    } else {
        if !features.nvenc_spatial_aq {
            cfg.nvenc_spatial_aq = false;
        }
        if !features.nvenc_target_quality || !matches!(cfg.method, RateControlMethod::Vbr) {
            cfg.nvenc_vbr_target_quality = 0;
        } else {
            cfg.nvenc_vbr_target_quality = cfg.nvenc_vbr_target_quality.min(51);
        }
    }
    cfg.nvenc_temporal_aq = false;
    cfg.nvenc_aq_strength = 0;
    cfg.ext_brc = false;
}

pub(super) fn sanitize_nvenc_tuning_fields(
    cfg: &mut RateControlConfig,
    support: Option<&NvencTuningSupport>,
) {
    let Some(support) = support else {
        cfg.nvenc_preset = Default::default();
        cfg.nvenc_split_encode_mode = Default::default();
        cfg.nvenc_multi_pass = Default::default();
        cfg.nvenc_spatial_aq = false;
        cfg.nvenc_temporal_aq = false;
        cfg.nvenc_aq_strength = 0;
        return;
    };

    if !support.presets.contains(&cfg.nvenc_preset) {
        cfg.nvenc_preset = support.presets.first().copied().unwrap_or_default();
    }
    if !support
        .split_encode_modes
        .contains(&cfg.nvenc_split_encode_mode)
    {
        cfg.nvenc_split_encode_mode = support
            .split_encode_modes
            .first()
            .copied()
            .unwrap_or_default();
    }
    if !support.multi_pass_modes.contains(&cfg.nvenc_multi_pass) {
        cfg.nvenc_multi_pass = support
            .multi_pass_modes
            .first()
            .copied()
            .unwrap_or_default();
    }
    if !support.spatial_aq {
        cfg.nvenc_spatial_aq = false;
    }
    cfg.nvenc_temporal_aq = false;
    cfg.nvenc_aq_strength = 0;
}

pub(super) fn nvenc_quality_fields(
    ui: &mut egui::Ui,
    cfg: &mut RateControlConfig,
    features: &RateControlFeatureSupport,
) {
    ui.separator();
    if features.nvenc_target_quality && matches!(cfg.method, RateControlMethod::Vbr) {
        ui.horizontal(|ui| {
            ui.label("NVENC VBR targetQuality (0=自动，1 最好，51 最差)");
            ui.add(egui::DragValue::new(&mut cfg.nvenc_vbr_target_quality).range(0..=51));
        });
    }
}

pub(super) fn nvenc_tuning_fields(
    ui: &mut egui::Ui,
    cfg: &mut RateControlConfig,
    support: &NvencTuningSupport,
) {
    sanitize_nvenc_tuning_fields(cfg, Some(support));

    ui.horizontal(|ui| {
        ui.label("性能预设");
        let combo = egui::ComboBox::from_id_salt("nvenc_preset_guid")
            .selected_text(cfg.nvenc_preset.selected_label())
            .show_ui(ui, |ui| {
                for preset in &support.presets {
                    ui.selectable_value(&mut cfg.nvenc_preset, *preset, preset.label())
                        .on_hover_text(preset.description());
                }
            });
        combo.response.on_hover_text(format!(
            "{}\n原始 GUID 常量：{}",
            cfg.nvenc_preset.description(),
            cfg.nvenc_preset.raw_name()
        ));
    });

    ui.horizontal(|ui| {
        ui.label("分帧编码");
        let combo = egui::ComboBox::from_id_salt("nvenc_split_encode_mode")
            .selected_text(cfg.nvenc_split_encode_mode.selected_label())
            .show_ui(ui, |ui| {
                for mode in &support.split_encode_modes {
                    ui.selectable_value(&mut cfg.nvenc_split_encode_mode, *mode, mode.label())
                        .on_hover_text(mode.description(support.encoder_engines));
                }
            });
        combo.response.on_hover_text(format!(
            "{}\n原始字段：NV_ENC_INITIALIZE_PARAMS::splitEncodeMode = {}",
            cfg.nvenc_split_encode_mode
                .description(support.encoder_engines),
            cfg.nvenc_split_encode_mode.raw_value()
        ));
    });

    ui.horizontal(|ui| {
        ui.label("二次编码");
        let combo = egui::ComboBox::from_id_salt("nvenc_multi_pass")
            .selected_text(cfg.nvenc_multi_pass.selected_label())
            .show_ui(ui, |ui| {
                for mode in &support.multi_pass_modes {
                    ui.selectable_value(&mut cfg.nvenc_multi_pass, *mode, mode.label())
                        .on_hover_text(mode.description());
                }
            });
        combo.response.on_hover_text(format!(
            "{}\n原始字段：NV_ENC_RC_PARAMS::multiPass = {}",
            cfg.nvenc_multi_pass.description(),
            cfg.nvenc_multi_pass.raw_value()
        ));
    });

    if support.spatial_aq {
        ui.checkbox(
            &mut cfg.nvenc_spatial_aq,
            "空间自适应量化（Spatial AQ）",
        )
        .on_hover_text(
            "按画面空间复杂度调整块级 QP，通常改善主观质量；原始字段为 NV_ENC_RC_PARAMS::enableAQ。AQ strength 保持 0，由驱动自动选择。",
        );
    }
}

pub(super) fn kbps_fields(
    ui: &mut egui::Ui,
    cfg: &mut RateControlConfig,
    show_max: bool,
    backend: Option<VideoEncoderBackend>,
) {
    target_field(ui, cfg);
    if show_max {
        ui.horizontal(|ui| {
            ui.label("MaxKbps");
            ui.add(
                egui::DragValue::new(&mut cfg.max_kbps)
                    .speed(100)
                    .range(0..=2_000_000),
            );
        });
    }
    let buffer_max_kb = if matches!(backend, Some(VideoEncoderBackend::Nvenc)) {
        // NVENC VBV fields are u32 bits, so 524_287 KiB is the largest safe
        // value representable after KiB -> bits conversion.
        524_287
    } else {
        4_000_000
    };
    ui.horizontal(|ui| {
        ui.label("BufferSizeInKB (0=库计算)");
        ui.add(
            egui::DragValue::new(&mut cfg.buffer_size_kb)
                .speed(64)
                .range(0..=buffer_max_kb),
        );
    });
    ui.horizontal(|ui| {
        ui.label("InitialDelayInKB (0=库计算)");
        ui.add(
            egui::DragValue::new(&mut cfg.initial_delay_kb)
                .speed(64)
                .range(0..=buffer_max_kb),
        );
    });
}

pub(super) fn target_field(ui: &mut egui::Ui, cfg: &mut RateControlConfig) {
    ui.horizontal(|ui| {
        ui.label("TargetKbps");
        ui.add(
            egui::DragValue::new(&mut cfg.target_kbps)
                .speed(100)
                .range(1..=2_000_000),
        );
    });
}

pub(super) fn qp_fields(ui: &mut egui::Ui, cfg: &mut RateControlConfig) {
    ui.horizontal(|ui| {
        ui.label("QPI");
        ui.add(egui::DragValue::new(&mut cfg.qpi).range(0..=51));
        ui.label("QPP");
        ui.add(egui::DragValue::new(&mut cfg.qpp).range(0..=51));
        ui.label("QPB");
        ui.add(egui::DragValue::new(&mut cfg.qpb).range(0..=51));
    });
}

pub(super) fn lookahead_field(
    ui: &mut egui::Ui,
    cfg: &mut RateControlConfig,
    backend: Option<VideoEncoderBackend>,
    max_depth: u16,
) {
    ui.horizontal(|ui| {
        if matches!(backend, Some(VideoEncoderBackend::Nvenc)) {
            let max_depth = max_depth.min(31);
            ui.label(format!("NVENC LookAheadDepth (0-{max_depth}，0=关闭/默认)"));
            ui.add(egui::DragValue::new(&mut cfg.look_ahead_depth).range(0..=max_depth));
        } else {
            let max_depth = max_depth.min(100);
            ui.label(format!("LookAheadDepth (10-{max_depth}，0=默认)"));
            ui.add(egui::DragValue::new(&mut cfg.look_ahead_depth).range(0..=max_depth));
            if (1..10).contains(&cfg.look_ahead_depth) {
                cfg.look_ahead_depth = 10;
            }
        }
    });
}

pub(super) fn icq_field(ui: &mut egui::Ui, cfg: &mut RateControlConfig) {
    ui.horizontal(|ui| {
        ui.label("ICQQuality (1 最好，51 最差)");
        ui.add(egui::DragValue::new(&mut cfg.icq_quality).range(1..=51));
    });
}

pub(super) fn sliding_window_fields(ui: &mut egui::Ui, cfg: &mut RateControlConfig) {
    ui.horizontal(|ui| {
        ui.label("WinBRCMaxAvgKbps (0=关闭)");
        ui.add(
            egui::DragValue::new(&mut cfg.win_brc_max_avg_kbps)
                .speed(100)
                .range(0..=2_000_000),
        );
    });
    ui.horizontal(|ui| {
        ui.label("WinBRCSize (frames, 0=关闭)");
        ui.add(egui::DragValue::new(&mut cfg.win_brc_size).range(0..=65535));
    });
}

pub(super) fn low_delay_brc_field(ui: &mut egui::Ui, cfg: &mut RateControlConfig) {
    ui.checkbox(
        &mut cfg.low_delay_brc,
        "LowDelayBRC（低延迟码控，按当前 GPU/route 探测显示）",
    );
}

pub(super) fn max_frame_size_field(ui: &mut egui::Ui, cfg: &mut RateControlConfig) {
    ui.horizontal(|ui| {
        ui.label("MaxFrameSize (bytes, 0=关闭)");
        ui.add(
            egui::DragValue::new(&mut cfg.max_frame_size)
                .speed(1024)
                .range(0..=100_000_000),
        );
    });
}
