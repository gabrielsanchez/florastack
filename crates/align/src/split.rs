//! Splitting a batch of frames into the stacks it holds: a folder of several stacks, of one subject or of several,
//! shot one after another, is cut where a new stack starts. Two neighbors of one stack differ in their focus and a
//! little in their scale (focus breathing, a rail's magnification), so a cut goes where two neighbors differ in
//! another way ([`Reason`]):
//!
//! - **A gap in capture time** longer than [`SplitSettings::gap`] or, by default, than [`MIN_GAP`] and
//!   [`GAP_FACTOR`] times the batch's median gap, whichever is longer ([`gap_threshold`]): the pause between two stacks
//!   is far longer than the pause between two frames of one, whether the camera brackets the focus by itself (a
//!   fraction of a second) or a rail moves between frames (a few seconds).
//! - **Another scene**: alignment finds no match for the next frame, or one that scores below
//!   [`SplitSettings::min_score`], *and* their low-passed luminance, which defocus barely changes (a blur keeps local
//!   averages), correlates less than [`SplitSettings::min_similarity`]. Both must hold: frames focused in front of the
//!   subject or behind it have too little detail to match, but keep their neighbors' low-passed look.
//! - **A jump in framing or scale**: the frames match, but the scale changes by more than
//!   [`SplitSettings::max_scale_step`] or the view shifts by more than [`SplitSettings::max_shift`], as when the same
//!   subject is framed anew.
//! - **Another frame size**, or orientation.
//!
//! [`link`] compares two neighbors, on thumbnails of [`LINK_SIDE`] pixels at most, into a [`Link`]; [`boundaries`] cuts
//! the batch from the links and the capture times. The engine reads the files, one at a time, and keeps only the
//! last thumbnail.

use florastack_core::{Image, Monitor};
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

use crate::error::AlignError;
use crate::framing::Framing;
use crate::prepare::{MIN_SIDE, luminance};
use crate::settings::AlignSettings;
use crate::stack::{Flag, StackAligner};

/// The longer side, in pixels, of the thumbnails [`link`] compares: enough to match and measure framing, and fast.
pub const LINK_SIDE: usize = 512;
/// The shortest gap in capture time that cuts a batch by default, in seconds.
pub const MIN_GAP: f64 = 30.0;
/// A gap this many times the batch's median gap cuts it by default (unless shorter than [`MIN_GAP`]).
pub const GAP_FACTOR: f64 = 8.0;
/// The longer side, in pixels, of the low-passed luminance [`link`] correlates.
const LOW_PASS_SIDE: usize = 32;

/// How two neighboring frames of a batch compare ([`link`]).
#[derive(Clone, Copy, Debug, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase")]
pub struct Link {
    /// Whether they have the same size, and so the same orientation: frames of different sizes are of different
    /// stacks, and the other fields are 0.
    pub same_size: bool,
    /// Whether alignment found a match for the second frame.
    pub matched: bool,
    /// How well it matched, from 0 to 1: alignment's score, the correlation of their band-passed luminance once
    /// aligned.
    pub score: f64,
    /// The correlation of their low-passed luminance, from -1 to 1: near 1 for two frames of one scene however their
    /// focus differs.
    pub similarity: f64,
    /// The change of scale from the first to the second, in percent (either way).
    pub scale_step: f64,
    /// How far the second frame's view is shifted from the first's, in percent of the frame's diagonal.
    pub shift: f64,
}

impl Link {
    /// The link of two frames of different sizes.
    const OTHER_SIZE: Link = Link { same_size: false, matched: false, score: 0.0, similarity: 0.0, scale_step: 0.0, shift: 0.0 };
}

