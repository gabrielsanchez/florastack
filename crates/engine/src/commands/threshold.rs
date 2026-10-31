//! The depth map's contrast threshold, shown before it is used (`stack.threshold`, a preview job): which parts of the
//! stack fall below the threshold, so that their depth is filled from the confident pixels around them rather than
//! followed. The first call measures the frames' focus at a quarter of their size, as the depth-map method's first pass
//! does (`florastack_stack::Estimator`), and the session keeps what it found ([`FocusMemo`]); every later call with the
//! same frames, alignment, develop settings and radius only resolves the new threshold, in milliseconds, so that a
//! slider can follow it live. The picture is the sharpest frame's luminance at each pixel, black where the pixel is
//! below the threshold and transparent where no frame covers it, as an 8-bit TIFF in the project's `previews/` folder;
//! each call removes the one before.

use std::fmt::Write as _;
use std::path::PathBuf;
use std::sync::Arc;

use florastack_core::{AnyImage, ColorSpace, Image, Monitor, Sample};
use florastack_project::{Area, ContentHash};
use florastack_raw::Quality;
use florastack_stack::{DepthSettings, FocusStats, PREVIEW_FACTOR, StackError, StackShape, ThresholdMode, Tiling};
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

use super::stack::{Chosen, DepthOverrides, StackPlan, Units, Warping, each};
use crate::error::{CommandError, Invalid};
use crate::jobs::{JobPlan, Lane, commit};
use crate::project::{stack_settings, write_image_in};
use crate::registry::{CommandSpec, Meta, Params, Requires};
use crate::session::Session;

const COMMAND: &str = "stack.threshold";

