use super::*;

#[test]
fn disk_segment_sidecar_roundtrips_tracks() {
    let dir = unique_temp_dir("sidecar_roundtrip");
    fs::create_dir_all(&dir).unwrap();
    let segment = segment(0, 90_000, 0, 48_000);
    let indexed = write_indexed_segment(&dir, "segment", &segment);

    let read = read_disk_segment_sidecar(&indexed_sidecar(&dir, "segment")).unwrap();

    assert_eq!(read.video_track.width, segment.video_track.width);
    assert_eq!(read.video_track.duration_90k, 90_000);
    assert_eq!(read.video_track.samples.len(), 1);
    assert_eq!(
        read.audio_track.as_ref().unwrap().duration_ticks,
        segment.audio_track.as_ref().unwrap().duration_ticks
    );
    assert!(
        fs::metadata(indexed_sidecar(&dir, "segment"))
            .unwrap()
            .len()
            < fs::metadata(indexed.mp4_path).unwrap().len()
    );
    let _ = fs::remove_dir_all(dir);
}

#[test]
fn disk_segment_concat_rebases_timestamps() {
    let dir = unique_temp_dir("concat_index");
    fs::create_dir_all(&dir).unwrap();
    let first = write_indexed_segment(&dir, "first", &segment(0, 90_000, 0, 48_000));
    let second = write_indexed_segment(&dir, "second", &segment(0, 45_000, 0, 24_000));

    let snapshot = concat_disk_indexed_segments(&[first, second])
        .unwrap()
        .unwrap();

    assert_eq!(snapshot.video_track.duration_90k, 135_000);
    assert_eq!(snapshot.video_track.samples[0].duration_90k, 90_000);
    assert_eq!(snapshot.video_track.samples[1].duration_90k, 45_000);
    let audio = snapshot.audio_track.unwrap();
    assert_eq!(audio.duration_ticks, 72_000);
    assert_eq!(audio.samples.len(), 4);
    let out = dir.join("final.mp4");
    crate::backend::mp4_mux::write_prepared_hevc_aac_mp4(&out, &snapshot.video_track, Some(&audio))
        .unwrap();
    assert!(fs::metadata(&out).unwrap().len() > 0);
    let _ = fs::remove_dir_all(dir);
}

#[test]
fn disk_segment_concat_rejects_incompatible_video_epoch() {
    let dir = unique_temp_dir("concat_incompatible_video");
    fs::create_dir_all(&dir).unwrap();
    let first = write_indexed_segment(&dir, "first", &segment(0, 90_000, 0, 48_000));
    let mut second = write_indexed_segment(&dir, "second", &segment(0, 90_000, 0, 48_000));
    second.index.video_track.width = 32;

    let err = concat_disk_indexed_segments(&[first, second]).unwrap_err();

    assert!(err.to_string().contains("保存 epoch 不一致"));
    let _ = fs::remove_dir_all(dir);
}

#[test]
fn disk_segment_concat_rejects_non_key_segment_start() {
    let dir = unique_temp_dir("concat_non_key");
    fs::create_dir_all(&dir).unwrap();
    let first = write_indexed_segment(&dir, "first", &segment(0, 90_000, 0, 48_000));
    let mut second = write_indexed_segment(&dir, "second", &segment(0, 90_000, 0, 48_000));
    second.index.video_track.samples[0].is_sync = false;

    let err = concat_disk_indexed_segments(&[first, second]).unwrap_err();

    assert!(err.to_string().contains("首个视频 sample 不是关键帧"));
    let _ = fs::remove_dir_all(dir);
}

#[test]
fn disk_store_selects_whole_recent_segments() {
    let mut store = DiskReplayStore::new(
        unique_temp_dir("select_recent"),
        Duration::from_secs(25),
        Duration::from_secs(10),
    );
    for index in 0..5 {
        store.segments.push_back(DiskSegmentMeta {
            index,
            mp4_path: PathBuf::from(format!("{index}.mp4")),
            sidecar_path: PathBuf::from(format!("{index}.rrseg")),
            duration_90k: 10 * VIDEO_CLOCK_HZ,
            audio_access_units: 1,
            bytes: 1,
            lease: Arc::new(()),
        });
    }

    let selected = store.select_recent_segments(Duration::from_secs(25));

    assert_eq!(
        selected
            .iter()
            .map(|segment| segment.index)
            .collect::<Vec<_>>(),
        vec![2, 3, 4]
    );

    let selected = store.select_recent_segments_after(Duration::from_secs(25), Some(3));
    assert_eq!(
        selected
            .iter()
            .map(|segment| segment.index)
            .collect::<Vec<_>>(),
        vec![4]
    );
}

#[test]
fn disk_segment_builder_keeps_audio_duration_aligned_to_video_segment() {
    let metadata = EncodedReplayMetadata {
        width: 16,
        height: 16,
        color: NclxColorMetadata::bt709_full(),
        codec: HevcCodecMetadata::main_420_8(),
        audio_sample_rate: 48_000,
        audio_channel_count: 2,
    };
    let mut builder = DiskSegmentBuilder::new(metadata, 90_000, 48_000);
    builder.end_90k = Some(180_000);
    builder.push_video(&HevcAccessUnit {
        timestamp_90k: 90_000,
        data: vec![0, 0, 1, 38, 1].into(),
        is_sync: true,
        discard_from_track: false,
    });
    builder.push_audio(&AacAccessUnit {
        timestamp_ticks: 48_000,
        duration_ticks: 1024,
        data: vec![0x21, 0x10].into(),
    });

    let segment = builder.into_tracks(&[]).unwrap();

    assert_eq!(segment.audio_track.unwrap().duration_ticks, 48_000);
}

