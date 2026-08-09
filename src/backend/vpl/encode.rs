use super::*;
use std::sync::Arc;

#[derive(Debug)]
pub(super) struct EncodedSurfaceBytes {
    pub(super) encode_status: i32,
    pub(super) sync_status: i32,
    pub(super) timestamp_90k: u64,
    pub(super) frame_type: u16,
    pub(super) bytes: Option<Arc<[u8]>>,
}

#[derive(Default)]
pub(super) struct StageTiming {
    pub(super) calls: u64,
    pub(super) total_ns: u128,
    pub(super) max_ns: u128,
}

impl StageTiming {
    pub(super) fn add(&mut self, duration: std::time::Duration) {
        let ns = duration.as_nanos();
        self.calls += 1;
        self.total_ns += ns;
        self.max_ns = self.max_ns.max(ns);
    }

    pub(super) fn avg_ms(&self) -> f64 {
        if self.calls == 0 {
            0.0
        } else {
            self.total_ns as f64 / self.calls as f64 / 1_000_000.0
        }
    }

    pub(super) fn max_ms(&self) -> f64 {
        self.max_ns as f64 / 1_000_000.0
    }
}

#[derive(Default)]
pub(super) struct RecordPerf {
    pub(super) acquire: StageTiming,
    pub(super) init: StageTiming,
    pub(super) snapshot: StageTiming,
    pub(super) source_fence: StageTiming,
    pub(super) surface: StageTiming,
    pub(super) convert: StageTiming,
    pub(super) copy: StageTiming,
    pub(super) submit: StageTiming,
    pub(super) sync: StageTiming,
    pub(super) release_frame: StageTiming,
    pub(super) frame: StageTiming,
    pub(super) surface_cache_hits: u64,
    pub(super) surface_cache_misses: u64,
    pub(super) dda_accumulated_frames_total: u64,
    pub(super) dda_accumulated_frames_max: u32,
}

impl RecordPerf {
    pub(super) fn summary(&self, captured_frames: u32) -> String {
        let accumulated_denominator = if self.acquire.calls == 0 {
            u64::from(captured_frames)
        } else {
            self.acquire.calls
        };
        let avg_accumulated = if accumulated_denominator == 0 {
            0.0
        } else {
            self.dda_accumulated_frames_total as f64 / accumulated_denominator as f64
        };
        format!(
            concat!(
                "perf(cpu ms avg/max): acquire={:.3}/{:.3}, init={:.3}/{:.3}, ",
                "dda_snapshot={:.3}/{:.3}, source_fence={:.3}/{:.3}, surface+native={:.3}/{:.3}, ",
                "convert={:.3}/{:.3}, copy={:.3}/{:.3}, ",
                "encode_submit={:.3}/{:.3}, encode_sync={:.3}/{:.3}, ",
                "release_frame={:.3}/{:.3}, frame_body={:.3}/{:.3}; ",
                "surface_cache hit/miss={}/{}, dda_accumulated avg/max={:.2}/{}, captured={}"
            ),
            self.acquire.avg_ms(),
            self.acquire.max_ms(),
            self.init.avg_ms(),
            self.init.max_ms(),
            self.snapshot.avg_ms(),
            self.snapshot.max_ms(),
            self.source_fence.avg_ms(),
            self.source_fence.max_ms(),
            self.surface.avg_ms(),
            self.surface.max_ms(),
            self.convert.avg_ms(),
            self.convert.max_ms(),
            self.copy.avg_ms(),
            self.copy.max_ms(),
            self.submit.avg_ms(),
            self.submit.max_ms(),
            self.sync.avg_ms(),
            self.sync.max_ms(),
            self.release_frame.avg_ms(),
            self.release_frame.max_ms(),
            self.frame.avg_ms(),
            self.frame.max_ms(),
            self.surface_cache_hits,
            self.surface_cache_misses,
            avg_accumulated,
            self.dda_accumulated_frames_max,
            captured_frames
        )
    }
}

pub(super) struct AsyncEncode {
    pub(super) bitstream: MfxBitstream,
    pub(super) storage: Vec<u8>,
    pub(super) syncp: MfxSyncPoint,
    pub(super) _ctrl: Option<Box<MfxEncodeCtrl>>,
    pub(super) timestamp_90k: u64,
    pub(super) is_sync: bool,
    pub(super) discard: bool,
}

