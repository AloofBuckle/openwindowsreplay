//! 保存即时回放热键。
//!
//! 这里刻意只注册一个“保存即时回放/重放”全局热键，不扩展开始/停止等其它动作。

use crate::config::{HotkeyConfig, HotkeyKey};
use std::sync::mpsc::{self, Receiver};

#[derive(Debug)]
pub enum HotkeyEvent {
    Pressed,
    Status(String),
}

#[cfg(windows)]
enum HotkeyCommand {
    Set(Option<HotkeyConfig>),
    Shutdown,
}

#[cfg(windows)]
pub struct HotkeyRuntime {
    cmd_tx: mpsc::Sender<HotkeyCommand>,
    event_rx: Receiver<HotkeyEvent>,
    handle: Option<std::thread::JoinHandle<()>>,
}

#[cfg(windows)]
impl HotkeyRuntime {
    pub fn new(initial: HotkeyConfig) -> Self {
        let (cmd_tx, cmd_rx) = mpsc::channel();
        let (event_tx, event_rx) = mpsc::channel();
        let handle = std::thread::Builder::new()
            .name("rustreplay-save-hotkey".to_owned())
            .spawn(move || hotkey_thread(cmd_rx, event_tx, Some(initial)))
            .ok();
        if handle.is_none() {
            let _ = cmd_tx.send(HotkeyCommand::Shutdown);
        }
        Self {
            cmd_tx,
            event_rx,
            handle,
        }
    }

    pub fn set_hotkey(&self, hotkey: Option<HotkeyConfig>) {
        let _ = self.cmd_tx.send(HotkeyCommand::Set(hotkey));
    }

    pub fn drain_events(&self) -> Vec<HotkeyEvent> {
        let mut events = Vec::new();
        while let Ok(event) = self.event_rx.try_recv() {
            events.push(event);
        }
        events
    }
}

#[cfg(windows)]
impl Drop for HotkeyRuntime {
    fn drop(&mut self) {
        let _ = self.cmd_tx.send(HotkeyCommand::Shutdown);
        if let Some(handle) = self.handle.take() {
            let _ = handle.join();
        }
    }
}

