use super::*;
use crate::rate_control::RateControlMethod;

#[test]
fn disk_segment_sidecar_roundtrips_tracks() {
    let dir = unique_temp_dir("sidecar_roundtrip");
    fs::create_dir_all(&dir).unwrap();
    let segment = segment(0, 90_000, 0, 48_000);
    let indexed = write_indexed_segment(&dir, "segment", &segment);

    let read = read_disk_segment_sidecar(&indexed_sidecar(&dir, "segment")).unwrap();

    assert_eq!(read.video_track.width, segment.video_track.width);
    assert_eq!(read.video_track.duration_ticks, 90_000);
    assert_eq!(read.video_track.samples.len(), 1);
    assert_eq!(
        read.audio_track.as_ref().unwrap().duration_ticks,
        segment.audio_track.as_ref().unwrap().duration_ticks
    );
    assert_eq!(
        read.audio_track.as_ref().unwrap().samples[0].timestamp_ticks,
        segment.audio_track.as_ref().unwrap().samples[0].timestamp_ticks
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

    assert_eq!(snapshot.video_track.duration_ticks, 135_000);
    assert_eq!(snapshot.video_track.samples[0].duration_ticks, 90_000);
    assert_eq!(snapshot.video_track.samples[1].duration_ticks, 45_000);
    let audio = snapshot.audio_track.unwrap();
    assert_eq!(audio.duration_ticks, 72_000);
    assert_eq!(audio.samples.len(), 4);
    assert_eq!(
        audio
            .samples
            .iter()
            .map(|sample| sample.timestamp_ticks)
            .collect::<Vec<_>>(),
        vec![0, 24_000, 48_000, 60_000]
    );
    let out = dir.join("final.mp4");
    crate::backend::mp4_mux::write_prepared_hevc_aac_mp4(&out, &snapshot.video_track, Some(&audio))
        .unwrap();
    assert!(fs::metadata(&out).unwrap().len() > 0);
    let _ = fs::remove_dir_all(dir);
}

#[test]
fn disk_segment_concat_tolerates_one_tick_vfr_origin_rounding() {
    let dir = unique_temp_dir("concat_vfr_rounding");
    fs::create_dir_all(&dir).unwrap();
    let first = write_indexed_segment(&dir, "first", &segment(1_000, 1_000, 0, 534));
    let second = write_indexed_segment(&dir, "second", &segment(2_000, 1_000, 0, 534));

    let snapshot = concat_disk_indexed_segments(&[first, second])
        .unwrap()
        .unwrap();
    let audio = snapshot.audio_track.unwrap();

    assert_eq!(
        audio
            .samples
            .iter()
            .map(|sample| sample.timestamp_ticks)
            .collect::<Vec<_>>(),
        vec![0, 267, 534, 801]
    );
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
            duration_90k: segment.index.video_track.duration_ticks,
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
    assert_eq!(snapshot.video_track.duration_ticks, 180_000);
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
    let mut builder = DiskSegmentBuilder::new(
        metadata,
        90_000,
        None,
        48_000,
        fake_hevc_parameter_sets().into(),
    );
    builder.end_90k = Some(180_000);
    builder.push_video(&HevcAccessUnit {
        timestamp_90k: 90_000,
        presentation_timestamp_100ns: None,
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
fn disk_segment_sink_applies_cancelable_backpressure_when_writer_queue_is_full() {
    let (writer_tx, writer_rx) = mpsc::sync_channel(1);
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
    let mut builder =
        DiskSegmentBuilder::new(metadata(), 0, None, 0, fake_hevc_parameter_sets().into());
    builder.end_90k = Some(90_000);
    builder.push_video(&HevcAccessUnit {
        timestamp_90k: 0,
        presentation_timestamp_100ns: None,
        data: fake_hevc_idr().into(),
        is_sync: true,
        discard_from_track: false,
    });

    let release_queue = std::thread::spawn(move || {
        std::thread::sleep(Duration::from_millis(20));
        let queued_before_backpressure = writer_rx.recv().unwrap();
        let queued_after_backpressure = writer_rx.recv().unwrap();
        (queued_before_backpressure, queued_after_backpressure)
    });
    sink.write_completed_segment(builder);
    let _ = release_queue.join().unwrap();

    assert!(!stop.load(Ordering::Relaxed));
    assert!(sink.failure_message.is_none());
    assert!(matches!(
        event_rx.recv().unwrap(),
        ReplayEvent::BackendStatus { .. }
    ));
}

#[test]
fn disk_segment_sink_reports_final_segment_drop_when_stop_interrupts_full_queue() {
    let (writer_tx, _writer_rx) = mpsc::sync_channel(1);
    writer_tx
        .try_send(DiskSegmentWriteJob {
            run_index: 0,
            segment: segment(0, 90_000, 0, 48_000),
            enqueued_at: Instant::now(),
        })
        .unwrap();
    let (event_tx, _event_rx) = mpsc::channel();
    let stop = Arc::new(AtomicBool::new(true));
    let mut sink = DiskSegmentSink::new(
        writer_tx,
        event_tx,
        0,
        Duration::from_secs(60),
        stop.clone(),
        Arc::new(Mutex::new(EncodedReplayRing::new(Duration::from_secs(65)))),
        Arc::new(AtomicU64::new(u64::MAX)),
    );
    let mut builder =
        DiskSegmentBuilder::new(metadata(), 0, None, 0, fake_hevc_parameter_sets().into());
    builder.end_90k = Some(90_000);
    builder.push_video(&HevcAccessUnit {
        timestamp_90k: 0,
        presentation_timestamp_100ns: None,
        data: fake_hevc_idr().into(),
        is_sync: true,
        discard_from_track: false,
    });

    sink.write_completed_segment(builder);
    assert!(sink.failed);
    assert!(
        sink.failure_message
            .as_deref()
            .is_some_and(|message| message.contains("最终分段未能排队"))
    );
    assert!(stop.load(Ordering::Relaxed));
}

#[test]
fn stale_replay_part_cleanup_failure_is_nonfatal() {
    let dir = unique_temp_dir("stale_part_cleanup_failure");
    fs::create_dir_all(&dir).unwrap();
    let undeletable_part = dir.join("RustReplay_undeletable.mp4.part");
    fs::create_dir(&undeletable_part).unwrap();

    let warnings = cleanup_stale_replay_parts_older_than(&dir, Duration::ZERO);

    assert_eq!(warnings.len(), 1);
    assert!(warnings[0].contains("已继续启动"));
    assert!(undeletable_part.is_dir());
    let _ = fs::remove_dir_all(dir);
}

#[test]
fn display_reconfigure_backoff_is_exponential_and_capped() {
    let delays: Vec<_> = (1..=MAX_CONSECUTIVE_DISPLAY_RECONFIGURES)
        .map(display_reconfigure_backoff)
        .collect();

    assert_eq!(
        delays,
        [200, 400, 800, 1_600, 3_200, 5_000, 5_000, 5_000].map(Duration::from_millis)
    );
    assert_eq!(
        next_display_reconfigure_count(7, Duration::from_secs(1)),
        MAX_CONSECUTIVE_DISPLAY_RECONFIGURES
    );
    assert_eq!(
        next_display_reconfigure_count(7, Duration::from_secs(30)),
        1
    );
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
        presentation_timestamp_100ns: None,
        data: fake_hevc_parameter_sets().into(),
        is_sync: false,
        discard_from_track: true,
    });
    sink.hevc_access_unit(&HevcAccessUnit {
        timestamp_90k: 0,
        presentation_timestamp_100ns: None,
        data: fake_hevc_idr().into(),
        is_sync: true,
        discard_from_track: false,
    });
    sink.hevc_access_unit(&HevcAccessUnit {
        timestamp_90k: 90_000,
        presentation_timestamp_100ns: None,
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
fn disk_segment_moves_early_aac_when_a_delayed_keyframe_reveals_the_boundary() {
    let (writer_tx, writer_rx) = mpsc::sync_channel(1);
    let (event_tx, _event_rx) = mpsc::channel();
    let live_ring = Arc::new(Mutex::new(EncodedReplayRing::new(Duration::from_secs(7))));
    let mut sink = DiskSegmentSink::new(
        writer_tx,
        event_tx,
        0,
        Duration::from_secs(2),
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
        presentation_timestamp_100ns: None,
        data: fake_hevc_parameter_sets().into(),
        is_sync: false,
        discard_from_track: true,
    });
    sink.hevc_access_unit(&HevcAccessUnit {
        timestamp_90k: 0,
        presentation_timestamp_100ns: None,
        data: fake_hevc_idr().into(),
        is_sync: true,
        discard_from_track: false,
    });
    sink.aac_access_unit(&AacAccessUnit {
        timestamp_ticks: 60_000,
        duration_ticks: 1_024,
        data: vec![0x21, 0x10].into(),
    });

    let mut boundary_au = fake_hevc_parameter_sets_with_seed(2);
    boundary_au.extend(fake_hevc_idr());
    sink.hevc_access_unit(&HevcAccessUnit {
        timestamp_90k: 90_000,
        presentation_timestamp_100ns: None,
        data: boundary_au.into(),
        is_sync: true,
        discard_from_track: false,
    });

    let job = writer_rx.try_recv().unwrap();
    assert!(job.segment.audio_track.is_none());
    let current = sink.current.as_ref().unwrap();
    assert_eq!(current.start_audio_ticks, 48_000);
    assert_eq!(current.audio_samples.len(), 1);
    assert_eq!(current.audio_samples[0].timestamp_ticks, 12_000);
    assert_eq!(live_ring.lock().unwrap().availability().audio_packets, 1);
}

#[test]
fn disk_segment_adopts_aac_published_before_the_first_delayed_idr() {
    let (writer_tx, _writer_rx) = mpsc::sync_channel(1);
    let (event_tx, _event_rx) = mpsc::channel();
    let live_ring = Arc::new(Mutex::new(EncodedReplayRing::new(Duration::from_secs(7))));
    let mut sink = DiskSegmentSink::new(
        writer_tx,
        event_tx,
        0,
        Duration::from_secs(2),
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
    for timestamp_ticks in [0, 1_024] {
        sink.aac_access_unit(&AacAccessUnit {
            timestamp_ticks,
            duration_ticks: 1_024,
            data: vec![0x21, 0x10].into(),
        });
    }

    let mut first_idr = fake_hevc_parameter_sets();
    first_idr.extend(fake_hevc_idr());
    sink.hevc_access_unit(&HevcAccessUnit {
        timestamp_90k: 0,
        presentation_timestamp_100ns: None,
        data: first_idr.into(),
        is_sync: true,
        discard_from_track: false,
    });

    assert!(sink.pending_audio_before_video.is_empty());
    let current = sink.current.as_ref().unwrap();
    assert_eq!(current.audio_samples.len(), 2);
    assert_eq!(current.audio_samples[0].timestamp_ticks, 0);
    assert_eq!(current.audio_samples[1].timestamp_ticks, 1_024);
    assert_eq!(live_ring.lock().unwrap().availability().audio_packets, 2);
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
        presentation_timestamp_100ns: None,
        data: first_header.clone().into(),
        is_sync: false,
        discard_from_track: true,
    });
    sink.hevc_access_unit(&HevcAccessUnit {
        timestamp_90k: 0,
        presentation_timestamp_100ns: None,
        data: fake_hevc_idr().into(),
        is_sync: true,
        discard_from_track: false,
    });
    sink.hevc_access_unit(&HevcAccessUnit {
        timestamp_90k: 45_000,
        presentation_timestamp_100ns: None,
        data: fake_hevc_parameter_sets_with_seed(2).into(),
        is_sync: false,
        discard_from_track: true,
    });
    sink.hevc_access_unit(&HevcAccessUnit {
        timestamp_90k: 90_000,
        presentation_timestamp_100ns: None,
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
        presentation_timestamp_100ns: None,
        data: fake_hevc_parameter_sets().into(),
        is_sync: false,
        discard_from_track: true,
    });
    sink.hevc_access_unit(&HevcAccessUnit {
        timestamp_90k: 0,
        presentation_timestamp_100ns: None,
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
fn memory_and_disk_sinks_expose_equivalent_open_segment_replays() {
    let retention = Duration::from_secs(65);
    let memory_ring = Arc::new(Mutex::new(EncodedReplayRing::new(retention)));
    let disk_ring = Arc::new(Mutex::new(EncodedReplayRing::new(retention)));
    let (memory_event_tx, _memory_event_rx) = mpsc::channel();
    let (disk_event_tx, _disk_event_rx) = mpsc::channel();
    let (disk_writer_tx, _disk_writer_rx) = mpsc::sync_channel(1);
    let mut memory_sink = ReplayRecordSink::Memory(SessionRingSink {
        ring: memory_ring.clone(),
        tx: memory_event_tx,
        segment_index: 0,
        started: false,
    });
    let mut disk_sink = ReplayRecordSink::Disk(Box::new(DiskSegmentSink::new(
        disk_writer_tx,
        disk_event_tx,
        0,
        Duration::from_secs(60),
        Arc::new(AtomicBool::new(false)),
        disk_ring.clone(),
        Arc::new(AtomicU64::new(u64::MAX)),
    )));
    let info = super::vpl::VplOutputTrackInfo {
        width: 16,
        height: 16,
        color: NclxColorMetadata::bt709_full(),
        codec: HevcCodecMetadata::main_420_8(),
    };
    let header = HevcAccessUnit {
        timestamp_90k: 0,
        presentation_timestamp_100ns: None,
        data: fake_hevc_parameter_sets().into(),
        is_sync: false,
        discard_from_track: true,
    };
    let idr = HevcAccessUnit {
        timestamp_90k: 0,
        presentation_timestamp_100ns: None,
        data: fake_hevc_idr().into(),
        is_sync: true,
        discard_from_track: false,
    };
    let mut predicted_data = Vec::new();
    append_fake_nal(&mut predicted_data, 1, &[13, 14, 15]);
    let predicted = HevcAccessUnit {
        timestamp_90k: 90_000,
        presentation_timestamp_100ns: None,
        data: predicted_data.into(),
        is_sync: false,
        discard_from_track: false,
    };
    let audio = AacAccessUnit {
        timestamp_ticks: 0,
        duration_ticks: 1_024,
        data: vec![0x21, 0x10].into(),
    };

    for sink in [&mut memory_sink, &mut disk_sink] {
        super::vpl::VplOneCopyRecordSink::video_track_started(sink, info);
        super::vpl::VplOneCopyRecordSink::hevc_access_unit(sink, &header);
        super::vpl::VplOneCopyRecordSink::hevc_access_unit(sink, &idr);
        super::vpl::VplOneCopyRecordSink::hevc_access_unit(sink, &predicted);
        super::vpl::VplOneCopyRecordSink::aac_access_unit(sink, &audio);
    }

    let memory_snapshot = memory_ring
        .lock()
        .unwrap()
        .snapshot_recent_tracks(Duration::from_secs(60))
        .unwrap();
    let disk_snapshot = disk_ring
        .lock()
        .unwrap()
        .snapshot_recent_tracks(Duration::from_secs(60))
        .unwrap();
    assert_eq!(
        memory_snapshot.video_track.width,
        disk_snapshot.video_track.width
    );
    assert_eq!(
        memory_snapshot.video_track.height,
        disk_snapshot.video_track.height
    );
    assert_eq!(
        memory_snapshot.video_track.duration_90k,
        disk_snapshot.video_track.duration_90k
    );
    assert_eq!(
        memory_snapshot.video_track.samples.len(),
        disk_snapshot.video_track.samples.len()
    );
    for (memory, disk) in memory_snapshot
        .video_track
        .samples
        .iter()
        .zip(&disk_snapshot.video_track.samples)
    {
        assert_eq!(memory.timestamp_90k, disk.timestamp_90k);
        assert_eq!(memory.is_sync, disk.is_sync);
        assert_eq!(memory.discard_from_track, disk.discard_from_track);
        assert_eq!(memory.data.as_ref(), disk.data.as_ref());
    }
    let memory_audio = memory_snapshot.audio_track.unwrap();
    let disk_audio = disk_snapshot.audio_track.unwrap();
    assert_eq!(memory_audio.sample_rate, disk_audio.sample_rate);
    assert_eq!(memory_audio.channel_count, disk_audio.channel_count);
    assert_eq!(memory_audio.duration_ticks, disk_audio.duration_ticks);
    assert_eq!(memory_audio.samples.len(), disk_audio.samples.len());
    assert_eq!(
        memory_audio.samples[0].data.as_ref(),
        disk_audio.samples[0].data.as_ref()
    );

    let mut memory_controller = ReplayController::default();
    memory_controller.state = ReplayState::Running {
        started_at: Instant::now(),
    };
    memory_controller.live_ring = Some(memory_ring);
    let mut disk_controller = ReplayController::default();
    disk_controller.state = ReplayState::Running {
        started_at: Instant::now(),
    };
    disk_controller.disk_store = Some(Arc::new(Mutex::new(DiskReplayStore::new(
        unique_temp_dir("disk_parity_store"),
        Duration::from_secs(60),
        Duration::from_secs(10),
    ))));
    disk_controller.disk_live_ring = Some(disk_ring);
    assert_eq!(
        memory_controller.save_readiness(),
        ReplaySaveReadiness::Ready
    );
    assert_eq!(disk_controller.save_readiness(), ReplaySaveReadiness::Ready);
}

#[test]
#[ignore = "需要现代 NVIDIA 驱动、NvFBC V3、HDR PQ 桌面、WASAPI 与 Media Foundation AAC"]
fn local_nvfbc_controller_memory_ring_saves_hevc_aac_mp4() {
    let caps = crate::backend::probe_all();
    assert!(
        caps.nvfbc_usable(),
        "NvFBC unavailable: {:?}",
        caps.nvfbc.error
    );
    let chroma = caps
        .supported_chroma_for_capture_mode(CaptureMode::DedicatedNvFbc)
        .into_iter()
        .next()
        .expect("NvFBC production chroma");
    let method = if caps.nvfbc.rate_controls.contains(&RateControlMethod::Cqp) {
        RateControlMethod::Cqp
    } else {
        *caps
            .nvfbc
            .rate_controls
            .first()
            .expect("NvFBC rate control")
    };
    let preset = *caps.nvfbc.presets.first().expect("NvFBC preset");
    let output_dir = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("target")
        .join(format!("nvfbc-controller-{}", timestamp_for_filename()));
    let mut config = AppConfig {
        capture_mode: CaptureMode::DedicatedNvFbc,
        replay_buffer_mode: ReplayBufferMode::Memory,
        chroma: Some(chroma),
        replay_minutes: 0.1,
        save_dir: output_dir.display().to_string(),
        ..AppConfig::default()
    };
    config.rate_control.method = method;
    config.rate_control.look_ahead_depth = 0;
    config.rate_control.nvenc_preset = preset;

    let mut controller = ReplayController::default();
    controller
        .start(&config, &caps)
        .expect("start NvFBC replay");
    let ready_deadline = Instant::now() + Duration::from_secs(20);
    while controller.save_readiness() != ReplaySaveReadiness::Ready {
        for message in controller.drain_log_messages() {
            eprintln!("nvfbc_controller={message}");
        }
        assert!(
            Instant::now() < ready_deadline,
            "NvFBC encoded ring did not become ready"
        );
        std::thread::sleep(Duration::from_millis(50));
    }
    // Cross the production source-timed IDR interval so the saved ring proves
    // that periodic keyframes, not only the first frame, reach the controller.
    std::thread::sleep(Duration::from_millis(5_300));
    controller.save(&config).expect("save NvFBC replay");
    let saved = std::fs::read_dir(&output_dir)
        .expect("read NvFBC output directory")
        .filter_map(Result::ok)
        .map(|entry| entry.path())
        .find(|path| path.extension().is_some_and(|extension| extension == "mp4"))
        .expect("saved NvFBC MP4");
    eprintln!("nvfbc_controller_output={}", saved.display());

    controller.stop().expect("stop NvFBC replay");
    let stop_deadline = Instant::now() + Duration::from_secs(10);
    while !matches!(controller.state(), ReplayState::Idle) {
        for message in controller.drain_log_messages() {
            eprintln!("nvfbc_controller={message}");
        }
        assert!(
            Instant::now() < stop_deadline,
            "NvFBC replay did not stop promptly"
        );
        std::thread::sleep(Duration::from_millis(20));
    }
    assert!(saved.metadata().expect("saved MP4 metadata").len() > 0);
}

#[test]
#[ignore = "需要本机 NVENC、D3D11/WGC 桌面、WASAPI 与 Media Foundation AAC"]
fn local_nvenc_controller_memory_and_disk_modes_save_mp4() {
    unsafe {
        std::env::set_var("RUST_REPLAY_WGC_POST_WARMUP_DISCARD_FRAMES", "0");
        std::env::set_var("RUST_REPLAY_WGC_PIPELINE_WARMUP_FRAMES", "0");
        std::env::set_var("RUST_REPLAY_WGC_PIPELINE_WARMUP_STABLE_INTERVALS", "0");
    }
    let caps = crate::backend::probe_all();
    assert_eq!(
        caps.video_encoder_selection.active,
        Some(crate::backend::VideoEncoderBackend::Nvenc),
        "本测试要求自动选择 NVENC：{:?}",
        caps.video_encoder_selection
    );
    let chroma = caps
        .supported_chroma_for_capture_mode(CaptureMode::Generic)
        .into_iter()
        .find(|chroma| *chroma == ChromaSampling::Yuv420)
        .expect("NVENC current display YUV420 production route");
    let root = unique_temp_dir("nvenc_controller_buffer_modes");
    fs::create_dir_all(&root).unwrap();
    let cursor_stop = Arc::new(AtomicBool::new(false));
    let cursor_thread = {
        let cursor_stop = cursor_stop.clone();
        std::thread::spawn(move || {
            use windows::Win32::Foundation::POINT;
            use windows::Win32::UI::WindowsAndMessaging::{GetCursorPos, SetCursorPos};

            let mut origin = POINT::default();
            if unsafe { GetCursorPos(&mut origin) }.is_err() {
                return;
            }
            let mut tick = 0i32;
            while !cursor_stop.load(Ordering::Relaxed) {
                let dx = if tick & 1 == 0 { 6 } else { -6 };
                let _ = unsafe { SetCursorPos(origin.x + dx, origin.y) };
                tick = tick.wrapping_add(1);
                std::thread::sleep(Duration::from_millis(50));
            }
            let _ = unsafe { SetCursorPos(origin.x, origin.y) };
        })
    };

    for mode in [ReplayBufferMode::Memory, ReplayBufferMode::Disk] {
        let label = mode.label();
        let save_dir = root.join(format!("save_{label}"));
        let cache_dir = root.join(format!("cache_{label}"));
        let mut config = AppConfig {
            capture_mode: CaptureMode::Generic,
            capture_backend: CaptureBackend::Wgc,
            replay_buffer_mode: mode,
            chroma: Some(chroma),
            replay_minutes: 0.1,
            save_dir: save_dir.display().to_string(),
            cache_dir: cache_dir.display().to_string(),
            ..AppConfig::default()
        };
        config.rate_control.method = RateControlMethod::Cbr;
        config.rate_control.look_ahead_depth = 0;

        let mut controller = ReplayController::default();
        controller
            .start(&config, &caps)
            .unwrap_or_else(|err| panic!("start {label}: {err}"));
        let ready_deadline = Instant::now() + Duration::from_secs(20);
        let mut log = Vec::new();
        while controller.save_readiness() != ReplaySaveReadiness::Ready {
            log.extend(controller.drain_log_messages());
            assert!(
                !matches!(controller.state(), ReplayState::Idle),
                "{label} stopped before ready: {}",
                log.join(" | ")
            );
            assert!(
                Instant::now() < ready_deadline,
                "{label} did not become saveable: {}",
                log.join(" | ")
            );
            std::thread::sleep(Duration::from_millis(50));
        }
        controller
            .save(&config)
            .unwrap_or_else(|err| panic!("save {label}: {err}"));
        let saved = fs::read_dir(&save_dir)
            .unwrap_or_else(|err| panic!("read {label} save dir: {err}"))
            .filter_map(Result::ok)
            .map(|entry| entry.path())
            .find(|path| path.extension().is_some_and(|extension| extension == "mp4"))
            .unwrap_or_else(|| panic!("{label} did not write an MP4"));
        assert!(
            saved.metadata().expect("saved MP4 metadata").len() > 0,
            "{label} wrote an empty MP4"
        );
        eprintln!("nvenc_controller_{label}_output={}", saved.display());

        controller
            .stop()
            .unwrap_or_else(|err| panic!("stop {label}: {err}"));
        let stop_deadline = Instant::now() + Duration::from_secs(10);
        while !matches!(controller.state(), ReplayState::Idle) {
            log.extend(controller.drain_log_messages());
            assert!(
                Instant::now() < stop_deadline,
                "{label} did not stop promptly: {}",
                log.join(" | ")
            );
            std::thread::sleep(Duration::from_millis(20));
        }
    }

    cursor_stop.store(true, Ordering::Relaxed);
    let _ = cursor_thread.join();
    unsafe {
        std::env::remove_var("RUST_REPLAY_WGC_POST_WARMUP_DISCARD_FRAMES");
        std::env::remove_var("RUST_REPLAY_WGC_PIPELINE_WARMUP_FRAMES");
        std::env::remove_var("RUST_REPLAY_WGC_PIPELINE_WARMUP_STABLE_INTERVALS");
    }
    let _ = fs::remove_dir_all(root);
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
        presentation_timestamp_100ns: None,
        data: fake_hevc_idr().into(),
        is_sync: false,
        discard_from_track: false,
    });
    sink.hevc_access_unit(&HevcAccessUnit {
        timestamp_90k: 360_000,
        presentation_timestamp_100ns: None,
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
fn disk_writer_shutdown_drains_a_queued_final_segment() {
    let dir = unique_temp_dir("disk_writer_shutdown_drain");
    fs::create_dir_all(&dir).unwrap();
    let store = Arc::new(Mutex::new(DiskReplayStore::new(
        dir.clone(),
        Duration::from_secs(60),
        Duration::from_secs(10),
    )));
    let (event_tx, _event_rx) = mpsc::channel();
    let stop = Arc::new(AtomicBool::new(false));
    let writer = DiskSegmentWriter::spawn(store.clone(), event_tx, stop);
    writer
        .sender()
        .unwrap()
        .send(DiskSegmentWriteJob {
            run_index: 3,
            segment: segment(0, 90_000, 0, 48_000),
            enqueued_at: Instant::now(),
        })
        .unwrap();

    assert!(writer.shutdown().is_none());
    let store = store.lock().unwrap();
    assert_eq!(store.segments.len(), 1);
    assert!(store.segments[0].mp4_path.exists());
    assert!(store.segments[0].sidecar_path.exists());
    drop(store);
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
    let first_audio_duration = (audio_duration_ticks / 2).max(1);
    let second_audio_duration = audio_duration_ticks
        .saturating_sub(first_audio_duration)
        .max(1);
    DiskSegmentTracks {
        source_start_90k: video_start_90k,
        source_end_90k: video_start_90k.saturating_add(duration_90k),
        video_track: HevcMp4Track {
            width: 16,
            height: 16,
            duration_90k,
            presentation_duration_100ns: None,
            color: NclxColorMetadata::bt709_full(),
            codec: HevcCodecMetadata::main_420_8(),
            samples: vec![
                HevcAccessUnit {
                    timestamp_90k: 0,
                    presentation_timestamp_100ns: None,
                    data: fake_hevc_parameter_sets().into(),
                    is_sync: false,
                    discard_from_track: true,
                },
                HevcAccessUnit {
                    timestamp_90k: 0,
                    presentation_timestamp_100ns: None,
                    data: fake_hevc_idr().into(),
                    is_sync: true,
                    discard_from_track: false,
                },
            ],
        },
        audio_track: Some(AacLcMp4Track {
            sample_rate: 48_000,
            channel_count: 2,
            duration_ticks: audio_start_ticks.saturating_add(audio_duration_ticks),
            samples: vec![
                AacAccessUnit {
                    timestamp_ticks: audio_start_ticks,
                    duration_ticks: first_audio_duration as u32,
                    data: vec![0x21, 0x10].into(),
                },
                AacAccessUnit {
                    timestamp_ticks: audio_start_ticks.saturating_add(first_audio_duration),
                    duration_ticks: second_audio_duration as u32,
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
