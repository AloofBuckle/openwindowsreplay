//! egui 中文界面。

use crate::backend::session::{ReplayController, ReplayState};
use crate::backend::{ProbeCaps, RateControlFeatureSupport};
use crate::config::{AppConfig, CaptureBackend, HotkeyConfig, HotkeyKey};
use crate::hotkey::{HotkeyEvent, HotkeyRuntime};
use crate::rate_control::{RateControlConfig, RateControlMethod};
use eframe::egui;
use std::sync::Arc;
use std::sync::mpsc::{self, Receiver};
use std::time::{Duration, Instant};

pub struct RustReplayApp {
    config: AppConfig,
    caps: Option<ProbeCaps>,
    probe_rx: Option<Receiver<ProbeCaps>>,
    controller: ReplayController,
    encoder_log: Vec<String>,
    loop_log: Vec<String>,
    last_status_refresh: Instant,
    last_saved_config_json: String,
    hotkey: HotkeyRuntime,
    waiting_save_hotkey: bool,
    hotkey_bind_message: Option<String>,
}

impl RustReplayApp {
    pub fn new(cc: &eframe::CreationContext<'_>) -> Self {
        let font_status = install_chinese_font(&cc.egui_ctx);
        let config_path = AppConfig::config_path();
        let (config, config_status) = match AppConfig::load_from_disk() {
            Ok(Some(config)) => (config, format!("配置已加载：{}", config_path.display())),
            Ok(None) => (
                AppConfig::default(),
                format!("未找到配置文件，将使用默认配置：{}", config_path.display()),
            ),
            Err(err) => (
                AppConfig::default(),
                format!("配置加载失败，已使用默认配置：{err}"),
            ),
        };
        let last_saved_config_json = config.stable_json();
        let initial_save_hotkey = config.save_hotkey;
        let mut this = Self {
            config,
            caps: None,
            probe_rx: None,
            controller: ReplayController::default(),
            encoder_log: vec![
                font_status,
                config_status,
                "正在启动能力探测线程……".to_owned(),
            ],
            loop_log: vec!["循环器尚未启动。".to_owned()],
            last_status_refresh: Instant::now(),
            last_saved_config_json,
            hotkey: HotkeyRuntime::new(initial_save_hotkey),
            waiting_save_hotkey: false,
            hotkey_bind_message: Some(format!(
                "当前保存即时回放热键：{}",
                initial_save_hotkey.label()
            )),
        };
        this.start_probe();
        this
    }

    fn start_probe(&mut self) {
        let (tx, rx) = mpsc::channel();
        self.probe_rx = Some(rx);
        self.encoder_log
            .push("开始探测 DXGI adapter 与 oneVPL HEVC 能力。".to_owned());
        std::thread::spawn(move || {
            let caps = crate::backend::probe_all();
            let _ = tx.send(caps);
        });
    }

    fn receive_probe(&mut self) {
        let Some(rx) = self.probe_rx.take() else {
            return;
        };
        match rx.try_recv() {
            Ok(caps) => {
                self.apply_caps(caps);
            }
            Err(mpsc::TryRecvError::Empty) => {
                self.probe_rx = Some(rx);
            }
            Err(mpsc::TryRecvError::Disconnected) => {
                self.encoder_log.push("能力探测线程异常退出。".to_owned());
            }
        }
    }

