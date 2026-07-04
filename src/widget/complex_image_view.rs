//! Display of a single 2D complex dataset with selectable visualization mode.
//!
//! Ports silx `ComplexImageView.py` (and the conversion math of
//! `silx/gui/plot/items/complex.py`, `ImageComplexData`). A
//! [`ComplexImageView`] owns a [`Plot2D`], the complex data, and the current
//! [`ComplexMode`]; switching modes recomputes the displayed image in place
//! without resetting the zoom.
//!
//! Scalar modes feed a colormapped `f32` image to the plot; `AMPLITUDE_PHASE`
//! feeds an HSV-composite RGBA image. silx maps the phase through a `hsv`
//! colormap over `[-pi, pi]`, which is what the [`phase_hsv_lut`] /
//! [`hsv_to_rgb`] helpers reproduce here (the crate colormap catalog has no
//! `hsv` entry).

use egui_wgpu::RenderState;

use crate::core::backend::{ImageSpec, ItemHandle};
use crate::core::colormap::{AutoscaleMode, Colormap, ColormapName};
use crate::core::plot::PlotId;
use crate::widget::high_level::{Plot2D, PlotDataError};
use crate::widget::plot_widget::PlotResponse;

// The visualization mode is the shared silx `ComplexMixIn.ComplexMode`; it lives
// in `core` so the 2D image view and the 3D `ComplexField3D` share one enum
// without `render` depending on `widget`. Re-exported here for the 2D path's
// existing call sites.
pub use crate::core::complex::ComplexMode;

/// Map an HSV triple to sRGB bytes (saturation and value in `[0, 1]`, hue
/// wrapped into `[0, 1)`).
///
/// This is the standard HSV→RGB conversion silx's `hsv` colormap performs;
/// at `s == 1`, `v == 1` it sweeps the full hue circle (red → yellow → green →
/// cyan → blue → magenta → red), which is how silx color-codes the phase.
pub fn hsv_to_rgb(h: f32, s: f32, v: f32) -> [u8; 3] {
    let h = h.rem_euclid(1.0) * 6.0;
    let sector = h.floor();
    let f = h - sector;
    let p = v * (1.0 - s);
    let q = v * (1.0 - s * f);
    let t = v * (1.0 - s * (1.0 - f));
    let (r, g, b) = match sector as i32 % 6 {
        0 => (v, t, p),
        1 => (q, v, p),
        2 => (p, v, t),
        3 => (p, q, v),
        4 => (t, p, v),
        _ => (v, p, q),
    };
    [
        (r * 255.0).round().clamp(0.0, 255.0) as u8,
        (g * 255.0).round().clamp(0.0, 255.0) as u8,
        (b * 255.0).round().clamp(0.0, 255.0) as u8,
    ]
}

/// Build the 256-entry `hsv` LUT silx uses for the phase colormap: hue swept
/// linearly across `[0, 1]` at full saturation and value.
pub fn phase_hsv_lut() -> [[u8; 4]; 256] {
    let mut lut = [[0u8; 4]; 256];
    for (i, entry) in lut.iter_mut().enumerate() {
        let [r, g, b] = hsv_to_rgb(i as f32 / 255.0, 1.0, 1.0);
        *entry = [r, g, b, 255];
    }
    lut
}

/// The silx phase colormap: an `hsv` LUT over the fixed range `[-pi, pi]`.
///
/// Used by [`ComplexMode::Phase`] (and the phase channel of the composite),
/// matching silx `Colormap(name="hsv", vmin=-numpy.pi, vmax=numpy.pi)`.
pub fn phase_colormap() -> Colormap {
    // Build through the constructor so colormap fields (gamma, nan_color, and
    // any added later) take their defaults, then install the bespoke phase hsv
    // LUT over the fixed [-pi, pi] range.
    Colormap {
        lut: phase_hsv_lut(),
        ..Colormap::new(
            ColormapName::Hsv,
            -std::f64::consts::PI,
            std::f64::consts::PI,
        )
    }
}

/// The colormap a scalar [`ComplexMode`] displays through: the fixed `hsv`
/// phase colormap for [`ComplexMode::Phase`], else `base` resolved per bound
/// against `scalar` — silx shares one `ColormapMixIn` colormap across the
/// non-phase scalar modes and fills only its unpinned bounds from the data
/// (`getColormapRange`). An auto default tracks the data; a user-pinned range
/// survives. Pure (no GPU), so the range derivation is unit-testable.
fn scalar_mode_colormap(mode: ComplexMode, base: &Colormap, scalar: &[f32]) -> Colormap {
    if mode == ComplexMode::Phase {
        phase_colormap()
    } else {
        let data: Vec<f64> = scalar.iter().map(|&v| f64::from(v)).collect();
        base.resolved(AutoscaleMode::MinMax, &data)
    }
}

