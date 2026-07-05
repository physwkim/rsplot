//! Alpha (opacity) slider.
//!
//! [`AlphaSlider`] mirrors silx `BaseAlphaSlider` (`AlphaSlider.py:86-156`): a
//! slider whose state is an integer in `0..=255`, exposing the corresponding
//! opacity as a float in `[0.0, 1.0]` with a step of `1/255`. silx's concrete
//! subclasses bind the value to a specific plot item:
//! [`ActiveImageAlphaSlider`] (silx `ActiveImageAlphaSlider`, the active image)
//! and [`NamedItemAlphaSlider`] (silx `NamedImageAlphaSlider`, an image by
//! legend) drive a [`PlotWidget`]'s image alpha through its retained
//! [`set_image_alpha`](PlotWidget::set_image_alpha) path. silx's curve/scatter
//! named bindings are not mirrored: rsplot retains a re-applicable per-item
//! alpha only for scalar images (a curve bakes opacity into its color and a
//! scatter retains no data), so those sliders would have nothing to drive — the
//! bindings disable when the named item is absent or carries no addressable
//! alpha, mirroring silx's "no item → disabled" rule.

use egui::{Response, Slider};

use crate::core::backend::ItemHandle;
use crate::widget::high_level::PlotWidget;

/// Orientation of the slider track.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Default)]
pub enum AlphaSliderOrientation {
    /// Track runs left→right (egui's default).
    #[default]
    Horizontal,
    /// Track runs bottom→top.
    Vertical,
}

/// A 0..=255 integer opacity slider, exposing alpha as both `u8` and `f32`.
///
/// The integer is the canonical state (silx stores `0..255` and emits it from
/// `valueChanged`); the float opacity is `value / 255` (silx
/// `BaseAlphaSlider.getAlpha`). Setters accept either form and clamp into range.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct AlphaSlider {
    /// Opacity as a `0..=255` integer (silx's internal slider state).
    value: u8,
    /// Track orientation.
    pub orientation: AlphaSliderOrientation,
}

impl Default for AlphaSlider {
    /// Fully opaque (silx initializes a slider with no item to 255).
    fn default() -> Self {
        Self {
            value: 255,
            orientation: AlphaSliderOrientation::default(),
        }
    }
}

impl AlphaSlider {
    /// A slider at integer opacity `value` (`0..=255`).
    pub fn new(value: u8) -> Self {
        Self {
            value,
            orientation: AlphaSliderOrientation::default(),
        }
    }

    /// A slider at float opacity `alpha` (`[0.0, 1.0]`); see [`Self::set_alpha`]
    /// for the conversion.
    pub fn from_alpha(alpha: f32) -> Self {
        let mut s = Self::default();
        s.set_alpha(alpha);
        s
    }

    /// Set the track orientation (builder form).
    pub fn with_orientation(mut self, orientation: AlphaSliderOrientation) -> Self {
        self.orientation = orientation;
        self
    }

    /// The opacity as a `0..=255` integer (silx `QSlider.value`).
    pub fn value(&self) -> u8 {
        self.value
    }

    /// Set the opacity from a `0..=255` integer.
    pub fn set_value(&mut self, value: u8) {
        self.value = value;
    }

    /// The opacity as a float in `[0.0, 1.0]`, `value / 255` (silx
    /// `BaseAlphaSlider.getAlpha`).
    pub fn alpha(&self) -> f32 {
        alpha_from_u8(self.value)
    }

    /// Set the opacity from a float in `[0.0, 1.0]`. The float is clamped into
    /// range and converted to the nearest integer, mirroring silx's
    /// `round(255 * alpha)` when seeding the slider from an item's alpha.
    pub fn set_alpha(&mut self, alpha: f32) {
        self.value = u8_from_alpha(alpha);
    }

    /// Show the slider; returns its [`egui::Response`]. When the value changes
    /// the response reports `changed()`, matching silx's `valueChanged` signal.
    pub fn ui(&mut self, ui: &mut egui::Ui) -> Response {
        match self.orientation {
            AlphaSliderOrientation::Horizontal => {
                ui.add(Slider::new(&mut self.value, 0..=255).text("alpha"))
            }
            AlphaSliderOrientation::Vertical => {
                ui.add(Slider::new(&mut self.value, 0..=255).vertical())
            }
        }
    }
}

/// Convert a `0..=255` integer opacity to a float in `[0.0, 1.0]`: `value / 255`
/// (silx `BaseAlphaSlider.getAlpha`).
fn alpha_from_u8(value: u8) -> f32 {
    value as f32 / 255.0
}

/// Convert a float opacity in `[0.0, 1.0]` to a `0..=255` integer: clamp then
/// `round(255 * alpha)` (silx `round(255 * alpha)`).
fn u8_from_alpha(alpha: f32) -> u8 {
    let clamped = alpha.clamp(0.0, 1.0);
    (clamped * 255.0).round() as u8
}

