use super::*;

pub(super) unsafe fn release_pending_surface(surface: Option<*mut MfxFrameSurface1>) {
    if let Some(surface) = surface
        && !surface.is_null()
        && !(*surface).FrameInterface.is_null()
    {
        let _ = ((*(*surface).FrameInterface).Release)(surface);
    }
}

#[cfg(windows)]
pub(super) struct RecordThreadPriorityGuard {
    pub(super) handle: windows::Win32::Foundation::HANDLE,
    pub(super) previous: i32,
}

#[cfg(windows)]
impl RecordThreadPriorityGuard {
    pub(super) unsafe fn raise(notes: &mut Vec<String>) -> Option<Self> {
        use windows::Win32::System::Threading::{
            GetCurrentThread, GetThreadPriority, SetThreadPriority, THREAD_PRIORITY_HIGHEST,
        };

        let handle = GetCurrentThread();
        let previous = GetThreadPriority(handle);
        match SetThreadPriority(handle, THREAD_PRIORITY_HIGHEST) {
            Ok(()) => {
                notes.push(
                    "录制线程临时提升到 THREAD_PRIORITY_HIGHEST 以降低 DDA 高刷新采集抖动"
                        .to_owned(),
                );
                Some(Self { handle, previous })
            }
            Err(err) => {
                notes.push(format!("录制线程提权失败，继续使用当前优先级：{}", err));
                None
            }
        }
    }

    pub(super) unsafe fn raise_capture_thread() -> Option<Self> {
        use windows::Win32::System::Threading::{
            GetCurrentThread, GetThreadPriority, SetThreadPriority, THREAD_PRIORITY_TIME_CRITICAL,
        };

        let handle = GetCurrentThread();
        let previous = GetThreadPriority(handle);
        SetThreadPriority(handle, THREAD_PRIORITY_TIME_CRITICAL)
            .ok()
            .map(|()| Self { handle, previous })
    }
}

#[cfg(windows)]
impl Drop for RecordThreadPriorityGuard {
    fn drop(&mut self) {
        unsafe {
            let _ = windows::Win32::System::Threading::SetThreadPriority(
                self.handle,
                windows::Win32::System::Threading::THREAD_PRIORITY(self.previous),
            );
        }
    }
}

pub(super) fn duration_to_90k(duration: std::time::Duration) -> u64 {
    (duration.as_secs_f64() * VIDEO_CLOCK_HZ as f64).round() as u64
}

#[cfg(windows)]
pub(super) fn query_performance_frequency() -> Option<i64> {
    let mut frequency = 0i64;
    unsafe {
        windows::Win32::System::Performance::QueryPerformanceFrequency(&mut frequency)
            .ok()
            .filter(|_| frequency > 0)
            .map(|_| frequency)
    }
}

#[cfg(windows)]
pub(super) fn dda_timestamp_90k(
    last_present_time_qpc: i64,
    qpc_frequency: i64,
    fallback_start: std::time::Instant,
) -> u64 {
    if last_present_time_qpc > 0 && qpc_frequency > 0 {
        qpc_delta_to_90k(last_present_time_qpc, qpc_frequency)
    } else {
        duration_to_90k(std::time::Instant::now().saturating_duration_since(fallback_start))
    }
}

#[cfg(windows)]
pub(super) fn dda_relative_timestamp_90k(
    last_present_time_qpc: i64,
    qpc_frequency: i64,
    fallback_start: std::time::Instant,
    origin_qpc: &mut Option<i64>,
    last_timestamp_90k: &mut Option<u64>,
) -> u64 {
    let mut timestamp = if last_present_time_qpc > 0 && qpc_frequency > 0 {
        let origin = *origin_qpc.get_or_insert(last_present_time_qpc);
        qpc_delta_to_90k(last_present_time_qpc.saturating_sub(origin), qpc_frequency)
    } else {
        duration_to_90k(std::time::Instant::now().saturating_duration_since(fallback_start))
    };
    if let Some(last) = *last_timestamp_90k
        && timestamp <= last
    {
        timestamp = last.saturating_add(1);
    }
    *last_timestamp_90k = Some(timestamp);
    timestamp
}

#[cfg(windows)]
pub(super) fn qpc_delta_to_90k(delta_qpc: i64, qpc_frequency: i64) -> u64 {
    ((delta_qpc.max(0) as i128 * VIDEO_CLOCK_HZ as i128 + (qpc_frequency as i128 / 2))
        / qpc_frequency as i128) as u64
}

