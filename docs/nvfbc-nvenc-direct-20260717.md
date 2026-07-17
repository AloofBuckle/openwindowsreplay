# NvFBC D3D9Ex surface to NVENC direct registration (2026-07-17)

## Scope

This document records the original isolated C++ proof. The route has since been
rewritten as a dormant Rust backend; see
`docs/nvfbc-rust-backend-20260717.md`. It still does not modify the RustReplay
GUI or automatic backend selection. The original proof validates HDR/PQ 10-bit
HEVC 4:2:0, 4:2:2, and 4:4:4 from the same NvFBC ARGB10 capture route without
an application-issued post-capture GPU copy.

Lookahead is deliberately out of scope. The probe always sets
`enableLookahead=0` and `lookaheadDepth=0`, and the capability interface reports
`lookahead=false`.

Test system:

- NVIDIA GeForce RTX 5090
- `NvFBC64.dll` file version `6.14.16.1074`
- 3840x2160 at 240 Hz, Windows HDR/PQ enabled
- NvFBC V3 ARGB10 capture
- NVENC 13 headers and HEVC encoder

## Direct resource path

The tested path is:

1. Select the NVIDIA D3D9Ex adapter and its matching DXGI output.
2. Obtain the active refresh rate from `EnumDisplaySettingsW`.
3. Create three `D3DFMT_A2B10G10R10` render targets on the NvFBC D3D9Ex
   device and give those exact surfaces to NvFBC V3.
4. Open NVENC on the same `IDirect3DDevice9Ex*`.
5. Query HEVC input formats, profiles, chroma/bit-depth caps, and maximum
   dimensions before selecting a route.
6. Register all three NvFBC surfaces directly as `NV_ENC_BUFFER_FORMAT_ABGR10`.
7. Rotate capture, mapped NVENC input, and bitstream slots; drain in submission
   order and verify every returned timestamp before reusing a surface.

The loop contains no `CopyResource`, `StretchRect`, shared handle, staging
surface, readback, D3D11 device, or application shader conversion. NVENC still
performs its internal RGB10-to-YUV conversion and may use driver-private
storage. The accurate claim is therefore "no application-issued post-capture
GPU copy," not absolute zero-copy inside the driver.

## Capability interface

Route selection is not tied to the RTX 5090 result. The probe accepts chroma
sampling and QP as caller options, obtains dimensions from NvFBC, obtains the
refresh rate from the selected display, and queries the following NVENC data:

- HEVC `ABGR10` input format support
- Main10 and FRExt profile GUIDs
- `NV_ENC_CAPS_SUPPORT_10BIT_ENCODE`
- `NV_ENC_CAPS_SUPPORT_YUV422_ENCODE`
- `NV_ENC_CAPS_SUPPORT_YUV444_ENCODE`
- maximum encode width and height

It emits one stable machine-readable capability record:

```json
route_caps_json={"schema":1,"device_api":"D3D9Ex","codec":"HEVC","input_format":"ABGR10","hevc":true,"abgr10_input":true,"main10_profile":true,"frext_profile":true,"ten_bit":true,"yuv422":true,"yuv444":true,"max_width":8192,"max_height":8192,"advertised_routes":{"420":true,"422":true,"444":true},"lookahead":false,"lookahead_policy":"disabled_for_nvfbc"}
```

`advertised_routes` means the static profile/format/caps intersection for the
current dimensions. It is intentionally distinct from runtime validation. A
selected route must also pass `nvEncInitializeEncoder`, registration of every
NvFBC surface, submission, timestamp-ordered drain, and external strict decode.
The probe prints `requested_route_initialized=true` only after Init and all
resource registrations succeed.

For a future mainline integration, this record maps directly to a backend
`ProbeCaps` result. The GUI/automatic selector may expose only advertised
routes, while recording startup must still validate the selected route and
return `Unsupported desktop mode` if runtime initialization fails. No adapter,
chroma route, display size, refresh rate, or QP is inferred from this machine's
test result.

The current mainline types already provide the required landing points:

- width, height, 10-bit, 4:2:2, and 4:4:4 caps map to `NvencCapsInfo`;
- ABGR10 and Main10/FRExt enumeration map to `NvencProbeInfo.input_formats`
  and `hevc_profiles`;
- each advertised chroma choice maps to an `NvencRouteProbe` with
  `input_format=ABGR10`, bit depth 10, and the selected Main10/FRExt profile;
