use super::*;
use std::sync::Arc;
use windows::Win32::Graphics::Direct3D11::ID3D11Resource;
use windows::core::Interface;

const CUDA_SUCCESS: i32 = 0;
const CU_GRAPHICS_REGISTER_FLAGS_SURFACE_LDST: u32 = 0x04;

type CuDevice = i32;
type CuContext = *mut c_void;
type CuGraphicsResource = *mut c_void;
type CuArray = *mut c_void;
type CuStream = *mut c_void;
type CuExternalMemory = *mut c_void;
type CuMipmappedArray = *mut c_void;
type CuExternalSemaphore = *mut c_void;

const CU_EXTERNAL_MEMORY_HANDLE_TYPE_D3D11_RESOURCE: u32 = 6;
const CUDA_EXTERNAL_MEMORY_DEDICATED: u32 = 0x01;
const CUDA_ARRAY3D_SURFACE_LDST: u32 = 0x02;
const CU_AD_FORMAT_UNSIGNED_INT8: u32 = 0x01;
const CU_AD_FORMAT_UNSIGNED_INT16: u32 = 0x02;
const CU_EXTERNAL_SEMAPHORE_HANDLE_TYPE_D3D11_KEYED_MUTEX: u32 = 7;

#[repr(C)]
struct CudaExternalMemoryHandleDesc {
    handle_type: u32,
    _align: u32,
    handle: *mut c_void,
    name: *const c_void,
    size: u64,
    flags: u32,
    reserved: [u32; 16],
}

#[repr(C)]
struct CudaArray3dDescriptor {
    width: usize,
    height: usize,
    depth: usize,
    format: u32,
    num_channels: u32,
    flags: u32,
}

#[repr(C)]
struct CudaExternalMemoryMipmappedArrayDesc {
    offset: u64,
    array_desc: CudaArray3dDescriptor,
    num_levels: u32,
    reserved: [u32; 16],
}

#[repr(C)]
struct CudaExternalSemaphoreHandleDesc {
    handle_type: u32,
    _align: u32,
    handle: *mut c_void,
    name: *const c_void,
    flags: u32,
    reserved: [u32; 16],
}

#[repr(C)]
struct CudaExternalSemaphoreWaitPayload {
    fence_value: u64,
    nv_sci_sync: u64,
    keyed_mutex_key: u64,
    keyed_mutex_timeout_ms: u32,
    _keyed_mutex_padding: u32,
    reserved: [u32; 10],
}

#[repr(C)]
struct CudaExternalSemaphoreWaitParams {
    params: CudaExternalSemaphoreWaitPayload,
    flags: u32,
    reserved: [u32; 16],
}

#[repr(C)]
struct CudaExternalSemaphoreSignalPayload {
    fence_value: u64,
    nv_sci_sync: u64,
    keyed_mutex_key: u64,
    reserved: [u32; 12],
}

#[repr(C)]
struct CudaExternalSemaphoreSignalParams {
    params: CudaExternalSemaphoreSignalPayload,
    flags: u32,
    reserved: [u32; 16],
}

struct CudaApi {
    _lib: Library,
    cu_init: unsafe extern "system" fn(u32) -> i32,
    cu_device_get_count: unsafe extern "system" fn(*mut i32) -> i32,
    cu_device_get: unsafe extern "system" fn(*mut CuDevice, i32) -> i32,
    cu_device_get_luid: unsafe extern "system" fn(*mut i8, *mut u32, CuDevice) -> i32,
    cu_device_primary_ctx_retain: unsafe extern "system" fn(*mut CuContext, CuDevice) -> i32,
    cu_device_primary_ctx_release: unsafe extern "system" fn(CuDevice) -> i32,
    cu_ctx_push_current: unsafe extern "system" fn(CuContext) -> i32,
    cu_ctx_pop_current: unsafe extern "system" fn(*mut CuContext) -> i32,
    cu_graphics_d3d11_register_resource:
        unsafe extern "system" fn(*mut CuGraphicsResource, *mut c_void, u32) -> i32,
    cu_graphics_map_resources:
        unsafe extern "system" fn(u32, *mut CuGraphicsResource, CuStream) -> i32,
    cu_graphics_sub_resource_get_mapped_array:
        unsafe extern "system" fn(*mut CuArray, CuGraphicsResource, u32, u32) -> i32,
    cu_graphics_unmap_resources:
        unsafe extern "system" fn(u32, *mut CuGraphicsResource, CuStream) -> i32,
    cu_graphics_unregister_resource: unsafe extern "system" fn(CuGraphicsResource) -> i32,
    cu_get_error_name: unsafe extern "system" fn(i32, *mut *const i8) -> i32,
    cu_get_error_string: unsafe extern "system" fn(i32, *mut *const i8) -> i32,
    cu_import_external_memory: unsafe extern "system" fn(
        *mut CuExternalMemory,
        *const CudaExternalMemoryHandleDesc,
    ) -> i32,
    cu_external_memory_get_mapped_mipmapped_array: unsafe extern "system" fn(
        *mut CuMipmappedArray,
        CuExternalMemory,
        *const CudaExternalMemoryMipmappedArrayDesc,
    ) -> i32,
    cu_mipmapped_array_get_level:
        unsafe extern "system" fn(*mut CuArray, CuMipmappedArray, u32) -> i32,
    cu_mipmapped_array_destroy: unsafe extern "system" fn(CuMipmappedArray) -> i32,
    cu_destroy_external_memory: unsafe extern "system" fn(CuExternalMemory) -> i32,
    cu_import_external_semaphore: unsafe extern "system" fn(
        *mut CuExternalSemaphore,
        *const CudaExternalSemaphoreHandleDesc,
    ) -> i32,
    cu_wait_external_semaphores_async: unsafe extern "system" fn(
        *const CuExternalSemaphore,
        *const CudaExternalSemaphoreWaitParams,
        u32,
        CuStream,
    ) -> i32,
    cu_signal_external_semaphores_async: unsafe extern "system" fn(
        *const CuExternalSemaphore,
        *const CudaExternalSemaphoreSignalParams,
        u32,
        CuStream,
    ) -> i32,
    cu_destroy_external_semaphore: unsafe extern "system" fn(CuExternalSemaphore) -> i32,
    cu_stream_synchronize: unsafe extern "system" fn(CuStream) -> i32,
}

