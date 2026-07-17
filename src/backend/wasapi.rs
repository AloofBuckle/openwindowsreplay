//! WASAPI 音频采集。
//!
//! 这里负责把系统 loopback 或默认麦克风 capture 转成带 QPC/100ns 时间戳的
//! interleaved f32 PCM。后续 mixer/resampler 继续保留这个时间戳，不能用音频块序号
//! 或视频帧号重建同步。
#![allow(unsafe_op_in_unsafe_fn)]

use crate::backend::audio::{AudioSourceKind, PcmFormat, PcmFrame};
use crate::error::BackendError;
use std::sync::Arc;
use std::sync::atomic::AtomicBool;
use std::sync::mpsc::Sender;
use std::time::{Duration, Instant};

#[derive(Debug, Clone, Copy, Default)]
pub struct WasapiCaptureStats {
    pub packet_count: usize,
    pub pcm_frames: usize,
    pub sample_rate: u32,
    pub channels: u16,
    pub bits_per_sample: u16,
    pub valid_bits_per_sample: u16,
    pub block_align: u16,
    pub timestamp_from_device_position: bool,
    pub silent_packets: usize,
    pub discontinuity_packets: usize,
    pub timestamp_error_packets: usize,
    pub device_gap_packets: usize,
    pub device_gap_frames: u64,
    pub device_overlap_packets: usize,
    pub device_overlap_frames: u64,
    pub qpc_delta_error_packets: usize,
    pub qpc_delta_error_abs_frames: u64,
    pub qpc_delta_error_max_frames: u64,
    pub qpc_timeline_gap_packets: usize,
    pub qpc_timeline_gap_frames: u64,
    pub qpc_timeline_overlap_packets: usize,
    pub qpc_timeline_overlap_frames: u64,
}

#[cfg(windows)]
mod platform {
    use super::*;
    use windows::Win32::Media::Audio::{
        AUDCLNT_BUFFERFLAGS_DATA_DISCONTINUITY, AUDCLNT_BUFFERFLAGS_SILENT,
        AUDCLNT_BUFFERFLAGS_TIMESTAMP_ERROR, AUDCLNT_SHAREMODE_SHARED,
        AUDCLNT_STREAMFLAGS_LOOPBACK, IAudioCaptureClient, IAudioClient, IMMDeviceEnumerator,
        MMDeviceEnumerator, WAVE_FORMAT_PCM, WAVEFORMATEX, WAVEFORMATEXTENSIBLE, eCapture,
        eConsole, eRender,
    };
    use windows::Win32::Media::KernelStreaming::WAVE_FORMAT_EXTENSIBLE;
    use windows::Win32::Media::Multimedia::{
        KSDATAFORMAT_SUBTYPE_IEEE_FLOAT, WAVE_FORMAT_IEEE_FLOAT,
    };
    use windows::Win32::System::Com::{
        CLSCTX_ALL, COINIT_MULTITHREADED, CoCreateInstance, CoInitializeEx, CoTaskMemFree,
        CoUninitialize,
    };
    use windows::core::GUID;

    const RPC_E_CHANGED_MODE: u32 = 0x8001_0106;
    const BUFFER_DURATION_100NS: i64 = 10_000_000;
    const POLL_SLEEP: Duration = Duration::from_millis(5);

    pub fn capture_default_streaming(
        source: AudioSourceKind,
        duration: Duration,
        stop: Option<Arc<AtomicBool>>,
        tx: Sender<PcmFrame>,
    ) -> Result<WasapiCaptureStats, BackendError> {
        let mut stats = WasapiCaptureStats::default();
        capture_default_with_sink(source, duration, stop, &mut stats, |frame| {
            // 录制结束或音频被关闭时接收端可能已经释放；这不是音频设备错误。
            let _ = tx.send(frame);
            Ok(())
        })?;
        Ok(stats)
    }

