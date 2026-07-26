use super::*;

pub(super) const VIDEO_TIMESCALE: u32 = 90_000;
pub(super) const MOVIE_TIMESCALE: u32 = 1_000;

#[derive(Debug, Clone)]
pub struct HevcAccessUnit {
    pub timestamp_90k: u64,
    pub data: Arc<[u8]>,
    pub is_sync: bool,
    /// 只用于从预热帧中提取 VPS/SPS/PPS 等参数集，不写入 MP4 sample 表。
    pub discard_from_track: bool,
}

#[derive(Debug, Clone)]
pub struct HevcMp4Track {
    pub width: u16,
    pub height: u16,
    pub duration_90k: u64,
    pub color: NclxColorMetadata,
    pub codec: HevcCodecMetadata,
    pub samples: Vec<HevcAccessUnit>,
}

#[derive(Debug, Clone)]
pub struct AacAccessUnit {
    pub timestamp_ticks: u64,
    pub duration_ticks: u32,
    /// MP4 `mp4a` sample 应写入裸 AAC access unit，不包含 ADTS 头。
    pub data: Arc<[u8]>,
}

#[derive(Debug, Clone)]
pub struct AacLcMp4Track {
    pub sample_rate: u32,
    pub channel_count: u16,
    pub duration_ticks: u64,
    pub samples: Vec<AacAccessUnit>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct HevcCodecMetadata {
    pub profile_idc: u8,
    pub chroma_format_idc: u8,
    pub bit_depth_luma_minus8: u8,
    pub bit_depth_chroma_minus8: u8,
}

impl HevcCodecMetadata {
    pub const fn main_420_8() -> Self {
        Self {
            profile_idc: 1,
            chroma_format_idc: 1,
            bit_depth_luma_minus8: 0,
            bit_depth_chroma_minus8: 0,
        }
    }

    pub const fn main10_420_10() -> Self {
        Self {
            profile_idc: 2,
            chroma_format_idc: 1,
            bit_depth_luma_minus8: 2,
            bit_depth_chroma_minus8: 2,
        }
    }

