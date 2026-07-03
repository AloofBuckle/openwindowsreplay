#![allow(non_snake_case, dead_code, unsafe_op_in_unsafe_fn)]
//! oneVPL 动态 FFI 能力探测。
//!
//! 不依赖封装 crate，运行时尝试加载系统/oneAPI/MSYS2 中的 libvpl。探测只读取
//! dispatcher 暴露的 `mfxImplDescription`，不启动同步编码循环。真正编码阶段仍需
//! `MFXVideoENCODE_Query/Init` 与 shared D3D11 surface import 再次验证。

use crate::config::ChromaSampling;
use crate::rate_control::RateControlMethod;
use libloading::Library;
use serde::{Deserialize, Serialize};
use std::collections::BTreeSet;
use std::ffi::{c_char, c_void};
use std::path::PathBuf;
use std::ptr;

const MFX_ERR_NONE: i32 = 0;
const MFX_ERR_NOT_FOUND: i32 = -9;
const MFX_WRN_PARTIAL_ACCELERATION: i32 = 4;
const MFX_IMPLCAPS_IMPLDESCSTRUCTURE: u32 = 1;
const MFX_IMPL_TYPE_HARDWARE: u32 = 0x0002;
const MFX_ACCEL_MODE_VIA_D3D11: u32 = 0x0300;
const MFX_RESOURCE_DX11_TEXTURE: u32 = 5;
const MFX_IOPATTERN_IN_VIDEO_MEMORY: u16 = 0x01;
const MFX_PICSTRUCT_PROGRESSIVE: u16 = 0x01;

const MFX_CODEC_HEVC: u32 = make_fourcc(b'H', b'E', b'V', b'C');
const MFX_FOURCC_NV12: u32 = make_fourcc(b'N', b'V', b'1', b'2');
const MFX_FOURCC_YUY2: u32 = make_fourcc(b'Y', b'U', b'Y', b'2');
const MFX_FOURCC_RGB4: u32 = make_fourcc(b'R', b'G', b'B', b'4');
const MFX_FOURCC_P010: u32 = make_fourcc(b'P', b'0', b'1', b'0');
const MFX_FOURCC_P210: u32 = make_fourcc(b'P', b'2', b'1', b'0');
const MFX_FOURCC_AYUV: u32 = make_fourcc(b'A', b'Y', b'U', b'V');
const MFX_FOURCC_Y210: u32 = make_fourcc(b'Y', b'2', b'1', b'0');
const MFX_FOURCC_Y410: u32 = make_fourcc(b'Y', b'4', b'1', b'0');

const MFX_PROFILE_HEVC_MAIN: u32 = 1;
const MFX_PROFILE_HEVC_MAIN10: u32 = 2;
const MFX_PROFILE_HEVC_MAINSP: u32 = 3;
const MFX_PROFILE_HEVC_REXT: u32 = 4;
const MFX_PROFILE_HEVC_SCC: u32 = 9;

