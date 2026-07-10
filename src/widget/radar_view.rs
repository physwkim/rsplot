//! Miniature overview widget, porting silx `RadarView`.
//!
//! A [`RadarView`] paints a small box showing the full data extent (gray fill,
//! white outline) with an inner draggable rectangle (blue outline) marking the
//! current visible viewport. Dragging the inner rect emits the new viewport
//! limits so the caller can pan the real plot.
//!
//! Faithful to `silx/gui/plot/tools/RadarView.py:139-360`:
//!
//! - coordinates are as in `QGraphicsView`: x left→right, y top→bottom
//!   (`RadarView` class docstring, lines 143-145);
//! - the data and visible rects are kept in data coordinates and fitted into
//!   the widget preserving aspect ratio and centered
//!   (`fitInView(itemsBoundingRect, KeepAspectRatio)`, lines 223/233/243);
//! - dragging the inner rect is clamped by `_DraggableRectItem.itemChange`
//!   (lines 82-125): when the visible rect is no wider/taller than the data
//!   it stays inside the data; when it is wider/taller the data stays inside
//!   it;
//! - the drag emits `(left, top, width, height)` in data coordinates
//!   (`visibleRectDragged`, lines 116-121, 156-160), which silx forwards to
//!   `plot.setLimits(left, left+width, top, top+height)` (line 326).
//!
//! Pens/brushes mirror the silx class constants (lines 162-171): data area is
//! a light-gray fill with a white outline, the visible rect is a 2px blue
//! outline with no fill.
//!
//! The coordinate mapping, clamp, and hit-test are kept as pure functions
//! ([`DataRect`], [`RadarMapping`]) so they are unit-tested without a device.
//!
//! Integration: `ImageView` wires the full silx binding — it feeds the data
//! extent via [`RadarView::set_data_bounds`] (silx `_updateDataContent` from
//! `getDataRange`), syncs the viewport each frame via
//! [`RadarView::set_viewport_limits`] (silx `__setVisibleRectFromPlot`),
//! renders with [`RadarView::ui`], and forwards a viewport drag to
//! `Plot2D::set_limits` (silx `visibleRectDragged` → `setLimits`, lines
//! 245-359). The on-screen paint stays GPU-unverified.

use egui::{Color32, Pos2, Rect, Sense, Stroke, Vec2, pos2};

/// An axis-aligned rectangle in data coordinates, mirroring the silx
/// `(left, top, width, height)` tuple used by `setDataRect` / `setVisibleRect`
/// (`RadarView.py:226-243`).
///
/// As in `QGraphicsView`, `left` is the minimum x, `top` is the minimum y, and
/// y increases downward; `width` and `height` are non-negative extents. The
/// rectangle therefore spans `[left, left + width] × [top, top + height]`.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct DataRect {
    /// Minimum x (the left edge).
    pub left: f64,
    /// Minimum y (the top edge; y increases downward as in `QGraphicsView`).
    pub top: f64,
    /// Width (`x` extent); non-negative.
    pub width: f64,
    /// Height (`y` extent); non-negative.
    pub height: f64,
}

impl DataRect {
    /// A rectangle from its `(left, top, width, height)` data-space tuple,
    /// matching the silx `setDataRect` / `setVisibleRect` argument order.
    pub fn new(left: f64, top: f64, width: f64, height: f64) -> Self {
        Self {
            left,
            top,
            width,
            height,
        }
    }

    /// Build from inclusive bounds `[x_min, x_max] × [y_min, y_max]`.
    ///
    /// The bounds are normalized so `width`/`height` are non-negative (the
    /// smaller endpoint becomes `left`/`top`).
    pub fn from_bounds(x_min: f64, x_max: f64, y_min: f64, y_max: f64) -> Self {
        let (left, right) = if x_min <= x_max {
            (x_min, x_max)
        } else {
            (x_max, x_min)
        };
        let (top, bottom) = if y_min <= y_max {
            (y_min, y_max)
        } else {
            (y_max, y_min)
        };
        Self {
            left,
            top,
            width: right - left,
            height: bottom - top,
        }
    }

    /// The right edge (`left + width`).
    pub fn right(&self) -> f64 {
        self.left + self.width
    }

