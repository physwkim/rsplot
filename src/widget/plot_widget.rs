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
//! primary drag for panning. Secondary drag pans in every mode; a secondary
//! *click* opens a zoom context menu (Zoom Back / Reset Zoom). Wheel zoom
//! remains available.

use egui::{PointerButton, Pos2, Rect, Sense, Stroke, Ui};

use crate::core::plot::Plot;
use crate::core::roi::ManagedRoi;
use crate::core::transform::{Scale, Transform};
use crate::widget::interaction::{RoiDrawKind, RoiGrab};

/// Pixel radius for grabbing an ROI edge handle.
const ROI_GRAB_PX: f32 = 6.0;

/// An in-progress ROI edit drag, stashed in egui temp memory across frames.
///
/// `grab` is what the drag grabbed on the ROI at `roi`: a specific
/// [`RoiEdge`](crate::core::roi::RoiEdge) handle ([`RoiGrab::Edge`]), applied via
/// [`Roi::move_edge`](crate::core::roi::Roi::move_edge), or the whole body
/// ([`RoiGrab::Translate`]), applied via
/// [`Roi::translate`](crate::core::roi::Roi::translate). For a translate, the
/// data position at the *previous* frame is carried in `last_data` so each
/// frame's delta is `cursor_data - last_data`.
///
/// `roi` is the index into `plot.rois`: that vector is the source of truth and
/// `sync_plot_items` never rebuilds or reorders it (unlike the per-frame
/// z-sorted marker mirror), so an index is a stable identity for the duration of
/// a drag and the Wave-11 handle-keying does not apply here.
#[derive(Clone, Copy)]
struct RoiDrag {
    roi: usize,
    grab: RoiGrab,
    /// Data position last frame, used to compute the per-frame translate delta.
    /// Unused for [`RoiGrab::Edge`] (which moves to the absolute cursor).
    last_data: (f64, f64),
}

/// An in-progress draggable-marker drag, stashed in egui temp memory across
/// frames. `handle` is the stable identity of the grabbed marker; the index into
/// `plot.markers` (the per-frame, z-sorted mirror that `sync_plot_items`
/// rebuilds) is re-resolved from `handle` each frame, so the drag keeps tracking
/// the same marker even if the mirror is rebuilt or reordered mid-drag. `anchor`
/// is the marker's data position at grab time, the constraint anchor passed to
/// [`Marker::drag`](crate::core::marker::Marker::drag) each frame.
#[derive(Clone, Copy)]
struct MarkerDrag {
    handle: crate::core::backend::ItemHandle,
    anchor: (f64, f64),
}

/// What `apply_interaction` produced this frame.
struct Interaction {
    /// In-progress box-zoom selection rectangle (screen space).
    selection: Option<egui::Rect>,
    /// Index of the ROI whose bounds an edge drag or whole-ROI translate changed
    /// this frame.
    roi_changed: Option<usize>,
    /// Index in `plot.rois` of an ROI just created this frame by a finished
    /// on-plot draw (silx `RegionOfInterestManager` shape-finished), or `None`.
    roi_created: Option<usize>,
    /// In-progress ROI-creation preview this frame: the draw mode plus the
    /// data-space preview vertices, painted by the caller via `draw_overlay`
    /// (the same overlay the box-zoom selection uses). `None` when no
    /// `RoiCreate` draw is active.
    roi_preview: Option<(interaction::DrawMode, Vec<(f64, f64)>)>,
    /// Handle of the marker a drag moved this frame (silx `markerMoving`), or
    /// `None`. Set only on the frame the marker actually moved.
    marker_moved: Option<crate::core::backend::ItemHandle>,
    /// Handle of the marker whose drag began this frame (silx `beginDrag` on a
    /// draggable marker), or `None`. Set on the frame the grab is anchored.
    marker_drag_started: Option<crate::core::backend::ItemHandle>,
    /// Handle of the marker whose drag ended this frame (silx `endDrag`
    /// `markerMoved`), or `None`. Set on the frame the primary button is
    /// released while a marker drag was active.
    marker_drag_finished: Option<crate::core::backend::ItemHandle>,
    /// The low-level pointer event detected over the data area this frame
    /// (click / double-click / move), or `None`.
    pointer_event: Option<interaction::PlotPointerEvent>,
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
    /// Pencil / mask-draw mode: the primary drag is reserved for painting the
    /// mask, so it must not pan, draw a box zoom, or start an ROI/box-select
    /// (silx `MaskToolsWidget` activating the plot's pencil draw interaction,
    /// `MaskToolsWidget.py:849-876`). Secondary-drag panning and wheel zoom are
    /// left intact, matching silx's draw interaction. [`apply_interaction`]
    /// suppresses pan (`== Pan`) and box-zoom (`== Zoom`) by mode comparison and
    /// suppresses the ROI-edge grab explicitly, so no primary-drag plot gesture
    /// fires in this mode.
    MaskDraw,
    /// On-plot ROI creation: the primary drag draws a new ROI of the given
    /// [`RoiDrawKind`] via a [`DrawState`](interaction::DrawState), mirroring
    /// silx `RegionOfInterestManager.start(roiClass)` arming a draw shape
    /// (`tools/roi.py`). Like [`MaskDraw`](Self::MaskDraw) it reserves the
    /// primary drag — it does not pan, box-zoom, or grab an ROI edge — while a
    /// finished draw appends a new ROI to `plot.rois`. Secondary-drag panning and
    /// wheel zoom stay intact. Creation re-arms continuously (draw repeatedly
    /// until the mode changes), matching silx's default continuous mode.
    RoiCreate(RoiDrawKind),
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
    /// from an edge drag or whole-ROI translate, or `None` (`doc/design.md`
    /// §13 C3).
    pub roi_changed: Option<usize>,
    /// Index into `Plot::rois` of an ROI created this frame by a finished
    /// on-plot draw in [`PlotInteractionMode::RoiCreate`] (silx
    /// `RegionOfInterestManager` shape-finished), or `None`.
    pub roi_created: Option<usize>,
    /// Handle of the marker an on-screen drag moved this frame (silx
    /// `markerMoving`), or `None`. The mirror `Plot::markers` is already updated
    /// for this frame's render; `PlotWidget::show` persists the change back to
    /// the backend item and emits [`crate::PlotEvent::MarkerMoved`].
    pub marker_moved: Option<crate::core::backend::ItemHandle>,
    /// Handle of the marker whose drag *began* this frame (silx `beginDrag` on a
    /// draggable marker), or `None`. `PlotWidget::show` emits
    /// [`crate::PlotEvent::MarkerDragStarted`]. The first
    /// [`Self::marker_moved`] fires on the same frame (the grab is also a move),
    /// matching silx emitting the first `markerMoving` at drag begin.
    pub marker_drag_started: Option<crate::core::backend::ItemHandle>,
    /// Handle of the marker whose drag *ended* this frame (silx `endDrag`
    /// `markerMoved`), or `None`. `PlotWidget::show` emits
    /// [`crate::PlotEvent::MarkerDragFinished`]; the marker's final position is
    /// already persisted by the preceding [`Self::marker_moved`] frames.
    pub marker_drag_finished: Option<crate::core::backend::ItemHandle>,
    /// The low-level pointer event detected over the data area this frame
    /// (silx `prepareMouseSignal` "mouseClicked" / "mouseDoubleClicked" /
    /// "mouseMoved", `PlotEvents.py:58-71`), or `None` when there was none. The
    /// data coordinates are projected through the display [`Transform`]. A
    /// click/double-click takes precedence over a bare move in the same frame.
    pub pointer_event: Option<interaction::PlotPointerEvent>,
    /// The latest draw-mode event produced this frame (silx `drawingProgress` /
    /// `drawingFinished`), or `None`. Populated only by
    /// [`PlotView::show_with_draw`]; the plain [`PlotView::show`] /
    /// [`PlotView::show_with_interaction`] paths leave it `None` (they run no
    /// draw state machine).
    pub draw_event: Option<interaction::DrawEvent>,
    /// The active primary-pointer interaction mode this frame (silx's current
    /// `Interaction.StateMachine` mode), surfaced read-only for status-bar UIs.
    pub interaction_mode: PlotInteractionMode,
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

        // Capture the initial view once; the Reset Zoom context-menu item
        // restores it.
        let current = plot.limits;
        plot.home_limits.get_or_insert(current);

        // Chrome gutters depend only on which axes/colorbar/labels show, not on
        // limits.
        let with_colorbar = plot.colormap.is_some();
        let with_y2 = plot.y2.is_some();
        let axes_displayed = plot.axes_displayed();
        let chrome_request = chrome::ChromeRequest {
            colorbar: with_colorbar,
            y2: with_y2,
            title: plot.title.is_some(),
            x_label: plot.x_label.is_some(),
            y_label: plot.y_label.is_some(),
            y2_label: plot.y2_label.is_some(),
            // Hidden axes zero the axis gutters (silx setAxesDisplayed(False)).
            axes_hidden: !axes_displayed,
        };
        let chrome_layout = chrome::layout(rect, &chrome_request);
        let area = plot.margins.data_area(chrome_layout.data_area);

