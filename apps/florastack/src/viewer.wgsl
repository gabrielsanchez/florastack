// The viewer's shader (viewer.rs): the image drawn as a textured quad (its mipmaps: levels.wgsl).
//
// The image is linear light in sRGB's primaries (florastack-engine's `view`), in an Rgba16Float texture whose mip
// levels hold 2 x 2 averages in linear light, so that a zoomed-out image is filtered properly (trilinearly) rather
// than aliased. The fragment shader clips to [0, 1], composites transparent pixels over a checkerboard in linear
// light, and applies sRGB's curve (the display transform) unless the target applies it.

struct View {
    // Clip space = offset + scale * image pixel.
    scale: vec2<f32>,
    offset: vec2<f32>,
    // The image's size, in pixels.
    size: vec2<f32>,
    // The viewport's top-left in framebuffer pixels, for the checkerboard's phase.
    origin: vec2<f32>,
    // The checkerboard's square, in framebuffer pixels.
    checker: f32,
    // 1 when the target isn't sRGB: the curve is applied here.
    encode: f32,
    // 1 to show each image pixel as a square (magnified), 0 to filter (minified).
    nearest: f32,
    // How many times brighter the image is shown: the retouching view's boost for dark regions, 1 otherwise.
    boost: f32,
}

@group(0) @binding(0) var<uniform> view: View;
@group(0) @binding(1) var image: texture_2d<f32>;
@group(0) @binding(2) var trilinear: sampler;
@group(0) @binding(3) var pixels: sampler;

struct Varying {
    @builtin(position) position: vec4<f32>,
    @location(0) uv: vec2<f32>,
}

@vertex
fn vs_image(@builtin(vertex_index) index: u32) -> Varying {
    var corners = array<vec2<f32>, 6>(
        vec2<f32>(0.0, 0.0), vec2<f32>(1.0, 0.0), vec2<f32>(0.0, 1.0),
        vec2<f32>(0.0, 1.0), vec2<f32>(1.0, 0.0), vec2<f32>(1.0, 1.0),
    );
    let uv = corners[index];
    var out: Varying;
    out.position = vec4<f32>(view.offset + view.scale * (uv * view.size), 0.0, 1.0);
    out.uv = uv;
    return out;
}

fn srgb(v: vec3<f32>) -> vec3<f32> {
    let low = v * 12.92;
    let high = 1.055 * pow(v, vec3<f32>(1.0 / 2.4)) - 0.055;
    return select(high, low, v <= vec3<f32>(0.0031308));
}

@fragment
fn fs_image(in: Varying) -> @location(0) vec4<f32> {
    let filtered = textureSample(image, trilinear, in.uv);
    let square = textureSample(image, pixels, in.uv);
    let c = select(filtered, square, view.nearest > 0.5);
    let cell = floor((in.position.xy - view.origin) / view.checker);
    let light = (i32(cell.x) + i32(cell.y)) % 2 == 0;
    let back = select(vec3<f32>(0.03), vec3<f32>(0.1), light);
    let rgb = mix(back, clamp(c.rgb * view.boost, vec3<f32>(0.0), vec3<f32>(1.0)), clamp(c.a, 0.0, 1.0));
    let shown = select(rgb, srgb(rgb), view.encode > 0.5);
    return vec4<f32>(shown, 1.0);
}
