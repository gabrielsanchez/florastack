// Two planes of the same shape combined sample by sample: `a + b` (op 0) or `a - b` (op 1), as florastack-stack's
// pyramid adds an expanded level to a band and subtracts one from a Gaussian level.

struct Params {
    count: u32,
    op: u32,
    pad0: u32,
    pad1: u32,
}

@group(0) @binding(0) var<uniform> p: Params;
@group(0) @binding(1) var<storage, read> a: array<f32>;
@group(0) @binding(2) var<storage, read> b: array<f32>;
@group(0) @binding(3) var<storage, read_write> dst: array<f32>;

@compute @workgroup_size(256, 1, 1)
fn main(@builtin(global_invocation_id) id: vec3<u32>, @builtin(num_workgroups) groups: vec3<u32>) {
    let i = flat(id, groups);
    if (i >= p.count) {
        return;
    }
    if (p.op == 0u) {
        dst[i] = a[i] + b[i];
    } else {
        dst[i] = a[i] - b[i];
    }
}
