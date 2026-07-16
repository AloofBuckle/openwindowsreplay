use super::*;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) struct HevcDecoderConfiguration {
    pub(super) profile_space: u8,
    pub(super) tier_flag: bool,
    pub(super) profile_idc: u8,
    pub(super) profile_compatibility_flags: u32,
    pub(super) constraint_indicator_flags: [u8; 6],
    pub(super) level_idc: u8,
    pub(super) chroma_format_idc: u8,
    pub(super) bit_depth_luma_minus8: u8,
    pub(super) bit_depth_chroma_minus8: u8,
    pub(super) num_temporal_layers: u8,
    pub(super) temporal_id_nested: bool,
}

pub(super) fn decoder_configuration_from_parameter_sets(
    sets: &HevcParameterSets,
    expected: HevcCodecMetadata,
) -> Result<HevcDecoderConfiguration, BackendError> {
    let first = sets.sps.first().ok_or_else(|| {
        BackendError::unsupported("MP4 hvcC", "HEVC SPS", "缺少 SPS，无法生成解码配置")
    })?;
    let config = parse_hevc_sps_configuration(first).map_err(|reason| {
        BackendError::unsupported("MP4 hvcC", "HEVC SPS profile_tier_level", reason)
    })?;
    validate_codec_metadata(config, expected)?;
    for sps in sets.sps.iter().skip(1) {
        let candidate = parse_hevc_sps_configuration(sps).map_err(|reason| {
            BackendError::unsupported("MP4 hvcC", "HEVC SPS profile_tier_level", reason)
        })?;
        validate_codec_metadata(candidate, expected)?;
        if candidate != config {
            return Err(BackendError::unsupported(
                "MP4 hvcC",
                "多个 HEVC SPS",
                format!(
                    "同一 codec epoch 内 SPS 解码配置不一致：first={config:?} candidate={candidate:?}"
                ),
            ));
        }
    }
    Ok(config)
}

fn validate_codec_metadata(
    actual: HevcDecoderConfiguration,
    expected: HevcCodecMetadata,
) -> Result<(), BackendError> {
    if actual.profile_idc != expected.profile_idc
        || actual.chroma_format_idc != expected.chroma_format_idc
        || actual.bit_depth_luma_minus8 != expected.bit_depth_luma_minus8
        || actual.bit_depth_chroma_minus8 != expected.bit_depth_chroma_minus8
    {
        return Err(BackendError::unsupported(
            "MP4 hvcC",
            format!(
                "declared profile={} chroma={} depth={}/{}",
                expected.profile_idc,
                expected.chroma_format_idc,
                expected.bit_depth_luma_minus8 + 8,
                expected.bit_depth_chroma_minus8 + 8
            ),
            format!(
                "SPS 实际为 profile={} chroma={} depth={}/{}，拒绝生成容器与码流不一致的 MP4",
                actual.profile_idc,
                actual.chroma_format_idc,
                actual.bit_depth_luma_minus8 + 8,
                actual.bit_depth_chroma_minus8 + 8
            ),
        ));
    }
    Ok(())
}

