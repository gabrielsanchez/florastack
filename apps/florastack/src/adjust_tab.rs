//! The Adjust tab: the output with its adjustments in the view, and in the right panel the whole image's sliders, the
//! masks and the selected mask's shapes and sliders. Everything goes through the engine's `adjust.*` commands: a
//! slider's drag is one undo step (`adjust.set` or `adjust.setMask`, coalesced), adding, removing and painting a mask
//! are one each. The view shows the engine's adjust session (`florastack_engine::adjust::AdjustView`): a preview at the
//! view's size, rendered as a draft while a slider moves and again in full when it stops, with the output as it was
//! beside it on request (Before), the selected mask tinted (Show mask), and handles on the image for the selected
//! mask's gradients and ellipses; dragging on the image paints the selected mask's brush.

use egui::{Pos2, Rect, RichText, Sense, Vec2};
use florastack_engine::adjust::{Component, Global, Local, MAX_SAMPLES, Mask, Op, Settings, Shape, Stroke};
use florastack_engine::json;

use crate::app::FloraStackApp;
use crate::i18n::{tr, tr_args};
use crate::viewer::{Picture, draw, fit_zoom};

/// How close to a handle, in points, a press grabs it.
const GRAB: f32 = 10.0;

/// The shapes a mask can be made of, with their labels' keys, as the Add mask menu lists them.
pub const SHAPES: [(&str, &str); 9] = [
    ("subject", "adjust-shape-subject"),
    ("background", "adjust-shape-background"),
    ("depth", "adjust-shape-depth"),
    ("linear", "adjust-shape-linear"),
    ("radial", "adjust-shape-radial"),
    ("brush", "adjust-shape-brush"),
    ("luminance", "adjust-shape-luminance"),
    ("color", "adjust-shape-color"),
    ("whole", "adjust-shape-whole"),
];

/// A shape of `kind`, with the settings it starts with.
pub fn new_shape(kind: &str) -> Shape {
    match kind {
        "subject" => Shape::Subject,
        "background" => Shape::Background,
        "depth" => Shape::Depth { from: 0.0, to: 0.3, feather: 0.1 },
        "linear" => Shape::Linear { start: [0.5, 0.0], end: [0.5, 0.5] },
        "radial" => Shape::Radial { center: [0.5, 0.5], radius_x: 0.25, radius_y: 0.2, angle: 0.0, feather: 50.0 },
        "brush" => Shape::Brush { strokes: Vec::new() },
        "luminance" => Shape::Luminance { low: 0.75, high: 1.0, low_feather: 0.1, high_feather: 0.0 },
        "color" => Shape::Color { samples: Vec::new(), refine: 50.0 },
        _ => Shape::Whole,
    }
}

/// The label key of a shape.
fn shape_label(shape: &Shape) -> &'static str {
    match shape {
        Shape::Subject => "adjust-shape-subject",
        Shape::Background => "adjust-shape-background",
        Shape::Depth { .. } => "adjust-shape-depth",
        Shape::Linear { .. } => "adjust-shape-linear",
        Shape::Radial { .. } => "adjust-shape-radial",
        Shape::Brush { .. } => "adjust-shape-brush",
        Shape::Luminance { .. } => "adjust-shape-luminance",
        Shape::Color { .. } => "adjust-shape-color",
        Shape::Whole => "adjust-shape-whole",
        Shape::Object { .. } => "adjust-shape-object",
        Shape::Described { .. } => "adjust-shape-described",
    }
}

/// A handle of the selected mask being dragged: the component, and which of its points.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Handle {
    Start(usize),
    End(usize),
    Center(usize),
    RadiusX(usize),
    RadiusY(usize),
}

/// The Adjust tab's state: what it shows of the engine's adjust session, the adjustments it edits, and what is being
/// dragged or painted.
#[allow(clippy::struct_excessive_bools)] // Four independent toggles of the panel.
pub struct AdjustUi {
    /// The output open, and its adjustments as the tab edits them (read again when the project changes).
    pub output: Option<u64>,
    pub settings: Settings,
    /// The project's revision the settings were read at.
    read: Option<u64>,
    /// The mask the panel and the handles show.
    pub selected: Option<u32>,
    preview: Option<Picture>,
    before: Option<Picture>,
    /// Shows the output without its adjustments (Before), and the selected mask tinted (Show mask).
    pub compare: bool,
    pub show_mask: bool,
    /// A preview is wanted: as a draft (a slider or a handle moves) or in full.
    pending: Option<bool>,
    /// The size previews are asked for, in pixels, and the size last asked.
    wanted: (usize, usize),
    asked_size: (usize, usize),
    /// The output the tab opened on its own, once, when it was shown with none open.
    asked: Option<u64>,
    dragging: Option<Handle>,
    /// The brush: its radius as a fraction of the long edge, feather and flow, and whether it erases; the stroke being
    /// painted, in fractions of the image.
    pub brush_size: f64,
    pub brush_feather: f64,
    pub brush_flow: f64,
    pub erase: bool,
    stroke: Vec<[f64; 2]>,
    /// The next click on the image samples a color for the selected mask's color range.
    pub picking: bool,
    /// Where the image was last drawn, on screen (the tests paint into it).
    pub image_rect: Option<Rect>,
}