#[test]
fn disk_segment_sink_fails_instead_of_blocking_when_writer_queue_is_full() {
    let (writer_tx, _writer_rx) = mpsc::sync_channel(1);
    writer_tx
        .try_send(DiskSegmentWriteJob {
            run_index: 0,
            segment: segment(0, 90_000, 0, 48_000),
            enqueued_at: Instant::now(),
        })
        .unwrap();
    let (event_tx, event_rx) = mpsc::channel();
    let stop = Arc::new(AtomicBool::new(false));
    let mut sink = DiskSegmentSink::new(
        writer_tx,
        event_tx,
        0,
        Duration::from_secs(60),
        stop.clone(),
    );
    let mut builder = DiskSegmentBuilder::new(metadata(), 0, 0);
    builder.end_90k = Some(90_000);
    builder.push_video(&HevcAccessUnit {
        timestamp_90k: 0,
        data: fake_hevc_idr().into(),
        is_sync: true,
        discard_from_track: false,
    });

    sink.write_completed_segment(builder);

    assert!(stop.load(Ordering::Relaxed));
    assert!(matches!(event_rx.recv().unwrap(), ReplayEvent::Error(_)));
}

#[test]
fn disk_store_prepare_directory_removes_only_replay_artifacts() {
    let dir = unique_temp_dir("cleanup_stale");
    fs::create_dir_all(&dir).unwrap();
    fs::write(dir.join("rustreplay_segment_old.mp4"), b"x").unwrap();
    fs::write(dir.join("rustreplay_segment_old.rrseg.part"), b"x").unwrap();
    fs::write(dir.join("keep.txt"), b"x").unwrap();

    DiskReplayStore::prepare_directory(&dir).unwrap();

    assert!(!dir.join("rustreplay_segment_old.mp4").exists());
    assert!(!dir.join("rustreplay_segment_old.rrseg.part").exists());
    assert!(dir.join("keep.txt").exists());
    let _ = fs::remove_dir_all(dir);
}

fn segment(
    video_start_90k: u64,
    duration_90k: u64,
    audio_start_ticks: u64,
    audio_duration_ticks: u64,
) -> DiskSegmentTracks {
    DiskSegmentTracks {
        video_track: HevcMp4Track {
            width: 16,
            height: 16,
            duration_90k,
            color: NclxColorMetadata::bt709_full(),
            codec: HevcCodecMetadata::main_420_8(),
            samples: vec![
                HevcAccessUnit {
                    timestamp_90k: video_start_90k,
                    data: fake_hevc_parameter_sets().into(),
                    is_sync: false,
                    discard_from_track: true,
                },
                HevcAccessUnit {
                    timestamp_90k: video_start_90k,
                    data: fake_hevc_idr().into(),
                    is_sync: true,
                    discard_from_track: false,
                },
            ],
        },
        audio_track: Some(AacLcMp4Track {
            sample_rate: 48_000,
            channel_count: 2,
            duration_ticks: audio_duration_ticks,
            samples: vec![
                AacAccessUnit {
                    timestamp_ticks: audio_start_ticks,
                    duration_ticks: 1024,
                    data: vec![0x21, 0x10].into(),
                },
                AacAccessUnit {
                    timestamp_ticks: audio_start_ticks + audio_duration_ticks / 2,
                    duration_ticks: 1024,
                    data: vec![0x21, 0x10].into(),
                },
            ],
        }),
    }
}

fn metadata() -> EncodedReplayMetadata {
    EncodedReplayMetadata {
        width: 16,
        height: 16,
        color: NclxColorMetadata::bt709_full(),
        codec: HevcCodecMetadata::main_420_8(),
        audio_sample_rate: 48_000,
        audio_channel_count: 2,
    }
}

fn write_indexed_segment(
    dir: &Path,
    name: &str,
    segment: &DiskSegmentTracks,
) -> DiskSegmentIndexedTracks {
    let mp4_path = dir.join(format!("{name}.mp4"));
    let sidecar_path = indexed_sidecar(dir, name);
    let index = crate::backend::mp4_mux::write_hevc_aac_mp4_with_index(
        &mp4_path,
        &segment.video_track,
        segment.audio_track.as_ref(),
    )
    .unwrap();
    write_disk_segment_sidecar(&sidecar_path, &index).unwrap();
    DiskSegmentIndexedTracks {
        mp4_path,
        index: read_disk_segment_sidecar(&sidecar_path).unwrap(),
        lease: Arc::new(()),
    }
}

fn indexed_sidecar(dir: &Path, name: &str) -> PathBuf {
    dir.join(format!("{name}.rrseg"))
}

fn fake_hevc_parameter_sets() -> Vec<u8> {
    let mut out = Vec::new();
    append_fake_nal(&mut out, 32, &[1, 2, 3]);
    append_fake_nal(&mut out, 33, &[4, 5, 6]);
    append_fake_nal(&mut out, 34, &[7, 8, 9]);
    out
}

fn fake_hevc_idr() -> Vec<u8> {
    let mut out = Vec::new();
    append_fake_nal(&mut out, 19, &[10, 11, 12]);
    out
}

fn append_fake_nal(out: &mut Vec<u8>, nal_type: u8, payload: &[u8]) {
    out.extend_from_slice(&[0, 0, 0, 1]);
    out.push(nal_type << 1);
    out.push(1);
    out.extend_from_slice(payload);
}

fn unique_temp_dir(name: &str) -> PathBuf {
    std::env::temp_dir().join(format!(
        "rustreplay_{name}_{}",
        SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap()
            .as_nanos()
    ))
}
