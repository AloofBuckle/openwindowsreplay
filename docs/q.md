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

7. **正常停止与并发错误混淆**
   - 用户取消使用独立取消错误；worker 只把该错误视作正常停止，不再因为 stop flag 已置位而吞掉同时发生的真实后端错误。

8. **adapter/output 与跨 GPU 误配**
   - 所有 DXGI adapter/output 都参与探测，并优先包含桌面原点的主显示器。
   - oneVPL implementation 使用 `mfxExtendedDeviceId.DeviceLUID` 精确匹配 DXGI adapter；旧 dispatcher 无 LUID 时只允许同厂商唯一 adapter 的无歧义兼容路径。
   - 非 identity rotation 明确返回“`不支持的桌面模式`”；显示 rect/rotation/HDR 指纹变化会触发重新探测。

9. **无界 bitstream/surface 显存占用**
   - oneVPL bitstream 按分辨率、route 与 HRD 推导初始容量，仅在 `MFX_ERR_NOT_ENOUGH_BUFFER` 时复用并增长。
   - capture slot 在 4K 保留 32 个，8K/跨设备路径按显存预算收紧。

10. **磁盘开放分段与完成分段保存游标分裂**
    - 两者改为共用源时间游标；游标落在分段中间时从下一段可独立解码关键帧继续，不会因保存过开放分段而永久跳过后来落盘的片段。

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

1. 通配符导入、长参数列表和局部 lint 抑制仍存在，需在不改变热路径行为的前提下渐进清理。
2. oneVPL/D3D11 FFI 的完整句柄 RAII 和逐操作 `SAFETY` 说明尚未全部完成。
3. WGC 持久捕获服务与两种后端仍缺少多小时长跑、睡眠/唤醒和显示器热插拔自动化。
4. 系统化 failpoint、磁盘空间耗尽、权限变化、writer panic 等故障测试仍未全部自动化。

## 本次合并验证

- `cargo fmt --check`
- `cargo test --locked --target x86_64-pc-windows-msvc`：88 passed，11 ignored
- `cargo clippy --all-targets --locked --target x86_64-pc-windows-msvc -- -D warnings`
- `cargo build --release --locked --target x86_64-pc-windows-msvc`
- NVENC WGC：4K/170 Hz/HDR PQ/P010，真实录制、MP4 封装及五次重复启停通过
- NVENC DDA：4K/170 Hz/HDR PQ/P010，真实录制、MP4 封装及五次重复启停通过
- `ffprobe`：HEVC Main10、yuv420p10le、BT.2020/PQ/full-range、AAC LC
- ffmpeg rawvideo 完整解码：WGC/DDA 均无 HEVC 解码错误
