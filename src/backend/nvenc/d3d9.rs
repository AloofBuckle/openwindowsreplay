#![deny(unsafe_op_in_unsafe_fn)]

use super::*;
use std::marker::PhantomData;
use std::rc::Rc;
use windows::Win32::Graphics::Direct3D9::{
    D3DFMT_A2B10G10R10, D3DSURFACE_DESC, IDirect3DDevice9Ex, IDirect3DSurface9,
};
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
        let encoder = std::mem::replace(&mut self.encoder, ptr::null_mut());
        self.destroy
            .map(|destroy| unsafe { destroy(encoder) })
            .unwrap_or(NV_ENC_SUCCESS)
    }
}

impl Drop for NvencD3d9Session {
    fn drop(&mut self) {
        let _ = self.destroy_now();
    }
}

struct PendingFrame {
    timestamp_90k: u64,
    discard_from_track: bool,
}

struct EncodeSlot {
    // Drop order is intentional: an input must be unmapped and unregistered
    // before its bitstream and backing D3D9 surface are released.
    mapped: Option<NvencMappedInputResource>,
    registered: NvencRegisteredResource,
    bitstream: NvencBitstreamBuffer,
    _surface: IDirect3DSurface9,
    pending: Option<PendingFrame>,
}

pub(crate) struct NvencD3d9Encoder {
    // Slots are released before the session, and the session before the DLL.
    slots: Vec<EncodeSlot>,
    pending_order: VecDeque<usize>,
    session: NvencD3d9Session,
    api: NvencApi,
    width: u32,
    height: u32,
    chroma: ChromaSampling,
    expected_color: NclxColorMetadata,
    vui_verified: bool,
    frame_index: u32,
    eos_submitted: bool,
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
        let session = open_d3d9_session(&api, device)?;
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
                "NvEncInitializeEncoder(HEVC NvFBC D3D9Ex)",
            )?;
        }

        let mut slots = Vec::with_capacity(surfaces.len());
        for surface in surfaces {
            let registered =
                register_d3d9_surface(&api, session.encoder(), surface, width, height)?;
            let bitstream = unsafe { create_bitstream_buffer(&api, session.encoder())? };
            slots.push(EncodeSlot {
                mapped: None,
                registered,
                bitstream,
                _surface: surface.clone(),
                pending: None,
            });
        }

        Ok(Self {
            slots,
            pending_order: VecDeque::with_capacity(NVFBC_SURFACE_COUNT),
            session,
            api,
            width,
            height,
            chroma,
            expected_color: color,
            vui_verified: false,
            frame_index: 0,
            eos_submitted: false,
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

        let slot = &mut self.slots[slot_index];
        let mapped =
            unsafe { map_input_resource(&self.api, self.session.encoder(), &slot.registered)? };
        let status = submit_d3d9_picture(
            &self.api,
            self.session.encoder(),
            &mapped,
            &slot.bitstream,
            self.width,
            self.height,
            self.frame_index,
            timestamp_90k,
            force_idr || self.frame_index == 0,
        );
        if let Err(err) = status {
            drop(mapped);
            return Err(err);
        }
        slot.mapped = Some(mapped);
        slot.pending = Some(PendingFrame {
            timestamp_90k,
            discard_from_track,
        });
        self.pending_order.push_back(slot_index);
        self.frame_index = self.frame_index.wrapping_add(1);

        if self.pending_order.len() >= self.slots.len() {
            Ok(vec![self.drain_one()?])
        } else {
            Ok(Vec::new())
        }
    }

    pub(crate) fn flush(&mut self) -> Result<Vec<HevcAccessUnit>, BackendError> {
        if !self.eos_submitted {
            unsafe { submit_encoder_eos(&self.api, self.session.encoder())? };
            self.eos_submitted = true;
        }
        let mut output = Vec::with_capacity(self.pending_order.len());
        while !self.pending_order.is_empty() {
            output.push(self.drain_one()?);
        }
        Ok(output)
    }

    fn drain_one(&mut self) -> Result<HevcAccessUnit, BackendError> {
        let slot_index = self.pending_order.pop_front().ok_or_else(|| {
            BackendError::unsupported("NvFBC NVENC drain", "pending queue", "输出队列为空")
        })?;
        let slot = &mut self.slots[slot_index];
        let pending = slot.pending.take().ok_or_else(|| {
            BackendError::unsupported(
                "NvFBC NVENC drain",
                format!("slot={slot_index}"),
                "队列指向的 surface 没有 pending frame",
            )
        })?;
        let output =
            unsafe { lock_and_copy_bitstream(&self.api, self.session.encoder(), &slot.bitstream) };
        // The copied bytes no longer borrow the mapped input. Release it before
        // validating or publishing the access unit so NvFBC may reuse the slot.
        slot.mapped.take();
        let output = output?;
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
        let is_sync = crate::backend::mp4_mux::hevc_annex_b_has_random_access_nal(&output.bytes);
        Ok(HevcAccessUnit {
            timestamp_90k: pending.timestamp_90k,
            data: output.bytes.into(),
            is_sync,
            discard_from_track: pending.discard_from_track,
        })
    }
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
    if desc.Width != width || desc.Height != height || desc.Format != D3DFMT_A2B10G10R10 {
        return Err(BackendError::unsupported(
            "NvFBC NVENC D3D9Ex input",
            format!(
                "{}x{} D3DFORMAT({})",
                desc.Width, desc.Height, desc.Format.0
            ),
            format!("需要 {width}x{height} A2B10G10R10 render target"),
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
