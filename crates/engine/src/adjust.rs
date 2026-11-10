//! Adjusting an output: tone, color and detail for the whole image and within masks (florastack-edit), applied after
//! its retouch strokes and before its annotations.
//!
//! - **Stored** with the output (`Output::adjustments`, florastack-edit's `Adjustments` as data), changed only by the
//!   `adjust.*` commands, one undo step each (`Edit::SetAdjustments`); `None` and neutral adjustments leave the output
//!   as it is.
//! - **Written**: `output.save` and `export.run` write an output with its adjustments (`commands::output::Pixels`),
//!   rendered at full size from its retouched result. The reproducibility sidecar keeps the output's record, so
//!   `export.reproduce` renders them again.
//! - **Shown**: the adjust session ([`AdjustSession`], `adjust.open` to `adjust.close`) holds one output's renderer,
//!   made once from its retouched result, so that `adjust.preview` renders a view's size in a fraction of a second,
//!   reusing what the last render left alone. Apps draw its last preview through [`AdjustView`] and learn what changed
//!   with [`crate::Engine::take_adjust_changes`].
//!
//! The subject and background masks find the subject in the output itself (florastack-edit's `subject`). The depth
//! masks need the stack's depth map ([`depth`]): a depth-map output's own (its `depth` map), or, for the other methods,
//! the frames' focus measured at a quarter of their size and kept as the output's `focus-depth` map
//! (`adjust.mapDepth`; an export that needs it and finds none measures it for itself).
//!
//! The renderer works in linear Rec. 2020 primaries: an output is converted from its own primaries (its ICC profile's,
//! or sRGB's without one) and back, keeping its color-space tag, so that an adjusted output is written as the output
//! is.

use std::sync::Arc;

use florastack_codecs::{ColorEngine, LightCraftColor, NamedSpace, Profile, StandardCurve};
use florastack_core::{AnyImage, ColorSpace, Image};
use florastack_edit::{Adjustments, Renderer, Request};
use florastack_project::{Output, OutputId};

use crate::error::CommandError;
use crate::view::DisplayImage;

pub use florastack_edit::Adjustments as Settings;
pub use florastack_edit::Click;
pub use florastack_edit::settings::{MAX_CLICKS, MAX_SAMPLES};
/// What the apps edit: florastack-edit's settings.
pub use florastack_edit::{Component, Global, Local, Mask, Op, Selection, Shape, Stroke};

/// An output's adjustments, the defaults when it has none; refused when they don't read or aren't valid.
pub(crate) fn adjustments(output: &Output) -> Result<Adjustments, CommandError> {
    let Some(value) = &output.adjustments else { return Ok(Adjustments::default()) };
    let adjustments: Adjustments = florastack_project::from_data("adjustments", value)?;
    adjustments.validate()?;
    Ok(adjustments)
}

/// The adjustments an output is written with: `None` when they change nothing.
pub(crate) fn to_apply(output: &Output) -> Result<Option<Adjustments>, CommandError> {
    let adjustments = adjustments(output)?;
    Ok((!adjustments.is_neutral()).then_some(adjustments))
}

/// What converts an output's linear samples to the renderer's primaries and back.
pub(crate) struct Working {
    own: Profile,
    rec2020: Profile,
    color: ColorSpace,
}

impl Working {
    /// For `image`, linear light in the primaries its profile names (sRGB's without one).
    pub(crate) fn of(image: &Image<f32>) -> Result<Working, CommandError> {
        let engine = LightCraftColor;
        let own = match &image.color_space().icc {
            Some(icc) => engine.parse(icc)?,
            None => engine.standard(NamedSpace::Srgb, StandardCurve::Linear)?,
        };
        Ok(Working { own, rec2020: engine.standard(NamedSpace::Rec2020, StandardCurve::Linear)?, color: image.color_space().clone() })
    }

    /// `image` in Rec. 2020 primaries.
    pub(crate) fn to_rec2020(&self, image: &mut Image<f32>) -> Result<(), CommandError> {
        LightCraftColor.convert(image, &self.own, &self.rec2020)?;
        Ok(())
    }

    /// `image`, in Rec. 2020 primaries, back in the output's, with its color-space tag.
    pub(crate) fn back(&self, image: &mut Image<f32>) -> Result<(), CommandError> {
        LightCraftColor.convert(image, &self.rec2020, &self.own)?;
        image.set_color_space(self.color.clone());
        Ok(())
    }
}

/// A renderer for an output's (retouched) result, with what converts its renders back.
pub(crate) fn renderer(result: AnyImage) -> Result<(Renderer, Working), CommandError> {
    let mut image = crate::commands::scientific::linear(result)?;
    let working = Working::of(&image)?;
    working.to_rec2020(&mut image)?;
    let renderer = Renderer::new(&image)?;
    Ok((renderer, working))
}

