//! Audio Spectrum Analyzer panel: the magnitude spectrum of the master output (after the Master
//! effects), drawn Voxengo SPAN style — a smooth filled curve over a log-frequency / dB grid with a
//! held peak line and an adjustable slope, fractional-octave smoothing and release.
//!
//! The DSP lives in `filmcraft_audio_dsp::spectrum`; the samples come from
//! [`filmcraft_render::mixer::LiveMix::master_snapshot`], fed by the playback mix path. One FFT per
//! displayed frame runs on the UI thread (allocation-free after the first frame).
//!
//! Automation ids: `audioSpectrum`, `audioSpectrum.plot`, `audioSpectrum.slope`,
//! `audioSpectrum.decay`, `audioSpectrum.smoothing`, `audioSpectrum.peakHold`, `audioSpectrum.reset`;
//! settings state: `ui.panels.spectrum`.

use egui::{Align2, Color32, Mesh, Pos2, Rect, Sense, Shape, Stroke, pos2, vec2};

use filmcraft_audio_dsp::spectrum::{SpectrumAnalyzer, SpectrumConfig};

use crate::FilmcraftApp;
use crate::panels::panel_state::{SPECTRUM_SMOOTHING, SpectrumState};
use crate::theme::Tokens;

/// Displayed frequency range.
const F_MIN: f32 = 20.0;
const F_MAX: f32 = 20_000.0;
/// FFT window length (frequency resolution = rate / FFT_SIZE).
const FFT_SIZE: usize = 4096;
/// Points in the drawn curve (one per ~2 px at a wide panel).
const CURVE_POINTS: usize = 720;
/// Frequencies carrying a grid line.
const FREQ_LINES: [f32; 22] =
    [20.0, 30.0, 40.0, 50.0, 60.0, 80.0, 100.0, 200.0, 300.0, 400.0, 500.0, 600.0, 800.0, 1000.0, 2000.0, 3000.0, 4000.0, 5000.0, 6000.0, 8000.0, 10000.0, 20000.0];

/// Log-frequency position of `f` inside `r`.
fn x_of(f: f32, r: Rect) -> f32 {
    if r.width() <= 0.0 {
        return r.min.x;
    }
    r.min.x + ((f.max(F_MIN) / F_MIN).ln() / (F_MAX / F_MIN).ln()) * r.width()
}
/// Frequency at the horizontal position `x`.
fn f_of(x: f32, r: Rect) -> f32 {
    if r.width() <= 0.0 {
        return F_MIN;
    }
    F_MIN * (F_MAX / F_MIN).powf(((x - r.min.x) / r.width()).clamp(0.0, 1.0))
}
/// Vertical position of a dB value (top = `max_db`, bottom = `min_db`).
fn y_of(db: f32, r: Rect, min_db: f32, max_db: f32) -> f32 {
    let span = (max_db - min_db).max(1.0);
    let u = ((db - min_db) / span).clamp(0.0, 1.0);
    r.max.y - u * r.height()
}

fn freq_label(f: f32) -> String {
    if f >= 1000.0 {
        let k = f / 1000.0;
        if (k - k.round()).abs() < 0.01 { format!("{}k", k.round() as i32) } else { format!("{k:.1}k") }
    } else {
        format!("{}", f.round() as i32)
    }
}

/// The panel.
pub fn show(app: &mut FilmcraftApp, ui: &mut egui::Ui, rect: Rect) {
    let t = app.tokens;
    ui.painter().rect_filled(rect, 0.0, t.plot_bg);
    app.auto.add("audioSpectrum", rect, "Audio Spectrum");
    if rect.width() < 80.0 || rect.height() < 50.0 {
        return;
    }
    let st = app.ui.panels.spectrum.clone();
    let header_h = if rect.height() > 150.0 && rect.width() > 380.0 { 26.0 } else { 0.0 };
    let axis_w = if rect.width() > 220.0 { 36.0 } else { 0.0 };
    let axis_h = if rect.height() > 110.0 { 16.0 } else { 0.0 };
    let plot = Rect::from_min_max(pos2(rect.min.x + axis_w, rect.min.y + header_h), pos2(rect.max.x - 2.0, rect.max.y - axis_h));
    if plot.width() < 10.0 || plot.height() < 10.0 {
        return;
    }
    draw_grid(ui, plot, &t, st.min_db, axis_w, axis_h);
    draw_spectrum(app, ui, plot, &st);
    if header_h > 0.0 {
        controls(app, ui, Rect::from_min_size(rect.min, vec2(rect.width(), header_h)));
    }
}