/// Build the `AMPLITUDE_PHASE` RGBA composite for row-major complex `data`.
///
/// Hue is the phase `atan2(im, re)` mapped from `[-pi, pi]` to `[0, 1]`,
/// saturation is `1`, and value is the amplitude `hypot(re, im)` normalized to
/// its maximum over the whole array (matching the task's HSV mapping; silx's
/// `_complex2rgbalin` puts the same normalized amplitude in the alpha channel).
/// Alpha is fully opaque. When the data is empty or the max amplitude is `0`,
/// all values are taken as `0`.
pub fn amplitude_phase_rgba(data: &[(f32, f32)]) -> Vec<[u8; 4]> {
    let max_amp = data
        .iter()
        .map(|&(re, im)| re.hypot(im))
        .fold(0.0f32, f32::max);

    data.iter()
        .map(|&(re, im)| {
            let phase = im.atan2(re);
            // Map [-pi, pi] -> [0, 1]; +pi and -pi share the same hue (red).
            let hue = (phase + std::f32::consts::PI) / (2.0 * std::f32::consts::PI);
            let value = if max_amp > 0.0 {
                re.hypot(im) / max_amp
            } else {
                0.0
            };
            let [r, g, b] = hsv_to_rgb(hue, 1.0, value);
            [r, g, b, 255]
        })
        .collect()
}

/// silx default displayed amplitude range in log10 units
/// (`_AmplitudeRangeDialog` `displayedRange` default `(None, 2)`,
/// `ImageComplexData._setAmplitudeRangeInfo` `delta=2`).
pub const DEFAULT_AMPLITUDE_DELTA: f32 = 2.0;

/// Build the LOG10 `AMPLITUDE_PHASE` RGBA composite for row-major complex
/// `data`, porting the amplitude math of silx `_complex2rgbalog`
/// (items/complex.py:62-82) with a runtime-settable displayed amplitude range.
///
/// `max_amplitude` is silx `smax` (the dialog's "Displayed Max."): `Some(m)`
/// clamps every amplitude above `m` down to `m` (so the brightest pixels
/// saturate); `None` autoscales to the data's own maximum amplitude. `delta` is
/// silx `dlogs` (the dialog's "Displayed delta (log10 unit)", default
/// [`DEFAULT_AMPLITUDE_DELTA`], `>= 1` per the silx validator): the number of
/// log10 orders of magnitude shown below the (clamped) maximum.
///
/// The amplitude is taken to `a = log10(|z| + 1e-20)`, shifted so the maximum
/// maps to `delta` (silx `a -= a.max() - dlogs`), then normalized to `a / delta`
/// clamped to `[0, 1]`. Like [`amplitude_phase_rgba`], that normalized amplitude
/// drives the HSV *value* channel (siplot's opaque-RGBA convention) rather than
/// silx's alpha channel; the clamping + log-window normalization is the faithful
/// silx port. Hue is the phase, saturation `1`, alpha opaque. Empty data yields
/// an empty vector. Uniform-amplitude data maps every pixel to full value (the
/// silx degenerate case where `a.max()` equals every `a`).
pub fn amplitude_phase_log_rgba(
    data: &[(f32, f32)],
    max_amplitude: Option<f32>,
    delta: f32,
) -> Vec<[u8; 4]> {
    if data.is_empty() {
        return Vec::new();
    }
    // Amplitudes, optionally clamped to the displayed max (silx `smax`), then
    // taken to log10 with the silx `+ 1e-20` floor so zero amplitudes are well
    // defined.
    let logs: Vec<f32> = data
        .iter()
        .map(|&(re, im)| {
            let mut a = re.hypot(im);
            if let Some(m) = max_amplitude
                && a > m
            {
                a = m;
            }
            (a + 1e-20_f32).log10()
        })
        .collect();
    let log_max = logs.iter().copied().fold(f32::NEG_INFINITY, f32::max);

    data.iter()
        .zip(logs)
        .map(|(&(re, im), log_a)| {
            // Shift so the max maps to `delta`, display `delta` orders of
            // magnitude, then normalize into [0, 1] (silx `a/dlogs`, with the
            // `(a > 0)` mask folded into the lower clamp).
            let a = log_a - (log_max - delta);
            let value = (a / delta).clamp(0.0, 1.0);
            let phase = im.atan2(re);
            let hue = (phase + std::f32::consts::PI) / (2.0 * std::f32::consts::PI);
            let [r, g, b] = hsv_to_rgb(hue, 1.0, value);
            [r, g, b, 255]
        })
        .collect()
}

/// The maximum finite amplitude `|z|` over `data`, or `0.0` when there is no
/// finite sample. Used to seed the "Displayed Max." field when the user leaves
/// autoscale (silx autoscale is `numpy.absolute(data).max()`); non-finite
/// samples are skipped so a stray NaN/inf does not poison the seeded value.
fn data_max_amplitude(data: &[(f32, f32)]) -> f32 {
    data.iter()
        .map(|&(re, im)| re.hypot(im))
        .filter(|a| a.is_finite())
        .fold(0.0_f32, f32::max)
}

