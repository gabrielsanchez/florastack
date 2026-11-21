// The end of the depth map's filling (florastack-stack's `depth::fill`): the depth held within the frames (0 where no
// frame covers, or where it isn't finite), and each pixel's blend width, 1 where the depth kept its smoothed value
// whole and up to `fill_blend` as far as it was filled; `wide` counts, a workgroup at a time, the pixels wider than 1.

struct Params {
    count: u32,
    last: f32,
    fill_blend: f32,
    has_coverage: u32,
}

@group(0) @binding(0) var<uniform> p: Params;
@group(0) @binding(1) var<storage, read> base: array<f32>;
@group(0) @binding(2) var<storage, read> coverage: array<f32>;
@group(0) @binding(3) var<storage, read_write> depth: array<f32>;
@group(0) @binding(4) var<storage, read_write> blend: array<f32>;
@group(0) @binding(5) var<storage, read_write> wide: array<atomic<u32>>;

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
        let covers = p.has_coverage == 0u || coverage[i] > 0.0;
        let k = base[2u * i + 1u];
        var filled = 0.0;
        if (covers && finite(k)) {
            filled = 1.0 - clamp(k, 0.0, 1.0);
        }
        let b = 1.0 + (p.fill_blend - 1.0) * filled;
        blend[i] = b;
        if (b > 1.0) {
            atomicAdd(&tally, 1u);
        }
        let d = depth[i];
        depth[i] = select(0.0, clamp(d, 0.0, p.last), covers && finite(d));
    }
    workgroupBarrier();
    if (local == 0u) {
        atomicAdd(&wide[0], atomicLoad(&tally));
    }
}
