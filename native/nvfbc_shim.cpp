#define WIN32_LEAN_AND_MEAN
#define NOMINMAX

#include "nvfbc_shim.h"

#include <windows.h>
#include <d3d9.h>

#include <cstddef>
#include <cstring>
#include <new>

namespace {

constexpr std::uint32_t kNvFbcDllVersion = 0x70;
constexpr std::uint32_t kToDx9Vid = 0x2003;
constexpr std::uint32_t kArgb10 = 2;
constexpr std::uint32_t kGrabNowait = 1;
constexpr std::int32_t kShimDllLoadFailed = -1001;
constexpr std::int32_t kShimExportMissing = -1002;
constexpr std::int32_t kShimOutOfMemory = -1003;
constexpr std::uint32_t kPrivateData[4] = {
    0xaef57ac5,
    0x401d1a39,
    0x1b856bbe,
    0x9ed0ceba,
};

constexpr std::uint32_t struct_version(std::size_t size, std::uint32_t version) {
    return static_cast<std::uint32_t>(size) | (version << 16) | (kNvFbcDllVersion << 24);
}

struct NvFbcCreateParams {
    std::uint32_t version;
    std::uint32_t interface_type;
    std::uint32_t max_display_width;
    std::uint32_t max_display_height;
    void* device;
    void* private_data;
    std::uint32_t private_data_size;
    std::uint32_t interface_version;
    void* nvfbc;
    std::uint32_t adapter_index;
    std::uint32_t nvfbc_version;
    void* cuda_context;
    void* private_data2;
    std::uint32_t private_data2_size;
    std::uint32_t reserved[55];
    void* reserved_ptrs[27];
};

struct NvFbcFrameGrabInfo {
    std::uint32_t width;
    std::uint32_t height;
    std::uint32_t buffer_width;
    std::uint32_t reserved;
    std::int32_t overlay_active;
    std::int32_t must_recreate;
    std::int32_t first_buffer;
    std::int32_t hw_mouse_visible;
    std::int32_t protected_content;
    std::uint32_t driver_internal_error;
    std::int32_t stereo_on;
    std::int32_t igpu_capture;
    std::uint32_t source_pid;
    std::uint32_t reserved3;
    std::uint32_t flags;
    std::uint32_t wait_mode_used;
    std::uint32_t reserved2[11];
};

struct NvFbcOutputBuffer {
    IDirect3DSurface9* primary;
    IDirect3DSurface9* secondary;
};

struct NvFbcSetupParams {
    std::uint32_t version;
    std::uint32_t flags;
    std::uint32_t mode;
    std::uint32_t buffer_count;
    std::uint32_t diff_map_block_size;
    std::uint32_t stereo_format;
    std::uint32_t diff_map_buffer_size;
    std::uint32_t classification_map_buffer_size;
    std::uint32_t classification_stamp_width;
    std::uint32_t classification_stamp_height;
    void** diff_maps;
    void** classification_maps;
    NvFbcOutputBuffer* buffers;
    void* cursor_event;
    std::uint32_t reserved[22];
    void* reserved_ptrs[12];
};

struct NvFbcGrabParams {
    std::uint32_t version;
    std::uint32_t flags;
    std::uint32_t target_width;
    std::uint32_t target_height;
    std::uint32_t start_x;
    std::uint32_t start_y;
    std::uint32_t mode;
    std::uint32_t buffer_index;
    NvFbcFrameGrabInfo* frame_info;
    std::uint32_t wait_time;
    std::uint32_t reserved[23];
    void* reserved_ptrs[15];
};

static_assert(sizeof(NvFbcCreateParams) == 512);
static_assert(sizeof(NvFbcFrameGrabInfo) == 108);
static_assert(sizeof(NvFbcOutputBuffer) == 16);
static_assert(sizeof(NvFbcSetupParams) == 256);
static_assert(sizeof(NvFbcGrabParams) == 256);

using CreateNvFbc = std::int32_t(__stdcall*)(void* params);
using NvFbcSetup = std::int32_t(__stdcall*)(void* session, NvFbcSetupParams* params);
using NvFbcGrab = std::int32_t(__stdcall*)(void* session, NvFbcGrabParams* params);
using NvFbcGpuSleep = std::int32_t(__stdcall*)(void* session, std::int64_t microseconds);
using NvFbcRelease = std::int32_t(__stdcall*)(void* session);

template <typename Function>
Function vtable_function(void* session, std::size_t index) {
    if (!session) {
        return nullptr;
    }
    void** vtable = *static_cast<void***>(session);
    if (!vtable || !vtable[index]) {
        return nullptr;
    }
    Function function = nullptr;
    static_assert(sizeof(function) == sizeof(vtable[index]));
    std::memcpy(&function, &vtable[index], sizeof(function));
    return function;
}

} // namespace

struct RrNvFbcHandle {
    HMODULE module;
    void* session;
    NvFbcSetup setup;
    NvFbcGrab grab;
    NvFbcGpuSleep gpu_sleep;
    NvFbcRelease release;
    std::uint32_t output_buffer_count;
    NvFbcOutputBuffer output_buffers[3];
};

