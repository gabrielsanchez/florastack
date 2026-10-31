//! The Retouch tab: the source (left) and the output being retouched (right) side by side, with zoom and pan locked
//! together and the brush's circle shown in both, and in the right panel the output, its layers, the source and the
//! brush. Everything goes through the engine's `retouch.*` commands: dragging on the output paints live
//! (`retouch.paint`) and commits the stroke as one undo step when the button is let go (`retouch.stroke`); Ctrl+click
//! (or hovering, with "Follow the sharpest frame" on) picks the frame where that point is sharpest as the source.
//! The views show what the engine holds (`florastack_engine::retouch::RetouchView`), updated region by region as
//! strokes paint, undo and redo; the brightness boost is the view's alone.

use std::sync::Arc;
use std::time::{Duration, Instant};

use egui::{Pos2, Rect, RichText, Sense, Vec2};
use florastack_engine::{Brush, RetouchSource, Value, json};

use crate::app::FloraStackApp;
use crate::i18n::{tr, tr_args};
use crate::viewer::{Picture, draw, fit_zoom, zoomed_center};

/// How long the pointer rests on a point before "Follow the sharpest frame" loads its frame.
const FOLLOW_REST: Duration = Duration::from_millis(300);
/// The zoom's limits, in device pixels per display pixel.
const MIN_ZOOM: f32 = 1.0 / 64.0;
const MAX_ZOOM: f32 = 64.0;

/// The brushes, with their labels' and tooltips' keys.
pub const BRUSHES: [(Brush, &str, &str); 5] = [
    (Brush::Details, "brush-details", "tip-brush-details"),
    (Brush::Pixels, "brush-pixels", "tip-brush-pixels"),
    (Brush::Darken, "brush-darken", "tip-brush-darken"),
    (Brush::Lighten, "brush-lighten", "tip-brush-lighten"),
    (Brush::Eraser, "brush-eraser", "tip-brush-eraser"),
];

/// The Retouch tab's state: what it shows of the engine's retouch session, the brush, and the stroke being drawn.
pub struct RetouchUi {
    /// The output open, and its pictures: the result and the source, reduced by `factor` for display.
    pub output: Option<u64>,
    result: Option<Picture>,
    source: Option<Picture>,
    factor: usize,
    /// The view, shared by both pictures: device pixels per display pixel, now and where it is going, the display
    /// point at each pane's center, the point the zoom is about, and whether it follows the window.
    zoom: f32,
    target: f32,
    center: Vec2,
    anchor: Vec2,
    fit: bool,
    pub brush: Brush,
    pub size: f64,
    pub hardness: f64,
    pub opacity: f64,
    pub pressure: bool,
    pub boost: f32,
    pub follow: bool,
    /// The layer strokes go into; the top one when none is picked.
    pub layer: Option<u64>,
    /// The stroke being drawn: its samples in output pixels, and how many `retouch.paint` has had.
    stroke: Vec<[f64; 3]>,
    sent: usize,
    /// The pen's last pressure, when the platform reports one.
    force: Option<f32>,
    /// The frame the pointer rests over with Follow on, since when.
    resting: Option<(u64, Instant)>,
    /// The panes' rectangles as last drawn (the tests paint into them).
    pub panes: Option<(Rect, Rect)>,
    /// The output the tab opened on its own, once, when it was shown with none open.
    asked: Option<u64>,
}

impl Default for RetouchUi {
    fn default() -> RetouchUi {
        RetouchUi {
            output: None,
            result: None,
            source: None,
            factor: 1,
            zoom: 1.0,
            target: 1.0,
            center: Vec2::ZERO,
            anchor: Vec2::ZERO,
            fit: true,
            brush: Brush::Details,
            size: 60.0,
            hardness: 0.3,
            opacity: 1.0,
            pressure: true,
            boost: 1.0,
            follow: false,
            layer: None,
            stroke: Vec::new(),
            sent: 0,
            force: None,
            resting: None,
            panes: None,
            asked: None,
        }
    }
}

impl RetouchUi {
    /// Whether a stroke is being drawn.
    pub fn drawing(&self) -> bool {
        !self.stroke.is_empty()
    }

    /// Whether the result is shown.
    pub fn is_shown(&self) -> bool {
        self.result.is_some()
    }