    fn capture_default_with_sink(
        source: AudioSourceKind,
        duration: Duration,
        stop: Option<Arc<AtomicBool>>,
        stats: &mut WasapiCaptureStats,
        mut on_frame: impl FnMut(PcmFrame) -> Result<(), BackendError>,
    ) -> Result<(), BackendError> {
        let _com = ComGuard::init()?;
        unsafe {
            let enumerator: IMMDeviceEnumerator =
                CoCreateInstance(&MMDeviceEnumerator, None, CLSCTX_ALL)
                    .map_err(windows_audio_error("CoCreateInstance(MMDeviceEnumerator)"))?;
            let device = match source {
                AudioSourceKind::Loopback => enumerator
                    .GetDefaultAudioEndpoint(eRender, eConsole)
                    .map_err(windows_audio_error("GetDefaultAudioEndpoint(eRender)"))?,
                AudioSourceKind::Microphone => enumerator
                    .GetDefaultAudioEndpoint(eCapture, eConsole)
                    .map_err(windows_audio_error("GetDefaultAudioEndpoint(eCapture)"))?,
            };
            let client: IAudioClient = device
                .Activate(CLSCTX_ALL, None)
                .map_err(windows_audio_error("IMMDevice::Activate(IAudioClient)"))?;
            let mix_ptr = client
                .GetMixFormat()
                .map_err(windows_audio_error("IAudioClient::GetMixFormat"))?;
            if mix_ptr.is_null() {
                return Err(BackendError::AudioUnsupported {
                    reason: "IAudioClient::GetMixFormat 返回空指针".to_owned(),
                });
            }
            let mix = WasapiMixFormat::from_waveformat(mix_ptr)?;
            stats.sample_rate = mix.sample_rate;
            stats.channels = mix.channels;
            stats.bits_per_sample = mix.bits_per_sample;
            stats.valid_bits_per_sample = mix.valid_bits_per_sample;
            stats.block_align = mix.block_align;
            let timestamp_mode = WasapiTimestampMode::from_env();
            stats.timestamp_from_device_position =
                matches!(timestamp_mode, WasapiTimestampMode::DevicePosition);
            let stream_flags = match source {
                AudioSourceKind::Loopback => AUDCLNT_STREAMFLAGS_LOOPBACK,
                AudioSourceKind::Microphone => 0,
            };
            client
                .Initialize(
                    AUDCLNT_SHAREMODE_SHARED,
                    stream_flags,
                    BUFFER_DURATION_100NS,
                    0,
                    mix_ptr,
                    None,
                )
                .map_err(windows_audio_error("IAudioClient::Initialize"))?;
            CoTaskMemFree(Some(mix_ptr as _));

            let capture: IAudioCaptureClient = client.GetService().map_err(windows_audio_error(
                "IAudioClient::GetService(IAudioCaptureClient)",
            ))?;
            client
                .Start()
                .map_err(windows_audio_error("IAudioClient::Start"))?;
            let mut clock = WasapiClockTracker::default();
            let start = Instant::now();
            while start.elapsed() < duration
                && !stop
                    .as_ref()
                    .is_some_and(|flag| flag.load(std::sync::atomic::Ordering::Relaxed))
            {
                drain_packets(
                    &capture,
                    source,
                    mix,
                    timestamp_mode,
                    &mut clock,
                    stats,
                    &mut on_frame,
                )?;
                std::thread::sleep(POLL_SLEEP);
            }
            drain_packets(
                &capture,
                source,
                mix,
                timestamp_mode,
                &mut clock,
                stats,
                &mut on_frame,
            )?;
            let _ = client.Stop();
            Ok(())
        }
    }

