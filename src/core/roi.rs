//! Regions of interest (ROIs): rectangular, horizontal-band, and vertical-band
//! selections drawn over the data area with draggable edge handles.
//!
//! The geometry is data-space and the hit-testing / edge-move math is pure (no
//! egui input), so it is unit-testable; the widget wires pointer drags to
//! [`Roi::edge_at`] and [`Roi::move_edge`] and emits a change when an edge moves
//! (silx `RegionOfInterest`, `doc/design.md` §13 C3).

use egui::{Color32, Pos2, Rect};

use crate::core::items::LineStyle;
use crate::core::transform::Transform;

/// silx `RegionOfInterestManager._color` default (`rgba("red")`).
pub const DEFAULT_ROI_COLOR: Color32 = Color32::RED;

/// silx `RegionOfInterest._DEFAULT_LINEWIDTH` (`items/_roi_base.py:245`).
pub const DEFAULT_ROI_LINE_WIDTH: f32 = 1.0;

/// A draggable edge of an ROI.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum RoiEdge {
    /// Data `x` minimum (left).
    Left,
    /// Data `x` maximum (right).
    Right,
    /// Data `y` minimum (bottom of the data area).
    Bottom,
    /// Data `y` maximum (top of the data area).
    Top,
    /// Bottom-left corner (`x` min, `y` min); diagonal resize of [`Roi::Rect`].
    BottomLeft,
    /// Bottom-right corner (`x` max, `y` min); diagonal resize of [`Roi::Rect`].
    BottomRight,
    /// Top-left corner (`x` min, `y` max); diagonal resize of [`Roi::Rect`].
    TopLeft,
    /// Top-right corner (`x` max, `y` max); diagonal resize of [`Roi::Rect`].
    TopRight,
    /// Generic vertex handle at `index`; used by [`Roi::Point`], [`Roi::Line`],
    /// and [`Roi::Polygon`] variants.
    Vertex(usize),
}

/// A region of interest in data coordinates. Bounds are kept normalized
/// (`min ≤ max`) by [`Roi::move_edge`].
#[derive(Clone, Debug, PartialEq)]
pub enum Roi {
    /// Axis-aligned rectangle `x = (x_min, x_max)`, `y = (y_min, y_max)`.
    Rect { x: (f64, f64), y: (f64, f64) },
    /// Horizontal band `y = (y_min, y_max)` spanning the full X extent.
    HRange { y: (f64, f64) },
    /// Vertical band `x = (x_min, x_max)` spanning the full Y extent.
    VRange { x: (f64, f64) },
    /// A single horizontal line at data `y`, spanning the full X extent (silx
    /// `HorizontalLineROI`). Distinct from [`Roi::HRange`]: one position, one
    /// handle (silx's single `YMarker`), not a two-edge band. Its sole draggable
    /// edge is named [`RoiEdge::Bottom`] (the line's `y`).
    HLine { y: f64 },
    /// A single vertical line at data `x`, spanning the full Y extent (silx
    /// `VerticalLineROI`). Distinct from [`Roi::VRange`]: one position, one
    /// handle (silx's single `XMarker`). Its sole draggable edge is named
    /// [`RoiEdge::Left`] (the line's `x`).
    VLine { x: f64 },
    /// Single movable point.
    Point { x: f64, y: f64 },
    /// Line segment between two movable endpoints.
    Line { start: (f64, f64), end: (f64, f64) },
    /// Polygon with N movable vertices (requires at least 1 vertex; 0-vertex is a no-op for drawing).
    Polygon { vertices: Vec<(f64, f64)> },
    /// A point drawn as full-span cross-hairs (silx `CrossROI`). One movable
    /// center handle.
    Cross { center: (f64, f64) },
    /// Circle with a movable center and a perimeter radius handle (silx
    /// `CircleROI`).
    Circle { center: (f64, f64), radius: f64 },
    /// An oriented ellipse (silx `EllipseROI`): `center`, two perpendicular
    /// semi-axes `radii`, and `orientation` in radians. `radii.0` is the
    /// semi-axis along the `orientation` direction and `radii.1` the one
    /// perpendicular to it (`orientation + π/2`); `orientation == 0.0` is the
    /// axis-aligned case where `radii = (x_radius, y_radius)`. Movable center
    /// plus one perimeter handle per semi-axis, each of which also rotates the
    /// ellipse when dragged off-axis (silx axis anchors set radius + orientation).
    Ellipse {
        center: (f64, f64),
        radii: (f64, f64),
        orientation: f64,
    },
    /// An annular sector (silx `ArcROI`): the ring of central `radius` and radial
    /// thickness `weight` around `center`, swept from `start_angle` to `end_angle`
    /// (radians; if `start_angle > end_angle` the sweep is the other way). A
    /// full `2π` sweep is a circle/donut.
    ///
    /// Stored as silx's `(radius, weight)` (silx `_ArcGeometry`), NOT as
    /// `(inner, outer)`: the reported inner radius `radius − weight/2` is clamped
    /// at 0 for drawing/hit-testing ([`arc_inner_radius`]), but the stored weight
    /// keeps the true thickness even when that clamp fires (`weight > 2·radius`),
    /// so a follow-up drag recomputes from the real weight instead of a
    /// collapsed-to-0 inner (silx `getInnerRadius`, `_arc_roi.py:856-865`; R2-15).
    Arc {
        center: (f64, f64),
        radius: f64,
        weight: f64,
        start_angle: f64,
        end_angle: f64,
    },
    /// A rotatable rectangle (silx `BandROI`): the band of full `width` swept
    /// along the segment `begin → end`. `width` is the band's extent across the
    /// segment direction.
    Band {
        begin: (f64, f64),
        end: (f64, f64),
        width: f64,
    },
}

/// What a [`RoiHandle`] manipulates, mirroring the silx handle roles
/// (`items/_roi_base.py` `addHandle`/`addTranslateHandle`): a shape-editing
/// vertex (silx `"default"`, drawn as a filled square `"s"`), an edge limit on a
/// band, the shape center, or a translate-only handle (silx `"translate"`, drawn
/// as a `"+"`).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum HandleKind {
    /// A vertex that edits the shape when dragged (silx default `"s"` handle).
    Vertex,
    /// A band limit handle (the bottom/top of an `HRange`, left/right of a
    /// `VRange`).
    Edge,
    /// The shape center used as a label/anchor point.
    Center,
    /// A translate-only handle: dragging it moves the whole ROI (silx
    /// `addTranslateHandle`, `"+"`).
    Translate,
}

/// One draggable handle of a ROI in data space, with the role it plays (silx
/// `HandleBasedROI` markers). Pure geometry: no pointer/event state.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct RoiHandle {
    /// Data-space position of the handle.
    pub pos: [f64; 2],
    /// What the handle manipulates.
    pub kind: HandleKind,
}

/// The three parallel infinite lines describing a band ROI in silx
/// **UnboundedMode** (silx `BandGeometry.slope` / `intercept` / `edgesIntercept`,
/// `items/_band_roi.py:99-125`), produced by [`Roi::band_lines`]. A sum type so a
/// vertical band (`x = const`) and a sloped band (`y = slope·x + c`) each carry
/// only the fields that have meaning, rather than silx's `inf`-slope encoding
/// where the `intercept` field doubles as an X value.
#[derive(Clone, Copy, Debug, PartialEq)]
pub enum BandLines {
    /// A non-vertical band: every line is `y = slope · x + c`. `middle` is the
    /// `c` of the `begin → end` line; `edges` are the `c` of the two width-offset
    /// parallel lines.
    Sloped {
        /// Shared slope of all three lines.
        slope: f64,
        /// Y-intercept of the centre (`begin → end`) line.
        middle: f64,
        /// Y-intercepts of the two width-offset edge lines.
        edges: (f64, f64),
    },
    /// A vertical band (`begin.x == end.x`): every line is `x = const`. `middle`
    /// is the centre line's X; `edges` are the two width-offset edge Xs.
    Vertical {
        /// X of the centre (`begin → end`) line.
        middle: f64,
        /// X of the two width-offset edge lines.
        edges: (f64, f64),
    },
}

impl Roi {
    /// The screen rectangle this ROI draws into. Bands span the data area on
    /// their free axis.
    pub fn screen_rect(&self, t: &Transform) -> Rect {
        let area = t.area;
        match self {
            Roi::Rect { x, y } => {
                let a = t.data_to_pixel(x.0, y.0);
                let b = t.data_to_pixel(x.1, y.1);
                Rect::from_two_pos(a, b)
            }
            Roi::HRange { y } => {
                let py0 = t.data_to_pixel(t.x.min, y.0).y;
                let py1 = t.data_to_pixel(t.x.min, y.1).y;
                Rect::from_x_y_ranges(area.left()..=area.right(), py0.min(py1)..=py0.max(py1))
            }
            Roi::VRange { x } => {
                let px0 = t.data_to_pixel(x.0, t.y.min).x;
                let px1 = t.data_to_pixel(x.1, t.y.min).x;
                Rect::from_x_y_ranges(px0.min(px1)..=px0.max(px1), area.top()..=area.bottom())
            }
            // Single full-span line: a zero-thickness strip at the line's pixel.
            Roi::HLine { y } => {
                let py = t.data_to_pixel(t.x.min, *y).y;
                Rect::from_x_y_ranges(area.left()..=area.right(), py..=py)
            }
            Roi::VLine { x } => {
                let px = t.data_to_pixel(*x, t.y.min).x;
                Rect::from_x_y_ranges(px..=px, area.top()..=area.bottom())
            }
            Roi::Point { x, y } => {
                let p = t.data_to_pixel(*x, *y);
                Rect::from_center_size(p, egui::vec2(1.0, 1.0))
            }
            Roi::Line { start, end } => {
                let a = t.data_to_pixel(start.0, start.1);
                let b = t.data_to_pixel(end.0, end.1);
                Rect::from_two_pos(a, b)
            }
            Roi::Polygon { vertices } => {
                let mut rect = Rect::NOTHING;
                for &(x, y) in vertices {
                    let p = t.data_to_pixel(x, y);
                    if rect.is_negative() {
                        rect = Rect::from_center_size(p, egui::vec2(1.0, 1.0));
                    } else {
                        rect = rect.union(Rect::from_center_size(p, egui::vec2(1.0, 1.0)));
                    }
                }
                if rect.is_negative() { area } else { rect }
            }
            Roi::Cross { center } => {
                let p = t.data_to_pixel(center.0, center.1);
                Rect::from_center_size(p, egui::vec2(1.0, 1.0))
            }
            Roi::Circle { center, radius } => {
                // Bounding box of the data-space circle, mapped to screen.
                let a = t.data_to_pixel(center.0 - radius, center.1 - radius);
                let b = t.data_to_pixel(center.0 + radius, center.1 + radius);
                Rect::from_two_pos(a, b)
            }
            Roi::Ellipse {
                center,
                radii,
                orientation,
            } => {
                let (hx, hy) = ellipse_aabb_half_extents(*radii, *orientation);
                let a = t.data_to_pixel(center.0 - hx, center.1 - hy);
                let b = t.data_to_pixel(center.0 + hx, center.1 + hy);
                Rect::from_two_pos(a, b)
            }
            Roi::Arc {
                center,
                radius,
                weight,
                ..
            } => {
                // Bounding box of the outer circle, mapped to screen.
                let outer = arc_outer_radius(*radius, *weight);
                let a = t.data_to_pixel(center.0 - outer, center.1 - outer);
                let b = t.data_to_pixel(center.0 + outer, center.1 + outer);
                Rect::from_two_pos(a, b)
            }
            Roi::Band { .. } => {
                let mut rect = Rect::NOTHING;
                for &(x, y) in &band_corners(self).unwrap_or_default() {
                    let p = t.data_to_pixel(x, y);
                    if rect.is_negative() {
                        rect = Rect::from_center_size(p, egui::vec2(1.0, 1.0));
                    } else {
                        rect = rect.union(Rect::from_center_size(p, egui::vec2(1.0, 1.0)));
                    }
                }
                if rect.is_negative() { area } else { rect }
            }
        }
    }

    /// The draggable edges this ROI exposes.
    fn edges(&self) -> Vec<RoiEdge> {
        match self {
            // Four mid-edge handles (one axis each) plus four corner handles
            // (both axes) so the rectangle resizes left/right, up/down, and
            // diagonally. silx `RectangleROI` exposes the corners; the mid-edge
            // handles are an rsplot addition that preserves single-axis
            // resize alongside the diagonal corners.
            Roi::Rect { .. } => vec![
                RoiEdge::Left,
                RoiEdge::Right,
                RoiEdge::Bottom,
                RoiEdge::Top,
                RoiEdge::BottomLeft,
                RoiEdge::BottomRight,
                RoiEdge::TopLeft,
                RoiEdge::TopRight,
            ],
            Roi::HRange { .. } => vec![RoiEdge::Bottom, RoiEdge::Top],
            Roi::VRange { .. } => vec![RoiEdge::Left, RoiEdge::Right],
            // Single line: one full-span edge (the line's position axis).
            Roi::HLine { .. } => vec![RoiEdge::Bottom],
            Roi::VLine { .. } => vec![RoiEdge::Left],
            Roi::Point { .. } => vec![RoiEdge::Vertex(0)],
            Roi::Line { .. } => vec![RoiEdge::Vertex(0), RoiEdge::Vertex(1)],
            Roi::Polygon { vertices } => (0..vertices.len()).map(RoiEdge::Vertex).collect(),
            // Cross: a single center handle (silx CrossROI center handle).
            Roi::Cross { .. } => vec![RoiEdge::Vertex(0)],
            // Circle: center (0) + perimeter radius handle (1) — silx CircleROI.
            Roi::Circle { .. } => vec![RoiEdge::Vertex(0), RoiEdge::Vertex(1)],
            // Ellipse: center (0) + x-axis handle (1) + y-axis handle (2) —
            // silx EllipseROI center + two axis handles.
            Roi::Ellipse { .. } => {
                vec![RoiEdge::Vertex(0), RoiEdge::Vertex(1), RoiEdge::Vertex(2)]
            }
            // Arc: mid (0) + outer/weight (1) + start (2) + end (3) — silx ArcROI
            // shape handles (mid/weight/start/end). Index order matches
            // [`Roi::vertex_pixel`].
            Roi::Arc { .. } => (0..4).map(RoiEdge::Vertex).collect(),
            // Band: begin (0) + end (1) + width-up (2) + width-down (3) — silx
            // BandROI handles.
            Roi::Band { .. } => (0..4).map(RoiEdge::Vertex).collect(),
        }
    }

    /// Screen-space position of vertex `index` for the handle-based ROIs
    /// (Point/Line/Polygon/Cross/Circle/Ellipse).
    fn vertex_pixel(&self, t: &Transform, index: usize) -> Option<Pos2> {
        let (x, y) = match self {
            Roi::Point { x, y } if index == 0 => (*x, *y),
            Roi::Line { start, end } => match index {
                0 => *start,
                1 => *end,
                _ => return None,
            },
            Roi::Polygon { vertices } => vertices.get(index).copied()?,
            Roi::Cross { center } if index == 0 => *center,
            Roi::Circle { center, radius } => match index {
                0 => *center,
                // Perimeter handle to the right of the center (silx places it
                // at center + (radius, 0)).
                1 => (center.0 + radius, center.1),
                _ => return None,
            },
            Roi::Ellipse {
                center,
                radii,
                orientation,
            } => {
                let (c, s) = (orientation.cos(), orientation.sin());
                match index {
                    0 => *center,
                    // axis0 handle at center + radii.0·(cosθ, sinθ).
                    1 => (center.0 + radii.0 * c, center.1 + radii.0 * s),
                    // axis1 handle, perpendicular: center + radii.1·(−sinθ, cosθ).
                    2 => (center.0 - radii.1 * s, center.1 + radii.1 * c),
                    _ => return None,
                }
            }
            // Arc shape vertices: 0=mid, 1=outer/weight, 2=start, 3=end.
            Roi::Arc { .. } => arc_vertex_pos(self, index)?,
            // Band shape vertices: 0=begin, 1=end, 2=width-up, 3=width-down.
            Roi::Band { .. } => band_vertex_pos(self, index)?,
            _ => return None,
        };
        Some(t.data_to_pixel(x, y))
    }

