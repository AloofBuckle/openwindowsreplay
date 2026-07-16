# NVENC 兼容后端合并记录

该后端已合并到主线。在保留 oneVPL 自动适配能力的前提下，程序会探测 NVIDIA NVENC，并由统一能力探测结果自动选择 oneVPL 或 NVENC。

## 当前已落地

- 新增 `src/backend/nvenc.rs`：
  - 运行时动态加载 `nvEncodeAPI64.dll`，不依赖 NVIDIA import lib。
  - 使用 D3D11 device 调用 `NvEncOpenEncodeSessionEx`，保持未来生产路径必须与 DXGI adapter 同 LUID。
  - 枚举 HEVC encode GUID、HEVC profile、D3D11 input formats、关键 caps。
  - 探测 NV12/P010/NV16/P210/AYUV/YUV444/YUV444_10BIT 与 420/422/444、8/10-bit 的对应关系。
  - 按当前 DXGI output 的 ColorSpace/BitsPerColor 推导 NVENC current-display route，遵守和 oneVPL 相同的“不支持桌面模式”阻断原则。
  - 为 CBR/VBR/CQP 建立 NVENC 通用码控字段能力：VBV / CONSTQP / spatial AQ / VBR targetQuality。CBR/VBR 在硬件 lookahead cap 可用时按当前 route 的 surface 预算暴露 `LookAheadDepth`（硬上限 31）；编码器使用多 bitstream buffer、按提交顺序保留输入 surface，并处理 `NV_ENC_ERR_NEED_MORE_INPUT`、EOS drain 与源 VFR 时间戳对应。
  - 将 NVENC CBR/VBR/CQP 映射到现有 GUI 通用码控模型，并在初始化时写入 `NV_ENC_RC_PARAMS`：CBR 写入 target bitrate、VBV buffer/initial delay；VBR 额外写入 max bitrate 与 targetQuality；CQP 写入 I/P/B QP。oneVPL 专用 `BRCParamMultiplier`、AVBR/LA/ICQ/VCM/LA_ICQ/LA_HRD/QVBR 等不会在 NVENC active backend 下暴露。
  - 新增独立的“NVENC 原始调参”区域，只暴露用户指定的四类额外调参：
    - Preset GUID：驱动通过 `NvEncGetEncodePresetGUIDs` 为 HEVC session 枚举到的 P1-P7。
    - `splitEncodeMode`：SDK 全部原始值 `0/1/2/3/4/15`。该字段不是驱动枚举项；`NV_ENC_CAPS_NUM_ENCODER_ENGINES` 只用于向用户说明实际条带数，请求 3/4 条带但硬件 engine 更少时会退化到实际 engine 数。
    - `multiPass`：SDK 原始值 `0/1/2`，分别对应 disabled、quarter-resolution two-pass、full-resolution two-pass。
    - Spatial AQ：直接控制 `NV_ENC_RC_PARAMS::enableAQ`，AQ strength 保持 0，由驱动自动选择强度。
  - 默认值保持 NVENC 后端原有编码行为：P4、split Auto、single pass、Spatial AQ 关闭。旧配置里的 Temporal AQ/AQ strength 会被清零，不作为允许用户修改的额外调参。
  - Preset 主标签直接显示 P1-P7；split/multipass 使用“人话名称 | SDK 常量 = 原始值”的格式，并在悬停说明中解释实际条带退化和二遍首遍分辨率。
  - `tuningInfo` 是另一组独立参数；SDK 13.1 还定义 HQ、Low Latency、Ultra Low Latency、Lossless 与 Ultra High Quality。本项目即时回放路径固定使用 `NV_ENC_TUNING_INFO_LOW_LATENCY (2)`，不把它加入用户指定的额外调参集合。
  - Encoder 初始化先以所选 preset 和 low-latency tuning 调用 `NvEncGetEncodePresetConfigEx` 获取驱动基线，再只覆盖本项目拥有的 profile/chroma/bit-depth、低延迟、VUI 和码控字段，避免把 preset 其余参数全部抹成零。
  - 增加统一 `NvencTextureEncoder`：NV12/P010/AYUV 使用原生 D3D11 registered resource；NV16/P210/YUV444/YUV444_10BIT 使用 D3D11 shared NT handle -> CUDA external memory -> CUDA array -> NVENC 的持久注册路径。两条路线都支持延迟输出和精确源 VFR 时间戳。
