#define WIN32_LEAN_AND_MEAN
#define NOMINMAX

#include <windows.h>
#include <d3d9.h>
#include <dxgi.h>

#include <algorithm>
#include <chrono>
#include <cstdint>
#include <cstring>
#include <cstdio>
#include <cstdlib>
#include <fstream>
#include <memory>
#include <string>
#include <vector>

#include "nvFBC.h"
#include "nvFBCToDx9Vid.h"
#include "nvEncodeAPI.h"

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
    FARPROC proc(const char* name) const {
        return GetProcAddress(value_, name);
    }
    template <typename T>
    T proc_as(const char* name) const {
        FARPROC raw = proc(name);
        T typed = nullptr;
        static_assert(sizeof(typed) == sizeof(raw));
        std::memcpy(&typed, &raw, sizeof(typed));
        return typed;
    }

private:
    HMODULE value_ = nullptr;
};

const char* nvenc_status_name(NVENCSTATUS status) {
    switch (status) {
    case NV_ENC_SUCCESS:
        return "NV_ENC_SUCCESS";
    case NV_ENC_ERR_NO_ENCODE_DEVICE:
        return "NV_ENC_ERR_NO_ENCODE_DEVICE";
    case NV_ENC_ERR_UNSUPPORTED_DEVICE:
        return "NV_ENC_ERR_UNSUPPORTED_DEVICE";
    case NV_ENC_ERR_INVALID_ENCODERDEVICE:
        return "NV_ENC_ERR_INVALID_ENCODERDEVICE";
    case NV_ENC_ERR_INVALID_DEVICE:
        return "NV_ENC_ERR_INVALID_DEVICE";
    case NV_ENC_ERR_DEVICE_NOT_EXIST:
        return "NV_ENC_ERR_DEVICE_NOT_EXIST";
    case NV_ENC_ERR_INVALID_PTR:
        return "NV_ENC_ERR_INVALID_PTR";
    case NV_ENC_ERR_INVALID_PARAM:
        return "NV_ENC_ERR_INVALID_PARAM";
    case NV_ENC_ERR_ENCODER_NOT_INITIALIZED:
        return "NV_ENC_ERR_ENCODER_NOT_INITIALIZED";
    case NV_ENC_ERR_UNSUPPORTED_PARAM:
        return "NV_ENC_ERR_UNSUPPORTED_PARAM";
    case NV_ENC_ERR_RESOURCE_REGISTER_FAILED:
        return "NV_ENC_ERR_RESOURCE_REGISTER_FAILED";
    case NV_ENC_ERR_RESOURCE_NOT_REGISTERED:
        return "NV_ENC_ERR_RESOURCE_NOT_REGISTERED";
    case NV_ENC_ERR_RESOURCE_NOT_MAPPED:
        return "NV_ENC_ERR_RESOURCE_NOT_MAPPED";
    case NV_ENC_ERR_NEED_MORE_INPUT:
        return "NV_ENC_ERR_NEED_MORE_INPUT";
    default:
        return "NV_ENC_ERROR";
    }
}

bool check_nvenc(const char* stage, NVENCSTATUS status) {
    std::printf("nvenc_stage=%s status=%d(%s)\n", stage, static_cast<int>(status),
                nvenc_status_name(status));
    return status == NV_ENC_SUCCESS;
}

uint64_t percentile_us(std::vector<uint64_t> values, double percentile) {
    if (values.empty()) {
        return 0;
    }
    std::sort(values.begin(), values.end());
    const auto index = static_cast<size_t>((values.size() - 1) * percentile + 0.5);
    return values[std::min(index, values.size() - 1)];
}

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
    const HRESULT result = d3d->CreateDeviceEx(adapter, D3DDEVTYPE_HAL, GetDesktopWindow(), behavior,
                                                &present, nullptr, device.put());
    std::printf("d3d9_create_device hr=0x%08lx\n", static_cast<unsigned long>(result));
    return SUCCEEDED(result) && device;
}

