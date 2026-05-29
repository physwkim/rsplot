//! The plot widget.
//!
//! Lays out the chrome gutters, applies pointer interaction to the plot limits,
//! clears the data area, draws the image and curve via wgpu paint callbacks,
//! then draws the axes and (optional) colorbar with egui's painter. The wgpu
//! layer and the chrome share one [`crate::core::transform::Transform`] derived
//! from the (possibly just-updated) limits, so they stay aligned while panning
//! and zooming (`doc/design.md` §4·§8·§11.6).
//!
//! Mouse mapping (silx default): left-drag = box zoom, right-drag = pan,
//! wheel = cursor-anchored zoom, double-click = reset.

use egui::{Color32, PointerButton, Pos2, Rect, Sense, Stroke, Ui};

use crate::core::plot::Plot;
use crate::core::roi::RoiEdge;
use crate::core::transform::{Scale, Transform};

/// Pixel radius for grabbing an ROI edge handle.
const ROI_GRAB_PX: f32 = 6.0;

/// An in-progress ROI edge drag, stashed in egui temp memory across frames.
#[derive(Clone, Copy)]
struct RoiDrag {
    roi: usize,
    edge: RoiEdge,
}

/// What `apply_interaction` produced this frame.
struct Interaction {
    /// In-progress box-zoom selection rectangle (screen space).
    selection: Option<egui::Rect>,
    /// Index of the ROI whose bounds an edge drag changed this frame.
    roi_changed: Option<usize>,
}
use crate::render::backend_wgpu::{ClearCallback, CurveCallback, ImageCallback};
use crate::widget::{chrome, interaction};

/// What [`PlotWidget::show`] returns: the egui [`Response`](egui::Response) plus
/// the display [`Transform`] used this frame. The transform lets callers map
/// pointer pixels to data coordinates and run picking
/// ([`interaction::nearest_point`](crate::nearest_point) /
/// [`image_index`](crate::image_index)) against their own data
/// (`doc/design.md` §13 C2).
pub struct PlotResponse {
    pub response: egui::Response,
    pub transform: Transform,
    /// Index into `Plot::rois` of the region whose bounds changed this frame
    /// from an edge drag, or `None` (`doc/design.md` §13 C3).
    pub roi_changed: Option<usize>,
}

/// Widget that renders a [`Plot`] into an egui `Ui`.
#[derive(Default)]
pub struct PlotWidget;

impl PlotWidget {
    /// Create a new plot widget.
    pub fn new() -> Self {
        Self
    }

