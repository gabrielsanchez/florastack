//! The quality report's synthetic stacks, made with `florastack_core::testutil` and scored against their ground truth.
//! They are what the gate compares with the baseline, so a stack's name, size and parameters are part of the baseline:
//! change them only with it (`cargo xtask quality --update-baseline`).
//!
//! The scenes are at a camera's scale (`scene::layered` with a scale of 3, or blur three times the generator's own), so
//! that every method runs at its defaults, as on camera frames. Frames are generated `MARGIN` pixels larger on every
//! side and cropped, as the engine's tests do, so that no frame shows the generator's clamped scene edge.

use florastack_core::testutil::{Backdrop, GroundTruth, StackParams, generate, generate_layered, scene};
use florastack_core::{Image, Rect};

/// The margin, in pixels, frames are generated with on every side and cropped of.
pub const MARGIN: usize = 8;

/// The synthetic stacks, by name, with what each is.
pub const STACKS: &[(&str, &str)] = &[
    ("layered-clean", "Bristles over discs over a textured backdrop at a camera's scale: 480x360, 12 frames in register, no noise"),
    ("layered-noisy", "The layered scene with sensor noise (read 0.005, shot 0.01)"),
    ("layered-smooth-noisy", "The layered scene over a smooth backdrop, with sensor noise: tonal smoothness, noise and color shift"),
    ("textured-moved", "A textured scene, 480x360, 12 frames that breathe, shift, rotate and change in brightness, with noise and dust"),
    ("layered-deep", "A deep stack: 48 frames of a layered scene, 360x270, each sharp over a few frames, lightly noisy"),
];

/// A synthetic stack: its frames, cropped by `MARGIN`, and its truth, in the coordinates of the uncropped scene.
pub struct Synthetic {
    pub frames: Vec<Image<f32>>,
    pub truth: GroundTruth,
}

/// The stack called `name`.
pub fn make(name: &str) -> Result<Synthetic, String> {
    let made = match name {
        "layered-clean" => layered(480, 360, Backdrop::Textured, 12, 0.0, 0.0, 1),
        "layered-noisy" => layered(480, 360, Backdrop::Textured, 12, 0.005, 0.01, 2),
        "layered-smooth-noisy" => layered(480, 360, Backdrop::SmoothRight, 12, 0.005, 0.01, 3),
        "textured-moved" => {
            let (w, h) = (480 + 2 * MARGIN, 360 + 2 * MARGIN);
            let (sharp, depth) = scene::textured(w, h, 4).map_err(|e| e.to_string())?;
            // The textured scene's detail is the generator's own size: three times its blur makes it a camera's.
            let params = StackParams { blur_per_depth: 24.0, max_blur: 24.0, blur_step: 1.5, ..StackParams::noisy_dusty(12, w, h, 4) };
            generate(&sharp, &depth, &params).map_err(|e| e.to_string())
        }
        "layered-deep" => {
            // A quarter of a pixel more blur, at the scene's own scale, from frame to frame.
            let frames = 48;
            let per_frame = 1.0 / frames as f32;
            let params = StackParams {
                focus_range: Some((0.0, 1.0)),
                blur_per_depth: 0.75 / per_frame,
                max_blur: 24.0,
                blur_step: 1.5,
                read_noise: 0.003,
                shot_noise: 0.005,
                seed: 5,
                ..StackParams::identity(frames)
            };
            let scene = scene::layered(360 + 2 * MARGIN, 270 + 2 * MARGIN, 3.0, Backdrop::Textured, 5).map_err(|e| e.to_string())?;
            generate_layered(&scene, &params).map_err(|e| e.to_string())
        }
        other => return Err(format!("no synthetic stack is called `{other}`")),
    }?;
    let (width, height) = (made.truth.sharp.width() - 2 * MARGIN, made.truth.sharp.height() - 2 * MARGIN);
    let inner = Rect::new(MARGIN, MARGIN, width, height);
    let frames = made.frames.iter().map(|f| f.crop(inner)).collect::<Result<Vec<_>, _>>().map_err(|e| e.to_string())?;
    Ok(Synthetic { frames, truth: made.truth })
}

/// A layered scene of `width` x `height` at a camera's scale, focused from front to back over `frames` frames that
/// don't move, as florastack-stack's quality bench makes them.
fn layered(
    width: usize,
    height: usize,
    backdrop: Backdrop,
    frames: usize,
    read_noise: f32,
    shot_noise: f32,
    seed: u64,
) -> Result<florastack_core::testutil::SyntheticStack, String> {
    const SCALE: f32 = 3.0;
    let scene = scene::layered(width + 2 * MARGIN, height + 2 * MARGIN, f64::from(SCALE), backdrop, seed).map_err(|e| e.to_string())?;
    let params = StackParams {
        focus_range: Some((0.0, 1.0)),
        blur_per_depth: 8.0 * SCALE,
        max_blur: 8.0 * SCALE,
        blur_step: 0.5 * SCALE,
        read_noise,
        shot_noise,
        seed,
        ..StackParams::identity(frames)
    };
    generate_layered(&scene, &params).map_err(|e| e.to_string())
}