#[cfg(windows)]
fn hotkey_thread(
    cmd_rx: Receiver<HotkeyCommand>,
    event_tx: mpsc::Sender<HotkeyEvent>,
    initial: Option<HotkeyConfig>,
) {
    use std::time::Duration;
    use windows::Win32::UI::Input::KeyboardAndMouse::{
        HOT_KEY_MODIFIERS, MOD_ALT, MOD_CONTROL, MOD_NOREPEAT, MOD_SHIFT, RegisterHotKey,
        UnregisterHotKey,
    };
    use windows::Win32::UI::WindowsAndMessaging::{MSG, PM_REMOVE, PeekMessageW, WM_HOTKEY};

    const HOTKEY_ID: i32 = 0x5252;

    let mut registered = false;
    let mut active = None;

    let apply =
        |target: Option<HotkeyConfig>, registered: &mut bool, active: &mut Option<HotkeyConfig>| {
            if *registered {
                unsafe {
                    let _ = UnregisterHotKey(None, HOTKEY_ID);
                }
                *registered = false;
                *active = None;
            }

            let Some(hotkey) = target else {
                let _ = event_tx.send(HotkeyEvent::Status(
                    "保存即时回放热键已暂停，等待重新绑定。".to_owned(),
                ));
                return;
            };

            if !hotkey.is_safe_global_binding() {
                let _ = event_tx.send(HotkeyEvent::Status(format!(
                    "保存即时回放热键未注册：{} 会拦截普通输入，字母/数字必须搭配 Ctrl/Alt/Shift。",
                    hotkey.label()
                )));
                return;
            }

            let mut modifiers = HOT_KEY_MODIFIERS(MOD_NOREPEAT.0);
            if hotkey.ctrl {
                modifiers |= MOD_CONTROL;
            }
            if hotkey.alt {
                modifiers |= MOD_ALT;
            }
            if hotkey.shift {
                modifiers |= MOD_SHIFT;
            }

            let vk = vk_for_key(hotkey.key);
            match unsafe { RegisterHotKey(None, HOTKEY_ID, modifiers, vk) } {
                Ok(()) => {
                    *registered = true;
                    *active = Some(hotkey);
                    let _ = event_tx.send(HotkeyEvent::Status(format!(
                        "保存即时回放全局热键已注册：{}",
                        hotkey.label()
                    )));
                }
                Err(err) => {
                    let _ = event_tx.send(HotkeyEvent::Status(format!(
                        "保存即时回放热键注册失败：{}；可能已被系统或其它程序占用。",
                        err.message()
                    )));
                }
            }
        };

    // 创建本线程消息队列后再注册热键，WM_HOTKEY 会投递到这个线程。
    let mut msg = MSG::default();
    unsafe {
        let _ = PeekMessageW(&mut msg, None, 0, 0, PM_REMOVE);
    }
    apply(initial, &mut registered, &mut active);

    loop {
        while let Ok(cmd) = cmd_rx.try_recv() {
            match cmd {
                HotkeyCommand::Set(hotkey) => apply(hotkey, &mut registered, &mut active),
                HotkeyCommand::Shutdown => {
                    if registered {
                        unsafe {
                            let _ = UnregisterHotKey(None, HOTKEY_ID);
                        }
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
            if msg.message == WM_HOTKEY && msg.wParam.0 as i32 == HOTKEY_ID && active.is_some() {
                let _ = event_tx.send(HotkeyEvent::Pressed);
            }
        }

        std::thread::sleep(Duration::from_millis(10));
    }
}

#[cfg(windows)]
fn vk_for_key(key: HotkeyKey) -> u32 {
    match key {
        HotkeyKey::Num0 => 0x30,
        HotkeyKey::Num1 => 0x31,
        HotkeyKey::Num2 => 0x32,
        HotkeyKey::Num3 => 0x33,
        HotkeyKey::Num4 => 0x34,
        HotkeyKey::Num5 => 0x35,
        HotkeyKey::Num6 => 0x36,
        HotkeyKey::Num7 => 0x37,
        HotkeyKey::Num8 => 0x38,
        HotkeyKey::Num9 => 0x39,
        HotkeyKey::A => 0x41,
        HotkeyKey::B => 0x42,
        HotkeyKey::C => 0x43,
        HotkeyKey::D => 0x44,
        HotkeyKey::E => 0x45,
        HotkeyKey::F => 0x46,
        HotkeyKey::G => 0x47,
        HotkeyKey::H => 0x48,
        HotkeyKey::I => 0x49,
        HotkeyKey::J => 0x4A,
        HotkeyKey::K => 0x4B,
        HotkeyKey::L => 0x4C,
        HotkeyKey::M => 0x4D,
        HotkeyKey::N => 0x4E,
        HotkeyKey::O => 0x4F,
        HotkeyKey::P => 0x50,
        HotkeyKey::Q => 0x51,
        HotkeyKey::R => 0x52,
        HotkeyKey::S => 0x53,
        HotkeyKey::T => 0x54,
        HotkeyKey::U => 0x55,
        HotkeyKey::V => 0x56,
        HotkeyKey::W => 0x57,
        HotkeyKey::X => 0x58,
        HotkeyKey::Y => 0x59,
        HotkeyKey::Z => 0x5A,
        HotkeyKey::F1 => 0x70,
        HotkeyKey::F2 => 0x71,
        HotkeyKey::F3 => 0x72,
        HotkeyKey::F4 => 0x73,
        HotkeyKey::F5 => 0x74,
        HotkeyKey::F6 => 0x75,
        HotkeyKey::F7 => 0x76,
        HotkeyKey::F8 => 0x77,
        HotkeyKey::F9 => 0x78,
        HotkeyKey::F10 => 0x79,
        HotkeyKey::F11 => 0x7A,
        HotkeyKey::F12 => 0x7B,
        HotkeyKey::F13 => 0x7C,
        HotkeyKey::F14 => 0x7D,
        HotkeyKey::F15 => 0x7E,
        HotkeyKey::F16 => 0x7F,
        HotkeyKey::F17 => 0x80,
        HotkeyKey::F18 => 0x81,
        HotkeyKey::F19 => 0x82,
        HotkeyKey::F20 => 0x83,
        HotkeyKey::F21 => 0x84,
        HotkeyKey::F22 => 0x85,
        HotkeyKey::F23 => 0x86,
        HotkeyKey::F24 => 0x87,
    }
}

#[cfg(not(windows))]
pub struct HotkeyRuntime {
    event_rx: Receiver<HotkeyEvent>,
}

#[cfg(not(windows))]
impl HotkeyRuntime {
    pub fn new(_initial: HotkeyConfig) -> Self {
        let (event_tx, event_rx) = mpsc::channel();
        let _ = event_tx.send(HotkeyEvent::Status(
            "全局保存热键仅在 Windows 上注册；当前平台只保存配置。".to_owned(),
        ));
        Self { event_rx }
    }

    pub fn set_hotkey(&self, _hotkey: Option<HotkeyConfig>) {}

    pub fn drain_events(&self) -> Vec<HotkeyEvent> {
        let mut events = Vec::new();
        while let Ok(event) = self.event_rx.try_recv() {
            events.push(event);
        }
        events
    }
}
