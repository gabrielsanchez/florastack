// A frame warped onto a region of the canvas with its coverage: florastack-align's `warp_to_canvas` (bicubic, Keys'
// a = -0.5, taps clamped to the frame) and `Coverage::weight` (the inset of the sampled point in the frame, ramping
// from 0 at the frame's edge to 1 at `feather` frame pixels inside it), then the frame's brightness gain
// (florastack-stack's `Gained`). Uncovered pixels are 0.
//
// Output pixel (i, j) of the region samples the frame at (u, v) = M (i, j, 1), in frame pixels relative to the
// uploaded block of the frame, which starts at (src_x0, src_y0) and holds every tap a pixel of the region reads. The
// host folds the region's origin and the block's into M's translation in f64, so that coordinates stay small in f32.
// `inset_*` are the frame's edges in the same coordinates: u + inset_left is the distance to the left edge.

struct Params {
    width: u32,
    height: u32,
    channels: u32,
    src_width: u32,
    src_height: u32,
    src_x0: i32,
    src_y0: i32,
    frame_width: i32,
    frame_height: i32,
    pad0: u32,
    pad1: u32,
    pad2: u32,
    m00: f32,
    m01: f32,
    m02: f32,
    m10: f32,
    m11: f32,
    m12: f32,
    inset_left: f32,
    inset_right: f32,
    inset_top: f32,
    inset_bottom: f32,
    feather: f32,
    gain: f32,
}

@group(0) @binding(0) var<uniform> p: Params;
@group(0) @binding(1) var<storage, read> src: array<f32>;
@group(0) @binding(2) var<storage, read_write> dst: array<f32>;
@group(0) @binding(3) var<storage, read_write> coverage: array<f32>;

// Keys' cubic convolution weights of the four samples around a point a fraction t past the second.
fn keys(t: f32) -> vec4<f32> {
    let a = -0.5;
    let d0 = 1.0 + t;
    let d3 = 2.0 - t;
    let far0 = ((a * d0 - 5.0 * a) * d0 + 8.0 * a) * d0 - 4.0 * a;
    let near1 = ((a + 2.0) * t - (a + 3.0)) * t * t + 1.0;
    let s = 1.0 - t;
    let near2 = ((a + 2.0) * s - (a + 3.0)) * s * s + 1.0;
    let far3 = ((a * d3 - 5.0 * a) * d3 + 8.0 * a) * d3 - 4.0 * a;
    return vec4<f32>(far0, near1, near2, far3);
}

// The block's index of tap `offset` around frame coordinate floor `f` (relative to the block), clamped to the frame.
fn tap(f: i32, offset: i32, origin: i32, frame: i32, len: u32) -> u32 {
    let global = clamp(f + offset + origin, 0, frame - 1);
    return u32(clamp(global - origin, 0, i32(len) - 1));
}

@compute @workgroup_size(16, 16, 1)
fn main(@builtin(global_invocation_id) id: vec3<u32>) {
    if (id.x >= p.width || id.y >= p.height) {
        return;
    }
    let fi = f32(id.x);
    let fj = f32(id.y);
    let u = p.m00 * fi + p.m01 * fj + p.m02;
    let v = p.m10 * fi + p.m11 * fj + p.m12;
    let d = min(min(u + p.inset_left, p.inset_right - u), min(v + p.inset_top, p.inset_bottom - v));
    var w = 0.0;
    if (d > 0.0) {
        if (p.feather <= 0.0 || d >= p.feather) {
            w = 1.0;
        } else {
            w = d / p.feather;
        }
    }
    let pixel = id.y * p.width + id.x;
    coverage[pixel] = w;
    let at = pixel * p.channels;
    if (w <= 0.0) {
        for (var c = 0u; c < p.channels; c++) {
            dst[at + c] = 0.0;
        }
        return;
    }
    let fu = floor(u);
    let fv = floor(v);
    let wx = keys(u - fu);
    let wy = keys(v - fv);
    let iu = i32(fu);
    let iv = i32(fv);
    var xs: array<u32, 4>;
    var ys: array<u32, 4>;
    for (var k = 0; k < 4; k++) {
        xs[k] = tap(iu, k - 1, p.src_x0, p.frame_width, p.src_width);
        ys[k] = tap(iv, k - 1, p.src_y0, p.frame_height, p.src_height);
    }
    for (var c = 0u; c < p.channels; c++) {
        var acc = 0.0;
        for (var r = 0; r < 4; r++) {
            let row = ys[r] * p.src_width;
            var across = 0.0;
            for (var k = 0; k < 4; k++) {
                across = across + wx[k] * src[(row + xs[k]) * p.channels + c];
            }
            acc = acc + wy[r] * across;
        }
        dst[at + c] = acc * p.gain;
    }
}