#[cfg(windows)]
pub(super) fn qpc_counter_to_100ns(qpc_value: i64, qpc_frequency: i64) -> Option<i64> {
    if qpc_value <= 0 || qpc_frequency <= 0 {
        return None;
    }
    Some(
        ((qpc_value as i128 * 10_000_000i128 + (qpc_frequency as i128 / 2)) / qpc_frequency as i128)
            as i64,
    )
}

#[cfg(windows)]
pub(super) fn video_90k_to_100ns(duration_90k: u64) -> i64 {
    ((duration_90k as u128 * 10_000_000u128).div_ceil(VIDEO_CLOCK_HZ as u128)).min(i64::MAX as u128)
        as i64
}

#[cfg(windows)]
pub(super) fn video_90k_to_audio_ticks(duration_90k: u64) -> u64 {
    ((duration_90k as u128 * crate::backend::audio::TARGET_SAMPLE_RATE as u128)
        .div_ceil(VIDEO_CLOCK_HZ as u128))
    .min(u64::MAX as u128) as u64
}

#[cfg(windows)]
pub(super) fn audio_100ns_to_ticks(duration_100ns: i64) -> u64 {
    if duration_100ns <= 0 {
        return 0;
    }
    ((duration_100ns as i128 * crate::backend::audio::TARGET_SAMPLE_RATE as i128 + 5_000_000i128)
        / 10_000_000i128)
        .max(0) as u64
}

#[cfg(windows)]
pub(super) fn audio_ticks_to_100ns(ticks: u64) -> i64 {
    ((ticks as u128 * 10_000_000u128).div_ceil(crate::backend::audio::TARGET_SAMPLE_RATE as u128))
        .min(i64::MAX as u128) as i64
}

#[cfg(windows)]
pub(super) fn wgc_relative_timestamp_90k(
    timestamp_100ns: i64,
    origin_100ns: &mut Option<i64>,
    last_timestamp_90k: &mut Option<u64>,
) -> u64 {
    // WGC is intentionally VFR.  This conversion is the only timestamp
    // transform for accepted WGC frames: SystemRelativeTime is made relative to
    // the first accepted WGC source frame and scaled to the 90 kHz MP4/video
    // timebase.  Do not add an external CFR clock here; source gaps must
    // remain visible as longer sample durations.
    let origin = *origin_100ns.get_or_insert(timestamp_100ns);
    let timestamp = wgc_timestamp_from_origin_90k(timestamp_100ns, origin);
    *last_timestamp_90k = Some(timestamp);
    timestamp
}

pub(super) fn wgc_timestamp_from_origin_90k(timestamp_100ns: i64, origin_100ns: i64) -> u64 {
    let delta_100ns = timestamp_100ns.saturating_sub(origin_100ns).max(0) as i128;
    (((delta_100ns * VIDEO_CLOCK_HZ as i128) + 5_000_000i128) / 10_000_000i128).max(0) as u64
}

/// Map a WGC source timestamp to the oneVPL transport clock without allowing
/// two distinct source frames to reuse the same 90 kHz key. The second return
/// value is the transport-only compensation; the caller must retain the
/// original 100 ns timestamp for presentation timing.
pub(super) fn quantize_wgc_timestamp_90k(
    timestamp_100ns: i64,
    origin_100ns: i64,
    previous_90k: Option<u64>,
) -> (u64, u64) {
    let mapped = wgc_timestamp_from_origin_90k(timestamp_100ns, origin_100ns);
    let timestamp = previous_90k
        .map(|previous| mapped.max(previous.saturating_add(1)))
        .unwrap_or(mapped);
    (timestamp, timestamp.saturating_sub(mapped))
}

pub(super) fn encoded_timeline_duration_90k(
    samples: &[crate::backend::mp4_mux::HevcAccessUnit],
    last_timestamp_90k: Option<u64>,
    fallback_90k: u64,
    extend_to_fallback: bool,
) -> u64 {
    // DDA historically extends the final sample to the requested recording
    // duration to keep a strict whole-duration timeline. WGC must not do that:
    // its VFR timeline is defined by accepted WGC SystemRelativeTime samples,
    // so an early stop should produce a shorter source-derived track instead
    // of stretching the last sample to an external wall-clock/CFR target.
    let source_end_90k = last_timestamp_90k
        .or_else(|| {
            samples
                .iter()
                .rev()
                .find(|sample| !sample.discard_from_track)
                .map(|sample| sample.timestamp_90k)
        })
        .map(|timestamp| timestamp.saturating_add(1))
        .unwrap_or(fallback_90k);
    if extend_to_fallback {
        source_end_90k.max(fallback_90k).max(1)
    } else {
        source_end_90k.max(1)
    }
}

pub(super) const fn align16(value: u16) -> u16 {
    value.div_ceil(16) * 16
}