    fn apply_caps(&mut self, caps: ProbeCaps) {
        self.encoder_log
            .push(format!("能力探测完成：{}", caps.short_status()));
        for adapter in &caps.dxgi_adapters {
            self.encoder_log.push(format!(
                "DXGI[{}] {} LUID={} VRAM={} MiB",
                adapter.index,
                adapter.description,
                adapter.luid_string(),
                adapter.dedicated_video_memory / 1024 / 1024
            ));
        }
        if caps.vpl.available {
            self.encoder_log.push(format!(
                "oneVPL DLL: {}",
                caps.vpl.dll_path.as_deref().unwrap_or("未知")
            ));
            for imp in &caps.vpl.implementations {
                self.encoder_log.push(format!(
                    "VPL[{}] {} API {} {} accel={} HEVC={}",
                    imp.index,
                    imp.impl_name,
                    imp.api_version,
                    imp.implementation,
                    imp.acceleration_mode,
                    imp.hevc_supported
                ));
                if imp.hevc_supported {
                    self.encoder_log.push(format!(
                        "  profiles=[{}] fourcc=[{}] rate=[{}] dx11_texture={}",
                        imp.hevc_profiles.join(", "),
                        imp.input_fourcc.join(", "),
                        imp.rate_controls
                            .iter()
                            .map(|m| m.short_name())
                            .collect::<Vec<_>>()
                            .join(", "),
                        imp.dx11_texture_input_seen
                    ));
                }
            }
        } else {
            self.encoder_log.push(format!(
                "oneVPL 不可用：{}",
                caps.vpl.load_error.as_deref().unwrap_or("未知错误")
            ));
        }
        if !caps.vpl_candidate_chroma.is_empty() {
            self.encoder_log.push(format!(
                "oneVPL 候选色度采样：{}",
                caps.vpl_candidate_chroma
                    .iter()
                    .map(|c| c.doc_label())
                    .collect::<Vec<_>>()
                    .join(" / ")
            ));
        }
        for route in &caps.vpl.current_display_routes {
            if route.fourcc.is_empty() {
                self.encoder_log.push(format!(
                    "当前显示器 route：{} 不可用，{}",
                    route.chroma.doc_label(),
                    route.note
                ));
            } else {
                self.encoder_log.push(format!(
                    "当前显示器 route：{} -> FourCC={} bit_depth={} profile={}；{}",
                    route.chroma.doc_label(),
                    route.fourcc,
                    route.bit_depth,
                    route.profile,
                    route.route_summary
                ));
            }
        }
        for route in caps
            .vpl
            .route_candidates
            .iter()
            .filter(|route| !route.production_record_supported)
        {
            self.encoder_log.push(format!(
                "oneVPL route 未生产化：FourCC={} chroma={} bit_depth={} profile={}；{}",
                route.fourcc,
                route.chroma.doc_label(),
                route.bit_depth,
                route.profile,
                route
                    .production_blocker
                    .as_deref()
                    .unwrap_or(route.note.as_str())
            ));
        }
        for policy in &caps.capture_cursor_policy {
            self.encoder_log.push(format!(
                "光标策略：{} cursor_recording={}，{}",
                policy.backend, policy.cursor_recording, policy.reason
            ));
        }
        self.encoder_log
            .push(format!("色彩策略：{}", caps.color_fidelity_policy.strategy));
        for item in &caps.color_fidelity_policy.cannot_guarantee {
            self.encoder_log.push(format!("色彩限制：{item}"));
        }
        self.encoder_log.push(format!(
            "音频策略：{}；允许重采样={}；{}",
            caps.audio_policy.target_format,
            caps.audio_policy.resampling_allowed,
            caps.audio_policy.timestamp_rule
        ));
        self.encoder_log.push(format!(
            "发布策略：允许打包依赖={}；{}",
            caps.package_policy.bundled_dependencies_allowed, caps.package_policy.note
        ));
        for item in &caps.rate_control_features_by_chroma {
            for feature in &item.features {
                self.encoder_log.push(format!(
                    "码控可选字段：{} {} lookahead={} win_brc={} low_delay={} max_frame_size={} mbbrc={}",
                    item.chroma.doc_label(),
                    feature.method.short_name(),
                    feature.look_ahead_depth,
                    feature.win_brc,
                    feature.low_delay_brc,
                    feature.max_frame_size,
                    feature.mbbrc
                ));
            }
        }
        for reason in &caps.path_blockers {
            self.encoder_log.push(format!("路径阻断：{reason}"));
        }

        if self.config.chroma.is_none()
            || self
                .config
                .chroma
                .is_some_and(|c| !caps.supported_chroma.contains(&c))
        {
            self.config.chroma = caps.supported_chroma.first().copied();
        }
        let selected_chroma = self.config.chroma;
        let supported_rate_controls = selected_chroma
            .map(|chroma| caps.rate_controls_for_chroma(chroma))
            .unwrap_or(&[]);
        if !supported_rate_controls.is_empty()
            && !supported_rate_controls.contains(&self.config.rate_control.method)
        {
            self.config.rate_control.method = supported_rate_controls[0];
        }
        self.caps = Some(caps);
    }

