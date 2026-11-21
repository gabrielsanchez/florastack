// A coarser level of the push-pull pyramid (florastack-stack's `depth::filter::Level::reduce`): each 2x2 block's
// premultiplied values and weights summed in the CPU's order, normalized where the weight passes 1.

struct Params {
    width: u32,
    height: u32,
    src_width: u32,
    src_height: u32,
}

@group(0) @binding(0) var<uniform> p: Params;
@group(0) @binding(1) var<storage, read> src: array<f32>;
@group(0) @binding(2) var<storage, read_write> dst: array<f32>;

@compute @workgroup_size(16, 16, 1)
fn main(@builtin(global_invocation_id) id: vec3<u32>) {
    if (id.x >= p.width || id.y >= p.height) {
        return;
    }
    var pv = 0.0;
    var kv = 0.0;
    // The block's samples in the CPU's order: (0, 0), (1, 0), (0, 1), (1, 1).
    for (var t = 0u; t < 4u; t++) {
        let sx = 2u * id.x + (t & 1u);
        let sy = 2u * id.y + (t >> 1u);
        if (sx < p.src_width && sy < p.src_height) {
            let s = sy * p.src_width + sx;
            pv = pv + src[2u * s];
            kv = kv + src[2u * s + 1u];
        }
    }
    if (kv > 1.0) {
        pv = pv / kv;
        kv = 1.0;
    }
    let o = id.y * p.width + id.x;
    dst[2u * o] = pv;
    dst[2u * o + 1u] = kv;
}
