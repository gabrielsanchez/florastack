// One pass of EXPAND along the rows (axis 0) or the columns (axis 1) (florastack-stack's `levels::expand`): output
// sample x of the finer level sums `2 KERNEL[x - 2 j + 2] * src[j]` over every coarser sample j with |x - 2 j| <= 2,
// in order of j, each j mirrored at the coarser level's edges and clamped to the input span.

struct Params {
    width: u32,
    height: u32,
    channels: u32,
    axis: u32,
    in_start: i32,
    in_len: i32,
    in_full: i32,
    out_start: i32,
    in_width: u32,
    pad0: u32,
    pad1: u32,
    pad2: u32,
}

@group(0) @binding(0) var<uniform> p: Params;
@group(0) @binding(1) var<storage, read> src: array<f32>;
@group(0) @binding(2) var<storage, read_write> dst: array<f32>;



@compute @workgroup_size(16, 16, 1)
fn main(@builtin(global_invocation_id) id: vec3<u32>) {
    if (id.x >= p.width || id.y >= p.height) {
        return;
    }
    var along = i32(id.x);
    if (p.axis == 1u) {
        along = i32(id.y);
    }
    let x = p.out_start + along;
    // The first j with 2 j >= x - 2, and the last with 2 j <= x + 2.
    let first = half_floor(x - 2) + select(0, 1, (x & 1) != 0);
    let last = half_floor(x + 2);
    let at = (id.y * p.width + id.x) * p.channels;
    for (var c = 0u; c < p.channels; c++) {
        var s = 0.0;
        for (var j = first; j <= last; j++) {
            let weight = 2.0 * binomial(x - 2 * j + 2);
            let t = u32(local_index(j, p.in_full, p.in_start, p.in_len));
            var src_index: u32;
            if (p.axis == 0u) {
                src_index = (id.y * p.in_width + t) * p.channels + c;
            } else {
                src_index = (t * p.in_width + id.x) * p.channels + c;
            }
            s = s + weight * src[src_index];
        }
        dst[at + c] = s;
    }
}
