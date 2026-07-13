use super::*;

pub(super) fn log_panel(
    ui: &mut egui::Ui,
    id_salt: &'static str,
    title: &str,
    lines: &[String],
    height: f32,
) {
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

pub(super) fn log_drag_selection_scroll_offset(
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

pub(super) fn visible_log_text(lines: &[String]) -> String {
    let start = lines.len().saturating_sub(MAX_VISIBLE_LOG_LINES);
    lines[start..].join("\n")
}
