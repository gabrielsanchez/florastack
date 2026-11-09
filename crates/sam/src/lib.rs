//! SAM 3 (Segment Anything with Concepts, Meta, 2025) inference in pure Rust on candle, for FloraStack's AI subject
//! selections (florastack-edit).
//!
//! - **Point prompts** ([`Sam3::segment_clicks`]): positive and negative clicks select one object, via SAM 3's
//!   interactive (SAM 2 style) prompt encoder and mask decoder.
//! - **Text prompts** ([`Sam3::segment_text`]): a noun phrase ("insect", "the red flower") selects every instance of
//!   that concept, via the CLIP text encoder and the DETR detector.
//!
//! The expensive part, the 32-layer ViT over the 1008 x 1008 input, runs once per image ([`Sam3::encode`]); each
//! click or prompt then reuses the [`Encoded`] features. On macOS the model runs on the GPU through Metal; elsewhere
//! on the CPU.
//!
//! The weights are not part of FloraStack: they are the `facebook/sam3` checkpoint (Meta's SAM License), which the
//! user downloads from Hugging Face and points FloraStack at ([`Sam3::load`], [`check_model_dir`]). Nothing requires
//! them: without them, AI selections report that the model isn't installed and everything else works.
//!
//! Adapted from LightCraft's `lightcraft-segment` crate (<https://github.com/storytold/lightcraft>, commit `a2a4138`,
//! `crates/segment/src/`), Apache-2.0, without its downloader and its remote worker, so that FloraStack has no HTTP or
//! TLS code. LightCraft's code is itself a modified work (Apache License 2.0, section 4(b)), ported by the LightCraft
//! contributors in 2026 from the Python/PyTorch SAM 3 code of Hugging Face Transformers
//! (`src/transformers/models/sam3` and `sam3_tracker`), Copyright The HuggingFace Team and Meta Platforms, Inc.;
//! translated to Rust on candle and restructured. This crate is therefore under the Apache License 2.0 only. See
//! NOTICE and ATTRIBUTION.md.
#![forbid(unsafe_code)]
// The port stays close to LightCraft's source, so that its fixes can be compared and carried over. These pedantic lints
// flag that source's style, not defects: model dimensions cast between float and integer types, the neck's exact scale
// factors (4, 2, 1) compared, and documentation without backticks. Input from files is checked where it is read: the
// weights' header and offsets (`weights`), the tokenizer's files (`tokenizer`).
#![allow(
    clippy::doc_markdown,
    clippy::needless_pass_by_value,
    clippy::cast_possible_truncation,
    clippy::cast_sign_loss,
    clippy::cast_precision_loss,
    clippy::cast_lossless,
    clippy::cast_possible_wrap,
    clippy::float_cmp,
    clippy::redundant_closure,
    clippy::redundant_closure_for_method_calls,
    clippy::needless_for_each
)]
#![deny(clippy::unwrap_used, clippy::expect_used, clippy::panic, clippy::unimplemented, clippy::todo, clippy::unreachable)]

#[cfg(not(target_arch = "wasm32"))]
mod clip;
#[cfg(not(target_arch = "wasm32"))]
mod detector;
pub mod mask;
#[cfg(not(target_arch = "wasm32"))]
mod neck;
#[cfg(not(target_arch = "wasm32"))]
mod nn;
pub mod tokenizer;
#[cfg(not(target_arch = "wasm32"))]
mod tracker;
#[cfg(not(target_arch = "wasm32"))]
mod vit;
#[cfg(not(target_arch = "wasm32"))]
mod weights;

use std::path::{Path, PathBuf};
#[cfg(not(target_arch = "wasm32"))]
use std::sync::Arc;

#[cfg(not(target_arch = "wasm32"))]
use candle_core::{DType, Device, Tensor};
#[cfg(not(target_arch = "wasm32"))]
use candle_nn::VarBuilder;

#[cfg(not(target_arch = "wasm32"))]
pub use tracker::Click;