const fn make_fourcc(a: u8, b: u8, c: u8, d: u8) -> u32 {
    (a as u32) | ((b as u32) << 8) | ((c as u32) << 16) | ((d as u32) << 24)
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct VplProbeInfo {
    pub available: bool,
    pub dll_path: Option<String>,
    pub load_error: Option<String>,
    pub implementations: Vec<VplImplementationInfo>,
    pub hevc_supported: bool,
    pub hevc_profiles: Vec<String>,
    pub input_fourcc: Vec<String>,
    pub chroma_candidates: Vec<ChromaSampling>,
    pub rate_controls: Vec<RateControlMethod>,
    pub dx11_texture_input_seen: bool,
    pub warnings: Vec<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct VplImplementationInfo {
    pub index: u32,
    pub impl_name: String,
    pub api_version: String,
    pub implementation: String,
    pub acceleration_mode: String,
    pub vendor_id: u32,
    pub vendor_impl_id: u32,
    pub device_id: String,
    pub media_adapter_type: u16,
    pub hevc_supported: bool,
    pub hevc_profiles: Vec<String>,
    pub input_fourcc: Vec<String>,
    pub rate_controls: Vec<RateControlMethod>,
    pub dx11_texture_input_seen: bool,
}

impl VplProbeInfo {
    fn unavailable(error: String) -> Self {
        Self {
            available: false,
            dll_path: None,
            load_error: Some(error.clone()),
            implementations: Vec::new(),
            hevc_supported: false,
            hevc_profiles: Vec::new(),
            input_fourcc: Vec::new(),
            chroma_candidates: Vec::new(),
            rate_controls: Vec::new(),
            dx11_texture_input_seen: false,
            warnings: vec![error],
        }
    }
}

pub fn probe_vpl() -> VplProbeInfo {
    let (api, dll_path) = match VplApi::load() {
        Ok(pair) => pair,
        Err(error) => return VplProbeInfo::unavailable(error),
    };

    let mut warnings = Vec::new();
    let mut implementations = Vec::new();

    unsafe {
        let loader = (api.mfx_load)();
        if loader.is_null() {
            return VplProbeInfo::unavailable(format!(
                "MFXLoad 返回空句柄，DLL={}",
                dll_path.display()
            ));
        }

        let mut index = 0u32;
        loop {
            let mut handle: MfxHDL = ptr::null_mut();
            let status = (api.mfx_enum_implementations)(
                loader,
                index,
                MFX_IMPLCAPS_IMPLDESCSTRUCTURE,
                &mut handle,
            );
            if status == MFX_ERR_NOT_FOUND {
                break;
            }
            if status != MFX_ERR_NONE {
                warnings.push(format!(
                    "MFXEnumImplementations({index}) 返回 status={status}"
                ));
                break;
            }
            if handle.is_null() {
                warnings.push(format!("MFXEnumImplementations({index}) 返回空描述"));
                index += 1;
                continue;
            }

            let desc = &*(handle as *const MfxImplDescription);
            implementations.push(parse_impl(index, desc, &api, loader, &mut warnings));

            let release_status = (api.mfx_release_impl_description)(loader, handle);
            if release_status != MFX_ERR_NONE {
                warnings.push(format!(
                    "MFXDispReleaseImplDescription({index}) 返回 status={release_status}"
                ));
            }
            index += 1;
        }
        (api.mfx_unload)(loader);
    }

    if implementations.is_empty() {
        warnings.push("oneVPL dispatcher 未枚举到任何实现".to_owned());
    }

    let mut hevc_profiles = BTreeSet::new();
    let mut input_fourcc = BTreeSet::new();
    let mut rate_controls = BTreeSet::new();
    let mut chroma_candidates = BTreeSet::new();
    let mut hevc_supported = false;
    let mut dx11_texture_input_seen = false;

    for imp in &implementations {
        if imp.hevc_supported {
            hevc_supported = true;
        }
        if imp.dx11_texture_input_seen {
            dx11_texture_input_seen = true;
        }
        for profile in &imp.hevc_profiles {
            hevc_profiles.insert(profile.clone());
        }
        for fourcc in &imp.input_fourcc {
            input_fourcc.insert(fourcc.clone());
            if let Some(chroma) = chroma_from_fourcc_name(fourcc) {
                chroma_candidates.insert(chroma);
            }
        }
        for method in &imp.rate_controls {
            rate_controls.insert(*method);
        }
    }

    if hevc_supported && rate_controls.is_empty() {
        warnings.push("oneVPL 运行时未在 mfxImplDescription 中暴露 RateControlMethod 列表；为避免展示不支持模式，GUI 将隐藏码控模式".to_owned());
    }
    if hevc_supported && !dx11_texture_input_seen {
        warnings.push(
            "HEVC 实现未报告 MFX_RESOURCE_DX11_TEXTURE 输入；不满足 D3D11 zero-copy 前提"
                .to_owned(),
        );
    }

    VplProbeInfo {
        available: true,
        dll_path: Some(dll_path.display().to_string()),
        load_error: None,
        implementations,
        hevc_supported,
        hevc_profiles: hevc_profiles.into_iter().collect(),
        input_fourcc: input_fourcc.into_iter().collect(),
        chroma_candidates: chroma_candidates.into_iter().collect(),
        rate_controls: rate_controls.into_iter().collect(),
        dx11_texture_input_seen,
        warnings,
    }
}

struct VplApi {
    _library: Library,
    mfx_load: unsafe extern "C" fn() -> MfxLoader,
    mfx_unload: unsafe extern "C" fn(MfxLoader),
    mfx_enum_implementations: unsafe extern "C" fn(MfxLoader, u32, u32, *mut MfxHDL) -> i32,
    mfx_release_impl_description: unsafe extern "C" fn(MfxLoader, MfxHDL) -> i32,
    mfx_create_session: unsafe extern "C" fn(MfxLoader, u32, *mut MfxSession) -> i32,
    mfx_close: unsafe extern "C" fn(MfxSession) -> i32,
    mfx_video_encode_query:
        unsafe extern "C" fn(MfxSession, *mut MfxVideoParam, *mut MfxVideoParam) -> i32,
}

impl VplApi {
    fn load() -> Result<(Self, PathBuf), String> {
        let mut attempts = Vec::new();
        for path in candidate_dlls() {
            let result = unsafe { Library::new(&path) };
            match result {
                Ok(library) => {
                    let api = unsafe {
                        let mfx_load = *library
                            .get::<unsafe extern "C" fn() -> MfxLoader>(b"MFXLoad\0")
                            .map_err(|e| e.to_string())?;
                        let mfx_unload = *library
                            .get::<unsafe extern "C" fn(MfxLoader)>(b"MFXUnload\0")
                            .map_err(|e| e.to_string())?;
                        let mfx_enum_implementations = *library
                            .get::<unsafe extern "C" fn(MfxLoader, u32, u32, *mut MfxHDL) -> i32>(
                                b"MFXEnumImplementations\0",
                            )
                            .map_err(|e| e.to_string())?;
                        let mfx_release_impl_description = *library
                            .get::<unsafe extern "C" fn(MfxLoader, MfxHDL) -> i32>(
                                b"MFXDispReleaseImplDescription\0",
                            )
                            .map_err(|e| e.to_string())?;
                        let mfx_create_session = *library
                            .get::<unsafe extern "C" fn(MfxLoader, u32, *mut MfxSession) -> i32>(
                                b"MFXCreateSession\0",
                            )
                            .map_err(|e| e.to_string())?;
                        let mfx_close = *library
                            .get::<unsafe extern "C" fn(MfxSession) -> i32>(b"MFXClose\0")
                            .map_err(|e| e.to_string())?;
                        let mfx_video_encode_query = *library
                            .get::<unsafe extern "C" fn(
                                MfxSession,
                                *mut MfxVideoParam,
                                *mut MfxVideoParam,
                            ) -> i32>(b"MFXVideoENCODE_Query\0")
                            .map_err(|e| e.to_string())?;
                        Self {
                            _library: library,
                            mfx_load,
                            mfx_unload,
                            mfx_enum_implementations,
                            mfx_release_impl_description,
                            mfx_create_session,
                            mfx_close,
                            mfx_video_encode_query,
                        }
                    };
                    return Ok((api, path));
                }
                Err(err) => attempts.push(format!("{}: {err}", path.display())),
            }
        }
        Err(format!(
            "未能加载 oneVPL DLL；尝试路径: {}",
            attempts.join(" | ")
        ))
    }
}

fn candidate_dlls() -> Vec<PathBuf> {
    let mut out = Vec::new();
    if let Ok(path) = std::env::var("RUSTREPLAY_VPL_DLL") {
        out.push(PathBuf::from(path));
    }
    out.extend([
        PathBuf::from("libvpl-2.dll"),
        PathBuf::from("libvpl.dll"),
        PathBuf::from("vpl.dll"),
        PathBuf::from("onevpl.dll"),
        PathBuf::from(r"C:\Program Files (x86)\Intel\oneAPI\vpl\latest\bin\libvpl.dll"),
        PathBuf::from(r"C:\Program Files\Intel\oneAPI\vpl\latest\bin\libvpl.dll"),
        PathBuf::from(r"C:\msys64\ucrt64\bin\libvpl-2.dll"),
    ]);
    out
}

unsafe fn parse_impl(
    index: u32,
    desc: &MfxImplDescription,
    api: &VplApi,
    loader: MfxLoader,
    warnings: &mut Vec<String>,
) -> VplImplementationInfo {
    let mut hevc_supported = false;
    let mut hevc_profiles = BTreeSet::new();
    let mut input_fourcc = BTreeSet::new();
    let mut rate_controls = BTreeSet::new();
    let mut dx11_texture_input_seen = false;

    let codecs = bounded_slice(desc.Enc.Codecs, desc.Enc.NumCodecs, 128);
    for codec in codecs {
        if codec.CodecID != MFX_CODEC_HEVC {
            continue;
        }
        hevc_supported = true;
        if desc.Enc.Version.version >= struct_version(1, 1) && !codec.EncExtDesc.is_null() {
            let ext = &*codec.EncExtDesc;
            let methods = bounded_slice(ext.RateControlMethods, ext.NumRateControlMethods, 128);
            for &raw in methods {
                if let Some(method) = RateControlMethod::from_vpl_value(raw) {
                    rate_controls.insert(method);
                }
            }
        }

        let profiles = bounded_slice(codec.Profiles, codec.NumProfiles, 128);
        for profile in profiles {
            hevc_profiles.insert(hevc_profile_name(profile.Profile).to_owned());
            let mem_descs = bounded_slice(profile.MemDesc, profile.NumMemTypes, 128);
            for mem in mem_descs {
                if mem.MemHandleType == MFX_RESOURCE_DX11_TEXTURE {
                    dx11_texture_input_seen = true;
                }
                let color_formats = bounded_slice(mem.ColorFormats, mem.NumColorFormats, 256);
                for &fourcc in color_formats {
                    input_fourcc.insert(fourcc_to_string(fourcc));
                }
                if desc.Enc.Version.version >= struct_version(1, 1) && !mem.MemExtDesc.is_null() {
                    let mem_ext = &*mem.MemExtDesc;
                    let chromas = bounded_slice(
                        mem_ext.TargetChromaSubsamplings,
                        mem_ext.NumTargetChromaSubsamplings,
                        32,
                    );
                    for &chroma in chromas {
                        if let Some(mapped) = chroma_from_vpl(chroma) {
                            input_fourcc.insert(format!("ChromaFormat:{:?}", mapped));
                        }
                    }
                }
            }
        }
    }

    if hevc_supported && rate_controls.is_empty() {
        for method in query_rate_controls(api, loader, index, &input_fourcc, warnings) {
            rate_controls.insert(method);
        }
    }

    if desc.Enc.NumCodecs > 128 {
        warnings.push(format!("实现 {index} 编码器数量异常，已截断读取"));
    }

    VplImplementationInfo {
        index,
        impl_name: c_char_array_to_string(&desc.ImplName),
        api_version: version_to_string(desc.ApiVersion.version),
        implementation: match desc.Impl {
            MFX_IMPL_TYPE_HARDWARE => "硬件".to_owned(),
            1 => "软件".to_owned(),
            other => format!("未知({other})"),
        },
        acceleration_mode: acceleration_to_string(
            desc.AccelerationMode,
            &desc.AccelerationModeDescription,
        ),
        vendor_id: desc.VendorID,
        vendor_impl_id: desc.VendorImplID,
        device_id: c_char_array_to_string(&desc.Dev.DeviceID),
        media_adapter_type: desc.Dev.MediaAdapterType,
        hevc_supported,
        hevc_profiles: hevc_profiles.into_iter().collect(),
        input_fourcc: input_fourcc.into_iter().collect(),
        rate_controls: rate_controls.into_iter().collect(),
        dx11_texture_input_seen,
    }
}

unsafe fn query_rate_controls(
    api: &VplApi,
    loader: MfxLoader,
    implementation_index: u32,
    input_fourcc: &BTreeSet<String>,
    warnings: &mut Vec<String>,
) -> Vec<RateControlMethod> {
    let mut session: MfxSession = ptr::null_mut();
    let create_status = (api.mfx_create_session)(loader, implementation_index, &mut session);
    if create_status != MFX_ERR_NONE || session.is_null() {
        warnings.push(format!(
            "MFXCreateSession({implementation_index}) 失败，无法用 MFXVideoENCODE_Query 探测码控: status={create_status}"
        ));
        return Vec::new();
    }

    let (fourcc, chroma, bit_depth, profile) = choose_query_format(input_fourcc);
    let mut supported = Vec::new();
    for method in RateControlMethod::all() {
        let mut input = make_query_param(method, fourcc, chroma, bit_depth, profile);
        let mut output = input;
        let status = (api.mfx_video_encode_query)(session, &mut input, &mut output);
        if status >= MFX_ERR_NONE
            && status != MFX_WRN_PARTIAL_ACCELERATION
            && output.mfx.RateControlMethod == method.vpl_value()
        {
            supported.push(method);
        }
    }

    let close_status = (api.mfx_close)(session);
    if close_status != MFX_ERR_NONE {
        warnings.push(format!(
            "MFXClose({implementation_index}) 返回 status={close_status}"
        ));
    }
    supported
}

fn choose_query_format(input_fourcc: &BTreeSet<String>) -> (u32, u16, u16, u16) {
    if input_fourcc.contains("NV12") {
        (MFX_FOURCC_NV12, 1, 8, MFX_PROFILE_HEVC_MAIN as u16)
    } else if input_fourcc.contains("P010") {
        (MFX_FOURCC_P010, 1, 10, MFX_PROFILE_HEVC_MAIN10 as u16)
    } else if input_fourcc.contains("YUY2") {
        (MFX_FOURCC_YUY2, 2, 8, MFX_PROFILE_HEVC_REXT as u16)
    } else if input_fourcc.contains("P210") {
        (MFX_FOURCC_P210, 2, 10, MFX_PROFILE_HEVC_REXT as u16)
    } else if input_fourcc.contains("AYUV") {
        (MFX_FOURCC_AYUV, 3, 8, MFX_PROFILE_HEVC_REXT as u16)
    } else if input_fourcc.contains("Y410") {
        (MFX_FOURCC_Y410, 3, 10, MFX_PROFILE_HEVC_REXT as u16)
    } else {
        (MFX_FOURCC_NV12, 1, 8, MFX_PROFILE_HEVC_MAIN as u16)
    }
}

fn make_query_param(
    method: RateControlMethod,
    fourcc: u32,
    chroma: u16,
    bit_depth: u16,
    profile: u16,
) -> MfxVideoParam {
    let mut param: MfxVideoParam = unsafe { std::mem::zeroed() };
    param.AsyncDepth = 4;
    param.IOPattern = MFX_IOPATTERN_IN_VIDEO_MEMORY;
    param.mfx.BRCParamMultiplier = 1;
    param.mfx.FrameInfo.FourCC = fourcc;
    param.mfx.FrameInfo.Width = 1920;
    param.mfx.FrameInfo.Height = 1088;
    param.mfx.FrameInfo.CropW = 1920;
    param.mfx.FrameInfo.CropH = 1080;
    param.mfx.FrameInfo.FrameRateExtN = 60;
    param.mfx.FrameInfo.FrameRateExtD = 1;
    param.mfx.FrameInfo.PicStruct = MFX_PICSTRUCT_PROGRESSIVE;
    param.mfx.FrameInfo.ChromaFormat = chroma;
    param.mfx.FrameInfo.BitDepthLuma = bit_depth;
    param.mfx.FrameInfo.BitDepthChroma = bit_depth;
    param.mfx.CodecId = MFX_CODEC_HEVC;
    param.mfx.CodecProfile = profile;
    param.mfx.TargetUsage = 4;
    param.mfx.GopPicSize = 60;
    param.mfx.GopRefDist = 1;
    param.mfx.IdrInterval = 1;
    param.mfx.RateControlMethod = method.vpl_value();

    match method {
        RateControlMethod::Cbr => {
            param.mfx.TargetKbps = 20_000;
        }
        RateControlMethod::Vbr
        | RateControlMethod::Vcm
        | RateControlMethod::LaHrd
        | RateControlMethod::Qvbr => {
            param.mfx.TargetKbps = 20_000;
            param.mfx.MaxKbps = 30_000;
        }
        RateControlMethod::Cqp => {
            param.mfx.InitialDelayInKB = 23; // QPI
            param.mfx.TargetKbps = 25; // QPP
            param.mfx.MaxKbps = 27; // QPB
        }
        RateControlMethod::Avbr => {
            param.mfx.TargetKbps = 20_000;
            param.mfx.InitialDelayInKB = 100; // Accuracy
            param.mfx.MaxKbps = 100; // Convergence
        }
        RateControlMethod::La => {
            param.mfx.TargetKbps = 20_000;
        }
        RateControlMethod::Icq | RateControlMethod::LaIcq => {
            param.mfx.TargetKbps = 23; // ICQQuality
        }
    }
    param
}

unsafe fn bounded_slice<'a, T>(ptr: *const T, len: u16, max: usize) -> &'a [T] {
    if ptr.is_null() || len == 0 {
        &[]
    } else {
        std::slice::from_raw_parts(ptr, usize::from(len).min(max))
    }
}

fn c_char_array_to_string<const N: usize>(buf: &[c_char; N]) -> String {
    let bytes: Vec<u8> = buf
        .iter()
        .take_while(|&&c| c != 0)
        .map(|&c| c as u8)
        .collect();
    String::from_utf8_lossy(&bytes).trim().to_owned()
}

fn version_to_string(version: u32) -> String {
    let minor = version & 0xFFFF;
    let major = (version >> 16) & 0xFFFF;
    format!("{major}.{minor}")
}

const fn struct_version(major: u16, minor: u16) -> u16 {
    major * 256 + minor
}

fn acceleration_to_string(default_mode: u32, desc: &MfxAccelerationModeDescription) -> String {
    let mut modes = vec![acceleration_mode_name(default_mode).to_owned()];
    unsafe {
        for mode in bounded_slice(desc.Mode, desc.NumAccelerationModes, 32) {
            let name = acceleration_mode_name(*mode).to_owned();
            if !modes.contains(&name) {
                modes.push(name);
            }
        }
    }
    modes.join(", ")
}

fn acceleration_mode_name(mode: u32) -> &'static str {
    match mode {
        0 => "NA",
        0x0200 => "D3D9",
        MFX_ACCEL_MODE_VIA_D3D11 => "D3D11",
        0x0400 => "VAAPI",
        0x0401 => "VAAPI_DRM_MODESET",
        0x0402 => "VAAPI_GLX",
        0x0403 => "VAAPI_X11",
        0x0404 => "VAAPI_WAYLAND",
        0x0500 => "HDDLUNITE",
        _ => "未知",
    }
}

fn hevc_profile_name(profile: u32) -> &'static str {
    match profile {
        MFX_PROFILE_HEVC_MAIN => "HEVC Main",
        MFX_PROFILE_HEVC_MAIN10 => "HEVC Main10",
        MFX_PROFILE_HEVC_MAINSP => "HEVC Main Still Picture",
        MFX_PROFILE_HEVC_REXT => "HEVC RExt(含 422/444)",
        MFX_PROFILE_HEVC_SCC => "HEVC SCC",
        _ => "HEVC Unknown Profile",
    }
}

