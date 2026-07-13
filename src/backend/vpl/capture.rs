//! Capture-side D3D11 resources, frame slots, and synchronization helpers.

use super::*;

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

    pub(super) fn stop_and_join(&mut self) {
        self.stop.store(true, std::sync::atomic::Ordering::Relaxed);
        if let Some(handle) = self.handle.take() {
            let _ = handle.join();
        }
    }
}

impl Drop for CaptureThreadGuard {
    fn drop(&mut self) {
        self.stop_and_join();
    }
}
