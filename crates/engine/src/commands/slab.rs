//! Slabbing (`stack.slabbed`, a job): a stack of many frames split into overlapping sub-stacks, the slabs, each stacked
//! with one method, and the slabs' outputs stacked with another into the final output, by default the pyramid for
//! both. A deep stack's final stack then reads a few slabs instead of hundreds of frames, and a new setting of the
//! final method, such as the depth map's threshold, reads only the slabs' outputs again.
//!
//! - **The methods**: by default the pyramid makes the slabs and the final stack, which scored best on the synthetic
//!   300-frame stack of occluding layers (SSIM 0.896, against the unslabbed pyramid's 0.886) and leaves the real
//!   stacks as clean as the unslabbed pyramid. Pyramid slabs then a depth map (`"final": {"method": "depth-map"}`) came
//!   close on the synthetic stack (0.868, the unslabbed depth map 0.733), but on the wasp's six slabs, at the depth
//!   map's defaults, it filled patches of the smooth inside of a leg from the blurred slabs behind it (roadmap 3.4).
//!
//! - **The slabs** ([`layout`]): `slabSize` consecutive frames each, in the order the stack is stacked, each starting
//!   before the one before it ends so that neighbors share at least `overlap` frames; spaced evenly, from the first
//!   frame to the last. By default a slab holds the square root of the number of frames, rounded up, which balances
//!   the two stages (about as many slabs as frames in a slab), and neighbors share a fifth of a slab, at least a frame.
//! - **The slab stage** stacks one slab at a time, as a stack of its frames is stacked, and stores it as soon as it is
//!   finished: memory holds one stack's accumulators, whatever the number of slabs, and the frames neighbors share
//!   are read once for each. Keeping two slabs open instead, to read those frames once, halved each one's memory
//!   budget: at 20 MP, the pyramid's accumulators spilled to disk at every frame, and the wasp's 23 frames took 476 s
//!   instead of 51 (roadmap 3.4).
//! - **Slab outputs**: every slab becomes a project output, made by the slab method from its frames, with its place in
//!   the stack in its parameters (`"slab": {"index": 0, "of": 18}`, from 0), so that retouching can take pixels from a
//!   slab, near in focus to the point it repairs, instead of from one of hundreds of frames. A slab covers what its
//!   frames cover together: in wide framing, the pixels none of them covers are transparent.
//! - **Reuse**: a slab's image is keyed by everything it is made from (the method and its settings, each frame's file,
//!   development, defect mask, alignment and gain, the canvas, FloraStack's version). With `reuse`, the default, a slab
//!   whose key an output of the project has already is not stacked again: the final stage reads that output. Trying
//!   other settings of the final method, such as the depth map's threshold, then reads only the slabs' outputs.
//! - **The final stage** stacks the slabs' outputs, read back from the cache in order, with their coverage (their alpha
//!   channel), by the final method. It reads a pyramid slab's result before tone compression when the slab kept one,
//!   so that values outside [0, 1] are compressed once, at the end, by the pyramid's `tone`: the final method's when it
//!   is the pyramid, the slabs' otherwise.
//! - **The commit** adds the new slab outputs and the final output, whose inputs are the slabs' outputs, in one undo
//!   step. A reused output removed while the job ran is added again from what the job read of it.

use std::fmt::Write as _;
use std::ops::Range;

use florastack_align::Framing;
use florastack_core::{AnyImage, Image, Monitor};
use florastack_project::{CacheDir, CacheRef, ContentHash, Edit, FrameId, Input, NewOutput, OutputId, Project};
use florastack_raw::Quality;
use florastack_stack::{CoveredFrame, FrameSource, StackError, Stacked, Tiling};
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

use super::stack::{
    DepthSummary, Feed, MethodName, MethodParams, RangeSummary, StackPlan, Stored, Units, UsedSettings, Warping, locks, stack_feed, store, unclipped,
};
use super::{MAX_ITEMS, Quiet, check_name};
use crate::error::{CommandError, Invalid};
use crate::jobs::{JobPlan, Lane, commit};
use crate::project::{develop_record, read_image, stack_settings};
use crate::registry::{CommandSpec, Meta, Params};
use crate::session::Session;