pub(super) struct AsyncEncodeRequest {
    pub(super) surface: *mut MfxFrameSurface1,
    pub(super) timestamp_90k: u64,
    pub(super) is_sync: bool,
    pub(super) storage: Vec<u8>,
    pub(super) discard: bool,
}

pub(super) enum TrySyncResult {
    Ready(Option<crate::backend::mp4_mux::HevcAccessUnit>),
    NotReady,
}

#[cfg(windows)]
pub(super) unsafe fn cached_vpl_surface_texture(
    surface: *mut MfxFrameSurface1,
    cache: &mut HashMap<usize, windows::Win32::Graphics::Direct3D11::ID3D11Texture2D>,
    cache_enabled: bool,
) -> Result<(windows::Win32::Graphics::Direct3D11::ID3D11Texture2D, bool), BackendError> {
    use windows::Win32::Graphics::Direct3D11::ID3D11Texture2D;
    use windows::core::Interface;

    let key = surface as usize;
    if cache_enabled && let Some(texture) = cache.get(&key) {
        return Ok((texture.clone(), true));
    }
    let frame_interface = (*surface).FrameInterface;
    if frame_interface.is_null() {
        return Err(BackendError::unsupported(
            "oneVPL record",
            "FrameInterface",
            "oneVPL surface 没有 FrameInterface",
        ));
    }
    let mut native: MfxHDL = ptr::null_mut();
    let mut native_type = 0u32;
    let status = ((*frame_interface).GetNativeHandle)(surface, &mut native, &mut native_type);
    if status != MFX_ERR_NONE || native_type != MFX_RESOURCE_DX11_TEXTURE {
        return Err(BackendError::VplStatus {
            func: "mfxFrameSurfaceInterface::GetNativeHandle",
            status,
        });
    }
    let Some(target) = <ID3D11Texture2D as Interface>::from_raw_borrowed(&native) else {
        return Err(BackendError::unsupported(
            "oneVPL record",
            "native texture",
            "GetNativeHandle 返回值不是 ID3D11Texture2D",
        ));
    };
    let texture = target.clone();
    if cache_enabled {
        cache.insert(key, texture.clone());
    }
    Ok((texture, false))
}

pub(super) fn sink_status(
    encoded_sink: &mut Option<&mut dyn VplOneCopyRecordSink>,
    message: impl AsRef<str>,
) {
    if let Some(sink) = encoded_sink.as_deref_mut() {
        sink.status(message.as_ref());
    }
}

#[derive(Debug, Default)]
pub(super) struct RecordHevcStats {
    pub(super) encoded_samples: u32,
    pub(super) encoded_bytes: u64,
    pub(super) discarded_header_units: u32,
    pub(super) last_timestamp_90k: Option<u64>,
    pub(super) last_sync_timestamp_90k: Option<u64>,
}

#[derive(Debug)]
enum PendingPresentationTimestamp {
    Discard,
    Track {
        timestamp_90k: u64,
        presentation_timestamp_100ns: Option<u64>,
    },
}

/// Correlates oneVPL's 90 kHz transport timestamp with the source presentation
/// timestamp. WGC can produce source intervals that cannot be represented at
/// 90 kHz without rounding; this keeps that rounding out of the MP4 timeline.
#[derive(Debug, Default)]
pub(super) struct PresentationTimestampTracker {
    source_timestamp_mode: Option<bool>,
    pending_by_transport_90k: std::collections::BTreeMap<u64, PendingPresentationTimestamp>,
    last_attached_90k: Option<u64>,
    last_attached_100ns: Option<u64>,
    presentation_duration_100ns: Option<u64>,
}

impl PresentationTimestampTracker {
    pub(super) fn remember_discard(
        &mut self,
        transport_timestamp_90k: u64,
    ) -> Result<(), BackendError> {
        if self
            .pending_by_transport_90k
            .insert(
                transport_timestamp_90k,
                PendingPresentationTimestamp::Discard,
            )
            .is_some()
        {
            return Err(BackendError::unsupported(
                "视频 source timestamp",
                format!("transport_timestamp_90k={transport_timestamp_90k}"),
                "编码器 warmup transport timestamp 重复",
            ));
        }
        Ok(())
    }

