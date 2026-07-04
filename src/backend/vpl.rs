#![allow(non_snake_case, dead_code, unsafe_op_in_unsafe_fn)]
//! oneVPL 动态 FFI 能力探测。
//!
//! 不依赖封装 crate，运行时尝试加载系统/oneAPI/MSYS2 中的 libvpl。探测只读取
//! dispatcher 暴露的 `mfxImplDescription`，不启动同步编码循环。真正编码阶段仍需
//! `MFXVideoENCODE_Query/Init` 与 shared D3D11 surface import 再次验证。

use crate::config::ChromaSampling;
use crate::error::BackendError;
use crate::rate_control::RateControlMethod;
use libloading::Library;
use serde::{Deserialize, Serialize};
use std::collections::BTreeSet;
use std::collections::VecDeque;
use std::ffi::{c_char, c_void};
use std::path::{Path, PathBuf};
use std::ptr;

const MFX_ERR_NONE: i32 = 0;
const MFX_ERR_NOT_FOUND: i32 = -9;
const MFX_ERR_MORE_DATA: i32 = -10;
const MFX_ERR_MORE_SURFACE: i32 = -11;
const MFX_ERR_NOT_IMPLEMENTED: i32 = -24;
const MFX_WRN_IN_EXECUTION: i32 = 1;
const MFX_WRN_DEVICE_BUSY: i32 = 2;
const MFX_WRN_PARTIAL_ACCELERATION: i32 = 4;
const MFX_IMPLCAPS_IMPLDESCSTRUCTURE: u32 = 1;
const MFX_IMPL_TYPE_HARDWARE: u32 = 0x0002;
const MFX_ACCEL_MODE_VIA_D3D11: u32 = 0x0300;
const MFX_RESOURCE_DX11_TEXTURE: u32 = 5;
const MFX_IOPATTERN_IN_VIDEO_MEMORY: u16 = 0x01;
const MFX_PICSTRUCT_PROGRESSIVE: u16 = 0x01;
const MFX_HANDLE_D3D11_DEVICE: u32 = 3;
const MFX_HANDLE_MEMORY_INTERFACE: u32 = 1001;
const MFX_VARIANT_VERSION: u16 = struct_version(1, 1);
const MFX_VARIANT_TYPE_U32: u32 = 5;
const MFX_VARIANT_TYPE_PTR: u32 = 11;
const MFX_IMPL_HARDWARE_ANY: u32 = 0x0004;
const MFX_IMPL_VIA_D3D11: u32 = 0x0300;
const MFX_SURFACE_TYPE_D3D11_TEX2D: u32 = 2;
const MFX_SURFACE_FLAG_IMPORT_SHARED: u32 = 0x0010;
const MFX_SURFACE_COMPONENT_ENCODE: u32 = 1;
const MFX_SURFACEINTERFACE_VERSION: u16 = struct_version(1, 0);
const VIDEO_CLOCK_HZ: u64 = 90_000;
const VPL_RECORD_ASYNC_DEPTH: u16 = 16;
const VPL_RECORD_MAX_IN_FLIGHT: usize = 64;
const VPL_BITSTREAM_BYTES: usize = 64 * 1024 * 1024;
const MFX_CODINGOPTION_ON: u16 = 0x10;

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
    mfx_init: Option<unsafe extern "C" fn(u32, *mut MfxVersion, *mut MfxSession) -> i32>,
    mfx_create_config: unsafe extern "C" fn(MfxLoader) -> MfxConfig,
    mfx_set_config_filter_property: unsafe extern "C" fn(MfxConfig, *const u8, MfxVariant) -> i32,
    mfx_enum_implementations: unsafe extern "C" fn(MfxLoader, u32, u32, *mut MfxHDL) -> i32,
    mfx_release_impl_description: unsafe extern "C" fn(MfxLoader, MfxHDL) -> i32,
    mfx_create_session: unsafe extern "C" fn(MfxLoader, u32, *mut MfxSession) -> i32,
    mfx_close: unsafe extern "C" fn(MfxSession) -> i32,
    mfx_video_encode_query:
        unsafe extern "C" fn(MfxSession, *mut MfxVideoParam, *mut MfxVideoParam) -> i32,
    mfx_video_core_set_handle: unsafe extern "C" fn(MfxSession, u32, MfxHDL) -> i32,
    mfx_video_core_get_handle: Option<unsafe extern "C" fn(MfxSession, u32, *mut MfxHDL) -> i32>,
    mfx_video_encode_query_iosurf:
        unsafe extern "C" fn(MfxSession, *mut MfxVideoParam, *mut MfxFrameAllocRequest) -> i32,
    mfx_video_encode_init: unsafe extern "C" fn(MfxSession, *mut MfxVideoParam) -> i32,
    mfx_memory_get_surface_for_encode:
        unsafe extern "C" fn(MfxSession, *mut *mut MfxFrameSurface1) -> i32,
    mfx_video_encode_frame_async: unsafe extern "C" fn(
        MfxSession,
        *mut c_void,
        *mut MfxFrameSurface1,
        *mut MfxBitstream,
        *mut MfxSyncPoint,
    ) -> i32,
    mfx_video_core_sync_operation: unsafe extern "C" fn(MfxSession, MfxSyncPoint, u32) -> i32,
    mfx_video_encode_close: unsafe extern "C" fn(MfxSession) -> i32,
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
                        let mfx_init = library
                            .get::<unsafe extern "C" fn(
                                u32,
                                *mut MfxVersion,
                                *mut MfxSession,
                            ) -> i32>(b"MFXInit\0")
                            .ok()
                            .map(|symbol| *symbol);
                        let mfx_create_config = *library
                            .get::<unsafe extern "C" fn(MfxLoader) -> MfxConfig>(
                                b"MFXCreateConfig\0",
                            )
                            .map_err(|e| e.to_string())?;
                        let mfx_set_config_filter_property = *library
                            .get::<unsafe extern "C" fn(MfxConfig, *const u8, MfxVariant) -> i32>(
                                b"MFXSetConfigFilterProperty\0",
                            )
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
                        let mfx_video_core_set_handle = *library
                            .get::<unsafe extern "C" fn(MfxSession, u32, MfxHDL) -> i32>(
                                b"MFXVideoCORE_SetHandle\0",
                            )
                            .map_err(|e| e.to_string())?;
                        let mfx_video_core_get_handle = library
                            .get::<unsafe extern "C" fn(MfxSession, u32, *mut MfxHDL) -> i32>(
                                b"MFXVideoCORE_GetHandle\0",
                            )
                            .ok()
                            .map(|symbol| *symbol);
                        let mfx_video_encode_query_iosurf = *library
                            .get::<unsafe extern "C" fn(
                                MfxSession,
                                *mut MfxVideoParam,
                                *mut MfxFrameAllocRequest,
                            ) -> i32>(b"MFXVideoENCODE_QueryIOSurf\0")
                            .map_err(|e| e.to_string())?;
                        let mfx_video_encode_init = *library
                            .get::<unsafe extern "C" fn(MfxSession, *mut MfxVideoParam) -> i32>(
                                b"MFXVideoENCODE_Init\0",
                            )
                            .map_err(|e| e.to_string())?;
                        let mfx_memory_get_surface_for_encode = *library
                            .get::<unsafe extern "C" fn(
                                MfxSession,
                                *mut *mut MfxFrameSurface1,
                            ) -> i32>(b"MFXMemory_GetSurfaceForEncode\0")
                            .map_err(|e| e.to_string())?;
                        let mfx_video_encode_frame_async = *library
                            .get::<unsafe extern "C" fn(
                                MfxSession,
                                *mut c_void,
                                *mut MfxFrameSurface1,
                                *mut MfxBitstream,
                                *mut MfxSyncPoint,
                            ) -> i32>(
                                b"MFXVideoENCODE_EncodeFrameAsync\0"
                            )
                            .map_err(|e| e.to_string())?;
                        let mfx_video_core_sync_operation = *library
                            .get::<unsafe extern "C" fn(MfxSession, MfxSyncPoint, u32) -> i32>(
                                b"MFXVideoCORE_SyncOperation\0",
                            )
                            .map_err(|e| e.to_string())?;
                        let mfx_video_encode_close = *library
                            .get::<unsafe extern "C" fn(MfxSession) -> i32>(
                                b"MFXVideoENCODE_Close\0",
                            )
                            .map_err(|e| e.to_string())?;
                        Self {
                            _library: library,
                            mfx_load,
                            mfx_unload,
                            mfx_init,
                            mfx_create_config,
                            mfx_set_config_filter_property,
                            mfx_enum_implementations,
                            mfx_release_impl_description,
                            mfx_create_session,
                            mfx_close,
                            mfx_video_encode_query,
                            mfx_video_core_set_handle,
                            mfx_video_core_get_handle,
                            mfx_video_encode_query_iosurf,
                            mfx_video_encode_init,
                            mfx_memory_get_surface_for_encode,
                            mfx_video_encode_frame_async,
                            mfx_video_core_sync_operation,
                            mfx_video_encode_close,
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

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct VplD3d11EncodeInitSmoke {
    pub adapter_index: u32,
    pub adapter_luid: String,
    pub dll_path: String,
    pub implementation_index: u32,
    pub session_mode: String,
    pub legacy_init_status: Option<i32>,
    pub cfg_impl_status: i32,
    pub cfg_accel_status: i32,
    pub cfg_handle_type_status: i32,
    pub cfg_handle_status: i32,
    pub set_handle_status: i32,
    pub query_status: i32,
    pub query_iosurf_status: i32,
    pub init_status: i32,
    pub close_status: i32,
    pub width: u16,
    pub height: u16,
    pub fourcc: String,
    pub num_frame_min: u16,
    pub num_frame_suggested: u16,
    pub request_type: u16,
    pub get_surface_status: i32,
    pub native_handle_status: i32,
    pub native_resource_type: u32,
    pub native_texture_width: u32,
    pub native_texture_height: u32,
    pub native_texture_format: u32,
    pub device_handle_status: i32,
    pub device_handle_type: u32,
    pub gpu_copy_status: String,
    pub memory_get_interface_status: i32,
    pub import_shared_status: i32,
    pub import_shared_actual_flags: u32,
    pub import_shared_encode_status: i32,
    pub import_shared_sync_status: i32,
    pub import_shared_bytes: u32,
    pub import_shared_release_status: i32,
    pub external_same_device_encode_status: i32,
    pub external_same_device_sync_status: i32,
    pub external_same_device_bytes: u32,
    pub onevpl_surface_encode_status: i32,
    pub onevpl_surface_sync_status: i32,
    pub onevpl_surface_bytes: u32,
    pub surface_release_status: i32,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct VplOneCopyRecordReport {
    pub adapter_index: u32,
    pub adapter_luid: String,
    pub output_path: String,
    pub width: u16,
    pub height: u16,
    pub duration_seconds: f32,
    pub captured_frames: u32,
    pub encoded_samples: u32,
    pub encoded_bytes: u64,
    pub dda_timeouts: u32,
    pub input_dxgi_format: u32,
    pub target_dxgi_format: u32,
    pub query_status: i32,
    pub init_status: i32,
    pub close_status: i32,
    pub first_get_surface_status: i32,
    pub video_processor_format_flags_in: u32,
    pub video_processor_format_flags_out: u32,
    pub notes: Vec<String>,
}

#[cfg(windows)]
pub fn run_d3d11_encode_init_smoke(
    adapter_index: u32,
) -> Result<VplD3d11EncodeInitSmoke, BackendError> {
    use windows::Win32::Foundation::HMODULE;
    use windows::Win32::Graphics::Direct3D::{
        D3D_DRIVER_TYPE_UNKNOWN, D3D_FEATURE_LEVEL, D3D_FEATURE_LEVEL_11_0, D3D_FEATURE_LEVEL_11_1,
    };
    use windows::Win32::Graphics::Direct3D11::{
        D3D11_CREATE_DEVICE_BGRA_SUPPORT, D3D11_CREATE_DEVICE_VIDEO_SUPPORT, D3D11_SDK_VERSION,
        D3D11_TEXTURE2D_DESC, D3D11CreateDevice, ID3D11Device, ID3D11Resource, ID3D11Texture2D,
    };
    use windows::Win32::Graphics::Dxgi::{CreateDXGIFactory1, IDXGIAdapter, IDXGIFactory1};
    use windows::core::Interface;

    let (api, dll_path) =
        VplApi::load().map_err(|err| BackendError::unsupported("oneVPL", "DLL", err))?;

    unsafe {
        let factory: IDXGIFactory1 =
            CreateDXGIFactory1().map_err(|err| BackendError::WindowsApi {
                func: "CreateDXGIFactory1",
                message: err.to_string(),
            })?;
        let adapter1 =
            factory
                .EnumAdapters1(adapter_index)
                .map_err(|err| BackendError::WindowsApi {
                    func: "IDXGIFactory1::EnumAdapters1",
                    message: err.to_string(),
                })?;
        let desc = adapter1
            .GetDesc1()
            .map_err(|err| BackendError::WindowsApi {
                func: "IDXGIAdapter1::GetDesc1",
                message: err.to_string(),
            })?;
        let adapter_luid = format!(
            "{:08X}:{:08X}",
            desc.AdapterLuid.HighPart as u32, desc.AdapterLuid.LowPart
        );
        let adapter: IDXGIAdapter = adapter1.cast().map_err(|err| BackendError::WindowsApi {
            func: "IDXGIAdapter1::cast",
            message: err.to_string(),
        })?;

        let mut device: Option<ID3D11Device> = None;
        let mut feature_level = D3D_FEATURE_LEVEL(0);
        let levels = [D3D_FEATURE_LEVEL_11_1, D3D_FEATURE_LEVEL_11_0];
        D3D11CreateDevice(
            Some(&adapter),
            D3D_DRIVER_TYPE_UNKNOWN,
            HMODULE::default(),
            D3D11_CREATE_DEVICE_BGRA_SUPPORT | D3D11_CREATE_DEVICE_VIDEO_SUPPORT,
            Some(&levels),
            D3D11_SDK_VERSION,
            Some(&mut device),
            Some(&mut feature_level),
            None,
        )
        .map_err(|err| BackendError::WindowsApi {
            func: "D3D11CreateDevice",
            message: err.to_string(),
        })?;
        let device = device.ok_or_else(|| BackendError::WindowsApi {
            func: "D3D11CreateDevice",
            message: "返回空 ID3D11Device".to_owned(),
        })?;

        let loader = (api.mfx_load)();
        if loader.is_null() {
            return Err(BackendError::unsupported(
                "oneVPL",
                "MFXLoad",
                "返回空 loader",
            ));
        }
        let raw_device = windows::core::Interface::as_raw(&device) as MfxHDL;
        // 注意：测试机上的 mfx-gen 对 loader 级 mfxHandleType/mfxHDL 过滤返回
        // MFX_ERR_UNDEFINED_BEHAVIOR，故这里先不启用；保留字段用于日志暴露。
        let cfg_impl_status = i32::MIN;
        let cfg_accel_status = i32::MIN;
        let cfg_handle_type_status = i32::MIN;
        let cfg_handle_status = i32::MIN;
        let implementation_index = 0;

        // 在独立短会话里测试“把应用自己的 D3D11 device 交给 oneVPL”。
        // 该路径在测试机 mfx-gen 上返回 MFX_ERR_UNDEFINED_BEHAVIOR；不要让这个
        // 失败状态污染后续 oneVPL 内部分配 surface 的生产路径 smoke。
        let set_handle_status = {
            let mut handle_session: MfxSession = ptr::null_mut();
            let create_status =
                (api.mfx_create_session)(loader, implementation_index, &mut handle_session);
            if create_status == MFX_ERR_NONE && !handle_session.is_null() {
                let status = (api.mfx_video_core_set_handle)(
                    handle_session,
                    MFX_HANDLE_D3D11_DEVICE,
                    raw_device,
                );
                let _ = (api.mfx_close)(handle_session);
                status
            } else {
                create_status
            }
        };

        let mut session: MfxSession = ptr::null_mut();
        let mut session_mode = "loader".to_owned();
        let mut legacy_init_status = None;
        if let Some(mfx_init) = api.mfx_init {
            let mut version = MfxVersion { version: (2 << 16) };
            let status = mfx_init(
                MFX_IMPL_HARDWARE_ANY | MFX_IMPL_VIA_D3D11,
                &mut version,
                &mut session,
            );
            legacy_init_status = Some(status);
            if status == MFX_ERR_NONE && !session.is_null() {
                session_mode = "legacy MFXInit(HARDWARE_ANY|D3D11)".to_owned();
            } else {
                session = ptr::null_mut();
            }
        }
        if session.is_null() {
            let create_status =
                (api.mfx_create_session)(loader, implementation_index, &mut session);
            if create_status != MFX_ERR_NONE || session.is_null() {
                (api.mfx_unload)(loader);
                return Err(BackendError::VplStatus {
                    func: "MFXCreateSession",
                    status: create_status,
                });
            }
        }

        let mut param = make_query_param(
            RateControlMethod::Cbr,
            MFX_FOURCC_P010,
            1,
            10,
            MFX_PROFILE_HEVC_MAIN10 as u16,
        );
        param.mfx.FrameInfo.Width = 3840;
        param.mfx.FrameInfo.Height = 2160;
        param.mfx.FrameInfo.CropW = 3840;
        param.mfx.FrameInfo.CropH = 2160;
        param.mfx.TargetKbps = 20_000;
        param.mfx.BufferSizeInKB = 40_000;
        param.mfx.GopRefDist = 1;
        param.AsyncDepth = VPL_RECORD_ASYNC_DEPTH;

        let mut queried_param = param;
        let query_status = (api.mfx_video_encode_query)(session, &mut param, &mut queried_param);
        if query_status >= MFX_ERR_NONE {
            param = queried_param;
        }

        let mut request: MfxFrameAllocRequest = std::mem::zeroed();
        let query_iosurf_status =
            (api.mfx_video_encode_query_iosurf)(session, &mut param, &mut request);
        let init_status = (api.mfx_video_encode_init)(session, &mut param);
        let mut get_surface_status = i32::MIN;
        let mut native_handle_status = i32::MIN;
        let mut native_resource_type = 0;
        let mut native_texture_width = 0;
        let mut native_texture_height = 0;
        let mut native_texture_format = 0;
        let mut device_handle_status = i32::MIN;
        let mut device_handle_type = 0;
        let mut gpu_copy_status = "not-run".to_owned();
        let mut memory_get_interface_status = i32::MIN;
        let mut import_shared_status = i32::MIN;
        let mut import_shared_actual_flags = 0;
        let mut import_shared_encode_status = i32::MIN;
        let mut import_shared_sync_status = i32::MIN;
        let mut import_shared_bytes = 0;
        let mut import_shared_release_status = i32::MIN;
        let mut external_same_device_encode_status = i32::MIN;
        let mut external_same_device_sync_status = i32::MIN;
        let mut external_same_device_bytes = 0;
        let mut onevpl_surface_encode_status = i32::MIN;
        let mut onevpl_surface_sync_status = i32::MIN;
        let mut onevpl_surface_bytes = 0;
        let mut surface_release_status = i32::MIN;
        let mut onevpl_surface_encoded = false;

        if init_status >= MFX_ERR_NONE {
            let mut surface: *mut MfxFrameSurface1 = ptr::null_mut();
            get_surface_status = (api.mfx_memory_get_surface_for_encode)(session, &mut surface);
            if get_surface_status == MFX_ERR_NONE && !surface.is_null() {
                let frame_interface = (*surface).FrameInterface;
                if !frame_interface.is_null() {
                    let mut native_resource: MfxHDL = ptr::null_mut();
                    native_handle_status = ((*frame_interface).GetNativeHandle)(
                        surface,
                        &mut native_resource,
                        &mut native_resource_type,
                    );
                    let mut native_device: MfxHDL = ptr::null_mut();
                    device_handle_status = ((*frame_interface).GetDeviceHandle)(
                        surface,
                        &mut native_device,
                        &mut device_handle_type,
                    );

                    if native_handle_status == MFX_ERR_NONE
                        && native_resource_type == MFX_RESOURCE_DX11_TEXTURE
                        && !native_resource.is_null()
                    {
                        let Some(target_texture) =
                            <ID3D11Texture2D as Interface>::from_raw_borrowed(&native_resource)
                        else {
                            gpu_copy_status =
                                "GetNativeHandle 返回的不是 ID3D11Texture2D".to_owned();
                            surface_release_status = ((*frame_interface).Release)(surface);
                            let close_status = (api.mfx_video_encode_close)(session);
                            let mfx_close_status = (api.mfx_close)(session);
                            (api.mfx_unload)(loader);
                            if mfx_close_status != MFX_ERR_NONE {
                                return Err(BackendError::VplStatus {
                                    func: "MFXClose",
                                    status: mfx_close_status,
                                });
                            }
                            return Ok(VplD3d11EncodeInitSmoke {
                                adapter_index,
                                adapter_luid,
                                dll_path: dll_path.display().to_string(),
                                implementation_index,
                                session_mode,
                                legacy_init_status,
                                cfg_impl_status,
                                cfg_accel_status,
                                cfg_handle_type_status,
                                cfg_handle_status,
                                set_handle_status,
                                query_status,
                                query_iosurf_status,
                                init_status,
                                close_status,
                                width: param.mfx.FrameInfo.Width,
                                height: param.mfx.FrameInfo.Height,
                                fourcc: fourcc_to_string(param.mfx.FrameInfo.FourCC),
                                num_frame_min: request.NumFrameMin,
                                num_frame_suggested: request.NumFrameSuggested,
                                request_type: request.Type,
                                get_surface_status,
                                native_handle_status,
                                native_resource_type,
                                native_texture_width,
                                native_texture_height,
                                native_texture_format,
                                device_handle_status,
                                device_handle_type,
                                gpu_copy_status,
                                memory_get_interface_status,
                                import_shared_status,
                                import_shared_actual_flags,
                                import_shared_encode_status,
                                import_shared_sync_status,
                                import_shared_bytes,
                                import_shared_release_status,
                                external_same_device_encode_status,
                                external_same_device_sync_status,
                                external_same_device_bytes,
                                onevpl_surface_encode_status,
                                onevpl_surface_sync_status,
                                onevpl_surface_bytes,
                                surface_release_status,
                            });
                        };
                        let mut texture_desc = D3D11_TEXTURE2D_DESC::default();
                        target_texture.GetDesc(&mut texture_desc);
                        native_texture_width = texture_desc.Width;
                        native_texture_height = texture_desc.Height;
                        native_texture_format = texture_desc.Format.0 as u32;

                        if device_handle_status == MFX_ERR_NONE
                            && device_handle_type == MFX_HANDLE_D3D11_DEVICE
                            && !native_device.is_null()
                        {
                            let mut memory_iface_handle: MfxHDL = ptr::null_mut();
                            memory_get_interface_status = if let Some(mfx_video_core_get_handle) =
                                api.mfx_video_core_get_handle
                            {
                                mfx_video_core_get_handle(
                                    session,
                                    MFX_HANDLE_MEMORY_INTERFACE,
                                    &mut memory_iface_handle,
                                )
                            } else {
                                MFX_ERR_NOT_IMPLEMENTED
                            };
                            if let Some(surface_device) =
                                <ID3D11Device as Interface>::from_raw_borrowed(&native_device)
                            {
                                let mut src_desc = texture_desc;
                                src_desc.BindFlags = Default::default();
                                src_desc.MiscFlags = Default::default();
                                let mut src_texture: Option<ID3D11Texture2D> = None;
                                match surface_device.CreateTexture2D(
                                    &src_desc,
                                    None,
                                    Some(&mut src_texture),
                                ) {
                                    Ok(()) => {
                                        if let Some(src_texture) = src_texture {
                                            match surface_device.GetImmediateContext() {
                                                Ok(context) => {
                                                    let dst_resource =
                                                        target_texture.cast::<ID3D11Resource>();
                                                    let src_resource =
                                                        src_texture.cast::<ID3D11Resource>();
                                                    match (dst_resource, src_resource) {
                                                        (Ok(dst_resource), Ok(src_resource)) => {
                                                            context.CopyResource(
                                                                Some(&dst_resource),
                                                                Some(&src_resource),
                                                            );
                                                            context.Flush();
                                                            gpu_copy_status = "CopyResource(P010 -> oneVPL surface) submitted".to_owned();

                                                            (*surface).Data.TimeStamp = 90_000;
                                                            (*surface).Data.FrameOrder = 1;
                                                            let copied = encode_one_surface(
                                                                &api, session, surface,
                                                            );
                                                            onevpl_surface_encode_status = copied.0;
                                                            onevpl_surface_sync_status = copied.1;
                                                            onevpl_surface_bytes = copied.2;
                                                            onevpl_surface_encoded = true;
                                                        }
                                                        (Err(dst_err), Ok(_)) => {
                                                            gpu_copy_status = format!(
                                                                "target texture cast ID3D11Resource 失败: {dst_err}"
                                                            );
                                                        }
                                                        (Ok(_), Err(src_err)) => {
                                                            gpu_copy_status = format!(
                                                                "source texture cast ID3D11Resource 失败: {src_err}"
                                                            );
                                                        }
                                                        (Err(dst_err), Err(src_err)) => {
                                                            gpu_copy_status = format!(
                                                                "texture cast ID3D11Resource 均失败: dst={dst_err}; src={src_err}"
                                                            );
                                                        }
                                                    }

                                                    if memory_get_interface_status == MFX_ERR_NONE
                                                        && !memory_iface_handle.is_null()
                                                    {
                                                        let imported = import_d3d11_texture_shared(
                                                            memory_iface_handle
                                                                as *mut MfxMemoryInterface,
                                                            Interface::as_raw(&src_texture)
                                                                as MfxHDL,
                                                        );
                                                        import_shared_status = imported.0;
                                                        import_shared_actual_flags = imported.1;
                                                        if !imported.2.is_null() {
                                                            (*imported.2).Data.TimeStamp = 90_000;
                                                            (*imported.2).Data.FrameOrder = 2;
                                                            let encoded = encode_one_surface(
                                                                &api, session, imported.2,
                                                            );
                                                            import_shared_encode_status = encoded.0;
                                                            import_shared_sync_status = encoded.1;
                                                            import_shared_bytes = encoded.2;
                                                            if !(*imported.2)
                                                                .FrameInterface
                                                                .is_null()
                                                            {
                                                                import_shared_release_status =
                                                                    ((*(*imported.2)
                                                                        .FrameInterface)
                                                                        .Release)(
                                                                        imported.2
                                                                    );
                                                            }
                                                        }
                                                    }

                                                    let mut pair = MfxHDLPair {
                                                        first: Interface::as_raw(&src_texture)
                                                            as MfxHDL,
                                                        second: ptr::null_mut(),
                                                    };
                                                    let mut external_surface =
                                                        make_external_surface(
                                                            &param.mfx.FrameInfo,
                                                            &mut pair,
                                                            request.Type,
                                                            90_000,
                                                        );
                                                    let direct = encode_one_surface(
                                                        &api,
                                                        session,
                                                        &mut external_surface,
                                                    );
                                                    external_same_device_encode_status = direct.0;
                                                    external_same_device_sync_status = direct.1;
                                                    external_same_device_bytes = direct.2;
                                                }
                                                Err(err) => {
                                                    gpu_copy_status =
                                                        format!("GetImmediateContext 失败: {err}");
                                                }
                                            }
                                        } else {
                                            gpu_copy_status =
                                                "CreateTexture2D 返回空源纹理".to_owned();
                                        }
                                    }
                                    Err(err) => {
                                        gpu_copy_status =
                                            format!("CreateTexture2D 源纹理失败: {err}");
                                    }
                                }
                            } else {
                                gpu_copy_status =
                                    "GetDeviceHandle 返回的不是 ID3D11Device".to_owned();
                            }
                        }
                    }

                    if !onevpl_surface_encoded {
                        (*surface).Data.TimeStamp = 90_000;
                        (*surface).Data.FrameOrder = 1;
                        let copied = encode_one_surface(&api, session, surface);
                        onevpl_surface_encode_status = copied.0;
                        onevpl_surface_sync_status = copied.1;
                        onevpl_surface_bytes = copied.2;
                    }
                    surface_release_status = ((*frame_interface).Release)(surface);
                }
            }
        }
        let close_status = if init_status >= MFX_ERR_NONE {
            (api.mfx_video_encode_close)(session)
        } else {
            MFX_ERR_NONE
        };
        let mfx_close_status = (api.mfx_close)(session);
        (api.mfx_unload)(loader);
        if mfx_close_status != MFX_ERR_NONE {
            return Err(BackendError::VplStatus {
                func: "MFXClose",
                status: mfx_close_status,
            });
        }

        Ok(VplD3d11EncodeInitSmoke {
            adapter_index,
            adapter_luid,
            dll_path: dll_path.display().to_string(),
            implementation_index,
            session_mode,
            legacy_init_status,
            cfg_impl_status,
            cfg_accel_status,
            cfg_handle_type_status,
            cfg_handle_status,
            set_handle_status,
            query_status,
            query_iosurf_status,
            init_status,
            close_status,
            width: param.mfx.FrameInfo.Width,
            height: param.mfx.FrameInfo.Height,
            fourcc: fourcc_to_string(param.mfx.FrameInfo.FourCC),
            num_frame_min: request.NumFrameMin,
            num_frame_suggested: request.NumFrameSuggested,
            request_type: request.Type,
            get_surface_status,
            native_handle_status,
            native_resource_type,
            native_texture_width,
            native_texture_height,
            native_texture_format,
            device_handle_status,
            device_handle_type,
            gpu_copy_status,
            memory_get_interface_status,
            import_shared_status,
            import_shared_actual_flags,
            import_shared_encode_status,
            import_shared_sync_status,
            import_shared_bytes,
            import_shared_release_status,
            external_same_device_encode_status,
            external_same_device_sync_status,
            external_same_device_bytes,
            onevpl_surface_encode_status,
            onevpl_surface_sync_status,
            onevpl_surface_bytes,
            surface_release_status,
        })
    }
}

#[cfg(not(windows))]
pub fn run_d3d11_encode_init_smoke(
    adapter_index: u32,
) -> Result<VplD3d11EncodeInitSmoke, BackendError> {
    let _ = adapter_index;
    Err(BackendError::unsupported(
        "oneVPL D3D11 encode init",
        "Windows D3D11",
        "仅 Windows 可用",
    ))
}

#[cfg(windows)]
pub fn record_d3d11_onecopy_mp4(
    adapter_index: u32,
    output: &Path,
    duration_seconds: f32,
    rate_control: RateControlMethod,
) -> Result<VplOneCopyRecordReport, BackendError> {
    use crate::backend::mp4_mux::{HevcMp4Track, write_hevc_mp4};
    use std::time::{Duration, Instant};
    use windows::Win32::Graphics::Direct3D11::{
        D3D11_TEXTURE2D_DESC, D3D11_VIDEO_FRAME_FORMAT_PROGRESSIVE,
        D3D11_VIDEO_PROCESSOR_CONTENT_DESC, D3D11_VIDEO_PROCESSOR_FORMAT_SUPPORT_INPUT,
        D3D11_VIDEO_PROCESSOR_FORMAT_SUPPORT_OUTPUT, D3D11_VIDEO_USAGE_PLAYBACK_NORMAL,
        ID3D11Device, ID3D11DeviceContext, ID3D11Multithread, ID3D11Texture2D, ID3D11VideoContext,
        ID3D11VideoDevice, ID3D11VideoProcessor, ID3D11VideoProcessorEnumerator,
    };
    use windows::Win32::Graphics::Dxgi::Common::{
        DXGI_FORMAT_B8G8R8A8_UNORM, DXGI_FORMAT_P010, DXGI_FORMAT_R8G8B8A8_UNORM,
        DXGI_FORMAT_R16G16B16A16_FLOAT, DXGI_RATIONAL,
    };
    use windows::Win32::Graphics::Dxgi::{
        CreateDXGIFactory1, DXGI_ERROR_WAIT_TIMEOUT, DXGI_OUTDUPL_FRAME_INFO, IDXGIFactory1,
        IDXGIResource,
    };
    use windows::core::Interface;

    let (api, dll_path) =
        VplApi::load().map_err(|err| BackendError::unsupported("oneVPL", "DLL", err))?;
    let mut notes = vec![format!("oneVPL DLL: {}", dll_path.display())];

    unsafe {
        let _thread_priority = RecordThreadPriorityGuard::raise(&mut notes);
        let factory: IDXGIFactory1 =
            CreateDXGIFactory1().map_err(|err| BackendError::WindowsApi {
                func: "CreateDXGIFactory1",
                message: err.to_string(),
            })?;
        let adapter1 =
            factory
                .EnumAdapters1(adapter_index)
                .map_err(|err| BackendError::WindowsApi {
                    func: "IDXGIFactory1::EnumAdapters1",
                    message: err.to_string(),
                })?;
        let desc = adapter1
            .GetDesc1()
            .map_err(|err| BackendError::WindowsApi {
                func: "IDXGIAdapter1::GetDesc1",
                message: err.to_string(),
            })?;
        let adapter_luid = format!(
            "{:08X}:{:08X}",
            desc.AdapterLuid.HighPart as u32, desc.AdapterLuid.LowPart
        );
        let output0 = adapter1
            .EnumOutputs(0)
            .map_err(|err| BackendError::WindowsApi {
                func: "IDXGIAdapter1::EnumOutputs(0)",
                message: err.to_string(),
            })?;
        let output_desc = output0.GetDesc().map_err(|err| BackendError::WindowsApi {
            func: "IDXGIOutput::GetDesc",
            message: err.to_string(),
        })?;
        let capture_width = (output_desc.DesktopCoordinates.right
            - output_desc.DesktopCoordinates.left)
            .max(1) as u16;
        let capture_height = (output_desc.DesktopCoordinates.bottom
            - output_desc.DesktopCoordinates.top)
            .max(1) as u16;
        let aligned_width = align16(capture_width);
        let aligned_height = align16(capture_height);

        let loader = (api.mfx_load)();
        if loader.is_null() {
            return Err(BackendError::unsupported(
                "oneVPL record",
                "MFXLoad",
                "返回空 loader",
            ));
        }
        let mut session: MfxSession = ptr::null_mut();
        let create_status = (api.mfx_create_session)(loader, 0, &mut session);
        if create_status != MFX_ERR_NONE || session.is_null() {
            (api.mfx_unload)(loader);
            return Err(BackendError::VplStatus {
                func: "MFXCreateSession",
                status: create_status,
            });
        }

        let mut param = make_query_param(
            rate_control,
            MFX_FOURCC_P010,
            1,
            10,
            MFX_PROFILE_HEVC_MAIN10 as u16,
        );
        param.AsyncDepth = VPL_RECORD_ASYNC_DEPTH;
        param.mfx.FrameInfo.Width = aligned_width;
        param.mfx.FrameInfo.Height = aligned_height;
        param.mfx.FrameInfo.CropW = capture_width;
        param.mfx.FrameInfo.CropH = capture_height;
        param.mfx.FrameInfo.FrameRateExtN = 144;
        param.mfx.FrameInfo.FrameRateExtD = 1;
        param.mfx.TargetKbps = 20_000;
        param.mfx.BufferSizeInKB = 40_000;
        param.mfx.GopPicSize = 60;
        param.mfx.GopRefDist = 1;
        param.mfx.IdrInterval = 1;

        let mut queried = param;
        let query_status = (api.mfx_video_encode_query)(session, &mut param, &mut queried);
        if query_status >= MFX_ERR_NONE {
            param = queried;
            param.AsyncDepth = VPL_RECORD_ASYNC_DEPTH;
            param.mfx.FrameInfo.FrameRateExtN = 144;
            param.mfx.FrameInfo.FrameRateExtD = 1;
            param.mfx.FrameInfo.Width = aligned_width;
            param.mfx.FrameInfo.Height = aligned_height;
            param.mfx.FrameInfo.CropW = capture_width;
            param.mfx.FrameInfo.CropH = capture_height;
        }
        let init_status = (api.mfx_video_encode_init)(session, &mut param);
        if init_status < MFX_ERR_NONE {
            let _ = (api.mfx_close)(session);
            (api.mfx_unload)(loader);
            return Err(BackendError::VplStatus {
                func: "MFXVideoENCODE_Init",
                status: init_status,
            });
        }

        let mut first_surface: *mut MfxFrameSurface1 = ptr::null_mut();
        let first_get_surface_status =
            (api.mfx_memory_get_surface_for_encode)(session, &mut first_surface);
        if first_get_surface_status != MFX_ERR_NONE || first_surface.is_null() {
            let _ = (api.mfx_video_encode_close)(session);
            let _ = (api.mfx_close)(session);
            (api.mfx_unload)(loader);
            return Err(BackendError::VplStatus {
                func: "MFXMemory_GetSurfaceForEncode",
                status: first_get_surface_status,
            });
        }

        let first_interface = (*first_surface).FrameInterface;
        if first_interface.is_null() {
            let _ = (api.mfx_video_encode_close)(session);
            let _ = (api.mfx_close)(session);
            (api.mfx_unload)(loader);
            return Err(BackendError::unsupported(
                "oneVPL record",
                "FrameInterface",
                "oneVPL surface 没有 FrameInterface",
            ));
        }
        let mut first_native: MfxHDL = ptr::null_mut();
        let mut first_native_type = 0u32;
        let native_status = ((*first_interface).GetNativeHandle)(
            first_surface,
            &mut first_native,
            &mut first_native_type,
        );
        if native_status != MFX_ERR_NONE || first_native_type != MFX_RESOURCE_DX11_TEXTURE {
            let _ = ((*first_interface).Release)(first_surface);
            let _ = (api.mfx_video_encode_close)(session);
            let _ = (api.mfx_close)(session);
            (api.mfx_unload)(loader);
            return Err(BackendError::VplStatus {
                func: "mfxFrameSurfaceInterface::GetNativeHandle",
                status: native_status,
            });
        }
        let Some(first_target) = <ID3D11Texture2D as Interface>::from_raw_borrowed(&first_native)
        else {
            let _ = ((*first_interface).Release)(first_surface);
            let _ = (api.mfx_video_encode_close)(session);
            let _ = (api.mfx_close)(session);
            (api.mfx_unload)(loader);
            return Err(BackendError::unsupported(
                "oneVPL record",
                "native texture",
                "GetNativeHandle 返回值不是 ID3D11Texture2D",
            ));
        };
        let mut target_desc = D3D11_TEXTURE2D_DESC::default();
        first_target.GetDesc(&mut target_desc);

        let mut device_handle: MfxHDL = ptr::null_mut();
        let mut device_type = 0u32;
        let device_status = ((*first_interface).GetDeviceHandle)(
            first_surface,
            &mut device_handle,
            &mut device_type,
        );
        if device_status != MFX_ERR_NONE || device_type != MFX_HANDLE_D3D11_DEVICE {
            let _ = ((*first_interface).Release)(first_surface);
            let _ = (api.mfx_video_encode_close)(session);
            let _ = (api.mfx_close)(session);
            (api.mfx_unload)(loader);
            return Err(BackendError::VplStatus {
                func: "mfxFrameSurfaceInterface::GetDeviceHandle",
                status: device_status,
            });
        }
        let Some(vpl_device) = <ID3D11Device as Interface>::from_raw_borrowed(&device_handle)
        else {
            let _ = ((*first_interface).Release)(first_surface);
            let _ = (api.mfx_video_encode_close)(session);
            let _ = (api.mfx_close)(session);
            (api.mfx_unload)(loader);
            return Err(BackendError::unsupported(
                "oneVPL record",
                "device handle",
                "GetDeviceHandle 返回值不是 ID3D11Device",
            ));
        };

        let video_device: ID3D11VideoDevice =
            vpl_device.cast().map_err(|err| BackendError::WindowsApi {
                func: "ID3D11Device::cast<ID3D11VideoDevice>",
                message: err.to_string(),
            })?;
        let immediate: ID3D11DeviceContext =
            vpl_device
                .GetImmediateContext()
                .map_err(|err| BackendError::WindowsApi {
                    func: "ID3D11Device::GetImmediateContext",
                    message: err.to_string(),
                })?;
        let video_context: ID3D11VideoContext =
            immediate.cast().map_err(|err| BackendError::WindowsApi {
                func: "ID3D11DeviceContext::cast<ID3D11VideoContext>",
                message: err.to_string(),
            })?;
        let enable_d3d_mt = std::env::var_os("RUST_REPLAY_ENABLE_D3D_MT").is_some();
        let d3d_multithread: Option<ID3D11Multithread> = if enable_d3d_mt {
            immediate.cast().ok()
        } else {
            None
        };
        if let Some(mt) = &d3d_multithread {
            let enabled = mt.SetMultithreadProtected(true);
            notes.push(format!(
                "D3D11 multithread protection enabled for capture/encode split: {}",
                enabled.as_bool()
            ));
        } else if enable_d3d_mt {
            notes.push("D3D11 multithread protection interface unavailable; capture/encode split will use unprotected immediate context".to_owned());
        }
        let p010_intermediate = create_p010_intermediate(vpl_device, &target_desc)?;
        let duplication = create_duplication_on_device(&adapter1, vpl_device)?;

        let mut samples = Vec::new();
        let mut captured_frames = 0u32;
        let mut dda_timeouts = 0u32;
        let mut input_dxgi_format = 0u32;
        let mut video_processor: Option<ID3D11VideoProcessor> = None;
        let mut enumerator: Option<ID3D11VideoProcessorEnumerator> = None;
        let mut rgba_converter: Option<GpuRgbaConverter> = None;
        let mut p010_converter: Option<GpuP010Converter> = None;
        let mut dda_snapshot: Option<ID3D11Texture2D> = None;
        let mut cached_blitter: Option<VideoProcessorBlitter> = None;
        let mut conversion_ready = false;
        let mut format_flags_in = 0u32;
        let mut format_flags_out = 0u32;
        let mut pending_surface = Some(first_surface);
        let mut in_flight: VecDeque<Box<AsyncEncode>> =
            VecDeque::with_capacity(VPL_RECORD_ASYNC_DEPTH as usize);
        let mut bitstream_pool: Vec<Vec<u8>> = Vec::with_capacity(VPL_RECORD_ASYNC_DEPTH as usize);
        for _ in 0..VPL_RECORD_ASYNC_DEPTH {
            bitstream_pool.push(vec![0u8; VPL_BITSTREAM_BYTES + 31]);
        }
        let async_depth = VPL_RECORD_ASYNC_DEPTH as usize;
        let mut perf = RecordPerf::default();
        let start = Instant::now();
        let end_at = start + Duration::from_secs_f32(duration_seconds.max(0.1));

        while Instant::now() < end_at || samples.is_empty() {
            let mut frame_info = DXGI_OUTDUPL_FRAME_INFO::default();
            let mut resource: Option<IDXGIResource> = None;
            let acquire_started = Instant::now();
            match duplication.AcquireNextFrame(100, &mut frame_info, &mut resource) {
                Ok(()) => {
                    perf.acquire.add(acquire_started.elapsed());
                    perf.dda_accumulated_frames_total += frame_info.AccumulatedFrames as u64;
                    perf.dda_accumulated_frames_max = perf
                        .dda_accumulated_frames_max
                        .max(frame_info.AccumulatedFrames);
                }
                Err(err) if err.code() == DXGI_ERROR_WAIT_TIMEOUT => {
                    perf.acquire.add(acquire_started.elapsed());
                    dda_timeouts += 1;
                    if Instant::now() >= end_at && !samples.is_empty() {
                        break;
                    }
                    continue;
                }
                Err(err) => {
                    release_pending_surface(pending_surface.take());
                    let _ = (api.mfx_video_encode_close)(session);
                    let _ = (api.mfx_close)(session);
                    (api.mfx_unload)(loader);
                    return Err(BackendError::WindowsApi {
                        func: "IDXGIOutputDuplication::AcquireNextFrame",
                        message: err.to_string(),
                    });
                }
            }

            let mut frame_released = false;
            let frame_result = (|| -> Result<(), BackendError> {
                let frame_started = Instant::now();
                let resource = resource.ok_or_else(|| BackendError::WindowsApi {
                    func: "AcquireNextFrame",
                    message: "返回空 IDXGIResource".to_owned(),
                })?;
                let source: ID3D11Texture2D =
                    resource.cast().map_err(|err| BackendError::WindowsApi {
                        func: "IDXGIResource::cast<ID3D11Texture2D>",
                        message: err.to_string(),
                    })?;
                let mut source_desc = D3D11_TEXTURE2D_DESC::default();
                source.GetDesc(&mut source_desc);
                if input_dxgi_format == 0 {
                    input_dxgi_format = source_desc.Format.0 as u32;
                }

                let init_started = Instant::now();
                if !conversion_ready {
                    let content_desc = D3D11_VIDEO_PROCESSOR_CONTENT_DESC {
                        InputFrameFormat: D3D11_VIDEO_FRAME_FORMAT_PROGRESSIVE,
                        InputFrameRate: DXGI_RATIONAL {
                            Numerator: 144,
                            Denominator: 1,
                        },
                        InputWidth: source_desc.Width,
                        InputHeight: source_desc.Height,
                        OutputFrameRate: DXGI_RATIONAL {
                            Numerator: 144,
                            Denominator: 1,
                        },
                        OutputWidth: target_desc.Width,
                        OutputHeight: target_desc.Height,
                        Usage: D3D11_VIDEO_USAGE_PLAYBACK_NORMAL,
                    };
                    let new_enum = video_device
                        .CreateVideoProcessorEnumerator(&content_desc)
                        .map_err(|err| BackendError::WindowsApi {
                            func: "ID3D11VideoDevice::CreateVideoProcessorEnumerator",
                            message: err.to_string(),
                        })?;
                    format_flags_in = new_enum
                        .CheckVideoProcessorFormat(source_desc.Format)
                        .map_err(|err| BackendError::WindowsApi {
                            func: "ID3D11VideoProcessorEnumerator::CheckVideoProcessorFormat(input)",
                            message: err.to_string(),
                        })?;
                    format_flags_out = new_enum
                        .CheckVideoProcessorFormat(DXGI_FORMAT_P010)
                        .map_err(|err| BackendError::WindowsApi {
                            func: "ID3D11VideoProcessorEnumerator::CheckVideoProcessorFormat(P010)",
                            message: err.to_string(),
                        })?;
                    let shader_convertible_input = source_desc.Format.0
                        == DXGI_FORMAT_R16G16B16A16_FLOAT.0
                        || source_desc.Format.0 == DXGI_FORMAT_B8G8R8A8_UNORM.0;
                    if p010_converter.is_none()
                        && ((format_flags_in & D3D11_VIDEO_PROCESSOR_FORMAT_SUPPORT_INPUT.0 as u32)
                            == 0
                            || shader_convertible_input)
                    {
                        if shader_convertible_input
                            && std::env::var_os("RUST_REPLAY_DISABLE_P010_SHADER").is_none()
                        {
                            let converter_result = GpuP010Converter::new(
                                vpl_device,
                                &immediate,
                                &p010_intermediate,
                                source_desc.Width,
                                source_desc.Height,
                            );
                            match converter_result {
                                Ok(converter) => {
                                    p010_converter = Some(converter);
                                    notes.push(format!(
                                        "DDA 输入 DXGI_FORMAT({}) 使用 GPU shader 直接写入 P010 plane；跳过 RGBA8+VideoProcessor",
                                        source_desc.Format.0
                                    ));
                                }
                                Err(err) => {
                                    return Err(BackendError::unsupported(
                                        "GPU shader -> P010 plane",
                                        format!("DXGI_FORMAT({}) 输入", source_desc.Format.0),
                                        format!("初始化失败：{err}"),
                                    ));
                                }
                            }
                        }
                        if p010_converter.is_none() {
                            let rgba_flags = new_enum
                                .CheckVideoProcessorFormat(DXGI_FORMAT_R8G8B8A8_UNORM)
                                .map_err(|err| BackendError::WindowsApi {
                                    func: "ID3D11VideoProcessorEnumerator::CheckVideoProcessorFormat(RGBA8)",
                                    message: err.to_string(),
                                })?;
                            if (rgba_flags & D3D11_VIDEO_PROCESSOR_FORMAT_SUPPORT_INPUT.0 as u32)
                                == 0
                            {
                                return Err(BackendError::unsupported(
                                    "D3D11 VideoProcessor",
                                    format!(
                                        "输入 DXGI_FORMAT({})、P010 plane shader 或 RGBA8 中间纹理",
                                        source_desc.Format.0
                                    ),
                                    "驱动既不支持原始输入，也不支持 P010 plane shader/VideoProcessor RGBA8 input",
                                ));
                            }
                            format_flags_in = rgba_flags;
                            let converter = GpuRgbaConverter::new(
                                vpl_device,
                                &immediate,
                                source_desc.Width,
                                source_desc.Height,
                            )?;
                            cached_blitter = Some(VideoProcessorBlitter::new(
                                &video_device,
                                &new_enum,
                                converter.output_texture(),
                                &p010_intermediate,
                            )?);
                            rgba_converter = Some(converter);
                            notes.push(format!(
                                "DDA 输入 DXGI_FORMAT({}) 不被 VideoProcessor 直接接受，已启用 GPU shader -> RGBA8 中间纹理",
                                source_desc.Format.0
                            ));
                        }
                    }
                    if p010_converter.is_none()
                        && (format_flags_out & D3D11_VIDEO_PROCESSOR_FORMAT_SUPPORT_OUTPUT.0 as u32)
                            == 0
                    {
                        return Err(BackendError::unsupported(
                            "D3D11 VideoProcessor",
                            "P010 输出",
                            "驱动未报告 VideoProcessor P010 output 支持",
                        ));
                    }
                    let new_processor =
                        if p010_converter.is_none() {
                            Some(video_device.CreateVideoProcessor(&new_enum, 0).map_err(
                                |err| BackendError::WindowsApi {
                                    func: "ID3D11VideoDevice::CreateVideoProcessor",
                                    message: err.to_string(),
                                },
                            )?)
                        } else {
                            None
                        };
                    enumerator = Some(new_enum);
                    video_processor = new_processor;
                    conversion_ready = true;
                }
                perf.init.add(init_started.elapsed());

                let snapshot_started = Instant::now();
                if dda_snapshot.is_none() {
                    dda_snapshot = Some(create_dda_snapshot_texture(vpl_device, &source_desc)?);
                    notes.push(
                        "为避免 DDA AcquireNextFrame 因持有帧而退到半刷新率，已启用 GPU snapshot copy 后立即 ReleaseFrame"
                            .to_owned(),
                    );
                }
                let snapshot = dda_snapshot.as_ref().expect("DDA snapshot initialized");
                copy_texture_resource(&immediate, &source, snapshot)?;
                perf.snapshot.add(snapshot_started.elapsed());

                let release_started = Instant::now();
                duplication
                    .ReleaseFrame()
                    .map_err(|err| BackendError::WindowsApi {
                        func: "IDXGIOutputDuplication::ReleaseFrame(snapshot)",
                        message: err.to_string(),
                    })?;
                frame_released = true;
                perf.release_frame.add(release_started.elapsed());

                let convert_started = Instant::now();
                if let Some(converter) = &p010_converter {
                    converter.convert(snapshot)?;
                } else if let Some(converter) = &mut rgba_converter {
                    let converted = converter.convert(snapshot)?;
                    if let Some(blitter) = &cached_blitter {
                        blitter.blit(
                            &video_context,
                            video_processor.as_ref().expect("processor initialized"),
                        )?;
                    } else {
                        process_with_video_processor(
                            &video_device,
                            &video_context,
                            enumerator.as_ref().expect("enumerator initialized"),
                            video_processor.as_ref().expect("processor initialized"),
                            &converted,
                            &p010_intermediate,
                        )?;
                    }
                } else {
                    process_with_video_processor(
                        &video_device,
                        &video_context,
                        enumerator.as_ref().expect("enumerator initialized"),
                        video_processor.as_ref().expect("processor initialized"),
                        snapshot,
                        &p010_intermediate,
                    )?;
                }
                perf.convert.add(convert_started.elapsed());

                let surface_started = Instant::now();
                let surface = if let Some(surface) = pending_surface.take() {
                    surface
                } else {
                    let mut next_surface: *mut MfxFrameSurface1 = ptr::null_mut();
                    let status =
                        (api.mfx_memory_get_surface_for_encode)(session, &mut next_surface);
                    if status != MFX_ERR_NONE || next_surface.is_null() {
                        return Err(BackendError::VplStatus {
                            func: "MFXMemory_GetSurfaceForEncode",
                            status,
                        });
                    }
                    next_surface
                };

                let frame_interface = (*surface).FrameInterface;
                let mut native: MfxHDL = ptr::null_mut();
                let mut native_type = 0u32;
                let status =
                    ((*frame_interface).GetNativeHandle)(surface, &mut native, &mut native_type);
                if status != MFX_ERR_NONE || native_type != MFX_RESOURCE_DX11_TEXTURE {
                    let _ = ((*frame_interface).Release)(surface);
                    return Err(BackendError::VplStatus {
                        func: "mfxFrameSurfaceInterface::GetNativeHandle",
                        status,
                    });
                }
                let Some(target) = <ID3D11Texture2D as Interface>::from_raw_borrowed(&native)
                else {
                    let _ = ((*frame_interface).Release)(surface);
                    return Err(BackendError::unsupported(
                        "oneVPL record",
                        "native texture",
                        "GetNativeHandle 返回值不是 ID3D11Texture2D",
                    ));
                };
                perf.surface.add(surface_started.elapsed());

                let copy_started = Instant::now();
                copy_texture_resource(&immediate, &p010_intermediate, target)?;
                perf.copy.add(copy_started.elapsed());

                let elapsed = Instant::now().saturating_duration_since(start);
                let ts90 = duration_to_90k(elapsed);
                (*surface).Data.TimeStamp = ts90;
                (*surface).Data.FrameOrder = captured_frames;
                let sample_ts90 = if captured_frames == 0 { 0 } else { ts90 };
                let submit_started = Instant::now();
                let submitted = submit_encode_async(
                    &api,
                    session,
                    surface,
                    sample_ts90,
                    captured_frames == 0,
                    bitstream_pool.pop().unwrap_or_default(),
                )?;
                perf.submit.add(submit_started.elapsed());
                let release_status = ((*frame_interface).Release)(surface);
                if release_status != MFX_ERR_NONE {
                    return Err(BackendError::VplStatus {
                        func: "mfxFrameSurfaceInterface::Release",
                        status: release_status,
                    });
                }
                if let Some(submitted) = submitted {
                    in_flight.push_back(submitted);
                }
                let sync_started = Instant::now();
                while in_flight.len() >= async_depth {
                    match try_sync_one_async_encode(
                        &api,
                        session,
                        &mut in_flight,
                        &mut bitstream_pool,
                        1,
                    )? {
                        TrySyncResult::Ready(Some(sample)) => samples.push(sample),
                        TrySyncResult::Ready(None) => {}
                        TrySyncResult::NotReady => break,
                    }
                }
                while in_flight.len() >= VPL_RECORD_MAX_IN_FLIGHT {
                    if let Some(sample) =
                        sync_one_async_encode(&api, session, &mut in_flight, &mut bitstream_pool)?
                    {
                        samples.push(sample);
                    }
                }
                perf.sync.add(sync_started.elapsed());
                captured_frames += 1;
                perf.frame.add(frame_started.elapsed());
                Ok(())
            })();

            if !frame_released {
                let release_started = Instant::now();
                duplication
                    .ReleaseFrame()
                    .map_err(|err| BackendError::WindowsApi {
                        func: "IDXGIOutputDuplication::ReleaseFrame",
                        message: err.to_string(),
                    })?;
                perf.release_frame.add(release_started.elapsed());
            }
            frame_result?;
        }

        let duration_90k = duration_to_90k(start.elapsed())
            .max((duration_seconds.max(0.1) as f64 * VIDEO_CLOCK_HZ as f64).round() as u64);
        while !in_flight.is_empty() {
            if let Some(sample) =
                sync_one_async_encode(&api, session, &mut in_flight, &mut bitstream_pool)?
            {
                samples.push(sample);
            }
        }
        flush_encoder(&api, session, &mut samples, duration_90k)?;
        let close_status = (api.mfx_video_encode_close)(session);
        let mfx_close_status = (api.mfx_close)(session);
        (api.mfx_unload)(loader);
        if mfx_close_status != MFX_ERR_NONE {
            return Err(BackendError::VplStatus {
                func: "MFXClose",
                status: mfx_close_status,
            });
        }

        let encoded_samples = samples.len() as u32;
        let encoded_bytes = samples.iter().map(|s| s.data.len() as u64).sum();
        write_hevc_mp4(
            output,
            &HevcMp4Track {
                width: capture_width,
                height: capture_height,
                duration_90k,
                samples,
            },
        )?;

        notes.push("视频路径为 DDA texture -> GPU shader(必要时RGBA8) -> D3D11 VideoProcessor(P010中间纹理) -> 一次 CopyResource 到 oneVPL surface -> HEVC -> MP4；未做 raw frame CPU 回读".to_owned());
        notes.push("当前成品先完成视频轨；音频/WASAPI+AAC 仍需下一轮接入".to_owned());
        notes.push(perf.summary(captured_frames));

        Ok(VplOneCopyRecordReport {
            adapter_index,
            adapter_luid,
            output_path: output.display().to_string(),
            width: capture_width,
            height: capture_height,
            duration_seconds,
            captured_frames,
            encoded_samples,
            encoded_bytes,
            dda_timeouts,
            input_dxgi_format,
            target_dxgi_format: target_desc.Format.0 as u32,
            query_status,
            init_status,
            close_status,
            first_get_surface_status,
            video_processor_format_flags_in: format_flags_in,
            video_processor_format_flags_out: format_flags_out,
            notes,
        })
    }
}

#[cfg(not(windows))]
pub fn record_d3d11_onecopy_mp4(
    adapter_index: u32,
    output: &Path,
    duration_seconds: f32,
    rate_control: RateControlMethod,
) -> Result<VplOneCopyRecordReport, BackendError> {
    let _ = (adapter_index, output, duration_seconds, rate_control);
    Err(BackendError::unsupported(
        "oneVPL D3D11 one-copy record",
        "Windows D3D11",
        "仅 Windows 可用",
    ))
}

unsafe fn make_external_surface(
    info: &MfxFrameInfo,
    pair: *mut MfxHDLPair,
    mem_type: u16,
    timestamp: u64,
) -> MfxFrameSurface1 {
    let mut surface: MfxFrameSurface1 = std::mem::zeroed();
    surface.Version = MfxStructVersion {
        version: struct_version(1, 1),
    };
    surface.Info = *info;
    surface.Data.MemType = mem_type;
    surface.Data.MemId = pair as MfxHDL;
    surface.Data.TimeStamp = timestamp;
    surface
}

unsafe fn import_d3d11_texture_shared(
    memory_interface: *mut MfxMemoryInterface,
    texture: MfxHDL,
) -> (i32, u32, *mut MfxFrameSurface1) {
    if memory_interface.is_null() {
        return (MFX_ERR_NOT_IMPLEMENTED, 0, ptr::null_mut());
    }

    let mut external: MfxSurfaceD3D11Tex2D = std::mem::zeroed();
    external.SurfaceInterface.Header.SurfaceType = MFX_SURFACE_TYPE_D3D11_TEX2D;
    external.SurfaceInterface.Header.SurfaceFlags = MFX_SURFACE_FLAG_IMPORT_SHARED;
    external.SurfaceInterface.Header.StructSize =
        std::mem::size_of::<MfxSurfaceD3D11Tex2D>() as u32;
    external.SurfaceInterface.Version = MfxStructVersion {
        version: MFX_SURFACEINTERFACE_VERSION,
    };
    external.texture2D = texture;

    let mut imported_surface: *mut MfxFrameSurface1 = ptr::null_mut();
    let status = ((*memory_interface).ImportFrameSurface)(
        memory_interface,
        MFX_SURFACE_COMPONENT_ENCODE,
        &mut external.SurfaceInterface.Header,
        &mut imported_surface,
    );
    (
        status,
        external.SurfaceInterface.Header.SurfaceFlags,
        imported_surface,
    )
}

#[derive(Debug)]
struct EncodedSurfaceBytes {
    encode_status: i32,
    sync_status: i32,
    bytes: Vec<u8>,
}

#[derive(Default)]
struct StageTiming {
    calls: u64,
    total_ns: u128,
    max_ns: u128,
}

impl StageTiming {
    fn add(&mut self, duration: std::time::Duration) {
        let ns = duration.as_nanos();
        self.calls += 1;
        self.total_ns += ns;
        self.max_ns = self.max_ns.max(ns);
    }

    fn avg_ms(&self) -> f64 {
        if self.calls == 0 {
            0.0
        } else {
            self.total_ns as f64 / self.calls as f64 / 1_000_000.0
        }
    }

    fn max_ms(&self) -> f64 {
        self.max_ns as f64 / 1_000_000.0
    }
}

#[derive(Default)]
struct RecordPerf {
    acquire: StageTiming,
    init: StageTiming,
    snapshot: StageTiming,
    surface: StageTiming,
    convert: StageTiming,
    copy: StageTiming,
    submit: StageTiming,
    sync: StageTiming,
    release_frame: StageTiming,
    frame: StageTiming,
    dda_accumulated_frames_total: u64,
    dda_accumulated_frames_max: u32,
}

impl RecordPerf {
    fn summary(&self, captured_frames: u32) -> String {
        let avg_accumulated = if self.acquire.calls == 0 {
            0.0
        } else {
            self.dda_accumulated_frames_total as f64 / self.acquire.calls as f64
        };
        format!(
            concat!(
                "perf(cpu ms avg/max): acquire={:.3}/{:.3}, init={:.3}/{:.3}, ",
                "dda_snapshot={:.3}/{:.3}, surface+native={:.3}/{:.3}, ",
                "convert={:.3}/{:.3}, copy={:.3}/{:.3}, ",
                "encode_submit={:.3}/{:.3}, encode_sync={:.3}/{:.3}, ",
                "release_frame={:.3}/{:.3}, frame_body={:.3}/{:.3}; ",
                "dda_accumulated avg/max={:.2}/{}, captured={}"
            ),
            self.acquire.avg_ms(),
            self.acquire.max_ms(),
            self.init.avg_ms(),
            self.init.max_ms(),
            self.snapshot.avg_ms(),
            self.snapshot.max_ms(),
            self.surface.avg_ms(),
            self.surface.max_ms(),
            self.convert.avg_ms(),
            self.convert.max_ms(),
            self.copy.avg_ms(),
            self.copy.max_ms(),
            self.submit.avg_ms(),
            self.submit.max_ms(),
            self.sync.avg_ms(),
            self.sync.max_ms(),
            self.release_frame.avg_ms(),
            self.release_frame.max_ms(),
            self.frame.avg_ms(),
            self.frame.max_ms(),
            avg_accumulated,
            self.dda_accumulated_frames_max,
            captured_frames
        )
    }
}

struct AsyncEncode {
    bitstream: MfxBitstream,
    storage: Vec<u8>,
    syncp: MfxSyncPoint,
    timestamp_90k: u64,
    is_sync: bool,
}

enum TrySyncResult {
    Ready(Option<crate::backend::mp4_mux::HevcAccessUnit>),
    NotReady,
}

unsafe fn submit_encode_async(
    api: &VplApi,
    session: MfxSession,
    surface: *mut MfxFrameSurface1,
    timestamp_90k: u64,
    is_sync: bool,
    mut storage: Vec<u8>,
) -> Result<Option<Box<AsyncEncode>>, BackendError> {
    storage.resize(VPL_BITSTREAM_BYTES + 31, 0);
    let aligned_offset = (32 - (storage.as_ptr() as usize & 31)) & 31;
    let aligned = storage.as_mut_ptr().add(aligned_offset);
    let mut flight = Box::new(AsyncEncode {
        bitstream: MfxBitstream {
            CodecId: MFX_CODEC_HEVC,
            Data: aligned,
            MaxLength: VPL_BITSTREAM_BYTES as u32,
            TimeStamp: timestamp_90k,
            ..std::mem::zeroed()
        },
        storage,
        syncp: ptr::null_mut(),
        timestamp_90k,
        is_sync,
    });

    let mut status = (api.mfx_video_encode_frame_async)(
        session,
        ptr::null_mut(),
        surface,
        &mut flight.bitstream,
        &mut flight.syncp,
    );
    let mut busy_retries = 0u32;
    while status == MFX_WRN_DEVICE_BUSY && busy_retries < 10 {
        std::thread::sleep(std::time::Duration::from_millis(1));
        flight.syncp = ptr::null_mut();
        flight.bitstream.DataLength = 0;
        flight.bitstream.DataOffset = 0;
        status = (api.mfx_video_encode_frame_async)(
            session,
            ptr::null_mut(),
            surface,
            &mut flight.bitstream,
            &mut flight.syncp,
        );
        busy_retries += 1;
    }

    if matches!(status, MFX_ERR_MORE_DATA | MFX_ERR_MORE_SURFACE) {
        return Ok(None);
    }
    if status < MFX_ERR_NONE {
        return Err(BackendError::VplStatus {
            func: "MFXVideoENCODE_EncodeFrameAsync",
            status,
        });
    }
    if flight.syncp.is_null() {
        return Ok(None);
    }
    Ok(Some(flight))
}

unsafe fn sync_one_async_encode(
    api: &VplApi,
    session: MfxSession,
    in_flight: &mut VecDeque<Box<AsyncEncode>>,
    bitstream_pool: &mut Vec<Vec<u8>>,
) -> Result<Option<crate::backend::mp4_mux::HevcAccessUnit>, BackendError> {
    let Some(flight) = in_flight.pop_front() else {
        return Ok(None);
    };
    let sync_status = (api.mfx_video_core_sync_operation)(session, flight.syncp, 60_000);
    if sync_status < MFX_ERR_NONE {
        return Err(BackendError::VplStatus {
            func: "MFXVideoCORE_SyncOperation",
            status: sync_status,
        });
    }
    finish_synced_async_encode(*flight, bitstream_pool)
}

unsafe fn try_sync_one_async_encode(
    api: &VplApi,
    session: MfxSession,
    in_flight: &mut VecDeque<Box<AsyncEncode>>,
    bitstream_pool: &mut Vec<Vec<u8>>,
    timeout_ms: u32,
) -> Result<TrySyncResult, BackendError> {
    let Some(front) = in_flight.front() else {
        return Ok(TrySyncResult::Ready(None));
    };
    let sync_status = (api.mfx_video_core_sync_operation)(session, front.syncp, timeout_ms);
    if matches!(sync_status, MFX_WRN_IN_EXECUTION | MFX_WRN_DEVICE_BUSY) {
        return Ok(TrySyncResult::NotReady);
    }
    if sync_status < MFX_ERR_NONE {
        return Err(BackendError::VplStatus {
            func: "MFXVideoCORE_SyncOperation(short)",
            status: sync_status,
        });
    }
    let flight = in_flight
        .pop_front()
        .expect("front existed before successful short sync");
    finish_synced_async_encode(*flight, bitstream_pool).map(TrySyncResult::Ready)
}

unsafe fn poll_completed_async_encodes(
    api: &VplApi,
    session: MfxSession,
    in_flight: &mut VecDeque<Box<AsyncEncode>>,
    bitstream_pool: &mut Vec<Vec<u8>>,
    samples: &mut Vec<crate::backend::mp4_mux::HevcAccessUnit>,
) -> Result<(), BackendError> {
    while let Some(front) = in_flight.front() {
        let sync_status = (api.mfx_video_core_sync_operation)(session, front.syncp, 0);
        if matches!(sync_status, MFX_WRN_IN_EXECUTION | MFX_WRN_DEVICE_BUSY) {
            break;
        }
        if sync_status < MFX_ERR_NONE {
            return Err(BackendError::VplStatus {
                func: "MFXVideoCORE_SyncOperation(poll)",
                status: sync_status,
            });
        }
        let flight = in_flight
            .pop_front()
            .expect("front existed before successful poll");
        if let Some(sample) = finish_synced_async_encode(*flight, bitstream_pool)? {
            samples.push(sample);
        }
    }
    Ok(())
}

unsafe fn finish_synced_async_encode(
    flight: AsyncEncode,
    bitstream_pool: &mut Vec<Vec<u8>>,
) -> Result<Option<crate::backend::mp4_mux::HevcAccessUnit>, BackendError> {
    let len = flight.bitstream.DataLength as usize;
    if len == 0 {
        bitstream_pool.push(flight.storage);
        return Ok(None);
    }
    let start = flight
        .bitstream
        .Data
        .add(flight.bitstream.DataOffset as usize);
    let data = std::slice::from_raw_parts(start, len).to_vec();
    bitstream_pool.push(flight.storage);
    Ok(Some(crate::backend::mp4_mux::HevcAccessUnit {
        timestamp_90k: flight.timestamp_90k,
        data,
        is_sync: flight.is_sync,
    }))
}

unsafe fn encode_surface_bytes(
    api: &VplApi,
    session: MfxSession,
    surface: *mut MfxFrameSurface1,
) -> Result<EncodedSurfaceBytes, BackendError> {
    encode_surface_or_flush_bytes(api, session, surface)
}

unsafe fn encode_surface_or_flush_bytes(
    api: &VplApi,
    session: MfxSession,
    surface: *mut MfxFrameSurface1,
) -> Result<EncodedSurfaceBytes, BackendError> {
    const BITSTREAM_BYTES: usize = 128 * 1024 * 1024;

    let mut storage = vec![0u8; BITSTREAM_BYTES + 31];
    let aligned_offset = (32 - (storage.as_ptr() as usize & 31)) & 31;
    let aligned = storage.as_mut_ptr().add(aligned_offset);
    let mut bitstream: MfxBitstream = std::mem::zeroed();
    bitstream.CodecId = MFX_CODEC_HEVC;
    bitstream.Data = aligned;
    bitstream.MaxLength = BITSTREAM_BYTES as u32;

    let mut syncp: MfxSyncPoint = ptr::null_mut();
    let mut encode_status = (api.mfx_video_encode_frame_async)(
        session,
        ptr::null_mut(),
        surface,
        &mut bitstream,
        &mut syncp,
    );
    let mut busy_retries = 0u32;
    while encode_status == MFX_WRN_DEVICE_BUSY && busy_retries < 50 {
        std::thread::sleep(std::time::Duration::from_millis(2));
        syncp = ptr::null_mut();
        bitstream.DataLength = 0;
        bitstream.DataOffset = 0;
        encode_status = (api.mfx_video_encode_frame_async)(
            session,
            ptr::null_mut(),
            surface,
            &mut bitstream,
            &mut syncp,
        );
        busy_retries += 1;
    }

    if matches!(encode_status, MFX_ERR_MORE_DATA | MFX_ERR_MORE_SURFACE) {
        return Ok(EncodedSurfaceBytes {
            encode_status,
            sync_status: i32::MIN,
            bytes: Vec::new(),
        });
    }
    if encode_status < MFX_ERR_NONE {
        return Err(BackendError::VplStatus {
            func: "MFXVideoENCODE_EncodeFrameAsync",
            status: encode_status,
        });
    }

    let sync_status = if !syncp.is_null() {
        (api.mfx_video_core_sync_operation)(session, syncp, 60_000)
    } else {
        i32::MIN
    };
    if sync_status < MFX_ERR_NONE && sync_status != i32::MIN {
        return Err(BackendError::VplStatus {
            func: "MFXVideoCORE_SyncOperation",
            status: sync_status,
        });
    }

    let start = bitstream.Data.add(bitstream.DataOffset as usize);
    let len = bitstream.DataLength as usize;
    let bytes = if len == 0 {
        Vec::new()
    } else {
        std::slice::from_raw_parts(start, len).to_vec()
    };
    Ok(EncodedSurfaceBytes {
        encode_status,
        sync_status,
        bytes,
    })
}

unsafe fn flush_encoder(
    api: &VplApi,
    session: MfxSession,
    samples: &mut Vec<crate::backend::mp4_mux::HevcAccessUnit>,
    duration_90k: u64,
) -> Result<(), BackendError> {
    loop {
        let encoded = encode_surface_or_flush_bytes(api, session, ptr::null_mut())?;
        if encoded.encode_status == MFX_ERR_MORE_DATA {
            break;
        }
        if !encoded.bytes.is_empty() {
            samples.push(crate::backend::mp4_mux::HevcAccessUnit {
                timestamp_90k: duration_90k.saturating_sub(1),
                data: encoded.bytes,
                is_sync: false,
            });
        } else {
            break;
        }
    }
    Ok(())
}

#[cfg(windows)]
unsafe fn create_duplication_on_device(
    adapter1: &windows::Win32::Graphics::Dxgi::IDXGIAdapter1,
    device: &windows::Win32::Graphics::Direct3D11::ID3D11Device,
) -> Result<windows::Win32::Graphics::Dxgi::IDXGIOutputDuplication, BackendError> {
    use windows::Win32::Graphics::Dxgi::IDXGIOutput1;
    use windows::core::Interface;

    let output = adapter1
        .EnumOutputs(0)
        .map_err(|err| BackendError::WindowsApi {
            func: "IDXGIAdapter1::EnumOutputs(0)",
            message: err.to_string(),
        })?;
    let output1: IDXGIOutput1 = output.cast().map_err(|err| BackendError::WindowsApi {
        func: "IDXGIOutput::cast<IDXGIOutput1>",
        message: err.to_string(),
    })?;
    output1
        .DuplicateOutput(device)
        .map_err(|err| BackendError::WindowsApi {
            func: "IDXGIOutput1::DuplicateOutput(oneVPL device)",
            message: err.to_string(),
        })
}

#[cfg(windows)]
unsafe fn create_p010_intermediate(
    device: &windows::Win32::Graphics::Direct3D11::ID3D11Device,
    target_desc: &windows::Win32::Graphics::Direct3D11::D3D11_TEXTURE2D_DESC,
) -> Result<windows::Win32::Graphics::Direct3D11::ID3D11Texture2D, BackendError> {
    use windows::Win32::Graphics::Direct3D11::{
        D3D11_BIND_RENDER_TARGET, D3D11_TEXTURE2D_DESC, D3D11_USAGE_DEFAULT,
    };
    use windows::Win32::Graphics::Dxgi::Common::DXGI_FORMAT_P010;

    let desc = D3D11_TEXTURE2D_DESC {
        Width: target_desc.Width,
        Height: target_desc.Height,
        MipLevels: 1,
        ArraySize: 1,
        Format: DXGI_FORMAT_P010,
        SampleDesc: target_desc.SampleDesc,
        Usage: D3D11_USAGE_DEFAULT,
        BindFlags: D3D11_BIND_RENDER_TARGET.0 as u32,
        CPUAccessFlags: 0,
        MiscFlags: 0,
    };
    let mut texture = None;
    device
        .CreateTexture2D(&desc, None, Some(&mut texture))
        .map_err(|err| BackendError::WindowsApi {
            func: "ID3D11Device::CreateTexture2D(P010 intermediate)",
            message: err.to_string(),
        })?;
    texture.ok_or_else(|| BackendError::WindowsApi {
        func: "CreateTexture2D(P010 intermediate)",
        message: "返回空纹理".to_owned(),
    })
}

#[cfg(windows)]
unsafe fn create_dda_snapshot_texture(
    device: &windows::Win32::Graphics::Direct3D11::ID3D11Device,
    source_desc: &windows::Win32::Graphics::Direct3D11::D3D11_TEXTURE2D_DESC,
) -> Result<windows::Win32::Graphics::Direct3D11::ID3D11Texture2D, BackendError> {
    use windows::Win32::Graphics::Direct3D11::{
        D3D11_BIND_SHADER_RESOURCE, D3D11_TEXTURE2D_DESC, D3D11_USAGE_DEFAULT,
    };

    let desc = D3D11_TEXTURE2D_DESC {
        Width: source_desc.Width.max(1),
        Height: source_desc.Height.max(1),
        MipLevels: 1,
        ArraySize: 1,
        Format: source_desc.Format,
        SampleDesc: source_desc.SampleDesc,
        Usage: D3D11_USAGE_DEFAULT,
        BindFlags: D3D11_BIND_SHADER_RESOURCE.0 as u32,
        CPUAccessFlags: 0,
        MiscFlags: 0,
    };
    let mut texture = None;
    device
        .CreateTexture2D(&desc, None, Some(&mut texture))
        .map_err(|err| BackendError::WindowsApi {
            func: "ID3D11Device::CreateTexture2D(DDA snapshot)",
            message: err.to_string(),
        })?;
    texture.ok_or_else(|| BackendError::WindowsApi {
        func: "CreateTexture2D(DDA snapshot)",
        message: "返回空纹理".to_owned(),
    })
}

#[cfg(windows)]
struct SnapshotSlot {
    id: usize,
    texture: windows::Win32::Graphics::Direct3D11::ID3D11Texture2D,
}

#[cfg(windows)]
struct CapturedSnapshot {
    slot: SnapshotSlot,
    source_desc: windows::Win32::Graphics::Direct3D11::D3D11_TEXTURE2D_DESC,
    timestamp_90k: u64,
    capture_index: u64,
    accumulated_frames: u32,
}

#[cfg(windows)]
struct CaptureStats {
    acquired: u64,
    copied: u64,
    dropped_no_slot: u64,
    dropped_queue_full: u64,
    dda_timeouts: u64,
    accumulated_frames_total: u64,
    accumulated_frames_max: u32,
}

#[cfg(windows)]
impl CaptureStats {
    fn new() -> Self {
        Self {
            acquired: 0,
            copied: 0,
            dropped_no_slot: 0,
            dropped_queue_full: 0,
            dda_timeouts: 0,
            accumulated_frames_total: 0,
            accumulated_frames_max: 0,
        }
    }

    fn summary(&self) -> String {
        let avg_accumulated = if self.acquired == 0 {
            0.0
        } else {
            self.accumulated_frames_total as f64 / self.acquired as f64
        };
        format!(
            "capture-thread: acquired={}, copied={}, dropped_no_slot={}, dropped_queue_full={}, dda_timeouts={}, dda_accumulated avg/max={:.2}/{}",
            self.acquired,
            self.copied,
            self.dropped_no_slot,
            self.dropped_queue_full,
            self.dda_timeouts,
            avg_accumulated,
            self.accumulated_frames_max
        )
    }
}

#[cfg(windows)]
fn snapshot_slot_matches(
    slot: &SnapshotSlot,
    desc: &windows::Win32::Graphics::Direct3D11::D3D11_TEXTURE2D_DESC,
) -> bool {
    let mut slot_desc = windows::Win32::Graphics::Direct3D11::D3D11_TEXTURE2D_DESC::default();
    unsafe {
        slot.texture.GetDesc(&mut slot_desc);
    }
    slot_desc.Width == desc.Width
        && slot_desc.Height == desc.Height
        && slot_desc.Format.0 == desc.Format.0
}

#[cfg(windows)]
enum CaptureMsg {
    Frame(CapturedSnapshot),
    Done(CaptureStats),
    Error(String),
}

#[cfg(windows)]
#[allow(clippy::too_many_arguments)]
fn spawn_dda_capture_thread(
    adapter1: windows::Win32::Graphics::Dxgi::IDXGIAdapter1,
    device: windows::Win32::Graphics::Direct3D11::ID3D11Device,
    context: windows::Win32::Graphics::Direct3D11::ID3D11DeviceContext,
    d3d_multithread: Option<windows::Win32::Graphics::Direct3D11::ID3D11Multithread>,
    start: std::time::Instant,
    end_at: std::time::Instant,
    pool_size: usize,
    stop: std::sync::Arc<std::sync::atomic::AtomicBool>,
    frame_tx: std::sync::mpsc::SyncSender<CaptureMsg>,
    free_rx: std::sync::mpsc::Receiver<SnapshotSlot>,
) -> std::thread::JoinHandle<()> {
    std::thread::spawn(move || {
        let result = unsafe {
            run_dda_capture_thread(
                adapter1,
                device,
                context,
                d3d_multithread,
                start,
                end_at,
                pool_size,
                stop,
                &frame_tx,
                free_rx,
            )
        };
        match result {
            Ok(stats) => {
                let _ = frame_tx.send(CaptureMsg::Done(stats));
            }
            Err(message) => {
                let _ = frame_tx.send(CaptureMsg::Error(message));
            }
        }
    })
}

#[cfg(windows)]
#[allow(clippy::too_many_arguments)]
unsafe fn run_dda_capture_thread(
    adapter1: windows::Win32::Graphics::Dxgi::IDXGIAdapter1,
    device: windows::Win32::Graphics::Direct3D11::ID3D11Device,
    context: windows::Win32::Graphics::Direct3D11::ID3D11DeviceContext,
    d3d_multithread: Option<windows::Win32::Graphics::Direct3D11::ID3D11Multithread>,
    start: std::time::Instant,
    end_at: std::time::Instant,
    pool_size: usize,
    stop: std::sync::Arc<std::sync::atomic::AtomicBool>,
    frame_tx: &std::sync::mpsc::SyncSender<CaptureMsg>,
    free_rx: std::sync::mpsc::Receiver<SnapshotSlot>,
) -> Result<CaptureStats, String> {
    use std::collections::VecDeque;
    use std::sync::atomic::Ordering;
    use windows::Win32::Graphics::Direct3D11::D3D11_TEXTURE2D_DESC;
    use windows::Win32::Graphics::Dxgi::{
        DXGI_ERROR_WAIT_TIMEOUT, DXGI_OUTDUPL_FRAME_INFO, IDXGIResource,
    };
    use windows::core::Interface;

    let duplication =
        create_duplication_on_device(&adapter1, &device).map_err(|err| err.to_string())?;
    let mut stats = CaptureStats::new();
    let mut free_slots: VecDeque<SnapshotSlot> = VecDeque::new();
    let mut source_desc0: Option<D3D11_TEXTURE2D_DESC> = None;
    let mut capture_index = 0u64;

    while !stop.load(Ordering::Relaxed) && std::time::Instant::now() < end_at {
        while let Ok(slot) = free_rx.try_recv() {
            if source_desc0
                .as_ref()
                .is_none_or(|desc| snapshot_slot_matches(&slot, desc))
            {
                free_slots.push_back(slot);
            }
        }

        let mut frame_info = DXGI_OUTDUPL_FRAME_INFO::default();
        let mut resource: Option<IDXGIResource> = None;
        match duplication.AcquireNextFrame(8, &mut frame_info, &mut resource) {
            Ok(()) => {}
            Err(err) if err.code() == DXGI_ERROR_WAIT_TIMEOUT => {
                stats.dda_timeouts += 1;
                continue;
            }
            Err(err) => {
                return Err(format!(
                    "IDXGIOutputDuplication::AcquireNextFrame(capture): {err}"
                ));
            }
        }

        let frame_result = (|| -> Result<(), String> {
            stats.acquired += 1;
            stats.accumulated_frames_total += u64::from(frame_info.AccumulatedFrames);
            stats.accumulated_frames_max = stats
                .accumulated_frames_max
                .max(frame_info.AccumulatedFrames);

            let resource =
                resource.ok_or_else(|| "AcquireNextFrame returned null resource".to_owned())?;
            let source: windows::Win32::Graphics::Direct3D11::ID3D11Texture2D = resource
                .cast()
                .map_err(|err| format!("IDXGIResource::cast<ID3D11Texture2D>(capture): {err}"))?;
            let mut source_desc = D3D11_TEXTURE2D_DESC::default();
            source.GetDesc(&mut source_desc);

            if let Some(first) = source_desc0 {
                if first.Width != source_desc.Width
                    || first.Height != source_desc.Height
                    || first.Format.0 != source_desc.Format.0
                {
                    free_slots.clear();
                    source_desc0 = Some(source_desc);
                    for id in 0..pool_size {
                        let texture = create_dda_snapshot_texture(&device, &source_desc)
                            .map_err(|err| err.to_string())?;
                        free_slots.push_back(SnapshotSlot { id, texture });
                    }
                }
            } else {
                source_desc0 = Some(source_desc);
                for id in 0..pool_size {
                    let texture = create_dda_snapshot_texture(&device, &source_desc)
                        .map_err(|err| err.to_string())?;
                    free_slots.push_back(SnapshotSlot { id, texture });
                }
            }

            let Some(slot) = free_slots.pop_front() else {
                stats.dropped_no_slot += 1;
                return Ok(());
            };

            {
                let _guard = D3d11MultithreadGuard::enter(&d3d_multithread);
                copy_texture_resource(&context, &source, &slot.texture)
                    .map_err(|err| err.to_string())?;
            }
            stats.copied += 1;
            let timestamp_90k = duration_to_90k(std::time::Instant::now().duration_since(start));
            let captured = CapturedSnapshot {
                slot,
                source_desc,
                timestamp_90k,
                capture_index,
                accumulated_frames: frame_info.AccumulatedFrames,
            };
            capture_index += 1;

            match frame_tx.try_send(CaptureMsg::Frame(captured)) {
                Ok(()) => {}
                Err(std::sync::mpsc::TrySendError::Full(CaptureMsg::Frame(frame))) => {
                    stats.dropped_queue_full += 1;
                    free_slots.push_back(frame.slot);
                }
                Err(std::sync::mpsc::TrySendError::Disconnected(_)) => {
                    stop.store(true, Ordering::Relaxed);
                }
                Err(std::sync::mpsc::TrySendError::Full(_)) => {}
            }
            Ok(())
        })();

        duplication
            .ReleaseFrame()
            .map_err(|err| format!("IDXGIOutputDuplication::ReleaseFrame(capture): {err}"))?;
        frame_result?;
    }

    Ok(stats)
}

#[cfg(windows)]
struct D3d11MultithreadGuard<'a> {
    mt: Option<&'a windows::Win32::Graphics::Direct3D11::ID3D11Multithread>,
}

#[cfg(windows)]
impl<'a> D3d11MultithreadGuard<'a> {
    unsafe fn enter(
        mt: &'a Option<windows::Win32::Graphics::Direct3D11::ID3D11Multithread>,
    ) -> Self {
        if let Some(mt) = mt {
            mt.Enter();
            Self { mt: Some(mt) }
        } else {
            Self { mt: None }
        }
    }
}

#[cfg(windows)]
impl Drop for D3d11MultithreadGuard<'_> {
    fn drop(&mut self) {
        unsafe {
            if let Some(mt) = self.mt {
                mt.Leave();
            }
        }
    }
}

