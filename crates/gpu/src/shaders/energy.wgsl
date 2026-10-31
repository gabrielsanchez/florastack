// The squared five-point Laplacian of the luminance where its five taps all fall on covered pixels, 0 elsewhere
// (florastack-stack's `WeightedAverage::weights`, the Laplacian's energy). Mode 1 is the depth map's modified
// Laplacian instead, `|2 v - left - right| + |2 v - up - down|`, squared (`FocusKernel::measure`).

struct Params {
    width: u32,
    height: u32,
    mode: u32,
    pad0: u32,
}

@group(0) @binding(0) var<uniform> p: Params;
@group(0) @binding(1) var<storage, read> luma: array<f32>;
@group(0) @binding(2) var<storage, read> coverage: array<f32>;
@group(0) @binding(3) var<storage, read_write> energy: array<f32>;

@compute @workgroup_size(16, 16, 1)
fn main(@builtin(global_invocation_id) id: vec3<u32>) {
    if (id.x >= p.width || id.y >= p.height) {
        return;
    }
    let x = id.x;
    let y = id.y;
    let w = p.width;
    let left = select(x - 1u, 0u, x == 0u);
    let right = min(x + 1u, w - 1u);
    let up = select(y - 1u, 0u, y == 0u);
    let down = min(y + 1u, p.height - 1u);
    let m = y * w + x;
    let l = y * w + left;
    let r = y * w + right;
    let u = up * w + x;
    let d = down * w + x;
    let covered = coverage[m] > 0.0 && coverage[l] > 0.0 && coverage[r] > 0.0 && coverage[u] > 0.0 && coverage[d] > 0.0;
    if (!covered) {
        energy[m] = 0.0;
        return;
    }
    let v = luma[m];
    if (p.mode == 0u) {
        let across = luma[l] + luma[r] - 4.0 * v;
        let lap = across + (luma[u] + luma[d]);
        energy[m] = lap * lap;
    } else {
        let ml = abs(2.0 * v - luma[l] - luma[r]) + abs(2.0 * v - luma[u] - luma[d]);
        energy[m] = ml * ml;
    }
}