    pub(super) fn remember(
        &mut self,
        transport_timestamp_90k: u64,
        timestamp_90k: u64,
        absolute_timestamp_100ns: Option<i64>,
        origin_timestamp_100ns: Option<i64>,
    ) -> Result<(), BackendError> {
        let presentation_timestamp_100ns = match (absolute_timestamp_100ns, origin_timestamp_100ns)
        {
            (Some(timestamp), Some(origin)) => {
                if timestamp < origin {
                    return Err(BackendError::unsupported(
                        "视频 source timestamp",
                        format!("timestamp_100ns={timestamp} origin_100ns={origin}"),
                        "源视频时间戳早于正式时间线原点",
                    ));
                }
                if self.source_timestamp_mode == Some(false) {
                    return Err(BackendError::unsupported(
                        "视频 source timestamp",
                        format!(
                            "absolute_100ns={absolute_timestamp_100ns:?} origin_100ns={origin_timestamp_100ns:?}"
                        ),
                        "正式录制期间 source timestamp 可用性发生变化",
                    ));
                }
                self.source_timestamp_mode = Some(true);
                Some(timestamp.saturating_sub(origin) as u64)
            }
            (None, None) => {
                if self.source_timestamp_mode == Some(true) {
                    return Err(BackendError::unsupported(
                        "视频 source timestamp",
                        format!(
                            "absolute_100ns={absolute_timestamp_100ns:?} origin_100ns={origin_timestamp_100ns:?}"
                        ),
                        "正式录制期间 source timestamp 可用性发生变化",
                    ));
                }
                self.source_timestamp_mode = Some(false);
                None
            }
            _ => {
                return Err(BackendError::unsupported(
                    "视频 source timestamp",
                    format!(
                        "absolute_100ns={absolute_timestamp_100ns:?} origin_100ns={origin_timestamp_100ns:?}"
                    ),
                    "正式录制期间 source timestamp 可用性发生变化",
                ));
            }
        };

        if self
            .pending_by_transport_90k
            .insert(
                transport_timestamp_90k,
                PendingPresentationTimestamp::Track {
                    timestamp_90k,
                    presentation_timestamp_100ns,
                },
            )
            .is_some()
        {
            return Err(BackendError::unsupported(
                "视频 source timestamp",
                format!("transport_timestamp_90k={transport_timestamp_90k}"),
                "编码器 transport timestamp 重复，无法唯一对应 source PTS",
            ));
        }
        Ok(())
    }

    pub(super) fn attach(
        &mut self,
        sample: &mut crate::backend::mp4_mux::HevcAccessUnit,
    ) -> Result<(), BackendError> {
        let transport_timestamp_90k = sample.timestamp_90k;
        let pending = self
            .pending_by_transport_90k
            .remove(&transport_timestamp_90k)
            .ok_or_else(|| {
                BackendError::unsupported(
                    "视频 source timestamp",
                    format!("transport_timestamp_90k={transport_timestamp_90k}"),
                    "编码器输出没有对应的提交时间戳",
                )
            })?;

        let PendingPresentationTimestamp::Track {
            timestamp_90k,
            presentation_timestamp_100ns,
        } = pending
        else {
            sample.discard_from_track = true;
            sample.presentation_timestamp_100ns = None;
            return Ok(());
        };

        sample.discard_from_track = false;
        sample.timestamp_90k = timestamp_90k;
        if let Some(previous) = self.last_attached_90k
            && timestamp_90k <= previous
        {
            return Err(BackendError::unsupported(
                "视频 source timestamp",
                format!("previous_90k={previous} current_90k={timestamp_90k}"),
                "编码输出的源 VFR 时间戳非严格递增",
            ));
        }
        self.last_attached_90k = Some(timestamp_90k);

        if let Some(exact) = presentation_timestamp_100ns {
            if let Some(previous) = self.last_attached_100ns
                && exact <= previous
            {
                return Err(BackendError::unsupported(
                    "视频 source timestamp",
                    format!("previous_100ns={previous} current_100ns={exact}"),
                    "编码输出的 source PTS 非严格递增",
                ));
            }
            let tail = self
                .last_attached_100ns
                .map(|previous| exact.saturating_sub(previous).max(1))
                .unwrap_or(1);
            self.presentation_duration_100ns = Some(exact.saturating_add(tail).max(1));
            self.last_attached_100ns = Some(exact);
            sample.presentation_timestamp_100ns = Some(exact);
        } else {
            sample.presentation_timestamp_100ns = None;
        }
        Ok(())
    }

    pub(super) fn presentation_duration_100ns(&self) -> Option<u64> {
        self.presentation_duration_100ns
    }

    fn has_pending_transport_timestamp(&self, transport_timestamp_90k: u64) -> bool {
        self.pending_by_transport_90k
            .contains_key(&transport_timestamp_90k)
    }

