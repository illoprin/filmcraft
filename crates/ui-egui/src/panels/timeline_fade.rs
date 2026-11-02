//! Audio fade handles on the Timeline: a small triangle at each top corner of an audio clip.
//! Hovering one shows the resize cursor; dragging it sets a one-sided fade at that edge (the
//! `clip.fade` command, a Constant Gain transition). The fade is drawn as a ramp on the clip — its
//! flat end meeting the clip's Volume line, straight for Constant Gain and a smooth Bezier for
//! Exponential Fade — and reflected in the waveform. The old transition box is not drawn for these
//! one-sided fades (see [`is_fade_transition`]). A gray boundary line marks the fade's inner edge:
//! the Volume line is hidden and cannot be changed inside the fade, and dragging the boundary
//! resizes it (Alt at any point makes it an Exponential Fade, under a frame removes it).
//!
//! Automation ids: `timeline.clip.<id>.fade.in` / `timeline.clip.<id>.fade.out` and their
//! `.boundary`.

use egui::{Color32, CursorIcon, Pos2, Rect, Sense, Stroke, pos2, vec2};
use filmcraft_edit::Edge;
use filmcraft_project::{ClipId, EffectKind, Sequence, TrackId, TrackItem, TrackKind, Transition};
use filmcraft_time::Tick;
use serde_json::{Value, json};

use super::timeline::{Layout, Row};
use crate::FilmcraftApp;
use crate::theme::Tokens;

/// Triangle size (points) of a fade handle.
const HANDLE: f32 = 7.0;
/// Extra hit area around a handle (points).
const HIT: f32 = 4.0;
/// Half-width (points) of the invisible fade-boundary drag zone.
const BOUNDARY: f32 = 4.0;

/// A one-sided audio fade on a clip edge: its sequence-time range and the transition effect id
/// (`constant_gain`, `exponential_fade`, `constant_power`), which sets the ramp's shape.
#[derive(Clone, Debug, PartialEq)]
pub struct Fade {
    pub start: Tick,
    pub end: Tick,
    pub effect: String,
}

fn body(layout: &Layout, r: &Row, it: &TrackItem) -> Rect {
    Rect::from_min_max(pos2(layout.x_of(it.start), r.rect.min.y + 1.0), pos2(layout.x_of(it.end()), r.rect.max.y - 1.0))
}

fn hit_rect(b: Rect, edge: Edge) -> Rect {
    let d = HANDLE + HIT;
    match edge {
        Edge::In => Rect::from_min_max(b.left_top(), b.left_top() + vec2(d, d)),
        Edge::Out => Rect::from_min_max(b.right_top() - vec2(d, 0.0), b.right_top() + vec2(0.0, d)),
    }
}

/// Whether `c` is the one-sided fade of `clip` at `edge`.
fn is_fade(c: &Transition, clip: ClipId, edge: Edge) -> bool {
    c.effect.def().is_some_and(|d| d.kind == EffectKind::AudioTransition)
        && match edge {
            Edge::In => c.to == Some(clip) && c.from.is_none(),
            Edge::Out => c.from == Some(clip) && c.to.is_none(),
        }
}

/// Whether `trn` is a one-sided audio fade (fade in or fade out), which the Timeline draws as a
/// ramp on the clip (see [`paint`]) instead of the transition box used for crossfades.
pub fn is_fade_transition(trn: &Transition) -> bool {
    trn.effect.def().is_some_and(|d| d.kind == EffectKind::AudioTransition) && (trn.from.is_none() || trn.to.is_none())
}

/// The one-sided fades of `clip` on `track`: (fade in, fade out).
pub fn fades_on(seq: &Sequence, track: TrackId, clip: ClipId) -> (Option<Fade>, Option<Fade>) {
    let Some(tr) = seq.track(track) else { return (None, None) };
    let mut fin = None;
    let mut fout = None;
    for c in &tr.transitions {
        if is_fade(c, clip, Edge::In) {
            fin = Some(Fade { start: c.start, end: c.end(), effect: c.effect.effect.clone() });
        } else if is_fade(c, clip, Edge::Out) {
            fout = Some(Fade { start: c.start, end: c.end(), effect: c.effect.effect.clone() });
        }
    }
    (fin, fout)
}

