# Modern Windows NvFBC probe (2026-07-17)

## Scope

This experiment lives only on `codex/nvfbc-modern-probe`, based on
`master@22e588e`. It does not modify the production GUI or recording backend.

The probe dynamically loads the installed `System32/NvFBC64.dll` and uses the
legacy Windows Capture SDK 7 ABI. It never calls `NvFBC_Enable`, replaces the
driver DLL, changes the registry, or patches the driver.

References used to reconstruct the ABI and scheduling options:

- Sunshine `9d2409f71b60f1812f482e6dd807dc52e2f72fe7`, Linux CUDA NvFBC backend.
- CloudyNvCapture `aa5a108d7905021c6570aac13e7798e4ede7a4c7`, old Windows SDK headers/samples.
- nvfbc-relay `4e138c6590be7cc2adbdfc807836f3580f87477c`, SDK 7.1 ARGB10/HDR definitions.

## Test system

- Machine: Buckle
- GPU: NVIDIA GeForce RTX 5090
- Desktop: 3840x2160, 240 Hz, Windows Advanced Color/HDR PQ enabled
- `NvFBC64.dll`: file version `6.14.16.1074`
- NvFBC ABI reported by the driver: `0x70`
- Workload: Vulkan Signal Pattern, Hard Noise, native HDR PQ at 240 Hz

## API feasibility

`NvFBC_GetSDKVersion` and `NvFBC_GetStatusEx` succeed from an ordinary Rust
process. The returned status reports capture possible, create-now, multi-head,
and multi-client support.

`NvFBC_CreateEx` behavior on the same D3D9Ex RTX 5090 device:

| Create input | Result |
| --- | --- |
| No private data | `NVFBC_ERROR_DRIVER_FAILURE (-5)` |
| Sunshine-compatible 16-byte private data | `NVFBC_SUCCESS` |

With the explicit private-data flag, both interface IDs succeed:

- `0x2002`: `NvFBCToDx9Vid_v2`, also observed in NVIDIA Replay logs.
- `0x2003`: `NvFBCToDx9Vid_v3`, Capture SDK 7.1 interface.

The driver reports a maximum capture surface of 3840x2160. V3 setup succeeds
with three D3D9Ex `A2B10G10R10` render targets, `ARGB10`, and `bHDRRequest=1`.

## Capture results

The first desktop-content run produced 936 frames in 10.026 seconds. Its
approximately 93 fps rate followed the changing desktop content and was not a
capture ceiling.

With the 240 Hz Hard Noise source:

| Scheduling method | Frames / 10 s | FPS | Return interval P50 | Grab call P50 | Grab-loop CPU |
| --- | ---: | ---: | ---: | ---: | ---: |
| NvFBC event-blocking Grab, release build | 2402 | 240.112 | about 4.23 ms | about 4.23 ms | 56.54% of one core |
| DXGI `WaitForVBlank` + NvFBC `NOWAIT` | 2400 | 239.989 | 4.166 ms | 0.200 ms | 13.12% of one core |

All steady-state frames reported 3840x2160 and HDR. A final GPU-to-CPU readback
covered all 8,294,400 pixels and produced non-zero, changing ARGB10 hashes. No
black frame or invalid surface was observed.

The NvFBC `GPUBasedCPUSleep` method returned `NVFBC_ERROR_GENERIC` on the V3
session and is not usable as the scheduler on this driver.

On this 32-logical-processor system, the vblank route's 13.12% of one core is
about 0.41% machine CPU before encoding. This is close to the previously
measured NVIDIA Replay process CPU order of magnitude.

## DDA fallback exclusion

Static inspection of `NvFBC64.dll` shows that this driver still contains a
standard Desktop Duplication fallback implementation, including strings for
`DDAOwner::CaptureToDx9`, `DDAImpl::Init`, `DuplicateOutput`, and
`AcquireNextFrame`. The DLL also imports `D3D11CreateDevice`. Static inspection
therefore cannot establish that a successful NvFBC session uses the native
framebuffer route.

