//! The stateless egui plot view.
//!
//! Lays out the chrome gutters, applies pointer interaction to the plot limits,
//! clears the data area, draws the image and curve via wgpu paint callbacks,
//! then draws the axes and (optional) colorbar with egui's painter. The wgpu
//! layer and the chrome share one [`crate::core::transform::Transform`] derived
//! from the (possibly just-updated) limits, so they stay aligned while panning
//! and zooming (`doc/design.md` §4·§8·§11.6).
//!
//! Mouse mapping follows the active interaction mode: select mode uses primary
//! drag for ROI handles, zoom mode uses primary drag for box zoom, pan mode uses
//! primary drag for panning. Secondary drag pans in every mode; wheel zoom and
//! double-click reset remain available.

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

/// Primary pointer behavior used by [`PlotView`].
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum PlotInteractionMode {
    /// Primary clicks select items in high-level widgets; primary drags adjust
    /// ROI handles without starting a box zoom.
    Select,
    /// Primary drag pans the plot.
    Pan,
    /// Primary drag draws a box zoom. This preserves the original low-level
    /// [`PlotView::show`] behavior.
    #[default]
    Zoom,
}

/// What [`PlotView::show`] returns: the egui [`Response`](egui::Response) plus
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

/// What [`PlotView::show_with_draw`] returns: the [`PlotResponse`] plus the
/// draw-mode event (if any) produced this frame.
pub struct DrawResponse {
    /// The underlying plot response (egui response + display transform).
    pub plot: PlotResponse,
    /// The [`DrawEvent`](interaction::DrawEvent) produced this frame: an
    /// `InProgress` preview while drawing, or `Finished` when the shape
    /// completes. `None` when nothing changed this frame.
    pub event: Option<interaction::DrawEvent>,
}

/// Stateless view that renders a [`Plot`] into an egui `Ui`.
#[derive(Default)]
pub struct PlotView;

impl PlotView {
    /// Create a new plot view.
    pub fn new() -> Self {
        Self
    }

    /// Render the plot with the default zoom interaction mode, filling the
    /// available space. Returns the egui response and the display transform used
    /// this frame.
    pub fn show(self, ui: &mut Ui, plot: &mut Plot) -> PlotResponse {
        self.show_with_interaction(ui, plot, PlotInteractionMode::Zoom)
    }

    /// Restore the previously stored view from the limits history, mirroring
    /// silx `ZoomBackAction` (`getLimitsHistory().pop()`). Returns `true` if a
    /// stored view was restored, `false` if the history was empty. The toolbar
    /// zoom-back button (a later wave) calls through this.
    pub fn zoom_back(&self, plot: &mut Plot) -> bool {
        plot.zoom_back()
    }