extern "C" std::int32_t rr_nvfbc_create(void* d3d9_device, std::uint32_t adapter,
                                         RrNvFbcHandle** out_handle,
                                         RrNvFbcCreateInfo* out_info) noexcept {
    if (!d3d9_device || !out_handle || !out_info) {
        return -2;
    }
    *out_handle = nullptr;
    *out_info = {};
    HMODULE module = LoadLibraryW(L"NvFBC64.dll");
    if (!module) {
        return kShimDllLoadFailed;
    }
    const FARPROC raw_create = GetProcAddress(module, "NvFBC_CreateEx");
    CreateNvFbc create = nullptr;
    static_assert(sizeof(create) == sizeof(raw_create));
    std::memcpy(&create, &raw_create, sizeof(create));
    if (!create) {
        FreeLibrary(module);
        return kShimExportMissing;
    }

    std::uint32_t private_data[4] = {
        kPrivateData[0], kPrivateData[1], kPrivateData[2], kPrivateData[3]};
    NvFbcCreateParams params{};
    params.version = struct_version(sizeof(params), 2);
    params.interface_type = kToDx9Vid;
    params.device = d3d9_device;
    params.private_data = private_data;
    params.private_data_size = static_cast<std::uint32_t>(sizeof(private_data));
    params.adapter_index = adapter;
    const std::int32_t status = create(&params);
    if (status != 0 || !params.nvfbc) {
        FreeLibrary(module);
        return status != 0 ? status : -1;
    }

    const NvFbcSetup setup = vtable_function<NvFbcSetup>(params.nvfbc, 0);
    const NvFbcGrab grab = vtable_function<NvFbcGrab>(params.nvfbc, 1);
    const NvFbcGpuSleep gpu_sleep = vtable_function<NvFbcGpuSleep>(params.nvfbc, 2);
    const NvFbcRelease release = vtable_function<NvFbcRelease>(params.nvfbc, 3);
    if (!setup || !grab || !gpu_sleep || !release) {
        if (release) {
            release(params.nvfbc);
        }
        FreeLibrary(module);
        return kShimExportMissing;
    }

    void* handle_storage =
        HeapAlloc(GetProcessHeap(), HEAP_ZERO_MEMORY, sizeof(RrNvFbcHandle));
    if (!handle_storage) {
        release(params.nvfbc);
        FreeLibrary(module);
        return kShimOutOfMemory;
    }
    auto* handle = ::new (handle_storage) RrNvFbcHandle{};
    handle->module = module;
    handle->session = params.nvfbc;
    handle->setup = setup;
    handle->grab = grab;
    handle->gpu_sleep = gpu_sleep;
    handle->release = release;
    out_info->max_width = params.max_display_width;
    out_info->max_height = params.max_display_height;
    out_info->nvfbc_version = params.nvfbc_version;
    *out_handle = handle;
    return 0;
}

extern "C" std::int32_t rr_nvfbc_setup(RrNvFbcHandle* handle, void* const* d3d9_surfaces,
                                        std::uint32_t surface_count, std::uint32_t hdr,
                                        std::uint32_t cursor) noexcept {
    if (!handle || !handle->session || !d3d9_surfaces || surface_count == 0 ||
        surface_count > 3) {
        return -2;
    }
    ZeroMemory(handle->output_buffers, sizeof(handle->output_buffers));
    handle->output_buffer_count = 0;
    for (std::uint32_t index = 0; index < surface_count; ++index) {
        if (!d3d9_surfaces[index]) {
            return -2;
        }
        handle->output_buffers[index].primary =
            static_cast<IDirect3DSurface9*>(d3d9_surfaces[index]);
    }

    NvFbcSetupParams params{};
    params.version = struct_version(sizeof(params), 3);
    params.flags = (cursor != 0 ? 1u : 0u) | (hdr != 0 ? (1u << 4) : 0u);
    params.mode = kArgb10;
    params.buffer_count = surface_count;
    params.buffers = handle->output_buffers;
    const std::int32_t status = handle->setup(handle->session, &params);
    if (status == 0) {
        handle->output_buffer_count = surface_count;
    }
    return status;
}

extern "C" std::int32_t rr_nvfbc_grab(RrNvFbcHandle* handle, std::uint32_t surface_index,
                                       RrNvFbcGrabInfo* out_info) noexcept {
    if (!handle || !handle->session || !out_info ||
        surface_index >= handle->output_buffer_count) {
        return -2;
    }
    NvFbcFrameGrabInfo frame_info{};
    NvFbcGrabParams params{};
    params.version = struct_version(sizeof(params), 1);
    params.flags = kGrabNowait;
    params.mode = 0;
    params.buffer_index = surface_index;
    params.frame_info = &frame_info;
    const std::int32_t status = handle->grab(handle->session, &params);
    *out_info = {
        frame_info.width,
        frame_info.height,
        frame_info.buffer_width,
        static_cast<std::uint32_t>(frame_info.must_recreate != 0),
        static_cast<std::uint32_t>(frame_info.protected_content != 0),
        frame_info.driver_internal_error,
        frame_info.source_pid,
        frame_info.flags & 1u,
        frame_info.wait_mode_used,
    };
    return status;
}

extern "C" std::int32_t rr_nvfbc_gpu_sleep(RrNvFbcHandle* handle,
                                            std::int64_t microseconds) noexcept {
    if (!handle || !handle->session || microseconds < 0) {
        return -2;
    }
    return handle->gpu_sleep(handle->session, microseconds);
}

extern "C" std::int32_t rr_nvfbc_destroy(RrNvFbcHandle* handle) noexcept {
    if (!handle) {
        return 0;
    }
    std::int32_t status = 0;
    if (handle->session) {
        status = handle->release(handle->session);
        handle->session = nullptr;
    }
    if (handle->module) {
        FreeLibrary(handle->module);
        handle->module = nullptr;
    }
    handle->~RrNvFbcHandle();
    HeapFree(GetProcessHeap(), 0, handle);
    return status;
}
