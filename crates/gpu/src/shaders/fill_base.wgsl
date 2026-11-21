// Push-pull's finest level from the smoothing (florastack-stack's `depth::fill` and `depth::filter::push_pull`): the
// smoothed depth and its reach, the smoothed weight over `reached` up to 1 and clamped to [0, 1] (0 if not finite),
// premultiplied and interleaved.

struct Params {
    count: u32,
    reached: f32,
    pad0: u32,
    pad1: u32,
}

@group(0) @binding(0) var<uniform> p: Params;
@group(0) @binding(1) var<storage, read> packed: array<f32>;
@group(0) @binding(2) var<storage, read_write> base: array<f32>;

@compute @workgroup_size(256, 1, 1)
fn main(@builtin(global_invocation_id) id: vec3<u32>, @builtin(num_workgroups) groups: vec3<u32>) {
    let i = flat(id, groups);
    if (i >= p.count) {
        return;
    }
    let w = packed[2u * i + 1u];
    var s = 0.0;
    var k = 0.0;
    if (w > 1e-30) {
        s = packed[2u * i] / w;
        k = w;
    }
    var reach = min(k / p.reached, 1.0);
    if (finite(reach)) {
        reach = clamp(reach, 0.0, 1.0);
    } else {
        reach = 0.0;
    }
    base[2u * i] = reach * s;
    base[2u * i + 1u] = reach;
}