    /// Render the plot with an explicit primary-pointer interaction mode.
    pub fn show_with_interaction(
        self,
        ui: &mut Ui,
        plot: &mut Plot,
        interaction_mode: PlotInteractionMode,
    ) -> PlotResponse {
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
        } = apply_interaction(ui, &response, plot, area, &view, interaction_mode);

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
            ClearCallback {
                color: bg,
                plot_id: plot.id,
            },
        ));
        painter.add(egui_wgpu::Callback::new_paint_callback(
            area,
            ImageCallback {
                ortho: ortho_left,
                axis_log: axis_log_left,
                plot_id: plot.id,
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
                plot_id: plot.id,
            },
        ));

        // Per-vertex-colored triangle meshes (silx addTriangles) sit in the data
        // layer: over the wgpu image/curve, under the chrome grid and frame.
        if !plot.triangles.is_empty() {
            chrome::draw_triangles(painter, &transform, &plot.triangles);
        }

        // Chrome (egui), drawn on top of / in the gutters around the data layer.
        // Per-axis tick mode routes date-time axes through dtime_ticks.
        chrome::draw_axes_with_tick_modes(
            painter,
            &transform,
            &style,
            plot.grid,
            plot.x_max_ticks,
            plot.y_max_ticks,
            plot.x_tick_mode(),
            plot.y_tick_mode(),
        );
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

        // Shapes (polygons / rectangles / polylines / lines) over the data layer
        // (silx addShape). Bound to the main (left) axes.
        if !plot.shapes.is_empty() {
            chrome::draw_shapes(painter, &transform, &plot.shapes);
        }

        // Point / line markers over the data layer (silx addMarker).
        if !plot.markers.is_empty() {
            chrome::draw_markers(painter, &transform, transform_right.as_ref(), &plot.markers);
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

    /// Render the plot in a draw/select mode driven by `draw`, painting the
    /// in-progress shape as a rubber-band overlay and returning any
    /// [`DrawEvent`](interaction::DrawEvent) produced this frame.
    ///
    /// The plot is shown in [`PlotInteractionMode::Select`] so a primary drag
    /// feeds the draw state machine instead of box-zooming (secondary drag still
    /// pans, wheel still zooms). Press / move / release on the data area are fed
    /// to `draw` (silx `Select*` `onPress` / `onMove` / `onRelease`); the
    /// resulting preview/finished shape is drawn with `style`
    /// (silx `setSelectionArea`, `PlotInteraction.py:98-141`).
    ///
    /// Wiring a `Finished` event to ROI / mask creation is left to the caller
    /// (silx high-level widgets / mask tools).
    pub fn show_with_draw(
        self,
        ui: &mut Ui,
        plot: &mut Plot,
        draw: &mut interaction::DrawState,
        style: interaction::SelectionStyle,
    ) -> DrawResponse {
        let plot_response =
            PlotView::new().show_with_interaction(ui, plot, PlotInteractionMode::Select);
        let response = &plot_response.response;
        let transform = plot_response.transform;

        // Feed pointer events to the draw state machine, mapping each pixel to
        // data through the display transform.
        let mut event = None;
        if let Some(p) = response.interact_pointer_pos() {
            let input = interaction::DrawInput::from_pixel(&transform, p);
            if response.drag_started_by(PointerButton::Primary) || response.clicked() {
                event = draw.on_press(input).or(event);
            }
            if response.dragged_by(PointerButton::Primary) {
                event = draw.on_move(input).or(event);
            }
            if response.drag_stopped_by(PointerButton::Primary) || response.clicked() {
                event = draw.on_release(input).or(event);
            }
        }
        // A bare hover (no button) still drives polygon snap / preview.
        if !response.is_pointer_button_down_on()
            && let Some(p) = response.hover_pos()
            && draw.is_active()
        {
            let input = interaction::DrawInput::from_pixel(&transform, p);
            event = draw.on_move(input).or(event);
        }

        // Paint the in-progress preview overlay (the rubber band).
        if let Some(points) = draw.preview() {
            draw_overlay(ui.painter(), &transform, draw.mode(), &points, style);
        } else if let Some(interaction::DrawEvent::InProgress { mode, points }) = &event {
            draw_overlay(ui.painter(), &transform, *mode, points, style);
        }

        DrawResponse {
            plot: plot_response,
            event,
        }
    }
}