impl Default for AdjustUi {
    fn default() -> AdjustUi {
        AdjustUi {
            output: None,
            settings: Settings::default(),
            read: None,
            selected: None,
            preview: None,
            before: None,
            compare: false,
            show_mask: false,
            pending: None,
            wanted: (0, 0),
            asked_size: (0, 0),
            asked: None,
            dragging: None,
            brush_size: 0.03,
            brush_feather: 50.0,
            brush_flow: 100.0,
            erase: false,
            stroke: Vec::new(),
            picking: false,
            image_rect: None,
        }
    }
}

impl AdjustUi {
    /// Whether a preview of the output is shown.
    pub fn is_shown(&self) -> bool {
        self.preview.is_some()
    }

    /// Asks for a preview: a draft while something moves, in full otherwise (a full one wins over a draft).
    fn want_preview(&mut self, draft: bool) {
        self.pending = Some(self.pending.map_or(draft, |d| d && draft));
    }

    fn mask(&self) -> Option<&Mask> {
        self.selected.and_then(|id| self.settings.mask(id))
    }
}

/// Brings the tab up to date with the engine: the output's adjustments when the project changed, and the previews the
/// adjust session rendered.
pub fn sync(app: &mut FloraStackApp) {
    let changes = app.shell.adjust.take();
    let engine = app.shell.engine();
    let Some(view) = engine.adjust() else {
        let ui = &mut app.adjust;
        (ui.output, ui.preview, ui.before) = (None, None, None);
        return;
    };
    let output = view.output().get();
    let (preview, before) = view.previews();
    let ui = &mut app.adjust;
    if changes.is_some_and(|c| c.opened) || ui.output != Some(output) {
        (ui.output, ui.preview, ui.before, ui.read, ui.selected) = (Some(output), None, None, None, None);
        ui.want_preview(false);
    }
    if changes.is_some_and(|c| c.preview) {
        ui.preview = preview.map(Picture::new);
        ui.before = before.map(Picture::new);
    }
    if ui.read != Some(app.shell.revision) {
        if let Some(read) =
            app.shell.run("adjust.get", json!({ "output": output })).and_then(|r| serde_json::from_value(r["adjustments"].clone()).ok())
        {
            app.adjust.settings = read;
        }
        // Any change can matter: the adjustments, or a retouch stroke under them.
        if app.adjust.read.is_some() {
            app.adjust.want_preview(false);
        }
        app.adjust.read = Some(app.shell.revision);
        let ui = &mut app.adjust;
        if ui.selected.is_some_and(|id| ui.settings.mask(id).is_none()) {
            ui.selected = None;
        }
    }
}

/// Asks the engine for the preview the tab wants, when no preview job is running.
fn request_preview(app: &mut FloraStackApp) {
    let Some(draft) = app.adjust.pending else { return };
    let (w, h) = app.adjust.wanted;
    if app.adjust.output.is_none() || w == 0 || h == 0 || app.shell.running(&["adjust.preview", "adjust.open"]) {
        return;
    }
    app.adjust.pending = None;
    let overlay = if app.adjust.show_mask { app.adjust.selected } else { None };
    let before = app.adjust.compare && !draft;
    app.adjust.asked_size = (w, h);
    app.shell.run("adjust.preview", json!({ "width": w, "height": h, "draft": draft, "overlay": overlay, "before": before }));
}

/// Stores the whole image's settings (one undo step for a slider's whole drag), and asks for a preview.
fn set_global(app: &mut FloraStackApp, global: Global, key: &str, dragging: bool) {
    let Some(output) = app.adjust.output else { return };
    app.adjust.settings.global = global;
    let coalesce = Some(format!("adjust.global {key}"));
    app.shell.run_with("adjust.set", json!({ "output": output, "adjustments": app.adjust.settings }), coalesce);
    app.adjust.want_preview(dragging);
}

/// Stores the selected mask (one undo step for a slider's or a handle's whole drag, keyed by `key`), and asks for a
/// preview.
fn set_mask(app: &mut FloraStackApp, mask: &Mask, key: &str, dragging: bool) {
    let Some(output) = app.adjust.output else { return };
    let coalesce = Some(format!("adjust.mask {} {key}", mask.id));
    if let Some(slot) = app.adjust.settings.mask_mut(mask.id) {
        slot.clone_from(mask);
    }
    app.shell.run_with("adjust.setMask", json!({ "output": output, "mask": mask }), coalesce);
    app.adjust.want_preview(dragging);
}

/// Adds a mask of one shape of `kind`, and selects it.
pub fn add_mask(app: &mut FloraStackApp, kind: &str) {
    let Some(output) = app.adjust.output else { return };
    let name = tr(SHAPES.iter().find(|(k, _)| *k == kind).map_or("adjust-shape-whole", |(_, label)| label));
    let mask = Mask { name, components: vec![Component { op: Op::Add, invert: false, shape: new_shape(kind) }], ..Mask::default() };
    if let Some(added) = app.shell.run("adjust.addMask", json!({ "output": output, "mask": mask })) {
        app.adjust.selected = added["mask"].as_u64().and_then(|id| u32::try_from(id).ok());
        if let Ok(settings) = serde_json::from_value(added["adjustments"].clone()) {
            app.adjust.settings = settings;
        }
        if kind == "depth" && !app.shell.engine().adjust().is_some_and(|v| v.has_depth()) {
            app.shell.run("adjust.mapDepth", json!({}));
        }
        app.adjust.want_preview(false);
    }
}

