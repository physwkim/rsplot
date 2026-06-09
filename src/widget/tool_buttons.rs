//! Standalone plot toolbar buttons (silx `PlotToolButtons`): dropdown buttons
//! that package a single piece of plot state behind a popup menu.
//!
//! These mirror silx's reusable `QToolButton` subclasses so the same control can
//! be dropped into any toolbar, rather than being baked into one view panel:
//!
//! - [`ProfileToolButton`] — pick the profile dimension (1D vs 2D), silx
//!   `ProfileToolButton` (`PlotToolButtons.py:304-391`).
//! - [`SymbolToolButton`] — pick the marker symbol and its size, silx
//!   `SymbolToolButton` (`PlotToolButtons.py:394-477`).
//!
//! Each splits into a pure, headlessly-tested state core (the selected value,
//! its setters/clamps, and the silx label/catalog mappings) and an egui `ui`
//! method that renders the popup. The `ui` method is GPU/UI and so is reported
//! unverified; the state core is unit-tested.

use crate::core::items::Symbol;

/// silx `ProfileToolButton`: a dropdown toolbar button switching the profile
/// dimension between **1D** (one profile on the visible image) and **2D** (one
/// 1D profile for each image in a stack). silx `PlotToolButtons.py:304-391`.
///
/// The dimension is `1` or `2` (silx `getDimension`/`setDimension`).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct ProfileToolButton {
    dimension: u8,
}

impl Default for ProfileToolButton {
    fn default() -> Self {
        // silx default: `self._dimension = 1` then `computeProfileIn1D()`.
        Self { dimension: 1 }
    }
}

impl ProfileToolButton {
    /// A 1D-profile button (the silx default).
    pub fn new() -> Self {
        Self::default()
    }

    /// The selected profile dimension, `1` or `2` (silx `getDimension`).
    pub fn dimension(&self) -> u8 {
        self.dimension
    }

    /// Set the profile dimension (silx `setDimension`, which asserts `1` or `2`).
    /// Returns `true` if the value actually changed; out-of-range values and
    /// no-op repeats return `false` and leave the state untouched.
    pub fn set_dimension(&mut self, dimension: u8) -> bool {
        if matches!(dimension, 1 | 2) && dimension != self.dimension {
            self.dimension = dimension;
            true
        } else {
            false
        }
    }

    /// The menu-action label for a dimension (silx `STATE[(dim, "action")]`).
    pub fn action_label(dimension: u8) -> &'static str {
        match dimension {
            2 => "2D profile on image stack",
            _ => "1D profile on visible image",
        }
    }

    /// The tooltip/status text for a dimension (silx `STATE[(dim, "state")]`).
    pub fn state_tooltip(dimension: u8) -> &'static str {
        match dimension {
            2 => "2D profile is computed, one 1D profile for each image in the stack",
            _ => "1D profile is computed on visible image",
        }
    }

    /// Render the dropdown button (silx `ProfileToolButton` `InstantPopup` menu).
    /// Returns `Some(new_dimension)` if the user changed it this frame (silx
    /// `sigDimensionChanged`), else `None`. GPU/UI — not covered by the tests.
    pub fn ui(&mut self, ui: &mut egui::Ui) -> Option<u8> {
        let mut changed = None;
        let title = if self.dimension == 2 { "2D" } else { "1D" };
        ui.menu_button(title, |ui| {
            for dim in [1u8, 2u8] {
                let selected = self.dimension == dim;
                let resp = ui
                    .selectable_label(selected, Self::action_label(dim))
                    .on_hover_text(Self::state_tooltip(dim));
                if resp.clicked() {
                    if self.set_dimension(dim) {
                        changed = Some(dim);
                    }
                    ui.close();
                }
            }
        })
        .response
        .on_hover_text(Self::state_tooltip(self.dimension));
        changed
    }
}

/// A change emitted by [`SymbolToolButton::ui`] (silx applies the symbol and the
/// size through separate slots, `_markerChanged` vs `_sizeChanged`).
#[derive(Clone, Copy, Debug, PartialEq)]
pub enum SymbolToolChange {
    /// The user picked a new marker symbol (silx `_markerChanged`).
    Symbol(Symbol),
    /// The user changed the marker size (silx `_sizeChanged`).
    Size(f32),
}

/// silx `SymbolToolButton`: a dropdown toolbar button controlling the marker
/// **symbol** and its **size**. silx `PlotToolButtons.py:394-477`: a size slider
/// (range `1..=20`) above the list of supported symbols.
///
/// silx applies the choice to every `SymbolMixIn` item in the plot; this widget
/// only owns the selection and emits a [`SymbolToolChange`], leaving the caller
/// to apply it to its items.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct SymbolToolButton {
    symbol: Symbol,
    size: f32,
}

