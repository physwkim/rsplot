//! Markers: point, vertical-line, and horizontal-line annotations drawn over the
//! data area (silx `BackendBase.addMarker`).
//!
//! A marker is a lightweight overlay like an [`crate::core::roi::Roi`]: its
//! geometry is data-space and the screen-placement math is pure (no egui input),
//! so it is unit-testable; the widget's chrome draws the list each frame via
//! [`crate::widget::chrome::draw_markers`]. silx selects the marker kind by which
//! coordinate is `None` — `x` `None` ⇒ horizontal line, `y` `None` ⇒ vertical
//! line, both set ⇒ a point marker drawn with a symbol (`doc/design.md` §8).

use egui::{Color32, Pos2};

use crate::core::items::LineStyle;
use crate::core::transform::{Transform, YAxis};

/// Default point-marker symbol size (full extent) in logical points.
pub const DEFAULT_MARKER_SIZE: f32 = 8.0;

/// Pixel slop added to a marker's geometry when hit-testing the cursor against
/// it (silx's draggable-marker pick tolerance). A point's pick radius is
/// `size / 2 + this`; a line's pick half-width is `this + line_width / 2`.
pub const MARKER_PICK_TOLERANCE_PX: f32 = 5.0;

/// Symbol drawn at a point marker (silx `addMarker` `symbol`). The catalog
/// matches silx's marker symbols, which differ from the GPU curve's scatter
/// symbols ([`crate::Symbol`]) — markers add the diamond, point, and pixel
/// glyphs and are drawn with egui's painter rather than an SDF shader.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum MarkerSymbol {
    /// Filled circle. silx `'o'`.
    Circle,
    /// Small filled dot. silx `'.'`.
    Point,
    /// A single pixel. silx `','`.
    Pixel,
    /// Upright "+". silx `'+'`.
    Plus,
    /// Diagonal "×". silx `'x'`.
    Cross,
    /// Filled diamond. silx `'d'`.
    Diamond,
    /// Filled square. silx `'s'`.
    Square,
}

/// What a marker is and where it sits. The illegal combinations (a line with a
/// symbol, a point without one) are unrepresentable: only [`MarkerKind::Point`]
/// carries a symbol/size, only the lines span an axis.
#[derive(Clone, Copy, Debug, PartialEq)]
pub enum MarkerKind {
    /// A point marker at data `(x, y)`, drawn with `symbol` at `size` (logical
    /// points). silx: both `x` and `y` set.
    Point {
        x: f64,
        y: f64,
        symbol: MarkerSymbol,
        size: f32,
    },
    /// A vertical line at data `x`, spanning the data-area height. silx: `y` `None`.
    VLine { x: f64 },
    /// A horizontal line at data `y`, spanning the data-area width. silx: `x` `None`.
    HLine { y: f64 },
}

/// The dragging constraint of a marker (silx `MarkerBase._setConstraint`,
/// `marker.py:208-235` plus the `Marker` presets `marker.py:273-292`).
///
/// A constraint is a drag-time *filter*: given the cursor position the user
/// dragged to, it returns the position the marker is actually allowed to move to.
/// silx supports an arbitrary callable plus the `'horizontal'` / `'vertical'`
/// string presets. The presets pin one coordinate to the marker's current value;
/// they are stored here as enum variants so [`Marker`] stays `Clone` / `Debug` /
/// `PartialEq`. An arbitrary closure is not stored on the marker (it would break
/// those derives and is interaction-layer state, not drawn geometry); apply a
/// custom filter with the pure [`apply_constraint`] free function instead.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Default)]
pub enum MarkerConstraint {
    /// No filtering: the marker moves freely to the cursor position (silx
    /// `_defaultConstraint`, which returns its arguments unchanged).
    #[default]
    None,
    /// Pin the X coordinate to the marker's current X, leaving Y free (silx
    /// `Marker._horizontalConstraint`: `return self.getXPosition(), y`).
    Horizontal,
    /// Pin the Y coordinate to the marker's current Y, leaving X free (silx
    /// `Marker._verticalConstraint`: `return x, self.getYPosition()`).
    Vertical,
}

