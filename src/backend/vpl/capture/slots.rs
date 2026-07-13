use super::*;

#[cfg(windows)]
#[derive(Clone)]
pub(in super::super) struct SnapshotSlot {
    pub(in super::super) inner: std::sync::Arc<SnapshotSlotInner>,
}

#[cfg(windows)]
impl std::ops::Deref for SnapshotSlot {
    type Target = SnapshotSlotInner;

    fn deref(&self) -> &Self::Target {
        &self.inner
    }
}

#[cfg(windows)]
pub(in super::super) struct SnapshotSlotInner {
    pub(in super::super) id: usize,
    pub(in super::super) capture_texture: windows::Win32::Graphics::Direct3D11::ID3D11Texture2D,
    pub(in super::super) encoder_texture: windows::Win32::Graphics::Direct3D11::ID3D11Texture2D,
    pub(in super::super) capture_mutex: windows::Win32::Graphics::Dxgi::IDXGIKeyedMutex,
    pub(in super::super) encoder_mutex: windows::Win32::Graphics::Dxgi::IDXGIKeyedMutex,
    pub(in super::super) encoder_fence: GpuCompletionFence,
    pub(in super::super) shared_handle: windows::Win32::Foundation::HANDLE,
}

#[cfg(windows)]
unsafe impl Send for SnapshotSlot {}
#[cfg(windows)]
unsafe impl Sync for SnapshotSlot {}
#[cfg(windows)]
unsafe impl Send for SnapshotSlotInner {}
#[cfg(windows)]
unsafe impl Sync for SnapshotSlotInner {}

#[cfg(windows)]
impl Drop for SnapshotSlotInner {
    fn drop(&mut self) {
        unsafe {
            let _ = windows::Win32::Foundation::CloseHandle(self.shared_handle);
        }
    }
}

#[cfg(windows)]
pub(in super::super) struct CapturedSnapshot {
    pub(in super::super) slot: CaptureFrameSlot,
    pub(in super::super) source_desc: windows::Win32::Graphics::Direct3D11::D3D11_TEXTURE2D_DESC,
    pub(in super::super) move_rect_bytes: u32,
    pub(in super::super) dirty_rects: Vec<windows::Win32::Foundation::RECT>,
    pub(in super::super) timestamp_90k: u64,
    pub(in super::super) timestamp_100ns: Option<i64>,
    pub(in super::super) capture_index: u64,
    pub(in super::super) accumulated_frames: u32,
    pub(in super::super) warmup: bool,
}

#[cfg(windows)]
pub(in super::super) struct WgcLocalSlot {
    pub(in super::super) id: usize,
    pub(in super::super) texture: windows::Win32::Graphics::Direct3D11::ID3D11Texture2D,
    pub(in super::super) converter: GpuRecordConverter,
}

#[cfg(windows)]
unsafe impl Send for WgcLocalSlot {}
#[cfg(windows)]
unsafe impl Sync for WgcLocalSlot {}

#[cfg(windows)]
pub(in super::super) enum CaptureFrameSlot {
    Shared(SnapshotSlot),
    WgcLocal(WgcLocalSlot),
}

#[cfg(windows)]
#[derive(Debug, Clone)]
pub(in super::super) struct CaptureStats {
    pub(in super::super) acquired: u64,
    pub(in super::super) copied: u64,
    pub(in super::super) dropped_no_slot: u64,
    pub(in super::super) dropped_queue_full: u64,
    pub(in super::super) dropped_duplicate_timestamp: u64,
    pub(in super::super) dropped_warmup: u64,
    pub(in super::super) dda_timeouts: u64,
    pub(in super::super) accumulated_frames_total: u64,
    pub(in super::super) accumulated_frames_max: u32,
    pub(in super::super) source_interval_count: u64,
    pub(in super::super) source_interval_min_90k: u64,
    pub(in super::super) source_interval_max_90k: u64,
    pub(in super::super) source_interval_above_7_5ms: u64,
    pub(in super::super) source_interval_below_6_5ms: u64,
    pub(in super::super) source_interval_bad_examples: Vec<(u64, u64)>,
    pub(in super::super) callback_frame_count: u64,
    pub(in super::super) callback_frame_cpu_total_us: u64,
    pub(in super::super) callback_frame_cpu_max_us: u64,
    pub(in super::super) callback_copy_cpu_max_us: u64,
    pub(in super::super) wgc_frame_queue_count: u64,
    pub(in super::super) wgc_frame_queue_total_us: u64,
    pub(in super::super) wgc_frame_queue_max_us: u64,
    pub(in super::super) wgc_input_queue_max: u64,
}

