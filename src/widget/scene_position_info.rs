//! [`ScenePositionInfo`] — a cursor position/value readout for a 3D scalar field.
//!
//! Port of silx `silx.gui.plot3d.tools.PositionInfoWidget.PositionInfoWidget`:
//! a small panel showing the **X / Y / Z** scene coordinates, the **Data** value,
//! and the **Item** of the thing picked under the cursor (silx fields
//! `_xLabel`/`_yLabel`/`_zLabel`/`_dataLabel`/`_itemLabel`, in that layout order,
//! `PositionInfoWidget.py:56-60`), each `-`/empty when nothing is picked. silx
//! drives it from the cursor position (`updateInfo` → `pick(x, y)`); here the
//! owner ([`crate::SceneWindow`]) feeds it the pick result of
//! [`crate::ScalarFieldView::pick`] each frame.
//!
//! The Item field shows the picked item's `getLabel()`
//! (`PositionInfoWidget.py:201`) — silx `Item3D`'s default label is its class
//! name (`items/core.py:99`), so an iso-surface hit reads "Isosurface" and a
//! cut-plane hit "CutPlane" ([`crate::FieldPickItem::label`]).
//!
//! The Qt picking-mode toggle action is not ported (interactive-mode toolbars
//! are Qt shell, like the rest of the `SceneWindow` chrome the roadmap lists as
//! N/A); the readout itself is the substance.

use egui::Ui;

use crate::widget::scalar_field_view::FieldPick;
use crate::widget::stats_widget::format_g_python;

/// A position/value readout fed by [`crate::ScalarFieldView::pick`]. Hold one,
/// call [`set`](ScenePositionInfo::set) with the current pick each frame, and
/// [`ui`](ScenePositionInfo::ui) to draw the X/Y/Z/Data fields.
#[derive(Clone, Copy, Debug, Default)]
pub struct ScenePositionInfo {
    last: Option<FieldPick>,
}

impl ScenePositionInfo {
    /// An empty readout (all fields `-`).
    pub fn new() -> Self {
        Self::default()
    }

    /// Set the current pick (or `None` to clear), as silx `pick` stores the
    /// closest `PickingResult`.
    pub fn set(&mut self, pick: Option<FieldPick>) {
        self.last = pick;
    }

    /// Clear the readout (silx `clear`: every field back to `-`).
    pub fn clear(&mut self) {
        self.last = None;
    }

    /// The last pick set on this readout, if any.
    pub fn last(&self) -> Option<FieldPick> {
        self.last
    }

    /// Draw the X / Y / Z / Data / Item fields in one row (silx layout order,
    /// `PositionInfoWidget.py:56-60`), showing `-` for any field without a value
    /// (silx lays them out as `label: value` pairs).
    pub fn ui(&self, ui: &mut Ui) {
        let (x, y, z) = match self.last {
            Some(p) => (g(p.position.x), g(p.position.y), g(p.position.z)),
            None => (dash(), dash(), dash()),
        };
        let data = match self.last.and_then(|p| p.value) {
            Some(v) => g(v),
            None => dash(),
        };
        // The Item field: the picked item's silx label, or `-` when nothing is
        // picked (silx leaves `_itemLabel` at its cleared `-`).
        let item = match self.last {
            Some(p) => p.item.label().to_string(),
            None => dash(),
        };
        ui.horizontal(|ui| {
            ui.label(format!("X: {x}"));
            ui.separator();
            ui.label(format!("Y: {y}"));
            ui.separator();
            ui.label(format!("Z: {z}"));
            ui.separator();
            ui.label(format!("Data: {data}"));
            ui.separator();
            ui.label(format!("Item: {item}"));
        });
    }
}

/// The empty-field placeholder (silx sets each label to `"-"`).
fn dash() -> String {
    "-".to_string()
}

/// Format a value as silx's readout does — CPython `"%g"` with its default 6
/// significant digits (`PositionInfoWidget.py:205-215`, `"%g" % x`), **not**
/// Rust's default float `Display`. `Display` prints the shortest round-trippable
/// form (`0.123456789` in full), whereas silx `%g` rounds to 6 sig digits
/// (`0.123457`). silx uses `"%.3g"` for array-valued data, but siplot's
/// [`FieldPick::value`] is a single scalar, so only the scalar `%g` path applies.
fn g(v: f32) -> String {
    format_g_python(f64::from(v), 6)
}

#[cfg(test)]
mod tests {
    use super::g;

    #[test]
    fn g_rounds_to_six_significant_digits_like_python_g() {
        // silx `"%g" % 0.12345679` → "0.123457" (6 sig digits); Rust `Display`
        // would print the full round-trippable "0.12345679". This is the exact
        // divergence the old `fn g` doc wrongly claimed was equivalent.
        let v = 0.123_456_79_f32;
        assert_eq!(g(v), "0.123457");
        assert_ne!(g(v), format!("{v}"));
    }

    #[test]
    fn g_drops_trailing_zeros_like_python_g() {
        // `%g` strips a trailing `.0` and trailing fraction zeros.
        assert_eq!(g(5.0), "5");
        assert_eq!(g(1.5), "1.5");
        assert_eq!(g(-0.25), "-0.25");
    }
}