impl CudaApi {
    unsafe fn load() -> Result<Arc<Self>, BackendError> {
        let lib = Library::new("nvcuda.dll").map_err(|err| {
            BackendError::unsupported("CUDA interop", "nvcuda.dll", err.to_string())
        })?;
        macro_rules! load {
            ($symbol:literal, $ty:ty) => {
                *lib.get::<$ty>(concat!($symbol, "\0").as_bytes())
                    .map_err(|err| {
                        BackendError::unsupported(
                            "CUDA interop",
                            $symbol,
                            format!("nvcuda.dll 缺少导出：{err}"),
                        )
                    })?
            };
        }
        Ok(Arc::new(Self {
            cu_init: load!("cuInit", unsafe extern "system" fn(u32) -> i32),
            cu_device_get_count: load!(
                "cuDeviceGetCount",
                unsafe extern "system" fn(*mut i32) -> i32
            ),
            cu_device_get: load!(
                "cuDeviceGet",
                unsafe extern "system" fn(*mut CuDevice, i32) -> i32
            ),
            cu_device_get_luid: load!(
                "cuDeviceGetLuid",
                unsafe extern "system" fn(*mut i8, *mut u32, CuDevice) -> i32
            ),
            cu_device_primary_ctx_retain: load!(
                "cuDevicePrimaryCtxRetain",
                unsafe extern "system" fn(*mut CuContext, CuDevice) -> i32
            ),
            cu_device_primary_ctx_release: load!(
                "cuDevicePrimaryCtxRelease_v2",
                unsafe extern "system" fn(CuDevice) -> i32
            ),
            cu_ctx_push_current: load!(
                "cuCtxPushCurrent_v2",
                unsafe extern "system" fn(CuContext) -> i32
            ),
            cu_ctx_pop_current: load!(
                "cuCtxPopCurrent_v2",
                unsafe extern "system" fn(*mut CuContext) -> i32
            ),
            cu_graphics_d3d11_register_resource: load!(
                "cuGraphicsD3D11RegisterResource",
                unsafe extern "system" fn(*mut CuGraphicsResource, *mut c_void, u32) -> i32
            ),
            cu_graphics_map_resources: load!(
                "cuGraphicsMapResources",
                unsafe extern "system" fn(u32, *mut CuGraphicsResource, CuStream) -> i32
            ),
            cu_graphics_sub_resource_get_mapped_array: load!(
                "cuGraphicsSubResourceGetMappedArray",
                unsafe extern "system" fn(*mut CuArray, CuGraphicsResource, u32, u32) -> i32
            ),
            cu_graphics_unmap_resources: load!(
                "cuGraphicsUnmapResources",
                unsafe extern "system" fn(u32, *mut CuGraphicsResource, CuStream) -> i32
            ),
            cu_graphics_unregister_resource: load!(
                "cuGraphicsUnregisterResource",
                unsafe extern "system" fn(CuGraphicsResource) -> i32
            ),
            cu_get_error_name: load!(
                "cuGetErrorName",
                unsafe extern "system" fn(i32, *mut *const i8) -> i32
            ),
            cu_get_error_string: load!(
                "cuGetErrorString",
                unsafe extern "system" fn(i32, *mut *const i8) -> i32
            ),
            cu_import_external_memory: load!(
                "cuImportExternalMemory",
                unsafe extern "system" fn(
                    *mut CuExternalMemory,
                    *const CudaExternalMemoryHandleDesc,
                ) -> i32
            ),
            cu_external_memory_get_mapped_mipmapped_array: load!(
                "cuExternalMemoryGetMappedMipmappedArray",
                unsafe extern "system" fn(
                    *mut CuMipmappedArray,
                    CuExternalMemory,
                    *const CudaExternalMemoryMipmappedArrayDesc,
                ) -> i32
            ),
            cu_mipmapped_array_get_level: load!(
                "cuMipmappedArrayGetLevel",
                unsafe extern "system" fn(*mut CuArray, CuMipmappedArray, u32) -> i32
            ),
            cu_mipmapped_array_destroy: load!(
                "cuMipmappedArrayDestroy",
                unsafe extern "system" fn(CuMipmappedArray) -> i32
            ),
            cu_destroy_external_memory: load!(
                "cuDestroyExternalMemory",
                unsafe extern "system" fn(CuExternalMemory) -> i32
            ),
            cu_import_external_semaphore: load!(
                "cuImportExternalSemaphore",
                unsafe extern "system" fn(
                    *mut CuExternalSemaphore,
                    *const CudaExternalSemaphoreHandleDesc,
                ) -> i32
            ),
            cu_wait_external_semaphores_async: load!(
                "cuWaitExternalSemaphoresAsync",
                unsafe extern "system" fn(
                    *const CuExternalSemaphore,
                    *const CudaExternalSemaphoreWaitParams,
                    u32,
                    CuStream,
                ) -> i32
            ),
            cu_signal_external_semaphores_async: load!(
                "cuSignalExternalSemaphoresAsync",
                unsafe extern "system" fn(
                    *const CuExternalSemaphore,
                    *const CudaExternalSemaphoreSignalParams,
                    u32,
                    CuStream,
                ) -> i32
            ),
            cu_destroy_external_semaphore: load!(
                "cuDestroyExternalSemaphore",
                unsafe extern "system" fn(CuExternalSemaphore) -> i32
            ),
            cu_stream_synchronize: load!(
                "cuStreamSynchronize",
                unsafe extern "system" fn(CuStream) -> i32
            ),
            _lib: lib,
        }))
    }

