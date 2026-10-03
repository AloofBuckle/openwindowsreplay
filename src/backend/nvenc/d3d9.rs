#![deny(unsafe_op_in_unsafe_fn)]

use super::*;
use std::marker::PhantomData;
use std::rc::Rc;
use std::time::{Duration, Instant};
use windows::Win32::Foundation::{CloseHandle, HANDLE, WAIT_OBJECT_0, WAIT_TIMEOUT};
use windows::Win32::Graphics::Direct3D9::{
    D3DFMT_A2B10G10R10, D3DSURFACE_DESC, IDirect3DDevice9Ex, IDirect3DSurface9,
};
use windows::Win32::System::Threading::{CreateEventW, WaitForSingleObject};
use windows::core::Interface;

const NVFBC_SURFACE_COUNT: usize = 3;

#[derive(Debug, Clone)]
pub(crate) struct NvencD3d9Caps {
    pub(crate) hevc: bool,
    pub(crate) abgr10_input: bool,
    pub(crate) main10_profile: bool,
    pub(crate) frext_profile: bool,
    pub(crate) ten_bit: bool,
    pub(crate) yuv422: bool,
    pub(crate) yuv444: bool,
    pub(crate) async_encode: bool,
    pub(crate) max_width: u32,
    pub(crate) max_height: u32,
    pub(crate) rate_controls: Vec<RateControlMethod>,
    pub(crate) presets: Vec<NvencPreset>,
    pub(crate) encoder_engines: u32,
    pub(crate) warnings: Vec<String>,
}

impl NvencD3d9Caps {
    pub(crate) fn supports(&self, chroma: ChromaSampling, width: u32, height: u32) -> bool {
        if !self.hevc
            || !self.abgr10_input
            || !self.ten_bit
            || width > self.max_width
            || height > self.max_height
        {
            return false;
        }
        match chroma {
            ChromaSampling::Yuv420 => self.main10_profile,
            ChromaSampling::Yuv422 => self.frext_profile && self.yuv422,
            ChromaSampling::Yuv444 => self.frext_profile && self.yuv444,
        }
    }
}

struct NvencD3d9Session {
    encoder: *mut c_void,
    destroy: Option<unsafe extern "system" fn(*mut c_void) -> i32>,
    _device: IDirect3DDevice9Ex,
}

impl NvencD3d9Session {
    fn encoder(&self) -> *mut c_void {
        self.encoder
    }

    fn destroy_now(&mut self) -> i32 {
        if self.encoder.is_null() {
            return NV_ENC_SUCCESS;
        }
        let status = self
            .destroy
            .map(|destroy| unsafe { destroy(self.encoder) })
            .unwrap_or(NV_ENC_SUCCESS);
        if status == NV_ENC_SUCCESS {
            self.encoder = ptr::null_mut();
        }
        status
    }
}

impl Drop for NvencD3d9Session {
    fn drop(&mut self) {
        let _ = self.destroy_now();
    }
}

struct NvencAsyncEvent {
    encoder: *mut c_void,
    handle: HANDLE,
    unregister: Option<unsafe extern "system" fn(*mut c_void, *mut NvEncEventParams) -> i32>,
}

impl NvencAsyncEvent {
    fn register(api: &NvencApi, encoder: *mut c_void) -> Result<Self, BackendError> {
        let register = api
            .functions
            .nvEncRegisterAsyncEvent
            .ok_or_else(|| nvenc_missing("NvEncRegisterAsyncEvent"))?;
        let unregister = api
            .functions
            .nvEncUnregisterAsyncEvent
            .ok_or_else(|| nvenc_missing("NvEncUnregisterAsyncEvent"))?;
        let handle = unsafe { CreateEventW(None, false, false, None) }.map_err(|err| {
            BackendError::WindowsApi {
                func: "CreateEventW(NVENC completion)",
                message: err.to_string(),
            }
        })?;
        let mut params: NvEncEventParams = unsafe { std::mem::zeroed() };
        params.version = NV_ENC_EVENT_PARAMS_VER;
        params.completionEvent = handle.0;
        let status = unsafe { register(encoder, &mut params) };
        if status != NV_ENC_SUCCESS {
            unsafe {
                let _ = CloseHandle(handle);
            }
            nvenc_check("NvEncRegisterAsyncEvent(NvFBC D3D9Ex)", status)?;
        }
        Ok(Self {
            encoder,
            handle,
            unregister: Some(unregister),
        })
    }

    fn raw(&self) -> *mut c_void {
        self.handle.0
    }

    fn wait(&self, timeout: Duration) -> Result<bool, BackendError> {
        let timeout_ms = timeout.as_millis().min(u128::from(u32::MAX)) as u32;
        let status = unsafe { WaitForSingleObject(self.handle, timeout_ms) };
        if status == WAIT_OBJECT_0 {
            Ok(true)
        } else if status == WAIT_TIMEOUT {
            Ok(false)
        } else {
            Err(BackendError::WindowsApi {
                func: "WaitForSingleObject(NVENC completion)",
                message: format!("status={}", status.0),
            })
        }
    }