const COMMAND: &str = "stack.slabbed";
/// The fewest frames slabbing takes: two slabs of two, sharing one.
pub const MIN_FRAMES: usize = 3;
/// The smallest slab.
pub const MIN_SLAB: usize = 2;
/// The most slabs, each an output of the project.
pub const MAX_SLABS: usize = 1000;

pub(crate) fn specs() -> Vec<CommandSpec> {
    vec![CommandSpec::job(
        Meta::new("stack.slabbed", "Stack in slabs", "Stacks the aligned frames in overlapping slabs by one method, then the slabs by another.")
            .help(
                "For very deep stacks. Splits the frames, in the order they are stacked, into slabs of `slabSize` \
                 consecutive frames, neighbors sharing at least `overlap` frames, stacks each slab with the `slabs` \
                 method and the slabs with the `final` method (the pyramid by default, for both), \
                 each with the project's settings (`stack.get`) under the ones given for this stack, as \
                 `stack.preview` takes them: `{\"method\": \"pyramid\", \"grit\": 0.25}`. By default a slab holds \
                 the square root of the number of frames, rounded up, and neighbors share a fifth of a slab. Slabs are \
                 stacked one at a time, in the memory of one stack. Each slab becomes an output of its own, made from \
                 its frames, for retouching to take pixels from; the final output is made from the slabs' outputs. \
                 With `reuse` (the default), a slab the project has an output of already, made from the same frames, \
                 development, alignment and settings, is taken from that output instead of being stacked again: run \
                 it again with other `final` settings and only the slabs' outputs are stacked, not the frames. Uses the \
                 stored alignment, as `stack.average` does. Runs as a job; the outputs are added in one undo step \
                 when it ends, or not at all when it is cancelled.",
            )
            .undoable()
            .examples(&[
                "{}",
                r#"{ "final": { "method": "depth-map", "threshold": "percentile", "percentile": 40 }, "name": "Slabbed depth map" }"#,
                r#"{ "slabs": { "grit": 0.25 }, "final": { "grit": 0.75 }, "slabSize": 12, "overlap": 3 }"#,
            ]),
        Lane::Heavy,
        slabbed,
    )]
}

/// A stack in slabs.
#[derive(Debug, Default, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields, rename_all = "camelCase")]
struct SlabbedParams {
    /// The final output's name; "Slabbed pyramid" (after the final method) when left out.
    #[serde(default)]
    name: Option<String>,
    /// The slabs' method and its settings, as `stack.preview` takes them; the pyramid method, with the project's
    /// settings, when left out.
    #[serde(default)]
    slabs: MethodParams,
    /// The final stack's method and its settings; the pyramid method, with the project's settings, when left out.
    #[serde(default, rename = "final")]
    final_stack: MethodParams,
    /// The frames in a slab, from 2, fewer than the frames stacked; the square root of their number, rounded up, when
    /// left out.
    #[serde(default)]
    #[schemars(range(min = 2, max = 100_000))]
    slab_size: Option<usize>,
    /// How many frames neighboring slabs share at least, fewer than a slab holds; a fifth of a slab, at least 1, when
    /// left out.
    #[serde(default)]
    #[schemars(range(max = 100_000))]
    overlap: Option<usize>,
    /// Whether a slab the project has an output of already, made from the same frames with the same settings, is
    /// taken from that output instead of being stacked again; true when left out.
    #[serde(default)]
    reuse: Option<bool>,
}

/// `e`, about a field of the parameters' `parent` object.
fn nested(parent: &str, e: Invalid) -> Invalid {
    Invalid::new(format!("{parent}.{}", e.field), e.reason)
}

impl Params for SlabbedParams {
    fn check(&self) -> Result<(), Invalid> {
        if let Some(name) = &self.name {
            check_name("name", name)?;
        }
        self.slabs.check_for(MethodName::Pyramid).map_err(|e| nested("slabs", e))?;
        self.final_stack.check_for(MethodName::Pyramid).map_err(|e| nested("final", e))?;
        if let Some(size) = self.slab_size
            && !(MIN_SLAB..=MAX_ITEMS).contains(&size)
        {
            return Err(Invalid::new("slabSize", "must be from 2 to 100000 frames"));
        }
        if let Some(overlap) = self.overlap {
            if overlap > MAX_ITEMS {
                return Err(Invalid::new("overlap", "must be at most 100000 frames"));
            }
            if let Some(size) = self.slab_size
                && overlap >= size
            {
                return Err(Invalid::new("overlap", "must be less than `slabSize`"));
            }
        }
        Ok(())
    }
}