/// Apply a drag constraint, returning the position the marker is allowed to move
/// to (silx `MarkerBase.setPosition` calling `getConstraint()(x, y)`,
/// `marker.py:200-206`).
///
/// `from` is the marker's current data position (the anchor a preset pins to);
/// `to` is the data position the cursor dragged to. The result is the filtered
/// position:
///
/// - [`MarkerConstraint::None`] returns `to` unchanged.
/// - [`MarkerConstraint::Horizontal`] keeps `from.0` (the current X) and `to.1`.
/// - [`MarkerConstraint::Vertical`] keeps `to.0` and `from.1` (the current Y).
///
/// For a silx arbitrary callable, call the closure directly on `to` rather than
/// going through a [`MarkerConstraint`].
pub fn apply_constraint(
    constraint: MarkerConstraint,
    from: (f64, f64),
    to: (f64, f64),
) -> (f64, f64) {
    match constraint {
        MarkerConstraint::None => to,
        MarkerConstraint::Horizontal => (from.0, to.1),
        MarkerConstraint::Vertical => (to.0, from.1),
    }
}

/// Where a marker's label text attaches to the marker point (silx marker text
/// `horizontalalignment` / `verticalalignment`, e.g.
/// `BackendMatplotlib.addMarker` `marker.py` rendering and the pygfx
/// `_mapAnchor` `BackendPygfx.py:1998`).
///
/// The named point of the text rectangle is placed at the marker point (plus the
/// pixel offset the backend applies). For example [`TextAnchor::TopLeft`] puts
/// the rect's top-left corner at the marker (silx point-marker default:
/// `horizontalalignment="left"` with the text growing down-right), and
/// [`TextAnchor::TopRight`] puts the rect's top-right corner there (silx hline
/// default: `horizontalalignment="right", verticalalignment="top"`).
#[derive(Clone, Copy, Debug, PartialEq, Eq, Default)]
pub enum TextAnchor {
    /// Marker at the text rect's top-left corner; text extends right and down.
    /// silx point-marker default (`ha="left"`).
    #[default]
    TopLeft,
    /// Marker at the center of the rect's top edge.
    Top,
    /// Marker at the text rect's top-right corner. silx hline default
    /// (`ha="right", va="top"`).
    TopRight,
    /// Marker at the center of the rect's left edge.
    Left,
    /// Marker at the rect's center.
    Center,
    /// Marker at the center of the rect's right edge.
    Right,
    /// Marker at the text rect's bottom-left corner.
    BottomLeft,
    /// Marker at the center of the rect's bottom edge.
    Bottom,
    /// Marker at the text rect's bottom-right corner.
    BottomRight,
}

impl TextAnchor {
    /// The top-left offset of the text rectangle relative to the marker point so
    /// that the anchor named by this variant lands on the marker (a pure layout
    /// computation, no egui input).
    ///
    /// Given the rendered text `size` (`(width, height)`), the returned `(dx, dy)`
    /// is added to the marker's screen position to get the rect's top-left corner.
    /// Y grows downward (egui screen convention), so e.g. [`TextAnchor::TopLeft`]
    /// returns `(0, 0)` (rect starts at the marker) and [`TextAnchor::BottomRight`]
    /// returns `(-w, -h)` (rect ends at the marker).
    ///
    /// This is the alignment offset only; the backend's fixed pixel padding (silx
    /// `pixel_offset`, e.g. `(10, 3)` for a symbol point) is applied separately by
    /// the renderer.
    pub fn rect_offset(self, size: (f32, f32)) -> (f32, f32) {
        let (w, h) = size;
        // Horizontal: Left edge -> 0, Center -> -w/2, Right edge -> -w.
        let dx = match self {
            TextAnchor::TopLeft | TextAnchor::Left | TextAnchor::BottomLeft => 0.0,
            TextAnchor::Top | TextAnchor::Center | TextAnchor::Bottom => -w / 2.0,
            TextAnchor::TopRight | TextAnchor::Right | TextAnchor::BottomRight => -w,
        };
        // Vertical: Top edge -> 0, Center -> -h/2, Bottom edge -> -h.
        let dy = match self {
            TextAnchor::TopLeft | TextAnchor::Top | TextAnchor::TopRight => 0.0,
            TextAnchor::Left | TextAnchor::Center | TextAnchor::Right => -h / 2.0,
            TextAnchor::BottomLeft | TextAnchor::Bottom | TextAnchor::BottomRight => -h,
        };
        (dx, dy)
    }
}