#[derive(Debug, thiserror::Error)]
pub enum Error {
    #[error("SAM 3 model: {0}")]
    Model(String),
    #[error("SAM 3 model files not found in {0} (expected model.safetensors, vocab.json, merges.txt)")]
    Missing(PathBuf),
    #[cfg(not(target_arch = "wasm32"))]
    #[error(transparent)]
    Candle(#[from] candle_core::Error),
}

pub type Result<T> = std::result::Result<T, Error>;

/// Side of the square model input.
pub const INPUT: usize = 1008;
/// Side of the low-resolution masks the model predicts.
pub const MASK_SIDE: usize = 288;
#[cfg(not(target_arch = "wasm32"))]
const _: () = assert!(MASK_SIDE == tracker::LOW_RES && INPUT == vit::IMAGE);

/// The files of a model directory.
pub const WEIGHTS_FILE: &str = "model.safetensors";

/// The size of the official `model.safetensors` (3.44 GB), and its SHA-256 (Hugging Face's LFS hash of the
/// `facebook/sam3` checkpoint, as LightCraft pins it).
pub const WEIGHTS_SIZE: u64 = 3_439_938_512;
pub const WEIGHTS_SHA256: &str = "6d06f0a5f84e435071fe6603e61d0b4cc7b40e0d39d487cfd4d67d8cc11cc14a";
/// The tokenizer's files, and the most either may hold.
pub const TOKENIZER_FILES: [&str; 2] = ["vocab.json", "merges.txt"];
pub const TOKENIZER_MAX: u64 = 16 << 20;

/// Checks the files of a model folder without reading the weights: each is there, the weights have the official size
/// and the tokenizer's files a plausible one. The weights' hash is the caller's to check, once ([`WEIGHTS_SHA256`]).
pub fn check_model_dir(dir: &Path) -> Result<()> {
    let size = |name: &str| std::fs::metadata(dir.join(name)).ok().filter(std::fs::Metadata::is_file).map(|m| m.len());
    match size(WEIGHTS_FILE) {
        Some(WEIGHTS_SIZE) => {}
        Some(other) => {
            return Err(Error::Model(format!(
                "{} is {other} bytes, not the {WEIGHTS_SIZE} of the facebook/sam3 checkpoint: an incomplete download, or another model",
                dir.join(WEIGHTS_FILE).display()
            )));
        }
        None => return Err(Error::Missing(dir.to_path_buf())),
    }
    for name in TOKENIZER_FILES {
        match size(name) {
            Some(n) if n > 0 && n <= TOKENIZER_MAX => {}
            Some(n) => return Err(Error::Model(format!("{} is {n} bytes: not the tokenizer's file", dir.join(name).display()))),
            None => return Err(Error::Missing(dir.to_path_buf())),
        }
    }
    Ok(())
}

/// Whether `dir` holds a usable checkpoint.
pub fn is_model_dir(dir: &Path) -> bool {
    dir.join(WEIGHTS_FILE).is_file() && dir.join("vocab.json").is_file() && dir.join("merges.txt").is_file()
}

#[cfg(not(target_arch = "wasm32"))]
/// The best device here: Metal on macOS (when a GPU is available), else the CPU.
pub fn best_device() -> Device {
    #[cfg(target_os = "macos")]
    if let Ok(d) = Device::new_metal(0) {
        return d;
    }
    Device::Cpu
}

#[cfg(not(target_arch = "wasm32"))]
/// A loaded model. The vision backbone loads with it; the point and text heads load on first use.
pub struct Sam3 {
    dir: PathBuf,
    device: Device,
    weights: Arc<weights::Weights>,
    backbone: vit::Vit,
    tracker_neck: Option<neck::Neck>,
    tracker: Option<tracker::Tracker>,
    detector: Option<detector::Detector>,
    tokenizer: Option<tokenizer::Tokenizer>,
}

#[cfg(not(target_arch = "wasm32"))]
/// An image run through the backbone, with the head-specific features computed on demand.
pub struct Encoded {
    backbone: Tensor,
    tracker: Option<tracker::Features>,
    detector: Option<detector::Features>,
}

/// A predicted mask: `MASK_SIDE²` logits over the model's square input (which the image was
/// stretched to), and a confidence 0..1.
#[derive(Clone, Debug)]
pub struct Prediction {
    pub logits: Vec<f32>,
    pub score: f32,
}

#[cfg(not(target_arch = "wasm32"))]
impl Sam3 {
    /// Load from a directory holding `model.safetensors` (the `facebook/sam3` checkpoint in
    /// Hugging Face format), `vocab.json` and `merges.txt`.
    pub fn load(dir: &Path) -> Result<Self> {
        Self::load_on(dir, best_device())
    }

    pub fn load_on(dir: &Path, device: Device) -> Result<Self> {
        if !is_model_dir(dir) {
            return Err(Error::Missing(dir.to_path_buf()));
        }
        let weights = weights::Weights::open(&dir.join(WEIGHTS_FILE))?;
        let backbone = vit::Vit::new(Self::vb(&weights, &device).pp("detector_model.vision_encoder.backbone"))?;
        Ok(Self { dir: dir.to_path_buf(), device, weights, backbone, tracker_neck: None, tracker: None, detector: None, tokenizer: None })
    }

    fn vb<'a>(w: &Arc<weights::Weights>, device: &Device) -> VarBuilder<'a> {
        VarBuilder::from_backend(Box::new(weights::Backend(w.clone())), DType::F32, device.clone())
    }

    pub fn device(&self) -> &Device {
        &self.device
    }

    /// Run the backbone on an RGB image (8-bit, row-major, `w × h`), stretched to the square
    /// input like the reference preprocessing.
    pub fn encode(&self, rgb: &[u8], w: usize, h: usize) -> Result<Encoded> {
        let pixels = mask::preprocess(rgb, w, h, INPUT)?;
        let pixels = Tensor::from_vec(pixels, (1, 3, INPUT, INPUT), &self.device)?;
        self.encode_pixels(&pixels)
    }

