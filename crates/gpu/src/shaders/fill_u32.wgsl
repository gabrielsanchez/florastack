// Every word of a buffer set to one value: bit patterns that f32 stores could flush (denormals) or change.

struct Params {
    count: u32,
    value: u32,
    pad0: u32,
    pad1: u32,
}

@group(0) @binding(0) var<uniform> p: Params;
@group(0) @binding(1) var<storage, read_write> dst: array<u32>;

@compute @workgroup_size(256, 1, 1)
fn main(@builtin(global_invocation_id) id: vec3<u32>, @builtin(num_workgroups) groups: vec3<u32>) {
    let i = flat(id, groups);
    if (i >= p.count) {
        return;
    }
    dst[i] = p.value;
}