/// The right panel's Adjust tab.
pub fn panel(app: &mut FloraStackApp, ui: &mut egui::Ui) {
    ui.heading(tr("tab-adjust"));
    if app.shell.outputs.is_empty() {
        ui.label(tr("adjust-nothing"));
        return;
    }
    let opening = app.shell.running(&["adjust.open"]);
    let current = app.adjust.output;
    let name = |id: Option<u64>| {
        id.and_then(|id| app.shell.outputs.iter().find(|o| o.id == id)).map_or_else(|| tr("adjust-pick-output"), |o| o.name.clone())
    };
    let mut picked = None;
    egui::ComboBox::from_label(tr("adjust-output")).selected_text(name(current)).show_ui(ui, |ui| {
        for output in &app.shell.outputs {
            if ui.selectable_label(current == Some(output.id), &output.name).clicked() && current != Some(output.id) {
                picked = Some(output.id);
            }
        }
    });
    // With none open, the output picked elsewhere (or the newest) opens, once.
    let wanted = match app.selection {
        crate::app::Selection::Output(id) => Some(id),
        _ => app.shell.outputs.last().map(|o| o.id),
    };
    if current.is_none() && !opening && app.adjust.asked.is_none() {
        picked = picked.or(wanted);
    }
    if let Some(id) = picked {
        app.adjust.asked = Some(id);
        app.shell.run("adjust.open", json!({ "output": id }));
    }
    if opening {
        ui.weak(tr("adjust-opening"));
    }
    let Some(output) = current else { return };
    ui.horizontal(|ui| {
        if ui.checkbox(&mut app.adjust.compare, tr("adjust-before")).on_hover_text(tr("tip-adjust-before")).changed() {
            app.adjust.want_preview(false);
        }
        if ui.checkbox(&mut app.adjust.show_mask, tr("adjust-show-mask")).on_hover_text(tr("tip-adjust-show-mask")).changed() {
            app.adjust.want_preview(false);
        }
    });
    if ui.button(tr("adjust-reset")).on_hover_text(tr("tip-adjust-reset")).clicked() {
        app.shell.run("adjust.reset", json!({ "output": output }));
        app.adjust.selected = None;
        app.adjust.want_preview(false);
    }
    ui.separator();
    global(app, ui);
    ui.separator();
    masks(app, ui, output);
    if let Some(mask) = app.adjust.mask().cloned() {
        ui.separator();
        mask_panel(app, ui, &mask);
    }
}

/// A slider of `value` in `range`, labeled and with its tooltip; returns whether it changed and whether it is being
/// dragged.
fn slider(ui: &mut egui::Ui, value: &mut f64, range: std::ops::RangeInclusive<f64>, (label, tip): (&str, &str)) -> (bool, bool) {
    let response = ui.add(egui::Slider::new(value, range).text(tr(label))).on_hover_text(tr(tip));
    (response.changed() || response.drag_stopped(), response.dragged())
}

