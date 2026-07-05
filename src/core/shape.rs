//! Shapes: polygon / rectangle / polyline / horizontal-line / vertical-line
//! annotations drawn over the data area (silx `BackendBase.addShape`).
//!
//! Like [`crate::core::marker::Marker`], a shape is a data-space annotation with
//! pure screen-placement math (unit-testable); the widget's chrome draws the
//! list each frame via [`crate::widget::chrome::draw_shapes`]. silx's `overlay`
//! flag chooses between the data layer and a separate overlay layer
//! (items/shape.py:54-73); [`Shape::is_overlay`] models it and the widget runs
//! two `draw_shapes` passes: non-overlay shapes in the base data layer (under
//! the overlay items), overlay shapes (the default) on top of the chrome like an
//! ROI (`doc/design.md` §8).

use egui::{Color32, Pos2};

use crate::core::items::LineStyle;
use crate::core::transform::Transform;

/// The geometry a [`Shape`] draws (silx `addShape` `shape`).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ShapeKind {
    /// A closed polygon through the points `(x[i], y[i])`. silx `'polygon'`.
    Polygon,
    /// An axis-aligned rectangle. Built from two corner points stored as
    /// `x = [x0, x1]`, `y = [y0, y1]`. silx `'rectangle'`.
    Rectangle,
    /// An open polyline through the points `(x[i], y[i])`. silx `'polylines'`.
    Polyline,
    /// One full-width horizontal line per entry in `y` (`x` unused). silx `'hline'`.
    HLine,
    /// One full-height vertical line per entry in `x` (`y` unused). silx `'vline'`.
    VLine,
}

/// A shape drawn over the data area (silx `BackendBase.addShape`).
///
/// `fill` is honored for [`ShapeKind::Polygon`] / [`ShapeKind::Rectangle`].
/// Concave polygons fill correctly: the widget chrome fills the polygon's
/// ear-clipping triangulation ([`triangulate_simple_polygon`]) as a mesh rather
/// than using egui's convex-only `Shape::convex_polygon`, matching silx's
/// matplotlib / pygfx backends. The outline (and all line kinds) honor
/// `line_style` / `line_width`, with `gap_color` filling dash gaps (silx
/// `gapcolor`).
#[derive(Clone, Debug, PartialEq)]
pub struct Shape {
    /// What geometry this shape draws.
    pub kind: ShapeKind,
    /// Data X coordinates of the shape's points (see [`ShapeKind`] for which
    /// array each kind reads).
    pub x: Vec<f64>,
    /// Data Y coordinates of the shape's points.
    pub y: Vec<f64>,
    /// Outline and fill color (silx `color`).
    pub color: Color32,
    /// Fill the interior (silx `fill`); honored for `Polygon` / `Rectangle`.
    pub fill: bool,
    /// Outline stroke style (silx `linestyle`).
    pub line_style: LineStyle,
    /// Outline width in logical points (silx `linewidth`).
    pub line_width: f32,
    /// Second color filling dash gaps in the outline (silx `gapcolor`); `None`
    /// leaves the gaps empty.
    pub gap_color: Option<Color32>,
    /// Whether the shape draws in the overlay layer (silx
    /// `_OverlayItem.isOverlay` / `setOverlay`, `shape.py:54-73`).
    ///
    /// Honored by the renderer's two-pass split: `true` draws in the overlay
    /// layer on top of the chrome (like an ROI), `false` in the base data layer
    /// under the overlay items (ROIs, markers, crosshair). Defaults to `true`
    /// (the port's historical behavior); silx's `_OverlayItem` defaults to
    /// `False`, but rsplot's shape constructors are used as on-top annotations,
    /// so the default is kept and callers opt into the data layer via
    /// [`Shape::with_overlay`].
    pub is_overlay: bool,
}

impl Shape {
    /// A closed polygon through `(x[i], y[i])`. Panics if `x` and `y` differ in
    /// length.
    pub fn polygon(x: Vec<f64>, y: Vec<f64>) -> Self {
        assert_eq!(
            x.len(),
            y.len(),
            "polygon x and y must have the same length"
        );
        Self::with_points(ShapeKind::Polygon, x, y)
    }