/// The default slab size for `n` frames: the square root, rounded up, at least 2 and less than `n` (`n` from 3).
pub fn default_slab_size(n: usize) -> usize {
    let mut size = n.isqrt();
    if size.saturating_mul(size) < n {
        size += 1;
    }
    size.max(MIN_SLAB).min(n.saturating_sub(1)).max(1)
}

/// The default overlap for slabs of `size` frames: a fifth, rounded, at least 1 and less than `size`.
pub fn default_overlap(size: usize) -> usize {
    (size.saturating_add(2) / 5).max(1).min(size.saturating_sub(1))
}

/// The slabs of `n` frames (see the module documentation), as ranges of the frames' positions: `size` frames each,
/// neighbors sharing at least `overlap`, the first starting at 0 and the last ending at `n`. `size` is from 2 and below
/// `n`, `overlap` below `size`; others make one slab of every frame.
pub fn layout(n: usize, size: usize, overlap: usize) -> Vec<Range<usize>> {
    if n == 0 {
        return Vec::new();
    }
    if size < MIN_SLAB || size >= n || overlap >= size {
        return std::iter::once(0..n).collect();
    }
    let step = size - overlap;
    // At least two slabs, since n - overlap > size - overlap.
    let count = (n - overlap).div_ceil(step);
    let (span, gaps) = ((n - size) as u64, (count - 1).max(1) as u64);
    (0..count as u64)
        .map(|j| {
            // The j-th start, j * span / gaps rounded to the nearest frame.
            let start = usize::try_from((2 * j * span + gaps) / (2 * gaps)).unwrap_or(0).min(n - size);
            start..start + size
        })
        .collect()
}

/// An output of the project that a slab is taken from.
struct Reused {
    id: OutputId,
    /// What the final stage reads of it ([`unclipped`]).
    source: CacheRef,
    /// The output as the job read it, to add again if it is removed meanwhile.
    output: NewOutput,
}

/// A slab: its frames' positions in the plan, its key, and the output it is taken from, if any.
struct Slab {
    range: Range<usize>,
    key: String,
    reused: Option<Reused>,
}

/// Whether the cache holds `reference`'s file.
fn present(cache: &CacheDir, reference: &CacheRef) -> bool {
    reference.path.resolve(cache.path()).is_file()
}

/// The output of `project` that a slab with `key`, by `method`, can be taken from: the first whose image has that key
/// and whose files are there.
fn reusable(project: &Project, cache: &CacheDir, key: &str, method: &str) -> Option<Reused> {
    let output = project.outputs.iter().find(|o| {
        o.method == method && o.image.key.as_deref() == Some(key) && present(cache, &o.image) && present(cache, unclipped(&o.image, &o.maps))
    })?;
    Some(Reused {
        id: output.id(),
        source: unclipped(&output.image, &output.maps).clone(),
        output: NewOutput {
            name: output.name.clone(),
            method: output.method.clone(),
            parameters: output.parameters.clone(),
            inputs: output.inputs.clone(),
            image: output.image.clone(),
            preview: output.preview.clone(),
            maps: output.maps.clone(),
        },
    })
}

/// The key of a slab's image: everything it is made from, hashed (see the module documentation).
fn slab_key(plan: &StackPlan, parameters: &serde_json::Value, develop: &str, range: &Range<usize>) -> Result<String, CommandError> {
    let c = &plan.canvas;
    let mut text = format!(
        "slab, FloraStack {}\n{} {parameters}\ncanvas {} {} {:?}\ndevelop {develop}\n",
        env!("CARGO_PKG_VERSION"),
        plan.settings.name(),
        c.width,
        c.height,
        c.to_master.m
    );
    for k in range.clone() {
        let f = plan.frames.get(k).ok_or_else(|| plan.internal())?;
        writeln!(text, "{} {:?} {}", plan.reader.frame_key(f.index)?, f.transform.m, f.gain).map_err(|_| plan.internal())?;
    }
    Ok(format!("slab {}", ContentHash::of(text.as_bytes())))
}