- `ProbeCaps` 新增 `nvenc` 与 `video_encoder_selection`：
  - oneVPL 与 NVENC 都进入能力探测。
  - oneVPL 仍保持优先；NVENC 在 NV12/P010、NV16/P210、AYUV 或 planar YUV444/YUV444_10BIT current-display route 可用时成为 production-ready fallback。`pipeline` 的普通 MP4、结构化输出与 encoded-sink 入口都会按 `video_encoder_selection.active` 自动分派 oneVPL/NVENC。
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

通过：106 passed，17 ignored，0 failed（ignored 项包含本机自动选择、NVENC 探测、registered-resource/CUDA external-memory 编码、Lookahead、shared-fence 输入格式、三种码控、非默认调参与真实录制 smoke）。

## NVENC 码控暴露与测试范围

- CBR：`averageBitRate`、`vbvBufferSize`、`vbvInitialDelay`，以及公共 Spatial AQ / multiPass；RTX 5090 P010 首帧编码通过。
- VBR：`averageBitRate`、`maxBitRate`、`vbvBufferSize`、`vbvInitialDelay`、整数 `targetQuality`，以及公共 Spatial AQ / multiPass；RTX 5090 P010 首帧编码通过。
- CQP：`constQP` 的 I/P/B 三个 QP，以及公共 Spatial AQ / multiPass；RTX 5090 P010 首帧编码通过。
- `NV_ENC_CAPS_SUPPORTED_RATECONTROL_MODES` 查询失败时不暴露任何模式；当前 5090 返回 CBR/VBR，CQP 按 SDK 的 0 值规则一并可用。
- Lookahead：CBR/VBR 且硬件 cap=true 时在 GUI 暴露 `0..route_max`，`route_max` 同时受 31 的 SDK 上限与 3 GiB capture-pool 预算约束。编码器为深度 `N` 分配至少 `N+1` 个 bitstream buffer，按 pending 队列保留输入 surface，接受 `NV_ENC_ERR_NEED_MORE_INPUT`，EOS 后完整 drain，并校验驱动返回时间戳与提交的源 VFR 时间戳一致。
- 未声称完整暴露整个 `NV_ENC_RC_PARAMS`：min/max/initial QP hint、Temporal AQ、AQ strength、strict GOP、non-reference P、external lookahead、QP map、fractional targetQuality、lookahead level、alpha/MV-HEVC 字段均不属于当前允许用户修改的范围。未由本项目拥有的 preset 字段保持驱动返回值，不再无条件清零 `lowDelayKeyFrameScale`。

## 已生产化的 NVENC 链路

