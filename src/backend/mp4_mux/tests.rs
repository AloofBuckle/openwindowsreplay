use super::*;

#[test]
fn aac_lc_asc_for_48k_stereo_matches_mpeg4_bits() {
    assert_eq!(
        aac_lc_audio_specific_config(48_000, 2).unwrap(),
        vec![0x11, 0x90]
    );
}

#[test]
fn bt2020_sdr_10_uses_nclx_2020_10bit_metadata() {
    let full = NclxColorMetadata::bt2020_sdr_10(true);
    assert_eq!(full.colour_primaries, 9);
    assert_eq!(full.transfer_characteristics, 14);
    assert_eq!(full.matrix_coefficients, 9);
    assert!(full.full_range);

    let limited = NclxColorMetadata::bt2020_sdr_10(false);
    assert_eq!(limited.colour_primaries, 9);
    assert_eq!(limited.transfer_characteristics, 14);
    assert_eq!(limited.matrix_coefficients, 9);
    assert!(!limited.full_range);
}

#[test]
fn bt2020_sdr_8_uses_2020_primaries_with_sdr_transfer() {
    let full = NclxColorMetadata::bt2020_sdr_8(true);
    assert_eq!(full.colour_primaries, 9);
    assert_eq!(full.transfer_characteristics, 1);
    assert_eq!(full.matrix_coefficients, 9);
    assert!(full.full_range);

    let limited = NclxColorMetadata::bt2020_sdr_8(false);
    assert_eq!(limited.colour_primaries, 9);
    assert_eq!(limited.transfer_characteristics, 1);
    assert_eq!(limited.matrix_coefficients, 9);
    assert!(!limited.full_range);
}

#[test]
fn muxer_can_emit_video_and_aac_tracks() {
    let dir = std::env::temp_dir();
    let unique = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap()
        .as_nanos();
    let path = dir.join(format!(
        "rustreplay_mux_audio_test_{}_{}.mp4",
        std::process::id(),
        unique
    ));
    let video = HevcMp4Track {
        width: 16,
        height: 16,
        duration_90k: 90_000,
        color: NclxColorMetadata::bt709_full(),
        codec: HevcCodecMetadata::main_420_8(),
        samples: vec![HevcAccessUnit {
            timestamp_90k: 0,
            data: fake_hevc_annex_b_access_unit().into(),
            is_sync: true,
            discard_from_track: false,
        }],
    };
    let audio = AacLcMp4Track {
        sample_rate: 48_000,
        channel_count: 2,
        duration_ticks: 48_000,
        samples: vec![
            AacAccessUnit {
                timestamp_ticks: 0,
                duration_ticks: 1024,
                data: vec![0x21, 0x10, 0x04, 0x60].into(),
            },
            AacAccessUnit {
                timestamp_ticks: 1024,
                duration_ticks: 1024,
                data: vec![0x21, 0x10, 0x04, 0x61].into(),
            },
        ],
    };
    write_hevc_aac_mp4(&path, &video, Some(&audio)).unwrap();
    let bytes = std::fs::read(&path).unwrap();
    let _ = std::fs::remove_file(&path);
    for needle in [b"hvc1".as_slice(), b"mp4a", b"esds", b"vide", b"soun"] {
        assert!(
            bytes.windows(needle.len()).any(|window| window == needle),
            "missing box/handler marker {:?}",
            String::from_utf8_lossy(needle)
        );
    }
}

#[test]
fn muxer_preserves_aac_leading_offset_with_an_edit_list() {
    let path = std::env::temp_dir().join(format!(
        "rustreplay_mux_audio_offset_{}_{}.mp4",
        std::process::id(),
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_nanos()
    ));
    let video = HevcMp4Track {
        width: 16,
        height: 16,
        duration_90k: 90_000,
        color: NclxColorMetadata::bt709_full(),
        codec: HevcCodecMetadata::main_420_8(),
        samples: vec![HevcAccessUnit {
            timestamp_90k: 0,
            data: fake_hevc_annex_b_access_unit().into(),
            is_sync: true,
            discard_from_track: false,
        }],
    };
    let audio = AacLcMp4Track {
        sample_rate: 48_000,
        channel_count: 2,
        duration_ticks: 48_000,
        samples: vec![
            AacAccessUnit {
                timestamp_ticks: 12_000,
                duration_ticks: 1_024,
                data: vec![0x21, 0x10, 0x04, 0x60].into(),
            },
            AacAccessUnit {
                timestamp_ticks: 13_024,
                duration_ticks: 1_024,
                data: vec![0x21, 0x10, 0x04, 0x61].into(),
            },
        ],
    };

    let index = write_hevc_aac_mp4_with_index(&path, &video, Some(&audio)).unwrap();
    let bytes = std::fs::read(&path).unwrap();
    let _ = std::fs::remove_file(&path);

    assert_eq!(
        index.audio_track.unwrap().samples[0].timestamp_ticks,
        12_000
    );
    for needle in [b"edts".as_slice(), b"elst"] {
        assert!(bytes.windows(needle.len()).any(|window| window == needle));
    }
}

