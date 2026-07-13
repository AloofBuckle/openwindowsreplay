#![allow(dead_code)]
//! Instant replay session state machine.
//!
//! The controller owns lifecycle and UI-facing state. Worker, disk-store,
//! segment, and sidecar modules own their respective background concerns.

use super::{ProbeCaps, mp4_mux, pipeline, vpl};
use crate::backend::mp4_mux::{
    AacAccessUnit, AacIndexedMp4Track, AacIndexedSample, AacLcMp4Track, AacPreparedMp4Track,
    AacPreparedSample, HevcAacMp4Index, HevcAccessUnit, HevcCodecMetadata, HevcIndexedMp4Track,
    HevcIndexedSample, HevcMp4Track, HevcParameterSets, HevcPreparedMp4Track, HevcPreparedSample,
    Mp4SampleFileRange, NclxColorMetadata,
};
use crate::config::{AppConfig, CaptureBackend, ChromaSampling, ReplayBufferMode};
use crate::error::BackendError;
use crate::ring::{EncodedReplayMetadata, EncodedReplayRing};
use std::collections::VecDeque;
use std::fs;
use std::io::{BufReader, BufWriter, Read, Write};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc::{self, Receiver, Sender, SyncSender, TrySendError};
use std::sync::{Arc, Mutex};
use std::thread::{self, JoinHandle};
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

const LIVE_RECORD_SECONDS: f32 = 24.0 * 60.0 * 60.0;
const DISK_SEGMENT_TARGET_SECONDS: f32 = 10.0;
const DISK_WRITER_QUEUE_CAPACITY: usize = 3;
const DISK_PENDING_SEGMENT_LIMIT: usize = 3;
const DISK_SEGMENT_SIDECAR_MAGIC: &[u8; 8] = b"RRSEG002";
const VIDEO_CLOCK_HZ: u64 = 90_000;

mod controller;
mod disk_segment;
mod disk_store;
mod sidecar;
mod worker;

use controller::*;
use disk_segment::*;
use disk_store::*;
use sidecar::*;
use worker::*;

pub use controller::{ReplayController, ReplaySaveReadiness, ReplayState};

#[cfg(test)]
mod tests;
