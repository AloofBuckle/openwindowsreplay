# NVENC 合并审计状态

本文记录 `RustReplay-NVENC` 合并到主线时对 fork 审计项的处理结果。它不是新增功能清单，而是用于区分已关闭风险和仍需保留的工程债务。

## 已关闭

1. **oneVPL/GPU/线程异常路径泄漏**
   - 增加 oneVPL loader/session/encoder、surface、capture thread、shared HANDLE 与 keyed mutex guard。
   - GPU event query 等待加入退避、device removed 检查和 2 秒超时。

2. **ring 依赖最后插入包判断最新时间**
   - 视频和音频使用独立有序队列；旧音频晚到时独立裁剪。
   - snapshot 按时间戳归并，保存 cursor 同时考虑音频完成位置和 pending video。

3. **磁盘 writer 队列无界**
   - 改为容量 3 的 `sync_channel` 和 `try_send`。
   - 队列满、writer 错误和 pending audio-gated segment 超限均升级为会话错误，不再无限积压。

4. **不同编码 epoch 被错误合并**
   - metadata 变化会清空旧 ring epoch。
   - 磁盘拼接验证宽高、色彩、codec、VPS/SPS/PPS、AAC 格式及每段 sync 起点。

5. **无关键帧仍保存**
   - 内存保存必须找到真实视频关键帧。
   - MP4 writer 拒绝首个可播放样本不是 IDR/CRA 的 track。

6. **磁盘临时文件与中途失败残留**
   - 分段 MP4、sidecar 和最终 replay 输出均使用 `.part` 后 rename。
   - 失败会 rollback；启动只清理 RustReplay 自己的旧分段产物，停止会清空当前磁盘缓存。
   - segment lease 防止保存读取期间被 prune 删除。

## 部分关闭

1. **资源释放故障覆盖**
   - RAII 已落地，WGC 连续五次开始/停止和 DDA 冒烟通过。
   - 尚无覆盖每个初始化阶段的系统化 failpoint 测试。

2. **乱序、慢磁盘与格式变化测试**
   - 已增加乱序音频、保存 cursor、关键帧、writer queue full、事务清理和分段不兼容测试。
   - 磁盘空间耗尽、权限变化、文件占用及 writer panic 尚未全部自动化。

3. **录制主函数职责与 unsafe 范围**
   - `vpl.rs` 已拆成真实 Rust 模块，capture、encode、route、timing、oneVPL loop 和 NVENC loop 已分离。
   - `record_loop.rs` 仍较大，部分 FFI 控制流仍位于宽 `unsafe` 边界内。

## 未关闭

1. 用户主动停止仍由 `BackendError` 表达，尚未引入 `RecordExit::{Stopped, Completed, Failed}`。
2. adapter/output 基本仍固定索引 0；混合 GPU、output 1+ 和旋转显示器未生产验证。
3. oneVPL bitstream pool 最坏约为 `64 MiB * async depth 16`，需要按能力和码率收紧上限。
4. GPU capture slot 固定为 32，尚未按显存预算动态配置。
5. 通配符导入、长参数列表和局部 lint 抑制仍存在，需在不改变热路径行为的前提下渐进清理。
6. WGC 500 us polling 已通过短时 4K/170 Hz 测试，但仍缺少多小时长跑与功耗审计。

## 本次合并验证

- `cargo fmt --check`
- `cargo test --locked --target x86_64-pc-windows-msvc`：69 passed，11 ignored
- `cargo build --release --locked --target x86_64-pc-windows-msvc`
- NVENC WGC：4K/170 Hz/HDR PQ/P010，连续 5 次 3 秒录制通过，每次 511 个视频 AU，无 slot/queue drop
- NVENC DDA：4K/170 Hz/HDR PQ/P010，3 秒录制和 MP4 封装通过
- `ffprobe`：HEVC Main10、yuv420p10le、BT.2020/PQ/full-range、AAC LC
- ffmpeg rawvideo 完整解码：WGC/DDA 均无 HEVC 解码错误
