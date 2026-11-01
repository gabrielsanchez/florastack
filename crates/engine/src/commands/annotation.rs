//! Non-destructive annotation layers. Their records live in the project; only exported copies are painted.

use florastack_core::{AnyImage, Image};
use florastack_project::{Edit, OutputId};
use font8x8::UnicodeFonts;
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

use crate::error::{CommandError, Invalid};
use crate::registry::{CommandSpec, Meta, Params, Requires};
use crate::session::Session;

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "kebab-case")]
pub enum Corner {
    TopLeft,
    TopRight,
    BottomLeft,
    #[default]
    BottomRight,
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "kebab-case")]
pub enum BarStyle {
    #[default]
    Solid,
    Bracket,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(tag = "kind", deny_unknown_fields, rename_all = "kebab-case", rename_all_fields = "camelCase")]
pub enum Annotation {
    ScaleBar {
        /// Physical length in micrometers; omit for an automatic 1/2/5 length up to a quarter of the image width.
        #[serde(default)]
        length: Option<f64>,
        /// Corner of the output; the margin and label are kept inside the image.
        #[serde(default)]
        position: Corner,
        /// A filled bar or a thin bar with end ticks.
        #[serde(default)]
        style: BarStyle,
    },
    Text {
        /// Label text (up to 200 characters, supported Latin and Greek glyphs).
        text: String,
        /// Top-left position as fractions of the output width and height, in [0, 1].
        position: [f64; 2],
    },
}

/// One independently named, visible or hidden annotation layer.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields, rename_all = "camelCase")]
pub struct Layer {
    /// The name in the layer list.
    pub name: String,
    /// Whether to render the layer.
    pub visible: bool,
    /// White ink on the image; false chooses black ink.
    pub white: bool,
    /// Glyph height in pixels, from 8 to 256 (rounded down to a multiple of 8).
    #[schemars(range(min = 8, max = 256))]
    pub text_size: usize,
    /// What this layer draws.
    pub annotation: Annotation,
}

impl Layer {
    pub fn validate(&self) -> Result<(), Invalid> {
        super::check_name("name", &self.name)?;
        if !(8..=256).contains(&self.text_size) {
            return Err(Invalid::new("textSize", "must be from 8 to 256 pixels"));
        }
        match &self.annotation {
            Annotation::ScaleBar { length: Some(v), .. } if !v.is_finite() || !(1e-9..=1e12).contains(v) => {
                Err(Invalid::new("length", "must be finite and from 1e-9 to 1e12 micrometers"))
            }
            Annotation::Text { text, position } => {
                super::check_name("text", text)?;
                if text.chars().any(|c| glyph(c).is_none()) {
                    return Err(Invalid::new("text", "contains a glyph the annotation font cannot render"));
                }
                if position.iter().any(|v| !v.is_finite() || !(0.0..=1.0).contains(v)) {
                    return Err(Invalid::new("position", "must contain two finite fractions in [0, 1]"));
                }
                Ok(())
            }
            Annotation::ScaleBar { .. } => Ok(()),
        }
    }
}

fn glyph(c: char) -> Option<[u8; 8]> {
    font8x8::BASIC_FONTS.get(c).or_else(|| font8x8::LATIN_FONTS.get(c)).or_else(|| font8x8::GREEK_FONTS.get(c))
}

pub(crate) fn layers(output: &florastack_project::Output) -> Result<Vec<Layer>, CommandError> {
    let layers: Vec<Layer> = florastack_project::from_data("annotation layers", &serde_json::json!(output.annotations))?;
    validate(&layers).map_err(|source| CommandError::Invalid { command: "annotation".into(), source })?;
    Ok(layers)
}

fn validate(layers: &[Layer]) -> Result<(), Invalid> {
    if layers.len() > 256 {
        return Err(Invalid::new("layers", "has more than 256 layers"));
    }
    layers.iter().try_for_each(Layer::validate)
}