#[cfg(windows)]
unsafe fn copy_texture_resource(
    context: &windows::Win32::Graphics::Direct3D11::ID3D11DeviceContext,
    source: &windows::Win32::Graphics::Direct3D11::ID3D11Texture2D,
    target: &windows::Win32::Graphics::Direct3D11::ID3D11Texture2D,
) -> Result<(), BackendError> {
    use windows::Win32::Graphics::Direct3D11::ID3D11Resource;
    use windows::core::Interface;

    let src_resource: ID3D11Resource = source.cast().map_err(|err| BackendError::WindowsApi {
        func: "ID3D11Texture2D::cast<ID3D11Resource>(copy source)",
        message: err.to_string(),
    })?;
    let dst_resource: ID3D11Resource = target.cast().map_err(|err| BackendError::WindowsApi {
        func: "ID3D11Texture2D::cast<ID3D11Resource>(copy target)",
        message: err.to_string(),
    })?;
    context.CopyResource(&dst_resource, &src_resource);
    Ok(())
}

#[cfg(windows)]
struct GpuRgbaConverter {
    device: windows::Win32::Graphics::Direct3D11::ID3D11Device,
    context: windows::Win32::Graphics::Direct3D11::ID3D11DeviceContext,
    output: windows::Win32::Graphics::Direct3D11::ID3D11Texture2D,
    render_target: windows::Win32::Graphics::Direct3D11::ID3D11RenderTargetView,
    vertex_shader: windows::Win32::Graphics::Direct3D11::ID3D11VertexShader,
    pixel_shader: windows::Win32::Graphics::Direct3D11::ID3D11PixelShader,
    viewport: windows::Win32::Graphics::Direct3D11::D3D11_VIEWPORT,
}

