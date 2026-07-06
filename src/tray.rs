//! Windows 通知区域菜单。
//!
//! 用户口径是“任务栏右键菜单”。Windows 上可稳定自绘的入口是通知区域
//! 图标，所以这里提供一个托盘图标，右键菜单只包含录制控制与真正退出程序。

use eframe::egui;
use std::sync::mpsc::{self, Receiver};

#[derive(Debug)]
pub enum TrayEvent {
    StartReplay,
    SaveReplay,
    StopReplay,
    ExitProgram,
    Status(String),
}

#[cfg(windows)]
enum TrayCommand {
    Shutdown,
}

#[cfg(windows)]
pub struct TrayRuntime {
    cmd_tx: mpsc::Sender<TrayCommand>,
    event_rx: Receiver<TrayEvent>,
    handle: Option<std::thread::JoinHandle<()>>,
}

#[cfg(windows)]
impl TrayRuntime {
    pub fn new(repaint_ctx: egui::Context) -> Self {
        let (cmd_tx, cmd_rx) = mpsc::channel();
        let (event_tx, event_rx) = mpsc::channel();
        let handle = std::thread::Builder::new()
            .name("rustreplay-tray".to_owned())
            .spawn(move || tray_thread(cmd_rx, event_tx, repaint_ctx))
            .ok();
        if handle.is_none() {
            let _ = cmd_tx.send(TrayCommand::Shutdown);
        }
        Self {
            cmd_tx,
            event_rx,
            handle,
        }
    }

    pub fn drain_events(&self) -> Vec<TrayEvent> {
        let mut events = Vec::new();
        while let Ok(event) = self.event_rx.try_recv() {
            events.push(event);
        }
        events
    }
}

#[cfg(windows)]
impl Drop for TrayRuntime {
    fn drop(&mut self) {
        let _ = self.cmd_tx.send(TrayCommand::Shutdown);
        if let Some(handle) = self.handle.take() {
            let _ = handle.join();
        }
    }
}