/// Log-frequency vertical lines with labels and dB horizontal lines with labels.
fn draw_grid(ui: &egui::Ui, plot: Rect, t: &Tokens, min_db: f32, axis_w: f32, axis_h: f32) {
    let p = ui.painter();
    // dB lines every 6 dB from 0 down to the floor.
    let mut db = 0.0;
    while db >= min_db - 0.5 {
        let y = y_of(db, plot, min_db, 0.0);
        let col = if db.abs() < 0.1 { t.plot_axis } else { t.plot_grid };
        p.line_segment([pos2(plot.min.x, y), pos2(plot.max.x, y)], Stroke::new(1.0, col));
        if axis_w > 0.0 {
            p.text(pos2(plot.min.x - 5.0, y), Align2::RIGHT_CENTER, format!("{}", db.round() as i32), Tokens::ui(9.0), t.text_faint);
        }
        db -= 6.0;
    }
    // Log-frequency lines; labels only where they do not collide.
    let mut last_label = f32::NEG_INFINITY;
    for f in FREQ_LINES {
        let x = x_of(f, plot);
        p.line_segment([pos2(x, plot.min.y), pos2(x, plot.max.y)], Stroke::new(1.0, t.plot_grid));
        if axis_h > 0.0 && x - last_label > 24.0 {
            p.text(pos2(x, plot.max.y + 3.0), Align2::CENTER_TOP, freq_label(f), Tokens::ui(9.0), t.text_faint);
            last_label = x;
        }
    }
}

/// Analyze the newest master window and draw the filled curve + peak line + hover readout.
fn draw_spectrum(app: &mut FilmcraftApp, ui: &egui::Ui, plot: Rect, st: &SpectrumState) {
    let t = app.tokens;
    let sr = app.session.active_sequence().map(|q| q.settings.sample_rate as f32).unwrap_or(48_000.0);
    if app.spectrum.as_ref().is_none_or(|a| a.fft_size() != FFT_SIZE || a.sample_rate() != sr) {
        app.spectrum = Some(SpectrumAnalyzer::new(FFT_SIZE, sr));
    }
    let cfg = SpectrumConfig {
        slope_db_per_oct: st.slope,
        decay_db_per_s: st.decay,
        smooth_octaves: st.smooth_octaves,
        peak_decay_db_per_s: st.peak_decay,
        min_db: st.min_db,
        max_db: 0.0,
    };
    // Newest master window while playing; silence (so the display falls) when stopped.
    let mut samples = vec![0.0f32; FFT_SIZE];
    if app.playback.playing {
        let _ = app.session.previews.live.master_snapshot(&mut samples);
    }
    let dt = ui.input(|i| i.stable_dt).clamp(1.0 / 240.0, 0.1);
    let Some(a) = app.spectrum.as_mut() else { return };
    a.process(&samples, dt, &cfg);

    let point = |i: usize, db: f32| {
        let u = i as f32 / CURVE_POINTS as f32;
        let x = plot.min.x + u * plot.width();
        pos2(x, y_of(db, plot, cfg.min_db, cfg.max_db))
    };
    let curve: Vec<Pos2> = (0..=CURVE_POINTS).map(|i| point(i, a.db_at(f_at(i)))).collect();

    // Fill: solid accent at the curve fading to transparent at the bottom.
    let top = Color32::from_rgba_unmultiplied(t.accent.r(), t.accent.g(), t.accent.b(), 120);
    let bottom = Color32::from_rgba_unmultiplied(t.accent.r(), t.accent.g(), t.accent.b(), 0);
    let mut mesh = Mesh::default();
    for q in &curve {
        mesh.colored_vertex(*q, top);
        mesh.colored_vertex(pos2(q.x, plot.max.y), bottom);
    }
    for i in 0..CURVE_POINTS {
        let v = (2 * i) as u32;
        mesh.add_triangle(v, v + 1, v + 2);
        mesh.add_triangle(v + 1, v + 3, v + 2);
    }
    ui.painter().add(Shape::mesh(mesh));
    ui.painter().add(Shape::line(curve, Stroke::new(1.4, t.accent)));

    // Peak-hold line: very thin, slowly falling.
    if st.peak_hold {
        let peak: Vec<Pos2> = (0..=CURVE_POINTS).map(|i| point(i, a.peak_at(f_at(i)))).collect();
        ui.painter().add(Shape::line(peak, Stroke::new(1.0, Color32::from_rgba_unmultiplied(t.text.r(), t.text.g(), t.text.b(), 160))));
    }

    // Hover: a crosshair and a (frequency, level) readout; a click resets the held peak.
    let resp = ui.interact(plot, egui::Id::unique("audioSpectrum.plot"), Sense::click());
    app.auto.add_response("audioSpectrum.plot", &resp, "Spectrum");
    if let Some(p) = resp.hover_pos().filter(|p| plot.contains(*p)) {
        ui.painter().line_segment([pos2(p.x, plot.min.y), pos2(p.x, plot.max.y)], Stroke::new(1.0, t.text_faint));
        let f = f_of(p.x, plot);
        ui.painter().text(
            pos2(plot.min.x + 5.0, plot.min.y + 2.0),
            Align2::LEFT_TOP,
            format!("{}   {:.1} dB", freq_label(f), a.db_at(f)),
            Tokens::mono(10.5),
            t.text,
        );
    }
    if resp.clicked() {
        a.reset_peak();
    }

    let falling = a.bins().iter().any(|v| *v > cfg.min_db + 0.5);
    let peak_up = st.peak_hold && a.peak_bins().iter().any(|v| *v > cfg.min_db + 0.5);
    if app.playback.playing || falling || peak_up {
        ui.ctx().request_repaint();
    }
}