/// Render a horizontal toolbar of selectable mode buttons (one per
/// [`ComplexMode`] in silx menu order) and return the mode the user picked this
/// frame, or `None` if no button was clicked.
///
/// Pure over an [`egui::Ui`] and the `current` mode (no GPU / [`Plot2D`]), so
/// the toolbar's selection behaviour is unit-testable with a headless egui
/// context. [`ComplexImageView::show_mode_toolbar`] applies the returned mode
/// via [`ComplexImageView::set_mode`].
pub fn mode_toolbar_ui(ui: &mut egui::Ui, current: ComplexMode) -> Option<ComplexMode> {
    let mut picked = None;
    ui.horizontal(|ui| {
        for mode in ComplexMode::ALL {
            if ui.selectable_label(current == mode, mode.label()).clicked() && current != mode {
                picked = Some(mode);
            }
        }
    });
    picked
}

/// Display an image of complex data and let the user choose the visualization.
///
/// Mirrors silx `ComplexImageView`: it owns a [`Plot2D`], the complex data
/// (`(re, im)` pairs) with its width/height, and the current [`ComplexMode`].
///
/// ```ignore
/// let mut view = ComplexImageView::new(render_state, 0);
/// view.set_data(w, h, &samples)?;
///
/// // frame loop
/// view.show_mode_controls(ui);
/// view.show(ui);
/// ```
pub struct ComplexImageView {
    plot: Plot2D,
    width: u32,
    height: u32,
    data: Vec<(f32, f32)>,
    mode: ComplexMode,
    /// The base colormap shared by every non-phase scalar mode (ABSOLUTE /
    /// REAL / IMAGINARY / SQUARE_AMPLITUDE), matching silx's single
    /// `ColormapMixIn` colormap object reused across those modes. Its
    /// name/normalization/gamma persist across mode and data changes. Its range
    /// follows silx `getColormapRange` per bound: an *auto* bound (the default
    /// `Colormap::autoscale`, silx `vmin`/`vmax = None`) re-derives from each
    /// scalar image, while a bound the user pinned via [`Self::set_colormap`] is
    /// kept. [`ComplexMode::Phase`] ignores this and uses the fixed hsv phase
    /// colormap.
    colormap: Colormap,
    /// Displayed max amplitude for [`ComplexMode::Log10AmplitudePhase`] (silx
    /// `smax`): `None` autoscales to the data max.
    max_amplitude: Option<f32>,
    /// Displayed range in log10 units for [`ComplexMode::Log10AmplitudePhase`]
    /// (silx `dlogs`/`delta`, default [`DEFAULT_AMPLITUDE_DELTA`]).
    delta: f32,
    image_handle: Option<ItemHandle>,
    dirty: bool,
}

impl ComplexImageView {
    /// Create a complex image view backed by wgpu plot id `id`.
    ///
    /// The default mode is [`ComplexMode::Absolute`], matching silx's
    /// `ImageComplexData` default.
    pub fn new(render_state: &RenderState, id: PlotId) -> Self {
        Self {
            plot: Plot2D::new(render_state, id),
            width: 0,
            height: 0,
            data: Vec::new(),
            mode: ComplexMode::Absolute,
            // silx's scalar-mode default colormap is the plot default: gray
            // with autoscale (vmin/vmax = None), re-ranged per image.
            colormap: Colormap::autoscale(ColormapName::Gray),
            // silx default amplitude range: autoscale max, 2 log10 decades.
            max_amplitude: None,
            delta: DEFAULT_AMPLITUDE_DELTA,
            image_handle: None,
            dirty: false,
        }
    }

    /// Set the complex data to display.
    ///
    /// `data` is a row-major array of `(re, im)` pairs of length
    /// `width * height`. Returns [`PlotDataError`] on a length mismatch.
    pub fn set_data(
        &mut self,
        width: u32,
        height: u32,
        data: &[(f32, f32)],
    ) -> Result<(), PlotDataError> {
        let expected = (width as usize).saturating_mul(height as usize);
        if data.len() != expected {
            return Err(PlotDataError::ImageDataLength {
                expected,
                actual: data.len(),
            });
        }
        self.width = width;
        self.height = height;
        self.data = data.to_vec();
        self.dirty = true;
        Ok(())
    }

    /// The current visualization mode.
    pub fn mode(&self) -> ComplexMode {
        self.mode
    }

    /// Set the visualization mode, recomputing the displayed image on the next
    /// [`Self::show`] without resetting the zoom.
    pub fn set_mode(&mut self, mode: ComplexMode) {
        if mode != self.mode {
            self.mode = mode;
            self.dirty = true;
        }
    }