    unsafe fn drain_packets<F>(
        capture: &IAudioCaptureClient,
        source: AudioSourceKind,
        mix: WasapiMixFormat,
        timestamp_mode: WasapiTimestampMode,
        clock: &mut WasapiClockTracker,
        stats: &mut WasapiCaptureStats,
        on_frame: &mut F,
    ) -> Result<(), BackendError>
    where
        F: FnMut(PcmFrame) -> Result<(), BackendError>,
    {
        loop {
            let next = capture.GetNextPacketSize().map_err(windows_audio_error(
                "IAudioCaptureClient::GetNextPacketSize",
            ))?;
            if next == 0 {
                break;
            }
            let mut data = std::ptr::null_mut::<u8>();
            let mut frame_count = 0u32;
            let mut flags = 0u32;
            let mut device_position = 0u64;
            let mut qpc_position = 0u64;
            capture
                .GetBuffer(
                    &mut data,
                    &mut frame_count,
                    &mut flags,
                    Some(&mut device_position),
                    Some(&mut qpc_position),
                )
                .map_err(windows_audio_error("IAudioCaptureClient::GetBuffer"))?;
            let silent = (flags & AUDCLNT_BUFFERFLAGS_SILENT.0 as u32) != 0;
            let discontinuity = (flags & AUDCLNT_BUFFERFLAGS_DATA_DISCONTINUITY.0 as u32) != 0;
            let timestamp_error = (flags & AUDCLNT_BUFFERFLAGS_TIMESTAMP_ERROR.0 as u32) != 0;
            stats.packet_count = stats.packet_count.saturating_add(1);
            stats.pcm_frames = stats.pcm_frames.saturating_add(frame_count as usize);
            stats.silent_packets = stats.silent_packets.saturating_add(usize::from(silent));
            stats.discontinuity_packets = stats
                .discontinuity_packets
                .saturating_add(usize::from(discontinuity));
            stats.timestamp_error_packets = stats
                .timestamp_error_packets
                .saturating_add(usize::from(timestamp_error));
            let start_time_100ns = clock.observe(
                WasapiClockObservation {
                    device_position,
                    qpc_position,
                    frame_count,
                    discontinuity,
                    timestamp_error,
                    sample_rate: mix.sample_rate,
                    timestamp_mode,
                },
                stats,
            );
            let samples = if silent || data.is_null() {
                vec![0.0f32; frame_count as usize * usize::from(mix.channels)]
            } else {
                convert_packet_to_f32(data, frame_count, mix)?
            };
            capture
                .ReleaseBuffer(frame_count)
                .map_err(windows_audio_error("IAudioCaptureClient::ReleaseBuffer"))?;
            if !samples.is_empty() {
                on_frame(PcmFrame {
                    source,
                    start_time_100ns,
                    format: PcmFormat {
                        sample_rate: mix.sample_rate,
                        channels: mix.channels,
                    },
                    samples,
                })?;
            }
        }
        Ok(())
    }

    unsafe fn convert_packet_to_f32(
        data: *const u8,
        frame_count: u32,
        mix: WasapiMixFormat,
    ) -> Result<Vec<f32>, BackendError> {
        let total_samples = frame_count as usize * usize::from(mix.channels);
        match mix.sample_kind {
            WasapiSampleKind::Float32 => {
                let input = std::slice::from_raw_parts(data as *const f32, total_samples);
                Ok(input.to_vec())
            }
            WasapiSampleKind::Pcm16 => {
                let input = std::slice::from_raw_parts(data as *const i16, total_samples);
                Ok(input
                    .iter()
                    .map(|sample| *sample as f32 / i16::MAX as f32)
                    .collect())
            }
            WasapiSampleKind::Pcm24In32 | WasapiSampleKind::Pcm32 => {
                let input = std::slice::from_raw_parts(data as *const i32, total_samples);
                let denom = if mix.bits_per_sample == 24 {
                    8_388_607.0
                } else {
                    i32::MAX as f32
                };
                Ok(input.iter().map(|sample| *sample as f32 / denom).collect())
            }
            WasapiSampleKind::Unsupported => Err(BackendError::AudioUnsupported {
                reason: format!(
                    "WASAPI mix format 不支持: tag={} bits={} block_align={}",
                    mix.format_tag, mix.bits_per_sample, mix.block_align
                ),
            }),
        }
    }

    #[derive(Debug, Clone, Copy)]
    struct WasapiMixFormat {
        format_tag: u16,
        sample_rate: u32,
        channels: u16,
        bits_per_sample: u16,
        valid_bits_per_sample: u16,
        block_align: u16,
        sample_kind: WasapiSampleKind,
    }

    #[derive(Debug, Clone, Copy)]
    enum WasapiSampleKind {
        Float32,
        Pcm16,
        Pcm24In32,
        Pcm32,
        Unsupported,
    }

    #[derive(Debug, Clone, Copy)]
    enum WasapiTimestampMode {
        PacketQpc,
        DevicePosition,
    }

    impl WasapiTimestampMode {
        fn from_env() -> Self {
            match std::env::var("RUST_REPLAY_AUDIO_CLOCK")
                .ok()
                .map(|value| value.trim().to_ascii_lowercase())
                .as_deref()
            {
                Some("device" | "device_position") => Self::DevicePosition,
                _ => Self::PacketQpc,
            }
        }
    }

