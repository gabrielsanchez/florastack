// The weighted average's sharpness from the blurred energy: scaled by ENERGY_SCALE, capped, floored and raised to
// the power, with the CPU's fast paths for whole powers (florastack-stack's `WeightedAverage::sharpen`).

struct Params {
    count: u32,
    power: f32,
    pad0: u32,
    pad1: u32,
}

@group(0) @binding(0) var<uniform> p: Params;
@group(0) @binding(1) var<storage, read_write> energy: array<f32>;

@compute @workgroup_size(256, 1, 1)
fn main(@builtin(global_invocation_id) id: vec3<u32>, @builtin(num_workgroups) groups: vec3<u32>) {
    let i = flat(id, groups);
    if (i >= p.count) {
        return;
    }
    let e = min(max(energy[i], 0.0) / ENERGY_SCALE, ENERGY_CAP) + ENERGY_FLOOR;
    var w: f32;
    if (p.power == 2.0) {
        w = e * e;
    } else if (p.power == 3.0) {
        w = e * e * e;
    } else if (p.power == 4.0) {
        let s = e * e;
        w = s * s;
    } else if (p.power == 1.0) {
        w = e;
    } else {
        w = pow(e, p.power);
    }
    energy[i] = w;
}
