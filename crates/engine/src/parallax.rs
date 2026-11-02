//! Relative-depth reliefs and synthetic camera views. Depth zero is nearest; one is farthest.
//! These are illustrations of focus order, not a reconstruction of hidden surfaces or metric geometry.

pub(crate) mod formats;
pub use formats::{glb, obj};

use florastack_core::{ColorSpace, Image, Monitor};
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};
use std::io::Read as _;

use crate::{CommandError, Invalid};

/// Shared surface preparation for mesh, stereo and animation outputs.
#[derive(Clone, Debug, Serialize, Deserialize, JsonSchema)]
#[serde(default, deny_unknown_fields, rename_all = "camelCase")]
pub struct Settings {
    /// Longest texture or rendered image side, in pixels, from 16 to 2048.
    #[schemars(range(min = 16, max = 2048))]
    pub max_side: usize,
    /// Longest mesh grid side, from 2 to 512.
    #[schemars(range(min = 2, max = 512))]
    pub mesh_side: usize,
    /// Minimum confidence, from zero to one. Samples below it remain holes after smoothing.
    #[schemars(range(min = 0.0, max = 1.0))]
    pub confidence: f32,
    /// Confidence-weighted smoothing radius in rendered pixels, from 0 to 16.
    #[schemars(range(min = 0, max = 16))]
    pub smoothing: usize,
    /// Reverse focus order when the first input frame is farthest from the camera.
    pub reverse_depth: bool,
    /// Mesh relief height as a fraction of the image's longest side, from 0.001 to 1.
    #[schemars(range(min = 0.001, max = 1.0))]
    pub relief: f32,
    /// Maximum normalized depth difference across a triangle; larger discontinuities leave holes.
    #[schemars(range(min = 0.0, max = 1.0))]
    pub max_depth_jump: f32,
    /// Stereo camera separation as a fraction of image width, from zero to 0.25.
    #[schemars(range(min = 0.0, max = 0.25))]
    pub baseline: f32,
}

impl Default for Settings {
    fn default() -> Self {
        Self {
            max_side: 1024,
            mesh_side: 256,
            confidence: 0.5,
            smoothing: 2,
            reverse_depth: false,
            relief: 0.2,
            max_depth_jump: 0.25,
            baseline: 0.04,
        }
    }
}

impl Settings {
    pub fn check(&self) -> Result<(), Invalid> {
        for (key, value, min, max) in
            [("maxSide", self.max_side, 16, 2048), ("meshSide", self.mesh_side, 2, 512), ("smoothing", self.smoothing, 0, 16)]
        {
            if !(min..=max).contains(&value) {
                return Err(Invalid::new(key, format!("must be from {min} to {max}")));
            }
        }
        for (key, value, min, max) in [
            ("confidence", self.confidence, 0.0, 1.0),
            ("relief", self.relief, 0.001, 1.0),
            ("maxDepthJump", self.max_depth_jump, 0.0, 1.0),
            ("baseline", self.baseline, 0.0, 0.25),
        ] {
            if !value.is_finite() || !(min..=max).contains(&value) {
                return Err(Invalid::new(key, format!("must be finite and from {min} to {max}")));
            }
        }
        Ok(())
    }
}

pub(crate) fn invalid(reason: impl Into<String>) -> CommandError {
    CommandError::Unavailable { command: "parallax".into(), reason: reason.into() }
}

/// A textured relief, shared by the desktop renderer and the file exporters. UV origin is top left.
#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Scene {
    /// Position XYZ followed by UV; X right, Y up, positive Z toward the viewer.
    pub vertices: Vec<[f32; 5]>,
    pub indices: Vec<u32>,
    pub width: usize,
    pub height: usize,
    /// sRGB RGBA8 texture, top row first, with straight alpha.
    pub rgba: Vec<u8>,
}