    /// Render the plot, filling the available space, and handle interaction.
    /// Returns the egui response and the display transform used this frame.
    pub fn show(self, ui: &mut Ui, plot: &mut Plot) -> PlotResponse {
        let (rect, response) = ui.allocate_exact_size(ui.available_size(), Sense::click_and_drag());

        // Capture the initial view once, for double-click reset.
        let current = plot.limits;
        plot.home_limits.get_or_insert(current);

        // Chrome gutters depend only on which axes/colorbar/labels show, not on
        // limits.
        let with_colorbar = plot.colormap.is_some();
        let with_y2 = plot.y2.is_some();
        let chrome_request = chrome::ChromeRequest {
            colorbar: with_colorbar,
            y2: with_y2,
            title: plot.title.is_some(),
            x_label: plot.x_label.is_some(),
            y_label: plot.y_label.is_some(),
            y2_label: plot.y2_label.is_some(),
        };
        let chrome_layout = chrome::layout(rect, &chrome_request);
        let area = plot.margins.data_area(chrome_layout.data_area);

        // Map input through the transform the user currently sees, then update
        // limits; this frame re-renders with the new limits below.
        let view = plot.transform(area);
        let Interaction {
            selection,
            roi_changed,
        } = apply_interaction(ui, &response, plot, area, &view);

        // Final transforms for this frame (after any interaction). The left
        // (main) transform drives the image, the left axes, and left-bound
        // curves; the optional right (y2) transform drives right-bound curves
        // and the right ticks.
        let transform = plot.transform(area);
        let transform_right = plot.transform_y2(area);
        let ortho_left = transform.ortho_matrix();
        let axis_log_left = axis_log_flags(&transform);
        // Right-axis matrices fall back to the left axis when there is no y2,
        // so a stray right-bound curve still draws against the main axis.
        let (ortho_right, axis_log_right) = match &transform_right {
            Some(t) => (t.ortho_matrix(), axis_log_flags(t)),
            None => (ortho_left, axis_log_left),
        };

        // Data-area size in physical pixels, for the curve's pixel-space line
        // expansion (`area` is in logical points).
        let ppp = ui.ctx().pixels_per_point();
        let viewport_px = [area.width() * ppp, area.height() * ppp];

        // Convert sRGB Color32 to linear, premultiplied RGBA expected by the shader.
        let bg = egui::Rgba::from(plot.data_background).to_array();
        let style = chrome::Style::from_visuals(ui.visuals())
            .with_overrides(plot.foreground, plot.grid_color);

        let painter = ui.painter();
        // Data layer (wgpu), clipped to the data area: clear, image, then curve.
        painter.add(egui_wgpu::Callback::new_paint_callback(
            area,
            ClearCallback { color: bg },
        ));
        painter.add(egui_wgpu::Callback::new_paint_callback(
            area,
            ImageCallback {
                ortho: ortho_left,
                axis_log: axis_log_left,
            },
        ));
        // Decimate to per-pixel-column min/max only when the x-axis is linear
        // (equal data-x bins map to equal pixel columns); on a log x-axis they
        // do not, so 0 disables it and the full curve is drawn.
        let decimate_columns = if transform.x.scale == Scale::Linear {
            viewport_px[0].ceil() as u32
        } else {
            0
        };
        painter.add(egui_wgpu::Callback::new_paint_callback(
            area,
            CurveCallback {
                ortho_left,
                axis_log_left,
                ortho_right,
                axis_log_right,
                viewport_px,
                x_window: (transform.x.min, transform.x.max),
                decimate_columns,
            },
        ));

        // Chrome (egui), drawn on top of / in the gutters around the data layer.
        chrome::draw_axes(painter, &transform, &style);
        if let Some(t_right) = &transform_right {
            chrome::draw_y2_ticks(painter, t_right, &style);
        }
        if let (Some(cbar), Some(cmap)) = (chrome_layout.colorbar, plot.colormap.as_ref()) {
            chrome::draw_colorbar(painter, cbar, cmap, &style);
        }

        // Title + axis labels in the reserved gutters.
        chrome::draw_labels(
            painter,
            rect,
            area,
            &chrome::Labels {
                title: plot.title.as_deref(),
                x: plot.x_label.as_deref(),
                y: plot.y_label.as_deref(),
                y2: plot.y2_label.as_deref(),
            },
            with_y2,
            &style,
        );

        // Regions of interest (fill, border, edge handles) over the data layer.
        if !plot.rois.is_empty() {
            chrome::draw_rois(painter, &transform, &plot.rois, &style);
        }

        // Hover crosshair + coordinate readout over the data area.
        if plot.crosshair
            && let Some(p) = response.hover_pos()
            && area.contains(p)
        {
            chrome::draw_crosshair(painter, &transform, p, &style);
        }

        // Box-zoom selection rectangle (drawn last, on top of everything).
        if let Some(sel) = selection {
            painter.rect_filled(
                sel,
                egui::CornerRadius::ZERO,
                Color32::from_rgba_unmultiplied(style.axis.r(), style.axis.g(), style.axis.b(), 32),
            );
            painter.rect_stroke(
                sel,
                egui::CornerRadius::ZERO,
                Stroke::new(1.0, style.axis),
                egui::StrokeKind::Inside,
            );
        }

        PlotResponse {
            response,
            transform,
            roi_changed,
        }
    }
}

/// Per-axis log flags `[x, y]` (1.0 = log10) for the shaders, matching a
/// transform's scales.
fn axis_log_flags(t: &crate::core::transform::Transform) -> [f32; 2] {
    [
        f32::from(t.x.scale == Scale::Log10),
        f32::from(t.y.scale == Scale::Log10),
    ]
}