    /// The bottom edge (`top + height`).
    pub fn bottom(&self) -> f64 {
        self.top + self.height
    }

    /// The union (bounding box) of `self` and `other`.
    ///
    /// Mirrors `QGraphicsScene.itemsBoundingRect()`, which the silx widget
    /// fits into the view: it is the smallest rect covering both items
    /// (`RadarView.py:223`).
    pub fn union(&self, other: &DataRect) -> DataRect {
        let left = self.left.min(other.left);
        let top = self.top.min(other.top);
        let right = self.right().max(other.right());
        let bottom = self.bottom().max(other.bottom());
        DataRect {
            left,
            top,
            width: right - left,
            height: bottom - top,
        }
    }

    /// The limits tuple `(x_min, x_max, y_min, y_max)` silx forwards to
    /// `plot.setLimits` after a drag (`RadarView.py:326`).
    pub fn limits(&self) -> (f64, f64, f64, f64) {
        (self.left, self.right(), self.top, self.bottom())
    }
}

/// The aspect-preserving fit from data coordinates onto a widget pixel rect.
///
/// Mirrors `QGraphicsView.fitInView(rect, Qt.KeepAspectRatio)`
/// (`RadarView.py:223/233/243`): a single uniform `scale` maps the data
/// bounding rect into the widget rect, and the scaled content is centered so
/// the unused axis is padded symmetrically.
///
/// The mapping is `widget = data * scale + offset` per axis, with the same
/// `scale` on both axes. Both axes increase in the same direction (x
/// left→right, y top→bottom), matching the silx default `QGraphicsView`
/// orientation before any `scale(1, -1)` Y inversion.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct RadarMapping {
    /// Uniform data→pixel scale (pixels per data unit). Always finite and
    /// strictly positive.
    pub scale: f64,
    /// Pixel offset added after scaling the data x.
    pub offset_x: f64,
    /// Pixel offset added after scaling the data y.
    pub offset_y: f64,
}

impl RadarMapping {
    /// Compute the aspect-preserving, centered fit of `bounds` into the pixel
    /// `widget` rect.
    ///
    /// Degenerate `bounds` (zero or non-finite width/height) fall back to a
    /// `scale` of `1.0` so the mapping stays invertible; the content is then
    /// centered on the widget. This matches silx tolerating a 1×1 default data
    /// rect (`RadarView.py:179-184`) before any real data arrives.
    pub fn fit(bounds: &DataRect, widget: Rect) -> RadarMapping {
        let w = widget.width() as f64;
        let h = widget.height() as f64;

        let bw = bounds.width;
        let bh = bounds.height;

        let scale = if bw.is_finite() && bh.is_finite() && bw > 0.0 && bh > 0.0 {
            (w / bw).min(h / bh)
        } else if bw.is_finite() && bw > 0.0 {
            w / bw
        } else if bh.is_finite() && bh > 0.0 {
            h / bh
        } else {
            1.0
        };
        let scale = if scale.is_finite() && scale > 0.0 {
            scale
        } else {
            1.0
        };

        // Center the scaled content inside the widget rect.
        let content_w = bw.max(0.0) * scale;
        let content_h = bh.max(0.0) * scale;
        let pad_x = (w - content_w) * 0.5;
        let pad_y = (h - content_h) * 0.5;

        let left = widget.left() as f64;
        let top = widget.top() as f64;
        let offset_x = left + pad_x - bounds.left * scale;
        let offset_y = top + pad_y - bounds.top * scale;

        RadarMapping {
            scale,
            offset_x,
            offset_y,
        }
    }

    /// Map a data-space point to its widget pixel position.
    pub fn data_to_widget(&self, x: f64, y: f64) -> Pos2 {
        pos2(
            (x * self.scale + self.offset_x) as f32,
            (y * self.scale + self.offset_y) as f32,
        )
    }

    /// Map a widget pixel position back to data space (inverse of
    /// [`Self::data_to_widget`]).
    pub fn widget_to_data(&self, p: Pos2) -> (f64, f64) {
        (
            (p.x as f64 - self.offset_x) / self.scale,
            (p.y as f64 - self.offset_y) / self.scale,
        )
    }

