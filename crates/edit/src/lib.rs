//! Adjusting a stacked output: tone, color and detail for the whole image and within masks.
//!
//! - [`settings`]: what the user sets, as data the project stores: [`Adjustments`] with the whole image's
//!   [`Global`] adjustments and a list of [`Mask`]s, each [`Component`]s (whole image, linear and radial gradients,
//!   brush strokes, luminance and color ranges, and the stack's own subject, background and depth ranges) with
//!   [`Local`] adjustments.
//! - [`render`]: a [`Renderer`] holds one image and renders any adjustments at full size or reduced for a view.
//!
//! The rendering is LightCraft's develop pipeline (book/src/dev/lightcraft.md), whose types stay inside this crate.
//! Images go in and come out as linear light in Rec. 2020 primaries; the engine converts outputs from and to their
//! own primaries.

pub mod error;
pub mod render;
pub mod settings;

pub use error::EditError;
pub use render::{Focus, Renderer, Request, Size};
pub use settings::{Adjustments, Component, Global, Local, Mask, Op, Shape, Stroke};