    fn unregister_now(&mut self) -> i32 {
        let Some(unregister) = self.unregister else {
            return NV_ENC_SUCCESS;
        };
        let mut params: NvEncEventParams = unsafe { std::mem::zeroed() };
        params.version = NV_ENC_EVENT_PARAMS_VER;
        params.completionEvent = self.handle.0;
        let status = unsafe { unregister(self.encoder, &mut params) };
        if status == NV_ENC_SUCCESS {
            self.unregister = None;
        }
        status
    }

    fn abandon_registration(&mut self) {
        self.encoder = ptr::null_mut();
        self.unregister = None;
    }
}

impl Drop for NvencAsyncEvent {
    fn drop(&mut self) {
        let _ = self.unregister_now();
        if !self.handle.is_invalid() {
            unsafe {
                let _ = CloseHandle(self.handle);
            }
            self.handle = HANDLE::default();
        }
    }
}

struct PendingFrame {
    timestamp_90k: u64,
    discard_from_track: bool,
    expect_sync: bool,
    submitted_at: Instant,
}

struct EncodeSlot {
    // Drop order is intentional: an input must be unmapped and unregistered
    // before its bitstream and backing D3D9 surface are released.
    mapped: Option<NvencMappedInputResource>,
    registered: NvencRegisteredResource,
    bitstream: NvencBitstreamBuffer,
    completion: Option<NvencAsyncEvent>,
    completion_seen: bool,
    _surface: IDirect3DSurface9,
    pending: Option<PendingFrame>,
}

#[derive(Debug, Clone)]
pub(crate) struct NvencD3d9RuntimeStats {
    pub(crate) async_encode: bool,
    pub(crate) async_fallback: Option<String>,
    pub(crate) completion_waits: u64,
    pub(crate) completion_wait_us: u64,
    pub(crate) completion_wait_max_us: u64,
    pub(crate) completed_frames: u64,
    pub(crate) completion_latency_us: u64,
    pub(crate) completion_latency_max_us: u64,
}

pub(crate) struct NvencD3d9Encoder {
    // Slots are released before the session, and the session before the DLL.
    slots: Vec<EncodeSlot>,
    eos_completion: Option<NvencAsyncEvent>,
    pending_order: VecDeque<usize>,
    session: Option<NvencD3d9Session>,
    api: Option<NvencApi>,
    width: u32,
    height: u32,
    chroma: ChromaSampling,
    expected_color: NclxColorMetadata,
    vui_verified: bool,
    frame_index: u32,
    eos_submitted: bool,
    async_encode: bool,
    async_fallback: Option<String>,
    completion_waits: u64,
    completion_wait_us: u64,
    completion_wait_max_us: u64,
    completed_frames: u64,
    completion_latency_us: u64,
    completion_latency_max_us: u64,
    _not_send_sync: PhantomData<Rc<()>>,
}

impl NvencD3d9Encoder {
    pub(crate) fn probe(device: &IDirect3DDevice9Ex) -> Result<NvencD3d9Caps, BackendError> {
        let (api, _) = NvencApi::load()
            .map_err(|err| BackendError::unsupported("NVENC", "nvEncodeAPI64.dll", err))?;
        let session = open_d3d9_session(&api, device)?;
        query_d3d9_caps(&api, session.encoder())
    }