#[cfg(windows)]
fn tray_thread(
    cmd_rx: Receiver<TrayCommand>,
    event_tx: mpsc::Sender<TrayEvent>,
    repaint_ctx: egui::Context,
) {
    use std::sync::{Mutex, OnceLock};
    use std::time::Duration;
    use windows::Win32::Foundation::{HWND, LPARAM, LRESULT, POINT, WPARAM};
    use windows::Win32::UI::Shell::{
        NIF_ICON, NIF_MESSAGE, NIF_TIP, NIM_ADD, NIM_DELETE, NOTIFYICONDATAW, Shell_NotifyIconW,
    };
    use windows::Win32::UI::WindowsAndMessaging::{
        AppendMenuW, CreatePopupMenu, CreateWindowExW, DefWindowProcW, DestroyMenu, DestroyWindow,
        DispatchMessageW, GetCursorPos, IDI_APPLICATION, LoadIconW, MF_STRING, MSG, PM_REMOVE,
        PeekMessageW, PostMessageW, RegisterClassW, SetForegroundWindow, TPM_RETURNCMD,
        TPM_RIGHTBUTTON, TrackPopupMenu, TranslateMessage, WINDOW_EX_STYLE, WINDOW_STYLE,
        WM_CONTEXTMENU, WM_LBUTTONDBLCLK, WM_NULL, WM_RBUTTONUP, WNDCLASSW,
    };

    const WM_TRAY_ICON: u32 = 0x8000 + 88;
    const TRAY_ID: u32 = 1;
    const CMD_START: u32 = 1001;
    const CMD_SAVE: u32 = 1002;
    const CMD_STOP: u32 = 1003;
    const CMD_EXIT: u32 = 1004;

    static EVENT_TX: OnceLock<Mutex<Option<mpsc::Sender<TrayEvent>>>> = OnceLock::new();
    static REPAINT_CTX: OnceLock<Mutex<Option<egui::Context>>> = OnceLock::new();

    fn send_event(event: TrayEvent) {
        if let Some(lock) = EVENT_TX.get()
            && let Ok(guard) = lock.lock()
            && let Some(tx) = guard.as_ref()
        {
            let _ = tx.send(event);
        }
        if let Some(lock) = REPAINT_CTX.get()
            && let Ok(guard) = lock.lock()
            && let Some(ctx) = guard.as_ref()
        {
            ctx.request_repaint();
        }
    }

    fn wide_with_nul(text: &str) -> Vec<u16> {
        text.encode_utf16().chain(std::iter::once(0)).collect()
    }

    fn write_wide<const N: usize>(dst: &mut [u16; N], text: &str) {
        dst.fill(0);
        for (slot, code) in dst
            .iter_mut()
            .take(N.saturating_sub(1))
            .zip(text.encode_utf16())
        {
            *slot = code;
        }
    }

    unsafe extern "system" fn wnd_proc(
        hwnd: HWND,
        msg: u32,
        wparam: WPARAM,
        lparam: LPARAM,
    ) -> LRESULT {
        match msg {
            WM_TRAY_ICON => {
                let mouse_msg = lparam.0 as u32;
                if mouse_msg == WM_RBUTTONUP
                    || mouse_msg == WM_CONTEXTMENU
                    || mouse_msg == WM_LBUTTONDBLCLK
                {
                    unsafe {
                        show_tray_menu(hwnd);
                    }
                }
                LRESULT(0)
            }
            _ => unsafe { DefWindowProcW(hwnd, msg, wparam, lparam) },
        }
    }

    unsafe fn show_tray_menu(hwnd: HWND) {
        let Ok(menu) = (unsafe { CreatePopupMenu() }) else {
            return;
        };
        let start = wide_with_nul("开始回放");
        let save = wide_with_nul("保存回放");
        let stop = wide_with_nul("结束回放");
        let exit = wide_with_nul("退出程序");

        unsafe {
            let _ = AppendMenuW(
                menu,
                MF_STRING,
                CMD_START as usize,
                windows::core::PCWSTR(start.as_ptr()),
            );
            let _ = AppendMenuW(
                menu,
                MF_STRING,
                CMD_SAVE as usize,
                windows::core::PCWSTR(save.as_ptr()),
            );
            let _ = AppendMenuW(
                menu,
                MF_STRING,
                CMD_STOP as usize,
                windows::core::PCWSTR(stop.as_ptr()),
            );
            let _ = AppendMenuW(
                menu,
                MF_STRING,
                CMD_EXIT as usize,
                windows::core::PCWSTR(exit.as_ptr()),
            );

            let mut point = POINT::default();
            if GetCursorPos(&mut point).is_err() {
                let _ = DestroyMenu(menu);
                return;
            }
            let _ = SetForegroundWindow(hwnd);
            let cmd = TrackPopupMenu(
                menu,
                TPM_RETURNCMD | TPM_RIGHTBUTTON,
                point.x,
                point.y,
                Some(0),
                hwnd,
                None,
            )
            .0 as u32;
            let _ = PostMessageW(Some(hwnd), WM_NULL, WPARAM(0), LPARAM(0));
            let _ = DestroyMenu(menu);

            match cmd {
                CMD_START => send_event(TrayEvent::StartReplay),
                CMD_SAVE => send_event(TrayEvent::SaveReplay),
                CMD_STOP => send_event(TrayEvent::StopReplay),
                CMD_EXIT => send_event(TrayEvent::ExitProgram),
                _ => {}
            }
        }
    }

    let event_lock = EVENT_TX.get_or_init(|| Mutex::new(None));
    if let Ok(mut guard) = event_lock.lock() {
        *guard = Some(event_tx);
    }
    let repaint_lock = REPAINT_CTX.get_or_init(|| Mutex::new(None));
    if let Ok(mut guard) = repaint_lock.lock() {
        *guard = Some(repaint_ctx);
    }

    let class_name = wide_with_nul("RustReplayTrayWindow");
    let window_name = wide_with_nul("RustReplay 托盘菜单");
    let wnd_class = WNDCLASSW {
        lpfnWndProc: Some(wnd_proc),
        lpszClassName: windows::core::PCWSTR(class_name.as_ptr()),
        ..Default::default()
    };
    unsafe {
        let _ = RegisterClassW(&wnd_class);
    }

    let hwnd = match unsafe {
        CreateWindowExW(
            WINDOW_EX_STYLE(0),
            windows::core::PCWSTR(class_name.as_ptr()),
            windows::core::PCWSTR(window_name.as_ptr()),
            WINDOW_STYLE(0),
            0,
            0,
            0,
            0,
            None,
            None,
            None,
            None,
        )
    } {
        Ok(hwnd) => hwnd,
        Err(err) => {
            send_event(TrayEvent::Status(format!(
                "任务栏菜单窗口创建失败：{}",
                err.message()
            )));
            return;
        }
    };

    let hicon = unsafe { LoadIconW(None, IDI_APPLICATION).unwrap_or_default() };
    let mut nid = NOTIFYICONDATAW {
        cbSize: std::mem::size_of::<NOTIFYICONDATAW>() as u32,
        hWnd: hwnd,
        uID: TRAY_ID,
        uFlags: NIF_MESSAGE | NIF_ICON | NIF_TIP,
        uCallbackMessage: WM_TRAY_ICON,
        hIcon: hicon,
        ..Default::default()
    };
    write_wide(&mut nid.szTip, "OneVPL Replay");
    let added = unsafe { Shell_NotifyIconW(NIM_ADD, &nid).as_bool() };
    if added {
        send_event(TrayEvent::Status(
            "任务栏通知区域菜单已启用：右键图标可开始/保存/结束回放或退出程序。".to_owned(),
        ));
    } else {
        send_event(TrayEvent::Status("任务栏通知区域菜单创建失败。".to_owned()));
    }

    loop {
        if let Ok(cmd) = cmd_rx.try_recv() {
            match cmd {
                TrayCommand::Shutdown => {
                    if added {
                        unsafe {
                            let _ = Shell_NotifyIconW(NIM_DELETE, &nid);
                        }
                    }
                    unsafe {
                        let _ = DestroyWindow(hwnd);
                    }
                    if let Ok(mut guard) = EVENT_TX.get_or_init(|| Mutex::new(None)).lock() {
                        *guard = None;
                    }
                    if let Ok(mut guard) = REPAINT_CTX.get_or_init(|| Mutex::new(None)).lock() {
                        *guard = None;
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

        std::thread::sleep(Duration::from_millis(20));
    }
}

#[cfg(not(windows))]
pub struct TrayRuntime {
    event_rx: Receiver<TrayEvent>,
}

#[cfg(not(windows))]
impl TrayRuntime {
    pub fn new(repaint_ctx: egui::Context) -> Self {
        let (event_tx, event_rx) = mpsc::channel();
        let _ = event_tx.send(TrayEvent::Status(
            "任务栏通知区域菜单仅在 Windows 上启用。".to_owned(),
        ));
        repaint_ctx.request_repaint();
        Self { event_rx }
    }

    pub fn drain_events(&self) -> Vec<TrayEvent> {
        let mut events = Vec::new();
        while let Ok(event) = self.event_rx.try_recv() {
            events.push(event);
        }
        events
    }
}