#[cfg(windows)]
impl GpuRgbaConverter {
    unsafe fn new(
        device: &windows::Win32::Graphics::Direct3D11::ID3D11Device,
        context: &windows::Win32::Graphics::Direct3D11::ID3D11DeviceContext,
        width: u32,
        height: u32,
    ) -> Result<Self, BackendError> {
        use windows::Win32::Graphics::Direct3D11::{
            D3D11_BIND_RENDER_TARGET, D3D11_BIND_SHADER_RESOURCE, D3D11_TEXTURE2D_DESC,
            D3D11_USAGE_DEFAULT,
        };
        use windows::Win32::Graphics::Dxgi::Common::{
            DXGI_FORMAT_R8G8B8A8_UNORM, DXGI_SAMPLE_DESC,
        };

        let desc = D3D11_TEXTURE2D_DESC {
            Width: width.max(1),
            Height: height.max(1),
            MipLevels: 1,
            ArraySize: 1,
            Format: DXGI_FORMAT_R8G8B8A8_UNORM,
            SampleDesc: DXGI_SAMPLE_DESC {
                Count: 1,
                Quality: 0,
            },
            Usage: D3D11_USAGE_DEFAULT,
            BindFlags: (D3D11_BIND_RENDER_TARGET.0 | D3D11_BIND_SHADER_RESOURCE.0) as u32,
            CPUAccessFlags: 0,
            MiscFlags: 0,
        };
        let mut output = None;
        device
            .CreateTexture2D(&desc, None, Some(&mut output))
            .map_err(|err| BackendError::WindowsApi {
                func: "ID3D11Device::CreateTexture2D(RGBA converter)",
                message: err.to_string(),
            })?;
        let output = output.ok_or_else(|| BackendError::WindowsApi {
            func: "CreateTexture2D(RGBA converter)",
            message: "返回空纹理".to_owned(),
        })?;
        let mut render_target = None;
        device
            .CreateRenderTargetView(&output, None, Some(&mut render_target))
            .map_err(|err| BackendError::WindowsApi {
                func: "ID3D11Device::CreateRenderTargetView(RGBA converter)",
                message: err.to_string(),
            })?;
        let render_target = render_target.ok_or_else(|| BackendError::WindowsApi {
            func: "CreateRenderTargetView(RGBA converter)",
            message: "返回空 RTV".to_owned(),
        })?;

        let vs_blob = compile_shader(RGBA_CONVERT_HLSL, b"vs_main\0", b"vs_5_0\0")?;
        let ps_blob = compile_shader(RGBA_CONVERT_HLSL, b"ps_main\0", b"ps_5_0\0")?;
        let mut vertex_shader = None;
        device
            .CreateVertexShader(&vs_blob, None, Some(&mut vertex_shader))
            .map_err(|err| BackendError::WindowsApi {
                func: "ID3D11Device::CreateVertexShader(RGBA converter)",
                message: err.to_string(),
            })?;
        let mut pixel_shader = None;
        device
            .CreatePixelShader(&ps_blob, None, Some(&mut pixel_shader))
            .map_err(|err| BackendError::WindowsApi {
                func: "ID3D11Device::CreatePixelShader(RGBA converter)",
                message: err.to_string(),
            })?;

        Ok(Self {
            device: device.clone(),
            context: context.clone(),
            output,
            render_target,
            vertex_shader: vertex_shader.ok_or_else(|| BackendError::WindowsApi {
                func: "CreateVertexShader(RGBA converter)",
                message: "返回空 VS".to_owned(),
            })?,
            pixel_shader: pixel_shader.ok_or_else(|| BackendError::WindowsApi {
                func: "CreatePixelShader(RGBA converter)",
                message: "返回空 PS".to_owned(),
            })?,
            viewport: windows::Win32::Graphics::Direct3D11::D3D11_VIEWPORT {
                TopLeftX: 0.0,
                TopLeftY: 0.0,
                Width: width as f32,
                Height: height as f32,
                MinDepth: 0.0,
                MaxDepth: 1.0,
            },
        })
    }

