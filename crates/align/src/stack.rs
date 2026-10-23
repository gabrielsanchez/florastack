//! Aligning a stack: each frame to its neighbor, the transforms composed back to the master.
//!
//! [`StackAligner`] takes the frames one at a time, in the order it asks for them ([`StackAligner::next_frame`]): the
//! master first, then outward from it, toward the end of the stack and then toward its start. Each frame is aligned
//! to the frame before it in that order, its neighbor in focus, and its transform to the master is the neighbor's
//! followed by that link. Neighbors are focused at nearly the same depth, so they share most of their detail, which
//! frames focused far apart don't. With [`AlignSettings::refine`], each frame is then aligned again directly against
//! the master, starting from the chained estimate.
//!
//! A frame whose alignment hits a limit, scores below [`AlignSettings::min_score`] or finds no match is flagged, and
//! with [`AlignSettings::reject`] rejected: it keeps the transform of the frame it was compared with (no motion
//! assumed), and the next frame is aligned to that frame instead, so that one bad frame doesn't break the chain. A
//! frame whose match is ambiguous, as on a repeating pattern, is flagged only.
//!
//! Only three prepared frames are held at a time (the master's, the last accepted one's and the new one's), however
//! long the stack, plus each frame's small focus map (`focus.rs`): the caller reads each frame when it is asked for
//! and can drop it once [`StackAligner::add`] returns. Once every frame is in, [`StackAligner::finish`] compares the
//! focus maps, through the frames' transforms, and flags the frames that are out of focus ([`Flag::OutOfFocus`]).

use florastack_core::{Affine, Image, Monitor};
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};
use tracing::debug;

use crate::brightness::gain;
use crate::ecc::{self, LIMIT_TOLERANCE};
use crate::error::AlignError;
use crate::focus::{FocusMap, sharp_fractions};
use crate::framing::{Canvas, Framing};
use crate::model::{Geometry, Params, Similarity};
use crate::prepare::Prepared;
use crate::settings::{AlignSettings, Term};

/// A second correlation peak at least this high, relative to the first, flags a repeating pattern.
const AMBIGUOUS: f64 = 0.5;

/// Whether a frame was aligned.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "kebab-case")]
pub enum Status {
    /// The master frame, which the others are aligned to.
    Master,
    Aligned,
    /// Its alignment couldn't be trusted (see its flags): it keeps the transform of the frame it was compared with.
    Rejected,
}

/// Something about a frame that needs a look: its alignment, or its focus.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "kebab-case")]
pub enum Flag {
    /// The term reached its limit (relative to the master) and would have gone beyond it.
    Limit(Term),
    /// The frame scored below the minimum score.
    LowScore,
    /// The frame matches its reference in several places about as well, as a repeating pattern (a grid, a fabric)
    /// does: the match found may be off by a period.
    RepeatingPattern,
    /// No match: the frames share no detail, or don't overlap.
    NoMatch,
    /// Refining against the master was asked for, but didn't match well enough: the chained estimate stays.
    NotRefined,
    /// The frame has no in-focus detail: its sharp-content fraction is less than
    /// [`AlignSettings::min_sharp_fraction`] (`focus.rs`). Only flagged, never rejected: the user decides whether to
    /// leave it out.
    OutOfFocus,
}

impl Flag {
    /// Whether the flag makes a frame rejected, when rejection is on.
    pub fn rejects(self) -> bool {
        matches!(self, Flag::Limit(_) | Flag::LowScore | Flag::NoMatch)
    }
}

