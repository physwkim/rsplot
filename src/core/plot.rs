//! The plot model.
//!
//! Holds the identifier, data-area background, data limits, margins, and the
//! optional colormap used to draw the colorbar. The item list, log/inverted
//! axis flags, and dirty tracking are added in later steps
//! (`doc/design.md` §1·§4·§11).

use egui::{Color32, Rect};

use crate::core::colormap::Colormap;
use crate::core::roi::Roi;
use crate::core::transform::{Axis, Margins, Scale, Transform, keep_aspect_limits};

/// Identifier for a single `Plot` instance.
///
/// `egui_wgpu`'s `callback_resources` is a global type map, so multi-plot keeps
/// per-plot GPU state separated by `PlotId` (`doc/design.md` §3.1·§12). The
/// current steps handle a single plot, so no separation map exists yet.
pub type PlotId = u64;

/// One plot.
pub struct Plot {
    /// Instance identifier.
    pub id: PlotId,
    /// Data-area background color (maps to silx `setBackgroundColors`' data background).
    pub data_background: Color32,
    /// Data-space limits `(x_min, x_max, y_min, y_max)`.
    pub limits: (f64, f64, f64, f64),
    /// Margins reserving extra space inside the chrome gutters. Zero by default.
    pub margins: Margins,
    /// Colormap drawn as the colorbar (mirrors the displayed image's colormap).
    /// `None` hides the colorbar (`doc/design.md` §5·§8).
    pub colormap: Option<Colormap>,
    /// Limits to restore on a double-click "reset". The widget captures the
    /// first observed `limits` here so the home view survives pan/zoom
    /// (`doc/design.md` §8·§11.6). `None` until the first frame.
    pub home_limits: Option<(f64, f64, f64, f64)>,
    /// X-axis scale (linear or log10) (`doc/design.md` §13 A3).
    pub x_scale: Scale,
    /// Y-axis scale (linear or log10).
    pub y_scale: Scale,
    /// Reverse the X-axis on-screen direction (`doc/design.md` §13 A2).
    pub x_inverted: bool,
    /// Reverse the Y-axis on-screen direction.
    pub y_inverted: bool,
    /// Keep data square on screen by expanding the tighter axis' display range
    /// (silx `setKeepDataAspectRatio`). Only honored when both axes are linear
    /// (`doc/design.md` §13 A4).
    pub keep_aspect: bool,
    /// Secondary right Y axis limits `(y2_min, y2_max)`, or `None` for no y2
    /// axis. Curves bound to [`crate::YAxis::Right`] are plotted against it and
    /// its ticks are drawn in the right gutter (linear, `doc/design.md` §13 A5).
    pub y2: Option<(f64, f64)>,
    /// Draw a crosshair + coordinate readout following the pointer when it is
    /// over the data area (silx `setGraphCursor`, `doc/design.md` §13 C1).
    pub crosshair: bool,
    /// Regions of interest drawn over the data area with draggable edge
    /// handles. Dragging an edge updates that ROI's bounds in place and the
    /// widget reports the changed index (`doc/design.md` §13 C3).
    pub rois: Vec<Roi>,
    /// Graph title, drawn centered above the data area (silx `setGraphTitle`,
    /// `BackendBase.setGraphTitle`). `None` reserves no top space for it.
    pub title: Option<String>,
    /// X-axis label, drawn centered below the X tick labels (silx
    /// `setGraphXLabel`). `None` reserves no extra bottom space.
    pub x_label: Option<String>,
    /// Left Y-axis label, drawn rotated at the far left (silx `setGraphYLabel`).
    /// `None` reserves no extra left space.
    pub y_label: Option<String>,
    /// Right (y2) Y-axis label, drawn rotated at the far right; only shown when
    /// a [`Self::y2`] axis exists. `None` reserves no extra right space.
    pub y2_label: Option<String>,
    /// Foreground color override for axes/frame/ticks/labels (silx
    /// `setForegroundColor`). `None` follows the egui theme's text color.
    pub foreground: Option<Color32>,
    /// Grid-line color override (silx `setGridColor`). `None` uses a faint tint
    /// of the foreground color.
    pub grid_color: Option<Color32>,
}