impl Scene {
    pub fn validate(&self) -> Result<(), CommandError> {
        if self.width == 0
            || self.height == 0
            || self.width.max(self.height) > 2048
            || self.width.checked_mul(self.height).and_then(|n| n.checked_mul(4)) != Some(self.rgba.len())
            || self.vertices.is_empty()
            || self.vertices.len() > 512 * 512
            || self.indices.is_empty()
            || !self.indices.len().is_multiple_of(3)
            || self.indices.len() > 512 * 512 * 6
            || self.vertices.iter().flatten().any(|v| !v.is_finite())
            || self.indices.iter().any(|&i| usize::try_from(i).map_or(true, |i| i >= self.vertices.len()))
        {
            return Err(invalid("invalid or oversized textured mesh"));
        }
        Ok(())
    }
}

/// Reads an engine-created preview. Length and geometry are checked before it reaches wgpu.
pub fn load_scene(path: &std::path::Path) -> Result<Scene, CommandError> {
    let read = |source| florastack_project::ProjectError::Read { path: path.into(), source };
    let mut file = std::fs::File::open(path).map_err(read)?;
    let mut bytes = Vec::new();
    file.by_ref().take(64 * 1024 * 1024 + 1).read_to_end(&mut bytes).map_err(read)?;
    if bytes.len() > 64 * 1024 * 1024 {
        return Err(invalid("3D preview exceeds 64 MiB"));
    }
    let scene: Scene = serde_json::from_slice(&bytes).map_err(|source| CommandError::Params { command: "parallax.preview".into(), source })?;
    scene.validate()?;
    Ok(scene)
}

/// A bounded image with normalized relative depth and confidence masking.
pub struct Surface {
    width: usize,
    height: usize,
    rgba: Vec<u8>,
    depth: Vec<f32>,
    valid: Vec<bool>,
}

impl Surface {
    /// `depth` is normalized to [0,1], independent of the current project's frame order.
    pub fn new(
        image: &Image<f32>,
        depth: &Image<f32>,
        confidence: &Image<f32>,
        settings: &Settings,
        monitor: &dyn Monitor,
    ) -> Result<Self, CommandError> {
        settings.check().map_err(|source| CommandError::Invalid { command: "parallax".into(), source })?;
        let size = (image.width(), image.height());
        if size.0.max(size.1) > 1_000_000 {
            return Err(invalid("input image sides exceed the supported one million pixels"));
        }
        if size.0 < 2
            || size.1 < 2
            || (depth.width(), depth.height()) != size
            || (confidence.width(), confidence.height()) != size
            || depth.channels() != 1
            || confidence.channels() != 1
        {
            return Err(invalid("the composite, depth and confidence maps must have matching dimensions of at least 2 by 2"));
        }
        let longest = size.0.max(size.1);
        let w = (size.0 * settings.max_side.min(longest) / longest).max(2);
        let h = (size.1 * settings.max_side.min(longest) / longest).max(2);
        // Reduce before allocating display copies; depth and color sample the same source coordinates.
        let reduced = Image::from_fn(w, h, image.channels(), image.color_space().clone(), |x, y, c| {
            let sx = x * (size.0 - 1) / (w - 1);
            let sy = y * (size.1 - 1) / (h - 1);
            image.row(sy).and_then(|r| r.get(sx * image.channels() + c)).copied().unwrap_or(0.0)
        })
        .map_err(florastack_stack::StackError::from)?;
        let display = crate::view::display(&reduced, None).map_err(|e| invalid(e.to_string()))?;
        let pixels = display.to_srgb8();
        let mut out =
            Self { width: w, height: h, rgba: Vec::with_capacity(w * h * 4), depth: Vec::with_capacity(w * h), valid: Vec::with_capacity(w * h) };
        let mut weighted = Vec::with_capacity(w * h);
        let mut weights = Vec::with_capacity(w * h);
        for y in 0..h {
            monitor.step(y as u64, h as u64)?;
            let sy = y * (size.1 - 1) / (h - 1);
            for x in 0..w {
                let sx = x * (size.0 - 1) / (w - 1);
                let d = depth.row(sy).and_then(|r| r.get(sx)).copied().unwrap_or(f32::NAN);
                let c = confidence.row(sy).and_then(|r| r.get(sx)).copied().unwrap_or(f32::NAN);
                let pixel = pixels.get((y * w + x) * 4..(y * w + x) * 4 + 4).ok_or_else(|| invalid("missing composite pixel"))?;
                let valid = d.is_finite()
                    && (0.0..=1.0).contains(&d)
                    && c.is_finite()
                    && (settings.confidence..=1.0).contains(&c)
                    && pixel.get(3).copied().unwrap_or(0) > 0;
                let d = if settings.reverse_depth { 1.0 - d } else { d };
                let weight = if valid { c.max(0.0001) } else { 0.0 };
                out.rgba.extend_from_slice(pixel);
                out.depth.push(if valid { d } else { 0.5 });
                out.valid.push(valid);
                weighted.push(if valid { d * weight } else { 0.0 });
                weights.push(weight);
            }
        }
        if !out.valid.iter().any(|v| *v) {
            return Err(invalid("no confident depth remains: lower the confidence threshold"));
        }
        if settings.smoothing > 0 {
            box_sum(&mut weighted, w, h, settings.smoothing, monitor)?;
            box_sum(&mut weights, w, h, settings.smoothing, monitor)?;
            for ((d, sum), weight) in out.depth.iter_mut().zip(weighted).zip(weights) {
                if weight > 0.0 {
                    *d = (sum / weight).clamp(0.0, 1.0);
                }
            }
        }
        Ok(out)
    }