/// Compares two neighboring frames of a batch (see the module documentation), on their thumbnails of at most
/// [`LINK_SIDE`] pixels; larger frames are only slower. Frames too small to align (less than 16 pixels on a side) are
/// compared by their low-passed luminance alone, as unmatched.
pub fn link(first: &Image<f32>, second: &Image<f32>, monitor: &dyn Monitor) -> Result<Link, AlignError> {
    let (w, h) = (first.width(), first.height());
    if (second.width(), second.height()) != (w, h) {
        return Ok(Link::OTHER_SIZE);
    }
    let similarity = low_pass_similarity(first, second)?;
    if w < MIN_SIDE || h < MIN_SIDE {
        return Ok(Link { same_size: true, matched: false, score: 0.0, similarity, scale_step: 0.0, shift: 0.0 });
    }
    let mut aligner = StackAligner::new(&link_settings(w, h), 2, 0)?;
    aligner.add(first, monitor)?;
    let found = aligner.add(second, monitor)?;
    let matched = !found.flags.contains(&Flag::NoMatch);
    let s = found.similarity;
    let finite = |v: f64| if v.is_finite() { v } else { 0.0 };
    let diagonal = (w as f64).hypot(h as f64).max(1.0);
    Ok(Link {
        same_size: true,
        matched,
        score: if matched { finite(found.score).clamp(0.0, 1.0) } else { 0.0 },
        similarity,
        scale_step: finite(100.0 * (s.scale - 1.0).abs()),
        shift: finite(100.0 * s.shift_x.hypot(s.shift_y) / diagonal),
    })
}

/// The alignment settings [`link`] measures with: every term, at the widest limits (the link reports what it finds,
/// [`boundaries`] judges it), nothing rejected or flagged for focus, at the thumbnail's own size.
fn link_settings(width: usize, height: usize) -> AlignSettings {
    let side = u32::try_from(width.max(height).min(LINK_SIDE)).unwrap_or(512);
    AlignSettings {
        refine: false,
        max_shift: 50.0,
        max_scale: 50.0,
        max_rotation: 45.0,
        min_score: 0.0,
        reject: false,
        resolution: side.clamp(crate::MIN_RESOLUTION, crate::MAX_RESOLUTION),
        automatic_order: false,
        reverse_order: false,
        framing: Framing::Narrow,
        min_sharp_fraction: 0.0,
        min_motion: 100.0,
        detect_defects: false,
        ..AlignSettings::default()
    }
}

/// The correlation of two frames' luminance averaged over blocks of a [`LOW_PASS_SIDE`]-th of their longer side: 1
/// when either is flat, which tells nothing.
fn low_pass_similarity(first: &Image<f32>, second: &Image<f32>) -> Result<f64, AlignError> {
    let k = first.width().max(first.height()) / LOW_PASS_SIDE;
    let (a, b) = (luminance(first, k)?, luminance(second, k)?);
    let n = a.as_raw().len().min(b.as_raw().len());
    if n == 0 {
        return Ok(1.0);
    }
    let mean = |v: &[f32]| v.iter().take(n).map(|&x| f64::from(x)).sum::<f64>() / n as f64;
    let (ma, mb) = (mean(a.as_raw()), mean(b.as_raw()));
    let (mut ab, mut aa, mut bb) = (0.0, 0.0, 0.0);
    for (&x, &y) in a.as_raw().iter().zip(b.as_raw()) {
        let (dx, dy) = (f64::from(x) - ma, f64::from(y) - mb);
        ab += dx * dy;
        aa += dx * dx;
        bb += dy * dy;
    }
    // Flat: a variation below a thousandth of the mean level, in either frame.
    let floor = 1e-6 * (ma * ma).max(mb * mb).max(1e-12) * n as f64;
    if aa <= floor || bb <= floor {
        return Ok(1.0);
    }
    let r = ab / (aa * bb).sqrt();
    Ok(if r.is_finite() { r.clamp(-1.0, 1.0) } else { 1.0 })
}