    fn persist_config_if_changed(&mut self) {
        let current = self.config.stable_json();
        if current == self.last_saved_config_json {
            return;
        }
        match self.config.save_to_disk() {
            Ok(()) => {
                self.last_saved_config_json = current;
                self.encoder_log.push(format!(
                    "配置已保存：{}",
                    AppConfig::config_path().display()
                ));
            }
            Err(err) => self.encoder_log.push(format!("配置保存失败：{err}")),
        }
    }

    fn click_start(&mut self) {
        let Some(caps) = &self.caps else {
            self.loop_log
                .push("尚未完成能力探测，不能开始录制。".to_owned());
            return;
        };
        match self.controller.start(&self.config, caps) {
            Ok(()) => self.loop_log.push("即时回放已开始。".to_owned()),
            Err(err) => self.loop_log.push(format!("开始失败：{err}")),
        }
    }

    fn click_save(&mut self) {
        match self.controller.save(&self.config) {
            Ok(()) => self.loop_log.push("即时回放已保存。".to_owned()),
            Err(err) => self.loop_log.push(format!("保存失败：{err}")),
        }
    }

    fn click_stop(&mut self) {
        match self.controller.stop() {
            Ok(()) => self.loop_log.push("即时回放停止请求已发送。".to_owned()),
            Err(err) => self.loop_log.push(format!("停止失败：{err}")),
        }
    }

    fn begin_save_hotkey_binding(&mut self) {
        self.waiting_save_hotkey = true;
        self.hotkey_bind_message = Some("请按新的保存即时回放热键；Esc 取消。".to_owned());
        self.hotkey.set_hotkey(None);
    }

    fn cancel_save_hotkey_binding(&mut self) {
        self.waiting_save_hotkey = false;
        self.hotkey_bind_message = Some(format!(
            "已取消更改，继续使用：{}",
            self.config.save_hotkey.label()
        ));
        self.hotkey.set_hotkey(Some(self.config.save_hotkey));
    }

    fn apply_save_hotkey_binding(&mut self, hotkey: HotkeyConfig) {
        self.config.save_hotkey = hotkey;
        self.waiting_save_hotkey = false;
        self.hotkey_bind_message = Some(format!("保存即时回放热键已改为：{}", hotkey.label()));
        self.hotkey.set_hotkey(Some(hotkey));
    }

    fn handle_save_hotkey_binding_input(&mut self, ctx: &egui::Context) {
        if !self.waiting_save_hotkey {
            return;
        }

        let events = ctx.input(|input| input.events.clone());
        for event in events {
            let egui::Event::Key {
                key,
                pressed: true,
                repeat: false,
                modifiers,
                ..
            } = event
            else {
                continue;
            };

            if key == egui::Key::Escape {
                self.cancel_save_hotkey_binding();
                return;
            }
            if is_modifier_key(key) {
                continue;
            }

            let Some(hotkey_key) = egui_key_to_hotkey_key(key) else {
                self.hotkey_bind_message = Some(
                    "这个按键暂不支持；请使用 F1-F24，或 Ctrl/Alt/Shift + 字母/数字。".to_owned(),
                );
                return;
            };

            let hotkey = HotkeyConfig {
                ctrl: modifiers.ctrl,
                alt: modifiers.alt,
                shift: modifiers.shift,
                key: hotkey_key,
            };
            if !hotkey.is_safe_global_binding() {
                self.hotkey_bind_message = Some(
                    "字母/数字单键会拦截普通输入；请按 Ctrl/Alt/Shift + 字母/数字，或直接使用 F1-F24。"
                        .to_owned(),
                );
                return;
            }

            self.apply_save_hotkey_binding(hotkey);
            return;
        }
    }

    fn handle_hotkey_events(&mut self) {
        for event in self.hotkey.drain_events() {
            match event {
                HotkeyEvent::Pressed => {
                    if self.waiting_save_hotkey {
                        continue;
                    }
                    self.loop_log.push(format!(
                        "保存即时回放热键触发：{}",
                        self.config.save_hotkey.label()
                    ));
                    self.click_save();
                }
                HotkeyEvent::Status(line) => self.loop_log.push(line),
            }
        }
    }

