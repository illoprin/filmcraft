//! Text styles (`textStyle.*`): reusable text appearance, held as project assets.
//!
//! A style is an ordinary project item ([`ItemKind::TextStyle`]); a text layer keeps a link to the
//! style it follows in its [`LayerExtra::style`](filmcraft_project::LayerExtra::style), but its
//! parameters stay its own. `textStyle.create` reads a layer's look into a new style, `apply`
//! writes a style onto the text layers of one or more graphic clips, `push` copies a layer's
//! current look back into its style and rewrites every linked layer, and `unlink` drops the link.
//! `textStyle.export` / `import` move a style to and from a small `.fstyle` JSON file.
//!
//! The style itself is [`filmcraft_project::text_style::TextStyle`]; this module only drives the
//! project through the usual undoable [`Session::edit`].

use std::collections::BTreeMap;

use filmcraft_project::graphic::{TEXT_LAYER, layer_indices};
use filmcraft_project::text_style::TextStyle;
use filmcraft_project::{ClipId, ItemId, ItemKind, Label, Project, TrackItem};
use filmcraft_time::Tick;
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};

use crate::commands::{CommandSpec, always, bad, str_p, target_clips_p, u64_p};
use crate::graphics::{layer_effect_index, target_clip};
use crate::{EngineError, Result, Session};

type Run = fn(&mut Session, &Value) -> Result<Value>;
type Enabled = fn(&Session) -> std::result::Result<(), String>;

fn spec(id: &'static str, label: &'static str, journal: bool, params: &'static str, enabled: Enabled, run: Run) -> CommandSpec {
    CommandSpec { id, label, menu: &[], shortcut: None, params, enabled, run, journal, named: crate::commands::NamedKeys::NONE }
}

/// The file format marker of a `.fstyle` file.
const STYLE_FORMAT: &str = "filmcraft.textstyle";
/// The `.fstyle` file version this build writes and understands.
const STYLE_VERSION: u32 = 1;

/// Whether the project has any text styles (for `enabled`).
fn has_style(s: &Session) -> std::result::Result<(), String> {
    if s.project.items.values().any(|i| matches!(i.kind, ItemKind::TextStyle(_))) { Ok(()) } else { Err("the project has no text styles".into()) }
}

/// The graphic clip a style command works from, requiring a text layer.
fn text_target(s: &Session, p: &Value) -> Result<(ClipId, usize, usize, Tick)> {
    let cmd = "textStyle";
    let seq = s.active_sequence().ok_or(EngineError::NoSequence)?;
    let clip = target_clip(s, p).ok_or_else(|| bad(cmd, "select a graphic clip with a text layer"))?;
    let (l, ei) = layer_effect_index(s, clip, p)?;
    let (_, it) = seq.find_item(clip).ok_or_else(|| bad(cmd, "no such clip"))?;
    if it.effects.get(ei).map(|e| e.effect.as_str()) != Some(TEXT_LAYER) {
        return Err(bad(cmd, "the selected layer is not a text layer"));
    }
    Ok((clip, l, ei, span_time(it, s.playhead())))
}

/// The media time a layer's fixed values are read at: the playhead, clamped into the clip.
fn span_time(it: &TrackItem, playhead: Tick) -> Tick {
    it.effect_time_at(playhead.clamp(it.start, it.end() - Tick(1)))
}

/// A name like `Text Style 3`, free in `pr`.
fn default_name(pr: &Project) -> String {
    let used: std::collections::HashSet<&str> = pr.items.values().map(|i| i.name.as_str()).collect();
    let mut n = pr.items.values().filter(|i| matches!(i.kind, ItemKind::TextStyle(_))).count() + 1;
    loop {
        let name = format!("Text Style {n}");
        if !used.contains(name.as_str()) {
            return name;
        }
        n += 1;
    }
}

/// The text layers of a graphic clip (effect indices), in paint order.
fn text_effects(it: &TrackItem) -> Vec<usize> {
    layer_indices(&it.effects).into_iter().filter(|&ei| it.effects[ei].effect == TEXT_LAYER).collect()
}