    /// Map a data-space rectangle to its widget pixel rect.
    pub fn data_rect_to_widget(&self, r: &DataRect) -> Rect {
        let min = self.data_to_widget(r.left, r.top);
        let max = self.data_to_widget(r.right(), r.bottom());
        Rect::from_min_max(min, max)
    }
}

/// Clamp a candidate viewport top-left so the viewport stays consistent with
/// the data extent on drag.
///
/// Faithful to `_DraggableRectItem.itemChange` (`RadarView.py:82-110`): the
/// constraint rectangle is the data extent `[xMin, xMax] × [yMin, yMax]`.
///
/// Per axis, given the candidate position `pos` and the viewport extent
/// `size`:
/// - when the viewport is **no larger** than the data (`size <= xMax - xMin`),
///   the viewport is kept *inside* the data: `pos` is clamped to
///   `[xMin, xMax - size]`;
/// - when the viewport is **larger** than the data, the data is kept *inside*
///   the viewport: `pos` is clamped to `[xMax - size, xMin]` (note the
///   reversed bounds).
///
/// `viewport.width` / `viewport.height` are preserved; only `left` / `top`
/// move. Returns the clamped viewport.
pub fn clamp_viewport(viewport: DataRect, data_extent: &DataRect) -> DataRect {
    let x_min = data_extent.left;
    let x_max = data_extent.right();
    let y_min = data_extent.top;
    let y_max = data_extent.bottom();

    let left = clamp_axis(viewport.left, viewport.width, x_min, x_max);
    let top = clamp_axis(viewport.top, viewport.height, y_min, y_max);

    DataRect {
        left,
        top,
        width: viewport.width,
        height: viewport.height,
    }
}

/// Clamp one axis position, mirroring the per-axis branch of
/// `_DraggableRectItem.itemChange` (`RadarView.py:90-110`).
fn clamp_axis(pos: f64, size: f64, lo: f64, hi: f64) -> f64 {
    if size <= (hi - lo) {
        // Viewport fits within the data: keep it inside [lo, hi - size].
        if pos < lo {
            lo
        } else if pos > hi - size {
            hi - size
        } else {
            pos
        }
    } else {
        // Viewport is wider than the data: keep the data inside the viewport,
        // i.e. clamp to the reversed range [hi - size, lo].
        if pos > lo {
            lo
        } else if pos < hi - size {
            hi - size
        } else {
            pos
        }
    }
}

/// Whether the widget pixel point `p` lies inside the viewport pixel `rect`.
///
/// Used for the drag hit-test: silx makes only the visible rect movable
/// (`_DraggableRectItem`, `RadarView.py:49`), so a drag is captured only when
/// it starts on that rect.
pub fn point_in_rect(rect: Rect, p: Pos2) -> bool {
    rect.contains(p)
}

/// Light-gray fill of the data area (silx `_DATA_BRUSH = QColor("light gray")`,
/// `RadarView.py:163`).
const DATA_FILL: Color32 = Color32::from_rgb(0xD3, 0xD3, 0xD3);
/// White outline of the data area (silx `_DATA_PEN = QColor("white")`,
/// `RadarView.py:162`).
const DATA_STROKE: Color32 = Color32::WHITE;
/// Blue 2px outline of the visible rect (silx `_VISIBLE_PEN`,
/// `RadarView.py:168-169`).
const VISIBLE_STROKE: Color32 = Color32::BLUE;
/// Default widget size in points (silx `_PIXMAP_SIZE = 256`,
/// `RadarView.py:174`).
pub const DEFAULT_SIZE: f32 = 256.0;

/// A miniature overview of a 2D plot: the full data extent with a draggable
/// inner rectangle marking the current viewport.
///
/// Ports silx `RadarView` (`RadarView.py:139-360`). The widget keeps both
/// rectangles in data coordinates ([`DataRect`]); [`RadarView::ui`] fits them
/// into the allotted pixel rect preserving aspect ratio, paints the extent and
/// viewport, and — while the user drags the inner rect — updates the stored
/// viewport (clamped inside the extent) and returns the new limits so the
/// caller can pan the real plot.
#[derive(Clone, Debug)]
pub struct RadarView {
    /// The full data extent (silx `_dataRect`, set via `setDataRect`).
    pub data_extent: DataRect,
    /// The current visible viewport (silx `_visibleRect`, set via
    /// `setVisibleRect`).
    pub viewport: DataRect,
}