fn fourcc_to_string(value: u32) -> String {
    let bytes = value.to_le_bytes();
    if bytes.iter().all(|b| b.is_ascii_graphic() || *b == b' ') {
        String::from_utf8_lossy(&bytes).to_string()
    } else {
        format!("0x{value:08X}")
    }
}

fn chroma_from_fourcc_name(name: &str) -> Option<ChromaSampling> {
    match name {
        "NV12" | "P010" => Some(ChromaSampling::Yuv420),
        "YUY2" | "Y210" | "P210" => Some(ChromaSampling::Yuv422),
        "AYUV" | "Y410" | "RGB4" => Some(ChromaSampling::Yuv444),
        _ => None,
    }
}

fn chroma_from_vpl(value: u16) -> Option<ChromaSampling> {
    match value {
        1 => Some(ChromaSampling::Yuv420),
        2 => Some(ChromaSampling::Yuv422),
        3 => Some(ChromaSampling::Yuv444),
        _ => None,
    }
}

type MfxLoader = *mut c_void;
type MfxSession = *mut c_void;
type MfxHDL = *mut c_void;

#[repr(C)]
#[derive(Debug, Clone, Copy)]
struct MfxStructVersion {
    version: u16,
}

#[repr(C)]
#[derive(Debug, Clone, Copy)]
struct MfxVersion {
    version: u32,
}

