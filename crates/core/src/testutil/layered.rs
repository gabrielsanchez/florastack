//! Synthetic stacks of a scene whose parts occlude one another: [`generate_layered`] turns a [`LayeredScene`] (a
//! background with a depth map, and flat-colored layers at fixed depths in front of it) into a focus stack whose
//! out-of-focus edges spread over what lies behind them, as a lens's do.
//!
//! [`generate`](super::synth::generate) blurs each pixel with its own depth's blur, by gathering: a defocused bristle
//! in front of a sharp background then never veils the background beside it, so its stacks have none of the veils and
//! halos that real subjects leave around their edges, and stacking methods can't be scored on them. Here each frame is
//! made as layers of a thin-lens scene are imaged: the background is defocused as `generate` does it (it occludes
//! nothing), then every layer, far to near, is composited over it with its alpha blurred by its own defocus,
//! `frame = color * blur(alpha) + (1 - blur(alpha)) * frame`: the "over" operator of T. Porter and T. Duff
//! ("Compositing digital images", SIGGRAPH 1984) on blurred layers, the layered model of an occluding edge's defocus
//! that N. Asada, H. Fujiwara and T. Matsuyama analyze ("Seeing behind the scene: analysis of photometric properties
//! of occluding edges by the reversed projection blurring model", IEEE Transactions on Pattern Analysis and Machine
//! Intelligence 20(2), 1998). The truth is the same composite at zero blur, and its depth map the depth of the nearest
//! layer covering at least half of each pixel.
//!
//! The steps after defocus (geometry, gain, dust and noise) are `generate`'s, with the same [`StackParams`]. A layer's
//! blur is [`gaussian_blur`]'s (box passes beyond 4 pixels), which costs the same at any radius: a stack has a blur per
//! layer and frame, and the exact kernel would take seconds.

use super::synth::{
    FrameTruth, GroundTruth, StackParams, SynthError, SyntheticStack, blur_levels, checked_depths, defocus, depth_range, draw_truths, finish_frame,
    packed,
};
use crate::image::Image;
use crate::ops::gaussian_blur;

/// A flat-colored layer of a [`LayeredScene`] at one depth.
#[derive(Clone, Debug, PartialEq)]
pub struct Layer {
    /// Its color, one value per channel of the scene's background.
    pub color: Vec<f32>,
    /// How much of each pixel it covers, from 0 to 1: one channel, the background's size.
    pub alpha: Image<f32>,
    pub depth: f32,
}

/// A background and the layers in front of it, far to near. See the module documentation.
#[derive(Clone, Debug, PartialEq)]
pub struct LayeredScene {
    /// The sharp background, linear, any number of channels.
    pub background: Image<f32>,
    /// The background's depth, one channel, its size.
    pub background_depth: Image<f32>,
    /// The layers, farthest first.
    pub layers: Vec<Layer>,
}

impl LayeredScene {
    /// Checks the layers against the background: each has a one-channel alpha of its size, a color per channel and a
    /// finite depth.
    fn check(&self) -> Result<(), SynthError> {
        let (w, h, c) = (self.background.width(), self.background.height(), self.background.channels());
        for (index, layer) in self.layers.iter().enumerate() {
            let a = &layer.alpha;
            if (a.width(), a.height(), a.channels()) != (w, h, 1) || layer.color.len() != c || !layer.depth.is_finite() {
                return Err(SynthError::Layer { index });
            }
        }
        Ok(())
    }

    /// The scene in focus everywhere: every layer composited over the background unblurred.
    pub fn sharp(&self) -> Result<Image<f32>, SynthError> {
        self.check()?;
        Ok(composite(packed(&self.background)?, &self.layers, |_| 0.0)?)
    }

    /// The depth of the nearest layer that covers at least half of each pixel, or the background's.
    pub fn depth(&self) -> Result<Image<f32>, SynthError> {
        self.check()?;
        let mut depth = packed(&self.background_depth)?;
        for layer in &self.layers {
            for (row, alpha) in depth.rows_mut().zip(layer.alpha.rows()) {
                for (d, &a) in row.iter_mut().zip(alpha) {
                    if a >= 0.5 {
                        *d = layer.depth;
                    }
                }
            }
        }
        Ok(depth)
    }
}

/// `layers` composited far to near over `base`, each layer's alpha blurred by `sigma(depth)` pixels first.
fn composite(mut base: Image<f32>, layers: &[Layer], sigma: impl Fn(f32) -> f64) -> Result<Image<f32>, crate::ops::OpError> {
    let c = base.channels();
    for layer in layers {
        let s = sigma(layer.depth);
        let blurred;
        let alpha = if s > 0.0 {
            blurred = gaussian_blur(&layer.alpha, s)?;
            &blurred
        } else {
            &layer.alpha
        };
        for (row, alpha) in base.rows_mut().zip(alpha.rows()) {
            for (pixel, &a) in row.chunks_exact_mut(c).zip(alpha) {
                for (v, &color) in pixel.iter_mut().zip(&layer.color) {
                    *v = color * a + (1.0 - a) * *v;
                }
            }
        }
    }
    Ok(base)
}

