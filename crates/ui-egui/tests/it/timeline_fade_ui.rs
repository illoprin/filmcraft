//! Audio fade handles in the Timeline: a small triangle at each top corner of an audio clip.
//! Hovering one shows the resize cursor; dragging it makes a one-sided Constant Gain fade on the
//! clip's waveform (the `clip.fade` command) without trimming the clip.

use std::sync::mpsc::{Sender, channel};

use egui::{CursorIcon, Pos2, pos2};
use egui_kittest::Harness;
use filmcraft_engine::Session;
use filmcraft_project::{ClipId, EffectKind};
use filmcraft_ui_egui::FilmcraftApp;
use filmcraft_ui_egui::control::ControlRequest;
use serde_json::{Value, json};

struct Driver {
    harness: Harness<'static, FilmcraftApp>,
    tx: Sender<ControlRequest>,
}

impl Driver {
    fn demo() -> Self {
        let mut session = Session::default();
        session.execute("file.openDemoProject", json!({})).expect("demo project");
        session.execute("sequence.snap", json!({"on": false})).expect("snapping off");
        let (tx, rx) = channel();
        let app = FilmcraftApp::new(session).with_control(rx);
        let harness = Harness::builder().with_size(egui::vec2(1600.0, 980.0)).with_step_dt(1.0 / 60.0).with_max_steps(10_000).build_eframe(move |_cc| app);
        let mut d = Driver { harness, tx };
        d.frames(4);
        d.ok("ui.elements", json!({"prefix": "timeline.clip."}));
        d
    }

    fn frames(&mut self, n: usize) {
        for _ in 0..n {
            let ctx = self.harness.ctx.clone();
            let mut raw = std::mem::take(self.harness.input_mut());
            eframe::App::raw_input_hook(self.harness.state_mut(), &ctx, &mut raw);
            *self.harness.input_mut() = raw;
            self.harness.step();
        }
    }

    fn call(&mut self, method: &str, params: Value) -> Value {
        let (req, reply) = ControlRequest::new(method, params.clone());
        self.tx.send(req).unwrap();
        for _ in 0..600 {
            self.frames(1);
            if let Ok(v) = reply.try_recv() {
                return v;
            }
        }
        panic!("no reply to {method} {params}");
    }

    fn ok(&mut self, method: &str, params: Value) -> Value {
        let v = self.call(method, params.clone());
        assert_eq!(v["ok"], json!(true), "{method} {params} failed: {v}");
        v["result"].clone()
    }

    fn app(&self) -> &FilmcraftApp {
        self.harness.state()
    }

    fn hover(&mut self, at: Pos2) {
        self.harness.input_mut().events.push(egui::Event::PointerMoved(at));
        self.frames(2);
    }

    fn drag(&mut self, from: Pos2, to: Pos2, steps: usize) {
        let push = |d: &mut Self, e: egui::Event| d.harness.input_mut().events.push(e);
        push(self, egui::Event::PointerMoved(from));
        self.frames(1);
        push(self, egui::Event::PointerButton { pos: from, button: egui::PointerButton::Primary, pressed: true, modifiers: Default::default() });
        self.frames(1);
        for i in 1..=steps {
            push(self, egui::Event::PointerMoved(from + (to - from) * (i as f32 / steps as f32)));
            self.frames(1);
        }
        push(self, egui::Event::PointerButton { pos: to, button: egui::PointerButton::Primary, pressed: false, modifiers: Default::default() });
        self.frames(2);
    }

    fn cursor(&self) -> CursorIcon {
        self.harness.output().platform_output.cursor_icon
    }

    fn span(&self, clip: u64) -> (i64, i64) {
        let (_, it) = self.app().session.active_sequence().expect("sequence").find_item(ClipId(clip)).expect("clip");
        (it.start.0, it.end().0)
    }

    /// Screen rect (x, y, w, h) of a clip block.
    fn clip_rect(&mut self, clip: u64) -> [f32; 4] {
        let id = format!("timeline.clip.{clip}");
        let els = self.ok("ui.elements", json!({"prefix": id}));
        let el = els.as_array().expect("elements").iter().find(|e| e["id"] == id.as_str()).unwrap_or_else(|| panic!("no {id}: {els}")).clone();
        let r = el["rect"].as_array().expect("rect");
        [r[0].as_f64().unwrap() as f32, r[1].as_f64().unwrap() as f32, r[2].as_f64().unwrap() as f32, r[3].as_f64().unwrap() as f32]
    }
}

/// The first audio clip on A1 of the demo.
fn a1_clip(d: &Driver) -> u64 {
    d.app().session.active_sequence().unwrap().audio_tracks[0].items[0].id.0
}

/// Dragging the top-left handle makes a fade in (a Constant Gain transition from nothing), and the
/// top-right handle a fade out; neither trims the clip. Hovering shows the resize cursor.
#[test]
fn dragging_a_fade_handle_fades_without_trimming() {
    let mut d = Driver::demo();
    let clip = a1_clip(&d);
    let (s0, e0) = d.span(clip);
    let r = d.clip_rect(clip);
    // the handle is at the clip's top-left corner, just inside it
    d.hover(pos2(r[0] + 3.0, r[1] + 3.0));
    assert_eq!(d.cursor(), CursorIcon::ResizeHorizontal, "fade cursor on the corner");
    let by = (r[2] / 4.0).round().max(12.0);
    d.drag(pos2(r[0] + 3.0, r[1] + 3.0), pos2(r[0] + 3.0 + by, r[1] + 3.0), 20);
    {
        let q = d.app().session.active_sequence().unwrap();
        let fin = q.audio_tracks[0].transitions.iter().find(|x| x.to == Some(ClipId(clip)) && x.from.is_none());
        let fin = fin.unwrap_or_else(|| panic!("no fade in: {:?}", q.audio_tracks[0].transitions));
        assert!(fin.effect.def().is_some_and(|d| d.kind == EffectKind::AudioTransition));
    }
    assert_eq!(d.span(clip), (s0, e0), "the fade drag did not trim the clip");
    // the handle is an automation element
    assert!(d.ok("ui.elements", json!({"prefix": format!("timeline.clip.{clip}.fade.")})).as_array().unwrap().len() >= 2, "fade handles are elements");
    // the top-right handle fades out
    let r = d.clip_rect(clip);
    d.drag(pos2(r[0] + r[2] - 3.0, r[1] + 3.0), pos2(r[0] + r[2] - 3.0 - by, r[1] + 3.0), 20);
    {
        let q = d.app().session.active_sequence().unwrap();
        assert!(
            q.audio_tracks[0].transitions.iter().any(|x| x.from == Some(ClipId(clip)) && x.to.is_none()),
            "no fade out: {:?}",
            q.audio_tracks[0].transitions
        );
    }
    assert_eq!(d.span(clip), (s0, e0), "the fade out drag did not trim the clip");
}