    /// Makes the brush smaller or larger by `factor`.
    pub fn resize_brush(&mut self, factor: f64) {
        self.size = (self.size * factor).clamp(1.0, 4000.0);
    }

    /// The display point under `pos`, in a pane `pane` drawn with the view.
    fn point(&self, pane: Rect, pos: Pos2, ppp: f32) -> Vec2 {
        self.center + (pos - pane.center()) * ppp / self.zoom
    }

    /// A display point in output pixels.
    fn output_point(&self, p: Vec2) -> (f64, f64) {
        let f = self.factor as f64;
        (f64::from(p.x) * f, f64::from(p.y) * f)
    }

    /// The stroke record with the brush's settings and `samples`.
    fn record(&self, source: Option<RetouchSource>, samples: &[[f64; 3]]) -> Value {
        let mut stroke = json!({
            "brush": self.brush,
            "size": self.size,
            "hardness": self.hardness,
            "opacity": self.opacity,
            "samples": samples,
        });
        if let (true, Some(source)) = (self.brush.needs_source(), source) {
            stroke["source"] = json!(source);
        }
        stroke
    }
}

/// Brings the pictures up to date with what changed in the engine's retouch session.
pub fn sync(app: &mut FloraStackApp) {
    let Some(changes) = app.shell.retouch.take() else { return };
    let max_side = app.viewer.max_side();
    let engine = app.shell.engine();
    let Some(view) = engine.retouch() else {
        (app.retouch.output, app.retouch.result, app.retouch.source) = (None, None, None);
        return;
    };
    let ui = &mut app.retouch;
    let (w, h) = view.size();
    let whole = florastack_engine::Rect::new(0, 0, w, h);
    let output = view.output().get();
    if changes.opened || ui.output != Some(output) || ui.result.is_none() {
        ui.factor = w.max(h).div_ceil(max_side.max(1)).max(1);
        ui.result = view.result(whole, ui.factor).ok().map(|(image, _)| Picture::new(Arc::new(image)));
        ui.source = None;
        ui.output = Some(output);
        ui.fit = true;
        ui.layer = None;
    } else if let (Some(region), Some(result)) = (changes.region, &mut ui.result)
        && let Ok((image, at)) = view.result(region, ui.factor)
    {
        result.update([at.x, at.y], Arc::new(image));
    }
    if changes.source || changes.opened {
        ui.source = view.source_pixels(whole, ui.factor).and_then(Result::ok).map(|(image, _)| Picture::new(Arc::new(image)));
    }
}

