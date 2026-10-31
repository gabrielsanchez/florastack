// The depth map's composite from its sums (florastack-stack's `CompositeKernel::result` and `covered`): the weighted
// mean of the frames at each pixel's depth, `sum * (1 / total)`; where neither frame at the depth covered, the
// nearest covering frame's pixel; 0 where no frame covered. Writes which pixels are covered too.

struct Params {
    count: u32,
    channels: u32,
    pad0: u32,
    pad1: u32,
}

@group(0) @binding(0) var<uniform> p: Params;
@group(0) @binding(1) var<storage, read> sum: array<f32>;
@group(0) @binding(2) var<storage, read> total: array<f32>;
@group(0) @binding(3) var<storage, read> nearest: array<f32>;
@group(0) @binding(4) var<storage, read> distance: array<f32>;
@group(0) @binding(5) var<storage, read_write> dst: array<f32>;
@group(0) @binding(6) var<storage, read_write> covered: array<f32>;

@compute @workgroup_size(256, 1, 1)
fn main(@builtin(global_invocation_id) id: vec3<u32>, @builtin(num_workgroups) groups: vec3<u32>) {
    let i = flat(id, groups);
    if (i >= p.count) {
        return;
    }
    let c = p.channels;
    let t = total[i];
    let reached = distance[i] < BIG;
    covered[i] = select(0.0, 1.0, reached);
    if (t > 0.0) {
        let inverse = 1.0 / t;
        for (var ch = 0u; ch < c; ch++) {
            dst[i * c + ch] = sum[i * c + ch] * inverse;
        }
    } else if (reached) {
        for (var ch = 0u; ch < c; ch++) {
            dst[i * c + ch] = nearest[i * c + ch];
        }
    } else {
        for (var ch = 0u; ch < c; ch++) {
            dst[i * c + ch] = 0.0;
        }
    }
}
