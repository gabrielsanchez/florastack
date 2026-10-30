//! Finding the stacks in a batch of files (`project.findStacks`, a job): a folder holding several stacks shot one
//! after another, of one subject or of several, is cut where a new stack starts, as florastack-align's `split.rs`
//! decides from each pair of neighbors: a long gap in their capture times, another scene, a jump in framing or
//! scale, or another frame size.
//!
//! The files are read one at a time, in the order given (a folder's frame files sorted by name, as
//! `project.addFrames` takes them): each one's capture time from its metadata, and its pixels, RAW frames with the fast
//! demosaic and one development for the batch, reduced to a thumbnail of florastack-align's `LINK_SIDE` pixels and
//! compared with the one before. Memory holds one frame and two thumbnails, however long the batch. Nothing in a
//! project changes, and none needs to be open: the result lists each stack's files, for a project each, or a batch
//! run.

use std::path::PathBuf;
use std::str::FromStr;

use florastack_align::{Boundary, LINK_SIDE, Neighbors, Reason, SplitSettings, boundaries, gap_threshold, link, thumbnail};
use florastack_codecs::ReadOptions;
use florastack_core::{Image, Monitor};
use florastack_project::Timestamp;
use florastack_raw::{DevelopSettings, Quality};
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

use super::frames::expand;
use super::project::NotDone;
use super::{Quiet, absolute, check_len, check_path, display, reason};
use crate::error::{CommandError, Invalid};
use crate::frames::FrameReader;
use crate::jobs::{JobPlan, Lane, commit};
use crate::registry::{CommandSpec, Meta, Params};
use crate::session::Session;

const COMMAND: &str = "project.findStacks";

pub(crate) fn specs() -> Vec<CommandSpec> {
    vec![CommandSpec::job(
        Meta::new("project.findStacks", "Find stacks", "Finds where each stack starts in a batch of files holding several, shot one after another.")
            .help(
                "`paths` lists files and folders, as `project.addFrames` takes them: a folder gives the frame files \
                 directly in it, sorted by name. Every file is read once, in that order, and compared with the one \
                 before it: a new stack starts where their capture times are further apart than `split.gap` seconds \
                 (by default 30 s, or 8 times the batch's median gap when that is longer), where the next frame shows \
                 another scene (it doesn't match the one before, scoring below `split.minScore`, and doesn't look \
                 like it once blurred, correlating below `split.minSimilarity`), where the framing jumps (the scale by \
                 more than `split.maxScaleStep` percent, or the view by more than `split.maxShift` percent of the \
                 diagonal) or where the frame size changes. Frames of one stack differ only in focus and a little in \
                 scale, so out-of-focus frames at a stack's ends stay with it. Needs no project and changes none: the \
                 result lists each stack's files, and every boundary with its reasons. Files that can't be read are \
                 listed in `skipped` and left out. Runs as a job.",
            )
            .examples(&[
                r#"{ "paths": ["C:/Stacks/2024-05-01"] }"#,
                r#"{ "paths": ["C:/Stacks/2024-05-01"], "split": { "gap": 60, "maxScaleStep": 3 } }"#,
            ]),
        Lane::Heavy,
        find_stacks,
    )]
}

/// Finds the stacks in a batch of files.
#[derive(Debug, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields, rename_all = "camelCase")]
struct FindStacksParams {
    /// Files and folders (at most 100000), in order: a folder gives the frame files directly in it, sorted by name.
    #[schemars(length(min = 1, max = 100_000))]
    paths: Vec<String>,
    /// When the batch is cut: the gap in capture time, in seconds (`gap`, chosen from the batch when left out), and
    /// the limits on neighbors (`minScore`, `minSimilarity`, `maxScaleStep`, `maxShift`); each left out is the default.
    #[serde(default)]
    split: SplitSettings,
}

impl Params for FindStacksParams {
    fn check(&self) -> Result<(), Invalid> {
        check_len("paths", &self.paths, 1)?;
        self.paths.iter().try_for_each(|p| check_path("paths", p))?;
        self.split.validate().map_err(|e| match e {
            florastack_align::AlignError::Setting { name, .. } => Invalid::new(format!("split.{}", camel(name)), e.to_string()),
            other => Invalid::new("split", other.to_string()),
        })
    }
}

/// A snake-case setting's name in camel case, as the parameters spell it.
fn camel(name: &str) -> String {
    let mut out = String::with_capacity(name.len());
    let mut upper = false;
    for c in name.chars() {
        if c == '_' {
            upper = true;
        } else if upper {
            out.extend(c.to_uppercase());
            upper = false;
        } else {
            out.push(c);
        }
    }
    out
}

/// A stack found in the batch.
#[derive(Debug, Serialize, JsonSchema)]
struct FoundStack {
    /// Its files, in order.
    files: Vec<String>,
}

/// Where a stack starts, after the first.
#[derive(Debug, Serialize, JsonSchema)]
struct FoundBoundary {
    /// Its first file.
    file: String,
    /// Its position among the files read (from 0).
    index: usize,
    /// Every reason that holds.
    reasons: Vec<Reason>,
}

/// The stacks of a batch.
#[derive(Debug, Serialize, JsonSchema)]
struct FoundStacks {
    /// The stacks, in the order of their files.
    stacks: Vec<FoundStack>,
    /// Where each stack after the first starts, and why.
    boundaries: Vec<FoundBoundary>,
    /// The gap in capture time that cuts the batch, in seconds: the one given, or the one chosen from the batch;
    /// absent when no file has a capture time.
    #[serde(skip_serializing_if = "Option::is_none")]
    gap: Option<f64>,
    /// The files left out, and why.
    skipped: Vec<NotDone>,
}

