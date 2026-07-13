
Texture2D<float4> src_tex : register(t0);
RWTexture2D<float> y_plane : register(u0);
RWTexture2D<float2> uv_plane : register(u1);
static const bool RR_FULL_RANGE = RR_FULL_RANGE_PLACEHOLDER;

float3 rgb_to_bt709_full_ycbcr(float3 rgb) {
    const float kr = 0.2126;
    const float kb = 0.0722;
    const float kg = 1.0 - kr - kb;
    float y = kr * rgb.r + kg * rgb.g + kb * rgb.b;
    float cb = (rgb.b - y) / (2.0 * (1.0 - kb));
    float cr = (rgb.r - y) / (2.0 * (1.0 - kr));
    return float3(
        saturate(y),
        saturate(cb + 0.5),
        saturate(cr + 0.5)
    );
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
    return apply_yuv8_range(rgb_to_bt709_full_ycbcr(saturate(src_tex.Load(int3(pixel, 0)).rgb)));
}

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