#[repr(C)]
#[derive(Debug, Clone, Copy)]
struct MfxRange32U {
    Min: u32,
    Max: u32,
    Step: u32,
}

#[repr(C)]
#[derive(Debug, Clone, Copy)]
struct MfxFrameId {
    TemporalId: u16,
    PriorityId: u16,
    DependencyId: u16,
    QualityId: u16,
}

#[repr(C)]
#[derive(Debug, Clone, Copy)]
struct MfxFrameInfo {
    reserved: [u32; 4],
    ChannelId: u16,
    BitDepthLuma: u16,
    BitDepthChroma: u16,
    Shift: u16,
    FrameId: MfxFrameId,
    FourCC: u32,
    Width: u16,
    Height: u16,
    CropX: u16,
    CropY: u16,
    CropW: u16,
    CropH: u16,
    FrameRateExtN: u32,
    FrameRateExtD: u32,
    reserved3: u16,
    AspectRatioW: u16,
    AspectRatioH: u16,
    PicStruct: u16,
    ChromaFormat: u16,
    reserved2: u16,
}

#[repr(C)]
#[derive(Debug, Clone, Copy)]
struct MfxInfoMFX {
    reserved: [u32; 7],
    LowPower: u16,
    BRCParamMultiplier: u16,
    FrameInfo: MfxFrameInfo,
    CodecId: u32,
    CodecProfile: u16,
    CodecLevel: u16,
    NumThread: u16,
    TargetUsage: u16,
    GopPicSize: u16,
    GopRefDist: u16,
    GopOptFlag: u16,
    IdrInterval: u16,
    RateControlMethod: u16,
    InitialDelayInKB: u16,
    BufferSizeInKB: u16,
    TargetKbps: u16,
    MaxKbps: u16,
    NumSlice: u16,
    NumRefFrame: u16,
    EncodedOrder: u16,
}

