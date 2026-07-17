use super::*;
use crate::rate_control::{NvencPreset, RateControlMethod};
use std::fs;
use std::path::PathBuf;

#[test]
#[ignore = "requires a modern NVIDIA driver exposing NvFBC V3 and an HDR PQ desktop"]
fn local_nvfbc_rust_backend_records_420_422_444() {
    eprintln!("nvfbc_rust_stage=probe_begin");
    let probe = probe();
    eprintln!(
        "nvfbc_rust_stage=probe_end available={} error={:?}",
        probe.available, probe.error
    );
    assert!(probe.available, "NvFBC probe failed: {:?}", probe.error);
    for chroma in [
        ChromaSampling::Yuv420,
        ChromaSampling::Yuv422,
        ChromaSampling::Yuv444,
    ] {
        record_route(chroma);
    }
}

fn record_route(chroma: ChromaSampling) {
    const FRAME_COUNT: usize = 240;
    let rate_control = RateControlConfig {
        method: RateControlMethod::Cqp,
        qpi: 27,
        qpp: 27,
        qpb: 27,
        look_ahead_depth: 0,
        nvenc_preset: NvencPreset::P1,
        ..RateControlConfig::default()
    };
    eprintln!("nvfbc_rust_stage=open_begin chroma={chroma:?}");
    let mut recorder = NvFbcRecorder::open(NvFbcOptions {
        chroma,
        rate_control,
        ..NvFbcOptions::default()
    })
    .expect("open Rust NvFBC backend");
    eprintln!("nvfbc_rust_stage=open_end chroma={chroma:?}");
    let mut access_units = Vec::with_capacity(FRAME_COUNT);
    let mut arrival_timestamps = Vec::with_capacity(FRAME_COUNT);
    for frame in 0..FRAME_COUNT {
        let captured = recorder
            .capture_next()
            .expect("capture and submit NvFBC frame");
        arrival_timestamps.push(captured.capture_timestamp_90k);
        access_units.extend(captured.access_units);
        if frame == 0 || (frame + 1) % 60 == 0 {
            eprintln!(
                "nvfbc_rust_stage=capture chroma={chroma:?} frames={}",
                frame + 1
            );
        }
    }
    eprintln!("nvfbc_rust_stage=finish_begin chroma={chroma:?}");
    access_units.extend(recorder.finish().expect("flush Rust NvFBC backend"));
    eprintln!("nvfbc_rust_stage=finish_end chroma={chroma:?}");

    assert_eq!(access_units.len(), FRAME_COUNT);
    assert_eq!(arrival_timestamps[0], 0);
    assert!(arrival_timestamps.windows(2).all(|pair| pair[0] < pair[1]));
    assert!(
        access_units
            .iter()
            .zip(arrival_timestamps.iter())
            .all(|(unit, timestamp)| unit.timestamp_90k == *timestamp)
    );
    assert!(access_units[0].is_sync);

    let mut elementary_stream = Vec::new();
    for unit in &access_units {
        elementary_stream.extend_from_slice(&unit.data);
    }
    let label = match chroma {
        ChromaSampling::Yuv420 => "420",
        ChromaSampling::Yuv422 => "422",
        ChromaSampling::Yuv444 => "444",
    };
    let output = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("target")
        .join(format!("nvfbc-rust-{label}.hevc"));
    fs::write(&output, elementary_stream).expect("write NvFBC HEVC hardware-test output");
    eprintln!(
        "nvfbc_rust_output={} frames={FRAME_COUNT}",
        output.display()
    );
}
