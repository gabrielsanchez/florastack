// The viewer's mipmaps (viewer.rs): each pixel of the next level samples the level above at its center, between four
// of its pixels, which bilinear filtering averages, in linear light.

struct Varying {
    @builtin(position) position: vec4<f32>,
    @location(0) uv: vec2<f32>,
}

@group(0) @binding(0) var source: texture_2d<f32>;
@group(0) @binding(1) var bilinear: sampler;

@vertex
fn vs_level(@builtin(vertex_index) index: u32) -> Varying {
    var corners = array<vec2<f32>, 3>(vec2<f32>(-1.0, -1.0), vec2<f32>(3.0, -1.0), vec2<f32>(-1.0, 3.0));
    let p = corners[index];
    var out: Varying;
    out.position = vec4<f32>(p, 0.0, 1.0);
    out.uv = vec2<f32>(0.5 * p.x + 0.5, 0.5 - 0.5 * p.y);
    return out;
}

@fragment
fn fs_level(in: Varying) -> @location(0) vec4<f32> {
    return textureSampleLevel(source, bilinear, in.uv, 0.0);
}
