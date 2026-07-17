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
#include <deque>
#include <fstream>
#include <memory>
#include <numeric>
#include <optional>
#include <string>
#include <utility>
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

enum class ChromaSampling : uint32_t {
    Yuv420 = 1,
    Yuv422 = 2,
    Yuv444 = 3,
};

const char* chroma_label(ChromaSampling chroma) {
    switch (chroma) {
    case ChromaSampling::Yuv420:
        return "420";
    case ChromaSampling::Yuv422:
        return "422";
    case ChromaSampling::Yuv444:
        return "444";
    }
    return "unknown";
}

bool parse_chroma(const char* value, ChromaSampling& chroma) {
    if (std::strcmp(value, "420") == 0) {
        chroma = ChromaSampling::Yuv420;
        return true;
    }
    if (std::strcmp(value, "422") == 0) {
        chroma = ChromaSampling::Yuv422;
        return true;
    }
    if (std::strcmp(value, "444") == 0) {
        chroma = ChromaSampling::Yuv444;
        return true;
    }
    return false;
}

struct EncodeOptions {
    ChromaSampling chroma = ChromaSampling::Yuv420;
    uint32_t frame_rate_num = 60;
    uint32_t frame_rate_den = 1;
    uint32_t qp_intra = 24;
    uint32_t qp_inter_p = 27;
    uint32_t qp_inter_b = 27;
};

struct DirectRouteCaps {
    bool hevc = false;
    bool abgr10_input = false;
    bool main10_profile = false;
    bool frext_profile = false;
    bool ten_bit = false;
    bool yuv422 = false;
    bool yuv444 = false;
    uint32_t max_width = 0;
    uint32_t max_height = 0;

    bool advertises(ChromaSampling chroma, uint32_t width, uint32_t height) const {
        if (!hevc || !abgr10_input || !ten_bit || width > max_width || height > max_height) {
            return false;
        }
        switch (chroma) {
        case ChromaSampling::Yuv420:
            return main10_profile;
        case ChromaSampling::Yuv422:
            return frext_profile && yuv422;
        case ChromaSampling::Yuv444:
            return frext_profile && yuv444;
        }
        return false;
    }

    void print_json(uint32_t width, uint32_t height) const {
        std::printf(
            "route_caps_json={\"schema\":1,\"device_api\":\"D3D9Ex\",\"codec\":\"HEVC\","
            "\"input_format\":\"ABGR10\",\"hevc\":%s,\"abgr10_input\":%s,"
            "\"main10_profile\":%s,\"frext_profile\":%s,\"ten_bit\":%s,"
            "\"yuv422\":%s,\"yuv444\":%s,\"max_width\":%u,\"max_height\":%u,"
            "\"advertised_routes\":{\"420\":%s,\"422\":%s,\"444\":%s},"
            "\"lookahead\":false,\"lookahead_policy\":\"disabled_for_nvfbc\"}\n",
            hevc ? "true" : "false", abgr10_input ? "true" : "false",
            main10_profile ? "true" : "false", frext_profile ? "true" : "false",
            ten_bit ? "true" : "false", yuv422 ? "true" : "false",
            yuv444 ? "true" : "false", max_width, max_height,
            advertises(ChromaSampling::Yuv420, width, height) ? "true" : "false",
            advertises(ChromaSampling::Yuv422, width, height) ? "true" : "false",
            advertises(ChromaSampling::Yuv444, width, height) ? "true" : "false");
    }
};