#[cfg(windows)]
impl CaptureStats {
    pub(in super::super) fn new() -> Self {
        Self {
            acquired: 0,
            copied: 0,
            dropped_no_slot: 0,
            dropped_queue_full: 0,
            dropped_duplicate_timestamp: 0,
            dropped_warmup: 0,
            dda_timeouts: 0,
            accumulated_frames_total: 0,
            accumulated_frames_max: 0,
            source_interval_count: 0,
            source_interval_min_90k: u64::MAX,
            source_interval_max_90k: 0,
            source_interval_above_7_5ms: 0,
            source_interval_below_6_5ms: 0,
            source_interval_bad_examples: Vec::new(),
            callback_frame_count: 0,
            callback_frame_cpu_total_us: 0,
            callback_frame_cpu_max_us: 0,
            callback_copy_cpu_max_us: 0,
            wgc_frame_queue_count: 0,
            wgc_frame_queue_total_us: 0,
            wgc_frame_queue_max_us: 0,
            wgc_input_queue_max: 0,
        }
    }

    pub(in super::super) fn observe_source_interval(&mut self, frame_index: u64, delta_90k: u64) {
        self.source_interval_count = self.source_interval_count.saturating_add(1);
        self.source_interval_min_90k = self.source_interval_min_90k.min(delta_90k);
        self.source_interval_max_90k = self.source_interval_max_90k.max(delta_90k);
        let above_threshold = (7.5f64 * VIDEO_CLOCK_HZ as f64 / 1000.0).round() as u64;
        let below_threshold = (6.5f64 * VIDEO_CLOCK_HZ as f64 / 1000.0).round() as u64;
        if delta_90k > above_threshold {
            self.source_interval_above_7_5ms = self.source_interval_above_7_5ms.saturating_add(1);
            if self.source_interval_bad_examples.len() < 8 {
                self.source_interval_bad_examples
                    .push((frame_index, delta_90k));
            }
        }
        if delta_90k < below_threshold {
            self.source_interval_below_6_5ms = self.source_interval_below_6_5ms.saturating_add(1);
            if self.source_interval_bad_examples.len() < 8 {
                self.source_interval_bad_examples
                    .push((frame_index, delta_90k));
            }
        }
    }

    pub(in super::super) fn observe_callback_frame_cpu(
        &mut self,
        frame_duration: std::time::Duration,
        copy_duration: std::time::Duration,
    ) {
        let frame_us = frame_duration.as_micros().min(u128::from(u64::MAX)) as u64;
        let copy_us = copy_duration.as_micros().min(u128::from(u64::MAX)) as u64;
        self.callback_frame_count = self.callback_frame_count.saturating_add(1);
        self.callback_frame_cpu_total_us =
            self.callback_frame_cpu_total_us.saturating_add(frame_us);
        self.callback_frame_cpu_max_us = self.callback_frame_cpu_max_us.max(frame_us);
        self.callback_copy_cpu_max_us = self.callback_copy_cpu_max_us.max(copy_us);
    }

    pub(in super::super) fn observe_wgc_frame_queue_delay(&mut self, delay: std::time::Duration) {
        let delay_us = delay.as_micros().min(u128::from(u64::MAX)) as u64;
        self.wgc_frame_queue_count = self.wgc_frame_queue_count.saturating_add(1);
        self.wgc_frame_queue_total_us = self.wgc_frame_queue_total_us.saturating_add(delay_us);
        self.wgc_frame_queue_max_us = self.wgc_frame_queue_max_us.max(delay_us);
    }

