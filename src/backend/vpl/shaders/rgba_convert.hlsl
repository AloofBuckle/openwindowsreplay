
Texture2D<float4> src_tex : register(t0);

float4 vs_main(uint id : SV_VertexID) : SV_Position {
    float2 pos[3] = {
        float2(-1.0,  1.0),
        float2( 3.0,  1.0),
        float2(-1.0, -3.0)
    };
    return float4(pos[id], 0.0, 1.0);
}

float4 ps_main(float4 pos : SV_Position) : SV_Target {
    return saturate(src_tex.Load(int3(uint2(pos.xy), 0)));
}
