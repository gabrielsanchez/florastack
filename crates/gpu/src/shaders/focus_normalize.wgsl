// The depth map's normalized convolution (florastack-stack's `FocusKernel::measure`): the blurred luminance divided
// by the blurred coverage where the blur reached pixels the frame doesn't cover (blurred coverage below FULL_WEIGHT),
// 0 where it reached none. Where every pixel is covered the blurred coverage is 1 but for its rounding and nothing
// changes, which is what the CPU does by skipping this step for patches without uncovered pixels.

struct Params {
    count: u32,
    pad0: u32,
    pad1: u32,
    pad2: u32,
}

@group(0) @binding(0) var<uniform> p: Params;
@group(0) @binding(1) var<storage, read_write> luma: array<f32>;
@group(0) @binding(2) var<storage, read> weight: array<f32>;

const FULL_WEIGHT: f32 = 0.9999;

@compute @workgroup_size(256, 1, 1)
fn main(@builtin(global_invocation_id) id: vec3<u32>, @builtin(num_workgroups) groups: vec3<u32>) {
    let i = flat(id, groups);
    if (i >= p.count) {
        return;
    }
    let k = weight[i];
    if (k < FULL_WEIGHT) {
        luma[i] = select(0.0, luma[i] / k, k > 1e-6);
    }
}