fn parse_hevc_sps_configuration(sps_nal: &[u8]) -> Result<HevcDecoderConfiguration, String> {
    if sps_nal.len() < 3 || ((sps_nal[0] >> 1) & 0x3f) != 33 {
        return Err("NAL unit 不是 HEVC SPS".to_owned());
    }
    let rbsp = hevc_ebsp_to_rbsp(&sps_nal[2..]);
    let mut bits = BitReader::new(&rbsp);
    bits.skip(4)?; // sps_video_parameter_set_id
    let max_sub_layers_minus1 = bits.read_bits(3)? as usize;
    let temporal_id_nested = bits.read_bit()?;
    let profile_space = bits.read_bits(2)? as u8;
    let tier_flag = bits.read_bit()?;
    let profile_idc = bits.read_bits(5)? as u8;
    let profile_compatibility_flags = bits.read_bits(32)? as u32;
    let constraints = bits.read_bits(48)?;
    let mut constraint_indicator_flags = [0u8; 6];
    for (index, byte) in constraint_indicator_flags.iter_mut().enumerate() {
        *byte = ((constraints >> ((5 - index) * 8)) & 0xff) as u8;
    }
    let level_idc = bits.read_bits(8)? as u8;

    let mut sub_layer_profile_present = Vec::with_capacity(max_sub_layers_minus1);
    let mut sub_layer_level_present = Vec::with_capacity(max_sub_layers_minus1);
    for _ in 0..max_sub_layers_minus1 {
        sub_layer_profile_present.push(bits.read_bit()?);
        sub_layer_level_present.push(bits.read_bit()?);
    }
    if max_sub_layers_minus1 > 0 {
        bits.skip(2 * (8 - max_sub_layers_minus1))?;
    }
    for index in 0..max_sub_layers_minus1 {
        if sub_layer_profile_present[index] {
            bits.skip(88)?;
        }
        if sub_layer_level_present[index] {
            bits.skip(8)?;
        }
    }

    bits.read_ue()?; // sps_seq_parameter_set_id
    let chroma_format_idc =
        u8::try_from(bits.read_ue()?).map_err(|_| "SPS chroma_format_idc 超出 u8".to_owned())?;
    if chroma_format_idc > 3 {
        return Err(format!("SPS chroma_format_idc={chroma_format_idc} 非法"));
    }
    if chroma_format_idc == 3 {
        bits.skip(1)?; // separate_colour_plane_flag
    }
    bits.read_ue()?; // pic_width_in_luma_samples
    bits.read_ue()?; // pic_height_in_luma_samples
    if bits.read_bit()? {
        for _ in 0..4 {
            bits.read_ue()?;
        }
    }
    let bit_depth_luma_minus8 = u8::try_from(bits.read_ue()?)
        .map_err(|_| "SPS bit_depth_luma_minus8 超出 u8".to_owned())?;
    let bit_depth_chroma_minus8 = u8::try_from(bits.read_ue()?)
        .map_err(|_| "SPS bit_depth_chroma_minus8 超出 u8".to_owned())?;
    if bit_depth_luma_minus8 > 7 || bit_depth_chroma_minus8 > 7 {
        return Err(format!(
            "SPS bit depth 超出 hvcC 3-bit 字段：luma_minus8={bit_depth_luma_minus8} chroma_minus8={bit_depth_chroma_minus8}"
        ));
    }

    Ok(HevcDecoderConfiguration {
        profile_space,
        tier_flag,
        profile_idc,
        profile_compatibility_flags,
        constraint_indicator_flags,
        level_idc,
        chroma_format_idc,
        bit_depth_luma_minus8,
        bit_depth_chroma_minus8,
        num_temporal_layers: (max_sub_layers_minus1 + 1).min(7) as u8,
        temporal_id_nested,
    })
}

fn hevc_ebsp_to_rbsp(ebsp: &[u8]) -> Vec<u8> {
    let mut rbsp = Vec::with_capacity(ebsp.len());
    let mut zero_count = 0usize;
    for &byte in ebsp {
        if zero_count >= 2 && byte == 0x03 {
            zero_count = 0;
            continue;
        }
        rbsp.push(byte);
        zero_count = if byte == 0 { zero_count + 1 } else { 0 };
    }
    rbsp
}

struct BitReader<'a> {
    data: &'a [u8],
    bit_offset: usize,
}

impl<'a> BitReader<'a> {
    fn new(data: &'a [u8]) -> Self {
        Self {
            data,
            bit_offset: 0,
        }
    }

    fn read_bit(&mut self) -> Result<bool, String> {
        if self.bit_offset >= self.data.len().saturating_mul(8) {
            return Err("SPS RBSP 提前结束".to_owned());
        }
        let byte = self.data[self.bit_offset / 8];
        let shift = 7 - (self.bit_offset % 8);
        self.bit_offset += 1;
        Ok(((byte >> shift) & 1) != 0)
    }

    fn read_bits(&mut self, count: usize) -> Result<u64, String> {
        if count > 64 {
            return Err(format!("一次读取的 SPS 位数过大：{count}"));
        }
        let mut value = 0u64;
        for _ in 0..count {
            value = (value << 1) | u64::from(self.read_bit()?);
        }
        Ok(value)
    }

    fn skip(&mut self, count: usize) -> Result<(), String> {
        for _ in 0..count {
            self.read_bit()?;
        }
        Ok(())
    }