    unsafe fn check(&self, func: &'static str, status: i32) -> Result<(), BackendError> {
        if status == CUDA_SUCCESS {
            return Ok(());
        }
        let mut name = ptr::null();
        let mut message = ptr::null();
        let _ = (self.cu_get_error_name)(status, &mut name);
        let _ = (self.cu_get_error_string)(status, &mut message);
        let name = if name.is_null() {
            "CUDA_ERROR_UNKNOWN".to_owned()
        } else {
            std::ffi::CStr::from_ptr(name)
                .to_string_lossy()
                .into_owned()
        };
        let message = if message.is_null() {
            String::new()
        } else {
            std::ffi::CStr::from_ptr(message)
                .to_string_lossy()
                .into_owned()
        };
        Err(BackendError::unsupported(
            "CUDA interop",
            func,
            format!("CUresult={status} {name}: {message}"),
        ))
    }
}

pub(super) struct CudaPrimaryContext {
    api: Arc<CudaApi>,
    device: CuDevice,
    context: CuContext,
    current: bool,
}

impl CudaPrimaryContext {
    pub(super) unsafe fn for_adapter_luid(luid: [u8; 8]) -> Result<Self, BackendError> {
        let api = CudaApi::load()?;
        api.check("cuInit", (api.cu_init)(0))?;
        let mut count = 0;
        api.check("cuDeviceGetCount", (api.cu_device_get_count)(&mut count))?;
        let mut selected = None;
        for ordinal in 0..count {
            let mut device = 0;
            api.check("cuDeviceGet", (api.cu_device_get)(&mut device, ordinal))?;
            let mut cuda_luid = [0i8; 8];
            let mut node_mask = 0;
            if (api.cu_device_get_luid)(cuda_luid.as_mut_ptr(), &mut node_mask, device)
                == CUDA_SUCCESS
                && cuda_luid.map(|byte| byte as u8) == luid
            {
                selected = Some(device);
                break;
            }
        }
        let device = selected.ok_or_else(|| {
            BackendError::unsupported(
                "CUDA interop",
                format!("DXGI LUID={:02X?}", luid),
                "没有找到与 NVENC/D3D11 adapter 相同 LUID 的 CUDA device",
            )
        })?;
        let mut context = ptr::null_mut();
        api.check(
            "cuDevicePrimaryCtxRetain",
            (api.cu_device_primary_ctx_retain)(&mut context, device),
        )?;
        if context.is_null() {
            return Err(BackendError::unsupported(
                "CUDA interop",
                "cuDevicePrimaryCtxRetain",
                "返回空 CUcontext",
            ));
        }
        if let Err(err) = api.check("cuCtxPushCurrent_v2", (api.cu_ctx_push_current)(context)) {
            let _ = (api.cu_device_primary_ctx_release)(device);
            return Err(err);
        }
        Ok(Self {
            api,
            device,
            context,
            current: true,
        })
    }

    pub(super) fn raw_context(&self) -> CuContext {
        self.context
    }

    pub(super) unsafe fn register_d3d11_resource(
        &self,
        resource: &ID3D11Resource,
    ) -> Result<CudaGraphicsResource, BackendError> {
        let mut registered = ptr::null_mut();
        self.api.check(
            "cuGraphicsD3D11RegisterResource",
            (self.api.cu_graphics_d3d11_register_resource)(
                &mut registered,
                resource.as_raw(),
                CU_GRAPHICS_REGISTER_FLAGS_SURFACE_LDST,
            ),
        )?;
        if registered.is_null() {
            return Err(BackendError::unsupported(
                "CUDA interop",
                "cuGraphicsD3D11RegisterResource",
                "返回空 CUgraphicsResource",
            ));
        }
        Ok(CudaGraphicsResource {
            api: self.api.clone(),
            registered,
            mapped: false,
            _resource: resource.clone(),
        })
    }