    /// Set the displayed amplitude range for
    /// [`ComplexMode::Log10AmplitudePhase`] (silx
    /// `ImageComplexData._setAmplitudeRangeInfo`).
    ///
    /// `max_amplitude` is silx `smax` (the dialog's "Displayed Max."): `None`
    /// autoscales to the data's maximum amplitude. `delta` is silx `dlogs` (the
    /// dialog's "Displayed delta (log10 unit)", `>= 1`). Recomputes the image on
    /// the next [`Self::show`] when in the log composite mode.
    pub fn set_amplitude_range_info(&mut self, max_amplitude: Option<f32>, delta: f32) {
        if self.max_amplitude != max_amplitude || self.delta != delta {
            self.max_amplitude = max_amplitude;
            self.delta = delta;
            // Only the log composite uses this range; flagging dirty is harmless
            // for other modes (the recomputed image is identical) but avoids a
            // mode check here.
            self.dirty = true;
        }
    }

    /// The displayed amplitude range `(max, delta)` for
    /// [`ComplexMode::Log10AmplitudePhase`] (silx
    /// `ImageComplexData._getAmplitudeRangeInfo`); `max` is `None` when
    /// autoscaling to the data max.
    pub fn amplitude_range_info(&self) -> (Option<f32>, f32) {
        (self.max_amplitude, self.delta)
    }

    /// Access the underlying [`Plot2D`].
    pub fn plot(&self) -> &Plot2D {
        &self.plot
    }

    /// Mutably access the underlying [`Plot2D`].
    pub fn plot_mut(&mut self) -> &mut Plot2D {
        &mut self.plot
    }

    /// Render the complex image in `ui`, rebuilding the displayed image first
    /// if the data or mode changed.
    pub fn show(&mut self, ui: &mut egui::Ui) -> PlotResponse {
        if self.dirty && !self.data.is_empty() {
            self.rebuild_image();
            self.dirty = false;
        }
        self.plot.show(ui)
    }

    /// A combo box to pick the visualization mode. Returns the current mode.
    ///
    /// Call before [`Self::show`].
    pub fn show_mode_controls(&mut self, ui: &mut egui::Ui) -> ComplexMode {
        egui::ComboBox::from_label("Complex mode")
            .selected_text(self.mode.label())
            .show_ui(ui, |ui| {
                for mode in ComplexMode::ALL {
                    if ui
                        .selectable_label(self.mode == mode, mode.label())
                        .clicked()
                        && self.mode != mode
                    {
                        self.mode = mode;
                        self.dirty = true;
                    }
                }
            });
        self.mode
    }

    /// A horizontal toolbar of selectable mode buttons (one per
    /// [`ComplexMode`], in silx menu order), mirroring silx's
    /// `_ComplexDataToolButton` mode menu more closely than the combo. Clicking
    /// a button activates that mode and recomputes the image on the next
    /// [`Self::show`]. Returns the current mode.
    ///
    /// Call before [`Self::show`].
    pub fn show_mode_toolbar(&mut self, ui: &mut egui::Ui) -> ComplexMode {
        if let Some(picked) = mode_toolbar_ui(ui, self.mode) {
            self.set_mode(picked);
        }
        self.mode
    }

    /// Inline controls for the displayed amplitude range used by
    /// [`ComplexMode::Log10AmplitudePhase`], mirroring silx
    /// `_AmplitudeRangeDialog` (`ComplexImageView.py:50-155`): an "autoscale"
    /// checkbox (max = `None`), a "Displayed Max." field enabled only when not
    /// autoscaling (silx validator bottom `0.0`), and a "Displayed delta (log10
    /// unit)" field clamped to `>= 1` (silx validator bottom `1.0`). Edits route
    /// through [`Self::set_amplitude_range_info`] so the composite recomputes on
    /// the next [`Self::show`]. Most useful in the log composite mode but
    /// harmless in others (the recomputed image is identical).
    ///
    /// Call before [`Self::show`].
    pub fn show_amplitude_range_controls(&mut self, ui: &mut egui::Ui) {
        ui.horizontal(|ui| {
            let mut autoscale = self.max_amplitude.is_none();
            if ui
                .checkbox(&mut autoscale, "autoscale")
                .on_hover_text("Autoscale the displayed max to the data's max amplitude")
                .changed()
            {
                // Leaving autoscale seeds the max from the data; entering it
                // clears the max to None (silx `_autoscaleCheckBoxToggled`).
                let max = (!autoscale).then(|| data_max_amplitude(&self.data));
                self.set_amplitude_range_info(max, self.delta);
            }

            ui.label("Displayed Max.:");
            let mut max_val = self
                .max_amplitude
                .unwrap_or_else(|| data_max_amplitude(&self.data));
            if ui
                .add_enabled(!autoscale, egui::DragValue::new(&mut max_val).speed(0.1))
                .changed()
                && !autoscale
            {
                self.set_amplitude_range_info(Some(max_val.max(0.0)), self.delta);
            }

            ui.label("Displayed delta (log10 unit):");
            let mut delta = self.delta;
            if ui
                .add(egui::DragValue::new(&mut delta).speed(0.1))
                .changed()
            {
                self.set_amplitude_range_info(self.max_amplitude, delta.max(1.0));
            }
        });
    }