/// [`fades_on`] for the clip's own track (drawing and waveform helpers).
pub fn clip_fades(app: &FilmcraftApp, clip: ClipId) -> (Option<Fade>, Option<Fade>) {
    let Some(q) = app.session.active_sequence() else { return (None, None) };
    let Some((track, _)) = q.find_item(clip) else { return (None, None) };
    fades_on(q, track, clip)
}

/// The sub-range of `clip` where its Volume line is drawn and editable: the clip minus any
/// one-sided fade region at its edges. Inside a fade the line is hidden and its level cannot be
/// changed — the fade, not the Volume line, owns that stretch.
pub fn volume_range(seq: &Sequence, track: TrackId, clip: ClipId) -> (Tick, Tick) {
    let Some(it) = seq.track(track).and_then(|t| t.item(clip)) else { return (Tick::ZERO, Tick::ZERO) };
    let (mut lo, mut hi) = (it.start, it.end());
    if let Some(t) = seq.track(track) {
        for c in &t.transitions {
            if c.effect.def().is_none_or(|d| d.kind != EffectKind::AudioTransition) {
                continue;
            }
            if c.to == Some(clip) && c.from.is_none() {
                lo = lo.max(c.end());
            }
            if c.from == Some(clip) && c.to.is_none() {
                hi = hi.min(c.start);
            }
        }
    }
    (lo.min(hi), hi)
}

/// The ramp's gain at progress `q` (0..1 from the clip edge inwards) for an effect, matching
/// `filmcraft_render::transitions::audio_gains` so the drawing is what the renderer plays.
fn fade_curve(effect: &str, fade_in: bool, q: f32) -> f32 {
    let q = q.clamp(0.0, 1.0);
    match (effect, fade_in) {
        ("exponential_fade", true) => 1.0 - (1.0 - q).powi(3),
        ("exponential_fade", false) => (1.0 - q).powi(3),
        ("constant_power", true) => (q * std::f32::consts::FRAC_PI_2).sin(),
        ("constant_power", false) => (q * std::f32::consts::FRAC_PI_2).cos(),
        (_, true) => q,
        (_, false) => 1.0 - q,
    }
}

/// The fade gain at sequence time `t` (1 outside a fade), for a clip's waveform.
pub fn fade_gain(fin: Option<&Fade>, fout: Option<&Fade>, t: Tick) -> f32 {
    let progress = |f: &Fade| -> f32 {
        if f.end <= f.start {
            return 1.0;
        }
        ((t.0 - f.start.0) as f64 / (f.end.0 - f.start.0) as f64).clamp(0.0, 1.0) as f32
    };
    let mut g = 1.0;
    if let Some(f) = fin {
        g *= fade_curve(&f.effect, true, progress(f));
    }
    if let Some(f) = fout {
        g *= fade_curve(&f.effect, false, progress(f));
    }
    g
}

/// Fill the region between `curve` and `bottom` with a translucent tint, as a triangle strip so a
/// curved (non-convex) shape fills correctly.
fn fill_under(cp: &egui::Painter, curve: &[Pos2], bottom: f32, col: Color32) {
    let mut mesh = egui::Mesh::default();
    for w in curve.windows(2) {
        let (a, b) = (w[0], w[1]);
        let (al, bl) = (pos2(a.x, bottom), pos2(b.x, bottom));
        let i = mesh.vertices.len() as u32;
        mesh.colored_vertex(a, col);
        mesh.colored_vertex(b, col);
        mesh.colored_vertex(bl, col);
        mesh.colored_vertex(al, col);
        mesh.add_triangle(i, i + 1, i + 2);
        mesh.add_triangle(i, i + 2, i + 3);
    }
    cp.add(egui::Shape::mesh(mesh));
}

