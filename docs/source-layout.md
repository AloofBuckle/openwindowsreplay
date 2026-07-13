# 源码布局

本项目的高频录制路径按职责使用真实 Rust 子模块组织。实现代码不再通过
`include!` 拼接到大型父文件；父文件只保留依赖、模块声明和稳定的公开接口重导出。

## GUI

- `src/app.rs`：GUI 门面，只导出 `RustReplayApp`。
- `src/app/model.rs`：应用状态、生命周期和后台事件处理。
- `src/app/ui.rs`：egui 布局与交互。
- `src/app/rate_control.rs`：码率控制配置控件与能力约束。
- `src/app/indicator.rs`：状态指示器图像生成和持久化。
- `src/app/platform.rs`：Windows 窗口、字体、文件选择和热键映射。
- `src/app/log.rs`：日志显示、选择和滚动行为。

## 即时回放会话

- `src/backend/session.rs`：会话门面和共享常量。
- `src/backend/session/controller.rs`：GUI 可见状态机与保存入口。
- `src/backend/session/worker.rs`：后台录制线程和内存环形缓存 sink。
- `src/backend/session/disk_store.rs`：磁盘循环目录、保留策略和异步写入器。
- `src/backend/session/disk_segment.rs`：磁盘分段构建及保存快照拼接。
- `src/backend/session/sidecar.rs`：磁盘索引 sidecar 的二进制读写。

## MP4 封装

- `src/backend/mp4_mux.rs`：轨道模型和封装入口门面。
- `src/backend/mp4_mux/types.rs`：公开轨道类型、索引和已准备 sample 模型。
- `src/backend/mp4_mux/writer.rs`：轨道校验、sample 准备和整体写入流程。
- `src/backend/mp4_mux/payload.rs`：`mdat` 头和 sample payload 流式写入。
- `src/backend/mp4_mux/boxes.rs`：ISO BMFF box 构造。

## oneVPL / D3D11 后端

- `src/backend/nvenc.rs`：NVENC 动态 FFI、能力探测、D3D11 registered-resource
  编码器与 NVENC 参数映射。
- `src/backend/vpl.rs`：oneVPL/NVENC 共用捕获生产后端门面和稳定公开 API。
- `src/backend/vpl/route.rs`：显示色彩、色度、FourCC、profile 路线选择。
- `src/backend/vpl/ffi.rs`：oneVPL 动态 API、FFI 结构和扩展 buffer。
- `src/backend/vpl/probe/`：实现发现、编码能力查询和参数映射。
- `src/backend/vpl/record.rs`：公开录制入口、音频增量 mixer 和 sink 接口。
- `src/backend/vpl/record_nvenc.rs`：复用公共捕获/转换链路的 NVENC 录制循环。
- `src/backend/vpl/record_loop.rs`：连续录制热路径。它保持为单个函数所在模块，避免把
  一条严格有序的资源生命周期机械拆散。
- `src/backend/vpl/encode.rs`：异步提交、同步、bitstream 回收和 flush。
- `src/backend/vpl/capture_dda.rs`：DDA 帧到达循环。
- `src/backend/vpl/capture_wgc.rs`：WGC 帧到达循环。
- `src/backend/vpl/capture/`：D3D11 资源创建、槽位/统计和同步工具。
- `src/backend/vpl/convert/`：快照转换、各输出格式转换器和 shader 支撑。
- `src/backend/vpl/timing.rs`：DDA/WGC 源时间戳和音视频时间基换算。
- `src/backend/vpl/shaders/`：独立 HLSL 源文件，通过 `include_str!` 作为资源嵌入。

## 可见性约定

- 父门面只重导出其他模块实际依赖的公开 API。
- 同一职责目录内共享的实现使用受限可见性，不扩大为项目级公开接口。
- 新功能优先落入拥有该资源生命周期或数据模型的模块；不要为了调用方便把实现重新
  移回门面文件。
- HLSL、二进制清单等构建资源可以使用 `include_str!` 或构建期生成清单；Rust 实现代码
  不使用 `include!` 作为拆文件手段。
