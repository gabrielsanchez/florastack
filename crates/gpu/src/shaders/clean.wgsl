// A frame's samples made safe for the pyramid (florastack-stack's pyramid `clean`): a pixel with a sample or a
// coverage that isn't finite, or coverage below MIN_COVERAGE, is uncovered (0, coverage 0); the rest is clamped to
// MAX_SAMPLE, its coverage at most 1. Without a coverage plane every finite pixel covers fully, which gives the CPU's
// results bit for bit: normalized convolution by a coverage of exactly 1 changes no sample.

struct Params {
    count: u32,
    channels: u32,
    has_coverage: u32,
    pad0: u32,
}

@group(0) @binding(0) var<uniform> p: Params;
@group(0) @binding(1) var<storage, read> image: array<f32>;
@group(0) @binding(2) var<storage, read> given: array<f32>;
@group(0) @binding(3) var<storage, read_write> dst: array<f32>;
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
    if (ok) {
        coverage[i] = min(k, 1.0);
        for (var c = 0u; c < p.channels; c++) {
            dst[at + c] = clamp(image[at + c], -MAX_SAMPLE, MAX_SAMPLE);
        }
    } else {
        coverage[i] = 0.0;
        for (var c = 0u; c < p.channels; c++) {
            dst[at + c] = 0.0;
        }
    }
}