    pub(super) unsafe fn import_external_texture(
        &self,
        shared_handle: windows::Win32::Foundation::HANDLE,
        width: u32,
        storage_height: u32,
        input_format: NvencD3d11InputFormat,
    ) -> Result<CudaExternalTexture, BackendError> {
        let desc = CudaExternalMemoryHandleDesc {
            handle_type: CU_EXTERNAL_MEMORY_HANDLE_TYPE_D3D11_RESOURCE,
            _align: 0,
            handle: shared_handle.0,
            name: ptr::null(),
            size: input_format.frame_size_bytes(
                width,
                storage_height
                    / match input_format {
                        NvencD3d11InputFormat::Nv16 | NvencD3d11InputFormat::P210 => 2,
                        NvencD3d11InputFormat::Yuv444 | NvencD3d11InputFormat::Yuv44410 => 3,
                        _ => 1,
                    },
            ) as u64,
            flags: CUDA_EXTERNAL_MEMORY_DEDICATED,
            reserved: [0; 16],
        };
        let mut external_memory = ptr::null_mut();
        self.api.check(
            "cuImportExternalMemory(D3D11 resource)",
            (self.api.cu_import_external_memory)(&mut external_memory, &desc),
        )?;
        let array_desc = CudaExternalMemoryMipmappedArrayDesc {
            offset: 0,
            array_desc: CudaArray3dDescriptor {
                width: width as usize,
                height: storage_height as usize,
                depth: 0,
                format: if matches!(
                    input_format,
                    NvencD3d11InputFormat::Nv16 | NvencD3d11InputFormat::Yuv444
                ) {
                    CU_AD_FORMAT_UNSIGNED_INT8
                } else {
                    CU_AD_FORMAT_UNSIGNED_INT16
                },
                num_channels: 1,
                flags: CUDA_ARRAY3D_SURFACE_LDST,
            },
            num_levels: 1,
            reserved: [0; 16],
        };
        let mut mipmapped = ptr::null_mut();
        if let Err(err) = self.api.check(
            "cuExternalMemoryGetMappedMipmappedArray",
            (self.api.cu_external_memory_get_mapped_mipmapped_array)(
                &mut mipmapped,
                external_memory,
                &array_desc,
            ),
        ) {
            let _ = (self.api.cu_destroy_external_memory)(external_memory);
            return Err(err);
        }
        let mut array = ptr::null_mut();
        if let Err(err) = self.api.check(
            "cuMipmappedArrayGetLevel",
            (self.api.cu_mipmapped_array_get_level)(&mut array, mipmapped, 0),
        ) {
            let _ = (self.api.cu_mipmapped_array_destroy)(mipmapped);
            let _ = (self.api.cu_destroy_external_memory)(external_memory);
            return Err(err);
        }
        let semaphore_desc = CudaExternalSemaphoreHandleDesc {
            handle_type: CU_EXTERNAL_SEMAPHORE_HANDLE_TYPE_D3D11_KEYED_MUTEX,
            _align: 0,
            handle: shared_handle.0,
            name: ptr::null(),
            flags: 0,
            reserved: [0; 16],
        };
        let mut semaphore = ptr::null_mut();
        if let Err(err) = self.api.check(
            "cuImportExternalSemaphore(D3D11 keyed mutex)",
            (self.api.cu_import_external_semaphore)(&mut semaphore, &semaphore_desc),
        ) {
            let _ = (self.api.cu_mipmapped_array_destroy)(mipmapped);
            let _ = (self.api.cu_destroy_external_memory)(external_memory);
            return Err(err);
        }
        Ok(CudaExternalTexture {
            api: self.api.clone(),
            external_memory,
            mipmapped,
            array,
            semaphore,
            shared_handle,
        })
    }
}

impl Drop for CudaPrimaryContext {
    fn drop(&mut self) {
        unsafe {
            if self.current {
                let mut popped = ptr::null_mut();
                let _ = (self.api.cu_ctx_pop_current)(&mut popped);
                self.current = false;
            }
            let _ = (self.api.cu_device_primary_ctx_release)(self.device);
        }
    }
}

pub(super) struct CudaGraphicsResource {
    api: Arc<CudaApi>,
    registered: CuGraphicsResource,
    mapped: bool,
    _resource: ID3D11Resource,
}

impl CudaGraphicsResource {
    pub(super) unsafe fn map_array(&mut self) -> Result<CuArray, BackendError> {
        if self.mapped {
            return Err(BackendError::unsupported(
                "CUDA interop",
                "cuGraphicsMapResources",
                "同一 D3D11 输入纹理仍被 NVENC 延迟输出持有，不能提前复用",
            ));
        }
        self.api.check(
            "cuGraphicsMapResources",
            (self.api.cu_graphics_map_resources)(1, &mut self.registered, ptr::null_mut()),
        )?;
        self.mapped = true;
        let mut array = ptr::null_mut();
        self.api.check(
            "cuGraphicsSubResourceGetMappedArray",
            (self.api.cu_graphics_sub_resource_get_mapped_array)(&mut array, self.registered, 0, 0),
        )?;
        if array.is_null() {
            return Err(BackendError::unsupported(
                "CUDA interop",
                "cuGraphicsSubResourceGetMappedArray",
                "返回空 CUarray",
            ));
        }
        Ok(array)
    }

    pub(super) unsafe fn unmap(&mut self) -> Result<(), BackendError> {
        if !self.mapped {
            return Ok(());
        }
        let result = self.api.check(
            "cuGraphicsUnmapResources",
            (self.api.cu_graphics_unmap_resources)(1, &mut self.registered, ptr::null_mut()),
        );
        if result.is_ok() {
            self.mapped = false;
        }
        result
    }
}

impl Drop for CudaGraphicsResource {
    fn drop(&mut self) {
        unsafe {
            if self.mapped {
                let _ = (self.api.cu_graphics_unmap_resources)(
                    1,
                    &mut self.registered,
                    ptr::null_mut(),
                );
            }
            if !self.registered.is_null() {
                let _ = (self.api.cu_graphics_unregister_resource)(self.registered);
                self.registered = ptr::null_mut();
            }
        }
    }
}

pub(super) struct CudaExternalTexture {
    api: Arc<CudaApi>,
    external_memory: CuExternalMemory,
    mipmapped: CuMipmappedArray,
    array: CuArray,
    semaphore: CuExternalSemaphore,
    shared_handle: windows::Win32::Foundation::HANDLE,
}

