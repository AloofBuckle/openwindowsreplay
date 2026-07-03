# RustReplay

RustReplay 是一个 Windows 桌面即时回放程序原型，GUI 使用 `egui/eframe`，后端按文档约束设计为 **GPU-only raw frame path**：捕获输出、色彩转换、色度写入、oneVPL 输入导入都必须保持在同一 DXGI adapter 的 D3D11 纹理路径上。

当前实现重点：

- 中文 GUI：开始/保存/停止即时回放、色度采样、oneVPL RateControlMethod 引导式参数、循环缓存目录/保存目录/回放时长、双日志区。
- 启动能力探测：DXGI adapter LUID、oneVPL dispatcher/implementation、HEVC profile、输入 FourCC、RateControlMethod（用 `MFXVideoENCODE_Query` 验证）。
- 前端规则：当前机器/路径不可用的字段直接隐藏；详细原因写入日志。
- 发布规则：允许发布包打包用户态依赖，例如 `libvpl.dll`；GPU 驱动、D3D11、Media Foundation 仍是系统/驱动前提。
- 色彩策略：按当前显示器状态与 DDA/WGC 实际给到的数据做高保真；不能可靠确定的字段不展示并写日志。
- 位深策略：丢弃 12-bit 路径，只保留内部 8-bit / 10-bit。
- 光标策略：DDA 不录光标；WGC 录光标。
- 音频策略：允许重采样和声道混合，但必须保留/重建绝对时间戳。
- 已编码码流环形缓存结构：按绝对时间戳裁剪，符合 VFR/音画同步设计方向。

## 构建

```powershell
cargo build --release
```

产物：`target/release/rust_replay.exe`。

## 发布包

```powershell
powershell -ExecutionPolicy Bypass -File scripts/package.ps1
```

输出：`dist/RustReplay.zip`。脚本会尽量把本机可找到的 `libvpl.dll` 或 `libvpl-2.dll` 放入发布包。

## 远程/无窗口探测

```powershell
rust_replay.exe --probe-json
```

该命令只输出 JSON，不打开 GUI，便于通过调试机 `/run` 接口执行。

## 不可能形成桌面同步路径时的行为

软件仍可打开 GUI，但隐藏录制相关不可用字段。点击“开始即时回放”会返回 `UnsupportedGpuPath`，日志会说明阻断点，例如：没有 HEVC 硬编、没有 D3D11 texture 输入、当前捕获/转换/import 端到端路径未成立等。程序不会自动降级为 SDR、AVC、软件编码或 CPU raw frame 回退。