/// Draw the fade handles, ramps and the boundary between the clip and each fade.
pub fn paint(app: &FilmcraftApp, p: &egui::Painter, body_rect: Rect, it: &TrackItem, t: &Tokens) {
    if body_rect.width() < 2.0 * HANDLE {
        return;
    }
    let (fin, fout) = clip_fades(app, it.id);
    let cp = p.with_clip_rect(body_rect.intersect(p.clip_rect()));
    let x_of = |tt: Tick| {
        body_rect.min.x + (tt.0.saturating_sub(it.start.0) as f64 / it.duration.0.max(1) as f64).clamp(0.0, 1.0) as f32 * body_rect.width()
    };
    let stroke = Stroke::new(1.0, Color32::from_white_alpha(225));
    for (fade, fade_in) in [(fin.as_ref(), true), (fout.as_ref(), false)] {
        let Some(fade) = fade else { continue };
        let (x0, x1) = (x_of(fade.start), x_of(fade.end));
        let bottom = body_rect.max.y;
        // where the ramp flattens: the Volume line's height at the fade's inner end
        let flat_at = if fade_in { fade.end } else { fade.start };
        let line = crate::panels::timeline_volume::line_y(body_rect, it, flat_at).clamp(body_rect.min.y, bottom);
        let n = (((x1 - x0).abs() / 2.0).ceil() as usize).clamp(1, 512);
        let curve: Vec<Pos2> = (0..=n)
            .map(|i| {
                let q = i as f32 / n as f32;
                pos2(x0 + (x1 - x0) * q, bottom + (line - bottom) * fade_curve(&fade.effect, fade_in, q))
            })
            .collect();
        fill_under(&cp, &curve, bottom, Color32::from_white_alpha(26));
        if fade.effect == "exponential_fade" {
            // the easing curve drawn as a smooth cubic Bezier (not a polyline)
            let (c1, c2) = if fade_in { (line, line) } else { (bottom, bottom) };
            let (p0, p3) = if fade_in { (pos2(x0, bottom), pos2(x1, line)) } else { (pos2(x0, line), pos2(x1, bottom)) };
            let dx = x1 - x0;
            cp.add(egui::epaint::CubicBezierShape::from_points_stroke(
                [p0, pos2(x0 + dx / 3.0, c1), pos2(x0 + 2.0 * dx / 3.0, c2), p3],
                false,
                Color32::TRANSPARENT,
                stroke,
            ));
        } else {
            cp.add(egui::Shape::line(curve, stroke));
        }
        // the boundary between the clip and the fade: a gray line with a 1 px shadow (the same
        // divider as the Volume line, so the two read alike)
        let bx = if fade_in { x1 } else { x0 };
        let (y0, y1) = (body_rect.min.y, body_rect.max.y);
        cp.add(egui::Shape::line_segment([pos2(bx + 1.0, y0), pos2(bx + 1.0, y1)], Stroke::new(1.0, Color32::from_black_alpha(150))));
        cp.add(egui::Shape::line_segment([pos2(bx, y0), pos2(bx, y1)], Stroke::new(1.0, t.text_dim)));
    }
    // the corner handles
    let col = Color32::from_white_alpha(235);
    let fill = Color32::from_white_alpha(60);
    let tl = body_rect.left_top();
    let tr = body_rect.right_top();
    cp.add(egui::Shape::convex_polygon(vec![tl, tl + vec2(HANDLE, 0.0), tl + vec2(0.0, HANDLE)], fill, Stroke::new(1.0, col)));
    cp.add(egui::Shape::convex_polygon(vec![tr, tr - vec2(HANDLE, 0.0), tr + vec2(0.0, HANDLE)], fill, Stroke::new(1.0, col)));
}

/// A fade handle being dragged: the clip and edge, whether it has changed yet, and the last length.
#[derive(Clone, Copy)]
struct FadeDrag {
    clip: ClipId,
    edge: Edge,
    begun: bool,
    last: Option<Tick>,
}

/// The boundary at a one-sided fade's inner edge being dragged: the clip and edge, whether it has
/// changed yet, the last (length, exponential) it sent, and whether the drag has been made an
/// Exponential Fade (Alt held at any point).
#[derive(Clone, Copy)]
struct BoundaryDrag {
    clip: ClipId,
    edge: Edge,
    begun: bool,
    last: Option<(i64, bool)>,
    exp: bool,
}

/// The fade length a pointer at `x` gives for `edge`.
fn duration_at(it: &TrackItem, b: Rect, edge: Edge, x: f32) -> Tick {
    let f = ((x - b.min.x) / b.width().max(1.0)).clamp(0.0, 1.0) as f64;
    let d = Tick((it.duration.0 as f64 * f).round() as i64);
    match edge {
        Edge::In => d,
        Edge::Out => it.duration - d,
    }
}