    #[allow(clippy::too_many_arguments)]
    pub(crate) fn open(
        device: &IDirect3DDevice9Ex,
        surfaces: &[IDirect3DSurface9],
        width: u32,
        height: u32,
        chroma: ChromaSampling,
        color: NclxColorMetadata,
        rate_control: &RateControlConfig,
        frame_rate_num: u32,
        frame_rate_den: u32,
    ) -> Result<Self, BackendError> {
        if surfaces.len() != NVFBC_SURFACE_COUNT {
            return Err(BackendError::unsupported(
                "NvFBC NVENC surface pool",
                format!("surface_count={}", surfaces.len()),
                "NvFBC 直连路线固定使用三个 D3D9Ex surface",
            ));
        }
        if rate_control.look_ahead_depth != 0 {
            return Err(BackendError::unsupported(
                "NvFBC NVENC Lookahead",
                format!("LookAheadDepth={}", rate_control.look_ahead_depth),
                "NvFBC 最多提供三个直连 surface；该路线明确禁用 Lookahead",
            ));
        }

        let (api, _) = NvencApi::load()
            .map_err(|err| BackendError::unsupported("NVENC", "nvEncodeAPI64.dll", err))?;
        let mut session = open_d3d9_session(&api, device)?;
        let caps = query_d3d9_caps(&api, session.encoder())?;
        if !caps.supports(chroma, width, height) {
            return Err(BackendError::unsupported(
                "NvFBC NVENC route",
                format!(
                    "{}x{} HDR PQ {} 10-bit",
                    width,
                    height,
                    chroma_label(chroma)
                ),
                "当前 D3D9 NVENC session 未报告完整 ABGR10/profile/chroma 能力",
            ));
        }
        if !caps.rate_controls.contains(&rate_control.method) {
            return Err(BackendError::unsupported(
                "NvFBC NVENC RateControl",
                rate_control.method.short_name(),
                "当前 D3D9 NVENC session 未报告该码控模式",
            ));
        }
        if !caps.presets.contains(&rate_control.nvenc_preset) {
            return Err(BackendError::unsupported(
                "NvFBC NVENC preset",
                rate_control.nvenc_preset.raw_name(),
                "当前 D3D9 NVENC session 未枚举到该 preset",
            ));
        }
        for surface in surfaces {
            validate_surface(surface, width, height)?;
        }

        let route = route_spec(chroma);
        let async_requested = caps.async_encode
            && api.functions.nvEncRegisterAsyncEvent.is_some()
            && api.functions.nvEncUnregisterAsyncEvent.is_some();
        let mut async_encode = false;
        let mut async_fallback = None;
        let mut completion_events = Vec::new();
        if async_requested {
            let initialize_result = unsafe {
                initialize_low_latency_hevc_encoder_for_route(
                    &api,
                    session.encoder(),
                    width,
                    height,
                    route,
                    color,
                    rate_control,
                    frame_rate_num,
                    frame_rate_den,
                    true,
                    "NvEncInitializeEncoder(HEVC NvFBC D3D9Ex async)",
                )
            };
            match initialize_result {
                Ok(()) => {
                    let registration = (0..=surfaces.len())
                        .map(|_| NvencAsyncEvent::register(&api, session.encoder()))
                        .collect::<Result<Vec<_>, _>>();
                    match registration {
                        Ok(events) => {
                            completion_events = events;
                            async_encode = true;
                        }
                        Err(err) => {
                            async_fallback = Some(format!("异步完成事件注册失败：{err}"));
                            drop(session);
                            session = open_d3d9_session(&api, device)?;
                            unsafe {
                                initialize_low_latency_hevc_encoder_for_route(
                                    &api,
                                    session.encoder(),
                                    width,
                                    height,
                                    route,
                                    color,
                                    rate_control,
                                    frame_rate_num,
                                    frame_rate_den,
                                    false,
                                    "NvEncInitializeEncoder(HEVC NvFBC D3D9Ex sync fallback)",
                                )?;
                            }
                        }
                    }
                }
                Err(err) => {
                    async_fallback = Some(format!("异步 NVENC 初始化失败：{err}"));
                    drop(session);
                    session = open_d3d9_session(&api, device)?;
                    unsafe {
                        initialize_low_latency_hevc_encoder_for_route(
                            &api,
                            session.encoder(),
                            width,
                            height,
                            route,
                            color,
                            rate_control,
                            frame_rate_num,
                            frame_rate_den,
                            false,
                            "NvEncInitializeEncoder(HEVC NvFBC D3D9Ex sync fallback)",
                        )?;
                    }
                }
            }
        } else {
            unsafe {
                initialize_low_latency_hevc_encoder_for_route(
                    &api,
                    session.encoder(),
                    width,
                    height,
                    route,
                    color,
                    rate_control,
                    frame_rate_num,
                    frame_rate_den,
                    false,
                    "NvEncInitializeEncoder(HEVC NvFBC D3D9Ex sync)",
                )?;
            }
            if caps.async_encode {
                async_fallback = Some("驱动未返回完整异步事件函数入口".to_owned());
            }
        }

        let mut slots = Vec::with_capacity(surfaces.len());
        let mut completion_events = completion_events.into_iter();
        for surface in surfaces {
            let registered =
                register_d3d9_surface(&api, session.encoder(), surface, width, height)?;
            let bitstream = unsafe { create_bitstream_buffer(&api, session.encoder())? };
            slots.push(EncodeSlot {
                mapped: None,
                registered,
                bitstream,
                completion: completion_events.next(),
                completion_seen: false,
                _surface: surface.clone(),
                pending: None,
            });
        }
        let eos_completion = completion_events.next();

        Ok(Self {
            slots,
            eos_completion,
            pending_order: VecDeque::with_capacity(NVFBC_SURFACE_COUNT),
            session: Some(session),
            api: Some(api),
            width,
            height,
            chroma,
            expected_color: color,
            vui_verified: false,
            frame_index: 0,
            eos_submitted: false,
            async_encode,
            async_fallback,
            completion_waits: 0,
            completion_wait_us: 0,
            completion_wait_max_us: 0,
            completed_frames: 0,
            completion_latency_us: 0,
            completion_latency_max_us: 0,
            _not_send_sync: PhantomData,
        })
    }