    /// Screen-space midpoints of this ROI's draggable edges, for drawing handle
    /// marks (one per edge, in `Roi::edges` order).
    ///
    /// Each handle is located by the *data* point its [`RoiEdge`] names (mapped
    /// through the transform), not by the screen-rect's geometric corners — so,
    /// like [`Roi::edge_at`] / [`Roi::move_edge`], it stays correct under an
    /// inverted axis (where e.g. the data `Top` = y.max edge is drawn at the
    /// bottom of the screen).
    pub fn handle_centers(&self, t: &Transform) -> Vec<Pos2> {
        let mid = |a: f64, b: f64| (a + b) * 0.5;
        let center = || self.screen_rect(t).center();
        self.edges()
            .iter()
            .map(|edge| match self {
                Roi::Rect { x, y } => {
                    let (dx, dy) = match edge {
                        RoiEdge::Left => (x.0, mid(y.0, y.1)),
                        RoiEdge::Right => (x.1, mid(y.0, y.1)),
                        RoiEdge::Bottom => (mid(x.0, x.1), y.0),
                        RoiEdge::Top => (mid(x.0, x.1), y.1),
                        RoiEdge::BottomLeft => (x.0, y.0),
                        RoiEdge::BottomRight => (x.1, y.0),
                        RoiEdge::TopLeft => (x.0, y.1),
                        RoiEdge::TopRight => (x.1, y.1),
                        RoiEdge::Vertex(_) => (mid(x.0, x.1), mid(y.0, y.1)),
                    };
                    t.data_to_pixel(dx, dy)
                }
                // HRange spans the full width: handles sit at the area's
                // horizontal centre, at the data y of each edge.
                Roi::HRange { y } => {
                    let cx = (t.area.left() + t.area.right()) * 0.5;
                    let dy = match edge {
                        RoiEdge::Bottom => y.0,
                        RoiEdge::Top => y.1,
                        _ => mid(y.0, y.1),
                    };
                    egui::pos2(cx, t.data_to_pixel(t.x.min, dy).y)
                }
                Roi::VRange { x } => {
                    let cy = (t.area.top() + t.area.bottom()) * 0.5;
                    let dx = match edge {
                        RoiEdge::Left => x.0,
                        RoiEdge::Right => x.1,
                        _ => mid(x.0, x.1),
                    };
                    egui::pos2(t.data_to_pixel(dx, t.y.min).x, cy)
                }
                // Single full-span line: one handle at the area's centre on the
                // free axis, at the line's data position.
                Roi::HLine { y } => {
                    let cx = (t.area.left() + t.area.right()) * 0.5;
                    egui::pos2(cx, t.data_to_pixel(t.x.min, *y).y)
                }
                Roi::VLine { x } => {
                    let cy = (t.area.top() + t.area.bottom()) * 0.5;
                    egui::pos2(t.data_to_pixel(*x, t.y.min).x, cy)
                }
                // Vertex-handled shapes: each edge is a stored vertex.
                _ => match edge {
                    RoiEdge::Vertex(n) => self.vertex_pixel(t, *n).unwrap_or_else(center),
                    _ => center(),
                },
            })
            .collect()
    }

    /// The edge under `cursor` (screen pixels) within `grab_px`, or `None`.
    /// When several edges are in range, the perpendicularly-closest one wins.
    pub fn edge_at(&self, t: &Transform, cursor: Pos2, grab_px: f32) -> Option<RoiEdge> {
        match self {
            Roi::Point { .. }
            | Roi::Line { .. }
            | Roi::Polygon { .. }
            | Roi::Cross { .. }
            | Roi::Circle { .. }
            | Roi::Ellipse { .. }
            | Roi::Arc { .. }
            | Roi::Band { .. } => {
                let mut best: Option<(RoiEdge, f32)> = None;
                for edge in self.edges() {
                    if let RoiEdge::Vertex(n) = edge
                        && let Some(p) = self.vertex_pixel(t, n)
                    {
                        let dist = cursor.distance(p);
                        if dist <= grab_px && best.is_none_or(|(_, d)| dist < d) {
                            best = Some((edge, dist));
                        }
                    }
                }
                best.map(|(e, _)| e)
            }
            _ => {
                // Rect, HRange, VRange: edge detection by *data* identity.
                //
                // Each [`RoiEdge`] names a data edge (`Left`=x.min, `Right`=x.max,
                // `Bottom`=y.min, `Top`=y.max) — that is how [`Roi::move_edge`]
                // applies it. So `edge_at` must locate each edge by its data
                // identity too: compute the data edge's screen coordinate through
                // the transform rather than reading a geometric screen top/left.
                // Under an inverted axis (e.g. an image plot, where Y is flipped
                // so data y.max sits at the *bottom* of the screen) the two differ
                // — a screen-geometry label would map a grab to the opposite data
                // edge, collapsing a rectangle on corner/edge drag. Deriving the
                // label from data identity keeps `edge_at` and `move_edge`
                // consistent on every axis orientation.
                let area = t.area;
                // Screen coords of each present data edge (x: Left/Right, y:
                // Bottom/Top), plus the screen-space spans the perpendicular
                // probe must fall within.
                let (lx, rx, by, ty, x_span, y_span) = match self {
                    Roi::Rect { x, y } => {
                        let lx = t.data_to_pixel(x.0, y.0).x;
                        let rx = t.data_to_pixel(x.1, y.0).x;
                        let by = t.data_to_pixel(x.0, y.0).y;
                        let ty = t.data_to_pixel(x.0, y.1).y;
                        (
                            Some(lx),
                            Some(rx),
                            Some(by),
                            Some(ty),
                            (lx.min(rx), lx.max(rx)),
                            (ty.min(by), ty.max(by)),
                        )
                    }
                    Roi::HRange { y } => {
                        let by = t.data_to_pixel(t.x.min, y.0).y;
                        let ty = t.data_to_pixel(t.x.min, y.1).y;
                        (
                            None,
                            None,
                            Some(by),
                            Some(ty),
                            (area.left(), area.right()),
                            (ty.min(by), ty.max(by)),
                        )
                    }
                    Roi::VRange { x } => {
                        let lx = t.data_to_pixel(x.0, t.y.min).x;
                        let rx = t.data_to_pixel(x.1, t.y.min).x;
                        (
                            Some(lx),
                            Some(rx),
                            None,
                            None,
                            (lx.min(rx), lx.max(rx)),
                            (area.top(), area.bottom()),
                        )
                    }
                    // Single horizontal line: its sole edge is `Bottom` at the
                    // line's screen y, grabbable anywhere across the full width.
                    Roi::HLine { y } => {
                        let py = t.data_to_pixel(t.x.min, *y).y;
                        (
                            None,
                            None,
                            Some(py),
                            None,
                            (area.left(), area.right()),
                            (py, py),
                        )
                    }
                    // Single vertical line: its sole edge is `Left` at the line's
                    // screen x, grabbable anywhere across the full height.
                    Roi::VLine { x } => {
                        let px = t.data_to_pixel(*x, t.y.min).x;
                        (
                            Some(px),
                            None,
                            None,
                            None,
                            (px, px),
                            (area.top(), area.bottom()),
                        )
                    }
                    _ => unreachable!(
                        "outer match restricts this arm to Rect/HRange/VRange/HLine/VLine"
                    ),
                };
                // Corner handles (Rect only) take priority: a cursor near a
                // corner is also near both adjoining edges, so resolve corners
                // first by Euclidean distance to the corner point. The closest
                // in-range corner wins, giving diagonal resize precedence over
                // single-axis edge resize at the rectangle's corners.
                let corner_pos = |edge: RoiEdge| -> Option<Pos2> {
                    Some(match edge {
                        RoiEdge::BottomLeft => egui::pos2(lx?, by?),
                        RoiEdge::BottomRight => egui::pos2(rx?, by?),
                        RoiEdge::TopLeft => egui::pos2(lx?, ty?),
                        RoiEdge::TopRight => egui::pos2(rx?, ty?),
                        _ => return None,
                    })
                };
                let mut best_corner: Option<(RoiEdge, f32)> = None;
                for edge in self.edges() {
                    if let Some(corner) = corner_pos(edge) {
                        let dist = cursor.distance(corner);
                        if dist <= grab_px && best_corner.is_none_or(|(_, d)| dist < d) {
                            best_corner = Some((edge, dist));
                        }
                    }
                }
                if let Some((edge, _)) = best_corner {
                    return Some(edge);
                }
                let (x_lo, x_hi) = x_span;
                let (y_lo, y_hi) = y_span;
                let mut best: Option<(RoiEdge, f32)> = None;
                for edge in self.edges() {
                    let dist = match edge {
                        // Vertical edges: cursor must be within the y span.
                        RoiEdge::Left | RoiEdge::Right => {
                            if cursor.y < y_lo - grab_px || cursor.y > y_hi + grab_px {
                                continue;
                            }
                            let ex = if edge == RoiEdge::Left { lx } else { rx };
                            match ex {
                                Some(ex) => (cursor.x - ex).abs(),
                                None => continue,
                            }
                        }
                        // Horizontal edges: cursor must be within the x span.
                        RoiEdge::Bottom | RoiEdge::Top => {
                            if cursor.x < x_lo - grab_px || cursor.x > x_hi + grab_px {
                                continue;
                            }
                            let ey = if edge == RoiEdge::Top { ty } else { by };
                            match ey {
                                Some(ey) => (cursor.y - ey).abs(),
                                None => continue,
                            }
                        }
                        // Corners handled above; vertices do not apply here.
                        _ => continue,
                    };
                    if dist <= grab_px && best.is_none_or(|(_, d)| dist < d) {
                        best = Some((edge, dist));
                    }
                }
                best.map(|(edge, _)| edge)
            }
        }
    }

    /// Move `edge` to the data point `data = (x, y)`, clamping so the ROI stays
    /// normalized (`min ≤ max`). Edges that do not apply to this ROI kind are
    /// ignored.
    pub fn move_edge(&mut self, edge: RoiEdge, data: (f64, f64)) {
        let (dx, dy) = data;
        match self {
            // silx `RectangleROI.handleDragUpdated`: each handle is paired with
            // its *fixed opposite* corner/edge, and the bounds are rebuilt from
            // the two via min/max (`_setBound`). A handle dragged past its
            // opposite therefore flips (the rectangle stays non-degenerate and
            // keeps following the cursor) instead of collapsing to zero size.
            // The bound being moved keeps its old opposite as the anchor; one
            // uniform rule for corners and rsplot's extra side handles, so no
            // boundary is special-cased.
            Roi::Rect { x, y } => {
                let (x0, x1, y0, y1) = (x.0, x.1, y.0, y.1);
                match edge {
                    RoiEdge::Left => *x = (dx.min(x1), dx.max(x1)),
                    RoiEdge::Right => *x = (dx.min(x0), dx.max(x0)),
                    RoiEdge::Bottom => *y = (dy.min(y1), dy.max(y1)),
                    RoiEdge::Top => *y = (dy.min(y0), dy.max(y0)),
                    RoiEdge::BottomLeft => {
                        *x = (dx.min(x1), dx.max(x1));
                        *y = (dy.min(y1), dy.max(y1));
                    }
                    RoiEdge::BottomRight => {
                        *x = (dx.min(x0), dx.max(x0));
                        *y = (dy.min(y1), dy.max(y1));
                    }
                    RoiEdge::TopLeft => {
                        *x = (dx.min(x1), dx.max(x1));
                        *y = (dy.min(y0), dy.max(y0));
                    }
                    RoiEdge::TopRight => {
                        *x = (dx.min(x0), dx.max(x0));
                        *y = (dy.min(y0), dy.max(y0));
                    }
                    RoiEdge::Vertex(_) => {}
                }
            }
            Roi::HRange { y } => match edge {
                RoiEdge::Bottom => *y = (dy.min(y.1), dy.max(y.1)),
                RoiEdge::Top => *y = (dy.min(y.0), dy.max(y.0)),
                _ => {}
            },
            Roi::VRange { x } => match edge {
                RoiEdge::Left => *x = (dx.min(x.1), dx.max(x.1)),
                RoiEdge::Right => *x = (dx.min(x.0), dx.max(x.0)),
                _ => {}
            },
            // Single line: the sole edge sets the line's scalar position (silx
            // `HorizontalLineROI.setPosition` / `VerticalLineROI.setPosition`).
            Roi::HLine { y } => {
                if let RoiEdge::Bottom = edge {
                    *y = dy;
                }
            }
            Roi::VLine { x } => {
                if let RoiEdge::Left = edge {
                    *x = dx;
                }
            }
            Roi::Point { x, y } => {
                if let RoiEdge::Vertex(0) = edge {
                    *x = dx;
                    *y = dy;
                }
            }
            Roi::Line { start, end } => match edge {
                RoiEdge::Vertex(0) => *start = (dx, dy),
                RoiEdge::Vertex(1) => *end = (dx, dy),
                _ => {}
            },
            Roi::Polygon { vertices } => {
                if let RoiEdge::Vertex(n) = edge
                    && let Some(v) = vertices.get_mut(n)
                {
                    *v = (dx, dy);
                }
            }
            Roi::Cross { center } => {
                if let RoiEdge::Vertex(0) = edge {
                    *center = (dx, dy);
                }
            }
            Roi::Circle { center, radius } => match edge {
                // Center handle translates the whole circle.
                RoiEdge::Vertex(0) => *center = (dx, dy),
                // Perimeter handle sets the radius to the distance from the
                // center (silx `setRadius(norm(center - current))`).
                RoiEdge::Vertex(1) => {
                    let (ex, ey) = (dx - center.0, dy - center.1);
                    *radius = (ex * ex + ey * ey).sqrt();
                }
                _ => {}
            },
            Roi::Ellipse {
                center,
                radii,
                orientation,
            } => match edge {
                // Center handle translates the whole ellipse.
                RoiEdge::Vertex(0) => *center = (dx, dy),
                // axis0 handle: set semi-axis 0 to the cursor distance and rotate
                // so axis0 points at the cursor (silx `EllipseROI.handleDragUpdated`
                // axis anchors set both radius and orientation).
                RoiEdge::Vertex(1) => {
                    let (ex, ey) = (dx - center.0, dy - center.1);
                    radii.0 = ex.hypot(ey);
                    *orientation = ey.atan2(ex);
                }
                // axis1 handle: set semi-axis 1; axis1 is perpendicular to
                // orientation, so orientation is the cursor angle minus π/2.
                RoiEdge::Vertex(2) => {
                    let (ex, ey) = (dx - center.0, dy - center.1);
                    radii.1 = ex.hypot(ey);
                    *orientation = ey.atan2(ex) - std::f64::consts::FRAC_PI_2;
                }
                _ => {}
            },
            // Arc handle drag — PolarMode editing, faithful to our polar
            // `{center, radius, weight, start_angle, end_angle}` representation
            // (silx `ArcROI.handleDragUpdated` PolarMode branch). silx's default
            // ThreePointMode (a circumcircle through three start/mid/end control
            // points) needs a point-based geometry we do not store, so PolarMode
            // is the faithful match for our model.
            Roi::Arc {
                center,
                radius,
                weight,
                start_angle,
                end_angle,
            } => {
                let (cx, cy) = *center;
                match edge {
                    // Mid handle (Vertex 0) → central radius, conserving the
                    // thickness (silx `withRadius`: weight kept). Because weight
                    // is stored directly it survives even when a prior drag drove
                    // the reported inner radius to 0 (R2-15).
                    RoiEdge::Vertex(0) => {
                        *radius = (dx - cx).hypot(dy - cy);
                    }
                    // Weight handle (Vertex 1) → thickness, symmetric about the
                    // central radius (silx `_getWeightFromHandle`:
                    // `weight = 2·|d − radius|`, `d = |center − handle|`).
                    RoiEdge::Vertex(1) => {
                        let d = (dx - cx).hypot(dy - cy);
                        *weight = 2.0 * (d - *radius).abs();
                    }
                    // Start / end handles (Vertex 2 / 3) → sweep angles (silx
                    // `withStartAngle` / `withEndAngle`), accumulated with the
                    // ±180° coherency rule so crossing the atan2 branch cut
                    // never flips the sweep by ~2π.
                    RoiEdge::Vertex(2) => {
                        *start_angle = coherent_angle(*start_angle, (dy - cy).atan2(dx - cx));
                    }
                    RoiEdge::Vertex(3) => {
                        *end_angle = coherent_angle(*end_angle, (dy - cy).atan2(dx - cx));
                    }
                    _ => {}
                }
            }
            // Band handle drag (silx `BandROI.handleDragUpdated`): the begin/end
            // handles set the segment endpoints; the two width handles set the
            // band width from the handle's signed projection onto the band
            // normal (silx `__handleWidthUp/DownConstraint`: the constrained
            // handle sits at `center ± offset·normal` with `offset = max(0,
            // ±normal·(p − center))`, and the width is `2·offset`). The
            // translate-center handle is handled by the ROI body-drag path, not
            // here.
            Roi::Band { begin, end, width } => match edge {
                RoiEdge::Vertex(0) => *begin = (dx, dy),
                RoiEdge::Vertex(1) => *end = (dx, dy),
                RoiEdge::Vertex(2) | RoiEdge::Vertex(3) => {
                    let center = ((begin.0 + end.0) * 0.5, (begin.1 + end.1) * 0.5);
                    let n = band_normal(*begin, *end);
                    let mut proj = n.0 * (dx - center.0) + n.1 * (dy - center.1);
                    // The down handle measures the opposite side of the normal.
                    if let RoiEdge::Vertex(3) = edge {
                        proj = -proj;
                    }
                    *width = 2.0 * proj.max(0.0);
                }
                _ => {}
            },
        }
    }