    unsafe fn convert(
        &self,
        source: &windows::Win32::Graphics::Direct3D11::ID3D11Texture2D,
    ) -> Result<windows::Win32::Graphics::Direct3D11::ID3D11Texture2D, BackendError> {
        use windows::Win32::Graphics::Direct3D::D3D_PRIMITIVE_TOPOLOGY_TRIANGLELIST;
        use windows::Win32::Graphics::Direct3D11::{
            ID3D11RenderTargetView, ID3D11Resource, ID3D11ShaderResourceView,
        };
        use windows::core::Interface;

        let source_resource: ID3D11Resource =
            source.cast().map_err(|err| BackendError::WindowsApi {
                func: "ID3D11Texture2D::cast<ID3D11Resource>(RGBA source)",
                message: err.to_string(),
            })?;
        let mut srv = None;
        self.device
            .CreateShaderResourceView(&source_resource, None, Some(&mut srv))
            .map_err(|err| BackendError::WindowsApi {
                func: "ID3D11Device::CreateShaderResourceView(RGBA source)",
                message: err.to_string(),
            })?;
        let srv = srv.ok_or_else(|| BackendError::WindowsApi {
            func: "CreateShaderResourceView(RGBA source)",
            message: "返回空 SRV".to_owned(),
        })?;

        self.context.RSSetViewports(Some(&[self.viewport]));
        self.context
            .IASetPrimitiveTopology(D3D_PRIMITIVE_TOPOLOGY_TRIANGLELIST);
        self.context.VSSetShader(&self.vertex_shader, None);
        self.context.PSSetShader(&self.pixel_shader, None);
        self.context.PSSetShaderResources(0, Some(&[Some(srv)]));
        self.context
            .OMSetRenderTargets(Some(&[Some(self.render_target.clone())]), None);
        self.context.Draw(3, 0);
        let empty_srv: [Option<ID3D11ShaderResourceView>; 1] = [None];
        self.context.PSSetShaderResources(0, Some(&empty_srv));
        let empty_rtv: [Option<ID3D11RenderTargetView>; 1] = [None];
        self.context.OMSetRenderTargets(Some(&empty_rtv), None);
        Ok(self.output.clone())
    }