- `src/backend/vpl.rs` 复用现有 DDA/WGC capture、GPU route converter、WASAPI/AAC、encoded ring 与 MP4 mux，只把编码段替换成 NVENC registered-resource。
- WGC 使用持久 MTA 服务线程，并在 NVENC D3D11 device 上直接把 shader 输出写入池化 registered input texture；等待 GPU event query 后直接送入 NVENC，不再经过额外 `CopyResource`。P010 固定使用 plane RTV 写入；compute/UAV 双平面直写在 NVIDIA persistent registered input 上实测会产生跨帧色度损坏，因此不进入生产路线。
- DDA 保持独立 capture device。捕获 shader 直接写 `D3D11_RESOURCE_MISC_SHARED` ordinary route texture，NVENC device 通过 legacy shared handle 打开同一 allocation；capture context `Signal`、encoder context `Wait` 同一个 D3D11 shared fence 后，执行一次 `CopyResource` 到池化 encoder-local texture，再将本地纹理作为 persistent registered input。copy pool 固定为 `LookaheadDepth + 1`，对应输入只在 NVENC 返回该帧 AU 后归还。
- NVENC D3D11 device/context 按 adapter LUID 缓存，WGC WinRT D3D device 在线程内缓存；重复开始 WGC 录制时不再反复重建整套设备对象。
- current-display route 自身记录 adapter LUID；录制初始化同时验证 route LUID、当前 DXGI adapter LUID 与 NVENC encoder D3D11 device LUID，防止多 NVIDIA GPU 或枚举顺序变化造成跨设备误配。
- oneVPL implementation 同样通过 dispatcher 的 `mfxExtendedDeviceId.DeviceLUID` 与 DXGI adapter 精确绑定；同厂商多 GPU 不再只按 vendor ID 猜测。
- 所有 adapter/output 都参与 route 探测，优先包含桌面原点的主显示器；rotation、desktop rect、DXGI color space 和 bits-per-color 会被验证并周期复核。
- 当前 production-ready 范围：
  - SDR/8-bit current-display route -> NV12 / HEVC Main
  - HDR PQ 或 10-bit current-display route -> P010 / HEVC Main10
  - SDR/8-bit 4:2:2 current-display route -> NV16 / HEVC FRExt
  - HDR PQ 或 10-bit 4:2:2 current-display route -> P210 / HEVC FRExt
  - SDR/8-bit 4:4:4 current-display route -> AYUV / HEVC FRExt
  - 驱动不暴露 AYUV 时的 SDR 4:4:4 route -> planar YUV444 / HEVC FRExt
  - HDR PQ 或 10-bit 4:4:4 current-display route -> planar YUV444_10BIT / HEVC FRExt
- NV16/P210 与 planar YUV444/YUV444_10BIT 不伪装成其他 DXGI 视频格式。捕获 shader 直接写 R8/R16 连续平面 allocation，D3D11 与 CUDA 通过 keyed mutex 交接；共享 NT handle、CUDA external memory、NVENC CUDA array 均只创建/注册一次，不执行 GPU 内容复制。
- 能力探测会按 adapter LUID 创建 64x64 shared keyed texture，完成一次 D3D11 -> CUDA -> D3D11 key round trip；失败的 adapter 仍保留 query 信息，但 planar route 不进入生产选择，也不显示在 GUI。
- `ProbeCaps` 现在按 active backend 聚合 `supported_chroma`、`supported_rate_controls` 与字段可见性；oneVPL 可用时优先 oneVPL，oneVPL 不可生产时 NVENC 可自动接管。
- `pipeline` 的三个 Windows 生产入口都根据 `video_encoder_selection.active` 分派 oneVPL 或 NVENC，GUI 会话使用 encoded-sink 入口。

## 本机新增验证结果

NVENC CUDA external-memory 与 Lookahead：

- NV16、P210、YUV444、YUV444_10BIT 均在 Lookahead depth 4 下完成持久注册、延迟输出、EOS drain 和时间戳顺序验证。
- P210 synthetic 1920x1080 持久输入 120 帧约 `2.19 ms/frame`。
- 在 4 个 P210 输入仍 pending 时直接销毁 encoder，随后 D3D11 可重新取得所有 keyed mutex key 0，快速取消不会永久占有 capture slot。
- CUDA external semaphore FFI 按 CUDA 13.1 头文件验证为 144 字节，`flags` 偏移 72，keyed-mutex reserved 偏移 32。

NVENC 4K HDR PQ 真实录制（2026-07-16，3840x2160、240 Hz）：

