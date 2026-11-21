// The coarse limit applied to a coarse level's coefficients (florastack-stack's `Pyramid::prepare`): a coefficient
// picked from a frame more than `limit` frames from the frame whose pooled focus is largest there takes that frame's
// coefficient instead. Pixels no frame focused on, and coefficients no frame picked, keep theirs.

struct Params {
    count: u32,
    channels: u32,
    decisions: u32,
    limit: f32,
}

@group(0) @binding(0) var<uniform> p: Params;
@group(0) @binding(1) var<storage, read> picked: array<f32>;
@group(0) @binding(2) var<storage, read> record: array<f32>;
@group(0) @binding(3) var<storage, read_write> band: array<f32>;

@compute @workgroup_size(256, 1, 1)
fn main(@builtin(global_invocation_id) id: vec3<u32>, @builtin(num_workgroups) groups: vec3<u32>) {
    let i = flat(id, groups);
    if (i >= p.count) {
        return;
    }
    let c = p.channels;
    let r = i * (c + 2u);
    let dc = record[r + 1u];
    if (dc < 0.0) {
        return;
    }
    for (var ch = 0u; ch < c; ch++) {
        var dch = ch;
        if (p.decisions == 1u) {
            dch = 0u;
        }
        let picked_frame = picked[i * p.decisions + dch];
        if (picked_frame >= 0.0 && abs(picked_frame - dc) > p.limit) {
            band[i * c + ch] = record[r + 2u + ch];
        }
    }
}