impl Default for RadarView {
    fn default() -> Self {
        // silx initializes both rects to a 1×1 unit square
        // (`RadarView.py:179-195`).
        Self {
            data_extent: DataRect::new(0.0, 0.0, 1.0, 1.0),
            viewport: DataRect::new(0.0, 0.0, 1.0, 1.0),
        }
    }
}

impl RadarView {
    /// A radar view with the given data extent and an initial viewport equal
    /// to the full extent.
    pub fn new(data_extent: DataRect) -> Self {
        Self {
            data_extent,
            viewport: data_extent,
        }
    }

    /// Set the data extent (silx `setDataRect`, `RadarView.py:226-233`).
    pub fn set_data_extent(&mut self, data_extent: DataRect) {
        self.data_extent = data_extent;
    }

    /// Set the data extent from inclusive bounds `[x_min, x_max] × [y_min,
    /// y_max]` (the form silx's `_updateDataContent` derives from
    /// `getDataRange`, `RadarView.py:333-336`).
    pub fn set_data_bounds(&mut self, x_min: f64, x_max: f64, y_min: f64, y_max: f64) {
        self.data_extent = DataRect::from_bounds(x_min, x_max, y_min, y_max);
    }

    /// Set the visible viewport (silx `setVisibleRect`,
    /// `RadarView.py:235-243`). This is the API path, so — like silx — it is
    /// *not* clamped to the data extent.
    pub fn set_viewport(&mut self, viewport: DataRect) {
        self.viewport = viewport;
    }

    /// Set the viewport from axis limits `[x_min, x_max] × [y_min, y_max]`, the
    /// form silx derives from the plot axes in `__setVisibleRectFromPlot`
    /// (`RadarView.py:250-252`).
    pub fn set_viewport_limits(&mut self, x_min: f64, x_max: f64, y_min: f64, y_max: f64) {
        self.viewport = DataRect::from_bounds(x_min, x_max, y_min, y_max);
    }

    /// Compute the current data→pixel mapping for an allotted `widget` rect.
    ///
    /// Fits the union of the data extent and the viewport (silx fits
    /// `itemsBoundingRect`, the bounding box of all items including the
    /// visible rect, `RadarView.py:223`).
    pub fn mapping(&self, widget: Rect) -> RadarMapping {
        let bounds = self.data_extent.union(&self.viewport);
        RadarMapping::fit(&bounds, widget)
    }

    /// Paint the overview into a `desired_size` region of `ui` and handle a
    /// drag of the inner viewport rect.
    ///
    /// While the user drags the inner rect, the stored [`viewport`](Self::viewport)
    /// is moved by the drag delta (in data space) and clamped inside the data
    /// extent ([`clamp_viewport`]); the new limits `(x_min, x_max, y_min,
    /// y_max)` are returned via [`RadarResponse::dragged_limits`] so the caller
    /// can pan the real plot. The viewport extent (width/height) is never
    /// changed by a drag, matching silx (only the position moves,
    /// `RadarView.py:241-242`).
    pub fn ui(&mut self, ui: &mut egui::Ui, desired_size: Vec2) -> RadarResponse {
        let (rect, response) = ui.allocate_exact_size(desired_size, Sense::drag());

        let mut dragged_limits = None;

        if response.dragged() {
            // Map the drag delta from pixels into data space and move the
            // viewport's top-left; the inverse of the uniform fit scale turns
            // a pixel delta into a data-space delta.
            let mapping = self.mapping(rect);
            let delta = response.drag_delta();
            let data_dx = delta.x as f64 / mapping.scale;
            let data_dy = delta.y as f64 / mapping.scale;

            let moved = DataRect {
                left: self.viewport.left + data_dx,
                top: self.viewport.top + data_dy,
                width: self.viewport.width,
                height: self.viewport.height,
            };
            let clamped = clamp_viewport(moved, &self.data_extent);
            if clamped != self.viewport {
                self.viewport = clamped;
                dragged_limits = Some(clamped.limits());
            }
        }

        if ui.is_rect_visible(rect) {
            // Recompute the mapping after any drag so the paint reflects the
            // updated viewport (the union bounds can change when the viewport
            // is larger than the extent).
            let mapping = self.mapping(rect);
            self.paint(ui, rect, &mapping);
        }

        RadarResponse {
            response,
            dragged_limits,
        }
    }