/// An [`AlphaSlider`] bound to a [`PlotWidget`]'s **active image** (silx
/// `ActiveImageAlphaSlider`, `AlphaSlider.py:158-193`).
///
/// Like silx's slider it holds the plot's active image as its target
/// (`getItem() = getActiveImage()`): the slider seeds from the bound image's
/// alpha and writes every change back through
/// [`PlotWidget::set_active_image_alpha`]. With no scalar active image present
/// the slider disables (silx disables when `getItem()` is `None`).
///
/// Immediate-mode binding: the host calls [`show`](Self::show) each frame with
/// the plot, instead of silx connecting to `sigActiveImageChanged`. One
/// deliberate deviation from silx: when the active image *switches* to a
/// different image, this re-seeds the slider from the new image's alpha (so the
/// slider always shows the bound image's opacity), whereas silx pushes the
/// slider's current value onto the newly-activated image. The first bind and
/// the write-on-change paths match silx.
#[derive(Clone, Debug, Default)]
pub struct ActiveImageAlphaSlider {
    slider: AlphaSlider,
    /// The image handle the slider is currently seeded from, so a change of
    /// active image triggers a re-seed (silx `_activeImageChanged`).
    bound: Option<ItemHandle>,
}

impl ActiveImageAlphaSlider {
    /// A new active-image alpha slider (unbound until [`show`](Self::show) sees a
    /// plot with a scalar active image).
    pub fn new() -> Self {
        Self::default()
    }

    /// Set the track orientation (builder form), forwarded to the inner slider.
    pub fn with_orientation(mut self, orientation: AlphaSliderOrientation) -> Self {
        self.slider.orientation = orientation;
        self
    }

    /// The current opacity as a float in `[0.0, 1.0]` (silx `getAlpha`).
    pub fn alpha(&self) -> f32 {
        self.slider.alpha()
    }

    /// The current opacity as a `0..=255` integer (silx slider value).
    pub fn value(&self) -> u8 {
        self.slider.value()
    }

    /// Show the slider bound to `plot`'s active image; returns its [`Response`].
    ///
    /// Seeds from the bound image's alpha on (re)binding, disables when there is
    /// no scalar active image, and on a value change applies the new opacity to
    /// the active image (silx `_updateItem` → `item.setAlpha`).
    pub fn show(&mut self, ui: &mut egui::Ui, plot: &mut PlotWidget) -> Response {
        let item = plot.active_image_handle();
        if item != self.bound {
            self.bound = item;
            if let Some(alpha) = plot.active_image_alpha() {
                self.slider.set_alpha(alpha);
            }
        }
        let enabled = item.is_some();
        let response = ui.add_enabled_ui(enabled, |ui| self.slider.ui(ui)).inner;
        if enabled && response.changed() {
            plot.set_active_image_alpha(self.slider.alpha());
        }
        response
    }
}

/// An [`AlphaSlider`] bound to a [`PlotWidget`]'s **image identified by legend**
/// (silx `NamedItemAlphaSlider` / `NamedImageAlphaSlider`,
/// `AlphaSlider.py:196-285`).
///
/// silx's `NamedItemAlphaSlider` is addressed by `(kind, legend)` and can target
/// an image, scatter, or curve. rsplot retains a re-applicable per-item alpha
/// only for scalar images, so this binds to the image carrying `legend`
/// (silx `NamedImageAlphaSlider`); the scatter/curve named bindings are deferred
/// (those items carry no addressable per-item alpha here). The slider disables
/// when no image with that legend exists, mirroring silx's
/// `_updateState`/`_onContentChanged` enable/disable on item add/remove.
#[derive(Clone, Debug, Default)]
pub struct NamedItemAlphaSlider {
    slider: AlphaSlider,
    /// Legend of the image whose opacity this slider controls (silx
    /// `_item_legend`).
    legend: String,
    /// The resolved image handle the slider is currently seeded from, so a
    /// change of target (new legend, item add/remove) triggers a re-seed.
    bound: Option<ItemHandle>,
}

impl NamedItemAlphaSlider {
    /// A slider controlling the opacity of the image with `legend` (silx
    /// `NamedImageAlphaSlider(legend=...)`).
    pub fn new(legend: impl Into<String>) -> Self {
        Self {
            slider: AlphaSlider::default(),
            legend: legend.into(),
            bound: None,
        }
    }

    /// Set the track orientation (builder form), forwarded to the inner slider.
    pub fn with_orientation(mut self, orientation: AlphaSliderOrientation) -> Self {
        self.slider.orientation = orientation;
        self
    }

    /// The legend of the image currently controlled by this slider (silx
    /// `getLegend`).
    pub fn legend(&self) -> &str {
        &self.legend
    }