#[repr(C)]
#[derive(Debug, Clone, Copy)]
struct MfxVideoParam {
    AllocId: u32,
    reserved: [u32; 2],
    reserved3: u16,
    AsyncDepth: u16,
    mfx: MfxInfoMFX,
    union_padding: [u8; 32],
    Protected: u16,
    IOPattern: u16,
    ExtParam: *mut *mut c_void,
    NumExtParam: u16,
    reserved2: u16,
}

#[repr(C)]
#[derive(Debug)]
struct MfxEncExtDescription {
    Version: MfxStructVersion,
    reserved: [u16; 10],
    NumRateControlMethods: u16,
    RateControlMethods: *const u16,
    reserved2: [u16; 11],
    NumExtBufferIDs: u16,
    ExtBufferIDs: *const u32,
}

#[repr(C)]
#[derive(Debug)]
struct MfxEncMemExtDescription {
    Version: MfxStructVersion,
    reserved: [u16; 13],
    TargetMaxBitDepth: u16,
    NumTargetChromaSubsamplings: u16,
    TargetChromaSubsamplings: *const u16,
}

#[repr(C)]
#[derive(Debug)]
struct MfxEncoderMemDesc {
    MemHandleType: u32,
    Width: MfxRange32U,
    Height: MfxRange32U,
    reserved: [u16; 2],
    MemExtDesc: *const MfxEncMemExtDescription,
    reserved3: u16,
    NumColorFormats: u16,
    ColorFormats: *const u32,
}