/// Apply the active pointer interaction to `plot.limits` (and, for an ROI edge
/// drag, to `plot.rois`). `view` is the transform matching what is currently on
/// screen, used to convert pointer pixels to data coordinates. Returns the
/// in-progress box-zoom selection rect and the ROI index changed this frame.
fn apply_interaction(
    ui: &Ui,
    response: &egui::Response,
    plot: &mut Plot,
    area: Rect,
    view: &crate::core::transform::Transform,
) -> Interaction {
    // Interaction operates on the displayed view's limits (which fold in any
    // aspect-ratio expansion), so pan/zoom act on exactly what is on screen.
    let base = (view.x.min, view.x.max, view.y.min, view.y.max);

    // Reset: double-click restores the home view.
    if response.double_clicked()
        && let Some(home) = plot.home_limits
    {
        plot.limits = home;
    }

    // Pan: right-drag translates the limits by the per-frame drag delta.
    if response.dragged_by(PointerButton::Secondary) {
        let delta = response.drag_delta();
        if delta != egui::Vec2::ZERO {
            let next = interaction::pan(base, area, delta);
            commit(plot, next);
        }
    }

    // Zoom: wheel over the data area scales about the data point under the cursor.
    let scroll = ui.input(|i| i.smooth_scroll_delta.y);
    if response.hovered()
        && scroll != 0.0
        && let Some(p) = response.hover_pos()
        && area.contains(p)
    {
        let (cx, cy) = view.pixel_to_data(p);
        let factor = interaction::wheel_zoom_factor(scroll);
        let next = interaction::zoom_about(base, factor, cx, cy);
        commit(plot, next);
    }

    // Left-drag start: prefer grabbing an ROI edge under the cursor; only if no
    // edge is grabbed does the same press begin a box-zoom selection. Both drag
    // states live in egui temp memory across frames (the ROI drag under a
    // distinct id from the box-zoom start pos).
    let id = response.id;
    let roi_id = id.with("roi-drag");
    let mut roi_changed = None;
    if response.drag_started_by(PointerButton::Primary)
        && let Some(p) = response.interact_pointer_pos()
    {
        // Topmost ROI wins (last in the list is drawn last, so hit-tested first).
        let grabbed = plot.rois.iter().enumerate().rev().find_map(|(i, roi)| {
            roi.edge_at(view, p, ROI_GRAB_PX)
                .map(|edge| RoiDrag { roi: i, edge })
        });
        match grabbed {
            Some(rd) => ui.data_mut(|d| {
                d.insert_temp(roi_id, rd);
            }),
            None => ui.data_mut(|d| {
                d.insert_temp(id, p);
            }),
        }
    }

    // An active ROI edge drag takes precedence over box zoom.
    let mut selection = None;
    if let Some(rd) = ui.data_mut(|d| d.get_temp::<RoiDrag>(roi_id)) {
        if response.dragged_by(PointerButton::Primary)
            && let Some(cur) = response.interact_pointer_pos()
            && let Some(roi) = plot.rois.get_mut(rd.roi)
        {
            roi.move_edge(rd.edge, view.pixel_to_data(cur));
            roi_changed = Some(rd.roi);
        }
        if response.drag_stopped_by(PointerButton::Primary) {
            ui.data_mut(|d| d.remove::<RoiDrag>(roi_id));
        }
    } else {
        // Box zoom: left-drag selects a rectangle; release zooms to it.
        if response.dragged_by(PointerButton::Primary) {
            let start = ui.data_mut(|d| d.get_temp::<Pos2>(id));
            if let (Some(start), Some(cur)) = (start, response.interact_pointer_pos()) {
                selection = Some(Rect::from_two_pos(start, cur));
            }
        }
        if response.drag_stopped_by(PointerButton::Primary) {
            let start = ui.data_mut(|d| {
                let s = d.get_temp::<Pos2>(id);
                d.remove::<Pos2>(id);
                s
            });
            if let (Some(start), Some(end)) = (start, response.interact_pointer_pos()) {
                // Ignore accidental click-sized drags.
                if (start - end).length() > 4.0 {
                    let (ax, ay) = view.pixel_to_data(start);
                    let (bx, by) = view.pixel_to_data(end);
                    let next = interaction::box_zoom(ax, ay, bx, by);
                    commit(plot, next);
                }
            }
        }
    }

    Interaction {
        selection,
        roi_changed,
    }
}

/// Adopt `next` limits only if they are non-degenerate, otherwise keep the
/// current ones (guards against a collapsed/inverted view).
fn commit(plot: &mut Plot, next: interaction::Limits) {
    if interaction::is_valid(next) {
        plot.limits = next;
    }
}