    fn output_texture(&self) -> &windows::Win32::Graphics::Direct3D11::ID3D11Texture2D {
        &self.output
    }
}

#[cfg(windows)]
struct GpuP010Converter {
    device: windows::Win32::Graphics::Direct3D11::ID3D11Device,
    context: windows::Win32::Graphics::Direct3D11::ID3D11DeviceContext,
    luma_target: windows::Win32::Graphics::Direct3D11::ID3D11RenderTargetView,
    chroma_target: windows::Win32::Graphics::Direct3D11::ID3D11RenderTargetView,
    pq_lut: windows::Win32::Graphics::Direct3D11::ID3D11ShaderResourceView,
    vertex_shader: windows::Win32::Graphics::Direct3D11::ID3D11VertexShader,
    luma_shader: windows::Win32::Graphics::Direct3D11::ID3D11PixelShader,
    chroma_shader: windows::Win32::Graphics::Direct3D11::ID3D11PixelShader,
    luma_viewport: windows::Win32::Graphics::Direct3D11::D3D11_VIEWPORT,
    chroma_viewport: windows::Win32::Graphics::Direct3D11::D3D11_VIEWPORT,
}

#[cfg(windows)]
impl GpuP010Converter {
    unsafe fn new(
        device: &windows::Win32::Graphics::Direct3D11::ID3D11Device,
        context: &windows::Win32::Graphics::Direct3D11::ID3D11DeviceContext,
        output: &windows::Win32::Graphics::Direct3D11::ID3D11Texture2D,
        width: u32,
        height: u32,
    ) -> Result<Self, BackendError> {
        use windows::Win32::Graphics::Direct3D11::{
            D3D11_RENDER_TARGET_VIEW_DESC1, D3D11_RENDER_TARGET_VIEW_DESC1_0,
            D3D11_RTV_DIMENSION_TEXTURE2D, D3D11_TEX2D_RTV1, ID3D11Device3, ID3D11RenderTargetView,
            ID3D11RenderTargetView1, ID3D11Resource,
        };
        use windows::Win32::Graphics::Dxgi::Common::{
            DXGI_FORMAT_R16_UNORM, DXGI_FORMAT_R16G16_UNORM,
        };
        use windows::core::Interface;

        let device3: ID3D11Device3 = device.cast().map_err(|err| BackendError::WindowsApi {
            func: "ID3D11Device::cast<ID3D11Device3>(P010 plane converter)",
            message: err.to_string(),
        })?;
        let output_resource: ID3D11Resource =
            output.cast().map_err(|err| BackendError::WindowsApi {
                func: "ID3D11Texture2D::cast<ID3D11Resource>(P010 output)",
                message: err.to_string(),
            })?;

        let luma_desc = D3D11_RENDER_TARGET_VIEW_DESC1 {
            Format: DXGI_FORMAT_R16_UNORM,
            ViewDimension: D3D11_RTV_DIMENSION_TEXTURE2D,
            Anonymous: D3D11_RENDER_TARGET_VIEW_DESC1_0 {
                Texture2D: D3D11_TEX2D_RTV1 {
                    MipSlice: 0,
                    PlaneSlice: 0,
                },
            },
        };
        let mut luma_target1: Option<ID3D11RenderTargetView1> = None;
        device3
            .CreateRenderTargetView1(&output_resource, Some(&luma_desc), Some(&mut luma_target1))
            .map_err(|err| BackendError::WindowsApi {
                func: "ID3D11Device3::CreateRenderTargetView1(P010 luma)",
                message: err.to_string(),
            })?;
        let luma_target: ID3D11RenderTargetView = luma_target1
            .ok_or_else(|| BackendError::WindowsApi {
                func: "CreateRenderTargetView1(P010 luma)",
                message: "返回空 luma RTV".to_owned(),
            })?
            .cast()
            .map_err(|err| BackendError::WindowsApi {
                func: "ID3D11RenderTargetView1::cast(P010 luma)",
                message: err.to_string(),
            })?;

        let chroma_desc = D3D11_RENDER_TARGET_VIEW_DESC1 {
            Format: DXGI_FORMAT_R16G16_UNORM,
            ViewDimension: D3D11_RTV_DIMENSION_TEXTURE2D,
            Anonymous: D3D11_RENDER_TARGET_VIEW_DESC1_0 {
                Texture2D: D3D11_TEX2D_RTV1 {
                    MipSlice: 0,
                    PlaneSlice: 1,
                },
            },
        };
        let mut chroma_target1: Option<ID3D11RenderTargetView1> = None;
        device3
            .CreateRenderTargetView1(
                &output_resource,
                Some(&chroma_desc),
                Some(&mut chroma_target1),
            )
            .map_err(|err| BackendError::WindowsApi {
                func: "ID3D11Device3::CreateRenderTargetView1(P010 chroma)",
                message: err.to_string(),
            })?;
        let chroma_target: ID3D11RenderTargetView = chroma_target1
            .ok_or_else(|| BackendError::WindowsApi {
                func: "CreateRenderTargetView1(P010 chroma)",
                message: "返回空 chroma RTV".to_owned(),
            })?
            .cast()
            .map_err(|err| BackendError::WindowsApi {
                func: "ID3D11RenderTargetView1::cast(P010 chroma)",
                message: err.to_string(),
            })?;

        let pq_lut = create_pq_lut_srv(device)?;

        let vs_blob = compile_shader(P010_CONVERT_HLSL, b"vs_main\0", b"vs_5_0\0")?;
        let luma_blob = compile_shader(P010_CONVERT_HLSL, b"ps_luma\0", b"ps_5_0\0")?;
        let chroma_blob = compile_shader(P010_CONVERT_HLSL, b"ps_chroma\0", b"ps_5_0\0")?;
        let mut vertex_shader = None;
        device
            .CreateVertexShader(&vs_blob, None, Some(&mut vertex_shader))
            .map_err(|err| BackendError::WindowsApi {
                func: "ID3D11Device::CreateVertexShader(P010 converter)",
                message: err.to_string(),
            })?;
        let mut luma_shader = None;
        device
            .CreatePixelShader(&luma_blob, None, Some(&mut luma_shader))
            .map_err(|err| BackendError::WindowsApi {
                func: "ID3D11Device::CreatePixelShader(P010 luma)",
                message: err.to_string(),
            })?;
        let mut chroma_shader = None;
        device
            .CreatePixelShader(&chroma_blob, None, Some(&mut chroma_shader))
            .map_err(|err| BackendError::WindowsApi {
                func: "ID3D11Device::CreatePixelShader(P010 chroma)",
                message: err.to_string(),
            })?;

        Ok(Self {
            device: device.clone(),
            context: context.clone(),
            luma_target,
            chroma_target,
            pq_lut,
            vertex_shader: vertex_shader.ok_or_else(|| BackendError::WindowsApi {
                func: "CreateVertexShader(P010 converter)",
                message: "返回空 VS".to_owned(),
            })?,
            luma_shader: luma_shader.ok_or_else(|| BackendError::WindowsApi {
                func: "CreatePixelShader(P010 luma)",
                message: "返回空 luma PS".to_owned(),
            })?,
            chroma_shader: chroma_shader.ok_or_else(|| BackendError::WindowsApi {
                func: "CreatePixelShader(P010 chroma)",
                message: "返回空 chroma PS".to_owned(),
            })?,
            luma_viewport: windows::Win32::Graphics::Direct3D11::D3D11_VIEWPORT {
                TopLeftX: 0.0,
                TopLeftY: 0.0,
                Width: width as f32,
                Height: height as f32,
                MinDepth: 0.0,
                MaxDepth: 1.0,
            },
            chroma_viewport: windows::Win32::Graphics::Direct3D11::D3D11_VIEWPORT {
                TopLeftX: 0.0,
                TopLeftY: 0.0,
                Width: (width / 2).max(1) as f32,
                Height: (height / 2).max(1) as f32,
                MinDepth: 0.0,
                MaxDepth: 1.0,
            },
        })
    }

