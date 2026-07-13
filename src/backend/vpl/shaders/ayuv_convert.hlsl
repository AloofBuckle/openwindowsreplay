
Texture2D<float4> src_tex : register(t0);
RWTexture2D<uint> ayuv_tex : register(u0);
static const bool RR_FULL_RANGE = RR_FULL_RANGE_PLACEHOLDER;

float3 rgb_to_bt709_full_ycbcr(float3 rgb) {
    const float kr = 0.2126;
    const float kb = 0.0722;
    const float kg = 1.0 - kr - kb;
    float y = kr * rgb.r + kg * rgb.g + kb * rgb.b;
    float cb = (rgb.b - y) / (2.0 * (1.0 - kb));
    float cr = (rgb.r - y) / (2.0 * (1.0 - kr));
    return float3(saturate(y), saturate(cb + 0.5), saturate(cr + 0.5));
}

float3 apply_yuv8_range(float3 ycbcr) {
    ycbcr = saturate(ycbcr);
    if (RR_FULL_RANGE) {
        return ycbcr;
    }
    float cb = ycbcr.y - 0.5;
    float cr = ycbcr.z - 0.5;
    return float3(
        (16.0 / 255.0) + ycbcr.x * (219.0 / 255.0),
        (128.0 / 255.0) + cb * (224.0 / 255.0),
        (128.0 / 255.0) + cr * (224.0 / 255.0)
    );
}

uint u8(float value) {
    return (uint)round(saturate(value) * 255.0);
}

[numthreads(16, 8, 1)]
void cs_main(uint3 tid : SV_DispatchThreadID) {
    uint width;
    uint height;
    src_tex.GetDimensions(width, height);
    if (tid.x >= width || tid.y >= height) {
        return;
    }
    float3 ycbcr = apply_yuv8_range(rgb_to_bt709_full_ycbcr(saturate(src_tex.Load(int3(tid.xy, 0)).rgb)));
    uint y = u8(ycbcr.x);
    uint u = u8(ycbcr.y);
    uint v = u8(ycbcr.z);
    // DXGI AYUV 的 R32_UINT UAV 视图按 V/U/Y/A 字节直写。
    ayuv_tex[tid.xy] = v | (u << 8) | (y << 16) | (255u << 24);
}