    impl WasapiMixFormat {
        unsafe fn from_waveformat(ptr: *const WAVEFORMATEX) -> Result<Self, BackendError> {
            let fmt = *ptr;
            let format_tag = fmt.wFormatTag;
            let bits = fmt.wBitsPerSample;
            let sample_rate = fmt.nSamplesPerSec;
            let channels = fmt.nChannels;
            let block_align = fmt.nBlockAlign;
            let mut subformat: Option<GUID> = None;
            let valid_bits = if format_tag == WAVE_FORMAT_EXTENSIBLE as u16 {
                let ext = *(ptr as *const WAVEFORMATEXTENSIBLE);
                subformat = Some(ext.SubFormat);
                ext.Samples.wValidBitsPerSample
            } else {
                bits
            };
            let sample_kind = if format_tag == WAVE_FORMAT_IEEE_FLOAT as u16
                || subformat == Some(KSDATAFORMAT_SUBTYPE_IEEE_FLOAT)
            {
                WasapiSampleKind::Float32
            } else if format_tag == WAVE_FORMAT_PCM as u16 || subformat == Some(pcm_guid()) {
                match (bits, valid_bits) {
                    (16, _) => WasapiSampleKind::Pcm16,
                    (32, 24) => WasapiSampleKind::Pcm24In32,
                    (32, _) => WasapiSampleKind::Pcm32,
                    _ => WasapiSampleKind::Unsupported,
                }
            } else {
                WasapiSampleKind::Unsupported
            };
            if sample_rate == 0 || channels == 0 || block_align == 0 {
                return Err(BackendError::AudioUnsupported {
                    reason: format!(
                        "WASAPI mix format 非法: sample_rate={} channels={} block_align={}",
                        sample_rate, channels, block_align
                    ),
                });
            }
            Ok(Self {
                format_tag,
                sample_rate,
                channels,
                bits_per_sample: bits,
                valid_bits_per_sample: valid_bits,
                block_align,
                sample_kind,
            })
        }
    }

    #[derive(Debug, Default)]
    struct WasapiClockTracker {
        last_device_position: Option<u64>,
        last_frame_count: u32,
        last_valid_qpc_position: Option<u64>,
        last_valid_qpc_device_position: Option<u64>,
        last_qpc_frame_position: Option<u64>,
        last_qpc_frame_count: u32,
        anchor_device_position: Option<u64>,
        anchor_qpc_position: Option<u64>,
    }

    #[derive(Debug, Clone, Copy)]
    struct WasapiClockObservation {
        device_position: u64,
        qpc_position: u64,
        frame_count: u32,
        discontinuity: bool,
        timestamp_error: bool,
        sample_rate: u32,
        timestamp_mode: WasapiTimestampMode,
    }