/// Paint a draw-mode rubber-band overlay: the fill per `style.fill` plus the
/// dashed outline silx uses (`linestyle="--"`, `PlotInteraction.py:98-141`).
/// `points` are data-space vertices (already in the preview shape for the mode).
fn draw_overlay(
    painter: &egui::Painter,
    transform: &Transform,
    mode: interaction::DrawMode,
    points: &[(f64, f64)],
    style: interaction::SelectionStyle,
) {
    use interaction::{DrawMode, FillMode};

    if points.is_empty() {
        return;
    }
    let pix: Vec<Pos2> = points
        .iter()
        .map(|&(x, y)| transform.data_to_pixel(x, y))
        .collect();

    // FreeHand / Line are open polylines; the rest are closed areas.
    let closed = !matches!(mode, DrawMode::FreeHand | DrawMode::Line);

    if closed && pix.len() >= 3 {
        let bb = Rect::from_points(&pix);
        match style.fill {
            FillMode::Solid => {
                let fill = Color32::from_rgba_unmultiplied(
                    style.color.r(),
                    style.color.g(),
                    style.color.b(),
                    style.color.a() / 2,
                );
                painter.add(egui::Shape::convex_polygon(pix.clone(), fill, Stroke::NONE));
            }
            FillMode::Hatch => {
                // Diagonal hatch over the bounding box (the box-clipped
                // approximation silx renders for the hatch fill).
                let clipped = painter.with_clip_rect(bb);
                for (a, b) in interaction::hatch_lines(bb, 6.0) {
                    clipped.line_segment([a, b], Stroke::new(1.0, style.color));
                }
            }
            FillMode::None => {}
        }
    }

    // Dashed outline (silx linestyle="--").
    let stroke = Stroke::new(1.5, style.color);
    let mut outline = pix.clone();
    if closed && outline.len() >= 2 {
        outline.push(outline[0]);
    }
    painter.add(egui::Shape::dashed_line(&outline, stroke, 6.0, 4.0));
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
    mode: PlotInteractionMode,
) -> Interaction {
    // Interaction operates on the displayed view's limits (which fold in any
    // aspect-ratio expansion), so pan/zoom act on exactly what is on screen.
    let base = (view.x.min, view.x.max, view.y.min, view.y.max);

    // Reset: double-click restores the home view (silx `resetZoom`) and clears
    // the limits history.
    if response.double_clicked()
        && let Some(home) = plot.home_limits
    {
        plot.limits = home;
        plot.clear_limits_history();
    }

    // Arrow-key pan: when the plot area has keyboard focus, arrow keys pan by a
    // fraction of the view (silx `PanWithArrowKeysAction` -> `PlotWidget.pan`
    // with factor 0.1). One press is one pan step.
    if response.has_focus() {
        for (key, dir) in [
            (egui::Key::ArrowLeft, interaction::PanDirection::Left),
            (egui::Key::ArrowRight, interaction::PanDirection::Right),
            (egui::Key::ArrowUp, interaction::PanDirection::Up),
            (egui::Key::ArrowDown, interaction::PanDirection::Down),
        ] {
            if ui.input(|i| i.key_pressed(key)) {
                arrow_pan(plot, dir);
            }
        }
    }

    // Pan: secondary-drag always pans; pan mode also binds primary-drag to pan.
    let primary_pan =
        mode == PlotInteractionMode::Pan && response.dragged_by(PointerButton::Primary);
    if response.dragged_by(PointerButton::Secondary) || primary_pan {
        // Push the pre-pan view once, at the start of the pan gesture, so
        // zoom-back can restore it (silx pushes on box-zoom; here the limits
        // history also captures pan gestures — push on drag-start, not every
        // frame).
        if response.drag_started() {
            plot.push_limits();
        }
        let delta = ui.input(|i| i.pointer.delta());
        if delta != egui::Vec2::ZERO {
            let next = interaction::pan(base, area, delta, plot.x_scale, plot.y_scale);
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
        // Push the pre-zoom view so zoom-back can step out of the wheel zoom.
        plot.push_limits();
        let next = interaction::zoom_about(base, factor, cx, cy, plot.x_scale, plot.y_scale);
        commit(plot, next);
    }

    // Left-drag start: select/zoom modes prefer grabbing an ROI edge under the
    // cursor. Zoom mode falls back to a box-zoom selection; select mode does
    // not, so item/handle interactions are not preempted by zoom.
    let id = response.id;
    let roi_id = id.with("roi-drag");
    let mut roi_changed = None;
    if mode != PlotInteractionMode::Pan
        && response.drag_started_by(PointerButton::Primary)
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
            None if mode == PlotInteractionMode::Zoom => ui.data_mut(|d| {
                d.insert_temp(id, p);
            }),
            None => {}
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
        if mode == PlotInteractionMode::Zoom && response.dragged_by(PointerButton::Primary) {
            let start = ui.data_mut(|d| d.get_temp::<Pos2>(id));
            if let (Some(start), Some(cur)) = (start, response.interact_pointer_pos()) {
                selection = Some(Rect::from_two_pos(start, cur));
            }
        }
        if mode == PlotInteractionMode::Zoom && response.drag_stopped_by(PointerButton::Primary) {
            let start = ui.data_mut(|d| {
                let s = d.get_temp::<Pos2>(id);
                d.remove::<Pos2>(id);
                s
            });
            if let (Some(start), Some(end)) = (start, response.interact_pointer_pos()) {
                // Ignore accidental click-sized drags.
                if (start - end).length() > 4.0 {
                    // Push the pre-zoom view before applying the box zoom (silx
                    // `Zoom._zoom` pushes to the limits history here).
                    plot.push_limits();
                    let (ax, ay) = view.pixel_to_data(start);
                    let (bx, by) = view.pixel_to_data(end);
                    let next = interaction::box_zoom(ax, ay, bx, by);
                    commit(plot, next);
                }
            }
        }
    }

    // Cursor shape: while hovering an ROI edge (and not box-zoom dragging), show
    // the matching resize/move cursor so a grabbable handle is discoverable,
    // mirroring silx `_setCursorForMarker` (`PlotInteraction.py:1165-1184`). Skip
    // in pan mode (primary drag pans there) and while an edge drag is active.
    if mode != PlotInteractionMode::Pan
        && selection.is_none()
        && !response.dragged_by(PointerButton::Primary)
        && let Some(p) = response.hover_pos()
        && area.contains(p)
    {
        let grabbed = plot
            .rois
            .iter()
            .rev()
            .find_map(|roi| roi.edge_at(view, p, ROI_GRAB_PX));
        let shape = interaction::cursor_for_grab(grabbed);
        if shape != interaction::CursorShape::Default {
            ui.ctx().set_cursor_icon(shape.to_egui());
        }
    }

    Interaction {
        selection,
        roi_changed,
    }
}