/// The whole image's sliders, in groups.
fn global(app: &mut FloraStackApp, ui: &mut egui::Ui) {
    let mut g = app.adjust.settings.global;
    let mut changed = None;
    let mut groups =
        |ui: &mut egui::Ui, title: &str, rows: &mut [(&mut f64, std::ops::RangeInclusive<f64>, &'static str, &'static str, &'static str)]| {
            egui::CollapsingHeader::new(RichText::new(tr(title)).strong()).id_salt(title).default_open(true).show(ui, |ui| {
                for (value, range, label, tip, key) in rows.iter_mut() {
                    let (moved, dragging) = slider(ui, value, range.clone(), (label, tip));
                    if moved {
                        changed = Some((*key, dragging));
                    }
                }
            });
        };
    groups(
        ui,
        "adjust-light",
        &mut [
            (&mut g.exposure, -5.0..=5.0, "adjust-exposure", "tip-adjust-exposure", "exposure"),
            (&mut g.contrast, -100.0..=100.0, "adjust-contrast", "tip-adjust-contrast", "contrast"),
            (&mut g.highlights, -100.0..=100.0, "adjust-highlights", "tip-adjust-highlights", "highlights"),
            (&mut g.shadows, -100.0..=100.0, "adjust-shadows", "tip-adjust-shadows", "shadows"),
            (&mut g.whites, -100.0..=100.0, "adjust-whites", "tip-adjust-whites", "whites"),
            (&mut g.blacks, -100.0..=100.0, "adjust-blacks", "tip-adjust-blacks", "blacks"),
        ],
    );
    groups(
        ui,
        "adjust-color",
        &mut [
            (&mut g.temperature, -100.0..=100.0, "adjust-temperature", "tip-adjust-temperature", "temperature"),
            (&mut g.tint, -100.0..=100.0, "adjust-tint", "tip-adjust-tint", "tint"),
            (&mut g.vibrance, -100.0..=100.0, "adjust-vibrance", "tip-adjust-vibrance", "vibrance"),
            (&mut g.saturation, -100.0..=100.0, "adjust-saturation", "tip-adjust-saturation", "saturation"),
        ],
    );
    groups(
        ui,
        "adjust-detail",
        &mut [
            (&mut g.texture, -100.0..=100.0, "adjust-texture", "tip-adjust-texture", "texture"),
            (&mut g.clarity, -100.0..=100.0, "adjust-clarity", "tip-adjust-clarity", "clarity"),
            (&mut g.dehaze, -100.0..=100.0, "adjust-dehaze", "tip-adjust-dehaze", "dehaze"),
            (&mut g.sharpening, 0.0..=150.0, "adjust-sharpening", "tip-adjust-sharpening", "sharpening"),
            (&mut g.sharpen_radius, 0.5..=3.0, "adjust-sharpen-radius", "tip-adjust-sharpen-radius", "sharpen_radius"),
            (&mut g.sharpen_detail, 0.0..=100.0, "adjust-sharpen-detail", "tip-adjust-sharpen-detail", "sharpen_detail"),
            (&mut g.sharpen_masking, 0.0..=100.0, "adjust-sharpen-masking", "tip-adjust-sharpen-masking", "sharpen_masking"),
            (&mut g.noise_reduction, 0.0..=100.0, "adjust-noise-reduction", "tip-adjust-noise-reduction", "noise_reduction"),
            (&mut g.noise_detail, 0.0..=100.0, "adjust-noise-detail", "tip-adjust-noise-detail", "noise_detail"),
            (&mut g.color_noise, 0.0..=100.0, "adjust-color-noise", "tip-adjust-color-noise", "color_noise"),
        ],
    );
    if let Some((key, dragging)) = changed {
        set_global(app, g, key, dragging);
    }
}

/// The masks: each one's visibility, the one selected, and adding and removing them.
fn masks(app: &mut FloraStackApp, ui: &mut egui::Ui, output: u64) {
    ui.label(RichText::new(tr("adjust-masks")).strong());
    let list: Vec<(u32, String, bool)> = app.adjust.settings.masks.iter().map(|m| (m.id, m.name.clone(), m.visible)).collect();
    if list.is_empty() {
        ui.weak(tr("adjust-no-masks"));
    }
    for (id, name, visible) in list.iter().rev() {
        ui.horizontal(|ui| {
            let mut shown = *visible;
            if ui.checkbox(&mut shown, "").on_hover_text(tr("tip-adjust-mask-visible")).changed()
                && let Some(mask) = app.adjust.settings.mask(*id).cloned()
            {
                set_mask(app, &Mask { visible: shown, ..mask }, "visible", false);
            }
            let label = if name.is_empty() { tr_args("adjust-mask-unnamed", &[("id", (*id).into())]) } else { name.clone() };
            if ui.selectable_label(app.adjust.selected == Some(*id), label).clicked() {
                app.adjust.selected = Some(*id);
                if app.adjust.show_mask {
                    app.adjust.want_preview(false);
                }
            }
        });
    }
    ui.horizontal(|ui| {
        ui.menu_button(tr("adjust-add-mask"), |ui| {
            for (kind, label) in SHAPES {
                if ui.button(tr(label)).clicked() {
                    add_mask(app, kind);
                    ui.close();
                }
            }
        });
        if let Some(id) = app.adjust.selected
            && ui.button(tr("adjust-remove-mask")).clicked()
        {
            app.shell.run("adjust.removeMask", json!({ "output": output, "mask": id }));
            app.adjust.selected = None;
            app.adjust.want_preview(false);
        }
    });
}

/// The selected mask: its name and how it applies, its components with their settings, and its sliders.
fn mask_panel(app: &mut FloraStackApp, ui: &mut egui::Ui, mask: &Mask) {
    let mut m = mask.clone();
    let mut key = None;
    ui.horizontal(|ui| {
        ui.label(tr("adjust-mask-name"));
        if ui.text_edit_singleline(&mut m.name).lost_focus() {
            key = Some(("name", false));
        }
    });
    ui.horizontal(|ui| {
        if ui.checkbox(&mut m.invert, tr("adjust-invert")).on_hover_text(tr("tip-adjust-invert")).changed() {
            key = Some(("invert", false));
        }
    });
    let (moved, dragging) = slider(ui, &mut m.refine, 0.0..=100.0, ("adjust-refine", "tip-adjust-refine"));
    if moved {
        key = Some(("refine", dragging));
    }
    let (moved, dragging) = slider(ui, &mut m.adjust.amount, 0.0..=100.0, ("adjust-amount", "tip-adjust-amount"));
    if moved {
        key = Some(("amount", dragging));
    }
    // The components.
    ui.label(RichText::new(tr("adjust-components")).strong());
    let mut remove = None;
    for (i, c) in m.components.iter_mut().enumerate() {
        ui.push_id(i, |ui| {
            ui.horizontal(|ui| {
                if i > 0 {
                    let ops = [(Op::Add, "adjust-op-add"), (Op::Subtract, "adjust-op-subtract"), (Op::Intersect, "adjust-op-intersect")];
                    let shown = ops.iter().find(|(op, _)| *op == c.op).map_or("adjust-op-add", |(_, l)| l);
                    egui::ComboBox::from_id_salt("op").selected_text(tr(shown)).show_ui(ui, |ui| {
                        for (op, label) in ops {
                            if ui.selectable_label(c.op == op, tr(label)).clicked() {
                                c.op = op;
                                key = Some(("op", false));
                            }
                        }
                    });
                }
                ui.label(tr(shape_label(&c.shape)));
                if ui.checkbox(&mut c.invert, tr("adjust-invert")).changed() {
                    key = Some(("component invert", false));
                }
                if ui.small_button(tr("adjust-remove-component")).on_hover_text(tr("tip-adjust-remove-component")).clicked() {
                    remove = Some(i);
                }
            });
            if let Some(k) = shape_settings(ui, &mut c.shape) {
                key = Some(k);
            }
        });
    }
    if let Some(i) = remove {
        m.components.remove(i);
        key = Some(("remove component", false));
    }
    ui.menu_button(tr("adjust-add-component"), |ui| {
        for (kind, label) in SHAPES {
            if ui.button(tr(label)).clicked() {
                m.components.push(Component { op: Op::Add, invert: false, shape: new_shape(kind) });
                key = Some(("add component", false));
                ui.close();
            }
        }
    });
    tools(app, ui, &m);
    // The mask's own adjustments.
    egui::CollapsingHeader::new(RichText::new(tr("adjust-mask-adjustments")).strong()).id_salt("mask adjustments").default_open(true).show(
        ui,
        |ui| {
            let l: &mut Local = &mut m.adjust;
            for (value, range, label, tip, k) in [
                (&mut l.exposure, -5.0..=5.0, "adjust-exposure", "tip-adjust-exposure", "exposure"),
                (&mut l.contrast, -100.0..=100.0, "adjust-contrast", "tip-adjust-contrast", "contrast"),
                (&mut l.highlights, -100.0..=100.0, "adjust-highlights", "tip-adjust-highlights", "highlights"),
                (&mut l.shadows, -100.0..=100.0, "adjust-shadows", "tip-adjust-shadows", "shadows"),
                (&mut l.whites, -100.0..=100.0, "adjust-whites", "tip-adjust-whites", "whites"),
                (&mut l.blacks, -100.0..=100.0, "adjust-blacks", "tip-adjust-blacks", "blacks"),
                (&mut l.temperature, -100.0..=100.0, "adjust-temperature", "tip-adjust-temperature", "temperature"),
                (&mut l.tint, -100.0..=100.0, "adjust-tint", "tip-adjust-tint", "tint"),
                (&mut l.saturation, -100.0..=100.0, "adjust-saturation", "tip-adjust-saturation", "saturation"),
                (&mut l.texture, -100.0..=100.0, "adjust-texture", "tip-adjust-texture", "texture"),
                (&mut l.clarity, -100.0..=100.0, "adjust-clarity", "tip-adjust-clarity", "clarity"),
                (&mut l.dehaze, -100.0..=100.0, "adjust-dehaze", "tip-adjust-dehaze", "dehaze"),
                (&mut l.sharpness, -100.0..=100.0, "adjust-sharpness", "tip-adjust-sharpness", "sharpness"),
                (&mut l.noise, -100.0..=100.0, "adjust-noise", "tip-adjust-noise", "noise"),
            ] {
                let (moved, dragging) = slider(ui, value, range, (label, tip));
                if moved {
                    key = Some((k, dragging));
                }
            }
        },
    );
    if let Some((k, dragging)) = key
        && m != *mask
    {
        set_mask(app, &m, k, dragging);
    }
}

/// The settings of one component's shape that sliders set (gradients and ellipses move with their handles on the
/// image); the key and whether it is dragged when one changed.
fn shape_settings(ui: &mut egui::Ui, shape: &mut Shape) -> Option<(&'static str, bool)> {
    let mut key = None;
    let mut s = |ui: &mut egui::Ui, value: &mut f64, range: std::ops::RangeInclusive<f64>, texts: (&str, &str), k: &'static str| {
        let (moved, dragging) = slider(ui, value, range, texts);
        if moved {
            key = Some((k, dragging));
        }
    };
    match shape {
        Shape::Radial { feather, angle, .. } => {
            s(ui, feather, 0.0..=100.0, ("adjust-feather", "tip-adjust-feather"), "feather");
            s(ui, angle, -180.0..=180.0, ("adjust-angle", "tip-adjust-angle"), "angle");
        }
        Shape::Luminance { low, high, low_feather, high_feather } => {
            s(ui, low, 0.0..=1.0, ("adjust-luminance-low", "tip-adjust-luminance-low"), "low");
            s(ui, high, 0.0..=1.0, ("adjust-luminance-high", "tip-adjust-luminance-high"), "high");
            s(ui, low_feather, 0.0..=1.0, ("adjust-low-feather", "tip-adjust-low-feather"), "low_feather");
            s(ui, high_feather, 0.0..=1.0, ("adjust-high-feather", "tip-adjust-high-feather"), "high_feather");
            if *low > *high {
                *high = *low;
            }
        }
        Shape::Depth { from, to, feather } => {
            s(ui, from, 0.0..=1.0, ("adjust-depth-from", "tip-adjust-depth-from"), "from");
            s(ui, to, 0.0..=1.0, ("adjust-depth-to", "tip-adjust-depth-to"), "to");
            s(ui, feather, 0.0..=1.0, ("adjust-feather", "tip-adjust-depth-feather"), "feather");
            if *from > *to {
                *to = *from;
            }
        }
        Shape::Color { samples, refine } => {
            s(ui, refine, 0.0..=100.0, ("adjust-refine-color", "tip-adjust-refine-color"), "color refine");
            ui.weak(tr_args("adjust-color-samples", &[("count", samples.len().into())]));
            if !samples.is_empty() && ui.small_button(tr("adjust-clear-samples")).clicked() {
                samples.clear();
                key = Some(("samples", false));
            }
        }
        Shape::Brush { strokes } => {
            ui.weak(tr_args("adjust-brush-strokes", &[("count", strokes.len().into())]));
        }
        Shape::Linear { .. } => {
            ui.weak(tr("adjust-linear-hint"));
        }
        Shape::Object { selection, .. } => {
            ui.weak(if selection.is_some() { tr("adjust-object-selected") } else { tr("adjust-object-hint") });
        }
        Shape::Described { text, .. } => {
            ui.weak(tr_args("adjust-described", &[("text", text.clone().into())]));
        }
        Shape::Subject | Shape::Background | Shape::Whole => {}
    }
    key
}

