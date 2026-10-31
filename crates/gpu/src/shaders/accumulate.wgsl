// A frame folded into the weighted average's accumulators over a tile's inner region: each pixel's weight is its
// smoothed sharpness times its coverage (0 where uncovered), and where it is positive the weighted samples (clamped
// to MAX_SAMPLE) and the weight are added (florastack-stack's `WeightedAverage` fold). The weights and coverage cover
// the patch's region, the sums the inner region, which starts at (offset_x, offset_y) of the region.

struct Params {
    width: u32,
    height: u32,
    channels: u32,
    region_width: u32,
    offset_x: u32,
    offset_y: u32,
    pad0: u32,
    pad1: u32,
}

@group(0) @binding(0) var<uniform> p: Params;
@group(0) @binding(1) var<storage, read> image: array<f32>;
@group(0) @binding(2) var<storage, read> coverage: array<f32>;
@group(0) @binding(3) var<storage, read> weights: array<f32>;
@group(0) @binding(4) var<storage, read_write> sum: array<f32>;
@group(0) @binding(5) var<storage, read_write> total: array<f32>;

@compute @workgroup_size(16, 16, 1)
fn main(@builtin(global_invocation_id) id: vec3<u32>) {
    if (id.x >= p.width || id.y >= p.height) {
        return;
    }
    let src = (p.offset_y + id.y) * p.region_width + p.offset_x + id.x;
    let k = coverage[src];
    var w = 0.0;
    if (k > 0.0) {
        w = weights[src] * k;
    }
    if (!(w > 0.0)) {
        return;
    }
    let dst = id.y * p.width + id.x;
    for (var c = 0u; c < p.channels; c++) {
        let v = clamp(image[src * p.channels + c], -MAX_SAMPLE, MAX_SAMPLE);
        sum[dst * p.channels + c] = sum[dst * p.channels + c] + w * v;
    }
    total[dst] = total[dst] + w;
}
