
Texture2D<float4> src_tex : register(t0);
static const bool RR_FULL_RANGE = RR_FULL_RANGE_PLACEHOLDER;

float4 vs_main(uint id : SV_VertexID) : SV_Position {
    float2 pos[3] = {
        float2(-1.0,  1.0),
        float2( 3.0,  1.0),
        float2(-1.0, -3.0)
    };
    return float4(pos[id], 0.0, 1.0);
}

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
    float3 bt2020_linear = max(rec709_linear_to_bt2020_linear(sc_rgb), 0.0);
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

float4 ps_luma(float4 pos : SV_Position) : SV_Target {
    return load_ycbcr(uint2(pos.xy)).xxxx;
}

float4 ps_chroma(float4 pos : SV_Position) : SV_Target {
    uint2 base_pixel = uint2(pos.xy) * 2;
    float2 chroma = (
        load_ycbcr(base_pixel).yz
        + load_ycbcr(base_pixel + uint2(1, 0)).yz
        + load_ycbcr(base_pixel + uint2(0, 1)).yz
        + load_ycbcr(base_pixel + uint2(1, 1)).yz
    ) * 0.25;
    return float4(chroma, 0.0, 1.0);
}

RWTexture2D<float> y_plane : register(u0);
RWTexture2D<float2> uv_plane : register(u1);

[numthreads(8, 8, 1)]
void cs_main(uint3 tid : SV_DispatchThreadID) {
    uint width;
    uint height;
    src_tex.GetDimensions(width, height);
    uint2 base_pixel = tid.xy * 2;
    if (base_pixel.x >= width || base_pixel.y >= height) {
        return;
    }

    float2 chroma_sum = float2(0.0, 0.0);
    float chroma_count = 0.0;
    [unroll]
    for (uint dy = 0; dy < 2; dy++) {
        [unroll]
        for (uint dx = 0; dx < 2; dx++) {
            uint2 pixel = base_pixel + uint2(dx, dy);
            if (pixel.x < width && pixel.y < height) {
                float3 ycbcr = load_ycbcr(pixel);
                y_plane[pixel] = ycbcr.x;
                chroma_sum += ycbcr.yz;
                chroma_count += 1.0;
            }
        }
    }
    uv_plane[tid.xy] = chroma_sum / max(chroma_count, 1.0);
}