bool find_matching_output(IDirect3D9Ex* d3d, UINT d3d_adapter, ComPtr<IDXGIOutput>& match) {
    const HMONITOR target = d3d->GetAdapterMonitor(d3d_adapter);
    ComPtr<IDXGIFactory1> factory;
    HRESULT result = CreateDXGIFactory1(__uuidof(IDXGIFactory1),
                                        reinterpret_cast<void**>(factory.put()));
    if (FAILED(result)) {
        std::printf("dxgi_create_factory hr=0x%08lx\n", static_cast<unsigned long>(result));
        return false;
    }

    for (UINT adapter_index = 0;; ++adapter_index) {
        ComPtr<IDXGIAdapter1> adapter;
        result = factory->EnumAdapters1(adapter_index, adapter.put());
        if (result == DXGI_ERROR_NOT_FOUND) {
            break;
        }
        if (FAILED(result)) {
            return false;
        }
        for (UINT output_index = 0;; ++output_index) {
            ComPtr<IDXGIOutput> output;
            result = adapter->EnumOutputs(output_index, output.put());
            if (result == DXGI_ERROR_NOT_FOUND) {
                break;
            }
            if (FAILED(result)) {
                return false;
            }
            DXGI_OUTPUT_DESC desc{};
            if (SUCCEEDED(output->GetDesc(&desc)) && desc.Monitor == target) {
                output.get()->AddRef();
                match.reset(output.get());
                return true;
            }
        }
    }
    return false;
}

class NvencDirectD3d9 {
public:
    ~NvencDirectD3d9() {
        close();
    }

