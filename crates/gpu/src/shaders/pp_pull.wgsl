// A level of push-pull's pull (florastack-stack's `depth::filter::push_pull` and `upsampled`): each pixel's own
// premultiplied value topped up by the coarser level's value, bilinearly interpolated, as far as its weight is below
// 1. With `top`, the coarsest level's values instead: premultiplied over weight, 0 where there is none.

struct Params {
    width: u32,
    height: u32,
    coarse_width: u32,
    coarse_height: u32,
    top: u32,
    pad0: u32,
    pad1: u32,
    pad2: u32,
}

@group(0) @binding(0) var<uniform> p: Params;
@group(0) @binding(1) var<storage, read> level: array<f32>;
@group(0) @binding(2) var<storage, read> coarse: array<f32>;
@group(0) @binding(3) var<storage, read_write> fine: array<f32>;

fn at(x: u32, y: u32) -> f32 {
    return coarse[y * p.coarse_width + x];
}

@compute @workgroup_size(16, 16, 1)
fn main(@builtin(global_invocation_id) id: vec3<u32>) {
    if (id.x >= p.width || id.y >= p.height) {
        return;
    }
    let i = id.y * p.width + id.x;
    let pv = level[2u * i];
    let kv = level[2u * i + 1u];
    if (p.top != 0u) {
        fine[i] = select(0.0, pv / kv, kv > 0.0);
        return;
    }
    let fx = max(f32(id.x) * 0.5 - 0.25, 0.0);
    let fy = max(f32(id.y) * 0.5 - 0.25, 0.0);
    let x0 = min(u32(fx), p.coarse_width - 1u);
    let y0 = min(u32(fy), p.coarse_height - 1u);
    let x1 = min(x0 + 1u, p.coarse_width - 1u);
    let y1 = min(y0 + 1u, p.coarse_height - 1u);
    let tx = clamp(fx - f32(x0), 0.0, 1.0);
    let ty = clamp(fy - f32(y0), 0.0, 1.0);
    let upper = at(x0, y0) + tx * (at(x1, y0) - at(x0, y0));
    let lower = at(x0, y1) + tx * (at(x1, y1) - at(x0, y1));
    let up = upper + ty * (lower - upper);
    fine[i] = pv + (1.0 - kv) * up;
}
