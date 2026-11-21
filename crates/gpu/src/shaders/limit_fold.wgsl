// The coarse limit's record of a frame at a coarse level (florastack-stack's `Pyramid::fold_coarse`, its `limit`
// planes), run just before the level's `select` with the same weights, so that it sees each pick as `select` makes
// it:
//
// - picked: for each decision channel, the frame whose coefficient `select` is about to keep (its energy, weighed by
//   the fourth power of its coverage, beyond the best so far);
// - focus, frame, fallback: the frame whose finest-level energy pooled to this level (`focus`), weighed the same way,
//   is the largest so far, and its coefficients, which replace picks too far from it (`limit_apply`).
//
// Over the whole level: band, energy, coverage and focus have its samples.

struct Params {
    count: u32,
    channels: u32,
    decisions: u32,
    frame: f32,
}

@group(0) @binding(0) var<uniform> p: Params;
@group(0) @binding(1) var<storage, read> band: array<f32>;
@group(0) @binding(2) var<storage, read> energy: array<f32>;
@group(0) @binding(3) var<storage, read> coverage: array<f32>;
@group(0) @binding(4) var<storage, read> weights: array<f32>;
@group(0) @binding(5) var<storage, read> focus: array<f32>;
@group(0) @binding(6) var<storage, read_write> picked: array<f32>;
// Per pixel: the best focus, its frame, then its `channels` coefficients.
@group(0) @binding(7) var<storage, read_write> record: array<f32>;

fn pick_weight(k: f32) -> f32 {
    if (k >= MIN_COVERAGE) {
        let c = min(k, 1.0);
        let s = c * c;
        return s * s;
    }
    return 0.0;
}

@compute @workgroup_size(256, 1, 1)
fn main(@builtin(global_invocation_id) id: vec3<u32>, @builtin(num_workgroups) groups: vec3<u32>) {
    let i = flat(id, groups);
    if (i >= p.count) {
        return;
    }
    let c = p.channels;
    let d = p.decisions;
    let pk = pick_weight(coverage[i]);
    if (pk <= 0.0) {
        return;
    }
    for (var dch = 0u; dch < d; dch++) {
        // `select`'s comparison, on the weights it compares with.
        if (energy[i * d + dch] * pk > weights[i * d + dch]) {
            picked[i * d + dch] = p.frame;
        }
    }
    let r = i * (c + 2u);
    let f = focus[i] * pk;
    if (f > record[r]) {
        record[r] = f;
        record[r + 1u] = p.frame;
        for (var ch = 0u; ch < c; ch++) {
            record[r + 2u + ch] = band[i * c + ch];
        }
    }
}
