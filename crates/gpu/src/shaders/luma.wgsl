// Each pixel's Rec. 709 luminance and the coverage that counts, for the weighted average and the depth map's focus
// measure (florastack-stack's `WeightedAverage::weights` and `FocusKernel::measure`, their first loop): the frame's
// coverage, or 1 where it has none, counts as none below MIN_COVERAGE, where it isn't finite or where a sample isn't;
// the luminance is 0 there. Mode 0 keeps the coverage's value (at most 1), mode 1 makes it 1 or 0.

struct Params {
    count: u32,
    channels: u32,
    has_coverage: u32,
    mode: u32,
}

@group(0) @binding(0) var<uniform> p: Params;
@group(0) @binding(1) var<storage, read> image: array<f32>;
@group(0) @binding(2) var<storage, read> given: array<f32>;
@group(0) @binding(3) var<storage, read_write> luma: array<f32>;
@group(0) @binding(4) var<storage, read_write> coverage: array<f32>;

@compute @workgroup_size(256, 1, 1)
fn main(@builtin(global_invocation_id) id: vec3<u32>, @builtin(num_workgroups) groups: vec3<u32>) {
    let i = flat(id, groups);
    if (i >= p.count) {
        return;
    }
    var k = 1.0;
    if (p.has_coverage != 0u) {
        k = given[i];
    }
    let at = i * p.channels;
    var ok = finite(k) && k >= MIN_COVERAGE;
    for (var c = 0u; c < p.channels; c++) {
        ok = ok && finite(image[at + c]);
    }
    if (!ok) {
        luma[i] = 0.0;
        coverage[i] = 0.0;
        return;
    }
    var y = image[at];
    if (p.channels >= 3u) {
        y = LUMA_R * image[at] + LUMA_G * image[at + 1u] + LUMA_B * image[at + 2u];
    }
    luma[i] = clamp(y, -MAX_SAMPLE, MAX_SAMPLE);
    if (p.mode == 0u) {
        coverage[i] = min(k, 1.0);
    } else {
        coverage[i] = 1.0;
    }
}
