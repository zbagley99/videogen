// Draws one pose image as a quad given its four canvas-pixel corners.
// Textures hold premultiplied alpha; the canvas accumulates premultiplied colour.

struct Sprite {
    // tl, tr, bl, br corners in canvas pixels (xy), padded to vec4 each.
    corners: array<vec4<f32>, 4>,
    canvas: vec4<f32>, // width, height, unused, unused
};

@group(0) @binding(0) var<uniform> sprite: Sprite;
@group(0) @binding(1) var tex: texture_2d<f32>;
@group(0) @binding(2) var samp: sampler;

struct VsOut {
    @builtin(position) pos: vec4<f32>,
    @location(0) uv: vec2<f32>,
};

@vertex
fn vs_sprite(@builtin(vertex_index) i: u32) -> VsOut {
    let uv = vec2<f32>(f32(i & 1u), f32(i >> 1u));
    let p = sprite.corners[i].xy;
    var out: VsOut;
    out.pos = vec4<f32>(p.x / sprite.canvas.x * 2.0 - 1.0, 1.0 - p.y / sprite.canvas.y * 2.0, 0.0, 1.0);
    out.uv = uv;
    return out;
}

@fragment
fn fs_sprite(in: VsOut) -> @location(0) vec4<f32> {
    return textureSample(tex, samp, in.uv);
}
