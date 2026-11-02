//! `clip.fade`: one-sided Constant Gain fade in/out at a clip edge, the fade handle a Timeline
//! drag makes. Adding, updating, clamping and removing, and the no-op case.

use filmcraft_project::{EffectKind, TransitionAlign};
use serde_json::json;

use crate::Session;

fn demo() -> Session {
    let mut s = Session::default();
    s.execute("file.openDemoProject", json!({})).unwrap();
    s
}

#[test]
fn clip_fade_adds_updates_and_removes_one_sided_fades() {
    let mut s = demo();
    let rate = s.sequence_rate();
    let (clip, start, dur) = {
        let it = &s.active_sequence().unwrap().audio_tracks[0].items[0];
        (it.id, it.start, it.duration)
    };
    // fade in: a Constant Gain transition hugging the clip's in edge, from nothing
    let in_ticks = rate.tick_of(12);
    let r = s.execute("clip.fade", json!({"clip": clip.0, "edge": "in", "frames": 12})).unwrap();
    assert_eq!(r["fade"], in_ticks.0);
    {
        let q = s.active_sequence().unwrap();
        let tr = q.audio_tracks[0].transitions.iter().find(|x| x.to == Some(clip)).unwrap();
        assert!(tr.from.is_none(), "a fade in comes from nothing");
        assert_eq!((tr.start, tr.duration, tr.align), (start, in_ticks, TransitionAlign::StartAtCut));
        assert!(tr.effect.def().is_some_and(|d| d.kind == EffectKind::AudioTransition), "an audio transition");
    }
    // updating reuses the same transition instead of adding another
    s.execute("clip.fade", json!({"clip": clip.0, "edge": "in", "frames": 6})).unwrap();
    let q = s.active_sequence().unwrap();
    let in_fades: Vec<_> = q.audio_tracks[0].transitions.iter().filter(|x| x.to == Some(clip) && x.from.is_none()).collect();
    assert_eq!(in_fades.len(), 1, "one fade in, updated in place");
    assert_eq!(in_fades[0].duration, rate.tick_of(6));
    // fade out at the out edge
    s.execute("clip.fade", json!({"clip": clip.0, "edge": "out", "frames": 8})).unwrap();
    {
        let tr = s.active_sequence().unwrap().audio_tracks[0].transitions.iter().find(|x| x.from == Some(clip) && x.to.is_none()).unwrap();
        assert!(tr.to.is_none(), "a fade out goes to nothing");
        assert_eq!((tr.start, tr.duration, tr.align), (start + dur - rate.tick_of(8), rate.tick_of(8), TransitionAlign::EndAtCut));
    }
    // a duration longer than the clip is clamped to it
    let r = s.execute("clip.fade", json!({"clip": clip.0, "edge": "in", "duration": dur.0 * 10})).unwrap();
    assert_eq!(r["fade"].as_i64().unwrap(), dur.0, "clamped to the clip length");
    // removing the fade in leaves the fade out
    s.execute("clip.fade", json!({"clip": clip.0, "edge": "in", "frames": 0})).unwrap();
    {
        let q = s.active_sequence().unwrap();
        assert!(q.audio_tracks[0].transitions.iter().all(|x| !(x.to == Some(clip) && x.from.is_none())));
        assert!(q.audio_tracks[0].transitions.iter().any(|x| x.from == Some(clip) && x.to.is_none()));
        q.check().unwrap();
    }
    // removing a fade that is not there changes nothing and adds no undo step
    let undo = s.history.undo.len();
    s.execute("clip.fade", json!({"clip": clip.0, "edge": "in", "frames": 0})).unwrap();
    assert_eq!(s.history.undo.len(), undo, "a no-op fade is not an edit");
    // hostile parameters
    assert!(s.execute("clip.fade", json!({"clip": clip.0, "edge": "side", "frames": 1})).is_err());
    assert!(s.execute("clip.fade", json!({"clip": 999_999, "edge": "in", "frames": 1})).is_err());
}