    /// The infinite-line geometry of a band ROI in silx **UnboundedMode** (silx
    /// `BandGeometry.slope` / `intercept` / `edgesIntercept`,
    /// `items/_band_roi.py:99-125`): the band drawn as three parallel infinite
    /// lines — a middle line through `begin → end` plus the two width edges —
    /// instead of the bounded corner polygon (`band_corners` / the default
    /// BoundedMode). Returns `None` for a non-band ROI.
    ///
    /// Where silx encodes a vertical band with an `inf` slope and reuses the
    /// `intercept` field as an X value, this returns a [`BandLines`] sum type so
    /// each field has a single meaning.
    pub fn band_lines(&self) -> Option<BandLines> {
        let Roi::Band { begin, end, width } = self else {
            return None;
        };
        let n = band_normal(*begin, *end);
        let off = (0.5 * width * n.0, 0.5 * width * n.1);
        // Vertical band (silx `begin.x == end.x`): edges are X-offset verticals.
        if begin.0 == end.0 {
            return Some(BandLines::Vertical {
                middle: begin.0,
                edges: (begin.0 - off.0, begin.0 + off.0),
            });
        }
        let slope = (end.1 - begin.1) / (end.0 - begin.0);
        let middle = begin.1 - slope * begin.0;
        let edges = (
            begin.1 - off.1 - slope * (begin.0 - off.0),
            begin.1 + off.1 - slope * (begin.0 + off.0),
        );
        Some(BandLines::Sloped {
            slope,
            middle,
            edges,
        })
    }

    /// The three line segments an **UnboundedMode** band draws, each spanned
    /// across the visible data extents `x_range` / `y_range` (silx renders the
    /// [`band_lines`](Self::band_lines) as full-width lines clipped to the plot):
    /// `[centre, edge0, edge1]`, every entry a `[start, end]` data-space pair.
    /// A sloped band is evaluated at the x-extents; a vertical band is drawn at
    /// the y-extents. Returns `None` for a non-band ROI. Pure (data-space only),
    /// so the spanned geometry is unit-testable without a `Transform`.
    #[must_use]
    pub fn band_unbounded_segments(
        &self,
        x_range: (f64, f64),
        y_range: (f64, f64),
    ) -> Option<[[(f64, f64); 2]; 3]> {
        Some(match self.band_lines()? {
            BandLines::Sloped {
                slope,
                middle,
                edges,
            } => {
                let (x0, x1) = x_range;
                let seg = |c: f64| [(x0, slope * x0 + c), (x1, slope * x1 + c)];
                [seg(middle), seg(edges.0), seg(edges.1)]
            }
            BandLines::Vertical { middle, edges } => {
                let (y0, y1) = y_range;
                let seg = |x: f64| [(x, y0), (x, y1)];
                [seg(middle), seg(edges.0), seg(edges.1)]
            }
        })
    }

    /// Test whether the data-space point `pos = (x, y)` is inside this ROI.
    ///
    /// Each variant mirrors the matching silx `RegionOfInterest.contains`
    /// (`items/roi.py`):
    /// - `Rect`/`HRange`/`VRange`: inclusive axis-aligned-bounding-box test
    ///   (silx `RectangleROI` via `_BoundingBox.contains`); a band ignores the
    ///   axis it spans.
    /// - `Point`: exact coordinate equality (`PointROI`).
    /// - `Cross`: on either crosshair, i.e. `x == cx || y == cy` (`CrossROI`).
    /// - `Line`: the segment intersects the unit square whose lower-left corner
    ///   is `pos` (`LineROI._intersects_unit_square`).
    /// - `Polygon`: even-odd ray-cast crossing test (`Polygon.is_inside`).
    /// - `Circle`: `dist(pos, center) <= radius` (`CircleROI`).
    /// - `Ellipse`: project `pos − center` onto the ellipse's own axes (rotate by
    ///   `−orientation`) and test `(x'/radii.0)² + (y'/radii.1)² <= 1` — the
    ///   oriented form of `EllipseROI.contains` (silx tests against the major-axis
    ///   angle; here `radii.0`/`radii.1` already are the axis0/axis1 semi-axes).
    /// - `Arc`: inside the `[inner, outer]` radius ring AND within the angular
    ///   sweep (`ArcROI._arc_roi.py`).
    /// - `Band`: point-in-the-rotated-rectangle of the four band corners
    ///   (`BandROI` via `Polygon.is_inside`).
    pub fn contains(&self, pos: (f64, f64)) -> bool {
        let (x, y) = pos;
        match self {
            Roi::Rect {
                x: (x0, x1),
                y: (y0, y1),
            } => x >= *x0 && x <= *x1 && y >= *y0 && y <= *y1,
            // A band ignores the axis it spans across.
            Roi::HRange { y: (y0, y1) } => y >= *y0 && y <= *y1,
            Roi::VRange { x: (x0, x1) } => x >= *x0 && x <= *x1,
            // Single line: exact position match on the bounded axis (silx
            // `HorizontalLineROI.contains` `positions[:, 1] == roi_x`).
            Roi::HLine { y: ly } => y == *ly,
            Roi::VLine { x: lx } => x == *lx,
            Roi::Point { x: px, y: py } => x == *px && y == *py,
            Roi::Cross { center } => x == center.0 || y == center.1,
            Roi::Line { start, end } => {
                // silx `LineROI.contains` first filters positions outside the
                // segment endpoints' axis-aligned bounding box (inclusive on
                // all four bounds — `_BoundingBox.from_points` +
                // `min_x <= x <= max_x & min_y <= y <= max_y`,
                // items/roi.py:314-332, image/_boundingbox.py:95-105), and only
                // then runs the unit-square test. Without the gate the unit
                // square anchored at the query point's lower-left still crosses
                // the segment for points up to one unit below/left of it, past
                // the endpoints.
                let (bx0, bx1) = (start.0.min(end.0), start.0.max(end.0));
                let (by0, by1) = (start.1.min(end.1), start.1.max(end.1));
                (bx0..=bx1).contains(&x)
                    && (by0..=by1).contains(&y)
                    && segment_intersects_unit_square(*start, *end, pos)
            }
            Roi::Polygon { vertices } => point_in_polygon(vertices, pos),
            Roi::Circle { center, radius } => {
                let (dx, dy) = (x - center.0, y - center.1);
                (dx * dx + dy * dy).sqrt() <= *radius
            }
            Roi::Ellipse {
                center,
                radii,
                orientation,
            } => {
                let (a, b) = *radii;
                if a <= 0.0 || b <= 0.0 {
                    return false;
                }
                let (dx, dy) = (x - center.0, y - center.1);
                let (c, s) = (orientation.cos(), orientation.sin());
                // Rotate into the ellipse's own frame: x' along axis0 (radii.0),
                // y' along axis1 (radii.1).
                let xr = dx * c + dy * s;
                let yr = -dx * s + dy * c;
                (xr * xr) / (a * a) + (yr * yr) / (b * b) <= 1.0
            }
            Roi::Arc {
                center,
                radius,
                weight,
                start_angle,
                end_angle,
            } => arc_contains(
                *center,
                arc_inner_radius(*radius, *weight),
                arc_outer_radius(*radius, *weight),
                *start_angle,
                *end_angle,
                pos,
            ),
            // Band containment is point-in-the-rotated-rectangle of the four
            // corners (silx `BandGeometry.contains` → `Polygon.is_inside`).
            Roi::Band { .. } => match band_corners(self) {
                Some(corners) => point_in_polygon(&corners, pos),
                None => false,
            },
        }
    }

    /// The draggable handles this ROI exposes, in data space (silx
    /// `HandleBasedROI` markers; `items/_roi_base.py`). Pure geometry: no
    /// pointer/event state. Handle roles mirror silx (`addHandle` "default"
    /// vertices, `addTranslateHandle` "+" translate handles).
    pub fn handles(&self) -> Vec<RoiHandle> {
        let v = |p: (f64, f64)| RoiHandle {
            pos: [p.0, p.1],
            kind: HandleKind::Vertex,
        };
        let center = |p: (f64, f64)| RoiHandle {
            pos: [p.0, p.1],
            kind: HandleKind::Center,
        };
        let translate = |p: (f64, f64)| RoiHandle {
            pos: [p.0, p.1],
            kind: HandleKind::Translate,
        };
        let edge = |p: (f64, f64)| RoiHandle {
            pos: [p.0, p.1],
            kind: HandleKind::Edge,
        };
        match self {
            // RectangleROI: 4 corner vertices + a translate center
            // (silx `addHandle` ×4 + `addTranslateHandle`).
            Roi::Rect {
                x: (x0, x1),
                y: (y0, y1),
            } => vec![
                v((*x0, *y0)),
                v((*x1, *y0)),
                v((*x0, *y1)),
                v((*x1, *y1)),
                translate(((x0 + x1) * 0.5, (y0 + y1) * 0.5)),
            ],
            // HorizontalRangeROI: min/max edge handles + a center handle.
            Roi::HRange { y: (y0, y1) } => vec![
                edge((0.0, *y0)),
                edge((0.0, *y1)),
                center((0.0, (y0 + y1) * 0.5)),
            ],
            // VerticalRangeROI analogue.
            Roi::VRange { x: (x0, x1) } => vec![
                edge((*x0, 0.0)),
                edge((*x1, 0.0)),
                center(((x0 + x1) * 0.5, 0.0)),
            ],
            // Horizontal/VerticalLineROI: one draggable handle at the line
            // position (silx's single Y/X marker at x=0 / y=0).
            Roi::HLine { y } => vec![v((0.0, *y))],
            Roi::VLine { x } => vec![v((*x, 0.0))],
            // PointROI: a single vertex handle.
            Roi::Point { x, y } => vec![v((*x, *y))],
            // CrossROI: a single square drag handle at the center. silx builds
            // it with `addHandle()` (default role -> symbol "s" square,
            // items/roi.py:147); the cross-hairs themselves are the separate
            // X/Y markers drawn by `draw_roi`.
            Roi::Cross { center: c } => vec![v(*c)],
            // LineROI: 2 endpoint vertices + a translate center handle.
            Roi::Line { start, end } => vec![
                v(*start),
                v(*end),
                translate(((start.0 + end.0) * 0.5, (start.1 + end.1) * 0.5)),
            ],
            // PolygonROI: N vertices + a translate center handle (silx places
            // the translate handle at the first vertex; empty polygon has none).
            Roi::Polygon { vertices } => {
                let mut hs: Vec<RoiHandle> = vertices.iter().map(|&p| v(p)).collect();
                if let Some(&first) = vertices.first() {
                    hs.push(translate(first));
                }
                hs
            }
            // CircleROI: a perimeter vertex (silx `addHandle`) + a translate
            // center (`addTranslateHandle`).
            Roi::Circle { center: c, radius } => {
                vec![v((c.0 + radius, c.1)), translate(*c)]
            }
            // EllipseROI: two axis vertices + a translate center. The axis
            // vertices follow `orientation`: axis0 at radii.0·(cosθ, sinθ),
            // axis1 (perpendicular) at radii.1·(−sinθ, cosθ).
            Roi::Ellipse {
                center: c,
                radii,
                orientation,
            } => {
                let (cs, sn) = (orientation.cos(), orientation.sin());
                vec![
                    v((c.0 + radii.0 * cs, c.1 + radii.0 * sn)),
                    v((c.0 - radii.1 * sn, c.1 + radii.1 * cs)),
                    translate(*c),
                ]
            }
            // ArcROI: mid/outer/start/end shape vertices + a translate move
            // handle at the circle center (silx mid/weight/start/end +
            // `addTranslateHandle`).
            Roi::Arc { center: c, .. } => {
                let mut hs: Vec<RoiHandle> = (0..4)
                    .filter_map(|i| arc_vertex_pos(self, i).map(v))
                    .collect();
                hs.push(translate(*c));
                hs
            }
            // BandROI: begin/end vertices + two width vertices (silx `"d"`
            // handles) + a translate center.
            Roi::Band { begin, end, .. } => {
                let mut hs: Vec<RoiHandle> = (0..4)
                    .filter_map(|i| band_vertex_pos(self, i).map(v))
                    .collect();
                hs.push(translate((
                    (begin.0 + end.0) * 0.5,
                    (begin.1 + end.1) * 0.5,
                )));
                hs
            }
        }
    }

    /// Translate the whole ROI by `(dx, dy)` in data space, moving every handle
    /// by the same delta (silx `RegionOfInterest`/`HandleBasedROI.translate`).
    pub fn translate(&mut self, dx: f64, dy: f64) {
        let shift = |p: &mut (f64, f64)| {
            p.0 += dx;
            p.1 += dy;
        };
        match self {
            Roi::Rect { x, y } => {
                x.0 += dx;
                x.1 += dx;
                y.0 += dy;
                y.1 += dy;
            }
            // A band moves only on its spanned axis (silx ranges translate the
            // bounded axis; the spanned axis is unbounded).
            Roi::HRange { y } => {
                y.0 += dy;
                y.1 += dy;
            }
            Roi::VRange { x } => {
                x.0 += dx;
                x.1 += dx;
            }
            // A single line moves only on its bounded axis.
            Roi::HLine { y } => *y += dy,
            Roi::VLine { x } => *x += dx,
            Roi::Point { x, y } => {
                *x += dx;
                *y += dy;
            }
            Roi::Cross { center } => shift(center),
            Roi::Line { start, end } => {
                shift(start);
                shift(end);
            }
            Roi::Polygon { vertices } => {
                for v in vertices.iter_mut() {
                    shift(v);
                }
            }
            Roi::Circle { center, .. } => shift(center),
            Roi::Ellipse { center, .. } => shift(center),
            Roi::Arc { center, .. } => shift(center),
            Roi::Band { begin, end, .. } => {
                shift(begin);
                shift(end);
            }
        }
    }
}

/// Per-ROI outline stroke style (silx `LineMixIn` `setLineStyle`/`getLineStyle`,
/// `items/_roi_base.py`). A reduced three-value catalog matching the ROI styles
/// silx exposes through the manager. The default is [`RoiLineStyle::Solid`]
/// (silx `_DEFAULT_LINESTYLE = "-"`).
#[derive(Clone, Copy, Debug, PartialEq, Eq, Default)]
pub enum RoiLineStyle {
    /// Continuous line (silx `"-"`).
    #[default]
    Solid,
    /// Dashed line (silx `"--"`).
    Dashed,
    /// Dotted line (silx `":"`).
    Dotted,
}

impl RoiLineStyle {
    /// Map to the shared painter [`LineStyle`] so chrome can emit the dash
    /// segments (silx maps the same `"-"`/`"--"`/`":"` codes to its line styles).
    pub fn to_line_style(self) -> LineStyle {
        match self {
            RoiLineStyle::Solid => LineStyle::Solid,
            RoiLineStyle::Dashed => LineStyle::Dashed,
            RoiLineStyle::Dotted => LineStyle::Dotted,
        }
    }

