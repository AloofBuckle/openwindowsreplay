//! 状态指示器的原生覆盖层。
//!
//! 状态指示器不能用 egui 子窗口实现：子窗口会参与普通窗口焦点/最小化逻辑，
//! 在状态变化时可能抢焦点，也容易被 Win+D 一起隐藏。这里使用 Win32
//! layered + topmost + toolwindow + noactivate + transparent popup，尽量接近游戏
//! 覆盖层的行为；它不接收鼠标，不显示在任务栏，也不会激活窗口。

use std::sync::mpsc::{self, Receiver};

#[derive(Debug, Clone)]
pub struct NativeIndicatorImage {
    pub width: i32,
    pub height: i32,
    /// BGRA，且 RGB 已按 alpha 预乘，供 UpdateLayeredWindow/AC_SRC_ALPHA 使用。
    pub premul_bgra: Vec<u8>,
}

#[cfg(windows)]
enum OverlayCommand {
    Configure {
        x: i32,
        y: i32,
        images: Vec<NativeIndicatorImage>,
        text_enabled: bool,
    },
    Show(usize),
    Hide,
    Shutdown,
}

#[cfg(windows)]
pub struct IndicatorOverlayRuntime {
    cmd_tx: mpsc::Sender<OverlayCommand>,
    handle: Option<std::thread::JoinHandle<()>>,
}

#[cfg(windows)]
impl IndicatorOverlayRuntime {
    pub fn new(x: i32, y: i32, images: Vec<NativeIndicatorImage>, text_enabled: bool) -> Self {
        let (cmd_tx, cmd_rx) = mpsc::channel();
        let handle = std::thread::Builder::new()
            .name("rustreplay-indicator-overlay".to_owned())
            .spawn(move || overlay_thread(cmd_rx, x, y, images, text_enabled))
            .ok();
        Self { cmd_tx, handle }
    }

    pub fn configure(&self, x: i32, y: i32, images: Vec<NativeIndicatorImage>, text_enabled: bool) {
        let _ = self.cmd_tx.send(OverlayCommand::Configure {
            x,
            y,
            images,
            text_enabled,
        });
    }

    pub fn show(&self, color_index: usize) {
        let _ = self.cmd_tx.send(OverlayCommand::Show(color_index));
    }

    pub fn hide(&self) {
        let _ = self.cmd_tx.send(OverlayCommand::Hide);
    }
}

#[cfg(windows)]
impl Drop for IndicatorOverlayRuntime {
    fn drop(&mut self) {
        let _ = self.cmd_tx.send(OverlayCommand::Shutdown);
        if let Some(handle) = self.handle.take() {
            let _ = handle.join();
        }
    }
}

