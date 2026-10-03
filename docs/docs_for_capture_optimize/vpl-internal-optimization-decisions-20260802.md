# oneVPL 内部性能路线结论（B370，2026-08-02）

状态：本轮性能与质量 QA 已完成。本文只记录内部实现路线，不把用户可调项、
输出分辨率、目标 FPS、刷新率或 HDR/SDR 状态作为优化评价轴。

## 结论摘要

| 路线 | 结论 | 是否进入主线 | 重新开启条件 |
| --- | --- | --- | --- |
| Direct-Arc 输出物化 | 已验证的低风险微优化 | 是，以最小生产补丁合入 | 无需继续扩展性能矩阵；后续只做常规回归 |
| `MFX_WRN_DEVICE_BUSY` 主动同步 | 当前负载未触发，仍未验证 | 否 | 可控注入 BUSY，限定同步 deadline，并覆盖 flush |
| DDA Shared-Fence transport | 局部 CPU 正收益，但整体无正收益 | 否 | 用 GPU timestamp 拆分 Wait、Copy 和 completion 延迟后重测 |
| pooled payload owner/range | 当前普通 heap/slab 设计硬否决 | 否 | 必须改成有严格 commit 上界的稀疏虚拟地址新设计 |

“不再扩展性能矩阵”不等于“不合并”。Direct-Arc 的收益虽小，但实现本体是等价的
分配与复制消除，没有引入新的所有权或兼容性风险，因此应保留。DEVICE_BUSY 和
Shared-Fence 则会改变调度或 GPU 生命周期，不能按同一标准处理。

## QA 固定边界

- 当前前端最多提供 38 个语义控制项，对应 42 个用户可修改 JSON leaf。
- 输出分辨率、目标 FPS、刷新率和 HDR/SDR 切换不是本轮用户配置 QA 轴。
- 正式 A/B 只切换隐藏内部实现，用户配置文件保持不变。
- B370 固定路径为 DDA 捕获、oneVPL 编码、内存 encoded ring、HEVC/AAC MP4。
- 固定用户配置 SHA-256：
  `9C572934B4AB059A17A3AC565F272C32094B9D3635FB7E6432349B0F291A1224`。

## Direct-Arc：接受并合入

旧路径：oneVPL output slice → `Vec<u8>` → `Arc<[u8]>`，需要两次分配和两次复制。

新路径：oneVPL output slice → `Arc<[u8]>`，只需要一次分配和一次复制。最终交给
ring 和 MP4 writer 的类型原本就是 `Arc<[u8]>`；新路径仍在 bitstream storage
归还池之前同步完成复制。

### A/B 结果

同一测试二进制按 A-B-B-A-A-B 顺序执行，每个 case 8 秒：

| 路线 | formal ns/KiB 三次结果 | 中位数 | 平均物化时间中位数 |
| --- | --- | ---: | ---: |
| legacy | 620.363 / 607.211 / 595.694 | 607.211 | 73.068 µs/AU |
| Direct-Arc | 528.273 / 543.926 / 544.530 | 543.926 | 65.377 µs/AU |

- formal ns/KiB 中位数改善 10.42%。
- 平均物化时间中位数改善 10.53%。
- 按当次 AU 频率折算约节省 0.923 ms CPU/录制秒。
- 六个 case 均通过生命周期、内部 drop、MP4 packet 和完整解码门禁。

绝对收益不足以继续占用整体性能 QA 矩阵，但补丁本身只减少工作量和瞬时内存峰值，
因此结论是“接受微优化”，而不是“排除路线”。

### 主线提取原则与验证

生产补丁没有带入以下实验接线：

- `RUST_REPLAY_VPL_OUTPUT_MATERIALIZE` 环境开关；
- per-AU 计时和统计输出；
- `EncodedPayload` 通用 backing；
- pooled-range arena。

生产实现覆盖稳态 async AU 和 flush AU；空输出使用 `None`，不会为结束标记分配空
`Arc`。回归测试会在返回 AU 后覆写已经归还池的 bitstream storage，验证最终
`Arc` 仍持有完整独立字节。

本地 `master` 基线：
`546e7b8e97481a295849d1b2544817082d858cb7`。

远端 `Anywhere` 生产补丁验证：

- 补丁 SHA-256：
  `6B56C0924828D1797105FE3FCFB9D38A0CEF0A564942176ACB034C9EC49EEE84`；
- MSVC test executable SHA-256：
  `698D5511FBDD47E600F8E96671E6AD0FD793113104CD1F158E4C248089BF539D`；
