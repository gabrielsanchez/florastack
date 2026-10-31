// Frame `frame`'s focus measure folded into the depth map's running record of each pixel of a tile's inner region
// (florastack-stack's depth `focus::update`): eight values a pixel, [best, peak, before, after, last, runner, floor,
// guide], which start at [-1, -1, -1, -1, -1, -1, BIG, 0]. Frames arrive in order; ties keep the earlier frame.

struct Params {
    width: u32,
    height: u32,
    region_width: u32,
    offset_x: u32,
    offset_y: u32,
    frame: f32,
    pad0: u32,
    pad1: u32,
}

@group(0) @binding(0) var<uniform> p: Params;
@group(0) @binding(1) var<storage, read> focus: array<f32>;
@group(0) @binding(2) var<storage, read> luma: array<f32>;
@group(0) @binding(3) var<storage, read_write> record: array<f32>;

@compute @workgroup_size(16, 16, 1)
fn main(@builtin(global_invocation_id) id: vec3<u32>) {
    if (id.x >= p.width || id.y >= p.height) {
        return;
    }
    let src = (p.offset_y + id.y) * p.region_width + p.offset_x + id.x;
    let at = (id.y * p.width + id.x) * 8u;
    let v = focus[src];
    let i = p.frame;
    if (v < 0.0) {
        record[at + 4u] = -1.0;
        return;
    }
    var best = record[at];
    var peak = record[at + 1u];
    var before = record[at + 2u];
    var after = record[at + 3u];
    let last = record[at + 4u];
    var runner = record[at + 5u];
    var floor_ = record[at + 6u];
    var guide = record[at + 7u];
    if (v > peak) {
        if (peak >= 0.0) {
            // The runner-up's frames are those at least two from the sharpest (see focus.rs).
            if (best <= i - 2.0) {
                runner = peak;
            } else {
                runner = max(runner, before);
            }
        }
        before = last;
        after = -1.0;
        best = i;
        peak = v;
        guide = luma[src];
    } else if (best + 1.0 == i) {
        after = v;
    } else {
        runner = max(runner, v);
    }
    floor_ = min(floor_, v);
    record[at] = best;
    record[at + 1u] = peak;
    record[at + 2u] = before;
    record[at + 3u] = after;
    record[at + 4u] = v;
    record[at + 5u] = runner;
    record[at + 6u] = floor_;
    record[at + 7u] = guide;
}