    /// A short label for the per-ROI style selector.
    pub(crate) fn label(self) -> &'static str {
        match self {
            RoiLineStyle::Solid => "─",
            RoiLineStyle::Dashed => "╌",
            RoiLineStyle::Dotted => "┈",
        }
    }
}

/// A named way to edit a region of interest with handles (silx
/// `RoiInteractionMode`, `items/_roi_base.py:132`). Only the shape kinds that
/// mix in silx's `InteractionModeMixIn` — [`Roi::Arc`] and [`Roi::Band`] —
/// offer more than one mode; every other kind has a single fixed edit
/// behaviour and no interaction mode (see [`Roi::available_interaction_modes`]).
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum RoiInteractionMode {
    /// Arc edited by three points on its main-radius circle (silx
    /// `ArcROI.ThreePointMode`; the silx default for an arc).
    ArcThreePoint,
    /// Arc edited by polar anchors — mid / weight / start / end (silx
    /// `ArcROI.PolarMode`).
    ArcPolar,
    /// Arc anchors only translate the whole ROI (silx `ArcROI.MoveMode`).
    ArcTranslation,
    /// Band drawn bounded on both sides as a four-corner polygon (silx
    /// `BandROI.BoundedMode`; the silx default for a band).
    BandBounded,
    /// Band drawn unbounded as three parallel infinite lines (silx
    /// `BandROI.UnboundedMode`).
    BandUnbounded,
}

impl RoiInteractionMode {
    /// Short label (silx `RoiInteractionMode.label`).
    #[must_use]
    pub fn label(self) -> &'static str {
        match self {
            RoiInteractionMode::ArcThreePoint => "3 points",
            RoiInteractionMode::ArcPolar => "Polar",
            RoiInteractionMode::ArcTranslation => "Translation",
            RoiInteractionMode::BandBounded => "Bounded",
            RoiInteractionMode::BandUnbounded => "Unbounded",
        }
    }

    /// Longer description (silx `RoiInteractionMode.description`).
    #[must_use]
    pub fn description(self) -> &'static str {
        match self {
            RoiInteractionMode::ArcThreePoint => {
                "Provides 3 points to define the main radius circle"
            }
            RoiInteractionMode::ArcPolar => "Provides anchors to edit the ROI in polar coords",
            RoiInteractionMode::ArcTranslation => "Provides anchors to only move the ROI",
            RoiInteractionMode::BandBounded => "Band is bounded on both sides",
            RoiInteractionMode::BandUnbounded => "Band is unbounded on both sides",
        }
    }
}

impl Roi {
    /// The interaction modes this ROI kind offers (silx
    /// `InteractionModeMixIn.availableInteractionModes`). [`Roi::Arc`] offers
    /// three (`3 points` / `Polar` / `Translation`), [`Roi::Band`] two
    /// (`Bounded` / `Unbounded`); every other kind has a single fixed edit
    /// behaviour and returns an empty slice. The first entry is the silx
    /// default (see [`Self::default_interaction_mode`]).
    #[must_use]
    pub fn available_interaction_modes(&self) -> &'static [RoiInteractionMode] {
        match self {
            Roi::Arc { .. } => &[
                RoiInteractionMode::ArcThreePoint,
                RoiInteractionMode::ArcPolar,
                RoiInteractionMode::ArcTranslation,
            ],
            Roi::Band { .. } => &[
                RoiInteractionMode::BandBounded,
                RoiInteractionMode::BandUnbounded,
            ],
            _ => &[],
        }
    }

    /// The mode a freshly-created ROI of this kind starts in (silx
    /// `_initInteractionMode`): Arc → `3 points`, Band → `Bounded`; `None` for
    /// kinds without interaction modes.
    #[must_use]
    pub fn default_interaction_mode(&self) -> Option<RoiInteractionMode> {
        self.available_interaction_modes().first().copied()
    }
}

/// A region of interest plus the metadata silx keeps on its `RegionOfInterest`:
/// an optional per-ROI color (falls back to the manager default, silx
/// `useManagerColor`), a display name (silx `getName`/`setName`), whether it is
/// currently selected/highlighted (silx `setHighlighted`), and the per-ROI
/// outline styling silx stores via `LineMixIn` (`setLineWidth`/`setLineStyle`,
/// `items/_roi_base.py`) plus the shape fill flag (silx `RectangleROI`
/// `setFill`, `items/roi.py:531-552`).
#[derive(Clone, Debug, PartialEq)]
pub struct ManagedRoi {
    /// Pure geometry of the region of interest.
    pub roi: Roi,
    /// Per-ROI color override; `None` uses the manager's default color.
    pub color: Option<Color32>,
    /// Display name (may be empty).
    pub name: String,
    /// Whether this ROI is the highlighted/current one.
    pub selected: bool,
    /// Outline line width in logical points (silx `setLineWidth`, default 1.0).
    pub line_width: f32,
    /// Outline stroke style (silx `setLineStyle`, default solid).
    pub line_style: RoiLineStyle,
    /// Color filling the gaps of a dashed/dotted outline (silx
    /// `LineMixIn.setLineGapColor`); `None` leaves the gaps transparent. Has no
    /// visible effect on a solid outline.
    pub gap_color: Option<Color32>,
    /// Whether the ROI's interior is filled (silx `setFill`, default `false`).
    pub fill: bool,
    /// Whether the ROI is shown in the plot (silx
    /// `_RegionOfInterestBase.setVisible`, `items/_roi_base.py:479-489`,
    /// default `true`). A hidden ROI is neither drawn nor pickable/editable
    /// (silx hides the backing plot items and their handles), but it stays in
    /// the list — the ruler tool hides its measurement on disarm and reshows
    /// it on re-arm.
    pub visible: bool,
    /// Current handle-editing mode (silx `InteractionModeMixIn.__modeId`).
    /// Private so the invariant `interaction_mode ∈ roi.available_interaction_modes()`
    /// (or `None` for kinds without modes) holds by construction — read it with
    /// [`Self::interaction_mode`] and change it through the validated
    /// [`Self::set_interaction_mode`].
    interaction_mode: Option<RoiInteractionMode>,
}

impl ManagedRoi {
    /// Wrap `roi` with default metadata: no color override, empty name, not
    /// selected, solid 1.0-width outline, no gap color, unfilled, visible, and
    /// the ROI kind's default interaction mode (silx defaults +
    /// `_initInteractionMode`).
    pub fn new(roi: Roi) -> Self {
        let interaction_mode = roi.default_interaction_mode();
        Self {
            roi,
            color: None,
            name: String::new(),
            selected: false,
            line_width: DEFAULT_ROI_LINE_WIDTH,
            line_style: RoiLineStyle::default(),
            gap_color: None,
            fill: false,
            visible: true,
            interaction_mode,
        }
    }

    /// The current handle-editing mode (silx
    /// `InteractionModeMixIn.getInteractionMode`). `None` for ROI kinds without
    /// interaction modes (everything but [`Roi::Arc`] / [`Roi::Band`]).
    #[must_use]
    pub fn interaction_mode(&self) -> Option<RoiInteractionMode> {
        self.interaction_mode
    }

    /// Switch the handle-editing mode (silx
    /// `InteractionModeMixIn.setInteractionMode`). Returns `false` and leaves
    /// the mode unchanged if `mode` is not one of this ROI's
    /// [`Roi::available_interaction_modes`] — silx raises in that case, but a
    /// rejecting setter keeps the by-construction invariant without a panic.
    pub fn set_interaction_mode(&mut self, mode: RoiInteractionMode) -> bool {
        if self.roi.available_interaction_modes().contains(&mode) {
            self.interaction_mode = Some(mode);
            true
        } else {
            false
        }
    }
}

/// The inner (smaller) radius of an arc ring, silx-style: `radius − weight/2`
/// clamped at 0 (silx `ArcROI.getInnerRadius`, `_arc_roi.py:856-865`). The clamp
/// is REPORT-ONLY — used for drawing, hit-testing, and handle placement — while
/// the stored `(radius, weight)` keep the true thickness, so a follow-up drag
/// recomputes from the real weight rather than a collapsed-to-0 inner (R2-15).
#[must_use]
pub fn arc_inner_radius(radius: f64, weight: f64) -> f64 {
    (radius - weight * 0.5).max(0.0)
}

/// The outer (bigger) radius of an arc ring: `radius + weight/2` (silx
/// `ArcROI.getOuterRadius`, `_arc_roi.py:867-874`; never clamped).
#[must_use]
pub fn arc_outer_radius(radius: f64, weight: f64) -> f64 {
    radius + weight * 0.5
}

/// Data-space position of arc shape-vertex `index`, mirroring silx `ArcROI`'s
/// handle layout: 0 = mid (at the central `radius`, mid angle), 1 = outer/weight
/// (the outer radius at the mid angle), 2 = start point, 3 = end point. Returns
/// `None` for any other index or a non-arc ROI.
fn arc_vertex_pos(roi: &Roi, index: usize) -> Option<(f64, f64)> {
    let Roi::Arc {
        center,
        radius,
        weight,
        start_angle,
        end_angle,
    } = roi
    else {
        return None;
    };
    let mid_angle = (start_angle + end_angle) * 0.5;
    let at = |r: f64, a: f64| (center.0 + r * a.cos(), center.1 + r * a.sin());
    Some(match index {
        0 => at(*radius, mid_angle),
        1 => at(arc_outer_radius(*radius, *weight), mid_angle),
        2 => at(*radius, *start_angle),
        3 => at(*radius, *end_angle),
        _ => return None,
    })
}

/// Data-space position of band shape-vertex `index`, mirroring silx `BandROI`'s
/// handle layout: 0 = begin, 1 = end, 2 = width-up (`center + 0.5·width·normal`),
/// 3 = width-down (`center − 0.5·width·normal`). Returns `None` otherwise.
fn band_vertex_pos(roi: &Roi, index: usize) -> Option<(f64, f64)> {
    let Roi::Band { begin, end, width } = roi else {
        return None;
    };
    let center = ((begin.0 + end.0) * 0.5, (begin.1 + end.1) * 0.5);
    let n = band_normal(*begin, *end);
    let off = (0.5 * width * n.0, 0.5 * width * n.1);
    Some(match index {
        0 => *begin,
        1 => *end,
        2 => (center.0 + off.0, center.1 + off.1),
        3 => (center.0 - off.0, center.1 - off.1),
        _ => return None,
    })
}

/// Accumulate a dragged arc angle onto its previous value without ever
/// jumping more than 180°: the delta from `previous` to the raw `target`
/// (an `atan2` result in `(-π, π]`) is wrapped into `±π` before being added
/// (silx `_ArcGeometry.withStartAngle`/`withEndAngle` "Never add more than
/// 180 to maintain coherency", `items/_arc_roi.py:139-146,162-170`). The
/// stored angle stays continuous across the branch cut and may accumulate
/// beyond `±π` — exactly like silx's geometry angles. silx applies a single
/// wrap correction, not a loop; ported as-is.
fn coherent_angle(previous: f64, target: f64) -> f64 {
    let mut delta = target - previous;
    if delta > std::f64::consts::PI {
        delta -= std::f64::consts::TAU;
    } else if delta < -std::f64::consts::PI {
        delta += std::f64::consts::TAU;
    }
    previous + delta
}

/// Unit normal to the band's `begin → end` direction (silx `BandGeometry.normal`:
/// `(-vy/len, vx/len)`). A zero-length band has a zero normal.
fn band_normal(begin: (f64, f64), end: (f64, f64)) -> (f64, f64) {
    let (vx, vy) = (end.0 - begin.0, end.1 - begin.1);
    let len = (vx * vx + vy * vy).sqrt();
    if len == 0.0 {
        (0.0, 0.0)
    } else {
        (-vy / len, vx / len)
    }
}

/// Half-extents of the axis-aligned bounding box of an oriented ellipse with
/// semi-axes `(a, b)` (`a` along `orientation`, `b` perpendicular). For the
/// rotated parametric ellipse the per-axis maxima are
/// `hx = sqrt((a·cosθ)² + (b·sinθ)²)` and `hy = sqrt((a·sinθ)² + (b·cosθ)²)`.
fn ellipse_aabb_half_extents(radii: (f64, f64), orientation: f64) -> (f64, f64) {
    let (a, b) = radii;
    let (c, s) = (orientation.cos(), orientation.sin());
    let hx = ((a * c).powi(2) + (b * s).powi(2)).sqrt();
    let hy = ((a * s).powi(2) + (b * c).powi(2)).sqrt();
    (hx, hy)
}

/// The four data-space corners of a band ROI, in silx order
/// (`begin−offset, begin+offset, end+offset, end−offset`), where `offset =
/// 0.5·width·normal` (silx `BandGeometry.corners`). `None` for a non-band ROI.
fn band_corners(roi: &Roi) -> Option<Vec<(f64, f64)>> {
    let Roi::Band { begin, end, width } = roi else {
        return None;
    };
    let n = band_normal(*begin, *end);
    let off = (0.5 * width * n.0, 0.5 * width * n.1);
    Some(vec![
        (begin.0 - off.0, begin.1 - off.1),
        (begin.0 + off.0, begin.1 + off.1),
        (end.0 + off.0, end.1 + off.1),
        (end.0 - off.0, end.1 - off.1),
    ])
}

/// Whether the data point `pos` lies in the annular sector (silx
/// `ArcROI.contains`, `items/_arc_roi.py:915`): inside the `[inner, outer]`
/// radius ring AND within the angular sweep `[start, end]`. The sweep is
/// normalized so the test works for either rotation direction.
fn arc_contains(
    center: (f64, f64),
    inner_radius: f64,
    outer_radius: f64,
    start_angle: f64,
    end_angle: f64,
    pos: (f64, f64),
) -> bool {
    let (dx, dy) = (pos.0 - center.0, pos.1 - center.1);
    let distance = dx.hypot(dy);
    if distance < inner_radius || distance > outer_radius {
        return false;
    }
    // arctan2(dy, dx) in [-pi, pi].
    let mut angle = dy.atan2(dx);

    // Make the azimuth range positive, swapping start/end conceptually.
    let (mut start, azim_range) = if end_angle - start_angle < 0.0 {
        (end_angle, start_angle - end_angle)
    } else {
        (start_angle, end_angle - start_angle)
    };
    // Normalize start into [-pi, pi) (silx `numpy.mod(start + pi, 2pi) - pi`).
    let two_pi = std::f64::consts::TAU;
    start = (start + std::f64::consts::PI).rem_euclid(two_pi) - std::f64::consts::PI;
    // Bring the query angle into the same branch as start.
    if angle < start {
        angle += two_pi;
    }
    angle >= start && angle <= start + azim_range
}

/// Even-odd ray-cast point-in-polygon test, mirroring silx
/// `silx.image.shapes.Polygon.c_is_inside` (a ray cast scanning by `x`, casting
/// in `+y`). Returns `false` for polygons with fewer than 3 vertices.
fn point_in_polygon(vertices: &[(f64, f64)], pos: (f64, f64)) -> bool {
    let n = vertices.len();
    if n < 3 {
        return false;
    }
    let (px, py) = pos;
    let mut inside = false;
    let (mut ax, mut ay) = vertices[n - 1];
    for &(bx, by) in vertices {
        // Edge straddles the scan line at x = px (half-open in x), and the
        // short-circuit silx uses to skip edges entirely left of the point.
        if ((ax <= px && px < bx) || (bx <= px && px < ax)) && (py <= ay || py <= by) {
            let yinters = (px - ax) * (by - ay) / (bx - ax) + ay;
            if py < yinters {
                inside = !inside;
            }
        }
        ax = bx;
        ay = by;
    }
    inside
}

/// Whether the segment `(p1, p2)` crosses the axis-aligned unit square whose
/// lower-left corner is `corner` (its other corners are `+1` along each axis).
/// Mirrors silx `LineROI._intersects_unit_square`.
fn segment_intersects_unit_square(p1: (f64, f64), p2: (f64, f64), corner: (f64, f64)) -> bool {
    let (cx, cy) = corner;
    let bl = (cx, cy);
    let br = (cx + 1.0, cy);
    let tr = (cx + 1.0, cy + 1.0);
    let tl = (cx, cy + 1.0);
    segments_intersect(p1, p2, bl, br)
        || segments_intersect(p1, p2, br, tr)
        || segments_intersect(p1, p2, tr, tl)
        || segments_intersect(p1, p2, tl, bl)
}