    fn status_text(&self) -> String {
        match self.controller.state() {
            ReplayState::Idle => "状态：空闲".to_owned(),
            ReplayState::Running { started_at } => format!(
                "状态：录制中，已运行 {:.1}s",
                started_at.elapsed().as_secs_f32()
            ),
            ReplayState::Stopping { started_at } => format!(
                "状态：正在停止后台录制，已运行 {:.1}s",
                started_at.elapsed().as_secs_f32()
            ),
        }
    }
}

impl eframe::App for RustReplayApp {
    fn logic(&mut self, ctx: &egui::Context, _frame: &mut eframe::Frame) {
        self.receive_probe();
        for line in self.controller.drain_log_messages() {
            self.loop_log.push(line);
        }
        self.handle_save_hotkey_binding_input(ctx);
        self.handle_hotkey_events();
        if self.last_status_refresh.elapsed() > Duration::from_millis(500) {
            ctx.request_repaint_after(Duration::from_millis(500));
            self.last_status_refresh = Instant::now();
        }
        self.persist_config_if_changed();
    }

    fn ui(&mut self, ui: &mut egui::Ui, _frame: &mut eframe::Frame) {
        egui::Panel::top("top_controls").show(ui, |ui| {
            ui.horizontal_wrapped(|ui| {
                if ui.button("开始即时回放").clicked() {
                    self.click_start();
                }
                if ui.button("保存即时回放").clicked() {
                    self.click_save();
                }
                if ui.button("停止即时回放").clicked() {
                    self.click_stop();
                }
                ui.separator();
                if ui.button("重新探测能力").clicked() {
                    self.start_probe();
                }
                ui.label(self.status_text());
            });
        });

        egui::CentralPanel::default().show(ui, |ui| {
            ui.columns(2, |cols| {
                self.left_encoder_panel(&mut cols[0]);
                self.right_loop_panel(&mut cols[1]);
            });
            ui.separator();
            ui.columns(2, |cols| {
                log_panel(&mut cols[0], "编码器日志", &self.encoder_log);
                log_panel(&mut cols[1], "循环器日志", &self.loop_log);
            });
        });
    }
}

impl RustReplayApp {
    fn left_encoder_panel(&mut self, ui: &mut egui::Ui) {
        ui.heading("编码器参数");
        ui.group(|ui| {
            ui.label("选什么色度采样？");
            let supported = self
                .caps
                .as_ref()
                .map(|c| c.supported_chroma.as_slice())
                .unwrap_or(&[]);
            if supported.is_empty() {
                ui.small("当前没有完整桌面同步路径，已隐藏不可用色度字段；详情见编码器日志。");
            } else {
                for &chroma in supported {
                    ui.radio_value(
                        &mut self.config.chroma,
                        Some(chroma),
                        format!("{} ({})", chroma.doc_label(), chroma.label()),
                    );
                }
            }
        });

        ui.add_space(8.0);
        ui.group(|ui| {
            ui.label("要什么码率控制模式？");
            let supported = self.current_rate_controls();
            if supported.is_empty() {
                ui.small(
                    "当前色度没有经 oneVPL Query 确认的码控模式，已隐藏码控字段；详情见编码器日志。",
                );
            } else {
                egui::ComboBox::from_label("RateControlMethod")
                    .selected_text(self.config.rate_control.method.label())
                    .show_ui(ui, |ui| {
                        for &method in &supported {
                            ui.selectable_value(
                                &mut self.config.rate_control.method,
                                method,
                                method.label(),
                            );
                        }
                    });
                ui.separator();
                if supported.contains(&self.config.rate_control.method) {
                    let features =
                        self.current_rate_control_features(self.config.rate_control.method);
                    rate_control_fields(ui, &mut self.config.rate_control, &features);
                } else if let Some(first) = supported.first() {
                    self.config.rate_control.method = *first;
                    let features =
                        self.current_rate_control_features(self.config.rate_control.method);
                    rate_control_fields(ui, &mut self.config.rate_control, &features);
                }
            }
        });
    }

    fn current_rate_controls(&self) -> Vec<RateControlMethod> {
        let Some(caps) = &self.caps else {
            return Vec::new();
        };
        let Some(chroma) = self.config.chroma else {
            return Vec::new();
        };
        caps.rate_controls_for_chroma(chroma).to_vec()
    }