#[repr(C)]
#[derive(Debug)]
struct MfxEncoderProfile {
    Profile: u32,
    reserved: [u16; 7],
    NumMemTypes: u16,
    MemDesc: *const MfxEncoderMemDesc,
}

#[repr(C)]
#[derive(Debug)]
struct MfxEncoderCodec {
    CodecID: u32,
    MaxcodecLevel: u16,
    BiDirectionalPrediction: u16,
    EncExtDesc: *const MfxEncExtDescription,
    reserved: [u16; 3],
    NumProfiles: u16,
    Profiles: *const MfxEncoderProfile,
}

#[repr(C)]
#[derive(Debug)]
struct MfxEncoderDescription {
    Version: MfxStructVersion,
    reserved: [u16; 7],
    NumCodecs: u16,
    Codecs: *const MfxEncoderCodec,
}

#[repr(C)]
#[derive(Debug)]
struct MfxDecoderDescription {
    Version: MfxStructVersion,
    reserved: [u16; 7],
    NumCodecs: u16,
    Codecs: *const c_void,
}

#[repr(C)]
#[derive(Debug)]
struct MfxVppDescription {
    Version: MfxStructVersion,
    reserved: [u16; 7],
    NumFilters: u16,
    Filters: *const c_void,
}

#[repr(C)]
#[derive(Debug)]
struct MfxDeviceDescription {
    Version: MfxStructVersion,
    reserved: [u16; 6],
    MediaAdapterType: u16,
    DeviceID: [c_char; 128],
    NumSubDevices: u16,
    SubDevices: *const c_void,
}