- WGC P010 plane RTV 零复制路线连续 10 秒产出 2401 帧，源间隔 `4.156-4.178 ms`，`dropped_no_slot=0`；Lookahead 4 的 5 秒回归产出 1201 帧并完整 drain。两份 MP4 均通过 ffmpeg 全流解码，暗区纵向色度异常抽样接近 0。
- 曾尝试把 `Direct3D11CaptureFrame` 保留到 GPU event query 完成后再 `Close`；该路线反而产生大面积蓝/洋红平面和固定竖带，已撤销，不作为 WGC 同步方案。
- DDA/WGC 的 P210 与 YUV444_10BIT 均成功输出 HEVC FRExt + AAC LC MP4。
- `ffprobe` 分别识别为 `yuv422p10le` / `yuv444p10le`、BT.2020 non-constant / SMPTE ST 2084 / full-range。
- DDA/WGC x P210/YUV444_10BIT x Lookahead 0/8 共 8 个输出均保持包级 PTS/DTS 严格递增，并通过 ffmpeg 全流解码。
- MP4 `hvcC` 从实际 SPS 的 profile/tier/compatibility/constraints/level/chroma/bit-depth 派生；MediaInfo 分别识别为 Main 4:2:2 10 与 Main 4:4:4 10，不再写死 Main/Main10 compatibility 或 level 5.1。
- WGC 停止时不再等待 Lookahead 持有的输入槽位全部提前归还；3 秒 Lookahead 8 回归总耗时由约 6.6 秒降到约 3.6-3.7 秒，EOS drain 约 34-39 ms。
- 4K240 下 P210 无 Lookahead 的 `encode_submit` 平均约 4.15-4.81 ms，Lookahead 8 约 7.6-7.8 ms；高刷新下会出现 capture slot drop，格式可用不等于所有刷新率都能无丢帧运行。

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

NVENC 真实生产录制冒烟（当前 3840x2160、170 Hz、HDR PQ BT.2020 full 桌面）：

```powershell
$env:RUST_REPLAY_NVENC_SMOKE_REPEATS='5'
$env:RUST_REPLAY_NVENC_SMOKE_SECONDS='3'
cargo test --release --locked --target x86_64-pc-windows-msvc backend::vpl::tests::local_nvenc_wgc_d3d11_record_smoke -- --ignored --nocapture --test-threads=1

$env:RUST_REPLAY_NVENC_SMOKE_REPEATS='1'
cargo test --release --locked --target x86_64-pc-windows-msvc backend::vpl::tests::local_nvenc_dda_d3d11_record_smoke -- --ignored --nocapture --test-threads=1
```

结果摘要：

- 两条 route 均为 P010 / HEVC Main10 / BT.2020 PQ full，并带 AAC LC。
- WGC 连续 5 次开始/结束均通过；每次 3 秒得到 511 个视频 AU，`dropped_no_slot=0`、`dropped_queue_full=0`。首次 NVENC device/session 初始化约 35 ms，后续约 5-6 ms。
- DDA 3 秒冒烟通过，成功产生 4K HDR HEVC/AAC MP4；独立 capture device、输入桌面绑定和 `DuplicateOutput1` 重试路径均实际运行。
- `ffprobe` 识别为 HEVC Main10、`yuv420p10le`、BT.2020/PQ/full-range；WGC 与 DDA 文件均可由 ffmpeg 完整解码为 rawvideo，未发现 HEVC 解码错误。
- DDA 在仅靠测试光标触发画面变化时，驱动可能把积累更新以极短源时间戳间隔突发交付。这是 DDA `LastPresentTime` 的源端节奏，不由 NVENC 合成 CFR；真实动态桌面仍需按目标设备长跑审计。

## 合并后的可靠性修复