/// A frame's alignment to the master.
#[derive(Clone, Debug, PartialEq)]
pub struct FrameAlignment {
    /// Maps a pixel of the master frame to the same point of the scene in this frame (as
    /// `florastack_core::testutil::GroundTruth::relative` does), so that warping this frame onto the master samples it
    /// there.
    pub transform: Affine,
    /// The same transform as its terms.
    pub similarity: Similarity,
    /// The gain that matches this frame's brightness to the master's: 1 when brightness isn't corrected or can't be
    /// measured.
    pub gain: f64,
    /// How well the frame matched the frame it was aligned to, from 0 to 1: the correlation of their band-passed
    /// luminance once aligned. 1 for the master.
    pub score: f64,
    /// The frame it was aligned to: its neighbor toward the master, or the last frame there that wasn't rejected.
    /// `None` for the master.
    pub reference: Option<usize>,
    pub status: Status,
    pub flags: Vec<Flag>,
    /// The share of the stack's in-focus detail on which this frame is among the sharpest, from 0 to 1 (`focus.rs`):
    /// near 0 for a frame focused where the subject isn't. `None` when it can't be told (fewer than three frames, too
    /// little detail that some frames show sharp and others blurred, or a rejected frame, whose transform is only its
    /// reference's), and until the stack is finished.
    pub sharp_fraction: Option<f64>,
}

impl FrameAlignment {
    fn master() -> FrameAlignment {
        FrameAlignment {
            transform: Affine::IDENTITY,
            similarity: Similarity::IDENTITY,
            gain: 1.0,
            score: 1.0,
            reference: None,
            status: Status::Master,
            flags: Vec::new(),
            sharp_fraction: None,
        }
    }
}

/// Every frame's alignment, in the stack's order.
#[derive(Clone, Debug, PartialEq)]
pub struct StackAlignment {
    pub master: usize,
    pub frames: Vec<FrameAlignment>,
    /// The frames' size, in pixels, which the transforms are in.
    pub width: usize,
    pub height: usize,
}

impl StackAlignment {
    /// The same alignment relative to frame `master` instead: each frame's transform from the new master's pixels is
    /// the inverse of the new master's transform followed by its own, and its gain is divided by the new master's.
    /// Scores, references, flags and sharp-content fractions stay; the new master's status becomes [`Status::Master`]
    /// and the old one's [`Status::Aligned`]. Rebase onto a frame that was aligned: a rejected frame's transform is
    /// only its reference's.
    pub fn rebased(&self, master: usize) -> Result<StackAlignment, AlignError> {
        let base = self.frames.get(master).ok_or(AlignError::Master { master, frames: self.frames.len() })?;
        if master == self.master {
            return Ok(self.clone());
        }
        let inverse = base.similarity.inverse();
        if !inverse.is_finite() {
            return Err(AlignError::Transform { index: master });
        }
        let base_gain = if base.gain.is_finite() && base.gain > 0.0 { base.gain } else { 1.0 };
        let frames = self
            .frames
            .iter()
            .enumerate()
            .map(|(i, f)| {
                let similarity = if i == master { Similarity::IDENTITY } else { inverse.then(&f.similarity) };
                let status = match f.status {
                    _ if i == master => Status::Master,
                    Status::Master => Status::Aligned,
                    other => other,
                };
                FrameAlignment {
                    transform: similarity.to_affine(self.width, self.height),
                    similarity,
                    gain: if i == master { 1.0 } else { f.gain / base_gain },
                    status,
                    ..f.clone()
                }
            })
            .collect();
        Ok(StackAlignment { master, frames, width: self.width, height: self.height })
    }

    /// The output canvas `framing` gives this alignment's frames (`framing.rs`): the frames that weren't rejected,
    /// with `narrow_frame`, the frame at the stack's narrow end ([`crate::ProcessingOrder::narrow_frame`]), for narrow
    /// framing.
    pub fn canvas(&self, framing: Framing, narrow_frame: Option<usize>) -> Result<Canvas, AlignError> {
        let used: Vec<usize> = (0..self.frames.len()).filter(|&i| self.frames.get(i).is_some_and(|f| f.status != Status::Rejected)).collect();
        let transforms: Vec<Affine> = used.iter().filter_map(|&i| self.frames.get(i).map(|f| f.transform)).collect();
        let narrow = narrow_frame.and_then(|n| used.iter().position(|&i| i == n));
        Canvas::new(framing, &transforms, self.width, self.height, narrow)
    }
}

/// The last frame accepted in the current direction: what the next one is aligned to.
struct Link {
    index: usize,
    prepared: Prepared,
    /// Its transform from the master.
    total: Params,
    /// Its own link, from the frame before it, which predicts the next one's: focus breathing changes smoothly.
    step: Params,
}

