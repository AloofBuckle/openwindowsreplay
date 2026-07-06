//! Media Foundation AAC LC 编码器。
//!
//! 视频 raw frame 路径必须 GPU-only；音频在文档中定义为 48kHz stereo float PCM
//! 后进入 AAC LC，因此这里使用系统 Media Foundation AAC encoder MFT，把已经按
//! 绝对时间戳对齐的 `AacPcmBlock` 编成 MP4 可直接封装的裸 AAC access unit。
#![allow(unsafe_op_in_unsafe_fn)]

use crate::backend::audio::{
    AAC_LC_FRAME_SAMPLES, AacPcmBlock, TARGET_CHANNELS, TARGET_SAMPLE_RATE,
};
use crate::backend::mp4_mux::AacAccessUnit;
use crate::error::BackendError;

#[cfg(windows)]
mod platform {
    use super::*;
    use std::mem::ManuallyDrop;
    use std::ptr;
    use windows::Win32::Media::MediaFoundation::{
        IMFActivate, IMFMediaBuffer, IMFSample, IMFTransform, MF_E_TRANSFORM_NEED_MORE_INPUT,
        MF_E_TRANSFORM_STREAM_CHANGE, MF_MT_AAC_PAYLOAD_TYPE, MF_MT_AUDIO_AVG_BYTES_PER_SECOND,
        MF_MT_AUDIO_BITS_PER_SAMPLE, MF_MT_AUDIO_BLOCK_ALIGNMENT, MF_MT_AUDIO_NUM_CHANNELS,
        MF_MT_AUDIO_SAMPLES_PER_SECOND, MF_MT_MAJOR_TYPE, MF_MT_SUBTYPE, MF_VERSION,
        MFAudioFormat_AAC, MFAudioFormat_Float, MFAudioFormat_PCM, MFCreateMediaType,
        MFCreateMemoryBuffer, MFCreateSample, MFMediaType_Audio, MFShutdown, MFStartup,
        MFT_CATEGORY_AUDIO_ENCODER, MFT_ENUM_FLAG_SYNCMFT, MFT_MESSAGE_COMMAND_DRAIN,
        MFT_MESSAGE_NOTIFY_BEGIN_STREAMING, MFT_MESSAGE_NOTIFY_END_OF_STREAM,
        MFT_MESSAGE_NOTIFY_START_OF_STREAM, MFT_OUTPUT_DATA_BUFFER,
        MFT_OUTPUT_STREAM_PROVIDES_SAMPLES, MFT_REGISTER_TYPE_INFO, MFTEnumEx,
    };
    use windows::Win32::System::Com::CoTaskMemFree;

    const HNS_PER_SECOND: i64 = 10_000_000;
    const DEFAULT_AAC_BITRATE: u32 = 192_000;

    pub struct MfAacLcEncoder {
        transform: IMFTransform,
        output_buffer_size: u32,
        provides_output_samples: bool,
        input_format: EncoderPcmInputFormat,
        _mf: MfPlatformGuard,
    }

    #[derive(Debug, Clone, Copy, PartialEq, Eq)]
    enum EncoderPcmInputFormat {
        Float32,
        Pcm16,
    }

    struct MfPlatformGuard;

    impl MfPlatformGuard {
        fn start() -> Result<Self, BackendError> {
            unsafe {
                MFStartup(MF_VERSION, 0).map_err(windows_audio_error("MFStartup"))?;
            }
            Ok(Self)
        }
    }

    impl Drop for MfPlatformGuard {
        fn drop(&mut self) {
            unsafe {
                let _ = MFShutdown();
            }
        }
    }

    impl MfAacLcEncoder {
        pub fn new() -> Result<Self, BackendError> {
            Self::with_bitrate(DEFAULT_AAC_BITRATE)
        }

