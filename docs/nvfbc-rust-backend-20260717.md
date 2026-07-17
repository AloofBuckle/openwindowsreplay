# NvFBC Rust backend integration (2026-07-17)

## Status

The validated NvFBC V3 D3D9Ex-to-NVENC route has been rewritten as a dormant
RustReplay backend. It is compiled into the normal single executable, but it is
not called by the global `ProbeCaps`, GUI, automatic backend selector, replay
ring, audio pipeline, or MP4 save path. Existing DDA/WGC behavior is therefore
unchanged.

The old standalone Rust CLI and C++ experiment sources were removed from the
branch tip. Their measurements and raw audit logs remain under `docs/` and the
source remains available in Git history.

## Modules and ownership

- `src/backend/nvfbc/mod.rs` is the safe outward state machine. It exposes
  `probe`, `NvFbcOptions`, and `NvFbcRecorder` without any `unsafe` block.
- `src/backend/nvfbc/d3d9.rs` owns NVIDIA adapter selection, the D3D9Ex device,
  the matching DXGI output, active refresh/color state, vblank waiting, and the
  three ARGB10 render targets.
- `src/backend/nvfbc/ffi.rs` is the only Rust module that calls the C shim. The
  opaque session is RAII-owned and deliberately `!Send`/`!Sync`.
- `src/backend/nvenc/d3d9.rs` owns the D3D9 NVENC session, capability queries,
  persistent registrations, map/submit/ordered-drain state, bitstream copying,
  flush, timestamp verification, and VUI verification.
- `native/nvfbc_shim.cpp` is statically linked. It contains only the private
  NvFBC ABI structs, DLL loading, private data, and the four validated vtable
  entry calls.

The shim does not use compiler-generated C++ virtual dispatch. The NVIDIA
object follows the driver's MSVC ABI while RustReplay may be built with MinGW;
the first rewrite exposed that mismatch as an access violation in `Release`.
The final shim copies vtable entries 0..3 into explicitly typed C function
pointers, matching the previously validated standalone probe and removing the
compiler C++ ABI dependency.

The shim uses a fixed three-entry POD array and the Windows process heap. It is
built without exceptions, RTTI, or a C++ standard-library link. Import-table
audits of both the production executable and the hardware-test executable show
no `libstdc++-6.dll` dependency, so enabling this code does not turn the product
into a multi-file C++ runtime deployment.

Destruction order is explicit in the Rust field layout:

1. mapped NVENC inputs are unmapped;
2. NvFBC surfaces are unregistered and bitstream buffers are destroyed;
3. the NVENC encoder session is destroyed;
4. the NvFBC session is released;
5. the three D3D9Ex surfaces are released;
6. the D3D9Ex device and DLL are released.

The bitstream bytes are copied into Rust-owned memory before NVENC unlocks the
buffer. No session or resource wrapper implements `Send` or `Sync`.

## Capability and configuration contract

`probe()` creates a real D3D9Ex NvFBC session and a real D3D9 NVENC query
session. Route support is computed from the current dimensions and these
runtime results:

- HEVC codec GUID;
- ABGR10 input format;
- Main10 and FRExt profile GUIDs;
- 10-bit, 4:2:2, and 4:4:4 caps;
- maximum width and height;
- CBR/VBR/CQP mask, P1..P7 preset GUIDs, and encoder-engine count;
- matching `IDXGIOutput6` color space and bits per color.

The backend currently rejects every non-HDR-PQ-10-bit desktop with the normal
"unsupported desktop mode" backend error. It does not infer support from the
RTX 5090 validation result. 4:2:0 uses Main10; 4:2:2 and 4:4:4 use FRExt. All
three register the exact NvFBC ARGB10 surfaces as NVENC ABGR10 inputs.

The recorder consumes the existing `RateControlConfig`. CBR, VBR, CQP, preset,
split encode (SFE), multipass, and spatial AQ therefore use the same config
writers as the D3D11 backend. Lookahead is rejected before initialization and
the probe always reports:

```text
lookahead=false
lookahead_policy=disabled_for_nvfbc
```

## Timing contract

There is no requested capture FPS. `WaitForVBlank -> NvFBC NOWAIT` controls
capture arrival; the active display refresh is used only as NVENC's nominal
frame-rate hint. Each successful grab is timestamped from monotonic arrival
time and converted to a 90 kHz value relative to the first frame. Every NVENC
output timestamp must exactly equal its submitted timestamp or recording
fails. The backend never replaces those values with an external CFR clock.

NvFBC V3 does not expose a separate public source-QPC timestamp. Diff-map based
unchanged-frame suppression is also not implemented yet, so frontend/product
integration must decide whether scanout-cadence duplicates are acceptable or
add a proven GPU-only unchanged-frame detector. This limitation is isolated
from the existing WGC/DDA VFR routes because NvFBC is dormant.

## Hardware verification

The ignored Rust hardware test
`backend::nvfbc::tests::local_nvfbc_rust_backend_records_420_422_444` ran on:

- NVIDIA GeForce RTX 5090;
- 3840x2160 at 240 Hz;
- Windows HDR/PQ desktop;
- current `NvFBC64.dll` and NVENC driver API.

It recorded 240 frames per route through the Rust-owned three-surface pipeline.
Internal assertions proved exact input/output frame counts, strictly increasing
arrival timestamps, exact NVENC timestamp round trips, and an IDR first frame.

External results:

| Route | Profile | Decoded format | Frames | Color | Strict decode |
| --- | --- | --- | ---: | --- | --- |
| 4:2:0 10-bit | Main 10 | `yuv420p10le` | 240 | BT.2020/PQ/full | pass |
| 4:2:2 10-bit | Rext | `yuv422p10le` | 240 | BT.2020/PQ/full | pass |
| 4:4:4 10-bit | Rext | `yuv444p10le` | 240 | BT.2020/PQ/full | pass |

Strict decode used a rawvideo sink so raw HEVC demuxer-generated timestamps do
not create unrelated null-muxer DTS warnings:

```powershell
cargo test --release --bin rust_replay `
  local_nvfbc_rust_backend_records_420_422_444 -- `
  --ignored --nocapture --test-threads=1

ffprobe -v error -count_frames -show_streams target/nvfbc-rust-420.hevc
ffmpeg -v error -err_detect explode -i target/nvfbc-rust-420.hevc `
  -map 0:v:0 -f rawvideo NUL
```

The same checks were run for `422` and `444`, with zero decoder output and exit
code 0.

## Deferred product integration

This merge intentionally does not define the GUI shape or automatic mode
switching. Before presenting NvFBC to users, a later stage still needs to wire
the returned HEVC access units into audio, the encoded replay ring, and MP4;
define cursor and unchanged-frame policy; and validate multi-monitor selection,
mode changes, sleep/resume, protected content, exclusive fullscreen games, and
additional NVIDIA generations/drivers.

The required 16-byte private data and Windows NvFBC interface remain
undocumented driver contracts. Runtime probing and clean failure are mandatory;
the backend must never be advertised solely because the executable contains
this code.