#[test]
fn hevc_annex_b_detects_sync_and_extracts_parameter_sets() {
    let au = fake_hevc_annex_b_access_unit();

    let (_, is_sync, _) = hevc_annex_b_to_length_prefixed(&au).unwrap();
    assert!(is_sync);
    assert!(hevc_annex_b_has_random_access_nal(&au));

    let sets = hevc_annex_b_parameter_set_access_unit(&au).unwrap();
    assert!(sets.windows(2).any(|window| window == [0, 1]));
    assert!(!sets.windows(2).any(|window| window == [38, 1]));
}

#[test]
fn video_track_rejects_non_key_first_sample() {
    let mut non_key = Vec::new();
    append_fake_nal(&mut non_key, 32, &[1, 2, 3]);
    append_fake_nal(&mut non_key, 33, &[4, 5, 6]);
    append_fake_nal(&mut non_key, 34, &[7, 8, 9]);
    append_fake_nal(&mut non_key, 1, &[10, 11, 12]);
    let track = HevcMp4Track {
        width: 16,
        height: 16,
        duration_90k: 90_000,
        color: NclxColorMetadata::bt709_full(),
        codec: HevcCodecMetadata::main_420_8(),
        samples: vec![HevcAccessUnit {
            timestamp_90k: 0,
            data: non_key.into(),
            is_sync: false,
            discard_from_track: false,
        }],
    };

    let err = writer::prepare_video_track(&track).unwrap_err();
    assert!(err.to_string().contains("不是 IDR/CRA 关键帧"));
}

#[test]
fn video_track_does_not_trust_a_false_sync_flag() {
    let mut non_key = Vec::new();
    append_fake_nal(&mut non_key, 32, &[1, 2, 3]);
    append_fake_nal(&mut non_key, 33, &[4, 5, 6]);
    append_fake_nal(&mut non_key, 34, &[7, 8, 9]);
    append_fake_nal(&mut non_key, 1, &[10, 11, 12]);
    let track = HevcMp4Track {
        width: 16,
        height: 16,
        duration_90k: 90_000,
        color: NclxColorMetadata::bt709_full(),
        codec: HevcCodecMetadata::main_420_8(),
        samples: vec![HevcAccessUnit {
            timestamp_90k: 0,
            data: non_key.into(),
            is_sync: true,
            discard_from_track: false,
        }],
    };

    let err = writer::prepare_video_track(&track).unwrap_err();
    assert!(err.to_string().contains("不是 IDR/CRA 关键帧"));
}

#[test]
fn hevc_bla_is_a_random_access_nal() {
    let mut bla = Vec::new();
    append_fake_nal(&mut bla, 16, &[1, 2, 3]);
    assert!(hevc_annex_b_has_random_access_nal(&bla));
}

#[test]
fn mdat_header_switches_to_large_size() {
    let small = make_mdat_header(4);
    assert_eq!(&small[0..4], &12u32.to_be_bytes());
    assert_eq!(&small[4..8], b"mdat");

    let payload = u32::MAX as u64;
    let large = make_mdat_header(payload);
    assert_eq!(&large[0..4], &1u32.to_be_bytes());
    assert_eq!(&large[4..8], b"mdat");
    assert_eq!(&large[8..16], &(payload + 16).to_be_bytes());
}

#[test]
fn chunk_offsets_switch_to_co64_when_needed() {
    let stco = make_chunk_offsets(&[32, u32::MAX as u64]).unwrap();
    assert!(stco.windows(4).any(|window| window == b"stco"));
    assert!(!stco.windows(4).any(|window| window == b"co64"));

    let co64 = make_chunk_offsets(&[32, u32::MAX as u64 + 1]).unwrap();
    assert!(co64.windows(4).any(|window| window == b"co64"));
    assert_eq!(&co64[16..24], &32u64.to_be_bytes());
    assert_eq!(&co64[24..32], &(u32::MAX as u64 + 1).to_be_bytes());
}

fn fake_hevc_annex_b_access_unit() -> Vec<u8> {
    let mut out = Vec::new();
    append_fake_nal(&mut out, 32, &[1, 2, 3]); // VPS
    out.extend_from_slice(&[0, 0, 0, 1]);
    out.extend(synthetic_sps_nal(
        HevcCodecMetadata::main_420_8(),
        0x6000_0000,
        120,
    ));
    append_fake_nal(&mut out, 34, &[7, 8, 9]); // PPS
    append_fake_nal(&mut out, 19, &[10, 11, 12]); // IDR
    out
}

fn append_fake_nal(out: &mut Vec<u8>, nal_type: u8, payload: &[u8]) {
    out.extend_from_slice(&[0, 0, 0, 1]);
    out.push(nal_type << 1);
    out.push(1);
    out.extend_from_slice(payload);
}
