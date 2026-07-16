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
fn disk_segment_concat_rejects_incompatible_audio_epoch() {
    let dir = unique_temp_dir("concat_audio_epoch");
    fs::create_dir_all(&dir).unwrap();
    let first = write_indexed_segment(&dir, "first", &segment(0, 90_000, 0, 48_000));
    let mut second_tracks = segment(0, 90_000, 0, 48_000);
    second_tracks.audio_track.as_mut().unwrap().sample_rate = 44_100;
    let second = write_indexed_segment(&dir, "second", &second_tracks);

    let err = concat_disk_indexed_segments(&[first, second]).unwrap_err();

    assert!(err.to_string().contains("AAC"));
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
            run_index: 0,
            source_start_ns: index * 10_000_000_000,
            source_end_ns: (index + 1) * 10_000_000_000,
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

    let selected = store.select_recent_segments_after(
        Duration::from_secs(25),
        Some(DiskSaveCursor {
            run_index: 0,
            source_pts_ns: 35_000_000_000,
        }),
    );
    assert_eq!(
        selected
            .iter()
            .map(|segment| segment.index)
            .collect::<Vec<_>>(),
        vec![4]
    );
}

#[test]
fn disk_store_source_cursor_resumes_at_the_next_segment_keyframe() {
    let mut store = DiskReplayStore::new(
        unique_temp_dir("source_cursor"),
        Duration::from_secs(30),
        Duration::from_secs(10),
    );
    for (index, start_ns) in [0, 5_000_000_000, 10_000_000_000].into_iter().enumerate() {
        store.segments.push_back(DiskSegmentMeta {
            index: index as u64,
            run_index: 0,
            source_start_ns: start_ns,
            source_end_ns: start_ns + 5_000_000_000,
            mp4_path: PathBuf::from(format!("{index}.mp4")),
            sidecar_path: PathBuf::from(format!("{index}.rrseg")),
            duration_90k: 5 * VIDEO_CLOCK_HZ,
            audio_access_units: 1,
            bytes: 1,
            lease: Arc::new(()),
        });
    }

    let selected = store.select_recent_segments_after(
        Duration::from_secs(30),
        Some(DiskSaveCursor {
            run_index: 0,
            source_pts_ns: 3_000_000_000,
        }),
    );

    assert_eq!(
        selected
            .iter()
            .map(|segment| segment.index)
            .collect::<Vec<_>>(),
        vec![1, 2]
    );
}

#[test]
fn disk_store_cursor_advances_across_recording_runs_that_restart_source_pts() {
    let mut store = DiskReplayStore::new(
        unique_temp_dir("run_cursor"),
        Duration::from_secs(30),
        Duration::from_secs(10),
    );
    for (index, run_index, start_ns) in [(0, 0, 20_000_000_000), (1, 1, 0)] {
        store.segments.push_back(DiskSegmentMeta {
            index,
            run_index,
            source_start_ns: start_ns,
            source_end_ns: start_ns + 10_000_000_000,
            mp4_path: PathBuf::from(format!("{index}.mp4")),
            sidecar_path: PathBuf::from(format!("{index}.rrseg")),
            duration_90k: 10 * VIDEO_CLOCK_HZ,
            audio_access_units: 1,
            bytes: 1,
            lease: Arc::new(()),
        });
    }

    let selected = store.select_recent_segments_after(
        Duration::from_secs(30),
        Some(DiskSaveCursor {
            run_index: 0,
            source_pts_ns: 30_000_000_000,
        }),
    );

    assert_eq!(
        selected
            .iter()
            .map(|segment| (segment.run_index, segment.index))
            .collect::<Vec<_>>(),
        vec![(1, 1)]
    );
}