/// Makes a focus stack of `scene`. Without a focus range in `params`, the frames are focused from the scene's nearest
/// depth (background or layer) to its farthest. See the module documentation.
pub fn generate_layered(scene: &LayeredScene, params: &StackParams) -> Result<SyntheticStack, SynthError> {
    params.validate()?;
    scene.check()?;
    let (w, h) = (scene.background.width(), scene.background.height());
    let depths = checked_depths(&scene.background_depth, w, h)?;
    let background_range = depth_range(&depths);
    let (lowest, highest) = scene.layers.iter().fold(background_range, |(lo, hi), layer| (lo.min(layer.depth), hi.max(layer.depth)));
    let (near, far) = params.focus_range.unwrap_or((lowest, highest));
    let truths: Vec<FrameTruth> = draw_truths(params, near, far, w, h);
    let base = packed(&scene.background)?;
    let levels = blur_levels(&base, &truths, background_range, params)?;
    let mut frames = Vec::with_capacity(params.frames);
    for (i, truth) in truths.iter().enumerate() {
        let background = if levels.len() == 1 { base.clone() } else { defocus(&levels, &depths, truth.focus_depth, params)? };
        let focus = truth.focus_depth;
        let frame = composite(background, &scene.layers, |d| f64::from((params.blur_per_depth * (d - focus).abs()).min(params.max_blur)))?;
        frames.push(finish_frame(frame, truth, params, i)?);
    }
    let truth = GroundTruth { sharp: scene.sharp()?, depth: scene.depth()?, frames: truths, dust: params.dust.clone() };
    Ok(SyntheticStack { frames, truth })
}

#[cfg(test)]
mod tests {
    use super::super::scene::{Backdrop, layered};
    use super::*;
    use crate::image::ColorSpace;

    #[test]
    fn without_blur_every_frame_is_the_sharp_composite() {
        let scene = layered(96, 72, 1.0, Backdrop::Textured, 3).unwrap();
        let stack = generate_layered(&scene, &StackParams::identity(4)).unwrap();
        let sharp = scene.sharp().unwrap();
        for frame in &stack.frames {
            assert_eq!(frame, &sharp);
        }
        assert_eq!(stack.truth.sharp, sharp);
        assert!(sharp.as_raw().iter().all(|v| (0.0..=1.0).contains(v)));
        // The depth map is the nearest layer's where one covers the pixel.
        let depth = scene.depth().unwrap();
        let nearest = scene.layers.last().unwrap();
        let covered = nearest.alpha.as_raw().iter().position(|&a| a >= 0.5).unwrap();
        assert_eq!(depth.as_raw()[covered], nearest.depth);
    }

    #[test]
    fn an_out_of_focus_layer_veils_the_background_beside_it() {
        // A bright bar in front of a dark, flat background: the frame focused on the background shows the bar's blur
        // spread over the background next to it, which the gathering generator never does.
        let background = Image::from_fn(64, 32, 1, ColorSpace::LINEAR, |_, _, _| 0.1).unwrap();
        let background_depth = Image::from_fn(64, 32, 1, ColorSpace::LINEAR, |_, _, _| 1.0).unwrap();
        let alpha = Image::from_fn(64, 32, 1, ColorSpace::LINEAR, |x, _, _| if (30..34).contains(&x) { 1.0 } else { 0.0 }).unwrap();
        let scene = LayeredScene { background, background_depth, layers: vec![Layer { color: vec![0.9], alpha, depth: 0.0 }] };
        let params = StackParams { focus_range: Some((0.0, 1.0)), blur_per_depth: 4.0, max_blur: 4.0, ..StackParams::identity(2) };
        let stack = generate_layered(&scene, &params).unwrap();
        let (near, far) = (&stack.frames[0], &stack.frames[1]);
        assert!((near.get(36, 16, 0).unwrap() - 0.1).abs() < 1e-6, "focused on the bar: the background beside it is untouched");
        assert!(far.get(36, 16, 0).unwrap() > 0.2, "focused on the background: the bar's blur veils it: {:?}", far.get(36, 16, 0));
        assert!(far.get(32, 16, 0).unwrap() < 0.9, "and the bar itself is dimmed");
        assert_eq!(stack.truth.depth.get(31, 3, 0), Some(0.0));
        assert_eq!(stack.truth.depth.get(10, 3, 0), Some(1.0));
    }

    #[test]
    fn layered_stacks_are_deterministic_and_checked() {
        let scene = layered(48, 40, 2.0, Backdrop::SmoothRight, 5).unwrap();
        let params = StackParams { read_noise: 0.01, focus_range: Some((0.0, 1.0)), ..StackParams::clean(3, 9) };
        assert_eq!(generate_layered(&scene, &params).unwrap(), generate_layered(&scene, &params).unwrap());
        assert_eq!(layered(48, 40, 2.0, Backdrop::SmoothRight, 5).unwrap(), scene);
        let mut bad = scene.clone();
        bad.layers[2].color.pop();
        assert_eq!(generate_layered(&bad, &params).unwrap_err(), SynthError::Layer { index: 2 });
        let mut bad = scene;
        bad.layers[0].depth = f32::NAN;
        assert_eq!(bad.sharp().unwrap_err(), SynthError::Layer { index: 0 });
    }
}