    pub(crate) fn next_surface_index(&self) -> Result<usize, BackendError> {
        let index = self.frame_index as usize % self.slots.len();
        if self.slots[index].pending.is_some() {
            return Err(BackendError::unsupported(
                "NvFBC NVENC surface scheduling",
                format!("slot={index}"),
                "NvFBC 即将覆盖仍在编码的 surface",
            ));
        }
        Ok(index)
    }

    pub(crate) fn submit_surface(
        &mut self,
        slot_index: usize,
        timestamp_90k: u64,
        force_idr: bool,
        discard_from_track: bool,
    ) -> Result<Vec<HevcAccessUnit>, BackendError> {
        if self.eos_submitted {
            return Err(BackendError::unsupported(
                "NvFBC NVENC encode",
                chroma_label(self.chroma),
                "EOS 已提交，不能继续编码",
            ));
        }
        if slot_index >= self.slots.len() || self.slots[slot_index].pending.is_some() {
            return Err(BackendError::unsupported(
                "NvFBC NVENC encode",
                format!("slot={slot_index}"),
                "无效或仍在使用的 D3D9Ex surface slot",
            ));
        }

        let api = self.api.as_ref().expect("NVENC API exists until shutdown");
        let encoder = self
            .session
            .as_ref()
            .expect("NVENC session exists until shutdown")
            .encoder();
        let slot = &mut self.slots[slot_index];
        let mapped = unsafe { map_input_resource(api, encoder, &slot.registered)? };
        let completion_event = slot
            .completion
            .as_ref()
            .map(NvencAsyncEvent::raw)
            .unwrap_or(ptr::null_mut());
        let expect_sync = force_idr || self.frame_index == 0;
        let status = submit_d3d9_picture(
            api,
            encoder,
            &mapped,
            &slot.bitstream,
            self.width,
            self.height,
            self.frame_index,
            timestamp_90k,
            expect_sync,
            completion_event,
        );
        if let Err(err) = status {
            drop(mapped);
            return Err(err);
        }
        slot.mapped = Some(mapped);
        slot.completion_seen = false;
        slot.pending = Some(PendingFrame {
            timestamp_90k,
            discard_from_track,
            expect_sync,
            submitted_at: Instant::now(),
        });
        self.pending_order.push_back(slot_index);
        self.frame_index = self.frame_index.wrapping_add(1);

        let mut output = Vec::new();
        if self.async_encode {
            while let Some(sample) = self.drain_one(false)? {
                output.push(sample);
            }
        }
        if self.pending_order.len() >= self.slots.len() {
            output.push(self.drain_one(true)?.ok_or_else(|| {
                BackendError::unsupported(
                    "NvFBC NVENC drain",
                    "blocking completion wait",
                    "等待完成事件后仍没有可读取的 access unit",
                )
            })?);
        }
        Ok(output)
    }

    pub(crate) fn flush(&mut self) -> Result<Vec<HevcAccessUnit>, BackendError> {
        let api = self.api.as_ref().expect("NVENC API exists until shutdown");
        let encoder = self
            .session
            .as_ref()
            .expect("NVENC session exists until shutdown")
            .encoder();
        if !self.eos_submitted {
            let completion_event = self
                .eos_completion
                .as_ref()
                .map(NvencAsyncEvent::raw)
                .unwrap_or(ptr::null_mut());
            unsafe { submit_encoder_eos_with_completion(api, encoder, completion_event)? };
            self.eos_submitted = true;
        }
        let mut output = Vec::with_capacity(self.pending_order.len());
        while !self.pending_order.is_empty() {
            output.push(self.drain_one(true)?.ok_or_else(|| {
                BackendError::unsupported(
                    "NvFBC NVENC flush",
                    "completion wait",
                    "flush 等待后仍没有可读取的 access unit",
                )
            })?);
        }
        if let Some(completion) = self.eos_completion.as_ref() {
            let wait_started = Instant::now();
            if !completion.wait(Duration::from_secs(5))? {
                return Err(BackendError::unsupported(
                    "NvFBC NVENC flush",
                    "EOS completion event",
                    "等待 NVENC EOS 完成事件 5 秒超时",
                ));
            }
            let wait_us = wait_started.elapsed().as_micros().min(u128::from(u64::MAX)) as u64;
            self.completion_waits = self.completion_waits.saturating_add(1);
            self.completion_wait_us = self.completion_wait_us.saturating_add(wait_us);
            self.completion_wait_max_us = self.completion_wait_max_us.max(wait_us);
        }
        Ok(output)
    }