    pub fn mesh(&self, settings: &Settings, monitor: &dyn Monitor) -> Result<Scene, CommandError> {
        settings.check().map_err(|source| CommandError::Invalid { command: "parallax".into(), source })?;
        let longest = self.width.max(self.height);
        let w = (self.width * settings.mesh_side.min(longest) / longest).max(2);
        let h = (self.height * settings.mesh_side.min(longest) / longest).max(2);
        let mut scene = Scene { vertices: Vec::new(), indices: Vec::new(), width: self.width, height: self.height, rgba: self.rgba.clone() };
        let mut grid = vec![None; w * h];
        let mut depths = vec![0.0; w * h];
        for y in 0..h {
            monitor.step(y as u64, h as u64)?;
            for x in 0..w {
                let (sx, sy) = (x * (self.width - 1) / (w - 1), y * (self.height - 1) / (h - 1));
                let at = sy * self.width + sx;
                if !self.valid.get(at).copied().unwrap_or(false) {
                    continue;
                }
                let d = self.depth.get(at).copied().ok_or_else(|| invalid("missing depth pixel"))?;
                let u = sx as f32 / (self.width - 1) as f32;
                let v = sy as f32 / (self.height - 1) as f32;
                let index = u32::try_from(scene.vertices.len()).map_err(|_| invalid("too many mesh vertices"))?;
                if let Some(cell) = grid.get_mut(y * w + x) {
                    *cell = Some(index);
                }
                if let Some(cell) = depths.get_mut(y * w + x) {
                    *cell = d;
                }
                scene.vertices.push([
                    (u - 0.5) * self.width as f32 / longest as f32,
                    (0.5 - v) * self.height as f32 / longest as f32,
                    (0.5 - d) * settings.relief,
                    u,
                    v,
                ]);
            }
        }
        for y in 0..h - 1 {
            monitor.step(y as u64, h as u64)?;
            for x in 0..w - 1 {
                // A coarse grid must not bridge a narrow confidence hole between its vertices.
                let (x0, x1) = (x * (self.width - 1) / (w - 1), (x + 1) * (self.width - 1) / (w - 1));
                let (y0, y1) = (y * (self.height - 1) / (h - 1), (y + 1) * (self.height - 1) / (h - 1));
                if (y0..=y1).any(|row| self.valid.get(row * self.width + x0..row * self.width + x1 + 1).is_none_or(|r| r.iter().any(|v| !v))) {
                    continue;
                }
                let a = y * w + x;
                for triangle in [[a, a + w, a + 1], [a + 1, a + w, a + w + 1]] {
                    let ids = triangle.map(|i| grid.get(i).copied().flatten());
                    if let [Some(a), Some(b), Some(c)] = ids {
                        let ds = triangle.map(|i| depths.get(i).copied().unwrap_or(0.0));
                        let min = ds.into_iter().fold(f32::INFINITY, f32::min);
                        let max = ds.into_iter().fold(f32::NEG_INFINITY, f32::max);
                        if max - min <= settings.max_depth_jump {
                            scene.indices.extend([a, b, c]);
                        }
                    }
                }
            }
        }
        if scene.indices.is_empty() {
            return Err(invalid("no surface triangles remain: lower confidence, increase mesh resolution or maximum depth jump"));
        }
        scene.validate()?;
        Ok(scene)
    }

