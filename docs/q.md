# NVENC 合并与问题审计状态

本文对应 `RustReplay-NVENC/docs/q.md` 的 16 项问题，并补充审计合并后新增的 NVENC DDA/WGC 代码。基线 NVENC 合并提交为 `88a8367`；本轮审计日期为 2026-07-15，并于 2026-07-16 补充 DDA split 高刷新损坏复现与修复结果。

## 结论

- NVENC 已同时具备 WGC 与 DDA 生产路线。
- WGC：capture shader 直接写同设备普通 NVENC input texture，无额外 `CopyResource`。
- DDA：独立 D3D11 capture device 的 shader 写 legacy ordinary shared route texture，NVENC device 通过 shared fence 建立 GPU 依赖后执行一次 `CopyResource` 到 encoder-local input，再交给 NVENC；不经过 CPU readback/staging。
- oneVPL 与 NVENC 共用 encoded ring、磁盘循环、AAC、MP4 和保存控制器；本轮修复均同时覆盖两种编码后端。
- 当前没有发现阻止主线构建、NVENC WGC 录制或 NVENC DDA 录制的剩余 P0/P1 缺陷。

## 逐项状态

### 1. oneVPL / NVENC / GPU 资源异常释放

**已关闭主要运行时风险。**

- oneVPL loader/session/encoder、surface、capture thread、shared HANDLE 与 keyed mutex 均有 RAII guard。
- oneVPL 正常结束显式检查 `MFXVideoENCODE_Close` 与 `MFXClose`；异常路径由幂等 `Drop` 继续清理。
- surface 获取后尽早进入 guard；后续 surface 即使返回空 `FrameInterface` 也使用已验证的 release function 释放引用。
- NVENC 正常路径显式检查 `NvEncUnlockBitstream`、`NvEncUnmapInputResource`、`NvEncUnregisterResource`、`NvEncDestroyBitstreamBuffer` 与 `NvEncDestroyEncoder`。
- NVENC 任一中途错误仍由 guard / `Drop` 解锁、unmap、unregister 和 destroy。
- 所有会产生资源的 NVENC FFI 调用在调用前先确认对应释放函数存在。

### 2. ring 使用最后插入包判断最新时间

**已关闭。**

- 音视频使用独立有序队列。
- ring 独立维护最大 PTS / 最大结束时间，不依赖最后插入顺序。
- 乱序晚到音频按自身时间线裁剪，snapshot 再按 PTS 合并。

### 3. 磁盘 writer 队列无界

**已关闭。**

- 使用容量 3 的 `sync_channel`。
- 队列满、writer 断开、writer panic、等待音频的开放分段超限都会终止会话。
- writer 失败只通过 worker 的单一终结通道上报，不再同时产生 `Error` 与误导性的 `Stopped`。

### 4. 编码格式变化后合并不兼容数据

**已关闭。**

- metadata 变化会切换 memory-ring codec epoch。
- 内存保存游标由 `codec_epoch + PTS` 共同约束，旧 epoch 的高 PTS 不会永久屏蔽新 epoch。
- VPS/SPS/PPS 使用共享 tracker 按类别累计；初始参数集三类齐全后才可保存。
- 参数集更新暂存到下一个 HEVC IRAP 边界，再切换 codec epoch。
- 每个磁盘 builder 固定持有创建时的参数集 header，后续 epoch 不会污染旧分段。
- 磁盘拼接验证宽高、色彩、profile/bit-depth、VPS/SPS/PPS、AAC 采样率/声道和关键帧起点。

### 5. 找不到关键帧仍生成文件

**已关闭。**

- memory snapshot 找不到真实 HEVC random-access AU 时返回不可保存。
- readiness 同时要求完整参数集、关键帧和音频。
- MP4 writer 再次验证首个可播放 sample 必须为 IDR/CRA/BLA。

### 6. 用户主动停止被表示为失败

**已关闭。**

- backend cancellation 与真实错误分离。
- 用户停止最终只产生 `Stopped`。
- sink queue full、writer failure、无关键帧超时等内部失败最终只产生 `Error`。
- 用户停止与 writer 失败并发时，真实 writer 失败不会被 cancellation 吞掉。

### 7. 停止时临时磁盘文件依赖完整流程清理

**已关闭主要路径。**

- MP4、sidecar 和最终 replay 均使用 `.part -> rename` 事务。
- sidecar publish 失败会回滚已经发布的 MP4。
- writer 先停止并 join，再清理 store。
- 所有 worker 早退都进入同一终结清理路径。
- 删除失败保留 segment metadata，后续 prune/clear 可以重试并报告具体路径。
- store mutex 中毒时停止路径会恢复锁并继续尝试清理。
- 启动时只删除 RustReplay 自己命名的残留缓存文件。

### 8. 录制主函数职责过多

**部分关闭，剩余为维护性债务。**

- capture、route、timing、encode、oneVPL loop、NVENC loop、session、disk store、disk segment 和 MP4 已是实际 Rust 模块。
- `record_loop.rs` 仍较大，但本轮没有为纯行数目标重写已验证的热路径。

### 9. `unsafe` 作用域过大

**部分关闭。**

- 资源所有权已由 guard 封装，关键 FFI 正常/异常释放路径已验证。
- `record_loop.rs` 与 `nvenc.rs` 仍存在较宽 `unsafe` 边界和模块级 `unsafe_op_in_unsafe_fn` 抑制；这是后续渐进整理项，不是当前已复现的运行时故障。

