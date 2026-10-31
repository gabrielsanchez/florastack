// The coefficients a level's state holds (florastack-stack's `Pyramid::coefficients`): the picks, 0 where no frame
// covered (weight below 0), or for the finest level's blend the weighted sum divided by its weights, 0 where nothing
// weighed.

struct Params {
    count: u32,
    channels: u32,
    decisions: u32,
    blend: u32,
}

@group(0) @binding(0) var<uniform> p: Params;
@group(0) @binding(1) var<storage, read> coefficients: array<f32>;
@group(0) @binding(2) var<storage, read> weights: array<f32>;
@group(0) @binding(3) var<storage, read_write> dst: array<f32>;

@compute @workgroup_size(256, 1, 1)
fn main(@builtin(global_invocation_id) id: vec3<u32>, @builtin(num_workgroups) groups: vec3<u32>) {
    let i = flat(id, groups);
    if (i >= p.count) {
        return;
    }
    for (var ch = 0u; ch < p.channels; ch++) {
        var dch = ch;
        if (p.decisions == 1u) {
            dch = 0u;
        }
        let w = weights[i * p.decisions + dch];
        let v = coefficients[i * p.channels + ch];
        var out = v;
        if (p.blend != 0u) {
            out = select(0.0, v / w, w > 0.0);
        } else if (w < 0.0) {
            out = 0.0;
        }
        dst[i * p.channels + ch] = out;
    }
}