    /// Recompute the displayed image for the current mode and update the plot
    /// in place (reusing the existing item handle so the zoom is preserved).
    fn rebuild_image(&mut self) {
        if self.mode.is_rgba() {
            let rgba = match self.mode {
                ComplexMode::Log10AmplitudePhase => {
                    amplitude_phase_log_rgba(&self.data, self.max_amplitude, self.delta)
                }
                _ => amplitude_phase_rgba(&self.data),
            };
            self.set_rgba_image(&rgba);
        } else {
            let scalar: Vec<f32> = self
                .data
                .iter()
                .map(|&(re, im)| self.mode.to_scalar(re, im))
                .collect();
            let colormap = self.scalar_colormap(&scalar);
            self.set_scalar_image(&scalar, colormap);
        }
    }

    /// The colormap for the current scalar mode against `scalar` — delegates to
    /// the pure [`scalar_mode_colormap`] with this view's mode and persistent
    /// base colormap.
    fn scalar_colormap(&self, scalar: &[f32]) -> Colormap {
        scalar_mode_colormap(self.mode, &self.colormap, scalar)
    }

    /// The base colormap shared by the non-phase scalar modes (silx
    /// `getColormap`).
    pub fn colormap(&self) -> &Colormap {
        &self.colormap
    }

    /// Set the base colormap for the non-phase scalar modes (silx
    /// `setColormap`); the setting persists across mode switches. The
    /// name / normalization / gamma / NaN color are always honored. Each *auto*
    /// bound (silx `vmin`/`vmax = None`) re-derives from each scalar image; a
    /// bound pinned on `colormap` is kept across data and mode changes (silx
    /// `getColormapRange`). Pass a [`Colormap::autoscale`] for the silx-default
    /// tracking behavior, or a pinned `Colormap::new`/`viridis` to fix the
    /// range. [`ComplexMode::Phase`] is unaffected (it uses the fixed hsv phase
    /// colormap).
    pub fn set_colormap(&mut self, colormap: Colormap) {
        self.colormap = colormap;
        self.dirty = true;
    }

    /// Upload or replace the scalar image, preserving the zoom on update.
    fn set_scalar_image(&mut self, scalar: &[f32], colormap: Colormap) {
        if let Some(handle) = self.image_handle {
            let spec = ImageSpec::scalar(self.width, self.height, scalar, colormap.clone());
            if self.plot.update_image_spec(handle, spec) {
                return;
            }
        }
        let handle = self
            .plot
            .try_add_image(self.width, self.height, scalar, colormap)
            .expect("scalar length validated by set_data");
        self.image_handle = Some(handle);
    }

