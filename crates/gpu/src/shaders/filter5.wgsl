// One pass of the 5-tap binomial kernel [1, 4, 6, 4, 1] / 16 along the rows (axis 0) or the columns (axis 1) of a
// span of a pyramid level, mirrored at the level's edges (florastack-stack's `levels`): with stride 2 it is REDUCE's
// pass (output sample j centered on input sample 2 j), with stride 1 the local energy's window. Taps are summed in
// order from 0. The input is a span of `in_len` samples from `in_start` along the axis of a level of `in_full`; the
// output holds the samples `out_start ..` of its level along the axis, and the input's samples across it.

struct Params {
    // The output's width and height.
    width: u32,
    height: u32,
    channels: u32,
    axis: u32,
    stride: i32,
    in_start: i32,
    in_len: i32,
    in_full: i32,
    out_start: i32,
    // The input's width (along the rows: in_len).
    in_width: u32,
    pad0: u32,
    pad1: u32,
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
    let center = p.stride * (p.out_start + along);
    let at = (id.y * p.width + id.x) * p.channels;
    for (var c = 0u; c < p.channels; c++) {
        var s = 0.0;
        for (var m = 0; m < 5; m++) {
            let t = u32(local_index(center + m - 2, p.in_full, p.in_start, p.in_len));
            var src_index: u32;
            if (p.axis == 0u) {
                src_index = (id.y * p.in_width + t) * p.channels + c;
            } else {
                src_index = (t * p.in_width + id.x) * p.channels + c;
            }
            s = s + binomial(m) * src[src_index];
        }
        dst[at + c] = s;
    }
}