    pub(super) fn finish(&self) -> Result<(), BackendError> {
        let pending_track_correlations = self
            .pending_by_transport_90k
            .values()
            .filter(|pending| matches!(pending, PendingPresentationTimestamp::Track { .. }))
            .count();
        if pending_track_correlations > 0 {
            return Err(BackendError::unsupported(
                "视频 source timestamp",
                format!("pending_correlations={pending_track_correlations}"),
                "编码器结束后仍有 source PTS 未对应到输出 AU",
            ));
        }
        Ok(())
    }
}

impl RecordHevcStats {
    fn observe(&mut self, sample: &crate::backend::mp4_mux::HevcAccessUnit) {
        if sample.discard_from_track {
            self.discarded_header_units = self.discarded_header_units.saturating_add(1);
        } else {
            self.encoded_samples = self.encoded_samples.saturating_add(1);
            self.encoded_bytes = self.encoded_bytes.saturating_add(sample.data.len() as u64);
            self.last_timestamp_90k = Some(sample.timestamp_90k);
            if sample.is_sync {
                self.last_sync_timestamp_90k = Some(sample.timestamp_90k);
            }
        }
    }
}

pub(super) fn push_record_hevc_sample(
    samples: &mut Vec<crate::backend::mp4_mux::HevcAccessUnit>,
    sample: crate::backend::mp4_mux::HevcAccessUnit,
    encoded_sink: &mut Option<&mut dyn VplOneCopyRecordSink>,
    retain_sample: bool,
    stats: &mut RecordHevcStats,
) {
    stats.observe(&sample);
    if let Some(sink) = encoded_sink.as_deref_mut() {
        sink.hevc_access_unit(&sample);
    }
    if retain_sample || sample.discard_from_track {
        samples.push(sample);
    }
}

pub(super) fn push_record_hevc_sample_with_timeline(
    samples: &mut Vec<crate::backend::mp4_mux::HevcAccessUnit>,
    mut sample: crate::backend::mp4_mux::HevcAccessUnit,
    encoded_sink: &mut Option<&mut dyn VplOneCopyRecordSink>,
    retain_sample: bool,
    stats: &mut RecordHevcStats,
    timeline: &mut PresentationTimestampTracker,
) -> Result<(), BackendError> {
    timeline.attach(&mut sample)?;
    push_record_hevc_sample(samples, sample, encoded_sink, retain_sample, stats);
    Ok(())
}

pub(super) fn presentation_timeline_duration_100ns(
    samples: &[crate::backend::mp4_mux::HevcAccessUnit],
) -> Option<u64> {
    let mut timestamps = samples
        .iter()
        .filter(|sample| !sample.discard_from_track)
        .map(|sample| sample.presentation_timestamp_100ns);
    let first = timestamps.next()??;
    let mut previous = None;
    let mut last = first;
    for timestamp in timestamps {
        let timestamp = timestamp?;
        previous = Some(last);
        last = timestamp;
    }
    let tail = previous
        .map(|previous| last.saturating_sub(previous).max(1))
        .unwrap_or(1);
    Some(last.saturating_add(tail).max(1))
}

pub(super) fn mfx_frame_type_is_sync(frame_type: u16) -> bool {
    frame_type & (MFX_FRAMETYPE_IDR | MFX_FRAMETYPE_I) != 0
}

pub(super) fn initial_bitstream_capacity_bytes(
    param: &MfxVideoParam,
    route: VplRecordRoute,
) -> usize {
    let width = u128::from(param.mfx.FrameInfo.CropW.max(param.mfx.FrameInfo.Width));
    let height = u128::from(param.mfx.FrameInfo.CropH.max(param.mfx.FrameInfo.Height));
    let component_bytes = if route.bit_depth > 8 { 2u128 } else { 1u128 };
    let raw_bytes = match route.chroma {
        1 => {
            width
                .saturating_mul(height)
                .saturating_mul(3)
                .saturating_mul(component_bytes)
                / 2
        }
        2 => width
            .saturating_mul(height)
            .saturating_mul(2)
            .saturating_mul(component_bytes),
        _ => width.saturating_mul(height).saturating_mul(4),
    };
    let frame_estimate = raw_bytes.div_ceil(2);
    let hrd_bytes = u128::from(param.mfx.BufferSizeInKB)
        .saturating_mul(u128::from(param.mfx.BRCParamMultiplier.max(1)))
        .saturating_mul(1024);
    let requested = frame_estimate
        .max(hrd_bytes)
        .max(VPL_BITSTREAM_MIN_BYTES as u128)
        .min(VPL_BITSTREAM_INITIAL_MAX_BYTES as u128);
    let aligned = requested.div_ceil(64 * 1024) * (64 * 1024);
    aligned.min(usize::MAX as u128) as usize
}

