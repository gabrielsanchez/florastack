// f32 samples through the sRGB transfer curve: decoded to linear (op 0, `srgb_to_linear`) or encoded from it (op 1,
// `linear_to_srgb`), mirrored for negative values, values above 1 following the curve; samples that aren't finite
// stay as they are. The alpha channel is left alone.

struct Params {
    count: u32,
    channels: u32,
    alpha: u32,
    op: u32,
}

@group(0) @binding(0) var<uniform> p: Params;
@group(0) @binding(1) var<storage, read> src: array<f32>;
@group(0) @binding(2) var<storage, read_write> dst: array<f32>;

fn decode(v: f32) -> f32 {
    let a = abs(v);
    var r: f32;
    if (a <= 0.04045) {
        r = a / 12.92;
    } else {
        r = pow((a + 0.055) / 1.055, 2.4);
    }
    return select(r, -r, v < 0.0);
}

fn encode(v: f32) -> f32 {
    let a = abs(v);
    var r: f32;
    if (a <= 0.0031308) {
        r = a * 12.92;
    } else {
        r = 1.055 * pow(a, 1.0 / 2.4) - 0.055;
    }
    return select(r, -r, v < 0.0);
}

@compute @workgroup_size(256, 1, 1)
fn main(@builtin(global_invocation_id) id: vec3<u32>, @builtin(num_workgroups) groups: vec3<u32>) {
    let i = flat(id, groups);
    if (i >= p.count) {
        return;
    }
    let v = src[i];
    if ((i % p.channels) == p.alpha || !finite(v)) {
        dst[i] = v;
    } else if (p.op == 0u) {
        dst[i] = decode(v);
    } else {
        dst[i] = encode(v);
    }
}
