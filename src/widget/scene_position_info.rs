//! [`ScenePositionInfo`] — a cursor position/value readout for a 3D scalar field.
//!
//! Port of silx `silx.gui.plot3d.tools.PositionInfoWidget.PositionInfoWidget`:
//! a small panel showing the **X / Y / Z** scene coordinates and the **Data**
//! value of the item picked under the cursor (silx fields `_xLabel`/`_yLabel`/
//! `_zLabel`/`_dataLabel`), each `-` when nothing is picked. silx drives it from
//! the cursor position (`updateInfo` → `pick(x, y)`); here the owner
//! ([`crate::widget::SceneWindow`]) feeds it the pick result of
//! [`crate::ScalarFieldView::pick`] each frame.
//!
//! The Qt picking-mode toggle action is not ported (interactive-mode toolbars
//! are Qt shell, like the rest of the `SceneWindow` chrome the roadmap lists as
//! N/A); the readout itself is the substance.

use egui::Ui;

use crate::widget::scalar_field_view::FieldPick;

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

    /// Draw the X / Y / Z / Data fields in one row, showing `-` for any field
    /// without a value (silx lays them out as `label: value` pairs).
    pub fn ui(&self, ui: &mut Ui) {
        let (x, y, z) = match self.last {
            Some(p) => (g(p.position.x), g(p.position.y), g(p.position.z)),
            None => (dash(), dash(), dash()),
        };
        let data = match self.last.and_then(|p| p.value) {
            Some(v) => g(v),
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
        });
    }
}

/// The empty-field placeholder (silx sets each label to `"-"`).
fn dash() -> String {
    "-".to_string()
}

/// Format a value the way silx's `"%g"` does for the readout: shortest
/// round-trippable form (Rust's default float `Display`), e.g. `1.5`, `0.5`.
fn g(v: f32) -> String {
    format!("{v}")
}