/// Aligns a stack frame by frame. See the module documentation.
pub struct StackAligner {
    settings: AlignSettings,
    terms: Vec<Term>,
    master: usize,
    /// The frames in the order they are aligned, and how many have been.
    order: Vec<usize>,
    added: usize,
    geometry: Option<Geometry>,
    reference: Option<Prepared>,
    last: Option<Link>,
    results: Vec<Option<FrameAlignment>>,
    /// Each frame's focus map, compared once every frame is in.
    focus: Vec<Option<FocusMap>>,
}

impl StackAligner {
    /// An aligner for a stack of `frames` frames whose frame `master` the others are aligned to.
    pub fn new(settings: &AlignSettings, frames: usize, master: usize) -> Result<StackAligner, AlignError> {
        settings.validate()?;
        if frames == 0 {
            return Err(AlignError::NoFrames);
        }
        if master >= frames {
            return Err(AlignError::Master { master, frames });
        }
        let order = std::iter::once(master).chain(master + 1..frames).chain((0..master).rev()).collect();
        Ok(StackAligner {
            settings: settings.clone(),
            terms: Term::ALL.into_iter().filter(|&t| settings.corrects(t)).collect(),
            master,
            order,
            added: 0,
            geometry: None,
            reference: None,
            last: None,
            results: vec![None; frames],
            focus: vec![None; frames],
        })
    }

    /// The frame to add next, or `None` once every frame has been.
    pub fn next_frame(&self) -> Option<usize> {
        self.order.get(self.added).copied()
    }

    /// The order the frames are added in: the master, then outward from it.
    pub fn order(&self) -> &[usize] {
        &self.order
    }

    /// Aligns the next frame ([`StackAligner::next_frame`]), which must have the master's size, and returns its
    /// alignment. Reports progress to `monitor` and stops with [`AlignError::Cancelled`] when it is cancelled.
    pub fn add(&mut self, frame: &Image<f32>, monitor: &dyn Monitor) -> Result<&FrameAlignment, AlignError> {
        let index = self.next_frame().ok_or(AlignError::Finished)?;
        let (w, h) = (frame.width(), frame.height());
        if let Some(g) = &self.geometry
            && (g.width, g.height) != (w, h)
        {
            return Err(AlignError::FrameSize { index, width: w, height: h, expected_width: g.width, expected_height: g.height });
        }
        monitor.checkpoint()?;
        let prepared = Prepared::new(frame, self.settings.resolution as usize, self.settings.brightness)?;
        if let Some(slot) = self.focus.get_mut(index) {
            *slot = Some(prepared.focus.clone());
        }
        monitor.checkpoint()?;
        let result = if index == self.master {
            self.geometry = Some(Geometry::new(w, h, &self.settings));
            self.reference = Some(prepared);
            FrameAlignment::master()
        } else {
            self.align(index, prepared)
        };
        debug!(frame = index, reference = ?result.reference, score = result.score, gain = result.gain, status = ?result.status, flags = ?result.flags, "aligned");
        let slot = self.results.get_mut(index).ok_or(AlignError::Finished)?;
        let result = slot.insert(result);
        self.added += 1;
        monitor.step(self.added as u64, self.order.len() as u64)?;
        Ok(result)
    }

    /// Every frame's alignment, once every frame has been added, with their sharp-content fractions, and the frames
    /// whose fraction is below [`AlignSettings::min_sharp_fraction`] flagged as out of focus.
    pub fn finish(self) -> Result<StackAlignment, AlignError> {
        let frames = self.results.len();
        let added = self.added;
        let results: Option<Vec<FrameAlignment>> = self.results.into_iter().collect();
        let mut frames = results.ok_or(AlignError::Incomplete { added, frames })?;
        let (width, height) = self.geometry.map_or((0, 0), |g| (g.width, g.height));
        let transforms: Vec<Affine> = frames.iter().map(|f| f.transform).collect();
        let gains: Vec<f64> = frames.iter().map(|f| f.gain).collect();
        // A rejected frame's transform is only its reference's: its map can't be laid over the others.
        let maps: Vec<Option<FocusMap>> = self.focus.into_iter().zip(&frames).map(|(map, f)| map.filter(|_| f.status != Status::Rejected)).collect();
        let found = sharp_fractions(&maps, &transforms, &gains, self.master);
        debug!(peaked = found.peaked, cells = found.cells, fractions = ?found.fractions, "focus");
        for (frame, fraction) in frames.iter_mut().zip(found.fractions) {
            frame.sharp_fraction = fraction;
            if fraction.is_some_and(|f| f < self.settings.min_sharp_fraction) {
                frame.flags.push(Flag::OutOfFocus);
            }
        }
        Ok(StackAlignment { master: self.master, frames, width, height })
    }