impl Plot {
    /// Create a plot with the given id, a default dark background, unit limits,
    /// no margins, and no colorbar.
    pub fn new(id: PlotId) -> Self {
        Self {
            id,
            data_background: Color32::from_rgb(16, 16, 24),
            limits: (0.0, 1.0, 0.0, 1.0),
            margins: Margins::ZERO,
            colormap: None,
            home_limits: None,
            x_scale: Scale::Linear,
            y_scale: Scale::Linear,
            x_inverted: false,
            y_inverted: false,
            keep_aspect: false,
            y2: None,
            crosshair: false,
            rois: Vec::new(),
            title: None,
            x_label: None,
            y_label: None,
            y2_label: None,
            foreground: None,
            grid_color: None,
        }
    }

    /// Build the data↔screen transform for the given data-area rect, honoring
    /// the per-axis scale, inversion, and (linear-only) aspect-ratio lock.
    ///
    /// Aspect correction is derived here from the stable requested `limits`, so
    /// it is the same view used for rendering, chrome, and pointer mapping —
    /// and resizing never compounds the expansion (`doc/design.md` §13 A4).
    pub fn transform(&self, area: Rect) -> Transform {
        let linear = self.x_scale == Scale::Linear && self.y_scale == Scale::Linear;
        let (x_min, x_max, y_min, y_max) = if self.keep_aspect && linear {
            keep_aspect_limits(self.limits, area)
        } else {
            self.limits
        };
        let x = Axis {
            min: x_min,
            max: x_max,
            scale: self.x_scale,
            inverted: self.x_inverted,
        };
        let y = Axis {
            min: y_min,
            max: y_max,
            scale: self.y_scale,
            inverted: self.y_inverted,
        };
        Transform::with_axes(x, y, area)
    }

    /// Build the transform for the secondary right (y2) axis, sharing the left
    /// transform's X axis exactly (including any aspect expansion) so curves on
    /// both axes stay aligned in X. `None` when the plot has no y2 axis. The y2
    /// axis is linear, non-inverted (`doc/design.md` §13 A5).
    pub fn transform_y2(&self, area: Rect) -> Option<Transform> {
        let (y2_min, y2_max) = self.y2?;
        let left = self.transform(area);
        let y2 = Axis::linear(y2_min, y2_max);
        Some(Transform::with_axes(left.x, y2, area))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use egui::pos2;

    fn area() -> Rect {
        Rect::from_min_max(pos2(0.0, 0.0), pos2(200.0, 100.0))
    }

    #[test]
    fn transform_y2_is_none_without_y2_axis() {
        let plot = Plot::new(0);
        assert!(plot.transform_y2(area()).is_none());
    }

    #[test]
    fn transform_y2_shares_left_x_and_maps_its_own_y() {
        let mut plot = Plot::new(0);
        plot.limits = (0.0, 10.0, 0.0, 100.0);
        plot.y2 = Some((-1.0, 1.0));
        let left = plot.transform(area());
        let right = plot.transform_y2(area()).expect("y2 transform");

        // X axis is shared exactly, so curves on both axes align in X.
        assert_eq!(left.x, right.x);
        // The right axis maps its own y2 range: y2_min at the bottom edge, y2_max
        // at the top edge of the same area.
        let bottom = right.data_to_pixel(0.0, -1.0).y;
        let top = right.data_to_pixel(0.0, 1.0).y;
        assert!((bottom - area().bottom()).abs() <= 1e-3, "{bottom}");
        assert!((top - area().top()).abs() <= 1e-3, "{top}");
    }

    #[test]
    fn transform_y2_shares_aspect_expanded_x() {
        // With the aspect lock on, the left transform's X is expanded; the y2
        // transform must inherit that same expanded X (not the raw limits).
        let mut plot = Plot::new(0);
        plot.limits = (0.0, 10.0, 0.0, 10.0);
        plot.keep_aspect = true;
        plot.y2 = Some((0.0, 5.0));
        let left = plot.transform(area());
        let right = plot.transform_y2(area()).expect("y2 transform");
        assert_eq!(left.x, right.x);
        // Sanity: the lock actually widened X beyond the raw [0, 10].
        assert!(left.x.min < 0.0 && left.x.max > 10.0, "{:?}", left.x);
    }
}