    /// Parallel, off-axis cameras converged on depth 0.5. A nearer point moves right in the left eye.
    /// Newly exposed surfaces are unknown and remain transparent. Z buffering keeps foreground in front.
    #[allow(clippy::cast_possible_truncation, clippy::cast_sign_loss)] // Finite bounded shifts and explicit pixel bounds precede casts.
    pub fn view(&self, camera: f32, baseline: f32, monitor: &dyn Monitor) -> Result<Image<u8>, CommandError> {
        if !camera.is_finite() || camera.abs() > 0.5 || !baseline.is_finite() || !(0.0..=0.25).contains(&baseline) {
            return Err(invalid("invalid camera offset or baseline"));
        }
        let mut out = Image::new(self.width, self.height, 4, ColorSpace::SRGB).map_err(florastack_stack::StackError::from)?;
        for (y, row) in out.rows_mut().enumerate() {
            monitor.step(y as u64, self.height as u64)?;
            let mut z = vec![f32::INFINITY; self.width];
            for x in 0..self.width {
                let i = y * self.width + x;
                if !self.valid.get(i).copied().unwrap_or(false) {
                    continue;
                }
                let d = self.depth.get(i).copied().unwrap_or(0.5);
                let px = (x as f32 - camera * baseline * self.width as f32 * (0.5 - d)).round();
                if px < 0.0 || px >= self.width as f32 {
                    continue;
                }
                let dx = px as usize;
                if let Some(z) = z.get_mut(dx)
                    && d < *z
                {
                    *z = d;
                    if let (Some(dst), Some(src)) = (row.get_mut(dx * 4..dx * 4 + 4), self.rgba.get(i * 4..i * 4 + 4)) {
                        dst.copy_from_slice(src);
                    }
                }
            }
        }
        Ok(out)
    }
}

fn box_sum(data: &mut [f32], w: usize, h: usize, radius: usize, monitor: &dyn Monitor) -> Result<(), CommandError> {
    let mut temp = vec![0.0; data.len()];
    for y in 0..h {
        monitor.step(y as u64, h as u64)?;
        let mut sum = 0.0;
        for x in 0..w + radius {
            if x < w {
                sum += data.get(y * w + x).copied().unwrap_or(0.0);
            }
            if x > 2 * radius {
                sum -= data.get(y * w + x - 2 * radius - 1).copied().unwrap_or(0.0);
            }
            if x >= radius
                && let Some(dst) = temp.get_mut(y * w + x - radius)
            {
                *dst = sum;
            }
        }
    }
    for x in 0..w {
        monitor.step(x as u64, w as u64)?;
        let mut sum = 0.0;
        for y in 0..h + radius {
            if y < h {
                sum += temp.get(y * w + x).copied().unwrap_or(0.0);
            }
            if y > 2 * radius {
                sum -= temp.get((y - 2 * radius - 1) * w + x).copied().unwrap_or(0.0);
            }
            if y >= radius
                && let Some(dst) = data.get_mut((y - radius) * w + x)
            {
                *dst = sum;
            }
        }
    }
    Ok(())
}