        pub fn with_bitrate(bitrate_bps: u32) -> Result<Self, BackendError> {
            let mf = MfPlatformGuard::start()?;
            let transform = create_aac_encoder_mft()?;
            unsafe {
                let output_type = make_aac_output_type(bitrate_bps)?;
                transform
                    .SetOutputType(0, &output_type, 0)
                    .map_err(windows_audio_error("IMFTransform::SetOutputType(AAC)"))?;
                let float_input = make_input_type(EncoderPcmInputFormat::Float32)?;
                let input_format =
                    match transform.SetInputType(0, &float_input, 0) {
                        Ok(()) => EncoderPcmInputFormat::Float32,
                        Err(_) => {
                            let pcm16_input = make_input_type(EncoderPcmInputFormat::Pcm16)?;
                            transform.SetInputType(0, &pcm16_input, 0).map_err(
                                windows_audio_error("IMFTransform::SetInputType(PCM16 fallback)"),
                            )?;
                            EncoderPcmInputFormat::Pcm16
                        }
                    };
                transform
                    .ProcessMessage(MFT_MESSAGE_NOTIFY_BEGIN_STREAMING, 0)
                    .map_err(windows_audio_error("IMFTransform::BEGIN_STREAMING"))?;
                transform
                    .ProcessMessage(MFT_MESSAGE_NOTIFY_START_OF_STREAM, 0)
                    .map_err(windows_audio_error("IMFTransform::START_OF_STREAM"))?;
                let stream_info = transform
                    .GetOutputStreamInfo(0)
                    .map_err(windows_audio_error("IMFTransform::GetOutputStreamInfo"))?;
                let provides_output_samples =
                    (stream_info.dwFlags & MFT_OUTPUT_STREAM_PROVIDES_SAMPLES.0 as u32) != 0;
                Ok(Self {
                    transform,
                    output_buffer_size: stream_info.cbSize.max(4096),
                    provides_output_samples,
                    input_format,
                    _mf: mf,
                })
            }
        }

        pub fn encode_block(
            &mut self,
            block: &AacPcmBlock,
        ) -> Result<Vec<AacAccessUnit>, BackendError> {
            if block.interleaved.len() != AAC_LC_FRAME_SAMPLES * usize::from(TARGET_CHANNELS) {
                return Err(BackendError::AudioUnsupported {
                    reason: format!(
                        "AAC encoder 期望 1024 stereo float frame，实际 interleaved samples={}",
                        block.interleaved.len()
                    ),
                });
            }
            let sample = make_input_sample(block, self.input_format)?;
            unsafe {
                self.transform
                    .ProcessInput(0, &sample, 0)
                    .map_err(windows_audio_error("IMFTransform::ProcessInput(AAC)"))?;
            }
            self.drain_available_output(false)
        }

        pub fn finish(mut self) -> Result<Vec<AacAccessUnit>, BackendError> {
            unsafe {
                self.transform
                    .ProcessMessage(MFT_MESSAGE_NOTIFY_END_OF_STREAM, 0)
                    .map_err(windows_audio_error("IMFTransform::END_OF_STREAM"))?;
                self.transform
                    .ProcessMessage(MFT_MESSAGE_COMMAND_DRAIN, 0)
                    .map_err(windows_audio_error("IMFTransform::DRAIN"))?;
            }
            self.drain_available_output(true)
        }

        fn drain_available_output(
            &mut self,
            draining: bool,
        ) -> Result<Vec<AacAccessUnit>, BackendError> {
            let mut out = Vec::new();
            loop {
                match self.process_output_once() {
                    Ok(Some(unit)) => out.push(unit),
                    Ok(None) => {
                        if draining {
                            continue;
                        }
                        break;
                    }
                    Err(err) if err.code() == MF_E_TRANSFORM_NEED_MORE_INPUT => break,
                    Err(err) if err.code() == MF_E_TRANSFORM_STREAM_CHANGE => {
                        return Err(BackendError::AudioUnsupported {
                            reason: "Media Foundation AAC encoder 请求 stream change；当前固定 AAC LC 输出类型未实现动态重协商".to_owned(),
                        });
                    }
                    Err(err) => {
                        return Err(windows_audio_error("IMFTransform::ProcessOutput(AAC)")(err));
                    }
                }
            }
            Ok(out)
        }