impl Default for SymbolToolButton {
    fn default() -> Self {
        Self {
            symbol: Symbol::Circle,
            // silx `config.DEFAULT_PLOT_SYMBOL_SIZE` (`_config.py:137`).
            size: Self::DEFAULT_SIZE,
        }
    }
}

impl SymbolToolButton {
    /// Smallest selectable symbol size (silx slider `setRange(1, 20)`).
    pub const MIN_SIZE: f32 = 1.0;
    /// Largest selectable symbol size (silx slider `setRange(1, 20)`).
    pub const MAX_SIZE: f32 = 20.0;
    /// Default symbol size (silx `config.DEFAULT_PLOT_SYMBOL_SIZE` = 6.0).
    pub const DEFAULT_SIZE: f32 = 6.0;

    /// A button defaulting to a circle at the silx default size.
    pub fn new() -> Self {
        Self::default()
    }

    /// The selected marker symbol.
    pub fn symbol(&self) -> Symbol {
        self.symbol
    }

    /// Set the selected marker symbol.
    pub fn set_symbol(&mut self, symbol: Symbol) {
        self.symbol = symbol;
    }

    /// The selected marker size (clamped to `[MIN_SIZE, MAX_SIZE]`).
    pub fn size(&self) -> f32 {
        self.size
    }

    /// Set the marker size, clamped to the silx slider range `[1, 20]`.
    pub fn set_size(&mut self, size: f32) {
        self.size = size.clamp(Self::MIN_SIZE, Self::MAX_SIZE);
    }

    /// Render the dropdown button (silx `SymbolToolButton` `InstantPopup` menu):
    /// a size slider over the list of supported symbols ([`Symbol::ALL`]).
    /// Returns the [`SymbolToolChange`] the user made this frame, else `None`.
    /// GPU/UI — not covered by the tests.
    pub fn ui(&mut self, ui: &mut egui::Ui) -> Option<SymbolToolChange> {
        let mut change = None;
        ui.menu_button(self.symbol.name(), |ui| {
            // Size slider (silx `_addSizeSliderToMenu`, range 1..=20).
            let mut size = self.size;
            if ui
                .add(egui::Slider::new(&mut size, Self::MIN_SIZE..=Self::MAX_SIZE).text("Size"))
                .changed()
            {
                self.set_size(size);
                change = Some(SymbolToolChange::Size(self.size));
            }
            ui.separator();
            // Symbol list (silx `_addSymbolsToMenu`).
            for symbol in Symbol::ALL {
                if ui
                    .selectable_label(self.symbol == symbol, symbol.name())
                    .clicked()
                {
                    self.set_symbol(symbol);
                    change = Some(SymbolToolChange::Symbol(symbol));
                    ui.close();
                }
            }
        });
        change
    }
}

/// silx `RulerToolButton` (`tools/RulerToolButton.py:83-181`): a **checkable**
/// toolbar button that, while active, lets the user measure the distance between
/// two points by drawing a line ROI whose label shows the distance.
///
/// Following the same split as the other [`tool_buttons`](self) widgets, this
/// owns only the reusable pieces silx's `RulerToolButton` provides — the
/// checked/active state (silx `setCheckable(True)`/`isChecked`) and the
/// distance-label formatter (silx `buildDistanceText`) — and leaves the host to
/// drive the line ROI: when [`is_active`](Self::is_active) the caller enters a
/// line-ROI draw and names the drawn ROI with [`distance_text`](Self::distance_text).
/// silx's `_RulerROI` maps onto siplot's existing line-ROI draw; siplot has no
/// live-updating ROI format-function, so the host recomputes the label.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct RulerToolButton {
    active: bool,
}

impl RulerToolButton {
    /// A ruler button, inactive by default (silx button starts unchecked).
    pub fn new() -> Self {
        Self::default()
    }

    /// Whether the ruler is active (silx `isChecked`). While active the host
    /// drives a line-ROI measurement.
    pub fn is_active(&self) -> bool {
        self.active
    }

    /// Set the active state (silx `setChecked`).
    pub fn set_active(&mut self, active: bool) {
        self.active = active;
    }

    /// Flip the active state and return the new value (silx checkable `toggle`).
    pub fn toggle(&mut self) -> bool {
        self.active = !self.active;
        self.active
    }

