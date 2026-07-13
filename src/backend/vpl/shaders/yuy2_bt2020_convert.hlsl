
Texture2D<float4> src_tex : register(t0);
RWTexture2D<uint> yuy2_tex : register(u0);
static const bool RR_FULL_RANGE = RR_FULL_RANGE_PLACEHOLDER;

float3 rgb_to_bt2020_ycbcr(float3 rgb) {
    const float kr = 0.2627;
    const float kb = 0.0593;
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

float3 load_ycbcr(uint2 pixel) {
    uint width;
    uint height;
    src_tex.GetDimensions(width, height);
    pixel = min(pixel, uint2(width - 1, height - 1));
    return apply_yuv8_range(rgb_to_bt2020_ycbcr(saturate(src_tex.Load(int3(pixel, 0)).rgb)));
}

uint u8(float value) {
    return (uint)round(saturate(value) * 255.0);
}

[numthreads(16, 8, 1)]
void cs_main(uint3 tid : SV_DispatchThreadID) {
    uint width;
    uint height;
    src_tex.GetDimensions(width, height);
    uint2 base_pixel = uint2(tid.x * 2, tid.y);
    if (base_pixel.x >= width || base_pixel.y >= height) {
        return;
    }
    float3 c0 = load_ycbcr(base_pixel);
    float3 c1 = load_ycbcr(uint2(min(base_pixel.x + 1, width - 1), base_pixel.y));
    float2 uv = (c0.yz + c1.yz) * 0.5;
    uint y0 = u8(c0.x);
    uint u = u8(uv.x);
    uint y1 = u8(c1.x);
    uint v = u8(uv.y);
    yuy2_tex[tid.xy] = y0 | (u << 8) | (y1 << 16) | (v << 24);
}