    /// An axis-aligned rectangle between corners `(x0, y0)` and `(x1, y1)`.
    pub fn rectangle(x0: f64, y0: f64, x1: f64, y1: f64) -> Self {
        Self::with_points(ShapeKind::Rectangle, vec![x0, x1], vec![y0, y1])
    }

    /// An open polyline through `(x[i], y[i])`. Panics if `x` and `y` differ in
    /// length.
    pub fn polyline(x: Vec<f64>, y: Vec<f64>) -> Self {
        assert_eq!(
            x.len(),
            y.len(),
            "polyline x and y must have the same length"
        );
        Self::with_points(ShapeKind::Polyline, x, y)
    }

    /// One full-width horizontal line at each y value (silx `'hline'`).
    pub fn hlines(y: Vec<f64>) -> Self {
        Self::with_points(ShapeKind::HLine, Vec::new(), y)
    }

    /// One full-height vertical line at each x value (silx `'vline'`).
    pub fn vlines(x: Vec<f64>) -> Self {
        Self::with_points(ShapeKind::VLine, x, Vec::new())
    }

    fn with_points(kind: ShapeKind, x: Vec<f64>, y: Vec<f64>) -> Self {
        Self {
            kind,
            x,
            y,
            color: Color32::WHITE,
            fill: false,
            line_style: LineStyle::Solid,
            line_width: 1.0,
            gap_color: None,
            is_overlay: true,
        }
    }

    /// Set the outline / fill color.
    pub fn with_color(mut self, color: Color32) -> Self {
        self.color = color;
        self
    }

    /// Fill the interior (`Polygon` / `Rectangle` only).
    pub fn with_fill(mut self, fill: bool) -> Self {
        self.fill = fill;
        self
    }

    /// Set the outline stroke style.
    pub fn with_line_style(mut self, style: LineStyle) -> Self {
        self.line_style = style;
        self
    }

    /// Set the outline width.
    pub fn with_line_width(mut self, width: f32) -> Self {
        self.line_width = width;
        self
    }

    /// Set the dash-gap fill color (silx `gapcolor`).
    pub fn with_gap_color(mut self, color: Color32) -> Self {
        self.gap_color = Some(color);
        self
    }

    /// Set whether the shape draws in the overlay layer (`true`) or the base
    /// data layer (`false`) — silx `_OverlayItem.setOverlay`.
    pub fn with_overlay(mut self, overlay: bool) -> Self {
        self.is_overlay = overlay;
        self
    }

    /// Screen-space vertices for the area-shaped kinds: the four corners of a
    /// [`ShapeKind::Rectangle`], or each `(x[i], y[i])` of a
    /// [`ShapeKind::Polygon`] / [`ShapeKind::Polyline`]. Empty for the line kinds,
    /// whose lines span the data area and are placed at draw time.
    pub fn screen_points(&self, t: &Transform) -> Vec<Pos2> {
        match self.kind {
            ShapeKind::Rectangle => {
                if self.x.len() < 2 || self.y.len() < 2 {
                    return Vec::new();
                }
                let (x0, x1, y0, y1) = (self.x[0], self.x[1], self.y[0], self.y[1]);
                vec![
                    t.data_to_pixel(x0, y0),
                    t.data_to_pixel(x1, y0),
                    t.data_to_pixel(x1, y1),
                    t.data_to_pixel(x0, y1),
                ]
            }
            ShapeKind::Polygon | ShapeKind::Polyline => self
                .x
                .iter()
                .zip(&self.y)
                .map(|(&x, &y)| t.data_to_pixel(x, y))
                .collect(),
            ShapeKind::HLine | ShapeKind::VLine => Vec::new(),
        }
    }
}

