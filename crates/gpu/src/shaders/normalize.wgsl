// A weighted sum divided by its weights, `sum * (1 / total)`, 0 where nothing weighed, and which pixels something
// covered (1) or not (0): the weighted average's result and coverage (florastack-stack's `WeightedAverage::result`
// and `covered`), and the pyramid's residual.

struct Params {
    count: u32,
    channels: u32,
    pad0: u32,
    pad1: u32,
}

@group(0) @binding(0) var<uniform> p: Params;
@group(0) @binding(1) var<storage, read> sum: array<f32>;
@group(0) @binding(2) var<storage, read> total: array<f32>;
@group(0) @binding(3) var<storage, read_write> dst: array<f32>;
@group(0) @binding(4) var<storage, read_write> covered: array<f32>;

@compute @workgroup_size(256, 1, 1)
fn main(@builtin(global_invocation_id) id: vec3<u32>, @builtin(num_workgroups) groups: vec3<u32>) {
    let i = flat(id, groups);
    if (i >= p.count) {
        return;
    }
    let t = total[i];
    let at = i * p.channels;
    if (t > 0.0) {
        let inverse = 1.0 / t;
        for (var c = 0u; c < p.channels; c++) {
            dst[at + c] = sum[at + c] * inverse;
        }
        covered[i] = 1.0;
    } else {
        for (var c = 0u; c < p.channels; c++) {
            dst[at + c] = 0.0;
        }
        covered[i] = 0.0;
    }
}
