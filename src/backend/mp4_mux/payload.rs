use super::*;

pub(super) fn make_ftyp() -> Vec<u8> {
    let mut p = Vec::new();
    p.extend_from_slice(b"mp42");
    p.extend_from_slice(&0u32.to_be_bytes());
    p.extend_from_slice(b"isom");
    p.extend_from_slice(b"mp42");
    mp4_box(*b"ftyp", p)
}

pub(super) fn make_mdat_header(payload_len: u64) -> Vec<u8> {
    let total = payload_len.saturating_add(8);
    if total <= u32::MAX as u64 {
        let mut out = Vec::with_capacity(8);
        be32(&mut out, total as u32);
        out.extend_from_slice(b"mdat");
        out
    } else {
        let mut out = Vec::with_capacity(16);
        be32(&mut out, 1);
        out.extend_from_slice(b"mdat");
        be64(&mut out, payload_len.saturating_add(16));
        out
    }
}

pub(super) fn write_mdat_payload(
    out: &mut impl Write,
    video_samples: &[PreparedSample],
    audio_samples: Option<&[PreparedAudioSample]>,
) -> Result<(), BackendError> {
    let mut cache = FileRangeReadCache::default();
    for sample in video_samples {
        write_sample_payload(out, &sample.data, &mut cache)?;
    }
    if let Some(audio_samples) = audio_samples {
        for sample in audio_samples {
            write_sample_payload(out, &sample.data, &mut cache)?;
        }
    }
    Ok(())
}

#[derive(Default)]
pub(super) struct FileRangeReadCache {
    pub(super) path: Option<PathBuf>,
    pub(super) file: Option<BufReader<File>>,
}

pub(super) fn write_sample_payload(
    out: &mut impl Write,
    payload: &SamplePayload,
    cache: &mut FileRangeReadCache,
) -> Result<(), BackendError> {
    match payload {
        SamplePayload::Memory(data) => out
            .write_all(data)
            .map_err(|err| BackendError::Io(err.to_string())),
        SamplePayload::FileRange(range) => {
            let needs_open = cache.path.as_ref() != Some(&range.path);
            if needs_open {
                let file =
                    File::open(&range.path).map_err(|err| BackendError::Io(err.to_string()))?;
                cache.file = Some(BufReader::with_capacity(1024 * 1024, file));
                cache.path = Some(range.path.clone());
            }
            let file = cache.file.as_mut().ok_or_else(|| {
                BackendError::unsupported(
                    "MP4 封装",
                    range.path.display().to_string(),
                    "file range reader 未初始化",
                )
            })?;
            file.seek(SeekFrom::Start(range.offset))
                .map_err(|err| BackendError::Io(err.to_string()))?;
            let copied = io::copy(&mut file.take(range.len), out)
                .map_err(|err| BackendError::Io(err.to_string()))?;
            if copied != range.len {
                return Err(BackendError::unsupported(
                    "MP4 封装",
                    range.path.display().to_string(),
                    format!(
                        "file range 读取长度不匹配：offset={} expected={} copied={}",
                        range.offset, range.len, copied
                    ),
                ));
            }
            Ok(())
        }
    }
}