impl CudaExternalTexture {
    pub(super) fn array(&self) -> CuArray {
        self.array
    }

    pub(super) unsafe fn wait_key(&self, key: u64, timeout_ms: u32) -> Result<(), BackendError> {
        let params = CudaExternalSemaphoreWaitParams {
            params: CudaExternalSemaphoreWaitPayload {
                fence_value: 0,
                nv_sci_sync: 0,
                keyed_mutex_key: key,
                keyed_mutex_timeout_ms: timeout_ms,
                _keyed_mutex_padding: 0,
                reserved: [0; 10],
            },
            flags: 0,
            reserved: [0; 16],
        };
        self.api.check(
            "cuWaitExternalSemaphoresAsync(D3D11 keyed mutex)",
            (self.api.cu_wait_external_semaphores_async)(
                &self.semaphore,
                &params,
                1,
                ptr::null_mut(),
            ),
        )?;
        self.api.check(
            "cuStreamSynchronize(D3D11 keyed mutex wait)",
            (self.api.cu_stream_synchronize)(ptr::null_mut()),
        )
    }

    pub(super) unsafe fn signal_key(&self, key: u64) -> Result<(), BackendError> {
        let params = CudaExternalSemaphoreSignalParams {
            params: CudaExternalSemaphoreSignalPayload {
                fence_value: 0,
                nv_sci_sync: 0,
                keyed_mutex_key: key,
                reserved: [0; 12],
            },
            flags: 0,
            reserved: [0; 16],
        };
        self.api.check(
            "cuSignalExternalSemaphoresAsync(D3D11 keyed mutex)",
            (self.api.cu_signal_external_semaphores_async)(
                &self.semaphore,
                &params,
                1,
                ptr::null_mut(),
            ),
        )
    }
}

impl Drop for CudaExternalTexture {
    fn drop(&mut self) {
        unsafe {
            if !self.semaphore.is_null() {
                let _ = (self.api.cu_stream_synchronize)(ptr::null_mut());
                let _ = (self.api.cu_destroy_external_semaphore)(self.semaphore);
                self.semaphore = ptr::null_mut();
            }
            if !self.mipmapped.is_null() {
                let _ = (self.api.cu_mipmapped_array_destroy)(self.mipmapped);
                self.mipmapped = ptr::null_mut();
            }
            if !self.external_memory.is_null() {
                let _ = (self.api.cu_destroy_external_memory)(self.external_memory);
                self.external_memory = ptr::null_mut();
            }
            if !self.shared_handle.is_invalid() {
                let _ = windows::Win32::Foundation::CloseHandle(self.shared_handle);
                self.shared_handle = windows::Win32::Foundation::HANDLE::default();
            }
        }
    }
}

struct CudaPendingFrame {
    bitstream: NvencBitstreamBuffer,
    texture_key: usize,
    timestamp_90k: u64,
    discard_from_track: bool,
}

struct CudaPersistentInput {
    mapped: Option<NvencMappedInputResource>,
    registered: NvencRegisteredResource,
    external: CudaExternalTexture,
    key_owned: bool,
}

impl CudaPersistentInput {
    unsafe fn unmap_for_reuse(
        &mut self,
        input_format: NvencD3d11InputFormat,
    ) -> Result<(), BackendError> {
        let mut mapped = self.mapped.take().ok_or_else(|| {
            BackendError::unsupported(
                "NVENC CUDA mapped input",
                input_format.label(),
                "输入完成时找不到对应的 mapped resource",
            )
        })?;
        let status = mapped.unmap_now();
        if status == NV_ENC_SUCCESS {
            Ok(())
        } else {
            Err(BackendError::unsupported(
                "NVENC CUDA mapped input",
                input_format.label(),
                format!("NvEncUnmapInputResource status={status}"),
            ))
        }
    }

    unsafe fn release_key(&mut self) -> Result<(), BackendError> {
        if !self.key_owned {
            return Ok(());
        }
        let result = self.external.signal_key(0);
        if result.is_ok() {
            self.key_owned = false;
        }
        result
    }
}

pub(crate) struct NvencCudaInteropEncoder {
    registered_inputs: HashMap<usize, CudaPersistentInput>,
    free_bitstreams: Vec<NvencBitstreamBuffer>,
    pending_frames: VecDeque<CudaPendingFrame>,
    session: NvencCudaSession,
    api: NvencApi,
    cuda_context: CudaPrimaryContext,
    device: windows::Win32::Graphics::Direct3D11::ID3D11Device,
    immediate: windows::Win32::Graphics::Direct3D11::ID3D11DeviceContext,
    width: u32,
    height: u32,
    input_format: NvencD3d11InputFormat,
    expected_color: NclxColorMetadata,
    vui_verified: bool,
    frame_idx: u32,
    lookahead_depth: u16,
    eos_submitted: bool,
}

