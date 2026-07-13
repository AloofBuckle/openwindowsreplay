//! Single-instance guard and activation signal.

#[cfg(windows)]
use windows::Win32::Foundation::{
    CloseHandle, ERROR_ALREADY_EXISTS, GetLastError, HANDLE, WAIT_OBJECT_0,
};
#[cfg(windows)]
use windows::Win32::System::Threading::{
    CreateEventW, CreateMutexW, OpenEventW, SYNCHRONIZATION_ACCESS_RIGHTS, SetEvent,
    WaitForSingleObject,
};

#[cfg(windows)]
const MUTEX_NAME: &str = "Local\\RustReplay_OneVPL_Replay_SingleInstance";
#[cfg(windows)]
const ACTIVATE_EVENT_NAME: &str = "Local\\RustReplay_OneVPL_Replay_ActivateMainWindow";

#[cfg(windows)]
pub struct SingleInstance {
    mutex: HANDLE,
    activate_event: HANDLE,
}

#[cfg(windows)]
impl SingleInstance {
    pub fn acquire() -> Result<Option<Self>, String> {
        let mutex_name = wide_with_nul(MUTEX_NAME);
        let mutex = unsafe { CreateMutexW(None, true, windows::core::PCWSTR(mutex_name.as_ptr())) }
            .map_err(|err| format!("创建单实例 mutex 失败：{}", err.message()))?;
        let already_running = unsafe { GetLastError() } == ERROR_ALREADY_EXISTS;
        if already_running {
            unsafe {
                let _ = CloseHandle(mutex);
            }
            signal_existing_instance();
            return Ok(None);
        }

        let event_name = wide_with_nul(ACTIVATE_EVENT_NAME);
        let activate_event = match unsafe {
            CreateEventW(
                None,
                false,
                false,
                windows::core::PCWSTR(event_name.as_ptr()),
            )
        } {
            Ok(event) => event,
            Err(err) => {
                unsafe {
                    let _ = CloseHandle(mutex);
                }
                return Err(format!("创建主界面唤醒 event 失败：{}", err.message()));
            }
        };

        Ok(Some(Self {
            mutex,
            activate_event,
        }))
    }

    pub fn take_activate_request(&self) -> bool {
        unsafe { WaitForSingleObject(self.activate_event, 0) == WAIT_OBJECT_0 }
    }
}

#[cfg(windows)]
impl Drop for SingleInstance {
    fn drop(&mut self) {
        unsafe {
            let _ = CloseHandle(self.activate_event);
            let _ = CloseHandle(self.mutex);
        }
    }
}

#[cfg(windows)]
fn signal_existing_instance() {
    let name = wide_with_nul(ACTIVATE_EVENT_NAME);
    let event = unsafe {
        OpenEventW(
            SYNCHRONIZATION_ACCESS_RIGHTS(windows::Win32::System::Threading::EVENT_MODIFY_STATE.0),
            false,
            windows::core::PCWSTR(name.as_ptr()),
        )
    };
    let Ok(event) = event else {
        return;
    };
    unsafe {
        let _ = SetEvent(event);
        let _ = CloseHandle(event);
    }
}

#[cfg(windows)]
fn wide_with_nul(text: &str) -> Vec<u16> {
    text.encode_utf16().chain(std::iter::once(0)).collect()
}

#[cfg(not(windows))]
pub struct SingleInstance;

#[cfg(not(windows))]
impl SingleInstance {
    pub fn acquire() -> Result<Option<Self>, String> {
        Ok(Some(Self))
    }

    pub const fn take_activate_request(&self) -> bool {
        false
    }
}