/// The selected mask's tools: the brush's settings, picking a color, mapping the depth.
fn tools(app: &mut FloraStackApp, ui: &mut egui::Ui, mask: &Mask) {
    let has = |f: fn(&Shape) -> bool| mask.components.iter().any(|c| f(&c.shape));
    if has(|s| matches!(s, Shape::Brush { .. })) {
        let a = &mut app.adjust;
        ui.label(RichText::new(tr("adjust-brush")).strong());
        ui.add(egui::Slider::new(&mut a.brush_size, 0.002..=0.3).logarithmic(true).text(tr("adjust-brush-size")))
            .on_hover_text(tr("tip-adjust-brush-size"));
        ui.add(egui::Slider::new(&mut a.brush_feather, 0.0..=100.0).text(tr("adjust-feather"))).on_hover_text(tr("tip-adjust-brush-feather"));
        ui.add(egui::Slider::new(&mut a.brush_flow, 1.0..=100.0).text(tr("adjust-brush-flow"))).on_hover_text(tr("tip-adjust-brush-flow"));
        ui.checkbox(&mut a.erase, tr("adjust-erase")).on_hover_text(tr("tip-adjust-erase"));
        ui.weak(tr("adjust-brush-hint"));
    }
    if has(|s| matches!(s, Shape::Color { .. })) {
        let label = if app.adjust.picking { tr("adjust-picking") } else { tr("adjust-pick-color") };
        if ui.button(label).on_hover_text(tr("tip-adjust-pick-color")).clicked() {
            app.adjust.picking = !app.adjust.picking;
        }
    }
    if has(|s| matches!(s, Shape::Depth { .. })) && !app.shell.engine().adjust().is_some_and(|v| v.has_depth()) {
        if app.shell.running(&["adjust.mapDepth"]) {
            ui.weak(tr("adjust-mapping-depth"));
        } else if ui.button(tr("adjust-map-depth")).on_hover_text(tr("tip-adjust-map-depth")).clicked() {
            app.shell.run("adjust.mapDepth", json!({}));
        }
    }
}

