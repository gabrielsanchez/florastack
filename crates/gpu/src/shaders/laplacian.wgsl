// The five-point Laplacian of every channel, `(left + right - 4 center) + (up + down)` with clamp-to-edge neighbors,
// in the order florastack-core's `ops::laplacian` adds them.

struct Params {
    width: u32,
    height: u32,
    channels: u32,
    pad0: u32,
}

@group(0) @binding(0) var<uniform> p: Params;
@group(0) @binding(1) var<storage, read> src: array<f32>;
@group(0) @binding(2) var<storage, read_write> dst: array<f32>;

fn at(x: u32, y: u32, c: u32) -> f32 {
    return src[(y * p.width + x) * p.channels + c];
}

@compute @workgroup_size(16, 16, 1)
fn main(@builtin(global_invocation_id) id: vec3<u32>) {
    if (id.x >= p.width || id.y >= p.height) {
        return;
    }
    let x = id.x;
    let y = id.y;
    let left = select(x - 1u, 0u, x == 0u);
    let right = min(x + 1u, p.width - 1u);
    let up = select(y - 1u, 0u, y == 0u);
    let down = min(y + 1u, p.height - 1u);
    for (var c = 0u; c < p.channels; c++) {
        let across = at(left, y, c) + at(right, y, c) - 4.0 * at(x, y, c);
        dst[(y * p.width + x) * p.channels + c] = across + (at(x, up, c) + at(x, down, c));
    }
}