#[test]
fn disk_store_snapshot_uses_only_the_latest_compatible_codec_epoch() {
    let dir = unique_temp_dir("latest_epoch");
    fs::create_dir_all(&dir).unwrap();
    let mut old_tracks = segment(0, 90_000, 0, 48_000);
    old_tracks.video_track.width = 32;
    let old = write_indexed_segment(&dir, "old", &old_tracks);
    let current_a = write_indexed_segment(&dir, "current_a", &segment(0, 90_000, 0, 48_000));
    let current_b = write_indexed_segment(&dir, "current_b", &segment(0, 90_000, 0, 48_000));
    let mut store = DiskReplayStore::new(
        dir.clone(),
        Duration::from_secs(60),
        Duration::from_secs(10),
    );
    for (index, segment) in [old, current_a, current_b].into_iter().enumerate() {
        store.segments.push_back(DiskSegmentMeta {
            index: index as u64,
            run_index: 0,
            source_start_ns: index as u64 * 1_000_000_000,
            source_end_ns: (index as u64 + 1) * 1_000_000_000,
            sidecar_path: indexed_sidecar(
                &dir,
                match index {
                    0 => "old",
                    1 => "current_a",
                    _ => "current_b",
                },
            ),
            mp4_path: segment.mp4_path,
            duration_90k: segment.index.video_track.duration_90k,
            audio_access_units: segment
                .index
                .audio_track
                .as_ref()
                .map(|track| track.samples.len())
                .unwrap_or(0),
            bytes: 1,
            lease: segment.lease,
        });
    }

    let snapshot = store
        .snapshot_recent_tracks(Duration::from_secs(60))
        .unwrap()
        .unwrap();

    assert_eq!(snapshot.video_track.width, 16);
    assert_eq!(snapshot.video_track.duration_90k, 180_000);
    assert_eq!(snapshot.video_track.samples.len(), 2);
    let _ = fs::remove_dir_all(dir);
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
    let mut builder =
        DiskSegmentBuilder::new(metadata, 90_000, 48_000, fake_hevc_parameter_sets().into());
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

    let segment = builder.into_tracks().unwrap();

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
        Arc::new(Mutex::new(EncodedReplayRing::new(Duration::from_secs(65)))),
        Arc::new(AtomicU64::new(u64::MAX)),
    );
    let mut builder = DiskSegmentBuilder::new(metadata(), 0, 0, fake_hevc_parameter_sets().into());
    builder.end_90k = Some(90_000);
    builder.push_video(&HevcAccessUnit {
        timestamp_90k: 0,
        data: fake_hevc_idr().into(),
        is_sync: true,
        discard_from_track: false,
    });

    sink.write_completed_segment(builder);

    assert!(stop.load(Ordering::Relaxed));
    assert!(sink.failure_message.is_some());
    assert!(matches!(
        event_rx.recv().unwrap(),
        ReplayEvent::BackendStatus { .. }
    ));
}

#[test]
fn disk_segment_assigns_cross_boundary_aac_before_flushing_the_old_segment() {
    let (writer_tx, writer_rx) = mpsc::sync_channel(1);
    let (event_tx, _event_rx) = mpsc::channel();
    let stop = Arc::new(AtomicBool::new(false));
    let mut sink = DiskSegmentSink::new(
        writer_tx,
        event_tx,
        0,
        Duration::from_secs(2),
        stop,
        Arc::new(Mutex::new(EncodedReplayRing::new(Duration::from_secs(7)))),
        Arc::new(AtomicU64::new(u64::MAX)),
    );
    sink.video_track_started(super::vpl::VplOutputTrackInfo {
        width: 16,
        height: 16,
        color: NclxColorMetadata::bt709_full(),
        codec: HevcCodecMetadata::main_420_8(),
    });
    sink.hevc_access_unit(&HevcAccessUnit {
        timestamp_90k: 0,
        data: fake_hevc_parameter_sets().into(),
        is_sync: false,
        discard_from_track: true,
    });
    sink.hevc_access_unit(&HevcAccessUnit {
        timestamp_90k: 0,
        data: fake_hevc_idr().into(),
        is_sync: true,
        discard_from_track: false,
    });
    sink.hevc_access_unit(&HevcAccessUnit {
        timestamp_90k: 90_000,
        data: fake_hevc_idr().into(),
        is_sync: true,
        discard_from_track: false,
    });

    sink.aac_access_unit(&AacAccessUnit {
        timestamp_ticks: 47_500,
        duration_ticks: 1_024,
        data: vec![0x21, 0x10].into(),
    });

    let job = writer_rx.try_recv().unwrap();
    let audio = job.segment.audio_track.unwrap();
    assert_eq!(audio.samples.len(), 1);
    assert_eq!(audio.samples[0].timestamp_ticks, 47_500);
}

