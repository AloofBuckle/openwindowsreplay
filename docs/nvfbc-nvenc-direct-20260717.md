# NvFBC D3D9Ex surface to NVENC direct registration (2026-07-17)

## Scope

This experiment remains isolated on `codex/nvfbc-modern-probe`; it does not
modify the production RustReplay recording backend or GUI. Its purpose is to
answer one question: can the D3D9Ex surface written by NvFBC be registered and
encoded by modern NVENC without an application-issued GPU copy?

The probe uses the current NVENC 13 header and the Capture SDK 7.1 NvFBC ABI.
It runs on the same Buckle system as the preceding DDA exclusion tests:

- NVIDIA GeForce RTX 5090
- `NvFBC64.dll` file version `6.14.16.1074`
- 3840x2160, 240 Hz, Windows HDR/PQ enabled
- NvFBC V3 ARGB10 capture
- HEVC Main10, 4:2:0, full-range BT.2020/PQ output

## Direct resource path

The tested path is:

1. Create one `D3DFMT_A2B10G10R10` render target on the NvFBC D3D9Ex device.
2. Pass that exact `IDirect3DSurface9*` to NvFBC V3 setup as ARGB10/HDR.
3. Open the NVENC session on the same `IDirect3DDevice9Ex*` with
   `NV_ENC_DEVICE_TYPE_DIRECTX`.
4. Register the same surface pointer with
   `NV_ENC_INPUT_RESOURCE_TYPE_DIRECTX` and
   `NV_ENC_BUFFER_FORMAT_ABGR10`.
5. For each frame, call NvFBC Grab, map the registered surface, encode, lock
   the bitstream, unlock, and unmap before NvFBC overwrites the surface.

The loop contains no `CopyResource`, `StretchRect`, shared handle, staging
surface, readback, D3D11 device, or cross-device synchronization. D3D9 command
ordering on the single device provides producer/consumer ordering for this
synchronous single-surface test.

Modern NVENC reports 16 HEVC input formats on this session and explicitly
includes ABGR10. Session creation, HEVC Main10 initialization, D3D9 surface
registration, mapping, and encoding all return `NV_ENC_SUCCESS`. The mapped
format is `0x20000000` (`NV_ENC_BUFFER_FORMAT_ABGR10`).

## Results

| Run | Frames | Elapsed | FPS | Grab P50/P95 | Encode P50/P95 | Encode max | Result |
| --- | ---: | ---: | ---: | ---: | ---: | ---: | --- |
| Smoke | 10 | 50.294 ms | 198.830 | 0.191/0.979 ms | 2.172/8.660 ms | 8.660 ms | Pass; includes first-frame warmup |
| Steady | 600 | 2512.167 ms | 238.838 | 0.196/0.311 ms | 1.927/2.111 ms | 9.520 ms | Pass |
| Stability | 2400 | 10026.547 ms | 239.365 | 0.225/0.332 ms | 1.931/2.296 ms | 9.599 ms | Pass |

The 2400-frame elementary stream contains exactly 2400 decodable frames.
`ffprobe` reports:

```text
codec_name=hevc
profile=Main 10
width=3840
height=2160
pix_fmt=yuv420p10le
color_range=pc
color_space=bt2020nc
color_transfer=smpte2084
color_primaries=bt2020
r_frame_rate=240/1
nb_read_frames=2400
```

`ffmpeg -err_detect explode` decodes all frames without an error.

## Conclusion

The direct D3D9Ex NvFBC-to-NVENC resource path is feasible on the tested modern
GeForce driver. The previously suspected API/device ownership conflict does
not apply when NVENC is opened on the same D3D9Ex device: NVENC accepts the
actual NvFBC output surface and sustains the 4K240 capture cadence.

This proves zero additional application-issued GPU copies after NvFBC. It does
not prove that NVENC performs no internal work: encoding 4:2:0 HEVC from ABGR10
necessarily includes an internal RGB10-to-YUV420 conversion, and the driver may
use private intermediate storage. The useful production claim is therefore
"no explicit post-capture copy or shader conversion in RustReplay," not
"absolute zero-copy inside the NVIDIA driver."

## Remaining production work

- Port the D3D9 NVENC session/resource registration into the Rust backend.
- Replace the synchronous single surface with an in-flight surface/bitstream
  pool before enabling lookahead, B frames, or delayed output.
- Preserve source timestamps in MP4; the elementary-stream probe uses a fixed
  240 Hz initialization value only to validate the encoder.
- Add unchanged-frame detection/diff-map handling for the project's VFR design.
- Integrate audio, encoded ring, save, cursor policy, mode-change recreation,
  and error recovery.
- Validate SDR, other NVIDIA generations/drivers, multi-monitor, sleep/resume,
  and full-screen game behavior.
- Resolve the unsupported private-data distribution and compatibility risk.

## Reproduction

```powershell
experiments\build_nvfbc_nvenc_direct.ps1
target\release\nvfbc_nvenc_direct.exe 2400 target\nvfbc_nvenc_direct.hevc
ffprobe -v error -count_frames -show_streams target\nvfbc_nvenc_direct.hevc
ffmpeg -v error -err_detect explode -r 240 `
  -i target\nvfbc_nvenc_direct.hevc -map 0:v:0 -fps_mode passthrough -f null NUL
```