/// A point / vertical-line / horizontal-line marker drawn over the data area
/// (silx `BackendBase.addMarker`).
///
/// `line_style`/`line_width` apply to the line kinds only (silx: "Only relevant
/// for line markers where X or Y is None"); a point ignores them. The silx
/// `constraint` (drag filter) and `font` (a `QFont`) parameters are interaction /
/// Qt-specific and not part of the marker's drawn geometry, so they are not
/// modeled here.
#[derive(Clone, Debug, PartialEq)]
pub struct Marker {
    /// What the marker is and where it sits.
    pub kind: MarkerKind,
    /// Line / symbol color (silx `color`).
    pub color: Color32,
    /// Optional label text drawn beside the marker (silx `text`).
    pub text: Option<String>,
    /// Background fill behind the label text (silx `bgcolor`); `None` draws no box.
    pub bgcolor: Option<Color32>,
    /// Stroke style for the line kinds (silx `linestyle`); ignored for points.
    pub line_style: LineStyle,
    /// Stroke width in logical points for the line kinds (silx `linewidth`);
    /// ignored for points.
    pub line_width: f32,
    /// Which Y axis the marker's data Y is measured against (silx `yaxis`).
    pub y_axis: YAxis,
    /// Whether the user may drag the marker (silx `DraggableMixIn.isDraggable`,
    /// default `false`). The interaction layer reads this to decide whether to
    /// move the marker on drag; the constraint below only applies while dragging.
    pub is_draggable: bool,
    /// Drag-time position filter (silx `MarkerBase._setConstraint` and the
    /// `Marker` `'horizontal'` / `'vertical'` presets). Defaults to
    /// [`MarkerConstraint::None`].
    pub constraint: MarkerConstraint,
    /// Where the label text attaches to the marker point (silx marker text
    /// `horizontalalignment` / `verticalalignment`). Defaults to
    /// [`TextAnchor::TopLeft`], the silx point-marker default.
    pub text_anchor: TextAnchor,
}

impl Marker {
    /// A point marker at data `(x, y)`: a filled circle at the default size, white,
    /// no text, bound to the left axis.
    pub fn point(x: f64, y: f64) -> Self {
        Self::with_kind(MarkerKind::Point {
            x,
            y,
            symbol: MarkerSymbol::Circle,
            size: DEFAULT_MARKER_SIZE,
        })
    }

    /// A vertical-line marker at data `x` (silx `y` `None`).
    pub fn vline(x: f64) -> Self {
        Self::with_kind(MarkerKind::VLine { x })
    }

    /// A horizontal-line marker at data `y` (silx `x` `None`).
    pub fn hline(y: f64) -> Self {
        Self::with_kind(MarkerKind::HLine { y })
    }

    fn with_kind(kind: MarkerKind) -> Self {
        Self {
            kind,
            color: Color32::WHITE,
            text: None,
            bgcolor: None,
            line_style: LineStyle::Solid,
            line_width: 1.0,
            y_axis: YAxis::Left,
            is_draggable: false,
            constraint: MarkerConstraint::None,
            text_anchor: TextAnchor::TopLeft,
        }
    }

    /// Set the marker color.
    pub fn with_color(mut self, color: Color32) -> Self {
        self.color = color;
        self
    }

    /// Attach label text.
    pub fn with_text(mut self, text: impl Into<String>) -> Self {
        self.text = Some(text.into());
        self
    }

    /// Set a background color for the label text.
    pub fn with_bgcolor(mut self, color: Color32) -> Self {
        self.bgcolor = Some(color);
        self
    }

    /// Set the symbol of a point marker. No-op on the line kinds, which have no
    /// symbol.
    pub fn with_symbol(mut self, symbol: MarkerSymbol) -> Self {
        if let MarkerKind::Point { symbol: s, .. } = &mut self.kind {
            *s = symbol;
        }
        self
    }