impl NvencCudaInteropEncoder {
    #[allow(clippy::too_many_arguments)]
    pub(super) unsafe fn open_with_rate_control(
        adapter_index: u32,
        width: u32,
        height: u32,
        input_format: NvencD3d11InputFormat,
        color: NclxColorMetadata,
        rate_control: &RateControlConfig,
        frame_rate_num: u32,
        frame_rate_den: u32,
    ) -> Result<Self, BackendError> {
        let (api, _) = NvencApi::load()
            .map_err(|err| BackendError::unsupported("NVENC", "nvEncodeAPI64.dll", err))?;
        let (device, immediate, luid) = cached_d3d11_device_for_adapter(adapter_index)?;
        let cuda_context = CudaPrimaryContext::for_adapter_luid(luid)?;
        let session = open_cuda_session(&api, cuda_context.raw_context())?;
        initialize_low_latency_hevc_encoder(
            &api,
            session.encoder,
            width,
            height,
            input_format,
            color,
            rate_control,
            frame_rate_num,
            frame_rate_den,
        )?;
        let output_buffer_count = usize::from(rate_control.look_ahead_depth).saturating_add(1);
        let mut free_bitstreams = Vec::with_capacity(output_buffer_count);
        for _ in 0..output_buffer_count {
            free_bitstreams.push(create_bitstream_buffer(&api, session.encoder)?);
        }
        Ok(Self {
            registered_inputs: HashMap::new(),
            free_bitstreams,
            pending_frames: VecDeque::with_capacity(output_buffer_count),
            session,
            api,
            cuda_context,
            device,
            immediate,
            width,
            height,
            input_format,
            expected_color: color,
            vui_verified: false,
            frame_idx: 0,
            lookahead_depth: rate_control.look_ahead_depth,
            eos_submitted: false,
        })
    }

    pub(super) fn device(&self) -> &windows::Win32::Graphics::Direct3D11::ID3D11Device {
        &self.device
    }

    pub(super) fn context(&self) -> &windows::Win32::Graphics::Direct3D11::ID3D11DeviceContext {
        &self.immediate
    }

    pub(super) fn input_format(&self) -> NvencD3d11InputFormat {
        self.input_format
    }

    pub(super) fn lookahead_depth(&self) -> u16 {
        self.lookahead_depth
    }

    pub(super) fn pending_frame_count(&self) -> usize {
        self.pending_frames.len()
    }