- 8 秒 DDA → oneVPL → MP4：2880×1800、HEVC Main10、681 packets；
- AAC-LC 48 kHz stereo：376 packets；
- HEVC 与 AAC 完整解码退出码 0；
- MP4 SHA-256：
  `6A0E5D616DF5CEDA6082B60C0FA289877F2866CFFB7F8CC9441B17D37E0C7E8D`；
- 用户配置测试前后 SHA-256 相同。

实验 A/B 证据根目录：

`C:\Users\Administrator\Desktop\RustReplay-vpl-materialize-msvc-AA14DB9CA1F9\results`

生产补丁 smoke 证据：

`C:\Users\Administrator\Desktop\RustReplay-direct-arc-master-546E7B8-6B56C0924828\results\direct-arc-onevpl-8s`

## DEVICE_BUSY：保持开放，不合入当前候选

20 秒和 60 秒正式 control 分别提交 2,554 和 7,552 个请求，均得到
`busy_requests/events=0/0`。候选主动同步分支没有执行机会，因此没有候选收益或
副作用数据。

主动同步的高层顺序与 oneVPL 建议一致：优先同步 BUSY 返回的 sync point，其次同步
最旧 in-flight 请求，最后短暂等待。但当前实验实现会调用最长 60 秒的
`SyncOperation`，可能把原来的短退避变成长时间录制停顿；flush 仍是旧 retry，覆盖
也不完整。

本轮结论：不判死，也不直接合入。重新开启前必须：

1. 用 mock 或可重复 workload 注入 BUSY 和不同 sync 返回状态；
2. 为同步等待设置适合录制线程的严格 deadline；
3. 验证 async submit 与 flush 行为一致；
4. 同时比较恢复率、尾延迟、吞吐和录制完整性。

证据目录：

- `C:\Users\Administrator\Desktop\RustReplay-vpl-busy-ab-msvc-D584D6B37C1F\results\sleep-dda-01`
- `C:\Users\Administrator\Desktop\RustReplay-vpl-busy-ab-msvc-D584D6B37C1F\results\sleep-dda-02-60s`

## Shared-Fence：当前不合入

同一二进制 A-B-B-A-A-B，每个 case 12 秒：

| 指标 | keyed | shared-fence | 结果 |
| --- | --- | --- | --- |
| `source_fence` ms/frame | 0.085 / 0.085 / 0.083 | 0.045 / 0.044 / 0.041 | 中位数下降 48.24% |
| `frame_body` ms/frame | 0.661 / 0.920 / 0.628 | 0.877 / 0.869 / 0.570 | 加权增加 4.89% |

所有 shared-fence case 均满足 `wait = copy = completion-ready = slot-return`，没有内部
drop 或媒体错误。但该路线会增加 `ID3D11Device5`/`ID3D11DeviceContext4` 能力要求、
shared fence/handle、GPU completion query 和 slot 回收状态机。现有 CPU wall-time
不能证明 GPU 依赖提前完成，整体指标也没有正收益，因此当前不替换 keyed 默认路线。

证据目录：

`C:\Users\Administrator\Desktop\RustReplay-vpl-fence-msvc-5A0664FB2CAD-130538DD6D4A\results`

## pooled owner/range：硬否决当前设计

pooled-range 让 oneVPL 直接写 arena page，并把 `Arc<Page> + Range` 交给 ring。它确实
把物化成本从约 77.452 µs/AU 降到 0.276 µs/AU，且 fallback copy 为 0；但 B370 驱动
要求约 32 MiB `MaxLength`，页面几乎无法打包复用。

8 秒录制只提交约 128 MiB 有效码流，却累计分配 36,591,108,096 bytes，约
34,896 MiB。内存放大远大于被消除的复制，普通 heap/slab 版本不能合入。

最终实验安全门限制 16 MiB page 最多接受 4 MiB window；当驱动要求增长至 8 MiB
时受控返回 `UnsupportedGpuPath`，不再 OOM。

证据目录：

`C:\Users\Administrator\Desktop\RustReplay-vpl-pooled-msvc-9C92D0E9-39D33A70`

## 尚未排除的方向

以下路线没有被本轮数据关闭：

1. oneVPL surface copy、encode submit 和 sync retirement 的调度重叠；
2. Media Foundation AAC live encode worker 与录制主循环解耦；
3. DDA dirty/move metadata 驱动的增量转换；
4. 使用 GPU timestamp 重新审计 Shared-Fence。

后续实验继续冻结用户可调项，并同时满足性能、内存、生命周期和媒体完整性门禁。