    /// Associate a different image legend with the slider (silx `setLegend`);
    /// the next [`show`](Self::show) re-seeds from the new target.
    pub fn set_legend(&mut self, legend: impl Into<String>) {
        self.legend = legend.into();
    }

    /// The current opacity as a float in `[0.0, 1.0]` (silx `getAlpha`).
    pub fn alpha(&self) -> f32 {
        self.slider.alpha()
    }

    /// The current opacity as a `0..=255` integer (silx slider value).
    pub fn value(&self) -> u8 {
        self.slider.value()
    }

    /// Show the slider bound to the image named [`legend`](Self::legend) in
    /// `plot`; returns its [`Response`]. Seeds from the bound image on
    /// (re)binding, disables when no such image exists, and applies a value
    /// change to that image (silx `_updateItem` → `item.setAlpha`).
    pub fn show(&mut self, ui: &mut egui::Ui, plot: &mut PlotWidget) -> Response {
        let item = plot.image_by_legend(&self.legend);
        if item != self.bound {
            self.bound = item;
            if let Some(handle) = item
                && let Some(alpha) = plot.image_alpha(handle)
            {
                self.slider.set_alpha(alpha);
            }
        }
        let enabled = item.is_some();
        let response = ui.add_enabled_ui(enabled, |ui| self.slider.ui(ui)).inner;
        if enabled
            && response.changed()
            && let Some(handle) = item
        {
            plot.set_image_alpha(handle, self.slider.alpha());
        }
        response
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    // --- u8 <-> f32 conversion boundaries --------------------------------

    #[test]
    fn alpha_from_u8_at_boundaries() {
        assert_eq!(alpha_from_u8(0), 0.0);
        assert_eq!(alpha_from_u8(255), 1.0);
        // 128 / 255 ~= 0.50196.
        assert!((alpha_from_u8(128) - 128.0 / 255.0).abs() < 1e-7);
    }

    #[test]
    fn u8_from_alpha_at_boundaries() {
        assert_eq!(u8_from_alpha(0.0), 0);
        assert_eq!(u8_from_alpha(1.0), 255);
        // round(255 * 0.5) = round(127.5) = 128 (round-half-away-from-zero).
        assert_eq!(u8_from_alpha(0.5), 128);
    }

    #[test]
    fn u8_from_alpha_clamps_out_of_range() {
        assert_eq!(u8_from_alpha(-0.5), 0);
        assert_eq!(u8_from_alpha(2.0), 255);
        // NaN clamps via f32::clamp's NaN handling to the low bound.
        assert_eq!(u8_from_alpha(f32::NAN), 0);
    }

    #[test]
    fn u8_from_alpha_rounds_to_nearest() {
        // round(255 * 0.4980) = round(126.99) = 127.
        assert_eq!(u8_from_alpha(0.498), 127);
        // round(255 * 0.502) = round(128.01) = 128.
        assert_eq!(u8_from_alpha(0.502), 128);
    }

    // --- widget value/setters --------------------------------------------

    #[test]
    fn default_is_fully_opaque() {
        let s = AlphaSlider::default();
        assert_eq!(s.value(), 255);
        assert_eq!(s.alpha(), 1.0);
        assert_eq!(s.orientation, AlphaSliderOrientation::Horizontal);
    }

    #[test]
    fn new_sets_integer_value() {
        let s = AlphaSlider::new(0);
        assert_eq!(s.value(), 0);
        assert_eq!(s.alpha(), 0.0);
    }

    #[test]
    fn from_alpha_rounds_to_integer() {
        // round(255 * 0.5) = 128.
        let s = AlphaSlider::from_alpha(0.5);
        assert_eq!(s.value(), 128);
        // Clamps out-of-range alpha.
        assert_eq!(AlphaSlider::from_alpha(2.0).value(), 255);
        assert_eq!(AlphaSlider::from_alpha(-1.0).value(), 0);
    }

    #[test]
    fn set_value_and_set_alpha_round_trip_at_boundaries() {
        let mut s = AlphaSlider::new(0);

        s.set_value(255);
        assert_eq!(s.value(), 255);
        assert_eq!(s.alpha(), 1.0);

        s.set_value(128);
        assert_eq!(s.value(), 128);
        assert!((s.alpha() - 128.0 / 255.0).abs() < 1e-7);

        s.set_alpha(0.0);
        assert_eq!(s.value(), 0);

        s.set_alpha(1.0);
        assert_eq!(s.value(), 255);
    }

    #[test]
    fn with_orientation_sets_vertical() {
        let s = AlphaSlider::default().with_orientation(AlphaSliderOrientation::Vertical);
        assert_eq!(s.orientation, AlphaSliderOrientation::Vertical);
    }
}