/// When a batch is cut: see the module documentation. The defaults suit stacks shot by hand, on a rail or by the
/// camera's focus bracketing.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(default, deny_unknown_fields, rename_all = "camelCase")]
pub struct SplitSettings {
    /// A gap in capture time longer than this, in seconds, cuts the batch; `None` chooses it from the batch: 30 s or
    /// 8 times its median gap, whichever is longer.
    #[schemars(range(min = 0.0))]
    pub gap: Option<f64>,
    /// Neighbors that match with a lower score than this, from 0 to 1, and aren't similar, are of different scenes.
    #[schemars(range(min = 0.0, max = 1.0))]
    pub min_score: f64,
    /// Neighbors whose low-passed luminance correlates less than this, from -1 to 1, and that don't match, are of
    /// different scenes.
    #[schemars(range(min = -1.0, max = 1.0))]
    pub min_similarity: f64,
    /// A change of scale between matching neighbors beyond this, in percent, is a new framing.
    #[schemars(range(min = 0.0, max = 50.0))]
    pub max_scale_step: f64,
    /// A shift between matching neighbors beyond this, in percent of the frame's diagonal, is a new framing.
    #[schemars(range(min = 0.0, max = 50.0))]
    pub max_shift: f64,
}

impl Default for SplitSettings {
    fn default() -> Self {
        SplitSettings { gap: None, min_score: 0.3, min_similarity: 0.9, max_scale_step: 5.0, max_shift: 10.0 }
    }
}

impl SplitSettings {
    /// Checks the ranges a type can't.
    pub fn validate(&self) -> Result<(), AlignError> {
        if let Some(gap) = self.gap
            && !(gap.is_finite() && gap >= 0.0)
        {
            return Err(AlignError::Setting { name: "gap", value: gap, expected: "a number of seconds from 0" });
        }
        let ranges = [
            ("min_score", self.min_score, 0.0, 1.0, "between 0 and 1"),
            ("min_similarity", self.min_similarity, -1.0, 1.0, "between -1 and 1"),
            ("max_scale_step", self.max_scale_step, 0.0, 50.0, "between 0 and 50 percent"),
            ("max_shift", self.max_shift, 0.0, 50.0, "between 0 and 50 percent"),
        ];
        for (name, value, lo, hi, expected) in ranges {
            if !(lo..=hi).contains(&value) {
                return Err(AlignError::Setting { name, value, expected });
            }
        }
        Ok(())
    }
}

/// Two neighboring frames of a batch, as [`boundaries`] judges them.
#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub struct Neighbors {
    /// The time from the first frame's capture to the second's, in seconds (negative when the second is older);
    /// `None` when either has no capture time.
    pub gap: Option<f64>,
    /// How they compare; `None` when they couldn't be compared.
    pub link: Option<Link>,
}

/// Why a batch is cut between two frames.
#[derive(Clone, Copy, Debug, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(tag = "reason", rename_all = "kebab-case")]
pub enum Reason {
    /// The frames differ in size or orientation.
    Size,
    /// Their capture times are further apart than the threshold, in seconds.
    Gap { seconds: f64, threshold: f64 },
    /// The second frame shows another scene: it doesn't match the first, nor look like it.
    Scene { score: f64, similarity: f64 },
    /// The frames match, but the scale jumps, in percent.
    Scale { percent: f64 },
    /// The frames match, but the view jumps, in percent of the diagonal.
    Shift { percent: f64 },
}

/// A cut: a new stack starts at frame `index`, for these reasons.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize, JsonSchema)]
pub struct Boundary {
    pub index: usize,
    pub reasons: Vec<Reason>,
}

/// The gap in capture time that cuts a batch whose neighbors are `pairs`: `settings.gap`, or else [`MIN_GAP`] or
/// [`GAP_FACTOR`] times the median gap, whichever is longer; `None` when no pair has a gap and none is set.
pub fn gap_threshold(pairs: &[Neighbors], settings: &SplitSettings) -> Option<f64> {
    if let Some(gap) = settings.gap {
        return Some(gap);
    }
    let mut gaps: Vec<f64> = pairs.iter().filter_map(|p| p.gap).filter(|g| g.is_finite()).map(f64::abs).collect();
    if gaps.is_empty() {
        return None;
    }
    gaps.sort_by(f64::total_cmp);
    let median = gaps.get(gaps.len() / 2).copied().unwrap_or(0.0);
    Some(MIN_GAP.max(GAP_FACTOR * median))
}