### 10. 通配符导入过多

**未作为本轮生产修复处理。**

- 模块边界已经拆开，但若全面替换 `use super::*` 会产生大范围无行为收益 diff。
- 后续按模块维护时逐步改为显式导入。

### 11. 参数数量过多

**部分关闭。**

- session、route、sink、record request 已有结构化对象。
- 底层 capture/record FFI 入口仍有长参数列表；本轮不为形式重构改变已验证调用顺序。

### 12. lint 抑制范围过大

**部分关闭。**

- 当前 `cargo clippy --all-targets --locked --target x86_64-pc-windows-msvc -- -D warnings` 通过。
- 少量 FFI/长参数局部抑制仍保留，原因与 ABI 或现有接口有关。

### 13. 缺少资源释放故障注入

**部分关闭。**

- 增加 disk writer panic 注入并验证 stop/error 传播。
- 增加 sidecar publish 失败后的事务回滚测试。
- NVENC 空 bitstream pointer、缺失释放函数和正常关闭状态均由代码防护或硬件 smoke 覆盖。
- 尚未给每一个 oneVPL/NVENC 初始化步骤建立系统化 failpoint 矩阵。

### 14. 缺少乱序时间戳测试

**已关闭主要数据结构风险。**

- 覆盖旧音频晚到、VFR retention 边界、pending video、重复保存 cursor、同 PTS 合并和长时间绝对时间清理。

### 15. 缺少慢磁盘和磁盘失败测试

**部分关闭。**

- 覆盖 queue full、writer panic、publish rollback、删除失败保留并重试、lease 延迟删除。
- 尚未自动化真实磁盘空间耗尽、ACL 动态变化、网络卷断开和第三方长期占用文件。

### 16. 缺少编码格式变化测试

**已关闭主要 epoch 合并风险。**

- 覆盖分辨率变化、AAC 采样率变化、参数集变化、分散 VPS/SPS/PPS、旧/new epoch 保存游标及不兼容磁盘拼接。
- HDR/SDR 与 profile 变化走同一 metadata/参数集比较路径。

## 新增 NVENC 审计结论

- NVENC route 记录并验证 adapter LUID，避免 DXGI 枚举顺序变化后把旧计划应用到另一张 NVIDIA GPU。
- NVENC encoder D3D11 device 在运行时再次与当前桌面 adapter LUID 对比。
- NVENC DDA 固定使用 ordinary shared texture + shared fence + encoder-local safety copy：同设备直接 DDA 会严重串行化，P010 NTHANDLE render-target 纹理在当前驱动返回 `E_INVALIDARG`，而 shared texture 直接注册在 4K/240 Hz + split Auto 下会稳定产生三分区跨帧混合；三条失败路线均未保留在生产分支。
- DDA/WGC 源纹理宽高或格式变化会触发 `ReconfigureRequired`，不会在旧 encoder/session 内临时换格式。
- NVENC `frameRateNum/Den` 只取当前显示器刷新率作为码控提示；MP4 继续使用 DDA `LastPresentTime` / WGC `SystemRelativeTime` 的 VFR 时间戳。
- 驱动 API 兼容检查使用 `NvEncodeAPIGetMaxSupportedVersion` 的 `(major << 4) | minor` 编码，不与 NVENC FFI struct version 编码混用。
- 低于本程序编译 API 13.1 的驱动会明确报告版本不足，不会尝试错误 ABI。

## 当前验证

- `cargo fmt --check`
- `cargo clippy --all-targets --locked --target x86_64-pc-windows-msvc -- -D warnings`
- `cargo test`：106 passed，17 ignored，0 failed；ignored 项均为显式硬件 smoke。
- 本机 RTX 5090，4K/240 Hz/HDR PQ/P010：NVENC WGC 与修复后的 NVENC DDA 真实 GPU 动态源录制均通过。
- DDA shared texture 直接注册 + split Auto 的 5 秒样本在 1192 帧中检出 425 帧明显分区混帧；split Disabled 画面干净但只能编码 738 帧并丢弃 455 个源帧。
- 固定 safety-copy 路线的 10 秒样本编码 2391/2391 帧，`dropped_no_slot=0`，`frame_body` 平均约 3.96ms；2391 帧区域审计未发现 split 混帧。Lookahead 4 的 3 秒样本也完成 712/712 帧与正常 EOS flush。
- 独立 shared-fence 格式 smoke 已验证 NV12/Main、P010/Main10、AYUV/RExt 三种生产输入均可被 NVENC 直接注册并编码。
- 保留的 3 秒审计文件经 `ffprobe` 识别为 HEVC Main10 / yuv420p10le / BT.2020 / PQ / full-range + AAC LC。
- WGC/DDA 视频与音频分别完整解码到 raw sink，0 decode error；视频 packet DTS 严格递增。

## 剩余验证边界

以下项目仍需要真实环境或长期测试，不能用单元测试声称完成：

1. 多小时 WGC/DDA 长跑、睡眠/唤醒、显示器热插拔与 GPU driver reset。
2. 真实磁盘满、权限撤销、SMB 中断及文件被外部进程长期占用。
3. 每个 oneVPL/NVENC FFI 初始化步骤的系统化 failpoint 注入。
4. 更广泛的多 NVIDIA GPU、eGPU、非主输出和不同驱动版本矩阵。
