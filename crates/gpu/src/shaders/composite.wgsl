// Frame `frame` folded into the depth map's composite over a tile's inner region (florastack-stack's
// `CompositeKernel` fold): it weighs `max(0, 1 - |frame - depth|)` times its coverage, and is kept as the nearest
// covering frame where it lies nearer the depth than any before (for wide framing, where neither frame at the depth
// covers). Pixels that the frame doesn't cover, or with a sample that isn't finite, are skipped. Samples are clamped
// to MAX_SAMPLE. The distance starts at BIG.

struct Params {
    width: u32,
    height: u32,
    channels: u32,
    region_width: u32,
    offset_x: u32,
    offset_y: u32,
    has_coverage: u32,
    frame: f32,
}

@group(0) @binding(0) var<uniform> p: Params;
@group(0) @binding(1) var<storage, read> image: array<f32>;
@group(0) @binding(2) var<storage, read> given: array<f32>;
@group(0) @binding(3) var<storage, read> depth: array<f32>;
@group(0) @binding(4) var<storage, read_write> sum: array<f32>;
@group(0) @binding(5) var<storage, read_write> total: array<f32>;
@group(0) @binding(6) var<storage, read_write> nearest: array<f32>;
@group(0) @binding(7) var<storage, read_write> distance: array<f32>;

@compute @workgroup_size(16, 16, 1)
fn main(@builtin(global_invocation_id) id: vec3<u32>) {
    if (id.x >= p.width || id.y >= p.height) {
        return;
    }
    let src = (p.offset_y + id.y) * p.region_width + p.offset_x + id.x;
    let s = id.y * p.width + id.x;
    let c = p.channels;
    var k = 1.0;
    if (p.has_coverage != 0u) {
        k = given[src];
    }
    var ok = finite(k) && k >= MIN_COVERAGE;
    for (var ch = 0u; ch < c; ch++) {
        ok = ok && finite(image[src * c + ch]);
    }
    if (!ok) {
        return;
    }
    let d = abs(p.frame - depth[s]);
    let weight = max(1.0 - d, 0.0) * min(k, 1.0);
    if (weight > 0.0) {
        for (var ch = 0u; ch < c; ch++) {
            sum[s * c + ch] = sum[s * c + ch] + weight * clamp(image[src * c + ch], -MAX_SAMPLE, MAX_SAMPLE);
        }
        total[s] = total[s] + weight;
    }
    if (d < distance[s]) {
        for (var ch = 0u; ch < c; ch++) {
            nearest[s * c + ch] = clamp(image[src * c + ch], -MAX_SAMPLE, MAX_SAMPLE);
        }
        distance[s] = d;
    }
}
