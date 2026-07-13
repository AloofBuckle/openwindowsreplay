use super::*;

pub(super) struct VplApi {
    pub(super) _library: Library,
    pub(super) mfx_load: unsafe extern "C" fn() -> MfxLoader,
    pub(super) mfx_unload: unsafe extern "C" fn(MfxLoader),
    pub(super) mfx_enum_implementations:
        unsafe extern "C" fn(MfxLoader, u32, u32, *mut MfxHDL) -> i32,
    pub(super) mfx_release_impl_description: unsafe extern "C" fn(MfxLoader, MfxHDL) -> i32,
    pub(super) mfx_create_session: unsafe extern "C" fn(MfxLoader, u32, *mut MfxSession) -> i32,
    pub(super) mfx_close: unsafe extern "C" fn(MfxSession) -> i32,
    pub(super) mfx_video_encode_query:
        unsafe extern "C" fn(MfxSession, *mut MfxVideoParam, *mut MfxVideoParam) -> i32,
    pub(super) mfx_video_encode_query_iosurf:
        unsafe extern "C" fn(MfxSession, *mut MfxVideoParam, *mut MfxFrameAllocRequest) -> i32,
    pub(super) mfx_video_encode_init: unsafe extern "C" fn(MfxSession, *mut MfxVideoParam) -> i32,
    pub(super) mfx_memory_get_surface_for_encode:
        unsafe extern "C" fn(MfxSession, *mut *mut MfxFrameSurface1) -> i32,
    pub(super) mfx_video_encode_frame_async: unsafe extern "C" fn(
        MfxSession,
        *mut c_void,
        *mut MfxFrameSurface1,
        *mut MfxBitstream,
        *mut MfxSyncPoint,
    ) -> i32,
    pub(super) mfx_video_core_sync_operation:
        unsafe extern "C" fn(MfxSession, MfxSyncPoint, u32) -> i32,
    pub(super) mfx_video_encode_close: unsafe extern "C" fn(MfxSession) -> i32,
}

impl VplApi {
    pub(super) fn load() -> Result<(Self, PathBuf), String> {
        let mut attempts = Vec::new();
        for path in candidate_dlls() {
            let result = unsafe { load_library(&path) };
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
                            mfx_enum_implementations,
                            mfx_release_impl_description,
                            mfx_create_session,
                            mfx_close,
                            mfx_video_encode_query,
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
                Err(err) => {
                    attempts.push(format!("{}: {err}", path.display()));
                    if path == AppConfig::vpl_dll_path() {
                        return Err(format!(
                            "内嵌 oneVPL dispatcher 加载失败，不允许回退系统 DLL：{}",
                            attempts.join(" | ")
                        ));
                    }
                }
            }
        }
        Err(format!(
            "未能加载 oneVPL DLL；尝试路径: {}",
            attempts.join(" | ")
        ))
    }
}

#[cfg(windows)]
pub(super) unsafe fn load_library(path: &Path) -> Result<Library, libloading::Error> {
    use libloading::os::windows::{
        LOAD_LIBRARY_SEARCH_DEFAULT_DIRS, LOAD_LIBRARY_SEARCH_DLL_LOAD_DIR,
        Library as WindowsLibrary,
    };

    if path.is_absolute() {
        WindowsLibrary::load_with_flags(
            path,
            LOAD_LIBRARY_SEARCH_DLL_LOAD_DIR | LOAD_LIBRARY_SEARCH_DEFAULT_DIRS,
        )
        .map(Into::into)
    } else {
        Library::new(path)
    }
}

#[cfg(not(windows))]
pub(super) unsafe fn load_library(path: &Path) -> Result<Library, libloading::Error> {
    Library::new(path)
}