    unsafe fn convert(
        &self,
        source: &windows::Win32::Graphics::Direct3D11::ID3D11Texture2D,
    ) -> Result<(), BackendError> {
        use windows::Win32::Graphics::Direct3D::D3D_PRIMITIVE_TOPOLOGY_TRIANGLELIST;
        use windows::Win32::Graphics::Direct3D11::{
            ID3D11RenderTargetView, ID3D11Resource, ID3D11ShaderResourceView,
        };
        use windows::core::Interface;

        let source_resource: ID3D11Resource =
            source.cast().map_err(|err| BackendError::WindowsApi {
                func: "ID3D11Texture2D::cast<ID3D11Resource>(P010 source)",
                message: err.to_string(),
            })?;
        let mut srv = None;
        self.device
            .CreateShaderResourceView(&source_resource, None, Some(&mut srv))
            .map_err(|err| BackendError::WindowsApi {
                func: "ID3D11Device::CreateShaderResourceView(P010 source)",
                message: err.to_string(),
            })?;
        let srv = srv.ok_or_else(|| BackendError::WindowsApi {
            func: "CreateShaderResourceView(P010 source)",
            message: "返回空 SRV".to_owned(),
        })?;

        self.context
            .IASetPrimitiveTopology(D3D_PRIMITIVE_TOPOLOGY_TRIANGLELIST);
        self.context.VSSetShader(&self.vertex_shader, None);
        self.context
            .PSSetShaderResources(0, Some(&[Some(srv), Some(self.pq_lut.clone())]));

        self.context.RSSetViewports(Some(&[self.luma_viewport]));
        self.context.PSSetShader(&self.luma_shader, None);
        self.context
            .OMSetRenderTargets(Some(&[Some(self.luma_target.clone())]), None);
        self.context.Draw(3, 0);

        self.context.RSSetViewports(Some(&[self.chroma_viewport]));
        self.context.PSSetShader(&self.chroma_shader, None);
        self.context
            .OMSetRenderTargets(Some(&[Some(self.chroma_target.clone())]), None);
        self.context.Draw(3, 0);

        let empty_srv: [Option<ID3D11ShaderResourceView>; 2] = [None, None];
        self.context.PSSetShaderResources(0, Some(&empty_srv));
        let empty_rtv: [Option<ID3D11RenderTargetView>; 1] = [None];
        self.context.OMSetRenderTargets(Some(&empty_rtv), None);
        Ok(())
    }
}