        // Map input through the transform the user currently sees, then update
        // limits; this frame re-renders with the new limits below.
        let view = plot.transform(area);
        let Interaction {
            selection,
            roi_changed,
            roi_created,
            roi_preview,
            marker_moved,
            marker_drag_started,
            marker_drag_finished,
            pointer_event,
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
        // Per-axis tick mode routes date-time axes through dtime_ticks. When the
        // axes are hidden the frame/ticks/labels are not drawn (silx
        // setAxesDisplayed(False) hides the axes and zeroes their margins).
        if axes_displayed {
            chrome::draw_axes_with_x_tick_mode(
                painter,
                &transform,
                &style,
                plot.grid,
                plot.x_max_ticks,
                plot.y_max_ticks,
                plot.x_tick_mode(),
            );
            if let Some(t_right) = &transform_right {
                chrome::draw_y2_ticks(painter, t_right, &style);
            }
        }
        if let (Some(cbar), Some(cmap)) = (chrome_layout.colorbar, plot.colormap.as_ref()) {
            chrome::draw_colorbar(painter, cbar, cmap, &style);
        }

        // Title + axis labels in the reserved gutters (hidden with the axes).
        // Axis labels resolve the active curve's per-axis label over the graph
        // default (silx `Axis._currentLabel`); the title has no active override.
        if axes_displayed {
            let x_label = plot.displayed_x_label();
            let y_label = plot.displayed_y_label();
            let y2_label = plot.displayed_y2_label();
            chrome::draw_labels(
                painter,
                rect,
                area,
                &chrome::Labels {
                    title: plot.title.as_deref(),
                    x: x_label.as_deref(),
                    y: y_label.as_deref(),
                    y2: y2_label.as_deref(),
                },
                with_y2,
                &style,
            );
        }

        // Regions of interest (per-ROI color/name/selection/width/style/fill,
        // border, edge handles) over the data layer.
        if !plot.rois.is_empty() {
            chrome::draw_rois(painter, &transform, &plot.rois, plot.roi_color, &style);
        }

        // Shapes (polygons / rectangles / polylines / lines) over the data layer
        // (silx addShape). Bound to the main (left) axes.
        if !plot.shapes.is_empty() {
            chrome::draw_shapes(painter, &transform, &plot.shapes);
        }

        // Infinite line items (silx Line), clipped to the viewport. Drawn right
        // after shapes, on the main (left) axes.
        if !plot.lines().is_empty() {
            chrome::draw_lines(painter, &transform, plot.lines());
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
                crate::core::color::with_alpha(style.axis, 32),
            );
            painter.rect_stroke(
                sel,
                egui::CornerRadius::ZERO,
                Stroke::new(1.0, style.axis),
                egui::StrokeKind::Inside,
            );
        }

        // In-progress ROI-creation rubber band (drawn last, like the box-zoom
        // selection). Uses the default selection style; the draw overlay renders
        // the per-mode preview shape (silx `setSelectionArea`).
        if let Some((mode, points)) = &roi_preview {
            draw_overlay(
                ui.painter(),
                &transform,
                *mode,
                points,
                interaction::SelectionStyle::default(),
            );
        }

