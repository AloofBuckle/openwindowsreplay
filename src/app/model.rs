use super::*;

use std::time::{Duration, Instant};

pub(super) const INDICATOR_FLASH_DURATION: Duration = Duration::from_secs(2);
pub(super) const MAX_VISIBLE_LOG_LINES: usize = 400;
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum IndicatorColor {
    Blue,
    Green,
    Red,
    Yellow,
    Purple,
}

impl IndicatorColor {
    pub(super) const ALL: [Self; 5] = [
        Self::Blue,
        Self::Green,
        Self::Red,
        Self::Yellow,
        Self::Purple,
    ];

    pub(super) const fn label(self) -> &'static str {
        match self {
            Self::Blue => "blue",
            Self::Green => "green",
            Self::Red => "red",
            Self::Yellow => "yellow",
            Self::Purple => "purple",
        }
    }

    pub(super) const fn color32(self) -> egui::Color32 {
        match self {
            Self::Blue => egui::Color32::from_rgb(0, 122, 255),
            Self::Green => egui::Color32::from_rgb(0, 200, 90),
            Self::Red => egui::Color32::from_rgb(255, 55, 55),
            Self::Yellow => egui::Color32::from_rgb(255, 210, 0),
            Self::Purple => egui::Color32::from_rgb(190, 0, 255),
        }
    }

    pub(super) const fn index(self) -> usize {
        match self {
            Self::Blue => 0,
            Self::Green => 1,
            Self::Red => 2,
            Self::Yellow => 3,
            Self::Purple => 4,
        }
    }
}

#[derive(Clone)]
pub(super) struct IndicatorImages {
    pub(super) diameter_px: u32,
    pub(super) images: Vec<egui::ColorImage>,
}

impl IndicatorImages {
    pub(super) fn image(&self, color: IndicatorColor) -> egui::ColorImage {
        self.images.get(color.index()).cloned().unwrap_or_else(|| {
            build_circle_indicator_images(self.diameter_px.max(1))[color.index()].clone()
        })
    }
}

pub struct RustReplayApp {
    pub(super) config: AppConfig,
    /// 本次启动实际使用的捕获模式。NvFBC 偏好不可用时仅此字段回退，
    /// 不覆盖配置文件中的专用捕获偏好。
    pub(super) effective_capture_mode: CaptureMode,
    pub(super) caps: Option<ProbeCaps>,
    pub(super) probe_rx: Option<Receiver<ProbeCaps>>,
    pub(super) controller: ReplayController,
    pub(super) encoder_log: Vec<String>,
    pub(super) loop_log: Vec<String>,
    pub(super) last_status_refresh: Instant,
    pub(super) last_saved_config_json: String,
    pub(super) hotkey: HotkeyRuntime,
    pub(super) tray: TrayRuntime,
    pub(super) indicator_overlay: IndicatorOverlayRuntime,
    pub(super) indicator_images: IndicatorImages,
    pub(super) indicator_flash: Option<(IndicatorColor, Instant)>,
    pub(super) last_indicator_color: Option<IndicatorColor>,
    pub(super) configuring_indicator_position: bool,
    pub(super) waiting_save_hotkey: bool,
    pub(super) startup_auto_start_pending: bool,
    pub(super) startup_hidden_to_tray: bool,
    pub(super) allow_exit: bool,
    pub(super) single_instance: SingleInstance,
}

