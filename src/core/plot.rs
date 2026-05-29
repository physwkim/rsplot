//! The plot model.
//!
//! Holds the identifier, data-area background, data limits, margins, and the
//! optional colormap used to draw the colorbar. The item list, log/inverted
//! axis flags, and dirty tracking are added in later steps
//! (`doc/design.md` §1·§4·§11).

use egui::{Color32, Rect};

use crate::core::colormap::Colormap;
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
}
