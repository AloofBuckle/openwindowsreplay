# RustReplay

RustReplay 是一个 Windows 桌面即时回放程序，GUI 使用 `egui/eframe`。成品后端按 **GPU-only raw frame path** 约束工作：捕获输出、色彩转换、色度写入、硬编码器输入前的未编码视频帧都必须保持在同一 DXGI adapter 的 D3D11 纹理路径上。

> NVENC fork 说明：本副本目标是在保留现有 oneVPL 自动适配能力的前提下增加 NVIDIA NVENC 后端，并由能力探测自动区分 oneVPL/NVENC。当前已生产化 4:2:0 NV12/P010 与 SDR 8-bit 4:4:4 AYUV 路径：DDA/WGC GPU texture → GPU route converter → NVENC D3D11 registered resource → HEVC → encoded ring/MP4。oneVPL 仍优先；当 oneVPL 不能形成生产路径而 NVENC 可用时自动选择 NVENC。进度见 `docs/nvenc.md`。

当前成品范围：

- 启动即打开中文 GUI；不保留调试命令行、无窗口探测或自检入口。
- GUI：开始/保存/停止即时回放、DDA/WGC 捕获后端、色度采样、oneVPL/NVENC 通用 RateControlMethod 引导式参数、循环缓存目录/保存目录/回放时长、双日志区。
- 启动能力探测：DXGI adapter LUID、oneVPL dispatcher/implementation、NVENC API/adapter、HEVC profile、输入 FourCC/format、RateControlMethod、当前显示器可生产路线。
- 前端规则：当前机器/路径不可用的字段直接隐藏；详细原因写入日志。配置自动保存到 `%ProgramData%\OneVPL Replay\config.json`。
- NVENC 原始调参：仅在 NVENC 成为 active backend 且当前色度有生产路线时显示。Preset 主标签直接使用 P1-P7；`splitEncodeMode` 与 `multiPass` 使用人话名称并保留 SDK 常量/原始值；Spatial AQ 直接控制 `enableAQ`。`splitEncodeMode` 暴露全部 0/1/2/3/4/15，GPU engine 数只用于说明实际条带数。额外调参不暴露 Temporal AQ 或 AQ strength。
- 视频生产路线：DDA texture 或 WGC BGRA8/FP16 → GPU shader/VideoProcessor 转换到目标 FourCC/format → oneVPL D3D11 surface 或 NVENC D3D11 registered resource → HEVC → MP4。
- 发布规则：oneVPL dispatcher 及其用户态运行库内嵌于 `rust_replay.exe`，启动时释放到 `%ProgramData%\OneVPL Replay\`；NVENC 使用 NVIDIA 驱动提供的 `nvEncodeAPI64.dll`。发布目录不携带 DLL，GPU 驱动、D3D11、Media Foundation 仍是系统/驱动前提。
- 色彩策略：按当前显示器状态与 DDA/WGC 实际给到的数据动态推导 primaries/transfer/matrix/range；range 不固化为 full 或 limited。未实现或不能保证的桌面模式返回 `UnsupportedGpuPath`，原因包含 `不支持的桌面模式`，不会伪装降级。
- 位深策略：丢弃 12-bit 路线，只保留内部 8-bit / 10-bit。
- 光标策略：DDA 不录光标；WGC 录光标。
- 音频策略：WASAPI loopback/mic → 48k stereo float PCM → AAC LC，允许重采样和声道混合，但保留/重建绝对时间戳。
- 已编码码流环形缓存：按绝对时间戳裁剪，符合 VFR/音画同步设计。

## 构建

```powershell
cargo build --release --target x86_64-pc-windows-msvc
```

产物：`target/x86_64-pc-windows-msvc/release/rust_replay.exe`。

## 发布包

```powershell
powershell -ExecutionPolicy Bypass -File scripts/package.ps1
```

输出：`dist/RustReplay.zip`。脚本在构建时把找到的 oneVPL dispatcher 及相邻用户态运行库嵌入 `rust_replay.exe`，`dist/RustReplay/` 中不再放置 DLL。

## 不可能形成桌面同步路径时的行为

软件仍可打开 GUI，但隐藏录制相关不可用字段。点击“开始即时回放”会返回 `UnsupportedGpuPath`，日志会说明阻断点，例如：没有 HEVC 硬编、没有 D3D11 texture 输入、当前桌面色彩/位深/捕获格式属于 `不支持的桌面模式`。程序不会自动降级为 SDR、AVC、软件编码或 CPU raw frame 回退。