impl RustReplayApp {
    pub fn new(cc: &eframe::CreationContext<'_>, single_instance: SingleInstance) -> Self {
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
        let (indicator_images, indicator_status) = build_indicator_images_for_config(&config);
        let native_indicator_images = native_images_from_indicator_images(&indicator_images);
        let indicator_x = config.indicator.position_x.round() as i32;
        let indicator_y = config.indicator.position_y.round() as i32;
        let indicator_text_enabled = config.indicator.text_enabled;
        let startup_auto_start_pending = config.start_recording_on_launch;
        let startup_hidden_to_tray = config.start_minimized_to_tray;
        let mut this = Self {
            config,
            effective_capture_mode: CaptureMode::Generic,
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
            hotkey: HotkeyRuntime::new(initial_save_hotkey, cc.egui_ctx.clone()),
            tray: TrayRuntime::new(cc.egui_ctx.clone()),
            indicator_overlay: IndicatorOverlayRuntime::new(
                indicator_x,
                indicator_y,
                native_indicator_images,
                indicator_text_enabled,
            ),
            indicator_images,
            indicator_flash: None,
            last_indicator_color: None,
            configuring_indicator_position: false,
            waiting_save_hotkey: false,
            startup_auto_start_pending,
            startup_hidden_to_tray,
            allow_exit: false,
            single_instance,
        };
        if let Some(status) = indicator_status {
            this.loop_log.push(status);
        }
        this.start_probe();
        if startup_hidden_to_tray || startup_auto_start_pending {
            cc.egui_ctx.request_repaint();
            cc.egui_ctx
                .request_repaint_after(Duration::from_millis(100));
        }
        this
    }

    pub(super) fn start_probe(&mut self) {
        if self.probe_rx.is_some() {
            self.encoder_log
                .push("能力探测正在进行，忽略重复探测请求。".to_owned());
            return;
        }
        if !matches!(self.controller.state(), ReplayState::Idle) {
            self.encoder_log
                .push("后台录制运行中，不能重新探测能力。".to_owned());
            return;
        }
        let (tx, rx) = mpsc::channel();
        self.caps = None;
        self.probe_rx = Some(rx);
        self.encoder_log
            .push("开始探测 DXGI adapter 与 oneVPL HEVC 能力。".to_owned());
        std::thread::spawn(move || {
            let caps = crate::backend::probe_all();
            let _ = tx.send(caps);
        });
    }

