//! Regions of interest (ROIs): rectangular, horizontal-band, and vertical-band
//! selections drawn over the data area with draggable edge handles.
//!
//! The geometry is data-space and the hit-testing / edge-move math is pure (no
//! egui input), so it is unit-testable; the widget wires pointer drags to
//! [`Roi::edge_at`] and [`Roi::move_edge`] and emits a change when an edge moves
//! (silx `RegionOfInterest`, `doc/design.md` §13 C3).

use egui::{Pos2, Rect};

use crate::core::transform::Transform;

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
    /// Axis-aligned ellipse with semi-axes `radii = (x_radius, y_radius)` (silx
    /// `EllipseROI` with no orientation). Movable center plus one handle per
    /// semi-axis.
    Ellipse {
        center: (f64, f64),
        radii: (f64, f64),
    },
    /// An annular sector (silx `ArcROI`): the ring between `inner_radius` and
    /// `outer_radius` around `center`, swept from `start_angle` to `end_angle`
    /// (radians; if `start_angle > end_angle` the sweep is the other way). A
    /// full `2π` sweep is a circle/donut.
    Arc {
        center: (f64, f64),
        inner_radius: f64,
        outer_radius: f64,
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
            Roi::Ellipse { center, radii } => {
                let a = t.data_to_pixel(center.0 - radii.0, center.1 - radii.1);
                let b = t.data_to_pixel(center.0 + radii.0, center.1 + radii.1);
                Rect::from_two_pos(a, b)
            }
            Roi::Arc {
                center,
                outer_radius,
                ..
            } => {
                // Bounding box of the outer circle, mapped to screen.
                let a = t.data_to_pixel(center.0 - outer_radius, center.1 - outer_radius);
                let b = t.data_to_pixel(center.0 + outer_radius, center.1 + outer_radius);
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
            // handles are an egui-silx addition that preserves single-axis
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
            Roi::Ellipse { center, radii } => match index {
                0 => *center,
                // x-axis handle at center + (x_radius, 0).
                1 => (center.0 + radii.0, center.1),
                // y-axis handle at center + (0, y_radius).
                2 => (center.0, center.1 + radii.1),
                _ => return None,
            },
            // Arc shape vertices: 0=mid, 1=outer/weight, 2=start, 3=end.
            Roi::Arc { .. } => arc_vertex_pos(self, index)?,
            // Band shape vertices: 0=begin, 1=end, 2=width-up, 3=width-down.
            Roi::Band { .. } => band_vertex_pos(self, index)?,
            _ => return None,
        };
        Some(t.data_to_pixel(x, y))
    }

    /// Screen-space midpoints of this ROI's draggable edges, for drawing handle
    /// marks (one per edge, in [`Roi::edges`] order).
    pub fn handle_centers(&self, t: &Transform) -> Vec<Pos2> {
        let r = self.screen_rect(t);
        self.edges()
            .iter()
            .map(|edge| match edge {
                RoiEdge::Left => egui::pos2(r.left(), r.center().y),
                RoiEdge::Right => egui::pos2(r.right(), r.center().y),
                RoiEdge::Top => egui::pos2(r.center().x, r.top()),
                RoiEdge::Bottom => egui::pos2(r.center().x, r.bottom()),
                RoiEdge::BottomLeft => egui::pos2(r.left(), r.bottom()),
                RoiEdge::BottomRight => egui::pos2(r.right(), r.bottom()),
                RoiEdge::TopLeft => egui::pos2(r.left(), r.top()),
                RoiEdge::TopRight => egui::pos2(r.right(), r.top()),
                RoiEdge::Vertex(n) => self.vertex_pixel(t, *n).unwrap_or(r.center()),
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
                // Rect, HRange, VRange: rect-based edge detection.
                let r = self.screen_rect(t);
                // Corner handles (Rect only) take priority: a cursor near a
                // corner is also near both adjoining edges, so resolve corners
                // first by Euclidean distance to the corner point. The closest
                // in-range corner wins, giving diagonal resize precedence over
                // single-axis edge resize at the rectangle's corners.
                let mut best_corner: Option<(RoiEdge, f32)> = None;
                for edge in self.edges() {
                    let corner = match edge {
                        RoiEdge::BottomLeft => egui::pos2(r.left(), r.bottom()),
                        RoiEdge::BottomRight => egui::pos2(r.right(), r.bottom()),
                        RoiEdge::TopLeft => egui::pos2(r.left(), r.top()),
                        RoiEdge::TopRight => egui::pos2(r.right(), r.top()),
                        _ => continue,
                    };
                    let dist = cursor.distance(corner);
                    if dist <= grab_px && best_corner.is_none_or(|(_, d)| dist < d) {
                        best_corner = Some((edge, dist));
                    }
                }
                if let Some((edge, _)) = best_corner {
                    return Some(edge);
                }
                let mut best: Option<(RoiEdge, f32)> = None;
                for edge in self.edges() {
                    let dist = match edge {
                        // Vertical edges: cursor must be within the rect's y span.
                        RoiEdge::Left | RoiEdge::Right => {
                            if cursor.y < r.top() - grab_px || cursor.y > r.bottom() + grab_px {
                                continue;
                            }
                            let ex = if edge == RoiEdge::Left {
                                r.left()
                            } else {
                                r.right()
                            };
                            (cursor.x - ex).abs()
                        }
                        // Horizontal edges: cursor must be within the rect's x span.
                        RoiEdge::Bottom | RoiEdge::Top => {
                            if cursor.x < r.left() - grab_px || cursor.x > r.right() + grab_px {
                                continue;
                            }
                            // Top edge = data y.max = screen top (smaller y).
                            let ey = if edge == RoiEdge::Top {
                                r.top()
                            } else {
                                r.bottom()
                            };
                            (cursor.y - ey).abs()
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
            Roi::Rect { x, y } => match edge {
                RoiEdge::Left => x.0 = dx.min(x.1),
                RoiEdge::Right => x.1 = dx.max(x.0),
                RoiEdge::Bottom => y.0 = dy.min(y.1),
                RoiEdge::Top => y.1 = dy.max(y.0),
                // Corner handles move both the x and y edge they own, giving
                // diagonal resize. x and y are independent tuples, so the two
                // assignments never interfere.
                RoiEdge::BottomLeft => {
                    x.0 = dx.min(x.1);
                    y.0 = dy.min(y.1);
                }
                RoiEdge::BottomRight => {
                    x.1 = dx.max(x.0);
                    y.0 = dy.min(y.1);
                }
                RoiEdge::TopLeft => {
                    x.0 = dx.min(x.1);
                    y.1 = dy.max(y.0);
                }
                RoiEdge::TopRight => {
                    x.1 = dx.max(x.0);
                    y.1 = dy.max(y.0);
                }
                RoiEdge::Vertex(_) => {}
            },
            Roi::HRange { y } => match edge {
                RoiEdge::Bottom => y.0 = dy.min(y.1),
                RoiEdge::Top => y.1 = dy.max(y.0),
                _ => {}
            },
            Roi::VRange { x } => match edge {
                RoiEdge::Left => x.0 = dx.min(x.1),
                RoiEdge::Right => x.1 = dx.max(x.0),
                _ => {}
            },
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
            Roi::Ellipse { center, radii } => match edge {
                // Center handle translates the whole ellipse.
                RoiEdge::Vertex(0) => *center = (dx, dy),
                // x-axis handle sets the x semi-axis; y-axis handle the y one.
                RoiEdge::Vertex(1) => radii.0 = (dx - center.0).abs(),
                RoiEdge::Vertex(2) => radii.1 = (dy - center.1).abs(),
                _ => {}
            },
            // Arc handle drag — PolarMode editing, faithful to our polar
            // `{center, inner_radius, outer_radius, start_angle, end_angle}`
            // representation (silx `ArcROI.handleDragUpdated` PolarMode branch).
            // silx's default ThreePointMode (a circumcircle through three
            // start/mid/end control points) needs a point-based geometry we do
            // not store, so PolarMode is the faithful match for our model.
            Roi::Arc {
                center,
                inner_radius,
                outer_radius,
                start_angle,
                end_angle,
            } => {
                let (cx, cy) = *center;
                let mid = (*inner_radius + *outer_radius) * 0.5;
                match edge {
                    // Mid handle (Vertex 0) → central radius, conserving the
                    // thickness (silx `withRadius`: weight = outer − inner kept).
                    RoiEdge::Vertex(0) => {
                        let r = (dx - cx).hypot(dy - cy);
                        let w = *outer_radius - *inner_radius;
                        *inner_radius = (r - w * 0.5).max(0.0);
                        *outer_radius = r + w * 0.5;
                    }
                    // Weight handle (Vertex 1) → thickness, symmetric about the
                    // mid radius (silx `_getWeightFromHandle`:
                    // `weight = 2·|d − radius|`, `d = |center − handle|`).
                    RoiEdge::Vertex(1) => {
                        let d = (dx - cx).hypot(dy - cy);
                        let w = 2.0 * (d - mid).abs();
                        *inner_radius = (mid - w * 0.5).max(0.0);
                        *outer_radius = mid + w * 0.5;
                    }
                    // Start / end handles (Vertex 2 / 3) → sweep angles (silx
                    // `withStartAngle` / `withEndAngle`).
                    RoiEdge::Vertex(2) => *start_angle = (dy - cy).atan2(dx - cx),
                    RoiEdge::Vertex(3) => *end_angle = (dy - cy).atan2(dx - cx),
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
    /// - `Ellipse`: `(dx/major)² + (dy/minor)² <= 1` with `major = max(radii)`,
    ///   `minor = min(radii)` (`EllipseROI` at orientation 0).
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
            Roi::Point { x: px, y: py } => x == *px && y == *py,
            Roi::Cross { center } => x == center.0 || y == center.1,
            Roi::Line { start, end } => segment_intersects_unit_square(*start, *end, pos),
            Roi::Polygon { vertices } => point_in_polygon(vertices, pos),
            Roi::Circle { center, radius } => {
                let (dx, dy) = (x - center.0, y - center.1);
                (dx * dx + dy * dy).sqrt() <= *radius
            }
            Roi::Ellipse { center, radii } => {
                let major = radii.0.max(radii.1);
                let minor = radii.0.min(radii.1);
                if major <= 0.0 || minor <= 0.0 {
                    return false;
                }
                let (dx, dy) = (x - center.0, y - center.1);
                (dx * dx) / (major * major) + (dy * dy) / (minor * minor) <= 1.0
            }
            Roi::Arc {
                center,
                inner_radius,
                outer_radius,
                start_angle,
                end_angle,
            } => arc_contains(
                *center,
                *inner_radius,
                *outer_radius,
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
            // PointROI: a single vertex handle.
            Roi::Point { x, y } => vec![v((*x, *y))],
            // CrossROI: a single center handle.
            Roi::Cross { center: c } => vec![center(*c)],
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
            // EllipseROI: two axis vertices + a translate center.
            Roi::Ellipse { center: c, radii } => vec![
                v((c.0 + radii.0, c.1)),
                v((c.0, c.1 + radii.1)),
                translate(*c),
            ],
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

/// Data-space position of arc shape-vertex `index`, mirroring silx `ArcROI`'s
/// handle layout: 0 = mid (at the mid-radius `(inner+outer)/2`, mid angle),
/// 1 = outer/weight (the outer radius at the mid angle), 2 = start point,
/// 3 = end point. Returns `None` for any other index or a non-arc ROI.
fn arc_vertex_pos(roi: &Roi, index: usize) -> Option<(f64, f64)> {
    let Roi::Arc {
        center,
        inner_radius,
        outer_radius,
        start_angle,
        end_angle,
    } = roi
    else {
        return None;
    };
    let radius = (inner_radius + outer_radius) * 0.5;
    let mid_angle = (start_angle + end_angle) * 0.5;
    let at = |r: f64, a: f64| (center.0 + r * a.cos(), center.1 + r * a.sin());
    Some(match index {
        0 => at(radius, mid_angle),
        1 => at(*outer_radius, mid_angle),
        2 => at(radius, *start_angle),
        3 => at(radius, *end_angle),
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
        // Dragging a corner past the opposite one clamps on both axes.
        roi.move_edge(RoiEdge::TopRight, (-5.0, -5.0));
        assert_eq!(
            roi,
            Roi::Rect {
                x: (1.0, 1.0),
                y: (1.0, 1.0)
            }
        );
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
    fn move_edge_clamps_to_stay_normalized() {
        let mut roi = Roi::Rect {
            x: (2.0, 8.0),
            y: (3.0, 7.0),
        };
        // Drag the left edge past the right edge: it clamps at the right.
        roi.move_edge(RoiEdge::Left, (12.0, 5.0));
        assert_eq!(
            roi,
            Roi::Rect {
                x: (8.0, 8.0),
                y: (3.0, 7.0)
            }
        );
        // Normal move.
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
        };
        roi.move_edge(RoiEdge::Vertex(1), (5.0, 0.0)); // x semi-axis -> 5
        roi.move_edge(RoiEdge::Vertex(2), (0.0, 7.0)); // y semi-axis -> 7
        roi.move_edge(RoiEdge::Vertex(0), (2.0, 3.0)); // center
        assert_eq!(
            roi,
            Roi::Ellipse {
                center: (2.0, 3.0),
                radii: (5.0, 7.0),
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
        // center origin, inner 2 / outer 4 (mid radius 3, thickness 2),
        // sweep 0 → π/2.
        let base = Roi::Arc {
            center: (0.0, 0.0),
            inner_radius: 2.0,
            outer_radius: 4.0,
            start_angle: 0.0,
            end_angle: FRAC_PI_2,
        };
        let approx =
            |a: f64, b: f64, what: &str| assert!((a - b).abs() < 1e-9, "{what}: {a} vs {b}");

        // Mid handle → central radius 5, thickness 2 conserved → inner 4, outer 6.
        let mut roi = base.clone();
        roi.move_edge(RoiEdge::Vertex(0), (5.0, 0.0));
        if let Roi::Arc {
            inner_radius,
            outer_radius,
            start_angle,
            end_angle,
            ..
        } = roi
        {
            approx(inner_radius, 4.0, "mid inner");
            approx(outer_radius, 6.0, "mid outer");
            approx(start_angle, 0.0, "mid keeps start");
            approx(end_angle, FRAC_PI_2, "mid keeps end");
        } else {
            panic!("not an arc");
        }

        // Weight handle → thickness 2·|d − mid|. d = 6, mid = 3 → weight 6 →
        // inner 0, outer 6 (mid radius unchanged).
        let mut roi = base.clone();
        roi.move_edge(RoiEdge::Vertex(1), (6.0, 0.0));
        if let Roi::Arc {
            inner_radius,
            outer_radius,
            ..
        } = roi
        {
            approx(inner_radius, 0.0, "weight inner");
            approx(outer_radius, 6.0, "weight outer");
        } else {
            panic!("not an arc");
        }
        // Weight clamps the inner radius at 0: d = 10 → weight 14 →
        // inner max(3 − 7, 0) = 0, outer 10.
        let mut roi = base.clone();
        roi.move_edge(RoiEdge::Vertex(1), (10.0, 0.0));
        if let Roi::Arc {
            inner_radius,
            outer_radius,
            ..
        } = roi
        {
            approx(inner_radius, 0.0, "weight clamp inner");
            approx(outer_radius, 10.0, "weight clamp outer");
        } else {
            panic!("not an arc");
        }

        // Start / end handles set the sweep angles and leave the radii alone.
        let mut roi = base.clone();
        roi.move_edge(RoiEdge::Vertex(2), (0.0, 5.0));
        roi.move_edge(RoiEdge::Vertex(3), (-5.0, 0.0));
        if let Roi::Arc {
            inner_radius,
            outer_radius,
            start_angle,
            end_angle,
            ..
        } = roi
        {
            approx(start_angle, FRAC_PI_2, "start angle");
            approx(end_angle, PI, "end angle");
            approx(inner_radius, 2.0, "angle keeps inner");
            approx(outer_radius, 4.0, "angle keeps outer");
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
            radii: (4.0, 2.0), // major=4 (x), minor=2 (y)
        };
        assert!(roi.contains((5.0, 5.0))); // center
        assert!(roi.contains((9.0, 5.0))); // on the major-axis tip (x): 1.0 == 1.0
        assert!(roi.contains((5.0, 7.0))); // on the minor-axis tip (y)
        assert!(!roi.contains((5.0, 7.001))); // just past the minor tip
        assert!(!roi.contains((9.001, 5.0))); // just past the major tip
        // Degenerate (zero radius) contains nothing.
        let degenerate = Roi::Ellipse {
            center: (0.0, 0.0),
            radii: (0.0, 1.0),
        };
        assert!(!degenerate.contains((0.0, 0.0)));
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
        // Horizontal segment along y=5 from x=2 to x=8 (silx LineROI semantics:
        // a position is "inside" when the unit square at its lower-left corner
        // is crossed by the segment).
        let roi = Roi::Line {
            start: (2.0, 5.0),
            end: (8.0, 5.0),
        };
        // Corner (4, 4.5): unit square spans y in [4.5, 5.5], so y=5 crosses it.
        assert!(roi.contains((4.0, 4.5)));
        // Corner (4, 5): square y in [5, 6]; the segment lies on the bottom edge
        // (a touching intersection is counted).
        assert!(roi.contains((4.0, 5.0)));
        // Corner (4, 6): square y in [6, 7], entirely above the segment.
        assert!(!roi.contains((4.0, 6.0)));
        // Corner far to the right in x: square x in [9, 10], past the segment end.
        assert!(!roi.contains((9.0, 4.5)));
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

        // Point: one vertex. Cross: one center.
        assert_eq!(
            kinds(&Roi::Point { x: 1.0, y: 2.0 }.handles()),
            vec![Vertex]
        );
        assert_eq!(
            kinds(&Roi::Cross { center: (1.0, 2.0) }.handles()),
            vec![Center]
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
            inner_radius: 1.0,
            outer_radius: 2.0,
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
            inner_radius: 1.0,
            outer_radius: 2.0,
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
    fn arc_and_band_handle_counts() {
        use HandleKind::*;
        // Arc: 4 shape vertices (mid/outer/start/end) + a translate center.
        let arc = Roi::Arc {
            center: (0.0, 0.0),
            inner_radius: 1.0,
            outer_radius: 2.0,
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
                inner_radius: 1.0,
                outer_radius: 2.0,
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
}