#[cfg(windows)]
fn overlay_thread(
    cmd_rx: Receiver<OverlayCommand>,
    initial_x: i32,
    initial_y: i32,
    initial_images: Vec<NativeIndicatorImage>,
    initial_text_enabled: bool,
) {
    use std::time::{Duration, Instant};
    use windows::Win32::Foundation::{COLORREF, HWND, LPARAM, LRESULT, POINT, SIZE, WPARAM};
    use windows::Win32::Graphics::Gdi::{
        AC_SRC_ALPHA, AC_SRC_OVER, ANTIALIASED_QUALITY, BI_RGB, BITMAPINFO, BITMAPINFOHEADER,
        BLENDFUNCTION, CLIP_DEFAULT_PRECIS, CreateCompatibleDC, CreateDIBSection, CreateFontW,
        DEFAULT_CHARSET, DEFAULT_PITCH, DIB_RGB_COLORS, DeleteDC, DeleteObject, FF_DONTCARE,
        FW_SEMIBOLD, GetDC, GetTextExtentPoint32W, HGDIOBJ, OUT_DEFAULT_PRECIS, ReleaseDC,
        SelectObject, SetBkMode, SetTextColor, TRANSPARENT, TextOutW,
    };
    use windows::Win32::UI::WindowsAndMessaging::{
        CreateWindowExW, DefWindowProcW, DestroyWindow, DispatchMessageW, HWND_TOPMOST, MSG,
        PM_REMOVE, PeekMessageW, RegisterClassW, SW_HIDE, SW_SHOWNOACTIVATE, SWP_NOACTIVATE,
        SWP_SHOWWINDOW, SetWindowPos, ShowWindow, TranslateMessage, ULW_ALPHA, UpdateLayeredWindow,
        WNDCLASSW, WS_EX_LAYERED, WS_EX_NOACTIVATE, WS_EX_TOOLWINDOW, WS_EX_TOPMOST,
        WS_EX_TRANSPARENT, WS_POPUP,
    };

    fn wide_with_nul(text: &str) -> Vec<u16> {
        text.encode_utf16().chain(std::iter::once(0)).collect()
    }

    fn text_for_color_index(index: usize) -> &'static str {
        match index {
            0 => "录制中",
            1 => "保存成功",
            2 => "保存失败",
            _ => "未定义",
        }
    }

    fn rgb_for_color_index(index: usize) -> (u8, u8, u8) {
        match index {
            0 => (0, 122, 255),
            1 => (0, 200, 90),
            2 => (255, 55, 55),
            3 => (255, 210, 0),
            _ => (255, 210, 0),
        }
    }

    unsafe extern "system" fn wnd_proc(
        hwnd: HWND,
        msg: u32,
        wparam: WPARAM,
        lparam: LPARAM,
    ) -> LRESULT {
        unsafe { DefWindowProcW(hwnd, msg, wparam, lparam) }
    }

    fn premul(channel: u8, alpha: u8) -> u8 {
        ((channel as u16 * alpha as u16 + 127) / 255) as u8
    }

    fn overlay_image_for(
        source: &NativeIndicatorImage,
        color_index: usize,
        text_enabled: bool,
    ) -> NativeIndicatorImage {
        if !text_enabled || source.width <= 0 || source.height <= 0 {
            return source.clone();
        }
        let font_px = ((source.height as f32 * 0.72).round() as i32).clamp(12, 64);
        let Some(text_image) = (unsafe {
            render_text_image(
                text_for_color_index(color_index),
                rgb_for_color_index(color_index),
                font_px,
            )
        }) else {
            return source.clone();
        };

        let gap = (source.height / 4).clamp(6, 16);
        let width = source
            .width
            .saturating_add(gap)
            .saturating_add(text_image.width);
        let height = source.height.max(text_image.height);
        if width <= 0 || height <= 0 {
            return source.clone();
        }
        let mut premul_bgra = vec![0; width as usize * height as usize * 4];
        blit_image(
            &mut premul_bgra,
            width,
            height,
            source,
            0,
            (height - source.height) / 2,
        );
        blit_image(
            &mut premul_bgra,
            width,
            height,
            &text_image,
            source.width + gap,
            (height - text_image.height) / 2,
        );
        NativeIndicatorImage {
            width,
            height,
            premul_bgra,
        }
    }

    fn blit_image(
        dst: &mut [u8],
        dst_width: i32,
        dst_height: i32,
        src: &NativeIndicatorImage,
        dst_x: i32,
        dst_y: i32,
    ) {
        if dst_width <= 0 || dst_height <= 0 || src.width <= 0 || src.height <= 0 {
            return;
        }
        let src_stride = src.width as usize * 4;
        let dst_stride = dst_width as usize * 4;
        for y in 0..src.height {
            let target_y = dst_y + y;
            if !(0..dst_height).contains(&target_y) {
                continue;
            }
            let target_x = dst_x.max(0);
            let copy_width = (src.width - (target_x - dst_x)).min(dst_width - target_x);
            if copy_width <= 0 {
                continue;
            }
            let src_offset = y as usize * src_stride + (target_x - dst_x) as usize * 4;
            let dst_offset = target_y as usize * dst_stride + target_x as usize * 4;
            let bytes = copy_width as usize * 4;
            if src_offset + bytes <= src.premul_bgra.len() && dst_offset + bytes <= dst.len() {
                dst[dst_offset..dst_offset + bytes]
                    .copy_from_slice(&src.premul_bgra[src_offset..src_offset + bytes]);
            }
        }
    }

    unsafe fn render_text_image(
        text: &str,
        (red, green, blue): (u8, u8, u8),
        font_px: i32,
    ) -> Option<NativeIndicatorImage> {
        let text_wide: Vec<u16> = text.encode_utf16().collect();
        if text_wide.is_empty() {
            return None;
        }

        let screen_dc = unsafe { GetDC(None) };
        if screen_dc.is_invalid() {
            return None;
        }
        let mem_dc = unsafe { CreateCompatibleDC(Some(screen_dc)) };
        if mem_dc.is_invalid() {
            unsafe {
                let _ = ReleaseDC(None, screen_dc);
            }
            return None;
        }

        let face_name = wide_with_nul("Microsoft YaHei UI");
        let font = unsafe {
            CreateFontW(
                -font_px,
                0,
                0,
                0,
                FW_SEMIBOLD.0 as i32,
                0,
                0,
                0,
                DEFAULT_CHARSET,
                OUT_DEFAULT_PRECIS,
                CLIP_DEFAULT_PRECIS,
                ANTIALIASED_QUALITY,
                DEFAULT_PITCH.0 as u32 | FF_DONTCARE.0 as u32,
                windows::core::PCWSTR(face_name.as_ptr()),
            )
        };
        if font.is_invalid() {
            unsafe {
                let _ = DeleteDC(mem_dc);
                let _ = ReleaseDC(None, screen_dc);
            }
            return None;
        }
        let old_font = unsafe { SelectObject(mem_dc, HGDIOBJ::from(font)) };

        let mut measured = SIZE::default();
        if !unsafe { GetTextExtentPoint32W(mem_dc, &text_wide, &mut measured).as_bool() } {
            unsafe {
                if !old_font.0.is_null() {
                    let _ = SelectObject(mem_dc, old_font);
                }
                let _ = DeleteObject(font.into());
                let _ = DeleteDC(mem_dc);
                let _ = ReleaseDC(None, screen_dc);
            }
            return None;
        }
        let width = (measured.cx + font_px / 3).max(1);
        let height = (measured.cy + 2).max(1);
        let expected = width as usize * height as usize * 4;
        let bitmap_info = BITMAPINFO {
            bmiHeader: BITMAPINFOHEADER {
                biSize: std::mem::size_of::<BITMAPINFOHEADER>() as u32,
                biWidth: width,
                biHeight: -height,
                biPlanes: 1,
                biBitCount: 32,
                biCompression: BI_RGB.0,
                biSizeImage: expected as u32,
                ..Default::default()
            },
            ..Default::default()
        };
        let mut bits = std::ptr::null_mut();
        let Ok(bitmap) = (unsafe {
            CreateDIBSection(
                Some(screen_dc),
                &bitmap_info,
                DIB_RGB_COLORS,
                &mut bits,
                None,
                0,
            )
        }) else {
            unsafe {
                if !old_font.0.is_null() {
                    let _ = SelectObject(mem_dc, old_font);
                }
                let _ = DeleteObject(font.into());
                let _ = DeleteDC(mem_dc);
                let _ = ReleaseDC(None, screen_dc);
            }
            return None;
        };
        if bits.is_null() {
            unsafe {
                let _ = DeleteObject(bitmap.into());
                if !old_font.0.is_null() {
                    let _ = SelectObject(mem_dc, old_font);
                }
                let _ = DeleteObject(font.into());
                let _ = DeleteDC(mem_dc);
                let _ = ReleaseDC(None, screen_dc);
            }
            return None;
        }

        unsafe {
            std::ptr::write_bytes(bits.cast::<u8>(), 0, expected);
        }
        let old_bitmap = unsafe { SelectObject(mem_dc, HGDIOBJ::from(bitmap)) };
        unsafe {
            let _ = SetBkMode(mem_dc, TRANSPARENT);
            let _ = SetTextColor(mem_dc, COLORREF(0x00ff_ffff));
            let _ = TextOutW(mem_dc, 0, 1, &text_wide);
        }

        let mask = unsafe { std::slice::from_raw_parts(bits.cast::<u8>(), expected) };
        let mut premul_bgra = Vec::with_capacity(expected);
        for pixel in mask.chunks_exact(4) {
            let alpha = pixel[0].max(pixel[1]).max(pixel[2]);
            premul_bgra.push(premul(blue, alpha));
            premul_bgra.push(premul(green, alpha));
            premul_bgra.push(premul(red, alpha));
            premul_bgra.push(alpha);
        }

        unsafe {
            if !old_bitmap.0.is_null() {
                let _ = SelectObject(mem_dc, old_bitmap);
            }
            if !old_font.0.is_null() {
                let _ = SelectObject(mem_dc, old_font);
            }
            let _ = DeleteObject(bitmap.into());
            let _ = DeleteObject(font.into());
            let _ = DeleteDC(mem_dc);
            let _ = ReleaseDC(None, screen_dc);
        }

        Some(NativeIndicatorImage {
            width,
            height,
            premul_bgra,
        })
    }

    unsafe fn paint_layered(hwnd: HWND, x: i32, y: i32, image: &NativeIndicatorImage) -> bool {
        if image.width <= 0 || image.height <= 0 {
            return false;
        }
        let expected = (image.width as usize)
            .saturating_mul(image.height as usize)
            .saturating_mul(4);
        if image.premul_bgra.len() != expected {
            return false;
        }

        let screen_dc = unsafe { GetDC(None) };
        if screen_dc.is_invalid() {
            return false;
        }
        let mem_dc = unsafe { CreateCompatibleDC(Some(screen_dc)) };
        if mem_dc.is_invalid() {
            unsafe {
                let _ = ReleaseDC(None, screen_dc);
            }
            return false;
        }

        let bitmap_info = BITMAPINFO {
            bmiHeader: BITMAPINFOHEADER {
                biSize: std::mem::size_of::<BITMAPINFOHEADER>() as u32,
                biWidth: image.width,
                // 负高度表示 top-down DIB，避免手动翻转。
                biHeight: -image.height,
                biPlanes: 1,
                biBitCount: 32,
                biCompression: BI_RGB.0,
                biSizeImage: expected as u32,
                ..Default::default()
            },
            ..Default::default()
        };
        let mut bits = std::ptr::null_mut();
        let Ok(bitmap) = (unsafe {
            CreateDIBSection(
                Some(screen_dc),
                &bitmap_info,
                DIB_RGB_COLORS,
                &mut bits,
                None,
                0,
            )
        }) else {
            unsafe {
                let _ = DeleteDC(mem_dc);
                let _ = ReleaseDC(None, screen_dc);
            }
            return false;
        };
        if bits.is_null() {
            unsafe {
                let _ = DeleteObject(bitmap.into());
                let _ = DeleteDC(mem_dc);
                let _ = ReleaseDC(None, screen_dc);
            }
            return false;
        }
        unsafe {
            std::ptr::copy_nonoverlapping(image.premul_bgra.as_ptr(), bits.cast::<u8>(), expected);
        }

        let old = unsafe { SelectObject(mem_dc, HGDIOBJ::from(bitmap)) };
        let dst = POINT { x, y };
        let size = SIZE {
            cx: image.width,
            cy: image.height,
        };
        let src = POINT { x: 0, y: 0 };
        let blend = BLENDFUNCTION {
            BlendOp: AC_SRC_OVER as u8,
            BlendFlags: 0,
            SourceConstantAlpha: 255,
            AlphaFormat: AC_SRC_ALPHA as u8,
        };

        let ok = unsafe {
            UpdateLayeredWindow(
                hwnd,
                Some(screen_dc),
                Some(&dst),
                Some(&size),
                Some(mem_dc),
                Some(&src),
                COLORREF(0),
                Some(&blend),
                ULW_ALPHA,
            )
            .is_ok()
        };

        unsafe {
            if !old.0.is_null() {
                let _ = SelectObject(mem_dc, old);
            }
            let _ = DeleteObject(bitmap.into());
            let _ = DeleteDC(mem_dc);
            let _ = ReleaseDC(None, screen_dc);
        }
        ok
    }

    let class_name = wide_with_nul("RustReplayNativeIndicatorOverlay");
    let window_name = wide_with_nul("RustReplay 状态指示器原生覆盖层");
    let wnd_class = WNDCLASSW {
        lpfnWndProc: Some(wnd_proc),
        lpszClassName: windows::core::PCWSTR(class_name.as_ptr()),
        ..Default::default()
    };
    unsafe {
        let _ = RegisterClassW(&wnd_class);
    }

    let ex_style =
        WS_EX_LAYERED | WS_EX_TRANSPARENT | WS_EX_TOOLWINDOW | WS_EX_NOACTIVATE | WS_EX_TOPMOST;
    let hwnd = match unsafe {
        CreateWindowExW(
            ex_style,
            windows::core::PCWSTR(class_name.as_ptr()),
            windows::core::PCWSTR(window_name.as_ptr()),
            WS_POPUP,
            initial_x,
            initial_y,
            1,
            1,
            None,
            None,
            None,
            None,
        )
    } {
        Ok(hwnd) => hwnd,
        Err(_) => return,
    };

    let mut x = initial_x;
    let mut y = initial_y;
    let mut images = initial_images;
    let mut text_enabled = initial_text_enabled;
    let mut active: Option<usize> = None;
    let mut active_size = SIZE { cx: 1, cy: 1 };
    let mut last_topmost = Instant::now();

    loop {
        while let Ok(cmd) = cmd_rx.try_recv() {
            match cmd {
                OverlayCommand::Configure {
                    x: new_x,
                    y: new_y,
                    images: new_images,
                    text_enabled: new_text_enabled,
                } => {
                    x = new_x;
                    y = new_y;
                    images = new_images;
                    text_enabled = new_text_enabled;
                    if let Some(index) = active
                        && let Some(image) = images.get(index)
                    {
                        let display_image = overlay_image_for(image, index, text_enabled);
                        active_size = SIZE {
                            cx: display_image.width,
                            cy: display_image.height,
                        };
                        unsafe {
                            let _ = paint_layered(hwnd, x, y, &display_image);
                            let _ = ShowWindow(hwnd, SW_SHOWNOACTIVATE);
                            let _ = SetWindowPos(
                                hwnd,
                                Some(HWND_TOPMOST),
                                x,
                                y,
                                active_size.cx,
                                active_size.cy,
                                SWP_NOACTIVATE | SWP_SHOWWINDOW,
                            );
                        }
                    }
                }
                OverlayCommand::Show(index) => {
                    active = Some(index);
                    if let Some(image) = images.get(index) {
                        let display_image = overlay_image_for(image, index, text_enabled);
                        active_size = SIZE {
                            cx: display_image.width,
                            cy: display_image.height,
                        };
                        unsafe {
                            let _ = paint_layered(hwnd, x, y, &display_image);
                            let _ = ShowWindow(hwnd, SW_SHOWNOACTIVATE);
                            let _ = SetWindowPos(
                                hwnd,
                                Some(HWND_TOPMOST),
                                x,
                                y,
                                active_size.cx,
                                active_size.cy,
                                SWP_NOACTIVATE | SWP_SHOWWINDOW,
                            );
                        }
                    }
                }
                OverlayCommand::Hide => {
                    active = None;
                    unsafe {
                        let _ = ShowWindow(hwnd, SW_HIDE);
                    }
                }
                OverlayCommand::Shutdown => {
                    unsafe {
                        let _ = DestroyWindow(hwnd);
                    }
                    return;
                }
            }
        }

        loop {
            let mut msg = MSG::default();
            let has_message = unsafe { PeekMessageW(&mut msg, None, 0, 0, PM_REMOVE).as_bool() };
            if !has_message {
                break;
            }
            unsafe {
                let _ = TranslateMessage(&msg);
                DispatchMessageW(&msg);
            }
        }

        // 对抗 Win+D / z-order 重排：只用 SW_SHOWNOACTIVATE + SWP_NOACTIVATE，
        // 不抢焦点，也不干扰浏览器/游戏输入。
        if last_topmost.elapsed() >= Duration::from_millis(250) {
            last_topmost = Instant::now();
            if active.is_some() {
                unsafe {
                    let _ = ShowWindow(hwnd, SW_SHOWNOACTIVATE);
                    let _ = SetWindowPos(
                        hwnd,
                        Some(HWND_TOPMOST),
                        x,
                        y,
                        active_size.cx,
                        active_size.cy,
                        SWP_NOACTIVATE | SWP_SHOWWINDOW,
                    );
                }
            }
        }

        std::thread::sleep(Duration::from_millis(10));
    }
}

#[cfg(not(windows))]
pub struct IndicatorOverlayRuntime;

#[cfg(not(windows))]
impl IndicatorOverlayRuntime {
    pub fn new(_x: i32, _y: i32, _images: Vec<NativeIndicatorImage>, _text_enabled: bool) -> Self {
        Self
    }

    pub fn configure(
        &self,
        _x: i32,
        _y: i32,
        _images: Vec<NativeIndicatorImage>,
        _text_enabled: bool,
    ) {
    }

    pub fn show(&self, _color_index: usize) {}

    pub fn hide(&self) {}
}