        fn process_output_once(&mut self) -> Result<Option<AacAccessUnit>, windows::core::Error> {
            unsafe {
                let sample = if self.provides_output_samples {
                    None
                } else {
                    Some(make_empty_output_sample(self.output_buffer_size)?)
                };
                let mut output = MFT_OUTPUT_DATA_BUFFER {
                    dwStreamID: 0,
                    pSample: ManuallyDrop::new(sample),
                    dwStatus: 0,
                    pEvents: ManuallyDrop::new(None),
                };
                let mut status = 0u32;
                let result =
                    self.transform
                        .ProcessOutput(0, std::slice::from_mut(&mut output), &mut status);
                let maybe_sample = ManuallyDrop::take(&mut output.pSample);
                let _ = ManuallyDrop::take(&mut output.pEvents);
                result?;
                let Some(sample) = maybe_sample else {
                    return Ok(None);
                };
                let data = copy_sample_bytes(&sample)?;
                if data.is_empty() {
                    return Ok(None);
                }
                let timestamp_100ns = sample.GetSampleTime().unwrap_or(0).max(0);
                let duration_100ns = sample
                    .GetSampleDuration()
                    .unwrap_or_else(|_| ticks_to_100ns(AAC_LC_FRAME_SAMPLES as u64))
                    .max(1);
                Ok(Some(AacAccessUnit {
                    timestamp_ticks: time_100ns_to_ticks(timestamp_100ns),
                    duration_ticks: time_100ns_to_ticks(duration_100ns).max(1) as u32,
                    data,
                }))
            }
        }
    }

    fn create_aac_encoder_mft() -> Result<IMFTransform, BackendError> {
        unsafe {
            let output_info = MFT_REGISTER_TYPE_INFO {
                guidMajorType: MFMediaType_Audio,
                guidSubtype: MFAudioFormat_AAC,
            };
            let mut raw: *mut Option<IMFActivate> = ptr::null_mut();
            let mut count = 0u32;
            MFTEnumEx(
                MFT_CATEGORY_AUDIO_ENCODER,
                MFT_ENUM_FLAG_SYNCMFT,
                None,
                Some(&output_info),
                &mut raw,
                &mut count,
            )
            .map_err(windows_audio_error("MFTEnumEx(AAC encoder)"))?;
            if raw.is_null() || count == 0 {
                return Err(BackendError::AudioUnsupported {
                    reason: "系统 Media Foundation 没有枚举到 AAC audio encoder MFT".to_owned(),
                });
            }
            let mut activates = Vec::with_capacity(count as usize);
            for index in 0..count as usize {
                activates.push(ptr::read(raw.add(index)));
            }
            CoTaskMemFree(Some(raw as _));
            let activate = activates.into_iter().flatten().next().ok_or_else(|| {
                BackendError::AudioUnsupported {
                    reason: "AAC encoder MFT 枚举结果为空".to_owned(),
                }
            })?;
            let transform = activate
                .ActivateObject::<IMFTransform>()
                .map_err(windows_audio_error("IMFActivate::ActivateObject(AAC)"))?;
            let _ = activate.ShutdownObject();
            Ok(transform)
        }
    }

    unsafe fn make_aac_output_type(
        bitrate_bps: u32,
    ) -> Result<windows::Win32::Media::MediaFoundation::IMFMediaType, BackendError> {
        let media_type =
            MFCreateMediaType().map_err(windows_audio_error("MFCreateMediaType(AAC output)"))?;
        media_type
            .SetGUID(&MF_MT_MAJOR_TYPE, &MFMediaType_Audio)
            .map_err(windows_audio_error("AAC output MF_MT_MAJOR_TYPE"))?;
        media_type
            .SetGUID(&MF_MT_SUBTYPE, &MFAudioFormat_AAC)
            .map_err(windows_audio_error("AAC output MF_MT_SUBTYPE"))?;
        media_type
            .SetUINT32(&MF_MT_AUDIO_NUM_CHANNELS, TARGET_CHANNELS as u32)
            .map_err(windows_audio_error("AAC output channels"))?;
        media_type
            .SetUINT32(&MF_MT_AUDIO_SAMPLES_PER_SECOND, TARGET_SAMPLE_RATE)
            .map_err(windows_audio_error("AAC output sample rate"))?;
        media_type
            .SetUINT32(&MF_MT_AUDIO_BITS_PER_SAMPLE, 16)
            .map_err(windows_audio_error("AAC output bits"))?;
        media_type
            .SetUINT32(&MF_MT_AUDIO_AVG_BYTES_PER_SECOND, bitrate_bps / 8)
            .map_err(windows_audio_error("AAC output avg bytes/sec"))?;
        media_type
            .SetUINT32(&MF_MT_AAC_PAYLOAD_TYPE, 0)
            .map_err(windows_audio_error("AAC output payload type"))?;
        Ok(media_type)
    }