unsafe fn refresh_async_bitstream_storage(flight: &mut AsyncEncode) {
    let usable = flight.storage.len().saturating_sub(31);
    let aligned_offset = (32 - (flight.storage.as_ptr() as usize & 31)) & 31;
    flight.bitstream.Data = flight.storage.as_mut_ptr().add(aligned_offset);
    flight.bitstream.MaxLength = usable.min(u32::MAX as usize) as u32;
    flight.bitstream.DataLength = 0;
    flight.bitstream.DataOffset = 0;
    flight.syncp = ptr::null_mut();
}

pub(super) unsafe fn submit_encode_async(
    api: &VplApi,
    session: MfxSession,
    request: AsyncEncodeRequest,
    bitstream_pool: &mut Vec<Vec<u8>>,
) -> Result<Option<Box<AsyncEncode>>, BackendError> {
    let AsyncEncodeRequest {
        surface,
        timestamp_90k,
        is_sync,
        mut storage,
        discard,
    } = request;
    if storage.len() < VPL_BITSTREAM_MIN_BYTES + 31 {
        storage.resize(VPL_BITSTREAM_MIN_BYTES + 31, 0);
    }
    let mut ctrl = if is_sync {
        Some(Box::new(MfxEncodeCtrl {
            FrameType: MFX_FRAMETYPE_I | MFX_FRAMETYPE_REF | MFX_FRAMETYPE_IDR,
            ..unsafe { std::mem::zeroed() }
        }))
    } else {
        None
    };
    let ctrl_ptr = ctrl
        .as_deref_mut()
        .map(|ctrl| ctrl as *mut MfxEncodeCtrl as *mut c_void)
        .unwrap_or(ptr::null_mut());
    let mut flight = Box::new(AsyncEncode {
        bitstream: MfxBitstream {
            CodecId: MFX_CODEC_HEVC,
            TimeStamp: timestamp_90k,
            ..std::mem::zeroed()
        },
        storage,
        syncp: ptr::null_mut(),
        _ctrl: ctrl,
        timestamp_90k,
        is_sync,
        discard,
    });
    refresh_async_bitstream_storage(&mut flight);

    let status = loop {
        let mut status = (api.mfx_video_encode_frame_async)(
            session,
            ctrl_ptr,
            surface,
            &mut flight.bitstream,
            &mut flight.syncp,
        );
        let mut busy_retries = 0u32;
        while status == MFX_WRN_DEVICE_BUSY && busy_retries < 10 {
            std::thread::sleep(std::time::Duration::from_millis(1));
            refresh_async_bitstream_storage(&mut flight);
            let ctrl_ptr = flight
                ._ctrl
                .as_deref_mut()
                .map(|ctrl| ctrl as *mut MfxEncodeCtrl as *mut c_void)
                .unwrap_or(ptr::null_mut());
            status = (api.mfx_video_encode_frame_async)(
                session,
                ctrl_ptr,
                surface,
                &mut flight.bitstream,
                &mut flight.syncp,
            );
            busy_retries += 1;
        }
        if status != MFX_ERR_NOT_ENOUGH_BUFFER {
            break status;
        }
        let current = flight.storage.len().saturating_sub(31);
        let next = current
            .saturating_mul(2)
            .min(VPL_BITSTREAM_GROWTH_MAX_BYTES);
        if next <= current {
            return Err(BackendError::VplStatus {
                func: "MFXVideoENCODE_EncodeFrameAsync(bitstream growth limit)",
                status,
            });
        }
        flight.storage.resize(next + 31, 0);
        refresh_async_bitstream_storage(&mut flight);
    };

    if matches!(status, MFX_ERR_MORE_DATA | MFX_ERR_MORE_SURFACE) {
        bitstream_pool.push(flight.storage);
        return Ok(None);
    }
    if status < MFX_ERR_NONE {
        return Err(BackendError::VplStatus {
            func: "MFXVideoENCODE_EncodeFrameAsync",
            status,
        });
    }
    if flight.syncp.is_null() {
        bitstream_pool.push(flight.storage);
        return Ok(None);
    }
    Ok(Some(flight))
}