        PlotResponse {
            response,
            transform,
            roi_changed,
            roi_created,
            marker_moved,
            marker_drag_started,
            marker_drag_finished,
            pointer_event,
            // The plain show path runs no draw state machine; show_with_draw
            // fills this in below.
            draw_event: None,
            interaction_mode,
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
        let mut plot_response =
            PlotView::new().show_with_interaction(ui, plot, PlotInteractionMode::Select);
        let response = &plot_response.response;
        let transform = plot_response.transform;

        // Feed pointer events to the draw state machine via the shared helper
        // (the same press/move/release/hover logic the RoiCreate mode uses).
        let event = feed_draw_state(draw, response, &transform);

        // Paint the in-progress preview overlay (the rubber band).
        if let Some(points) = draw.preview() {
            draw_overlay(ui.painter(), &transform, draw.mode(), &points, style);
        } else if let Some(interaction::DrawEvent::InProgress { mode, points }) = &event {
            draw_overlay(ui.painter(), &transform, *mode, points, style);
        }

        // Surface this frame's draw event through PlotResponse too, so consumers
        // reading the embedded plot response (not only DrawResponse.event) see
        // the latest draw event on this path.
        plot_response.draw_event = event.clone();

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
                let fill = crate::core::color::with_alpha(style.color, style.color.a() / 2);
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

/// Feed this frame's primary-pointer press / move / release / bare-hover from
/// `response` into the draw state machine `draw`, projecting each cursor pixel to
/// data through `transform`, and return the latest [`DrawEvent`](interaction::DrawEvent)
/// it produced (silx `Select*` `onPress` / `onMove` / `onRelease`). Shared by
/// [`PlotView::show_with_draw`] and the [`PlotInteractionMode::RoiCreate`] block
/// in [`apply_interaction`] so both drive the state machine identically.
fn feed_draw_state(
    draw: &mut interaction::DrawState,
    response: &egui::Response,
    transform: &Transform,
) -> Option<interaction::DrawEvent> {
    let mut event = None;
    if let Some(p) = response.interact_pointer_pos() {
        let input = interaction::DrawInput::from_pixel(transform, p);
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
        let input = interaction::DrawInput::from_pixel(transform, p);
        event = draw.on_move(input).or(event);
    }
    event
}

/// Per-axis log flags `[x, y]` (1.0 = log10) for the shaders, matching a
/// transform's scales.
fn axis_log_flags(t: &crate::core::transform::Transform) -> [f32; 2] {
    [
        f32::from(t.x.scale == Scale::Log10),
        f32::from(t.y.scale == Scale::Log10),
    ]
}

/// Whether `mode` may grab an ROI edge/body on a primary drag (and show the
/// matching resize cursor on hover). Every mode except
/// [`PlotInteractionMode::Pan`], [`PlotInteractionMode::MaskDraw`], and
/// [`PlotInteractionMode::RoiCreate`] does: Pan binds the primary drag to
/// panning, MaskDraw reserves it for mask painting, and RoiCreate reserves it for
/// drawing a new ROI, so none preempts its own gesture by grabbing an ROI
/// edge/body. Pure, so the gating is unit-testable without a `Ui`.
fn mode_grabs_roi_edge(mode: PlotInteractionMode) -> bool {
    !matches!(
        mode,
        PlotInteractionMode::Pan
            | PlotInteractionMode::MaskDraw
            | PlotInteractionMode::RoiCreate(_)
    )
}

/// Whether `mode` allows the highest-precedence marker drag (and the marker
/// hover cursor) to consume the primary drag. Every mode except
/// [`PlotInteractionMode::MaskDraw`] and [`PlotInteractionMode::RoiCreate`] does;
/// those two reserve the primary drag for mask painting / ROI drawing. Pure, so
/// the gating is unit-testable without a `Ui`.
fn mode_allows_marker_drag(mode: PlotInteractionMode) -> bool {
    !matches!(
        mode,
        PlotInteractionMode::MaskDraw | PlotInteractionMode::RoiCreate(_)
    )
}

/// The set of primary-drag plot gestures [`apply_interaction`] runs in `mode`,
/// surfaced for tests: `(pans, box_zooms, grabs_roi_edge)`. In
/// [`PlotInteractionMode::MaskDraw`] all three are `false` — the primary drag is
/// fully reserved for mask painting (silx's pencil draw interaction). Pure, so
/// the per-mode gating is verifiable without a `Ui`/GPU.
#[cfg(test)]
fn primary_drag_gestures(mode: PlotInteractionMode) -> (bool, bool, bool) {
    (
        mode == PlotInteractionMode::Pan,  // primary-drag pan
        mode == PlotInteractionMode::Zoom, // primary-drag box zoom
        mode_grabs_roi_edge(mode),         // primary-drag ROI-edge grab
    )
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

    // Marker drag (silx item-drag branch of the default interaction): the
    // highest-precedence primary-drag consumer in every mode except MaskDraw
    // and RoiCreate (which reserve the primary drag for mask painting / ROI
    // drawing). It pre-empts pan/zoom/ROI so a draggable marker under the cursor
    // wins the gesture.
    let id = response.id;

    // Anchor for every primary-drag *grab* hit-test (marker grab, ROI
    // edge/body grab). egui only reports `drag_started` once the pointer has
    // moved past `max_click_dist` (6px) from the press, so on that frame
    // `interact_pointer_pos()` is already >6px from where the user clicked.
    // Hit-testing a grab there misses any handle whose grab zone is a *point*
    // of radius <= that drift: rect corners, circle/ellipse perimeter
    // vertices, and small point markers all become un-grabbable and fall
    // through to the body-translate (or no-op) — the user-reported "corner /
    // diagonal doesn't resize, circle/ellipse only move". Line handles (rect
    // sides) survived only because their grab zone is unbounded along the
    // edge. The press origin is exactly where the user clicked (on the
    // handle), so anchoring every grab decision there fixes the point handles
    // without regressing the line handles or body translate. Falls back to the
    // interaction position if the press origin is somehow absent.
    let press_anchor = ui
        .input(|i| i.pointer.press_origin())
        .or_else(|| response.interact_pointer_pos());

    let marker_id = id.with("marker-drag");
    let mut marker_moved = None;
    let mut marker_drag_started = None;
    let mut marker_drag_finished = None;
    // Grab on drag-start: hit-test the topmost draggable marker at the press.
    if mode_allows_marker_drag(mode)
        && response.drag_started_by(PointerButton::Primary)
        && let Some(p) = press_anchor
        && let Some(index) = interaction::marker_at(&plot.markers, view, p)
        && let Some(&handle) = plot.marker_handles.get(index)
    {
        let anchor = plot.markers[index].position();
        ui.data_mut(|d| d.insert_temp(marker_id, MarkerDrag { handle, anchor }));
        marker_drag_started = Some(handle);
    }
    // Whether a marker drag is active this frame; gates pan/zoom/ROI below so
    // the marker drag is the sole primary-drag consumer while it runs.
    let marker_dragging = ui
        .data_mut(|d| d.get_temp::<MarkerDrag>(marker_id))
        .is_some();
    // Apply / finish the active marker drag.
    if let Some(md) = ui.data_mut(|d| d.get_temp::<MarkerDrag>(marker_id)) {
        if response.dragged_by(PointerButton::Primary)
            && let Some(cur) = response.interact_pointer_pos()
            // Re-resolve the mirror index from the stable handle each frame, so a
            // mid-drag rebuild/reorder of `plot.markers` can never move the wrong
            // marker; if the marker was removed mid-drag this simply no-ops.
            && let Some(index) = plot.marker_handles.iter().position(|&h| h == md.handle)
            && let Some(marker) = plot.markers.get_mut(index)
        {
            // Live-render this frame via the mirror; persistence to the backend
            // item happens in PlotWidget::show via the returned handle.
            marker.drag(md.anchor, view.pixel_to_data(cur));
            marker_moved = Some(md.handle);
        }
        if response.drag_stopped_by(PointerButton::Primary) {
            ui.data_mut(|d| d.remove::<MarkerDrag>(marker_id));
            marker_drag_finished = Some(md.handle);
        }
    }

    // Pan: secondary-drag always pans; pan mode also binds primary-drag to pan.
    // A marker drag pre-empts the primary-drag pan (secondary-drag pan is
    // unaffected, matching silx — only the item-drag branch competes with the
    // primary pan).
    let primary_pan = mode == PlotInteractionMode::Pan
        && !marker_dragging
        && response.dragged_by(PointerButton::Primary);
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

    // Left-drag start: select/zoom modes prefer grabbing an ROI edge handle or
    // body under the cursor. Zoom mode falls back to a box-zoom selection; select
    // mode does not, so item/handle interactions are not preempted by zoom.
    // MaskDraw / RoiCreate reserve the primary drag (mask painting / ROI
    // drawing), so they grab no ROI. A marker drag (grabbed above) pre-empts
    // both, so neither runs while it is active.
    let roi_id = id.with("roi-drag");
    let mut roi_changed = None;
    if mode_grabs_roi_edge(mode)
        && !marker_dragging
        && response.drag_started_by(PointerButton::Primary)
        && let Some(p) = press_anchor
    {
        // Topmost ROI wins; within an ROI a handle wins over the body (the
        // priority lives in `roi_grab_at`). `p` is the press origin (see
        // `press_anchor`), so a precise click on a point handle anchors the
        // edge grab even though egui only recognizes the drag after the cursor
        // has drifted off the handle.
        let grabbed =
            interaction::roi_grab_at(&plot.rois, view, p, ROI_GRAB_PX).map(|(roi, grab)| RoiDrag {
                roi,
                grab,
                last_data: view.pixel_to_data(p),
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

    // An active ROI edit drag (edge resize or whole-ROI translate) takes
    // precedence over box zoom.
    let mut selection = None;
    if let Some(mut rd) = ui.data_mut(|d| d.get_temp::<RoiDrag>(roi_id)) {
        // A stored ROI drag is only valid while the mode still grabs ROI edges.
        // The start gate above already requires `mode_grabs_roi_edge`; mirror it
        // here so the apply path is symmetric. A mid-drag switch to a non-ROI
        // mode (silx `setInteractiveMode` resets the in-progress interaction)
        // cancels the drag: drop the temp entry and apply no edit, so it can
        // neither leak edits into the new mode nor resume if the mode switches
        // back. The drag-stopped removal stays inside the still-grabbing branch
        // for the normal end-of-gesture path.
        if !mode_grabs_roi_edge(mode) {
            ui.data_mut(|d| d.remove::<RoiDrag>(roi_id));
        } else {
            if response.dragged_by(PointerButton::Primary)
                && let Some(cur) = response.interact_pointer_pos()
                && let Some(managed) = plot.rois.get_mut(rd.roi)
            {
                let cur_data = view.pixel_to_data(cur);
                match rd.grab {
                    // Edge resize: move the grabbed handle to the absolute cursor.
                    RoiGrab::Edge(edge) => managed.roi.move_edge(edge, cur_data),
                    // Whole-ROI translate: shift by this frame's delta, then
                    // advance the carried anchor so deltas accumulate (silx body
                    // drag).
                    RoiGrab::Translate => {
                        managed
                            .roi
                            .translate(cur_data.0 - rd.last_data.0, cur_data.1 - rd.last_data.1);
                        rd.last_data = cur_data;
                        ui.data_mut(|d| d.insert_temp(roi_id, rd));
                    }
                }
                roi_changed = Some(rd.roi);
            }
            if response.drag_stopped_by(PointerButton::Primary) {
                ui.data_mut(|d| d.remove::<RoiDrag>(roi_id));
            }
        }
    } else if !marker_dragging {
        // Box zoom: left-drag selects a rectangle; release zooms to it. A marker
        // drag pre-empts box zoom (no box-zoom start was stored above when a
        // marker was grabbed), so this is skipped while a marker drag is active.
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

    // On-plot ROI creation (silx RegionOfInterestManager arming a draw shape):
    // when in RoiCreate mode, run a DrawState (kept in egui temp memory across
    // frames) fed by the same press/move/release helper as `show_with_draw`. A
    // finished draw appends a new ROI to `plot.rois` and re-arms the DrawState
    // for the next shape (silx's continuous default); the in-progress preview is
    // surfaced for the caller to paint via `draw_overlay`.
    let mut roi_created = None;
    let mut roi_preview = None;
    if let PlotInteractionMode::RoiCreate(kind) = mode {
        let draw_id = id.with("roi-draw");
        let mut draw = ui
            .data_mut(|d| d.get_temp::<interaction::DrawState>(draw_id))
            .unwrap_or_else(|| interaction::DrawState::new(interaction::roi_draw_mode(kind)));
        let event = feed_draw_state(&mut draw, response, view);

        if let Some(interaction::DrawEvent::Finished { params, .. }) = &event {
            if let Some(roi) = interaction::roi_from_draw(kind, params) {
                plot.rois.push(ManagedRoi::new(roi));
                roi_created = Some(plot.rois.len() - 1);
            }
            // Re-arm a fresh DrawState for the next shape (continuous creation).
            draw = interaction::DrawState::new(interaction::roi_draw_mode(kind));
        } else if let Some(points) = draw.preview() {
            roi_preview = Some((draw.mode(), points));
        } else if let Some(interaction::DrawEvent::InProgress { mode: m, points }) = &event {
            roi_preview = Some((*m, points.clone()));
        }

        ui.data_mut(|d| d.insert_temp(draw_id, draw));
    }

    // Marker cursor (silx size cursor over a draggable marker), taking
    // precedence over the ROI-edge cursor: while dragging a marker show that
    // marker's drag-DOF cursor; otherwise, when hovering a draggable marker (in
    // any mode except MaskDraw / RoiCreate, which own the primary drag), show its
    // cursor. `marker_cursor_set` suppresses the ROI-edge cursor below so a
    // marker under an ROI handle still shows the marker's cursor.
    let mut marker_cursor_set = false;
    if mode_allows_marker_drag(mode) {
        let cursor_marker = if marker_dragging {
            // While dragging, follow the grabbed marker by its stable handle
            // (re-resolving the mirror index each frame, like the drag-apply).
            ui.data_mut(|d| d.get_temp::<MarkerDrag>(marker_id))
                .and_then(|md| plot.marker_handles.iter().position(|&h| h == md.handle))
                .and_then(|i| plot.markers.get(i))
        } else if let Some(p) = response.hover_pos().filter(|p| area.contains(*p)) {
            interaction::marker_at(&plot.markers, view, p).and_then(|i| plot.markers.get(i))
        } else {
            None
        };
        if let Some(marker) = cursor_marker {
            ui.ctx()
                .set_cursor_icon(interaction::marker_cursor(marker).to_egui());
            marker_cursor_set = true;
        }
    }

    // Cursor shape: while hovering an ROI edge (and not box-zoom dragging), show
    // the matching resize/move cursor so a grabbable handle is discoverable,
    // mirroring silx `_setCursorForMarker` (`PlotInteraction.py:1165-1184`). Skip
    // in pan mode (primary drag pans there), in MaskDraw mode (primary drag
    // paints the mask, so the edge is not grabbable), while an edge drag is
    // active, and when a marker cursor already claimed this frame (marker takes
    // precedence over an ROI edge).
    if mode_grabs_roi_edge(mode)
        && !marker_cursor_set
        && selection.is_none()
        && !response.dragged_by(PointerButton::Primary)
        && let Some(p) = response.hover_pos()
        && area.contains(p)
    {
        let grabbed = plot
            .rois
            .iter()
            .rev()
            .find_map(|managed| managed.roi.edge_at(view, p, ROI_GRAB_PX));
        let shape = interaction::cursor_for_grab(grabbed, view);
        if shape != interaction::CursorShape::Default {
            ui.ctx().set_cursor_icon(shape.to_egui());
        }
    }

    // Right-click context menu (silx `PlotWidget.contextMenuEvent`): a secondary
    // *click* opens a zoom menu. silx's default menu carries `Zoom Back`;
    // egui-silx adds `Reset Zoom` to absorb the view reset (silx binds reset to
    // the toolbar/home, never to a double-click, so the former double-click reset
    // is relocated here). A secondary *drag* still pans — egui opens the menu on a
    // click, not a drag — and the `mouseClicked "right"` event still fires
    // alongside, matching silx emitting the click signal while showing the menu.
    response.context_menu(|ui| {
        // Zoom Back: pop the last pushed view, falling back to a reset-zoom on an
        // empty history (silx `ZoomBackAction` -> `LimitsHistory.pop`, which
        // `resetZoom`s when empty; mirrors `actions::control::zoom_back`).
        if ui.button("Zoom Back").clicked() {
            if !plot.zoom_back() {
                plot.reset_zoom();
            }
            ui.close();
        }
        // Reset Zoom: restore the home view captured on first show and clear the
        // limits history (the behavior the double-click reset used to provide).
        if ui.button("Reset Zoom").clicked() {
            if let Some(home) = plot.home_limits {
                plot.limits = home;
            }
            plot.clear_limits_history();
            ui.close();
        }
    });

    // Low-level pointer event over the data area (silx prepareMouseSignal). A
    // click/double-click is reported at the interaction pointer position; a bare
    // move (no button held) is reported at the hover position. Click and
    // double-click take precedence over a move in the same frame, mirroring silx
    // emitting the click/double-click signal in `click()` rather than a move.
    let pointer_event = detect_pointer_event(response, view, area);

    Interaction {
        selection,
        roi_changed,
        roi_created,
        roi_preview,
        marker_moved,
        marker_drag_started,
        marker_drag_finished,
        pointer_event,
    }
}

/// Detect the low-level pointer event over the data `area` this frame, projecting
/// the cursor pixel to data through `view` (silx `prepareMouseSignal`,
/// `PlotEvents.py:58-71`). Returns, in priority order: a double-click, a single
/// click, then a bare move. `None` when nothing happened over the data area.
///
/// silx reports the double-click at the position of the *first* click; egui only
/// exposes the current pointer position, so the double-click here carries the
/// current (second-click) position. The data coordinate is otherwise faithful.
fn detect_pointer_event(
    response: &egui::Response,
    view: &Transform,
    area: Rect,
) -> Option<interaction::PlotPointerEvent> {
    use interaction::{MouseButton, PlotPointerEvent};

    // Click / double-click use the interaction pointer position (the pressed /
    // released pixel), which is what silx passes to prepareMouseSignal.
    if let Some(p) = response.interact_pointer_pos()
        && area.contains(p)
    {
        if response.double_clicked() {
            // silx only emits mouseDoubleClicked for the left button.
            return Some(PlotPointerEvent::double_clicked(MouseButton::Left, view, p));
        }
        for button in [
            egui::PointerButton::Primary,
            egui::PointerButton::Secondary,
            egui::PointerButton::Middle,
        ] {
            if response.clicked_by(button) {
                return Some(PlotPointerEvent::clicked(
                    MouseButton::from_egui(button),
                    view,
                    p,
                ));
            }
        }
    }

    // Bare move: the cursor moved over the data area this frame. silx leaves the
    // button unset for a hover move; report the held button when one is down.
    if let Some(p) = response.hover_pos()
        && area.contains(p)
        && ui_pointer_moved(response)
    {
        let button = if response.dragged_by(egui::PointerButton::Primary) {
            Some(MouseButton::Left)
        } else if response.dragged_by(egui::PointerButton::Secondary) {
            Some(MouseButton::Right)
        } else if response.dragged_by(egui::PointerButton::Middle) {
            Some(MouseButton::Middle)
        } else {
            None
        };
        return Some(PlotPointerEvent::moved(button, view, p));
    }

    None
}

/// Whether the pointer moved this frame (silx hover "mouseMoved" only fires on
/// actual movement). Uses the egui per-frame pointer delta via the response's
/// context.
fn ui_pointer_moved(response: &egui::Response) -> bool {
    response
        .ctx
        .input(|i| i.pointer.delta() != egui::Vec2::ZERO)
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

#[cfg(test)]
mod tests {
    use super::*;
    use crate::core::plot::Plot;

    /// Drive a headless egui frame with the given raw input, run `show`, and
    /// return the resulting [`PlotResponse`] and its data area. The wgpu paint
    /// callbacks are recorded but never executed in a headless run.
    fn run_frame(
        ctx: &egui::Context,
        plot: &mut Plot,
        raw: egui::RawInput,
    ) -> (PlotResponse, Rect) {
        let mut captured: Option<(PlotResponse, Rect)> = None;
        let _ = ctx.run_ui(raw, |ui| {
            let resp = PlotView::new().show(ui, plot);
            let area = resp.transform.area;
            captured = Some((resp, area));
        });
        captured.expect("ui ran")
    }

    /// Run a headless frame with `show_with_draw`, returning the [`DrawResponse`]
    /// and the data area.
    fn run_draw_frame(
        ctx: &egui::Context,
        plot: &mut Plot,
        draw: &mut interaction::DrawState,
        raw: egui::RawInput,
    ) -> (DrawResponse, Rect) {
        let mut captured: Option<(DrawResponse, Rect)> = None;
        let _ = ctx.run_ui(raw, |ui| {
            let resp = PlotView::new().show_with_draw(
                ui,
                plot,
                draw,
                interaction::SelectionStyle::default(),
            );
            let area = resp.plot.transform.area;
            captured = Some((resp, area));
        });
        captured.expect("ui ran")
    }

    fn screen_input(screen: egui::Vec2) -> egui::RawInput {
        egui::RawInput {
            screen_rect: Some(Rect::from_min_size(Pos2::ZERO, screen)),
            ..Default::default()
        }
    }

    #[test]
    fn click_emits_clicked_event_with_correct_data_coords() {
        let ctx = egui::Context::default();
        let mut plot = Plot::new(0);
        plot.limits = (0.0, 10.0, 0.0, 10.0);
        let screen = egui::vec2(200.0, 200.0);

        // Frame 1: discover the data area (no input).
        let (_r0, area) = run_frame(&ctx, &mut plot, screen_input(screen));
        let click_px = area.center();

        // Frame 2: pointer pressed at the click pixel.
        let mut press = screen_input(screen);
        press.events.push(egui::Event::PointerMoved(click_px));
        press.events.push(egui::Event::PointerButton {
            pos: click_px,
            button: egui::PointerButton::Primary,
            pressed: true,
            modifiers: egui::Modifiers::default(),
        });
        let _ = run_frame(&ctx, &mut plot, press);

        // Frame 3: pointer released at the same pixel -> egui registers a click.
        let mut release = screen_input(screen);
        release.events.push(egui::Event::PointerButton {
            pos: click_px,
            button: egui::PointerButton::Primary,
            pressed: false,
            modifiers: egui::Modifiers::default(),
        });
        let (resp, _area3) = run_frame(&ctx, &mut plot, release);

        let event = resp.pointer_event.expect("a pointer event on click frame");
        match event {
            interaction::PlotPointerEvent::Clicked {
                button,
                data,
                pixel,
            } => {
                assert_eq!(button, interaction::MouseButton::Left);
                // The data coordinate is the transform inverse of the click pixel.
                let expected = resp.transform.pixel_to_data(click_px);
                assert!(
                    (data.0 - expected.0).abs() < 1e-6,
                    "x {data:?} {expected:?}"
                );
                assert!(
                    (data.1 - expected.1).abs() < 1e-6,
                    "y {data:?} {expected:?}"
                );
                // The center of [0,10]x[0,10] is (5, 5).
                assert!((data.0 - 5.0).abs() < 0.5, "x≈5: {}", data.0);
                assert!((data.1 - 5.0).abs() < 0.5, "y≈5: {}", data.1);
                assert!((pixel.0 - click_px.x).abs() < 1e-3);
                assert!((pixel.1 - click_px.y).abs() < 1e-3);
            }
            other => panic!("expected Clicked, got {other:?}"),
        }
    }

    #[test]
    fn bare_move_emits_moved_event() {
        let ctx = egui::Context::default();
        let mut plot = Plot::new(0);
        plot.limits = (0.0, 10.0, 0.0, 10.0);
        let screen = egui::vec2(200.0, 200.0);

        // Frame 1: discover the data area.
        let (_r0, area) = run_frame(&ctx, &mut plot, screen_input(screen));
        let p0 = area.center();
        // Frame 2: park the pointer at p0 (establishes hover, no move delta yet).
        let mut f2 = screen_input(screen);
        f2.events.push(egui::Event::PointerMoved(p0));
        let _ = run_frame(&ctx, &mut plot, f2);

        // Frame 3: move the pointer by a few pixels -> bare move event.
        let p1 = p0 + egui::vec2(7.0, -5.0);
        let mut f3 = screen_input(screen);
        f3.events.push(egui::Event::PointerMoved(p1));
        let (resp, _area) = run_frame(&ctx, &mut plot, f3);

        let event = resp.pointer_event.expect("a moved event");
        match event {
            interaction::PlotPointerEvent::Moved {
                button,
                data,
                pixel,
            } => {
                // A bare move (no button held) leaves the button unset.
                assert_eq!(button, None);
                let expected = resp.transform.pixel_to_data(p1);
                assert!(
                    (data.0 - expected.0).abs() < 1e-6,
                    "x {data:?} {expected:?}"
                );
                assert!(
                    (data.1 - expected.1).abs() < 1e-6,
                    "y {data:?} {expected:?}"
                );
                assert!((pixel.0 - p1.x).abs() < 1e-3);
                assert!((pixel.1 - p1.y).abs() < 1e-3);
            }
            other => panic!("expected Moved, got {other:?}"),
        }
    }

    /// Press + release `button` at `px` across two frames and return the
    /// [`PlotResponse`] from the release frame (where egui registers the click).
    fn click_cycle(
        ctx: &egui::Context,
        plot: &mut Plot,
        screen: egui::Vec2,
        px: Pos2,
        button: egui::PointerButton,
    ) -> PlotResponse {
        let mut press = screen_input(screen);
        press.events.push(egui::Event::PointerMoved(px));
        press.events.push(egui::Event::PointerButton {
            pos: px,
            button,
            pressed: true,
            modifiers: egui::Modifiers::default(),
        });
        let _ = run_frame(ctx, plot, press);
        let mut release = screen_input(screen);
        release.events.push(egui::Event::PointerButton {
            pos: px,
            button,
            pressed: false,
            modifiers: egui::Modifiers::default(),
        });
        run_frame(ctx, plot, release).0
    }

    #[test]
    fn right_and_middle_click_emit_clicked_with_correct_button() {
        // silx's prepareMouseSignal reports the actual button; detect_pointer_event
        // maps Secondary/Middle through MouseButton::from_egui. Both paths are
        // exercised here (the click test above only covers the left button).
        let screen = egui::vec2(200.0, 200.0);

        for (button, expected) in [
            (
                egui::PointerButton::Secondary,
                interaction::MouseButton::Right,
            ),
            (
                egui::PointerButton::Middle,
                interaction::MouseButton::Middle,
            ),
        ] {
            // A fresh context per button: the Secondary click opens the right-click
            // context menu (silx `contextMenuEvent`), which would otherwise stay
            // open and swallow the next iteration's click. silx emits the
            // `mouseClicked` event AND shows the menu, so the Right click still
            // reports its event here; isolating the contexts only prevents the
            // open menu from leaking across the two independent button cases.
            let ctx = egui::Context::default();
            let mut plot = Plot::new(0);
            plot.limits = (0.0, 10.0, 0.0, 10.0);
            let (_r0, area) = run_frame(&ctx, &mut plot, screen_input(screen));
            let px = area.center();

            let resp = click_cycle(&ctx, &mut plot, screen, px, button);
            match resp.pointer_event {
                Some(interaction::PlotPointerEvent::Clicked {
                    button: got, data, ..
                }) => {
                    assert_eq!(got, expected, "button for {button:?}");
                    // Data coordinate is still the transform inverse of the pixel.
                    let want = resp.transform.pixel_to_data(px);
                    assert!((data.0 - want.0).abs() < 1e-6, "x {data:?} {want:?}");
                    assert!((data.1 - want.1).abs() < 1e-6, "y {data:?} {want:?}");
                }
                other => panic!("expected Clicked({expected:?}), got {other:?}"),
            }
        }
    }

    #[test]
    fn double_click_emits_double_clicked_event() {
        // silx emits mouseDoubleClicked only for the left button; detect_pointer_event
        // checks response.double_clicked() before the single-click loop. Two rapid
        // left press/release cycles at one pixel, with explicit close timestamps
        // inside egui's double-click delay, make the run deterministic.
        let ctx = egui::Context::default();
        let mut plot = Plot::new(0);
        plot.limits = (0.0, 10.0, 0.0, 10.0);
        let screen = egui::vec2(200.0, 200.0);
        let (_r0, area) = run_frame(&ctx, &mut plot, screen_input(screen));
        let px = area.center();

        let button_frame = |pressed: bool, time: f64| {
            let mut raw = screen_input(screen);
            raw.time = Some(time);
            raw.events.push(egui::Event::PointerMoved(px));
            raw.events.push(egui::Event::PointerButton {
                pos: px,
                button: egui::PointerButton::Primary,
                pressed,
                modifiers: egui::Modifiers::default(),
            });
            raw
        };

        // First click (press @0.10, release @0.12), then a second click within
        // egui's default 0.30s double-click delay (press @0.18, release @0.20).
        let _ = run_frame(&ctx, &mut plot, button_frame(true, 0.10));
        let _ = run_frame(&ctx, &mut plot, button_frame(false, 0.12));
        let _ = run_frame(&ctx, &mut plot, button_frame(true, 0.18));
        let (resp, _) = run_frame(&ctx, &mut plot, button_frame(false, 0.20));

        match resp.pointer_event {
            Some(interaction::PlotPointerEvent::DoubleClicked { button, .. }) => {
                assert_eq!(button, interaction::MouseButton::Left);
            }
            other => panic!("expected DoubleClicked, got {other:?}"),
        }
    }

    #[test]
    fn double_click_no_longer_resets_view() {
        // silx binds the view reset to the right-click context menu (Zoom Back /
        // Reset Zoom), not to a double-click. A double-click must therefore leave
        // the limits untouched; the reset is reachable only through the menu.
        let ctx = egui::Context::default();
        let mut plot = Plot::new(0);
        plot.limits = (0.0, 10.0, 0.0, 10.0);
        let screen = egui::vec2(200.0, 200.0);
        // Frame 1 captures home_limits = (0, 10, 0, 10) and the data area.
        let (_r0, area) = run_frame(&ctx, &mut plot, screen_input(screen));
        let px = area.center();
        // Move the view away from home, as a pan/zoom would.
        plot.limits = (5.0, 15.0, 5.0, 15.0);

        let button_frame = |pressed: bool, time: f64| {
            let mut raw = screen_input(screen);
            raw.time = Some(time);
            raw.events.push(egui::Event::PointerMoved(px));
            raw.events.push(egui::Event::PointerButton {
                pos: px,
                button: egui::PointerButton::Primary,
                pressed,
                modifiers: egui::Modifiers::default(),
            });
            raw
        };

        // Two rapid clicks within egui's default double-click delay.
        let _ = run_frame(&ctx, &mut plot, button_frame(true, 0.10));
        let _ = run_frame(&ctx, &mut plot, button_frame(false, 0.12));
        let _ = run_frame(&ctx, &mut plot, button_frame(true, 0.18));
        let (resp, _) = run_frame(&ctx, &mut plot, button_frame(false, 0.20));

        // The double-click still fires its event (silx mouseDoubleClicked)...
        assert!(matches!(
            resp.pointer_event,
            Some(interaction::PlotPointerEvent::DoubleClicked { .. })
        ));
        // ...but no longer reverts the view to home_limits.
        assert_eq!(plot.limits, (5.0, 15.0, 5.0, 15.0));
    }

    #[test]
    fn show_with_draw_surfaces_finished_draw_event_through_plot_response() {
        // A Line draw: press at one point, release at another -> Finished event,
        // surfaced both via DrawResponse.event AND PlotResponse.draw_event.
        let ctx = egui::Context::default();
        let mut plot = Plot::new(0);
        plot.limits = (0.0, 10.0, 0.0, 10.0);
        let mut draw = interaction::DrawState::new(interaction::DrawMode::Line);
        let screen = egui::vec2(200.0, 200.0);

        // Frame 1: discover the data area.
        let (_d0, area) = run_draw_frame(&ctx, &mut plot, &mut draw, screen_input(screen));
        let p0 = area.center() - egui::vec2(20.0, 20.0);
        let p1 = area.center() + egui::vec2(20.0, 20.0);

        // Frame 2: press at p0 (drag start).
        let mut f2 = screen_input(screen);
        f2.events.push(egui::Event::PointerMoved(p0));
        f2.events.push(egui::Event::PointerButton {
            pos: p0,
            button: egui::PointerButton::Primary,
            pressed: true,
            modifiers: egui::Modifiers::default(),
        });
        let _ = run_draw_frame(&ctx, &mut plot, &mut draw, f2);

        // Frame 3: drag to p1.
        let mut f3 = screen_input(screen);
        f3.events.push(egui::Event::PointerMoved(p1));
        let _ = run_draw_frame(&ctx, &mut plot, &mut draw, f3);

        // Frame 4: release at p1 -> Finished.
        let mut f4 = screen_input(screen);
        f4.events.push(egui::Event::PointerButton {
            pos: p1,
            button: egui::PointerButton::Primary,
            pressed: false,
            modifiers: egui::Modifiers::default(),
        });
        let (resp, _a) = run_draw_frame(&ctx, &mut plot, &mut draw, f4);

        // The same event is surfaced on both channels.
        assert_eq!(resp.event, resp.plot.draw_event);
        match resp.plot.draw_event {
            Some(interaction::DrawEvent::Finished {
                mode: interaction::DrawMode::Line,
                ..
            }) => {}
            other => panic!("expected Finished Line draw event, got {other:?}"),
        }
    }

    #[test]
    fn plain_show_leaves_draw_event_none_and_surfaces_mode() {
        // The plain show path runs no draw state machine -> draw_event is None,
        // and the interaction mode is surfaced read-only.
        let ctx = egui::Context::default();
        let mut plot = Plot::new(0);
        let (resp, _area) = run_frame(&ctx, &mut plot, screen_input(egui::vec2(200.0, 200.0)));
        assert!(resp.draw_event.is_none());
        assert_eq!(resp.interaction_mode, PlotInteractionMode::Zoom);
    }

    #[test]
    fn mask_draw_reserves_primary_drag_for_painting() {
        // MaskDraw is its own pencil-draw mode, distinct from Pan and Zoom, and
        // it reserves the primary drag entirely: apply_interaction must run no
        // primary-drag pan, no box zoom, and no ROI-edge grab in MaskDraw (silx
        // pencil draw interaction owns the drag). Assert the exact gating
        // booleans apply_interaction computes for each mode at the boundary.
        assert_ne!(PlotInteractionMode::MaskDraw, PlotInteractionMode::Pan);
        assert_ne!(PlotInteractionMode::MaskDraw, PlotInteractionMode::Zoom);
        assert_ne!(PlotInteractionMode::MaskDraw, PlotInteractionMode::Select);

        // (pans, box_zooms, grabs_roi_edge) per mode.
        assert_eq!(
            primary_drag_gestures(PlotInteractionMode::MaskDraw),
            (false, false, false),
            "MaskDraw must fire no primary-drag plot gesture",
        );
        assert_eq!(
            primary_drag_gestures(PlotInteractionMode::Pan),
            (true, false, false),
        );
        assert_eq!(
            primary_drag_gestures(PlotInteractionMode::Zoom),
            (false, true, true),
        );
        assert_eq!(
            primary_drag_gestures(PlotInteractionMode::Select),
            (false, false, true),
        );

        // The ROI-edge-grab gate (also used for the hover resize cursor) skips
        // Pan and MaskDraw, and only those.
        assert!(!mode_grabs_roi_edge(PlotInteractionMode::MaskDraw));
        assert!(!mode_grabs_roi_edge(PlotInteractionMode::Pan));
        assert!(mode_grabs_roi_edge(PlotInteractionMode::Zoom));
        assert!(mode_grabs_roi_edge(PlotInteractionMode::Select));
    }

    /// Run a headless frame with an explicit interaction mode.
    fn run_mode_frame(
        ctx: &egui::Context,
        plot: &mut Plot,
        mode: PlotInteractionMode,
        raw: egui::RawInput,
    ) -> (PlotResponse, Rect) {
        let mut captured: Option<(PlotResponse, Rect)> = None;
        let _ = ctx.run_ui(raw, |ui| {
            let resp = PlotView::new().show_with_interaction(ui, plot, mode);
            let area = resp.transform.area;
            captured = Some((resp, area));
        });
        captured.expect("ui ran")
    }

    fn press_at(screen: egui::Vec2, p: Pos2) -> egui::RawInput {
        let mut raw = screen_input(screen);
        raw.events.push(egui::Event::PointerMoved(p));
        raw.events.push(egui::Event::PointerButton {
            pos: p,
            button: egui::PointerButton::Primary,
            pressed: true,
            modifiers: egui::Modifiers::default(),
        });
        raw
    }

    fn move_to(screen: egui::Vec2, p: Pos2) -> egui::RawInput {
        let mut raw = screen_input(screen);
        raw.events.push(egui::Event::PointerMoved(p));
        raw
    }

    fn release_at(screen: egui::Vec2, p: Pos2) -> egui::RawInput {
        let mut raw = screen_input(screen);
        raw.events.push(egui::Event::PointerButton {
            pos: p,
            button: egui::PointerButton::Primary,
            pressed: false,
            modifiers: egui::Modifiers::default(),
        });
        raw
    }

    #[test]
    fn roi_create_mode_reserves_primary_drag_like_mask_draw() {
        // RoiCreate, like MaskDraw, must fire no primary-drag pan, box zoom, or
        // ROI-edge/body grab — the primary drag draws a new ROI instead.
        let mode = PlotInteractionMode::RoiCreate(RoiDrawKind::Rect);
        assert_eq!(primary_drag_gestures(mode), (false, false, false));
        assert!(!mode_grabs_roi_edge(mode));
        assert!(!mode_allows_marker_drag(mode));
        // Other modes still allow marker drag.
        assert!(mode_allows_marker_drag(PlotInteractionMode::Select));
        assert!(mode_allows_marker_drag(PlotInteractionMode::Zoom));
    }

    #[test]
    fn roi_create_point_single_click_appends_roi() {
        // A Point ROI finishes on a single click (no drag): one new Roi::Point.
        let ctx = egui::Context::default();
        let mut plot = Plot::new(0);
        plot.limits = (0.0, 10.0, 0.0, 10.0);
        let mode = PlotInteractionMode::RoiCreate(RoiDrawKind::Point);
        let screen = egui::vec2(200.0, 200.0);

        let (_r0, area) = run_mode_frame(&ctx, &mut plot, mode, screen_input(screen));
        let click_px = area.center();

        // Press then release form a single egui click; the Point ROI finishes
        // on the click. egui collapses press/drag/click frames unpredictably in
        // the headless harness, so assert on the click as a whole rather than on
        // one specific frame.
        let (press_resp, _a) = run_mode_frame(&ctx, &mut plot, mode, press_at(screen, click_px));
        let (release_resp, _a) =
            run_mode_frame(&ctx, &mut plot, mode, release_at(screen, click_px));

        // Exactly one Point ROI was created, and its create index (0) was
        // reported exactly once across the click's frames — this catches both a
        // missing report and a double-create (re-fire on both frames).
        assert_eq!(plot.rois.len(), 1);
        assert!(matches!(
            plot.rois[0].roi,
            crate::core::roi::Roi::Point { .. }
        ));
        let reported: Vec<usize> = [press_resp.roi_created, release_resp.roi_created]
            .into_iter()
            .flatten()
            .collect();
        assert_eq!(
            reported,
            vec![0],
            "create index reported exactly once on the finishing frame"
        );
    }

    #[test]
    fn roi_create_line_drag_appends_roi_and_rearms() {
        // A Line ROI drag (press, move, release) appends one Roi::Line and the
        // DrawState re-arms so a second drag appends another (continuous mode).
        let ctx = egui::Context::default();
        let mut plot = Plot::new(0);
        plot.limits = (0.0, 10.0, 0.0, 10.0);
        let mode = PlotInteractionMode::RoiCreate(RoiDrawKind::Line);
        let screen = egui::vec2(200.0, 200.0);

        let (_r0, area) = run_mode_frame(&ctx, &mut plot, mode, screen_input(screen));
        let a = area.center() - egui::vec2(20.0, 20.0);
        let b = area.center() + egui::vec2(20.0, 20.0);

        // First drag.
        let _ = run_mode_frame(&ctx, &mut plot, mode, press_at(screen, a));
        let _ = run_mode_frame(&ctx, &mut plot, mode, move_to(screen, b));
        let (resp1, _) = run_mode_frame(&ctx, &mut plot, mode, release_at(screen, b));
        assert_eq!(plot.rois.len(), 1);
        assert!(matches!(
            plot.rois[0].roi,
            crate::core::roi::Roi::Line { .. }
        ));
        assert_eq!(resp1.roi_created, Some(0));

        // Second drag: the DrawState re-armed, so a fresh Line is appended.
        let c = area.center() + egui::vec2(30.0, -10.0);
        let _ = run_mode_frame(&ctx, &mut plot, mode, press_at(screen, area.center()));
        let _ = run_mode_frame(&ctx, &mut plot, mode, move_to(screen, c));
        let (resp2, _) = run_mode_frame(&ctx, &mut plot, mode, release_at(screen, c));
        assert_eq!(plot.rois.len(), 2);
        assert_eq!(resp2.roi_created, Some(1));
    }

    #[test]
    fn roi_create_preview_surfaced_mid_drag() {
        // While dragging a rectangle in RoiCreate, the in-progress preview is
        // surfaced for painting and no ROI is created yet.
        let ctx = egui::Context::default();
        let mut plot = Plot::new(0);
        plot.limits = (0.0, 10.0, 0.0, 10.0);
        let mode = PlotInteractionMode::RoiCreate(RoiDrawKind::Rect);
        let screen = egui::vec2(200.0, 200.0);

        let (_r0, area) = run_mode_frame(&ctx, &mut plot, mode, screen_input(screen));
        let a = area.center() - egui::vec2(20.0, 20.0);
        let b = area.center() + egui::vec2(20.0, 20.0);

        let _ = run_mode_frame(&ctx, &mut plot, mode, press_at(screen, a));
        let (mid, _) = run_mode_frame(&ctx, &mut plot, mode, move_to(screen, b));
        // Still drawing: no ROI created mid-drag.
        assert!(plot.rois.is_empty());
        assert_eq!(mid.roi_created, None);
    }

    #[test]
    fn select_mode_body_drag_translates_roi() {
        // In Select mode, a primary drag that starts inside an ROI body (away
        // from any handle) translates the whole ROI by the drag delta. The grab
        // anchors when egui's drag_started fires; the rect then translates by the
        // data delta of each subsequent move. To keep the test independent of
        // exactly which frame egui starts the drag, the rect is captured AFTER
        // the grab is established (post first move) and compared after one more
        // move, asserting the displacement equals the cursor data delta.
        let ctx = egui::Context::default();
        let mut plot = Plot::new(0);
        plot.limits = (0.0, 10.0, 0.0, 10.0);
        // A big rect (data x[1,9] y[1,9]) so the body interior is generous and
        // the cursor stays well clear of the corner handles throughout.
        plot.rois.push(ManagedRoi::new(crate::core::roi::Roi::Rect {
            x: (1.0, 9.0),
            y: (1.0, 9.0),
        }));
        let mode = PlotInteractionMode::Select;
        let screen = egui::vec2(200.0, 200.0);

        let (r0, area) = run_mode_frame(&ctx, &mut plot, mode, screen_input(screen));
        let t = r0.transform;
        let c = area.center();
        // Small in-body moves (all far from any edge): press, then two moves.
        let a_px = c + egui::vec2(8.0, 8.0);
        let b_px = c + egui::vec2(18.0, -2.0);

        let _ = run_mode_frame(&ctx, &mut plot, mode, press_at(screen, c));
        // First move: drag_started fires here at the latest; grab anchored.
        let _ = run_mode_frame(&ctx, &mut plot, mode, move_to(screen, a_px));
        // Capture the rect with the grab established; the next move's data delta
        // is what the rect must shift by.
        let before = match &plot.rois[0].roi {
            crate::core::roi::Roi::Rect { x, y } => (*x, *y),
            other => panic!("{other:?}"),
        };
        let a_data = t.pixel_to_data(a_px);
        let b_data = t.pixel_to_data(b_px);
        let (ddx, ddy) = (b_data.0 - a_data.0, b_data.1 - a_data.1);

        let (resp, _) = run_mode_frame(&ctx, &mut plot, mode, move_to(screen, b_px));
        assert_eq!(resp.roi_changed, Some(0));
        match &plot.rois[0].roi {
            crate::core::roi::Roi::Rect { x, y } => {
                // The whole rect translated by exactly the cursor data delta; no
                // edge moved independently (both bounds shift by the same amount).
                assert!((x.0 - (before.0.0 + ddx)).abs() < 1e-6, "x0 {x:?}");
                assert!((x.1 - (before.0.1 + ddx)).abs() < 1e-6, "x1 {x:?}");
                assert!((y.0 - (before.1.0 + ddy)).abs() < 1e-6, "y0 {y:?}");
                assert!((y.1 - (before.1.1 + ddy)).abs() < 1e-6, "y1 {y:?}");
                // A real shift occurred (the test would be vacuous otherwise).
                assert!(ddx.abs() > 1e-6 || ddy.abs() > 1e-6);
            }
            other => panic!("{other:?}"),
        }
    }

    #[test]
    fn draggable_marker_drag_emits_started_moved_finished_triad() {
        // Silx beginDrag/drag/endDrag lifecycle for a draggable marker:
        // MarkerDragStarted (grab) → MarkerMoved×N → MarkerDragFinished (release).
        // The start fires on the same frame as the first move (the grab is also a
        // move), matching silx emitting the first markerMoving at begin.
        use crate::core::marker::Marker;
        let ctx = egui::Context::default();
        let mut plot = Plot::new(0);
        plot.limits = (0.0, 10.0, 0.0, 10.0);
        let mode = PlotInteractionMode::Select;
        let screen = egui::vec2(200.0, 200.0);

        // Baseline frame to read the transform, then place a draggable marker at
        // the data point under the area center (so a press at the center picks it).
        let (r0, area) = run_mode_frame(&ctx, &mut plot, mode, screen_input(screen));
        let c = area.center();
        let (mx, my) = r0.transform.pixel_to_data(c);
        let mut marker = Marker::point(mx, my);
        marker.is_draggable = true;
        plot.markers.push(marker);
        plot.marker_handles.push(42);

        // Press on the marker, then move >6px so egui starts the drag: the grab
        // anchors and the first move applies on the same frame.
        let _ = run_mode_frame(&ctx, &mut plot, mode, press_at(screen, c));
        let (r1, _) = run_mode_frame(
            &ctx,
            &mut plot,
            mode,
            move_to(screen, c + egui::vec2(12.0, 9.0)),
        );
        assert_eq!(
            r1.marker_drag_started,
            Some(42),
            "drag start on the grab frame"
        );
        assert_eq!(
            r1.marker_moved,
            Some(42),
            "first markerMoving on the same frame as the grab"
        );
        assert_eq!(r1.marker_drag_finished, None);

        // A further move: moving feedback only, the start does not repeat.
        let (r2, _) = run_mode_frame(
            &ctx,
            &mut plot,
            mode,
            move_to(screen, c + egui::vec2(24.0, 3.0)),
        );
        assert_eq!(r2.marker_drag_started, None, "start fires exactly once");
        assert_eq!(r2.marker_moved, Some(42));
        assert_eq!(r2.marker_drag_finished, None);

        // Release: drag finished, no move on the release frame.
        let (r3, _) = run_mode_frame(
            &ctx,
            &mut plot,
            mode,
            release_at(screen, c + egui::vec2(24.0, 3.0)),
        );
        assert_eq!(r3.marker_drag_finished, Some(42), "drag finish on release");
        assert_eq!(r3.marker_drag_started, None);
        assert_eq!(
            r3.marker_moved, None,
            "the release frame carries no markerMoving"
        );
    }

    #[test]
    fn roi_drag_cancelled_on_mid_drag_mode_switch() {
        // A body-translate ROI drag started in a grab-allowing mode (Select)
        // must NOT keep editing the ROI if the mode switches mid-drag to one
        // that does not grab ROI edges (MaskDraw), and must not resume when the
        // mode switches back — the stale drag is cancelled (silx
        // `setInteractiveMode` resets the in-progress interaction).
        let ctx = egui::Context::default();
        let mut plot = Plot::new(0);
        plot.limits = (0.0, 10.0, 0.0, 10.0);
        plot.rois.push(ManagedRoi::new(crate::core::roi::Roi::Rect {
            x: (1.0, 9.0),
            y: (1.0, 9.0),
        }));
        let screen = egui::vec2(200.0, 200.0);

        let (_r0, area) = run_mode_frame(
            &ctx,
            &mut plot,
            PlotInteractionMode::Select,
            screen_input(screen),
        );
        let c = area.center();
        let a_px = c + egui::vec2(8.0, 8.0);
        let b_px = c + egui::vec2(18.0, -2.0);

        // Start the drag in Select and anchor the grab (drag_started fires by
        // the first move at the latest).
        let _ = run_mode_frame(
            &ctx,
            &mut plot,
            PlotInteractionMode::Select,
            press_at(screen, c),
        );
        let _ = run_mode_frame(
            &ctx,
            &mut plot,
            PlotInteractionMode::Select,
            move_to(screen, a_px),
        );
        let before = match &plot.rois[0].roi {
            crate::core::roi::Roi::Rect { x, y } => (*x, *y),
            other => panic!("{other:?}"),
        };

        // Mid-drag switch to MaskDraw (no ROI-edge grab) and move: the drag is
        // cancelled, so no edit lands this frame.
        let (resp, _) = run_mode_frame(
            &ctx,
            &mut plot,
            PlotInteractionMode::MaskDraw,
            move_to(screen, b_px),
        );
        assert_eq!(
            resp.roi_changed, None,
            "ROI must not edit in a mode that does not grab ROI edges"
        );
        match &plot.rois[0].roi {
            crate::core::roi::Roi::Rect { x, y } => {
                assert_eq!((*x, *y), before, "rect unchanged after the mode switch")
            }
            other => panic!("{other:?}"),
        }

        // Switching back to Select must not resume the cancelled drag; with the
        // button still held (no new drag_started), a further move does nothing.
        let (resp2, _) = run_mode_frame(
            &ctx,
            &mut plot,
            PlotInteractionMode::Select,
            move_to(screen, c),
        );
        assert_eq!(
            resp2.roi_changed, None,
            "cancelled drag must not resume when the mode switches back"
        );
        match &plot.rois[0].roi {
            crate::core::roi::Roi::Rect { x, y } => {
                assert_eq!(
                    (*x, *y),
                    before,
                    "rect still unchanged after switching back"
                )
            }
            other => panic!("{other:?}"),
        }
    }

    #[test]
    fn circle_perimeter_drag_resizes_end_to_end_under_inverted_y() {
        // End-to-end (apply_interaction) proof that the circle's perimeter handle
        // is grabbable and resizes the radius — even on an inverted-Y image plot.
        // This distinguishes the real edge-grab path from the body-translate
        // fallback (the user reported circle/ellipse "only translate").
        let ctx = egui::Context::default();
        let mut plot = Plot::new(0);
        plot.limits = (0.0, 10.0, 0.0, 10.0);
        plot.y_inverted = true;
        plot.rois
            .push(ManagedRoi::new(crate::core::roi::Roi::Circle {
                center: (5.0, 5.0),
                radius: 3.0,
            }));
        let mode = PlotInteractionMode::Select;
        let screen = egui::vec2(200.0, 200.0);

        let (r0, _area) = run_mode_frame(&ctx, &mut plot, mode, screen_input(screen));
        let t = r0.transform;
        // Perimeter handle at data (center.x + r, center.y) = (8, 5).
        let handle_px = t.data_to_pixel(8.0, 5.0);

        // Press on the handle, anchor the grab with a small move that stays
        // within the handle's grab radius, then drag out to data (9,5): the
        // radius must grow from 3 to 4 (perimeter resize, not a translate).
        let _ = run_mode_frame(&ctx, &mut plot, mode, press_at(screen, handle_px));
        let _ = run_mode_frame(
            &ctx,
            &mut plot,
            mode,
            move_to(screen, handle_px + egui::vec2(2.0, 0.0)),
        );
        let target_px = t.data_to_pixel(9.0, 5.0);
        let (resp, _) = run_mode_frame(&ctx, &mut plot, mode, move_to(screen, target_px));
        assert_eq!(resp.roi_changed, Some(0), "perimeter grab edits the ROI");
        match &plot.rois[0].roi {
            crate::core::roi::Roi::Circle { center, radius } => {
                assert!(
                    (center.0 - 5.0).abs() < 1e-6 && (center.1 - 5.0).abs() < 1e-6,
                    "center unchanged: {center:?}"
                );
                assert!(
                    (*radius - 4.0).abs() < 1e-6,
                    "radius grew to 4, got {radius}"
                );
            }
            other => panic!("{other:?}"),
        }
    }

    #[test]
    fn rect_corner_drag_resizes_diagonally_end_to_end_under_inverted_y() {
        // End-to-end (apply_interaction) proof that a rect CORNER handle is
        // grabbable and resizes diagonally. The user reported "직사각형 ...
        // 상하/좌우로는 되는데 대각선은 안됨" — rect side (top/bottom/left/right)
        // resize worked but the diagonal corner did not. A corner is a point
        // handle, un-grabbable before the press-origin anchor (the cursor
        // drifts off the 6px corner zone before egui recognizes the drag); the
        // side handles always worked because their grab zone is a line.
        let ctx = egui::Context::default();
        let mut plot = Plot::new(0);
        plot.limits = (0.0, 10.0, 0.0, 10.0);
        plot.y_inverted = true;
        plot.rois.push(ManagedRoi::new(crate::core::roi::Roi::Rect {
            x: (2.0, 7.0),
            y: (2.0, 7.0),
        }));
        let mode = PlotInteractionMode::Select;
        let screen = egui::vec2(200.0, 200.0);

        let (r0, _area) = run_mode_frame(&ctx, &mut plot, mode, screen_input(screen));
        let t = r0.transform;
        // Data corner (x.max, y.max) = (7, 7).
        let corner_px = t.data_to_pixel(7.0, 7.0);

        // Press on the corner, anchor with a small within-grab move, then drag
        // the corner out to data (9, 9): the (x.max, y.max) corner follows while
        // the opposite (x.min, y.min) corner stays fixed.
        let _ = run_mode_frame(&ctx, &mut plot, mode, press_at(screen, corner_px));
        let _ = run_mode_frame(
            &ctx,
            &mut plot,
            mode,
            move_to(screen, corner_px + egui::vec2(2.0, 2.0)),
        );
        let target_px = t.data_to_pixel(9.0, 9.0);
        let (resp, _) = run_mode_frame(&ctx, &mut plot, mode, move_to(screen, target_px));
        assert_eq!(resp.roi_changed, Some(0), "corner grab edits the ROI");
        match &plot.rois[0].roi {
            crate::core::roi::Roi::Rect { x, y } => {
                assert!((x.0 - 2.0).abs() < 1e-6, "x.min fixed: {x:?}");
                assert!((y.0 - 2.0).abs() < 1e-6, "y.min fixed: {y:?}");
                assert!((x.1 - 9.0).abs() < 1e-6, "x.max followed cursor: {x:?}");
                assert!((y.1 - 9.0).abs() < 1e-6, "y.max followed cursor: {y:?}");
            }
            other => panic!("{other:?}"),
        }
    }

    #[test]
    fn ellipse_axis_handle_drag_resizes_end_to_end_under_inverted_y() {
        // End-to-end (apply_interaction) proof that an ellipse axis handle is
        // grabbable and resizes a semi-axis. The user reported "타원은 위치 이동만
        // 가능하고 크기조절이 안됨" — ellipse only translated, no resize. The axis
        // handle is a point handle, un-grabbable before the press-origin anchor.
        let ctx = egui::Context::default();
        let mut plot = Plot::new(0);
        plot.limits = (0.0, 10.0, 0.0, 10.0);
        plot.y_inverted = true;
        plot.rois
            .push(ManagedRoi::new(crate::core::roi::Roi::Ellipse {
                center: (5.0, 5.0),
                radii: (3.0, 2.0),
            }));
        let mode = PlotInteractionMode::Select;
        let screen = egui::vec2(200.0, 200.0);

        let (r0, _area) = run_mode_frame(&ctx, &mut plot, mode, screen_input(screen));
        let t = r0.transform;
        // x-axis handle at data (center.x + radii.0, center.y) = (8, 5).
        let handle_px = t.data_to_pixel(8.0, 5.0);

        let _ = run_mode_frame(&ctx, &mut plot, mode, press_at(screen, handle_px));
        let _ = run_mode_frame(
            &ctx,
            &mut plot,
            mode,
            move_to(screen, handle_px + egui::vec2(2.0, 0.0)),
        );
        // Drag out to data (9, 5): the x semi-axis grows 3 -> 4, the y one and
        // the center stay put.
        let target_px = t.data_to_pixel(9.0, 5.0);
        let (resp, _) = run_mode_frame(&ctx, &mut plot, mode, move_to(screen, target_px));
        assert_eq!(resp.roi_changed, Some(0), "axis-handle grab edits the ROI");
        match &plot.rois[0].roi {
            crate::core::roi::Roi::Ellipse { center, radii } => {
                assert!(
                    (center.0 - 5.0).abs() < 1e-6 && (center.1 - 5.0).abs() < 1e-6,
                    "center unchanged: {center:?}"
                );
                assert!(
                    (radii.0 - 4.0).abs() < 1e-6,
                    "x semi-axis grew to 4: {radii:?}"
                );
                assert!(
                    (radii.1 - 2.0).abs() < 1e-6,
                    "y semi-axis unchanged: {radii:?}"
                );
            }
            other => panic!("{other:?}"),
        }
    }
}