/// Where a batch of frames is cut into stacks (see the module documentation): `pairs[i]` compares frame `i` with frame
/// `i + 1`, and a cut there starts a new stack at `i + 1`. Every reason that holds is listed.
pub fn boundaries(pairs: &[Neighbors], settings: &SplitSettings) -> Vec<Boundary> {
    let threshold = gap_threshold(pairs, settings);
    let mut cuts = Vec::new();
    for (i, pair) in pairs.iter().enumerate() {
        let mut reasons = Vec::new();
        if let (Some(gap), Some(threshold)) = (pair.gap, threshold)
            && gap.is_finite()
            && gap.abs() > threshold
        {
            reasons.push(Reason::Gap { seconds: gap, threshold });
        }
        if let Some(link) = pair.link {
            reasons.extend(link_reasons(&link, settings));
        }
        if !reasons.is_empty() {
            cuts.push(Boundary { index: i + 1, reasons });
        }
    }
    cuts
}

/// What a link alone says about a cut.
fn link_reasons(link: &Link, settings: &SplitSettings) -> Vec<Reason> {
    if !link.same_size {
        return vec![Reason::Size];
    }
    let good = link.matched && link.score >= settings.min_score;
    if !good {
        return if link.similarity < settings.min_similarity {
            vec![Reason::Scene { score: link.score, similarity: link.similarity }]
        } else {
            vec![]
        };
    }
    let mut reasons = Vec::new();
    if link.scale_step > settings.max_scale_step {
        reasons.push(Reason::Scale { percent: link.scale_step });
    }
    if link.shift > settings.max_shift {
        reasons.push(Reason::Shift { percent: link.shift });
    }
    reasons
}

#[cfg(test)]
mod tests {
    use super::*;

    fn linked(score: f64, similarity: f64, scale_step: f64, shift: f64) -> Link {
        Link { same_size: true, matched: score > 0.0, score, similarity, scale_step, shift }
    }

    fn at(cuts: &[Boundary]) -> Vec<usize> {
        cuts.iter().map(|b| b.index).collect()
    }

    #[test]
    fn gaps_cut_where_the_pause_is_long_against_the_batch() {
        // A rail stack, a frame every 3 s, then two minutes, then the next stack; and a bracketed burst at 0.2 s.
        let mut pairs: Vec<Neighbors> = (0..20).map(|_| Neighbors { gap: Some(3.0), link: None }).collect();
        pairs[9].gap = Some(120.0);
        pairs[14].gap = Some(20.0);
        let settings = SplitSettings::default();
        assert_eq!(gap_threshold(&pairs, &settings), Some(30.0));
        assert_eq!(at(&boundaries(&pairs, &settings)), [10]);
        // A slow manual stack, 10 s a frame: 80 s is its threshold, so a 60 s pause doesn't cut, 100 s does.
        let mut slow: Vec<Neighbors> = (0..12).map(|_| Neighbors { gap: Some(10.0), link: None }).collect();
        slow[3].gap = Some(60.0);
        slow[7].gap = Some(100.0);
        assert_eq!(gap_threshold(&slow, &settings), Some(80.0));
        assert_eq!(at(&boundaries(&slow, &settings)), [8]);
        // Time going back (another card, a reset clock) is a gap too; a gap given is used as is.
        slow[1].gap = Some(-3600.0);
        assert_eq!(at(&boundaries(&slow, &settings)), [2, 8]);
        assert_eq!(at(&boundaries(&slow, &SplitSettings { gap: Some(50.0), ..settings.clone() })), [2, 4, 8]);
        // No times: no gap threshold, and no cut from time.
        let none: Vec<Neighbors> = vec![Neighbors::default(); 5];
        assert_eq!(gap_threshold(&none, &settings), None);
        assert_eq!(boundaries(&none, &settings), Vec::new());
        assert_eq!(boundaries(&pairs, &settings)[0].reasons, [Reason::Gap { seconds: 120.0, threshold: 30.0 }]);
    }

