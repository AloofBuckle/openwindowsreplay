# NVENC 兼容 fork 工作记录

本 fork 的目标是在保留现有 oneVPL 自动适配能力的前提下，增加 NVIDIA NVENC 后端，并由能力探测自动区分/选择 oneVPL 与 NVENC。

## 当前已落地

- 新增 `src/backend/nvenc.rs`：
  - 运行时动态加载 `nvEncodeAPI64.dll`，不依赖 NVIDIA import lib。
  - 使用 D3D11 device 调用 `NvEncOpenEncodeSessionEx`，保持未来生产路径必须与 DXGI adapter 同 LUID。
  - 枚举 HEVC encode GUID、HEVC profile、D3D11 input formats、关键 caps。
  - 探测 NV12/P010/NV16/P210/AYUV/YUV444/YUV444_10BIT 与 420/422/444、8/10-bit 的对应关系。
  - 按当前 DXGI output 的 ColorSpace/BitsPerColor 推导 NVENC current-display route，遵守和 oneVPL 相同的“不支持桌面模式”阻断原则。
  - 为 CBR/VBR/CQP 建立 NVENC 通用码控字段能力：VBV / CONSTQP / spatial AQ / VBR targetQuality。硬件 lookahead cap 继续记录，但当前单 bitstream 同步编码器不能处理 `NV_ENC_ERR_NEED_MORE_INPUT` 的延迟输出，因此 production feature 明确为不可用并从 GUI 隐藏。
  - 将 NVENC CBR/VBR/CQP 映射到现有 GUI 通用码控模型，并在初始化时写入 `NV_ENC_RC_PARAMS`：CBR 写入 target bitrate、VBV buffer/initial delay；VBR 额外写入 max bitrate 与 targetQuality；CQP 写入 I/P/B QP。oneVPL 专用 `BRCParamMultiplier`、AVBR/LA/ICQ/VCM/LA_ICQ/LA_HRD/QVBR 等不会在 NVENC active backend 下暴露。
  - 新增独立的“NVENC 原始调参”区域，只暴露用户指定的四类额外调参：
    - Preset GUID：驱动通过 `NvEncGetEncodePresetGUIDs` 为 HEVC session 枚举到的 P1-P7。
    - `splitEncodeMode`：SDK 全部原始值 `0/1/2/3/4/15`。该字段不是驱动枚举项；`NV_ENC_CAPS_NUM_ENCODER_ENGINES` 只用于向用户说明实际条带数，请求 3/4 条带但硬件 engine 更少时会退化到实际 engine 数。
    - `multiPass`：SDK 原始值 `0/1/2`，分别对应 disabled、quarter-resolution two-pass、full-resolution two-pass。
    - Spatial AQ：直接控制 `NV_ENC_RC_PARAMS::enableAQ`，AQ strength 保持 0，由驱动自动选择强度。
  - 默认值保持本 fork 原有编码行为：P4、split Auto、single pass、Spatial AQ 关闭。旧配置里的 Temporal AQ/AQ strength 会被清零，不作为允许用户修改的额外调参。
  - Preset 主标签直接显示 P1-P7；split/multipass 使用“人话名称 | SDK 常量 = 原始值”的格式，并在悬停说明中解释实际条带退化和二遍首遍分辨率。
  - `tuningInfo` 是另一组独立参数；SDK 13.1 还定义 HQ、Low Latency、Ultra Low Latency、Lossless 与 Ultra High Quality。本项目即时回放路径固定使用 `NV_ENC_TUNING_INFO_LOW_LATENCY (2)`，不把它加入用户指定的额外调参集合。
  - Encoder 初始化先以所选 preset 和 low-latency tuning 调用 `NvEncGetEncodePresetConfigEx` 获取驱动基线，再只覆盖本项目拥有的 profile/chroma/bit-depth、低延迟、VUI 和码控字段，避免把 preset 其余参数全部抹成零。
  - 增加可复用 `NvencD3d11Encoder`，通过 `NvEncInitializeEncoder` / `NvEncCreateBitstreamBuffer` / `NvEncRegisterResource` / `NvEncMapInputResource` / `NvEncEncodePicture` / `NvEncLockBitstream` 从 D3D11 NV12/P010/AYUV texture 产出 HEVC Annex-B access unit。
- `ProbeCaps` 新增 `nvenc` 与 `video_encoder_selection`：
  - oneVPL 与 NVENC 都进入能力探测。
  - oneVPL 仍保持优先；NVENC 在 NV12/P010 或 SDR AYUV current-display route 可用时成为 production-ready fallback。`pipeline` 的普通 MP4、结构化输出与 encoded-sink 入口都会按 `video_encoder_selection.active` 自动分派 oneVPL/NVENC。