pub(super) unsafe fn sync_one_async_encode(
    api: &VplApi,
    session: MfxSession,
    in_flight: &mut VecDeque<Box<AsyncEncode>>,
    bitstream_pool: &mut Vec<Vec<u8>>,
) -> Result<Option<crate::backend::mp4_mux::HevcAccessUnit>, BackendError> {
    let Some(flight) = in_flight.front() else {
        return Ok(None);
    };
    let sync_status = (api.mfx_video_core_sync_operation)(session, flight.syncp, 60_000);
    if sync_status < MFX_ERR_NONE {
        return Err(BackendError::VplStatus {
            func: "MFXVideoCORE_SyncOperation",
            status: sync_status,
        });
    }
    let flight = in_flight
        .pop_front()
        .expect("front existed before successful sync");
    finish_synced_async_encode(*flight, bitstream_pool)
}

pub(super) unsafe fn try_sync_one_async_encode(
    api: &VplApi,
    session: MfxSession,
    in_flight: &mut VecDeque<Box<AsyncEncode>>,
    bitstream_pool: &mut Vec<Vec<u8>>,
    timeout_ms: u32,
) -> Result<TrySyncResult, BackendError> {
    let Some(front) = in_flight.front() else {
        return Ok(TrySyncResult::Ready(None));
    };
    let sync_status = (api.mfx_video_core_sync_operation)(session, front.syncp, timeout_ms);
    if matches!(sync_status, MFX_WRN_IN_EXECUTION | MFX_WRN_DEVICE_BUSY) {
        return Ok(TrySyncResult::NotReady);
    }
    if sync_status < MFX_ERR_NONE {
        return Err(BackendError::VplStatus {
            func: "MFXVideoCORE_SyncOperation(short)",
            status: sync_status,
        });
    }
    let flight = in_flight
        .pop_front()
        .expect("front existed before successful short sync");
    finish_synced_async_encode(*flight, bitstream_pool).map(TrySyncResult::Ready)
}

pub(super) unsafe fn poll_completed_async_encodes(
    api: &VplApi,
    session: MfxSession,
    in_flight: &mut VecDeque<Box<AsyncEncode>>,
    bitstream_pool: &mut Vec<Vec<u8>>,
    samples: &mut Vec<crate::backend::mp4_mux::HevcAccessUnit>,
) -> Result<(), BackendError> {
    while let Some(front) = in_flight.front() {
        let sync_status = (api.mfx_video_core_sync_operation)(session, front.syncp, 0);
        if matches!(sync_status, MFX_WRN_IN_EXECUTION | MFX_WRN_DEVICE_BUSY) {
            break;
        }
        if sync_status < MFX_ERR_NONE {
            return Err(BackendError::VplStatus {
                func: "MFXVideoCORE_SyncOperation(poll)",
                status: sync_status,
            });
        }
        let flight = in_flight
            .pop_front()
            .expect("front existed before successful poll");
        if let Some(sample) = finish_synced_async_encode(*flight, bitstream_pool)? {
            samples.push(sample);
        }
    }
    Ok(())
}

pub(super) unsafe fn finish_synced_async_encode(
    flight: AsyncEncode,
    bitstream_pool: &mut Vec<Vec<u8>>,
) -> Result<Option<crate::backend::mp4_mux::HevcAccessUnit>, BackendError> {
    let len = flight.bitstream.DataLength as usize;
    if len == 0 {
        bitstream_pool.push(flight.storage);
        return Ok(None);
    }
    let start = flight
        .bitstream
        .Data
        .add(flight.bitstream.DataOffset as usize);
    let data = Arc::<[u8]>::from(std::slice::from_raw_parts(start, len));
    let sync_candidate = flight.is_sync || mfx_frame_type_is_sync(flight.bitstream.FrameType);
    let is_sync =
        sync_candidate && crate::backend::mp4_mux::hevc_annex_b_has_random_access_nal(&data);
    bitstream_pool.push(flight.storage);
    let output_timestamp_90k = if flight.bitstream.TimeStamp == u64::MAX {
        flight.timestamp_90k
    } else {
        flight.bitstream.TimeStamp
    };
    Ok(Some(crate::backend::mp4_mux::HevcAccessUnit {
        timestamp_90k: output_timestamp_90k,
        presentation_timestamp_100ns: None,
        data,
        is_sync,
        discard_from_track: flight.discard,
    }))
}