- adapter/output identity and nclx values continue to come from the existing
  `NvencCurrentDisplayRouteInfo` / `IDXGIOutput6` display probe.

Until the complete NvFBC capture backend is integrated, those future route
entries must keep `production_record_supported=false`; passing this standalone
probe must not make the current D3D11 backend claim it owns the D3D9Ex route.

## HEVC route configuration

All three routes use the NvFBC-written ABGR10 surface as NVENC input. The
requested output sampling determines profile and `chromaFormatIDC`:

| Requested route | Profile | `chromaFormatIDC` | Expected decoded format |
| --- | --- | ---: | --- |
| 4:2:0 10-bit | Main10 | 1 | `yuv420p10le` |
| 4:2:2 10-bit | FRExt | 2 | `yuv422p10le` |
| 4:4:4 10-bit | FRExt | 3 | `yuv444p10le` |

BT.2020 primaries, PQ transfer, BT.2020 non-constant matrix, and full range are
written for this HDR probe. A production route must take those values from the
display/capture route plan rather than assume every NvFBC session is HDR.

## Results

The original single-surface loop showed why a small no-Lookahead pool is still
needed. 4:4:4 encode work approached the 4.167 ms refresh interval and missed
47 source intervals over ten seconds. The three-surface pipeline separates
submit from ordered bitstream drain without allowing NvFBC to overwrite an
in-flight input.

| Route | Surfaces | Frames | FPS | Source long intervals | Submit P50/P95 | Pipeline P50/P95 | Result |
| --- | ---: | ---: | ---: | ---: | ---: | ---: | --- |
| 4:2:2 | 1 | 2400/2400 | 239.385 | 5 | 2.445/2.803 ms combined encode | n/a | Pass |
| 4:4:4 | 1 | 2400/2400 | 235.146 | 47 | 3.516/3.756 ms combined encode | n/a | Pass, below refresh cadence |
| 4:2:2 | 3 | 2400/2400 | 239.838 | 1 | 0.164/0.294 ms | 0.184/0.333 ms | Pass |
| 4:4:4 | 3 | 2400/2400 | 239.856 | 1 | 0.153/0.261 ms | 0.171/0.296 ms | Pass |

The final 4:2:2 and 4:4:4 runs each contain exactly 2400 frames. `ffprobe`
reports:

```text
4:2:2: profile=Rext pix_fmt=yuv422p10le nb_read_frames=2400
4:4:4: profile=Rext pix_fmt=yuv444p10le nb_read_frames=2400
both: 3840x2160, pc, bt2020nc, smpte2084, bt2020, 240/1
```

`ffmpeg -err_detect explode` decodes both complete streams without an error.
A 600-frame 4:2:0 Main10 regression run also reports 600 frames and decodes
without an error after the three-surface refactor.
The final evidence logs are:

- `docs/nvfbc-nvenc-422-pipeline-2400f-run1-20260717.log`
- `docs/nvfbc-nvenc-444-pipeline-2400f-run1-20260717.log`

## Status after the Rust rewrite

Within this isolated probe, HDR/PQ HEVC 10-bit 4:2:0, 4:2:2, and 4:4:4 are
functional direct-resource routes. 4:2:2 and 4:4:4 have passed actual Init,
registration, 2400-frame encode, exact frame count, metadata inspection, and
strict decode; they are not caps-only branches.

The D3D9/NVENC session, runtime capability mapping, current-display HDR/PQ
metadata, three-slot ownership, VFR arrival timestamps, and 420/422/444 hardware
tests now live in Rust-owned backend modules. The merge intentionally keeps the
backend dormant. Audio, the encoded ring, MP4 save, unchanged-frame policy,
cursor policy, GUI/automatic selection, mode-change recovery, and broader
hardware validation remain deferred product integration work.

Lookahead is intentionally excluded from the NvFBC route. A follow-up probe
showed that D3D9Ex setup fails with four output surfaces and that the legacy
NvFBC CUDA interface fails during Setup for every HDR/format combination. The
route therefore remains fixed to three direct D3D9Ex surfaces and does not add
a copy-backed analysis ring. See `docs/nvfbc-lookahead-feasibility-20260717.md`.

## Historical reproduction

```powershell
git show 233da20:experiments/nvfbc_nvenc_direct.cpp

# Current Rust hardware regression:
cargo test --release --bin rust_replay `
  local_nvfbc_rust_backend_records_420_422_444 -- `
  --ignored --nocapture --test-threads=1
```