/// An infinite line `y = slope * x + intercept`, or a vertical line
/// `x = intercept` when `slope` is non-finite (silx `Line`, `shape.py:289-393`).
///
/// silx warns: "If slope is not finite, then the line is x = intercept." The
/// line is drawn as the segment of itself that crosses the visible data bounds;
/// [`Line::clipped_segment`] computes that segment (its rendering wiring — the
/// data-to-screen transform and the chrome draw — is deferred to the
/// interaction/chrome layer).
#[derive(Clone, Debug, PartialEq)]
pub struct Line {
    /// Line slope (silx `slope`). Non-finite (`inf` / `NaN`) means a vertical
    /// line `x = intercept`.
    pub slope: f64,
    /// Line intercept (silx `intercept`). For a sloped line it is the y-intercept;
    /// for a vertical line it is the x position. silx asserts this is finite.
    pub intercept: f64,
    /// Line color (silx `color`).
    pub color: Color32,
    /// Outline stroke style (silx `linestyle`).
    pub line_style: LineStyle,
    /// Outline width in logical points (silx `linewidth`).
    pub line_width: f32,
    /// Second color filling dash gaps (silx `gapcolor`); `None` leaves them empty.
    pub gap_color: Option<Color32>,
    /// Whether the line draws in the overlay pass (silx `Line` is an
    /// `_OverlayItem`; matches [`Shape::is_overlay`], defaulting to `true`).
    pub is_overlay: bool,
}

impl Line {
    /// An infinite line `y = slope * x + intercept`. A non-finite `slope` makes a
    /// vertical line `x = intercept`. Panics if `intercept` is not finite, matching
    /// silx's `assert numpy.isfinite(intercept)`.
    pub fn new(slope: f64, intercept: f64) -> Self {
        assert!(intercept.is_finite(), "Line intercept must be finite");
        Self {
            slope,
            intercept,
            color: Color32::WHITE,
            line_style: LineStyle::Solid,
            line_width: 1.0,
            gap_color: None,
            is_overlay: true,
        }
    }

    /// A line through two `(x, y)` points (silx `setSlopeInterceptFromPoints`,
    /// `shape.py:370-383`). Equal x values make a vertical line `x = x0`.
    pub fn from_points(point0: (f64, f64), point1: (f64, f64)) -> Self {
        let (x0, y0) = point0;
        let (x1, y1) = point1;
        if x0 == x1 {
            // Vertical line: slope inf, intercept = x0 (silx special case).
            return Self::new(f64::INFINITY, x0);
        }
        let slope = (y1 - y0) / (x1 - x0);
        Self::new(slope, y0 - x0 * slope)
    }

    /// Set the line color.
    pub fn with_color(mut self, color: Color32) -> Self {
        self.color = color;
        self
    }

    /// Set the outline stroke style.
    pub fn with_line_style(mut self, style: LineStyle) -> Self {
        self.line_style = style;
        self
    }

    /// Set the outline width.
    pub fn with_line_width(mut self, width: f32) -> Self {
        self.line_width = width;
        self
    }

    /// Set the dash-gap fill color (silx `gapcolor`).
    pub fn with_gap_color(mut self, color: Color32) -> Self {
        self.gap_color = Some(color);
        self
    }

    /// Set whether the line draws in the overlay pass (silx `setOverlay`).
    pub fn with_overlay(mut self, overlay: bool) -> Self {
        self.is_overlay = overlay;
        self
    }

    /// The visible segment of the line within the data `bounds`, or `None` when
    /// the line does not cross the bounds (silx `Line.__updatePoints`,
    /// `shape.py:305-340`).
    ///
    /// `bounds` is the data-space window expressed as a [`Rect`](egui::Rect): `bounds.min` is
    /// `(xmin, ymin)`, `bounds.max` is `(xmax, ymax)`. The returned endpoints are
    /// in **data coordinates** (`Pos2` = `(x, y)`); the data-to-screen transform
    /// and drawing are the renderer's job (deferred).
    ///
    /// The clipping reproduces silx exactly:
    ///
    /// - Vertical line (non-finite slope): visible iff `xmin <= intercept <= xmax`;
    ///   the segment is `((intercept, ymin), (intercept, ymax))`.
    /// - Sloped line: the y at `xmin` and `xmax` are ` y0 = slope*xmin + intercept`
    ///   and `y1 = slope*xmax + intercept`; the line is visible iff
    ///   `min(y0, y1) < ymax AND max(y0, y1) > ymin`, and the segment is
    ///   `((xmin, y0), (xmax, y1))`.
    pub fn clipped_segment(&self, bounds: egui::Rect) -> Option<(Pos2, Pos2)> {
        let xmin = bounds.min.x as f64;
        let ymin = bounds.min.y as f64;
        let xmax = bounds.max.x as f64;
        let ymax = bounds.max.y as f64;

        if !self.slope.is_finite() {
            // Vertical line x = intercept.
            if self.intercept < xmin || self.intercept > xmax {
                return None;
            }
            let x = self.intercept as f32;
            return Some((Pos2::new(x, ymin as f32), Pos2::new(x, ymax as f32)));
        }

        let y0 = self.slope * xmin + self.intercept;
        let y1 = self.slope * xmax + self.intercept;
        let (lo, hi) = if y0 <= y1 { (y0, y1) } else { (y1, y0) };
        if lo < ymax && hi > ymin {
            Some((
                Pos2::new(xmin as f32, y0 as f32),
                Pos2::new(xmax as f32, y1 as f32),
            ))
        } else {
            None
        }
    }
}

