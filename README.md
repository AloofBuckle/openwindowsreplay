# RustReplay

RustReplay 是一个 Windows 桌面即时回放程序，GUI 使用 `egui/eframe`。成品后端按 **GPU-only raw frame path** 约束工作：捕获输出、色彩转换、色度写入、oneVPL 输入前的未编码视频帧都必须保持在同一 DXGI adapter 的 D3D11 纹理路径上。

当前成品范围：

- 启动即打开中文 GUI；不保留调试命令行、无窗口探测或自检入口。
- GUI：开始/保存/停止即时回放、色度采样、oneVPL RateControlMethod 引导式参数、循环缓存目录/保存目录/回放时长、双日志区。
- 启动能力探测：DXGI adapter LUID、oneVPL dispatcher/implementation、HEVC profile、输入 FourCC、RateControlMethod、当前显示器可生产路线。
- 前端规则：当前机器/路径不可用的字段直接隐藏；详细原因写入日志。
- 视频生产路线：DDA texture 或 WGC BGRA8/FP16 → GPU shader/VideoProcessor 转换到目标 FourCC → 一次 GPU `CopyResource` 写入 oneVPL 内部分配 D3D11 surface → HEVC → MP4。
- 发布规则：发布包包含 `rust_replay.exe`，并打包 `libvpl.dll` / `libvpl-2.dll` 等用户态依赖；GPU 驱动、D3D11、Media Foundation 仍是系统/驱动前提。
- 色彩策略：按当前显示器状态与 DDA/WGC 实际给到的数据做高保真；未实现或不能保证的桌面模式返回 `UnsupportedGpuPath`，原因包含 `不支持的桌面模式`，不会伪装降级。
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

输出：`dist/RustReplay.zip`。脚本会把 `rust_replay.exe` 和找到的 oneVPL 用户态 DLL 放入 `dist/RustReplay/`。

## 不可能形成桌面同步路径时的行为

软件仍可打开 GUI，但隐藏录制相关不可用字段。点击“开始即时回放”会返回 `UnsupportedGpuPath`，日志会说明阻断点，例如：没有 HEVC 硬编、没有 D3D11 texture 输入、当前桌面色彩/位深/捕获格式属于 `不支持的桌面模式`。程序不会自动降级为 SDR、AVC、软件编码或 CPU raw frame 回退。
