//! Reusable text styles: a saved appearance for text graphics layers, held as a project asset.
//!
//! A style owns the *look* of text — font, size, spacing, alignment, fill, strokes, background and
//! shadow — and never its content, layout box or transform. It is an ordinary project item
//! ([`ItemKind::TextStyle`](crate::ItemKind::TextStyle)), so it lives in the Project panel, moves
//! between bins, saves with the project and copies between projects as a small `.fstyle` file.
//!
//! A layer keeps a link to its style ([`LayerExtra::style`](crate::graphic_design::LayerExtra::style))
//! but its parameters stay its own: applying a style writes the stored values onto the layer, and
//! pushing a later edit back to the style ([`TextStyle::from_layer`]) rewrites every linked layer.

use std::collections::BTreeMap;

use filmcraft_time::Tick;
use serde::{Deserialize, Serialize};

use crate::effect::EffectInstance;
use crate::keyframe::{Param, ParamValue};

/// The layer parameters a style owns, in a stable order. Anything else (name, source text, box,
/// vertical text, position, anchor, scale, rotation, opacity) is not part of a style.
pub const STYLE_PARAMS: &[&str] = &[
    // Text styling
    "font",
    "font_style",
    "size",
    "align",
    "tracking",
    "kerning",
    "ligatures",
    "leading",
    "baseline_shift",
    "faux_bold",
    "faux_italic",
    "caps",
    "underline",
    "strikethrough",
    // Appearance
    "fill",
    "fill_color",
    "fill_kind",
    "gradient_start",
    "gradient_end",
    "gradient_angle",
    "stroke",
    "stroke_color",
    "stroke_width",
    "stroke_type",
    "stroke2",
    "stroke2_color",
    "stroke2_width",
    "stroke2_type",
    "background",
    "background_color",
    "background_opacity",
    "background_size",
    "background_radius",
    "shadow",
    "shadow_color",
    "shadow_opacity",
    "shadow_angle",
    "shadow_distance",
    "shadow_size",
    "shadow_blur",
];

/// The saved appearance of a text layer, as a reusable project asset.
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct TextStyle {
    /// The style's parameters, keyed by the text layer's parameter id. Only [`STYLE_PARAMS`].
    pub params: BTreeMap<String, ParamValue>,
}

impl TextStyle {
    /// The style of a text layer at time `t`, taking each parameter's value there (animated
    /// properties resolve at `t`).
    pub fn from_layer(e: &EffectInstance, t: Tick) -> TextStyle {
        let mut params = BTreeMap::new();
        for id in STYLE_PARAMS {
            if let Some(p) = e.params.get(*id) {
                params.insert((*id).to_string(), p.value_at(t));
            }
        }
        TextStyle { params }
    }

    pub fn is_empty(&self) -> bool {
        self.params.is_empty()
    }

    /// Write every stored parameter onto `e` as a fixed value, dropping any keyframes they had (the
    /// style is the whole of these properties).
    pub fn apply_to(&self, e: &mut EffectInstance) {
        for (id, v) in &self.params {
            e.params.insert(id.clone(), Param::new(v.clone()));
        }
    }

    /// Whether the layer's look has drifted from the style: a parameter moved, or one of them
    /// animates (a style holds fixed values only). A parameter the layer no longer has also counts.
    pub fn is_modified(&self, e: &EffectInstance, t: Tick) -> bool {
        self.params.iter().any(|(id, v)| match e.params.get(id) {
            Some(p) => p.is_animated() || p.value_at(t) != *v,
            None => true,
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::graphic::TEXT_LAYER;

    fn text_layer() -> EffectInstance {
        crate::effect::find_effect(TEXT_LAYER).unwrap().instance()
    }

    #[test]
    fn a_style_round_trips_and_covers_only_the_look() {
        let mut e = text_layer();
        e.params.get_mut("font").unwrap().value = ParamValue::Text("Georgia".into());
        e.params.get_mut("size").unwrap().value = ParamValue::Float(144.0);
        e.params.get_mut("fill_color").unwrap().value = ParamValue::Color([1.0, 0.0, 0.0, 1.0]);
        e.params.get_mut("text").unwrap().value = ParamValue::Text("Hi".into());
        e.params.get_mut("position").unwrap().value = ParamValue::Vec2(filmcraft_geom::Vec2::new(10.0, 20.0));
        let s = TextStyle::from_layer(&e, Tick::ZERO);
        assert_eq!(s.params.get("font"), Some(&ParamValue::Text("Georgia".into())));
        assert_eq!(s.params.get("size"), Some(&ParamValue::Float(144.0)));
        assert!(!s.params.contains_key("text"), "the content is not part of a style");
        assert!(!s.params.contains_key("position"), "the transform is not part of a style");
        let back: TextStyle = serde_json::from_str(&serde_json::to_string(&s).unwrap()).unwrap();
        assert_eq!(back, s);
    }

    #[test]
    fn applying_a_style_writes_the_look_and_clears_keyframes() {
        let mut src = text_layer();
        src.params.get_mut("size").unwrap().value = ParamValue::Float(200.0);
        src.params.get_mut("stroke").unwrap().value = ParamValue::Bool(true);
        let style = TextStyle::from_layer(&src, Tick::ZERO);

        let mut dst = text_layer();
        dst.params.get_mut("size").unwrap().put_keyframe(Tick(5), ParamValue::Float(50.0));
        dst.params.get_mut("position").unwrap().value = ParamValue::Vec2(filmcraft_geom::Vec2::new(1.0, 2.0));
        assert!(style.is_modified(&dst, Tick::ZERO));
        style.apply_to(&mut dst);
        assert!(!style.is_modified(&dst, Tick::ZERO));
        assert_eq!(dst.params["size"].value, ParamValue::Float(200.0));
        assert!(!dst.params["size"].is_animated(), "the style replaces the value, keyframes and all");
        assert_eq!(dst.params["stroke"].value, ParamValue::Bool(true));
        // a property the style does not own is left alone
        assert_eq!(dst.params["position"].value, ParamValue::Vec2(filmcraft_geom::Vec2::new(1.0, 2.0)));
    }
}
