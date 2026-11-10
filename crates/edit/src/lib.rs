//! Adjusting a stacked output: tone, color and detail for the whole image and within masks.
//!
//! - [`settings`]: what the user sets, as data the project stores: [`Adjustments`] with the whole image's
//!   [`Global`] adjustments and a list of [`Mask`]s, each [`Component`]s (whole image, linear and radial gradients,
//!   brush strokes, luminance and color ranges, and the stack's own subject, background and depth ranges) with
//!   [`Local`] adjustments.
//! - [`render`]: a [`Renderer`] holds one image and renders any adjustments at full size or reduced for a view.
//! - [`subject`]: the sharp areas of a stacked image, where it has fine detail, for the sharp, soft and depth masks.
//! - `segment` (not in the browser build): AI selections, Meta's SAM 3 model run on an output from clicks or a
//!   description, kept with their masks so that rendering needs no model.
//!
//! The rendering is LightCraft's develop pipeline (book/src/dev/lightcraft.md), whose types stay inside this crate.
//! Images go in and come out as linear light in Rec. 2020 primaries; the engine converts outputs from and to their
//! own primaries.

pub mod error;
pub mod render;
#[cfg(not(target_arch = "wasm32"))]
pub mod segment;
pub mod settings;
pub mod subject;

pub use error::EditError;
pub use render::{Renderer, Request, Size};
pub use settings::{Adjustments, Click, Component, Global, Local, Mask, Op, Selection, Shape, Stroke};