    bool open(IDirect3DDevice9Ex* device, IDirect3DSurface9* surface, uint32_t width,
              uint32_t height, uint32_t frame_rate) {
        module_ = std::make_unique<Module>(L"nvEncodeAPI64.dll");
        if (!*module_) {
            std::printf("nvenc_load=false error=%lu\n", GetLastError());
            return false;
        }
        using CreateInstanceFn =
            NVENCSTATUS(NVENCAPI*)(NV_ENCODE_API_FUNCTION_LIST* function_list);
        using GetMaxSupportedVersionFn = NVENCSTATUS(NVENCAPI*)(uint32_t* version);
        const auto create_instance =
            module_->proc_as<CreateInstanceFn>("NvEncodeAPICreateInstance");
        const auto get_max_version =
            module_->proc_as<GetMaxSupportedVersionFn>("NvEncodeAPIGetMaxSupportedVersion");
        if (!create_instance || !get_max_version) {
            std::printf("nvenc_exports_missing=true\n");
            return false;
        }

        uint32_t max_version = 0;
        if (!check_nvenc("get_max_supported_version", get_max_version(&max_version))) {
            return false;
        }
        std::printf("nvenc_api requested=0x%08x max_supported=0x%08x\n", NVENCAPI_VERSION,
                    max_version);

        functions_ = {};
        functions_.version = NV_ENCODE_API_FUNCTION_LIST_VER;
        if (!check_nvenc("create_instance", create_instance(&functions_))) {
            return false;
        }

        NV_ENC_OPEN_ENCODE_SESSION_EX_PARAMS open_params{};
        open_params.version = NV_ENC_OPEN_ENCODE_SESSION_EX_PARAMS_VER;
        open_params.deviceType = NV_ENC_DEVICE_TYPE_DIRECTX;
        open_params.device = device;
        open_params.apiVersion = NVENCAPI_VERSION;
        if (!check_nvenc("open_d3d9_session",
                         functions_.nvEncOpenEncodeSessionEx(&open_params, &encoder_))) {
            return false;
        }
        if (!encoder_) {
            std::printf("nvenc_encoder_null=true\n");
            return false;
        }

        uint32_t format_count = 0;
        if (!check_nvenc("get_input_format_count",
                         functions_.nvEncGetInputFormatCount(encoder_, NV_ENC_CODEC_HEVC_GUID,
                                                             &format_count))) {
            return false;
        }
        std::vector<NV_ENC_BUFFER_FORMAT> formats(format_count);
        uint32_t returned_count = 0;
        if (!check_nvenc("get_input_formats",
                         functions_.nvEncGetInputFormats(encoder_, NV_ENC_CODEC_HEVC_GUID,
                                                         formats.data(), format_count,
                                                         &returned_count))) {
            return false;
        }
        const bool abgr10_supported =
            std::find(formats.begin(), formats.begin() + returned_count,
                      NV_ENC_BUFFER_FORMAT_ABGR10) != formats.begin() + returned_count;
        std::printf("nvenc_input_formats count=%u abgr10_supported=%s\n", returned_count,
                    abgr10_supported ? "true" : "false");
        if (!abgr10_supported) {
            return false;
        }

        NV_ENC_PRESET_CONFIG preset{};
        preset.version = NV_ENC_PRESET_CONFIG_VER;
        preset.presetCfg.version = NV_ENC_CONFIG_VER;
        if (!check_nvenc(
                "get_preset_config",
                functions_.nvEncGetEncodePresetConfigEx(
                    encoder_, NV_ENC_CODEC_HEVC_GUID, NV_ENC_PRESET_P1_GUID,
                    NV_ENC_TUNING_INFO_LOW_LATENCY, &preset))) {
            return false;
        }

        NV_ENC_CONFIG config = preset.presetCfg;
        config.version = NV_ENC_CONFIG_VER;
        config.profileGUID = NV_ENC_HEVC_PROFILE_MAIN10_GUID;
        config.gopLength = frame_rate * 2;
        config.frameIntervalP = 1;
        config.rcParams.rateControlMode = NV_ENC_PARAMS_RC_CONSTQP;
        config.rcParams.constQP.qpIntra = 24;
        config.rcParams.constQP.qpInterP = 27;
        config.rcParams.constQP.qpInterB = 27;

        auto& hevc = config.encodeCodecConfig.hevcConfig;
        hevc.chromaFormatIDC = 1;
        hevc.idrPeriod = config.gopLength;
        hevc.repeatSPSPPS = 1;
        hevc.outputBitDepth = NV_ENC_BIT_DEPTH_10;
        hevc.inputBitDepth = NV_ENC_BIT_DEPTH_10;
        auto& vui = hevc.hevcVUIParameters;
        vui.videoSignalTypePresentFlag = 1;
        vui.videoFormat = NV_ENC_VUI_VIDEO_FORMAT_UNSPECIFIED;
        vui.videoFullRangeFlag = 1;
        vui.colourDescriptionPresentFlag = 1;
        vui.colourPrimaries = static_cast<NV_ENC_VUI_COLOR_PRIMARIES>(9);
        vui.transferCharacteristics = static_cast<NV_ENC_VUI_TRANSFER_CHARACTERISTIC>(16);
        vui.colourMatrix = static_cast<NV_ENC_VUI_MATRIX_COEFFS>(9);

        NV_ENC_INITIALIZE_PARAMS initialize{};
        initialize.version = NV_ENC_INITIALIZE_PARAMS_VER;
        initialize.encodeGUID = NV_ENC_CODEC_HEVC_GUID;
        initialize.presetGUID = NV_ENC_PRESET_P1_GUID;
        initialize.encodeWidth = width;
        initialize.encodeHeight = height;
        initialize.darWidth = width;
        initialize.darHeight = height;
        initialize.frameRateNum = frame_rate;
        initialize.frameRateDen = 1;
        initialize.enableEncodeAsync = 0;
        initialize.enablePTD = 1;
        initialize.encodeConfig = &config;
        initialize.maxEncodeWidth = width;
        initialize.maxEncodeHeight = height;
        initialize.tuningInfo = NV_ENC_TUNING_INFO_LOW_LATENCY;
        initialize.bufferFormat = NV_ENC_BUFFER_FORMAT_ABGR10;
        if (!check_nvenc("initialize_hevc_main10",
                         functions_.nvEncInitializeEncoder(encoder_, &initialize))) {
            return false;
        }

        NV_ENC_REGISTER_RESOURCE registration{};
        registration.version = NV_ENC_REGISTER_RESOURCE_VER;
        registration.resourceType = NV_ENC_INPUT_RESOURCE_TYPE_DIRECTX;
        registration.width = width;
        registration.height = height;
        registration.pitch = 0;
        registration.subResourceIndex = 0;
        registration.resourceToRegister = surface;
        registration.bufferFormat = NV_ENC_BUFFER_FORMAT_ABGR10;
        registration.bufferUsage = NV_ENC_INPUT_IMAGE;
        if (!check_nvenc("register_nvfbc_d3d9_surface",
                         functions_.nvEncRegisterResource(encoder_, &registration))) {
            return false;
        }
        registered_ = registration.registeredResource;
        if (!registered_) {
            std::printf("nvenc_registered_resource_null=true\n");
            return false;
        }

        NV_ENC_CREATE_BITSTREAM_BUFFER create_bitstream{};
        create_bitstream.version = NV_ENC_CREATE_BITSTREAM_BUFFER_VER;
        if (!check_nvenc("create_bitstream",
                         functions_.nvEncCreateBitstreamBuffer(encoder_, &create_bitstream))) {
            return false;
        }
        bitstream_ = create_bitstream.bitstreamBuffer;
        width_ = width;
        height_ = height;
        return bitstream_ != nullptr;
    }

