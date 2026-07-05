//! `RsdmSymbol` — a distinct symbol shown for each integer value of a channel.
//!
//! Ports the core of `pydm/widgets/symbol.py` (`PyDMSymbol`): the widget holds a
//! map keyed on integer channel values, and for the current value it draws the
//! matching symbol. PyDM's lookup is an exact dictionary hit
//! (`_state_images.get(_current_key)`) — a value with no configured state, or no
//! value at all, draws nothing. That selection rule is the pure, unit-tested core
//! ([`value_as_state_key`] + [`symbol_index_for_value`]); the drawing is verified
//! by a headless wgpu readback.
//!
//! **Deviation:** PyDM maps each state to an image *file* (SVG or raster, loaded
//! through Qt with `KeepAspectRatio`/`IgnoreAspectRatio`/`KeepAspectRatioByExpanding`
//! scaling). Loading arbitrary image files needs a filesystem path resolver plus
//! SVG/raster decoders — out of scope for this dependency-light port — so each
//! state is instead a [`DrawingShape`] + fill colour, filling the widget bounds.
//! The aspect-ratio mode (an image-scaling concept) is therefore not ported:
//! vector shapes simply fill the bounds.

use rsplot::egui::{self, Color32, Vec2};

use crate::channel::{Channel, PvValue};
use crate::engine::{Engine, EngineError};
use crate::widgets::base::{ChannelBase, justified_size, layout_justify};
use crate::widgets::drawing::{DrawingShape, shape_points};

/// Default symbol size in points (PyDM `minimumSizeHint` is `10×10`; this is a
/// more usable default).
const DEFAULT_SIZE: Vec2 = Vec2::new(48.0, 48.0);

/// One configured symbol: the integer channel value it represents and how it is
/// drawn (PyDM's `imageFiles` entry, here a shape + colour rather than a file).
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct SymbolState {
    /// The exact integer channel value this symbol is shown for.
    pub key: i64,
    /// The shape drawn for this state.
    pub shape: DrawingShape,
    /// The fill colour for this state.
    pub color: Color32,
}

/// Reduce a channel value to the integer state key PyDM would look up. Mirrors
/// Python dict semantics: an integral float matches an integer key
/// (`{2: x}.get(2.0)` hits in Python), a fractional or non-finite float does
/// not, and non-numeric values (strings, arrays) have no key.
pub fn value_as_state_key(value: &PvValue) -> Option<i64> {
    match value {
        PvValue::Int(i) => Some(*i),
        PvValue::Bool(b) => Some(i64::from(*b)),
        PvValue::Enum { index, .. } => Some(i64::from(*index)),
        PvValue::Float(f) if f.is_finite() && f.fract() == 0.0 => Some(*f as i64),
        _ => None,
    }
}

/// Index into the configured states of the one whose key equals `value`, or
/// `None` when there is no value or no state matches (PyDM
/// `_state_images.get(_current_key)` returns nothing → nothing is painted). The
/// first matching key wins.
pub fn symbol_index_for_value(value: Option<i64>, keys: &[i64]) -> Option<usize> {
    let value = value?;
    keys.iter().position(|&k| k == value)
}

/// A widget that draws a different symbol for each integer value of a channel
/// (PyDM `PyDMSymbol`).
pub struct RsdmSymbol {
    base: ChannelBase,
    states: Vec<SymbolState>,
    size: Vec2,
}

impl RsdmSymbol {
    /// Connect `address` and wrap it as a symbol with no states yet (add them
    /// with [`Self::with_state`]).
    pub fn new(engine: &Engine, address: &str) -> Result<Self, EngineError> {
        Ok(Self {
            base: ChannelBase::new(engine.connect(address)?),
            states: Vec::new(),
            size: DEFAULT_SIZE,
        })
    }

    /// Add the symbol shown when the channel value equals `key` (builder style;
    /// PyDM `imageFiles` entry). Later states with the same key never win, since
    /// the first match is used.
    pub fn with_state(mut self, key: i64, shape: DrawingShape, color: Color32) -> Self {
        self.states.push(SymbolState { key, shape, color });
        self
    }

    /// Set the symbol size in points (builder style).
    pub fn with_size(mut self, size: Vec2) -> Self {
        self.size = size;
        self
    }

    /// The underlying channel.
    pub fn channel(&self) -> &Channel {
        self.base.channel()
    }

    /// Render the symbol for the current value this frame.
    pub fn show(&mut self, ui: &mut egui::Ui) -> egui::Response {
        let state = self.base.channel().state();
        let key = state.value.as_ref().and_then(value_as_state_key);
        let keys: Vec<i64> = self.states.iter().map(|s| s.key).collect();
        let selected = symbol_index_for_value(key, &keys).map(|i| self.states[i]);

        self.base
            .framed(ui, &state, false, |ui| {
                let size = justified_size(layout_justify(ui), ui, self.size);
                let (rect, _) = ui.allocate_exact_size(size, egui::Sense::hover());
                if let Some(symbol) = selected
                    && ui.is_rect_visible(rect)
                {
                    let pts = shape_points(
                        symbol.shape,
                        rect.center(),
                        f64::from(rect.width()),
                        f64::from(rect.height()),
                        0.0,
                    );
                    ui.painter().add(egui::Shape::convex_polygon(
                        pts,
                        symbol.color,
                        egui::Stroke::NONE,
                    ));
                }
                // No matching state (or no value): nothing is drawn (PyDM paints
                // nothing on a missing key).
            })
            .response
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::Arc;

    #[test]
    fn integral_numbers_map_to_their_state_key() {
        assert_eq!(value_as_state_key(&PvValue::Int(3)), Some(3));
        assert_eq!(value_as_state_key(&PvValue::Bool(true)), Some(1));
        assert_eq!(value_as_state_key(&PvValue::Bool(false)), Some(0));
        assert_eq!(
            value_as_state_key(&PvValue::Enum {
                index: 2,
                label: None
            }),
            Some(2)
        );
        // A whole float matches an int key (Python dict semantics).
        assert_eq!(value_as_state_key(&PvValue::Float(2.0)), Some(2));
    }

    #[test]
    fn non_integral_and_non_numeric_values_have_no_key() {
        assert_eq!(value_as_state_key(&PvValue::Float(2.5)), None);
        assert_eq!(value_as_state_key(&PvValue::Float(f64::NAN)), None);
        assert_eq!(value_as_state_key(&PvValue::Str(Arc::from("on"))), None);
        assert_eq!(
            value_as_state_key(&PvValue::FloatArray(Arc::from(vec![1.0, 2.0]))),
            None
        );
    }

    #[test]
    fn lookup_returns_matching_index_or_none() {
        let keys = [0, 1, 5];
        assert_eq!(symbol_index_for_value(Some(0), &keys), Some(0));
        assert_eq!(symbol_index_for_value(Some(5), &keys), Some(2));
        // No value → nothing selected.
        assert_eq!(symbol_index_for_value(None, &keys), None);
        // Value with no configured state → nothing selected.
        assert_eq!(symbol_index_for_value(Some(2), &keys), None);
    }

    #[test]
    fn first_matching_key_wins() {
        let keys = [1, 1, 1];
        assert_eq!(symbol_index_for_value(Some(1), &keys), Some(0));
    }
}