    /// Paint the data extent (gray fill, white outline) and the viewport (blue
    /// outline) into `rect` using `mapping`.
    fn paint(&self, ui: &egui::Ui, rect: Rect, mapping: &RadarMapping) {
        let painter = ui.painter_at(rect);

        let data_px = mapping.data_rect_to_widget(&self.data_extent);
        painter.rect_filled(data_px, 0.0, DATA_FILL);
        painter.rect_stroke(
            data_px,
            0.0,
            Stroke::new(1.0_f32, DATA_STROKE),
            egui::StrokeKind::Inside,
        );

        let view_px = mapping.data_rect_to_widget(&self.viewport);
        painter.rect_stroke(
            view_px,
            0.0,
            Stroke::new(2.0_f32, VISIBLE_STROKE),
            egui::StrokeKind::Inside,
        );
    }
}

/// The result of [`RadarView::ui`]: the egui [`Response`](egui::Response) plus
/// the new viewport limits when a drag moved the inner rect.
pub struct RadarResponse {
    /// The egui response of the allocated widget area.
    pub response: egui::Response,
    /// When the user dragged the viewport this frame, the new limits
    /// `(x_min, x_max, y_min, y_max)` in data coordinates, ready to forward to
    /// the real plot (silx `plot.setLimits(left, left+width, top, top+height)`,
    /// `RadarView.py:326`). `None` when no drag occurred.
    pub dragged_limits: Option<(f64, f64, f64, f64)>,
}

#[cfg(test)]
mod tests {
    use super::*;
    use egui::vec2;

    const EPS: f64 = 1e-9;

    fn widget_rect() -> Rect {
        Rect::from_min_size(pos2(0.0, 0.0), vec2(200.0, 200.0))
    }

    /// A 1:1 (square) data extent fitted into a square widget maps the extent
    /// to fill the whole widget, with the expected scale.
    #[test]
    fn square_extent_fills_square_widget() {
        let extent = DataRect::new(0.0, 0.0, 10.0, 10.0);
        let view = RadarView::new(extent);
        let mapping = view.mapping(widget_rect());

        assert!((mapping.scale - 20.0).abs() < EPS); // 200px / 10 data units
        let r = mapping.data_rect_to_widget(&extent);
        assert!((r.min.x - 0.0).abs() < 1e-4);
        assert!((r.min.y - 0.0).abs() < 1e-4);
        assert!((r.max.x - 200.0).abs() < 1e-4);
        assert!((r.max.y - 200.0).abs() < 1e-4);
    }

    /// data → widget → data round-trips exactly for the fit mapping.
    #[test]
    fn data_widget_data_round_trip() {
        let extent = DataRect::new(-3.0, 7.0, 5.0, 9.0);
        let view = RadarView::new(extent);
        let mapping = view.mapping(widget_rect());

        for &(x, y) in &[(-3.0, 7.0), (2.0, 16.0), (-1.0, 10.5), (-3.0, 16.0)] {
            let px = mapping.data_to_widget(x, y);
            let (bx, by) = mapping.widget_to_data(px);
            assert!((bx - x).abs() < 1e-3, "x round-trip {x} -> {bx}");
            assert!((by - y).abs() < 1e-3, "y round-trip {y} -> {by}");
        }
    }

    /// A viewport equal to the extent fills the mini-box exactly.
    #[test]
    fn viewport_equal_to_extent_fills_box() {
        let extent = DataRect::new(0.0, 0.0, 4.0, 4.0);
        let mut view = RadarView::new(extent);
        view.set_viewport(extent);
        let mapping = view.mapping(widget_rect());

        let data_px = mapping.data_rect_to_widget(&extent);
        let view_px = mapping.data_rect_to_widget(&view.viewport);
        assert!((data_px.min.x - view_px.min.x).abs() < 1e-4);
        assert!((data_px.min.y - view_px.min.y).abs() < 1e-4);
        assert!((data_px.max.x - view_px.max.x).abs() < 1e-4);
        assert!((data_px.max.y - view_px.max.y).abs() < 1e-4);
    }