pub(super) fn specs() -> Vec<CommandSpec> {
    vec![
        CommandSpec::query(
            Meta::new("annotation.get", "Annotation layers", "Lists an output's non-destructive annotation layers.")
                .requires(Requires::Project).examples(&[r#"{"output": 3}"#]),
            get,
        ),
        CommandSpec::instant(
            Meta::new("annotation.set", "Set annotation layers", "Replaces an output's annotation layers in one undo step.")
                .help("Layers are rendered onto exported copies only. Scale bars use the export's pixels_per_micrometer calibration; automatic lengths use 1, 2 or 5 times a power of ten. Text positions are fractions of the output size. Hidden layers stay saved. An empty list removes every layer.")
                .undoable().examples(&[r#"{"output": 3, "layers": [{"name": "Scale", "visible": true, "white": true, "textSize": 16, "annotation": {"kind": "scale-bar"}}]}"#]),
            set,
        ),
    ]
}

#[derive(Debug, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields, rename_all = "camelCase")]
struct OutputParams {
    /// Output ID from output.list.
    output: OutputId,
}
impl Params for OutputParams {}

#[derive(Debug, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields, rename_all = "camelCase")]
struct SetParams {
    /// Output ID from output.list.
    output: OutputId,
    /// Complete layer list, bottom first, at most 256 layers.
    layers: Vec<Layer>,
}
impl Params for SetParams {
    fn check(&self) -> Result<(), Invalid> {
        validate(&self.layers)
    }
}

#[derive(Debug, Serialize, JsonSchema)]
struct Layers {
    layers: Vec<Layer>,
}

fn get(session: &Session, p: OutputParams) -> Result<Layers, CommandError> {
    let output = session.project()?.output(p.output).ok_or(CommandError::NotFound { what: p.output.to_string() })?;
    Ok(Layers { layers: layers(output)? })
}

fn set(session: &mut Session, p: SetParams) -> Result<Layers, CommandError> {
    let records = p.layers.iter().map(|l| florastack_project::to_data("annotation layer", l)).collect::<Result<_, _>>()?;
    session.apply(Edit::SetAnnotations { output: p.output, layers: records })?;
    Ok(Layers { layers: p.layers })
}

/// A readable physical length no greater than `maximum` (1, 2 or 5 times a power of ten).
pub fn automatic_length(maximum: f64) -> f64 {
    let magnitude = 10.0_f64.powf(maximum.log10().floor());
    [5.0, 2.0, 1.0].into_iter().find(|v| v * magnitude <= maximum).unwrap_or(1.0) * magnitude
}

/// Rasterize validated layers on a copy, preserving the stored pixels and all data maps.
#[allow(clippy::cast_possible_truncation, clippy::cast_sign_loss)] // Sizes checked before bounded pixel conversions.
pub(crate) fn render(pixels: AnyImage, layers: &[Layer], calibration: Option<f64>) -> Result<AnyImage, CommandError> {
    if !layers.iter().any(|l| l.visible) {
        return Ok(pixels);
    }
    validate(layers).map_err(|source| CommandError::Invalid { command: "export.run".into(), source })?;
    let mut image = super::scientific::linear(pixels)?;
    let (w, h) = (image.width(), image.height());
    for layer in layers.iter().filter(|l| l.visible) {
        let scale = layer.text_size / 8;
        let ink = if layer.white { 1.0 } else { 0.0 };
        match &layer.annotation {
            Annotation::Text { text, position: [x, y] } => {
                let (x, y) = ((*x * w as f64) as usize, (*y * h as f64) as usize);
                if x.saturating_add(text.chars().count() * 8 * scale) > w || y.saturating_add(8 * scale) > h {
                    return Err(CommandError::Unavailable {
                        command: "export.run".into(),
                        reason: "the text label does not fit: reduce its size or move it inside the image".into(),
                    });
                }
                text_on(&mut image, text, x, y, scale, ink);
            }
            Annotation::ScaleBar { length, position, style } => {
                let ppm = calibration.ok_or_else(|| CommandError::Unavailable {
                    command: "export.run".into(),
                    reason: "a visible scale bar needs a pixels_per_micrometer calibration".into(),
                })?;
                let physical = length.unwrap_or_else(|| automatic_length(w as f64 / (4.0 * ppm)));
                let bar = (physical * ppm).round();
                let label = format!("{physical} µm");
                let label_width = label.chars().count().saturating_mul(8 * scale);
                let margin = (w.min(h) / 30).max(1);
                let width = (bar as usize).max(label_width);
                let height = 12 * scale;
                if !bar.is_finite() || bar < 1.0 || bar > w as f64 || width.saturating_add(2 * margin) > w || height.saturating_add(2 * margin) > h {
                    return Err(CommandError::Unavailable {
                        command: "export.run".into(),
                        reason: "the scale bar and label do not fit: reduce the length or text size".into(),
                    });
                }
                let x = if matches!(position, Corner::TopRight | Corner::BottomRight) { w - margin - width } else { margin };
                let y = if matches!(position, Corner::BottomLeft | Corner::BottomRight) { h - margin - height } else { margin };
                let bx = x + (width - bar as usize) / 2;
                rect(&mut image, bx, y, bar as usize, scale.max(1) * 2, ink);
                if *style == BarStyle::Bracket {
                    let tick = scale.min(bar as usize);
                    rect(&mut image, bx, y, tick, 4 * scale, ink);
                    rect(&mut image, bx + bar as usize - tick, y, tick, 4 * scale, ink);
                }
                text_on(&mut image, &label, x + (width - label_width) / 2, y + 4 * scale, scale, ink);
            }
        }
    }
    Ok(AnyImage::F32(image))
}

fn rect(image: &mut Image<f32>, x: usize, y: usize, w: usize, h: usize, ink: f32) {
    let channels = image.channels();
    let colors = if channels >= 3 { 3 } else { 1 };
    for j in y..y.saturating_add(h).min(image.height()) {
        for i in x..x.saturating_add(w).min(image.width()) {
            if let Some(pixel) = image.pixel_mut(i, j) {
                for v in pixel.iter_mut().take(colors) {
                    *v = ink;
                }
                if (channels == 2 || channels == 4)
                    && let Some(alpha) = pixel.last_mut()
                {
                    *alpha = 1.0;
                }
            }
        }
    }
}

fn text_on(image: &mut Image<f32>, text: &str, x: usize, y: usize, scale: usize, ink: f32) {
    for (i, c) in text.chars().enumerate() {
        if let Some(glyph) = glyph(c) {
            for (j, row) in glyph.into_iter().enumerate() {
                for bit in 0..8 {
                    if row & (1 << bit) != 0 {
                        rect(image, x.saturating_add((i * 8 + bit) * scale), y.saturating_add(j * scale), scale, scale, ink);
                    }
                }
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn automatic_scale_and_bracket_keep_the_calibrated_pixel_length() {
        let layer = Layer {
            name: "Scale".into(),
            visible: true,
            white: true,
            text_size: 16,
            annotation: Annotation::ScaleBar { length: None, position: Corner::TopLeft, style: BarStyle::Bracket },
        };
        let base = Image::new(200, 100, 3, florastack_core::ColorSpace::LINEAR).unwrap();
        let AnyImage::F32(image) = render(AnyImage::F32(base), &[layer], Some(2.0)).unwrap() else { panic!("float image") };
        // One quarter of 200 px / 2 px per µm is 25 µm: the automatic 1/2/5 choice is 20 µm, hence 40 px.
        // At two-pixel glyph scaling, bracket ticks must remain inside those same endpoints.
        assert_eq!(image.row(3).unwrap().as_chunks::<3>().0.iter().filter(|p| p.iter().any(|v| *v > 0.0)).count(), 40);
    }
}