    #[test]
    fn links_cut_at_another_scene_or_framing_but_not_at_blurred_neighbors() {
        let settings = SplitSettings::default();
        let pairs = [
            Neighbors { gap: None, link: Some(linked(0.95, 0.99, 0.4, 0.3)) }, // 0-1: neighbors in focus
            Neighbors { gap: None, link: Some(linked(0.05, 0.97, 0.0, 0.0)) }, // 1-2: blurred neighbors that barely match
            Neighbors { gap: None, link: Some(linked(0.0, 0.92, 0.0, 0.0)) },  // 2-3: no match, but alike
            Neighbors { gap: None, link: Some(linked(0.1, 0.2, 0.0, 0.0)) },   // 3-4: another scene
            Neighbors { gap: None, link: Some(linked(0.9, 0.95, 12.0, 0.5)) }, // 4-5: framed anew, larger
            Neighbors { gap: None, link: Some(linked(0.9, 0.95, 1.0, 25.0)) }, // 5-6: framed anew, elsewhere
            Neighbors { gap: None, link: Some(Link::OTHER_SIZE) },             // 6-7: turned
            Neighbors { gap: Some(1.0), link: None },                          // 7-8: not compared
        ];
        let cuts = boundaries(&pairs, &settings);
        assert_eq!(at(&cuts), [4, 5, 6, 7]);
        assert_eq!(cuts[0].reasons, [Reason::Scene { score: 0.1, similarity: 0.2 }]);
        assert_eq!(cuts[1].reasons, [Reason::Scale { percent: 12.0 }]);
        assert_eq!(cuts[2].reasons, [Reason::Shift { percent: 25.0 }]);
        assert_eq!(cuts[3].reasons, [Reason::Size]);
        // A jump measured on frames that don't match is no jump.
        let unmatched = [Neighbors { gap: None, link: Some(linked(0.1, 0.95, 30.0, 40.0)) }];
        assert_eq!(boundaries(&unmatched, &settings), Vec::new());
    }

    #[test]
    fn settings_validate_their_ranges() {
        assert_eq!(SplitSettings::default().validate(), Ok(()));
        let bad = [
            SplitSettings { gap: Some(-1.0), ..SplitSettings::default() },
            SplitSettings { gap: Some(f64::NAN), ..SplitSettings::default() },
            SplitSettings { min_score: 1.5, ..SplitSettings::default() },
            SplitSettings { min_similarity: -2.0, ..SplitSettings::default() },
            SplitSettings { max_scale_step: 60.0, ..SplitSettings::default() },
            SplitSettings { max_shift: f64::INFINITY, ..SplitSettings::default() },
        ];
        let names: Vec<&str> = bad
            .iter()
            .map(|s| match s.validate() {
                Err(AlignError::Setting { name, .. }) => name,
                other => panic!("{s:?}: {other:?}"),
            })
            .collect();
        assert_eq!(names, ["gap", "gap", "min_score", "min_similarity", "max_scale_step", "max_shift"]);
    }

    #[test]
    fn flat_or_tiny_frames_are_compared_without_failing() {
        let flat = Image::from_fn(40, 30, 3, florastack_core::ColorSpace::LINEAR, |_, _, _| 0.5).unwrap();
        let tiny = Image::from_fn(8, 6, 1, florastack_core::ColorSpace::LINEAR, |x, y, _| (x * y) as f32 / 40.0).unwrap();
        let other = Image::from_fn(30, 40, 3, florastack_core::ColorSpace::LINEAR, |_, _, _| 0.5).unwrap();
        let unmonitored = florastack_core::Unmonitored;
        let l = link(&flat, &flat, &unmonitored).unwrap();
        assert!(l.same_size && (l.similarity - 1.0).abs() < 1e-12, "{l:?}");
        let l = link(&tiny, &tiny, &unmonitored).unwrap();
        assert!(l.same_size && !l.matched && (l.similarity - 1.0).abs() < 1e-9, "{l:?}");
        assert_eq!(link(&flat, &other, &unmonitored).unwrap(), Link::OTHER_SIZE);
    }
}
