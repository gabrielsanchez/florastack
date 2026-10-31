// A band folded into a level's running choice (florastack-stack's `Pyramid::fold_level` and the coarse levels'
// fold), over the state's samples, which start at (offset_x, offset_y) of the band's span:
//
// - pick (blend 0): each decision channel keeps the coefficients of the frame whose energy, weighed by the fourth
//   power of its coverage, is the largest so far; ties keep the earlier frame. The state's weights hold that energy
//   (-1 before any frame covers).
// - blend (blend 1, the finest level against grit): each frame's coefficients weighted by its energy, scaled as the
//   weighted average scales it, raised to `power`, times its coverage, and added up with their weights.

struct Params {
    width: u32,
    height: u32,
    channels: u32,
    decisions: u32,
    band_width: u32,
    offset_x: u32,
    offset_y: u32,
    blend: u32,
    power: f32,
    pad0: u32,
    pad1: u32,
    pad2: u32,
}

@group(0) @binding(0) var<uniform> p: Params;
@group(0) @binding(1) var<storage, read> band: array<f32>;
@group(0) @binding(2) var<storage, read> energy: array<f32>;
@group(0) @binding(3) var<storage, read> coverage: array<f32>;
@group(0) @binding(4) var<storage, read_write> coefficients: array<f32>;
@group(0) @binding(5) var<storage, read_write> weights: array<f32>;

fn pick_weight(k: f32) -> f32 {
    if (k >= MIN_COVERAGE) {
        let c = min(k, 1.0);
        let s = c * c;
        return s * s;
    }
    return 0.0;
}

fn blend_weight(e: f32, k: f32) -> f32 {
    let s = min(max(e, 0.0) / ENERGY_SCALE, ENERGY_CAP) + ENERGY_FLOOR;
    var w: f32;
    if (p.power == 2.0) {
        w = s * s;
    } else if (p.power == 1.0) {
        w = s;
    } else if (p.power == 4.0) {
        let q = s * s;
        w = q * q;
    } else {
        w = pow(s, p.power);
    }
    return w * k;
}

@compute @workgroup_size(16, 16, 1)
fn main(@builtin(global_invocation_id) id: vec3<u32>) {
    if (id.x >= p.width || id.y >= p.height) {
        return;
    }
    let b = (p.offset_y + id.y) * p.band_width + p.offset_x + id.x;
    let s = id.y * p.width + id.x;
    let c = p.channels;
    let d = p.decisions;
    let k = coverage[b];
    if (p.blend != 0u) {
        if (k < MIN_COVERAGE) {
            return;
        }
        for (var ch = 0u; ch < c; ch++) {
            var dch = ch;
            if (d == 1u) {
                dch = 0u;
            }
            let weight = blend_weight(energy[b * d + dch], k);
            coefficients[s * c + ch] = coefficients[s * c + ch] + weight * band[b * c + ch];
            if (d > 1u || ch == 0u) {
                weights[s * d + dch] = weights[s * d + dch] + weight;
            }
        }
        return;
    }
    let pk = pick_weight(k);
    if (pk <= 0.0) {
        return;
    }
    for (var dch = 0u; dch < d; dch++) {
        let candidate = energy[b * d + dch] * pk;
        if (candidate > weights[s * d + dch]) {
            weights[s * d + dch] = candidate;
            if (d == 1u) {
                for (var ch = 0u; ch < c; ch++) {
                    coefficients[s * c + ch] = band[b * c + ch];
                }
            } else {
                coefficients[s * c + dch] = band[b * c + dch];
            }
        }
    }
}