    /// Set the symbol size (logical points) of a point marker. No-op on the line
    /// kinds.
    pub fn with_symbol_size(mut self, size: f32) -> Self {
        if let MarkerKind::Point { size: s, .. } = &mut self.kind {
            *s = size;
        }
        self
    }

    /// Set the line style of a line marker (silx `linestyle`).
    pub fn with_line_style(mut self, style: LineStyle) -> Self {
        self.line_style = style;
        self
    }

    /// Set the line width of a line marker (silx `linewidth`).
    pub fn with_line_width(mut self, width: f32) -> Self {
        self.line_width = width;
        self
    }

    /// Bind the marker's data Y to a Y axis (silx `yaxis`).
    pub fn with_y_axis(mut self, axis: YAxis) -> Self {
        self.y_axis = axis;
        self
    }

    /// Set whether the user may drag the marker (silx `DraggableMixIn`).
    pub fn with_draggable(mut self, draggable: bool) -> Self {
        self.is_draggable = draggable;
        self
    }

    /// Set the drag constraint (silx `_setConstraint` and the `'horizontal'` /
    /// `'vertical'` presets).
    pub fn with_constraint(mut self, constraint: MarkerConstraint) -> Self {
        self.constraint = constraint;
        self
    }

    /// Set where the label text attaches to the marker point.
    pub fn with_text_anchor(mut self, anchor: TextAnchor) -> Self {
        self.text_anchor = anchor;
        self
    }

    /// The marker's current data position `(x, y)` (silx `getPosition`). For a
    /// line marker the off-axis coordinate is the constraint anchor only and has
    /// no drawn meaning; it is reported as `0.0` (silx stores `None` there, which
    /// is not representable in this `(f64, f64)` model — the line markers carry a
    /// single coordinate).
    pub fn position(&self) -> (f64, f64) {
        match self.kind {
            MarkerKind::Point { x, y, .. } => (x, y),
            MarkerKind::VLine { x } => (x, 0.0),
            MarkerKind::HLine { y } => (0.0, y),
        }
    }

    /// Whether the cursor (screen pixels) hits this marker under `transform`,
    /// mirroring silx's per-kind marker pick test (`backends/BackendBase.py`
    /// `pickItems`). The geometry is projected to pixels and compared against the
    /// cursor with a [`MARKER_PICK_TOLERANCE_PX`] slop:
    ///
    /// - [`MarkerKind::Point`]: the cursor is within `size / 2 + tolerance` pixels
    ///   of the projected point.
    /// - [`MarkerKind::VLine`]: the cursor's X is within `tolerance + line_width / 2`
    ///   of the projected line X *and* its Y lies within the data-area Y span.
    /// - [`MarkerKind::HLine`]: the symmetric test on the projected line Y.
    ///
    /// Pure (no egui input beyond the cursor position and the transform), so it is
    /// unit-testable and shared by the backend's `pick_marker` and the
    /// interaction layer's drag hit-test.
    pub fn pick(&self, transform: &Transform, cursor: Pos2) -> bool {
        let tolerance = MARKER_PICK_TOLERANCE_PX + self.line_width.max(1.0) * 0.5;
        match self.kind {
            MarkerKind::Point { x, y, size, .. } => {
                let radius = size.max(1.0) * 0.5 + MARKER_PICK_TOLERANCE_PX;
                transform.data_to_pixel(x, y).distance(cursor) <= radius
            }
            MarkerKind::VLine { x } => {
                let px = transform.data_to_pixel(x, transform.y.min).x;
                (cursor.x - px).abs() <= tolerance
                    && cursor.y >= transform.area.top() - tolerance
                    && cursor.y <= transform.area.bottom() + tolerance
            }
            MarkerKind::HLine { y } => {
                let py = transform.data_to_pixel(transform.x.min, y).y;
                (cursor.y - py).abs() <= tolerance
                    && cursor.x >= transform.area.left() - tolerance
                    && cursor.x <= transform.area.right() + tolerance
            }
        }
    }

