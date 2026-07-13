//! Capture-side D3D11 resources, frame slots, and synchronization helpers.

use super::*;

mod d3d11;
mod resources;
mod slots;

pub(super) use d3d11::*;
pub(super) use resources::*;
pub(super) use slots::*;