    fn current_rate_control_features(
        &self,
        method: RateControlMethod,
    ) -> RateControlFeatureSupport {
        let Some(caps) = &self.caps else {
            return RateControlFeatureSupport::hidden(method);
        };
        let Some(chroma) = self.config.chroma else {
            return RateControlFeatureSupport::hidden(method);
        };
        caps.rate_control_features_for(chroma, method)
    }

    fn right_loop_panel(&mut self, ui: &mut egui::Ui) {
        ui.heading("录制循环器参数");
        ui.group(|ui| {
            ui.label("捕获后端");
            for backend in CaptureBackend::all() {
                ui.radio_value(&mut self.config.capture_backend, backend, backend.label());
            }
            ui.small(format!(
                "当前选择：{}；DDA 不录制鼠标光标，WGC 录制鼠标光标。",
                self.config.capture_backend.short_name()
            ));
            ui.separator();
            ui.horizontal(|ui| {
                ui.label("循环缓存的目录 {dir：}");
                ui.text_edit_singleline(&mut self.config.cache_dir);
                if ui.button("选择…").clicked()
                    && let Some(path) = pick_folder(&self.config.cache_dir)
                {
                    self.config.cache_dir = path;
                }
            });
            ui.horizontal(|ui| {
                ui.label("落盘保存的目录 {dir：}");
                ui.text_edit_singleline(&mut self.config.save_dir);
                if ui.button("选择…").clicked()
                    && let Some(path) = pick_folder(&self.config.save_dir)
                {
                    self.config.save_dir = path;
                }
            });
            ui.horizontal(|ui| {
                ui.label("要回放多久 {(允许填小数):min}");
                ui.add(
                    egui::DragValue::new(&mut self.config.replay_minutes)
                        .speed(0.1)
                        .range(0.1..=240.0)
                        .suffix(" min"),
                );
            });
        });

        ui.add_space(8.0);
        ui.group(|ui| {
            ui.label("保存热键");
            ui.horizontal_wrapped(|ui| {
                ui.label("保存即时重放：");
                ui.monospace(self.config.save_hotkey.label());
                let change_label = if self.waiting_save_hotkey {
                    "等待按键…"
                } else {
                    "更改…"
                };
                if ui.button(change_label).clicked() {
                    self.begin_save_hotkey_binding();
                }
                if self.waiting_save_hotkey && ui.button("取消").clicked() {
                    self.cancel_save_hotkey_binding();
                }
                if ui.button("恢复默认").clicked() {
                    self.apply_save_hotkey_binding(HotkeyConfig::default());
                }
            });
            ui.small("只允许更改“保存即时重放”的热键；触发效果等同点击顶部“保存即时回放”。");
            ui.small("字母/数字必须搭配 Ctrl/Alt/Shift；F1-F24 可单独使用。");
            if let Some(message) = &self.hotkey_bind_message {
                ui.small(message);
            }
        });
    }
}

