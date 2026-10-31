// Helpers and constants every kernel of florastack-gpu shares: prepended to each kernel's source (shaders.rs).
//
// Planes are arrays of f32, row-major, channels interleaved: sample c of pixel (x, y) of a plane w pixels wide with
// C channels is at (y * w + x) * C + c. Each kernel computes one output sample (or pixel) per invocation, summing its
// taps in the order its CPU reference does, so results depend on nothing but the inputs.

// Whether `v` is finite, read from its bits: shader compilers may assume there is no NaN or infinity, and fold away
// arithmetic tests such as `v != v`.
fn finite(v: f32) -> bool {
    return (bitcast<u32>(v) & 0x7f800000u) != 0x7f800000u;
}

// Sample `i` of a level of `n` samples, mirrored about its edge samples (florastack-stack's `levels::mirror`).
fn mirror(i: i32, n: i32) -> i32 {
    if (n <= 1) {
        return 0;
    }
    let period = 2 * (n - 1);
    var m = i % period;
    if (m < 0) {
        m = m + period;
    }
    if (m >= n) {
        m = period - m;
    }
    return m;
}

// Sample `g` of a level of `full` samples as an index into a span of it from `start`, `len` samples long: mirrored
// at the level's edges, then clamped to the span (florastack-stack's `levels::local`).
fn local_index(g: i32, full: i32, start: i32, len: i32) -> i32 {
    return clamp(mirror(g, full) - start, 0, len - 1);
}

// `a / 2` rounded down, for negative `a` too.
fn half_floor(a: i32) -> i32 {
    let q = a / 2;
    if (a < 0 && (a % 2) != 0) {
        return q - 1;
    }
    return q;
}

// The index of a one-dimensional invocation in a dispatch of `groups` workgroups of 256 (two-dimensional when one
// dimension can't hold them all).
fn flat(id: vec3<u32>, groups: vec3<u32>) -> u32 {
    return id.y * groups.x * 256u + id.x;
}

// The stacking constants of florastack-stack (`average.rs`), which its tests check against these.
const MIN_COVERAGE: f32 = 1e-3;
const MAX_SAMPLE: f32 = 1e6;
const ENERGY_SCALE: f32 = 1e-4;
const ENERGY_FLOOR: f32 = 1e-8;
const ENERGY_CAP: f32 = 1e6;
// The Rec. 709 luminance weights.
const LUMA_R: f32 = 0.2126;
const LUMA_G: f32 = 0.7152;
const LUMA_B: f32 = 0.0722;
// A finite stand-in for infinity, for the same reason as `finite`.
const BIG: f32 = 3.0e38;

// The pyramid's generating kernel [1, 4, 6, 4, 1] / 16 (Burt and Adelson's a = 0.375), tap m from 0 to 4: exact in f32.
fn binomial(m: i32) -> f32 {
    var w = 0.0;
    switch (m) {
        case 0, 4: {
            w = 0.0625;
        }
        case 1, 3: {
            w = 0.25;
        }
        case 2: {
            w = 0.375;
        }
        default: {}
    }
    return w;
}
