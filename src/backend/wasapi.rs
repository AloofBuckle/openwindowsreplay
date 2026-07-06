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
}

#[cfg(windows)]
mod platform {
    use super::*;
    use windows::Win32::Media::Audio::{
        AUDCLNT_BUFFERFLAGS_SILENT, AUDCLNT_SHAREMODE_SHARED, AUDCLNT_STREAMFLAGS_LOOPBACK,
        IAudioCaptureClient, IAudioClient, IMMDeviceEnumerator, MMDeviceEnumerator,
        WAVE_FORMAT_PCM, WAVEFORMATEX, WAVEFORMATEXTENSIBLE, eCapture, eConsole, eRender,
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
        capture_default_with_sink(source, duration, stop, |frame| {
            stats.packet_count += 1;
            stats.pcm_frames += frame.frame_count();
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
            let start = Instant::now();
            while start.elapsed() < duration
                && !stop
                    .as_ref()
                    .is_some_and(|flag| flag.load(std::sync::atomic::Ordering::Relaxed))
            {
                drain_packets(&capture, source, mix, &mut on_frame)?;
                std::thread::sleep(POLL_SLEEP);
            }
            drain_packets(&capture, source, mix, &mut on_frame)?;
            let _ = client.Stop();
            Ok(())
        }
    }

    unsafe fn drain_packets<F>(
        capture: &IAudioCaptureClient,
        source: AudioSourceKind,
        mix: WasapiMixFormat,
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
                    start_time_100ns: qpc_position.min(i64::MAX as u64) as i64,
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
                block_align,
                sample_kind,
            })
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