/// Apply `style` to every text layer of `clip`; returns how many changed.
fn apply_to_clip(q: &mut filmcraft_project::Sequence, clip: ClipId, item: ItemId, style: &TextStyle) -> usize {
    let Some((_, it)) = q.find_item_mut(clip) else { return 0 };
    let mut n = 0;
    for ei in text_effects(it) {
        style.apply_to(&mut it.effects[ei]);
        it.effects[ei].layer.get_or_insert_with(Default::default).style = Some(item);
        n += 1;
    }
    n
}

/// `textStyle.create`: read the target text layer's look into a new style asset and link the layer.
fn create(s: &mut Session, p: &Value) -> Result<Value> {
    const ID: &str = "textStyle.create";
    let seq_id = s.state.active_sequence.ok_or(EngineError::NoSequence)?;
    let (clip, l, ei, mt) = text_target(s, p)?;
    let style = {
        let seq = s.active_sequence().ok_or(EngineError::NoSequence)?;
        let (_, it) = seq.find_item(clip).ok_or_else(|| bad(ID, "no such clip"))?;
        TextStyle::from_layer(it.effects.get(ei).ok_or_else(|| bad(ID, "no such layer"))?, mt)
    };
    let name = str_p(p, "name").map(str::to_string).unwrap_or_else(|| default_name(&s.project));
    let bin = u64_p(p, "bin").map(filmcraft_project::BinId);
    let item_name = name.clone();
    let item = s.edit("Create Text Style", move |pr, _| {
        let item = pr.add_item(&item_name, Label::Purple, ItemKind::TextStyle(Box::new(style)), bin);
        let q = pr.sequence_mut(seq_id).ok_or(EngineError::NoSequence)?;
        let (_, it) = q.find_item_mut(clip).ok_or_else(|| bad(ID, "no such clip"))?;
        let target = *layer_indices(&it.effects).get(l).ok_or_else(|| bad(ID, "no such layer"))?;
        it.effects[target].layer.get_or_insert_with(Default::default).style = Some(item);
        Ok(item)
    })?;
    Ok(json!({"item": item.0, "name": name, "clip": clip.0, "layer": l}))
}

/// `textStyle.apply`: write a style onto every text layer of the target clip(s).
fn apply(s: &mut Session, p: &Value) -> Result<Value> {
    const ID: &str = "textStyle.apply";
    let item = ItemId(u64_p(p, "style").ok_or_else(|| bad(ID, "need `style`"))?);
    let style = style_of(&s.project, item).ok_or_else(|| bad(ID, "no such text style"))?;
    let seq_id = s.state.active_sequence.ok_or(EngineError::NoSequence)?;
    let clips = match target_clips_p(s, p, ID)? {
        Some(clips) => clips,
        None => s.state.selection.clone(),
    };
    if clips.is_empty() {
        return Err(bad(ID, "select one or more graphic clips"));
    }
    let changed = s.edit("Apply Text Style", move |pr, _| {
        let q = pr.sequence_mut(seq_id).ok_or(EngineError::NoSequence)?;
        Ok(clips.iter().map(|c| apply_to_clip(q, *c, item, &style)).sum::<usize>())
    })?;
    if changed == 0 {
        return Err(bad(ID, "the selection has no graphic text layers"));
    }
    Ok(json!({"item": item.0, "layers": changed}))
}

/// `textStyle.push`: copy a linked layer's current look back into its style and rewrite every
/// other layer linked to it.
fn push(s: &mut Session, p: &Value) -> Result<Value> {
    const ID: &str = "textStyle.push";
    let (clip, _, ei, mt) = text_target(s, p)?;
    let item = {
        let seq = s.active_sequence().ok_or(EngineError::NoSequence)?;
        let (_, it) = seq.find_item(clip).ok_or_else(|| bad(ID, "no such clip"))?;
        it.effects.get(ei).and_then(|e| e.layer.as_ref()).and_then(|x| x.style).ok_or_else(|| bad(ID, "the layer is not linked to a style"))?
    };
    let style = {
        let seq = s.active_sequence().ok_or(EngineError::NoSequence)?;
        let (_, it) = seq.find_item(clip).ok_or_else(|| bad(ID, "no such clip"))?;
        TextStyle::from_layer(it.effects.get(ei).ok_or_else(|| bad(ID, "no such layer"))?, mt)
    };
    let n = s.edit("Update Text Style", move |pr, _| {
        if let Some(pi) = pr.item_mut(item) {
            pi.kind = ItemKind::TextStyle(Box::new(style.clone()));
        }
        Ok(relink_all(pr, item, &style))
    })?;
    Ok(json!({"item": item.0, "layers": n}))
}

