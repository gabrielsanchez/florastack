// The focus statistics of each pixel from its record (florastack-stack's `FocusKernel::result` and `covered`): the
// sharpest frame refined between frames by the Gaussian through its neighbors' focus (`interpolate`), the confidence
// (`confidence`) and the guide luminance; 0 where no frame covers. Writes which pixels are covered too.

struct Params {
    count: u32,
    pad0: u32,
    pad1: u32,
    pad2: u32,
}

@group(0) @binding(0) var<uniform> p: Params;
@group(0) @binding(1) var<storage, read> record: array<f32>;
@group(0) @binding(2) var<storage, read_write> stats: array<f32>;
@group(0) @binding(3) var<storage, read_write> covered: array<f32>;

const MIN_PROMINENCE: f32 = 0.5;
const TINY: f32 = 1e-30;

fn interpolate(best: f32, before: f32, peak: f32, after: f32) -> f32 {
    if (before < 0.0 || after < 0.0 || peak <= 0.0) {
        return best;
    }
    let lb = log(before + TINY);
    let lp = log(peak + TINY);
    let la = log(after + TINY);
    let curvature = lb - 2.0 * lp + la;
    if (!(curvature < 0.0)) {
        return best;
    }
    return best + clamp(0.5 * (lb - la) / curvature, -0.5, 0.5);
}

fn confidence(peak: f32, runner: f32, floor_: f32) -> f32 {
    if (!(peak > 0.0) || 1.0 - sqrt(max(floor_, 0.0) / peak) < MIN_PROMINENCE) {
        return 0.0;
    }
    return max(sqrt(peak) - sqrt(max(runner, 0.0)), 0.0);
}

@compute @workgroup_size(256, 1, 1)
fn main(@builtin(global_invocation_id) id: vec3<u32>, @builtin(num_workgroups) groups: vec3<u32>) {
    let i = flat(id, groups);
    if (i >= p.count) {
        return;
    }
    let r = i * 8u;
    let peak = record[r + 1u];
    if (peak >= 0.0) {
        stats[i * 3u] = interpolate(record[r], record[r + 2u], peak, record[r + 3u]);
        stats[i * 3u + 1u] = confidence(peak, record[r + 5u], record[r + 6u]);
        stats[i * 3u + 2u] = record[r + 7u];
        covered[i] = 1.0;
    } else {
        stats[i * 3u] = 0.0;
        stats[i * 3u + 1u] = 0.0;
        stats[i * 3u + 2u] = 0.0;
        covered[i] = 0.0;
    }
}
