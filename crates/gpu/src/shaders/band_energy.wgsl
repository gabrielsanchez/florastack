// The squares a band's local energy filters (florastack-stack's `levels::local_energy`, its first step): of each
// pixel's Rec. 709 luminance (`levels::luminance`, decisions on luminance), or of each channel (decisions on every
// channel). The output has 1 channel or the band's.

struct Params {
    count: u32,
    channels: u32,
    luminance: u32,
    pad0: u32,
}

@group(0) @binding(0) var<uniform> p: Params;
@group(0) @binding(1) var<storage, read> band: array<f32>;
@group(0) @binding(2) var<storage, read_write> dst: array<f32>;

@compute @workgroup_size(256, 1, 1)
fn main(@builtin(global_invocation_id) id: vec3<u32>, @builtin(num_workgroups) groups: vec3<u32>) {
    let i = flat(id, groups);
    if (i >= p.count) {
        return;
    }
    let at = i * p.channels;
    if (p.luminance != 0u) {
        var y = band[at];
        if (p.channels >= 3u) {
            y = LUMA_R * band[at] + LUMA_G * band[at + 1u] + LUMA_B * band[at + 2u];
        }
        dst[i] = y * y;
    } else {
        for (var c = 0u; c < p.channels; c++) {
            let v = band[at + c];
            dst[at + c] = v * v;
        }
    }
}
