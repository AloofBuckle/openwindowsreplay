//! oneVPL implementation discovery and capability queries.

use super::*;

mod discovery;
mod implementation;
mod params;

pub(super) use implementation::*;
pub(super) use params::*;

pub use discovery::{
    VplCurrentDisplayRouteInfo, VplImplementationInfo, VplProbeInfo, VplRateControlFeatureProbe,
    VplRouteProbe, probe_vpl,
};
