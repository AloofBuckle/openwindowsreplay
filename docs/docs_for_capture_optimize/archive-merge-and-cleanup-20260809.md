# RustReplay 实验归档合并与清理记录（2026-08-09）

## 范围

- 正式主仓：`Y:\Program\RustReplay`
- 实验归档：`C:\Users\Administrator\Documents\RustReplay-experiment-archive-20260809`
- 压缩恢复副本：`C:\Users\Administrator\Documents\RustReplay-experiment-archive-20260809.zip`
- ZIP SHA-256：`D9F06D3B67DA39EED2020BF48BECCE27F7F0BAEB4A0C7AB7527C9BC136C739D0`

两个 Git bundle 均已通过 `git bundle verify`，并报告完整历史。ZIP 已成功列出归档内容。

## 已合入主仓

- oneVPL Direct-Arc 输出物化与空输出处理。
- 流式音频重采样、Media Foundation AAC 预热及时间戳账本。
- WASAPI `GetMixFormat` RAII、原子配置写入。
- 内存/磁盘 encoded ring 的 drain、背压、清理告警、锁范围缩短和显示重配退避。
- WGC `FrameArrived` 事件等待。
- 基于源时间的 IDR 请求去重。
- 默认关闭、失败开放的 CPU placement。
- MP4 10 MHz 精确视频时间线、`RRSEG004` sidecar 及 `RRSEG003` 兼容读取。
- oneVPL/WGC 生产循环的 transport timestamp 与 100 ns presentation timestamp 分离：
  warmup 输出只提取参数集，正式 AU 保留精确源 PTS，AAC 与 ring 使用同一精确尾点。
- memory/disk ring 的音视频尾部兼容处理及离线回归测试。

## 明确未合入

以下 NVENC busy/lock-busy 重试与强制销毁实验保持排除：

- `retry_nvenc_status_with_timeout`
- `retry_nvenc_lock_busy_with_timeout`
- `submit_texture_with_cancel`
- `force_destroy_and_disarm`
- `destroy_now_retrying_busy`
- `NVENC_BUSY_RETRY`

D3D12、AMF、shared-fence、pooled payload、NvFBC/D3D9 新捕获路线等已否决实验也未进入主线。

## 本机可完成的验证

未运行 ignored 测试、真实录制或硬件负载测试。完成的离线验证：

```text
cargo fmt --all
git diff --check
cargo test --locked --no-run --jobs 1
cargo test --locked -- --test-threads=1

147 passed
0 failed
24 ignored
```

新增的精确时间线测试覆盖：

- 低于单个 90 kHz tick 的 100 ns 间隔仍以 10 MHz MP4 时间线保存。
- transport timestamp 量化不替换精确 source PTS。
- 延迟 warmup AU 不会进入正式视频时间线。
- 长 10 MHz 时长使用 version 1 `mdhd`。

oneVPL 设备不在本机，因此生产录制、flush 时间戳和 A/V 实机同步仍需设备回来后复测。

## 清理结果

- 归档清单列出的 15 个旧 Desktop/Codex/Temp 实验工作树均已不存在。
- 保留正式主仓、归档目录、ZIP 恢复副本及未归档的图像/音频分析结果。
- 清除了 7 个可重建的 RustReplay Cargo 审计/验证 target，释放约 6.5 GiB。
- 主仓改动未提交、未推送。
