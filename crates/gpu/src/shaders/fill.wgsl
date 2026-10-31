// Every sample of a plane set from a repeating pattern of `period` values (1 to 8): sample i takes value i % period.
// One value fills a plane; eight start the depth map's records.

struct Params {
    count: u32,
    period: u32,
    pad0: u32,
    pad1: u32,
    a: vec4<f32>,
    b: vec4<f32>,
}

@group(0) @binding(0) var<uniform> p: Params;
@group(0) @binding(1) var<storage, read_write> dst: array<f32>;

@compute @workgroup_size(256, 1, 1)
fn main(@builtin(global_invocation_id) id: vec3<u32>, @builtin(num_workgroups) groups: vec3<u32>) {
    let i = flat(id, groups);
    if (i >= p.count) {
        return;
    }
    let j = i % max(p.period, 1u);
    if (j < 4u) {
        dst[i] = p.a[j];
    } else {
        dst[i] = p.b[j - 4u];
    }
}