    pub(super) fn registration_mode(&self) -> &'static str {
        "persistent D3D11 shared NT handle / CUDA external memory / NVENC CUDA array"
    }

    pub(super) fn submit_texture(
        &mut self,
        texture: &windows::Win32::Graphics::Direct3D11::ID3D11Texture2D,
        timestamp_90k: u64,
        force_idr: bool,
        discard_from_track: bool,
    ) -> Result<Vec<HevcAccessUnit>, BackendError> {
        if self.eos_submitted {
            return Err(BackendError::unsupported(
                "NVENC CUDA encode",
                self.input_format.label(),
                "EOS 已提交，不能继续提交输入帧",
            ));
        }

        unsafe {
            validate_d3d11_input_texture(texture, self.width, self.height, self.input_format)?;
            let bitstream = self.free_bitstreams.pop().ok_or_else(|| {
                BackendError::unsupported(
                    "NVENC CUDA delayed output",
                    format!(
                        "LookAheadDepth={} pending={}",
                        self.lookahead_depth,
                        self.pending_frames.len()
                    ),
                    "延迟输出超过已分配 bitstream pool",
                )
            })?;
            let texture_key = texture.as_raw() as usize;
            if !self.registered_inputs.contains_key(&texture_key) {
                let shared_handle = match create_cuda_external_shared_handle(texture) {
                    Ok(handle) => handle,
                    Err(err) => {
                        self.free_bitstreams.push(bitstream);
                        return Err(err);
                    }
                };
                let external = match self.cuda_context.import_external_texture(
                    shared_handle,
                    self.width,
                    self.input_format.texture_height(self.height),
                    self.input_format,
                ) {
                    Ok(external) => external,
                    Err(err) => {
                        let _ = windows::Win32::Foundation::CloseHandle(shared_handle);
                        self.free_bitstreams.push(bitstream);
                        return Err(err);
                    }
                };
                let registered = match register_cuda_array_input(
                    &self.api,
                    self.session.encoder,
                    external.array(),
                    self.width,
                    self.height,
                    self.input_format,
                ) {
                    Ok(registered) => registered,
                    Err(err) => {
                        self.free_bitstreams.push(bitstream);
                        return Err(err);
                    }
                };
                self.registered_inputs.insert(
                    texture_key,
                    CudaPersistentInput {
                        mapped: None,
                        registered,
                        external,
                        key_owned: false,
                    },
                );
            }
            let input = self
                .registered_inputs
                .get_mut(&texture_key)
                .ok_or_else(|| {
                    BackendError::unsupported(
                        "CUDA external-memory input",
                        "persistent input pool",
                        "D3D11 纹理导入后未保留 CUDA/NVENC 输入",
                    )
                })?;
            if input.key_owned {
                self.free_bitstreams.push(bitstream);
                return Err(BackendError::unsupported(
                    "CUDA keyed-mutex input",
                    self.input_format.label(),
                    "同一 capture slot 在上一个 NVENC 输出完成前被再次提交",
                ));
            }
            if let Err(err) = input.external.wait_key(1, 1_000) {
                self.free_bitstreams.push(bitstream);
                return Err(err);
            }
            input.key_owned = true;
            let mapped =
                match map_input_resource(&self.api, self.session.encoder, &input.registered) {
                    Ok(mapped) => mapped,
                    Err(err) => {
                        let release_result = input.release_key();
                        self.free_bitstreams.push(bitstream);
                        return match release_result {
                            Ok(()) => Err(err),
                            Err(release_err) => Err(BackendError::unsupported(
                                "NVENC CUDA map cleanup",
                                self.input_format.label(),
                                format!(
                                    "输入 map 失败：{err}；keyed mutex 释放也失败：{release_err}"
                                ),
                            )),
                        };
                    }
                };
            input.mapped = Some(mapped);
            let status = match encode_one_d3d11_frame(
                &self.api,
                self.session.encoder,
                input
                    .mapped
                    .as_ref()
                    .expect("mapped input was stored immediately above"),
                &bitstream,
                self.width,
                self.height,
                self.input_format,
                self.frame_idx,
                timestamp_90k,
                force_idr || self.frame_idx == 0,
            ) {
                Ok(status) => status,
                Err(err) => {
                    let unmap_result = input.unmap_for_reuse(self.input_format);
                    let release_result = input.release_key();
                    self.free_bitstreams.push(bitstream);
                    return match (unmap_result, release_result) {
                        (Ok(()), Ok(())) => Err(err),
                        (Err(unmap_err), Ok(())) => Err(BackendError::unsupported(
                            "NVENC CUDA submit cleanup",
                            self.input_format.label(),
                            format!("编码提交失败：{err}；输入 unmap 也失败：{unmap_err}"),
                        )),
                        (Ok(()), Err(release_err)) => Err(BackendError::unsupported(
                            "NVENC CUDA submit cleanup",
                            self.input_format.label(),
                            format!("编码提交失败：{err}；keyed mutex 释放也失败：{release_err}"),
                        )),
                        (Err(unmap_err), Err(release_err)) => Err(BackendError::unsupported(
                            "NVENC CUDA submit cleanup",
                            self.input_format.label(),
                            format!(
                                "编码提交失败：{err}；输入 unmap 失败：{unmap_err}；keyed mutex 释放失败：{release_err}"
                            ),
                        )),
                    };
                }
            };
            self.pending_frames.push_back(CudaPendingFrame {
                bitstream,
                texture_key,
                timestamp_90k,
                discard_from_track,
            });
            self.frame_idx = self.frame_idx.wrapping_add(1);
            match status {
                NvencEncodePictureStatus::OutputAvailable => {
                    Ok(vec![self.drain_one_pending_output()?])
                }
                NvencEncodePictureStatus::NeedMoreInput => Ok(Vec::new()),
            }
        }
    }

    pub(super) fn flush(&mut self) -> Result<Vec<HevcAccessUnit>, BackendError> {
        if self.eos_submitted {
            return Ok(Vec::new());
        }
        unsafe {
            submit_encoder_eos(&self.api, self.session.encoder)?;
            self.eos_submitted = true;
            let mut outputs = Vec::with_capacity(self.pending_frames.len());
            while !self.pending_frames.is_empty() {
                outputs.push(self.drain_one_pending_output()?);
            }
            Ok(outputs)
        }
    }

    unsafe fn drain_one_pending_output(&mut self) -> Result<HevcAccessUnit, BackendError> {
        let pending = self.pending_frames.pop_front().ok_or_else(|| {
            BackendError::unsupported(
                "NVENC CUDA delayed output",
                self.input_format.label(),
                "驱动报告有输出，但 pending frame 队列为空",
            )
        })?;
        let CudaPendingFrame {
            bitstream,
            texture_key,
            timestamp_90k,
            discard_from_track,
        } = pending;
        let output_result = lock_and_copy_bitstream(&self.api, self.session.encoder, &bitstream);
        let input = self
            .registered_inputs
            .get_mut(&texture_key)
            .ok_or_else(|| {
                BackendError::unsupported(
                    "CUDA external-memory input",
                    "persistent input pool",
                    "延迟输出完成时找不到对应 CUDA/NVENC 输入",
                )
            })?;
        let unmap_result = input.unmap_for_reuse(self.input_format);
        let signal_result = input.release_key();
        self.free_bitstreams.push(bitstream);

        let output = output_result?;
        unmap_result?;
        signal_result?;
        if output.output_timestamp_90k != timestamp_90k {
            return Err(BackendError::unsupported(
                "NVENC CUDA delayed output timestamp",
                format!(
                    "submitted={} returned={}",
                    timestamp_90k, output.output_timestamp_90k
                ),
                "NVENC 没有按输入 VFR 时间戳返回对应输出",
            ));
        }
        if output.bytes.is_empty() || !output.annex_b_start_code_seen {
            return Err(BackendError::unsupported(
                "NVENC CUDA encode",
                self.input_format.label(),
                "返回空 bitstream 或缺少 Annex-B start code",
            ));
        }
        if !self.vui_verified {
            verify_hevc_vui_matches(&output.bytes, self.expected_color)?;
            self.vui_verified = true;
        }
        let is_sync = crate::backend::mp4_mux::hevc_annex_b_has_random_access_nal(&output.bytes);
        Ok(HevcAccessUnit {
            timestamp_90k,
            data: output.bytes,
            is_sync,
            discard_from_track,
        })
    }

    pub(super) fn shutdown(mut self) -> Result<(), BackendError> {
        if !self.pending_frames.is_empty() {
            return Err(BackendError::unsupported(
                "NVENC CUDA shutdown",
                format!("pending_frames={}", self.pending_frames.len()),
                "正常关闭前必须调用 flush",
            ));
        }
        let mut failures = Vec::new();
        for (_, mut input) in self.registered_inputs.drain() {
            if input.mapped.is_some()
                && let Err(err) = unsafe { input.unmap_for_reuse(self.input_format) }
            {
                failures.push(format!("CUDA mapped input release: {err}"));
            }
            if input.key_owned
                && let Err(err) = unsafe { input.release_key() }
            {
                failures.push(format!("CUDA keyed-mutex release: {err}"));
            }
            let unregister_status = unsafe { input.registered.unregister_now() };
            if unregister_status != NV_ENC_SUCCESS {
                failures.push(format!(
                    "NvEncUnregisterResource status={unregister_status}"
                ));
            }
        }
        for mut bitstream in self.free_bitstreams.drain(..) {
            let status = unsafe { bitstream.destroy_now() };
            if status != NV_ENC_SUCCESS {
                failures.push(format!("NvEncDestroyBitstreamBuffer status={status}"));
            }
        }
        let status = unsafe { self.session.destroy_now() };
        if status != NV_ENC_SUCCESS {
            failures.push(format!("NvEncDestroyEncoder status={status}"));
        }
        if failures.is_empty() {
            Ok(())
        } else {
            Err(BackendError::unsupported(
                "NVENC CUDA shutdown",
                self.input_format.label(),
                failures.join(" | "),
            ))
        }
    }
}