fn rate_control_fields(
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

fn sanitize_hidden_rate_control_fields(
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

fn kbps_fields(ui: &mut egui::Ui, cfg: &mut RateControlConfig, show_max: bool) {
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

fn target_field(ui: &mut egui::Ui, cfg: &mut RateControlConfig) {
    ui.horizontal(|ui| {
        ui.label("TargetKbps");
        ui.add(
            egui::DragValue::new(&mut cfg.target_kbps)
                .speed(100)
                .range(1..=2_000_000),
        );
    });
}

fn qp_fields(ui: &mut egui::Ui, cfg: &mut RateControlConfig) {
    ui.horizontal(|ui| {
        ui.label("QPI");
        ui.add(egui::DragValue::new(&mut cfg.qpi).range(0..=51));
        ui.label("QPP");
        ui.add(egui::DragValue::new(&mut cfg.qpp).range(0..=51));
        ui.label("QPB");
        ui.add(egui::DragValue::new(&mut cfg.qpb).range(0..=51));
    });
}

fn lookahead_field(ui: &mut egui::Ui, cfg: &mut RateControlConfig) {
    ui.horizontal(|ui| {
        ui.label("LookAheadDepth (10-100，0=默认)");
        ui.add(egui::DragValue::new(&mut cfg.look_ahead_depth).range(0..=100));
        if (1..10).contains(&cfg.look_ahead_depth) {
            cfg.look_ahead_depth = 10;
        }
    });
}

fn icq_field(ui: &mut egui::Ui, cfg: &mut RateControlConfig) {
    ui.horizontal(|ui| {
        ui.label("ICQQuality (1 最好，51 最差)");
        ui.add(egui::DragValue::new(&mut cfg.icq_quality).range(1..=51));
    });
}

fn sliding_window_fields(ui: &mut egui::Ui, cfg: &mut RateControlConfig) {
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

fn low_delay_brc_field(ui: &mut egui::Ui, cfg: &mut RateControlConfig) {
    ui.checkbox(
        &mut cfg.low_delay_brc,
        "LowDelayBRC（低延迟码控，按当前 GPU/route 探测显示）",
    );
}

fn max_frame_size_field(ui: &mut egui::Ui, cfg: &mut RateControlConfig) {
    ui.horizontal(|ui| {
        ui.label("MaxFrameSize (bytes, 0=关闭)");
        ui.add(
            egui::DragValue::new(&mut cfg.max_frame_size)
                .speed(1024)
                .range(0..=100_000_000),
        );
    });
}

fn egui_key_to_hotkey_key(key: egui::Key) -> Option<HotkeyKey> {
    Some(match key {
        egui::Key::Num0 => HotkeyKey::Num0,
        egui::Key::Num1 => HotkeyKey::Num1,
        egui::Key::Num2 => HotkeyKey::Num2,
        egui::Key::Num3 => HotkeyKey::Num3,
        egui::Key::Num4 => HotkeyKey::Num4,
        egui::Key::Num5 => HotkeyKey::Num5,
        egui::Key::Num6 => HotkeyKey::Num6,
        egui::Key::Num7 => HotkeyKey::Num7,
        egui::Key::Num8 => HotkeyKey::Num8,
        egui::Key::Num9 => HotkeyKey::Num9,
        egui::Key::A => HotkeyKey::A,
        egui::Key::B => HotkeyKey::B,
        egui::Key::C => HotkeyKey::C,
        egui::Key::D => HotkeyKey::D,
        egui::Key::E => HotkeyKey::E,
        egui::Key::F => HotkeyKey::F,
        egui::Key::G => HotkeyKey::G,
        egui::Key::H => HotkeyKey::H,
        egui::Key::I => HotkeyKey::I,
        egui::Key::J => HotkeyKey::J,
        egui::Key::K => HotkeyKey::K,
        egui::Key::L => HotkeyKey::L,
        egui::Key::M => HotkeyKey::M,
        egui::Key::N => HotkeyKey::N,
        egui::Key::O => HotkeyKey::O,
        egui::Key::P => HotkeyKey::P,
        egui::Key::Q => HotkeyKey::Q,
        egui::Key::R => HotkeyKey::R,
        egui::Key::S => HotkeyKey::S,
        egui::Key::T => HotkeyKey::T,
        egui::Key::U => HotkeyKey::U,
        egui::Key::V => HotkeyKey::V,
        egui::Key::W => HotkeyKey::W,
        egui::Key::X => HotkeyKey::X,
        egui::Key::Y => HotkeyKey::Y,
        egui::Key::Z => HotkeyKey::Z,
        egui::Key::F1 => HotkeyKey::F1,
        egui::Key::F2 => HotkeyKey::F2,
        egui::Key::F3 => HotkeyKey::F3,
        egui::Key::F4 => HotkeyKey::F4,
        egui::Key::F5 => HotkeyKey::F5,
        egui::Key::F6 => HotkeyKey::F6,
        egui::Key::F7 => HotkeyKey::F7,
        egui::Key::F8 => HotkeyKey::F8,
        egui::Key::F9 => HotkeyKey::F9,
        egui::Key::F10 => HotkeyKey::F10,
        egui::Key::F11 => HotkeyKey::F11,
        egui::Key::F12 => HotkeyKey::F12,
        egui::Key::F13 => HotkeyKey::F13,
        egui::Key::F14 => HotkeyKey::F14,
        egui::Key::F15 => HotkeyKey::F15,
        egui::Key::F16 => HotkeyKey::F16,
        egui::Key::F17 => HotkeyKey::F17,
        egui::Key::F18 => HotkeyKey::F18,
        egui::Key::F19 => HotkeyKey::F19,
        egui::Key::F20 => HotkeyKey::F20,
        egui::Key::F21 => HotkeyKey::F21,
        egui::Key::F22 => HotkeyKey::F22,
        egui::Key::F23 => HotkeyKey::F23,
        egui::Key::F24 => HotkeyKey::F24,
        _ => return None,
    })
}

fn is_modifier_key(key: egui::Key) -> bool {
    matches!(
        key,
        egui::Key::ShiftLeft
            | egui::Key::ShiftRight
            | egui::Key::ControlLeft
            | egui::Key::ControlRight
            | egui::Key::AltLeft
            | egui::Key::AltRight
            | egui::Key::SuperLeft
            | egui::Key::SuperRight
    )
}

fn install_chinese_font(ctx: &egui::Context) -> String {
    let candidates = [
        r"C:\Windows\Fonts\msyh.ttc",
        r"C:\Windows\Fonts\msyh.ttf",
        r"C:\Windows\Fonts\simhei.ttf",
        r"C:\Windows\Fonts\simsun.ttc",
        r"C:\Windows\Fonts\NotoSansCJK-Regular.ttc",
    ];

    for path in candidates {
        if let Ok(bytes) = std::fs::read(path) {
            let mut fonts = egui::FontDefinitions::default();
            fonts.font_data.insert(
                "rustreplay_cjk".to_owned(),
                Arc::new(egui::FontData::from_owned(bytes)),
            );
            if let Some(family) = fonts.families.get_mut(&egui::FontFamily::Proportional) {
                family.insert(0, "rustreplay_cjk".to_owned());
            }
            if let Some(family) = fonts.families.get_mut(&egui::FontFamily::Monospace) {
                family.insert(0, "rustreplay_cjk".to_owned());
            }
            ctx.set_fonts(fonts);
            return format!("中文字体已加载：{path}");
        }
    }
    "未找到系统中文字体，界面可能显示方框；请安装 Microsoft YaHei/SimHei/SimSun/Noto CJK".to_owned()
}

#[cfg(windows)]
fn pick_folder(_current: &str) -> Option<String> {
    use windows::Win32::System::Com::{
        CLSCTX_INPROC_SERVER, COINIT_APARTMENTTHREADED, CoCreateInstance, CoInitializeEx,
        CoTaskMemFree,
    };
    use windows::Win32::UI::Shell::{
        FOS_FORCEFILESYSTEM, FOS_PICKFOLDERS, FileOpenDialog, IFileOpenDialog, SIGDN_FILESYSPATH,
    };
    unsafe {
        let _ = CoInitializeEx(None, COINIT_APARTMENTTHREADED);
        let dialog: IFileOpenDialog =
            CoCreateInstance(&FileOpenDialog, None, CLSCTX_INPROC_SERVER).ok()?;
        let mut options = dialog.GetOptions().ok()?;
        options |= FOS_PICKFOLDERS | FOS_FORCEFILESYSTEM;
        dialog.SetOptions(options).ok()?;
        dialog.SetTitle(windows::core::w!("选择目录")).ok()?;
        dialog.Show(None).ok()?;
        let item = dialog.GetResult().ok()?;
        let path = item.GetDisplayName(SIGDN_FILESYSPATH).ok()?;
        let text = path.to_string().ok()?;
        CoTaskMemFree(Some(path.0.cast()));
        Some(text)
    }
}

#[cfg(not(windows))]
fn pick_folder(_current: &str) -> Option<String> {
    None
}

fn log_panel(ui: &mut egui::Ui, title: &str, lines: &[String]) {
    ui.heading(title);
    egui::Frame::group(ui.style()).show(ui, |ui| {
        egui::ScrollArea::vertical()
            .stick_to_bottom(true)
            .max_height(260.0)
            .show(ui, |ui| {
                for line in lines.iter().rev().take(400).rev() {
                    ui.monospace(line);
                }
            });
    });
}