    /// Drag the marker to data position `to`, applying its [`constraint`] anchored
    /// at `from` (silx `DraggableMixIn.drag` → `setPosition`, `marker.py:113-114`
    /// and the per-kind `setPosition` overrides `marker.py:177-206`, `296-352`).
    ///
    /// `from` is the marker's position before the drag (the anchor a preset pins
    /// to — silx reads it from `getXPosition()` / `getYPosition()`); pass
    /// [`position`](Marker::position). The constraint is applied to `to`, then the
    /// coordinate(s) relevant to the marker kind are updated:
    ///
    /// - [`MarkerKind::Point`] takes both filtered coordinates.
    /// - [`MarkerKind::VLine`] takes only the filtered X (silx `XMarker.setPosition`).
    /// - [`MarkerKind::HLine`] takes only the filtered Y (silx `YMarker.setPosition`).
    ///
    /// This is a no-op when [`is_draggable`](Marker::is_draggable) is `false`,
    /// matching silx, which moves the item on drag only if `isDraggable()`.
    pub fn drag(&mut self, from: (f64, f64), to: (f64, f64)) {
        if !self.is_draggable {
            return;
        }
        let (fx, fy) = apply_constraint(self.constraint, from, to);
        match &mut self.kind {
            MarkerKind::Point { x, y, .. } => {
                *x = fx;
                *y = fy;
            }
            MarkerKind::VLine { x } => *x = fx,
            MarkerKind::HLine { y } => *y = fy,
        }
    }

    /// Screen position of a point marker, or `None` for a line marker.
    pub fn screen_point(&self, t: &Transform) -> Option<Pos2> {
        match self.kind {
            MarkerKind::Point { x, y, .. } => Some(t.data_to_pixel(x, y)),
            _ => None,
        }
    }

    /// Screen x of a vertical-line marker, or `None` otherwise. Independent of y,
    /// so it is evaluated at the axis minimum.
    pub fn screen_x(&self, t: &Transform) -> Option<f32> {
        match self.kind {
            MarkerKind::VLine { x } => Some(t.data_to_pixel(x, t.y.min).x),
            _ => None,
        }
    }

    /// Screen y of a horizontal-line marker, or `None` otherwise. Independent of
    /// x, so it is evaluated at the axis minimum.
    pub fn screen_y(&self, t: &Transform) -> Option<f32> {
        match self.kind {
            MarkerKind::HLine { y } => Some(t.data_to_pixel(t.x.min, y).y),
            _ => None,
        }
    }
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
    fn point_defaults_are_circle_white_left_axis() {
        let m = Marker::point(1.0, 2.0);
        assert_eq!(
            m.kind,
            MarkerKind::Point {
                x: 1.0,
                y: 2.0,
                symbol: MarkerSymbol::Circle,
                size: DEFAULT_MARKER_SIZE,
            }
        );
        assert_eq!(m.color, Color32::WHITE);
        assert_eq!(m.y_axis, YAxis::Left);
        assert_eq!(m.line_style, LineStyle::Solid);
        assert!(m.text.is_none() && m.bgcolor.is_none());
    }

    #[test]
    fn builders_set_fields() {
        let m = Marker::point(0.0, 0.0)
            .with_color(Color32::RED)
            .with_text("peak")
            .with_bgcolor(Color32::BLACK)
            .with_symbol(MarkerSymbol::Diamond)
            .with_symbol_size(12.0)
            .with_y_axis(YAxis::Right);
        assert_eq!(m.color, Color32::RED);
        assert_eq!(m.text.as_deref(), Some("peak"));
        assert_eq!(m.bgcolor, Some(Color32::BLACK));
        assert_eq!(m.y_axis, YAxis::Right);
        assert_eq!(
            m.kind,
            MarkerKind::Point {
                x: 0.0,
                y: 0.0,
                symbol: MarkerSymbol::Diamond,
                size: 12.0,
            }
        );
    }