/// The slabs' outputs, read back from the cache in the stack's order, each with its coverage: an alpha channel, when
/// it has one (2 or 4 channels), is the slab's coverage.
struct Cached<'a> {
    cache: &'a CacheDir,
    sources: &'a [CacheRef],
    /// The canvas's size, which every slab has.
    size: (usize, usize),
}

/// `image` without its last channel, and that channel.
fn split_alpha(image: &Image<f32>) -> Result<(Image<f32>, Image<f32>), StackError> {
    let (w, h, c) = (image.width(), image.height(), image.channels());
    let color_channels = c.saturating_sub(1).max(1);
    let mut color = Image::new(w, h, color_channels, image.color_space().clone())?;
    let mut alpha = Image::new(w, h, 1, florastack_core::ColorSpace::LINEAR)?;
    for ((src, dst), a) in image.rows().zip(color.rows_mut()).zip(alpha.rows_mut()) {
        for ((s, d), a) in src.chunks_exact(c).zip(dst.chunks_exact_mut(color_channels)).zip(a) {
            if let (Some(colors), Some(&last)) = (s.get(..color_channels), s.last()) {
                d.copy_from_slice(colors);
                *a = last;
            }
        }
    }
    Ok((color, alpha))
}

impl Feed for Cached<'_> {
    fn len(&self) -> usize {
        self.sources.len()
    }

    fn with(&mut self, i: usize, f: &mut dyn FnMut(&dyn FrameSource) -> Result<(), CommandError>) -> Result<(), CommandError> {
        let internal = || CommandError::Internal { command: COMMAND.to_owned() };
        let reference = self.sources.get(i).ok_or_else(internal)?;
        // Slabs are stored as linear f32, which reads back as such.
        let AnyImage::F32(image) = read_image(self.cache, reference)? else { return Err(internal()) };
        if (image.width(), image.height()) != self.size {
            return Err(CommandError::Unavailable {
                command: COMMAND.to_owned(),
                reason: format!(
                    "the output of slab {} is {}x{} pixels, but the canvas is {}x{}: stack the slabs again with `\"reuse\": false`",
                    i + 1,
                    image.width(),
                    image.height(),
                    self.size.0,
                    self.size.1
                ),
            });
        }
        if matches!(image.channels(), 2 | 4) {
            let (color, alpha) = split_alpha(&image)?;
            drop(image);
            f(&CoveredFrame::new(color, alpha)?)
        } else {
            f(&image)
        }
    }
}

/// Stores slab `j`'s stack, keyed.
fn store_slab(
    plan: &StackPlan,
    j: usize,
    stacked: Stacked,
    depth: Option<florastack_stack::DepthMap>,
    slabs: &[Slab],
) -> Result<Stored, CommandError> {
    let key = slabs.get(j).map(|s| s.key.clone()).ok_or_else(|| plan.internal())?;
    store(&plan.cache, COMMAND, &format!("slab-{}-{}", j + 1, plan.settings.name()), stacked, depth, plan.settings.ranged(), Some(key))
}

/// A slab of a slabbed stack.
#[derive(Debug, Serialize, JsonSchema)]
#[serde(rename_all = "camelCase")]
struct SlabSummary {
    /// Its output.
    output: OutputId,
    /// Its frames: `count` of the stack's `frames`, from position `start` (from 0).
    start: usize,
    count: usize,
    /// Whether it was taken from an output the project had, instead of being stacked again.
    reused: bool,
}