    /// The ruler label for a measured segment, mirroring silx
    /// `RulerToolButton.buildDistanceText`: the Euclidean distance between the
    /// two data-space endpoints, formatted `" {:.1}px"`. silx uses Python's
    /// `f"{distance: .1f}px"`; the space flag prints a leading space for a
    /// non-negative value, and the distance is a vector norm so it is always
    /// ≥ 0 — hence the always-present leading space. Pure and deterministic, so
    /// the formatting is unit-testable without a GPU backend.
    pub fn distance_text(start: [f64; 2], end: [f64; 2]) -> String {
        let dx = end[0] - start[0];
        let dy = end[1] - start[1];
        let distance = (dx * dx + dy * dy).sqrt();
        format!(" {distance:.1}px")
    }

    /// Render the checkable ruler button (silx `RulerToolButton`, a checkable
    /// `QToolButton`). Returns `Some(new_active)` when the user toggles it this
    /// frame (silx `toggled`), else `None`. GPU/UI — not covered by the tests.
    pub fn ui(&mut self, ui: &mut egui::Ui) -> Option<bool> {
        let resp = ui
            .selectable_label(self.active, "Ruler")
            .on_hover_text("Measure the distance between two points of the plot");
        if resp.clicked() {
            return Some(self.toggle());
        }
        None
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn profile_button_defaults_to_1d() {
        assert_eq!(ProfileToolButton::new().dimension(), 1);
    }

    #[test]
    fn profile_set_dimension_accepts_only_1_and_2() {
        let mut b = ProfileToolButton::new();
        // No-op on the current value.
        assert!(!b.set_dimension(1));
        // Switch to 2D.
        assert!(b.set_dimension(2));
        assert_eq!(b.dimension(), 2);
        // Out-of-range is rejected and leaves the state at 2.
        assert!(!b.set_dimension(0));
        assert!(!b.set_dimension(3));
        assert_eq!(b.dimension(), 2);
        // Back to 1D.
        assert!(b.set_dimension(1));
        assert_eq!(b.dimension(), 1);
    }

    #[test]
    fn profile_labels_match_silx_state() {
        assert_eq!(
            ProfileToolButton::action_label(1),
            "1D profile on visible image"
        );
        assert_eq!(
            ProfileToolButton::action_label(2),
            "2D profile on image stack"
        );
        assert_eq!(
            ProfileToolButton::state_tooltip(1),
            "1D profile is computed on visible image"
        );
        assert_eq!(
            ProfileToolButton::state_tooltip(2),
            "2D profile is computed, one 1D profile for each image in the stack"
        );
    }

    #[test]
    fn symbol_button_defaults_to_circle_at_silx_size() {
        let b = SymbolToolButton::new();
        assert_eq!(b.symbol(), Symbol::Circle);
        assert_eq!(b.size(), 6.0);
    }

    #[test]
    fn symbol_set_size_clamps_to_silx_slider_range() {
        let mut b = SymbolToolButton::new();
        b.set_size(0.5);
        assert_eq!(b.size(), SymbolToolButton::MIN_SIZE); // 1.0
        b.set_size(99.0);
        assert_eq!(b.size(), SymbolToolButton::MAX_SIZE); // 20.0
        b.set_size(12.0);
        assert_eq!(b.size(), 12.0);
    }

    #[test]
    fn symbol_set_symbol_updates_selection() {
        let mut b = SymbolToolButton::new();
        b.set_symbol(Symbol::Diamond);
        assert_eq!(b.symbol(), Symbol::Diamond);
    }

    #[test]
    fn ruler_button_defaults_inactive_and_toggles() {
        let mut b = RulerToolButton::new();
        assert!(!b.is_active(), "silx ruler button starts unchecked");
        assert!(b.toggle(), "toggle activates");
        assert!(b.is_active());
        assert!(!b.toggle(), "toggle deactivates");
        assert!(!b.is_active());
        b.set_active(true);
        assert!(b.is_active());
    }

    #[test]
    fn ruler_distance_text_matches_silx_format() {
        // 3-4-5 right triangle: norm == 5.0. silx `f"{5.0: .1f}px"` -> " 5.0px"
        // (space flag => leading space for the non-negative norm).
        assert_eq!(
            RulerToolButton::distance_text([0.0, 0.0], [3.0, 4.0]),
            " 5.0px"
        );
        // Zero-length segment.
        assert_eq!(
            RulerToolButton::distance_text([2.0, 7.0], [2.0, 7.0]),
            " 0.0px"
        );
        // Direction-independent (norm), and rounds to one decimal.
        assert_eq!(
            RulerToolButton::distance_text([4.0, 4.0], [0.0, 1.0]),
            " 5.0px"
        );
        assert_eq!(
            RulerToolButton::distance_text([0.0, 0.0], [1.0, 1.0]),
            " 1.4px"
        );
    }
}