/// `result` with `adjustments` applied at full size, as files are written, with the stack's `depth` map when its masks
/// need it.
pub(crate) fn apply(result: AnyImage, adjustments: &Adjustments, depth: Option<&Image<f32>>) -> Result<AnyImage, CommandError> {
    let (renderer, working) = renderer(result)?;
    let renderer = match depth {
        Some(depth) => renderer.with_depth(depth)?,
        None => renderer,
    };
    let mut image = renderer.render(adjustments, &Request::FULL)?;
    working.back(&mut image)?;
    Ok(AnyImage::F32(image))
}

/// The output map that keeps the stack's depth map of an output that isn't a depth map.
pub(crate) const FOCUS_DEPTH: &str = "focus-depth";

/// A depth map's 16-bit samples (0 for the first frame to 65535 for the last) as fractions, for the renderer.
pub(crate) fn depth(map: &Image<u16>) -> Result<Image<f32>, CommandError> {
    let data = map.rows().flat_map(|row| row.iter().map(|&v| f32::from(v) / 65535.0)).collect();
    Ok(Image::from_vec(map.width(), map.height(), 1, ColorSpace::LINEAR, data).map_err(florastack_stack::StackError::from)?)
}

/// See the module documentation.
pub(crate) struct AdjustSession {
    pub output: OutputId,
    pub renderer: Arc<Renderer>,
    pub working: Arc<Working>,
    /// What the renderer was made from: the output's image and its visible strokes (`crate::retouch::render_key`).
    pub base: String,
    /// The last preview rendered, and the adjustments it shows; and the output without them, at the same size, when
    /// asked for.
    pub preview: Option<Arc<DisplayImage>>,
    pub before: Option<Arc<DisplayImage>>,
    pub shown: Option<Adjustments>,
    pub changes: AdjustChanges,
}

/// What changed in the adjust session since an app last asked ([`crate::Engine::take_adjust_changes`]).
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct AdjustChanges {
    /// Whether a new preview was rendered.
    pub preview: bool,
    /// Whether an output was opened (everything is new) or the session closed.
    pub opened: bool,
    pub closed: bool,
}

/// The adjust session, as an app draws it.
pub struct AdjustView<'a> {
    pub(crate) session: &'a AdjustSession,
}

impl AdjustView<'_> {
    pub fn output(&self) -> OutputId {
        self.session.output
    }

    /// The output's size, in pixels.
    pub fn size(&self) -> (usize, usize) {
        (self.session.renderer.width(), self.session.renderer.height())
    }

    /// The last preview rendered (`adjust.preview`), linear light in sRGB's primaries.
    pub fn preview(&self) -> Option<&DisplayImage> {
        self.session.preview.as_deref()
    }

    /// The output without its adjustments, at the last preview's size, when `adjust.preview` was asked for it.
    pub fn before(&self) -> Option<&DisplayImage> {
        self.session.before.as_deref()
    }

    /// The previews, shared: the last one, and the output without its adjustments.
    pub fn previews(&self) -> (Option<Arc<DisplayImage>>, Option<Arc<DisplayImage>>) {
        (self.session.preview.clone(), self.session.before.clone())
    }

    /// Whether the session has the stack's depth map, which the depth masks need.
    pub fn has_depth(&self) -> bool {
        self.session.renderer.has_depth()
    }
}

#[cfg(test)]
mod tests {
    use florastack_core::Encoding;

    use super::*;

    /// An image in Adobe RGB's primaries goes to Rec. 2020 and back unchanged, with its tag.
    #[test]
    fn primaries_go_to_rec2020_and_back() {
        let profile = LightCraftColor.standard(NamedSpace::AdobeRgb, StandardCurve::Linear).unwrap();
        let color = ColorSpace { encoding: Encoding::Linear, icc: Some(profile.bytes().clone()) };
        let data: Vec<f32> = (0..48).map(|i| (i % 7) as f32 / 7.0).collect();
        let original = Image::from_vec(4, 4, 3, color.clone(), data).unwrap();
        let working = Working::of(&original).unwrap();
        let mut image = original.clone();
        working.to_rec2020(&mut image).unwrap();
        assert!(image.as_raw().iter().zip(original.as_raw()).any(|(a, b)| (a - b).abs() > 1e-3), "converted");
        working.back(&mut image).unwrap();
        assert_eq!(image.color_space(), &color);
        let d = image.as_raw().iter().zip(original.as_raw()).map(|(a, b)| (a - b).abs()).fold(0.0, f32::max);
        assert!(d < 1e-5, "{d}");
        // Without a profile, sRGB's primaries.
        let srgb = Image::from_vec(2, 2, 3, ColorSpace::LINEAR, vec![0.5; 12]).unwrap();
        let mut image = srgb.clone();
        let working = Working::of(&srgb).unwrap();
        working.to_rec2020(&mut image).unwrap();
        working.back(&mut image).unwrap();
        assert!(image.as_raw().iter().all(|v| (v - 0.5).abs() < 1e-5));
    }
}