struct DirectEncodeSlot {
    NV_ENC_REGISTERED_PTR registered = nullptr;
    NV_ENC_INPUT_PTR mapped = nullptr;
    NV_ENC_OUTPUT_PTR bitstream = nullptr;
    bool in_flight = false;
    uint32_t frame_index = 0;
    uint64_t timestamp = 0;
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

bool guid_equal(const GUID& left, const GUID& right) {
    return std::memcmp(&left, &right, sizeof(GUID)) == 0;
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

bool query_output_refresh(IDXGIOutput* output, uint32_t& numerator, uint32_t& denominator) {
    DXGI_OUTPUT_DESC desc{};
    if (FAILED(output->GetDesc(&desc))) {
        return false;
    }
    DEVMODEW mode{};
    mode.dmSize = sizeof(mode);
    if (!EnumDisplaySettingsW(desc.DeviceName, ENUM_CURRENT_SETTINGS, &mode) ||
        mode.dmDisplayFrequency <= 1) {
        return false;
    }
    numerator = mode.dmDisplayFrequency;
    denominator = 1;
    std::printf("display_refresh numerator=%u denominator=%u source=EnumDisplaySettingsW\n",
                numerator, denominator);
    return true;
}

class NvencDirectD3d9 {
public:
    ~NvencDirectD3d9() {
        close();
    }

    bool open(IDirect3DDevice9Ex* device, const std::vector<IDirect3DSurface9*>& surfaces,
              uint32_t width, uint32_t height, const EncodeOptions& options) {
        if (surfaces.empty() || surfaces.size() > 3) {
            std::printf("invalid_surface_count=%llu expected=1..3\n",
                        static_cast<unsigned long long>(surfaces.size()));
            return false;
        }
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

        caps_ = {};
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
        caps_.hevc = true;
        caps_.abgr10_input =
            std::find(formats.begin(), formats.begin() + returned_count,
                      NV_ENC_BUFFER_FORMAT_ABGR10) != formats.begin() + returned_count;
        std::printf("nvenc_input_formats count=%u abgr10_supported=%s\n", returned_count,
                    caps_.abgr10_input ? "true" : "false");

        uint32_t profile_count = 0;
        if (!check_nvenc("get_profile_count",
                         functions_.nvEncGetEncodeProfileGUIDCount(
                             encoder_, NV_ENC_CODEC_HEVC_GUID, &profile_count))) {
            return false;
        }
        std::vector<GUID> profiles(profile_count);
        uint32_t returned_profiles = 0;
        if (!check_nvenc("get_profiles",
                         functions_.nvEncGetEncodeProfileGUIDs(
                             encoder_, NV_ENC_CODEC_HEVC_GUID, profiles.data(), profile_count,
                             &returned_profiles))) {
            return false;
        }
        caps_.main10_profile =
            std::any_of(profiles.begin(), profiles.begin() + returned_profiles,
                        [](const GUID& profile) {
                            return guid_equal(profile, NV_ENC_HEVC_PROFILE_MAIN10_GUID);
                        });
        caps_.frext_profile =
            std::any_of(profiles.begin(), profiles.begin() + returned_profiles,
                        [](const GUID& profile) {
                            return guid_equal(profile, NV_ENC_HEVC_PROFILE_FREXT_GUID);
                        });

        const auto query_cap = [&](NV_ENC_CAPS cap, const char* stage, int& value) {
            NV_ENC_CAPS_PARAM parameter{};
            parameter.version = NV_ENC_CAPS_PARAM_VER;
            parameter.capsToQuery = cap;
            return check_nvenc(
                stage,
                functions_.nvEncGetEncodeCaps(encoder_, NV_ENC_CODEC_HEVC_GUID, &parameter,
                                              &value));
        };
        int ten_bit = 0;
        int yuv422 = 0;
        int yuv444 = 0;
        int max_width = 0;
        int max_height = 0;
        if (!query_cap(NV_ENC_CAPS_SUPPORT_10BIT_ENCODE, "cap_10bit", ten_bit) ||
            !query_cap(NV_ENC_CAPS_SUPPORT_YUV422_ENCODE, "cap_yuv422", yuv422) ||
            !query_cap(NV_ENC_CAPS_SUPPORT_YUV444_ENCODE, "cap_yuv444", yuv444) ||
            !query_cap(NV_ENC_CAPS_WIDTH_MAX, "cap_width_max", max_width) ||
            !query_cap(NV_ENC_CAPS_HEIGHT_MAX, "cap_height_max", max_height)) {
            return false;
        }
        caps_.ten_bit = ten_bit != 0;
        caps_.yuv422 = yuv422 != 0;
        caps_.yuv444 = yuv444 != 0;
        caps_.max_width = max_width > 0 ? static_cast<uint32_t>(max_width) : 0;
        caps_.max_height = max_height > 0 ? static_cast<uint32_t>(max_height) : 0;
        caps_.print_json(width, height);
        if (!caps_.advertises(options.chroma, width, height)) {
            std::printf("requested_route_advertised=false chroma=%s width=%u height=%u\n",
                        chroma_label(options.chroma), width, height);
            return false;
        }
        std::printf("requested_route_advertised=true chroma=%s width=%u height=%u\n",
                    chroma_label(options.chroma), width, height);

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
        config.profileGUID = options.chroma == ChromaSampling::Yuv420
                                 ? NV_ENC_HEVC_PROFILE_MAIN10_GUID
                                 : NV_ENC_HEVC_PROFILE_FREXT_GUID;
        const uint32_t nominal_fps =
            std::max(1u, (options.frame_rate_num + options.frame_rate_den - 1) /
                             std::max(1u, options.frame_rate_den));
        config.gopLength = nominal_fps * 2;
        config.frameIntervalP = 1;
        config.rcParams.rateControlMode = NV_ENC_PARAMS_RC_CONSTQP;
        config.rcParams.constQP.qpIntra = options.qp_intra;
        config.rcParams.constQP.qpInterP = options.qp_inter_p;
        config.rcParams.constQP.qpInterB = options.qp_inter_b;
        config.rcParams.enableLookahead = 0;
        config.rcParams.lookaheadDepth = 0;

        auto& hevc = config.encodeCodecConfig.hevcConfig;
        hevc.chromaFormatIDC = static_cast<uint32_t>(options.chroma);
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
        initialize.frameRateNum = options.frame_rate_num;
        initialize.frameRateDen = std::max(1u, options.frame_rate_den);
        initialize.enableEncodeAsync = 0;
        initialize.enablePTD = 1;
        initialize.encodeConfig = &config;
        initialize.maxEncodeWidth = width;
        initialize.maxEncodeHeight = height;
        initialize.tuningInfo = NV_ENC_TUNING_INFO_LOW_LATENCY;
        initialize.bufferFormat = NV_ENC_BUFFER_FORMAT_ABGR10;
        if (!check_nvenc("initialize_hevc_10bit",
                         functions_.nvEncInitializeEncoder(encoder_, &initialize))) {
            return false;
        }

        slots_.clear();
        slots_.reserve(surfaces.size());
        for (size_t index = 0; index < surfaces.size(); ++index) {
            NV_ENC_REGISTER_RESOURCE registration{};
            registration.version = NV_ENC_REGISTER_RESOURCE_VER;
            registration.resourceType = NV_ENC_INPUT_RESOURCE_TYPE_DIRECTX;
            registration.width = width;
            registration.height = height;
            registration.pitch = 0;
            registration.subResourceIndex = 0;
            registration.resourceToRegister = surfaces[index];
            registration.bufferFormat = NV_ENC_BUFFER_FORMAT_ABGR10;
            registration.bufferUsage = NV_ENC_INPUT_IMAGE;
            if (!check_nvenc("register_nvfbc_d3d9_surface",
                             functions_.nvEncRegisterResource(encoder_, &registration))) {
                return false;
            }
            if (!registration.registeredResource) {
                std::printf("nvenc_registered_resource_null=true slot=%llu\n",
                            static_cast<unsigned long long>(index));
                return false;
            }

            NV_ENC_CREATE_BITSTREAM_BUFFER create_bitstream{};
            create_bitstream.version = NV_ENC_CREATE_BITSTREAM_BUFFER_VER;
            if (!check_nvenc("create_bitstream",
                             functions_.nvEncCreateBitstreamBuffer(encoder_,
                                                                   &create_bitstream))) {
                return false;
            }
            if (!create_bitstream.bitstreamBuffer) {
                std::printf("nvenc_bitstream_null=true slot=%llu\n",
                            static_cast<unsigned long long>(index));
                return false;
            }
            slots_.push_back(DirectEncodeSlot{
                registration.registeredResource,
                nullptr,
                create_bitstream.bitstreamBuffer,
                false,
                0,
                0,
            });
        }
        width_ = width;
        height_ = height;
        std::printf("nvenc_surface_pool count=%llu lookahead=false\n",
                    static_cast<unsigned long long>(slots_.size()));
        std::printf("requested_route_initialized=true chroma=%s registered_surfaces=%llu\n",
                    chroma_label(options.chroma),
                    static_cast<unsigned long long>(slots_.size()));
        return true;
    }

    const DirectRouteCaps& capabilities() const {
        return caps_;
    }

    size_t slot_count() const {
        return slots_.size();
    }

    bool submit(size_t slot_index, uint32_t frame_index, uint64_t timestamp,
                uint32_t& mapped_format) {
        if (slot_index >= slots_.size() || slots_[slot_index].in_flight) {
            std::printf("nvenc_submit_invalid_slot slot=%llu in_flight=%s\n",
                        static_cast<unsigned long long>(slot_index),
                        slot_index < slots_.size() && slots_[slot_index].in_flight ? "true"
                                                                                  : "false");
            return false;
        }
        DirectEncodeSlot& slot = slots_[slot_index];
        NV_ENC_MAP_INPUT_RESOURCE map{};
        map.version = NV_ENC_MAP_INPUT_RESOURCE_VER;
        map.registeredResource = slot.registered;
        const NVENCSTATUS map_status = functions_.nvEncMapInputResource(encoder_, &map);
        if (map_status != NV_ENC_SUCCESS) {
            check_nvenc("map_input", map_status);
            return false;
        }
        slot.mapped = map.mappedResource;
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
        picture.inputBuffer = slot.mapped;
        picture.outputBitstream = slot.bitstream;
        picture.bufferFmt = map.mappedBufferFmt;
        picture.pictureStruct = NV_ENC_PIC_STRUCT_FRAME;
        const NVENCSTATUS encode_status = functions_.nvEncEncodePicture(encoder_, &picture);
        if (encode_status != NV_ENC_SUCCESS) {
            check_nvenc("encode_picture", encode_status);
            unmap_slot(slot);
            return false;
        }
        slot.in_flight = true;
        slot.frame_index = frame_index;
        slot.timestamp = timestamp;
        return true;
    }

    bool drain(size_t slot_index, std::ofstream& output, uint64_t& output_bytes) {
        if (slot_index >= slots_.size() || !slots_[slot_index].in_flight) {
            std::printf("nvenc_drain_invalid_slot slot=%llu\n",
                        static_cast<unsigned long long>(slot_index));
            return false;
        }
        DirectEncodeSlot& slot = slots_[slot_index];
        NV_ENC_LOCK_BITSTREAM lock{};
        lock.version = NV_ENC_LOCK_BITSTREAM_VER;
        lock.outputBitstream = slot.bitstream;
        const NVENCSTATUS lock_status = functions_.nvEncLockBitstream(encoder_, &lock);
        if (lock_status != NV_ENC_SUCCESS) {
            check_nvenc("lock_bitstream", lock_status);
            unmap_slot(slot);
            slot.in_flight = false;
            return false;
        }
        if (!lock.bitstreamBufferPtr || lock.bitstreamSizeInBytes == 0) {
            std::printf("nvenc_empty_bitstream frame=%u\n", slot.frame_index);
            functions_.nvEncUnlockBitstream(encoder_, slot.bitstream);
            unmap_slot(slot);
            slot.in_flight = false;
            return false;
        }
        if (lock.outputTimeStamp != slot.timestamp) {
            std::printf("nvenc_timestamp_mismatch frame=%u submitted=%llu returned=%llu\n",
                        slot.frame_index, static_cast<unsigned long long>(slot.timestamp),
                        static_cast<unsigned long long>(lock.outputTimeStamp));
            functions_.nvEncUnlockBitstream(encoder_, slot.bitstream);
            unmap_slot(slot);
            slot.in_flight = false;
            return false;
        }
        output.write(static_cast<const char*>(lock.bitstreamBufferPtr),
                     static_cast<std::streamsize>(lock.bitstreamSizeInBytes));
        output_bytes += lock.bitstreamSizeInBytes;
        const bool write_ok = output.good();
        const NVENCSTATUS unlock_status =
            functions_.nvEncUnlockBitstream(encoder_, slot.bitstream);
        const bool unmap_ok = unmap_slot(slot);
        slot.in_flight = false;
        if (unlock_status != NV_ENC_SUCCESS) {
            check_nvenc("unlock_bitstream", unlock_status);
            return false;
        }
        return write_ok && unmap_ok;
    }

private:
    bool unmap_slot(DirectEncodeSlot& slot) {
        if (!slot.mapped) {
            return true;
        }
        const NVENCSTATUS status = functions_.nvEncUnmapInputResource(encoder_, slot.mapped);
        slot.mapped = nullptr;
        if (status != NV_ENC_SUCCESS) {
            check_nvenc("unmap_input", status);
            return false;
        }
        return true;
    }

    void close() {
        for (auto& slot : slots_) {
            unmap_slot(slot);
            if (encoder_ && slot.registered) {
                functions_.nvEncUnregisterResource(encoder_, slot.registered);
                slot.registered = nullptr;
            }
            if (encoder_ && slot.bitstream) {
                functions_.nvEncDestroyBitstreamBuffer(encoder_, slot.bitstream);
                slot.bitstream = nullptr;
            }
        }
        slots_.clear();
        if (encoder_) {
            functions_.nvEncDestroyEncoder(encoder_);
            encoder_ = nullptr;
        }
        module_.reset();
    }

    std::unique_ptr<Module> module_;
    NV_ENCODE_API_FUNCTION_LIST functions_{};
    void* encoder_ = nullptr;
    std::vector<DirectEncodeSlot> slots_;
    uint32_t width_ = 0;
    uint32_t height_ = 0;
    DirectRouteCaps caps_{};
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

int run(uint32_t requested_frames, const char* output_path, ChromaSampling chroma,
        uint32_t qp) {
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
    EncodeOptions encode_options{};
    encode_options.chroma = chroma;
    encode_options.qp_intra = qp;
    encode_options.qp_inter_p = qp;
    encode_options.qp_inter_b = qp;
    if (!query_output_refresh(dxgi_output.get(), encode_options.frame_rate_num,
                              encode_options.frame_rate_den)) {
        std::printf("display_refresh_available=false\n");
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
    constexpr size_t surface_count = 3;
    std::vector<ComPtr<IDirect3DSurface9>> surfaces(surface_count);
    std::vector<IDirect3DSurface9*> surface_ptrs;
    std::vector<NVFBC_TODX9VID_OUT_BUF> output_buffers(surface_count);
    surface_ptrs.reserve(surface_count);
    for (size_t index = 0; index < surface_count; ++index) {
        hr = device->CreateRenderTarget(width, height, D3DFMT_A2B10G10R10,
                                        D3DMULTISAMPLE_NONE, 0, FALSE,
                                        surfaces[index].put(), nullptr);
        std::printf("surface_create slot=%llu format=A2B10G10R10 hr=0x%08lx ptr=0x%p\n",
                    static_cast<unsigned long long>(index), static_cast<unsigned long>(hr),
                    surfaces[index].get());
        if (FAILED(hr) || !surfaces[index]) {
            return 1;
        }
        surface_ptrs.push_back(surfaces[index].get());
        output_buffers[index].pPrimary = surfaces[index].get();
    }

    NVFBC_TODX9VID_SETUP_PARAMS setup{};
    setup.dwVersion = NVFBC_TODX9VID_SETUP_PARAMS_V3_VER;
    setup.bHDRRequest = TRUE;
    setup.eMode = NVFBC_TODX9VID_ARGB10;
    setup.dwNumBuffers = static_cast<NvU32>(output_buffers.size());
    setup.ppBuffer = output_buffers.data();
    const NVFBCRESULT setup_result = nvfbc.get()->NvFBCToDx9VidSetUp(&setup);
    std::printf("nvfbc_setup status=%d format=ARGB10 hdr_request=true\n",
                static_cast<int>(setup_result));
    if (setup_result != NVFBC_SUCCESS) {
        return 1;
    }

    NvencDirectD3d9 encoder;
    if (!encoder.open(device.get(), surface_ptrs, width, height, encode_options)) {
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
    std::vector<uint64_t> submit_us;
    std::vector<uint64_t> drain_us;
    std::vector<uint64_t> pipeline_work_us;
    std::vector<uint64_t> vblank_wait_us;
    std::vector<uint64_t> source_interval_us;
    grab_us.reserve(requested_frames);
    submit_us.reserve(requested_frames);
    drain_us.reserve(requested_frames);
    pipeline_work_us.reserve(requested_frames);
    vblank_wait_us.reserve(requested_frames);
    source_interval_us.reserve(requested_frames);
    uint64_t output_bytes = 0;
    uint32_t completed_frames = 0;
    uint32_t mapped_format = 0;
    bool failed = false;
    std::deque<size_t> pending_slots;
    std::optional<std::chrono::steady_clock::time_point> last_grab_return;
    std::optional<std::chrono::steady_clock::time_point> first_grab_return;
    const auto run_start = std::chrono::steady_clock::now();
    for (uint32_t frame = 0; frame < requested_frames; ++frame) {
        const size_t slot_index = frame % encoder.slot_count();
        grab.dwBufferIdx = static_cast<NvU32>(slot_index);
        const auto vblank_start = std::chrono::steady_clock::now();
        hr = dxgi_output->WaitForVBlank();
        const auto vblank_end = std::chrono::steady_clock::now();
        vblank_wait_us.push_back(static_cast<uint64_t>(
            std::chrono::duration_cast<std::chrono::microseconds>(vblank_end - vblank_start)
                .count()));
        if (FAILED(hr)) {
            std::printf("wait_vblank_failed frame=%u hr=0x%08lx\n", frame,
                        static_cast<unsigned long>(hr));
            failed = true;
            break;
        }
        const auto grab_start = std::chrono::steady_clock::now();
        const NVFBCRESULT grab_result = nvfbc.get()->NvFBCToDx9VidGrabFrame(&grab);
        const auto grab_end = std::chrono::steady_clock::now();
        if (last_grab_return) {
            source_interval_us.push_back(static_cast<uint64_t>(
                std::chrono::duration_cast<std::chrono::microseconds>(grab_end - *last_grab_return)
                    .count()));
        }
        if (!first_grab_return) {
            first_grab_return = grab_end;
        }
        last_grab_return = grab_end;
        grab_us.push_back(static_cast<uint64_t>(
            std::chrono::duration_cast<std::chrono::microseconds>(grab_end - grab_start).count()));
        if (grab_result != NVFBC_SUCCESS) {
            std::printf("nvfbc_grab_failed frame=%u status=%d driver=0x%08lx recreate=%d\n",
                        frame, static_cast<int>(grab_result), frame_info.dwDriverInternalError,
                        frame_info.bMustRecreate);
            failed = true;
            break;
        }

        const auto pipeline_start = std::chrono::steady_clock::now();
        const auto submit_start = pipeline_start;
        if (!encoder.submit(slot_index, frame, frame, mapped_format)) {
            std::printf("nvenc_submit_failed frame=%u slot=%llu\n", frame,
                        static_cast<unsigned long long>(slot_index));
            failed = true;
            break;
        }
        const auto submit_end = std::chrono::steady_clock::now();
        submit_us.push_back(static_cast<uint64_t>(
            std::chrono::duration_cast<std::chrono::microseconds>(submit_end - submit_start)
                .count()));
        pending_slots.push_back(slot_index);
        if (pending_slots.size() >= encoder.slot_count()) {
            const size_t drain_slot = pending_slots.front();
            pending_slots.pop_front();
            const auto drain_start = std::chrono::steady_clock::now();
            if (!encoder.drain(drain_slot, output, output_bytes)) {
                std::printf("nvenc_drain_failed frame=%u slot=%llu\n", frame,
                            static_cast<unsigned long long>(drain_slot));
                failed = true;
                break;
            }
            const auto drain_end = std::chrono::steady_clock::now();
            drain_us.push_back(static_cast<uint64_t>(
                std::chrono::duration_cast<std::chrono::microseconds>(drain_end - drain_start)
                    .count()));
            ++completed_frames;
        }
        const auto pipeline_end = std::chrono::steady_clock::now();
        pipeline_work_us.push_back(static_cast<uint64_t>(
            std::chrono::duration_cast<std::chrono::microseconds>(pipeline_end - pipeline_start)
                .count()));
    }
    while (!failed && !pending_slots.empty()) {
        const size_t drain_slot = pending_slots.front();
        pending_slots.pop_front();
        const auto drain_start = std::chrono::steady_clock::now();
        if (!encoder.drain(drain_slot, output, output_bytes)) {
            std::printf("nvenc_final_drain_failed slot=%llu\n",
                        static_cast<unsigned long long>(drain_slot));
            failed = true;
            break;
        }
        const auto drain_end = std::chrono::steady_clock::now();
        drain_us.push_back(static_cast<uint64_t>(
            std::chrono::duration_cast<std::chrono::microseconds>(drain_end - drain_start)
                .count()));
        ++completed_frames;
    }
    output.flush();
    const auto run_end = std::chrono::steady_clock::now();
    const double elapsed_seconds =
        std::chrono::duration<double>(run_end - run_start).count();
    const double fps = elapsed_seconds > 0.0 ? completed_frames / elapsed_seconds : 0.0;
    const uint64_t expected_interval_us =
        (1'000'000ull * encode_options.frame_rate_den) / encode_options.frame_rate_num;
    const uint64_t long_interval_threshold_us = expected_interval_us * 3 / 2;
    const size_t long_intervals = static_cast<size_t>(std::count_if(
        source_interval_us.begin(), source_interval_us.end(),
        [&](uint64_t interval) { return interval > long_interval_threshold_us; }));
    const uint64_t first_frame_latency_us = first_grab_return
                                                ? static_cast<uint64_t>(
                                                      std::chrono::duration_cast<
                                                          std::chrono::microseconds>(
                                                          *first_grab_return - run_start)
                                                          .count())
                                                : 0;
    const uint64_t steady_interval_sum_us =
        std::accumulate(source_interval_us.begin(), source_interval_us.end(), 0ull);
    const double steady_source_fps = steady_interval_sum_us > 0
                                         ? source_interval_us.size() * 1'000'000.0 /
                                               static_cast<double>(steady_interval_sum_us)
                                         : 0.0;
    std::printf(
        "direct_encode_summary chroma=%s requested=%u completed=%u elapsed_ms=%.3f fps=%.3f bytes=%llu "
        "mapped_format=0x%08x first_frame_latency_us=%llu steady_source_fps=%.3f "
        "vblank_wait_p50_us=%llu vblank_wait_p95_us=%llu "
        "source_interval_p50_us=%llu source_interval_p95_us=%llu source_interval_max_us=%llu "
        "source_long_intervals=%llu grab_p50_us=%llu grab_p95_us=%llu submit_p50_us=%llu "
        "submit_p95_us=%llu drain_p50_us=%llu drain_p95_us=%llu pipeline_work_p50_us=%llu "
        "pipeline_work_p95_us=%llu pipeline_work_max_us=%llu explicit_gpu_copies=0 output=%s\n",
        chroma_label(chroma), requested_frames, completed_frames, elapsed_seconds * 1000.0, fps,
        static_cast<unsigned long long>(output_bytes), mapped_format,
        static_cast<unsigned long long>(first_frame_latency_us), steady_source_fps,
        static_cast<unsigned long long>(percentile_us(vblank_wait_us, 0.50)),
        static_cast<unsigned long long>(percentile_us(vblank_wait_us, 0.95)),
        static_cast<unsigned long long>(percentile_us(source_interval_us, 0.50)),
        static_cast<unsigned long long>(percentile_us(source_interval_us, 0.95)),
        static_cast<unsigned long long>(source_interval_us.empty()
                                            ? 0
                                            : *std::max_element(source_interval_us.begin(),
                                                                source_interval_us.end())),
        static_cast<unsigned long long>(long_intervals),
        static_cast<unsigned long long>(percentile_us(grab_us, 0.50)),
        static_cast<unsigned long long>(percentile_us(grab_us, 0.95)),
        static_cast<unsigned long long>(percentile_us(submit_us, 0.50)),
        static_cast<unsigned long long>(percentile_us(submit_us, 0.95)),
        static_cast<unsigned long long>(percentile_us(drain_us, 0.50)),
        static_cast<unsigned long long>(percentile_us(drain_us, 0.95)),
        static_cast<unsigned long long>(percentile_us(pipeline_work_us, 0.50)),
        static_cast<unsigned long long>(percentile_us(pipeline_work_us, 0.95)),
        static_cast<unsigned long long>(
            pipeline_work_us.empty()
                ? 0
                : *std::max_element(pipeline_work_us.begin(), pipeline_work_us.end())),
        output_path);
    return !failed && completed_frames == requested_frames && output_bytes > 0 ? 0 : 1;
}

} // namespace

int main(int argc, char** argv) {
    uint32_t frames = 600;
    const char* output = "nvfbc_nvenc_direct.hevc";
    ChromaSampling chroma = ChromaSampling::Yuv420;
    uint32_t qp = 27;
    if (argc >= 2) {
        frames = static_cast<uint32_t>(std::strtoul(argv[1], nullptr, 10));
    }
    if (argc >= 3) {
        output = argv[2];
    }
    if (argc >= 4 && !parse_chroma(argv[3], chroma)) {
        std::printf("invalid_chroma=%s expected=420|422|444\n", argv[3]);
        return 2;
    }
    if (argc >= 5) {
        qp = static_cast<uint32_t>(std::strtoul(argv[4], nullptr, 10));
    }
    if (frames == 0 || qp > 51) {
        std::printf("invalid_parameters frames=%u qp=%u expected=frames>0,qp=0..51\n", frames,
                    qp);
        return 2;
    }
    std::printf("nvfbc_nvenc_direct frames=%u output=%s chroma=%s qp=%u\n", frames, output,
                chroma_label(chroma), qp);
    return run(frames, output, chroma, qp);
}
