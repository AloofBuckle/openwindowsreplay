//! Minimal HEVC/H.265 and AAC MP4 muxer.
//!
//! This facade exposes track models and mux entry points. Sample preparation,
//! payload I/O, and ISO BMFF box construction are kept in separate modules.

use crate::error::BackendError;
use std::collections::BTreeSet;
use std::fs;
use std::fs::File;
use std::io::{self, BufReader, BufWriter, Read, Seek, SeekFrom, Write};
use std::path::{Path, PathBuf};
use std::sync::Arc;

mod boxes;
mod parameter_sets;
mod payload;
mod types;
mod writer;

use boxes::*;
pub(crate) use parameter_sets::HevcParameterSetTracker;
use payload::*;
use types::*;
#[cfg(test)]
use writer::hevc_annex_b_to_length_prefixed;

pub use types::{
    AacAccessUnit, AacLcMp4Track, HevcAccessUnit, HevcCodecMetadata, HevcMp4Track,
    NclxColorMetadata,
};
pub(crate) use types::{
    AacIndexedMp4Track, AacIndexedSample, AacPreparedMp4Track, AacPreparedSample, HevcAacMp4Index,
    HevcIndexedMp4Track, HevcIndexedSample, HevcParameterSets, HevcPreparedMp4Track,
    HevcPreparedSample, Mp4SampleFileRange,
};
#[cfg(test)]
pub(crate) use writer::hevc_annex_b_parameter_set_access_unit;
pub(crate) use writer::{
    hevc_annex_b_has_random_access_nal, write_hevc_aac_mp4_with_index, write_prepared_hevc_aac_mp4,
};
#[allow(unused_imports)]
pub use writer::{write_hevc_aac_mp4, write_hevc_mp4};

#[cfg(test)]
mod tests;