    bool encode(uint32_t frame_index, uint64_t timestamp, std::ofstream& output,
                uint64_t& output_bytes, uint32_t& mapped_format) {
        NV_ENC_MAP_INPUT_RESOURCE map{};
        map.version = NV_ENC_MAP_INPUT_RESOURCE_VER;
        map.registeredResource = registered_;
        const NVENCSTATUS map_status = functions_.nvEncMapInputResource(encoder_, &map);
        if (map_status != NV_ENC_SUCCESS) {
            check_nvenc("map_input", map_status);
            return false;
        }
        mapped_ = map.mappedResource;
        mapped_format = static_cast<uint32_t>(map.mappedBufferFmt);

        NV_ENC_PIC_PARAMS picture{};
        picture.version = NV_ENC_PIC_PARAMS_VER;
        picture.inputWidth = width_;
        picture.inputHeight = height_;
        picture.inputPitch = 0;
        picture.encodePicFlags = frame_index == 0
                                     ? NV_ENC_PIC_FLAG_FORCEIDR | NV_ENC_PIC_FLAG_OUTPUT_SPSPPS
                                     : 0;
        picture.frameIdx = frame_index;
        picture.inputTimeStamp = timestamp;
        picture.inputDuration = 1;
        picture.inputBuffer = mapped_;
        picture.outputBitstream = bitstream_;
        picture.bufferFmt = map.mappedBufferFmt;
        picture.pictureStruct = NV_ENC_PIC_STRUCT_FRAME;
        const NVENCSTATUS encode_status = functions_.nvEncEncodePicture(encoder_, &picture);
        if (encode_status != NV_ENC_SUCCESS) {
            check_nvenc("encode_picture", encode_status);
            unmap();
            return false;
        }

        NV_ENC_LOCK_BITSTREAM lock{};
        lock.version = NV_ENC_LOCK_BITSTREAM_VER;
        lock.outputBitstream = bitstream_;
        const NVENCSTATUS lock_status = functions_.nvEncLockBitstream(encoder_, &lock);
        if (lock_status != NV_ENC_SUCCESS) {
            check_nvenc("lock_bitstream", lock_status);
            unmap();
            return false;
        }
        if (!lock.bitstreamBufferPtr || lock.bitstreamSizeInBytes == 0) {
            std::printf("nvenc_empty_bitstream frame=%u\n", frame_index);
            functions_.nvEncUnlockBitstream(encoder_, bitstream_);
            unmap();
            return false;
        }
        output.write(static_cast<const char*>(lock.bitstreamBufferPtr),
                     static_cast<std::streamsize>(lock.bitstreamSizeInBytes));
        output_bytes += lock.bitstreamSizeInBytes;
        const bool write_ok = output.good();
        const NVENCSTATUS unlock_status =
            functions_.nvEncUnlockBitstream(encoder_, bitstream_);
        const bool unmap_ok = unmap();
        if (unlock_status != NV_ENC_SUCCESS) {
            check_nvenc("unlock_bitstream", unlock_status);
            return false;
        }
        return write_ok && unmap_ok;
    }

private:
    bool unmap() {
        if (!mapped_) {
            return true;
        }
        const NVENCSTATUS status = functions_.nvEncUnmapInputResource(encoder_, mapped_);
        mapped_ = nullptr;
        if (status != NV_ENC_SUCCESS) {
            check_nvenc("unmap_input", status);
            return false;
        }
        return true;
    }

    void close() {
        unmap();
        if (encoder_ && registered_) {
            functions_.nvEncUnregisterResource(encoder_, registered_);
            registered_ = nullptr;
        }
        if (encoder_ && bitstream_) {
            functions_.nvEncDestroyBitstreamBuffer(encoder_, bitstream_);
            bitstream_ = nullptr;
        }
        if (encoder_) {
            functions_.nvEncDestroyEncoder(encoder_);
            encoder_ = nullptr;
        }
        module_.reset();
    }

