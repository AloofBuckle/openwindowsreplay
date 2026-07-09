# RustReplay experimental branch failure record

Date: 2026-07-10

This document records optimization experiments that were intentionally not kept
in the production mainline. The production baseline after cleanup is
`master@e487e65` plus this record.

## Kept in production

- oneVPL/D3D11 GPU-only production backend remains the shipping route.
- DDA/WGC source timestamps remain the MP4 VFR timeline; no external CFR clock
  is introduced.
- Production route remains one GPU copy into oneVPL-owned D3D11 surfaces.
- WGC same-device ordinary ring textures and oneVPL native surface handle cache
  were already merged before this cleanup.

## Failed or abandoned experiments

### oneVPL ImportFrameSurface / external surface import

Branches/worktrees:

- `codex/vpl-import-surface-flags`
- `codex/direct-vpl-surface-experiment`

Conclusion: not merged.

Reason: the tested `MFX_SURFACE_FLAG_IMPORT_SHARED` and
`MFX_SURFACE_FLAG_IMPORT_SHARED | MFX_SURFACE_FLAG_IMPORT_COPY` routes did not
provide a dependable production zero-copy replacement for the current copy into
oneVPL-owned D3D11 surfaces. Even where the API boundary could be exercised, it
added fragile ownership, synchronization, and surface-lifetime behavior without
proving a better end-to-end path. The product target is therefore kept at the
one-copy GPU route.

### D3D11 direct oneVPL surface writes

Branch/worktree:

- `codex/direct-vpl-surface-experiment`

Conclusion: not merged.

Reason: direct writes into oneVPL surfaces are not a safe production contract
for this backend. The oneVPL allocator and frame-surface interface remain the
authority for encode surfaces, and bypassing the explicit copy path made the
route harder to reason about without producing a clean, stable improvement.

### D3D12 capture zero-copy probe

Branch/worktree:

- `codex/d3d12-zero-copy-experiment`

Conclusion: archived as a probe only.

Reason: DDA/WGC textures can cross some D3D11/D3D12 boundaries, but the source
texture is still a desktop-format input. HDR/scRGB/BGRA desktop capture still
requires GPU conversion into the encode format before HEVC Main10. This probe
does not remove the real production work in the oneVPL backend.

### Native D3D12 Video Encode backend

Branch/worktree:

- `codex/d3d12va-encode-experiment`

Conclusion: not a mainline optimization; archived as future-backend research.

Reason: later testing showed that native D3D12 Video Encode HEVC Main10/P010 can
be initialized on the test host with FFmpeg-style negotiation, and simple
encoded output can be produced. However, this is a replacement encoder backend,
not a local optimization of the current oneVPL backend. It still needs
production SPS/PPS/VPS handling, real DDA/WGC conversion integration, reference
management, rate-control mapping, HDR metadata handling, and a full validation
matrix before it can be compared fairly with the stable oneVPL route.

### FFmpeg D3D12VA / zero-copy references

Branches/worktrees:

- `codex/ffmpeg-zero-copy-experiment`
- `codex/ffmpeg-encode-experiment`

Conclusion: not merged.

Reason: the FFmpeg experiments are useful references for D3D12VA negotiation,
packet format, and muxing behavior. They are not suitable as production code in
this Rust GUI backend, and they do not improve the current oneVPL route without
switching encoder architecture.

### GPU timestamp and stage measurement branches

Branches/worktrees:

- `codex/gpu-timestamp-experiment`
- `codex/wgc-gpu-timestamp-experiment`
- `codex/vpl-stage-measurements`

Conclusion: not merged.

Reason: these branches were instrumentation-only. They established the useful
optimization priorities: shader conversion is the largest single GPU segment,
oneVPL surface copy is measurable but stable, and old WGC shared/keyed snapshot
backlog was a major source of queue pressure. The production fixes derived from
that work have already been merged where appropriate; the measurement code
itself should not remain in the shipping application.

### HDR PQ P010 420 specialized converter

Worktree:

- `C:\Users\Administrator\Desktop\RustReplay-mainline`

Conclusion: not merged.

Reason: A/B testing on the real HDR PQ P010 420 path measured only a small
shader-stage improvement, about `873 us/frame` to `840 us/frame` in the tested
configuration. That is roughly `34 us/frame` and does not materially change the
full recording pipeline. The added specialization and test hooks are not worth
shipping now.

## Archive policy

The removed branches and worktrees were archived under
`Y:\Program\Code\测试\RustReplay-experimental-archive-20260710`. The archive
contains branch bundles, uncommitted patch files where available, and snapshots
of the removed experiment directories for later forensic reference.
