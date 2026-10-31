// The two steps of REDUCE by normalized convolution around the filter passes (florastack-stack's
// `levels::reduce_covered`): op 0 weighs every sample by its pixel's coverage (`dst = src * coverage`), op 1 divides
// the reduced samples by the reduced coverage in place (`values * (1 / k)`, or 0 where k is below 1e-6).

struct Params {
    count: u32,
    channels: u32,
    op: u32,
    pad0: u32,
}

@group(0) @binding(0) var<uniform> p: Params;
@group(0) @binding(1) var<storage, read> src: array<f32>;
@group(0) @binding(2) var<storage, read> coverage: array<f32>;
@group(0) @binding(3) var<storage, read_write> dst: array<f32>;

@compute @workgroup_size(256, 1, 1)
fn main(@builtin(global_invocation_id) id: vec3<u32>, @builtin(num_workgroups) groups: vec3<u32>) {
    let i = flat(id, groups);
    if (i >= p.count) {
        return;
    }
    let k = coverage[i];
    let at = i * p.channels;
    if (p.op == 0u) {
        for (var c = 0u; c < p.channels; c++) {
            dst[at + c] = src[at + c] * k;
        }
    } else if (k >= 1e-6) {
        let inverse = 1.0 / k;
        for (var c = 0u; c < p.channels; c++) {
            dst[at + c] = dst[at + c] * inverse;
        }
    } else {
        for (var c = 0u; c < p.channels; c++) {
            dst[at + c] = 0.0;
        }
    }
}