/// What stacking in slabs made.
#[derive(Debug, Serialize, JsonSchema)]
#[serde(rename_all = "camelCase")]
struct SlabbedSummary {
    /// The final output, made from the slabs' outputs.
    output: OutputId,
    /// Its size, in pixels.
    width: usize,
    height: usize,
    /// The frames stacked, in the order they were added.
    frames: Vec<FrameId>,
    /// The enabled frames left out, because their alignment was rejected.
    skipped: Vec<FrameId>,
    /// How many pixels no slab covers: transparent in the output (only wide framing has them).
    uncovered: usize,
    /// The frames in a slab, and how many neighboring slabs share at least.
    slab_size: usize,
    overlap: usize,
    /// The slabs, in order.
    slabs: Vec<SlabSummary>,
    /// The settings the slabs and the final stack were made with, and the framing.
    slab_settings: UsedSettings,
    final_settings: UsedSettings,
    framing: Framing,
    /// How the result was brought into range, when a pyramid made the slabs or the final stack.
    #[serde(skip_serializing_if = "Option::is_none")]
    range: Option<RangeSummary>,
    /// The final depth map's threshold and the pixels it filled.
    #[serde(skip_serializing_if = "Option::is_none")]
    depth: Option<DepthSummary>,
}

/// A slab's output for the commit: the one to keep, by its id and image's hash, or the one to add.
struct SlabOutput {
    kept: Option<(OutputId, ContentHash)>,
    output: NewOutput,
}