/// Packs eyes for parallel or cross-eyed viewing, or selects red from left and cyan from right.
pub fn stereo(left: &Image<u8>, right: &Image<u8>, mode: Stereo) -> Result<Image<u8>, CommandError> {
    if (left.width(), left.height(), left.channels()) != (right.width(), right.height(), 4) {
        return Err(invalid("stereo views must be matching RGBA images"));
    }
    let width =
        if mode == Stereo::Anaglyph { left.width() } else { left.width().checked_mul(2).ok_or_else(|| invalid("stereo pair is too wide"))? };
    let mut out = Image::new(width, left.height(), 4, ColorSpace::SRGB).map_err(florastack_stack::StackError::from)?;
    for ((dst, l), r) in out.rows_mut().zip(left.rows()).zip(right.rows()) {
        if mode == Stereo::Anaglyph {
            for ((dst, l), r) in dst.as_chunks_mut::<4>().0.iter_mut().zip(l.as_chunks::<4>().0).zip(r.as_chunks::<4>().0) {
                dst.copy_from_slice(&[l[0], r[1], r[2], l[3].min(r[3])]);
            }
        } else {
            let (first, second) = if mode == Stereo::Parallel { (l, r) } else { (r, l) };
            let (a, b) = dst.split_at_mut(first.len());
            a.copy_from_slice(first);
            b.copy_from_slice(second);
        }
    }
    Ok(out)
}

/// Eye order of a PNG stereo export.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "kebab-case")]
pub enum Stereo {
    #[default]
    Parallel,
    CrossEyed,
    Anaglyph,
}

/// One smooth periodic camera excursion, with no repeated endpoint at the loop boundary.
pub fn rocking_offset(frame: u16, frames: u16) -> f32 {
    0.5 * (std::f32::consts::TAU * f32::from(frame) / f32::from(frames.max(1))).sin()
}

pub fn animation(surface: &Surface, settings: &Settings, frames: u16, fps: u16, monitor: &dyn Monitor) -> Result<Vec<u8>, CommandError> {
    if !(4..=120).contains(&frames) || !(1..=50).contains(&fps) {
        return Err(invalid("animation needs 4 to 120 frames and 1 to 50 fps"));
    }
    let error = |e: gif::EncodingError| florastack_project::ProjectError::Write { path: "parallax.gif".into(), source: std::io::Error::other(e) };
    let w = u16::try_from(surface.width).map_err(|_| invalid("animation too wide"))?;
    let h = u16::try_from(surface.height).map_err(|_| invalid("animation too tall"))?;
    let mut encoder = gif::Encoder::new(Vec::new(), w, h, &[]).map_err(error)?;
    encoder.set_repeat(gif::Repeat::Infinite).map_err(error)?;
    for i in 0..frames {
        monitor.step(u64::from(i), u64::from(frames))?;
        let view = surface.view(rocking_offset(i, frames), settings.baseline, &crate::commands::Quiet(monitor))?;
        let rgb: Vec<u8> =
            view.rows().flat_map(|r| r.as_chunks::<4>().0.iter().flat_map(|p| if p[3] == 0 { [0, 0, 0] } else { [p[0], p[1], p[2]] })).collect();
        let mut frame = gif::Frame::from_rgb_speed(w, h, &rgb, 10);
        // Round cumulative times, rather than each frame, so fractional GIF centiseconds do not drift.
        frame.delay = u16::try_from(
            (u32::from(i + 1) * 100 + u32::from(fps) / 2) / u32::from(fps) - (u32::from(i) * 100 + u32::from(fps) / 2) / u32::from(fps),
        )
        .map_err(|_| invalid("invalid GIF frame delay"))?;
        frame.dispose = gif::DisposalMethod::Background;
        encoder.write_frame(&frame).map_err(error)?;
    }
    Ok(encoder.into_inner().map_err(error)?)
}