- GUI 编码器日志新增：
  - 自动编码器选择结果。
  - NVENC DLL/API 版本、adapter、HEVC/profile/input format/caps/route 候选。

## 本机验证结果

本机环境：NVIDIA GeForce RTX 5090，驱动 `610.62`，`C:\Windows\System32\nvEncodeAPI64.dll`。

手动 smoke test：

```powershell
cargo test --locked --target x86_64-pc-windows-msvc backend::nvenc::tests::local_nvenc_probe_smoke -- --ignored --nocapture
```

结果摘要：

- NVENC API compiled/max supported：13.1 / 13.1
- HEVC：可用
- HEVC profile：Main / Main10 / FRExt / Unknown
- input formats：NV12 / P010 / NV16 / P210 / AYUV / YUV444 / YUV444_10BIT / ARGB/ABGR 系列等
- caps：max 8192x8192，async=true，10bit=true，422=true，444=true，lookahead=true，temporal_aq=true，encoder_engines=3
- HEVC presets：P1 / P2 / P3 / P4 / P5 / P6 / P7
- 共同码控映射：CBR / VBR / CQP
- 当前显示器为 HDR PQ BT.2020 full，NVENC route 推导为：
  - 420 -> P010 / Main10 / 10-bit
  - 422 -> P210 / FRExt / 10-bit
  - 444 -> YUV444_10BIT / FRExt / 10-bit

NVENC D3D11 registered-resource 编码冒烟：

```powershell
cargo test --locked --target x86_64-pc-windows-msvc backend::nvenc::tests::local_nvenc_d3d11_encode_smoke -- --ignored --nocapture
```

结果摘要：

- adapter_index=0
- 输入：D3D11 `DXGI_FORMAT_NV12` texture，1280x720
- 输出：HEVC bitstream 非空
- Annex-B start code：true

常规测试：

```powershell
cargo test --locked --target x86_64-pc-windows-msvc
```

通过：62 passed，11 ignored（ignored 项为本机自动选择、NVENC 探测、registered-resource 编码、三种码控、非默认调参与真实录制 smoke）。

## NVENC 码控暴露与测试范围

- CBR：`averageBitRate`、`vbvBufferSize`、`vbvInitialDelay`，以及公共 Spatial AQ / multiPass；RTX 5090 P010 首帧编码通过。
- VBR：`averageBitRate`、`maxBitRate`、`vbvBufferSize`、`vbvInitialDelay`、整数 `targetQuality`，以及公共 Spatial AQ / multiPass；RTX 5090 P010 首帧编码通过。
- CQP：`constQP` 的 I/P/B 三个 QP，以及公共 Spatial AQ / multiPass；RTX 5090 P010 首帧编码通过。
- `NV_ENC_CAPS_SUPPORTED_RATECONTROL_MODES` 查询失败时不暴露任何模式；当前 5090 返回 CBR/VBR，CQP 按 SDK 的 0 值规则一并可用。
- Lookahead：硬件 cap=true，但当前同步单输出 buffer 实测返回 `NV_ENC_ERR_NEED_MORE_INPUT`；已从 production feature 隐藏，并在 encoder 入口提前拒绝非零深度。
- 未声称完整暴露整个 `NV_ENC_RC_PARAMS`：min/max/initial QP hint、Temporal AQ、AQ strength、strict GOP、non-reference P、external lookahead、QP map、fractional targetQuality、lookahead level、alpha/MV-HEVC 字段均不属于当前允许用户修改的范围。未由本项目拥有的 preset 字段保持驱动返回值，不再无条件清零 `lowDelayKeyFrameScale`。

## 已生产化的 NVENC 链路

- `src/backend/vpl.rs` 复用现有 DDA/WGC capture、GPU route converter、WASAPI/AAC、encoded ring 与 MP4 mux，只把编码段替换成 NVENC registered-resource。
- 当前 production-ready 范围：
  - SDR/8-bit current-display route -> NV12 / HEVC Main
  - HDR PQ 或 10-bit current-display route -> P010 / HEVC Main10
  - SDR/8-bit 4:4:4 current-display route -> AYUV / HEVC FRExt
- NV16/P210 与 planar YUV444/YUV444_10BIT 仍保留 probe-only。NVIDIA 官方 D3D11 sample 没有这些 planar 格式的原生 DXGI texture 映射；不能用 NV12/P010/Y210/Y410 静默替代不同平面布局。
- `ProbeCaps` 现在按 active backend 聚合 `supported_chroma`、`supported_rate_controls` 与字段可见性；oneVPL 可用时优先 oneVPL，oneVPL 不可生产时 NVENC 可自动接管。
- `pipeline` 的三个 Windows 生产入口都根据 `video_encoder_selection.active` 分派 oneVPL 或 NVENC，GUI 会话使用 encoded-sink 入口。

