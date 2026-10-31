// The range limit's bounds over a tile's inner region (florastack-stack's pyramid fold, after focus-stack's range
// limiting; see range.rs): each channel's least and largest value over the frames that cover the pixel, kept as the
// bits of a bfloat16 rounded outward, the least in the low 16 bits of a word and the largest in the high 16, as the
// CPU keeps them in two u16. EMPTY_LOW and EMPTY_HIGH (+inf and -inf) mean no frame yet; they are compared as bits,
// not as infinities.

struct Params {
    width: u32,
    height: u32,
    channels: u32,
    region_width: u32,
    offset_x: u32,
    offset_y: u32,
    pad0: u32,
    pad1: u32,
}

@group(0) @binding(0) var<uniform> p: Params;
@group(0) @binding(1) var<storage, read> image: array<f32>;
@group(0) @binding(2) var<storage, read> coverage: array<f32>;
@group(0) @binding(3) var<storage, read_write> bounds: array<u32>;

const EMPTY_LOW: u32 = 0x7f80u;
const EMPTY_HIGH: u32 = 0xff80u;

fn value(bits: u32) -> f32 {
    return bitcast<f32>(bits << 16u);
}

// The bfloat16 bits of the largest bfloat16 at most v (`bound_down`), and of the smallest at least v (`bound_up`).
fn down(v: f32) -> u32 {
    let bits = bitcast<u32>(v);
    let exact = (bits & 0xffffu) == 0u;
    let negative = (bits >> 31u) != 0u;
    return (bits >> 16u) + select(0u, 1u, negative && !exact);
}

fn up(v: f32) -> u32 {
    let bits = bitcast<u32>(v);
    let exact = (bits & 0xffffu) == 0u;
    let negative = (bits >> 31u) != 0u;
    return (bits >> 16u) + select(0u, 1u, !negative && !exact);
}

@compute @workgroup_size(16, 16, 1)
fn main(@builtin(global_invocation_id) id: vec3<u32>) {
    if (id.x >= p.width || id.y >= p.height) {
        return;
    }
    let src = (p.offset_y + id.y) * p.region_width + p.offset_x + id.x;
    if (coverage[src] < MIN_COVERAGE) {
        return;
    }
    let dst = (id.y * p.width + id.x) * p.channels;
    for (var c = 0u; c < p.channels; c++) {
        let v = image[src * p.channels + c];
        var l = bounds[dst + c] & 0xffffu;
        var h = bounds[dst + c] >> 16u;
        if (l == EMPTY_LOW || v < value(l)) {
            l = down(v);
        }
        if (h == EMPTY_HIGH || v > value(h)) {
            h = up(v);
        }
        bounds[dst + c] = (h << 16u) | (l & 0xffffu);
    }
}