/// Hover and drag of the fade handles, run after the Timeline's own interaction so the handles sit
/// on top of the clip (a trim edge or a move would otherwise take the press).
pub fn interact(app: &mut FilmcraftApp, ui: &mut egui::Ui, seq: &Sequence, layout: &Layout, aclip: Rect) {
    if app.tl.drag.is_some() || !matches!(app.ui.tool, crate::state::Tool::Selection | crate::state::Tool::Pen) {
        return;
    }
    let area = aclip.intersect(layout.content);
    if area.width() <= 0.0 {
        return;
    }
    let drag_id = egui::Id::unique("tl-fade-drag");
    let edge_id = egui::Id::unique("tl-fade-edge-drag");
    let held: Option<FadeDrag> = ui.data(|d| d.get_temp(drag_id));
    let held_edge: Option<BoundaryDrag> = ui.data(|d| d.get_temp(edge_id));
    if (held.is_some() || held_edge.is_some()) && ui.input(|i| i.key_pressed(egui::Key::Escape)) {
        if held.is_some_and(|h| h.begun) || held_edge.is_some_and(|h| h.begun) {
            let _ = app.session.execute("edit.undo", json!({}));
        }
        ui.data_mut(|d| {
            d.remove::<FadeDrag>(drag_id);
            d.remove::<BoundaryDrag>(edge_id);
        });
        return;
    }
    let pointer = ui.input(|i| i.pointer.interact_pos());
    let alt = ui.input(|i| i.modifiers.alt);
    let rate = seq.settings.frame_rate;
    let fd = rate.frame_duration();
    let mut next = held;
    let mut next_edge = held_edge;
    let mut acts: Vec<(String, Value)> = Vec::new();
    for r in layout.rows.iter().filter(|r| r.kind == TrackKind::Audio && !r.lane) {
        let Some(tr) = seq.track(r.track).filter(|t| !t.locked) else { continue };
        let row = r.rect.intersect(area);
        if row.height() <= 0.0 {
            continue;
        }
        for it in &tr.items {
            let b = body(layout, r, it);
            if b.width() < 2.0 * HANDLE {
                continue;
            }
            let (fin, fout) = fades_on(seq, r.track, it.id);
            for edge in [Edge::In, Edge::Out] {
                let name = if matches!(edge, Edge::In) { "in" } else { "out" };
                let hr = hit_rect(b, edge).intersect(area);
                if hr.width() > 0.0 {
                    let resp = ui.interact(hr, egui::Id::unique(("tl-fade", it.id.0, name)), Sense::click_and_drag());
                    app.auto.add(&format!("timeline.clip.{}.fade.{name}", it.id.0), hr, "Audio fade handle");
                    let mine = held.filter(|h| h.clip == it.id && h.edge == edge);
                    if resp.hovered() || mine.is_some() {
                        ui.ctx().set_cursor_icon(CursorIcon::ResizeHorizontal);
                    }
                    if resp.drag_started_by(egui::PointerButton::Primary) {
                        next = Some(FadeDrag { clip: it.id, edge, begun: false, last: None });
                    }
                    if resp.dragged()
                        && let (Some(h), Some(p)) = (mine, pointer)
                    {
                        let dur = duration_at(it, b, edge, p.x);
                        if h.last != Some(dur) {
                            acts.push((
                                "clip.fade".into(),
                                json!({"clip": it.id.0, "edge": name, "duration": dur.0, "begin": !h.begun}),
                            ));
                            next = Some(FadeDrag { begun: true, ..h });
                        }
                    }
                }
                // the boundary at the fade's inner edge: drag it to resize the fade. A length under
                // one frame removes the fade; Alt held at any point makes it Exponential.
                let fade = match edge {
                    Edge::In => fin.as_ref(),
                    Edge::Out => fout.as_ref(),
                };
                let Some(fade) = fade else { continue };
                let bx = layout.x_of(if matches!(edge, Edge::In) { fade.end } else { fade.start });
                let er = Rect::from_min_max(pos2(bx - BOUNDARY, b.min.y), pos2(bx + BOUNDARY, b.max.y)).intersect(area);
                if er.width() <= 0.0 {
                    continue;
                }
                let eresp = ui.interact(er, egui::Id::unique(("tl-fade-edge", it.id.0, name)), Sense::click_and_drag());
                app.auto.add(&format!("timeline.clip.{}.fade.{name}.boundary", it.id.0), er, "Audio fade boundary");
                let mine = held_edge.filter(|h| h.clip == it.id && h.edge == edge);
                if eresp.hovered() || mine.is_some() {
                    ui.ctx().set_cursor_icon(CursorIcon::ResizeHorizontal);
                }
                if eresp.drag_started_by(egui::PointerButton::Primary) {
                    next_edge = Some(BoundaryDrag { clip: it.id, edge, begun: false, last: None, exp: false });
                }
                if eresp.dragged()
                    && let (Some(h), Some(p)) = (mine, pointer)
                {
                    let raw = duration_at(it, b, edge, p.x);
                    let hi = it.duration.max(fd);
                    let dur = if raw < fd { Tick::ZERO } else { rate.snap_nearest(raw).clamp(fd, hi) };
                    let exp = h.exp || alt;
                    if h.last != Some((dur.0, exp)) {
                        let effect = if exp { "exponential_fade" } else { "constant_gain" };
                        acts.push((
                            "clip.fade".into(),
                            json!({"clip": it.id.0, "edge": name, "duration": dur.0, "effect": effect, "begin": !h.begun}),
                        ));
                        next_edge = Some(BoundaryDrag { begun: true, last: Some((dur.0, exp)), exp, ..h });
                    }
                }
            }
        }
    }
    if !ui.input(|i| i.pointer.any_down()) {
        next = None;
        next_edge = None;
    }
    ui.data_mut(|d| {
        match next {
            Some(h) => {
                d.insert_temp(drag_id, h);
            }
            None => {
                d.remove::<FadeDrag>(drag_id);
            }
        }
        match next_edge {
            Some(h) => {
                d.insert_temp(edge_id, h);
            }
            None => {
                d.remove::<BoundaryDrag>(edge_id);
            }
        }
    });
    for (id, p) in acts {
        if let Err(e) = app.session.execute(&id, p) {
            app.ui.status = e.to_string();
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use filmcraft_time::Tick;

    fn fade(start: i64, end: i64, effect: &str) -> Fade {
        Fade { start: Tick(start), end: Tick(end), effect: effect.to_string() }
    }

    #[test]
    fn fade_gain_ramps_in_and_out() {
        let fin = fade(0, 100, "constant_gain");
        let fout = fade(900, 1000, "constant_gain");
        assert!((fade_gain(Some(&fin), Some(&fout), Tick(50)) - 0.5).abs() < 1e-6);
        assert!((fade_gain(Some(&fin), Some(&fout), Tick(500)) - 1.0).abs() < 1e-6);
        assert!((fade_gain(Some(&fin), Some(&fout), Tick(950)) - 0.5).abs() < 1e-6);
        assert_eq!(fade_gain(None, None, Tick(500)), 1.0);
        // fully faded at the very start (in) and end (out); a degenerate range is no fade
        assert!(fade_gain(Some(&fin), Some(&fout), Tick(0)) < 1e-6);
        assert!(fade_gain(Some(&fin), Some(&fout), Tick(1000)) < 1e-6);
        assert_eq!(fade_gain(Some(&fade(10, 10, "constant_gain")), None, Tick(10)), 1.0);
    }

    #[test]
    fn fade_gain_follows_the_effect_curve() {
        // the exponential fade is the rendered 1-(1-p)^3 (in) / (1-p)^3 (out), not a straight line
        let fin = fade(0, 100, "exponential_fade");
        let fout = fade(0, 100, "exponential_fade");
        assert!((fade_gain(Some(&fin), None, Tick(50)) - 0.875).abs() < 1e-6);
        assert!((fade_gain(None, Some(&fout), Tick(50)) - 0.125).abs() < 1e-6);
        // equal power (the default transition) is sin / cos of a quarter turn
        let pin = fade(0, 100, "constant_power");
        assert!((fade_gain(Some(&pin), None, Tick(50)) - std::f32::consts::FRAC_PI_4.sin()).abs() < 1e-6);
    }
}