    /// Upload or replace the RGBA composite image, preserving the zoom on
    /// update.
    fn set_rgba_image(&mut self, rgba: &[[u8; 4]]) {
        if let Some(handle) = self.image_handle {
            let spec = ImageSpec::rgba(self.width, self.height, rgba);
            if self.plot.update_image_spec(handle, spec) {
                return;
            }
        }
        let handle = self
            .plot
            .try_add_rgba_image(self.width, self.height, rgba)
            .expect("rgba length validated by set_data");
        self.image_handle = Some(handle);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const PI: f32 = std::f32::consts::PI;

    // ── Mode toolbar selection (Item 3) ─────────────────────────────────────

    /// Capture the on-screen rect of each toolbar button by running one
    /// headless layout frame with the same widget sequence `mode_toolbar_ui`
    /// emits. Geometry is deterministic, so the rects line up with the real
    /// `mode_toolbar_ui` call on an identically-sized frame.
    fn capture_button_rects(
        ctx: &egui::Context,
        current: ComplexMode,
    ) -> Vec<(ComplexMode, egui::Rect)> {
        let mut rects = Vec::new();
        let _ = ctx.run_ui(egui::RawInput::default(), |ui| {
            ui.horizontal(|ui| {
                for mode in ComplexMode::ALL {
                    let r = ui.selectable_label(current == mode, mode.label());
                    rects.push((mode, r.rect));
                }
            });
        });
        rects
    }

    /// Run one headless frame of the real `mode_toolbar_ui` with `raw` input,
    /// returning the mode it reports as picked (if any).
    fn run_toolbar(
        ctx: &egui::Context,
        current: ComplexMode,
        raw: egui::RawInput,
    ) -> Option<ComplexMode> {
        let mut picked = None;
        let _ = ctx.run_ui(raw, |ui| {
            picked = mode_toolbar_ui(ui, current);
        });
        picked
    }

    fn click_at(point: egui::Pos2) -> egui::RawInput {
        egui::RawInput {
            events: vec![
                egui::Event::PointerMoved(point),
                egui::Event::PointerButton {
                    pos: point,
                    button: egui::PointerButton::Primary,
                    pressed: true,
                    modifiers: egui::Modifiers::default(),
                },
                egui::Event::PointerButton {
                    pos: point,
                    button: egui::PointerButton::Primary,
                    pressed: false,
                    modifiers: egui::Modifiers::default(),
                },
            ],
            ..Default::default()
        }
    }

    #[test]
    fn mode_toolbar_returns_none_without_a_click() {
        let ctx = egui::Context::default();
        // Layout frame, then an empty frame: no pointer input -> no selection.
        let _ = run_toolbar(&ctx, ComplexMode::Absolute, egui::RawInput::default());
        assert_eq!(
            run_toolbar(&ctx, ComplexMode::Absolute, egui::RawInput::default()),
            None
        );
    }

    #[test]
    fn mode_toolbar_click_selects_that_mode() {
        let ctx = egui::Context::default();
        // Current is Phase, so clicking the (non-active) Real button selects it.
        let current = ComplexMode::Phase;
        let rects = capture_button_rects(&ctx, current);
        let (_, real_rect) = rects
            .iter()
            .find(|(m, _)| *m == ComplexMode::Real)
            .copied()
            .expect("Real button present");

        // Frame 1: lay the toolbar out so widget ids/rects are registered.
        let _ = run_toolbar(&ctx, current, egui::RawInput::default());
        // Frame 2: click the captured Real-button center.
        let picked = run_toolbar(&ctx, current, click_at(real_rect.center()));
        assert_eq!(picked, Some(ComplexMode::Real));
    }

    #[test]
    fn mode_toolbar_click_on_active_mode_is_noop() {
        let ctx = egui::Context::default();
        let current = ComplexMode::Absolute;
        let rects = capture_button_rects(&ctx, current);
        let (_, active_rect) = rects
            .iter()
            .find(|(m, _)| *m == ComplexMode::Absolute)
            .copied()
            .expect("Absolute button present");

        let _ = run_toolbar(&ctx, current, egui::RawInput::default());
        // Clicking the already-active button reports no change.
        let picked = run_toolbar(&ctx, current, click_at(active_rect.center()));
        assert_eq!(picked, None);
    }

    // ── Per-mode scalar conversion at known complex values ──────────────────

    #[test]
    fn absolute_is_hypot() {
        assert_eq!(ComplexMode::Absolute.to_scalar(3.0, 4.0), 5.0);
        assert_eq!(ComplexMode::Absolute.to_scalar(0.0, 0.0), 0.0);
    }

    #[test]
    fn phase_is_atan2() {
        // atan2(im, re): real axis -> 0, +imag axis -> +pi/2, -real -> +pi.
        assert_eq!(ComplexMode::Phase.to_scalar(1.0, 0.0), 0.0);
        assert!((ComplexMode::Phase.to_scalar(0.0, 1.0) - PI / 2.0).abs() < 1e-6);
        assert!((ComplexMode::Phase.to_scalar(-1.0, 0.0) - PI).abs() < 1e-6);
        assert!((ComplexMode::Phase.to_scalar(0.0, -1.0) + PI / 2.0).abs() < 1e-6);
    }

    #[test]
    fn real_is_re() {
        assert_eq!(ComplexMode::Real.to_scalar(3.0, 4.0), 3.0);
        assert_eq!(ComplexMode::Real.to_scalar(-2.5, 9.0), -2.5);
    }

    #[test]
    fn imaginary_is_im() {
        assert_eq!(ComplexMode::Imaginary.to_scalar(3.0, 4.0), 4.0);
        assert_eq!(ComplexMode::Imaginary.to_scalar(-2.5, -9.0), -9.0);
    }

    #[test]
    fn square_amplitude_is_re2_plus_im2() {
        assert_eq!(ComplexMode::SquareAmplitude.to_scalar(3.0, 4.0), 25.0);
        assert_eq!(ComplexMode::SquareAmplitude.to_scalar(0.0, 0.0), 0.0);
    }

    #[test]
    fn log10_amplitude_is_log10_of_hypot() {
        // |z| = 100 -> log10 = 2; |z| = 1 -> 0.
        assert!((ComplexMode::Log10Amplitude.to_scalar(100.0, 0.0) - 2.0).abs() < 1e-6);
        assert_eq!(ComplexMode::Log10Amplitude.to_scalar(1.0, 0.0), 0.0);
    }

    #[test]
    fn amplitude_phase_returns_zero_scalar() {
        // No scalar representation; the RGBA path is used instead.
        assert_eq!(ComplexMode::AmplitudePhase.to_scalar(3.0, 4.0), 0.0);
    }

    // ── AMPLITUDE_PHASE HSV mapping at boundary phases ──────────────────────

    #[test]
    fn amplitude_phase_hue_at_zero_phase_is_cyan() {
        // phase 0 -> hue 0.5 (mid of [-pi,pi]->[0,1]) -> cyan; |z| = max -> v = 1.
        // Use a single sample so it is the max amplitude (value = 1).
        let rgba = amplitude_phase_rgba(&[(1.0, 0.0)]);
        // hue = (0 + pi)/(2pi) = 0.5 -> hsv(0.5,1,1) = cyan (0,255,255).
        assert_eq!(rgba, vec![[0, 255, 255, 255]]);
    }

    #[test]
    fn amplitude_phase_hue_at_plus_half_pi() {
        // phase +pi/2 -> hue (pi/2 + pi)/(2pi) = 0.75 -> hsv(0.75,1,1):
        // h*6 = 4.5 (sector 4, f = 0.5) -> (t, p, v) = (0.5, 0, 1) -> violet.
        let rgba = amplitude_phase_rgba(&[(0.0, 1.0)]);
        assert_eq!(rgba, vec![[128, 0, 255, 255]]);
    }

    #[test]
    fn amplitude_phase_hue_at_minus_pi() {
        // phase -pi -> hue 0.0 -> hsv(0,1,1) = red (255,0,0).
        let rgba = amplitude_phase_rgba(&[(-1.0, 0.0)]);
        // atan2(0, -1) = +pi in IEEE, but -0.0 imaginary gives -pi; use exact -pi sample.
        let rgba_neg = amplitude_phase_rgba(&[(-1.0, -0.0)]);
        // +pi maps to hue 1.0 == 0.0 (red) and -pi maps to hue 0.0 (red): both red.
        assert_eq!(rgba, vec![[255, 0, 0, 255]]);
        assert_eq!(rgba_neg, vec![[255, 0, 0, 255]]);
    }

    #[test]
    fn amplitude_phase_value_scales_with_amplitude() {
        // Two samples: max amplitude 2 at phase 0 (cyan, v=1); half amplitude 1
        // at phase 0 (v=0.5 -> half-bright cyan).
        let rgba = amplitude_phase_rgba(&[(2.0, 0.0), (1.0, 0.0)]);
        assert_eq!(rgba[0], [0, 255, 255, 255]);
        // v = 0.5: hsv(0.5, 1, 0.5) -> (0, 128, 128) after rounding.
        assert_eq!(rgba[1], [0, 128, 128, 255]);
    }

    #[test]
    fn amplitude_phase_empty_is_empty() {
        assert!(amplitude_phase_rgba(&[]).is_empty());
    }

    #[test]
    fn amplitude_phase_zero_amplitude_is_black() {
        // max_amp == 0 -> value 0 everywhere -> black.
        let rgba = amplitude_phase_rgba(&[(0.0, 0.0), (0.0, 0.0)]);
        assert_eq!(rgba, vec![[0, 0, 0, 255], [0, 0, 0, 255]]);
    }

    // ── LOG10_AMPLITUDE_PHASE composite (silx _complex2rgbalog) ─────────────

    #[test]
    fn log10_amplitude_phase_mode_is_rgba_with_no_scalar() {
        assert!(ComplexMode::Log10AmplitudePhase.is_rgba());
        assert_eq!(ComplexMode::Log10AmplitudePhase.to_scalar(3.0, 4.0), 0.0);
        assert!(ComplexMode::ALL.contains(&ComplexMode::Log10AmplitudePhase));
        assert_eq!(
            ComplexMode::Log10AmplitudePhase.label(),
            "Log10 Amplitude and Phase"
        );
    }

    #[test]
    fn log_composite_empty_is_empty() {
        assert!(amplitude_phase_log_rgba(&[], None, DEFAULT_AMPLITUDE_DELTA).is_empty());
    }

    #[test]
    fn log_composite_normalizes_over_delta_decades() {
        // delta = 2 decades. Max amplitude 100 (phase 0) -> value 1 (full cyan);
        // amplitude 10 is one decade below the max -> a = 1, value = 1/2 ->
        // half-bright cyan. Autoscale (max = None).
        let rgba = amplitude_phase_log_rgba(&[(100.0, 0.0), (10.0, 0.0)], None, 2.0);
        assert_eq!(rgba[0], [0, 255, 255, 255]);
        assert_eq!(rgba[1], [0, 128, 128, 255]);
    }

    #[test]
    fn log_composite_floor_below_window_is_zero() {
        // amplitude 1 is two decades below max 100 with delta = 2 -> a = 0 ->
        // value clamped to 0 -> black.
        let rgba = amplitude_phase_log_rgba(&[(100.0, 0.0), (1.0, 0.0)], None, 2.0);
        assert_eq!(rgba[0], [0, 255, 255, 255]);
        assert_eq!(rgba[1], [0, 0, 0, 255]);
    }

    #[test]
    fn log_composite_clamps_to_displayed_max() {
        // Without clamping, amplitude 10 sits two decades below the 1000 max
        // (delta 2) and floors to 0. Clamping the displayed max to 100 lifts the
        // reference to 100, so amplitude 10 is only one decade below -> value 0.5.
        let data = [(1000.0, 0.0), (10.0, 0.0)];
        let uncapped = amplitude_phase_log_rgba(&data, None, 2.0);
        assert_eq!(uncapped[1], [0, 0, 0, 255]);
        let capped = amplitude_phase_log_rgba(&data, Some(100.0), 2.0);
        assert_eq!(capped[0], [0, 255, 255, 255]); // 1000 saturates to 100
        assert_eq!(capped[1], [0, 128, 128, 255]); // 10 -> one decade below 100
    }

    #[test]
    fn log_composite_uniform_amplitude_is_full_value() {
        // silx degenerate case: every amplitude equal -> a.max() == every a ->
        // value 1 for all pixels.
        let rgba = amplitude_phase_log_rgba(&[(5.0, 0.0), (5.0, 0.0)], None, 2.0);
        assert_eq!(rgba, vec![[0, 255, 255, 255], [0, 255, 255, 255]]);
    }

    #[test]
    fn log_composite_zero_amplitude_matches_silx_degenerate() {
        // All zeros: log10(0 + 1e-20) is uniform, so the silx shift maps every
        // pixel to full value (matching _complex2rgbalog alpha == 255).
        let rgba = amplitude_phase_log_rgba(&[(0.0, 0.0), (0.0, 0.0)], None, 2.0);
        assert_eq!(rgba, vec![[0, 255, 255, 255], [0, 255, 255, 255]]);
    }

    // ── hsv LUT / phase colormap ────────────────────────────────────────────

    #[test]
    fn hsv_lut_endpoints_are_red() {
        // Full hue sweep is cyclic: index 0 and the wrapped end are both red.
        let lut = phase_hsv_lut();
        assert_eq!(lut[0], [255, 0, 0, 255]);
        assert_eq!(hsv_to_rgb(1.0, 1.0, 1.0), [255, 0, 0]);
    }

    #[test]
    fn phase_colormap_range_is_minus_pi_to_pi() {
        let cm = phase_colormap();
        assert_eq!(cm.vmin, -std::f64::consts::PI);
        assert_eq!(cm.vmax, std::f64::consts::PI);
    }

    #[test]
    fn scalar_mode_default_colormap_tracks_the_image() {
        // The persistent default (gray autoscale) re-ranges to each scalar
        // image (silx ColormapMixIn vmin/vmax = None).
        let base = Colormap::autoscale(ColormapName::Gray);
        let cm = scalar_mode_colormap(ComplexMode::Absolute, &base, &[2.0, 8.0, 5.0]);
        assert_eq!((cm.vmin, cm.vmax), (2.0, 8.0));
        assert!(cm.is_autoscale(), "flags preserved for the next rebuild");
    }

    #[test]
    fn scalar_mode_pinned_colormap_survives_data_change() {
        // A user who pins vmin/vmax via set_colormap keeps that range across
        // data changes — silx getColormapRange fills only the None bounds.
        let base = Colormap::new(ColormapName::Viridis, 0.0, 100.0);
        let cm = scalar_mode_colormap(ComplexMode::Real, &base, &[3.0, 7.0]);
        assert_eq!((cm.vmin, cm.vmax), (0.0, 100.0));
        // A different image does not move the pinned range either.
        let cm2 = scalar_mode_colormap(ComplexMode::Real, &base, &[-50.0, 900.0]);
        assert_eq!((cm2.vmin, cm2.vmax), (0.0, 100.0));
    }

    #[test]
    fn scalar_mode_phase_ignores_the_base_colormap() {
        // Phase always uses the fixed [-pi, pi] hsv colormap regardless of base.
        let base = Colormap::new(ColormapName::Viridis, 0.0, 1.0);
        let cm = scalar_mode_colormap(ComplexMode::Phase, &base, &[100.0, 200.0]);
        assert_eq!(cm.vmin, -std::f64::consts::PI);
        assert_eq!(cm.vmax, std::f64::consts::PI);
        assert_eq!(cm.lut, phase_colormap().lut);
    }

    #[test]
    fn data_max_amplitude_is_finite_max_modulus() {
        // The seeded "Displayed Max." is the max finite |z| over the data.
        assert_eq!(
            data_max_amplitude(&[(3.0, 4.0), (0.0, 0.0), (1.0, 1.0)]),
            5.0
        );
        // Empty data -> 0.0.
        assert_eq!(data_max_amplitude(&[]), 0.0);
        // NaN / inf amplitudes are skipped; the finite max wins.
        assert_eq!(
            data_max_amplitude(&[(f32::NAN, 0.0), (f32::INFINITY, 0.0), (6.0, 8.0)]),
            10.0
        );
    }
}