/// Whether the two closed segments intersect, mirroring silx
/// `silx.gui.plot.utils.intersections.segments_intersection`: solve for the
/// infinite-line crossing, then confirm it lies within both segments' bounding
/// extents. Parallel/collinear segments (zero denominator) report no crossing.
fn segments_intersect(a1: (f64, f64), a2: (f64, f64), b1: (f64, f64), b2: (f64, f64)) -> bool {
    let dir_a = (a2.0 - a1.0, a2.1 - a1.1);
    let dir_b = (b2.0 - b1.0, b2.1 - b1.1);
    let dp = (a1.0 - b1.0, a1.1 - b1.1);
    // perp(dir_a) = (-dir_a.1, dir_a.0)
    let denom = -dir_a.1 * dir_b.0 + dir_a.0 * dir_b.1;
    if denom == 0.0 {
        return false;
    }
    let num = -dir_a.1 * dp.0 + dir_a.0 * dp.1;
    let s = num / denom;
    let ix = s * dir_b.0 + b1.0;
    let iy = s * dir_b.1 + b1.1;

    let min_x = a1.0.min(a2.0).max(b1.0.min(b2.0));
    let max_x = a1.0.max(a2.0).min(b1.0.max(b2.0));
    let min_y = a1.1.min(a2.1).max(b1.1.min(b2.1));
    let max_y = a1.1.max(a2.1).min(b1.1.max(b2.1));
    (min_x..=max_x).contains(&ix) && (min_y..=max_y).contains(&iy)
}

#[cfg(test)]
mod tests {
    use super::*;
    use egui::pos2;

    // 100×100 px area mapping data [0,10]×[0,10]; 1 data unit = 10 px, y flipped.
    fn t() -> Transform {
        Transform::new(
            0.0,
            10.0,
            0.0,
            10.0,
            Rect::from_min_max(pos2(0.0, 0.0), pos2(100.0, 100.0)),
        )
    }

    // Like `t()`, but with the Y axis INVERTED — an image plot (`Plot2D::new`
    // calls `set_y_inverted(true)`), where data y.max sits at the *top* of the
    // screen (smaller screen-y) and y.min at the bottom is flipped: data y=0 ->
    // screen y=0 (top), data y=10 -> screen y=100 (bottom).
    fn t_inv() -> Transform {
        let mut y = crate::core::transform::Axis::linear(0.0, 10.0);
        y.inverted = true;
        Transform::with_axes(
            crate::core::transform::Axis::linear(0.0, 10.0),
            y,
            Rect::from_min_max(pos2(0.0, 0.0), pos2(100.0, 100.0)),
        )
    }

    #[test]
    fn edge_at_corner_keeps_data_identity_under_inverted_y() {
        // Image-plot orientation: an inverted Y axis must NOT swap which data
        // corner a screen grab maps to. The data corner (x.min, y.max) = TopLeft
        // is drawn at the screen *bottom*-left under inversion; grabbing it must
        // still report `TopLeft`, so `move_edge(TopLeft)` resizes the grabbed
        // corner instead of the opposite one (which collapsed the rect before).
        let roi = Roi::Rect {
            x: (2.0, 8.0),
            y: (3.0, 7.0),
        };
        let t = t_inv();
        // data y=3 -> screen 30; data y=7 -> screen 70. data x=2 -> 20, 8 -> 80.
        // So screen-top-left (20,30) is data (x.min, y.min) = BottomLeft;
        // screen-bottom-left (20,70) is data (x.min, y.max) = TopLeft.
        assert_eq!(
            roi.edge_at(&t, pos2(20.0, 30.0), 4.0),
            Some(RoiEdge::BottomLeft)
        );
        assert_eq!(
            roi.edge_at(&t, pos2(20.0, 70.0), 4.0),
            Some(RoiEdge::TopLeft)
        );
        assert_eq!(
            roi.edge_at(&t, pos2(80.0, 30.0), 4.0),
            Some(RoiEdge::BottomRight)
        );
        assert_eq!(
            roi.edge_at(&t, pos2(80.0, 70.0), 4.0),
            Some(RoiEdge::TopRight)
        );
    }

    #[test]
    fn corner_drag_under_inverted_y_tracks_cursor_without_collapse() {
        // A full grab→move on the visual-bottom-left corner under inverted Y:
        // it must resize that corner (move x.min and y.max) to the cursor, not
        // collapse the rectangle (the pre-fix behaviour set the wrong y edge).
        let roi = Roi::Rect {
            x: (2.0, 8.0),
            y: (3.0, 7.0),
        };
        let t = t_inv();
        // Under inversion py = 10·y: data y.min=3 -> screen 30 (top), y.max=7 ->
        // screen 70 (bottom). So the visual bottom-left handle @ (20,70) is data
        // (x.min=2, y.max=7) = TopLeft.
        let grab = pos2(20.0, 70.0);
        let edge = roi.edge_at(&t, grab, 4.0).expect("corner grabbed");
        assert_eq!(edge, RoiEdge::TopLeft);
        // Drag it to screen (10, 90) = data (1, 9): x.min -> 1, y.max -> 9, with
        // y.min (3) untouched. The grabbed corner tracks the cursor; no collapse.
        let mut moved = roi.clone();
        moved.move_edge(edge, t.pixel_to_data(pos2(10.0, 90.0)));
        assert_eq!(
            moved,
            Roi::Rect {
                x: (1.0, 8.0),
                y: (3.0, 9.0)
            },
            "x.min and y.max follow the cursor; y.min untouched; no collapse"
        );
    }

    #[test]
    fn side_edge_under_inverted_y_maps_to_correct_data_edge() {
        // Under inversion (py = 10·y) data y.min=3 is drawn at the screen TOP
        // (30) and y.max=7 at the screen BOTTOM (70). edge_at must report the
        // *data* edge by identity, so the screen-top probe is the data Bottom
        // (y.min) edge and the screen-bottom probe is the data Top (y.max) edge.
        let roi = Roi::Rect {
            x: (2.0, 8.0),
            y: (3.0, 7.0),
        };
        let t = t_inv();
        assert_eq!(
            roi.edge_at(&t, pos2(50.0, 30.0), 4.0),
            Some(RoiEdge::Bottom)
        );
        assert_eq!(roi.edge_at(&t, pos2(50.0, 70.0), 4.0), Some(RoiEdge::Top));
        // Dragging the screen-top edge (data Bottom = y.min) up to screen y=10
        // (data y=1) moves y.min, so the visual top edge tracks the cursor.
        let mut moved = roi.clone();
        moved.move_edge(RoiEdge::Bottom, t.pixel_to_data(pos2(50.0, 10.0)));
        let Roi::Rect { x, y } = moved else {
            panic!("still a rect")
        };
        assert!((x.0 - 2.0).abs() < 1e-9 && (x.1 - 8.0).abs() < 1e-9);
        assert!(
            (y.0 - 1.0).abs() < 1e-9 && (y.1 - 7.0).abs() < 1e-9,
            "y.min tracks the cursor to data 1; y.max untouched: {y:?}"
        );
    }

    #[test]
    fn circle_perimeter_resize_works_under_inverted_y() {
        // The circle perimeter handle stays grabbable and resizes correctly under
        // an inverted Y axis (it uses a data-distance radius, not a screen label).
        let mut circ = Roi::Circle {
            center: (5.0, 5.0),
            radius: 2.0,
        };
        let t = t_inv();
        // Perimeter handle at data (center.x + r, center.y) = (7, 5).
        let handle = t.data_to_pixel(7.0, 5.0);
        assert_eq!(circ.edge_at(&t, handle, 6.0), Some(RoiEdge::Vertex(1)));
        // Drag it out to data x=8: radius becomes 3.
        circ.move_edge(
            RoiEdge::Vertex(1),
            t.pixel_to_data(t.data_to_pixel(8.0, 5.0)),
        );
        assert_eq!(
            circ,
            Roi::Circle {
                center: (5.0, 5.0),
                radius: 3.0
            }
        );
    }

    #[test]
    fn rect_screen_rect_flips_y() {
        let roi = Roi::Rect {
            x: (2.0, 8.0),
            y: (3.0, 7.0),
        };
        let r = roi.screen_rect(&t());
        // x: 2->20, 8->80; y: data 3 (bottom) -> 70px, data 7 (top) -> 30px.
        assert!((r.left() - 20.0).abs() < 1e-3 && (r.right() - 80.0).abs() < 1e-3);
        assert!((r.top() - 30.0).abs() < 1e-3 && (r.bottom() - 70.0).abs() < 1e-3);
    }

    #[test]
    fn edge_at_grabs_nearest_edge() {
        let roi = Roi::Rect {
            x: (2.0, 8.0),
            y: (3.0, 7.0),
        };
        // Near the left edge (x≈20px), mid-height.
        assert_eq!(
            roi.edge_at(&t(), pos2(21.0, 50.0), 4.0),
            Some(RoiEdge::Left)
        );
        // Near the top edge (screen y≈30px).
        assert_eq!(roi.edge_at(&t(), pos2(50.0, 31.0), 4.0), Some(RoiEdge::Top));
        // Far from any edge -> None.
        assert_eq!(roi.edge_at(&t(), pos2(50.0, 50.0), 4.0), None);
    }

    #[test]
    fn edge_at_corner_takes_priority_over_edges() {
        let roi = Roi::Rect {
            x: (2.0, 8.0),
            y: (3.0, 7.0),
        };
        // Screen corners: TL(20,30) TR(80,30) BL(20,70) BR(80,70).
        // A cursor a pixel inside each corner is also within grab range of the
        // two adjoining edges, so the corner must win.
        assert_eq!(
            roi.edge_at(&t(), pos2(21.0, 31.0), 4.0),
            Some(RoiEdge::TopLeft)
        );
        assert_eq!(
            roi.edge_at(&t(), pos2(79.0, 31.0), 4.0),
            Some(RoiEdge::TopRight)
        );
        assert_eq!(
            roi.edge_at(&t(), pos2(21.0, 69.0), 4.0),
            Some(RoiEdge::BottomLeft)
        );
        assert_eq!(
            roi.edge_at(&t(), pos2(79.0, 69.0), 4.0),
            Some(RoiEdge::BottomRight)
        );
        // Mid-edge probes (far from every corner) still resolve to the edge.
        assert_eq!(
            roi.edge_at(&t(), pos2(21.0, 50.0), 4.0),
            Some(RoiEdge::Left)
        );
        assert_eq!(roi.edge_at(&t(), pos2(50.0, 31.0), 4.0), Some(RoiEdge::Top));
    }

    #[test]
    fn move_edge_corner_resizes_both_axes() {
        let mut roi = Roi::Rect {
            x: (2.0, 8.0),
            y: (3.0, 7.0),
        };
        // Top-right corner drag moves x.max and y.max together (diagonal).
        roi.move_edge(RoiEdge::TopRight, (9.0, 9.0));
        assert_eq!(
            roi,
            Roi::Rect {
                x: (2.0, 9.0),
                y: (3.0, 9.0)
            }
        );
        // Bottom-left corner drag moves x.min and y.min together.
        roi.move_edge(RoiEdge::BottomLeft, (1.0, 1.0));
        assert_eq!(
            roi,
            Roi::Rect {
                x: (1.0, 9.0),
                y: (1.0, 9.0)
            }
        );
        // Dragging a corner past its opposite flips the rectangle rather than
        // collapsing it (silx `RectangleROI` rebuilds from the dragged corner
        // and the fixed opposite (1, 1) via min/max). TopRight → (−5, −5) past
        // the opposite BottomLeft (1, 1) yields the box spanning the two.
        roi.move_edge(RoiEdge::TopRight, (-5.0, -5.0));
        assert_eq!(
            roi,
            Roi::Rect {
                x: (-5.0, 1.0),
                y: (-5.0, 1.0)
            }
        );
    }

    #[test]
    fn rect_side_edge_crosses_opposite_instead_of_collapsing() {
        // Dragging the Left edge past the Right edge flips x rather than
        // collapsing to zero width; y is untouched (silx min/max rebuild).
        let mut roi = Roi::Rect {
            x: (2.0, 8.0),
            y: (3.0, 7.0),
        };
        roi.move_edge(RoiEdge::Left, (10.0, 0.0));
        assert_eq!(
            roi,
            Roi::Rect {
                x: (8.0, 10.0), // Left passed Right (8) → new span [8, 10]
                y: (3.0, 7.0),  // y unchanged by an x-edge drag
            }
        );
    }

    #[test]
    fn hrange_edge_crosses_opposite_instead_of_collapsing() {
        // The HRange Bottom edge dragged above the Top edge flips the range.
        let mut roi = Roi::HRange { y: (3.0, 7.0) };
        roi.move_edge(RoiEdge::Bottom, (0.0, 9.0));
        assert_eq!(roi, Roi::HRange { y: (7.0, 9.0) });
    }

    #[test]
    fn handle_centers_includes_four_corners() {
        let roi = Roi::Rect {
            x: (2.0, 8.0),
            y: (3.0, 7.0),
        };
        // edges() order: Left, Right, Bottom, Top, BL, BR, TL, TR.
        let centers = roi.handle_centers(&t());
        assert_eq!(centers.len(), 8);
        let corner = |c: Pos2| (c.x, c.y);
        assert_eq!(corner(centers[4]), (20.0, 70.0)); // BottomLeft
        assert_eq!(corner(centers[5]), (80.0, 70.0)); // BottomRight
        assert_eq!(corner(centers[6]), (20.0, 30.0)); // TopLeft
        assert_eq!(corner(centers[7]), (80.0, 30.0)); // TopRight
    }

    #[test]
    fn handle_centers_keep_data_identity_under_inverted_y() {
        // Under inverted Y (py = 10·y): data y.min=3 -> screen 30 (top), y.max=7
        // -> screen 70 (bottom). Each handle must sit at its DATA point: the
        // data Bottom (y.min) handle is drawn at the screen top, Top at the
        // bottom — matching edge_at/move_edge so the drawn marks line up with
        // the grab targets.
        let roi = Roi::Rect {
            x: (2.0, 8.0),
            y: (3.0, 7.0),
        };
        let centers = roi.handle_centers(&t_inv());
        let corner = |c: Pos2| (c.x, c.y);
        // edges() order: Left, Right, Bottom, Top, BL, BR, TL, TR.
        assert_eq!(corner(centers[2]), (50.0, 30.0)); // Bottom = y.min @ screen top
        assert_eq!(corner(centers[3]), (50.0, 70.0)); // Top = y.max @ screen bottom
        assert_eq!(corner(centers[4]), (20.0, 30.0)); // BottomLeft (x.min, y.min)
        assert_eq!(corner(centers[6]), (20.0, 70.0)); // TopLeft (x.min, y.max)
    }

    #[test]
    fn hrange_only_exposes_horizontal_edges() {
        let roi = Roi::HRange { y: (3.0, 7.0) };
        // Anywhere along the bottom band edge (full-width) grabs Bottom.
        assert_eq!(
            roi.edge_at(&t(), pos2(5.0, 70.0), 4.0),
            Some(RoiEdge::Bottom)
        );
        // A vertical-edge probe finds nothing (no Left/Right on a band).
        assert_eq!(roi.edge_at(&t(), pos2(0.0, 50.0), 4.0), None);
    }

