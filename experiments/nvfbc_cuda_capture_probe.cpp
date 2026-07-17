#define WIN32_LEAN_AND_MEAN
#define NOMINMAX

#include <windows.h>
#include <d3d9.h>

#include <cuda.h>
#include <cudaD3D9.h>

#include <algorithm>
#include <cstdint>
#include <cstring>
#include <cstdio>
#include <cstdlib>
#include <iterator>
#include <utility>
#include <vector>

#include "nvFBC.h"
#include "nvFBCCuda.h"

namespace {

constexpr uint32_t kNvidiaVendorId = 0x10de;
constexpr uint32_t kPrivateData[4] = {
    0xaef57ac5,
    0x401d1a39,
    0x1b856bbe,
    0x9ed0ceba,
};

template <typename T>
class ComPtr {
public:
    ComPtr() = default;
    ~ComPtr() {
        reset();
    }
    ComPtr(const ComPtr&) = delete;
    ComPtr& operator=(const ComPtr&) = delete;
    ComPtr(ComPtr&& other) noexcept : value_(std::exchange(other.value_, nullptr)) {}
    ComPtr& operator=(ComPtr&& other) noexcept {
        if (this != &other) {
            reset(std::exchange(other.value_, nullptr));
        }
        return *this;
    }

    T* get() const {
        return value_;
    }
    T** put() {
        reset();
        return &value_;
    }
    T* operator->() const {
        return value_;
    }
    explicit operator bool() const {
        return value_ != nullptr;
    }
    void reset(T* value = nullptr) {
        if (value_) {
            value_->Release();
        }
        value_ = value;
    }

private:
    T* value_ = nullptr;
};

class Module {
public:
    explicit Module(const wchar_t* name) : value_(LoadLibraryW(name)) {}
    ~Module() {
        if (value_) {
            FreeLibrary(value_);
        }
    }
    Module(const Module&) = delete;
    Module& operator=(const Module&) = delete;
    explicit operator bool() const {
        return value_ != nullptr;
    }
    template <typename T>
    T proc_as(const char* name) const {
        FARPROC raw = value_ ? GetProcAddress(value_, name) : nullptr;
        T typed = nullptr;
        static_assert(sizeof(typed) == sizeof(raw));
        std::memcpy(&typed, &raw, sizeof(typed));
        return typed;
    }

private:
    HMODULE value_ = nullptr;
};

class NvFbcCudaSession {
public:
    ~NvFbcCudaSession() {
        release();
    }
    NvFBCCuda* get() const {
        return value_;
    }
    NvFBCCuda** put() {
        release();
        return &value_;
    }
    void release() {
        if (value_) {
            const NVFBCRESULT status = value_->NvFBCCudaRelease();
            std::printf("nvfbc_cuda_release status=%d\n", static_cast<int>(status));
            value_ = nullptr;
        }
    }

private:
    NvFBCCuda* value_ = nullptr;
};

bool find_nvidia_adapter(IDirect3D9Ex* d3d, UINT& adapter) {
    for (UINT index = 0; index < d3d->GetAdapterCount(); ++index) {
        D3DADAPTER_IDENTIFIER9 identifier{};
        if (FAILED(d3d->GetAdapterIdentifier(index, 0, &identifier))) {
            continue;
        }
        std::printf("d3d9_adapter=%u vendor=0x%04x device=0x%04x description=%s\n", index,
                    static_cast<uint32_t>(identifier.VendorId),
                    static_cast<uint32_t>(identifier.DeviceId), identifier.Description);
        if (identifier.VendorId == kNvidiaVendorId) {
            adapter = index;
            return true;
        }
    }
    return false;
}

bool create_d3d9_device(IDirect3D9Ex* d3d, UINT adapter, ComPtr<IDirect3DDevice9Ex>& device) {
    D3DPRESENT_PARAMETERS present{};
    present.BackBufferWidth = 1;
    present.BackBufferHeight = 1;
    present.BackBufferFormat = D3DFMT_X8R8G8B8;
    present.BackBufferCount = 1;
    present.SwapEffect = D3DSWAPEFFECT_COPY;
    present.hDeviceWindow = GetDesktopWindow();
    present.Windowed = TRUE;
    present.Flags = D3DPRESENTFLAG_VIDEO;
    present.PresentationInterval = D3DPRESENT_INTERVAL_IMMEDIATE;
    const DWORD behavior = D3DCREATE_FPU_PRESERVE | D3DCREATE_MULTITHREADED |
                           D3DCREATE_HARDWARE_VERTEXPROCESSING;
    const HRESULT result = d3d->CreateDeviceEx(adapter, D3DDEVTYPE_HAL, GetDesktopWindow(),
                                                behavior, &present, nullptr, device.put());
    std::printf("d3d9_create_device hr=0x%08lx\n", static_cast<unsigned long>(result));
    return SUCCEEDED(result) && device;
}

bool cuda_ok(const char* stage, CUresult status) {
    const char* name = nullptr;
    const char* message = nullptr;
    cuGetErrorName(status, &name);
    cuGetErrorString(status, &message);
    std::printf("cuda_stage=%s status=%d name=%s message=%s\n", stage,
                static_cast<int>(status), name ? name : "unknown",
                message ? message : "unknown");
    return status == CUDA_SUCCESS;
}

int run(uint32_t buffer_count, uint32_t frames, bool hdr, bool ten_bit) {
    if (!cuda_ok("init", cuInit(0))) {
        return 1;
    }

    ComPtr<IDirect3D9Ex> d3d;
    HRESULT hr = Direct3DCreate9Ex(D3D_SDK_VERSION, d3d.put());
    if (FAILED(hr) || !d3d) {
        std::printf("d3d9_create hr=0x%08lx\n", static_cast<unsigned long>(hr));
        return 1;
    }
    UINT adapter = 0;
    if (!find_nvidia_adapter(d3d.get(), adapter)) {
        std::printf("nvidia_d3d9_adapter_found=false\n");
        return 1;
    }
    ComPtr<IDirect3DDevice9Ex> device;
    if (!create_d3d9_device(d3d.get(), adapter, device)) {
        return 1;
    }

    CUcontext cuda_context = nullptr;
    CUdevice cuda_device = 0;
    if (!cuda_ok("d3d9_context",
                 cuD3D9CtxCreate(&cuda_context, &cuda_device, CU_CTX_SCHED_AUTO,
                                 device.get()))) {
        return 1;
    }
    std::printf("cuda_context_created=true device=%d context=0x%p\n", cuda_device,
                cuda_context);
    CUcontext current_context = nullptr;
    if (!cuda_ok("context_get_current", cuCtxGetCurrent(&current_context)) ||
        current_context != cuda_context) {
        std::printf("cuda_context_current=false expected=0x%p actual=0x%p\n", cuda_context,
                    current_context);
        cuda_ok("context_destroy", cuCtxDestroy(cuda_context));
        return 1;
    }

    int result = 1;
    std::vector<CUdeviceptr> buffers;
    Module nvfbc_module(L"NvFBC64.dll");
    NvFbcCudaSession nvfbc;
    do {
        if (!nvfbc_module) {
            std::printf("nvfbc_load=false error=%lu\n", GetLastError());
            break;
        }
        const auto create_nvfbc =
            nvfbc_module.proc_as<NvFBC_CreateFunctionExType>("NvFBC_CreateEx");
        if (!create_nvfbc) {
            std::printf("nvfbc_create_export=false\n");
            break;
        }

        uint32_t private_data[4];
        std::copy(std::begin(kPrivateData), std::end(kPrivateData), private_data);
        NvFBCCreateParams create{};
        create.dwVersion = NVFBC_CREATE_PARAMS_VER;
        create.dwInterfaceType = NVFBC_SHARED_CUDA;
        create.pDevice = device.get();
        create.cudaCtx = cuda_context;
        create.pPrivateData = private_data;
        create.dwPrivateDataSize = sizeof(private_data);
        create.dwAdapterIdx = adapter;
        const NVFBCRESULT create_status = create_nvfbc(&create);
        std::printf(
            "nvfbc_cuda_create status=%d width=%u height=%u interface=0x%p version=%u\n",
            static_cast<int>(create_status), static_cast<uint32_t>(create.dwMaxDisplayWidth),
            static_cast<uint32_t>(create.dwMaxDisplayHeight), create.pNvFBC,
            static_cast<uint32_t>(create.dwNvFBCVersion));
        if (create_status != NVFBC_SUCCESS || !create.pNvFBC) {
            break;
        }
        *nvfbc.put() = static_cast<NvFBCCuda*>(create.pNvFBC);

        NvU32 max_buffer_size = 0;
        const NVFBCRESULT size_status =
            nvfbc.get()->NvFBCCudaGetMaxBufferSize(&max_buffer_size);
        std::printf("nvfbc_cuda_max_buffer_size status=%d bytes=%u\n",
                    static_cast<int>(size_status), static_cast<uint32_t>(max_buffer_size));
        if (size_status != NVFBC_SUCCESS || max_buffer_size == 0) {
            break;
        }

        NVFBC_CUDA_SETUP_PARAMS setup{};
        setup.dwVersion = NVFBC_CUDA_SETUP_PARAMS_VER;
        setup.bHDRRequest = hdr ? 1u : 0u;
        setup.eFormat = ten_bit ? NVFBC_TOCUDA_ARGB10 : NVFBC_TOCUDA_ARGB;
        if (!cuda_ok("context_set_current_before_setup", cuCtxSetCurrent(cuda_context))) {
            break;
        }
        const NVFBCRESULT setup_status = nvfbc.get()->NvFBCCudaSetup(&setup);
        std::printf("nvfbc_cuda_setup status=%d format=%s hdr_request=%s\n",
                    static_cast<int>(setup_status), ten_bit ? "ARGB10" : "ARGB",
                    hdr ? "true" : "false");
        if (setup_status != NVFBC_SUCCESS) {
            break;
        }

        buffers.resize(buffer_count, 0);
        bool allocation_ok = true;
        for (uint32_t index = 0; index < buffer_count; ++index) {
            const CUresult status = cuMemAlloc(&buffers[index], max_buffer_size);
            if (!cuda_ok("mem_alloc", status)) {
                allocation_ok = false;
                break;
            }
            std::printf("cuda_buffer_allocated index=%u pointer=0x%llx bytes=%u\n", index,
                        static_cast<unsigned long long>(buffers[index]),
                        static_cast<uint32_t>(max_buffer_size));
        }
        if (!allocation_ok) {
            break;
        }

        NvFBCFrameGrabInfo frame_info{};
        NVFBC_CUDA_GRAB_FRAME_PARAMS grab{};
        grab.dwVersion = NVFBC_CUDA_GRAB_FRAME_PARAMS_VER;
        grab.dwFlags = NVFBC_TOCUDA_NOWAIT | NVFBC_TOCUDA_CPU_SYNC;
        grab.pNvFBCFrameGrabInfo = &frame_info;
        uint32_t completed = 0;
        for (uint32_t frame = 0; frame < frames; ++frame) {
            nvfbc.get()->NvFBCCudaGPUBasedCPUSleep(4200);
            const uint32_t slot = frame % buffer_count;
            grab.pCUDADeviceBuffer =
                reinterpret_cast<void*>(static_cast<uintptr_t>(buffers[slot]));
            const NVFBCRESULT grab_status = nvfbc.get()->NvFBCCudaGrabFrame(&grab);
            if (grab_status != NVFBC_SUCCESS) {
                std::printf(
                    "nvfbc_cuda_grab status=%d frame=%u slot=%u driver=0x%08lx recreate=%d\n",
                    static_cast<int>(grab_status), frame, slot,
                    static_cast<unsigned long>(frame_info.dwDriverInternalError),
                    frame_info.bMustRecreate);
                break;
            }
            ++completed;
        }
        std::printf(
            "nvfbc_cuda_capture_summary buffers=%u requested=%u completed=%u "
            "bytes_per_buffer=%u explicit_gpu_copies=0\n",
            buffer_count, frames, completed, static_cast<uint32_t>(max_buffer_size));
        result = completed == frames ? 0 : 1;
    } while (false);

    nvfbc.release();
    for (CUdeviceptr buffer : buffers) {
        if (buffer != 0) {
            cuda_ok("mem_free", cuMemFree(buffer));
        }
    }
    cuda_ok("context_destroy", cuCtxDestroy(cuda_context));
    return result;
}

} // namespace