    #[test]
    fn symbol_builders_are_noops_on_line_markers() {
        // A vertical line has no symbol/size; the builders leave the kind intact.
        let v = Marker::vline(3.0)
            .with_symbol(MarkerSymbol::Square)
            .with_symbol_size(20.0);
        assert_eq!(v.kind, MarkerKind::VLine { x: 3.0 });
        // Line style/width builders apply.
        let v = v.with_line_style(LineStyle::Dashed).with_line_width(2.0);
        assert_eq!(v.line_style, LineStyle::Dashed);
        assert_eq!(v.line_width, 2.0);
    }

    #[test]
    fn constraint_none_moves_both_coordinates() {
        let from = (1.0, 2.0);
        let to = (5.0, 6.0);
        assert_eq!(
            apply_constraint(MarkerConstraint::None, from, to),
            (5.0, 6.0)
        );
    }

    #[test]
    fn horizontal_constraint_pins_x_keeps_y() {
        // silx _horizontalConstraint: return (getXPosition(), y).
        let from = (1.0, 2.0);
        let to = (5.0, 6.0);
        assert_eq!(
            apply_constraint(MarkerConstraint::Horizontal, from, to),
            (1.0, 6.0)
        );
    }

    #[test]
    fn vertical_constraint_pins_y_keeps_x() {
        // silx _verticalConstraint: return (x, getYPosition()).
        let from = (1.0, 2.0);
        let to = (5.0, 6.0);
        assert_eq!(
            apply_constraint(MarkerConstraint::Vertical, from, to),
            (5.0, 2.0)
        );
    }

    #[test]
    fn custom_constraint_closure_applied_directly() {
        // The custom path: caller invokes the closure on `to` (silx arbitrary
        // callable). Here a closure that snaps to integer grid.
        let snap = |to: (f64, f64)| (to.0.round(), to.1.round());
        assert_eq!(snap((5.4, 6.6)), (5.0, 7.0));
    }

    #[test]
    fn drag_free_point_moves_both() {
        let mut m = Marker::point(1.0, 2.0).with_draggable(true);
        let from = m.position();
        m.drag(from, (5.0, 6.0));
        assert_eq!(m.position(), (5.0, 6.0));
    }

    #[test]
    fn drag_horizontal_constraint_pins_x() {
        let mut m = Marker::point(1.0, 2.0)
            .with_draggable(true)
            .with_constraint(MarkerConstraint::Horizontal);
        let from = m.position();
        m.drag(from, (5.0, 6.0));
        // X stays at 1.0 (current), Y moves to 6.0.
        assert_eq!(m.position(), (1.0, 6.0));
    }

    #[test]
    fn drag_vertical_constraint_pins_y() {
        let mut m = Marker::point(1.0, 2.0)
            .with_draggable(true)
            .with_constraint(MarkerConstraint::Vertical);
        let from = m.position();
        m.drag(from, (5.0, 6.0));
        // X moves to 5.0, Y stays at 2.0 (current).
        assert_eq!(m.position(), (5.0, 2.0));
    }

    #[test]
    fn drag_non_draggable_is_a_noop() {
        // Default is_draggable == false: drag must not move the marker.
        let mut m = Marker::point(1.0, 2.0);
        assert!(!m.is_draggable);
        let from = m.position();
        m.drag(from, (5.0, 6.0));
        assert_eq!(m.position(), (1.0, 2.0));
    }

    #[test]
    fn drag_line_markers_update_only_their_axis() {
        // VLine: only X updates (silx XMarker.setPosition takes constraint X).
        let mut v = Marker::vline(3.0).with_draggable(true);
        let from = v.position();
        v.drag(from, (7.0, 99.0));
        assert_eq!(v.kind, MarkerKind::VLine { x: 7.0 });

        // HLine: only Y updates (silx YMarker.setPosition takes constraint Y).
        let mut h = Marker::hline(3.0).with_draggable(true);
        let from = h.position();
        h.drag(from, (99.0, 7.0));
        assert_eq!(h.kind, MarkerKind::HLine { y: 7.0 });
    }

    #[test]
    fn text_anchor_default_is_top_left() {
        // silx point-marker default is horizontalalignment="left".
        assert_eq!(TextAnchor::default(), TextAnchor::TopLeft);
        assert_eq!(Marker::point(0.0, 0.0).text_anchor, TextAnchor::TopLeft);
    }