#[cfg(windows)]
unsafe fn create_pq_lut_srv(
    device: &windows::Win32::Graphics::Direct3D11::ID3D11Device,
) -> Result<windows::Win32::Graphics::Direct3D11::ID3D11ShaderResourceView, BackendError> {
    use windows::Win32::Graphics::Direct3D11::{
        D3D11_BIND_SHADER_RESOURCE, D3D11_SUBRESOURCE_DATA, D3D11_TEXTURE1D_DESC,
        D3D11_USAGE_IMMUTABLE, ID3D11Resource,
    };
    use windows::Win32::Graphics::Dxgi::Common::DXGI_FORMAT_R16_UNORM;
    use windows::core::Interface;

    const LUT_SIZE: usize = 4096;
    let mut data = [0u16; LUT_SIZE];
    for (i, value) in data.iter_mut().enumerate() {
        let nits = (i as f64 / (LUT_SIZE - 1) as f64) * 10_000.0;
        let pq = pq_oetf_scalar(nits);
        *value = (pq.clamp(0.0, 1.0) * 65535.0).round() as u16;
    }

    let desc = D3D11_TEXTURE1D_DESC {
        Width: LUT_SIZE as u32,
        MipLevels: 1,
        ArraySize: 1,
        Format: DXGI_FORMAT_R16_UNORM,
        Usage: D3D11_USAGE_IMMUTABLE,
        BindFlags: D3D11_BIND_SHADER_RESOURCE.0 as u32,
        CPUAccessFlags: 0,
        MiscFlags: 0,
    };
    let initial = D3D11_SUBRESOURCE_DATA {
        pSysMem: data.as_ptr() as *const c_void,
        SysMemPitch: (LUT_SIZE * std::mem::size_of::<u16>()) as u32,
        SysMemSlicePitch: 0,
    };
    let mut texture = None;
    device
        .CreateTexture1D(&desc, Some(&initial), Some(&mut texture))
        .map_err(|err| BackendError::WindowsApi {
            func: "ID3D11Device::CreateTexture1D(PQ LUT)",
            message: err.to_string(),
        })?;
    let texture = texture.ok_or_else(|| BackendError::WindowsApi {
        func: "CreateTexture1D(PQ LUT)",
        message: "返回空纹理".to_owned(),
    })?;
    let resource: ID3D11Resource = texture.cast().map_err(|err| BackendError::WindowsApi {
        func: "ID3D11Texture1D::cast<ID3D11Resource>(PQ LUT)",
        message: err.to_string(),
    })?;
    let mut srv = None;
    device
        .CreateShaderResourceView(&resource, None, Some(&mut srv))
        .map_err(|err| BackendError::WindowsApi {
            func: "ID3D11Device::CreateShaderResourceView(PQ LUT)",
            message: err.to_string(),
        })?;
    srv.ok_or_else(|| BackendError::WindowsApi {
        func: "CreateShaderResourceView(PQ LUT)",
        message: "返回空 SRV".to_owned(),
    })
}

fn pq_oetf_scalar(nits: f64) -> f64 {
    let m1 = 0.159_301_757_812_5;
    let m2 = 78.84375;
    let c1 = 0.8359375;
    let c2 = 18.851_562_5;
    let c3 = 18.6875;
    let x = (nits / 10_000.0).clamp(0.0, 1.0).powf(m1);
    ((c1 + c2 * x) / (1.0 + c3 * x)).powf(m2)
}

#[cfg(windows)]
const P010_CONVERT_HLSL: &str = r#"
Texture2D<float4> src_tex : register(t0);
Texture1D<float> pq_lut : register(t1);

float4 vs_main(uint id : SV_VertexID) : SV_Position {
    float2 pos[3] = {
        float2(-1.0,  1.0),
        float2( 3.0,  1.0),
        float2(-1.0, -3.0)
    };
    return float4(pos[id], 0.0, 1.0);
}

float pq_oetf(float sc_rgb_linear) {
    uint idx = (uint)(saturate(sc_rgb_linear / 125.0) * 4095.0 + 0.5);
    return pq_lut.Load(int2(idx, 0));
}

float3 sc_rgb_to_pq2020(float3 sc_rgb) {
    float3 rgb709 = max(sc_rgb, 0.0);
    float3 xyz = float3(
        0.4123908 * rgb709.r + 0.3575843 * rgb709.g + 0.1804808 * rgb709.b,
        0.2126390 * rgb709.r + 0.7151687 * rgb709.g + 0.0721923 * rgb709.b,
        0.0193308 * rgb709.r + 0.1191948 * rgb709.g + 0.9505322 * rgb709.b
    );
    float3 bt2020 = max(float3(
         1.7166512 * xyz.x - 0.3556708 * xyz.y - 0.2533663 * xyz.z,
        -0.6666844 * xyz.x + 1.6164812 * xyz.y + 0.0157685 * xyz.z,
         0.0176399 * xyz.x - 0.0427706 * xyz.y + 0.9421031 * xyz.z
    ), 0.0);
    return float3(
        pq_oetf(bt2020.r),
        pq_oetf(bt2020.g),
        pq_oetf(bt2020.b)
    );
}

float3 pq2020_to_ycbcr(float3 rgb) {
    const float kr = 0.2627;
    const float kb = 0.0593;
    const float kg = 1.0 - kr - kb;
    float y = kr * rgb.r + kg * rgb.g + kb * rgb.b;
    float cb = (rgb.b - y) / (2.0 * (1.0 - kb)) + 0.5;
    float cr = (rgb.r - y) / (2.0 * (1.0 - kr)) + 0.5;
    return saturate(float3(y, cb, cr));
}

float3 load_ycbcr(uint2 pixel) {
    uint width;
    uint height;
    src_tex.GetDimensions(width, height);
    pixel = min(pixel, uint2(width - 1, height - 1));
    return pq2020_to_ycbcr(sc_rgb_to_pq2020(src_tex.Load(int3(pixel, 0)).rgb));
}

float4 ps_luma(float4 pos : SV_Position) : SV_Target {
    return load_ycbcr(uint2(pos.xy)).xxxx;
}

float4 ps_chroma(float4 pos : SV_Position) : SV_Target {
    uint2 base_pixel = uint2(pos.xy) * 2;
    float3 c = load_ycbcr(base_pixel + uint2(1, 1));
    return float4(c.yz, 0.0, 1.0);
}
"#;

#[cfg(windows)]
struct VideoProcessorBlitter {
    input_view: windows::Win32::Graphics::Direct3D11::ID3D11VideoProcessorInputView,
    output_view: windows::Win32::Graphics::Direct3D11::ID3D11VideoProcessorOutputView,
}

#[cfg(windows)]
impl VideoProcessorBlitter {
    unsafe fn new(
        video_device: &windows::Win32::Graphics::Direct3D11::ID3D11VideoDevice,
        enumerator: &windows::Win32::Graphics::Direct3D11::ID3D11VideoProcessorEnumerator,
        source: &windows::Win32::Graphics::Direct3D11::ID3D11Texture2D,
        target: &windows::Win32::Graphics::Direct3D11::ID3D11Texture2D,
    ) -> Result<Self, BackendError> {
        use windows::Win32::Graphics::Direct3D11::{
            D3D11_TEX2D_VPIV, D3D11_TEX2D_VPOV, D3D11_VIDEO_PROCESSOR_INPUT_VIEW_DESC,
            D3D11_VIDEO_PROCESSOR_INPUT_VIEW_DESC_0, D3D11_VIDEO_PROCESSOR_OUTPUT_VIEW_DESC,
            D3D11_VIDEO_PROCESSOR_OUTPUT_VIEW_DESC_0, D3D11_VPIV_DIMENSION_TEXTURE2D,
            D3D11_VPOV_DIMENSION_TEXTURE2D, ID3D11Resource, ID3D11VideoProcessorInputView,
            ID3D11VideoProcessorOutputView,
        };
        use windows::core::Interface;

        let source_resource: ID3D11Resource =
            source.cast().map_err(|err| BackendError::WindowsApi {
                func: "ID3D11Texture2D::cast<ID3D11Resource>(cached VP source)",
                message: err.to_string(),
            })?;
        let target_resource: ID3D11Resource =
            target.cast().map_err(|err| BackendError::WindowsApi {
                func: "ID3D11Texture2D::cast<ID3D11Resource>(cached VP target)",
                message: err.to_string(),
            })?;

        let input_desc = D3D11_VIDEO_PROCESSOR_INPUT_VIEW_DESC {
            FourCC: 0,
            ViewDimension: D3D11_VPIV_DIMENSION_TEXTURE2D,
            Anonymous: D3D11_VIDEO_PROCESSOR_INPUT_VIEW_DESC_0 {
                Texture2D: D3D11_TEX2D_VPIV {
                    MipSlice: 0,
                    ArraySlice: 0,
                },
            },
        };
        let mut input_view: Option<ID3D11VideoProcessorInputView> = None;
        video_device
            .CreateVideoProcessorInputView(
                &source_resource,
                enumerator,
                &input_desc,
                Some(&mut input_view),
            )
            .map_err(|err| BackendError::WindowsApi {
                func: "ID3D11VideoDevice::CreateVideoProcessorInputView(cached)",
                message: err.to_string(),
            })?;
        let input_view = input_view.ok_or_else(|| BackendError::WindowsApi {
            func: "CreateVideoProcessorInputView(cached)",
            message: "返回空 input view".to_owned(),
        })?;

        let output_desc = D3D11_VIDEO_PROCESSOR_OUTPUT_VIEW_DESC {
            ViewDimension: D3D11_VPOV_DIMENSION_TEXTURE2D,
            Anonymous: D3D11_VIDEO_PROCESSOR_OUTPUT_VIEW_DESC_0 {
                Texture2D: D3D11_TEX2D_VPOV { MipSlice: 0 },
            },
        };
        let mut output_view: Option<ID3D11VideoProcessorOutputView> = None;
        video_device
            .CreateVideoProcessorOutputView(
                &target_resource,
                enumerator,
                &output_desc,
                Some(&mut output_view),
            )
            .map_err(|err| BackendError::WindowsApi {
                func: "ID3D11VideoDevice::CreateVideoProcessorOutputView(cached)",
                message: err.to_string(),
            })?;
        let output_view = output_view.ok_or_else(|| BackendError::WindowsApi {
            func: "CreateVideoProcessorOutputView(cached)",
            message: "返回空 output view".to_owned(),
        })?;

        Ok(Self {
            input_view,
            output_view,
        })
    }

    unsafe fn blit(
        &self,
        video_context: &windows::Win32::Graphics::Direct3D11::ID3D11VideoContext,
        processor: &windows::Win32::Graphics::Direct3D11::ID3D11VideoProcessor,
    ) -> Result<(), BackendError> {
        use std::mem::ManuallyDrop;
        use windows::Win32::Graphics::Direct3D11::D3D11_VIDEO_PROCESSOR_STREAM;

        let mut stream = D3D11_VIDEO_PROCESSOR_STREAM {
            Enable: windows::core::BOOL(1),
            OutputIndex: 0,
            InputFrameOrField: 0,
            PastFrames: 0,
            FutureFrames: 0,
            ppPastSurfaces: ptr::null_mut(),
            pInputSurface: ManuallyDrop::new(Some(self.input_view.clone())),
            ppFutureSurfaces: ptr::null_mut(),
            ppPastSurfacesRight: ptr::null_mut(),
            pInputSurfaceRight: ManuallyDrop::new(None),
            ppFutureSurfacesRight: ptr::null_mut(),
        };
        let blt_result = video_context.VideoProcessorBlt(
            processor,
            &self.output_view,
            0,
            std::slice::from_ref(&stream),
        );
        ManuallyDrop::drop(&mut stream.pInputSurface);
        blt_result.map_err(|err| BackendError::WindowsApi {
            func: "ID3D11VideoContext::VideoProcessorBlt(cached)",
            message: err.to_string(),
        })
    }
}

#[cfg(windows)]
const RGBA_CONVERT_HLSL: &str = r#"
Texture2D<float4> src_tex : register(t0);

float4 vs_main(uint id : SV_VertexID) : SV_Position {
    float2 pos[3] = {
        float2(-1.0,  1.0),
        float2( 3.0,  1.0),
        float2(-1.0, -3.0)
    };
    return float4(pos[id], 0.0, 1.0);
}

float4 ps_main(float4 pos : SV_Position) : SV_Target {
    return saturate(src_tex.Load(int3(uint2(pos.xy), 0)));
}
"#;