    #[allow(dead_code)]
    pub const fn rext(chroma_format_idc: u8, bit_depth: u8) -> Self {
        let minus8 = bit_depth.saturating_sub(8);
        Self {
            profile_idc: 4,
            chroma_format_idc,
            bit_depth_luma_minus8: minus8,
            bit_depth_chroma_minus8: minus8,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct NclxColorMetadata {
    pub colour_primaries: u16,
    pub transfer_characteristics: u16,
    pub matrix_coefficients: u16,
    pub full_range: bool,
}

impl NclxColorMetadata {
    pub const fn bt2020_pq(full_range: bool) -> Self {
        Self {
            colour_primaries: 9,
            transfer_characteristics: 16,
            matrix_coefficients: 9,
            full_range,
        }
    }

    pub const fn bt2020_pq_full() -> Self {
        Self::bt2020_pq(true)
    }

    pub const fn bt2020_pq_limited() -> Self {
        Self::bt2020_pq(false)
    }

    pub const fn bt2020_sdr_10(full_range: bool) -> Self {
        Self {
            colour_primaries: 9,
            // ITU-T H.273/ISO 23091-2: BT.2020 10-bit transfer. Values
            // 1/6/14/15 are functionally equivalent for SDR, but 14 keeps the
            // BT.2020 SDR route distinguishable from BT.709 in metadata.
            transfer_characteristics: 14,
            matrix_coefficients: 9,
            full_range,
        }
    }

    pub const fn bt2020_sdr_8(full_range: bool) -> Self {
        Self {
            colour_primaries: 9,
            // 8-bit BT.2020 SDR has no dedicated H.273 transfer value; use
            // BT.709-compatible SDR transfer while preserving BT.2020 primaries
            // and non-constant-luminance matrix.
            transfer_characteristics: 1,
            matrix_coefficients: 9,
            full_range,
        }
    }

    pub const fn bt709(full_range: bool) -> Self {
        Self {
            colour_primaries: 1,
            transfer_characteristics: 1,
            matrix_coefficients: 1,
            full_range,
        }
    }

    #[allow(dead_code)]
    pub const fn bt709_full() -> Self {
        Self::bt709(true)
    }

    #[allow(dead_code)]
    pub const fn bt709_limited() -> Self {
        Self::bt709(false)
    }
}

#[derive(Debug, Clone)]
pub(super) struct PreparedSample {
    pub(super) duration_90k: u32,
    pub(super) data: SamplePayload,
    pub(super) is_sync: bool,
}

#[derive(Debug, Clone)]
pub(super) struct PreparedAudioSample {
    pub(super) timestamp_ticks: u64,
    pub(super) duration_ticks: u32,
    pub(super) data: SamplePayload,
}

#[derive(Debug, Clone)]
pub(super) enum SamplePayload {
    Memory(Arc<[u8]>),
    FileRange(Mp4SampleFileRange),
}

impl SamplePayload {
    pub(super) fn len(&self) -> u64 {
        match self {
            Self::Memory(data) => data.len() as u64,
            Self::FileRange(range) => range.len,
        }
    }
}

#[derive(Debug, Clone, Copy)]
pub(super) struct PreparedAacTrackRef<'a> {
    pub(super) track: &'a AacLcMp4Track,
    pub(super) samples: &'a [PreparedAudioSample],
    pub(super) offsets: &'a [u64],
}

#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub(crate) struct HevcParameterSets {
    pub(crate) vps: Vec<Vec<u8>>,
    pub(crate) sps: Vec<Vec<u8>>,
    pub(crate) pps: Vec<Vec<u8>>,
}

#[derive(Debug, Clone)]
pub(crate) struct Mp4SampleFileRange {
    pub(crate) path: PathBuf,
    pub(crate) offset: u64,
    pub(crate) len: u64,
}

#[derive(Debug, Clone)]
pub(crate) struct HevcIndexedSample {
    pub(crate) duration_90k: u32,
    pub(crate) is_sync: bool,
    pub(crate) offset: u64,
    pub(crate) len: u64,
}

#[derive(Debug, Clone)]
pub(crate) struct AacIndexedSample {
    pub(crate) timestamp_ticks: u64,
    pub(crate) duration_ticks: u32,
    pub(crate) offset: u64,
    pub(crate) len: u64,
}

#[derive(Debug, Clone)]
pub(crate) struct HevcIndexedMp4Track {
    pub(crate) width: u16,
    pub(crate) height: u16,
    pub(crate) duration_90k: u64,
    pub(crate) color: NclxColorMetadata,
    pub(crate) codec: HevcCodecMetadata,
    pub(crate) parameter_sets: HevcParameterSets,
    pub(crate) samples: Vec<HevcIndexedSample>,
}

#[derive(Debug, Clone)]
pub(crate) struct AacIndexedMp4Track {
    pub(crate) sample_rate: u32,
    pub(crate) channel_count: u16,
    pub(crate) duration_ticks: u64,
    pub(crate) samples: Vec<AacIndexedSample>,
}

#[derive(Debug, Clone)]
pub(crate) struct HevcAacMp4Index {
    pub(crate) video_track: HevcIndexedMp4Track,
    pub(crate) audio_track: Option<AacIndexedMp4Track>,
}

#[derive(Debug, Clone)]
pub(crate) struct HevcPreparedSample {
    pub(crate) duration_90k: u32,
    pub(crate) is_sync: bool,
    pub(crate) data: Mp4SampleFileRange,
}

#[derive(Debug, Clone)]
pub(crate) struct AacPreparedSample {
    pub(crate) timestamp_ticks: u64,
    pub(crate) duration_ticks: u32,
    pub(crate) data: Mp4SampleFileRange,
}

#[derive(Debug, Clone)]
pub(crate) struct HevcPreparedMp4Track {
    pub(crate) width: u16,
    pub(crate) height: u16,
    pub(crate) duration_90k: u64,
    pub(crate) color: NclxColorMetadata,
    pub(crate) codec: HevcCodecMetadata,
    pub(crate) parameter_sets: HevcParameterSets,
    pub(crate) samples: Vec<HevcPreparedSample>,
}

#[derive(Debug, Clone)]
pub(crate) struct AacPreparedMp4Track {
    pub(crate) sample_rate: u32,
    pub(crate) channel_count: u16,
    pub(crate) duration_ticks: u64,
    pub(crate) samples: Vec<AacPreparedSample>,
}