## 本机新增验证结果

NVENC P010 registered-resource 编码冒烟：

```powershell
cargo test --locked --target x86_64-pc-windows-msvc backend::nvenc::tests::local_nvenc_d3d11_p010_encode_smoke -- --ignored --nocapture
```

结果摘要：P010 1280x720 -> HEVC Annex-B，输出非空并含 start code。

NVENC 非默认原始调参 4K 冒烟：

```powershell
cargo test --locked --target x86_64-pc-windows-msvc backend::nvenc::tests::local_nvenc_non_default_tuning_smoke -- --ignored --nocapture
```

结果摘要：RTX 5090 在 3840x2160 P010 路线上逐一成功初始化并编码全部 split 原始值 `0/1/2/3/4/15`，同时写入 P7、`NV_ENC_TWO_PASS_FULL_RESOLUTION (2)` 与 Spatial AQ=true。该 GPU 报告 3 个 engine，但原始值 `4` 仍是合法请求，实际条带数退化到 3。

NVENC AYUV registered-resource 编码冒烟：

```powershell
cargo test --locked --target x86_64-pc-windows-msvc backend::nvenc::tests::local_nvenc_d3d11_ayuv_encode_smoke -- --ignored --nocapture
```

结果摘要：`DXGI_FORMAT_AYUV` 1280x720 -> HEVC FRExt Annex-B，输出非空并含 start code。本机当前为 HDR 桌面，因此真实桌面长链路验证使用 P010；AYUV 的 GPU writer 复用 oneVPL 已生产化的 packed shader 路径。

自动后端选择冒烟：

- oneVPL dispatcher 可加载，但本机未报告 oneVPL HEVC 硬编，`production_ready=false`。
- NVENC HEVC/D3D11/current-display route 完整，`production_ready=true`。
- `probe_all()` 最终选择 `NVENC`；纯单元测试同时验证两者都 ready 时优先 oneVPL。

NVENC 真实生产录制冒烟（当前 HDR PQ BT.2020 full 桌面，WGC -> P010 -> NVENC -> MP4 mux）：

```powershell
cargo test --locked --target x86_64-pc-windows-msvc backend::vpl::tests::local_nvenc_d3d11_record_smoke -- --ignored --nocapture
```

结果摘要：

- route：P010 / HEVC Main10 / BT.2020 PQ full
- captured_frames=820
- encoded_samples=820
- encoded_bytes=2187479
- audio_access_units=237
- 输出 HEVC/AAC track 可成功 mux 成 MP4

## 明确边界 / 后续可选增强

1. NVENC `NV_ENC_CONFIG` 已补齐 production 必需的 HEVC profile/chroma/bit-depth、IP-only/low-latency 初始化，并写入 CBR/VBR/CQP、VBV、Spatial AQ 与 VBR targetQuality；Preset、split encode 与 multipass 已按原始 SDK 值在 GUI 暴露。Lookahead 在延迟输出队列完成前隐藏；Temporal AQ/AQ strength 不属于允许用户修改的额外调参。
2. 4:2:2 NV16/P210 与 planar 4:4:4 缺少本项目可证明的原生 D3D11 texture layout，保持 probe-only；未来若接入 CUDA interop 或 NVIDIA 给出可验证 D3D11 layout，再单独生产化。
3. NVENC caps 报告 async=true，但当前编码器采用同步 registered-resource 提交；捕获 snapshot pool 与编码线程已解耦，本机 240 Hz 实录 `encode_submit` 平均约 2.2 ms。后续如引入 completion event + 多 bitstream buffer，需要保持现有 VFR 时间戳与停止语义。
4. WGC 在静态桌面短测中可能因为 warmup 与“仅变化产帧”导致短时间无正式帧；生产长跑不改默认 warmup，手动 ignored smoke 使用环境变量把 warmup 缩短到 0，并在测试期间轻微移动鼠标来触发 WGC 帧，以验证链路。
5. DDA 在当前 HDR/FP16 桌面上可能因 `DuplicateOutput1` FP16 不受支持而返回 `UnsupportedGpuPath`；这保持不伪装 SDR、不 CPU fallback 的策略。WGC 是当前 HDR 桌面的已验证路径。

## 约束不变

- 不允许 raw frame CPU Map/Readback/Staging 回退。
- DDA/WGC capture、GPU color/chroma conversion、NVENC input resource 必须处在同一 DXGI adapter 上。
- 色彩 range 必须由当前显示器和捕获路线动态推导并同时写入 HEVC VUI/MP4 nclx，不得固化为 full 或 limited。
- 若当前显示器状态、色彩/位深、D3D11 texture 输入或 NVENC HEVC route 无法保证，必须返回 `UnsupportedGpuPath`，不能伪装降级。