    pub(in super::super) fn summary(&self) -> String {
        let avg_accumulated = if self.acquired == 0 {
            0.0
        } else {
            self.accumulated_frames_total as f64 / self.acquired as f64
        };
        let min_interval_ms = if self.source_interval_min_90k == u64::MAX {
            0.0
        } else {
            self.source_interval_min_90k as f64 * 1000.0 / VIDEO_CLOCK_HZ as f64
        };
        let max_interval_ms = self.source_interval_max_90k as f64 * 1000.0 / VIDEO_CLOCK_HZ as f64;
        let bad_examples = self
            .source_interval_bad_examples
            .iter()
            .map(|(index, delta)| {
                format!(
                    "{}:{:.3}ms",
                    index,
                    *delta as f64 * 1000.0 / VIDEO_CLOCK_HZ as f64
                )
            })
            .collect::<Vec<_>>()
            .join(",");
        let callback_avg_ms = if self.callback_frame_count == 0 {
            0.0
        } else {
            self.callback_frame_cpu_total_us as f64 / self.callback_frame_count as f64 / 1000.0
        };
        let wgc_queue_avg_ms = if self.wgc_frame_queue_count == 0 {
            0.0
        } else {
            self.wgc_frame_queue_total_us as f64 / self.wgc_frame_queue_count as f64 / 1000.0
        };
        format!(
            "capture-thread: acquired={}, copied={}, dropped_no_slot={}, dropped_queue_full={}, dropped_duplicate_timestamp={}, dropped_warmup={}, dda_timeouts={}, dda_accumulated avg/max={:.2}/{}, source_interval count={} min/max={:.3}/{:.3}ms above7.5={} below6.5={} bad_examples=[{}], callback_cpu avg/max={:.3}/{:.3}ms copy_max={:.3}ms, wgc_frame_queue avg/max={:.3}/{:.3}ms input_queue_max={}",
            self.acquired,
            self.copied,
            self.dropped_no_slot,
            self.dropped_queue_full,
            self.dropped_duplicate_timestamp,
            self.dropped_warmup,
            self.dda_timeouts,
            avg_accumulated,
            self.accumulated_frames_max,
            self.source_interval_count,
            min_interval_ms,
            max_interval_ms,
            self.source_interval_above_7_5ms,
            self.source_interval_below_6_5ms,
            bad_examples,
            callback_avg_ms,
            self.callback_frame_cpu_max_us as f64 / 1000.0,
            self.callback_copy_cpu_max_us as f64 / 1000.0,
            wgc_queue_avg_ms,
            self.wgc_frame_queue_max_us as f64 / 1000.0,
            self.wgc_input_queue_max,
        )
    }
}

#[cfg(windows)]
pub(in super::super) fn snapshot_slot_matches(
    slot: &SnapshotSlot,
    desc: &windows::Win32::Graphics::Direct3D11::D3D11_TEXTURE2D_DESC,
) -> bool {
    let mut slot_desc = windows::Win32::Graphics::Direct3D11::D3D11_TEXTURE2D_DESC::default();
    unsafe {
        slot.capture_texture.GetDesc(&mut slot_desc);
    }
    slot_desc.Width == desc.Width
        && slot_desc.Height == desc.Height
        && slot_desc.Format.0 == desc.Format.0
}

#[cfg(windows)]
pub(in super::super) fn wgc_local_slot_matches(
    slot: &WgcLocalSlot,
    desc: &windows::Win32::Graphics::Direct3D11::D3D11_TEXTURE2D_DESC,
) -> bool {
    let mut slot_desc = windows::Win32::Graphics::Direct3D11::D3D11_TEXTURE2D_DESC::default();
    unsafe {
        slot.texture.GetDesc(&mut slot_desc);
    }
    slot_desc.Width == desc.Width
        && slot_desc.Height == desc.Height
        && slot_desc.Format.0 == desc.Format.0
}

#[cfg(windows)]
#[allow(clippy::too_many_arguments)]
pub(in super::super) unsafe fn create_wgc_local_slot(
    id: usize,
    device: &windows::Win32::Graphics::Direct3D11::ID3D11Device,
    context: &windows::Win32::Graphics::Direct3D11::ID3D11DeviceContext,
    source_desc: &windows::Win32::Graphics::Direct3D11::D3D11_TEXTURE2D_DESC,
    route: VplRecordRoute,
    input_width: u32,
    input_height: u32,
    source_srv_cache: std::sync::Arc<std::sync::Mutex<ShaderResourceViewCache>>,
) -> Result<WgcLocalSlot, BackendError> {
    let texture = create_route_intermediate(device, source_desc, route, true)?;
    let converter = GpuRecordConverter::new_with_source_cache(
        route,
        device,
        context,
        &texture,
        input_width,
        input_height,
        true,
        source_srv_cache,
    )?;
    Ok(WgcLocalSlot {
        id,
        texture,
        converter,
    })
}