    pub(super) fn receive_probe(&mut self) {
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

    pub(super) fn apply_caps(&mut self, caps: ProbeCaps) {
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
        self.encoder_log.push(format!(
            "自动编码器选择：active={}；{}",
            caps.video_encoder_selection
                .active
                .map(|backend| backend.label())
                .unwrap_or("无"),
            caps.video_encoder_selection.reason
        ));
        for candidate in &caps.video_encoder_selection.candidates {
            self.encoder_log.push(format!(
                "  候选编码器 {} available={} HEVC={} d3d11_texture={} current_routes={} production_ready={}；{}",
                candidate.backend.label(),
                candidate.available,
                candidate.hevc_supported,
                candidate.d3d11_texture_input_supported,
                candidate.current_display_route_count,
                candidate.production_ready,
                candidate.reason
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
        if caps.nvenc.available {
            self.encoder_log.push(format!(
                "NVENC DLL: {}，compiled API={}，driver max API={}",
                caps.nvenc.dll_path.as_deref().unwrap_or("未知"),
                caps.nvenc.compiled_api_version,
                caps.nvenc
                    .max_supported_version
                    .as_deref()
                    .unwrap_or("未知")
            ));
            for adapter in &caps.nvenc.adapters {
                self.encoder_log.push(format!(
                    "NVENC[adapter{}] {} LUID={} HEVC={} d3d11_session={} profiles=[{}] presets=[{}] input=[{}] rate=[{}]",
                    adapter.adapter_index,
                    adapter.adapter_name,
                    adapter.adapter_luid,
                    adapter.hevc_supported,
                    adapter.d3d11_session_opened,
                    adapter.hevc_profiles.join(", "),
                    adapter
                        .hevc_presets
                        .iter()
                        .map(|preset| preset.raw_name())
                        .collect::<Vec<_>>()
                        .join(", "),
                    adapter.input_formats.join(", "),
                    adapter
                        .rate_controls
                        .iter()
                        .map(|m| m.short_name())
                        .collect::<Vec<_>>()
                        .join(", "),
                ));
                self.encoder_log.push(format!(
                    "  caps: max={}x{} engines={:?} async={:?} 10bit={:?} 422={:?} 444={:?} lookahead={:?} temporal_aq={:?} rc_mask={:?}",
                    adapter.caps.max_width.unwrap_or_default(),
                    adapter.caps.max_height.unwrap_or_default(),
                    adapter.caps.encoder_engines,
                    adapter.caps.async_encode,
                    adapter.caps.ten_bit,
                    adapter.caps.yuv422,
                    adapter.caps.yuv444,
                    adapter.caps.lookahead,
                    adapter.caps.temporal_aq,
                    adapter.caps.rate_control_mask,
                ));
                for route in &adapter.route_candidates {
                    self.encoder_log.push(format!(
                        "  NVENC route 可见：input={} chroma={} bit_depth={} profile={}；production={}；{}",
                        route.input_format,
                        route.chroma.doc_label(),
                        route.bit_depth,
                        route.profile,
                        route.production_record_supported,
                        route
                            .production_blocker
                            .as_deref()
                            .unwrap_or(route.note.as_str())
                    ));
                    for feature in &route.rate_control_features {
                        self.encoder_log.push(format!(
                            "    NVENC 码控字段：{} lookahead={} vbv={} spatial_aq={} temporal_aq={} target_quality={}",
                            feature.method.short_name(),
                            feature.lookahead,
                            feature.vbv,
                            feature.spatial_aq,
                            feature.temporal_aq,
                            feature.target_quality
                        ));
                    }
                }
                for route in &adapter.current_display_routes {
                    if route.input_format.is_empty() {
                        self.encoder_log.push(format!(
                            "  NVENC 当前显示器 route：{} 不可用，{}",
                            route.chroma.doc_label(),
                            route.note
                        ));
                    } else {
                        self.encoder_log.push(format!(
                            "  NVENC 当前显示器 route：{} -> input={} bit_depth={} profile={}；{}",
                            route.chroma.doc_label(),
                            route.input_format,
                            route.bit_depth,
                            route.profile,
                            route.route_summary
                        ));
                    }
                }
                for warning in &adapter.warnings {
                    self.encoder_log
                        .push(format!("  NVENC adapter warning：{warning}"));
                }
            }
        } else {
            self.encoder_log.push(format!(
                "NVENC 不可用：{}",
                caps.nvenc.load_error.as_deref().unwrap_or("未知错误")
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
                    "码控可选字段：{} {} brc_multiplier={} lookahead={} lookahead_max={} win_brc={} low_delay={} max_frame_size={} mbbrc={} nvenc_spatial_aq={} nvenc_temporal_aq={} nvenc_target_quality={}",
                    item.chroma.doc_label(),
                    feature.method.short_name(),
                    feature.brc_param_multiplier,
                    feature.look_ahead_depth,
                    feature.look_ahead_depth_max,
                    feature.win_brc,
                    feature.low_delay_brc,
                    feature.max_frame_size,
                    feature.mbbrc,
                    feature.nvenc_spatial_aq,
                    feature.nvenc_temporal_aq,
                    feature.nvenc_target_quality
                ));
            }
        }
        for reason in &caps.path_blockers {
            self.encoder_log.push(format!("路径阻断：{reason}"));
        }

        #[cfg(windows)]
        if caps.nvfbc.available {
            self.encoder_log.push(format!(
                "NvFBC 本次启动探测：available=true version={} display={}x{} routes=[{}] rate=[{}] presets=[{}] engines={}",
                caps.nvfbc.nvfbc_version,
                caps.nvfbc.capture_width,
                caps.nvfbc.capture_height,
                caps.nvfbc
                    .routes
                    .iter()
                    .filter(|route| route.supported)
                    .map(|route| route.chroma.doc_label())
                    .collect::<Vec<_>>()
                    .join("/"),
                caps.nvfbc
                    .rate_controls
                    .iter()
                    .map(|method| method.short_name())
                    .collect::<Vec<_>>()
                    .join(", "),
                caps.nvfbc
                    .presets
                    .iter()
                    .map(|preset| preset.raw_name())
                    .collect::<Vec<_>>()
                    .join(", "),
                caps.nvfbc.encoder_engines,
            ));
        } else {
            self.encoder_log.push(format!(
                "NvFBC 本次启动探测：不可用；{}",
                caps.nvfbc
                    .error
                    .as_deref()
                    .unwrap_or("未形成支持的专用捕获 route")
            ));
        }

        self.effective_capture_mode = caps.effective_capture_mode(self.config.capture_mode);
        if self.config.capture_mode == CaptureMode::DedicatedNvFbc
            && self.effective_capture_mode == CaptureMode::Generic
        {
            self.encoder_log.push(
                "配置请求 NvFBC 专用捕获，但本次启动探测不可用；仅本次运行回退到配置中保留的 DDA/WGC 通用捕获，专用偏好未被覆盖。"
                    .to_owned(),
            );
        } else {
            self.encoder_log.push(format!(
                "本次启动有效捕获模式：{}",
                self.effective_capture_mode.label()
            ));
        }

        sanitize_config_against_caps(&mut self.config, &caps, self.effective_capture_mode);
        self.caps = Some(caps);
        if self.startup_auto_start_pending && self.config.start_recording_on_launch {
            self.loop_log
                .push("随启动开始已启用，等待开始按钮可用后自动开始录制。".to_owned());
        } else {
            self.startup_auto_start_pending = false;
        }
    }

    pub(super) fn persist_config_if_changed(&mut self) {
        if self.config_read_only() {
            return;
        }
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

    pub(super) fn click_start(&mut self) -> bool {
        if self.is_initializing() {
            self.loop_log
                .push("等待初始化完成，暂不能开始录制。".to_owned());
            return false;
        }
        let Some(caps) = &self.caps else {
            self.loop_log
                .push("尚未完成能力探测，不能开始录制。".to_owned());
            return false;
        };
        match self.controller.start(&self.config, caps) {
            Ok(()) => {
                self.indicator_flash = None;
                self.loop_log.push("即时回放已开始。".to_owned());
                true
            }
            Err(err) => {
                self.loop_log.push(format!("开始失败：{err}"));
                false
            }
        }
    }

    pub(super) fn click_save(&mut self) {
        if self.is_initializing() {
            self.loop_log
                .push("等待初始化完成，暂不能保存回放。".to_owned());
            return;
        }
        for line in self.controller.drain_log_messages() {
            self.loop_log.push(line);
        }
        match self.controller.save_readiness() {
            ReplaySaveReadiness::Ready => {}
            ReplaySaveReadiness::Idle => {
                self.loop_log
                    .push("当前没有正在运行的后台录制，不能保存回放。".to_owned());
                return;
            }
            ReplaySaveReadiness::Starting => {
                self.indicator_flash = Some((IndicatorColor::Yellow, Instant::now()));
                self.loop_log.push(
                    "后台录制正在初始化，等待 encoded ring 收到首批 HEVC/AAC 后再保存。".to_owned(),
                );
                return;
            }
            ReplaySaveReadiness::WaitingForAudio => {
                self.indicator_flash = Some((IndicatorColor::Yellow, Instant::now()));
                self.loop_log.push(
                    "后台录制已有视频 access unit，仍在等待音频 access unit；请稍后保存。"
                        .to_owned(),
                );
                return;
            }
        }
        match self.controller.save(&self.config) {
            Ok(()) => {
                self.indicator_flash = Some((IndicatorColor::Green, Instant::now()));
                self.loop_log.push("即时回放已保存。".to_owned());
            }
            Err(err) => {
                self.indicator_flash = Some((IndicatorColor::Red, Instant::now()));
                self.loop_log.push(format!("保存失败：{err}"));
            }
        }
    }

    pub(super) fn click_stop(&mut self) {
        if self.is_initializing() {
            self.loop_log
                .push("等待初始化完成，暂不能停止回放。".to_owned());
            return;
        }
        if !self.can_stop_replay() {
            self.loop_log
                .push("当前没有正在运行的后台录制，不能停止回放。".to_owned());
            return;
        }
        match self.controller.stop() {
            Ok(()) => {
                self.indicator_flash = None;
                self.loop_log.push("即时回放停止请求已发送。".to_owned());
            }
            Err(err) => self.loop_log.push(format!("停止失败：{err}")),
        }
    }

    pub(super) fn begin_save_hotkey_binding(&mut self) {
        self.waiting_save_hotkey = true;
        self.hotkey.set_hotkey(None);
    }

    pub(super) fn cancel_save_hotkey_binding(&mut self) {
        self.waiting_save_hotkey = false;
        self.hotkey.set_hotkey(Some(self.config.save_hotkey));
    }

    pub(super) fn apply_save_hotkey_binding(&mut self, hotkey: HotkeyConfig) {
        self.config.save_hotkey = hotkey;
        self.waiting_save_hotkey = false;
        self.loop_log
            .push(format!("保存即时回放热键已改为：{}", hotkey.label()));
        self.hotkey.set_hotkey(Some(hotkey));
    }

    pub(super) fn handle_save_hotkey_binding_input(&mut self, ctx: &egui::Context) {
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
                self.loop_log
                    .push("这个热键暂不支持；请换一个按键组合。".to_owned());
                return;
            };

            let hotkey = HotkeyConfig {
                ctrl: modifiers.ctrl,
                alt: modifiers.alt,
                shift: modifiers.shift,
                key: hotkey_key,
            };
            if !hotkey.is_safe_global_binding() {
                self.loop_log
                    .push("字母/数字单键会拦截普通输入，请搭配 Ctrl/Alt/Shift。".to_owned());
                return;
            }

            self.apply_save_hotkey_binding(hotkey);
            return;
        }
    }

    pub(super) fn handle_hotkey_events(&mut self) {
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

    pub(super) fn handle_tray_events(&mut self, ctx: &egui::Context) {
        for event in self.tray.drain_events() {
            match event {
                TrayEvent::OpenMainWindow => self.open_main_window(ctx),
                TrayEvent::StartReplay => {
                    let _ = self.click_start();
                }
                TrayEvent::SaveReplay => self.click_save(),
                TrayEvent::StopReplay => self.click_stop(),
                TrayEvent::ExitProgram => {
                    self.loop_log.push("收到退出程序请求。".to_owned());
                    self.allow_exit = true;
                    ctx.send_viewport_cmd(egui::ViewportCommand::Close);
                }
                TrayEvent::Status(line) => self.loop_log.push(line),
            }
        }
    }

    pub(super) fn handle_single_instance_activation(&mut self, ctx: &egui::Context) {
        if self.single_instance.take_activate_request() {
            self.open_main_window(ctx);
        }
    }

    pub(super) fn handle_close_request(&mut self, ctx: &egui::Context) {
        if ctx.input(|input| input.viewport().close_requested()) && !self.allow_exit {
            ctx.send_viewport_cmd(egui::ViewportCommand::CancelClose);
            self.minimize_to_taskbar(ctx);
        }
    }

    pub(super) fn minimize_to_taskbar(&mut self, ctx: &egui::Context) {
        self.loop_log
            .push("窗口已收起到任务栏，后台录制状态不变。".to_owned());
        ctx.send_viewport_cmd(egui::ViewportCommand::Minimized(true));
    }

    pub(super) fn open_main_window(&mut self, ctx: &egui::Context) {
        self.startup_hidden_to_tray = false;
        self.loop_log.push("已打开主界面。".to_owned());
        restore_root_window_from_tray_start();
        ctx.send_viewport_cmd(egui::ViewportCommand::Visible(true));
        ctx.send_viewport_cmd(egui::ViewportCommand::Minimized(false));
        if let Some(command) = egui::ViewportCommand::center_on_screen(ctx) {
            ctx.send_viewport_cmd(command);
        }
        ctx.send_viewport_cmd(egui::ViewportCommand::Focus);
        ctx.request_repaint();
    }

    pub(super) fn reset_all_config(&mut self) {
        match AppConfig::clear_global_entry() {
            Ok(()) => self.encoder_log.push(format!(
                "已清空全局配置：{}",
                AppConfig::config_path().display()
            )),
            Err(err) => self.encoder_log.push(format!("重置配置失败：{err}")),
        }

        self.config = AppConfig::default();
        if let Some(caps) = &self.caps {
            self.effective_capture_mode = caps.effective_capture_mode(self.config.capture_mode);
            sanitize_config_against_caps(&mut self.config, caps, self.effective_capture_mode);
        }
        self.rebuild_indicator_images();
        self.configuring_indicator_position = false;
        self.indicator_flash = None;
        self.waiting_save_hotkey = false;
        self.startup_auto_start_pending = false;
        self.startup_hidden_to_tray = false;
        self.hotkey.set_hotkey(Some(self.config.save_hotkey));
        self.last_saved_config_json.clear();
        self.persist_config_if_changed();
    }

    pub(super) fn restart_with_capture_mode(
        &mut self,
        ctx: &egui::Context,
        capture_mode: CaptureMode,
    ) {
        if self.config_read_only() || self.is_initializing() {
            self.loop_log
                .push("录制运行或能力探测期间不能切换捕获模式。".to_owned());
            return;
        }
        let previous = self.config.capture_mode;
        self.config.capture_mode = capture_mode;
        let current = self.config.stable_json();
        if let Err(err) = self.config.save_to_disk() {
            self.config.capture_mode = previous;
            self.loop_log
                .push(format!("切换捕获模式前保存配置失败：{err}"));
            return;
        }
        self.last_saved_config_json = current;
        if let Err(err) = spawn_relaunch_after_exit() {
            self.config.capture_mode = previous;
            if let Err(rollback_err) = self.config.save_to_disk() {
                self.loop_log.push(format!(
                    "重启失败：{err}；恢复原捕获模式时写配置也失败：{rollback_err}"
                ));
            } else {
                self.last_saved_config_json = self.config.stable_json();
                self.loop_log
                    .push(format!("重启失败，已恢复原捕获模式：{err}"));
            }
            return;
        }
        self.loop_log.push(format!(
            "已保存捕获模式为 {}，正在重启应用。",
            capture_mode.label()
        ));
        self.allow_exit = true;
        ctx.send_viewport_cmd(egui::ViewportCommand::Close);
    }

    pub(super) fn update_indicator_runtime(&mut self, ctx: &egui::Context) {
        if !self.replay_indicator_visible() {
            self.indicator_flash = None;
        }

        if self
            .indicator_flash
            .as_ref()
            .is_some_and(|(_, shown_at)| shown_at.elapsed() >= INDICATOR_FLASH_DURATION)
        {
            self.indicator_flash = None;
            ctx.request_repaint();
        } else if self.indicator_flash.is_some() {
            ctx.request_repaint_after(Duration::from_millis(100));
        }
        self.sync_indicator_overlay();
    }

    pub(super) fn replay_indicator_visible(&self) -> bool {
        !matches!(self.controller.state(), ReplayState::Idle)
    }

    pub(super) fn current_indicator_color(&self) -> Option<IndicatorColor> {
        match self.controller.state() {
            ReplayState::Idle => None,
            ReplayState::Running { .. } => self
                .indicator_flash
                .map(|(color, _)| color)
                .or(Some(IndicatorColor::Blue)),
            ReplayState::Stopping { .. } => Some(IndicatorColor::Yellow),
        }
    }

    pub(super) fn rebuild_indicator_images(&mut self) {
        let (images, status) = build_indicator_images_for_config(&self.config);
        self.indicator_images = images;
        self.configure_indicator_overlay();
        if let Some(status) = status {
            self.loop_log.push(status);
        }
    }

    pub(super) fn configure_indicator_overlay(&self) {
        self.indicator_overlay.configure(
            self.config.indicator.position_x.round() as i32,
            self.config.indicator.position_y.round() as i32,
            native_images_from_indicator_images(&self.indicator_images),
            self.config.indicator.text_enabled,
        );
    }

    pub(super) fn sync_indicator_overlay(&mut self) {
        let desired = self.current_indicator_color();
        if desired == self.last_indicator_color {
            return;
        }
        match desired {
            Some(color) => self.indicator_overlay.show(color.index()),
            None => self.indicator_overlay.hide(),
        }
        self.last_indicator_color = desired;
    }

    pub(super) fn import_indicator_image(&mut self) {
        let Some(path) = pick_image_file() else {
            return;
        };
        self.config.indicator.image_path = Some(path);
        self.rebuild_indicator_images();
    }

    pub(super) fn status_text(&self) -> String {
        if self.is_initializing() {
            return "状态：等待初始化".to_owned();
        }
        if self.startup_auto_start_pending && matches!(self.controller.state(), ReplayState::Idle) {
            return "状态：等待随启动开始".to_owned();
        }
        match self.controller.state() {
            ReplayState::Idle => "状态：空闲".to_owned(),
            ReplayState::Running { started_at } => match self.controller.save_readiness() {
                ReplaySaveReadiness::Starting => format!(
                    "状态：录制初始化中，已运行 {:.1}s",
                    started_at.elapsed().as_secs_f32()
                ),
                ReplaySaveReadiness::WaitingForAudio => format!(
                    "状态：录制中，等待音频，已运行 {:.1}s",
                    started_at.elapsed().as_secs_f32()
                ),
                ReplaySaveReadiness::Ready => format!(
                    "状态：录制中，可保存，已运行 {:.1}s",
                    started_at.elapsed().as_secs_f32()
                ),
                ReplaySaveReadiness::Idle => format!(
                    "状态：录制中，已运行 {:.1}s",
                    started_at.elapsed().as_secs_f32()
                ),
            },
            ReplayState::Stopping { started_at } => format!(
                "状态：正在停止后台录制，已运行 {:.1}s",
                started_at.elapsed().as_secs_f32()
            ),
        }
    }

    pub(super) fn can_start_replay(&self) -> bool {
        self.probe_rx.is_none()
            && self.caps.is_some()
            && matches!(self.controller.state(), ReplayState::Idle)
    }

    pub(super) fn can_save_replay(&self) -> bool {
        !self.is_initializing() && self.controller.save_readiness().can_save()
    }

    pub(super) fn can_stop_replay(&self) -> bool {
        !self.is_initializing()
            && matches!(
                self.controller.state(),
                ReplayState::Running { .. } | ReplayState::Stopping { .. }
            )
    }

    pub(super) fn config_read_only(&self) -> bool {
        matches!(
            self.controller.state(),
            ReplayState::Running { .. } | ReplayState::Stopping { .. }
        )
    }

    pub(super) fn config_is_persisted(&self) -> bool {
        self.config.stable_json() == self.last_saved_config_json
    }

    pub(super) fn is_initializing(&self) -> bool {
        self.probe_rx.is_some()
    }

    pub(super) fn maintain_startup_tray_hidden(&mut self, ctx: &egui::Context) {
        if !self.startup_hidden_to_tray {
            return;
        }
        prepare_root_window_for_tray_start();
        ctx.send_viewport_cmd(egui::ViewportCommand::Visible(false));
        ctx.request_repaint_after(Duration::from_millis(100));
    }

    pub(super) fn maybe_run_startup_auto_start(&mut self, ctx: &egui::Context) {
        if !self.startup_auto_start_pending {
            return;
        }
        if !self.config.start_recording_on_launch {
            self.startup_auto_start_pending = false;
            return;
        }
        if !self.config_is_persisted() || !self.can_start_replay() {
            ctx.request_repaint_after(Duration::from_millis(100));
            return;
        }
        self.startup_auto_start_pending = false;
        self.loop_log
            .push("随启动开始：开始按钮可用，自动开始录制。".to_owned());
        let _ = self.click_start();
    }
}