#[test]
fn disk_segment_builder_keeps_the_parameter_sets_from_its_own_codec_epoch() {
    let (writer_tx, writer_rx) = mpsc::sync_channel(1);
    let (event_tx, _event_rx) = mpsc::channel();
    let mut sink = DiskSegmentSink::new(
        writer_tx,
        event_tx,
        0,
        Duration::from_secs(2),
        Arc::new(AtomicBool::new(false)),
        Arc::new(Mutex::new(EncodedReplayRing::new(Duration::from_secs(7)))),
        Arc::new(AtomicU64::new(u64::MAX)),
    );
    sink.video_track_started(super::vpl::VplOutputTrackInfo {
        width: 16,
        height: 16,
        color: NclxColorMetadata::bt709_full(),
        codec: HevcCodecMetadata::main_420_8(),
    });
    let first_header = fake_hevc_parameter_sets_with_seed(1);
    sink.hevc_access_unit(&HevcAccessUnit {
        timestamp_90k: 0,
        data: first_header.clone().into(),
        is_sync: false,
        discard_from_track: true,
    });
    sink.hevc_access_unit(&HevcAccessUnit {
        timestamp_90k: 0,
        data: fake_hevc_idr().into(),
        is_sync: true,
        discard_from_track: false,
    });
    sink.hevc_access_unit(&HevcAccessUnit {
        timestamp_90k: 45_000,
        data: fake_hevc_parameter_sets_with_seed(2).into(),
        is_sync: false,
        discard_from_track: true,
    });
    sink.hevc_access_unit(&HevcAccessUnit {
        timestamp_90k: 90_000,
        data: fake_hevc_idr().into(),
        is_sync: true,
        discard_from_track: false,
    });
    sink.aac_access_unit(&AacAccessUnit {
        timestamp_ticks: 47_500,
        duration_ticks: 1_024,
        data: vec![0x21, 0x10].into(),
    });

    let job = writer_rx.try_recv().unwrap();
    assert_eq!(
        job.segment.video_track.samples[0].data.as_ref(),
        first_header
    );
}

#[test]
fn disk_open_segment_ring_can_save_a_static_first_idr_before_any_rotation() {
    let (writer_tx, _writer_rx) = mpsc::sync_channel(1);
    let (event_tx, _event_rx) = mpsc::channel();
    let live_ring = Arc::new(Mutex::new(EncodedReplayRing::new(Duration::from_secs(65))));
    let mut sink = DiskSegmentSink::new(
        writer_tx,
        event_tx,
        0,
        Duration::from_secs(60),
        Arc::new(AtomicBool::new(false)),
        live_ring.clone(),
        Arc::new(AtomicU64::new(u64::MAX)),
    );
    sink.video_track_started(super::vpl::VplOutputTrackInfo {
        width: 16,
        height: 16,
        color: NclxColorMetadata::bt709_full(),
        codec: HevcCodecMetadata::main_420_8(),
    });
    sink.hevc_access_unit(&HevcAccessUnit {
        timestamp_90k: 0,
        data: fake_hevc_parameter_sets().into(),
        is_sync: false,
        discard_from_track: true,
    });
    sink.hevc_access_unit(&HevcAccessUnit {
        timestamp_90k: 0,
        data: fake_hevc_idr().into(),
        is_sync: true,
        discard_from_track: false,
    });
    sink.aac_access_unit(&AacAccessUnit {
        timestamp_ticks: 0,
        duration_ticks: 1_024,
        data: vec![0x21, 0x10].into(),
    });

    let snapshot = live_ring
        .lock()
        .unwrap()
        .snapshot_recent_tracks(Duration::from_secs(60))
        .unwrap();
    assert_eq!(
        snapshot
            .video_track
            .samples
            .iter()
            .filter(|sample| !sample.discard_from_track)
            .count(),
        1
    );
    assert_eq!(snapshot.audio_track.unwrap().samples.len(), 1);
}

