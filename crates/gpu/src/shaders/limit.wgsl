// The end of the pyramid's result over a tile's inner region (florastack-stack's `Pyramid::result` and `covered`):
// 0 where no frame covered (the finest level's weights: any above 0 for the blend, any at least 0 for picks), and
// with the range limit on, each channel moved toward the frames' range there by `limit` of its excess (`limit_to`).
// Writes which pixels are covered too.

struct Params {
    count: u32,
    channels: u32,
    decisions: u32,
    blend: u32,
    bounded: u32,
    limit: f32,
    pad0: u32,
    pad1: u32,
}

@group(0) @binding(0) var<uniform> p: Params;
@group(0) @binding(1) var<storage, read_write> image: array<f32>;
@group(0) @binding(2) var<storage, read> weights: array<f32>;
// Each word: the least value's bfloat16 bits low, the largest's high (bounds.wgsl).
@group(0) @binding(3) var<storage, read> bounds: array<u32>;
@group(0) @binding(4) var<storage, read_write> covered: array<f32>;

const EMPTY_LOW: u32 = 0x7f80u;
const EMPTY_HIGH: u32 = 0xff80u;

fn value(bits: u32) -> f32 {
    return bitcast<f32>(bits << 16u);
}

@compute @workgroup_size(256, 1, 1)
fn main(@builtin(global_invocation_id) id: vec3<u32>, @builtin(num_workgroups) groups: vec3<u32>) {
    let i = flat(id, groups);
    if (i >= p.count) {
        return;
    }
    var hit = false;
    for (var dch = 0u; dch < p.decisions; dch++) {
        let w = weights[i * p.decisions + dch];
        hit = hit || select(w >= 0.0, w > 0.0, p.blend != 0u);
    }
    let at = i * p.channels;
    if (!hit) {
        covered[i] = 0.0;
        for (var c = 0u; c < p.channels; c++) {
            image[at + c] = 0.0;
        }
        return;
    }
    covered[i] = 1.0;
    if (p.bounded == 0u) {
        return;
    }
    for (var c = 0u; c < p.channels; c++) {
        let l = bounds[at + c] & 0xffffu;
        let h = bounds[at + c] >> 16u;
        if (l == EMPTY_LOW || h == EMPTY_HIGH) {
            continue;
        }
        let lo = value(l);
        let hi = value(h);
        if (lo > hi) {
            continue;
        }
        let v = image[at + c];
        let k = clamp(v, lo, hi);
        image[at + c] = k + (1.0 - p.limit) * (v - k);
    }
}