/// Triangulate a simple polygon (possibly concave; no holes, no
/// self-intersection) by ear clipping, returning index triples into `points`.
///
/// egui's [`egui::Shape::convex_polygon`] fills the *convex interpretation* of
/// its vertices, so a concave polygon's fill spills across the concavity. silx's
/// matplotlib / pygfx backends fill concave polygons correctly; the widget
/// chrome matches that by filling the returned triangles as an [`egui::Mesh`]
/// instead of one `convex_polygon`. The outline is unaffected (it is stroked
/// from the point list directly).
///
/// Returns `N - 2` triangles for an `N`-vertex simple polygon (`N >= 3`), an
/// empty vector for fewer than 3 points, and — defensively — a fan over the
/// leftover ring if the input is degenerate or self-intersecting (never panics,
/// never loops forever). Orientation is normalized internally, so clockwise and
/// counter-clockwise inputs triangulate identically.
pub fn triangulate_simple_polygon(points: &[Pos2]) -> Vec<[u32; 3]> {
    let n = points.len();
    if n < 3 {
        return Vec::new();
    }

    // Work on a ring of indices, normalized to counter-clockwise so a convex
    // corner is a left turn (positive orientation).
    let mut ring: Vec<usize> = (0..n).collect();
    if signed_area_2(points) < 0.0 {
        ring.reverse();
    }

    let mut tris = Vec::with_capacity(n - 2);
    // Each successful clip removes one vertex; cap total attempts so a
    // non-simple polygon cannot loop forever.
    let mut attempts = 0;
    let max_attempts = n * n;
    while ring.len() > 3 {
        let m = ring.len();
        let mut clipped = false;
        for i in 0..m {
            let a = ring[(i + m - 1) % m];
            let b = ring[i];
            let c = ring[(i + 1) % m];
            if is_ear(points, &ring, a, b, c) {
                tris.push([a as u32, b as u32, c as u32]);
                ring.remove(i);
                clipped = true;
                break;
            }
        }
        attempts += 1;
        if !clipped || attempts > max_attempts {
            // Degenerate / self-intersecting: fan the leftover ring so something
            // is drawn rather than nothing.
            for k in 1..ring.len() - 1 {
                tris.push([ring[0] as u32, ring[k] as u32, ring[k + 1] as u32]);
            }
            return tris;
        }
    }
    tris.push([ring[0] as u32, ring[1] as u32, ring[2] as u32]);
    tris
}

/// Twice the signed area of the polygon (shoelace); positive for CCW.
fn signed_area_2(points: &[Pos2]) -> f32 {
    let n = points.len();
    let mut sum = 0.0;
    for i in 0..n {
        let p = points[i];
        let q = points[(i + 1) % n];
        sum += p.x * q.y - q.x * p.y;
    }
    sum
}

/// Twice the signed area of triangle `(a, b, c)`; > 0 when `a → b → c` turns
/// left (CCW). Doubles as the left-of-directed-edge test for `a → b` against `c`
/// (the two are algebraically equal).
fn orient(a: Pos2, b: Pos2, c: Pos2) -> f32 {
    (b.x - a.x) * (c.y - a.y) - (b.y - a.y) * (c.x - a.x)
}

