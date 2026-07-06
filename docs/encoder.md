编码后端设计:完整的Rust低可变纯GPU即时重放后端，固定HEVC编码，DDA 不录制鼠标光标、WGC 录制鼠标光标，固定MPEG-4的mp4容器，设计面向VFR，最终设计目标是桌面同步，即此时DDA/WGC吐出来的帧节奏(VFR)，色彩空间(bt709/p3/bt2020)，量化范围(limited/full)和位深度(8/10；12-bit 路线已丢弃)是什么，最终输出的.mp4就是什么，后端只处理色度抽样转换(420/422/444)以及随之带来的色彩转换；后端应设计可被配置的项是色度抽样和码率控制模式，这里需要向前端暴露所有oneVPL可配置字段，具体都有什么请参考GUI.md;视频录制流水线如下
CaptureBackend:
  DDA
  WGC_BGRA8
  WGC_FP16
ColorTransform:
  SDR8_to_YUV
  SDR10_to_YUV10
  HDRPQ10_to_YUV10
ChromaWriter:
  420_NV12
  420_P010
  422_YUY2
  422_Y210
  422_P210（Query 可见但生产录制保持 Unsupported，因 DXGI/D3D11 无安全 P210 texture layout）
  444_AYUV
  444_Y410
  444_RGB4
VplEncoder:
  Query path support
  Init with FourCC + ChromaFormat + profile
  async encode pipeline；
已编码的裸流循环在内存里，保存时打包成mp4，；声音回放同样固定格式，流水线如下
WASAPI loopback 系统声
        ↓
48k stereo float PCM
        ↓
                混音器 → AAC LC → MP4 音轨
        ↑
48k stereo float PCM
        ↑
WASAPI 麦克风 capture
，两路声音和视频都需要需要对齐时间戳，最终合并为MP4


0,绝对前提是必须全程GPU拷贝，不设计也不需要任何CPU回落路径，整个视频录制后端都应该在显存中拷贝直到吐出裸流即接口契约:
- Capture 输出必须是同一 DXGI adapter 上的 ID3D11Texture2D。
- ColorTransform / ChromaWriter 只允许 Compute Shader / Video Processor / GPU copy。
- oneVPL 输入只允许 D3D11 video-memory surface；生产路线固定为一次 GPU CopyResource 写入 oneVPL 内部分配的 D3D11 surface，不保留旧绝对 0 拷贝或外部 surface 导入分支。
- 禁止 Map/Readback/Staging texture/CPU memcpy。
- 若 D3D11 video-memory surface / GPU-only 输入不可用，返回 UnsupportedGpuPath，不创建录制会话。
- GPU-only 只覆盖“未编码视频帧路径”；编码后的 mfxBitstream 是码流字节缓存，不再是 raw frame surface。
1，使用oneVPL的方法应是FFI
2，oneVPL操作按oneVPL的设计保证异步和多比特流，同步设计是性能危险的
3，对齐时间戳需要按绝对时间而不是帧号，因为我前面已经声明设计目标是VFR，帧号对齐必然面临速度不均问题
4，音画同步同样按绝对时间戳，这里不再赘述
5，启动时探测能力，即对以下能力进行验证
- adapter LUID
- oneVPL implementation name/version
- CodecId = HEVC
- ChromaFormat: 420 / 422 / 444
- FourCC: NV12 / P010 / YUY2 / Y210 / P210(Query 可见但生产 Unsupported) / AYUV / Y410 / RGB4
- BitDepthLuma/Chroma: 8 / 10（12-bit 路线已丢弃）
- CodecProfile: Main / Main10 / 422 / 444 相关 profile
- RateControlMethod 支持集
- D3D11 video-memory surface / one-copy GPU 输入支持状态
- AsyncDepth 建议值
，应对前端隐藏的参数应传回前端，并且后端应负责探测码率控制模式可用性，防止前端展示不支持的模式