/// A capture time, ISO 8601 as florastack-codecs summarizes it, in seconds since 1970. A time without a UTC offset
/// (most cameras record none) is taken as UTC: only the gaps between times count.
pub(crate) fn capture_seconds(text: &str) -> Option<f64> {
    let text = text.trim();
    let parsed = Timestamp::from_str(text).or_else(|_| Timestamp::from_str(&format!("{text}Z"))).ok()?;
    #[allow(clippy::cast_precision_loss)] // Milliseconds since 1970: exact far beyond any capture time.
    Some(parsed.unix_millis() as f64 / 1000.0)
}

/// A file read for comparison: its capture time and its thumbnail.
struct Read {
    time: Option<f64>,
    thumbnail: Image<f32>,
}

/// Reads `path` for comparison with its neighbors.
fn read(reader: &mut FrameReader, path: &std::path::Path) -> Result<Read, CommandError> {
    let bytes =
        std::fs::read(path).map_err(|source| CommandError::from(florastack_project::ProjectError::Read { path: path.to_path_buf(), source }))?;
    let time = florastack_codecs::read_metadata(&bytes).summary().capture_time.as_deref().and_then(capture_seconds);
    let frame = reader
        .read(&bytes, Quality::Preview)
        .map_err(|source| CommandError::from(crate::project::ProjectDataError::ReadFrame { path: path.to_path_buf(), source }))?;
    drop(bytes);
    Ok(Read { time, thumbnail: thumbnail(&frame.image, LINK_SIDE)? })
}

fn find_stacks(_: &Session, p: FindStacksParams) -> Result<JobPlan<FoundStacks>, CommandError> {
    let paths = p.paths.iter().map(|path| absolute(path)).collect::<Result<Vec<PathBuf>, _>>()?;
    let settings = p.split;
    let job = JobPlan::new(move |ctx| {
        ctx.stage(0);
        let mut skipped = Vec::new();
        let files = expand(COMMAND, &paths, &mut skipped)?;
        ctx.stage(1);
        let mut reader = FrameReader::new(DevelopSettings::default(), ReadOptions::default());
        let (mut read_files, mut pairs) = (Vec::with_capacity(files.len()), Vec::new());
        let mut previous: Option<Read> = None;
        let total = files.len() as u64;
        for (n, file) in files.iter().enumerate() {
            ctx.checkpoint()?;
            match read(&mut reader, file) {
                Ok(next) => {
                    if let Some(before) = &previous {
                        let gap = before.time.zip(next.time).map(|(a, b)| b - a);
                        let link = match link(&before.thumbnail, &next.thumbnail, &Quiet(ctx)) {
                            Ok(link) => Some(link),
                            Err(florastack_align::AlignError::Cancelled(c)) => return Err(c.into()),
                            // Thumbnails alignment can't take: compared by their capture times alone.
                            Err(_) => None,
                        };
                        pairs.push(Neighbors { gap, link });
                    }
                    read_files.push(display(file));
                    previous = Some(next);
                }
                Err(CommandError::Cancelled) => return Err(CommandError::Cancelled),
                Err(e) => skipped.push(NotDone { path: display(file), reason: reason(&e) }),
            }
            ctx.step(n as u64 + 1, total)?;
        }
        let cuts: Vec<Boundary> = boundaries(&pairs, &settings);
        let gap = gap_threshold(&pairs, &settings);
        let mut stacks = Vec::with_capacity(cuts.len() + 1);
        let mut start = 0;
        for end in cuts.iter().map(|c| c.index).chain(std::iter::once(read_files.len())) {
            if let Some(files) = read_files.get(start..end)
                && !files.is_empty()
            {
                stacks.push(FoundStack { files: files.to_vec() });
            }
            start = end;
        }
        let boundaries = cuts
            .into_iter()
            .filter_map(|c| read_files.get(c.index).map(|file| FoundBoundary { file: file.clone(), index: c.index, reasons: c.reasons }))
            .collect();
        let found = FoundStacks { stacks, boundaries, gap, skipped };
        Ok(commit(move |_: &mut Session| Ok(found)))
    });
    Ok(job.stages(&[("Listing files", 0.01), ("Comparing frames", 0.99)]))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    #[allow(clippy::float_cmp)] // Whole seconds and milliseconds, exact in f64.
    fn capture_times_are_read_with_or_without_an_offset() {
        let local = capture_seconds("2024-05-01T12:00:00").unwrap();
        assert_eq!(capture_seconds("2024-05-01T12:00:01.250").unwrap() - local, 1.25);
        assert_eq!(capture_seconds("2024-05-01T12:00:00Z").unwrap(), local);
        assert_eq!(capture_seconds("2024-05-01T14:00:00+02:00").unwrap(), local);
        assert_eq!(capture_seconds(" 2024-05-01 12:01:00 ").unwrap() - local, 60.0);
        for bad in ["", "yesterday", "2024-13-01T00:00:00", "2024-05-01"] {
            assert_eq!(capture_seconds(bad), None, "{bad}");
        }
    }

    #[test]
    fn setting_names_are_spelled_as_the_parameters_spell_them() {
        assert_eq!(camel("max_scale_step"), "maxScaleStep");
        assert_eq!(camel("gap"), "gap");
    }
}
