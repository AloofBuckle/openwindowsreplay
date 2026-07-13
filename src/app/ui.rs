use super::*;

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
        self.handle_single_instance_activation(ctx);
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
                    if ui
                        .add_enabled(!self.config_read_only(), egui::Button::new("重置所有配置"))
                        .on_disabled_hover_text("录制运行中，配置只读")
                        .clicked()
                    {
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
    pub(super) fn top_bar(&mut self, ui: &mut egui::Ui) {
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
                .add_enabled(self.can_stop_replay(), egui::Button::new(stop_label))
                .on_disabled_hover_text("当前没有正在运行的后台录制")
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
            let config_enabled = !self.config_read_only();
            ui.add_enabled_ui(config_enabled, |ui| {
                ui.checkbox(&mut self.config.start_recording_on_launch, "随启动开始");
                ui.checkbox(&mut self.config.start_minimized_to_tray, "启动自动折叠");
            });
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

    pub(super) fn window_buttons(&mut self, ui: &mut egui::Ui) {
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

    pub(super) fn toggle_maximized(&mut self, ctx: &egui::Context) {
        let maximized = ctx.input(|input| input.viewport().maximized.unwrap_or(false));
        ctx.send_viewport_cmd(egui::ViewportCommand::Maximized(!maximized));
    }

    pub(super) fn indicator_size_points(&self, ctx: &egui::Context) -> f32 {
        let pixels_per_point =
            ctx.input(|input| input.viewport().native_pixels_per_point.unwrap_or(1.0));
        (self.config.indicator.diameter_px.max(1) as f32 / pixels_per_point.max(0.1)).max(1.0)
    }

    pub(super) fn show_indicator_viewports(&mut self, ctx: &egui::Context) {
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

    pub(super) fn main_panel(&mut self, ui: &mut egui::Ui) {
        let available_height = ui.available_height();
        let log_total_height = (available_height * 0.42).clamp(260.0, 360.0);
        let params_height = (available_height - log_total_height - 10.0).max(180.0);

        ui.allocate_ui(egui::vec2(ui.available_width(), params_height), |ui| {
            ui.columns(2, |cols| {
                egui::ScrollArea::vertical()
                    .id_salt("encoder_params_scroll")
                    .max_height(params_height)
                    .show(&mut cols[0], |ui| {
                        ui.add_enabled_ui(!self.config_read_only(), |ui| {
                            self.left_encoder_panel(ui);
                        });
                    });
                egui::ScrollArea::vertical()
                    .id_salt("loop_params_scroll")
                    .max_height(params_height)
                    .show(&mut cols[1], |ui| {
                        ui.add_enabled_ui(!self.config_read_only(), |ui| {
                            self.right_loop_panel(ui);
                        });
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

    pub(super) fn left_encoder_panel(&mut self, ui: &mut egui::Ui) {
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

    pub(super) fn current_rate_controls(&self) -> Vec<RateControlMethod> {
        let Some(caps) = &self.caps else {
            return Vec::new();
        };
        let Some(chroma) = self.config.chroma else {
            return Vec::new();
        };
        caps.rate_controls_for_chroma(chroma).to_vec()
    }

    pub(super) fn current_rate_control_features(
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

    pub(super) fn right_loop_panel(&mut self, ui: &mut egui::Ui) {
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
            ui.horizontal_wrapped(|ui| {
                let mut disk_mode = self.config.replay_buffer_mode.is_disk();
                if ui.checkbox(&mut disk_mode, "磁盘循环缓存").changed() {
                    self.config.replay_buffer_mode = if disk_mode {
                        ReplayBufferMode::Disk
                    } else {
                        ReplayBufferMode::Memory
                    };
                }
                ui.label(format!(
                    "当前模式：{}",
                    self.config.replay_buffer_mode.label()
                ));
            });
            ui.horizontal(|ui| {
                ui.label("循环缓存的目录 {dir：}");
                ui.add_enabled_ui(self.config.replay_buffer_mode.is_disk(), |ui| {
                    ui.text_edit_singleline(&mut self.config.cache_dir);
                    if ui.button("选择…").clicked()
                        && let Some(path) = pick_folder(&self.config.cache_dir)
                    {
                        self.config.cache_dir = path;
                    }
                });
            });
            if !self.config.replay_buffer_mode.is_disk() {
                ui.small(
                    "内存循环不使用缓存目录；保存时直接从内存 encoded ring 写入落盘保存目录。",
                );
            }
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