    /// A non-matching (wide) extent is centered vertically in a square widget:
    /// the fit scale is driven by the wider axis, leaving symmetric top/bottom
    /// padding.
    #[test]
    fn aspect_fit_centers_non_matching_extent() {
        // 20 wide × 10 tall data into 200×200 widget: scale = min(10, 20) = 10.
        let extent = DataRect::new(0.0, 0.0, 20.0, 10.0);
        let view = RadarView::new(extent);
        let mapping = view.mapping(widget_rect());

        assert!((mapping.scale - 10.0).abs() < EPS);
        let r = mapping.data_rect_to_widget(&extent);
        // Full width used, height = 100px centered → 50px pad top and bottom.
        assert!((r.min.x - 0.0).abs() < 1e-4);
        assert!((r.max.x - 200.0).abs() < 1e-4);
        assert!((r.min.y - 50.0).abs() < 1e-4);
        assert!((r.max.y - 150.0).abs() < 1e-4);
        // Symmetric padding.
        assert!(((r.min.y - 0.0) - (200.0 - r.max.y)).abs() < 1e-4);
    }

    /// A tall extent (taller than wide) into a square widget is centered
    /// horizontally.
    #[test]
    fn aspect_fit_centers_tall_extent() {
        // 10 wide × 20 tall into 200×200: scale = min(20, 10) = 10.
        let extent = DataRect::new(0.0, 0.0, 10.0, 20.0);
        let view = RadarView::new(extent);
        let mapping = view.mapping(widget_rect());

        assert!((mapping.scale - 10.0).abs() < EPS);
        let r = mapping.data_rect_to_widget(&extent);
        assert!((r.min.y - 0.0).abs() < 1e-4);
        assert!((r.max.y - 200.0).abs() < 1e-4);
        assert!((r.min.x - 50.0).abs() < 1e-4);
        assert!((r.max.x - 150.0).abs() < 1e-4);
    }

    /// Dragging the viewport past the left/top edge clamps it to the data
    /// extent's minimum corner.
    #[test]
    fn clamp_past_min_edge() {
        let extent = DataRect::new(0.0, 0.0, 10.0, 10.0);
        // Viewport 2×2 dragged well past the top-left corner.
        let moved = DataRect::new(-5.0, -3.0, 2.0, 2.0);
        let clamped = clamp_viewport(moved, &extent);
        assert!((clamped.left - 0.0).abs() < EPS);
        assert!((clamped.top - 0.0).abs() < EPS);
        assert!((clamped.width - 2.0).abs() < EPS);
        assert!((clamped.height - 2.0).abs() < EPS);
    }

    /// Dragging the viewport past the right/bottom edge clamps it so its far
    /// edge sits on the extent's maximum corner.
    #[test]
    fn clamp_past_max_edge() {
        let extent = DataRect::new(0.0, 0.0, 10.0, 10.0);
        let moved = DataRect::new(20.0, 15.0, 2.0, 2.0);
        let clamped = clamp_viewport(moved, &extent);
        // left clamped to xMax - width = 10 - 2 = 8; top to 8.
        assert!((clamped.left - 8.0).abs() < EPS);
        assert!((clamped.top - 8.0).abs() < EPS);
    }

    /// A viewport that fits inside the extent and is already in-bounds is left
    /// unchanged by the clamp.
    #[test]
    fn clamp_in_bounds_is_identity() {
        let extent = DataRect::new(0.0, 0.0, 10.0, 10.0);
        let v = DataRect::new(3.0, 4.0, 2.0, 2.0);
        let clamped = clamp_viewport(v, &extent);
        assert_eq!(clamped, v);
    }

