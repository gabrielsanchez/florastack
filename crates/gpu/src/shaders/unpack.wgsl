// Integer samples normalized to f32, sRGB-decoded when asked: florastack-core's `Image::to_linear_f32` of a `u8` or
// `u16` image (and `convert` when not decoding). The samples arrive packed in 32-bit words, little end first: four
// `u8` or two `u16` a word. The alpha channel (the second of two, the fourth of four) is only normalized.

struct Params {
    count: u32,
    channels: u32,
    // The alpha channel's index, or a value no channel has.
    alpha: u32,
    // 8 or 16.
    bits: u32,
    decode: u32,
    pad0: u32,
    pad1: u32,
    pad2: u32,
}

@group(0) @binding(0) var<uniform> p: Params;
@group(0) @binding(1) var<storage, read> words: array<u32>;
@group(0) @binding(2) var<storage, read_write> dst: array<f32>;

// The sRGB decoding curve (IEC 61966-2-1), mirrored for negative values (florastack-core's `srgb_to_linear`).
fn srgb_decode(v: f32) -> f32 {
    let a = abs(v);
    var r: f32;
    if (a <= 0.04045) {
        r = a / 12.92;
    } else {
        r = pow((a + 0.055) / 1.055, 2.4);
    }
    return select(r, -r, v < 0.0);
}

@compute @workgroup_size(256, 1, 1)
fn main(@builtin(global_invocation_id) id: vec3<u32>, @builtin(num_workgroups) groups: vec3<u32>) {
    let i = flat(id, groups);
    if (i >= p.count) {
        return;
    }
    var v: f32;
    if (p.bits == 8u) {
        let s = (words[i / 4u] >> ((i % 4u) * 8u)) & 0xffu;
        v = f32(s) / 255.0;
    } else {
        let s = (words[i / 2u] >> ((i % 2u) * 16u)) & 0xffffu;
        v = f32(s) / 65535.0;
    }
    if (p.decode != 0u && (i % p.channels) != p.alpha) {
        v = srgb_decode(v);
    }
    dst[i] = v;
}