    fn drain_one(&mut self, wait: bool) -> Result<Option<HevcAccessUnit>, BackendError> {
        let Some(slot_index) = pending_slot_for_drain(&self.pending_order, wait)? else {
            return Ok(None);
        };
        let slot = &mut self.slots[slot_index];
        if let Some(completion) = slot.completion.as_ref()
            && !slot.completion_seen
        {
            let wait_started = Instant::now();
            let ready = completion.wait(if wait {
                Duration::from_secs(5)
            } else {
                Duration::ZERO
            })?;
            let wait_us = wait_started.elapsed().as_micros().min(u128::from(u64::MAX)) as u64;
            if wait {
                self.completion_waits = self.completion_waits.saturating_add(1);
                self.completion_wait_us = self.completion_wait_us.saturating_add(wait_us);
                self.completion_wait_max_us = self.completion_wait_max_us.max(wait_us);
            }
            if !ready {
                if wait {
                    return Err(BackendError::unsupported(
                        "NvFBC NVENC completion",
                        format!("slot={slot_index}"),
                        "等待 NVENC 完成事件 5 秒超时",
                    ));
                }
                return Ok(None);
            }
            slot.completion_seen = true;
        }
        if slot.pending.is_none() {
            return Err(BackendError::unsupported(
                "NvFBC NVENC drain",
                format!("slot={slot_index}"),
                "队列指向的 surface 没有 pending frame",
            ));
        }
        let output = unsafe {
            let api = self.api.as_ref().expect("NVENC API exists until shutdown");
            let encoder = self
                .session
                .as_ref()
                .expect("NVENC session exists until shutdown")
                .encoder();
            lock_and_copy_bitstream_with_mode(api, encoder, &slot.bitstream, self.async_encode)
        }?;
        let pending = slot.pending.take().expect("validated above");
        self.pending_order.pop_front();
        // The copied bytes no longer borrow the mapped input. Release it before
        // validating or publishing the access unit so NvFBC may reuse the slot.
        let mut mapped = slot.mapped.take().ok_or_else(|| {
            BackendError::unsupported(
                "NvFBC NVENC drain",
                format!("slot={slot_index}"),
                "完成输出时找不到 mapped input resource",
            )
        })?;
        nvenc_check("NvEncUnmapInputResource(NvFBC D3D9Ex)", unsafe {
            mapped.unmap_now()
        })?;
        slot.completion_seen = false;
        let completion_latency_us = pending
            .submitted_at
            .elapsed()
            .as_micros()
            .min(u128::from(u64::MAX)) as u64;
        self.completed_frames = self.completed_frames.saturating_add(1);
        self.completion_latency_us = self
            .completion_latency_us
            .saturating_add(completion_latency_us);
        self.completion_latency_max_us = self.completion_latency_max_us.max(completion_latency_us);
        if output.output_timestamp_90k != pending.timestamp_90k {
            return Err(BackendError::unsupported(
                "NvFBC NVENC VFR timestamp",
                format!(
                    "submitted={} returned={}",
                    pending.timestamp_90k, output.output_timestamp_90k
                ),
                "NVENC 未保留输入时间戳，拒绝合成或重写时间线",
            ));
        }
        if output.bytes.is_empty() || !output.annex_b_start_code_seen {
            return Err(BackendError::unsupported(
                "NvFBC NVENC bitstream",
                "HEVC Annex-B",
                "NVENC 返回空数据或缺少 Annex-B start code",
            ));
        }
        if !self.vui_verified {
            verify_hevc_vui_matches(&output.bytes, self.expected_color)?;
            self.vui_verified = true;
        }
        let is_sync = pending.expect_sync
            && crate::backend::mp4_mux::hevc_annex_b_has_random_access_nal(&output.bytes);
        if pending.expect_sync && !is_sync {
            return Err(BackendError::unsupported(
                "NvFBC NVENC random access",
                format!("timestamp_90k={}", pending.timestamp_90k),
                "请求 IDR 后 NVENC 输出中没有随机访问 NAL",
            ));
        }
        Ok(Some(HevcAccessUnit {
            timestamp_90k: pending.timestamp_90k,
            presentation_timestamp_100ns: None,
            data: output.bytes,
            is_sync,
            discard_from_track: pending.discard_from_track,
        }))
    }

    pub(crate) fn runtime_stats(&self) -> NvencD3d9RuntimeStats {
        NvencD3d9RuntimeStats {
            async_encode: self.async_encode,
            async_fallback: self.async_fallback.clone(),
            completion_waits: self.completion_waits,
            completion_wait_us: self.completion_wait_us,
            completion_wait_max_us: self.completion_wait_max_us,
            completed_frames: self.completed_frames,
            completion_latency_us: self.completion_latency_us,
            completion_latency_max_us: self.completion_latency_max_us,
        }
    }

    fn abort_handles(&mut self) -> i32 {
        let status = self
            .session
            .as_mut()
            .map(NvencD3d9Session::destroy_now)
            .unwrap_or(NV_ENC_SUCCESS);
        if status != NV_ENC_SUCCESS {
            return status;
        }
        for slot in &mut self.slots {
            if let Some(mapped) = slot.mapped.as_mut() {
                unsafe { mapped.abandon() };
            }
            unsafe {
                slot.registered.abandon();
                slot.bitstream.abandon();
            }
            if let Some(completion) = slot.completion.as_mut() {
                completion.abandon_registration();
            }
            slot.pending = None;
        }
        if let Some(completion) = self.eos_completion.as_mut() {
            completion.abandon_registration();
        }
        self.pending_order.clear();
        status
    }

