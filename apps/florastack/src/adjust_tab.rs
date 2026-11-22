//! The Adjust tab: the output with its adjustments in the view, and in the right panel the whole image's sliders, the
//! masks and the selected mask's shapes and sliders. Everything goes through the engine's `adjust.*` commands: a
//! slider's drag is one undo step (`adjust.set` or `adjust.setMask`, coalesced), adding, removing and painting a mask
//! are one each. The view shows the engine's adjust session (`florastack_engine::adjust::AdjustView`): a preview at the
//! view's size, rendered as a draft while a slider moves and again in full when it stops, with the output as it was
//! beside it on request (Before), the selected mask tinted (Show mask), and handles on the image for the selected
//! mask's gradients and ellipses; dragging on the image paints the selected mask's brush.

use egui::{Pos2, Rect, RichText, Sense, Vec2};
use florastack_engine::adjust::{Click, Component, Global, Local, MAX_SAMPLES, Mask, Op, Placement, Settings, Shape, Stroke};
use florastack_engine::json;

use crate::app::FloraStackApp;
use crate::i18n::{tr, tr_args};
use crate::viewer::{Picture, draw, fit_zoom, zoomed_center};

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

/// The AI selections in Add mask: their kinds and labels' keys.
pub const AI_SHAPES: [(&str, &str); 3] =
    [("ai-subject", "adjust-shape-object"), ("ai-background", "adjust-shape-ai-background"), ("ai-describe", "adjust-shape-described")];

/// An AI mask waiting for its first selection: from clicks, or from a description.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum AiPending {
    Clicks(u32),
    Description(u32),
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
    /// The last preview of the whole output; when zoomed in, the last preview of the part in view, which a window of
    /// a larger preview is (`adjust.preview`'s `window`), with where it lies; and the output without its adjustments,
    /// as the last preview asked for it showed it.
    preview: Option<Picture>,
    zoomed: Option<(Picture, Placement)>,
    before: Option<(Picture, Placement)>,
    /// The view: device pixels per output pixel, now and where it is going, the output's point at the view's center,
    /// the point the zoom is about, whether it fits the output in the view, and whether it is being panned.
    zoom: f32,
    target: f32,
    center: Vec2,
    anchor: Vec2,
    fit: bool,
    panning: bool,
    /// Shows the output without its adjustments (Before), and the selected mask tinted (Show mask).
    pub compare: bool,
    pub show_mask: bool,
    /// A preview is wanted: as a draft (a slider or a handle moves) or in full.
    pending: Option<bool>,
    /// The preview the view wants, and the one last asked for.
    wanted: Option<Ask>,
    sent: Option<Ask>,
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
    /// An AI mask added and waiting for its first selection, and the description being typed.
    pub ai_pending: Option<AiPending>,
    pub description: String,
    /// What `adjust.model` said, and the project revision it was asked at.
    model: Option<florastack_engine::Value>,
    model_asked: Option<u64>,
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
            zoomed: None,
            before: None,
            zoom: 1.0,
            target: 1.0,
            center: Vec2::ZERO,
            anchor: Vec2::ZERO,
            fit: true,
            panning: false,
            compare: false,
            show_mask: false,
            pending: None,
            wanted: None,
            sent: None,
            asked: None,
            dragging: None,
            brush_size: 0.03,
            brush_feather: 50.0,
            brush_flow: 100.0,
            erase: false,
            stroke: Vec::new(),
            picking: false,
            ai_pending: None,
            description: String::new(),
            model: None,
            model_asked: None,
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

    /// The view: device pixels per output pixel, and the output's point at the view's center.
    pub fn view(&self) -> (f32, Vec2) {
        (self.zoom, self.center)
    }

    /// Where the last zoomed-in preview lies in the whole preview it is part of; `None` when the view shows the whole
    /// output's.
    pub fn zoomed(&self) -> Option<Placement> {
        self.zoomed.as_ref().map(|(_, p)| *p)
    }
}

/// A preview of the output: the size it fits in and, zoomed in, the window of that preview to render (left, top,
/// width, height).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
struct Ask {
    size: (usize, usize),
    window: Option<[usize; 4]>,
}

