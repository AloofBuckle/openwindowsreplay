# NvFBC Lookahead feasibility (2026-07-17)

## Decision

NVENC Lookahead is excluded from the NvFBC route. The route keeps its three
direct NvFBC D3D9Ex surfaces, sets `enableLookahead=0` and `lookaheadDepth=0`,
and reports:

```json
"lookahead":false,"lookahead_policy":"disabled_for_nvfbc"
```

This is a route policy even when the selected NVENC hardware advertises
Lookahead. RustReplay will not add a separate copy-backed lookahead ring or an
additional frame-analysis loop for NvFBC.

## D3D9Ex surface-count test

The original three-surface path passes NvFBC Setup, direct NVENC registration,
and sustained 4K240 encoding. To determine whether the pool could be enlarged,
the probe created four independent 3840x2160 `D3DFMT_A2B10G10R10` render
targets and passed all four through `dwNumBuffers=4`.

All four D3D9Ex allocations succeeded. `NvFBCToDx9VidSetUp` then returned `-5`
(`NVFBC_ERROR_DRIVER_FAILURE`) before NVENC initialization. Testing larger
counts was unnecessary because four is already the minimum expansion beyond
the known working pool.

Evidence: `docs/nvfbc-surface-count-4-20260717.log`.

## NvFBC CUDA test

The alternative path used the SDK-prescribed CUDA/D3D9 interop sequence:

1. `cuInit` succeeded.
2. `cuD3D9CtxCreate` succeeded on the same NVIDIA D3D9Ex device.
3. `NvFBC_CreateEx(NVFBC_SHARED_CUDA)` succeeded.
4. `NvFBCCudaGetMaxBufferSize` succeeded and returned 33,423,360 bytes.
5. The CUDA context was verified current immediately before Setup.

`NvFBCCudaSetup` returned `-6` (`NVFBC_ERROR_CUDA_FAILURE`) for every tested
combination:

| HDR request | Format | Result |
| --- | --- | --- |
| false | ARGB | `-6` |
| false | ARGB10 | `-6` |
| true | ARGB | `-6` |
| true | ARGB10 | `-6` |

Both `CU_CTX_SCHED_BLOCKING_SYNC` and `CU_CTX_SCHED_AUTO` produced the same
failure. Because Setup never completed, no CUDA output buffer existed that
could be registered directly with CUDA NVENC.

Evidence:

- `docs/nvfbc-cuda-setup-hdr0-argb-20260717.log`
- `docs/nvfbc-cuda-setup-hdr0-argb10-20260717.log`
- `docs/nvfbc-cuda-setup-hdr1-argb-20260717.log`
- `docs/nvfbc-cuda-setup-hdr1-argb10-20260717.log`
- `docs/nvfbc-cuda-setup-auto-context-20260717.log`

## Why three surfaces are insufficient

The NVENC SDK requires input frames to remain available until encode
completion when Lookahead is enabled. A depth greater than the available pool
therefore cannot safely reuse the three NvFBC surfaces. External Lookahead does
not remove this ownership requirement.

The only remaining implementation would copy every NvFBC frame into a larger
application-owned GPU ring. That would give up the direct-resource property and
add the extra capture/analysis loop explicitly rejected for this backend.

## Fixed-route regression

After restoring the fixed three-surface route, a 96-frame 4K240 Main10 run
completed 96/96 frames, reported zero source long intervals, and passed strict
FFmpeg decode. Its capability record contains `lookahead=false` and the NvFBC
policy marker. Evidence: `docs/nvfbc-no-lookahead-regression-20260717.log`.

The decision can be revisited only if a future NvFBC/driver version accepts a
larger direct surface pool or restores a working client-buffer CUDA interface.
