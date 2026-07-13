use super::*;

pub(super) fn draw_indicator_image(
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

pub(super) fn native_images_from_indicator_images(
    images: &IndicatorImages,
) -> Vec<NativeIndicatorImage> {
    images
        .images
        .iter()
        .map(native_image_from_color_image)
        .collect()
}

pub(super) fn native_image_from_color_image(image: &egui::ColorImage) -> NativeIndicatorImage {
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

pub(super) fn build_indicator_images_for_config(
    config: &AppConfig,
) -> (IndicatorImages, Option<String>) {
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

pub(super) fn build_circle_indicator_images(diameter_px: u32) -> Vec<egui::ColorImage> {
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

pub(super) fn build_custom_indicator_images(
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

pub(super) fn save_indicator_images(images: &[egui::ColorImage]) -> Result<(), String> {
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