/// The central view: the preview (or the output before its adjustments), fitted, with the selected mask's handles.
pub fn view(app: &mut FloraStackApp, ui: &mut egui::Ui) {
    let (pane, response) = ui.allocate_exact_size(ui.available_size(), Sense::click_and_drag());
    ui.painter().rect_filled(pane, 0.0, ui.visuals().extreme_bg_color);
    let ppp = ui.ctx().pixels_per_point();
    #[allow(clippy::cast_possible_truncation, clippy::cast_sign_loss)] // A view's size in pixels.
    let wanted = ((pane.width() * ppp) as usize, (pane.height() * ppp) as usize);
    let max = app.viewer.max_side();
    let wanted = (wanted.0.clamp(1, max), wanted.1.clamp(1, max));
    if wanted != app.adjust.wanted {
        app.adjust.wanted = wanted;
        if app.adjust.asked_size != wanted {
            app.adjust.want_preview(false);
        }
    }
    request_preview(app);
    let gpu = app.viewer.gpu();
    let ctx = ui.ctx().clone();
    let a = &mut app.adjust;
    let shown = if a.compare { a.before.as_mut().or(a.preview.as_mut()) } else { a.preview.as_mut() };
    let Some(picture) = shown else {
        let text = if app.shell.running(&["adjust.open"]) { tr("adjust-opening") } else { tr("adjust-hint") };
        ui.painter().text(pane.center(), egui::Align2::CENTER_CENTER, text, egui::FontId::proportional(15.0), ui.visuals().weak_text_color());
        return;
    };
    let size = picture.size();
    let zoom = fit_zoom(pane.size() * ppp, size);
    draw(ui, &ctx, gpu, 0, picture, (pane, pane), zoom, size / 2.0, 1.0);
    let image = Rect::from_center_size(pane.center(), size * zoom / ppp);
    a.image_rect = Some(image);
    if a.compare {
        label(ui, pane, tr("adjust-before"));
        return;
    }
    interact(app, ui, &response, image);
    if app.shell.running(&["adjust.preview"]) || app.adjust.pending.is_some() {
        ui.ctx().request_repaint_after(std::time::Duration::from_millis(30));
    }
}