    /// Aligns frame `index`, which isn't the master, to the last frame accepted on its side of the master.
    fn align(&mut self, index: usize, prepared: Prepared) -> FrameAlignment {
        // The master comes first, so the geometry and the master's preparation are there.
        let (Some(geometry), Some(master)) = (self.geometry, self.reference.as_ref()) else {
            return FrameAlignment { status: Status::Rejected, flags: vec![Flag::NoMatch], ..FrameAlignment::master() };
        };
        // Turning back toward the start of the stack, the chain starts again from the master.
        if self.last.as_ref().is_some_and(|l| (l.index > self.master) != (index > self.master)) {
            self.last = None;
        }
        let (reference_index, reference, base, predicted) = match &self.last {
            Some(l) => (l.index, &l.prepared, l.total, l.step),
            None => (self.master, master, Params::default(), Params::default()),
        };
        let expected = (predicted.get(Term::ShiftX), predicted.get(Term::ShiftY));
        let link = ecc::align(reference, &prepared, &geometry, &self.terms, predicted, Some(expected));
        let mut total = compose(&geometry, &base, &link.params);

        let mut flags = Vec::new();
        if let Some(term) = link.pushed.or_else(|| geometry.beyond_limits(&total, LIMIT_TOLERANCE)) {
            flags.push(Flag::Limit(term));
        }
        if !link.matched {
            flags.push(Flag::NoMatch);
        } else if link.score < self.settings.min_score {
            flags.push(Flag::LowScore);
        }
        if link.ambiguity >= AMBIGUOUS {
            flags.push(Flag::RepeatingPattern);
        }
        if self.settings.refine && link.matched {
            let direct = ecc::align(master, &prepared, &geometry, &self.terms, total, None);
            let trusted = direct.matched
                && direct.score >= self.settings.min_score
                && direct.pushed.is_none()
                && geometry.beyond_limits(&direct.params, LIMIT_TOLERANCE).is_none();
            if trusted {
                total = direct.params;
            } else {
                flags.push(Flag::NotRefined);
            }
        }

        let rejected = self.settings.reject && flags.iter().any(|f| f.rejects());
        let total = if rejected { base } else { total };
        let similarity = geometry.similarity(&total);
        let transform = similarity.to_affine(geometry.width, geometry.height);
        let gain = if self.settings.brightness { gain(master, &prepared, &transform).unwrap_or(1.0) } else { 1.0 };
        if !rejected {
            self.last = Some(Link { index, prepared, total, step: link.params });
        }
        FrameAlignment {
            transform,
            similarity,
            gain,
            score: link.score.clamp(0.0, 1.0),
            reference: Some(reference_index),
            status: if rejected { Status::Rejected } else { Status::Aligned },
            flags,
            sharp_fraction: None,
        }
    }
}

/// The transform from the master that applies `base`, then `link`.
fn compose(geometry: &Geometry, base: &Params, link: &Params) -> Params {
    geometry.params(&geometry.similarity(base).then(&geometry.similarity(link)))
}

/// Aligns frames already in memory to frame `master`, reporting progress to `monitor`.
pub fn align_frames(frames: &[Image<f32>], master: usize, settings: &AlignSettings, monitor: &dyn Monitor) -> Result<StackAlignment, AlignError> {
    let mut aligner = StackAligner::new(settings, frames.len(), master)?;
    while let Some(index) = aligner.next_frame() {
        let frame = frames.get(index).ok_or(AlignError::Finished)?;
        aligner.add(frame, monitor)?;
    }
    aligner.finish()
}
