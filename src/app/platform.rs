use super::*;

#[cfg(windows)]
pub(super) fn current_cursor_physical_pos() -> Option<egui::Pos2> {
    use windows::Win32::Foundation::POINT;
    use windows::Win32::UI::WindowsAndMessaging::GetCursorPos;

    let mut point = POINT::default();
    unsafe { GetCursorPos(&mut point) }
        .ok()
        .map(|_| egui::pos2(point.x as f32, point.y as f32))
}

#[cfg(not(windows))]
pub(super) fn current_cursor_physical_pos() -> Option<egui::Pos2> {
    None
}

#[cfg(windows)]
pub(super) fn prepare_root_window_for_tray_start() {
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
pub(super) fn prepare_root_window_for_tray_start() {}

#[cfg(windows)]
pub(super) fn restore_root_window_from_tray_start() {
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
pub(super) fn restore_root_window_from_tray_start() {}

#[cfg(windows)]
pub(super) fn find_root_window() -> Option<windows::Win32::Foundation::HWND> {
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

pub(super) fn egui_key_to_hotkey_key(key: egui::Key) -> Option<HotkeyKey> {
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

pub(super) fn is_modifier_key(key: egui::Key) -> bool {
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

pub(super) fn install_chinese_font(ctx: &egui::Context) -> String {
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
pub(super) fn pick_folder(_current: &str) -> Option<String> {
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
pub(super) fn pick_image_file() -> Option<String> {
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
pub(super) fn pick_folder(_current: &str) -> Option<String> {
    None
}

#[cfg(not(windows))]
pub(super) fn pick_image_file() -> Option<String> {
    None
}
