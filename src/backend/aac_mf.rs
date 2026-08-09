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
    use std::collections::VecDeque;
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
    const MAX_DRAIN_NO_PROGRESS: usize = 8;
    const PRIME_SILENCE_BLOCKS: u32 = 4;

    pub struct MfAacLcEncoder {
        transform: IMFTransform,
        output_buffer_size: u32,
        provides_output_samples: bool,
        input_format: EncoderPcmInputFormat,
        pending_timestamps: VecDeque<PendingTimestamp>,
        transport_offset_ticks: u64,
        _mf: MfPlatformGuard,
    }

    #[derive(Debug, Clone, Copy)]
    struct PendingTimestamp {
        transport_ticks: u64,
        presentation_ticks: Option<u64>,
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
                    pending_timestamps: VecDeque::new(),
                    transport_offset_ticks: 0,
                    _mf: mf,
                })
            }
        }

        pub fn encode_block(
            &mut self,
            block: &AacPcmBlock,
        ) -> Result<Vec<AacAccessUnit>, BackendError> {
            let transport_ticks = block
                .timestamp_ticks
                .checked_add(self.transport_offset_ticks)
                .ok_or_else(|| BackendError::AudioUnsupported {
                    reason: format!(
                        "AAC transport timestamp 溢出: presentation={} offset={}",
                        block.timestamp_ticks, self.transport_offset_ticks
                    ),
                })?;
            self.submit_block(block, transport_ticks, Some(block.timestamp_ticks))?;
            self.drain_available_output(false)
        }

        /// Warm the Media Foundation transform before live capture begins without
        /// exposing the discarded AAC access units on the presentation timeline.
        pub fn prime_silence(&mut self) -> Result<u32, BackendError> {
            if self.transport_offset_ticks != 0 || !self.pending_timestamps.is_empty() {
                return Err(BackendError::AudioUnsupported {
                    reason: "AAC encoder 只能在正式输入前预热一次".to_owned(),
                });
            }
            for index in 0..PRIME_SILENCE_BLOCKS {
                let transport_ticks = u64::from(index) * AAC_LC_FRAME_SAMPLES as u64;
                let block = AacPcmBlock {
                    timestamp_ticks: transport_ticks,
                    interleaved: vec![0.0; AAC_LC_FRAME_SAMPLES * usize::from(TARGET_CHANNELS)],
                };
                self.submit_block(&block, transport_ticks, None)?;
                let discarded = self.drain_available_output(false)?;
                debug_assert!(discarded.is_empty());
            }
            self.transport_offset_ticks = u64::from(PRIME_SILENCE_BLOCKS)
                .checked_mul(AAC_LC_FRAME_SAMPLES as u64)
                .ok_or_else(|| BackendError::AudioUnsupported {
                    reason: "AAC prime transport offset 溢出".to_owned(),
                })?;
            Ok(PRIME_SILENCE_BLOCKS)
        }

        fn submit_block(
            &mut self,
            block: &AacPcmBlock,
            transport_ticks: u64,
            presentation_ticks: Option<u64>,
        ) -> Result<(), BackendError> {
            if block.interleaved.len() != AAC_LC_FRAME_SAMPLES * usize::from(TARGET_CHANNELS) {
                return Err(BackendError::AudioUnsupported {
                    reason: format!(
                        "AAC encoder 期望 1024 stereo float frame，实际 interleaved samples={}",
                        block.interleaved.len()
                    ),
                });
            }
            let sample = make_input_sample(block, transport_ticks, self.input_format)?;
            unsafe {
                self.transform
                    .ProcessInput(0, &sample, 0)
                    .map_err(windows_audio_error("IMFTransform::ProcessInput(AAC)"))?;
            }
            self.pending_timestamps.push_back(PendingTimestamp {
                transport_ticks,
                presentation_ticks,
            });
            Ok(())
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
            let output = self.drain_available_output(true)?;
            if !self.pending_timestamps.is_empty() {
                return Err(BackendError::AudioUnsupported {
                    reason: format!(
                        "Media Foundation AAC drain 后仍有 {} 个输入时间戳没有对应输出",
                        self.pending_timestamps.len()
                    ),
                });
            }
            Ok(output)
        }

        fn drain_available_output(
            &mut self,
            draining: bool,
        ) -> Result<Vec<AacAccessUnit>, BackendError> {
            let mut out = Vec::new();
            let mut no_progress = 0usize;
            loop {
                match self.process_output_once() {
                    Ok(MfOutput::Unit(unit)) => {
                        no_progress = 0;
                        out.push(unit);
                    }
                    Ok(MfOutput::Discarded) => {
                        no_progress = 0;
                    }
                    Ok(MfOutput::NoSample) if draining => {
                        no_progress = no_progress.saturating_add(1);
                        if no_progress >= MAX_DRAIN_NO_PROGRESS {
                            return Err(BackendError::AudioUnsupported {
                                reason: format!(
                                    "Media Foundation AAC drain 连续 {no_progress} 次成功但没有输出，已中止以避免停止路径死循环"
                                ),
                            });
                        }
                    }
                    Ok(MfOutput::NoSample | MfOutput::NeedMoreInput) => break,
                    Ok(MfOutput::StreamChange) => {
                        return Err(BackendError::AudioUnsupported {
                            reason: "Media Foundation AAC encoder 请求 stream change；当前固定 AAC LC 输出类型未实现动态重协商".to_owned(),
                        });
                    }
                    Err(err) => return Err(err),
                }
            }
            Ok(out)
        }

        fn process_output_once(&mut self) -> Result<MfOutput, BackendError> {
            unsafe {
                let sample = if self.provides_output_samples {
                    None
                } else {
                    Some(
                        make_empty_output_sample(self.output_buffer_size)
                            .map_err(windows_audio_error("MFCreateSample(AAC output)"))?,
                    )
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
                if let Err(err) = result {
                    return if err.code() == MF_E_TRANSFORM_NEED_MORE_INPUT {
                        Ok(MfOutput::NeedMoreInput)
                    } else if err.code() == MF_E_TRANSFORM_STREAM_CHANGE {
                        Ok(MfOutput::StreamChange)
                    } else {
                        Err(windows_audio_error("IMFTransform::ProcessOutput(AAC)")(err))
                    };
                }
                let Some(sample) = maybe_sample else {
                    return Ok(MfOutput::NoSample);
                };
                let data = copy_sample_bytes(&sample)
                    .map_err(windows_audio_error("IMFSample AAC output buffer"))?;
                if data.is_empty() {
                    return Ok(MfOutput::NoSample);
                }
                let pending = self.pending_timestamps.pop_front().ok_or_else(|| {
                    BackendError::AudioUnsupported {
                        reason: "Media Foundation AAC 输出没有对应的已提交源时间戳".to_owned(),
                    }
                })?;
                if let Ok(output_timestamp_100ns) = sample.GetSampleTime() {
                    if output_timestamp_100ns < 0 {
                        return Err(BackendError::AudioUnsupported {
                            reason: format!(
                                "Media Foundation AAC 输出负时间戳: {output_timestamp_100ns}"
                            ),
                        });
                    }
                    let output_ticks = time_100ns_to_ticks(output_timestamp_100ns);
                    if output_ticks.abs_diff(pending.transport_ticks) > 1 {
                        return Err(BackendError::AudioUnsupported {
                            reason: format!(
                                "Media Foundation AAC 输出 PTS 与 transport ledger 不匹配: source_ticks={} mft_ticks={output_ticks}",
                                pending.transport_ticks
                            ),
                        });
                    }
                }
                if let Ok(output_duration_100ns) = sample.GetSampleDuration() {
                    if output_duration_100ns <= 0 {
                        return Err(BackendError::AudioUnsupported {
                            reason: format!(
                                "Media Foundation AAC 输出 duration 非正数: {output_duration_100ns}"
                            ),
                        });
                    }
                    let output_duration_ticks = time_100ns_to_ticks(output_duration_100ns);
                    if output_duration_ticks.abs_diff(AAC_LC_FRAME_SAMPLES as u64) > 1 {
                        return Err(BackendError::AudioUnsupported {
                            reason: format!(
                                "Media Foundation AAC 输出 duration 与 AAC-LC frame 不匹配: expected_ticks={} mft_ticks={output_duration_ticks}",
                                AAC_LC_FRAME_SAMPLES
                            ),
                        });
                    }
                }
                Ok(match pending.presentation_ticks {
                    Some(timestamp_ticks) => MfOutput::Unit(AacAccessUnit {
                        timestamp_ticks,
                        duration_ticks: AAC_LC_FRAME_SAMPLES as u32,
                        data: data.into(),
                    }),
                    None => MfOutput::Discarded,
                })
            }
        }
    }

    enum MfOutput {
        Unit(AacAccessUnit),
        Discarded,
        NoSample,
        NeedMoreInput,
        StreamChange,
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
        timestamp_ticks: u64,
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
                .SetSampleTime(ticks_to_100ns(timestamp_ticks))
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

    #[cfg(test)]
    mod tests {
        use super::*;

        #[test]
        fn media_foundation_aac_preprime_preserves_presentation_ledger() {
            let mut encoder = MfAacLcEncoder::new().unwrap();
            assert_eq!(encoder.prime_silence().unwrap(), PRIME_SILENCE_BLOCKS);

            let mut output = Vec::new();
            for index in 0..3 {
                output.extend(
                    encoder
                        .encode_block(&AacPcmBlock {
                            timestamp_ticks: index as u64 * AAC_LC_FRAME_SAMPLES as u64,
                            interleaved: vec![
                                0.0;
                                AAC_LC_FRAME_SAMPLES * usize::from(TARGET_CHANNELS)
                            ],
                        })
                        .unwrap(),
                );
            }
            output.extend(encoder.finish().unwrap());

            assert_eq!(output.len(), 3);
            for (index, sample) in output.iter().enumerate() {
                assert_eq!(
                    sample.timestamp_ticks,
                    index as u64 * AAC_LC_FRAME_SAMPLES as u64
                );
                assert_eq!(sample.duration_ticks, AAC_LC_FRAME_SAMPLES as u32);
                assert!(!sample.data.is_empty());
            }
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

    pub fn prime_silence(&mut self) -> Result<u32, BackendError> {
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