/// Pan the plot by one arrow-key step in `dir`, mirroring silx
/// `PlotWidget.pan(direction, factor=0.1)`. Left/right pan the X axis; up/down
/// pan the left Y axis and (if present) the y2 axis by the same factor, with the
/// sign flipped when the Y axis is inverted. The shift is log-aware per axis
/// (silx `applyPan`). Like silx's arrow-key path, this does not push to the
/// limits history.
fn arrow_pan(plot: &mut Plot, dir: interaction::PanDirection) {
    use interaction::PanDirection::*;
    const FACTOR: f64 = 0.1;
    let x_is_log = plot.x_scale == Scale::Log10;
    let y_is_log = plot.y_scale == Scale::Log10;
    let (x_min, x_max, y_min, y_max) = plot.limits;

    match dir {
        Left | Right => {
            let x_factor = if dir == Right { FACTOR } else { -FACTOR };
            let (nx0, nx1) = interaction::apply_pan(x_min, x_max, x_factor, x_is_log);
            commit(plot, (nx0, nx1, y_min, y_max));
        }
        Up | Down => {
            // silx flips the sign when the Y axis is displayed inverted.
            let sign = if plot.y_inverted { -1.0 } else { 1.0 };
            let y_factor = sign * if dir == Up { FACTOR } else { -FACTOR };
            let (ny0, ny1) = interaction::apply_pan(y_min, y_max, y_factor, y_is_log);
            commit(plot, (x_min, x_max, ny0, ny1));
            // y2 pans with the same factor (silx pans the right axis too).
            if let Some((y2_min, y2_max)) = plot.y2 {
                let (n2_0, n2_1) = interaction::apply_pan(y2_min, y2_max, y_factor, y_is_log);
                if interaction::is_valid((x_min, x_max, n2_0, n2_1)) {
                    plot.y2 = Some((n2_0, n2_1));
                }
            }
        }
    }
}

/// Adopt `next` limits only if they are non-degenerate, otherwise keep the
/// current ones (guards against a collapsed/inverted view). Clamps into the
/// float32-safe range (silx `checkAxisLimits` after pan/zoom), then applies
/// per-axis constraints, before the validity check.
fn commit(plot: &mut Plot, next: interaction::Limits) {
    // Clamp first so an extreme pan/zoom cannot push a bound past the
    // float32-safe window (silx `PlotInteraction.py:241-250`).
    let next = interaction::clamp_limits(
        next,
        plot.x_scale == Scale::Log10,
        plot.y_scale == Scale::Log10,
    );
    if !interaction::is_valid(next) {
        return;
    }
    let (x0, x1, y0, y1) = next;
    let (x0, x1) = plot.x_constraints.apply(x0, x1);
    let (y0, y1) = plot.y_constraints.apply(y0, y1);
    let constrained = (x0, x1, y0, y1);
    if interaction::is_valid(constrained) {
        plot.limits = constrained;
    }
}