- keyed mutex 改为检查原始 HRESULT，并用 guard 保证所有错误路径都释放；`WAIT_TIMEOUT` 和 `WAIT_ABANDONED` 均视为失败。
- oneVPL loader/session/encoder/surface、capture thread 和共享 HANDLE 增加 RAII 清理；GPU event query 等待会检查 device removed，并在 2 秒后超时。
- encoded ring 分离音视频有序队列，支持乱序音频裁剪、按时间戳合并快照、codec epoch 清空和真实关键帧起点校验。
- MP4 mux 不再强制把首样本标为 sync；非 IDR/CRA 起始的视频会被拒绝。
- 磁盘 writer 改为容量 3 的有界队列；队列满或 writer 失败会终止会话。分段、sidecar 和最终 replay 输出均通过 `.part` 事务写入。
- 磁盘开放分段和已完成分段共用源时间保存游标；跨分段重复保存从下一处真实关键帧继续。
- 磁盘保存游标进一步扩展为 `run_index + source PTS`；显示环境重探测导致新录制轮次 PTS 归零时，旧游标不会屏蔽新分段。
- memory ring 保存游标绑定 codec epoch；分辨率、色彩、AAC 格式或 VPS/SPS/PPS 变化后不会沿用旧 epoch 的 PTS 游标。
- VPS/SPS/PPS 按类别累计并只在 HEVC IRAP 边界切换；每个磁盘分段固定持有创建时的参数集 header。
- NVENC 正常路径显式检查 unlock、unmap、unregister、bitstream destroy 和 encoder destroy；异常路径继续由幂等 RAII guard 兜底。
- `NvEncodeAPIGetMaxSupportedVersion` 使用独立的 driver-version 编码比较；低于编译 API 13.1 的驱动会明确拒绝，不与 FFI struct-version 编码混淆。

## 明确边界 / 后续可选增强

1. NVENC `NV_ENC_CONFIG` 已补齐 production 必需的 HEVC profile/chroma/bit-depth、IP-only/low-latency 初始化，并写入 CBR/VBR/CQP、VBV、Spatial AQ、VBR targetQuality 与 Lookahead；Preset、split encode 与 multipass 已按原始 SDK 值在 GUI 暴露。Temporal AQ/AQ strength 不属于允许用户修改的额外调参。
2. 4:2:2 NV16/P210 与 planar 4:4:4 已通过 CUDA external-memory 生产化，但要求 NVIDIA 驱动同时支持对应 NVENC input format/profile/caps、CUDA D3D11 external memory 和 keyed-mutex external semaphore；任一环节失败都会明确返回不支持，不回退 CPU 或伪造布局。
3. Lookahead 已实现多 bitstream 延迟输出，但会增加输入 surface 占用和编码压力；高分辨率路线会按 3 GiB capture-pool 预算限制最大可用深度，例如 8K P010/YUV444_10BIT 当前最大为 15。性能不足时应降低 Lookahead/色度或刷新率，后端不会合成 CFR 掩盖源端丢帧。
4. WGC 使用 500 us polling 获取 `TryGetNextFrame`，同时保留 WGC `SystemRelativeTime` 的 VFR 时间戳。短测已稳定，仍应在目标硬件上做长时间 4K/高刷新性能验证。
5. NVENC DDA 要求 capture/encoder device 支持 `ID3D11Device5`、`ID3D11DeviceContext4`、ordinary shared video texture、shared fence 和一次 encoder-local GPU copy。shared texture 直接注册在 4K/240 Hz + split Auto 下会产生跨 split 的未来帧混合，因此不再作为生产路线；缺少所需接口或资源创建失败时明确返回不支持，不退回 CPU 路径。
6. 旋转输出当前明确不支持；多 GPU/非主输出已进入探测和 LUID 绑定，但仍需在更多真实拓扑上做生产验证。
7. 4K capture slot 保持 32；8K 和跨设备分配已按显存预算收紧，后续仍可按刷新率和实测 backlog 自适应。
8. 用户取消已有独立错误类型；完整 `RecordExit::{Stopped, Completed, Failed}` 状态枚举仍可作为后续 API 整理项。

## 约束不变

- 不允许 raw frame CPU Map/Readback/Staging 回退。
- DDA/WGC capture、GPU color/chroma conversion、NVENC input resource 必须处在同一 DXGI adapter 上。
- 色彩 range 必须由当前显示器和捕获路线动态推导并同时写入 HEVC VUI/MP4 nclx，不得固化为 full 或 limited。
- 若当前显示器状态、色彩/位深、D3D11 texture 输入或 NVENC HEVC route 无法保证，必须返回 `UnsupportedGpuPath`，不能伪装降级。