/// Frequency of curve point `i` (log-spaced across the display range).
fn f_at(i: usize) -> f32 {
    F_MIN * (F_MAX / F_MIN).powf(i as f32 / CURVE_POINTS as f32)
}

/// The toolbar: Slope, smoothing, Decay, Peak hold and Reset.
fn controls(app: &mut FilmcraftApp, ui: &mut egui::Ui, rect: Rect) {
    let mut hdr = ui.new_child(egui::UiBuilder::new().max_rect(rect).layout(egui::Layout::left_to_right(egui::Align::Center)));
    hdr.spacing_mut().item_spacing.x = 8.0;
    hdr.label(tl!("Spectrum"));
    hdr.label(tl!("Slope"));
    let r = hdr.add(egui::DragValue::new(&mut app.ui.panels.spectrum.slope).range(0.0..=6.0).speed(0.1).max_decimals(1).suffix(" dB/oct"));
    app.auto.add_response("audioSpectrum.slope", &r, "Slope");

    let current = app.ui.panels.spectrum.smooth_octaves;
    let cur = SPECTRUM_SMOOTHING.iter().position(|(_, v)| (*v - current).abs() < 1e-6).unwrap_or(1);
    let mut pick = None;
    let combo = craft_ui::ComboBox::from_id_salt("spectrum-smoothing").selected_text(crate::i18n::t(SPECTRUM_SMOOTHING[cur].0)).width(84.0).show_ui(&mut hdr, |ui| {
        for (i, (label, _)) in SPECTRUM_SMOOTHING.iter().enumerate() {
            if ui.selectable_label(i == cur, crate::i18n::t(label)).clicked() {
                pick = Some(i);
            }
        }
    });
    app.auto.add_response("audioSpectrum.smoothing", &combo.response, "Smoothing");
    if let Some(i) = pick {
        app.ui.panels.spectrum.smooth_octaves = SPECTRUM_SMOOTHING[i].1;
    }

    hdr.label(tl!("Decay"));
    let r = hdr.add(egui::DragValue::new(&mut app.ui.panels.spectrum.decay).range(0.0..=240.0).speed(1.0).max_decimals(0).suffix(" dB/s"));
    app.auto.add_response("audioSpectrum.decay", &r, "Decay");

    let r = hdr.checkbox(&mut app.ui.panels.spectrum.peak_hold, tl!("Peak"));
    app.auto.add_response("audioSpectrum.peakHold", &r, "Peak");
    if hdr.button(tl!("Reset")).clicked()
        && let Some(a) = app.spectrum.as_mut()
    {
        a.reset_peak();
    }
}
