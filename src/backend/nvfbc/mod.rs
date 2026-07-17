#![allow(dead_code)]
//! Dormant Windows NvFBC capture backend.
//!
//! This module deliberately is not connected to the global capability probe,
//! GUI, or automatic backend selector. It exposes a typed backend API so the
//! proven D3D9Ex NvFBC-to-NVENC route can live in mainline without changing the
//! current DDA/WGC production behavior.

mod d3d9;
mod ffi;

use crate::backend::mp4_mux::{HevcAccessUnit, NclxColorMetadata};
use crate::backend::nvenc::{
    NvencD3d9Caps, NvencD3d9Encoder, NvencD3d9RuntimeStats, nvenc_display_route_color,
};
use crate::config::ChromaSampling;
use crate::error::BackendError;
use crate::rate_control::{NvencPreset, RateControlConfig, RateControlMethod};
use serde::{Deserialize, Serialize};

const SURFACE_COUNT: usize = 3;

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct NvFbcProbeInfo {
    pub available: bool,
    pub error: Option<String>,
    pub adapter: Option<NvFbcAdapterInfo>,
    pub display: Option<NvFbcDisplayInfo>,
    pub capture_width: u32,
    pub capture_height: u32,
    pub nvfbc_version: u32,
    pub routes: Vec<NvFbcRouteInfo>,
    pub rate_controls: Vec<RateControlMethod>,
    pub presets: Vec<NvencPreset>,
    pub encoder_engines: u32,
    pub lookahead: bool,
    pub lookahead_policy: String,
    pub warnings: Vec<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct NvFbcAdapterInfo {
    pub d3d9_adapter_index: u32,
    pub adapter_name: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct NvFbcDisplayInfo {
    pub dxgi_adapter_index: u32,
    pub output_index: u32,
    pub device_name: String,
    pub desktop_left: i32,
    pub desktop_top: i32,
    pub desktop_right: i32,
    pub desktop_bottom: i32,
    pub color_space: u32,
    pub bits_per_color: u32,
    pub refresh_numerator: u32,
    pub refresh_denominator: u32,
    pub hdr_pq: bool,
    pub color_note: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct NvFbcRouteInfo {
    pub chroma: ChromaSampling,
    pub input_format: String,
    pub profile: String,
    pub bit_depth: u16,
    pub supported: bool,
    pub blocker: Option<String>,
}

#[derive(Debug, Clone)]
pub struct NvFbcOptions {
    /// D3D9 adapter index. `None` selects the first NVIDIA D3D9 adapter.
    pub adapter_index: Option<u32>,
    pub chroma: ChromaSampling,
    pub capture_cursor: bool,
    pub rate_control: RateControlConfig,
}

impl Default for NvFbcOptions {
    fn default() -> Self {
        Self {
            adapter_index: None,
            chroma: ChromaSampling::Yuv420,
            capture_cursor: false,
            rate_control: RateControlConfig::default(),
        }
    }
}

#[derive(Debug)]
pub struct NvFbcCaptureResult {
    /// 独立 DXGI vblank waiter 采样的扫描时刻，相对首帧映射到 90kHz。
    /// NvFBC V3 不提供应用呈现 QPC，因此这里只描述桌面扫描节奏。
    pub capture_timestamp_90k: u64,
    /// 与 WASAPI `IAudioCaptureClient::GetBuffer` QPCPosition 相同的
    /// 100ns 绝对时基，用于音画同步，不能由相对视频时间线反推。
    pub capture_timestamp_100ns: i64,
    /// 自上一次 Grab 后 vblank waiter 观察到的扫描周期数。大于 1 表示
    /// 捕获/编码确实来不及处理全部扫描帧；不会再因为重新等待而额外丢一周期。
    pub vblank_ticks: u64,
    pub source_pid: u32,
    pub wait_mode_used: u32,
    pub access_units: Vec<HevcAccessUnit>,
}

struct CaptureStack {
    // Drop order is part of the safety contract: release NvFBC before its
    // output surfaces, then release surfaces before the D3D9Ex device.
    session: Option<ffi::NvFbcSession>,
    surfaces: d3d9::CaptureSurfacePool,
    device: d3d9::D3d9Device,
    vblank_waiter: d3d9::VblankWaiter,
}

impl CaptureStack {
    fn close(mut self) -> Result<(), BackendError> {
        if let Some(session) = self.session.take() {
            session.close()?;
        }
        Ok(())
    }
}

pub struct NvFbcRecorder {
    // Encoder registrations must disappear before NvFBC may release or the
    // backing D3D9Ex surfaces may be destroyed.
    encoder: Option<NvencD3d9Encoder>,
    capture: Option<CaptureStack>,
    width: u32,
    height: u32,
    chroma: ChromaSampling,
    color: NclxColorMetadata,
    timeline_origin_100ns: Option<i64>,
    last_timestamp_90k: Option<u64>,
    last_vblank_generation: u64,
}

pub fn probe() -> NvFbcProbeInfo {
    match probe_impl(None) {
        Ok(info) => info,
        Err(err) => NvFbcProbeInfo {
            available: false,
            error: Some(err.to_string()),
            adapter: None,
            display: None,
            capture_width: 0,
            capture_height: 0,
            nvfbc_version: 0,
            routes: Vec::new(),
            rate_controls: Vec::new(),
            presets: Vec::new(),
            encoder_engines: 0,
            lookahead: false,
            lookahead_policy: "disabled_for_nvfbc".to_owned(),
            warnings: vec![err.to_string()],
        },
    }
}

impl NvFbcRecorder {
    pub fn open(options: NvFbcOptions) -> Result<Self, BackendError> {
        if options.rate_control.look_ahead_depth != 0 {
            return Err(BackendError::unsupported(
                "NvFBC recorder",
                format!("LookAheadDepth={}", options.rate_control.look_ahead_depth),
                "NvFBC 直连路线不支持 Lookahead",
            ));
        }

        let device = d3d9::D3d9Device::open(options.adapter_index)?;
        let display = device.display();
        let display_color = nvenc_display_route_color(display.color_space, display.bits_per_color)?;
        if !display_color.hdr_pq || display_color.bit_depth != 10 {
            return Err(BackendError::unsupported(
                "NvFBC recorder desktop mode",
                format!(
                    "ColorSpace={} BitsPerColor={}",
                    display.color_space, display.bits_per_color
                ),
                "不支持的桌面模式；当前 NvFBC 后端只生产化 HDR PQ 10-bit ARGB10",
            ));
        }

        let (mut session, create_info) = ffi::NvFbcSession::create(&device)?;
        let surfaces =
            device.create_surface_pool(create_info.width, create_info.height, SURFACE_COUNT)?;
        session.setup(&surfaces, true, options.capture_cursor)?;

        // Once Setup succeeds the shim may retain the output-buffer descriptors.
        // Move all owners into a correctly ordered guard before another fallible call.
        let capture = CaptureStack {
            session: Some(session),
            surfaces,
            vblank_waiter: device.spawn_vblank_waiter()?,
            device,
        };
        let encoder = NvencD3d9Encoder::open(
            capture.device.device(),
            capture.surfaces.surfaces(),
            create_info.width,
            create_info.height,
            options.chroma,
            display_color.mp4_color,
            &options.rate_control,
            capture.device.display().refresh_numerator,
            capture.device.display().refresh_denominator,
        )?;
        // The waiter has been running while NVENC initialized. Start from its
        // current generation so the first Grab waits for a fresh vblank instead
        // of consuming a stale notification and producing a near-zero interval.
        let last_vblank_generation = capture.vblank_waiter.current_generation();

        Ok(Self {
            encoder: Some(encoder),
            capture: Some(capture),
            width: create_info.width,
            height: create_info.height,
            chroma: options.chroma,
            color: display_color.mp4_color,
            timeline_origin_100ns: None,
            last_timestamp_90k: None,
            last_vblank_generation,
        })
    }

    pub fn dimensions(&self) -> (u32, u32) {
        (self.width, self.height)
    }

    pub fn chroma(&self) -> ChromaSampling {
        self.chroma
    }

    pub fn color(&self) -> NclxColorMetadata {
        self.color
    }

    pub fn capture_next(&mut self) -> Result<NvFbcCaptureResult, BackendError> {
        self.capture_next_with_force_idr(false)
    }

    pub fn capture_next_with_force_idr(
        &mut self,
        force_idr: bool,
    ) -> Result<NvFbcCaptureResult, BackendError> {
        let slot_index = self.encoder()?.next_surface_index()?;
        let capture = self.capture.as_mut().ok_or_else(|| {
            BackendError::unsupported("NvFBC recorder", "capture stack", "recorder 已关闭")
        })?;
        let vblank = capture
            .vblank_waiter
            .wait_after(self.last_vblank_generation)?;
        let vblank_ticks = if self.last_vblank_generation == 0 {
            1
        } else {
            vblank
                .generation
                .saturating_sub(self.last_vblank_generation)
                .max(1)
        };
        self.last_vblank_generation = vblank.generation;
        let info = capture
            .session
            .as_mut()
            .ok_or_else(|| {
                BackendError::unsupported("NvFBC recorder", "capture session", "session 已关闭")
            })?
            .grab(slot_index)?;

        if info.width != self.width
            || info.height != self.height
            || info.buffer_width < self.width
            || info.must_recreate
        {
            return Err(BackendError::reconfigure_required(format!(
                "NvFBC frame dimensions changed: expected={}x{} actual={}x{} buffer_width={} driver_internal=0x{:08x}",
                self.width,
                self.height,
                info.width,
                info.height,
                info.buffer_width,
                info.driver_internal_error
            )));
        }
        if info.protected_content {
            return Err(BackendError::unsupported(
                "NvFBC capture",
                "protected desktop content",
                "NvFBC 报告受保护内容，拒绝输出不完整帧",
            ));
        }
        if !info.is_hdr {
            return Err(BackendError::reconfigure_required(
                "NvFBC HDR flag disappeared while the recorder is configured for HDR PQ",
            ));
        }

        let timestamp_100ns = vblank.timestamp_100ns;
        if timestamp_100ns <= 0 {
            return Err(BackendError::unsupported(
                "NvFBC source timestamp",
                "vblank QPC",
                "独立 vblank waiter 未提供有效绝对时间戳",
            ));
        }
        let timestamp_90k = self.arrival_timestamp_90k(timestamp_100ns)?;
        let force_idr = force_idr || self.last_timestamp_90k.is_none();
        let access_units =
            self.encoder()?
                .submit_surface(slot_index, timestamp_90k, force_idr, false)?;
        self.last_timestamp_90k = Some(timestamp_90k);
        Ok(NvFbcCaptureResult {
            capture_timestamp_90k: timestamp_90k,
            capture_timestamp_100ns: timestamp_100ns,
            vblank_ticks,
            source_pid: info.source_pid,
            wait_mode_used: info.wait_mode_used,
            access_units,
        })
    }

    pub fn finish(self) -> Result<Vec<HevcAccessUnit>, BackendError> {
        self.finish_with_stats().map(|(output, _)| output)
    }

    pub(crate) fn finish_with_stats(
        mut self,
    ) -> Result<(Vec<HevcAccessUnit>, NvencD3d9RuntimeStats), BackendError> {
        let mut encoder = self.encoder.take().ok_or_else(|| {
            BackendError::unsupported("NvFBC recorder", "NVENC encoder", "encoder 已关闭")
        })?;
        let output = encoder.flush()?;
        let stats = encoder.runtime_stats();
        drop(encoder);
        if let Some(capture) = self.capture.take() {
            capture.close()?;
        }
        Ok((output, stats))
    }

    fn encoder(&mut self) -> Result<&mut NvencD3d9Encoder, BackendError> {
        self.encoder.as_mut().ok_or_else(|| {
            BackendError::unsupported("NvFBC recorder", "NVENC encoder", "encoder 已关闭")
        })
    }

    fn arrival_timestamp_90k(&mut self, timestamp_100ns: i64) -> Result<u64, BackendError> {
        let origin = *self.timeline_origin_100ns.get_or_insert(timestamp_100ns);
        let timestamp = ((i128::from(timestamp_100ns.saturating_sub(origin).max(0)) * 90_000
            + 5_000_000)
            / 10_000_000)
            .min(i128::from(u64::MAX)) as u64;
        if let Some(previous) = self.last_timestamp_90k
            && timestamp <= previous
        {
            return Err(BackendError::unsupported(
                "NvFBC VFR vblank timestamp",
                format!("previous={previous} current={timestamp}"),
                "连续 vblank 事件不能映射为严格递增的 90 kHz 时间戳；拒绝重写时间线",
            ));
        }
        Ok(timestamp)
    }
}

fn probe_impl(adapter_index: Option<u32>) -> Result<NvFbcProbeInfo, BackendError> {
    #[cfg(test)]
    eprintln!("nvfbc_rust_probe=d3d9_open_begin");
    let device = d3d9::D3d9Device::open(adapter_index)?;
    #[cfg(test)]
    eprintln!("nvfbc_rust_probe=d3d9_open_end");
    let display = device.display().clone();
    let display_color = nvenc_display_route_color(display.color_space, display.bits_per_color)?;
    #[cfg(test)]
    eprintln!("nvfbc_rust_probe=ffi_create_begin");
    let (session, create_info) = ffi::NvFbcSession::create(&device)?;
    #[cfg(test)]
    eprintln!("nvfbc_rust_probe=ffi_create_end");
    let caps = NvencD3d9Encoder::probe(device.device())?;
    #[cfg(test)]
    eprintln!("nvfbc_rust_probe=nvenc_probe_end");
    session.close()?;
    #[cfg(test)]
    eprintln!("nvfbc_rust_probe=session_close_end");

    let routes = route_infos(
        &caps,
        create_info.width,
        create_info.height,
        display_color.hdr_pq && display_color.bit_depth == 10,
    );
    Ok(NvFbcProbeInfo {
        available: true,
        error: None,
        adapter: Some(NvFbcAdapterInfo {
            d3d9_adapter_index: device.adapter_index(),
            adapter_name: device.adapter_name().to_owned(),
        }),
        display: Some(NvFbcDisplayInfo {
            dxgi_adapter_index: display.dxgi_adapter_index,
            output_index: display.output_index,
            device_name: display.device_name,
            desktop_left: display.desktop_left,
            desktop_top: display.desktop_top,
            desktop_right: display.desktop_right,
            desktop_bottom: display.desktop_bottom,
            color_space: display.color_space,
            bits_per_color: display.bits_per_color,
            refresh_numerator: display.refresh_numerator,
            refresh_denominator: display.refresh_denominator,
            hdr_pq: display_color.hdr_pq,
            color_note: display_color.note.to_owned(),
        }),
        capture_width: create_info.width,
        capture_height: create_info.height,
        nvfbc_version: create_info.nvfbc_version,
        routes,
        rate_controls: caps.rate_controls,
        presets: caps.presets,
        encoder_engines: caps.encoder_engines,
        lookahead: false,
        lookahead_policy: "disabled_for_nvfbc".to_owned(),
        warnings: caps.warnings,
    })
}

fn route_infos(
    caps: &NvencD3d9Caps,
    width: u32,
    height: u32,
    hdr_pq_10bit: bool,
) -> Vec<NvFbcRouteInfo> {
    [
        (ChromaSampling::Yuv420, "Main10"),
        (ChromaSampling::Yuv422, "FRExt"),
        (ChromaSampling::Yuv444, "FRExt"),
    ]
    .into_iter()
    .map(|(chroma, profile)| {
        let hardware_supported = caps.supports(chroma, width, height);
        let supported = hdr_pq_10bit && hardware_supported;
        let blocker = if !hdr_pq_10bit {
            Some("不支持的桌面模式；NvFBC 后端当前只生产化 HDR PQ 10-bit".to_owned())
        } else if !hardware_supported {
            Some("D3D9 NVENC session 未报告完整 ABGR10/profile/chroma/size 能力".to_owned())
        } else {
            None
        };
        NvFbcRouteInfo {
            chroma,
            input_format: "ABGR10".to_owned(),
            profile: profile.to_owned(),
            bit_depth: 10,
            supported,
            blocker,
        }
    })
    .collect()
}

#[cfg(test)]
mod tests;