pub(super) fn specs() -> Vec<CommandSpec> {
    vec![CommandSpec::job(
        Meta::new(COMMAND, "Show contrast threshold", "Shows which parts of the stack fall below the depth map's contrast threshold.")
            .help(
                "The depth-map method follows each pixel's sharpest frame only where that frame stands out clearly; \
                 below the contrast threshold, in smooth or blurred areas, it fills the depth from the confident \
                 pixels around. This shows where that happens, at a quarter of the frames' size: the sharpest frame's \
                 luminance at each pixel, black below the threshold, transparent where no frame covers it, as an 8-bit \
                 TIFF in the project's `previews/` folder (each call removes the one before), with the threshold's \
                 level and the share of the pixels below it. Takes the threshold settings of `stack.depthMap`, the \
                 project's (`stack.get`) when left out. The first call reads every frame, as the depth-map method's \
                 first pass does; later calls with the same frames, alignment, develop settings and radius reuse what \
                 it measured and take milliseconds, so call it on every move of a threshold slider. Runs beside other \
                 jobs as a preview; a newer call replaces an older one still running.",
            )
            .requires(Requires::Project)
            .supersede()
            .examples(&["{}", r#"{ "threshold": "percentile", "percentile": 40 }"#, r#"{ "threshold": "level", "level": 0.002 }"#]),
        Lane::Preview,
        threshold,
    )]
}

/// The threshold to show.
#[derive(Debug, Default, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields, rename_all = "camelCase")]
struct ThresholdParams {
    /// The estimation radius, in pixels of the full-size frames, from 0 to 64; the project's (`stack.get`) when left
    /// out.
    #[serde(default)]
    #[schemars(range(min = 0.0, max = 64.0))]
    radius: Option<f64>,
    /// How the contrast threshold is chosen: auto, level or percentile; the project's when left out.
    #[serde(default)]
    threshold: Option<ThresholdMode>,
    /// The threshold's absolute level, from 0, for `"threshold": "level"`; the project's when left out.
    #[serde(default)]
    #[schemars(range(min = 0.0))]
    level: Option<f64>,
    /// The share of the pixels to fill at least, in percent from 0 to 100, for `"threshold": "percentile"`; the
    /// project's when left out.
    #[serde(default)]
    #[schemars(range(min = 0.0, max = 100.0))]
    percentile: Option<f64>,
}

impl ThresholdParams {
    fn overrides(&self) -> DepthOverrides {
        DepthOverrides { radius: self.radius, smoothing: None, threshold: self.threshold, level: self.level, percentile: self.percentile }
    }
}

impl Params for ThresholdParams {
    fn check(&self) -> Result<(), Invalid> {
        self.overrides().check()
    }
}

/// Where the threshold falls.
#[derive(Debug, Serialize, JsonSchema)]
#[serde(rename_all = "camelCase")]
struct Shown {
    /// The picture: an 8-bit TIFF, gray with an alpha channel.
    path: String,
    /// Its size, in pixels: a quarter of the full stack's on each side.
    width: usize,
    height: usize,
    /// The threshold's level of contrast, which `"threshold": "level"` takes.
    level: f64,
    /// The share of the covered pixels below it, from 0 to 1: their depth is filled from around them.
    below: f64,
    /// Whether the frames' focus was measured again, rather than reused from the call before.
    measured: bool,
}

/// What `stack.threshold` measured last, kept in the session for the next threshold: the focus statistics with the
/// key of what they were measured from, and the picture shown last, which the next call removes.
pub(crate) struct FocusMemo {
    key: String,
    stats: Arc<FocusStats>,
    shown: Option<PathBuf>,
}

/// The key of the focus measured from `plan`'s frames with `depth`'s radius: everything it depends on, hashed.
fn focus_key(plan: &StackPlan, depth: &DepthSettings) -> Result<String, CommandError> {
    let c = &plan.canvas;
    let mut text =
        format!("focus, FloraStack {}\nradius {}\ncanvas {} {} {:?}\n", env!("CARGO_PKG_VERSION"), depth.radius, c.width, c.height, c.to_master.m);
    for f in &plan.frames {
        writeln!(text, "{} {:?} {}", plan.reader.frame_key(f.index)?, f.transform.m, f.gain).map_err(|_| plan.internal())?;
    }
    Ok(format!("focus {}", ContentHash::of(text.as_bytes())))
}

fn threshold(session: &Session, p: ThresholdParams) -> Result<JobPlan<Shown>, CommandError> {
    let depth = p.overrides().settings(&stack_settings(session.project()?)?.depth_map);
    let mut plan = StackPlan::new(session, COMMAND, Chosen::DepthMap(depth.clone()), Quality::Preview)?;
    let key = focus_key(&plan, &depth)?;
    let memo = session.focus.as_ref();
    let cached = memo.filter(|m| m.key == key).map(|m| Arc::clone(&m.stats));
    let previous = memo.and_then(|m| m.shown.clone());
    let job = JobPlan::new(move |ctx| {
        let reduced = depth.reduced(PREVIEW_FACTOR as f64);
        let measured = cached.is_none();
        let stats = if let Some(stats) = cached {
            stats
        } else {
            plan.reader.load_mask()?;
            let n = plan.frames.len();
            let mut units = Units::new(n);
            let mut estimator = reduced.estimator(&Tiling::default())?;
            let mut feed = Warping { plan: &mut plan, range: 0..n, reduced: true };
            each(&mut feed, ctx, &mut units, |loaded, progress, first| {
                loaded.source(|source| {
                    if first {
                        estimator.begin(StackShape::of(source))?;
                    }
                    Ok::<_, CommandError>(estimator.add_frame(source, progress)?)
                })
            })?;
            ctx.stage(1);
            Arc::new(estimator.finish(ctx)?)
        };
        ctx.checkpoint()?;
        let level = stats.level(reduced.contrast_threshold());
        let (picture, below) = picture(&stats, level)?;
        let reference = write_image_in(&plan.cache, Area::Previews, "stack-threshold", &AnyImage::U8(picture), None)?;
        let path = reference.path.resolve(plan.cache.path());
        let shown =
            Shown { path: path.display().to_string(), width: stats.width(), height: stats.height(), level: f64::from(level), below, measured };
        Ok(commit(move |session: &mut Session| {
            if let Some(old) = previous.filter(|old| *old != path)
                && let Err(e) = std::fs::remove_file(&old)
            {
                tracing::debug!("cannot remove the old threshold picture {}: {e}", old.display());
            }
            session.focus = Some(FocusMemo { key, stats, shown: Some(path) });
            Ok(shown)
        }))
    });
    Ok(job.stages(&[("Measuring focus", 0.95), ("Showing the threshold", 0.05)]))
}

/// The picture of `level` over `stats`: the guide (the sharpest frame's linear luminance) in sRGB, black where the
/// confidence is below `level`, transparent where no frame covers; and the share of the covered pixels below.
fn picture(stats: &FocusStats, level: f32) -> Result<(Image<u8>, f64), StackError> {
    let (w, h) = (stats.width(), stats.height());
    let mut out = Image::new(w, h, 2, ColorSpace::SRGB)?;
    let (mut covered, mut below) = (0_u64, 0_u64);
    for (y, row) in out.rows_mut().enumerate() {
        let (Some(guide), Some(confidence)) = (stats.guide.row(y), stats.confidence.row(y)) else { continue };
        let coverage = stats.covered.as_ref().and_then(|c| c.row(y));
        for (x, pixel) in row.as_chunks_mut::<2>().0.iter_mut().enumerate() {
            let covers = coverage.is_none_or(|c| c.get(x).is_some_and(|&k| k > 0.0));
            let (Some(&g), Some(&c)) = (guide.get(x), confidence.get(x)) else { continue };
            if !covers {
                pixel.copy_from_slice(&[0, 0]);
                continue;
            }
            covered += 1;
            let dark = c.is_nan() || c < level;
            below += u64::from(dark);
            let v = if dark { 0.0 } else { florastack_core::convert::linear_to_srgb(g.clamp(0.0, 1.0)) };
            pixel.copy_from_slice(&[u8::from_normalized(v), u8::MAX]);
        }
    }
    #[allow(clippy::cast_precision_loss)] // A share, shown rounded.
    let share = if covered > 0 { below as f64 / covered as f64 } else { 0.0 };
    Ok((out, share))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn stats(confidence: &[f32], covered: Option<&[f32]>) -> FocusStats {
        let n = confidence.len();
        let plane = |values: &[f32]| Image::from_vec(n, 1, 1, ColorSpace::LINEAR, values.to_vec()).unwrap();
        FocusStats {
            frames: 3,
            depth: plane(&vec![1.0; n]),
            confidence: plane(confidence),
            guide: plane(&vec![1.0; n]),
            covered: covered.map(plane),
            uncovered: covered.map_or(0, |c| c.iter().filter(|&&k| k == 0.0).count()),
            radius: 2.0,
        }
    }

    #[test]
    fn pixels_below_the_level_are_black_and_uncovered_ones_transparent() {
        let (image, below) = picture(&stats(&[0.5, 2.0, 0.1, 3.0], Some(&[1.0, 1.0, 1.0, 0.0])), 1.0).unwrap();
        assert_eq!(image.as_raw(), &[0, 255, 255, 255, 0, 255, 0, 0]);
        assert!((below - 2.0 / 3.0).abs() < 1e-9, "two of the three covered pixels are below: {below}");
        let (_, none) = picture(&stats(&[0.5, 2.0], None), 0.0).unwrap();
        assert_eq!(none, 0.0, "nothing is below a level of 0");
        let (image, all) = picture(&stats(&[f32::NAN, 0.0], None), 0.5).unwrap();
        assert_eq!((image.as_raw()[0], all), (0, 1.0), "a confidence that isn't a number counts as below");
    }
}
