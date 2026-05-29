//! egui-silx — silx-style scientific plotting on top of egui + wgpu.
//!
//! See `doc/design.md` for the design. The layering mirrors silx's
//! `BackendBase ↔ BackendPygfx`:
//!
//! - [`core`]   — the `Plot` model + `Backend` trait + shared types (Transform/Colormap …)
//! - [`render`] — the wgpu renderer (`egui_wgpu::CallbackTrait` impl), owns GPU resources
//! - [`widget`] — the egui widget ([`PlotWidget`]): chrome + interaction + paint-callback registration
//!
//! Current scope: vertical slice 1, steps 1–5 — a wgpu data layer (clear,
//! colormapped image, polyline curve) under egui chrome (frame, grid, ticks,
//! tick labels, colorbar), all sharing one coordinate transform.

pub mod core;
pub mod render;
pub mod widget;

pub use crate::core::colormap::{Colormap, ColormapName};
pub use crate::core::plot::{Plot, PlotId};
pub use crate::core::transform::{Axis, Margins, Scale, Transform, YAxis};
pub use crate::render::backend_wgpu::{
    install, set_curve, set_curves, set_image, update_curve, update_curve_at, update_image_region,
};
pub use crate::render::gpu_curve::CurveData;
pub use crate::render::gpu_image::ImageData;
pub use crate::widget::plot_widget::PlotWidget;

// Plotting-library convention: re-export so downstreams use the same
// egui/egui-wgpu without version skew.
pub use egui;
pub use egui_wgpu;