pub(super) fn candidate_dlls() -> Vec<PathBuf> {
    let mut out = Vec::new();
    if let Ok(path) = std::env::var("RUSTREPLAY_VPL_DLL") {
        out.push(PathBuf::from(path));
    }
    out.push(AppConfig::vpl_dll_path());
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

pub(super) type MfxLoader = *mut c_void;
pub(super) type MfxSession = *mut c_void;
pub(super) type MfxHDL = *mut c_void;
pub(super) type MfxSyncPoint = *mut c_void;

#[repr(C)]
#[derive(Debug, Clone, Copy)]
pub(super) struct MfxStructVersion {
    pub(super) version: u16,
}

#[repr(C)]
#[derive(Debug, Clone, Copy)]
pub(super) struct MfxExtBuffer {
    pub(super) BufferId: u32,
    pub(super) BufferSz: u32,
}

#[repr(C)]
#[derive(Debug, Clone, Copy)]
pub(super) struct MfxEncodeCtrl {
    pub(super) Header: MfxExtBuffer,
    pub(super) reserved: [u32; 4],
    pub(super) reserved1: u16,
    pub(super) MfxNalUnitType: u16,
    pub(super) SkipFrame: u16,
    pub(super) QP: u16,
    pub(super) FrameType: u16,
    pub(super) NumExtParam: u16,
    pub(super) NumPayload: u16,
    pub(super) reserved2: u16,
    pub(super) ExtParam: *mut *mut c_void,
    pub(super) Payload: *mut *mut c_void,
}

#[repr(C)]
#[derive(Debug, Clone, Copy)]
pub(super) struct MfxExtVideoSignalInfo {
    pub(super) Header: MfxExtBuffer,
    pub(super) VideoFormat: u16,
    pub(super) VideoFullRange: u16,
    pub(super) ColourDescriptionPresent: u16,
    pub(super) ColourPrimaries: u16,
    pub(super) TransferCharacteristics: u16,
    pub(super) MatrixCoefficients: u16,
}

impl MfxExtVideoSignalInfo {
    pub(super) fn bt2020_pq_full() -> Self {
        Self::from_nclx(NclxColorMetadata::bt2020_pq_full())
    }

    pub(super) fn from_nclx(color: NclxColorMetadata) -> Self {
        Self {
            Header: MfxExtBuffer {
                BufferId: MFX_EXTBUFF_VIDEO_SIGNAL_INFO,
                BufferSz: std::mem::size_of::<Self>() as u32,
            },
            // ITU-T H.265 video_format value 5 means "unspecified"; colour
            // description below carries the normative HDR signal identity.
            VideoFormat: 5,
            VideoFullRange: u16::from(color.full_range),
            ColourDescriptionPresent: 1,
            ColourPrimaries: color.colour_primaries,
            TransferCharacteristics: color.transfer_characteristics,
            MatrixCoefficients: color.matrix_coefficients,
        }
    }
}

#[repr(C)]
#[derive(Debug, Clone, Copy)]
pub(super) struct MfxExtCodingOption2 {
    pub(super) Header: MfxExtBuffer,
    pub(super) IntRefType: u16,
    pub(super) IntRefCycleSize: u16,
    pub(super) IntRefQPDelta: i16,
    pub(super) MaxFrameSize: u32,
    pub(super) MaxSliceSize: u32,
    pub(super) BitrateLimit: u16,
    pub(super) MBBRC: u16,
    pub(super) ExtBRC: u16,
    pub(super) LookAheadDepth: u16,
    pub(super) Trellis: u16,
    pub(super) RepeatPPS: u16,
    pub(super) BRefType: u16,
    pub(super) AdaptiveI: u16,
    pub(super) AdaptiveB: u16,
    pub(super) LookAheadDS: u16,
    pub(super) NumMbPerSlice: u16,
    pub(super) SkipFrame: u16,
    pub(super) MinQPI: u8,
    pub(super) MaxQPI: u8,
    pub(super) MinQPP: u8,
    pub(super) MaxQPP: u8,
    pub(super) MinQPB: u8,
    pub(super) MaxQPB: u8,
    pub(super) FixedFrameRate: u16,
    pub(super) DisableDeblockingIdc: u16,
    pub(super) DisableVUI: u16,
    pub(super) BufferingPeriodSEI: u16,
    pub(super) EnableMAD: u16,
    pub(super) UseRawRef: u16,
}

impl MfxExtCodingOption2 {
    pub(super) fn for_rate_control(rate_control: &RateControlConfig) -> Self {
        let mut out: Self = unsafe { std::mem::zeroed() };
        out.Header = MfxExtBuffer {
            BufferId: MFX_EXTBUFF_CODING_OPTION2,
            BufferSz: std::mem::size_of::<Self>() as u32,
        };
        let mut coding3 = MfxExtCodingOption3::empty();
        apply_rate_control_config_to_ext_buffers(&mut out, &mut coding3, rate_control);
        out
    }

    pub(super) fn has_rate_control_overrides(&self) -> bool {
        self.MaxFrameSize != 0
            || self.MBBRC != 0
            || self.ExtBRC != 0
            || self.LookAheadDepth != 0
            || self.RepeatPPS != 0
    }
}

#[repr(C)]
#[derive(Debug, Clone, Copy)]
pub(super) struct MfxExtCodingOption3 {
    pub(super) Header: MfxExtBuffer,
    pub(super) NumSliceI: u16,
    pub(super) NumSliceP: u16,
    pub(super) NumSliceB: u16,
    pub(super) WinBRCMaxAvgKbps: u16,
    pub(super) WinBRCSize: u16,
    pub(super) QVBRQuality: u16,
    pub(super) EnableMBQP: u16,
    pub(super) IntRefCycleDist: u16,
    pub(super) DirectBiasAdjustment: u16,
    pub(super) GlobalMotionBiasAdjustment: u16,
    pub(super) MVCostScalingFactor: u16,
    pub(super) MBDisableSkipMap: u16,
    pub(super) WeightedPred: u16,
    pub(super) WeightedBiPred: u16,
    pub(super) AspectRatioInfoPresent: u16,
    pub(super) OverscanInfoPresent: u16,
    pub(super) OverscanAppropriate: u16,
    pub(super) TimingInfoPresent: u16,
    pub(super) BitstreamRestriction: u16,
    pub(super) LowDelayHrd: u16,
    pub(super) MotionVectorsOverPicBoundaries: u16,
    pub(super) reserved1: [u16; 2],
    pub(super) ScenarioInfo: u16,
    pub(super) ContentInfo: u16,
    pub(super) PRefType: u16,
    pub(super) FadeDetection: u16,
    pub(super) reserved2: [u16; 2],
    pub(super) GPB: u16,
    pub(super) MaxFrameSizeI: u32,
    pub(super) MaxFrameSizeP: u32,
    pub(super) reserved3: [u32; 3],
    pub(super) EnableQPOffset: u16,
    pub(super) QPOffset: [i16; 8],
    pub(super) NumRefActiveP: [u16; 8],
    pub(super) NumRefActiveBL0: [u16; 8],
    pub(super) NumRefActiveBL1: [u16; 8],
    pub(super) reserved6: u16,
    pub(super) TransformSkip: u16,
    pub(super) TargetChromaFormatPlus1: u16,
    pub(super) TargetBitDepthLuma: u16,
    pub(super) TargetBitDepthChroma: u16,
    pub(super) BRCPanicMode: u16,
    pub(super) LowDelayBRC: u16,
    pub(super) EnableMBForceIntra: u16,
    pub(super) AdaptiveMaxFrameSize: u16,
    pub(super) RepartitionCheckEnable: u16,
    pub(super) reserved5: [u16; 3],
    pub(super) EncodedUnitsInfo: u16,
    pub(super) EnableNalUnitType: u16,
    pub(super) AdaptiveLTR: u16,
    pub(super) AdaptiveCQM: u16,
    pub(super) AdaptiveRef: u16,
    pub(super) reserved: [u16; 161],
}

impl MfxExtCodingOption3 {
    pub(super) fn empty() -> Self {
        let mut out: Self = unsafe { std::mem::zeroed() };
        out.Header = MfxExtBuffer {
            BufferId: MFX_EXTBUFF_CODING_OPTION3,
            BufferSz: std::mem::size_of::<Self>() as u32,
        };
        out
    }

    pub(super) fn for_rate_control(rate_control: &RateControlConfig) -> Self {
        let mut coding2 = MfxExtCodingOption2 {
            Header: MfxExtBuffer {
                BufferId: MFX_EXTBUFF_CODING_OPTION2,
                BufferSz: std::mem::size_of::<MfxExtCodingOption2>() as u32,
            },
            ..unsafe { std::mem::zeroed() }
        };
        let mut out = Self::empty();
        apply_rate_control_config_to_ext_buffers(&mut coding2, &mut out, rate_control);
        out
    }

    pub(super) fn apply_route(&mut self, route: VplRecordRoute) {
        if u32::from(route.profile) == MFX_PROFILE_HEVC_REXT {
            self.TargetChromaFormatPlus1 = route.chroma.saturating_add(1);
            self.TargetBitDepthLuma = route.bit_depth;
            self.TargetBitDepthChroma = route.bit_depth;
        } else {
            self.TargetChromaFormatPlus1 = 0;
            self.TargetBitDepthLuma = 0;
            self.TargetBitDepthChroma = 0;
        }
    }

    pub(super) fn has_rate_control_overrides(&self) -> bool {
        self.WinBRCMaxAvgKbps != 0
            || self.WinBRCSize != 0
            || self.QVBRQuality != 0
            || self.LowDelayBRC != 0
            || self.TargetChromaFormatPlus1 != 0
            || self.TargetBitDepthLuma != 0
            || self.TargetBitDepthChroma != 0
    }
}

pub(super) struct VplEncodeExtBuffers {
    pub(super) video_signal: MfxExtVideoSignalInfo,
    pub(super) coding2: MfxExtCodingOption2,
    pub(super) coding3: MfxExtCodingOption3,
    pub(super) ext_params: [*mut c_void; 3],
}

impl VplEncodeExtBuffers {
    pub(super) fn hdr_pq_full(rate_control: &RateControlConfig) -> Self {
        Self::for_route(VplRecordRoute::hdr_pq_p010(), rate_control)
    }

    pub(super) fn for_route(route: VplRecordRoute, rate_control: &RateControlConfig) -> Self {
        let mut out = Self {
            video_signal: MfxExtVideoSignalInfo::from_nclx(route.mp4_color),
            coding2: MfxExtCodingOption2::for_rate_control(rate_control),
            coding3: MfxExtCodingOption3::for_rate_control(rate_control),
            ext_params: [ptr::null_mut(); 3],
        };
        out.coding2.RepeatPPS = MFX_CODINGOPTION_ON;
        out.coding3.apply_route(route);
        out
    }

    pub(super) fn refresh(&mut self, route: VplRecordRoute, rate_control: &RateControlConfig) {
        self.video_signal = MfxExtVideoSignalInfo::from_nclx(route.mp4_color);
        apply_rate_control_config_to_ext_buffers(
            &mut self.coding2,
            &mut self.coding3,
            rate_control,
        );
        self.coding2.RepeatPPS = MFX_CODINGOPTION_ON;
        self.coding3.apply_route(route);
    }

    pub(super) fn attach(&mut self, param: &mut MfxVideoParam) {
        let mut count = 0usize;
        self.ext_params[count] =
            &mut self.video_signal as *mut MfxExtVideoSignalInfo as *mut c_void;
        count += 1;
        if self.coding2.has_rate_control_overrides() {
            self.ext_params[count] = &mut self.coding2 as *mut MfxExtCodingOption2 as *mut c_void;
            count += 1;
        }
        if self.coding3.has_rate_control_overrides() {
            self.ext_params[count] = &mut self.coding3 as *mut MfxExtCodingOption3 as *mut c_void;
            count += 1;
        }
        param.ExtParam = self.ext_params.as_mut_ptr();
        param.NumExtParam = count as u16;
    }
}

#[repr(C)]
#[derive(Debug, Clone, Copy)]
pub(super) struct MfxVersion {
    pub(super) version: u32,
}

#[repr(C)]
#[derive(Debug, Clone, Copy)]
pub(super) struct MfxRange32U {
    pub(super) Min: u32,
    pub(super) Max: u32,
    pub(super) Step: u32,
}

#[repr(C)]
#[derive(Debug, Clone, Copy)]
pub(super) struct MfxFrameId {
    pub(super) TemporalId: u16,
    pub(super) PriorityId: u16,
    pub(super) DependencyId: u16,
    pub(super) QualityId: u16,
}

#[repr(C)]
#[derive(Debug, Clone, Copy)]
pub(super) struct MfxFrameInfo {
    pub(super) reserved: [u32; 4],
    pub(super) ChannelId: u16,
    pub(super) BitDepthLuma: u16,
    pub(super) BitDepthChroma: u16,
    pub(super) Shift: u16,
    pub(super) FrameId: MfxFrameId,
    pub(super) FourCC: u32,
    pub(super) Width: u16,
    pub(super) Height: u16,
    pub(super) CropX: u16,
    pub(super) CropY: u16,
    pub(super) CropW: u16,
    pub(super) CropH: u16,
    pub(super) FrameRateExtN: u32,
    pub(super) FrameRateExtD: u32,
    pub(super) reserved3: u16,
    pub(super) AspectRatioW: u16,
    pub(super) AspectRatioH: u16,
    pub(super) PicStruct: u16,
    pub(super) ChromaFormat: u16,
    pub(super) reserved2: u16,
}

#[repr(C)]
#[derive(Debug, Clone, Copy)]
pub(super) struct MfxInfoMFX {
    pub(super) reserved: [u32; 7],
    pub(super) LowPower: u16,
    pub(super) BRCParamMultiplier: u16,
    pub(super) FrameInfo: MfxFrameInfo,
    pub(super) CodecId: u32,
    pub(super) CodecProfile: u16,
    pub(super) CodecLevel: u16,
    pub(super) NumThread: u16,
    pub(super) TargetUsage: u16,
    pub(super) GopPicSize: u16,
    pub(super) GopRefDist: u16,
    pub(super) GopOptFlag: u16,
    pub(super) IdrInterval: u16,
    pub(super) RateControlMethod: u16,
    pub(super) InitialDelayInKB: u16,
    pub(super) BufferSizeInKB: u16,
    pub(super) TargetKbps: u16,
    pub(super) MaxKbps: u16,
    pub(super) NumSlice: u16,
    pub(super) NumRefFrame: u16,
    pub(super) EncodedOrder: u16,
}

#[repr(C)]
#[derive(Debug, Clone, Copy)]
pub(super) struct MfxVideoParam {
    pub(super) AllocId: u32,
    pub(super) reserved: [u32; 2],
    pub(super) reserved3: u16,
    pub(super) AsyncDepth: u16,
    pub(super) mfx: MfxInfoMFX,
    pub(super) union_padding: [u8; 32],
    pub(super) Protected: u16,
    pub(super) IOPattern: u16,
    pub(super) ExtParam: *mut *mut c_void,
    pub(super) NumExtParam: u16,
    pub(super) reserved2: u16,
}

#[repr(C)]
#[derive(Debug, Clone, Copy)]
pub(super) struct MfxFrameAllocRequest {
    pub(super) AllocId: u32,
    pub(super) reserved3: [u32; 3],
    pub(super) Info: MfxFrameInfo,
    pub(super) Type: u16,
    pub(super) NumFrameMin: u16,
    pub(super) NumFrameSuggested: u16,
    pub(super) reserved2: u16,
}

#[repr(C)]
#[derive(Debug, Clone, Copy)]
pub(super) struct MfxFrameData {
    pub(super) ExtParam: *mut *mut c_void,
    pub(super) NumExtParam: u16,
    pub(super) reserved: [u16; 9],
    pub(super) MemType: u16,
    pub(super) PitchHigh: u16,
    pub(super) TimeStamp: u64,
    pub(super) FrameOrder: u32,
    pub(super) Locked: u16,
    pub(super) Pitch: u16,
    pub(super) Y: *mut u8,
    pub(super) UV: *mut u8,
    pub(super) V: *mut u8,
    pub(super) A: *mut u8,
    pub(super) MemId: MfxHDL,
    pub(super) Corrupted: u16,
    pub(super) DataFlag: u16,
    pub(super) reserved4: [u16; 2],
}

#[repr(C)]
#[derive(Debug, Clone, Copy)]
pub(super) struct MfxFrameSurface1 {
    pub(super) FrameInterface: *mut MfxFrameSurfaceInterface,
    pub(super) Version: MfxStructVersion,
    pub(super) reserved1: [u16; 3],
    pub(super) Info: MfxFrameInfo,
    pub(super) Data: MfxFrameData,
}

#[repr(C)]
#[derive(Clone, Copy)]
pub(super) struct MfxFrameSurfaceInterface {
    pub(super) Context: MfxHDL,
    pub(super) Version: MfxStructVersion,
    pub(super) reserved1: [u16; 3],
    pub(super) AddRef: unsafe extern "C" fn(*mut MfxFrameSurface1) -> i32,
    pub(super) Release: unsafe extern "C" fn(*mut MfxFrameSurface1) -> i32,
    pub(super) GetRefCounter: unsafe extern "C" fn(*mut MfxFrameSurface1, *mut u32) -> i32,
    pub(super) Map: unsafe extern "C" fn(*mut MfxFrameSurface1, u32) -> i32,
    pub(super) Unmap: unsafe extern "C" fn(*mut MfxFrameSurface1) -> i32,
    pub(super) GetNativeHandle:
        unsafe extern "C" fn(*mut MfxFrameSurface1, *mut MfxHDL, *mut u32) -> i32,
    pub(super) GetDeviceHandle:
        unsafe extern "C" fn(*mut MfxFrameSurface1, *mut MfxHDL, *mut u32) -> i32,
    pub(super) Synchronize: unsafe extern "C" fn(*mut MfxFrameSurface1, u32) -> i32,
    pub(super) OnComplete: unsafe extern "C" fn(i32),
    pub(super) QueryInterface:
        unsafe extern "C" fn(*mut MfxFrameSurface1, MfxGuid, *mut MfxHDL) -> i32,
    pub(super) reserved2: [MfxHDL; 2],
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
pub(super) struct MfxGuid {
    pub(super) Data: [u8; 16],
}

#[repr(C)]
#[derive(Debug, Clone, Copy)]
pub(super) struct MfxBitstream {
    pub(super) EncryptedData: *mut c_void,
    pub(super) ExtParam: *mut *mut c_void,
    pub(super) NumExtParam: u16,
    pub(super) reserved1: u16,
    pub(super) CodecId: u32,
    pub(super) DecodeTimeStamp: i64,
    pub(super) TimeStamp: u64,
    pub(super) Data: *mut u8,
    pub(super) DataOffset: u32,
    pub(super) DataLength: u32,
    pub(super) MaxLength: u32,
    pub(super) PicStruct: u16,
    pub(super) FrameType: u16,
    pub(super) DataFlag: u16,
    pub(super) reserved2: u16,
}

#[repr(C)]
#[derive(Debug)]
pub(super) struct MfxEncExtDescription {
    pub(super) Version: MfxStructVersion,
    pub(super) reserved: [u16; 10],
    pub(super) NumRateControlMethods: u16,
    pub(super) RateControlMethods: *const u16,
    pub(super) reserved2: [u16; 11],
    pub(super) NumExtBufferIDs: u16,
    pub(super) ExtBufferIDs: *const u32,
}

#[repr(C)]
#[derive(Debug)]
pub(super) struct MfxEncMemExtDescription {
    pub(super) Version: MfxStructVersion,
    pub(super) reserved: [u16; 13],
    pub(super) TargetMaxBitDepth: u16,
    pub(super) NumTargetChromaSubsamplings: u16,
    pub(super) TargetChromaSubsamplings: *const u16,
}

#[repr(C)]
#[derive(Debug)]
pub(super) struct MfxEncoderMemDesc {
    pub(super) MemHandleType: u32,
    pub(super) Width: MfxRange32U,
    pub(super) Height: MfxRange32U,
    pub(super) reserved: [u16; 2],
    pub(super) MemExtDesc: *const MfxEncMemExtDescription,
    pub(super) reserved3: u16,
    pub(super) NumColorFormats: u16,
    pub(super) ColorFormats: *const u32,
}

#[repr(C)]
#[derive(Debug)]
pub(super) struct MfxEncoderProfile {
    pub(super) Profile: u32,
    pub(super) reserved: [u16; 7],
    pub(super) NumMemTypes: u16,
    pub(super) MemDesc: *const MfxEncoderMemDesc,
}

#[repr(C)]
#[derive(Debug)]
pub(super) struct MfxEncoderCodec {
    pub(super) CodecID: u32,
    pub(super) MaxcodecLevel: u16,
    pub(super) BiDirectionalPrediction: u16,
    pub(super) EncExtDesc: *const MfxEncExtDescription,
    pub(super) reserved: [u16; 3],
    pub(super) NumProfiles: u16,
    pub(super) Profiles: *const MfxEncoderProfile,
}

#[repr(C)]
#[derive(Debug)]
pub(super) struct MfxEncoderDescription {
    pub(super) Version: MfxStructVersion,
    pub(super) reserved: [u16; 7],
    pub(super) NumCodecs: u16,
    pub(super) Codecs: *const MfxEncoderCodec,
}

#[repr(C)]
#[derive(Debug)]
pub(super) struct MfxDecoderDescription {
    pub(super) Version: MfxStructVersion,
    pub(super) reserved: [u16; 7],
    pub(super) NumCodecs: u16,
    pub(super) Codecs: *const c_void,
}

#[repr(C)]
#[derive(Debug)]
pub(super) struct MfxVppDescription {
    pub(super) Version: MfxStructVersion,
    pub(super) reserved: [u16; 7],
    pub(super) NumFilters: u16,
    pub(super) Filters: *const c_void,
}

#[repr(C)]
#[derive(Debug)]
pub(super) struct MfxDeviceDescription {
    pub(super) Version: MfxStructVersion,
    pub(super) reserved: [u16; 6],
    pub(super) MediaAdapterType: u16,
    pub(super) DeviceID: [c_char; 128],
    pub(super) NumSubDevices: u16,
    pub(super) SubDevices: *const c_void,
}

#[repr(C)]
#[derive(Debug)]
pub(super) struct MfxAccelerationModeDescription {
    pub(super) Version: MfxStructVersion,
    pub(super) reserved: [u16; 2],
    pub(super) NumAccelerationModes: u16,
    pub(super) Mode: *const u32,
}

#[repr(C)]
#[derive(Debug)]
pub(super) struct MfxPoolPolicyDescription {
    pub(super) Version: MfxStructVersion,
    pub(super) reserved: [u16; 2],
    pub(super) NumPoolPolicies: u16,
    pub(super) Policy: *const u32,
}

#[repr(C)]
#[derive(Debug)]
pub(super) struct MfxImplDescription {
    pub(super) Version: MfxStructVersion,
    pub(super) Impl: u32,
    pub(super) AccelerationMode: u32,
    pub(super) ApiVersion: MfxVersion,
    pub(super) ImplName: [c_char; 32],
    pub(super) License: [c_char; 128],
    pub(super) Keywords: [c_char; 128],
    pub(super) VendorID: u32,
    pub(super) VendorImplID: u32,
    pub(super) Dev: MfxDeviceDescription,
    pub(super) Dec: MfxDecoderDescription,
    pub(super) Enc: MfxEncoderDescription,
    pub(super) VPP: MfxVppDescription,
    pub(super) AccelerationModeDescription: MfxAccelerationModeDescription,
    pub(super) PoolPolicies: MfxPoolPolicyDescription,
    pub(super) reserved: [u32; 8],
    pub(super) NumExtParam: u32,
    pub(super) ExtParam: *const c_void,
}