    unsafe fn make_input_type(
        input_format: EncoderPcmInputFormat,
    ) -> Result<windows::Win32::Media::MediaFoundation::IMFMediaType, BackendError> {
        let media_type =
            MFCreateMediaType().map_err(windows_audio_error("MFCreateMediaType(PCM input)"))?;
        media_type
            .SetGUID(&MF_MT_MAJOR_TYPE, &MFMediaType_Audio)
            .map_err(windows_audio_error("PCM input MF_MT_MAJOR_TYPE"))?;
        media_type
            .SetGUID(
                &MF_MT_SUBTYPE,
                match input_format {
                    EncoderPcmInputFormat::Float32 => &MFAudioFormat_Float,
                    EncoderPcmInputFormat::Pcm16 => &MFAudioFormat_PCM,
                },
            )
            .map_err(windows_audio_error("PCM input MF_MT_SUBTYPE"))?;
        let bytes_per_sample = match input_format {
            EncoderPcmInputFormat::Float32 => 4,
            EncoderPcmInputFormat::Pcm16 => 2,
        };
        let bits_per_sample = bytes_per_sample * 8;
        media_type
            .SetUINT32(&MF_MT_AUDIO_NUM_CHANNELS, TARGET_CHANNELS as u32)
            .map_err(windows_audio_error("PCM input channels"))?;
        media_type
            .SetUINT32(&MF_MT_AUDIO_SAMPLES_PER_SECOND, TARGET_SAMPLE_RATE)
            .map_err(windows_audio_error("PCM input sample rate"))?;
        media_type
            .SetUINT32(&MF_MT_AUDIO_BITS_PER_SAMPLE, bits_per_sample)
            .map_err(windows_audio_error("PCM input bits"))?;
        media_type
            .SetUINT32(
                &MF_MT_AUDIO_BLOCK_ALIGNMENT,
                TARGET_CHANNELS as u32 * bytes_per_sample,
            )
            .map_err(windows_audio_error("PCM input block alignment"))?;
        media_type
            .SetUINT32(
                &MF_MT_AUDIO_AVG_BYTES_PER_SECOND,
                TARGET_SAMPLE_RATE * TARGET_CHANNELS as u32 * bytes_per_sample,
            )
            .map_err(windows_audio_error("PCM input avg bytes/sec"))?;
        Ok(media_type)
    }