int main(int argc, char** argv) {
    uint32_t buffer_count = 32;
    uint32_t frames = 96;
    bool hdr = true;
    bool ten_bit = true;
    if (argc >= 2) {
        buffer_count = static_cast<uint32_t>(std::strtoul(argv[1], nullptr, 10));
    }
    if (argc >= 3) {
        frames = static_cast<uint32_t>(std::strtoul(argv[2], nullptr, 10));
    }
    if (argc >= 4) {
        hdr = std::strtoul(argv[3], nullptr, 10) != 0;
    }
    if (argc >= 5) {
        if (std::strcmp(argv[4], "argb") == 0) {
            ten_bit = false;
        } else if (std::strcmp(argv[4], "argb10") == 0) {
            ten_bit = true;
        } else {
            std::printf("invalid_format=%s expected=argb|argb10\n", argv[4]);
            return 2;
        }
    }
    if (buffer_count == 0 || buffer_count > 64 || frames == 0) {
        std::printf("invalid_parameters buffers=%u frames=%u\n", buffer_count, frames);
        return 2;
    }
    std::printf("nvfbc_cuda_capture_probe buffers=%u frames=%u hdr=%s format=%s\n",
                buffer_count, frames, hdr ? "true" : "false",
                ten_bit ? "ARGB10" : "ARGB");
    return run(buffer_count, frames, hdr, ten_bit);
}