/// `textStyle.unlink`: drop a layer's link, keeping its current look.
fn unlink(s: &mut Session, p: &Value) -> Result<Value> {
    const ID: &str = "textStyle.unlink";
    let (clip, l, _, _) = text_target(s, p)?;
    s.edit_sequence("Unlink Text Style", |q, _, _| {
        let (_, it) = q.find_item_mut(clip).ok_or_else(|| bad(ID, "no such clip"))?;
        let target = *layer_indices(&it.effects).get(l).ok_or_else(|| bad(ID, "no such layer"))?;
        if let Some(x) = it.effects[target].layer.as_mut() {
            x.style = None;
        }
        Ok(())
    })?;
    Ok(json!({"clip": clip.0, "layer": l}))
}

/// `textStyle.rename`: rename a style asset (the item name; the layer link is by id).
fn rename(s: &mut Session, p: &Value) -> Result<Value> {
    const ID: &str = "textStyle.rename";
    let item = ItemId(u64_p(p, "style").ok_or_else(|| bad(ID, "need `style`"))?);
    let name = str_p(p, "name").map(str::to_string).ok_or_else(|| bad(ID, "need `name`"))?;
    let name = if name.trim().is_empty() { default_name(&s.project) } else { name };
    if style_of(&s.project, item).is_none() {
        return Err(bad(ID, "no such text style"));
    }
    s.edit("Rename Text Style", move |pr, _| {
        if let Some(pi) = pr.item_mut(item) {
            pi.name = name.clone();
        }
        Ok(())
    })?;
    Ok(json!({"item": item.0}))
}

/// Write `style` onto every layer of `pr` linked to `item`; returns how many changed.
fn relink_all(pr: &mut Project, item: ItemId, style: &TextStyle) -> usize {
    let mut n = 0;
    for pi in pr.items.values_mut() {
        let ItemKind::Sequence(seq) = &mut pi.kind else { continue };
        for tr in std::sync::Arc::make_mut(seq).all_tracks_mut() {
            for it in tr.items.iter_mut() {
                for ei in layer_indices(&it.effects) {
                    if it.effects[ei].layer.as_ref().and_then(|x| x.style) == Some(item) {
                        style.apply_to(&mut it.effects[ei]);
                        n += 1;
                    }
                }
            }
        }
    }
    for sg in pr.source_graphics.values_mut() {
        for ei in layer_indices(&sg.layers) {
            if sg.layers[ei].layer.as_ref().and_then(|x| x.style) == Some(item) {
                style.apply_to(&mut sg.layers[ei]);
                n += 1;
            }
        }
    }
    n
}

/// The style asset's value, if `item` is one.
fn style_of(p: &Project, item: ItemId) -> Option<TextStyle> {
    match &p.item(item)?.kind {
        ItemKind::TextStyle(s) => Some((**s).clone()),
        _ => None,
    }
}

// ------------------------------------------------------------------------------------- .fstyle

/// A `.fstyle` file: a name and the style's properties as plain JSON.
#[derive(Clone, Debug, Serialize, Deserialize)]
struct StyleFile {
    format: String,
    version: u32,
    #[serde(default)]
    name: String,
    #[serde(default)]
    params: BTreeMap<String, Value>,
}

/// The text layer's parameter definitions, used to decode the file's values by type.
fn text_layer_template(id: &str) -> Option<filmcraft_project::ParamValue> {
    filmcraft_project::find_effect(TEXT_LAYER)?.param(id).map(|d| d.default.clone())
}