    fn make_input_sample(
        block: &AacPcmBlock,
        input_format: EncoderPcmInputFormat,
    ) -> Result<IMFSample, BackendError> {
        unsafe {
            let pcm16;
            let bytes = if input_format == EncoderPcmInputFormat::Float32 {
                std::slice::from_raw_parts(
                    block.interleaved.as_ptr() as *const u8,
                    block.interleaved.len() * std::mem::size_of::<f32>(),
                )
            } else {
                pcm16 = block
                    .interleaved
                    .iter()
                    .map(|sample| (sample.clamp(-1.0, 1.0) * i16::MAX as f32).round() as i16)
                    .collect::<Vec<_>>();
                std::slice::from_raw_parts(
                    pcm16.as_ptr() as *const u8,
                    pcm16.len() * std::mem::size_of::<i16>(),
                )
            };
            let buffer = MFCreateMemoryBuffer(bytes.len() as u32)
                .map_err(windows_audio_error("MFCreateMemoryBuffer(input PCM)"))?;
            write_buffer(&buffer, bytes)?;
            let sample = MFCreateSample().map_err(windows_audio_error("MFCreateSample(input)"))?;
            sample
                .AddBuffer(&buffer)
                .map_err(windows_audio_error("IMFSample::AddBuffer(input)"))?;
            sample
                .SetSampleTime(ticks_to_100ns(block.timestamp_ticks))
                .map_err(windows_audio_error("IMFSample::SetSampleTime(input)"))?;
            sample
                .SetSampleDuration(ticks_to_100ns(AAC_LC_FRAME_SAMPLES as u64))
                .map_err(windows_audio_error("IMFSample::SetSampleDuration(input)"))?;
            Ok(sample)
        }
    }

    unsafe fn make_empty_output_sample(max_bytes: u32) -> Result<IMFSample, windows::core::Error> {
        let buffer = MFCreateMemoryBuffer(max_bytes)?;
        let sample = MFCreateSample()?;
        sample.AddBuffer(&buffer)?;
        Ok(sample)
    }

    unsafe fn write_buffer(buffer: &IMFMediaBuffer, bytes: &[u8]) -> Result<(), BackendError> {
        let mut ptr = ptr::null_mut::<u8>();
        buffer
            .Lock(&mut ptr, None, None)
            .map_err(windows_audio_error("IMFMediaBuffer::Lock(input)"))?;
        ptr.copy_from_nonoverlapping(bytes.as_ptr(), bytes.len());
        buffer
            .Unlock()
            .map_err(windows_audio_error("IMFMediaBuffer::Unlock(input)"))?;
        buffer
            .SetCurrentLength(bytes.len() as u32)
            .map_err(windows_audio_error(
                "IMFMediaBuffer::SetCurrentLength(input)",
            ))?;
        Ok(())
    }

    unsafe fn copy_sample_bytes(sample: &IMFSample) -> Result<Vec<u8>, windows::core::Error> {
        let buffer = sample.ConvertToContiguousBuffer()?;
        let length = buffer.GetCurrentLength()? as usize;
        if length == 0 {
            return Ok(Vec::new());
        }
        let mut ptr = ptr::null_mut::<u8>();
        let mut current = 0u32;
        buffer.Lock(&mut ptr, None, Some(&mut current))?;
        let data = std::slice::from_raw_parts(ptr, current as usize).to_vec();
        buffer.Unlock()?;
        Ok(data)
    }

    fn ticks_to_100ns(ticks: u64) -> i64 {
        ((ticks as u128 * HNS_PER_SECOND as u128) / TARGET_SAMPLE_RATE as u128) as i64
    }

    fn time_100ns_to_ticks(time_100ns: i64) -> u64 {
        if time_100ns <= 0 {
            return 0;
        }
        ((time_100ns as i128 * TARGET_SAMPLE_RATE as i128 + HNS_PER_SECOND as i128 / 2)
            / HNS_PER_SECOND as i128) as u64
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
pub use platform::MfAacLcEncoder;

#[cfg(not(windows))]
pub struct MfAacLcEncoder;

#[cfg(not(windows))]
impl MfAacLcEncoder {
    pub fn new() -> Result<Self, BackendError> {
        Err(BackendError::AudioUnsupported {
            reason: "Media Foundation AAC encoder 仅支持 Windows".to_owned(),
        })
    }

    pub fn encode_block(
        &mut self,
        _block: &AacPcmBlock,
    ) -> Result<Vec<AacAccessUnit>, BackendError> {
        Err(BackendError::AudioUnsupported {
            reason: "Media Foundation AAC encoder 仅支持 Windows".to_owned(),
        })
    }

    pub fn finish(self) -> Result<Vec<AacAccessUnit>, BackendError> {
        Err(BackendError::AudioUnsupported {
            reason: "Media Foundation AAC encoder 仅支持 Windows".to_owned(),
        })
    }
}