#[allow(clippy::too_many_lines)] // The plan, the two stages and the commit, in the order the job runs them.
fn slabbed(session: &Session, p: SlabbedParams) -> Result<JobPlan<SlabbedSummary>, CommandError> {
    let project_settings = stack_settings(session.project()?)?;
    let final_settings = p.final_stack.chosen(MethodName::Pyramid, &project_settings);
    final_settings.validate()?;
    let mut plan = StackPlan::new(session, COMMAND, p.slabs.chosen(MethodName::Pyramid, &project_settings), Quality::Output)?;
    let n = plan.frames.len();
    if n < MIN_FRAMES {
        return Err(session.unavailable(format!(
            "slabbing needs at least {MIN_FRAMES} frames to stack, and the stack has {n}: stack it with one method, such as `stack.pyramid`"
        )));
    }
    let size = match p.slab_size {
        Some(size) if size >= n => return Err(session.invalid("slabSize", format!("must be less than the number of frames stacked, {n}"))),
        Some(size) => size,
        None => default_slab_size(n),
    };
    let overlap = match p.overlap {
        Some(overlap) if overlap >= size => return Err(session.invalid("overlap", format!("must be less than the slab size, {size}"))),
        Some(overlap) => overlap,
        None => default_overlap(size),
    };
    let ranges = layout(n, size, overlap);
    if ranges.len() > MAX_SLABS {
        return Err(session.invalid(
            "slabSize",
            format!("and `overlap` make {} slabs of the {n} frames, more than {MAX_SLABS}: take larger slabs, or less overlap", ranges.len()),
        ));
    }
    let (width, height) = (plan.canvas.width, plan.canvas.height);
    let slab_parameters = plan.settings.parameters(plan.framing, width, height);
    let develop = plan.reader.develop_key()?;
    let project = session.project()?;
    let reuse = p.reuse.unwrap_or(true);
    let mut slabs = Vec::with_capacity(ranges.len());
    for range in ranges {
        let key = slab_key(&plan, &slab_parameters, &develop, &range)?;
        let reused = if reuse { reusable(project, &plan.cache, &key, plan.settings.name()) } else { None };
        slabs.push(Slab { range, key, reused });
    }
    let name = p.name.unwrap_or_else(|| format!("Slabbed {}", final_settings.title().to_lowercase()));
    let job = JobPlan::new(move |ctx| {
        ctx.stage(0);
        plan.reader.load_mask()?;
        let count = slabs.len();
        let mut made: Vec<Option<Stored>> = (0..count).map(|_| None).collect();
        let quiet = Quiet(ctx);
        let todo: Vec<usize> = slabs.iter().enumerate().filter(|(_, s)| s.reused.is_none()).map(|(j, _)| j).collect();
        let passes = plan.settings.passes();
        let mut units = Units::new(todo.iter().filter_map(|&j| slabs.get(j)).map(|s| passes * s.range.len()).sum());
        for j in todo {
            let (range, settings) = (slabs.get(j).map(|s| s.range.clone()).ok_or_else(|| plan.internal())?, plan.settings.clone());
            let mut feed = Warping { plan: &mut plan, range, reduced: false };
            let (stacked, depth) = stack_feed(&mut feed, &settings, &Tiling::default(), ctx, &quiet, &mut units, |_, _| {})?;
            let stored = store_slab(&plan, j, stacked, depth, &slabs)?;
            *made.get_mut(j).ok_or_else(|| plan.internal())? = Some(stored);
        }

        // The final stage, over the slabs' outputs.
        ctx.stage(1);
        let sources = slabs
            .iter()
            .zip(&made)
            .map(|(slab, made)| match (&slab.reused, made) {
                (Some(reused), _) => Ok(reused.source.clone()),
                (None, Some(stored)) => Ok(unclipped(&stored.image, &stored.maps).clone()),
                (None, None) => Err(plan.internal()),
            })
            .collect::<Result<Vec<CacheRef>, CommandError>>()?;
        let passes = final_settings.passes();
        let mut units = Units::new(passes * sources.len());
        let mut feed = Cached { cache: &plan.cache, sources: &sources, size: (width, height) };
        let quiet = Quiet(ctx);
        let (stacked, depth) = stack_feed(&mut feed, &final_settings, &Tiling::default(), ctx, &quiet, &mut units, |i, _| {
            if i == passes {
                ctx.stage(2);
            }
        })?;
        ctx.checkpoint()?;
        let tone = final_settings.ranged().or(plan.settings.ranged());
        let stem = format!("slabbed-{}", final_settings.name());
        let Stored { image, preview, maps, width, height, uncovered, range, depth } = store(&plan.cache, COMMAND, &stem, stacked, depth, tone, None)?;

        // What the commit adds.
        let develop = match plan.reader.resolved_develop() {
            Some((frame, develop)) => Some(develop_record(frame, develop)?),
            None => None,
        };
        let ids = plan.ids();
        let mut outputs = Vec::with_capacity(count);
        let mut summaries = Vec::with_capacity(count);
        for (j, (slab, made)) in slabs.into_iter().zip(made).enumerate() {
            let (start, len) = (slab.range.start, slab.range.len());
            summaries.push((start, len, slab.reused.is_some()));
            outputs.push(match (slab.reused, made) {
                (Some(reused), _) => SlabOutput { kept: Some((reused.id, reused.output.image.hash)), output: reused.output },
                (None, Some(stored)) => {
                    let mut parameters = slab_parameters.clone();
                    if let Some(object) = parameters.as_object_mut() {
                        object.insert("slab".into(), serde_json::json!({ "index": j, "of": count }));
                    }
                    let frames = ids.get(slab.range.clone()).ok_or_else(|| plan.internal())?;
                    SlabOutput {
                        kept: None,
                        output: NewOutput {
                            name: format!("{} slab {} of {count}", plan.settings.title(), j + 1),
                            method: plan.settings.name().to_owned(),
                            parameters,
                            inputs: frames.iter().copied().map(Input::Frame).collect(),
                            image: stored.image,
                            preview: Some(stored.preview),
                            maps: stored.maps,
                        },
                    }
                }
                (None, None) => return Err(plan.internal()),
            });
        }
        let mut parameters = final_settings.parameters(plan.framing, width, height);
        if let Some(object) = parameters.as_object_mut() {
            let slabs = serde_json::json!({
                "method": plan.settings.name(),
                "parameters": slab_parameters,
                "size": size,
                "overlap": overlap,
                "count": count,
            });
            object.insert("slabs".into(), slabs);
        }
        let method = final_settings.name();
        let (skipped, slab_settings, used, framing) = (plan.skipped.clone(), plan.settings.used(), final_settings.used(), plan.framing);
        Ok(commit(move |session: &mut Session| {
            let project = session.project()?;
            let mut edits = Vec::new();
            if let Some(develop) = develop {
                edits.push(Edit::SetDevelop(Some(develop)));
            }
            // A reused output is kept when the project still has it, unchanged; else it is added again.
            let mut kept = Vec::with_capacity(outputs.len());
            for SlabOutput { kept: k, output } in outputs {
                let still = k.filter(|&(id, hash)| project.outputs.iter().any(|o| o.id() == id && o.image.hash == hash));
                if still.is_none() {
                    edits.push(Edit::AddOutput(Box::new(output)));
                }
                kept.push(still.map(|(id, _)| id));
            }
            let added = kept.iter().filter(|k| k.is_none()).count();
            if !edits.is_empty() {
                session.apply(Edit::Batch(edits))?;
            }
            let internal = || CommandError::Internal { command: COMMAND.to_owned() };
            let all = &session.project()?.outputs;
            let mut new = all.iter().skip(all.len().saturating_sub(added)).map(florastack_project::Output::id);
            let slab_ids = kept.into_iter().map(|k| k.or_else(|| new.next())).collect::<Option<Vec<OutputId>>>().ok_or_else(internal)?;
            session.apply(Edit::AddOutput(Box::new(NewOutput {
                name,
                method: method.to_owned(),
                parameters,
                inputs: slab_ids.iter().copied().map(Input::Output).collect(),
                image,
                preview: Some(preview),
                maps,
            })))?;
            let output = session.project()?.outputs.last().map(florastack_project::Output::id).ok_or_else(internal)?;
            let slabs =
                slab_ids.into_iter().zip(summaries).map(|(output, (start, count, reused))| SlabSummary { output, start, count, reused }).collect();
            Ok(SlabbedSummary {
                output,
                width,
                height,
                frames: ids,
                skipped,
                uncovered,
                slab_size: size,
                overlap,
                slabs,
                slab_settings,
                final_settings: used,
                framing,
                range,
                depth,
            })
        }))
    });
    Ok(job.stages(&[("Stacking slabs", 0.85), ("Stacking the slabs", 0.13), ("Finishing", 0.02)]).locks(locks()))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn slabs_cover_every_frame_evenly_with_their_overlap() {
        for (n, size, overlap) in [(300, 18, 4), (300, 18, 1), (10, 4, 1), (3, 2, 1), (7, 6, 0), (1000, 32, 6), (5, 4, 3), (100_000, 2, 1)] {
            let slabs = layout(n, size, overlap);
            assert!(slabs.len() >= 2, "{n} {size} {overlap}");
            assert_eq!(slabs.first().map(|s| s.start), Some(0));
            assert_eq!(slabs.last().map(|s| s.end), Some(n));
            assert!(slabs.iter().all(|s| s.len() == size));
            for pair in slabs.windows(2) {
                let shared = pair[0].end.saturating_sub(pair[1].start);
                assert!(pair[1].start > pair[0].start && shared >= overlap && shared < size, "{n} {size} {overlap}: {pair:?}");
            }
            // As few slabs as the overlap allows.
            assert_eq!(slabs.len(), (n - overlap).div_ceil(size - overlap), "{n} {size} {overlap}");
        }
        assert_eq!(layout(300, 18, 4).len(), 22);
        assert_eq!(layout(10, 4, 1), [0..4, 3..7, 6..10]);
        // What isn't a layout makes one slab, and no frames none.
        for (size, overlap) in [(5, 1), (1, 0), (3, 3)] {
            assert_eq!(layout(5, size, overlap), std::iter::once(0..5).collect::<Vec<_>>());
        }
        assert_eq!(layout(0, 2, 1), Vec::new());
    }

    #[test]
    fn defaults_follow_the_number_of_frames() {
        let sizes: Vec<usize> = [3, 4, 10, 100, 300, 1000].iter().map(|&n| default_slab_size(n)).collect();
        assert_eq!(sizes, [2, 2, 4, 10, 18, 32]);
        let overlaps: Vec<usize> = [2, 4, 10, 18, 32].iter().map(|&s| default_overlap(s)).collect();
        assert_eq!(overlaps, [1, 1, 2, 4, 6]);
        for n in 3..500 {
            let size = default_slab_size(n);
            let overlap = default_overlap(size);
            assert!(size >= MIN_SLAB && size < n && overlap < size, "{n}");
            assert!(layout(n, size, overlap).len() >= 2, "{n}");
        }
    }

    #[test]
    fn an_alpha_channel_comes_off_as_coverage() {
        let image = Image::from_fn(3, 2, 4, florastack_core::ColorSpace::LINEAR, |x, y, c| (x * 10 + y * 100 + c) as f32).unwrap();
        let (color, alpha) = split_alpha(&image).unwrap();
        assert_eq!((color.channels(), alpha.channels()), (3, 1));
        assert_eq!((color.get(2, 1, 2), alpha.get(2, 1, 0)), (Some(122.0), Some(123.0)));
    }
}
