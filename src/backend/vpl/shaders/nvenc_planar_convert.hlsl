Texture2D<float4> src_tex : register(t0);
RWTexture2D<uint> dst_tex : register(u0);

static const bool RR_FULL_RANGE = RR_FULL_RANGE_PLACEHOLDER;
static const bool RR_BIT_DEPTH_10 = RR_BIT_DEPTH_10_PLACEHOLDER;
static const bool RR_CHROMA_422 = RR_CHROMA_422_PLACEHOLDER;
// 0: BT.709 signal, 1: BT.2020 signal, 2: scRGB -> BT.709,
// 3: scRGB -> BT.2020, 4: scRGB -> BT.2020 PQ.
static const uint RR_COLOR_MODE = RR_COLOR_MODE_PLACEHOLDER;

float pq_oetf(float normalized_luminance) {
    const float m1 = 2610.0 / 16384.0;
    const float m2 = 2523.0 / 32.0;
    const float c1 = 3424.0 / 4096.0;
    const float c2 = 2413.0 / 128.0;
    const float c3 = 2392.0 / 128.0;
    float n = saturate(normalized_luminance);
    float p = pow(n, m1);
    return pow((c1 + c2 * p) / (1.0 + c3 * p), m2);
}

float signal_oetf(float linear_value) {
    float value = max(linear_value, 0.0);
    return (value < 0.018) ? (4.5 * value) : (1.099 * pow(value, 0.45) - 0.099);
}

float3 rec709_linear_to_bt2020_linear(float3 rgb709) {
    return float3(
        0.6274039 * rgb709.r + 0.3292830 * rgb709.g + 0.0433131 * rgb709.b,
        0.0690973 * rgb709.r + 0.9195404 * rgb709.g + 0.0113623 * rgb709.b,
        0.0163914 * rgb709.r + 0.0880133 * rgb709.g + 0.8955953 * rgb709.b
    );
}

float3 source_to_signal(float3 source) {
    if (RR_COLOR_MODE <= 1) {
        return saturate(source);
    }
    if (RR_COLOR_MODE == 2) {
        float3 linear_rgb = max(source, 0.0);
        return saturate(float3(
            signal_oetf(linear_rgb.r),
            signal_oetf(linear_rgb.g),
            signal_oetf(linear_rgb.b)
        ));
    }
    float3 bt2020_linear = max(rec709_linear_to_bt2020_linear(source), 0.0);
    if (RR_COLOR_MODE == 3) {
        return saturate(float3(
            signal_oetf(bt2020_linear.r),
            signal_oetf(bt2020_linear.g),
            signal_oetf(bt2020_linear.b)
        ));
    }
    float3 normalized_nits = bt2020_linear * (80.0 / 10000.0);
    return float3(
        pq_oetf(normalized_nits.r),
        pq_oetf(normalized_nits.g),
        pq_oetf(normalized_nits.b)
    );
}

float3 signal_to_ycbcr(float3 rgb) {
    float kr = (RR_COLOR_MODE == 1 || RR_COLOR_MODE >= 3) ? 0.2627 : 0.2126;
    float kb = (RR_COLOR_MODE == 1 || RR_COLOR_MODE >= 3) ? 0.0593 : 0.0722;
    float kg = 1.0 - kr - kb;
    float y = kr * rgb.r + kg * rgb.g + kb * rgb.b;
    float cb = (rgb.b - y) / (2.0 * (1.0 - kb));
    float cr = (rgb.r - y) / (2.0 * (1.0 - kr));
    return float3(saturate(y), saturate(cb + 0.5), saturate(cr + 0.5));
}

float3 apply_range(float3 ycbcr) {
    ycbcr = saturate(ycbcr);
    if (RR_FULL_RANGE) {
        return ycbcr;
    }
    float cb = ycbcr.y - 0.5;
    float cr = ycbcr.z - 0.5;
    if (RR_BIT_DEPTH_10) {
        return float3(
            (64.0 / 1023.0) + ycbcr.x * (876.0 / 1023.0),
            (512.0 / 1023.0) + cb * (896.0 / 1023.0),
            (512.0 / 1023.0) + cr * (896.0 / 1023.0)
        );
    }
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
    return apply_range(signal_to_ycbcr(source_to_signal(src_tex.Load(int3(pixel, 0)).rgb)));
}

uint quantize(float value) {
    if (RR_BIT_DEPTH_10) {
        return ((uint)round(saturate(value) * 1023.0)) << 6;
    }
    return (uint)round(saturate(value) * 255.0);
}

[numthreads(16, 8, 1)]
void cs_main(uint3 tid : SV_DispatchThreadID) {
    uint width;
    uint height;
    src_tex.GetDimensions(width, height);
    if (RR_CHROMA_422) {
        uint2 p0 = uint2(tid.x * 2, tid.y);
        if (p0.x + 1 >= width || p0.y >= height) {
            return;
        }
        uint2 p1 = uint2(p0.x + 1, p0.y);
        float3 c0 = load_ycbcr(p0);
        float3 c1 = load_ycbcr(p1);
        float2 uv = (c0.yz + c1.yz) * 0.5;
        dst_tex[p0] = quantize(c0.x);
        dst_tex[p1] = quantize(c1.x);
        dst_tex[uint2(p0.x, p0.y + height)] = quantize(uv.x);
        dst_tex[uint2(p1.x, p1.y + height)] = quantize(uv.y);
        return;
    }

    uint2 pixel = tid.xy;
    if (pixel.x >= width || pixel.y >= height) {
        return;
    }
    float3 ycbcr = load_ycbcr(pixel);
    dst_tex[pixel] = quantize(ycbcr.x);
    dst_tex[uint2(pixel.x, pixel.y + height)] = quantize(ycbcr.y);
    dst_tex[uint2(pixel.x, pixel.y + height * 2)] = quantize(ycbcr.z);
}