/// The zoom's limits, in device pixels per output pixel.
const MIN_ZOOM: f32 = 1.0 / 64.0;
const MAX_ZOOM: f32 = 64.0;
/// The largest preview the engine renders a window of, a side.
const MAX_PREVIEW_SIDE: f32 = 16384.0;

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
    let placement = view.placement();
    let ui = &mut app.adjust;
    if changes.is_some_and(|c| c.opened) || ui.output != Some(output) {
        (ui.output, ui.preview, ui.zoomed, ui.before, ui.read, ui.selected) = (Some(output), None, None, None, None, None);
        ui.fit = true;
        ui.want_preview(false);
    }
    if changes.is_some_and(|c| c.preview) {
        let whole = preview.as_ref().is_some_and(|p| (placement.x, placement.y) == (0, 0) && placement.full == (p.width, p.height));
        if whole {
            (ui.preview, ui.zoomed) = (preview.map(Picture::new), None);
        } else {
            ui.zoomed = preview.map(|p| (Picture::new(p), placement));
        }
        ui.before = before.map(|b| (Picture::new(b), placement));
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
    let Some(ask) = app.adjust.wanted else { return };
    if app.adjust.output.is_none() || app.shell.running(&["adjust.preview", "adjust.open"]) {
        return;
    }
    app.adjust.pending = None;
    let overlay = if app.adjust.show_mask { app.adjust.selected } else { None };
    let before = app.adjust.compare && !draft;
    app.adjust.sent = Some(ask);
    let (w, h) = ask.size;
    let mut params = json!({ "width": w, "height": h, "draft": draft, "overlay": overlay, "before": before });
    if let Some([x, y, width, height]) = ask.window {
        params["window"] = json!({ "x": x, "y": y, "width": width, "height": height });
    }
    app.shell.run("adjust.preview", params);
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

/// Whether this build runs the AI model (not in the browser).
fn ai_available(app: &FloraStackApp) -> bool {
    app.shell.engine().registry().get("adjust.select").is_some()
}

/// Adds an empty AI mask of `kind` (`ai-subject`, `ai-background` or `ai-describe`), selected and waiting for its
/// first selection.
pub fn add_ai_mask(app: &mut FloraStackApp, kind: &str) {
    let Some(output) = app.adjust.output else { return };
    let name = tr(AI_SHAPES.iter().find(|(k, _)| *k == kind).map_or("adjust-shape-object", |(_, label)| label));
    let mask = Mask { name, invert: kind == "ai-background", ..Mask::default() };
    if let Some(added) = app.shell.run("adjust.addMask", json!({ "output": output, "mask": mask })) {
        let id = added["mask"].as_u64().and_then(|id| u32::try_from(id).ok());
        app.adjust.selected = id;
        if let Ok(settings) = serde_json::from_value(added["adjustments"].clone()) {
            app.adjust.settings = settings;
        }
        app.adjust.ai_pending = id.map(|id| if kind == "ai-describe" { AiPending::Description(id) } else { AiPending::Clicks(id) });
        app.adjust.description.clear();
    }
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
            if ai_available(app) {
                for (kind, label) in AI_SHAPES {
                    if ui.button(tr(label)).clicked() {
                        add_ai_mask(app, kind);
                        ui.close();
                    }
                }
                ui.separator();
            }
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

/// What the AI does for the selected mask: selecting from clicks, or from a description; and the model's setup when
/// it isn't installed.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum AiUse {
    Clicks,
    Description,
}

/// The AI use of `mask`: from its shapes, or from its waiting to be selected.
fn ai_use(app: &FloraStackApp, mask: &Mask) -> Option<AiUse> {
    if mask.components.iter().any(|c| matches!(c.shape, Shape::Object { .. })) || app.adjust.ai_pending == Some(AiPending::Clicks(mask.id)) {
        Some(AiUse::Clicks)
    } else if mask.components.iter().any(|c| matches!(c.shape, Shape::Described { .. }))
        || app.adjust.ai_pending == Some(AiPending::Description(mask.id))
    {
        Some(AiUse::Description)
    } else {
        None
    }
}

/// What `adjust.model` says, asked again when the project changed or a folder was just checked.
fn model_info(app: &mut FloraStackApp) -> florastack_engine::Value {
    let revision = app.shell.revision;
    if app.adjust.model.is_none() || app.adjust.model_asked != Some(revision) {
        app.adjust.model = app.shell.run("adjust.model", json!({}));
        app.adjust.model_asked = Some(revision);
    }
    app.adjust.model.clone().unwrap_or_default()
}

/// The AI part of the selected mask's panel.
fn ai(app: &mut FloraStackApp, ui: &mut egui::Ui, mask: &Mask) {
    let Some(using) = ai_use(app, mask) else { return };
    ui.label(RichText::new(tr("adjust-ai")).strong());
    if app.shell.running(&["adjust.setModel"]) {
        ui.weak(tr("adjust-model-checking"));
        app.adjust.model_asked = None;
        return;
    }
    let model = model_info(app);
    if model["installed"] != json!(true) {
        ui.label(tr("adjust-model-missing"));
        if let Some(problem) = model["problem"].as_str() {
            ui.colored_label(ui.visuals().warn_fg_color, problem);
        }
        ui.horizontal(|ui| {
            if ui.link(tr("adjust-model-download")).clicked() {
                ui.ctx().open_url(egui::OpenUrl::new_tab(model["download"].as_str().unwrap_or_default()));
            }
            if ui.link(tr("adjust-model-license")).clicked() {
                ui.ctx().open_url(egui::OpenUrl::new_tab(model["license"].as_str().unwrap_or_default()));
            }
        });
        ui.weak(tr("adjust-model-steps"));
        // The browser build offers no AI selection (`ai_available`), and has no folders to pick.
        #[cfg(not(target_arch = "wasm32"))]
        if ui.button(tr("adjust-model-choose")).on_hover_text(tr("tip-adjust-model-choose")).clicked()
            && let Some(folder) = crate::dialogs::read_folder(None)
        {
            app.shell.run("adjust.setModel", json!({ "folder": folder }));
            app.adjust.model_asked = None;
        }
        return;
    }
    if app.shell.running(&["adjust.select", "adjust.describe"]) {
        ui.weak(tr("adjust-ai-working"));
        return;
    }
    match using {
        AiUse::Clicks => {
            let has = mask.components.iter().any(|c| matches!(c.shape, Shape::Object { selection: Some(_), .. }));
            ui.weak(if has { tr("adjust-object-selected") } else { tr("adjust-object-hint") });
        }
        AiUse::Description => {
            let response = ui.add(egui::TextEdit::singleline(&mut app.adjust.description).hint_text(tr("adjust-describe-hint")));
            let enter = response.lost_focus() && ui.input(|i| i.key_pressed(egui::Key::Enter));
            if (ui.button(tr("adjust-describe-select")).clicked() || enter) && !app.adjust.description.trim().is_empty() {
                let text = app.adjust.description.trim().to_owned();
                app.shell.run("adjust.describe", json!({ "mask": mask.id, "text": text }));
                app.adjust.ai_pending = None;
            }
        }
    }
}

/// The selected mask's tools: the brush's settings, picking a color, mapping the depth, the AI.
fn tools(app: &mut FloraStackApp, ui: &mut egui::Ui, mask: &Mask) {
    ai(app, ui, mask);
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

/// The central view: the preview (or the output before its adjustments), fitted or zoomed, with the selected mask's
/// handles. Zoomed in past the fitted size, the engine renders the part in view (with a margin) at the zoom's size, up
/// to the output's own, and the whole output's last preview fills in around it while that comes.
pub fn view(app: &mut FloraStackApp, ui: &mut egui::Ui) {
    let (pane, response) = ui.allocate_exact_size(ui.available_size(), Sense::click_and_drag());
    ui.painter().rect_filled(pane, 0.0, ui.visuals().extreme_bg_color);
    let ppp = ui.ctx().pixels_per_point();
    let size = app.shell.engine().adjust().map(|v| v.size());
    let shown = if app.adjust.compare { app.adjust.before.is_some() } else { app.adjust.preview.is_some() || app.adjust.zoomed.is_some() };
    let (Some((width, height)), true) = (size, shown) else {
        app.adjust.image_rect = None;
        let text = if app.shell.running(&["adjust.open"]) { tr("adjust-opening") } else { tr("adjust-hint") };
        ui.painter().text(pane.center(), egui::Align2::CENTER_CENTER, text, egui::FontId::proportional(15.0), ui.visuals().weak_text_color());
        // The first preview, at the view's size.
        app.adjust.wanted = Some(wanted(&app.adjust, pane.size() * ppp, None, app.viewer.max_side()));
        request_preview(app);
        return;
    };
    #[allow(clippy::cast_precision_loss)] // An image's size.
    let size = Vec2::new(width as f32, height as f32);
    navigate(&mut app.adjust, ui, &response, (pane, ppp, size));
    let ask = wanted(&app.adjust, pane.size() * ppp, Some(size), app.viewer.max_side());
    if Some(ask) != app.adjust.wanted {
        app.adjust.wanted = Some(ask);
        if app.adjust.sent != Some(ask) {
            app.adjust.want_preview(false);
        }
    }
    request_preview(app);
    let gpu = app.viewer.gpu();
    let ctx = ui.ctx().clone();
    let a = &mut app.adjust;
    let (zoom, center) = (a.zoom, a.center);
    // A picture lying at `placement` in a preview of the whole output, drawn with the view.
    let place = |slot: usize, picture: &mut Picture, placement: Option<Placement>| {
        #[allow(clippy::cast_precision_loss)] // Sizes in pixels.
        let (scale, corner) = match placement {
            Some(p) => (p.full.0 as f32 / size.x, Vec2::new(p.x as f32, p.y as f32)),
            None => (picture.size().x / size.x, Vec2::ZERO),
        };
        draw(ui, &ctx, gpu, slot, picture, (pane, pane), zoom / scale, center * scale - corner, 1.0);
    };
    if a.compare {
        if let Some((picture, placement)) = a.before.as_mut() {
            place(0, picture, Some(*placement));
        }
    } else {
        if let Some(picture) = a.preview.as_mut() {
            place(0, picture, None);
        }
        if let (false, Some((picture, placement))) = (a.fit, a.zoomed.as_mut()) {
            place(1, picture, Some(*placement));
        }
    }
    let image = Rect::from_min_size(pane.center() - center * zoom / ppp, size * zoom / ppp);
    a.image_rect = Some(image);
    if a.compare {
        label(ui, pane, tr("adjust-before"));
        return;
    }
    if !a.panning {
        interact(app, ui, &response, image);
    }
    if app.shell.running(&["adjust.preview"]) || app.adjust.pending.is_some() {
        ui.ctx().request_repaint_after(std::time::Duration::from_millis(30));
    }
}

/// Panning (the middle or right button, or Space with the left), zooming (the wheel or a pinch, about the pointer, and
/// eased), and fitting again (a double click), as in the Retouch tab.
fn navigate(a: &mut AdjustUi, ui: &egui::Ui, response: &egui::Response, (pane, ppp, size): (Rect, f32, Vec2)) {
    let fitted = fit_zoom(pane.size() * ppp, size);
    if a.fit {
        (a.zoom, a.target, a.center, a.anchor) = (fitted, fitted, size / 2.0, Vec2::ZERO);
    }
    let space = ui.input(|i| i.key_down(egui::Key::Space));
    a.panning = response.dragged_by(egui::PointerButton::Middle)
        || response.dragged_by(egui::PointerButton::Secondary)
        || (space && response.dragged_by(egui::PointerButton::Primary));
    if a.panning {
        a.fit = false;
        a.center -= response.drag_delta() * ppp / a.zoom;
    }
    if response.hovered() {
        let (scroll, pinch, hover) = ui.input(|i| (i.smooth_scroll_delta.y, i.zoom_delta(), i.pointer.hover_pos()));
        let factor = pinch * (scroll * 0.0015).exp();
        if (factor - 1.0).abs() > 1e-4 {
            a.fit = false;
            if let Some(p) = hover {
                a.anchor = (p - pane.center()) * ppp;
            }
            a.target = (a.target * factor).clamp(MIN_ZOOM.min(fitted), MAX_ZOOM);
        }
    }
    if response.double_clicked() {
        (a.fit, a.zoomed) = (true, None);
        a.want_preview(false);
    }
    if (a.target / a.zoom).ln().abs() > 1e-3 {
        let dt = ui.input(|i| i.stable_dt).min(0.1);
        let next = a.zoom * (a.target / a.zoom).powf((dt * 14.0).min(1.0));
        a.center = zoomed_center(a.center, a.anchor, a.zoom, next);
        a.zoom = next;
        ui.ctx().request_repaint();
    } else if (a.zoom - a.target).abs() > 0.0 {
        a.center = zoomed_center(a.center, a.anchor, a.zoom, a.target);
        a.zoom = a.target;
    }
}

/// The preview the view wants: the whole output fitted in the view (`view`, in device pixels) when it fits it or is
/// zoomed out further; zoomed in, a window of the output at the zoom's size (never above its own), around the part in
/// view and with a margin for panning and for the adjustments that read around each pixel. The window shown is kept
/// while it still covers the view. Without the output's `size`, the view's size.
fn wanted(a: &AdjustUi, view: Vec2, size: Option<Vec2>, max: usize) -> Ask {
    #[allow(clippy::cast_possible_truncation, clippy::cast_sign_loss)] // Sizes in pixels.
    let pixels = |v: f32| (v.round().max(1.0) as usize).min(max.max(1));
    let fitted = Ask { size: (pixels(view.x), pixels(view.y)), window: None };
    let Some(size) = size else { return fitted };
    if a.fit || a.target <= fit_zoom(view, size) * 1.001 {
        return fitted;
    }
    // The zoomed preview's scale: the zoom's, at most the output's own size, and within what the engine renders.
    let scale = a.target.min(1.0).min(MAX_PREVIEW_SIDE / size.x.max(size.y));
    let full = size * scale;
    let shown = Rect::from_center_size((a.center * scale).to_pos2(), view * scale / a.target).intersect(Rect::from_min_size(Pos2::ZERO, full));
    if let Some((_, p)) = &a.zoomed {
        #[allow(clippy::cast_precision_loss)] // Sizes in pixels.
        let (same, covered) = (
            (p.full.0 as f32 - full.x).abs() <= 1.0 && (p.full.1 as f32 - full.y).abs() <= 1.0,
            a.wanted.and_then(|w| w.window).map(|[x, y, w, h]| Rect::from_min_size(Pos2::new(x as f32, y as f32), Vec2::new(w as f32, h as f32))),
        );
        if same
            && covered.is_some_and(|c| c.contains_rect(shown))
            && let Some(w) = a.wanted
        {
            return w;
        }
    }
    let margin = (0.02 * full.x.max(full.y)).clamp(16.0, 256.0) + 0.25 * shown.width().max(shown.height());
    let window = shown.expand(margin).intersect(Rect::from_min_size(Pos2::ZERO, full));
    #[allow(clippy::cast_possible_truncation, clippy::cast_sign_loss)] // Pixels of a preview within 16384 a side.
    let (x0, y0, x1, y1) = (window.min.x.floor() as usize, window.min.y.floor() as usize, window.max.x.ceil() as usize, window.max.y.ceil() as usize);
    #[allow(clippy::cast_possible_truncation, clippy::cast_sign_loss)] // As above.
    let size = ((full.x.round() as usize).max(1), (full.y.round() as usize).max(1));
    Ask { size, window: Some([x0, y0, (x1 - x0).max(1), (y1 - y0).max(1)]) }
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
    let painter = ui.painter().with_clip_rect(image.intersect(response.rect));
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
    // Selecting with the AI: a click on the image selects there; an Alt-click leaves the part there out.
    if ai_use(app, &mask) == Some(AiUse::Clicks) && !app.adjust.picking {
        select_by_clicks(app, ui, response, (image, at), &mask);
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

/// The selected mask's AI clicks, drawn; a click adds one and asks the model again (`adjust.select`, with every click).
fn select_by_clicks(app: &mut FloraStackApp, ui: &egui::Ui, response: &egui::Response, (image, at): (Rect, Option<Pos2>), mask: &Mask) {
    let clicks: Vec<Click> = mask
        .components
        .iter()
        .find_map(|c| if let Shape::Object { clicks, .. } = &c.shape { Some(clicks.clone()) } else { None })
        .unwrap_or_default();
    let painter = ui.painter().with_clip_rect(image.intersect(response.rect));
    for c in &clicks {
        let color = if c.include { egui::Color32::from_rgb(40, 200, 90) } else { egui::Color32::from_rgb(230, 40, 40) };
        painter.circle(to_screen(image, c.at), 5.0, color, egui::Stroke::new(1.5, egui::Color32::WHITE));
    }
    if at.is_some() {
        ui.ctx().set_cursor_icon(egui::CursorIcon::Crosshair);
    }
    let busy = app.shell.running(&["adjust.select", "adjust.describe", "adjust.setModel"]);
    if response.clicked()
        && !busy
        && let Some(p) = at
    {
        let leave_out = ui.input(|i| i.modifiers.alt);
        let mut clicks = clicks;
        if clicks.is_empty() && leave_out {
            return;
        }
        clicks.push(Click { at: to_image(image, p), include: !leave_out });
        clicks.truncate(florastack_engine::adjust::MAX_CLICKS);
        app.shell.run("adjust.select", json!({ "mask": mask.id, "clicks": clicks }));
        app.adjust.ai_pending = None;
    }
}

/// Painting the selected mask's brush: its circle under the pointer, the stroke drawn as the pointer drags, and the
/// stroke sent (`adjust.stroke`) when the button is let go or the image clicked.
fn paint(app: &mut FloraStackApp, ui: &egui::Ui, response: &egui::Response, (image, at): (Rect, Option<Pos2>), mask: u32) {
    let long = image.width().max(image.height());
    let painter = ui.painter().with_clip_rect(image.intersect(response.rect));
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