fn style_to_file(style: &TextStyle, name: &str) -> StyleFile {
    StyleFile {
        format: STYLE_FORMAT.into(),
        version: STYLE_VERSION,
        name: name.to_string(),
        params: style.params.iter().map(|(k, v)| (k.clone(), crate::commands::param_value_to_json(v))).collect(),
    }
}

fn style_from_file(file: &StyleFile) -> Result<TextStyle> {
    let cmd = "textStyle.import";
    if file.format != STYLE_FORMAT {
        return Err(bad(cmd, format!("not a text style file (format `{}`)", file.format)));
    }
    if file.version > STYLE_VERSION {
        return Err(bad(cmd, format!("text style version {} is newer than this build supports ({STYLE_VERSION})", file.version)));
    }
    let mut params = BTreeMap::new();
    for (k, v) in &file.params {
        let Some(template) = text_layer_template(k) else { continue };
        if let Some(pv) = crate::graphics::to_param(&template, k, v) {
            params.insert(k.clone(), pv);
        }
    }
    Ok(TextStyle { params })
}

/// `textStyle.export`: write a style to a `.fstyle` file.
fn export(s: &mut Session, p: &Value) -> Result<Value> {
    const ID: &str = "textStyle.export";
    let item = ItemId(u64_p(p, "style").ok_or_else(|| bad(ID, "need `style`"))?);
    let path = str_p(p, "path").ok_or_else(|| bad(ID, "need `path`"))?.to_string();
    let pi = s.project.item(item).ok_or_else(|| bad(ID, "no such item"))?;
    if !matches!(pi.kind, ItemKind::TextStyle(_)) {
        return Err(bad(ID, "the item is not a text style"));
    }
    let name = str_p(p, "name").map(str::to_string).unwrap_or_else(|| pi.name.clone());
    let style = style_of(&s.project, item).ok_or_else(|| bad(ID, "no such text style"))?;
    let file = style_to_file(&style, &name);
    let bytes = serde_json::to_vec_pretty(&file).map_err(|e| bad(ID, e.to_string()))?;
    s.services.write_file(&path, &bytes).map_err(|e| EngineError::Other(format!("{path}: {e}")))?;
    Ok(json!({"path": path, "bytes": bytes.len(), "item": item.0}))
}

/// `textStyle.import`: read a `.fstyle` file into a new style asset.
fn import(s: &mut Session, p: &Value) -> Result<Value> {
    const ID: &str = "textStyle.import";
    let path = str_p(p, "path").ok_or_else(|| bad(ID, "need `path`"))?.to_string();
    let bytes = s.services.read_file(&path).map_err(|e| EngineError::Other(format!("{path}: {e}")))?;
    let file: StyleFile = serde_json::from_slice(&bytes).map_err(|e| bad(ID, format!("{path}: {e}")))?;
    let style = style_from_file(&file)?;
    let name = if file.name.trim().is_empty() { default_name(&s.project) } else { file.name.clone() };
    let bin = u64_p(p, "bin").map(filmcraft_project::BinId);
    let item_name = name.clone();
    let item = s.edit("Import Text Style", move |pr, _| {
        let id = pr.add_item(&item_name, Label::Purple, ItemKind::TextStyle(Box::new(style)), bin);
        Ok(id)
    })?;
    Ok(json!({"item": item.0, "name": name, "path": path}))
}