#[repr(C)]
#[derive(Debug)]
struct MfxAccelerationModeDescription {
    Version: MfxStructVersion,
    reserved: [u16; 2],
    NumAccelerationModes: u16,
    Mode: *const u32,
}

#[repr(C)]
#[derive(Debug)]
struct MfxPoolPolicyDescription {
    Version: MfxStructVersion,
    reserved: [u16; 2],
    NumPoolPolicies: u16,
    Policy: *const u32,
}

#[repr(C)]
#[derive(Debug)]
struct MfxImplDescription {
    Version: MfxStructVersion,
    Impl: u32,
    AccelerationMode: u32,
    ApiVersion: MfxVersion,
    ImplName: [c_char; 32],
    License: [c_char; 128],
    Keywords: [c_char; 128],
    VendorID: u32,
    VendorImplID: u32,
    Dev: MfxDeviceDescription,
    Dec: MfxDecoderDescription,
    Enc: MfxEncoderDescription,
    VPP: MfxVppDescription,
    AccelerationModeDescription: MfxAccelerationModeDescription,
    PoolPolicies: MfxPoolPolicyDescription,
    reserved: [u32; 8],
    NumExtParam: u32,
    ExtParam: *const c_void,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn fourcc_roundtrip() {
        assert_eq!(fourcc_to_string(MFX_FOURCC_NV12), "NV12");
        assert_eq!(fourcc_to_string(MFX_FOURCC_P010), "P010");
        assert_eq!(fourcc_to_string(MFX_FOURCC_YUY2), "YUY2");
        assert_eq!(fourcc_to_string(MFX_FOURCC_Y210), "Y210");
        assert_eq!(fourcc_to_string(MFX_FOURCC_P210), "P210");
        assert_eq!(fourcc_to_string(MFX_FOURCC_AYUV), "AYUV");
        assert_eq!(fourcc_to_string(MFX_FOURCC_Y410), "Y410");
        assert_eq!(fourcc_to_string(MFX_FOURCC_RGB4), "RGB4");
    }

    #[test]
    fn struct_version_matches_header_macro() {
        assert_eq!(struct_version(1, 2), 258);
    }

    #[test]
    fn ffi_struct_sizes_match_onevpl_headers() {
        assert_eq!(std::mem::size_of::<MfxFrameInfo>(), 68);
        assert_eq!(std::mem::size_of::<MfxInfoMFX>(), 136);
        assert_eq!(std::mem::size_of::<MfxVideoParam>(), 208);
    }

    #[test]
    fn dll_candidates_include_env_first() {
        assert!(candidate_dlls().iter().any(|p| p.ends_with("libvpl-2.dll")));
    }
}