    /// Run the backbone on preprocessed pixels `[1, 3, 1008, 1008]` (−1..1).
    pub fn encode_pixels(&self, pixels: &Tensor) -> Result<Encoded> {
        Ok(Encoded { backbone: self.backbone.forward(&pixels.to_device(&self.device)?)?, tracker: None, detector: None })
    }

    fn tracker_parts(&mut self) -> Result<(&neck::Neck, &tracker::Tracker)> {
        if self.tracker_neck.is_none() {
            self.tracker_neck = Some(neck::Neck::new(Self::vb(&self.weights, &self.device).pp("tracker_neck"))?);
        }
        if self.tracker.is_none() {
            self.tracker = Some(tracker::Tracker::new(Self::vb(&self.weights, &self.device).pp("tracker_model"))?);
        }
        match (&self.tracker_neck, &self.tracker) {
            (Some(n), Some(t)) => Ok((n, t)),
            _ => Err(Error::Model("tracker failed to load".into())),
        }
    }

    /// Compute the click decoder's features for `enc` now (so the first click is fast).
    pub fn prepare_clicks(&mut self, enc: &mut Encoded) -> Result<()> {
        let (neck, tracker) = self.tracker_parts()?;
        if enc.tracker.is_none() {
            let levels = neck.forward(&enc.backbone, 3)?;
            enc.tracker = Some(tracker.features(&levels)?);
        }
        Ok(())
    }

    /// Segment one object from clicks (positions normalized to the image, 0..1).
    pub fn segment_clicks(&mut self, enc: &mut Encoded, clicks: &[Click]) -> Result<Prediction> {
        if clicks.is_empty() {
            return Err(Error::Model("no clicks".into()));
        }
        let (neck, tracker) = self.tracker_parts()?;
        if enc.tracker.is_none() {
            let levels = neck.forward(&enc.backbone, 3)?;
            enc.tracker = Some(tracker.features(&levels)?);
        }
        let f = enc.tracker.as_ref().ok_or_else(|| Error::Model("tracker features".into()))?;
        // one click: SAM's three alternatives, best by predicted IoU; more: the single mask
        let p = tracker.predict(f, clicks, clicks.len() == 1)?;
        Ok(Prediction { logits: p.logits, score: p.iou * sigmoid(p.object_score) })
    }

    fn detector_parts(&mut self) -> Result<(&detector::Detector, &tokenizer::Tokenizer)> {
        if self.detector.is_none() {
            self.detector = Some(detector::Detector::new(Self::vb(&self.weights, &self.device).pp("detector_model"))?);
        }
        if self.tokenizer.is_none() {
            self.tokenizer = Some(tokenizer::Tokenizer::load(&self.dir)?);
        }
        match (&self.detector, &self.tokenizer) {
            (Some(d), Some(t)) => Ok((d, t)),
            _ => Err(Error::Model("detector failed to load".into())),
        }
    }

    /// Segment every instance of the concept `text` names. Instances scoring above
    /// `threshold` (0..1, 0.5 is the reference default) are merged into one mask; `None` when
    /// nothing matches.
    pub fn segment_text(&mut self, enc: &mut Encoded, text: &str, threshold: f32) -> Result<Option<Prediction>> {
        let (det, tok) = self.detector_parts()?;
        if enc.detector.is_none() {
            enc.detector = Some(det.features(&enc.backbone)?);
        }
        let f = enc.detector.as_ref().ok_or_else(|| Error::Model("detector features".into()))?;
        let (ids, valid) = tok.encode(text);
        let out = det.predict(f, &ids, &valid)?;
        Ok(detector::merge(&out, threshold))
    }

    /// Raw detector outputs for `text` (tests and tools).
    pub fn detect(&mut self, enc: &mut Encoded, text: &str) -> Result<detector::Output> {
        let (det, tok) = self.detector_parts()?;
        if enc.detector.is_none() {
            enc.detector = Some(det.features(&enc.backbone)?);
        }
        let f = enc.detector.as_ref().ok_or_else(|| Error::Model("detector features".into()))?;
        let (ids, valid) = tok.encode(text);
        det.predict(f, &ids, &valid)
    }
}

#[cfg(not(target_arch = "wasm32"))]
impl Encoded {
    /// The backbone output `[1, 1024, 72, 72]` (tests).
    pub fn backbone(&self) -> &Tensor {
        &self.backbone
    }
}

#[cfg(not(target_arch = "wasm32"))]
fn sigmoid(x: f32) -> f32 {
    1.0 / (1.0 + (-x).exp())
}

#[cfg(not(target_arch = "wasm32"))]
pub use detector::Output as DetectorOutput;

#[cfg(not(target_arch = "wasm32"))]
/// Wait for queued GPU work to finish (benchmarks).
pub fn sync(model: &Sam3) -> Result<()> {
    Ok(model.device.synchronize()?)
}
