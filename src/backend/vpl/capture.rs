//! Capture-side D3D11 resources, frame slots, and synchronization helpers.

use super::*;

#[cfg(windows)]
pub(super) enum CaptureFailure {
    Fatal(String),
    Reconfigure(String),
}

mod d3d11;
mod resources;
mod slots;

pub(super) use d3d11::*;
pub(super) use resources::*;
pub(super) use slots::*;

pub(super) struct CaptureThreadGuard {
    stop: std::sync::Arc<std::sync::atomic::AtomicBool>,
    handle: Option<std::thread::JoinHandle<()>>,
}

impl CaptureThreadGuard {
    pub(super) fn new(
        stop: std::sync::Arc<std::sync::atomic::AtomicBool>,
        handle: std::thread::JoinHandle<()>,
    ) -> Self {
        Self {
            stop,
            handle: Some(handle),
        }
    }

    pub(super) fn stop_and_join(&mut self) -> Result<(), BackendError> {
        self.stop.store(true, std::sync::atomic::Ordering::Relaxed);
        if let Some(handle) = self.handle.take() {
            handle.join().map_err(|payload| BackendError::WindowsApi {
                func: "capture thread join",
                message: panic_payload_message(payload),
            })?;
        }
        Ok(())
    }
}

impl Drop for CaptureThreadGuard {
    fn drop(&mut self) {
        let _ = self.stop_and_join();
    }
}

fn panic_payload_message(payload: Box<dyn std::any::Any + Send + 'static>) -> String {
    if let Some(message) = payload.downcast_ref::<&str>() {
        (*message).to_owned()
    } else if let Some(message) = payload.downcast_ref::<String>() {
        message.clone()
    } else {
        "capture thread panicked with a non-string payload".to_owned()
    }
}
