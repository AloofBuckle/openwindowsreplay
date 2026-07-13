
Texture2D<float4> src_tex : register(t0);
RWTexture2D<uint> y410_tex : register(u0);
static const bool RR_FULL_RANGE = RR_FULL_RANGE_PLACEHOLDER;

float3 rgb_to_bt709_ycbcr(float3 rgb) {
    const float kr = 0.2126;
    const float kb = 0.0722;
    const float kg = 1.0 - kr - kb;
    float y = kr * rgb.r + kg * rgb.g + kb * rgb.b;
    float cb = (rgb.b - y) / (2.0 * (1.0 - kb));
    float cr = (rgb.r - y) / (2.0 * (1.0 - kr));
    return float3(saturate(y), saturate(cb + 0.5), saturate(cr + 0.5));
}

float bt709_oetf(float linear_value) {
    float v = max(linear_value, 0.0);
    return (v < 0.018) ? (4.5 * v) : (1.099 * pow(v, 0.45) - 0.099);
}

float3 sc_rgb_to_bt709_signal(float3 sc_rgb) {
    return saturate(float3(
        bt709_oetf(sc_rgb.r),
        bt709_oetf(sc_rgb.g),
        bt709_oetf(sc_rgb.b)
    ));
}

float3 apply_yuv10_range(float3 ycbcr) {
    ycbcr = saturate(ycbcr);
    if (RR_FULL_RANGE) {
        return ycbcr;
    }
    float cb = ycbcr.y - 0.5;
    float cr = ycbcr.z - 0.5;
    return float3(
        (64.0 / 1023.0) + ycbcr.x * (876.0 / 1023.0),
        (512.0 / 1023.0) + cb * (896.0 / 1023.0),
        (512.0 / 1023.0) + cr * (896.0 / 1023.0)
    );
}

uint u10(float value) {
    return (uint)round(saturate(value) * 1023.0);
}

[numthreads(16, 8, 1)]
void cs_main(uint3 tid : SV_DispatchThreadID) {
    uint width;
    uint height;
    src_tex.GetDimensions(width, height);
    if (tid.x >= width || tid.y >= height) {
        return;
    }
    float3 ycbcr = apply_yuv10_range(rgb_to_bt709_ycbcr(sc_rgb_to_bt709_signal(src_tex.Load(int3(tid.xy, 0)).rgb)));
    uint y = u10(ycbcr.x);
    uint u = u10(ycbcr.y);
    uint v = u10(ycbcr.z);
    y410_tex[tid.xy] = u | (y << 10) | (v << 20) | (3u << 30);
}
