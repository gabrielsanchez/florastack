// The support test of the depth map's resolve (florastack-stack's `depth::fill`): a confident pixel (weight above 0)
// whose neighborhood's smoothed weight is below `min_support` is left out. Counts the pixels that stay confident into
// `confident`, a workgroup at a time.

struct Params {
    count: u32,
    min_support: f32,
    pad0: u32,
    pad1: u32,
}

@group(0) @binding(0) var<uniform> p: Params;
@group(0) @binding(1) var<storage, read> support: array<f32>;
@group(0) @binding(2) var<storage, read_write> weights: array<f32>;
@group(0) @binding(3) var<storage, read_write> confident: array<atomic<u32>>;

var<workgroup> tally: atomic<u32>;

@compute @workgroup_size(256, 1, 1)
fn main(
    @builtin(global_invocation_id) id: vec3<u32>,
    @builtin(num_workgroups) groups: vec3<u32>,
    @builtin(local_invocation_index) local: u32,
) {
    if (local == 0u) {
        atomicStore(&tally, 0u);
    }
    workgroupBarrier();
    let i = flat(id, groups);
    if (i < p.count) {
        var k = weights[i];
        if (k > 0.0 && support[i] < p.min_support) {
            k = 0.0;
            weights[i] = 0.0;
        }
        if (k > 0.0) {
            atomicAdd(&tally, 1u);
        }
    }
    workgroupBarrier();
    if (local == 0u) {
        atomicAdd(&confident[0], atomicLoad(&tally));
    }
}
