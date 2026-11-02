//! Scientific data conventions and the immutable input state recorded when an output is stacked.

use florastack_core::{ColorSpace, Image};
use florastack_project::{FrameId, Input, Output, Project};

use crate::error::CommandError;
use crate::session::Session;

pub(crate) fn linear(image: florastack_core::AnyImage) -> Result<Image<f32>, CommandError> {
    Ok(match image {
        florastack_core::AnyImage::U8(i) => i.to_linear_f32().map_err(florastack_stack::StackError::from)?,
        florastack_core::AnyImage::U16(i) => i.to_linear_f32().map_err(florastack_stack::StackError::from)?,
        florastack_core::AnyImage::F32(i) => i,
    })
}

/// An input snapshot contains no output pixels or previews. Non-regenerable maps keep cache references, which the
/// reproducibility sidecar embeds separately. Absolute input paths survive moving a project or its export.
pub(super) fn snapshot(session: &Session) -> Result<Project, CommandError> {
    let mut project = session.project()?.clone();
    let folder = session.folder()?;
    project.outputs.clear();
    project.preferences.set(florastack_project::Group::Develop, &crate::project::develop_settings(&project)?)?;
    project.preferences.set(florastack_project::Group::Align, &crate::project::align_settings(&project)?)?;
    project.preferences.set(florastack_project::Group::Stack, &crate::project::stack_settings(&project)?)?;
    project.preferences.set(florastack_project::Group::Output, &super::export::output_settings(&project)?)?;
    for frame in &mut project.frames {
        frame.path = super::display(&folder.frame_path(frame));
        frame.preview = None;
        frame.aligned_preview = None;
        frame.aligned = None;
        frame.maps.clear();
    }
    Ok(project)
}

pub(crate) fn source(project: &Project, output: &Output) -> Result<Project, CommandError> {
    match output.source_state.as_ref() {
        Some(value) => Ok((**value).clone()),
        None => Ok(project.clone()), // Projects from before scientific exports have no snapshot.
    }
}

/// Calibrations are measured in the master's pixels. Narrow framing may rescale the canvas to a different frame.
pub(super) fn canvas_calibration(source: &Project, output: &Output, ppm: Option<f64>) -> Result<Option<f64>, CommandError> {
    let Some(ppm) = ppm else { return Ok(None) };
    let Some(alignment) = crate::project::stored_alignment(source)? else { return Ok(Some(ppm)) };
    let canvas = alignment.canvas(crate::retouch::output_framing(output))?;
    let [[xx, _, _], [yx, _, _]] = canvas.to_master.m;
    let scale = xx.hypot(yx);
    if !scale.is_finite() || scale <= 0.0 {
        return Err(unavailable("the output canvas has an invalid scale"));
    }
    Ok(Some(ppm / scale))
}

/// The physical Z for each source frame, interpolated at the full-precision depth. The first frame in the recorded
/// capture order is at `origin`; omitted/rejected frames still occupy a step. A negative step reverses travel.
#[allow(clippy::cast_precision_loss, clippy::cast_possible_truncation, clippy::cast_sign_loss)] // Frame count is bounded; finite depth checked before indexing.
pub(super) fn calibrated_depth(depth: &Image<f32>, source: &Project, output: &Output, step: f64, origin: f64) -> Result<Image<f32>, CommandError> {
    if depth.channels() != 1 {
        return Err(unavailable("calibrated depth requires a single-channel frame-coordinate map"));
    }
    let ids: Vec<FrameId> = output.inputs.iter().filter_map(|i| if let Input::Frame(f) = i { Some(*f) } else { None }).collect();
    if ids.len() != output.inputs.len() || ids.is_empty() {
        return Err(unavailable("calibrated depth needs a depth map indexed by input frames; a slab depth indexes slabs"));
    }
    let positions = ids
        .iter()
        .map(|id| {
            source
                .frames
                .iter()
                .position(|f| f.id() == *id)
                .map(|i| origin + i as f64 * step)
                .ok_or_else(|| unavailable(format!("the captured position of {id} is missing")))
        })
        .collect::<Result<Vec<_>, _>>()?;
    let mut out = Image::new(depth.width(), depth.height(), 1, ColorSpace::LINEAR).map_err(florastack_stack::StackError::from)?;
    for (row, values) in out.rows_mut().zip(depth.rows()) {
        for (dst, &d) in row.iter_mut().zip(values) {
            if !d.is_finite() || d < 0.0 || f64::from(d) > ids.len().saturating_sub(1) as f64 {
                return Err(unavailable("the depth map contains an invalid frame coordinate"));
            }
            let low = d.floor() as usize;
            let a = positions.get(low).copied().ok_or_else(|| unavailable("depth frame is out of range"))?;
            let b = positions.get(low.saturating_add(1)).copied().unwrap_or(a);
            let z = a + (b - a) * f64::from(d.fract());
            if !z.is_finite() || z.abs() > f64::from(f32::MAX) {
                return Err(unavailable("calibrated depth exceeds float TIFF's range"));
            }
            *dst = z as f32;
        }
    }
    Ok(out)
}

pub(super) fn unavailable(reason: impl Into<String>) -> CommandError {
    CommandError::Unavailable { command: "export.run".into(), reason: reason.into() }
}
