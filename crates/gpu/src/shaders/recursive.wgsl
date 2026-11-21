// One pass of the domain transform's recursive filter (florastack-stack's `depth::filter::recursive_rows` and
// `recursive_columns`), in place on a plane of one or two channels: forth and back along every row (axis 0) or column
// (axis 1), one invocation per line, each sample pulled toward its neighbor's by the feedback `exp(ln_a * distance)`,
// in the CPU's order. Each channel's recursion is its own.

struct Params {
    width: u32,
    height: u32,
    channels: u32,
    axis: u32,
    ln_a: f32,
    pad0: u32,
    pad1: u32,
    pad2: u32,
}

@group(0) @binding(0) var<uniform> p: Params;
@group(0) @binding(1) var<storage, read> distances: array<f32>;
@group(0) @binding(2) var<storage, read_write> f: array<f32>;

@compute @workgroup_size(256, 1, 1)
fn main(@builtin(global_invocation_id) id: vec3<u32>, @builtin(num_workgroups) groups: vec3<u32>) {
    let line = flat(id, groups);
    var lines = p.height;
    var n = p.width;
    var start = line * p.width;
    var step = 1u;
    if (p.axis != 0u) {
        lines = p.width;
        n = p.height;
        start = line;
        step = p.width;
    }
    if (line >= lines) {
        return;
    }
    let c = p.channels;
    for (var i = 1u; i < n; i++) {
        let at = start + i * step;
        let before = at - step;
        let v = exp(p.ln_a * distances[at]);
        for (var ch = 0u; ch < c; ch++) {
            let cur = f[at * c + ch];
            f[at * c + ch] = cur + v * (f[before * c + ch] - cur);
        }
    }
    var i = n;
    while (i > 1u) {
        i = i - 1u;
        // Sample i - 1 from sample i, by sample i's distance.
        let next = start + i * step;
        let at = next - step;
        let v = exp(p.ln_a * distances[next]);
        for (var ch = 0u; ch < c; ch++) {
            let cur = f[at * c + ch];
            f[at * c + ch] = cur + v * (f[next * c + ch] - cur);
        }
    }
}
