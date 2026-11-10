//! AI selections: Meta's SAM 3 model ([`florastack_sam`]) run on an output, for the [`Shape::Object`] and
//! [`Shape::Described`] shapes.
//!
//! The model sees the output as it was stacked and retouched, without its adjustments, at the model's input size
//! ([`florastack_sam::INPUT`] pixels on its long side, sRGB-encoded as the model was trained on photos): the
//! selection then stays the same whatever the adjustments do. Its expensive part runs once per image
//! ([`Segmenter::select`] encodes the image the first time it is asked about it); each click or description then takes
//! about a second. What it selects is kept with the mask as a [`Selection`], which rendering reads without the model.
//!
//! The weights aren't FloraStack's: they are the `facebook/sam3` checkpoint, under Meta's SAM License, which the user
//! downloads ([`check_model`] checks a folder of them).
//!
//! [`Shape::Object`]: crate::Shape::Object
//! [`Shape::Described`]: crate::Shape::Described

use std::path::Path;

pub use florastack_sam::{INPUT, TOKENIZER_FILES, WEIGHTS_FILE, WEIGHTS_SHA256, WEIGHTS_SIZE};

use crate::error::EditError;
use crate::render::Renderer;
use crate::settings::{Click, Selection};

/// How sure the model must be that an instance is what a description names, from 0 to 1 (SAM 3's reference default).
const DESCRIPTION_THRESHOLD: f32 = 0.5;

fn model_error(e: florastack_sam::Error) -> EditError {
    match e {
        florastack_sam::Error::Missing(dir) => EditError::NoModel { folder: dir.display().to_string() },
        other => EditError::Model { reason: other.to_string() },
    }
}

/// Checks a folder of the model's files without reading the weights: each file is there, the weights have the
/// official checkpoint's size. Their hash ([`WEIGHTS_SHA256`]) is the caller's to check, once.
pub fn check_model(dir: &Path) -> Result<(), EditError> {
    florastack_sam::check_model_dir(dir).map_err(model_error)
}

/// What a selection is made from.
#[derive(Clone, Copy, Debug)]
pub enum Prompt<'a> {
    /// Clicks on the object, and on what to leave out of it.
    Clicks(&'a [Click]),
    /// A description of what to select ("insect").
    Text(&'a str),
}

/// The model, loaded, with the last image it read.
pub struct Segmenter {
    sam: florastack_sam::Sam3,
    /// The renderer whose image the model read ([`Renderer::image_id`]), and what it made of it.
    encoded: Option<(u64, florastack_sam::Encoded)>,
}

impl std::fmt::Debug for Segmenter {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Segmenter").field("encoded", &self.encoded.as_ref().map(|(id, _)| *id)).finish_non_exhaustive()
    }
}

impl Segmenter {
    /// Loads the model from `dir` (several gigabytes of memory, and seconds to minutes on a CPU).
    pub fn load(dir: &Path) -> Result<Segmenter, EditError> {
        check_model(dir)?;
        Ok(Segmenter { sam: florastack_sam::Sam3::load(dir).map_err(model_error)?, encoded: None })
    }

    /// Whether the model has read `renderer`'s image already, so that a selection takes only a second.
    pub fn has_read(&self, renderer: &Renderer) -> bool {
        self.encoded.as_ref().is_some_and(|(id, _)| *id == renderer.image_id())
    }

    /// Lets the model read `renderer`'s image, unless it has: the slow part of a first selection.
    pub fn read(&mut self, renderer: &Renderer) -> Result<(), EditError> {
        if self.has_read(renderer) {
            return Ok(());
        }
        self.encoded = None;
        let (rgb, w, h) = renderer.rgb8(INPUT)?;
        let encoded = self.sam.encode(&rgb, w, h).map_err(model_error)?;
        self.encoded = Some((renderer.image_id(), encoded));
        Ok(())
    }

    /// What the model selects in `renderer`'s image for `prompt`: `None` when a description names nothing there.
    pub fn select(&mut self, renderer: &Renderer, prompt: Prompt<'_>) -> Result<Option<Selection>, EditError> {
        self.read(renderer)?;
        let Some((_, encoded)) = self.encoded.as_mut() else {
            return Err(EditError::Model { reason: "the image wasn't read".to_owned() });
        };
        let prediction = match prompt {
            Prompt::Clicks(clicks) => {
                if !clicks.iter().any(|c| c.include) {
                    return Err(EditError::invalid("an AI selection needs a click on what to select"));
                }
                #[allow(clippy::cast_possible_truncation)] // Fractions of the image, checked finite and small.
                let clicks: Vec<florastack_sam::Click> =
                    clicks.iter().map(|c| florastack_sam::Click { x: c.at[0] as f32, y: c.at[1] as f32, positive: c.include }).collect();
                Some(self.sam.segment_clicks(encoded, &clicks).map_err(model_error)?)
            }
            Prompt::Text(text) => self.sam.segment_text(encoded, text, DESCRIPTION_THRESHOLD).map_err(model_error)?,
        };
        Ok(prediction.map(|p| Selection::from_logits(florastack_sam::MASK_SIDE, &p.logits)))
    }
}