pub fn commands() -> Vec<CommandSpec> {
    vec![
        spec(
            "textStyle.create",
            "Create Text Style",
            true,
            r#"{"clip":id?,"layer":n|name?,"name":str?,"bin":binId?}"#,
            |s| {
                crate::commands::has_seq(s)?;
                text_target(s, &Value::Null).map(|_| ()).map_err(|e| e.to_string())
            },
            create,
        ),
        spec(
            "textStyle.apply",
            "Apply Text Style",
            true,
            r#"{"style":id,"clip":id?,"clips":[id]? (default: the selected clips)"}"#,
            |s| {
                crate::commands::has_seq(s)?;
                has_style(s)
            },
            apply,
        ),
        spec(
            "textStyle.push",
            "Update Text Style",
            true,
            r#"{"clip":id?,"layer":n|name?"}"#,
            |s| {
                crate::commands::has_seq(s)?;
                text_target(s, &Value::Null).map(|_| ()).map_err(|e| e.to_string())
            },
            push,
        ),
        spec(
            "textStyle.unlink",
            "Unlink Text Style",
            true,
            r#"{"clip":id?,"layer":n|name?"}"#,
            |s| {
                crate::commands::has_seq(s)?;
                text_target(s, &Value::Null).map(|_| ()).map_err(|e| e.to_string())
            },
            unlink,
        ),
        spec("textStyle.rename", "Rename Text Style", true, r#"{"style":id,"name":str}"#, has_style, rename),
        spec("textStyle.export", "Export Text Style", false, r#"{"style":id,"path":str,"name":str?}"#, has_style, export),
        spec("textStyle.import", "Import Text Style", true, r#"{"path":str,"bin":binId?}"#, always, import),
    ]
}

#[cfg(test)]
mod tests {
    use super::*;
    use filmcraft_project::ParamValue;
    use filmcraft_project::graphic::layer_indices;

    fn demo() -> Session {
        let mut s = Session::default();
        s.execute("file.openDemoProject", json!({})).unwrap();
        s
    }

    fn layer(s: &Session, clip: ClipId, l: usize) -> filmcraft_project::EffectInstance {
        let q = s.active_sequence().unwrap();
        let (_, it) = q.find_item(clip).unwrap();
        let idx = layer_indices(&it.effects);
        it.effects[idx[l]].clone()
    }

    fn param(s: &Session, clip: ClipId, l: usize, id: &str) -> ParamValue {
        layer(s, clip, l).params[id].value.clone()
    }

    fn linked(s: &Session, clip: ClipId, l: usize) -> Option<ItemId> {
        layer(s, clip, l).layer.as_ref().and_then(|x| x.style)
    }

    fn style_params(s: &Session, item: ItemId) -> TextStyle {
        style_of(&s.project, item).unwrap()
    }

    #[test]
    fn create_reads_the_layer_and_links_it() {
        let mut s = demo();
        let clip = ClipId(s.execute("graphics.newText", json!({"text": "Hi", "size": 100})).unwrap()["clip"].as_u64().unwrap());
        s.execute("graphics.set", json!({"clip": clip.0, "props": {"font": "Georgia", "size": 144.0, "fill_color": "#ff0000"}})).unwrap();
        let r = s.execute("textStyle.create", json!({"clip": clip.0, "name": "Headline"})).unwrap();
        let item = ItemId(r["item"].as_u64().unwrap());
        assert_eq!(r["name"], "Headline");
        let pi = s.project.item(item).unwrap();
        assert_eq!(pi.name, "Headline");
        assert!(matches!(pi.kind, ItemKind::TextStyle(_)));
        let sp = style_params(&s, item);
        assert_eq!(sp.params["font"], ParamValue::Text("Georgia".into()));
        assert_eq!(sp.params["size"], ParamValue::Float(144.0));
        assert!(sp.params.contains_key("fill_color"));
        assert!(!sp.params.contains_key("text"), "the content is never part of a style");
        assert_eq!(linked(&s, clip, 0), Some(item), "the layer is linked to the new style");
        // undo removes both the item and the link
        s.execute("edit.undo", json!({})).unwrap();
        assert!(s.project.item(item).is_none());
        assert_eq!(linked(&s, clip, 0), None);
    }

    #[test]
    fn apply_writes_the_look_onto_other_text_layers() {
        let mut s = demo();
        let a = ClipId(s.execute("graphics.newText", json!({"text": "A", "size": 100})).unwrap()["clip"].as_u64().unwrap());
        let b = ClipId(s.execute("graphics.newText", json!({"text": "B", "size": 40, "position": [500, 500]})).unwrap()["clip"].as_u64().unwrap());
        s.execute("graphics.set", json!({"clip": a.0, "props": {"size": 200.0, "tracking": 30.0}})).unwrap();
        let item = ItemId(s.execute("textStyle.create", json!({"clip": a.0, "name": "Big"})).unwrap()["item"].as_u64().unwrap());

        // b keeps its content and place, but takes a's look
        s.execute("textStyle.apply", json!({"style": item.0, "clips": [b.0]})).unwrap();
        assert_eq!(param(&s, b, 0, "size"), ParamValue::Float(200.0));
        assert_eq!(param(&s, b, 0, "tracking"), ParamValue::Float(30.0));
        assert_eq!(param(&s, b, 0, "text"), ParamValue::Text("B".into()));
        assert_eq!(linked(&s, b, 0), Some(item));
        assert!(!style_params(&s, item).is_modified(&layer(&s, b, 0), Tick::ZERO));
    }

    #[test]
    fn push_rewrites_every_linked_layer() {
        let mut s = demo();
        let a = ClipId(s.execute("graphics.newText", json!({"text": "A", "size": 100})).unwrap()["clip"].as_u64().unwrap());
        let b = ClipId(s.execute("graphics.newText", json!({"text": "B", "size": 100})).unwrap()["clip"].as_u64().unwrap());
        let item = ItemId(s.execute("textStyle.create", json!({"clip": a.0})).unwrap()["item"].as_u64().unwrap());
        s.execute("textStyle.apply", json!({"style": item.0, "clips": [b.0]})).unwrap();

        // change a's size, then push: the style and b both follow
        s.execute("graphics.set", json!({"clip": a.0, "props": {"size": 260.0}})).unwrap();
        assert!(style_params(&s, item).is_modified(&layer(&s, a, 0), Tick::ZERO));
        s.execute("textStyle.push", json!({"clip": a.0})).unwrap();
        assert_eq!(style_params(&s, item).params["size"], ParamValue::Float(260.0));
        assert_eq!(param(&s, b, 0, "size"), ParamValue::Float(260.0), "the other linked layer is rewritten");
        assert!(!style_params(&s, item).is_modified(&layer(&s, a, 0), Tick::ZERO));
    }

    #[test]
    fn unlink_keeps_the_look() {
        let mut s = demo();
        let a = ClipId(s.execute("graphics.newText", json!({"text": "A", "size": 100})).unwrap()["clip"].as_u64().unwrap());
        let item = ItemId(s.execute("textStyle.create", json!({"clip": a.0})).unwrap()["item"].as_u64().unwrap());
        s.execute("textStyle.unlink", json!({"clip": a.0})).unwrap();
        assert_eq!(linked(&s, a, 0), None);
        // the style asset is still there; unlinking never deletes it
        assert!(s.project.item(item).is_some());
    }

    #[test]
    fn a_style_exports_and_imports_round_trip() {
        let mut s = demo();
        let a = ClipId(s.execute("graphics.newText", json!({"text": "A"})).unwrap()["clip"].as_u64().unwrap());
        s.execute(
            "graphics.set",
            json!({"clip": a.0, "props": {"font": "Georgia", "size": 90.0, "caps": "all caps", "stroke": true, "stroke_color": "#00ff00"}}),
        )
        .unwrap();
        let item = ItemId(s.execute("textStyle.create", json!({"clip": a.0, "name": "Brand"})).unwrap()["item"].as_u64().unwrap());
        let before = style_params(&s, item);

        let path = std::env::temp_dir().join(format!("filmcraft-style-{}-roundtrip.fstyle", std::process::id()));
        let path = path.to_string_lossy().to_string();
        s.execute("textStyle.export", json!({"style": item.0, "path": path})).unwrap();
        let text = std::fs::read_to_string(&path).unwrap();
        assert!(text.contains("filmcraft.textstyle"));
        let back = ItemId(s.execute("textStyle.import", json!({"path": path})).unwrap()["item"].as_u64().unwrap());
        let _ = std::fs::remove_file(&path);
        assert_ne!(back, item);
        assert_eq!(s.project.item(back).unwrap().name, "Brand");
        assert_eq!(style_params(&s, back), before, "the imported style matches the exported one");
    }
}