/// Whether triangle `(a, b, c)` is an ear of the CCW `ring`: a convex corner at
/// `b` whose triangle contains no other ring vertex.
fn is_ear(points: &[Pos2], ring: &[usize], a: usize, b: usize, c: usize) -> bool {
    let (pa, pb, pc) = (points[a], points[b], points[c]);
    // Convex (left turn) at b for a CCW polygon; reflex/collinear is no ear.
    if orient(pa, pb, pc) <= 0.0 {
        return false;
    }
    for &v in ring {
        if v == a || v == b || v == c {
            continue;
        }
        if point_in_triangle(points[v], pa, pb, pc) {
            return false;
        }
    }
    true
}

/// Whether `p` lies inside or on the boundary of CCW triangle `(a, b, c)`.
fn point_in_triangle(p: Pos2, a: Pos2, b: Pos2, c: Pos2) -> bool {
    let s1 = orient(a, b, p);
    let s2 = orient(b, c, p);
    let s3 = orient(c, a, p);
    let has_neg = s1 < 0.0 || s2 < 0.0 || s3 < 0.0;
    let has_pos = s1 > 0.0 || s2 > 0.0 || s3 > 0.0;
    !(has_neg && has_pos)
}

#[cfg(test)]
mod tests {
    use super::*;
    use egui::{Rect, pos2};

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
    fn constructors_set_kind_and_defaults() {
        let p = Shape::polygon(vec![0.0, 1.0, 2.0], vec![0.0, 1.0, 0.0]);
        assert_eq!(p.kind, ShapeKind::Polygon);
        assert_eq!(p.color, Color32::WHITE);
        assert!(!p.fill);
        assert_eq!(p.line_style, LineStyle::Solid);
        assert_eq!(p.line_width, 1.0);
        assert!(p.gap_color.is_none());

        assert_eq!(
            Shape::rectangle(0.0, 0.0, 1.0, 1.0).kind,
            ShapeKind::Rectangle
        );
        assert_eq!(
            Shape::polyline(vec![0.0], vec![0.0]).kind,
            ShapeKind::Polyline
        );
        assert_eq!(Shape::hlines(vec![1.0, 2.0]).kind, ShapeKind::HLine);
        assert_eq!(Shape::vlines(vec![1.0, 2.0]).kind, ShapeKind::VLine);
    }

    #[test]
    #[should_panic(expected = "polygon x and y must have the same length")]
    fn polygon_rejects_length_mismatch() {
        Shape::polygon(vec![0.0, 1.0], vec![0.0]);
    }

    #[test]
    #[should_panic(expected = "polyline x and y must have the same length")]
    fn polyline_rejects_length_mismatch() {
        Shape::polyline(vec![0.0], vec![0.0, 1.0]);
    }

    #[test]
    fn builders_set_fields() {
        let s = Shape::rectangle(0.0, 0.0, 1.0, 1.0)
            .with_color(Color32::RED)
            .with_fill(true)
            .with_line_style(LineStyle::Dashed)
            .with_line_width(2.0)
            .with_gap_color(Color32::BLACK);
        assert_eq!(s.color, Color32::RED);
        assert!(s.fill);
        assert_eq!(s.line_style, LineStyle::Dashed);
        assert_eq!(s.line_width, 2.0);
        assert_eq!(s.gap_color, Some(Color32::BLACK));
    }

    #[test]
    fn overlay_defaults_true_and_builder_toggles() {
        // Default is the port's current behavior: shapes draw in the overlay pass.
        assert!(Shape::rectangle(0.0, 0.0, 1.0, 1.0).is_overlay);
        // The builder can opt out (silx setOverlay(False)).
        let s = Shape::rectangle(0.0, 0.0, 1.0, 1.0).with_overlay(false);
        assert!(!s.is_overlay);
    }

    #[test]
    fn rectangle_screen_points_are_the_four_corners() {
        // Rectangle data (2,3)-(8,7): x 2->20,8->80; y 3->70,7->30 (y flipped).
        let r = Shape::rectangle(2.0, 3.0, 8.0, 7.0);
        let pts = r.screen_points(&t());
        assert_eq!(
            pts,
            vec![
                pos2(20.0, 70.0), // (x0, y0)
                pos2(80.0, 70.0), // (x1, y0)
                pos2(80.0, 30.0), // (x1, y1)
                pos2(20.0, 30.0), // (x0, y1)
            ]
        );
    }