    std::unique_ptr<Module> module_;
    NV_ENCODE_API_FUNCTION_LIST functions_{};
    void* encoder_ = nullptr;
    NV_ENC_REGISTERED_PTR registered_ = nullptr;
    NV_ENC_INPUT_PTR mapped_ = nullptr;
    NV_ENC_OUTPUT_PTR bitstream_ = nullptr;
    uint32_t width_ = 0;
    uint32_t height_ = 0;
};

class NvFbcSession {
public:
    ~NvFbcSession() {
        if (interface_) {
            interface_->NvFBCToDx9VidRelease();
        }
    }
    NvFBCToDx9Vid* get() const {
        return interface_;
    }
    NvFBCToDx9Vid** put() {
        return &interface_;
    }

private:
    NvFBCToDx9Vid* interface_ = nullptr;
};

int run(uint32_t requested_frames, const char* output_path) {
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
    ComPtr<IDXGIOutput> dxgi_output;
    if (!find_matching_output(d3d.get(), adapter, dxgi_output)) {
        std::printf("matching_dxgi_output=false\n");
        return 1;
    }

    Module nvfbc_module(L"NvFBC64.dll");
    if (!nvfbc_module) {
        std::printf("nvfbc_load=false error=%lu\n", GetLastError());
        return 1;
    }
    const auto create_nvfbc =
        nvfbc_module.proc_as<NvFBC_CreateFunctionExType>("NvFBC_CreateEx");
    if (!create_nvfbc) {
        std::printf("nvfbc_create_export=false\n");
        return 1;
    }

    uint32_t private_data[4];
    std::copy(std::begin(kPrivateData), std::end(kPrivateData), private_data);
    NvFBCCreateParams create{};
    create.dwVersion = NVFBC_CREATE_PARAMS_VER;
    create.dwInterfaceType = NVFBC_TO_DX9_VID;
    create.pDevice = device.get();
    create.pPrivateData = private_data;
    create.dwPrivateDataSize = sizeof(private_data);
    create.dwAdapterIdx = adapter;
    const NVFBCRESULT create_result = create_nvfbc(&create);
    std::printf("nvfbc_create status=%d width=%u height=%u interface=0x%p\n",
                static_cast<int>(create_result),
                static_cast<uint32_t>(create.dwMaxDisplayWidth),
                static_cast<uint32_t>(create.dwMaxDisplayHeight), create.pNvFBC);
    if (create_result != NVFBC_SUCCESS || !create.pNvFBC) {
        return 1;
    }

    NvFbcSession nvfbc;
    *nvfbc.put() = static_cast<NvFBCToDx9Vid*>(create.pNvFBC);
    const uint32_t width = create.dwMaxDisplayWidth;
    const uint32_t height = create.dwMaxDisplayHeight;
    ComPtr<IDirect3DSurface9> surface;
    hr = device->CreateRenderTarget(width, height, D3DFMT_A2B10G10R10,
                                    D3DMULTISAMPLE_NONE, 0, FALSE, surface.put(), nullptr);
    std::printf("surface_create format=A2B10G10R10 hr=0x%08lx ptr=0x%p\n",
                static_cast<unsigned long>(hr), surface.get());
    if (FAILED(hr) || !surface) {
        return 1;
    }

    NVFBC_TODX9VID_OUT_BUF output_buffer{};
    output_buffer.pPrimary = surface.get();
    NVFBC_TODX9VID_SETUP_PARAMS setup{};
    setup.dwVersion = NVFBC_TODX9VID_SETUP_PARAMS_V3_VER;
    setup.bHDRRequest = TRUE;
    setup.eMode = NVFBC_TODX9VID_ARGB10;
    setup.dwNumBuffers = 1;
    setup.ppBuffer = &output_buffer;
    const NVFBCRESULT setup_result = nvfbc.get()->NvFBCToDx9VidSetUp(&setup);
    std::printf("nvfbc_setup status=%d format=ARGB10 hdr_request=true\n",
                static_cast<int>(setup_result));
    if (setup_result != NVFBC_SUCCESS) {
        return 1;
    }

    constexpr uint32_t frame_rate = 240;
    NvencDirectD3d9 encoder;
    if (!encoder.open(device.get(), surface.get(), width, height, frame_rate)) {
        std::printf("direct_d3d9_nvenc_open=false\n");
        return 1;
    }

    std::ofstream output(output_path, std::ios::binary | std::ios::trunc);
    if (!output) {
        std::printf("output_open=false path=%s\n", output_path);
        return 1;
    }

    NVFBC_TODX9VID_GRAB_FRAME_PARAMS grab{};
    NvFBCFrameGrabInfo frame_info{};
    grab.dwVersion = NVFBC_TODX9VID_GRAB_FRAME_PARAMS_V1_VER;
    grab.dwFlags = NVFBC_TODX9VID_NOWAIT;
    grab.eGMode = NVFBC_TODX9VID_SOURCEMODE_FULL;
    grab.dwBufferIdx = 0;
    grab.pNvFBCFrameGrabInfo = &frame_info;

    std::vector<uint64_t> grab_us;
    std::vector<uint64_t> encode_us;
    grab_us.reserve(requested_frames);
    encode_us.reserve(requested_frames);
    uint64_t output_bytes = 0;
    uint32_t completed_frames = 0;
    uint32_t mapped_format = 0;
    const auto run_start = std::chrono::steady_clock::now();
    for (uint32_t frame = 0; frame < requested_frames; ++frame) {
        hr = dxgi_output->WaitForVBlank();
        if (FAILED(hr)) {
            std::printf("wait_vblank_failed frame=%u hr=0x%08lx\n", frame,
                        static_cast<unsigned long>(hr));
            break;
        }
        const auto grab_start = std::chrono::steady_clock::now();
        const NVFBCRESULT grab_result = nvfbc.get()->NvFBCToDx9VidGrabFrame(&grab);
        const auto grab_end = std::chrono::steady_clock::now();
        grab_us.push_back(static_cast<uint64_t>(
            std::chrono::duration_cast<std::chrono::microseconds>(grab_end - grab_start).count()));
        if (grab_result != NVFBC_SUCCESS) {
            std::printf("nvfbc_grab_failed frame=%u status=%d driver=0x%08lx recreate=%d\n",
                        frame, static_cast<int>(grab_result), frame_info.dwDriverInternalError,
                        frame_info.bMustRecreate);
            break;
        }

        const auto encode_start = std::chrono::steady_clock::now();
        if (!encoder.encode(frame, frame, output, output_bytes, mapped_format)) {
            std::printf("nvenc_encode_failed frame=%u\n", frame);
            break;
        }
        const auto encode_end = std::chrono::steady_clock::now();
        encode_us.push_back(static_cast<uint64_t>(
            std::chrono::duration_cast<std::chrono::microseconds>(encode_end - encode_start)
                .count()));
        ++completed_frames;
    }
    output.flush();
    const auto run_end = std::chrono::steady_clock::now();
    const double elapsed_seconds =
        std::chrono::duration<double>(run_end - run_start).count();
    const double fps = elapsed_seconds > 0.0 ? completed_frames / elapsed_seconds : 0.0;
    std::printf(
        "direct_encode_summary requested=%u completed=%u elapsed_ms=%.3f fps=%.3f bytes=%llu "
        "mapped_format=0x%08x grab_p50_us=%llu grab_p95_us=%llu encode_p50_us=%llu "
        "encode_p95_us=%llu encode_max_us=%llu explicit_gpu_copies=0 output=%s\n",
        requested_frames, completed_frames, elapsed_seconds * 1000.0, fps,
        static_cast<unsigned long long>(output_bytes), mapped_format,
        static_cast<unsigned long long>(percentile_us(grab_us, 0.50)),
        static_cast<unsigned long long>(percentile_us(grab_us, 0.95)),
        static_cast<unsigned long long>(percentile_us(encode_us, 0.50)),
        static_cast<unsigned long long>(percentile_us(encode_us, 0.95)),
        static_cast<unsigned long long>(
            encode_us.empty() ? 0 : *std::max_element(encode_us.begin(), encode_us.end())),
        output_path);
    return completed_frames == requested_frames && output_bytes > 0 ? 0 : 1;
}

} // namespace

int main(int argc, char** argv) {
    uint32_t frames = 600;
    const char* output = "nvfbc_nvenc_direct.hevc";
    if (argc >= 2) {
        frames = static_cast<uint32_t>(std::strtoul(argv[1], nullptr, 10));
    }
    if (argc >= 3) {
        output = argv[2];
    }
    std::printf("nvfbc_nvenc_direct frames=%u output=%s\n", frames, output);
    return run(frames, output);
}