    #[test]
    fn text_anchor_offsets_against_a_known_rect() {
        // A 40x10 text rect; check each anchor's top-left offset.
        let size = (40.0, 10.0);
        // Corners.
        assert_eq!(TextAnchor::TopLeft.rect_offset(size), (0.0, 0.0));
        assert_eq!(TextAnchor::TopRight.rect_offset(size), (-40.0, 0.0));
        assert_eq!(TextAnchor::BottomLeft.rect_offset(size), (0.0, -10.0));
        assert_eq!(TextAnchor::BottomRight.rect_offset(size), (-40.0, -10.0));
        // Edge midpoints.
        assert_eq!(TextAnchor::Top.rect_offset(size), (-20.0, 0.0));
        assert_eq!(TextAnchor::Bottom.rect_offset(size), (-20.0, -10.0));
        assert_eq!(TextAnchor::Left.rect_offset(size), (0.0, -5.0));
        assert_eq!(TextAnchor::Right.rect_offset(size), (-40.0, -5.0));
        // Center.
        assert_eq!(TextAnchor::Center.rect_offset(size), (-20.0, -5.0));
    }

    #[test]
    fn pick_point_inside_radius_hits_outside_misses() {
        // Point at data (5, 5) -> pixel (50, 50); size 10 -> radius 5+5 = 10px.
        let m = Marker::point(5.0, 5.0).with_symbol_size(10.0);
        // 9px away (just inside the 10px radius): hit.
        assert!(m.pick(&t(), pos2(50.0 + 9.0, 50.0)));
        // 12px away (outside the radius): miss.
        assert!(!m.pick(&t(), pos2(50.0 + 12.0, 50.0)));
    }

    #[test]
    fn pick_vline_near_x_within_span_hits_off_span_misses() {
        // VLine at x=4 -> pixel x=40; line_width 1 -> tolerance 5+0.5 = 5.5px.
        let v = Marker::vline(4.0);
        // Within tolerance in X and inside the [0,100] y-span: hit.
        assert!(v.pick(&t(), pos2(43.0, 50.0)));
        // X too far from the line: miss even on-span.
        assert!(!v.pick(&t(), pos2(60.0, 50.0)));
        // On the line X but well outside the y-span (above the area): miss.
        assert!(!v.pick(&t(), pos2(40.0, -50.0)));
    }

    #[test]
    fn pick_hline_near_y_within_span_hits_off_span_misses() {
        // HLine at y=8 -> pixel y=20 (y flipped); tolerance 5.5px.
        let h = Marker::hline(8.0);
        // Within tolerance in Y and inside the [0,100] x-span: hit.
        assert!(h.pick(&t(), pos2(50.0, 23.0)));
        // Y too far from the line: miss even on-span.
        assert!(!h.pick(&t(), pos2(50.0, 60.0)));
        // On the line Y but well outside the x-span (left of the area): miss.
        assert!(!h.pick(&t(), pos2(-50.0, 20.0)));
    }

    #[test]
    fn screen_helpers_select_by_kind() {
        // Point at data (2, 3): x 2->20px, y 3 (bottom-ish) -> 70px (y flipped).
        let p = Marker::point(2.0, 3.0);
        let pos = p.screen_point(&t()).expect("point pos");
        assert!((pos.x - 20.0).abs() < 1e-3 && (pos.y - 70.0).abs() < 1e-3);
        assert!(p.screen_x(&t()).is_none() && p.screen_y(&t()).is_none());

        // Vertical line at x=4 -> 40px; no point/y.
        let v = Marker::vline(4.0);
        assert!((v.screen_x(&t()).expect("vline x") - 40.0).abs() < 1e-3);
        assert!(v.screen_point(&t()).is_none() && v.screen_y(&t()).is_none());

        // Horizontal line at y=8 -> screen 20px (y flipped); no point/x.
        let h = Marker::hline(8.0);
        assert!((h.screen_y(&t()).expect("hline y") - 20.0).abs() < 1e-3);
        assert!(h.screen_point(&t()).is_none() && h.screen_x(&t()).is_none());
    }
}