    fn leak_live_encoder_state(&mut self) {
        std::mem::forget(std::mem::take(&mut self.slots));
        if let Some(event) = self.eos_completion.take() {
            std::mem::forget(event);
        }
        std::mem::forget(std::mem::take(&mut self.pending_order));
        if let Some(session) = self.session.take() {
            std::mem::forget(session);
        }
        if let Some(api) = self.api.take() {
            std::mem::forget(api);
        }
    }

    pub(crate) fn abort(mut self) -> Result<(), BackendError> {
        let status = self.abort_handles();
        if status == NV_ENC_SUCCESS {
            Ok(())
        } else {
            self.leak_live_encoder_state();
            Err(BackendError::unsupported(
                "NvFBC NVENC abort",
                chroma_label(self.chroma),
                format!("NvEncDestroyEncoder status={status}"),
            ))
        }
    }

    pub(crate) fn shutdown(mut self) -> Result<(), BackendError> {
        let mut failures = Vec::new();
        let mut events_pending_encoder_destroy = Vec::new();
        if !self.pending_order.is_empty() || self.slots.iter().any(|slot| slot.pending.is_some()) {
            let queue = self.pending_order.len();
            let slots = self
                .slots
                .iter()
                .filter(|slot| slot.pending.is_some())
                .count();
            let destroy_status = self.abort_handles();
            if destroy_status != NV_ENC_SUCCESS {
                self.leak_live_encoder_state();
            }
            return Err(BackendError::unsupported(
                "NvFBC NVENC shutdown",
                chroma_label(self.chroma),
                format!(
                    "pending frames remain: queue={queue} slots={slots}; encoder aborted before child resources; NvEncDestroyEncoder status={destroy_status}"
                ),
            ));
        }
        for (slot_index, slot) in self.slots.iter_mut().enumerate() {
            if let Some(mut mapped) = slot.mapped.take() {
                let status = unsafe { mapped.unmap_now() };
                if status != NV_ENC_SUCCESS {
                    failures.push(format!(
                        "slot={slot_index} NvEncUnmapInputResource status={status}"
                    ));
                }
            }
            let unregister_status = unsafe { slot.registered.unregister_now() };
            if unregister_status != NV_ENC_SUCCESS {
                failures.push(format!(
                    "slot={slot_index} NvEncUnregisterResource status={unregister_status}"
                ));
            }
            if let Some(mut completion) = slot.completion.take() {
                let status = completion.unregister_now();
                if status != NV_ENC_SUCCESS {
                    failures.push(format!(
                        "slot={slot_index} NvEncUnregisterAsyncEvent status={status}"
                    ));
                    events_pending_encoder_destroy.push(completion);
                }
            }
            let bitstream_status = unsafe { slot.bitstream.destroy_now() };
            if bitstream_status != NV_ENC_SUCCESS {
                failures.push(format!(
                    "slot={slot_index} NvEncDestroyBitstreamBuffer status={bitstream_status}"
                ));
            }
        }
        if let Some(mut completion) = self.eos_completion.take() {
            let status = completion.unregister_now();
            if status != NV_ENC_SUCCESS {
                failures.push(format!("EOS NvEncUnregisterAsyncEvent status={status}"));
                events_pending_encoder_destroy.push(completion);
            }
        }
        let session_status = self
            .session
            .as_mut()
            .map(NvencD3d9Session::destroy_now)
            .unwrap_or(NV_ENC_SUCCESS);
        if session_status != NV_ENC_SUCCESS {
            failures.push(format!("NvEncDestroyEncoder status={session_status}"));
            std::mem::forget(events_pending_encoder_destroy);
            self.leak_live_encoder_state();
            return Err(BackendError::unsupported(
                "NvFBC NVENC shutdown",
                chroma_label(self.chroma),
                failures.join(" | "),
            ));
        }
        for event in &mut events_pending_encoder_destroy {
            event.abandon_registration();
        }
        if failures.is_empty() {
            Ok(())
        } else {
            Err(BackendError::unsupported(
                "NvFBC NVENC shutdown",
                chroma_label(self.chroma),
                failures.join(" | "),
            ))
        }
    }
}

impl Drop for NvencD3d9Encoder {
    fn drop(&mut self) {
        let encoder_live = self
            .session
            .as_ref()
            .is_some_and(|session| !session.encoder().is_null());
        if encoder_live && self.abort_handles() != NV_ENC_SUCCESS {
            self.leak_live_encoder_state();
        }
    }
}

fn pending_slot_for_drain(
    pending_order: &VecDeque<usize>,
    wait: bool,
) -> Result<Option<usize>, BackendError> {
    if let Some(&slot_index) = pending_order.front() {
        return Ok(Some(slot_index));
    }
    if wait {
        return Err(BackendError::unsupported(
            "NvFBC NVENC drain",
            "pending queue",
            "阻塞 drain 请求没有对应的 pending frame",
        ));
    }
    Ok(None)
}