    #[test]
    fn hline_is_grabbable_anywhere_along_its_span_and_moves_in_y() {
        // Single horizontal line at y=4. `t()` is y-up (non-inverted), so data
        // y=4 maps to screen y = 100 - 10*4 = 60, full width.
        let mut roi = Roi::HLine { y: 4.0 };
        assert_eq!(roi.edges(), vec![RoiEdge::Bottom]);
        // Grab near the line at any x within grab radius.
        assert_eq!(
            roi.edge_at(&t(), pos2(5.0, 61.0), 4.0),
            Some(RoiEdge::Bottom)
        );
        assert_eq!(
            roi.edge_at(&t(), pos2(95.0, 59.0), 4.0),
            Some(RoiEdge::Bottom)
        );
        // Far from the line in Y -> no grab.
        assert_eq!(roi.edge_at(&t(), pos2(50.0, 20.0), 4.0), None);
        // Dragging the edge sets the line's y (no normalization needed).
        roi.move_edge(RoiEdge::Bottom, (7.0, 6.5));
        assert_eq!(roi, Roi::HLine { y: 6.5 });
        // Translate only moves the bounded (Y) axis.
        roi.translate(3.0, -1.5);
        assert_eq!(roi, Roi::HLine { y: 5.0 });
    }

    #[test]
    fn vline_is_grabbable_anywhere_along_its_span_and_moves_in_x() {
        // Single vertical line at x=2 -> screen x=20, full height.
        let mut roi = Roi::VLine { x: 2.0 };
        assert_eq!(roi.edges(), vec![RoiEdge::Left]);
        assert_eq!(
            roi.edge_at(&t(), pos2(21.0, 10.0), 4.0),
            Some(RoiEdge::Left)
        );
        assert_eq!(
            roi.edge_at(&t(), pos2(19.0, 90.0), 4.0),
            Some(RoiEdge::Left)
        );
        assert_eq!(roi.edge_at(&t(), pos2(60.0, 50.0), 4.0), None);
        roi.move_edge(RoiEdge::Left, (8.0, 3.0));
        assert_eq!(roi, Roi::VLine { x: 8.0 });
        roi.translate(-1.0, 5.0);
        assert_eq!(roi, Roi::VLine { x: 7.0 });
    }

    #[test]
    fn line_roi_contains_only_on_the_line_and_has_one_handle() {
        let h = Roi::HLine { y: 4.0 };
        assert!(h.contains((123.0, 4.0))); // any x, exact y
        assert!(!h.contains((123.0, 4.1)));
        assert_eq!(h.handles().len(), 1);
        assert_eq!(h.handles()[0].kind, HandleKind::Vertex);

        let v = Roi::VLine { x: 2.0 };
        assert!(v.contains((2.0, -50.0))); // exact x, any y
        assert!(!v.contains((2.1, -50.0)));
        assert_eq!(v.handles().len(), 1);
    }

    #[test]
    fn move_edge_flips_but_stays_normalized() {
        let mut roi = Roi::Rect {
            x: (2.0, 8.0),
            y: (3.0, 7.0),
        };
        // Drag the left edge past the right edge: it flips around the fixed
        // right edge (8), staying normalized (x.0 <= x.1) — silx min/max rebuild.
        roi.move_edge(RoiEdge::Left, (12.0, 5.0));
        assert_eq!(
            roi,
            Roi::Rect {
                x: (8.0, 12.0),
                y: (3.0, 7.0)
            }
        );
        // Normal move of the right edge back inside.
        roi.move_edge(RoiEdge::Right, (9.0, 5.0));
        assert_eq!(
            roi,
            Roi::Rect {
                x: (8.0, 9.0),
                y: (3.0, 7.0)
            }
        );
    }

    #[test]
    fn point_roi_vertex_handle_moves_it() {
        let mut roi = Roi::Point { x: 5.0, y: 5.0 };
        roi.move_edge(RoiEdge::Vertex(0), (3.0, 4.0));
        assert_eq!(roi, Roi::Point { x: 3.0, y: 4.0 });
    }

    #[test]
    fn circle_handles_drag_center_and_radius() {
        // Vertex 0 translates the center; Vertex 1 sets the radius to the
        // distance from the center (silx `CircleROI`).
        let mut roi = Roi::Circle {
            center: (5.0, 5.0),
            radius: 2.0,
        };
        roi.move_edge(RoiEdge::Vertex(1), (8.0, 9.0)); // dist √(9+16) = 5
        roi.move_edge(RoiEdge::Vertex(0), (1.0, 2.0));
        if let Roi::Circle { center, radius } = roi {
            assert_eq!(center, (1.0, 2.0));
            assert!((radius - 5.0).abs() < 1e-9, "radius {radius}");
        } else {
            panic!("not a circle");
        }
    }

    #[test]
    fn ellipse_handles_drag_center_and_each_semi_axis() {
        // Vertex 0 translates the center; Vertex 1 sets the x semi-axis,
        // Vertex 2 the y semi-axis (silx `EllipseROI`, axis-aligned).
        let mut roi = Roi::Ellipse {
            center: (0.0, 0.0),
            radii: (3.0, 4.0),
            orientation: 0.0,
        };
        // Axis-aligned drags (along +x, +y) keep orientation at 0.
        roi.move_edge(RoiEdge::Vertex(1), (5.0, 0.0)); // axis0 semi-axis -> 5
        roi.move_edge(RoiEdge::Vertex(2), (0.0, 7.0)); // axis1 semi-axis -> 7
        roi.move_edge(RoiEdge::Vertex(0), (2.0, 3.0)); // center
        assert_eq!(
            roi,
            Roi::Ellipse {
                center: (2.0, 3.0),
                radii: (5.0, 7.0),
                orientation: 0.0,
            }
        );
    }

    #[test]
    fn line_roi_endpoints_move_independently() {
        let mut roi = Roi::Line {
            start: (0.0, 0.0),
            end: (10.0, 10.0),
        };
        roi.move_edge(RoiEdge::Vertex(0), (1.0, 2.0));
        roi.move_edge(RoiEdge::Vertex(1), (9.0, 8.0));
        assert_eq!(
            roi,
            Roi::Line {
                start: (1.0, 2.0),
                end: (9.0, 8.0)
            }
        );
    }

    #[test]
    fn polygon_vertex_move_updates_specific_vertex() {
        let mut roi = Roi::Polygon {
            vertices: vec![(0.0, 0.0), (5.0, 0.0), (5.0, 5.0)],
        };
        roi.move_edge(RoiEdge::Vertex(1), (6.0, 1.0));
        assert_eq!(
            roi,
            Roi::Polygon {
                vertices: vec![(0.0, 0.0), (6.0, 1.0), (5.0, 5.0)]
            }
        );
    }

    #[test]
    fn arc_handles_drag_radius_weight_and_angles() {
        use std::f64::consts::{FRAC_PI_2, PI};
        // center origin, radius 3 / weight 2 (reported inner 2, outer 4),
        // sweep 0 → π/2.
        let base = Roi::Arc {
            center: (0.0, 0.0),
            radius: 3.0,
            weight: 2.0,
            start_angle: 0.0,
            end_angle: FRAC_PI_2,
        };
        let approx =
            |a: f64, b: f64, what: &str| assert!((a - b).abs() < 1e-9, "{what}: {a} vs {b}");

        // Mid handle → central radius 5, weight 2 conserved.
        let mut roi = base.clone();
        roi.move_edge(RoiEdge::Vertex(0), (5.0, 0.0));
        if let Roi::Arc {
            radius,
            weight,
            start_angle,
            end_angle,
            ..
        } = roi
        {
            approx(radius, 5.0, "mid radius");
            approx(weight, 2.0, "mid keeps weight");
            approx(start_angle, 0.0, "mid keeps start");
            approx(end_angle, FRAC_PI_2, "mid keeps end");
        } else {
            panic!("not an arc");
        }

        // Weight handle → weight 2·|d − radius|. d = 6, radius = 3 → weight 6
        // (radius unchanged); reported inner clamps to 0, outer 6.
        let mut roi = base.clone();
        roi.move_edge(RoiEdge::Vertex(1), (6.0, 0.0));
        if let Roi::Arc { radius, weight, .. } = roi {
            approx(radius, 3.0, "weight keeps radius");
            approx(weight, 6.0, "weight");
            approx(
                arc_inner_radius(radius, weight),
                0.0,
                "reported inner clamps",
            );
            approx(arc_outer_radius(radius, weight), 6.0, "reported outer");
        } else {
            panic!("not an arc");
        }

        // Start / end handles set the sweep angles and leave radius/weight alone.
        let mut roi = base.clone();
        roi.move_edge(RoiEdge::Vertex(2), (0.0, 5.0));
        roi.move_edge(RoiEdge::Vertex(3), (-5.0, 0.0));
        if let Roi::Arc {
            radius,
            weight,
            start_angle,
            end_angle,
            ..
        } = roi
        {
            approx(start_angle, FRAC_PI_2, "start angle");
            approx(end_angle, PI, "end angle");
            approx(radius, 3.0, "angle keeps radius");
            approx(weight, 2.0, "angle keeps weight");
        } else {
            panic!("not an arc");
        }
    }

    #[test]
    fn arc_weight_survives_a_reported_inner_radius_clamp() {
        // R2-15: silx stores (radius, weight) and clamps only the REPORTED inner
        // radius (getInnerRadius, _arc_roi.py:856-865), so `weight > 2·radius`
        // survives a follow-up drag. The old (inner, outer) storage collapsed the
        // inner to 0 and lost the true weight, so re-dragging the radius rebuilt a
        // thinner ring — the recurring edge this closes.
        use std::f64::consts::FRAC_PI_2;
        let approx =
            |a: f64, b: f64, what: &str| assert!((a - b).abs() < 1e-9, "{what}: {a} vs {b}");
        let mut roi = Roi::Arc {
            center: (0.0, 0.0),
            radius: 3.0,
            weight: 2.0,
            start_angle: 0.0,
            end_angle: FRAC_PI_2,
        };
        // Fatten past 2·radius: d = 10 → weight 2·|10 − 3| = 14. The reported inner
        // clamps to 0, but the stored weight is the true 14 (not the clamped 10).
        roi.move_edge(RoiEdge::Vertex(1), (10.0, 0.0));
        if let Roi::Arc { radius, weight, .. } = roi {
            approx(radius, 3.0, "radius unchanged by the weight drag");
            approx(weight, 14.0, "true weight stored, not the clamped 10");
            approx(
                arc_inner_radius(radius, weight),
                0.0,
                "reported inner clamps at 0",
            );
            approx(arc_outer_radius(radius, weight), 10.0, "reported outer");
        } else {
            panic!("not an arc");
        }
        // Now drag the MID (radius) handle. silx conserves weight, so the ring
        // keeps its full thickness 14: outer 8+7=15, inner un-clamps to 8−7=1.
        // The old storage would have used weight = outer − inner = 10 − 0 = 10,
        // giving inner 3 / outer 13 — the exact bug.
        roi.move_edge(RoiEdge::Vertex(0), (8.0, 0.0));
        if let Roi::Arc { radius, weight, .. } = roi {
            approx(radius, 8.0, "radius moved to 8");
            approx(weight, 14.0, "weight conserved across the radius drag");
            approx(arc_outer_radius(radius, weight), 15.0, "outer = 8 + 7");
            approx(
                arc_inner_radius(radius, weight),
                1.0,
                "inner un-clamps to 8 − 7",
            );
        } else {
            panic!("not an arc");
        }
    }

    #[test]
    fn arc_angle_drag_stays_coherent_across_the_branch_cut() {
        // silx "Never add more than 180 to maintain coherency"
        // (_ArcGeometry.withStartAngle/withEndAngle, _arc_roi.py:139-146,
        // 162-170): a drag crossing the atan2 branch cut wraps the delta into
        // ±π and accumulates, so the stored angle stays continuous instead of
        // flipping by ~2π (which visually inverts the arc).
        let approx =
            |a: f64, b: f64, what: &str| assert!((a - b).abs() < 1e-9, "{what}: {a} vs {b}");

        // Start handle at 3.0 rad nudged to a cursor whose raw atan2 is −3.0:
        // delta −6.0 wraps to +0.283 → the angle advances to ≈ 3.283.
        let mut roi = Roi::Arc {
            center: (0.0, 0.0),
            radius: 3.0,
            weight: 2.0,
            start_angle: 3.0,
            end_angle: std::f64::consts::FRAC_PI_2,
        };
        let target = -3.0f64;
        roi.move_edge(RoiEdge::Vertex(2), (5.0 * target.cos(), 5.0 * target.sin()));
        if let Roi::Arc { start_angle, .. } = roi {
            approx(
                start_angle,
                3.0 + (std::f64::consts::TAU - 6.0),
                "coherent start",
            );
        } else {
            panic!("not an arc");
        }

        // End handle mirrored: −3.0 rad dragged to raw +3.0 → delta +6.0
        // wraps to −0.283 → ≈ −3.283.
        let mut roi = Roi::Arc {
            center: (0.0, 0.0),
            radius: 3.0,
            weight: 2.0,
            start_angle: 0.0,
            end_angle: -3.0,
        };
        let target = 3.0f64;
        roi.move_edge(RoiEdge::Vertex(3), (5.0 * target.cos(), 5.0 * target.sin()));
        if let Roi::Arc { end_angle, .. } = roi {
            approx(
                end_angle,
                -3.0 - (std::f64::consts::TAU - 6.0),
                "coherent end",
            );
        } else {
            panic!("not an arc");
        }

        // A stored angle accumulated past ±π keeps accumulating from there
        // (silx geometry angles are unbounded): 3.283 nudged a bit further
        // across lands near 3.4, not near −2.9.
        let mut roi = Roi::Arc {
            center: (0.0, 0.0),
            radius: 3.0,
            weight: 2.0,
            start_angle: 3.0 + (std::f64::consts::TAU - 6.0),
            end_angle: 0.0,
        };
        let target = 3.4 - std::f64::consts::TAU; // raw atan2 of a 3.4-rad cursor
        roi.move_edge(RoiEdge::Vertex(2), (5.0 * target.cos(), 5.0 * target.sin()));
        if let Roi::Arc { start_angle, .. } = roi {
            approx(start_angle, 3.4, "accumulated start");
        } else {
            panic!("not an arc");
        }
    }

    #[test]
    fn band_handles_drag_endpoints_and_width() {
        // Axis-aligned band begin(0,0)→end(10,0), width 4. normal = (0,1),
        // center = (5,0); width-up handle at (5,2), width-down at (5,-2).
        let mut roi = Roi::Band {
            begin: (0.0, 0.0),
            end: (10.0, 0.0),
            width: 4.0,
        };
        // begin / end handles set the segment endpoints directly.
        roi.move_edge(RoiEdge::Vertex(0), (1.0, 1.0));
        roi.move_edge(RoiEdge::Vertex(1), (9.0, 1.0));
        assert_eq!(
            roi,
            Roi::Band {
                begin: (1.0, 1.0),
                end: (9.0, 1.0),
                width: 4.0,
            }
        );
        // Width-up handle: width = 2·(normal·(p − center)). New center (5,1),
        // normal (0,1); dragging to (5,4) → proj 3 → width 6.
        roi.move_edge(RoiEdge::Vertex(2), (5.0, 4.0));
        if let Roi::Band { width, .. } = roi {
            assert!((width - 6.0).abs() < 1e-9, "width {width}");
        } else {
            panic!("not a band");
        }
        // Width-down handle measures the opposite side: dragging to (5,-2) →
        // proj 3 (sign-flipped) → width 6; the same point on the up side clamps
        // the width to 0.
        roi.move_edge(RoiEdge::Vertex(3), (5.0, -2.0));
        if let Roi::Band { width, .. } = roi {
            assert!((width - 6.0).abs() < 1e-9, "width {width}");
        } else {
            panic!("not a band");
        }
        roi.move_edge(RoiEdge::Vertex(3), (5.0, 5.0));
        if let Roi::Band { width, .. } = roi {
            assert!((width - 0.0).abs() < 1e-9, "clamped width {width}");
        } else {
            panic!("not a band");
        }
    }

    #[test]
    fn edge_at_finds_line_endpoint() {
        let roi = Roi::Line {
            start: (2.0, 5.0),
            end: (8.0, 5.0),
        };
        // start is at data (2,5) → pixel (20, 50); end at (8,5) → pixel (80, 50)
        assert_eq!(
            roi.edge_at(&t(), pos2(21.0, 50.0), 4.0),
            Some(RoiEdge::Vertex(0))
        );
        assert_eq!(
            roi.edge_at(&t(), pos2(79.0, 50.0), 4.0),
            Some(RoiEdge::Vertex(1))
        );
        assert_eq!(roi.edge_at(&t(), pos2(50.0, 50.0), 4.0), None); // mid-line, no handle
    }

