//! egui 中文界面。

use crate::backend::session::{ReplayController, ReplaySaveReadiness, ReplayState};
use crate::backend::{ProbeCaps, RateControlFeatureSupport};
use crate::config::{AppConfig, CaptureBackend, HotkeyConfig, HotkeyKey};
use crate::hotkey::{HotkeyEvent, HotkeyRuntime};
use crate::indicator_overlay::{IndicatorOverlayRuntime, NativeIndicatorImage};
use crate::rate_control::{RateControlConfig, RateControlMethod};
use crate::tray::{TrayEvent, TrayRuntime};
use eframe::egui;
use std::path::Path;
use std::sync::Arc;
use std::sync::mpsc::{self, Receiver};
use std::time::{Duration, Instant};

const INDICATOR_FLASH_DURATION: Duration = Duration::from_secs(2);
const MAX_VISIBLE_LOG_LINES: usize = 400;
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum IndicatorColor {
    Blue,
    Green,
    Red,
    Yellow,
    Purple,
}

impl IndicatorColor {
    const ALL: [Self; 5] = [
        Self::Blue,
        Self::Green,
        Self::Red,
        Self::Yellow,
        Self::Purple,
    ];

    const fn label(self) -> &'static str {
        match self {
            Self::Blue => "blue",
            Self::Green => "green",
            Self::Red => "red",
            Self::Yellow => "yellow",
            Self::Purple => "purple",
        }
    }

    const fn color32(self) -> egui::Color32 {
        match self {
            Self::Blue => egui::Color32::from_rgb(0, 122, 255),
            Self::Green => egui::Color32::from_rgb(0, 200, 90),
            Self::Red => egui::Color32::from_rgb(255, 55, 55),
            Self::Yellow => egui::Color32::from_rgb(255, 210, 0),
            Self::Purple => egui::Color32::from_rgb(190, 0, 255),
        }
    }

    const fn index(self) -> usize {
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
struct IndicatorImages {
    diameter_px: u32,
    images: Vec<egui::ColorImage>,
}

impl IndicatorImages {
    fn image(&self, color: IndicatorColor) -> egui::ColorImage {
        self.images.get(color.index()).cloned().unwrap_or_else(|| {
            build_circle_indicator_images(self.diameter_px.max(1))[color.index()].clone()
        })
    }
}

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
    tray: TrayRuntime,
    indicator_overlay: IndicatorOverlayRuntime,
    indicator_images: IndicatorImages,
    indicator_flash: Option<(IndicatorColor, Instant)>,
    last_indicator_color: Option<IndicatorColor>,
    configuring_indicator_position: bool,
    waiting_save_hotkey: bool,
    startup_auto_start_pending: bool,
    startup_hidden_to_tray: bool,
    allow_exit: bool,
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
        let (indicator_images, indicator_status) = build_indicator_images_for_config(&config);
        let native_indicator_images = native_images_from_indicator_images(&indicator_images);
        let indicator_x = config.indicator.position_x.round() as i32;
        let indicator_y = config.indicator.position_y.round() as i32;
        let indicator_text_enabled = config.indicator.text_enabled;
        let startup_auto_start_pending = config.start_recording_on_launch;
        let startup_hidden_to_tray = config.start_minimized_to_tray;
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
        };
        if let Some(status) = indicator_status {
            this.loop_log.push(status);
        }
        this.start_probe();
        this
    }

    fn start_probe(&mut self) {
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

        sanitize_config_against_caps(&mut self.config, &caps);
        self.caps = Some(caps);
        if self.startup_auto_start_pending && self.config.start_recording_on_launch {
            self.loop_log
                .push("随启动开始已启用，等待开始按钮可用后自动开始录制。".to_owned());
        } else {
            self.startup_auto_start_pending = false;
        }
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

    fn click_start(&mut self) -> bool {
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

    fn click_save(&mut self) {
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

    fn click_stop(&mut self) {
        if self.is_initializing() {
            self.loop_log
                .push("等待初始化完成，暂不能停止回放。".to_owned());
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

    fn begin_save_hotkey_binding(&mut self) {
        self.waiting_save_hotkey = true;
        self.hotkey.set_hotkey(None);
    }

    fn cancel_save_hotkey_binding(&mut self) {
        self.waiting_save_hotkey = false;
        self.hotkey.set_hotkey(Some(self.config.save_hotkey));
    }

    fn apply_save_hotkey_binding(&mut self, hotkey: HotkeyConfig) {
        self.config.save_hotkey = hotkey;
        self.waiting_save_hotkey = false;
        self.loop_log
            .push(format!("保存即时回放热键已改为：{}", hotkey.label()));
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

    fn handle_tray_events(&mut self, ctx: &egui::Context) {
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

    fn handle_close_request(&mut self, ctx: &egui::Context) {
        if ctx.input(|input| input.viewport().close_requested()) && !self.allow_exit {
            ctx.send_viewport_cmd(egui::ViewportCommand::CancelClose);
            self.minimize_to_taskbar(ctx);
        }
    }

    fn minimize_to_taskbar(&mut self, ctx: &egui::Context) {
        self.loop_log
            .push("窗口已收起到任务栏，后台录制状态不变。".to_owned());
        ctx.send_viewport_cmd(egui::ViewportCommand::Minimized(true));
    }

    fn open_main_window(&mut self, ctx: &egui::Context) {
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

    fn reset_all_config(&mut self) {
        match AppConfig::clear_global_entry() {
            Ok(()) => self.encoder_log.push(format!(
                "已清空全局配置：{}",
                AppConfig::config_path().display()
            )),
            Err(err) => self.encoder_log.push(format!("重置配置失败：{err}")),
        }

        self.config = AppConfig::default();
        if let Some(caps) = &self.caps {
            sanitize_config_against_caps(&mut self.config, caps);
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

    fn update_indicator_runtime(&mut self, ctx: &egui::Context) {
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

    fn replay_indicator_visible(&self) -> bool {
        !matches!(self.controller.state(), ReplayState::Idle)
    }

    fn current_indicator_color(&self) -> Option<IndicatorColor> {
        match self.controller.state() {
            ReplayState::Idle => None,
            ReplayState::Running { .. } => self
                .indicator_flash
                .map(|(color, _)| color)
                .or(Some(IndicatorColor::Blue)),
            ReplayState::Stopping { .. } => Some(IndicatorColor::Yellow),
        }
    }

    fn rebuild_indicator_images(&mut self) {
        let (images, status) = build_indicator_images_for_config(&self.config);
        self.indicator_images = images;
        self.configure_indicator_overlay();
        if let Some(status) = status {
            self.loop_log.push(status);
        }
    }

    fn configure_indicator_overlay(&self) {
        self.indicator_overlay.configure(
            self.config.indicator.position_x.round() as i32,
            self.config.indicator.position_y.round() as i32,
            native_images_from_indicator_images(&self.indicator_images),
            self.config.indicator.text_enabled,
        );
    }

    fn sync_indicator_overlay(&mut self) {
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

    fn import_indicator_image(&mut self) {
        let Some(path) = pick_image_file() else {
            return;
        };
        self.config.indicator.image_path = Some(path);
        self.rebuild_indicator_images();
    }

    fn status_text(&self) -> String {
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

    fn can_start_replay(&self) -> bool {
        self.probe_rx.is_none()
            && self.caps.is_some()
            && matches!(self.controller.state(), ReplayState::Idle)
    }

    fn can_save_replay(&self) -> bool {
        !self.is_initializing() && self.controller.save_readiness().can_save()
    }

    fn config_is_persisted(&self) -> bool {
        self.config.stable_json() == self.last_saved_config_json
    }

    fn is_initializing(&self) -> bool {
        self.probe_rx.is_some()
    }

    fn maintain_startup_tray_hidden(&mut self, ctx: &egui::Context) {
        if !self.startup_hidden_to_tray {
            return;
        }
        prepare_root_window_for_tray_start();
        ctx.send_viewport_cmd(egui::ViewportCommand::Visible(false));
        ctx.request_repaint_after(Duration::from_millis(100));
    }

    fn maybe_run_startup_auto_start(&mut self, ctx: &egui::Context) {
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

impl eframe::App for RustReplayApp {
    fn clear_color(&self, _visuals: &egui::Visuals) -> [f32; 4] {
        [0.0, 0.0, 0.0, 0.0]
    }

    fn logic(&mut self, ctx: &egui::Context, _frame: &mut eframe::Frame) {
        self.maintain_startup_tray_hidden(ctx);
        self.handle_close_request(ctx);
        self.receive_probe();
        for line in self.controller.drain_log_messages() {
            self.loop_log.push(line);
        }
        self.handle_save_hotkey_binding_input(ctx);
        self.handle_hotkey_events();
        self.handle_tray_events(ctx);
        self.update_indicator_runtime(ctx);
        if self.last_status_refresh.elapsed() > Duration::from_millis(500) {
            ctx.request_repaint_after(Duration::from_millis(500));
            self.last_status_refresh = Instant::now();
        }
        self.persist_config_if_changed();
        self.maybe_run_startup_auto_start(ctx);
    }

    fn ui(&mut self, ui: &mut egui::Ui, _frame: &mut eframe::Frame) {
        egui::Panel::top("top_controls")
            .exact_size(36.0)
            .frame(egui::Frame::NONE.fill(ui.visuals().panel_fill))
            .show(ui, |ui| {
                self.top_bar(ui);
            });

        egui::Panel::bottom("bottom_actions")
            .exact_size(34.0)
            .show(ui, |ui| {
                ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                    if ui.button("重置所有配置").clicked() {
                        self.reset_all_config();
                    }
                });
            });

        egui::CentralPanel::default().show(ui, |ui| {
            self.main_panel(ui);
        });
        self.show_indicator_viewports(ui.ctx());
    }
}

impl RustReplayApp {
    fn top_bar(&mut self, ui: &mut egui::Ui) {
        ui.add_space(2.0);
        ui.horizontal(|ui| {
            let initializing = self.is_initializing();
            let start_label = if initializing {
                "等待初始化"
            } else {
                "开始即时回放"
            };
            let save_label = if initializing {
                "等待初始化"
            } else {
                "保存即时回放"
            };
            let stop_label = if initializing {
                "等待初始化"
            } else {
                "停止即时回放"
            };
            let probe_label = if initializing {
                "等待初始化"
            } else {
                "重新探测能力"
            };
            if ui
                .add_enabled(
                    !initializing && self.can_start_replay(),
                    egui::Button::new(start_label),
                )
                .on_disabled_hover_text("等待初始化")
                .clicked()
            {
                let _ = self.click_start();
            }
            if ui
                .add_enabled(self.can_save_replay(), egui::Button::new(save_label))
                .on_disabled_hover_text("等待后台录制产生可保存的 HEVC/AAC")
                .clicked()
            {
                self.click_save();
            }
            if ui
                .add_enabled(!initializing, egui::Button::new(stop_label))
                .on_disabled_hover_text("等待初始化")
                .clicked()
            {
                self.click_stop();
            }
            let can_probe = !initializing && matches!(self.controller.state(), ReplayState::Idle);
            if ui
                .add_enabled(can_probe, egui::Button::new(probe_label))
                .on_disabled_hover_text("等待初始化")
                .clicked()
            {
                self.start_probe();
            }
            ui.checkbox(&mut self.config.start_recording_on_launch, "随启动开始");
            ui.checkbox(&mut self.config.start_minimized_to_tray, "启动自动折叠");
            ui.separator();
            ui.label(self.status_text());

            let controls_width = 110.0;
            let drag_width = (ui.available_width() - controls_width).max(8.0);
            let (drag_rect, drag_response) =
                ui.allocate_exact_size(egui::vec2(drag_width, 28.0), egui::Sense::click_and_drag());
            if drag_response.drag_started() {
                ui.ctx().send_viewport_cmd(egui::ViewportCommand::StartDrag);
            }
            if drag_response.double_clicked() {
                self.toggle_maximized(ui.ctx());
            }
            let stroke = ui.visuals().widgets.noninteractive.bg_stroke;
            ui.painter().line_segment(
                [drag_rect.left_center(), drag_rect.right_center()],
                egui::Stroke::new(1.0, stroke.color.linear_multiply(0.35)),
            );

            self.window_buttons(ui);
        });
    }

    fn window_buttons(&mut self, ui: &mut egui::Ui) {
        if ui.button("—").on_hover_text("最小化").clicked() {
            ui.ctx()
                .send_viewport_cmd(egui::ViewportCommand::Minimized(true));
        }
        let maximized = ui
            .ctx()
            .input(|input| input.viewport().maximized.unwrap_or(false));
        let max_label = if maximized { "❐" } else { "□" };
        if ui.button(max_label).on_hover_text("最大化/还原").clicked() {
            self.toggle_maximized(ui.ctx());
        }
        if ui.button("×").on_hover_text("收起到任务栏").clicked() {
            self.minimize_to_taskbar(ui.ctx());
        }
    }

    fn toggle_maximized(&mut self, ctx: &egui::Context) {
        let maximized = ctx.input(|input| input.viewport().maximized.unwrap_or(false));
        ctx.send_viewport_cmd(egui::ViewportCommand::Maximized(!maximized));
    }

    fn indicator_size_points(&self, ctx: &egui::Context) -> f32 {
        let pixels_per_point =
            ctx.input(|input| input.viewport().native_pixels_per_point.unwrap_or(1.0));
        (self.config.indicator.diameter_px.max(1) as f32 / pixels_per_point.max(0.1)).max(1.0)
    }

    fn show_indicator_viewports(&mut self, ctx: &egui::Context) {
        if self.configuring_indicator_position {
            let size = self.indicator_size_points(ctx);
            let image = self.indicator_images.image(IndicatorColor::Purple);
            let mut picked_top_left = None;
            let mut cancelled = false;
            ctx.show_viewport_immediate(
                egui::ViewportId::from_hash_of("rustreplay_indicator_position_overlay"),
                egui::ViewportBuilder::default()
                    .with_title("配置指示器位置")
                    .with_decorations(false)
                    .with_fullscreen(true)
                    .with_always_on_top()
                    .with_resizable(false),
                |ui, _class| {
                    ui.ctx()
                        .send_viewport_cmd(egui::ViewportCommand::CursorVisible(false));
                    let rect = ui.max_rect();
                    ui.painter().rect_filled(rect, 0.0, egui::Color32::BLACK);
                    let pointer = ui
                        .ctx()
                        .input(|input| input.pointer.hover_pos())
                        .unwrap_or_else(|| rect.center());
                    draw_indicator_image(ui, &image, size, Some(pointer));
                    if ui.ctx().input(|input| input.pointer.primary_clicked()) {
                        let diameter = self.config.indicator.diameter_px.max(1) as f32;
                        picked_top_left = current_cursor_physical_pos()
                            .or_else(|| {
                                let viewport_origin = ui
                                    .ctx()
                                    .input(|input| input.viewport().outer_rect.map(|rect| rect.min))
                                    .unwrap_or(egui::Pos2::ZERO);
                                let pixels_per_point = ui.ctx().input(|input| {
                                    input.viewport().native_pixels_per_point.unwrap_or(1.0)
                                });
                                Some(egui::pos2(
                                    (viewport_origin.x + pointer.x) * pixels_per_point,
                                    (viewport_origin.y + pointer.y) * pixels_per_point,
                                ))
                            })
                            .map(|pos| pos - egui::vec2(diameter / 2.0, diameter / 2.0));
                        ui.ctx().send_viewport_cmd(egui::ViewportCommand::Close);
                    }
                    if ui.ctx().input(|input| input.key_pressed(egui::Key::Escape)) {
                        cancelled = true;
                        ui.ctx().send_viewport_cmd(egui::ViewportCommand::Close);
                    }
                },
            );
            if let Some(pos) = picked_top_left {
                self.config.indicator.position_x = pos.x;
                self.config.indicator.position_y = pos.y;
                self.configuring_indicator_position = false;
                self.configure_indicator_overlay();
                self.loop_log.push(format!(
                    "状态指示器位置已设置为 ({:.0}, {:.0})。",
                    pos.x, pos.y
                ));
            } else if cancelled {
                self.configuring_indicator_position = false;
                self.loop_log.push("已取消状态指示器位置配置。".to_owned());
            }
        }
    }

    fn main_panel(&mut self, ui: &mut egui::Ui) {
        let available_height = ui.available_height();
        let log_total_height = (available_height * 0.42).clamp(260.0, 360.0);
        let params_height = (available_height - log_total_height - 10.0).max(180.0);

        ui.allocate_ui(egui::vec2(ui.available_width(), params_height), |ui| {
            ui.columns(2, |cols| {
                egui::ScrollArea::vertical()
                    .id_salt("encoder_params_scroll")
                    .max_height(params_height)
                    .show(&mut cols[0], |ui| {
                        self.left_encoder_panel(ui);
                    });
                egui::ScrollArea::vertical()
                    .id_salt("loop_params_scroll")
                    .max_height(params_height)
                    .show(&mut cols[1], |ui| {
                        self.right_loop_panel(ui);
                    });
            });
        });

        ui.separator();
        let log_height = ui.available_height().max(200.0);
        ui.columns(2, |cols| {
            log_panel(
                &mut cols[0],
                "encoder_log_scroll",
                "编码器日志",
                &self.encoder_log,
                log_height,
            );
            log_panel(
                &mut cols[1],
                "loop_log_scroll",
                "循环器日志",
                &self.loop_log,
                log_height,
            );
        });
    }

    fn left_encoder_panel(&mut self, ui: &mut egui::Ui) {
        ui.heading("编码器参数");
        ui.group(|ui| {
            ui.label("色度采样");
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
            ui.label("码率控制模式");
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
        });

        ui.add_space(8.0);
        ui.group(|ui| {
            ui.label("配置指示器");
            ui.horizontal_wrapped(|ui| {
                ui.label(format!(
                    "位置：{:.0}, {:.0}",
                    self.config.indicator.position_x, self.config.indicator.position_y
                ));
                if ui.button("更改").clicked() {
                    self.configuring_indicator_position = true;
                    self.loop_log.push(
                        "进入状态指示器位置配置：移动紫色图案，左键确认，Esc 取消。".to_owned(),
                    );
                }
            });
            ui.horizontal_wrapped(|ui| {
                ui.label("直径");
                let before = self.config.indicator.diameter_px;
                ui.add(
                    egui::DragValue::new(&mut self.config.indicator.diameter_px)
                        .speed(1)
                        .range(4..=512)
                        .suffix(" px"),
                );
                if self.config.indicator.diameter_px != before {
                    self.rebuild_indicator_images();
                }
                if ui.button("导入图像").clicked() {
                    self.import_indicator_image();
                }
                if self.config.indicator.image_path.is_some() && ui.button("恢复圆形").clicked()
                {
                    self.config.indicator.image_path = None;
                    self.rebuild_indicator_images();
                }
            });
            ui.horizontal_wrapped(|ui| {
                let before = self.config.indicator.text_enabled;
                ui.checkbox(&mut self.config.indicator.text_enabled, "文本指示");
                if self.config.indicator.text_enabled != before {
                    self.configure_indicator_overlay();
                }
            });
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

#[cfg(windows)]
fn current_cursor_physical_pos() -> Option<egui::Pos2> {
    use windows::Win32::Foundation::POINT;
    use windows::Win32::UI::WindowsAndMessaging::GetCursorPos;

    let mut point = POINT::default();
    unsafe { GetCursorPos(&mut point) }
        .ok()
        .map(|_| egui::pos2(point.x as f32, point.y as f32))
}

#[cfg(not(windows))]
fn current_cursor_physical_pos() -> Option<egui::Pos2> {
    None
}

#[cfg(windows)]
fn prepare_root_window_for_tray_start() {
    use windows::Win32::UI::WindowsAndMessaging::{
        GWL_EXSTYLE, GetWindowLongPtrW, SW_HIDE, SWP_HIDEWINDOW, SWP_NOACTIVATE, SWP_NOSIZE,
        SWP_NOZORDER, SetWindowLongPtrW, SetWindowPos, ShowWindow, WS_EX_APPWINDOW,
        WS_EX_TOOLWINDOW,
    };

    let Some(hwnd) = find_root_window() else {
        return;
    };
    unsafe {
        let style = GetWindowLongPtrW(hwnd, GWL_EXSTYLE) as u32;
        let style = (style & !WS_EX_APPWINDOW.0) | WS_EX_TOOLWINDOW.0;
        let _ = SetWindowLongPtrW(hwnd, GWL_EXSTYLE, style as isize);
        let _ = SetWindowPos(
            hwnd,
            None,
            -32_000,
            -32_000,
            0,
            0,
            SWP_NOSIZE | SWP_NOZORDER | SWP_NOACTIVATE | SWP_HIDEWINDOW,
        );
        let _ = ShowWindow(hwnd, SW_HIDE);
    }
}

#[cfg(not(windows))]
fn prepare_root_window_for_tray_start() {}

#[cfg(windows)]
fn restore_root_window_from_tray_start() {
    use windows::Win32::UI::WindowsAndMessaging::{
        GWL_EXSTYLE, GetWindowLongPtrW, SW_SHOWNORMAL, SWP_NOACTIVATE, SWP_NOSIZE, SWP_NOZORDER,
        SetWindowLongPtrW, SetWindowPos, ShowWindow, WS_EX_APPWINDOW, WS_EX_TOOLWINDOW,
    };

    let Some(hwnd) = find_root_window() else {
        return;
    };
    unsafe {
        let style = GetWindowLongPtrW(hwnd, GWL_EXSTYLE) as u32;
        let style = (style & !WS_EX_TOOLWINDOW.0) | WS_EX_APPWINDOW.0;
        let _ = SetWindowLongPtrW(hwnd, GWL_EXSTYLE, style as isize);
        let _ = SetWindowPos(
            hwnd,
            None,
            80,
            80,
            0,
            0,
            SWP_NOSIZE | SWP_NOZORDER | SWP_NOACTIVATE,
        );
        let _ = ShowWindow(hwnd, SW_SHOWNORMAL);
    }
}

#[cfg(not(windows))]
fn restore_root_window_from_tray_start() {}

#[cfg(windows)]
fn find_root_window() -> Option<windows::Win32::Foundation::HWND> {
    use windows::Win32::UI::WindowsAndMessaging::FindWindowW;

    let title: Vec<u16> = "RustReplay 即时回放"
        .encode_utf16()
        .chain(std::iter::once(0))
        .collect();
    let hwnd = unsafe {
        FindWindowW(
            windows::core::PCWSTR::null(),
            windows::core::PCWSTR(title.as_ptr()),
        )
    }
    .ok()?;
    (!hwnd.0.is_null()).then_some(hwnd)
}

fn draw_indicator_image(
    ui: &mut egui::Ui,
    image: &egui::ColorImage,
    size: f32,
    center: Option<egui::Pos2>,
) {
    let rect = center
        .map(|center| egui::Rect::from_center_size(center, egui::vec2(size, size)))
        .unwrap_or_else(|| egui::Rect::from_min_size(ui.max_rect().min, egui::vec2(size, size)));
    let texture = ui.ctx().load_texture(
        format!(
            "rustreplay_indicator_{}x{}_{}",
            image.size[0],
            image.size[1],
            ui.ctx().cumulative_frame_nr()
        ),
        image.clone(),
        egui::TextureOptions::LINEAR,
    );
    ui.painter().image(
        texture.id(),
        rect,
        egui::Rect::from_min_max(egui::Pos2::ZERO, egui::pos2(1.0, 1.0)),
        egui::Color32::WHITE,
    );
}

fn native_images_from_indicator_images(images: &IndicatorImages) -> Vec<NativeIndicatorImage> {
    images
        .images
        .iter()
        .map(native_image_from_color_image)
        .collect()
}

fn native_image_from_color_image(image: &egui::ColorImage) -> NativeIndicatorImage {
    let mut premul_bgra = Vec::with_capacity(image.pixels.len() * 4);
    for pixel in &image.pixels {
        let [red, green, blue, alpha] = pixel.to_srgba_unmultiplied();
        let alpha_u16 = alpha as u16;
        let premul = |channel: u8| ((channel as u16 * alpha_u16 + 127) / 255) as u8;
        premul_bgra.push(premul(blue));
        premul_bgra.push(premul(green));
        premul_bgra.push(premul(red));
        premul_bgra.push(alpha);
    }
    NativeIndicatorImage {
        width: image.size[0] as i32,
        height: image.size[1] as i32,
        premul_bgra,
    }
}

fn build_indicator_images_for_config(config: &AppConfig) -> (IndicatorImages, Option<String>) {
    let diameter_px = config.indicator.diameter_px.max(1);
    let mut status = None;
    let images = if let Some(path) = &config.indicator.image_path {
        match build_custom_indicator_images(Path::new(path), diameter_px) {
            Ok(images) => {
                status = Some(format!("状态指示器图像已导入并生成五色图：{path}"));
                images
            }
            Err(err) => {
                status = Some(format!("状态指示器图像处理失败，已回退默认圆点：{err}"));
                build_circle_indicator_images(diameter_px)
            }
        }
    } else {
        build_circle_indicator_images(diameter_px)
    };

    if let Err(err) = save_indicator_images(&images) {
        status = Some(format!("状态指示器五色图写入失败：{err}"));
    }

    (
        IndicatorImages {
            diameter_px,
            images,
        },
        status,
    )
}

fn build_circle_indicator_images(diameter_px: u32) -> Vec<egui::ColorImage> {
    let size = diameter_px.max(1) as usize;
    let radius = size as f32 / 2.0;
    let center = (size as f32 - 1.0) / 2.0;
    IndicatorColor::ALL
        .iter()
        .map(|&indicator_color| {
            let color = indicator_color.color32();
            let mut pixels = Vec::with_capacity(size * size);
            for y in 0..size {
                for x in 0..size {
                    let dx = x as f32 - center;
                    let dy = y as f32 - center;
                    if (dx * dx + dy * dy).sqrt() <= radius {
                        pixels.push(color);
                    } else {
                        pixels.push(egui::Color32::TRANSPARENT);
                    }
                }
            }
            egui::ColorImage::new([size, size], pixels)
        })
        .collect()
}

fn build_custom_indicator_images(
    path: &Path,
    diameter_px: u32,
) -> Result<Vec<egui::ColorImage>, String> {
    let decoded =
        image::open(path).map_err(|err| format!("读取图像 {} 失败：{err}", path.display()))?;
    let rgba = decoded.to_rgba8();
    let (width, height) = rgba.dimensions();
    if width == 0 || height == 0 {
        return Err(format!("图像 {} 尺寸为空", path.display()));
    }
    let side = width.min(height);
    let crop_x = (width - side) / 2;
    let crop_y = (height - side) / 2;
    let cropped = image::imageops::crop_imm(&rgba, crop_x, crop_y, side, side).to_image();
    let size = diameter_px.max(1);
    let resized =
        image::imageops::resize(&cropped, size, size, image::imageops::FilterType::Lanczos3);
    Ok(IndicatorColor::ALL
        .iter()
        .map(|&indicator_color| {
            let [red, green, blue, _] = indicator_color.color32().to_srgba_unmultiplied();
            let mut pixels = Vec::with_capacity((size * size) as usize);
            for pixel in resized.pixels() {
                let alpha = pixel[3];
                if alpha == 0 {
                    pixels.push(egui::Color32::TRANSPARENT);
                } else {
                    pixels.push(egui::Color32::from_rgba_unmultiplied(
                        red, green, blue, alpha,
                    ));
                }
            }
            egui::ColorImage::new([size as usize, size as usize], pixels)
        })
        .collect())
}

fn save_indicator_images(images: &[egui::ColorImage]) -> Result<(), String> {
    let dir = AppConfig::indicator_dir();
    std::fs::create_dir_all(&dir)
        .map_err(|err| format!("创建目录 {} 失败：{err}", dir.display()))?;
    for &color in &IndicatorColor::ALL {
        let Some(image) = images.get(color.index()) else {
            continue;
        };
        let mut bytes = Vec::with_capacity(image.pixels.len() * 4);
        for pixel in &image.pixels {
            bytes.extend_from_slice(&pixel.to_srgba_unmultiplied());
        }
        let rgba = image::RgbaImage::from_raw(image.size[0] as u32, image.size[1] as u32, bytes)
            .ok_or_else(|| "构造 RGBA 图像失败".to_owned())?;
        let path = dir.join(format!("indicator_{}.png", color.label()));
        rgba.save(&path)
            .map_err(|err| format!("写入 {} 失败：{err}", path.display()))?;
    }
    Ok(())
}

fn sanitize_config_against_caps(config: &mut AppConfig, caps: &ProbeCaps) {
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

#[cfg(windows)]
fn pick_image_file() -> Option<String> {
    use windows::Win32::System::Com::{
        CLSCTX_INPROC_SERVER, COINIT_APARTMENTTHREADED, CoCreateInstance, CoInitializeEx,
        CoTaskMemFree,
    };
    use windows::Win32::UI::Shell::{
        FOS_FILEMUSTEXIST, FOS_FORCEFILESYSTEM, FileOpenDialog, IFileOpenDialog, SIGDN_FILESYSPATH,
    };
    unsafe {
        let _ = CoInitializeEx(None, COINIT_APARTMENTTHREADED);
        let dialog: IFileOpenDialog =
            CoCreateInstance(&FileOpenDialog, None, CLSCTX_INPROC_SERVER).ok()?;
        let mut options = dialog.GetOptions().ok()?;
        options |= FOS_FORCEFILESYSTEM | FOS_FILEMUSTEXIST;
        dialog.SetOptions(options).ok()?;
        dialog.SetTitle(windows::core::w!("选择指示器图像")).ok()?;
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

#[cfg(not(windows))]
fn pick_image_file() -> Option<String> {
    None
}

fn log_panel(ui: &mut egui::Ui, id_salt: &'static str, title: &str, lines: &[String], height: f32) {
    ui.push_id(id_salt, |ui| {
        ui.allocate_ui_with_layout(
            egui::vec2(ui.available_width(), height),
            egui::Layout::top_down(egui::Align::Min),
            |ui| {
                ui.set_height(height);
                ui.heading(title);
                let body_height = ui.available_height().max(120.0);
                let body_width = ui.available_width();
                egui::Frame::group(ui.style()).show(ui, |ui| {
                    ui.set_min_size(egui::vec2(body_width, body_height));
                    ui.set_max_size(egui::vec2(body_width, body_height));
                    let text = visible_log_text(lines);
                    let mut text_view = text.as_str();
                    let scroll_output = egui::ScrollArea::vertical()
                        .id_salt((id_salt, "scroll"))
                        .stick_to_bottom(true)
                        .min_scrolled_height(body_height)
                        .max_height(body_height)
                        .auto_shrink([false, false])
                        .show(ui, |ui| {
                            ui.set_width(ui.available_width());
                            ui.add(
                                egui::TextEdit::multiline(&mut text_view)
                                    .id_salt((id_salt, "text"))
                                    .font(egui::TextStyle::Monospace)
                                    .desired_width(f32::INFINITY)
                                    .desired_rows(1)
                                    .min_size(egui::vec2(ui.available_width(), body_height))
                                    .cursor_at_end(true),
                            )
                        });
                    if let Some(offset_y) = log_drag_selection_scroll_offset(
                        ui,
                        &scroll_output.inner,
                        scroll_output.inner_rect,
                        scroll_output.content_size,
                        scroll_output.state.offset.y,
                    ) {
                        let mut state = scroll_output.state;
                        state.offset.y = offset_y;
                        state.store(ui.ctx(), scroll_output.id);
                        ui.ctx().request_repaint_after(Duration::from_millis(16));
                    }
                });
            },
        );
    });
}

fn log_drag_selection_scroll_offset(
    ui: &egui::Ui,
    response: &egui::Response,
    inner_rect: egui::Rect,
    content_size: egui::Vec2,
    current_offset_y: f32,
) -> Option<f32> {
    if !response.dragged() {
        return None;
    }
    let pointer_pos = ui.ctx().input(|input| input.pointer.interact_pos())?;

    let delta_y = if pointer_pos.y < inner_rect.top() {
        -((inner_rect.top() - pointer_pos.y) * 0.75).clamp(2.0, 42.0)
    } else if pointer_pos.y > inner_rect.bottom() {
        ((pointer_pos.y - inner_rect.bottom()) * 0.75).clamp(2.0, 42.0)
    } else {
        return None;
    };

    let max_offset_y = (content_size.y - inner_rect.height()).max(0.0);
    if max_offset_y <= 0.0 {
        return None;
    }

    let new_offset_y = (current_offset_y + delta_y).clamp(0.0, max_offset_y);
    if (new_offset_y - current_offset_y).abs() <= f32::EPSILON {
        return None;
    }
    Some(new_offset_y)
}

fn visible_log_text(lines: &[String]) -> String {
    let start = lines.len().saturating_sub(MAX_VISIBLE_LOG_LINES);
    lines[start..].join("\n")
}
