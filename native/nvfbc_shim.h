#pragma once

#include <cstdint>

struct RrNvFbcHandle;

struct RrNvFbcCreateInfo {
    std::uint32_t width;
    std::uint32_t height;
    std::uint32_t nvfbc_version;
};

struct RrNvFbcGrabInfo {
    std::uint32_t width;
    std::uint32_t height;
    std::uint32_t buffer_width;
    std::uint32_t must_recreate;
    std::uint32_t protected_content;
    std::uint32_t driver_internal_error;
    std::uint32_t source_pid;
    std::uint32_t is_hdr;
    std::uint32_t wait_mode_used;
};

extern "C" {

std::int32_t rr_nvfbc_create(void* d3d9_device, std::uint32_t adapter,
                             RrNvFbcHandle** out_handle, RrNvFbcCreateInfo* out_info) noexcept;

std::int32_t rr_nvfbc_setup(RrNvFbcHandle* handle, void* const* d3d9_surfaces,
                            std::uint32_t surface_count, std::uint32_t hdr,
                            std::uint32_t cursor) noexcept;

std::int32_t rr_nvfbc_grab(RrNvFbcHandle* handle, std::uint32_t surface_index,
                           RrNvFbcGrabInfo* out_info) noexcept;

std::int32_t rr_nvfbc_gpu_sleep(RrNvFbcHandle* handle, std::int64_t microseconds) noexcept;

std::int32_t rr_nvfbc_destroy(RrNvFbcHandle* handle) noexcept;

}