/// A name in the view's top left corner.
fn label(ui: &egui::Ui, pane: Rect, text: String) {
    let painter = ui.painter();
    let galley = painter.layout_no_wrap(text, egui::FontId::proportional(13.0), egui::Color32::WHITE);
    let back = Rect::from_min_size(pane.left_top() + Vec2::splat(8.0), galley.size()).expand(4.0);
    painter.rect_filled(back, 3.0, egui::Color32::from_black_alpha(150));
    painter.galley(back.min + Vec2::splat(4.0), galley, egui::Color32::WHITE);
}

/// A point of the image, in fractions of its size, on screen.
fn to_screen(image: Rect, p: [f64; 2]) -> Pos2 {
    #[allow(clippy::cast_possible_truncation)] // A point on screen.
    let (x, y) = (p[0] as f32, p[1] as f32);
    Pos2::new(image.min.x + x * image.width(), image.min.y + y * image.height())
}

/// A point on screen, in fractions of the image's size.
fn to_image(image: Rect, p: Pos2) -> [f64; 2] {
    [f64::from((p.x - image.min.x) / image.width().max(1.0)), f64::from((p.y - image.min.y) / image.height().max(1.0))]
}

/// The selected mask's handles, drawn and dragged; painting its brush; sampling a color.
fn interact(app: &mut FloraStackApp, ui: &egui::Ui, response: &egui::Response, image: Rect) {
    let Some(mask) = app.adjust.mask().cloned() else { return };
    let long = image.width().max(image.height());
    let painter = ui.painter().with_clip_rect(image);
    let mut handles: Vec<(Handle, Pos2)> = Vec::new();
    for (i, c) in mask.components.iter().enumerate() {
        match &c.shape {
            Shape::Linear { start, end } => {
                let (a, b) = (to_screen(image, *start), to_screen(image, *end));
                painter.line_segment([a, b], egui::Stroke::new(1.5, egui::Color32::WHITE));
                handles.extend([(Handle::Start(i), a), (Handle::End(i), b)]);
            }
            Shape::Radial { center, radius_x, radius_y, angle, .. } => {
                let c0 = to_screen(image, *center);
                #[allow(clippy::cast_possible_truncation)] // Sizes on screen.
                let (rx, ry, turn) = ((*radius_x as f32) * long, (*radius_y as f32) * long, (*angle as f32).to_radians());
                let (s, co) = turn.sin_cos();
                let at = |u: f32, v: f32| c0 + Vec2::new(u * co - v * s, u * s + v * co);
                let points: Vec<Pos2> = (0..=64)
                    .map(|k| {
                        #[allow(clippy::cast_precision_loss)] // A small count.
                        let t = k as f32 / 64.0 * std::f32::consts::TAU;
                        at(rx * t.cos(), ry * t.sin())
                    })
                    .collect();
                painter.add(egui::Shape::line(points, egui::Stroke::new(1.5, egui::Color32::WHITE)));
                handles.extend([(Handle::Center(i), c0), (Handle::RadiusX(i), at(rx, 0.0)), (Handle::RadiusY(i), at(0.0, ry))]);
            }
            _ => {}
        }
    }
    for (_, p) in &handles {
        painter.circle(*p, 5.0, egui::Color32::from_black_alpha(160), egui::Stroke::new(1.5, egui::Color32::WHITE));
    }
    let at = response.interact_pointer_pos().or_else(|| response.hover_pos());
    let a = &mut app.adjust;
    // A drag grabs the handle nearest to where the button went down: it starts only once the pointer has moved.
    let pressed = ui.input(|i| i.pointer.press_origin()).or(at);
    if response.drag_started_by(egui::PointerButton::Primary)
        && let Some(p) = pressed
    {
        a.dragging =
            handles.iter().filter(|(_, h)| h.distance(p) <= GRAB).min_by(|x, y| x.1.distance(p).total_cmp(&y.1.distance(p))).map(|(h, _)| *h);
        a.stroke.clear();
    }
    // Dragging a handle moves its point.
    if let (Some(handle), Some(p)) = (a.dragging, at)
        && response.dragged_by(egui::PointerButton::Primary)
    {
        let mut m = mask.clone();
        let point = to_image(image, p);
        move_handle(&mut m, handle, point, image);
        set_mask(app, &m, "handle", true);
        return;
    }
    if response.drag_stopped() && app.adjust.dragging.take().is_some() {
        app.adjust.want_preview(false);
        return;
    }
    let brush = mask.components.iter().any(|c| matches!(c.shape, Shape::Brush { .. }));
    let a = &mut app.adjust;
    // Painting the brush.
    if brush && a.dragging.is_none() && !a.picking {
        paint(app, ui, response, (image, at), mask.id);
        return;
    }
    // Sampling a color.
    if a.picking
        && response.clicked()
        && let Some(p) = at
    {
        a.picking = false;
        let [x, y] = to_image(image, p);
        let sampled = app.shell.run("adjust.sampleColor", json!({ "x": x, "y": y }));
        if let Some(color) = sampled.and_then(|s| serde_json::from_value::<[f64; 3]>(s["color"].clone()).ok()) {
            let mut m = mask.clone();
            if let Some(Shape::Color { samples, .. }) = m.components.iter_mut().map(|c| &mut c.shape).find(|s| matches!(s, Shape::Color { .. })) {
                samples.push(color);
                samples.truncate(MAX_SAMPLES);
            }
            set_mask(app, &m, "sample", false);
        }
    }
}

