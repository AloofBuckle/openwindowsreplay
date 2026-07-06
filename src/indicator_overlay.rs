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
    pub fn new(x: i32, y: i32, images: Vec<NativeIndicatorImage>) -> Self {
        let (cmd_tx, cmd_rx) = mpsc::channel();
        let handle = std::thread::Builder::new()
            .name("rustreplay-indicator-overlay".to_owned())
            .spawn(move || overlay_thread(cmd_rx, x, y, images))
            .ok();
        Self { cmd_tx, handle }
    }

    pub fn configure(&self, x: i32, y: i32, images: Vec<NativeIndicatorImage>) {
        let _ = self.cmd_tx.send(OverlayCommand::Configure { x, y, images });
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
) {
    use std::time::{Duration, Instant};
    use windows::Win32::Foundation::{COLORREF, HWND, LPARAM, LRESULT, POINT, SIZE, WPARAM};
    use windows::Win32::Graphics::Gdi::{
        AC_SRC_ALPHA, AC_SRC_OVER, BI_RGB, BITMAPINFO, BITMAPINFOHEADER, BLENDFUNCTION,
        CreateCompatibleDC, CreateDIBSection, DIB_RGB_COLORS, DeleteDC, DeleteObject, GetDC,
        HGDIOBJ, ReleaseDC, SelectObject,
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

    unsafe extern "system" fn wnd_proc(
        hwnd: HWND,
        msg: u32,
        wparam: WPARAM,
        lparam: LPARAM,
    ) -> LRESULT {
        unsafe { DefWindowProcW(hwnd, msg, wparam, lparam) }
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
    let mut active: Option<usize> = None;
    let mut last_topmost = Instant::now();

    loop {
        while let Ok(cmd) = cmd_rx.try_recv() {
            match cmd {
                OverlayCommand::Configure {
                    x: new_x,
                    y: new_y,
                    images: new_images,
                } => {
                    x = new_x;
                    y = new_y;
                    images = new_images;
                    if let Some(index) = active
                        && let Some(image) = images.get(index)
                    {
                        unsafe {
                            let _ = paint_layered(hwnd, x, y, image);
                            let _ = ShowWindow(hwnd, SW_SHOWNOACTIVATE);
                            let _ = SetWindowPos(
                                hwnd,
                                Some(HWND_TOPMOST),
                                x,
                                y,
                                image.width,
                                image.height,
                                SWP_NOACTIVATE | SWP_SHOWWINDOW,
                            );
                        }
                    }
                }
                OverlayCommand::Show(index) => {
                    active = Some(index);
                    if let Some(image) = images.get(index) {
                        unsafe {
                            let _ = paint_layered(hwnd, x, y, image);
                            let _ = ShowWindow(hwnd, SW_SHOWNOACTIVATE);
                            let _ = SetWindowPos(
                                hwnd,
                                Some(HWND_TOPMOST),
                                x,
                                y,
                                image.width,
                                image.height,
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
            if let Some(index) = active
                && let Some(image) = images.get(index)
            {
                unsafe {
                    let _ = ShowWindow(hwnd, SW_SHOWNOACTIVATE);
                    let _ = SetWindowPos(
                        hwnd,
                        Some(HWND_TOPMOST),
                        x,
                        y,
                        image.width,
                        image.height,
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
    pub fn new(_x: i32, _y: i32, _images: Vec<NativeIndicatorImage>) -> Self {
        Self
    }

    pub fn configure(&self, _x: i32, _y: i32, _images: Vec<NativeIndicatorImage>) {}

    pub fn show(&self, _color_index: usize) {}

    pub fn hide(&self) {}
}
