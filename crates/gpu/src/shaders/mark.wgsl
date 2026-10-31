// The focus measure marked -1 where the frame doesn't cover the pixel (florastack-stack's `FocusKernel::measure`).

struct Params {
    count: u32,
    pad0: u32,
    pad1: u32,
    pad2: u32,
}

@group(0) @binding(0) var<uniform> p: Params;
@group(0) @binding(1) var<storage, read_write> focus: array<f32>;
@group(0) @binding(2) var<storage, read> coverage: array<f32>;

@compute @workgroup_size(256, 1, 1)
fn main(@builtin(global_invocation_id) id: vec3<u32>, @builtin(num_workgroups) groups: vec3<u32>) {
    let i = flat(id, groups);
    if (i >= p.count) {
        return;
    }
    if (coverage[i] <= 0.0) {
        focus[i] = -1.0;
    }
}