    // --- contains() boundary tests (one case per boundary) ---

    #[test]
    fn rect_contains_inside_edge_outside() {
        let roi = Roi::Rect {
            x: (2.0, 8.0),
            y: (3.0, 7.0),
        };
        assert!(roi.contains((5.0, 5.0))); // strictly inside
        assert!(roi.contains((2.0, 5.0))); // on the left edge (inclusive)
        assert!(roi.contains((8.0, 7.0))); // on a corner (inclusive)
        assert!(!roi.contains((1.999, 5.0))); // just outside in x
        assert!(!roi.contains((5.0, 7.001))); // just outside in y
    }

    #[test]
    fn band_contains_ignores_spanned_axis() {
        let h = Roi::HRange { y: (3.0, 7.0) };
        assert!(h.contains((1e9, 5.0))); // any x inside the y band
        assert!(h.contains((0.0, 3.0))); // on the lower edge
        assert!(!h.contains((0.0, 2.999))); // below the band
        let v = Roi::VRange { x: (2.0, 8.0) };
        assert!(v.contains((5.0, -1e9))); // any y inside the x band
        assert!(!v.contains((8.001, 0.0))); // right of the band
    }

    #[test]
    fn point_contains_requires_exact_match() {
        let roi = Roi::Point { x: 5.0, y: 5.0 };
        assert!(roi.contains((5.0, 5.0)));
        assert!(!roi.contains((5.0, 5.000001)));
    }

    #[test]
    fn cross_contains_on_either_crosshair() {
        let roi = Roi::Cross { center: (5.0, 5.0) };
        assert!(roi.contains((5.0, 5.0))); // the center
        assert!(roi.contains((5.0, -100.0))); // on the vertical crosshair
        assert!(roi.contains((100.0, 5.0))); // on the horizontal crosshair
        assert!(!roi.contains((4.999, 5.001))); // on neither
    }

    #[test]
    fn circle_contains_inside_edge_outside() {
        let roi = Roi::Circle {
            center: (5.0, 5.0),
            radius: 2.0,
        };
        assert!(roi.contains((5.0, 5.0))); // center
        assert!(roi.contains((7.0, 5.0))); // exactly on the perimeter (<=)
        assert!(roi.contains((6.0, 6.0))); // inside (dist ≈ 1.41)
        assert!(!roi.contains((7.001, 5.0))); // just outside the perimeter
    }

    #[test]
    fn ellipse_contains_inside_edge_outside() {
        let roi = Roi::Ellipse {
            center: (5.0, 5.0),
            radii: (4.0, 2.0), // axis0=4 (x), axis1=2 (y), orientation 0
            orientation: 0.0,
        };
        assert!(roi.contains((5.0, 5.0))); // center
        assert!(roi.contains((9.0, 5.0))); // on the axis0 tip (x): 1.0 == 1.0
        assert!(roi.contains((5.0, 7.0))); // on the axis1 tip (y)
        assert!(!roi.contains((5.0, 7.001))); // just past the axis1 tip
        assert!(!roi.contains((9.001, 5.0))); // just past the axis0 tip
        // Degenerate (zero radius) contains nothing.
        let degenerate = Roi::Ellipse {
            center: (0.0, 0.0),
            radii: (0.0, 1.0),
            orientation: 0.0,
        };
        assert!(!degenerate.contains((0.0, 0.0)));
    }

    #[test]
    fn ellipse_axis0_handle_drag_off_axis_rotates_and_resizes() {
        use std::f64::consts::FRAC_PI_4;
        // Drag the axis0 handle to a 45° direction at distance 5: silx's
        // `EllipseROI.handleDragUpdated` axis anchor sets radii.0 = distance and
        // orientation = the cursor angle.
        let mut roi = Roi::Ellipse {
            center: (0.0, 0.0),
            radii: (3.0, 2.0),
            orientation: 0.0,
        };
        let d = 5.0_f64;
        roi.move_edge(
            RoiEdge::Vertex(1),
            (d * FRAC_PI_4.cos(), d * FRAC_PI_4.sin()),
        );
        match roi {
            Roi::Ellipse {
                center,
                radii,
                orientation,
            } => {
                assert!(center.0.abs() < 1e-9 && center.1.abs() < 1e-9, "{center:?}");
                assert!((radii.0 - 5.0).abs() < 1e-9, "axis0 = cursor distance");
                assert!((radii.1 - 2.0).abs() < 1e-9, "axis1 unchanged");
                assert!(
                    (orientation - FRAC_PI_4).abs() < 1e-9,
                    "orientation = cursor angle: {orientation}"
                );
            }
            other => panic!("{other:?}"),
        }
    }

    #[test]
    fn ellipse_axis1_handle_drag_sets_perpendicular_orientation() {
        use std::f64::consts::FRAC_PI_2;
        // axis1 is perpendicular to orientation, so dragging it to +x (angle 0)
        // makes orientation = 0 − π/2 = −π/2; the semi-axis becomes the distance.
        let mut roi = Roi::Ellipse {
            center: (0.0, 0.0),
            radii: (3.0, 2.0),
            orientation: 0.0,
        };
        roi.move_edge(RoiEdge::Vertex(2), (4.0, 0.0));
        match roi {
            Roi::Ellipse {
                radii, orientation, ..
            } => {
                assert!((radii.1 - 4.0).abs() < 1e-9, "axis1 = 4: {radii:?}");
                assert!(
                    (orientation + FRAC_PI_2).abs() < 1e-9,
                    "axis1→+x ⟹ θ = −π/2: {orientation}"
                );
            }
            other => panic!("{other:?}"),
        }
    }

    #[test]
    fn ellipse_contains_respects_orientation() {
        use std::f64::consts::FRAC_PI_2;
        // Rotated 90°: the long semi-axis (radii.0 = 4) now points along +y and
        // the short one (radii.1 = 2) along ±x.
        let roi = Roi::Ellipse {
            center: (0.0, 0.0),
            radii: (4.0, 2.0),
            orientation: FRAC_PI_2,
        };
        assert!(roi.contains((0.0, 4.0))); // axis0 tip, now vertical
        assert!(!roi.contains((0.0, 4.001)));
        assert!(roi.contains((2.0, 0.0))); // axis1 tip, now horizontal
        assert!(!roi.contains((2.001, 0.0)));
        // The pre-rotation +x tip (4, 0) is now outside the rotated ellipse.
        assert!(!roi.contains((4.0, 0.0)));
    }

    #[test]
    fn ellipse_handles_follow_orientation() {
        use std::f64::consts::FRAC_PI_2;
        let roi = Roi::Ellipse {
            center: (1.0, 1.0),
            radii: (4.0, 2.0),
            orientation: FRAC_PI_2,
        };
        let hs = roi.handles();
        // axis0 handle: center + radii.0·(cos90°, sin90°) = (1, 5).
        assert!(
            (hs[0].pos[0] - 1.0).abs() < 1e-9 && (hs[0].pos[1] - 5.0).abs() < 1e-9,
            "{:?}",
            hs[0].pos
        );
        // axis1 handle: center + radii.1·(−sin90°, cos90°) = (−1, 1).
        assert!(
            (hs[1].pos[0] + 1.0).abs() < 1e-9 && (hs[1].pos[1] - 1.0).abs() < 1e-9,
            "{:?}",
            hs[1].pos
        );
        // translate handle stays at the center.
        assert!((hs[2].pos[0] - 1.0).abs() < 1e-9 && (hs[2].pos[1] - 1.0).abs() < 1e-9);
    }

    #[test]
    fn ellipse_aabb_half_extents_axis_aligned_and_rotated() {
        use std::f64::consts::{FRAC_PI_2, FRAC_PI_4};
        // θ = 0: the bounding box half-extents are just the semi-axes.
        let (hx, hy) = ellipse_aabb_half_extents((4.0, 2.0), 0.0);
        assert!((hx - 4.0).abs() < 1e-9 && (hy - 2.0).abs() < 1e-9);
        // θ = 90°: the axes swap.
        let (hx, hy) = ellipse_aabb_half_extents((4.0, 2.0), FRAC_PI_2);
        assert!((hx - 2.0).abs() < 1e-9 && (hy - 4.0).abs() < 1e-9);
        // A circle's bounding box is orientation-invariant.
        let (hx, hy) = ellipse_aabb_half_extents((3.0, 3.0), FRAC_PI_4);
        assert!((hx - 3.0).abs() < 1e-9 && (hy - 3.0).abs() < 1e-9);
    }

    #[test]
    fn polygon_contains_inside_outside() {
        // Axis-aligned square (0,0)-(4,4) wound counter-clockwise.
        let roi = Roi::Polygon {
            vertices: vec![(0.0, 0.0), (4.0, 0.0), (4.0, 4.0), (0.0, 4.0)],
        };
        assert!(roi.contains((2.0, 2.0))); // clearly inside
        assert!(!roi.contains((5.0, 2.0))); // outside in x
        assert!(!roi.contains((2.0, -1.0))); // outside in y
        // A triangle, to exercise a non-rectangular crossing.
        let tri = Roi::Polygon {
            vertices: vec![(0.0, 0.0), (4.0, 0.0), (0.0, 4.0)],
        };
        assert!(tri.contains((1.0, 1.0))); // inside the lower-left half
        assert!(!tri.contains((3.0, 3.0))); // above the hypotenuse
        // Fewer than 3 vertices is never inside (matches degenerate polygons).
        let line = Roi::Polygon {
            vertices: vec![(0.0, 0.0), (4.0, 0.0)],
        };
        assert!(!line.contains((2.0, 0.0)));
    }

    #[test]
    fn line_contains_unit_square_intersection() {
        // Horizontal segment along y=5 from x=2 to x=8. silx LineROI.contains:
        // a position is "inside" when it is within the endpoints' bounding box
        // (here x in [2,8], y in [5,5] — degenerate in y) AND the unit square
        // at its lower-left corner is crossed by the segment.
        let roi = Roi::Line {
            start: (2.0, 5.0),
            end: (8.0, 5.0),
        };
        // Corner (4, 5): in the bbox; square y in [5, 6]; segment on the bottom
        // edge (a touching intersection is counted).
        assert!(roi.contains((4.0, 5.0)));
        // R2-26: Corner (4, 4.5) — the unit square y in [4.5, 5.5] would cross
        // y=5, but 4.5 is below the endpoints' bbox (y in [5,5]); silx's gate
        // excludes it. (This was the divergent True before the fix.)
        assert!(!roi.contains((4.0, 4.5)));
        // Corner (4, 6): square y in [6, 7], entirely above the segment.
        assert!(!roi.contains((4.0, 6.0)));
        // Corner far to the right in x: square x in [9, 10], past the segment end
        // AND outside the bbox (x > 8).
        assert!(!roi.contains((9.0, 4.5)));
    }

    #[test]
    fn line_contains_bbox_gate_trims_beyond_endpoint_strip() {
        // R2-26: for a diagonal segment (2,2)->(8,8) the endpoints' bbox is the
        // full [2,8]×[2,8] rectangle, so the one-unit unit-square tolerance is
        // trimmed only where it extends PAST an endpoint.
        let roi = Roi::Line {
            start: (2.0, 2.0),
            end: (8.0, 8.0),
        };
        // (1.5, 1.5): the unit square [1.5,2.5]×[1.5,2.5] contains the start
        // endpoint (2,2), so the raw unit-square test says "inside" — but the
        // point is below-left of the start, outside the bbox (x,y < 2). silx's
        // gate excludes it.
        assert!(!roi.contains((1.5, 1.5)));
        // (5.0, 5.0): on the segment and inside the bbox -> inside.
        assert!(roi.contains((5.0, 5.0)));
        // (5.0, 4.5): inside the bbox; unit square [5,6]×[4.5,5.5] is crossed by
        // y=x (touches at (5,5)) -> inside (the upper-right tolerance band).
        assert!(roi.contains((5.0, 4.5)));
    }

    // --- handle geometry tests (counts per ROI kind, translate invariant) ---

    fn kinds(handles: &[RoiHandle]) -> Vec<HandleKind> {
        handles.iter().map(|h| h.kind).collect()
    }

    #[test]
    fn handle_counts_and_roles_per_kind() {
        use HandleKind::*;
        // Rect: 4 corner vertices + a translate center.
        let rect = Roi::Rect {
            x: (2.0, 8.0),
            y: (3.0, 7.0),
        };
        assert_eq!(
            kinds(&rect.handles()),
            vec![Vertex, Vertex, Vertex, Vertex, Translate]
        );
        // The 4 vertices are exactly the 4 corners.
        let corners: Vec<[f64; 2]> = rect.handles()[..4].iter().map(|h| h.pos).collect();
        assert!(corners.contains(&[2.0, 3.0]));
        assert!(corners.contains(&[8.0, 7.0]));
        assert_eq!(rect.handles()[4].pos, [5.0, 5.0]); // center

        // HRange / VRange: two edge limits + a center.
        assert_eq!(
            kinds(&Roi::HRange { y: (3.0, 7.0) }.handles()),
            vec![Edge, Edge, Center]
        );
        assert_eq!(
            kinds(&Roi::VRange { x: (2.0, 8.0) }.handles()),
            vec![Edge, Edge, Center]
        );

        // Point: one vertex. Cross: one square (Vertex) drag handle, matching
        // silx `addHandle()` default symbol "s".
        assert_eq!(
            kinds(&Roi::Point { x: 1.0, y: 2.0 }.handles()),
            vec![Vertex]
        );
        assert_eq!(
            kinds(&Roi::Cross { center: (1.0, 2.0) }.handles()),
            vec![Vertex]
        );

        // Line: 2 endpoint vertices + translate center.
        assert_eq!(
            kinds(
                &Roi::Line {
                    start: (0.0, 0.0),
                    end: (4.0, 2.0),
                }
                .handles()
            ),
            vec![Vertex, Vertex, Translate]
        );

        // Polygon: N vertices + translate; empty polygon has no handles.
        let poly = Roi::Polygon {
            vertices: vec![(0.0, 0.0), (4.0, 0.0), (4.0, 4.0)],
        };
        assert_eq!(
            kinds(&poly.handles()),
            vec![Vertex, Vertex, Vertex, Translate]
        );
        assert!(
            Roi::Polygon {
                vertices: Vec::new()
            }
            .handles()
            .is_empty()
        );

        // Circle: perimeter vertex + translate center.
        assert_eq!(
            kinds(
                &Roi::Circle {
                    center: (5.0, 5.0),
                    radius: 2.0,
                }
                .handles()
            ),
            vec![Vertex, Translate]
        );
        // Ellipse: two axis vertices + translate center.
        assert_eq!(
            kinds(
                &Roi::Ellipse {
                    center: (5.0, 5.0),
                    radii: (4.0, 2.0),
                    orientation: 0.0,
                }
                .handles()
            ),
            vec![Vertex, Vertex, Translate]
        );
    }

    #[test]
    fn translate_moves_every_2d_handle_by_the_same_delta() {
        // Shapes with genuine 2D positions: every handle shifts by (dx, dy).
        let rois = [
            Roi::Rect {
                x: (2.0, 8.0),
                y: (3.0, 7.0),
            },
            Roi::Point { x: 1.0, y: 2.0 },
            Roi::Cross { center: (1.0, 2.0) },
            Roi::Line {
                start: (0.0, 0.0),
                end: (4.0, 2.0),
            },
            Roi::Polygon {
                vertices: vec![(0.0, 0.0), (4.0, 0.0), (4.0, 4.0)],
            },
            Roi::Circle {
                center: (5.0, 5.0),
                radius: 2.0,
            },
            Roi::Ellipse {
                center: (5.0, 5.0),
                radii: (4.0, 2.0),
                orientation: 0.0,
            },
        ];
        let (dx, dy) = (1.5, -0.5);
        for roi in rois {
            let before = roi.handles();
            let mut moved = roi.clone();
            moved.translate(dx, dy);
            let after = moved.handles();
            assert_eq!(before.len(), after.len());
            for (b, a) in before.iter().zip(&after) {
                assert_eq!(a.kind, b.kind);
                assert!((a.pos[0] - (b.pos[0] + dx)).abs() < 1e-9, "{roi:?}");
                assert!((a.pos[1] - (b.pos[1] + dy)).abs() < 1e-9, "{roi:?}");
            }
        }
    }