fn chroma_label(chroma: ChromaSampling) -> &'static str {
    match chroma {
        ChromaSampling::Yuv420 => "420",
        ChromaSampling::Yuv422 => "422",
        ChromaSampling::Yuv444 => "444",
    }
}

fn route_spec(chroma: ChromaSampling) -> NvencHevcRouteSpec {
    let (profile, chroma_format_idc) = match chroma {
        ChromaSampling::Yuv420 => (NV_ENC_HEVC_PROFILE_MAIN10_GUID, 1),
        ChromaSampling::Yuv422 => (NV_ENC_HEVC_PROFILE_FREXT_GUID, 2),
        ChromaSampling::Yuv444 => (NV_ENC_HEVC_PROFILE_FREXT_GUID, 3),
    };
    NvencHevcRouteSpec {
        profile,
        bit_depth: NV_ENC_BIT_DEPTH_10,
        chroma_format_idc,
        buffer_format: NV_ENC_BUFFER_FORMAT_ABGR10,
    }
}

fn open_d3d9_session(
    api: &NvencApi,
    device: &IDirect3DDevice9Ex,
) -> Result<NvencD3d9Session, BackendError> {
    let open = api
        .functions
        .nvEncOpenEncodeSessionEx
        .ok_or_else(|| nvenc_missing("NvEncOpenEncodeSessionEx"))?;
    let destroy = api
        .functions
        .nvEncDestroyEncoder
        .ok_or_else(|| nvenc_missing("NvEncDestroyEncoder"))?;
    let mut params: NvEncOpenEncodeSessionExParams = unsafe { std::mem::zeroed() };
    params.version = NV_ENC_OPEN_ENCODE_SESSION_EX_PARAMS_VER;
    params.deviceType = NV_ENC_DEVICE_TYPE_DIRECTX;
    params.device = device.as_raw();
    params.apiVersion = NVENCAPI_VERSION;
    let mut encoder = ptr::null_mut();
    nvenc_check("NvEncOpenEncodeSessionEx(D3D9Ex)", unsafe {
        open(&mut params, &mut encoder)
    })?;
    if encoder.is_null() {
        return Err(BackendError::unsupported(
            "NvFBC NVENC D3D9Ex",
            "NvEncOpenEncodeSessionEx",
            "返回空 encoder handle",
        ));
    }
    Ok(NvencD3d9Session {
        encoder,
        destroy: Some(destroy),
        _device: device.clone(),
    })
}

fn query_d3d9_caps(api: &NvencApi, encoder: *mut c_void) -> Result<NvencD3d9Caps, BackendError> {
    let codecs = unsafe {
        query_guid_list(
            "NvEncGetEncodeGUIDCount(D3D9Ex)",
            "NvEncGetEncodeGUIDs(D3D9Ex)",
            encoder,
            api.functions.nvEncGetEncodeGUIDCount,
            |guids, capacity, written| {
                let list = api
                    .functions
                    .nvEncGetEncodeGUIDs
                    .ok_or_else(|| nvenc_missing("NvEncGetEncodeGUIDs"))?;
                Ok(list(encoder, guids, capacity, written))
            },
        )?
    };
    let hevc = codecs.contains(&NV_ENC_CODEC_HEVC_GUID);
    if !hevc {
        return Ok(NvencD3d9Caps {
            hevc: false,
            abgr10_input: false,
            main10_profile: false,
            frext_profile: false,
            ten_bit: false,
            yuv422: false,
            yuv444: false,
            async_encode: false,
            max_width: 0,
            max_height: 0,
            rate_controls: Vec::new(),
            presets: Vec::new(),
            encoder_engines: 0,
            warnings: Vec::new(),
        });
    }

    let profiles = unsafe { query_hevc_profiles(api, encoder)? };
    let formats = unsafe { query_input_formats(api, encoder)? };
    let mut warnings = Vec::new();
    let presets = unsafe { query_hevc_presets(api, encoder, &mut warnings)? };
    let caps = query_hevc_caps(api, encoder, &mut warnings);
    let rate_controls = caps
        .rate_control_mask
        .map(rate_controls_from_mask)
        .unwrap_or_else(|| vec![RateControlMethod::Cqp]);
    Ok(NvencD3d9Caps {
        hevc,
        abgr10_input: formats.iter().any(|format| format == "ABGR10"),
        main10_profile: profiles.iter().any(|profile| profile == "Main10"),
        frext_profile: profiles.iter().any(|profile| profile == "FRExt"),
        ten_bit: caps.ten_bit.unwrap_or(false),
        yuv422: caps.yuv422.unwrap_or(false),
        yuv444: caps.yuv444.unwrap_or(false),
        async_encode: caps.async_encode.unwrap_or(false),
        max_width: caps
            .max_width
            .and_then(|value| value.try_into().ok())
            .unwrap_or(0),
        max_height: caps
            .max_height
            .and_then(|value| value.try_into().ok())
            .unwrap_or(0),
        rate_controls,
        presets,
        encoder_engines: caps.encoder_engines.unwrap_or(0),
        warnings,
    })
}

