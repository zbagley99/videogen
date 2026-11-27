// Full-screen pass: premultiplied canvas -> straight alpha, for encoders.
@group(0) @binding(0) var canvas_tex: texture_2d<f32>;

@vertex
fn vs_full(@builtin(vertex_index) i: u32) -> @builtin(position) vec4<f32> {
    let xy = vec2<f32>(f32((i << 1u) & 2u), f32(i & 2u));
    return vec4<f32>(xy * 2.0 - 1.0, 0.0, 1.0);
}

@fragment
fn fs_unpremultiply(@builtin(position) pos: vec4<f32>) -> @location(0) vec4<f32> {
    let c = textureLoad(canvas_tex, vec2<i32>(pos.xy), 0);
    if (c.a <= 0.0) {
        return vec4<f32>(0.0);
    }
    return vec4<f32>(c.rgb / c.a, c.a);
}
