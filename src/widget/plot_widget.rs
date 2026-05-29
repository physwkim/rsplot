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
use crate::core::transform::Scale;
use crate::render::backend_wgpu::{ClearCallback, CurveCallback, ImageCallback};
use crate::widget::{chrome, interaction};

/// Widget that renders a [`Plot`] into an egui `Ui`.
#[derive(Default)]
pub struct PlotWidget;

impl PlotWidget {
    /// Create a new plot widget.
    pub fn new() -> Self {
        Self
    }

    /// Render the plot, filling the available space, and handle interaction.
    pub fn show(self, ui: &mut Ui, plot: &mut Plot) -> egui::Response {
        let (rect, response) = ui.allocate_exact_size(ui.available_size(), Sense::click_and_drag());

        // Capture the initial view once, for double-click reset.
        let current = plot.limits;
        plot.home_limits.get_or_insert(current);

        // Chrome gutters depend only on whether a colorbar shows, not on limits.
        let with_colorbar = plot.colormap.is_some();
        let chrome_layout = chrome::layout(rect, with_colorbar);
        let area = plot.margins.data_area(chrome_layout.data_area);

        // Map input through the transform the user currently sees, then update
        // limits; this frame re-renders with the new limits below.
        let view = plot.transform(area);
        let selection = apply_interaction(ui, &response, plot, area, &view);

        // Final transform for this frame (after any interaction).
        let transform = plot.transform(area);
        let ortho = transform.ortho_matrix();
        // Per-axis log flags for the shaders (must match the transform's scale).
        let axis_log = [
            f32::from(transform.x.scale == Scale::Log10),
            f32::from(transform.y.scale == Scale::Log10),
        ];

        // Convert sRGB Color32 to linear, premultiplied RGBA expected by the shader.
        let bg = egui::Rgba::from(plot.data_background).to_array();
        let style = chrome::Style::from_visuals(ui.visuals());

        let painter = ui.painter();
        // Data layer (wgpu), clipped to the data area: clear, image, then curve.
        painter.add(egui_wgpu::Callback::new_paint_callback(
            area,
            ClearCallback { color: bg },
        ));
        painter.add(egui_wgpu::Callback::new_paint_callback(
            area,
            ImageCallback { ortho, axis_log },
        ));
        painter.add(egui_wgpu::Callback::new_paint_callback(
            area,
            CurveCallback { ortho, axis_log },
        ));

        // Chrome (egui), drawn on top of / in the gutters around the data layer.
        chrome::draw_axes(painter, &transform, &style);
        if let (Some(cbar), Some(cmap)) = (chrome_layout.colorbar, plot.colormap.as_ref()) {
            chrome::draw_colorbar(painter, cbar, cmap, &style);
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

        response
    }
}

/// Apply the active pointer interaction to `plot.limits`, returning the
/// in-progress box-zoom selection rect (screen space) if a left-drag is under
/// way. `view` is the transform matching what is currently on screen, used to
/// convert pointer pixels to data coordinates.
fn apply_interaction(
    ui: &Ui,
    response: &egui::Response,
    plot: &mut Plot,
    area: Rect,
    view: &crate::core::transform::Transform,
) -> Option<Rect> {
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

    // Box zoom: left-drag selects a rectangle; release zooms to it. The drag
    // start (screen pixels) is stashed in egui's temp memory under the
    // response id across frames.
    let id = response.id;
    if response.drag_started_by(PointerButton::Primary)
        && let Some(p) = response.interact_pointer_pos()
    {
        ui.data_mut(|d| d.insert_temp(id, p));
    }
    let mut selection = None;
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

    selection
}

/// Adopt `next` limits only if they are non-degenerate, otherwise keep the
/// current ones (guards against a collapsed/inverted view).
fn commit(plot: &mut Plot, next: interaction::Limits) {
    if interaction::is_valid(next) {
        plot.limits = next;
    }
}