fn validate_surface(
    surface: &IDirect3DSurface9,
    width: u32,
    height: u32,
) -> Result<(), BackendError> {
    let mut desc = D3DSURFACE_DESC::default();
    unsafe { surface.GetDesc(&mut desc) }.map_err(|err| BackendError::WindowsApi {
        func: "IDirect3DSurface9::GetDesc(NvFBC NVENC)",
        message: err.to_string(),
    })?;
    if desc.Width < width || desc.Height < height || desc.Format != D3DFMT_A2B10G10R10 {
        return Err(BackendError::unsupported(
            "NvFBC NVENC D3D9Ex input",
            format!(
                "{}x{} D3DFORMAT({})",
                desc.Width, desc.Height, desc.Format.0
            ),
            format!("需要至少 {width}x{height} A2B10G10R10 render target"),
        ));
    }
    Ok(())
}

fn register_d3d9_surface(
    api: &NvencApi,
    encoder: *mut c_void,
    surface: &IDirect3DSurface9,
    width: u32,
    height: u32,
) -> Result<NvencRegisteredResource, BackendError> {
    let register = api
        .functions
        .nvEncRegisterResource
        .ok_or_else(|| nvenc_missing("NvEncRegisterResource"))?;
    let unregister = api
        .functions
        .nvEncUnregisterResource
        .ok_or_else(|| nvenc_missing("NvEncUnregisterResource"))?;
    let mut params: NvEncRegisterResource = unsafe { std::mem::zeroed() };
    params.version = NV_ENC_REGISTER_RESOURCE_VER;
    params.resourceType = NV_ENC_INPUT_RESOURCE_TYPE_DIRECTX;
    params.width = width;
    params.height = height;
    params.pitch = 0;
    params.subResourceIndex = 0;
    params.resourceToRegister = surface.as_raw();
    params.bufferFormat = NV_ENC_BUFFER_FORMAT_ABGR10;
    params.bufferUsage = NV_ENC_INPUT_IMAGE;
    nvenc_check("NvEncRegisterResource(NvFBC D3D9Ex surface)", unsafe {
        register(encoder, &mut params)
    })?;
    if params.registeredResource.is_null() {
        return Err(BackendError::unsupported(
            "NvFBC NVENC D3D9Ex input",
            "NvEncRegisterResource",
            "返回空 registeredResource",
        ));
    }
    Ok(NvencRegisteredResource {
        encoder,
        resource: params.registeredResource,
        unregister: Some(unregister),
        _resource: None,
    })
}

#[allow(clippy::too_many_arguments)]
fn submit_d3d9_picture(
    api: &NvencApi,
    encoder: *mut c_void,
    mapped: &NvencMappedInputResource,
    bitstream: &NvencBitstreamBuffer,
    width: u32,
    height: u32,
    frame_index: u32,
    timestamp_90k: u64,
    force_idr: bool,
    completion_event: *mut c_void,
) -> Result<(), BackendError> {
    let encode = api
        .functions
        .nvEncEncodePicture
        .ok_or_else(|| nvenc_missing("NvEncEncodePicture"))?;
    let mut params: NvEncPicParams = unsafe { std::mem::zeroed() };
    params.version = NV_ENC_PIC_PARAMS_VER;
    params.inputWidth = width;
    params.inputHeight = height;
    // D3D9 surfaces are driver-managed resources. The proven direct route uses
    // a zero pitch and the format returned by NvEncMapInputResource.
    params.inputPitch = 0;
    params.encodePicFlags = if force_idr {
        NV_ENC_PIC_FLAG_FORCEIDR | NV_ENC_PIC_FLAG_OUTPUT_SPSPPS
    } else {
        0
    };
    params.frameIdx = frame_index;
    params.inputTimeStamp = timestamp_90k;
    params.inputDuration = 1;
    params.inputBuffer = mapped.mapped;
    params.outputBitstream = bitstream.buffer;
    params.completionEvent = completion_event;
    params.bufferFmt = if mapped.format == 0 {
        NV_ENC_BUFFER_FORMAT_ABGR10
    } else {
        mapped.format
    };
    params.pictureStruct = NV_ENC_PIC_STRUCT_FRAME;
    nvenc_check("NvEncEncodePicture(NvFBC D3D9Ex surface)", unsafe {
        encode(encoder, &mut params)
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn empty_nonblocking_drain_is_not_an_error() {
        let pending = VecDeque::new();
        assert_eq!(pending_slot_for_drain(&pending, false).unwrap(), None);
        assert!(pending_slot_for_drain(&pending, true).is_err());
    }
}