    impl WasapiClockTracker {
        fn observe(
            &mut self,
            observation: WasapiClockObservation,
            stats: &mut WasapiCaptureStats,
        ) -> i64 {
            let WasapiClockObservation {
                device_position,
                qpc_position,
                frame_count,
                discontinuity,
                timestamp_error,
                sample_rate,
                timestamp_mode,
            } = observation;
            if let Some(previous_position) = self.last_device_position {
                let expected = previous_position.saturating_add(u64::from(self.last_frame_count));
                if device_position > expected {
                    stats.device_gap_packets = stats.device_gap_packets.saturating_add(1);
                    stats.device_gap_frames = stats
                        .device_gap_frames
                        .saturating_add(device_position.saturating_sub(expected));
                } else if device_position < expected {
                    stats.device_overlap_packets = stats.device_overlap_packets.saturating_add(1);
                    stats.device_overlap_frames = stats
                        .device_overlap_frames
                        .saturating_add(expected.saturating_sub(device_position));
                }
            }
            self.last_device_position = Some(device_position);
            self.last_frame_count = frame_count;

            if discontinuity && !timestamp_error {
                self.anchor_device_position = Some(device_position);
                self.anchor_qpc_position = Some(qpc_position);
            }
            if timestamp_error || sample_rate == 0 {
                self.last_valid_qpc_position = None;
                self.last_valid_qpc_device_position = None;
                self.last_qpc_frame_position = None;
                self.last_qpc_frame_count = 0;
                return self.timestamp_from_mode(
                    device_position,
                    qpc_position,
                    sample_rate,
                    timestamp_mode,
                    false,
                );
            }
            let qpc_frame_position =
                ((u128::from(qpc_position) * u128::from(sample_rate) + 5_000_000) / 10_000_000)
                    .min(u64::MAX as u128) as u64;
            if let Some(previous_qpc_frame_position) = self.last_qpc_frame_position {
                let expected = previous_qpc_frame_position
                    .saturating_add(u64::from(self.last_qpc_frame_count));
                if qpc_frame_position > expected {
                    stats.qpc_timeline_gap_packets =
                        stats.qpc_timeline_gap_packets.saturating_add(1);
                    stats.qpc_timeline_gap_frames = stats
                        .qpc_timeline_gap_frames
                        .saturating_add(qpc_frame_position.saturating_sub(expected));
                } else if qpc_frame_position < expected {
                    stats.qpc_timeline_overlap_packets =
                        stats.qpc_timeline_overlap_packets.saturating_add(1);
                    stats.qpc_timeline_overlap_frames = stats
                        .qpc_timeline_overlap_frames
                        .saturating_add(expected.saturating_sub(qpc_frame_position));
                }
            }
            if let (Some(previous_qpc), Some(previous_device_position)) = (
                self.last_valid_qpc_position,
                self.last_valid_qpc_device_position,
            ) {
                let qpc_delta = qpc_position.saturating_sub(previous_qpc);
                let qpc_frames = ((u128::from(qpc_delta) * u128::from(sample_rate) + 5_000_000)
                    / 10_000_000) as i128;
                let device_frames = device_position as i128 - previous_device_position as i128;
                let error = (qpc_frames - device_frames)
                    .unsigned_abs()
                    .min(u64::MAX as u128) as u64;
                if error > 0 {
                    stats.qpc_delta_error_packets = stats.qpc_delta_error_packets.saturating_add(1);
                    stats.qpc_delta_error_abs_frames =
                        stats.qpc_delta_error_abs_frames.saturating_add(error);
                    stats.qpc_delta_error_max_frames = stats.qpc_delta_error_max_frames.max(error);
                }
            }
            self.last_valid_qpc_position = Some(qpc_position);
            self.last_valid_qpc_device_position = Some(device_position);
            self.last_qpc_frame_position = Some(qpc_frame_position);
            self.last_qpc_frame_count = frame_count;
            self.timestamp_from_mode(
                device_position,
                qpc_position,
                sample_rate,
                timestamp_mode,
                true,
            )
        }

        fn timestamp_from_mode(
            &mut self,
            device_position: u64,
            qpc_position: u64,
            sample_rate: u32,
            timestamp_mode: WasapiTimestampMode,
            allow_anchor: bool,
        ) -> i64 {
            if !matches!(timestamp_mode, WasapiTimestampMode::DevicePosition) || sample_rate == 0 {
                return qpc_position.min(i64::MAX as u64) as i64;
            }
            let (anchor_device, anchor_qpc) =
                match (self.anchor_device_position, self.anchor_qpc_position) {
                    (Some(device), Some(qpc)) => (device, qpc),
                    _ if allow_anchor => {
                        self.anchor_device_position = Some(device_position);
                        self.anchor_qpc_position = Some(qpc_position);
                        (device_position, qpc_position)
                    }
                    _ => return qpc_position.min(i64::MAX as u64) as i64,
                };
            let delta_frames = device_position.saturating_sub(anchor_device);
            let delta_100ns = ((u128::from(delta_frames) * 10_000_000
                + u128::from(sample_rate / 2))
                / u128::from(sample_rate))
            .min(i64::MAX as u128) as i64;
            (anchor_qpc.min(i64::MAX as u64) as i64).saturating_add(delta_100ns)
        }
    }

    const fn pcm_guid() -> GUID {
        GUID::from_u128(0x00000001_0000_0010_8000_00aa00389b71)
    }

    struct ComGuard {
        should_uninit: bool,
    }

    impl ComGuard {
        fn init() -> Result<Self, BackendError> {
            let hr = unsafe { CoInitializeEx(None, COINIT_MULTITHREADED) };
            if hr.0 < 0 && hr.0 as u32 != RPC_E_CHANGED_MODE {
                return Err(BackendError::AudioUnsupported {
                    reason: format!("CoInitializeEx(MTA) 失败: HRESULT=0x{:08X}", hr.0 as u32),
                });
            }
            Ok(Self {
                should_uninit: hr.0 >= 0,
            })
        }
    }

