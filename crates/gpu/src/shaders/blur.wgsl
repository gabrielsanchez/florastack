// One pass of florastack-core's Gaussian blur (`ops::blur`), along the rows (axis 0) or the columns (axis 1), with
// clamp-to-edge borders:
//
// - mode 0, the exact kernel: output sample x sums `weights[k] * src[clamp(x + k - radius)]` over k in order, from 0,
//   as `convolve_row` and `columns_exact` do;
// - mode 1, one extended box: `(sum of src[clamp(x + k)] for |k| <= radius + alpha * (src[clamp(x - radius - 1)] +
//   src[clamp(x + radius + 1)])) * norm`, which the CPU computes with running sums in f64 and the GPU directly in f32.

struct Params {
    width: u32,
    height: u32,
    channels: u32,
    radius: u32,
    taps: u32,
    axis: u32,
    mode: u32,
    alpha: f32,
    norm: f32,
    pad0: u32,
    pad1: u32,
    pad2: u32,
}

@group(0) @binding(0) var<uniform> p: Params;
@group(0) @binding(1) var<storage, read> src: array<f32>;
@group(0) @binding(2) var<storage, read_write> dst: array<f32>;
@group(0) @binding(3) var<storage, read> weights: array<f32>;

// Sample c of the pixel `offset` along the pass's axis from (x, y), clamped to the plane.
fn tap(x: i32, y: i32, offset: i32, c: u32) -> f32 {
    var tx = x;
    var ty = y;
    if (p.axis == 0u) {
        tx = clamp(x + offset, 0, i32(p.width) - 1);
    } else {
        ty = clamp(y + offset, 0, i32(p.height) - 1);
    }
    return src[(u32(ty) * p.width + u32(tx)) * p.channels + c];
}

@compute @workgroup_size(16, 16, 1)
fn main(@builtin(global_invocation_id) id: vec3<u32>) {
    if (id.x >= p.width || id.y >= p.height) {
        return;
    }
    let x = i32(id.x);
    let y = i32(id.y);
    let r = i32(p.radius);
    let at = (id.y * p.width + id.x) * p.channels;
    for (var c = 0u; c < p.channels; c++) {
        var s = 0.0;
        if (p.mode == 0u) {
            for (var k = 0u; k < p.taps; k++) {
                s = s + weights[k] * tap(x, y, i32(k) - r, c);
            }
        } else {
            for (var k = -r; k <= r; k++) {
                s = s + tap(x, y, k, c);
            }
            s = (s + p.alpha * (tap(x, y, -r - 1, c) + tap(x, y, r + 1, c))) * p.norm;
        }
        dst[at + c] = s;
    }
}