    #[test]
    fn polygon_screen_points_map_each_vertex_and_lines_are_empty() {
        let p = Shape::polygon(vec![1.0, 5.0], vec![2.0, 6.0]);
        assert_eq!(
            p.screen_points(&t()),
            vec![pos2(10.0, 80.0), pos2(50.0, 40.0)]
        );
        // Line kinds carry no fixed-extent vertices.
        assert!(Shape::hlines(vec![1.0]).screen_points(&t()).is_empty());
        assert!(Shape::vlines(vec![1.0]).screen_points(&t()).is_empty());
    }

    // Data bounds [0,10] x [0,10] expressed as a Rect (min=(xmin,ymin),
    // max=(xmax,ymax)) for the Line clipping tests.
    fn bounds() -> Rect {
        Rect::from_min_max(pos2(0.0, 0.0), pos2(10.0, 10.0))
    }

    #[test]
    fn line_defaults_and_overlay_builder() {
        let l = Line::new(2.0, 1.0);
        assert_eq!(l.slope, 2.0);
        assert_eq!(l.intercept, 1.0);
        assert_eq!(l.color, Color32::WHITE);
        assert_eq!(l.line_style, LineStyle::Solid);
        assert!(l.is_overlay);
        assert!(!Line::new(0.0, 0.0).with_overlay(false).is_overlay);
    }

    #[test]
    #[should_panic(expected = "Line intercept must be finite")]
    fn line_rejects_non_finite_intercept() {
        Line::new(1.0, f64::NAN);
    }

    #[test]
    fn sloped_line_clips_to_box_entry_and_exit() {
        // y = x: crosses the box [0,10]^2 from (0,0) to (10,10).
        let seg = Line::new(1.0, 0.0).clipped_segment(bounds());
        assert_eq!(seg, Some((pos2(0.0, 0.0), pos2(10.0, 10.0))));

        // y = 0.5*x + 2: y0 = 2 at x=0, y1 = 7 at x=10; both inside.
        let seg = Line::new(0.5, 2.0).clipped_segment(bounds()).unwrap();
        assert_eq!(seg.0, pos2(0.0, 2.0));
        assert_eq!(seg.1, pos2(10.0, 7.0));
    }

    #[test]
    fn horizontal_line_inside_box_is_visible() {
        // slope 0, intercept 5: a horizontal line y=5 that crosses the box.
        // y0 = y1 = 5; min(5,5)=5 < ymax 10 and max=5 > ymin 0 -> visible.
        let seg = Line::new(0.0, 5.0).clipped_segment(bounds());
        assert_eq!(seg, Some((pos2(0.0, 5.0), pos2(10.0, 5.0))));
    }

    #[test]
    fn sloped_line_entirely_above_box_is_none() {
        // y = x + 20: y0 = 20, y1 = 30, both above ymax 10. min(20,30)=20 is
        // not < ymax 10 -> None.
        assert_eq!(Line::new(1.0, 20.0).clipped_segment(bounds()), None);
    }

    #[test]
    fn sloped_line_entirely_below_box_is_none() {
        // y = x - 20: y0 = -20, y1 = -10, both below ymin 0. max(-20,-10)=-10
        // is not > ymin 0 -> None.
        assert_eq!(Line::new(1.0, -20.0).clipped_segment(bounds()), None);
    }

    #[test]
    fn vertical_line_inside_outside_and_on_edge() {
        // x = 5: inside [0,10] -> segment spans the full y range.
        let seg = Line::new(f64::INFINITY, 5.0).clipped_segment(bounds());
        assert_eq!(seg, Some((pos2(5.0, 0.0), pos2(5.0, 10.0))));

        // On the min edge x = 0 (xmin <= 0 <= xmax) -> visible (inclusive).
        let seg = Line::new(f64::INFINITY, 0.0).clipped_segment(bounds());
        assert_eq!(seg, Some((pos2(0.0, 0.0), pos2(0.0, 10.0))));
        // On the max edge x = 10 -> visible (inclusive).
        let seg = Line::new(f64::INFINITY, 10.0).clipped_segment(bounds());
        assert_eq!(seg, Some((pos2(10.0, 0.0), pos2(10.0, 10.0))));

        // x = 15: outside -> None.
        assert_eq!(
            Line::new(f64::INFINITY, 15.0).clipped_segment(bounds()),
            None
        );
        // x = -1: outside on the low side -> None.
        assert_eq!(
            Line::new(f64::INFINITY, -1.0).clipped_segment(bounds()),
            None
        );
    }

