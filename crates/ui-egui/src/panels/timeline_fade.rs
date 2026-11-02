//! Audio fade handles on the Timeline: a small triangle at each top corner of an audio clip.
//! Hovering one shows the resize cursor; dragging it sets a one-sided Constant Gain fade at that
//! edge (the `clip.fade` command), drawn as a ramp on the clip and reflected in its waveform.
//!
//! Automation ids: `timeline.clip.<id>.fade.in` / `timeline.clip.<id>.fade.out`.

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

/// Whether `c` is the one-sided Constant Gain fade of `clip` at `edge`.
fn is_fade(c: &Transition, clip: ClipId, edge: Edge) -> bool {
    c.effect.def().is_some_and(|d| d.kind == EffectKind::AudioTransition)
        && match edge {
            Edge::In => c.to == Some(clip) && c.from.is_none(),
            Edge::Out => c.from == Some(clip) && c.to.is_none(),
        }
}

/// The one-sided fades of `clip` on `track`: (fade in, fade out) as sequence-time ranges.
pub fn fades_on(seq: &Sequence, track: TrackId, clip: ClipId) -> (Option<(Tick, Tick)>, Option<(Tick, Tick)>) {
    let Some(tr) = seq.track(track) else { return (None, None) };
    let mut fin = None;
    let mut fout = None;
    for c in &tr.transitions {
        if is_fade(c, clip, Edge::In) {
            fin = Some((c.start, c.end()));
        } else if is_fade(c, clip, Edge::Out) {
            fout = Some((c.start, c.end()));
        }
    }
    (fin, fout)
}

/// [`fades_on`] for the clip's own track (drawing and waveform helpers).
pub fn clip_fades(app: &FilmcraftApp, clip: ClipId) -> (Option<(Tick, Tick)>, Option<(Tick, Tick)>) {
    let Some(q) = app.session.active_sequence() else { return (None, None) };
    let Some((track, _)) = q.find_item(clip) else { return (None, None) };
    fades_on(q, track, clip)
}

/// The fade gain at sequence time `t` (1 outside a fade), for a clip's waveform.
pub fn fade_gain(fin: Option<(Tick, Tick)>, fout: Option<(Tick, Tick)>, t: Tick) -> f32 {
    let progress = |r: (Tick, Tick)| -> f32 {
        let (s, e) = r;
        if e <= s {
            return 1.0;
        }
        ((t.0 - s.0) as f64 / (e.0 - s.0) as f64).clamp(0.0, 1.0) as f32
    };
    let mut g = 1.0;
    if let Some(r) = fin {
        g *= progress(r);
    }
    if let Some(r) = fout {
        g *= 1.0 - progress(r);
    }
    g
}

/// Which fade handle (if any) is under `pos` on the audio clip `clip` of `track`.
pub fn on_handle(seq: &Sequence, layout: &Layout, track: TrackId, clip: ClipId, pos: Pos2) -> Option<Edge> {
    let r = layout.rows.iter().find(|r| r.track == track && r.kind == TrackKind::Audio && !r.lane)?;
    let it = seq.track(track)?.item(clip)?;
    let b = body(layout, r, it);
    if b.width() < 2.0 * HANDLE {
        return None;
    }
    [Edge::In, Edge::Out].into_iter().find(|e| hit_rect(b, *e).contains(pos))
}

/// Draw the fade handles and ramps of an audio clip drawn at `body_rect`.
pub fn paint(app: &FilmcraftApp, p: &egui::Painter, body_rect: Rect, it: &TrackItem, _t: &Tokens) {
    if body_rect.width() < 2.0 * HANDLE {
        return;
    }
    let (fin, fout) = clip_fades(app, it.id);
    let cp = p.with_clip_rect(body_rect.intersect(p.clip_rect()));
    let x_of = |tt: Tick| {
        body_rect.min.x + (tt.0.saturating_sub(it.start.0) as f64 / it.duration.0.max(1) as f64).clamp(0.0, 1.0) as f32 * body_rect.width()
    };
    let y_of = |gain: f32| body_rect.max.y - gain * body_rect.height();
    for (edge, range) in [(Edge::In, fin), (Edge::Out, fout)] {
        let Some((s, e)) = range else { continue };
        let (x0, x1) = (x_of(s), x_of(e));
        let (line, wedge) = match edge {
            Edge::In => (
                [pos2(x0, y_of(0.0)), pos2(x1, y_of(1.0))],
                vec![pos2(x0, y_of(0.0)), pos2(x1, y_of(1.0)), pos2(x1, y_of(0.0))],
            ),
            Edge::Out => (
                [pos2(x0, y_of(1.0)), pos2(x1, y_of(0.0))],
                vec![pos2(x0, y_of(1.0)), pos2(x1, y_of(0.0)), pos2(x0, y_of(0.0))],
            ),
        };
        cp.add(egui::Shape::convex_polygon(wedge, Color32::from_white_alpha(26), Stroke::NONE));
        cp.add(egui::Shape::line_segment(line, Stroke::new(1.0, Color32::from_white_alpha(225))));
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
    let held: Option<FadeDrag> = ui.data(|d| d.get_temp(drag_id));
    if held.is_some() && ui.input(|i| i.key_pressed(egui::Key::Escape)) {
        if let Some(h) = held
            && h.begun
        {
            let _ = app.session.execute("edit.undo", json!({}));
        }
        ui.data_mut(|d| d.remove::<FadeDrag>(drag_id));
        return;
    }
    let pointer = ui.input(|i| i.pointer.interact_pos());
    let mut next = held;
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
            for edge in [Edge::In, Edge::Out] {
                let hr = hit_rect(b, edge).intersect(area);
                if hr.width() <= 0.0 {
                    continue;
                }
                let name = if matches!(edge, Edge::In) { "in" } else { "out" };
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
        }
    }
    if !ui.input(|i| i.pointer.any_down()) {
        next = None;
    }
    match next {
        Some(h) => ui.data_mut(|d| {
            d.insert_temp(drag_id, h);
        }),
        None => ui.data_mut(|d| {
            d.remove::<FadeDrag>(drag_id);
        }),
    }
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

    #[test]
    fn fade_gain_ramps_in_and_out() {
        let fin = Some((Tick(0), Tick(100)));
        let fout = Some((Tick(900), Tick(1000)));
        assert!((fade_gain(fin, fout, Tick(50)) - 0.5).abs() < 1e-6);
        assert!((fade_gain(fin, fout, Tick(500)) - 1.0).abs() < 1e-6);
        assert!((fade_gain(fin, fout, Tick(950)) - 0.5).abs() < 1e-6);
        assert_eq!(fade_gain(None, None, Tick(500)), 1.0);
        // fully faded at the very start (in) and end (out); a degenerate range is no fade
        assert!(fade_gain(fin, fout, Tick(0)) < 1e-6);
        assert!(fade_gain(fin, fout, Tick(1000)) < 1e-6);
        assert_eq!(fade_gain(Some((Tick(10), Tick(10))), None, Tick(10)), 1.0);
    }
}