/// Painting the selected mask's brush: its circle under the pointer, the stroke drawn as the pointer drags, and the
/// stroke sent (`adjust.stroke`) when the button is let go or the image clicked.
fn paint(app: &mut FloraStackApp, ui: &egui::Ui, response: &egui::Response, (image, at): (Rect, Option<Pos2>), mask: u32) {
    let long = image.width().max(image.height());
    let painter = ui.painter().with_clip_rect(image);
    let a = &mut app.adjust;
    if let Some(p) = at {
        #[allow(clippy::cast_possible_truncation)] // A radius on screen.
        let radius = (a.brush_size as f32 * long).max(1.0);
        painter.circle_stroke(p, radius, egui::Stroke::new(1.0, egui::Color32::WHITE));
        ui.ctx().set_cursor_icon(egui::CursorIcon::Crosshair);
    }
    if response.dragged_by(egui::PointerButton::Primary)
        && let Some(p) = at
    {
        let point = to_image(image, p);
        if a.stroke.last().is_none_or(|l| (l[0] - point[0]).hypot(l[1] - point[1]) >= 0.002) {
            a.stroke.push(point);
        }
        let points: Vec<Pos2> = a.stroke.iter().map(|q| to_screen(image, *q)).collect();
        painter.add(egui::Shape::line(points, egui::Stroke::new(1.5, egui::Color32::from_rgb(230, 40, 40))));
    }
    let let_go = response.drag_stopped_by(egui::PointerButton::Primary) && !a.stroke.is_empty();
    let clicked = response.clicked() && at.is_some();
    if let_go || clicked {
        if a.stroke.is_empty()
            && let Some(p) = at
        {
            a.stroke.push(to_image(image, p));
        }
        let stroke =
            Stroke { points: std::mem::take(&mut a.stroke), size: a.brush_size, feather: a.brush_feather, flow: a.brush_flow, erase: a.erase };
        if let Some(output) = a.output {
            app.shell.run("adjust.stroke", json!({ "output": output, "mask": mask, "stroke": stroke }));
            app.adjust.want_preview(false);
        }
    }
}

/// Moves `handle` of `mask` to `point` (fractions of the image), for an image drawn in `image` on screen.
fn move_handle(mask: &mut Mask, handle: Handle, point: [f64; 2], image: Rect) {
    let clamp = |p: [f64; 2]| p.map(|v| v.clamp(-1.0, 2.0));
    let (w, h) = (f64::from(image.width()), f64::from(image.height()));
    let long = w.max(h).max(1.0);
    let index = match handle {
        Handle::Start(i) | Handle::End(i) | Handle::Center(i) | Handle::RadiusX(i) | Handle::RadiusY(i) => i,
    };
    let Some(component) = mask.components.get_mut(index) else { return };
    match (&mut component.shape, handle) {
        (Shape::Linear { start, .. }, Handle::Start(_)) => *start = clamp(point),
        (Shape::Linear { end, .. }, Handle::End(_)) => *end = clamp(point),
        (Shape::Radial { center, .. }, Handle::Center(_)) => *center = clamp(point),
        (Shape::Radial { center, radius_x, radius_y, angle, .. }, Handle::RadiusX(_) | Handle::RadiusY(_)) => {
            // The handle's distance from the center, in fractions of the long edge, along the ellipse's axis.
            let (dx, dy) = ((point[0] - center[0]) * w / long, (point[1] - center[1]) * h / long);
            let turn = angle.to_radians();
            let (u, v) = (dx * turn.cos() + dy * turn.sin(), -dx * turn.sin() + dy * turn.cos());
            if matches!(handle, Handle::RadiusX(_)) {
                *radius_x = u.abs().clamp(0.001, 10.0);
            } else {
                *radius_y = v.abs().clamp(0.001, 10.0);
            }
        }
        _ => {}
    }
}