/// The right panel's Retouch tab.
pub fn panel(app: &mut FloraStackApp, ui: &mut egui::Ui) {
    ui.heading(tr("tab-retouch"));
    if app.shell.outputs.is_empty() {
        ui.label(tr("retouch-nothing"));
        return;
    }
    let opening = app.shell.running(&["retouch.open"]);
    // The output: the one open, or the one to open.
    let current = app.retouch.output;
    let name = |id: Option<u64>| {
        id.and_then(|id| app.shell.outputs.iter().find(|o| o.id == id)).map_or_else(|| tr("retouch-pick-output"), |o| o.name.clone())
    };
    let mut picked = None;
    egui::ComboBox::from_label(tr("retouch-output")).selected_text(name(current)).show_ui(ui, |ui| {
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
    if current.is_none() && !opening && app.retouch.asked.is_none() {
        picked = picked.or(wanted);
    }
    if let Some(id) = picked {
        app.retouch.asked = Some(id);
        app.shell.run("retouch.open", json!({ "output": id }));
    }
    if opening {
        ui.weak(tr("retouch-opening"));
    }
    let Some(output) = current.filter(|_| app.retouch.is_shown()) else { return };
    ui.separator();
    layers(app, ui, output);
    ui.separator();
    source(app, ui, output);
    ui.separator();
    brush(app, ui);
}

/// The output's layers: each one's visibility, the one strokes go into, and adding and removing them.
fn layers(app: &mut FloraStackApp, ui: &mut egui::Ui, output: u64) {
    ui.label(RichText::new(tr("retouch-layers")).strong());
    let layers: Vec<(u64, String, bool, usize)> = app
        .shell
        .engine()
        .project()
        .and_then(|p| p.outputs.iter().find(|o| o.id().get() == output))
        .map(|o| o.retouch.iter().map(|l| (l.id().get(), l.name.clone(), l.visible, l.strokes.len())).collect())
        .unwrap_or_default();
    if layers.is_empty() {
        ui.weak(tr("retouch-no-layers"));
    }
    let active = app.retouch.layer.or_else(|| layers.last().map(|l| l.0));
    for (id, name, visible, strokes) in layers.iter().rev() {
        ui.horizontal(|ui| {
            let mut shown = *visible;
            if ui.checkbox(&mut shown, "").on_hover_text(tr("tip-layer-visible")).changed() {
                app.shell.run("retouch.setLayerVisible", json!({ "output": output, "layer": id, "visible": shown }));
            }
            let label = tr_args("retouch-layer", &[("name", name.clone().into()), ("strokes", (*strokes).into())]);
            if ui.selectable_label(active == Some(*id), label).on_hover_text(tr("tip-layer-active")).clicked() {
                app.retouch.layer = Some(*id);
            }
        });
    }
    ui.horizontal(|ui| {
        if ui.button(tr("retouch-add-layer")).clicked()
            && let Some(added) = app.shell.run("retouch.addLayer", json!({ "output": output }))
        {
            app.retouch.layer = added["layer"].as_u64();
        }
        if let Some(id) = active
            && ui.button(tr("retouch-remove-layer")).clicked()
        {
            app.shell.run("retouch.removeLayer", json!({ "output": output, "layer": id }));
            app.retouch.layer = None;
        }
    });
}

/// The source: a frame or another output picked by hand, or the sharpest frame where the pointer is.
fn source(app: &mut FloraStackApp, ui: &mut egui::Ui, output: u64) {
    ui.label(RichText::new(tr("retouch-source")).strong());
    let (current, sharpest) = app.shell.engine().retouch().map_or((None, false), |v| (v.source(), v.has_sharpest()));
    let loading = app.shell.running(&["retouch.setSource"]);
    let frame_name = |id: u64| app.shell.frames.iter().find(|f| f.id == id).map_or_else(|| id.to_string(), |f| f.name.clone());
    let output_name = |id: u64| app.shell.outputs.iter().find(|o| o.id == id).map_or_else(|| id.to_string(), |o| o.name.clone());
    let shown = match current {
        Some(RetouchSource::Frame(f)) => tr_args("retouch-source-frame", &[("name", frame_name(f.get()).into())]),
        Some(RetouchSource::Output(o)) => tr_args("retouch-source-output", &[("name", output_name(o.get()).into())]),
        None => tr("retouch-source-none"),
    };
    ui.label(if loading { tr("retouch-source-loading") } else { shown });
    let mut pick = None;
    egui::ComboBox::from_label(tr("retouch-frame")).selected_text(tr("retouch-pick")).show_ui(ui, |ui| {
        for frame in &app.shell.frames {
            if ui.selectable_label(current.is_some() && current == frame_source(frame.id), &frame.name).clicked() {
                pick = Some(json!({ "frame": frame.id }));
            }
        }
    });
    egui::ComboBox::from_label(tr("retouch-other-output")).selected_text(tr("retouch-pick")).show_ui(ui, |ui| {
        for other in app.shell.outputs.iter().filter(|o| o.id != output) {
            if ui.selectable_label(false, &other.name).clicked() {
                pick = Some(json!({ "output": other.id }));
            }
        }
    });
    if let Some(params) = pick {
        app.shell.run("retouch.setSource", params);
    }
    if sharpest {
        ui.checkbox(&mut app.retouch.follow, tr("retouch-follow")).on_hover_text(tr("tip-follow"));
        ui.weak(tr("retouch-jump-hint"));
    } else if app.shell.running(&["retouch.mapFocus"]) {
        ui.weak(tr("retouch-mapping"));
    } else if ui.button(tr("retouch-map")).on_hover_text(tr("tip-map")).clicked() {
        app.shell.run("retouch.mapFocus", json!({}));
    }
}

/// Frame `id` as a source (the project's ids are made only from their JSON outside it).
fn frame_source(id: u64) -> Option<RetouchSource> {
    serde_json::from_value(json!({ "frame": id })).ok()
}

/// The brush and its settings, and the view's brightness boost.
fn brush(app: &mut FloraStackApp, ui: &mut egui::Ui) {
    ui.label(RichText::new(tr("retouch-brush")).strong());
    let r = &mut app.retouch;
    for (brush, label, tip) in BRUSHES {
        if ui.radio(r.brush == brush, tr(label)).on_hover_text(tr(tip)).clicked() {
            r.brush = brush;
        }
    }
    ui.add(egui::Slider::new(&mut r.size, 1.0..=1000.0).logarithmic(true).text(tr("brush-size"))).on_hover_text(tr("tip-brush-size"));
    ui.add(egui::Slider::new(&mut r.hardness, 0.0..=1.0).text(tr("brush-hardness"))).on_hover_text(tr("tip-brush-hardness"));
    ui.add(egui::Slider::new(&mut r.opacity, 0.0..=1.0).text(tr("brush-opacity"))).on_hover_text(tr("tip-brush-opacity"));
    ui.checkbox(&mut r.pressure, tr("brush-pressure")).on_hover_text(tr("tip-brush-pressure"));
    ui.add(egui::Slider::new(&mut r.boost, 1.0..=8.0).logarithmic(true).text(tr("retouch-boost"))).on_hover_text(tr("tip-boost"));
}

/// What the pointer does over the view this frame: the panes, the pane it is over (and whether it is the result's)
/// with the display point under it, and the keys held.
struct Pointer {
    left: Rect,
    right: Rect,
    ppp: f32,
    over: Option<(Rect, bool, Pos2)>,
    under: Option<Vec2>,
    space: bool,
    ctrl: bool,
}

impl Pointer {
    fn on_result(&self) -> bool {
        self.over.is_some_and(|(_, result, _)| result)
    }
}

/// The central view: the source and the result side by side.
pub fn view(app: &mut FloraStackApp, ui: &mut egui::Ui) {
    let (rect, response) = ui.allocate_exact_size(ui.available_size(), Sense::click_and_drag());
    ui.painter().rect_filled(rect, 0.0, ui.visuals().extreme_bg_color);
    let Some(size) = app.retouch.result.as_ref().map(Picture::size) else {
        let text = if app.shell.running(&["retouch.open"]) { tr("retouch-opening") } else { tr("retouch-hint") };
        ui.painter().text(rect.center(), egui::Align2::CENTER_CENTER, text, egui::FontId::proportional(15.0), ui.visuals().weak_text_color());
        return;
    };
    let ppp = ui.ctx().pixels_per_point();
    let half = (rect.width() - 2.0) / 2.0;
    let left = Rect::from_min_size(rect.min, Vec2::new(half, rect.height()));
    let right = Rect::from_min_size(rect.min + Vec2::new(half + 2.0, 0.0), Vec2::new(half, rect.height()));
    let r = &mut app.retouch;
    r.panes = Some((left, right));
    if r.fit {
        let z = fit_zoom(left.size() * ppp, size);
        (r.zoom, r.target, r.center, r.anchor) = (z, z, size / 2.0, Vec2::ZERO);
    }
    let (hover, space, ctrl, events) = ui.input(|i| (i.pointer.hover_pos(), i.key_down(egui::Key::Space), i.modifiers.command, i.events.clone()));
    for event in &events {
        if let egui::Event::Touch { force: Some(force), .. } = event {
            r.force = Some(*force);
        }
    }
    let over =
        hover.and_then(|p| [(left, false), (right, true)].into_iter().find(|(pane, _)| pane.contains(p)).map(|(pane, result)| (pane, result, p)));
    let under = over.map(|(pane, _, p)| r.point(pane, p, ppp));
    let pointer = Pointer { left, right, ppp, over, under, space, ctrl };
    navigate(r, ui, &response, &pointer);
    draw_panes(app, ui, &pointer);
    let current = pick_source(app, ui, &response, &pointer);
    paint(app, &response, &pointer, current);
}

/// Panning (the middle or right button, or Space with the left), zooming (the wheel or a pinch, about the pointer, and
/// eased), and fitting again (a double click).
fn navigate(r: &mut RetouchUi, ui: &egui::Ui, response: &egui::Response, pointer: &Pointer) {
    let ppp = pointer.ppp;
    let panning = response.dragged_by(egui::PointerButton::Middle)
        || response.dragged_by(egui::PointerButton::Secondary)
        || (pointer.space && response.dragged_by(egui::PointerButton::Primary));
    if panning {
        r.fit = false;
        r.center -= response.drag_delta() * ppp / r.zoom;
    }
    if response.hovered() {
        let (scroll, pinch) = ui.input(|i| (i.smooth_scroll_delta.y, i.zoom_delta()));
        let factor = pinch * (scroll * 0.0015).exp();
        if (factor - 1.0).abs() > 1e-4 {
            r.fit = false;
            if let Some((pane, _, p)) = pointer.over {
                r.anchor = (p - pane.center()) * ppp;
            }
            r.target = (r.target * factor).clamp(MIN_ZOOM, MAX_ZOOM);
        }
    }
    if response.double_clicked() {
        r.fit = true;
    }
    if (r.target / r.zoom).ln().abs() > 1e-3 {
        let dt = ui.input(|i| i.stable_dt).min(0.1);
        let next = r.zoom * (r.target / r.zoom).powf((dt * 14.0).min(1.0));
        r.center = zoomed_center(r.center, r.anchor, r.zoom, next);
        r.zoom = next;
        ui.ctx().request_repaint();
    } else if (r.zoom - r.target).abs() > 0.0 {
        r.center = zoomed_center(r.center, r.anchor, r.zoom, r.target);
        r.zoom = r.target;
    }
}

/// Both pictures with the view and the boost, the brush's circle at the same point of both, and the panes' names.
fn draw_panes(app: &mut FloraStackApp, ui: &egui::Ui, pointer: &Pointer) {
    let (left, right, ppp) = (pointer.left, pointer.right, pointer.ppp);
    let ctx = ui.ctx().clone();
    let gpu = app.viewer.gpu();
    let output_name = app.retouch.output.and_then(|id| app.shell.outputs.iter().find(|o| o.id == id)).map(|o| o.name.clone()).unwrap_or_default();
    let r = &mut app.retouch;
    let (zoom, center, boost) = (r.zoom, r.center, r.boost);
    if let Some(source) = &mut r.source {
        draw(ui, &ctx, gpu, 0, source, (left, left), zoom, center, boost);
    } else {
        let weak = ui.visuals().weak_text_color();
        ui.painter().text(left.center(), egui::Align2::CENTER_CENTER, tr("retouch-source-none"), egui::FontId::proportional(14.0), weak);
    }
    if let Some(result) = &mut r.result {
        draw(ui, &ctx, gpu, 1, result, (right, right), zoom, center, boost);
    }
    if let Some(p) = pointer.under {
        #[allow(clippy::cast_possible_truncation)] // A radius on screen.
        let radius = ((r.size / 2.0 / r.factor as f64) as f32 * r.zoom / ppp).max(1.0);
        for pane in [left, right] {
            let at = pane.center() + (p - r.center) * r.zoom / ppp;
            let clipped = ui.painter().with_clip_rect(pane);
            clipped.circle_stroke(at, radius, egui::Stroke::new(1.5, egui::Color32::BLACK));
            clipped.circle_stroke(at, radius, egui::Stroke::new(0.75, egui::Color32::WHITE));
        }
        ui.ctx().set_cursor_icon(egui::CursorIcon::Crosshair);
    }
    let names = ui.painter();
    let label = |pane: Rect, text: String| {
        let galley = names.layout_no_wrap(text, egui::FontId::proportional(13.0), egui::Color32::WHITE);
        let back = Rect::from_min_size(pane.left_top() + Vec2::splat(8.0), galley.size()).expand(4.0);
        names.rect_filled(back, 3.0, egui::Color32::from_black_alpha(150));
        names.galley(back.min + Vec2::splat(4.0), galley, egui::Color32::WHITE);
    };
    label(left, tr("retouch-source"));
    label(right, output_name);
}

/// The pointer's sample in output pixels, with the pen's pressure when it is used.
fn sample(r: &RetouchUi, pointer: &Pointer) -> Option<[f64; 3]> {
    let pressure = if r.pressure { f64::from(r.force.unwrap_or(1.0).clamp(0.0, 1.0)) } else { 1.0 };
    pointer.under.map(|p| {
        let (x, y) = r.output_point(p);
        [x, y, pressure]
    })
}

/// Ctrl+click picks the sharpest frame at the point as the source; with Follow on, resting the pointer does. Returns
/// the source strokes paint from now.
fn pick_source(app: &mut FloraStackApp, ui: &egui::Ui, response: &egui::Response, pointer: &Pointer) -> Option<RetouchSource> {
    let at = sample(&app.retouch, pointer).filter(|_| pointer.on_result());
    let (sharpest, current) = {
        let view = app.shell.engine().retouch();
        let sharpest = at.and_then(|[x, y, _]| view.as_ref().and_then(|v| v.sharpest(x, y))).map(florastack_engine::model::FrameId::get);
        (sharpest, view.as_ref().and_then(florastack_engine::retouch::RetouchView::source))
    };
    let r = &mut app.retouch;
    let mut source_to = None;
    if response.clicked() && pointer.ctrl {
        source_to = sharpest;
    } else if r.follow && !r.drawing() {
        match (sharpest, r.resting) {
            (Some(f), Some((g, since))) if f == g => {
                if since.elapsed() >= FOLLOW_REST && current != frame_source(f) {
                    source_to = Some(f);
                    r.resting = None;
                } else {
                    ui.ctx().request_repaint_after(FOLLOW_REST);
                }
            }
            (Some(f), _) => {
                r.resting = Some((f, Instant::now()));
                ui.ctx().request_repaint_after(FOLLOW_REST);
            }
            (None, _) => r.resting = None,
        }
    }
    if let Some(frame) = source_to.filter(|f| current != frame_source(*f)) {
        app.shell.run("retouch.setSource", json!({ "frame": frame }));
    }
    current
}

/// Painting: a drag with the left button on the result paints live and commits when let go; a click paints a dab.
fn paint(app: &mut FloraStackApp, response: &egui::Response, pointer: &Pointer, current: Option<RetouchSource>) {
    let s = sample(&app.retouch, pointer);
    let r = &mut app.retouch;
    if response.drag_started_by(egui::PointerButton::Primary) && !pointer.space && pointer.on_result() {
        r.stroke.clear();
        r.sent = 0;
    }
    if response.dragged_by(egui::PointerButton::Primary) && !pointer.space && (pointer.on_result() || r.drawing()) {
        if let Some(s) = s
            && r.stroke.last().is_none_or(|last| (last[0] - s[0]).hypot(last[1] - s[1]) >= 0.5)
        {
            r.stroke.push(s);
        }
        let begin = r.sent == 0;
        if r.stroke.len() > r.sent {
            let new = r.stroke[r.sent..].to_vec();
            let record = r.record(current, &new);
            r.sent = r.stroke.len();
            if app.shell.run("retouch.paint", json!({ "begin": begin, "stroke": record })).is_none() {
                app.retouch.stroke.clear();
            }
        }
    }
    let r = &mut app.retouch;
    let let_go = response.drag_stopped_by(egui::PointerButton::Primary) && r.drawing();
    let clicked = response.clicked() && !pointer.ctrl && pointer.on_result();
    if let_go || clicked {
        if r.stroke.is_empty()
            && let Some(s) = s
        {
            r.stroke.push(s);
        }
        let record = r.record(current, &r.stroke);
        let mut params = json!({ "stroke": record });
        if let Some(layer) = r.layer {
            params["layer"] = json!(layer);
        }
        r.stroke.clear();
        r.sent = 0;
        app.shell.run("retouch.stroke", params);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn strokes_carry_the_brush_and_its_source() {
        let ui = RetouchUi { brush: Brush::Pixels, size: 30.0, ..RetouchUi::default() };
        let record = ui.record(frame_source(4), &[[1.0, 2.0, 0.5]]);
        assert_eq!(record["brush"], "pixels");
        assert_eq!(record["source"], json!({ "frame": 4 }));
        assert_eq!(record["samples"], json!([[1.0, 2.0, 0.5]]));
        let eraser = RetouchUi { brush: Brush::Eraser, ..RetouchUi::default() }.record(frame_source(4), &[[1.0, 2.0, 1.0]]);
        assert!(eraser.get("source").is_none(), "the eraser takes no source");
        let mut ui = RetouchUi::default();
        ui.resize_brush(1e9);
        assert_eq!(ui.size, 4000.0);
    }
}