pub(super) unsafe fn probe_external_texture_interop(
    adapter_luid: [u8; 8],
    device: &windows::Win32::Graphics::Direct3D11::ID3D11Device,
    input_format: NvencD3d11InputFormat,
) -> Result<(), BackendError> {
    use windows::Win32::Graphics::Dxgi::IDXGIKeyedMutex;

    let cuda_context = CudaPrimaryContext::for_adapter_luid(adapter_luid)?;
    let width = 64;
    let height = 64;
    let (texture, shared_handle) =
        create_synthetic_external_input_texture(device, width, height, input_format)?;
    let external = match cuda_context.import_external_texture(
        shared_handle,
        width,
        input_format.texture_height(height),
        input_format,
    ) {
        Ok(external) => external,
        Err(err) => {
            let _ = windows::Win32::Foundation::CloseHandle(shared_handle);
            return Err(err);
        }
    };
    let mutex = texture
        .cast::<IDXGIKeyedMutex>()
        .map_err(|err| BackendError::WindowsApi {
            func: "ID3D11Texture2D::cast<IDXGIKeyedMutex>(CUDA capability probe)",
            message: err.to_string(),
        })?;
    mutex
        .AcquireSync(0, 1_000)
        .map_err(|err| BackendError::WindowsApi {
            func: "IDXGIKeyedMutex::AcquireSync(CUDA capability probe)",
            message: err.to_string(),
        })?;
    mutex
        .ReleaseSync(1)
        .map_err(|err| BackendError::WindowsApi {
            func: "IDXGIKeyedMutex::ReleaseSync(CUDA capability probe)",
            message: err.to_string(),
        })?;
    external.wait_key(1, 1_000)?;
    external.signal_key(0)?;
    mutex
        .AcquireSync(0, 1_000)
        .map_err(|err| BackendError::WindowsApi {
            func: "IDXGIKeyedMutex::AcquireSync(CUDA capability probe return)",
            message: err.to_string(),
        })?;
    mutex
        .ReleaseSync(0)
        .map_err(|err| BackendError::WindowsApi {
            func: "IDXGIKeyedMutex::ReleaseSync(CUDA capability probe return)",
            message: err.to_string(),
        })?;
    Ok(())
}

impl Drop for NvencCudaInteropEncoder {
    fn drop(&mut self) {
        if self.session.encoder.is_null() {
            return;
        }
        unsafe {
            let _ = self.session.destroy_now();
            for mut pending in self.pending_frames.drain(..) {
                pending.bitstream.abandon();
            }
            for bitstream in &mut self.free_bitstreams {
                bitstream.abandon();
            }
            for input in self.registered_inputs.values_mut() {
                if let Some(mapped) = input.mapped.as_mut() {
                    mapped.abandon();
                }
                input.registered.abandon();
                if input.key_owned && input.external.signal_key(0).is_ok() {
                    input.key_owned = false;
                }
            }
            self.registered_inputs.clear();
        }
    }
}

unsafe fn create_cuda_external_shared_handle(
    texture: &windows::Win32::Graphics::Direct3D11::ID3D11Texture2D,
) -> Result<windows::Win32::Foundation::HANDLE, BackendError> {
    use windows::Win32::Foundation::GENERIC_ALL;
    use windows::Win32::Graphics::Dxgi::IDXGIResource1;
    use windows::core::PCWSTR;

    let resource = texture
        .cast::<IDXGIResource1>()
        .map_err(|err| BackendError::WindowsApi {
            func: "ID3D11Texture2D::cast<IDXGIResource1>(CUDA external memory)",
            message: err.to_string(),
        })?;
    resource
        .CreateSharedHandle(None, GENERIC_ALL.0, PCWSTR::null())
        .map_err(|err| BackendError::WindowsApi {
            func: "IDXGIResource1::CreateSharedHandle(CUDA external memory)",
            message: err.to_string(),
        })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn external_semaphore_ffi_layout_matches_cuda_13_header() {
        assert_eq!(std::mem::size_of::<CudaExternalSemaphoreHandleDesc>(), 96);
        assert_eq!(std::mem::size_of::<CudaExternalSemaphoreWaitPayload>(), 72);
        assert_eq!(
            std::mem::offset_of!(CudaExternalSemaphoreWaitPayload, reserved),
            32
        );
        assert_eq!(std::mem::size_of::<CudaExternalSemaphoreWaitParams>(), 144);
        assert_eq!(
            std::mem::offset_of!(CudaExternalSemaphoreWaitParams, flags),
            72
        );
        assert_eq!(
            std::mem::size_of::<CudaExternalSemaphoreSignalPayload>(),
            72
        );
        assert_eq!(
            std::mem::size_of::<CudaExternalSemaphoreSignalParams>(),
            144
        );
        assert_eq!(
            std::mem::offset_of!(CudaExternalSemaphoreSignalParams, flags),
            72
        );
    }
}