The probe's `--deny-dda` mode installs process-local deny hooks for every unique
`IDXGIOutput1::DuplicateOutput` and `IDXGIOutput5::DuplicateOutput1` entry point
found across all enumerated DXGI outputs. The hooks are installed before
`NvFBC64.dll` is loaded, so they cover `GetSDKVersion`, every `GetStatusEx`,
`CreateEx`, setup, the complete capture loop, and release. A self-test invokes
both patched entry points and verifies that they return
`DXGI_ERROR_NOT_CURRENTLY_AVAILABLE` before the counters are reset.

Two independent 10-second V3 HDR captures then completed while DDA creation was
denied:

| Run | Frames | FPS | HDR frames | `DuplicateOutput` calls | `DuplicateOutput1` calls | Readback |
| --- | ---: | ---: | ---: | ---: | ---: | --- |
| Early deny 1 | 2401 | 240.029 | 2400 | 0 | 0 | 8,294,400 non-zero pixels |
| Early deny 2 | 2401 | 240.068 | 2398 | 0 | 0 | 8,294,400 non-zero pixels |

This excludes the public DXGI Desktop Duplication path for the tested RTX 5090,
driver `6.14.16.1074`, V3 ARGB10 HDR route. A DDA duplication object cannot be
created without one of the denied entry points, and none existed before the
DLL was loaded. The result does not prove that every NVIDIA driver, output
mode, or failure recovery path avoids DDA; the DLL demonstrably retains that
fallback code.

## GPU A/B

PDH was sampled for the same Hard Noise process before and during NvFBC capture:

| Counter | Baseline | NvFBC | Delta |
| --- | ---: | ---: | ---: |
| Aggregate 3D average | 17.692% | 18.252% | +0.560 pp |
| Aggregate Copy average | 0.128% | 0.260% | +0.132 pp |

The final CPU readback caused most of the Copy maximum and is not part of the
normal capture loop. NvFBC work was not attributed to the probe process's own
3D/Copy context; the visible movement was primarily in DWM/System contexts.
PDH is coarse, so these values establish scale rather than precise GPU time.

## Conclusions

Modern Windows GeForce NvFBC is technically callable by a third-party process
on this driver. 4K240 HDR ARGB10 capture works, and its GPU cost is materially
lower than the existing explicit WGC color-conversion path.

For the exact tested route, the capture is not a wrapper around the public
DXGI Desktop Duplication API. Both DDA creation methods can be made unavailable
for the whole NvFBC lifetime without affecting capture success or cadence.

The useful scheduler is `DXGI WaitForVBlank -> NvFBC NOWAIT`. Calling the NvFBC
blocking Grab path directly wastes roughly half a CPU core at 240 Hz.

This is not production-ready yet:

- The required 16-byte private data is undocumented and not part of a supported
  Windows SDK contract. Driver compatibility and redistribution remain open.
- The probe captures ARGB10 D3D9 surfaces but does not yet feed RustReplay's
  D3D11 NVENC backend or produce MP4.
- The legacy Windows frame-info structure has no public source timestamp. A
  production route must timestamp each successful vblank/QPC capture without
  rewriting later timestamps.
- Vblank capture emits scanout cadence. Preserving the project's VFR behavior
  requires reliable unchanged-frame detection, likely using NvFBC diff maps or
  a low-cost GPU comparison.
- Multi-monitor selection, display hotplug/mode changes, cursor composition,
  protected content, sleep/resume, and multiple driver versions are untested.
- The current probe surfaces are not shared with NVENC. The next experiment
  must test D3D9Ex shared surfaces or a native NVENC D3D9 session and prove the
  capture-to-encode path does not add another GPU copy.

## Reproduction

Build and run from the experimental worktree:

```powershell
cargo build --release --target x86_64-pc-windows-msvc --bin nvfbc_probe
target\x86_64-pc-windows-msvc\release\nvfbc_probe.exe `
  --sunshine-private-data --capture --vblank-grab

# Exclude the standard DDA fallback for the complete NvFBC lifetime.
target\x86_64-pc-windows-msvc\release\nvfbc_probe.exe `
  --sunshine-private-data --capture --vblank-grab --deny-dda
```

Without `--sunshine-private-data`, the probe remains a native-access control
test and `NvFBC_CreateEx` is expected to fail on this GeForce configuration.
