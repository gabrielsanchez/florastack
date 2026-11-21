// The normalized convolution's result (florastack-stack's `depth::filter::smooth`): the smoothed weighted value
// divided by the smoothed weight, and that weight, both 0 where no weight reaches (1e-30 or less).

struct Params {
    count: u32,
    pad0: u32,
    pad1: u32,
    pad2: u32,
}

@group(0) @binding(0) var<uniform> p: Params;
@group(0) @binding(1) var<storage, read> packed: array<f32>;
@group(0) @binding(2) var<storage, read_write> smoothed: array<f32>;
@group(0) @binding(3) var<storage, read_write> weight: array<f32>;

@compute @workgroup_size(256, 1, 1)
fn main(@builtin(global_invocation_id) id: vec3<u32>, @builtin(num_workgroups) groups: vec3<u32>) {
    let i = flat(id, groups);
    if (i >= p.count) {
        return;
    }
    let w = packed[2u * i + 1u];
    if (w > 1e-30) {
        smoothed[i] = packed[2u * i] / w;
        weight[i] = w;
    } else {
        smoothed[i] = 0.0;
        weight[i] = 0.0;
    }
}