    impl Drop for ComGuard {
        fn drop(&mut self) {
            if self.should_uninit {
                unsafe {
                    CoUninitialize();
                }
            }
        }
    }

    fn windows_audio_error(
        func: &'static str,
    ) -> impl FnOnce(windows::core::Error) -> BackendError {
        move |err| BackendError::AudioUnsupported {
            reason: format!("{func} 失败: HRESULT=0x{:08X} {}", err.code().0 as u32, err),
        }
    }

    #[cfg(test)]
    mod tests {
        use super::*;

        #[test]
        #[ignore = "requires Windows audio render/capture endpoints"]
        fn local_default_wasapi_sources_stream_packets() {
            for source in [AudioSourceKind::Loopback, AudioSourceKind::Microphone] {
                let (tx, rx) = std::sync::mpsc::channel();
                let stats = capture_default_streaming(source, Duration::from_secs(2), None, tx)
                    .unwrap_or_else(|err| panic!("{source:?} capture failed: {err}"));
                let frames = rx.into_iter().collect::<Vec<_>>();
                let delivered_frames = frames.iter().map(PcmFrame::frame_count).sum::<usize>();
                eprintln!(
                    "wasapi_source={source:?} clock={} format={}Hz/{}ch packets={} captured_frames={} delivered_frames={} discontinuities={} timestamp_errors={} device_gaps={} device_overlaps={} qpc_gaps={} qpc_overlaps={}",
                    if stats.timestamp_from_device_position {
                        "device_position"
                    } else {
                        "packet_qpc"
                    },
                    stats.sample_rate,
                    stats.channels,
                    stats.packet_count,
                    stats.pcm_frames,
                    delivered_frames,
                    stats.discontinuity_packets,
                    stats.timestamp_error_packets,
                    stats.device_gap_frames,
                    stats.device_overlap_frames,
                    stats.qpc_timeline_gap_frames,
                    stats.qpc_timeline_overlap_frames,
                );
                assert!(stats.packet_count > 0, "{source:?} returned no packets");
                assert_eq!(delivered_frames, stats.pcm_frames);
            }
        }

        #[test]
        fn device_position_timestamps_ignore_packet_qpc_jitter() {
            let mut tracker = WasapiClockTracker::default();
            let mut stats = WasapiCaptureStats::default();
            let first = tracker.observe(
                WasapiClockObservation {
                    device_position: 10_000,
                    qpc_position: 1_000_000,
                    frame_count: 480,
                    discontinuity: false,
                    timestamp_error: false,
                    sample_rate: 48_000,
                    timestamp_mode: WasapiTimestampMode::DevicePosition,
                },
                &mut stats,
            );
            let second = tracker.observe(
                WasapiClockObservation {
                    device_position: 10_480,
                    qpc_position: 1_099_790,
                    frame_count: 480,
                    discontinuity: false,
                    timestamp_error: false,
                    sample_rate: 48_000,
                    timestamp_mode: WasapiTimestampMode::DevicePosition,
                },
                &mut stats,
            );
            let third = tracker.observe(
                WasapiClockObservation {
                    device_position: 10_960,
                    qpc_position: 1_200_210,
                    frame_count: 480,
                    discontinuity: false,
                    timestamp_error: false,
                    sample_rate: 48_000,
                    timestamp_mode: WasapiTimestampMode::DevicePosition,
                },
                &mut stats,
            );

            assert_eq!(second - first, 100_000);
            assert_eq!(third - second, 100_000);
            assert_eq!(stats.device_gap_packets, 0);
            assert_eq!(stats.device_overlap_packets, 0);
            assert!(stats.qpc_timeline_gap_packets > 0);
            assert!(stats.qpc_timeline_overlap_packets > 0);
        }
    }
}

#[cfg(windows)]
pub use platform::capture_default_streaming;

#[cfg(not(windows))]
pub fn capture_default_streaming(
    _source: AudioSourceKind,
    _duration: Duration,
    _stop: Option<Arc<AtomicBool>>,
    _tx: Sender<PcmFrame>,
) -> Result<WasapiCaptureStats, BackendError> {
    Err(BackendError::AudioUnsupported {
        reason: "WASAPI 仅支持 Windows".to_owned(),
    })
}
