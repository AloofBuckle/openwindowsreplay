
Texture2D<float4> src_tex : register(t0);
RWTexture2D<uint4> y210_tex : register(u0);
static const bool RR_FULL_RANGE = RR_FULL_RANGE_PLACEHOLDER;

float3 rec709_linear_to_bt2020_linear(float3 rgb709) {
    return float3(
        0.6274039 * rgb709.r + 0.3292830 * rgb709.g + 0.0433131 * rgb709.b,
        0.0690973 * rgb709.r + 0.9195404 * rgb709.g + 0.0113623 * rgb709.b,
        0.0163914 * rgb709.r + 0.0880133 * rgb709.g + 0.8955953 * rgb709.b
    );
}

float bt2020_oetf(float linear_value) {
    float v = max(linear_value, 0.0);
    return (v < 0.018) ? (4.5 * v) : (1.099 * pow(v, 0.45) - 0.099);
}

float3 sc_rgb_to_bt2020_signal(float3 sc_rgb) {
    float3 bt2020_linear = max(rec709_linear_to_bt2020_linear(max(sc_rgb, 0.0)), 0.0);
    return saturate(float3(
        bt2020_oetf(bt2020_linear.r),
        bt2020_oetf(bt2020_linear.g),
        bt2020_oetf(bt2020_linear.b)
    ));
}

float3 rgb_to_bt2020_ycbcr(float3 rgb) {
    const float kr = 0.2627;
    const float kb = 0.0593;
    const float kg = 1.0 - kr - kb;
    float y = kr * rgb.r + kg * rgb.g + kb * rgb.b;
    float cb = (rgb.b - y) / (2.0 * (1.0 - kb));
    float cr = (rgb.r - y) / (2.0 * (1.0 - kr));
    return float3(saturate(y), saturate(cb + 0.5), saturate(cr + 0.5));
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

float3 load_ycbcr(uint2 pixel) {
    uint width;
    uint height;
    src_tex.GetDimensions(width, height);
    pixel = min(pixel, uint2(width - 1, height - 1));
    return apply_yuv10_range(rgb_to_bt2020_ycbcr(sc_rgb_to_bt2020_signal(src_tex.Load(int3(pixel, 0)).rgb)));
}

uint u10_to_msb16(float value) {
    return ((uint)round(saturate(value) * 1023.0)) << 6;
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
    y210_tex[tid.xy] = uint4(u10_to_msb16(c0.x), u10_to_msb16(uv.x), u10_to_msb16(c1.x), u10_to_msb16(uv.y));
}
