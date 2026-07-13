use super::*;

pub(super) fn rate_control_fields(
    ui: &mut egui::Ui,
    cfg: &mut RateControlConfig,
    features: &RateControlFeatureSupport,
) {
    sanitize_hidden_rate_control_fields(cfg, features);
    ui.horizontal(|ui| {
        ui.label("BRCParamMultiplier");
        ui.add(egui::DragValue::new(&mut cfg.brc_param_multiplier).range(1..=65535));
    });

    match cfg.method {
        RateControlMethod::Cbr => {
            kbps_fields(ui, cfg, false);
            if features.win_brc {
                sliding_window_fields(ui, cfg);
            }
        }
        RateControlMethod::Vbr => {
            kbps_fields(ui, cfg, true);
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
                lookahead_field(ui, cfg);
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
            kbps_fields(ui, cfg, true);
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
                lookahead_field(ui, cfg);
            }
        }
        RateControlMethod::LaHrd => {
            kbps_fields(ui, cfg, true);
            if features.look_ahead_depth {
                lookahead_field(ui, cfg);
            }
            if features.win_brc {
                sliding_window_fields(ui, cfg);
            }
            if features.max_frame_size {
                max_frame_size_field(ui, cfg);
            }
        }
        RateControlMethod::Qvbr => {
            kbps_fields(ui, cfg, true);
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

    let fields = cfg.to_vpl_fields();
    ui.collapsing("将写入 oneVPL 的字段预览", |ui| {
        ui.monospace(
            serde_json::to_string_pretty(&fields).unwrap_or_else(|_| "<序列化失败>".to_owned()),
        );
    });
}

pub(super) fn sanitize_config_against_caps(config: &mut AppConfig, caps: &ProbeCaps) {
    if config.chroma.is_none()
        || config
            .chroma
            .is_some_and(|chroma| !caps.supported_chroma.contains(&chroma))
    {
        config.chroma = caps.supported_chroma.first().copied();
    }
    let supported_rate_controls = config
        .chroma
        .map(|chroma| caps.rate_controls_for_chroma(chroma))
        .unwrap_or(&[]);
    if !supported_rate_controls.is_empty()
        && !supported_rate_controls.contains(&config.rate_control.method)
    {
        config.rate_control.method = supported_rate_controls[0];
    }
}

pub(super) fn sanitize_hidden_rate_control_fields(
    cfg: &mut RateControlConfig,
    features: &RateControlFeatureSupport,
) {
    if !features.look_ahead_depth {
        cfg.look_ahead_depth = 0;
    } else if (1..10).contains(&cfg.look_ahead_depth) {
        cfg.look_ahead_depth = 10;
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
    cfg.ext_brc = false;
}

pub(super) fn kbps_fields(ui: &mut egui::Ui, cfg: &mut RateControlConfig, show_max: bool) {
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
    ui.horizontal(|ui| {
        ui.label("BufferSizeInKB (0=库计算)");
        ui.add(
            egui::DragValue::new(&mut cfg.buffer_size_kb)
                .speed(64)
                .range(0..=4_000_000),
        );
    });
    ui.horizontal(|ui| {
        ui.label("InitialDelayInKB (0=库计算)");
        ui.add(
            egui::DragValue::new(&mut cfg.initial_delay_kb)
                .speed(64)
                .range(0..=4_000_000),
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

pub(super) fn lookahead_field(ui: &mut egui::Ui, cfg: &mut RateControlConfig) {
    ui.horizontal(|ui| {
        ui.label("LookAheadDepth (10-100，0=默认)");
        ui.add(egui::DragValue::new(&mut cfg.look_ahead_depth).range(0..=100));
        if (1..10).contains(&cfg.look_ahead_depth) {
            cfg.look_ahead_depth = 10;
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