#[cfg(windows)]
unsafe fn compile_shader(
    source: &str,
    entry: &[u8],
    target: &[u8],
) -> Result<Vec<u8>, BackendError> {
    use windows::Win32::Graphics::Direct3D::Fxc::D3DCompile;
    use windows::Win32::Graphics::Direct3D::{ID3DBlob, ID3DInclude};
    use windows::core::PCSTR;

    let mut code: Option<ID3DBlob> = None;
    let mut errors: Option<ID3DBlob> = None;
    let result = D3DCompile(
        source.as_ptr() as *const c_void,
        source.len(),
        PCSTR::null(),
        None,
        Option::<&ID3DInclude>::None,
        PCSTR(entry.as_ptr()),
        PCSTR(target.as_ptr()),
        0,
        0,
        &mut code,
        Some(&mut errors),
    );
    if let Err(err) = result {
        let message = if let Some(errors) = errors {
            let ptr = errors.GetBufferPointer() as *const u8;
            let len = errors.GetBufferSize();
            String::from_utf8_lossy(std::slice::from_raw_parts(ptr, len)).to_string()
        } else {
            err.to_string()
        };
        return Err(BackendError::WindowsApi {
            func: "D3DCompile",
            message,
        });
    }
    let code = code.ok_or_else(|| BackendError::WindowsApi {
        func: "D3DCompile",
        message: "返回空 shader blob".to_owned(),
    })?;
    let ptr = code.GetBufferPointer() as *const u8;
    let len = code.GetBufferSize();
    Ok(std::slice::from_raw_parts(ptr, len).to_vec())
}

#[cfg(windows)]
unsafe fn process_with_video_processor(
    video_device: &windows::Win32::Graphics::Direct3D11::ID3D11VideoDevice,
    video_context: &windows::Win32::Graphics::Direct3D11::ID3D11VideoContext,
    enumerator: &windows::Win32::Graphics::Direct3D11::ID3D11VideoProcessorEnumerator,
    processor: &windows::Win32::Graphics::Direct3D11::ID3D11VideoProcessor,
    source: &windows::Win32::Graphics::Direct3D11::ID3D11Texture2D,
    target: &windows::Win32::Graphics::Direct3D11::ID3D11Texture2D,
) -> Result<(), BackendError> {
    use std::mem::ManuallyDrop;
    use windows::Win32::Graphics::Direct3D11::{
        D3D11_TEX2D_VPIV, D3D11_TEX2D_VPOV, D3D11_VIDEO_PROCESSOR_INPUT_VIEW_DESC,
        D3D11_VIDEO_PROCESSOR_INPUT_VIEW_DESC_0, D3D11_VIDEO_PROCESSOR_OUTPUT_VIEW_DESC,
        D3D11_VIDEO_PROCESSOR_OUTPUT_VIEW_DESC_0, D3D11_VIDEO_PROCESSOR_STREAM,
        D3D11_VPIV_DIMENSION_TEXTURE2D, D3D11_VPOV_DIMENSION_TEXTURE2D, ID3D11Resource,
        ID3D11VideoProcessorInputView, ID3D11VideoProcessorOutputView,
    };
    use windows::core::Interface;

    let source_resource: ID3D11Resource =
        source.cast().map_err(|err| BackendError::WindowsApi {
            func: "ID3D11Texture2D::cast<ID3D11Resource>(source)",
            message: err.to_string(),
        })?;
    let target_resource: ID3D11Resource =
        target.cast().map_err(|err| BackendError::WindowsApi {
            func: "ID3D11Texture2D::cast<ID3D11Resource>(target)",
            message: err.to_string(),
        })?;

    let input_desc = D3D11_VIDEO_PROCESSOR_INPUT_VIEW_DESC {
        FourCC: 0,
        ViewDimension: D3D11_VPIV_DIMENSION_TEXTURE2D,
        Anonymous: D3D11_VIDEO_PROCESSOR_INPUT_VIEW_DESC_0 {
            Texture2D: D3D11_TEX2D_VPIV {
                MipSlice: 0,
                ArraySlice: 0,
            },
        },
    };
    let mut input_view: Option<ID3D11VideoProcessorInputView> = None;
    video_device
        .CreateVideoProcessorInputView(
            &source_resource,
            enumerator,
            &input_desc,
            Some(&mut input_view),
        )
        .map_err(|err| BackendError::WindowsApi {
            func: "ID3D11VideoDevice::CreateVideoProcessorInputView",
            message: err.to_string(),
        })?;
    let input_view = input_view.ok_or_else(|| BackendError::WindowsApi {
        func: "CreateVideoProcessorInputView",
        message: "返回空 input view".to_owned(),
    })?;

    let output_desc = D3D11_VIDEO_PROCESSOR_OUTPUT_VIEW_DESC {
        ViewDimension: D3D11_VPOV_DIMENSION_TEXTURE2D,
        Anonymous: D3D11_VIDEO_PROCESSOR_OUTPUT_VIEW_DESC_0 {
            Texture2D: D3D11_TEX2D_VPOV { MipSlice: 0 },
        },
    };
    let mut output_view: Option<ID3D11VideoProcessorOutputView> = None;
    video_device
        .CreateVideoProcessorOutputView(
            &target_resource,
            enumerator,
            &output_desc,
            Some(&mut output_view),
        )
        .map_err(|err| BackendError::WindowsApi {
            func: "ID3D11VideoDevice::CreateVideoProcessorOutputView",
            message: err.to_string(),
        })?;
    let output_view = output_view.ok_or_else(|| BackendError::WindowsApi {
        func: "CreateVideoProcessorOutputView",
        message: "返回空 output view".to_owned(),
    })?;

    let mut stream = D3D11_VIDEO_PROCESSOR_STREAM {
        Enable: windows::core::BOOL(1),
        OutputIndex: 0,
        InputFrameOrField: 0,
        PastFrames: 0,
        FutureFrames: 0,
        ppPastSurfaces: ptr::null_mut(),
        pInputSurface: ManuallyDrop::new(Some(input_view)),
        ppFutureSurfaces: ptr::null_mut(),
        ppPastSurfacesRight: ptr::null_mut(),
        pInputSurfaceRight: ManuallyDrop::new(None),
        ppFutureSurfacesRight: ptr::null_mut(),
    };
    let blt_result =
        video_context.VideoProcessorBlt(processor, &output_view, 0, std::slice::from_ref(&stream));
    ManuallyDrop::drop(&mut stream.pInputSurface);
    blt_result.map_err(|err| BackendError::WindowsApi {
        func: "ID3D11VideoContext::VideoProcessorBlt",
        message: err.to_string(),
    })
}

unsafe fn release_pending_surface(surface: Option<*mut MfxFrameSurface1>) {
    if let Some(surface) = surface
        && !surface.is_null()
        && !(*surface).FrameInterface.is_null()
    {
        let _ = ((*(*surface).FrameInterface).Release)(surface);
    }
}

#[cfg(windows)]
struct RecordThreadPriorityGuard {
    handle: windows::Win32::Foundation::HANDLE,
    previous: i32,
}

#[cfg(windows)]
impl RecordThreadPriorityGuard {
    unsafe fn raise(notes: &mut Vec<String>) -> Option<Self> {
        use windows::Win32::System::Threading::{
            GetCurrentThread, GetThreadPriority, SetThreadPriority, THREAD_PRIORITY_HIGHEST,
        };

        let handle = GetCurrentThread();
        let previous = GetThreadPriority(handle);
        match SetThreadPriority(handle, THREAD_PRIORITY_HIGHEST) {
            Ok(()) => {
                notes.push(
                    "录制线程临时提升到 THREAD_PRIORITY_HIGHEST 以降低 144Hz DDA 抖动".to_owned(),
                );
                Some(Self { handle, previous })
            }
            Err(err) => {
                notes.push(format!("录制线程提权失败，继续使用当前优先级：{}", err));
                None
            }
        }
    }
}

#[cfg(windows)]
impl Drop for RecordThreadPriorityGuard {
    fn drop(&mut self) {
        unsafe {
            let _ = windows::Win32::System::Threading::SetThreadPriority(
                self.handle,
                windows::Win32::System::Threading::THREAD_PRIORITY(self.previous),
            );
        }
    }
}

fn duration_to_90k(duration: std::time::Duration) -> u64 {
    (duration.as_secs_f64() * VIDEO_CLOCK_HZ as f64).round() as u64
}

const fn align16(value: u16) -> u16 {
    value.div_ceil(16) * 16
}

unsafe fn encode_one_surface(
    api: &VplApi,
    session: MfxSession,
    surface: *mut MfxFrameSurface1,
) -> (i32, i32, u32) {
    const BITSTREAM_BYTES: usize = 128 * 1024 * 1024;

    let mut storage = vec![0u8; BITSTREAM_BYTES + 31];
    let aligned = ((storage.as_mut_ptr() as usize + 31) & !31usize) as *mut u8;
    let mut bitstream: MfxBitstream = std::mem::zeroed();
    bitstream.CodecId = MFX_CODEC_HEVC;
    bitstream.Data = aligned;
    bitstream.MaxLength = BITSTREAM_BYTES as u32;

    let mut syncp: MfxSyncPoint = ptr::null_mut();
    let mut encode_status = (api.mfx_video_encode_frame_async)(
        session,
        ptr::null_mut(),
        surface,
        &mut bitstream,
        &mut syncp,
    );

    if encode_status == MFX_WRN_DEVICE_BUSY {
        std::thread::sleep(std::time::Duration::from_millis(1));
        syncp = ptr::null_mut();
        bitstream.DataLength = 0;
        encode_status = (api.mfx_video_encode_frame_async)(
            session,
            ptr::null_mut(),
            surface,
            &mut bitstream,
            &mut syncp,
        );
    }

    let sync_status = if !syncp.is_null() && encode_status >= MFX_ERR_NONE {
        (api.mfx_video_core_sync_operation)(session, syncp, 60_000)
    } else {
        i32::MIN
    };

    (encode_status, sync_status, bitstream.DataLength)
}

unsafe fn set_loader_u32(api: &VplApi, loader: MfxLoader, name: *const u8, value: u32) -> i32 {
    let cfg = (api.mfx_create_config)(loader);
    if cfg.is_null() {
        return -2;
    }
    let variant = MfxVariant {
        Version: MfxStructVersion {
            version: MFX_VARIANT_VERSION,
        },
        Type: MFX_VARIANT_TYPE_U32,
        Data: value as u64,
    };
    (api.mfx_set_config_filter_property)(cfg, name, variant)
}

unsafe fn set_loader_ptr(api: &VplApi, loader: MfxLoader, name: *const u8, value: MfxHDL) -> i32 {
    let cfg = (api.mfx_create_config)(loader);
    if cfg.is_null() {
        return -2;
    }
    let variant = MfxVariant {
        Version: MfxStructVersion {
            version: MFX_VARIANT_VERSION,
        },
        Type: MFX_VARIANT_TYPE_PTR,
        Data: value as usize as u64,
    };
    (api.mfx_set_config_filter_property)(cfg, name, variant)
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
    param.AsyncDepth = VPL_RECORD_ASYNC_DEPTH;
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
    param.mfx.LowPower = MFX_CODINGOPTION_ON;
    param.mfx.TargetUsage = 7;
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
type MfxConfig = *mut c_void;
type MfxSession = *mut c_void;
type MfxHDL = *mut c_void;
type MfxSyncPoint = *mut c_void;

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
struct MfxVariant {
    Version: MfxStructVersion,
    Type: u32,
    Data: u64,
}

#[repr(C)]
#[derive(Debug, Clone, Copy)]
struct MfxHDLPair {
    first: MfxHDL,
    second: MfxHDL,
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
#[derive(Debug, Clone, Copy)]
struct MfxFrameAllocRequest {
    AllocId: u32,
    reserved3: [u32; 3],
    Info: MfxFrameInfo,
    Type: u16,
    NumFrameMin: u16,
    NumFrameSuggested: u16,
    reserved2: u16,
}

#[repr(C)]
#[derive(Debug, Clone, Copy)]
struct MfxFrameData {
    ExtParam: *mut *mut c_void,
    NumExtParam: u16,
    reserved: [u16; 9],
    MemType: u16,
    PitchHigh: u16,
    TimeStamp: u64,
    FrameOrder: u32,
    Locked: u16,
    Pitch: u16,
    Y: *mut u8,
    UV: *mut u8,
    V: *mut u8,
    A: *mut u8,
    MemId: MfxHDL,
    Corrupted: u16,
    DataFlag: u16,
    reserved4: [u16; 2],
}

#[repr(C)]
#[derive(Debug, Clone, Copy)]
struct MfxFrameSurface1 {
    FrameInterface: *mut MfxFrameSurfaceInterface,
    Version: MfxStructVersion,
    reserved1: [u16; 3],
    Info: MfxFrameInfo,
    Data: MfxFrameData,
}

#[repr(C)]
#[derive(Clone, Copy)]
struct MfxFrameSurfaceInterface {
    Context: MfxHDL,
    Version: MfxStructVersion,
    reserved1: [u16; 3],
    AddRef: unsafe extern "C" fn(*mut MfxFrameSurface1) -> i32,
    Release: unsafe extern "C" fn(*mut MfxFrameSurface1) -> i32,
    GetRefCounter: unsafe extern "C" fn(*mut MfxFrameSurface1, *mut u32) -> i32,
    Map: unsafe extern "C" fn(*mut MfxFrameSurface1, u32) -> i32,
    Unmap: unsafe extern "C" fn(*mut MfxFrameSurface1) -> i32,
    GetNativeHandle: unsafe extern "C" fn(*mut MfxFrameSurface1, *mut MfxHDL, *mut u32) -> i32,
    GetDeviceHandle: unsafe extern "C" fn(*mut MfxFrameSurface1, *mut MfxHDL, *mut u32) -> i32,
    Synchronize: unsafe extern "C" fn(*mut MfxFrameSurface1, u32) -> i32,
    OnComplete: unsafe extern "C" fn(i32),
    QueryInterface: unsafe extern "C" fn(*mut MfxFrameSurface1, MfxGuid, *mut MfxHDL) -> i32,
    reserved2: [MfxHDL; 2],
}

impl std::fmt::Debug for MfxFrameSurfaceInterface {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("MfxFrameSurfaceInterface")
            .field("Context", &self.Context)
            .field("Version", &self.Version.version)
            .finish_non_exhaustive()
    }
}

#[repr(C)]
#[derive(Debug, Clone, Copy)]
struct MfxGuid {
    Data: [u8; 16],
}

#[repr(C)]
#[derive(Debug, Clone, Copy)]
struct MfxBitstream {
    EncryptedData: *mut c_void,
    ExtParam: *mut *mut c_void,
    NumExtParam: u16,
    reserved1: u16,
    CodecId: u32,
    DecodeTimeStamp: i64,
    TimeStamp: u64,
    Data: *mut u8,
    DataOffset: u32,
    DataLength: u32,
    MaxLength: u32,
    PicStruct: u16,
    FrameType: u16,
    DataFlag: u16,
    reserved2: u16,
}

#[repr(C)]
#[derive(Clone, Copy)]
struct MfxSurfaceHeader {
    SurfaceType: u32,
    SurfaceFlags: u32,
    StructSize: u32,
    NumExtParam: u16,
    ExtParam: *mut *mut c_void,
    reserved: [u32; 6],
}

#[repr(C)]
#[derive(Clone, Copy)]
struct MfxSurfaceInterface {
    Header: MfxSurfaceHeader,
    Version: MfxStructVersion,
    Context: MfxHDL,
    AddRef: Option<unsafe extern "C" fn(*mut MfxSurfaceInterface) -> i32>,
    Release: Option<unsafe extern "C" fn(*mut MfxSurfaceInterface) -> i32>,
    GetRefCounter: Option<unsafe extern "C" fn(*mut MfxSurfaceInterface, *mut u32) -> i32>,
    Synchronize: Option<unsafe extern "C" fn(*mut MfxSurfaceInterface, u32) -> i32>,
    reserved: [MfxHDL; 11],
}

#[repr(C)]
#[derive(Clone, Copy)]
struct MfxSurfaceD3D11Tex2D {
    SurfaceInterface: MfxSurfaceInterface,
    texture2D: MfxHDL,
    reserved: [MfxHDL; 7],
}

#[repr(C)]
#[derive(Clone, Copy)]
struct MfxMemoryInterface {
    Context: MfxHDL,
    Version: MfxStructVersion,
    ImportFrameSurface: unsafe extern "C" fn(
        *mut MfxMemoryInterface,
        u32,
        *mut MfxSurfaceHeader,
        *mut *mut MfxFrameSurface1,
    ) -> i32,
    reserved: [MfxHDL; 16],
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
        assert_eq!(std::mem::size_of::<MfxBitstream>(), 72);
        assert_eq!(std::mem::size_of::<MfxFrameData>(), 96);
        assert_eq!(std::mem::size_of::<MfxFrameSurface1>(), 184);
        assert_eq!(std::mem::size_of::<MfxSurfaceHeader>(), 48);
        assert_eq!(std::mem::size_of::<MfxSurfaceInterface>(), 184);
        assert_eq!(std::mem::size_of::<MfxSurfaceD3D11Tex2D>(), 248);
        assert_eq!(std::mem::size_of::<MfxMemoryInterface>(), 152);
    }

    #[test]
    fn dll_candidates_include_env_first() {
        assert!(candidate_dlls().iter().any(|p| p.ends_with("libvpl-2.dll")));
    }
}