    // --- Arc / Band contains() and handle tests ---

    #[test]
    fn arc_contains_inside_outside_ring_and_sweep() {
        // Quarter ring in the first quadrant: r in [1, 2], theta in [0, pi/2].
        let arc = Roi::Arc {
            center: (0.0, 0.0),
            radius: 1.5,
            weight: 1.0,
            start_angle: 0.0,
            end_angle: std::f64::consts::FRAC_PI_2,
        };
        // Inside the ring and inside the sweep.
        assert!(arc.contains((1.5, 0.0))); // on the +x ray, mid radius
        assert!(arc.contains((0.0, 1.5))); // on the +y ray (end angle, inclusive)
        let d = std::f64::consts::FRAC_1_SQRT_2 * 1.5;
        assert!(arc.contains((d, d))); // 45 deg, mid radius
        // Outside the radius ring.
        assert!(!arc.contains((0.5, 0.0))); // inside inner radius
        assert!(!arc.contains((2.5, 0.0))); // beyond outer radius
        // Outside the angular sweep (third quadrant ray, in-radius).
        assert!(!arc.contains((-1.5, 0.0))); // theta = pi
        assert!(!arc.contains((0.0, -1.5))); // theta = -pi/2
    }

    #[test]
    fn arc_contains_handles_the_pi_branch_wrap() {
        // Left-side sweep crossing the +/-pi branch: theta in [3pi/4, 5pi/4].
        let arc = Roi::Arc {
            center: (0.0, 0.0),
            radius: 1.5,
            weight: 1.0,
            start_angle: 3.0 * std::f64::consts::FRAC_PI_4,
            end_angle: 5.0 * std::f64::consts::FRAC_PI_4,
        };
        assert!(arc.contains((-1.5, 0.0))); // theta = pi, within the sweep
        assert!(!arc.contains((1.5, 0.0))); // theta = 0, outside
        assert!(!arc.contains((0.0, -1.5))); // theta = -pi/2, outside
    }

    #[test]
    fn band_contains_axis_aligned_inside_edge_outside() {
        // Horizontal band along y=0 from x=0..4 with width 2: rect x∈[0,4], y∈[-1,1].
        let band = Roi::Band {
            begin: (0.0, 0.0),
            end: (4.0, 0.0),
            width: 2.0,
        };
        assert!(band.contains((2.0, 0.0))); // strictly inside
        assert!(band.contains((2.0, 0.5))); // inside across the width
        assert!(!band.contains((2.0, 1.5))); // past the upper band edge
        assert!(!band.contains((2.0, -1.5))); // past the lower band edge
        assert!(!band.contains((5.0, 0.0))); // past the end along the segment
        assert!(!band.contains((-0.5, 0.0))); // before the begin along the segment
    }

    #[test]
    fn band_contains_rotated_band() {
        // Vertical band begin=(0,0) end=(0,4) width 2: rect x∈[-1,1], y∈[0,4].
        let band = Roi::Band {
            begin: (0.0, 0.0),
            end: (0.0, 4.0),
            width: 2.0,
        };
        assert!(band.contains((0.0, 2.0))); // inside
        assert!(!band.contains((1.5, 2.0))); // past the band edge (normal is x)
        assert!(!band.contains((0.0, 5.0))); // past the end
    }

    #[test]
    fn band_lines_horizontal_band_centres_on_the_segment() {
        // begin=(0,0) end=(4,0) width 2: middle y=0, edges y=±1 (silx Unbounded).
        let band = Roi::Band {
            begin: (0.0, 0.0),
            end: (4.0, 0.0),
            width: 2.0,
        };
        match band.band_lines().expect("band") {
            BandLines::Sloped {
                slope,
                middle,
                edges,
            } => {
                assert_eq!(slope, 0.0);
                assert_eq!(middle, 0.0);
                assert_eq!(edges, (-1.0, 1.0));
            }
            other => panic!("{other:?}"),
        }
    }

    #[test]
    fn band_lines_vertical_band_uses_x_constants() {
        // begin=(0,0) end=(0,4) width 2: vertical, middle x=0, edges x=±1.
        let band = Roi::Band {
            begin: (0.0, 0.0),
            end: (0.0, 4.0),
            width: 2.0,
        };
        match band.band_lines().expect("band") {
            BandLines::Vertical { middle, edges } => {
                assert_eq!(middle, 0.0);
                // normal of a +y segment is (-1, 0): edges at x = 0 ∓ off.0.
                assert_eq!(edges, (1.0, -1.0));
            }
            other => panic!("{other:?}"),
        }
    }

    #[test]
    fn band_lines_diagonal_band_edges_are_parallel_offsets() {
        // begin=(0,0) end=(2,2) width 2√2: slope 1, middle c=0, edges c=±2.
        let band = Roi::Band {
            begin: (0.0, 0.0),
            end: (2.0, 2.0),
            width: 2.0 * std::f64::consts::SQRT_2,
        };
        match band.band_lines().expect("band") {
            BandLines::Sloped {
                slope,
                middle,
                edges,
            } => {
                assert!((slope - 1.0).abs() <= 1e-12, "slope={slope}");
                assert!(middle.abs() <= 1e-12, "middle={middle}");
                assert!((edges.0 - (-2.0)).abs() <= 1e-12, "e0={}", edges.0);
                assert!((edges.1 - 2.0).abs() <= 1e-12, "e1={}", edges.1);
            }
            other => panic!("{other:?}"),
        }
    }

    #[test]
    fn band_lines_is_none_for_non_band() {
        let rect = Roi::Rect {
            x: (0.0, 1.0),
            y: (0.0, 1.0),
        };
        assert_eq!(rect.band_lines(), None);
    }

    #[test]
    fn band_unbounded_segments_span_horizontal_band_across_x() {
        // A horizontal band along y=0, width 2 → centre at y=0, edges at y=±1,
        // each drawn across the x-extents.
        let band = Roi::Band {
            begin: (0.0, 0.0),
            end: (4.0, 0.0),
            width: 2.0,
        };
        let segs = band
            .band_unbounded_segments((-5.0, 5.0), (-9.0, 9.0))
            .expect("band has unbounded segments");
        // Centre line y=0 spanned at x=-5..5.
        assert_eq!(segs[0], [(-5.0, 0.0), (5.0, 0.0)]);
        // Edge lines y=-1 and y=+1 (begin−off then begin+off; off = 0.5·2·normal,
        // normal of (1,0) is (0,1) → off=(0,1)).
        assert_eq!(segs[1], [(-5.0, -1.0), (5.0, -1.0)]);
        assert_eq!(segs[2], [(-5.0, 1.0), (5.0, 1.0)]);
    }

    #[test]
    fn band_unbounded_segments_span_vertical_band_across_y() {
        // A vertical band along x=3, width 2 → centre at x=3, edges at x=2 and 4,
        // each drawn across the y-extents.
        let band = Roi::Band {
            begin: (3.0, 0.0),
            end: (3.0, 5.0),
            width: 2.0,
        };
        let segs = band
            .band_unbounded_segments((-1.0, 1.0), (-8.0, 8.0))
            .expect("vertical band has unbounded segments");
        // Edge order follows band_lines `(begin.x − off.x, begin.x + off.x)`; the
        // upward band's normal is (−1, 0) so off.x = −1 → edges come out (4, 2).
        assert_eq!(segs[0], [(3.0, -8.0), (3.0, 8.0)]);
        assert_eq!(segs[1], [(4.0, -8.0), (4.0, 8.0)]);
        assert_eq!(segs[2], [(2.0, -8.0), (2.0, 8.0)]);
    }

    #[test]
    fn band_unbounded_segments_is_none_for_non_band() {
        assert_eq!(
            Roi::Point { x: 1.0, y: 2.0 }.band_unbounded_segments((0.0, 1.0), (0.0, 1.0)),
            None
        );
    }

    #[test]
    fn arc_and_band_handle_counts() {
        use HandleKind::*;
        // Arc: 4 shape vertices (mid/outer/start/end) + a translate center.
        let arc = Roi::Arc {
            center: (0.0, 0.0),
            radius: 1.5,
            weight: 1.0,
            start_angle: 0.0,
            end_angle: std::f64::consts::FRAC_PI_2,
        };
        assert_eq!(
            kinds(&arc.handles()),
            vec![Vertex, Vertex, Vertex, Vertex, Translate]
        );
        // The translate handle is at the arc center.
        assert_eq!(arc.handles().last().unwrap().pos, [0.0, 0.0]);

        // Band: begin/end + 2 width vertices + a translate center.
        let band = Roi::Band {
            begin: (0.0, 0.0),
            end: (4.0, 0.0),
            width: 2.0,
        };
        assert_eq!(
            kinds(&band.handles()),
            vec![Vertex, Vertex, Vertex, Vertex, Translate]
        );
        // begin/end handles are at the endpoints; center at the midpoint.
        assert_eq!(band.handles()[0].pos, [0.0, 0.0]);
        assert_eq!(band.handles()[1].pos, [4.0, 0.0]);
        assert_eq!(band.handles().last().unwrap().pos, [2.0, 0.0]);
    }

    #[test]
    fn arc_and_band_translate_move_every_handle() {
        let (dx, dy) = (2.0, -1.0);
        let rois = [
            Roi::Arc {
                center: (1.0, 1.0),
                radius: 1.5,
                weight: 1.0,
                start_angle: 0.0,
                end_angle: std::f64::consts::FRAC_PI_2,
            },
            Roi::Band {
                begin: (0.0, 0.0),
                end: (4.0, 2.0),
                width: 1.5,
            },
        ];
        for roi in rois {
            let before = roi.handles();
            let mut moved = roi.clone();
            moved.translate(dx, dy);
            let after = moved.handles();
            assert_eq!(before.len(), after.len());
            for (b, a) in before.iter().zip(&after) {
                assert_eq!(a.kind, b.kind);
                assert!((a.pos[0] - (b.pos[0] + dx)).abs() < 1e-9, "{roi:?}");
                assert!((a.pos[1] - (b.pos[1] + dy)).abs() < 1e-9, "{roi:?}");
            }
        }
    }

    #[test]
    fn translate_band_rois_move_only_the_bounded_axis() {
        // A horizontal band has no x position; translate moves only its y limits.
        let mut h = Roi::HRange { y: (3.0, 7.0) };
        h.translate(1.5, -0.5);
        assert_eq!(h, Roi::HRange { y: (2.5, 6.5) });
        // A vertical band moves only its x limits.
        let mut v = Roi::VRange { x: (2.0, 8.0) };
        v.translate(1.5, -0.5);
        assert_eq!(v, Roi::VRange { x: (3.5, 9.5) });
    }

    // --- ManagedRoi / RoiLineStyle ---

    #[test]
    fn new_managed_roi_uses_silx_style_defaults() {
        // silx RegionOfInterest defaults: linewidth 1.0, solid, unfilled.
        let r = ManagedRoi::new(Roi::Point { x: 0.0, y: 0.0 });
        assert_eq!(r.color, None);
        assert!(r.name.is_empty());
        assert!(!r.selected);
        assert_eq!(r.line_width, 1.0);
        assert_eq!(r.line_style, RoiLineStyle::Solid);
        assert!(!r.fill);
    }

    #[test]
    fn roi_line_style_maps_to_painter_line_style() {
        assert_eq!(RoiLineStyle::Solid.to_line_style(), LineStyle::Solid);
        assert_eq!(RoiLineStyle::Dashed.to_line_style(), LineStyle::Dashed);
        assert_eq!(RoiLineStyle::Dotted.to_line_style(), LineStyle::Dotted);
    }

    // --- RoiInteractionMode (silx InteractionModeMixIn) ---

    fn sample_arc() -> Roi {
        Roi::Arc {
            center: (0.0, 0.0),
            radius: 1.5,
            weight: 1.0,
            start_angle: 0.0,
            end_angle: std::f64::consts::FRAC_PI_2,
        }
    }

    fn sample_band() -> Roi {
        Roi::Band {
            begin: (0.0, 0.0),
            end: (4.0, 0.0),
            width: 1.0,
        }
    }

    #[test]
    fn available_interaction_modes_match_silx_per_kind() {
        // silx ArcROI.availableInteractionModes -> [ThreePoint, Polar, Move].
        assert_eq!(
            sample_arc().available_interaction_modes(),
            &[
                RoiInteractionMode::ArcThreePoint,
                RoiInteractionMode::ArcPolar,
                RoiInteractionMode::ArcTranslation,
            ]
        );
        // silx BandROI -> {Bounded, Unbounded}.
        assert_eq!(
            sample_band().available_interaction_modes(),
            &[
                RoiInteractionMode::BandBounded,
                RoiInteractionMode::BandUnbounded,
            ]
        );
        // Every kind without InteractionModeMixIn offers no modes.
        assert!(
            Roi::Point { x: 0.0, y: 0.0 }
                .available_interaction_modes()
                .is_empty()
        );
        assert!(
            Roi::Rect {
                x: (0.0, 1.0),
                y: (0.0, 1.0),
            }
            .available_interaction_modes()
            .is_empty()
        );
    }

    #[test]
    fn default_interaction_mode_is_the_silx_init_mode() {
        // silx _initInteractionMode: Arc -> ThreePointMode, Band -> BoundedMode.
        assert_eq!(
            sample_arc().default_interaction_mode(),
            Some(RoiInteractionMode::ArcThreePoint)
        );
        assert_eq!(
            sample_band().default_interaction_mode(),
            Some(RoiInteractionMode::BandBounded)
        );
        assert_eq!(
            Roi::Point { x: 0.0, y: 0.0 }.default_interaction_mode(),
            None
        );
    }

    #[test]
    fn new_managed_roi_seeds_the_kind_default_mode() {
        assert_eq!(
            ManagedRoi::new(sample_arc()).interaction_mode(),
            Some(RoiInteractionMode::ArcThreePoint)
        );
        assert_eq!(
            ManagedRoi::new(sample_band()).interaction_mode(),
            Some(RoiInteractionMode::BandBounded)
        );
        assert_eq!(
            ManagedRoi::new(Roi::Point { x: 0.0, y: 0.0 }).interaction_mode(),
            None
        );
    }

    #[test]
    fn set_interaction_mode_accepts_available_and_rejects_foreign() {
        let mut arc = ManagedRoi::new(sample_arc());
        // An available mode switches and reports success.
        assert!(arc.set_interaction_mode(RoiInteractionMode::ArcPolar));
        assert_eq!(arc.interaction_mode(), Some(RoiInteractionMode::ArcPolar));
        // A mode belonging to another kind is rejected; the mode is unchanged.
        assert!(!arc.set_interaction_mode(RoiInteractionMode::BandBounded));
        assert_eq!(arc.interaction_mode(), Some(RoiInteractionMode::ArcPolar));
        // A kind without modes rejects every mode and stays None.
        let mut point = ManagedRoi::new(Roi::Point { x: 0.0, y: 0.0 });
        assert!(!point.set_interaction_mode(RoiInteractionMode::ArcThreePoint));
        assert_eq!(point.interaction_mode(), None);
    }

    #[test]
    fn mode_label_and_description_match_silx_strings() {
        assert_eq!(RoiInteractionMode::ArcThreePoint.label(), "3 points");
        assert_eq!(
            RoiInteractionMode::ArcThreePoint.description(),
            "Provides 3 points to define the main radius circle"
        );
        assert_eq!(RoiInteractionMode::ArcPolar.label(), "Polar");
        assert_eq!(
            RoiInteractionMode::ArcPolar.description(),
            "Provides anchors to edit the ROI in polar coords"
        );
        assert_eq!(RoiInteractionMode::ArcTranslation.label(), "Translation");
        assert_eq!(
            RoiInteractionMode::ArcTranslation.description(),
            "Provides anchors to only move the ROI"
        );
        assert_eq!(RoiInteractionMode::BandBounded.label(), "Bounded");
        assert_eq!(
            RoiInteractionMode::BandBounded.description(),
            "Band is bounded on both sides"
        );
        assert_eq!(RoiInteractionMode::BandUnbounded.label(), "Unbounded");
        assert_eq!(
            RoiInteractionMode::BandUnbounded.description(),
            "Band is unbounded on both sides"
        );
    }
}