pub(super) unsafe fn encode_surface_bytes(
    api: &VplApi,
    session: MfxSession,
    surface: *mut MfxFrameSurface1,
) -> Result<EncodedSurfaceBytes, BackendError> {
    let mut storage = vec![0u8; VPL_BITSTREAM_MIN_BYTES + 31];
    encode_surface_or_flush_bytes(api, session, surface, &mut storage)
}

pub(super) unsafe fn encode_surface_or_flush_bytes(
    api: &VplApi,
    session: MfxSession,
    surface: *mut MfxFrameSurface1,
    storage: &mut Vec<u8>,
) -> Result<EncodedSurfaceBytes, BackendError> {
    let mut bitstream: MfxBitstream = std::mem::zeroed();
    bitstream.CodecId = MFX_CODEC_HEVC;
    refresh_bitstream_storage(storage, &mut bitstream);

    let mut syncp: MfxSyncPoint = ptr::null_mut();
    let encode_status = loop {
        let mut status = (api.mfx_video_encode_frame_async)(
            session,
            ptr::null_mut(),
            surface,
            &mut bitstream,
            &mut syncp,
        );
        let mut busy_retries = 0u32;
        while status == MFX_WRN_DEVICE_BUSY && busy_retries < 50 {
            std::thread::sleep(std::time::Duration::from_millis(2));
            syncp = ptr::null_mut();
            refresh_bitstream_storage(storage, &mut bitstream);
            status = (api.mfx_video_encode_frame_async)(
                session,
                ptr::null_mut(),
                surface,
                &mut bitstream,
                &mut syncp,
            );
            busy_retries += 1;
        }
        if status != MFX_ERR_NOT_ENOUGH_BUFFER {
            break status;
        }
        let current = storage.len().saturating_sub(31);
        let next = current
            .saturating_mul(2)
            .min(VPL_BITSTREAM_GROWTH_MAX_BYTES);
        if next <= current {
            return Err(BackendError::VplStatus {
                func: "MFXVideoENCODE_EncodeFrameAsync(flush growth limit)",
                status,
            });
        }
        storage.resize(next + 31, 0);
        syncp = ptr::null_mut();
        refresh_bitstream_storage(storage, &mut bitstream);
    };

    if matches!(encode_status, MFX_ERR_MORE_DATA | MFX_ERR_MORE_SURFACE) {
        return Ok(EncodedSurfaceBytes {
            encode_status,
            sync_status: i32::MIN,
            timestamp_90k: 0,
            frame_type: 0,
            bytes: None,
        });
    }
    if encode_status < MFX_ERR_NONE {
        return Err(BackendError::VplStatus {
            func: "MFXVideoENCODE_EncodeFrameAsync",
            status: encode_status,
        });
    }

    let sync_status = if !syncp.is_null() {
        (api.mfx_video_core_sync_operation)(session, syncp, 60_000)
    } else {
        i32::MIN
    };
    if sync_status < MFX_ERR_NONE && sync_status != i32::MIN {
        return Err(BackendError::VplStatus {
            func: "MFXVideoCORE_SyncOperation",
            status: sync_status,
        });
    }

    let start = bitstream.Data.add(bitstream.DataOffset as usize);
    let len = bitstream.DataLength as usize;
    let bytes = if len == 0 {
        None
    } else {
        Some(Arc::<[u8]>::from(std::slice::from_raw_parts(start, len)))
    };
    Ok(EncodedSurfaceBytes {
        encode_status,
        sync_status,
        timestamp_90k: bitstream.TimeStamp,
        frame_type: bitstream.FrameType,
        bytes,
    })
}

unsafe fn refresh_bitstream_storage(storage: &mut Vec<u8>, bitstream: &mut MfxBitstream) {
    if storage.len() < VPL_BITSTREAM_MIN_BYTES + 31 {
        storage.resize(VPL_BITSTREAM_MIN_BYTES + 31, 0);
    }
    let usable = storage.len().saturating_sub(31);
    let aligned_offset = (32 - (storage.as_ptr() as usize & 31)) & 31;
    bitstream.Data = storage.as_mut_ptr().add(aligned_offset);
    bitstream.MaxLength = usable.min(u32::MAX as usize) as u32;
    bitstream.DataLength = 0;
    bitstream.DataOffset = 0;
}

