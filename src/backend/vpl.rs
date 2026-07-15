#![allow(non_snake_case, dead_code, unsafe_op_in_unsafe_fn)]
//! oneVPL and NVENC capability probing with the production D3D11 recording backend.
//!
//! The public API stays in this facade. Route selection, FFI, probing, capture,
//! conversion, encoding, and timing live in focused child modules.

use crate::backend::mp4_mux::{HevcCodecMetadata, NclxColorMetadata};
use crate::config::{AppConfig, ChromaSampling};
use crate::error::BackendError;
use crate::rate_control::{RateControlConfig, RateControlMethod};
use libloading::Library;
use serde::{Deserialize, Serialize};
use std::collections::{BTreeSet, HashMap, VecDeque};
use std::ffi::{c_char, c_void};
use std::path::{Path, PathBuf};
use std::ptr;

mod capture;
mod capture_dda;
mod capture_wgc;
mod convert;
mod encode;
mod ffi;
mod probe;
mod record;
mod record_loop;
mod record_nvenc;
mod route;
mod shaders;
mod timing;

use capture::*;
use capture_dda::*;
use capture_wgc::*;
use convert::*;
use encode::*;
use ffi::*;
use probe::*;
use record::*;
use record_loop::*;
use route::*;
use shaders::*;
use timing::*;

#[allow(unused_imports)]
pub use probe::{
    VplCurrentDisplayRouteInfo, VplImplementationInfo, VplProbeInfo, VplRateControlFeatureProbe,
    VplRouteProbe, probe_vpl,
};
#[allow(unused_imports)]
pub use record::{
    VplOneCopyRecordOutput, VplOneCopyRecordReport, VplOneCopyRecordSink, VplOutputTrackInfo,
    record_d3d11_onecopy_memory_output_cancelable,
    record_d3d11_onecopy_memory_output_with_sink_cancelable, record_d3d11_onecopy_mp4,
    record_d3d11_onecopy_mp4_cancelable, record_d3d11_onecopy_mp4_output_cancelable,
    record_d3d11_onecopy_mp4_output_with_route_cancelable,
    record_wgc_d3d11_onecopy_memory_output_cancelable,
    record_wgc_d3d11_onecopy_memory_output_with_sink_cancelable, record_wgc_d3d11_onecopy_mp4,
    record_wgc_d3d11_onecopy_mp4_cancelable, record_wgc_d3d11_onecopy_mp4_output_cancelable,
    record_wgc_d3d11_onecopy_mp4_output_with_route_cancelable,
};
pub use record_nvenc::{
    record_nvenc_d3d11_onecopy_memory_output_with_sink_cancelable,
    record_nvenc_d3d11_onecopy_mp4_output_cancelable,
    record_nvenc_wgc_d3d11_onecopy_memory_output_with_sink_cancelable,
    record_nvenc_wgc_d3d11_onecopy_mp4_output_cancelable,
};

#[cfg(test)]
mod tests;