#[test]
fn disk_segment_sink_stops_if_encoder_never_emits_a_keyframe() {
    let (writer_tx, _writer_rx) = mpsc::sync_channel(1);
    let (event_tx, event_rx) = mpsc::channel();
    let mut sink = DiskSegmentSink::new(
        writer_tx,
        event_tx,
        0,
        Duration::from_secs(2),
        Arc::new(AtomicBool::new(false)),
        Arc::new(Mutex::new(EncodedReplayRing::new(Duration::from_secs(7)))),
        Arc::new(AtomicU64::new(u64::MAX)),
    );
    sink.video_track_started(super::vpl::VplOutputTrackInfo {
        width: 16,
        height: 16,
        color: NclxColorMetadata::bt709_full(),
        codec: HevcCodecMetadata::main_420_8(),
    });
    sink.hevc_access_unit(&HevcAccessUnit {
        timestamp_90k: 0,
        data: fake_hevc_idr().into(),
        is_sync: false,
        discard_from_track: false,
    });
    sink.hevc_access_unit(&HevcAccessUnit {
        timestamp_90k: 360_000,
        data: fake_hevc_idr().into(),
        is_sync: false,
        discard_from_track: false,
    });

    assert!(sink.failed);
    assert!(sink.failure_message.is_some());
    assert!(
        event_rx
            .try_iter()
            .any(|event| matches!(event, ReplayEvent::BackendStatus { .. }))
    );
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

#[test]
fn disk_store_clear_defers_leased_segments_and_retries_after_release() {
    let dir = unique_temp_dir("clear_leased");
    fs::create_dir_all(&dir).unwrap();
    let mp4_path = dir.join("leased.mp4");
    let sidecar_path = dir.join("leased.rrseg");
    fs::write(&mp4_path, b"mp4").unwrap();
    fs::write(&sidecar_path, b"sidecar").unwrap();
    let lease = Arc::new(());
    let held = lease.clone();
    let mut store = DiskReplayStore::new(
        dir.clone(),
        Duration::from_secs(60),
        Duration::from_secs(10),
    );
    store.segments.push_back(DiskSegmentMeta {
        index: 0,
        run_index: 0,
        source_start_ns: 0,
        source_end_ns: 1_000_000_000,
        mp4_path: mp4_path.clone(),
        sidecar_path: sidecar_path.clone(),
        duration_90k: 90_000,
        audio_access_units: 1,
        bytes: 7,
        lease,
    });

    store.clear_segments().unwrap();
    assert_eq!(store.segments.len(), 1);
    assert!(mp4_path.exists());
    drop(held);
    store.clear_segments().unwrap();
    assert!(store.segments.is_empty());
    assert!(!mp4_path.exists());
    assert!(!sidecar_path.exists());
    let _ = fs::remove_dir_all(dir);
}

#[test]
fn disk_store_prune_retains_deletion_failures_for_retry() {
    let dir = unique_temp_dir("prune_retry");
    fs::create_dir_all(&dir).unwrap();
    let old_mp4 = dir.join("old.mp4");
    let old_sidecar = dir.join("old.rrseg");
    fs::write(&old_mp4, b"mp4").unwrap();
    fs::create_dir_all(&old_sidecar).unwrap();
    let mut store = DiskReplayStore::new(dir.clone(), Duration::from_secs(1), Duration::ZERO);
    for index in 0..2u64 {
        store.segments.push_back(DiskSegmentMeta {
            index,
            run_index: 0,
            source_start_ns: index * 1_000_000_000,
            source_end_ns: (index + 1) * 1_000_000_000,
            mp4_path: if index == 0 {
                old_mp4.clone()
            } else {
                dir.join("new.mp4")
            },
            sidecar_path: if index == 0 {
                old_sidecar.clone()
            } else {
                dir.join("new.rrseg")
            },
            duration_90k: VIDEO_CLOCK_HZ,
            audio_access_units: 1,
            bytes: 1,
            lease: Arc::new(()),
        });
    }

    let failures = store.prune_old_segments();
    assert_eq!(store.segments.len(), 2);
    assert!(!failures.is_empty());
    assert!(!old_mp4.exists());
    assert!(old_sidecar.is_dir());

    fs::remove_dir(&old_sidecar).unwrap();
    assert!(store.prune_old_segments().is_empty());
    assert_eq!(
        store
            .segments
            .iter()
            .map(|segment| segment.index)
            .collect::<Vec<_>>(),
        vec![1]
    );
    let _ = fs::remove_dir_all(dir);
}

#[test]
fn disk_writer_panic_stops_the_session_and_is_reported_to_the_worker() {
    let dir = unique_temp_dir("writer_panic");
    fs::create_dir_all(&dir).unwrap();
    let store = Arc::new(Mutex::new(DiskReplayStore::new(
        dir.clone(),
        Duration::from_secs(60),
        Duration::from_secs(10),
    )));
    let (event_tx, event_rx) = mpsc::channel();
    let stop = Arc::new(AtomicBool::new(false));
    let writer = DiskSegmentWriter::spawn(store, event_tx, stop.clone());
    panic_disk_writer_on_next_job();
    writer
        .sender()
        .unwrap()
        .send(DiskSegmentWriteJob {
            run_index: 7,
            segment: segment(0, 90_000, 0, 48_000),
            enqueued_at: Instant::now(),
        })
        .unwrap();
    let deadline = Instant::now() + Duration::from_secs(1);
    while !stop.load(Ordering::Acquire) && Instant::now() < deadline {
        std::thread::sleep(Duration::from_millis(1));
    }

    let failure = writer.shutdown().unwrap();
    assert!(stop.load(Ordering::Acquire));
    assert!(failure.contains("panic"));
    assert!(event_rx.try_iter().any(|event| {
        matches!(event, ReplayEvent::BackendStatus { message, .. } if message.contains("panic"))
    }));
    let _ = fs::remove_dir_all(dir);
}

#[test]
fn disk_segment_transaction_rolls_back_if_sidecar_publish_fails() {
    let dir = unique_temp_dir("publish_rollback");
    fs::create_dir_all(&dir).unwrap();
    let reservation = DiskSegmentReservation {
        index: 0,
        mp4_path: dir.join("segment.mp4"),
        sidecar_path: dir.join("segment.rrseg"),
        mp4_part_path: dir.join("segment.mp4.part"),
        sidecar_part_path: dir.join("segment.rrseg.part"),
    };
    fs::write(&reservation.mp4_part_path, b"mp4").unwrap();
    fs::write(&reservation.sidecar_part_path, b"sidecar").unwrap();
    fs::create_dir(&reservation.sidecar_path).unwrap();
    let transaction = DiskSegmentWriteTransaction::new(reservation.clone());

    assert!(transaction.publish().is_err());
    drop(transaction);

    assert!(!reservation.mp4_path.exists());
    assert!(!reservation.mp4_part_path.exists());
    assert!(!reservation.sidecar_part_path.exists());
    let _ = fs::remove_dir_all(dir);
}

fn segment(
    video_start_90k: u64,
    duration_90k: u64,
    audio_start_ticks: u64,
    audio_duration_ticks: u64,
) -> DiskSegmentTracks {
    DiskSegmentTracks {
        source_start_90k: video_start_90k,
        source_end_90k: video_start_90k.saturating_add(duration_90k),
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
    fake_hevc_parameter_sets_with_seed(0)
}

fn fake_hevc_parameter_sets_with_seed(seed: u8) -> Vec<u8> {
    let mut out = Vec::new();
    append_fake_nal(&mut out, 32, &[seed, 2, 3]);
    out.extend_from_slice(&[0, 0, 0, 1]);
    out.extend(crate::backend::mp4_mux::synthetic_sps_nal(
        metadata().codec,
        0x6000_0000 | u32::from(seed),
        120,
    ));
    append_fake_nal(&mut out, 34, &[seed, 8, 9]);
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
