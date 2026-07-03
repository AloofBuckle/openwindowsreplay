1，GUI应是简体中文
2，控件列表：
顶部：开始即时回放 保存即时回放 停止即时回放
中间左侧：选什么色度采样？{420 422 444}（注意此处较为特殊，设备并不总是全部支持这些色度抽样，所有后端返回不支持时应隐藏对应的选项）；要什么码率控制模式？这里非常特殊，要求的做法是暴露完整oneVPL的RateControlMethod字段，并对每个本机支持的字段都做完整模式实现，这里先给你接口列表，按列表做:{1. oneVPL 现有 RateControlMethod

oneVPL 文档里 RateControlMethod 枚举包括：

模式	名称	核心含义
MFX_RATECONTROL_CBR	CBR	恒定码率算法
MFX_RATECONTROL_VBR	VBR	可变码率算法
MFX_RATECONTROL_CQP	CQP	固定 QP
MFX_RATECONTROL_AVBR	AVBR	Average VBR，按一段收敛期逼近平均码率
MFX_RATECONTROL_LA	Look-ahead VBR	带前瞻分析的 VBR，提升质量但增加延迟和内存
MFX_RATECONTROL_ICQ	Intelligent Constant Quality	智能恒定质量，只吃质量因子
MFX_RATECONTROL_VCM	Video Conferencing Mode	视频会议模式，类似 VBR，偏 IPPP、强时序相关内容
MFX_RATECONTROL_LA_ICQ	Look-ahead ICQ	ICQ + look-ahead
MFX_RATECONTROL_LA_HRD	Look-ahead HRD	HRD compliant look-ahead
MFX_RATECONTROL_QVBR	Quality-defined VBR	VBR + 恒定主观质量目标 + 码率/HRD 约束

MFX_RATECONTROL_LA_EXT 在当前文档里已经标注为 removed，不应作为现在的可用枚举理解。oneVPL 对这些模式的描述见官方枚举表，尤其是 LA、ICQ、VCM、LA_ICQ、LA_HRD、QVBR 的说明。

2. mfxInfoMFX 里的码控主字段

核心在 mfxVideoParam par; par.mfx... 里。头文件结构大致是这样，注意它用了 union，所以同一位置在不同模式下含义不同：

par.mfx.RateControlMethod;

par.mfx.BRCParamMultiplier; // 码率/缓冲字段的倍率

// union 1
par.mfx.InitialDelayInKB;   // CBR/VBR/VCM/QVBR/HRD类
par.mfx.QPI;                // CQP
par.mfx.Accuracy;           // AVBR

par.mfx.BufferSizeInKB;     // VBV/HRD buffer size

// union 2
par.mfx.TargetKbps;         // CBR/VBR/AVBR/LA/VCM/QVBR/LA_HRD
par.mfx.QPP;                // CQP
par.mfx.ICQQuality;         // ICQ/LA_ICQ

// union 3
par.mfx.MaxKbps;            // VBR/VCM/QVBR/HRD类
par.mfx.QPB;                // CQP
par.mfx.Convergence;        // AVBR

RateControlMethod、InitialDelayInKB/QPI/Accuracy、TargetKbps/QPP/ICQQuality、MaxKbps/QPB/Convergence 这些 union 字段在 Intel 当前头文件里就是这么定义的。
BRCParamMultiplier 会乘到 InitialDelayInKB、BufferSizeInKB、TargetKbps、MaxKbps、WinBRCMaxAvgKbps，这是处理超过 16-bit 表达范围码率时很重要的字段。

3. 按模式展开字段
CBR
par.mfx.RateControlMethod = MFX_RATECONTROL_CBR;
par.mfx.TargetKbps        = ...;
par.mfx.BufferSizeInKB    = ...; // 0 = 由库计算
par.mfx.InitialDelayInKB  = ...; // 0 = 由库计算

含义是目标恒定码率。HRD/VBV 模型里，数据按 TargetKbps 流入 BufferSizeInKB 大小的 buffer；InitialDelayInKB 决定初始延迟。InitialDelayInKB 或 BufferSizeInKB 为 0 时，库会按码率、帧率、profile、level 等计算。

VBR
par.mfx.RateControlMethod = MFX_RATECONTROL_VBR;
par.mfx.TargetKbps        = ...; // 平均/目标码率
par.mfx.MaxKbps           = ...; // VBV 输入最大码率，0 = 由库计算
par.mfx.BufferSizeInKB    = ...;
par.mfx.InitialDelayInKB  = ...;

VBR 下 MaxKbps 表示 encoded data 进入 VBV buffer 的最大码率；为 0 时由库按码率、帧率、profile、level 等计算。

CQP
par.mfx.RateControlMethod = MFX_RATECONTROL_CQP;
par.mfx.QPI               = ...;
par.mfx.QPP               = ...;
par.mfx.QPB               = ...;

含义是 I/P/B 帧分别固定 QP。QPI/QPP/QPB 为 0 通常不是“QP=0”，而是让库分配默认值；AV1 有特殊说明：QPI=QPP=QPB=0 时是 lossless。

CQP 也可以做 CQP HRD，但那时需要按 CBR/VBR 类似方式提供 HRD 相关码率参数，而不是主要靠 QPI/QPP/QPB；Intel 文档明确说应用要自己负责 per-frame QP、HRD conformance 和 SEI。

AVBR
par.mfx.RateControlMethod = MFX_RATECONTROL_AVBR;
par.mfx.TargetKbps        = ...;
par.mfx.Accuracy          = ...; // 十分之一百分点
par.mfx.Convergence       = ...; // 单位：100 frames

AVBR 使用 TargetKbps + Accuracy + Convergence。它的目标是在 Convergence 收敛期之后，让整体码率落在 TargetKbps 的 Accuracy 范围内；它 不遵循 HRD，瞬时码率也不会被 cap 或 padding。

LA
par.mfx.RateControlMethod = MFX_RATECONTROL_LA;
par.mfx.TargetKbps        = ...;

mfxExtCodingOption2 co2 = {};
co2.LookAheadDepth      = ...; // 10-100，0 = 默认

LA 是 “VBR with look ahead”。官方说它会在实际编码前分析几十帧，因此提升质量，但会显著增加编码延迟和内存消耗。这个模式下官方明确说主码控参数只有 TargetKbps，MaxKbps 和 InitialDelayInKB 被忽略；LookAheadDepth 控制前瞻深度。

可选：

mfxExtCodingOption3 co3 = {};
co3.WinBRCMaxAvgKbps = ...;
co3.WinBRCSize       = ...;

WinBRCMaxAvgKbps/WinBRCSize 对 CBR、VBR、LA、LA_HRD、QVBR 都适用，用于限制滑动窗口内平均最大码率；两个都设 0 表示关闭 sliding window BRC。

ICQ
par.mfx.RateControlMethod = MFX_RATECONTROL_ICQ;
par.mfx.ICQQuality        = ...; // 1-51，1 最好

ICQ 是 Intelligent Constant Quality，只用一个控制参数：ICQQuality。官方定义为 1 到 51，1 是最佳质量。

VCM
par.mfx.RateControlMethod = MFX_RATECONTROL_VCM;
par.mfx.TargetKbps        = ...;
par.mfx.MaxKbps           = ...;
par.mfx.BufferSizeInKB    = ...;
par.mfx.InitialDelayInKB  = ...;

VCM 类似 VBR，使用 InitialDelayInKB / TargetKbps / MaxKbps 这组参数；它面向视频会议场景，偏 IPPP GOP 和强时间相关内容。官方还说它不支持 interlaced、不支持 B 帧，输出流不 HRD compliant。

可选：

mfxExtCodingOption3 co3 = {};
co3.LowDelayBRC = MFX_CODINGOPTION_ON; // VBR/QVBR/VCM 可用

LowDelayBRC 在 VBR/QVBR/VCM 下表示 frame size tolerance，可用于更严格遵守由 MaxKbps 推出的平均帧大小。

LA_ICQ
par.mfx.RateControlMethod = MFX_RATECONTROL_LA_ICQ;
par.mfx.ICQQuality        = ...; // 1-51，1 最好

mfxExtCodingOption2 co2 = {};
co2.LookAheadDepth      = ...; // 10-100，0 = 默认

LA_ICQ 就是 ICQ + look-ahead；官方说质量因子用 ICQQuality，前瞻深度用 mfxExtCodingOption2::LookAheadDepth，并且这个模式不 HRD compliant。

LA_HRD
par.mfx.RateControlMethod = MFX_RATECONTROL_LA_HRD;
par.mfx.TargetKbps        = ...;
par.mfx.MaxKbps           = ...;
par.mfx.BufferSizeInKB    = ...;
par.mfx.InitialDelayInKB  = ...;

mfxExtCodingOption2 co2 = {};
co2.LookAheadDepth      = ...;

官方对 LA_HRD 的枚举描述很短：HRD compliant look ahead rate control algorithm。字段组合可理解为 look-ahead + HRD/VBV 约束：LookAheadDepth 控制前瞻，HRD/VBV 类字段由 TargetKbps / MaxKbps / BufferSizeInKB / InitialDelayInKB 提供。这个组合最好用 MFXVideoENCODE_Query() 对具体 codec/GPU/driver 验证。

可选同 LA：

mfxExtCodingOption3 co3 = {};
co3.WinBRCMaxAvgKbps = ...;
co3.WinBRCSize       = ...;

这两个 sliding-window 字段明确适用于 LA_HRD。

QVBR
par.mfx.RateControlMethod = MFX_RATECONTROL_QVBR;
par.mfx.TargetKbps        = ...;
par.mfx.MaxKbps           = ...;
par.mfx.BufferSizeInKB    = ...;
par.mfx.InitialDelayInKB  = ...;

mfxExtCodingOption3 co3 = {};
co3.QVBRQuality          = ...; // 1-51，1 最好

QVBR 是 VBR + constant subjective quality。官方说它试图用最少 bits 达到目标主观质量，同时满足码率约束和 HRD conformance；字段就是 VBR 那套，再加 mfxExtCodingOption3::QVBRQuality。QVBRQuality 范围 1 到 51，1 是最佳质量。

可选：

co3.WinBRCMaxAvgKbps = ...;
co3.WinBRCSize       = ...;
co3.LowDelayBRC      = MFX_CODINGOPTION_ON;

WinBRCMaxAvgKbps/WinBRCSize 明确适用于 QVBR；LowDelayBRC 也明确适用于 VBR/QVBR/VCM。

4. HRD / VUI 相关字段

AVC 下还有这些常和码控一起用的扩展字段：

mfxExtCodingOption co = {};
co.NalHrdConformance   = MFX_CODINGOPTION_ON;
co.VuiNalHrdParameters = MFX_CODINGOPTION_ON;
co.VuiVclHrdParameters = MFX_CODINGOPTION_ON; // VBR 时写 VCL HRD 参数

NalHrdConformance=ON 会要求 AVC encoder 产生 HRD conformant bitstream；VuiNalHrdParameters 控制是否在 VUI header 写入 NAL HRD 参数；VuiVclHrdParameters 在 VBR 下会写 VCL HRD 参数，并让其值与 NAL HRD 参数相同。

5. 额外码控开关
mfxExtCodingOption2 co2 = {};
co2.MaxFrameSize   = ...; // VBR-based modes
co2.MBBRC          = MFX_CODINGOPTION_ON;
co2.ExtBRC         = MFX_CODINGOPTION_ON;
co2.LookAheadDepth = ...;

MaxFrameSize 用于 VBR-based 码控模式，限制单帧最大编码大小，但可能有轻微 overshoot；MBBRC 开启宏块级码率控制，通常提升主观质量但可能影响性能和客观指标；ExtBRC 开启外部 BRC，需要配合 mfxExtBRC 回调结构。}
以上这些RateControlMethod与前面的其他编码器参数放在一个区域内，引导式填参即先选RateControlMethod，然后才能填选中的这个RateControlMethod的参数字段；
中间右侧：录制循环器参数，可选字段为循环缓存的目录{dir：}，落盘保存的目录{dir：}，要回放多久{(允许填小数):min}
底部左侧显示编码器日志，右侧显示循环器日志