    #[test]
    fn from_points_sloped_and_vertical() {
        // Two points on y = x.
        let l = Line::from_points((0.0, 0.0), (2.0, 2.0));
        assert_eq!(l.slope, 1.0);
        assert_eq!(l.intercept, 0.0);

        // y = 2x + 1 through (1, 3) and (2, 5).
        let l = Line::from_points((1.0, 3.0), (2.0, 5.0));
        assert_eq!(l.slope, 2.0);
        assert_eq!(l.intercept, 1.0);

        // Equal x -> vertical line x = x0 (silx special case).
        let l = Line::from_points((4.0, 1.0), (4.0, 9.0));
        assert!(!l.slope.is_finite());
        assert_eq!(l.intercept, 4.0);
    }

    /// Total area covered by a triangulation (each triangle is `½·|orient|`).
    fn triangulation_area(points: &[Pos2], tris: &[[u32; 3]]) -> f32 {
        tris.iter()
            .map(|t| {
                0.5 * orient(
                    points[t[0] as usize],
                    points[t[1] as usize],
                    points[t[2] as usize],
                )
                .abs()
            })
            .sum()
    }

    fn polygon_area(points: &[Pos2]) -> f32 {
        0.5 * signed_area_2(points).abs()
    }

    #[test]
    fn triangulate_too_few_points_is_empty() {
        assert!(triangulate_simple_polygon(&[]).is_empty());
        assert!(triangulate_simple_polygon(&[pos2(0.0, 0.0)]).is_empty());
        assert!(triangulate_simple_polygon(&[pos2(0.0, 0.0), pos2(1.0, 1.0)]).is_empty());
    }

    #[test]
    fn triangulate_triangle_is_itself() {
        let p = [pos2(0.0, 0.0), pos2(2.0, 0.0), pos2(1.0, 2.0)];
        let tris = triangulate_simple_polygon(&p);
        assert_eq!(tris.len(), 1);
        assert!((triangulation_area(&p, &tris) - polygon_area(&p)).abs() < 1e-4);
    }

    #[test]
    fn triangulate_convex_quad_tiles_it() {
        let p = [
            pos2(0.0, 0.0),
            pos2(4.0, 0.0),
            pos2(4.0, 3.0),
            pos2(0.0, 3.0),
        ];
        let tris = triangulate_simple_polygon(&p);
        assert_eq!(tris.len(), 2); // N - 2
        assert!((triangulation_area(&p, &tris) - 12.0).abs() < 1e-4);
    }

    #[test]
    fn triangulate_concave_polygon_tiles_exactly_not_its_hull() {
        // Arrowhead: D is a reflex vertex strictly inside hull triangle A,B,C.
        // Polygon area is 3.0; the convex hull area is 4.0. A correct concave
        // triangulation must sum to 3.0 (egui's convex_polygon would cover 4.0).
        let p = [
            pos2(0.0, 0.0), // A
            pos2(4.0, 1.0), // B
            pos2(0.0, 2.0), // C
            pos2(1.0, 1.0), // D (reflex)
        ];
        let tris = triangulate_simple_polygon(&p);
        assert_eq!(tris.len(), 2); // N - 2
        let area = triangulation_area(&p, &tris);
        assert!(
            (area - 3.0).abs() < 1e-4,
            "expected polygon area 3.0, got {area}"
        );
        assert!(area < 4.0 - 1e-3, "must not cover the convex hull (4.0)");
    }

    #[test]
    fn triangulate_normalizes_clockwise_input() {
        // Same arrowhead wound clockwise: orientation is normalized internally.
        let p = [
            pos2(1.0, 1.0), // D (reflex)
            pos2(0.0, 2.0), // C
            pos2(4.0, 1.0), // B
            pos2(0.0, 0.0), // A
        ];
        let tris = triangulate_simple_polygon(&p);
        assert_eq!(tris.len(), 2);
        assert!((triangulation_area(&p, &tris) - 3.0).abs() < 1e-4);
    }
}
