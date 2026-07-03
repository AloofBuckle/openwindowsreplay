//! egui 中文界面。

use crate::backend::ProbeCaps;
use crate::backend::session::{ReplayController, ReplayState};
use crate::config::AppConfig;
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
}

impl RustReplayApp {
    pub fn new(cc: &eframe::CreationContext<'_>) -> Self {
        let font_status = install_chinese_font(&cc.egui_ctx);
        let mut this = Self {
            config: AppConfig::default(),
            caps: None,
            probe_rx: None,
            controller: ReplayController::default(),
            encoder_log: vec![font_status, "正在启动能力探测线程……".to_owned()],
            loop_log: vec!["循环器尚未启动。".to_owned()],
            last_status_refresh: Instant::now(),
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
        if !caps.supported_rate_controls.is_empty()
            && !caps
                .supported_rate_controls
                .contains(&self.config.rate_control.method)
        {
            self.config.rate_control.method = caps.supported_rate_controls[0];
        }
        self.caps = Some(caps);
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
            Ok(()) => self.loop_log.push("即时回放已停止。".to_owned()),
            Err(err) => self.loop_log.push(format!("停止失败：{err}")),
        }
    }

    fn status_text(&self) -> String {
        match self.controller.state() {
            ReplayState::Idle => "状态：空闲".to_owned(),
            ReplayState::Running { started_at } => format!(
                "状态：录制中，已运行 {:.1}s",
                started_at.elapsed().as_secs_f32()
            ),
        }
    }
}

impl eframe::App for RustReplayApp {
    fn logic(&mut self, ctx: &egui::Context, _frame: &mut eframe::Frame) {
        self.receive_probe();
        if self.last_status_refresh.elapsed() > Duration::from_millis(500) {
            ctx.request_repaint_after(Duration::from_millis(500));
            self.last_status_refresh = Instant::now();
        }
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
            let supported = self
                .caps
                .as_ref()
                .map(|c| c.supported_rate_controls.clone())
                .unwrap_or_default();
            if supported.is_empty() {
                ui.small(
                    "当前没有经 oneVPL Query 确认的码控模式，已隐藏码控字段；详情见编码器日志。",
                );
            } else {
                egui::ComboBox::from_label("RateControlMethod")
                    .selected_text(self.config.rate_control.method.label())
                    .show_ui(ui, |ui| {
                        for method in supported {
                            ui.selectable_value(
                                &mut self.config.rate_control.method,
                                method,
                                method.label(),
                            );
                        }
                    });
                ui.separator();
                rate_control_fields(ui, &mut self.config.rate_control);
            }
        });
    }

    fn right_loop_panel(&mut self, ui: &mut egui::Ui) {
        ui.heading("录制循环器参数");
        ui.group(|ui| {
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
            ui.label("后端硬约束");
            ui.small("Capture 必须输出同一 DXGI adapter 上的 ID3D11Texture2D。 ");
            ui.small("ColorTransform / ChromaWriter 只允许 Compute Shader / Video Processor / GPU copy。 ");
            ui.small("oneVPL 输入只允许 shared D3D11 surface import；失败则 UnsupportedGpuPath。 ");
            ui.small("禁止 Map / Readback / Staging texture / CPU memcpy raw frame。 ");
        });
    }
}

fn rate_control_fields(ui: &mut egui::Ui, cfg: &mut RateControlConfig) {
    ui.horizontal(|ui| {
        ui.label("BRCParamMultiplier");
        ui.add(egui::DragValue::new(&mut cfg.brc_param_multiplier).range(1..=65535));
    });

    match cfg.method {
        RateControlMethod::Cbr => {
            kbps_fields(ui, cfg, false);
            sliding_window_fields(ui, cfg);
        }
        RateControlMethod::Vbr => {
            kbps_fields(ui, cfg, true);
            sliding_window_fields(ui, cfg);
            low_delay_and_frame_size(ui, cfg);
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
            lookahead_field(ui, cfg);
            sliding_window_fields(ui, cfg);
        }
        RateControlMethod::Icq => {
            icq_field(ui, cfg);
        }
        RateControlMethod::Vcm => {
            kbps_fields(ui, cfg, true);
            ui.checkbox(&mut cfg.low_delay_brc, "LowDelayBRC");
        }
        RateControlMethod::LaIcq => {
            icq_field(ui, cfg);
            lookahead_field(ui, cfg);
        }
        RateControlMethod::LaHrd => {
            kbps_fields(ui, cfg, true);
            lookahead_field(ui, cfg);
            sliding_window_fields(ui, cfg);
        }
        RateControlMethod::Qvbr => {
            kbps_fields(ui, cfg, true);
            ui.horizontal(|ui| {
                ui.label("QVBRQuality (1 最好，51 最差)");
                ui.add(egui::DragValue::new(&mut cfg.qvbr_quality).range(1..=51));
            });
            sliding_window_fields(ui, cfg);
            ui.checkbox(&mut cfg.low_delay_brc, "LowDelayBRC");
        }
    }

    ui.separator();
    ui.checkbox(&mut cfg.mbbrc, "MBBRC（宏块级码率控制，可提升主观质量）");
    ui.checkbox(
        &mut cfg.ext_brc,
        "ExtBRC（外部 BRC，需要配合回调，当前仅记录配置）",
    );

    let fields = cfg.to_vpl_fields();
    ui.collapsing("将写入 oneVPL 的字段预览", |ui| {
        ui.monospace(
            serde_json::to_string_pretty(&fields).unwrap_or_else(|_| "<序列化失败>".to_owned()),
        );
    });
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

fn low_delay_and_frame_size(ui: &mut egui::Ui, cfg: &mut RateControlConfig) {
    ui.checkbox(&mut cfg.low_delay_brc, "LowDelayBRC");
    ui.horizontal(|ui| {
        ui.label("MaxFrameSize (bytes, 0=关闭)");
        ui.add(
            egui::DragValue::new(&mut cfg.max_frame_size)
                .speed(1024)
                .range(0..=100_000_000),
        );
    });
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
