// A rectangle of samples copied from one plane into another: `width` x `height` pixels of `channels` channels, from
// (src_x, src_y) of a plane `src_width` wide to (dst_x, dst_y) of one `dst_width` wide.

struct Params {
    width: u32,
    height: u32,
    channels: u32,
    src_width: u32,
    src_x: u32,
    src_y: u32,
    dst_width: u32,
    dst_x: u32,
    dst_y: u32,
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
    let src_at = ((p.src_y + id.y) * p.src_width + p.src_x + id.x) * p.channels;
    let dst_at = ((p.dst_y + id.y) * p.dst_width + p.dst_x + id.x) * p.channels;
    for (var c = 0u; c < p.channels; c++) {
        dst[dst_at + c] = src[src_at + c];
    }
}
