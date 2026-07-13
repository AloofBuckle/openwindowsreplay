#[cfg(windows)]
pub(crate) const P010_CONVERT_HLSL: &str = include_str!("shaders/p010_convert.hlsl");

#[cfg(windows)]
pub(crate) const P010_SDR10_CONVERT_HLSL: &str = include_str!("shaders/p010_sdr10_convert.hlsl");

#[cfg(windows)]
pub(crate) const P010_SDR_BT2020_CONVERT_HLSL: &str =
    include_str!("shaders/p010_sdr_bt2020_convert.hlsl");

#[cfg(windows)]
pub(crate) const NV12_CONVERT_HLSL: &str = include_str!("shaders/nv12_convert.hlsl");

#[cfg(windows)]
pub(crate) const NV12_BT2020_CONVERT_HLSL: &str = include_str!("shaders/nv12_bt2020_convert.hlsl");

#[cfg(windows)]
pub(crate) const YUY2_CONVERT_HLSL: &str = include_str!("shaders/yuy2_convert.hlsl");

#[cfg(windows)]
pub(crate) const YUY2_BT2020_CONVERT_HLSL: &str = include_str!("shaders/yuy2_bt2020_convert.hlsl");

#[cfg(windows)]
pub(crate) const AYUV_CONVERT_HLSL: &str = include_str!("shaders/ayuv_convert.hlsl");

#[cfg(windows)]
pub(crate) const AYUV_BT2020_CONVERT_HLSL: &str = include_str!("shaders/ayuv_bt2020_convert.hlsl");

#[cfg(windows)]
pub(crate) const Y210_CONVERT_HLSL: &str = include_str!("shaders/y210_convert.hlsl");

#[cfg(windows)]
pub(crate) const Y410_CONVERT_HLSL: &str = include_str!("shaders/y410_convert.hlsl");

#[cfg(windows)]
pub(crate) const Y210_SDR10_CONVERT_HLSL: &str = include_str!("shaders/y210_sdr10_convert.hlsl");

#[cfg(windows)]
pub(crate) const Y210_SDR_BT2020_CONVERT_HLSL: &str =
    include_str!("shaders/y210_sdr_bt2020_convert.hlsl");

#[cfg(windows)]
pub(crate) const Y410_SDR10_CONVERT_HLSL: &str = include_str!("shaders/y410_sdr10_convert.hlsl");

#[cfg(windows)]
pub(crate) const Y410_SDR_BT2020_CONVERT_HLSL: &str =
    include_str!("shaders/y410_sdr_bt2020_convert.hlsl");

#[cfg(windows)]
pub(crate) const SNAPSHOT_COPY_HLSL: &str = include_str!("shaders/snapshot_copy.hlsl");

#[cfg(windows)]
pub(crate) const RGBA_CONVERT_HLSL: &str = include_str!("shaders/rgba_convert.hlsl");