    fn read_ue(&mut self) -> Result<u64, String> {
        let mut leading_zero_bits = 0usize;
        while !self.read_bit()? {
            leading_zero_bits += 1;
            if leading_zero_bits > 63 {
                return Err("SPS Exp-Golomb 前导零过长".to_owned());
            }
        }
        let suffix = self.read_bits(leading_zero_bits)?;
        Ok(((1u64 << leading_zero_bits) - 1).saturating_add(suffix))
    }
}

#[cfg(test)]
pub(crate) fn synthetic_sps_nal(
    codec: HevcCodecMetadata,
    profile_compatibility_flags: u32,
    level_idc: u8,
) -> Vec<u8> {
    struct BitWriter {
        bytes: Vec<u8>,
        current: u8,
        used: u8,
    }

    impl BitWriter {
        fn new() -> Self {
            Self {
                bytes: Vec::new(),
                current: 0,
                used: 0,
            }
        }

        fn bit(&mut self, value: bool) {
            self.current = (self.current << 1) | u8::from(value);
            self.used += 1;
            if self.used == 8 {
                self.bytes.push(self.current);
                self.current = 0;
                self.used = 0;
            }
        }

        fn bits(&mut self, value: u64, count: usize) {
            for shift in (0..count).rev() {
                self.bit(((value >> shift) & 1) != 0);
            }
        }

        fn ue(&mut self, value: u64) {
            let code_num = value + 1;
            let bit_count = 64 - code_num.leading_zeros() as usize;
            for _ in 1..bit_count {
                self.bit(false);
            }
            self.bits(code_num, bit_count);
        }

        fn finish(mut self) -> Vec<u8> {
            self.bit(true); // rbsp_stop_one_bit
            while self.used != 0 {
                self.bit(false);
            }
            self.bytes
        }
    }

    let mut bits = BitWriter::new();
    bits.bits(0, 4); // sps_video_parameter_set_id
    bits.bits(0, 3); // sps_max_sub_layers_minus1
    bits.bit(true); // sps_temporal_id_nesting_flag
    bits.bits(0, 2); // general_profile_space
    bits.bit(false); // general_tier_flag
    bits.bits(u64::from(codec.profile_idc), 5);
    bits.bits(u64::from(profile_compatibility_flags), 32);
    bits.bits(0, 48); // general_constraint_indicator_flags
    bits.bits(u64::from(level_idc), 8);
    bits.ue(0); // sps_seq_parameter_set_id
    bits.ue(u64::from(codec.chroma_format_idc));
    if codec.chroma_format_idc == 3 {
        bits.bit(false); // separate_colour_plane_flag
    }
    bits.ue(16);
    bits.ue(16);
    bits.bit(false); // conformance_window_flag
    bits.ue(u64::from(codec.bit_depth_luma_minus8));
    bits.ue(u64::from(codec.bit_depth_chroma_minus8));
    let rbsp = bits.finish();

    let mut ebsp = Vec::with_capacity(rbsp.len() + 2);
    let mut zero_count = 0usize;
    for byte in rbsp {
        if zero_count >= 2 && byte <= 3 {
            ebsp.push(3);
            zero_count = 0;
        }
        ebsp.push(byte);
        zero_count = if byte == 0 { zero_count + 1 } else { 0 };
    }
    let mut nal = vec![33 << 1, 1];
    nal.extend(ebsp);
    nal
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_rext_profile_level_and_layout_from_sps() {
        let codec = HevcCodecMetadata::rext(3, 10);
        let sps = synthetic_sps_nal(codec, 0x1020_3040, 186);
        let parsed = parse_hevc_sps_configuration(&sps).unwrap();
        assert_eq!(parsed.profile_idc, 4);
        assert_eq!(parsed.profile_compatibility_flags, 0x1020_3040);
        assert_eq!(parsed.level_idc, 186);
        assert_eq!(parsed.chroma_format_idc, 3);
        assert_eq!(parsed.bit_depth_luma_minus8, 2);
        assert_eq!(parsed.bit_depth_chroma_minus8, 2);
        assert_eq!(parsed.num_temporal_layers, 1);
        assert!(parsed.temporal_id_nested);
    }
}