    /// A viewport *larger* than the extent is clamped so the data stays inside
    /// the viewport: the reversed-bounds branch keeps the extent enclosed.
    #[test]
    fn clamp_viewport_larger_than_extent() {
        let extent = DataRect::new(0.0, 0.0, 10.0, 10.0);
        // Viewport 20×20 (wider than the 10×10 extent).
        // Reversed range per axis: [xMax - size, xMin] = [10 - 20, 0] = [-10, 0].
        // Far below the range → clamp toward xMax - size = -10.
        let below = DataRect::new(-50.0, -50.0, 20.0, 20.0);
        let c1 = clamp_viewport(below, &extent);
        assert!((c1.left - (-10.0)).abs() < EPS);
        assert!((c1.top - (-10.0)).abs() < EPS);

        // Above the range (pos > xMin = 0) → clamp toward xMin = 0.
        let above = DataRect::new(5.0, 5.0, 20.0, 20.0);
        let c2 = clamp_viewport(above, &extent);
        assert!((c2.left - 0.0).abs() < EPS);
        assert!((c2.top - 0.0).abs() < EPS);

        // In the reversed range [-10, 0] → unchanged.
        let inside = DataRect::new(-5.0, -2.0, 20.0, 20.0);
        let c3 = clamp_viewport(inside, &extent);
        assert!((c3.left - (-5.0)).abs() < EPS);
        assert!((c3.top - (-2.0)).abs() < EPS);
    }

    /// `clamp_viewport` never resizes the viewport, only its position moves.
    #[test]
    fn clamp_preserves_size() {
        let extent = DataRect::new(0.0, 0.0, 10.0, 10.0);
        for moved in [
            DataRect::new(-5.0, -5.0, 3.0, 4.0),
            DataRect::new(50.0, 50.0, 3.0, 4.0),
            DataRect::new(-50.0, -50.0, 30.0, 40.0),
        ] {
            let c = clamp_viewport(moved, &extent);
            assert!((c.width - moved.width).abs() < EPS);
            assert!((c.height - moved.height).abs() < EPS);
        }
    }

    /// Hit-testing: a point inside the viewport pixel rect is detected, one
    /// outside is not.
    #[test]
    fn point_in_rect_hit_test() {
        let extent = DataRect::new(0.0, 0.0, 10.0, 10.0);
        let mut view = RadarView::new(extent);
        view.set_viewport(DataRect::new(2.0, 2.0, 4.0, 4.0));
        let mapping = view.mapping(widget_rect());
        let view_px = mapping.data_rect_to_widget(&view.viewport);

        let center = mapping.data_to_widget(4.0, 4.0);
        assert!(point_in_rect(view_px, center));

        let corner_outside = mapping.data_to_widget(0.5, 0.5);
        assert!(!point_in_rect(view_px, corner_outside));
    }

    /// `from_bounds` normalizes reversed endpoints into non-negative extents.
    #[test]
    fn from_bounds_normalizes_reversed() {
        let r = DataRect::from_bounds(7.0, -3.0, 16.0, 7.0);
        assert!((r.left - (-3.0)).abs() < EPS);
        assert!((r.top - 7.0).abs() < EPS);
        assert!((r.width - 10.0).abs() < EPS);
        assert!((r.height - 9.0).abs() < EPS);
    }

    /// `limits` returns the `(x_min, x_max, y_min, y_max)` tuple silx forwards
    /// to `setLimits`.
    #[test]
    fn limits_matches_setlimits_order() {
        let r = DataRect::new(2.0, 3.0, 4.0, 5.0);
        assert_eq!(r.limits(), (2.0, 6.0, 3.0, 8.0));
    }

    /// `union` covers both rects, matching `itemsBoundingRect`.
    #[test]
    fn union_covers_both() {
        let a = DataRect::new(0.0, 0.0, 10.0, 10.0);
        let b = DataRect::new(-5.0, 5.0, 20.0, 3.0);
        let u = a.union(&b);
        assert!((u.left - (-5.0)).abs() < EPS);
        assert!((u.top - 0.0).abs() < EPS);
        assert!((u.right() - 15.0).abs() < EPS);
        assert!((u.bottom() - 10.0).abs() < EPS);
    }

    /// A degenerate (zero-area) extent falls back to a unit scale rather than
    /// producing a non-finite mapping.
    #[test]
    fn degenerate_extent_falls_back_to_unit_scale() {
        let extent = DataRect::new(0.0, 0.0, 0.0, 0.0);
        let view = RadarView::new(extent);
        let mapping = view.mapping(widget_rect());
        assert!(mapping.scale.is_finite());
        assert!(mapping.scale > 0.0);
        assert!((mapping.scale - 1.0).abs() < EPS);
    }
}
