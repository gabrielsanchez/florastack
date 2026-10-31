// The pyramid's residual (florastack-stack's coarse fold): each frame's coarsest level added to a sum weighted as
// choices are, by the fourth power of its coverage, and that weight to the sum of weights.

struct Params {
    count: u32,
    channels: u32,
    pad0: u32,
    pad1: u32,
}

@group(0) @binding(0) var<uniform> p: Params;
@group(0) @binding(1) var<storage, read> values: array<f32>;
@group(0) @binding(2) var<storage, read> coverage: array<f32>;
@group(0) @binding(3) var<storage, read_write> sum: array<f32>;
@group(0) @binding(4) var<storage, read_write> weight: array<f32>;

@compute @workgroup_size(256, 1, 1)
fn main(@builtin(global_invocation_id) id: vec3<u32>, @builtin(num_workgroups) groups: vec3<u32>) {
    let i = flat(id, groups);
    if (i >= p.count) {
        return;
    }
    let c = coverage[i];
    if (c < MIN_COVERAGE) {
        return;
    }
    let m = min(c, 1.0);
    let s = m * m;
    let k = s * s;
    if (!(k > 0.0)) {
        return;
    }
    for (var ch = 0u; ch < p.channels; ch++) {
        sum[i * p.channels + ch] = sum[i * p.channels + ch] + k * values[i * p.channels + ch];
    }
    weight[i] = weight[i] + k;
}