pub(super) unsafe fn flush_encoder(
    api: &VplApi,
    session: MfxSession,
    samples: &mut Vec<crate::backend::mp4_mux::HevcAccessUnit>,
    encoded_sink: &mut Option<&mut dyn VplOneCopyRecordSink>,
    retain_output_samples: bool,
    stats: &mut RecordHevcStats,
    bitstream_pool: &mut Vec<Vec<u8>>,
) -> Result<u32, BackendError> {
    let mut storage = bitstream_pool
        .pop()
        .unwrap_or_else(|| vec![0u8; VPL_BITSTREAM_MIN_BYTES + 31]);
    let result = (|| -> Result<u32, BackendError> {
        let mut skipped_non_monotonic = 0u32;
        loop {
            let encoded =
                encode_surface_or_flush_bytes(api, session, ptr::null_mut(), &mut storage)?;
            if encoded.encode_status == MFX_ERR_MORE_DATA {
                break;
            }
            if let Some(bytes) = encoded.bytes {
                let last_timestamp = samples.last().map(|sample| sample.timestamp_90k);
                let last_timestamp = stats
                    .last_timestamp_90k
                    .or_else(|| last_track_timestamp_90k(samples))
                    .or(last_timestamp);
                let timestamp_90k = encoded.timestamp_90k;
                if let Some(last) = last_timestamp
                    && timestamp_90k <= last
                {
                    skipped_non_monotonic = skipped_non_monotonic.saturating_add(1);
                    continue;
                }
                let is_sync = mfx_frame_type_is_sync(encoded.frame_type)
                    && crate::backend::mp4_mux::hevc_annex_b_has_random_access_nal(&bytes);
                push_record_hevc_sample(
                    samples,
                    crate::backend::mp4_mux::HevcAccessUnit {
                        timestamp_90k,
                        presentation_timestamp_100ns: None,
                        data: bytes,
                        is_sync,
                        discard_from_track: false,
                    },
                    encoded_sink,
                    retain_output_samples,
                    stats,
                );
            } else {
                break;
            }
        }
        Ok(skipped_non_monotonic)
    })();
    bitstream_pool.push(storage);
    result
}

/// Timeline-aware variant for routes that submit source-derived presentation
/// timestamps. Kept separate from `flush_encoder` so legacy 90 kHz-only
/// callers retain their existing behavior until they opt into the tracker.
pub(super) unsafe fn flush_encoder_with_timeline(
    api: &VplApi,
    session: MfxSession,
    samples: &mut Vec<crate::backend::mp4_mux::HevcAccessUnit>,
    encoded_sink: &mut Option<&mut dyn VplOneCopyRecordSink>,
    retain_output_samples: bool,
    stats: &mut RecordHevcStats,
    presentation_timeline: &mut PresentationTimestampTracker,
    bitstream_pool: &mut Vec<Vec<u8>>,
) -> Result<u32, BackendError> {
    let mut storage = bitstream_pool
        .pop()
        .unwrap_or_else(|| vec![0u8; VPL_BITSTREAM_MIN_BYTES + 31]);
    let result = (|| -> Result<u32, BackendError> {
        let mut skipped_non_monotonic = 0u32;
        loop {
            let encoded =
                encode_surface_or_flush_bytes(api, session, ptr::null_mut(), &mut storage)?;
            if encoded.encode_status == MFX_ERR_MORE_DATA {
                break;
            }
            if let Some(bytes) = encoded.bytes {
                let timestamp_90k = encoded.timestamp_90k;
                if !presentation_timeline.has_pending_transport_timestamp(timestamp_90k) {
                    skipped_non_monotonic = skipped_non_monotonic.saturating_add(1);
                    continue;
                }
                let is_sync = mfx_frame_type_is_sync(encoded.frame_type)
                    && crate::backend::mp4_mux::hevc_annex_b_has_random_access_nal(&bytes);
                push_record_hevc_sample_with_timeline(
                    samples,
                    crate::backend::mp4_mux::HevcAccessUnit {
                        timestamp_90k,
                        presentation_timestamp_100ns: None,
                        data: bytes,
                        is_sync,
                        discard_from_track: false,
                    },
                    encoded_sink,
                    retain_output_samples,
                    stats,
                    presentation_timeline,
                )?;
            } else {
                break;
            }
        }
        Ok(skipped_non_monotonic)
    })();
    bitstream_pool.push(storage);
    result
}

pub(super) fn last_track_timestamp_90k(
    samples: &[crate::backend::mp4_mux::HevcAccessUnit],
) -> Option<u64> {
    samples
        .iter()
        .rev()
        .find(|sample| !sample.discard_from_track)
        .map(|sample| sample.timestamp_90k)
}
