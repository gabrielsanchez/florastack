// The domain transform's distances (florastack-stack's `depth::filter::Domain::new`): from each pixel to the one
// before it in its row (`across`) and in its column (`down`), `1 + ratio |guide difference|`, 1 for the first. With
// `cube_root`, the guide is taken as the depth map's resolve takes it: the cube root of its luminance, 0 below 0.

struct Params {
    width: u32,
    height: u32,
    ratio: f32,
    cube_root: u32,
}

@group(0) @binding(0) var<uniform> p: Params;
@group(0) @binding(1) var<storage, read> guide: array<f32>;
@group(0) @binding(2) var<storage, read_write> across: array<f32>;
@group(0) @binding(3) var<storage, read_write> down: array<f32>;

// The cube root of `v`, 0 for what isn't above 0: an estimate refined by one Newton step, within an ulp or two of
// Rust's `cbrt`.
fn cube_root(v: f32) -> f32 {
    if (!(v > 0.0) || !finite(v)) {
        return 0.0;
    }
    let y = exp2(log2(v) / 3.0);
    return y - (y * y * y - v) / (3.0 * y * y);
}

fn g(x: u32, y: u32) -> f32 {
    let v = guide[y * p.width + x];
    if (p.cube_root != 0u) {
        return cube_root(v);
    }
    return v;
}

@compute @workgroup_size(16, 16, 1)
fn main(@builtin(global_invocation_id) id: vec3<u32>) {
    if (id.x >= p.width || id.y >= p.height) {
        return;
    }
    let i = id.y * p.width + id.x;
    let here = g(id.x, id.y);
    var a = 0.0;
    if (id.x > 0u) {
        a = abs(here - g(id.x - 1u, id.y));
    }
    var d = 0.0;
    if (id.y > 0u) {
        d = abs(here - g(id.x, id.y - 1u));
    }
    across[i] = 1.0 + p.ratio * a;
    down[i] = 1.0 + p.ratio * d;
}
