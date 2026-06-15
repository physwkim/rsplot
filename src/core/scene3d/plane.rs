//! Plane geometry for cut planes — the pure, headless port of silx's
//! `scene.utils.Plane` plus the box/plane-intersection helpers
//! (`segmentPlaneIntersect`, `boxPlaneIntersect`, `angleBetweenVectors`) and the
//! unit-box corner/edge tables from `scene.primitives.Box`.
//!
//! A [`Plane`] is a point + (unit) normal. [`box_plane_intersect`] returns the
//! ordered contour polygon where the plane cuts an axis-aligned box — the outline
//! silx renders for a cut plane and the support the [`crate::render::scene3d_items`]
//! `CutPlane` colour-maps. The GPU side (texturing the slice) lives in
//! [`crate::render`]; this module is the geometry only.

use crate::core::scene3d::mat4::Vec3;

/// The 8 corners of the unit box, silx `Box._vertices` order: the `z = 0` face
/// `(0,0,0),(1,0,0),(1,1,0),(0,1,0)` then the `z = 1` face.
const BOX_CORNERS: [[f32; 3]; 8] = [
    [0.0, 0.0, 0.0],
    [1.0, 0.0, 0.0],
    [1.0, 1.0, 0.0],
    [0.0, 1.0, 0.0],
    [0.0, 0.0, 1.0],
    [1.0, 0.0, 1.0],
    [1.0, 1.0, 1.0],
    [0.0, 1.0, 1.0],
];

/// The 12 box edges as corner-index pairs, silx `Box._lineIndices` order
/// (`z = 0` face, the four verticals, then the `z = 1` face).
const BOX_EDGES: [[usize; 2]; 12] = [
    [0, 1],
    [1, 2],
    [2, 3],
    [3, 0],
    [0, 4],
    [1, 5],
    [2, 6],
    [3, 7],
    [4, 5],
    [5, 6],
    [6, 7],
    [7, 4],
];

/// Points within this Euclidean distance are treated as the same intersection
/// when de-duplicating box-edge crossings. (silx uses an exact float-tuple set;
/// an epsilon merge is more robust on planes that graze a box corner from
/// several edges, and agrees for distinct interior crossings.)
const DEDUP_EPS: f32 = 1e-5;

/// A plane defined by a point on it and a (unit) normal. Port of silx
/// `scene.utils.Plane`.
///
/// The normal is normalized on construction and on every update (silx normalizes
/// in `setPlane`; this also normalizes in [`new`](Self::new), smoothing silx's
/// un-normalized `__init__`). A zero normal means "no plane"
/// ([`is_plane`](Self::is_plane) is then `false`).
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Plane {
    point: Vec3,
    normal: Vec3,
}

impl Default for Plane {
    /// silx default: point at the origin, normal `(0, 0, 1)`.
    fn default() -> Self {
        Self::new(Vec3::new(0.0, 0.0, 0.0), Vec3::new(0.0, 0.0, 1.0))
    }
}

impl Plane {
    /// A plane through `point` with the given `normal` (normalized if non-zero).
    pub fn new(point: Vec3, normal: Vec3) -> Self {
        Self {
            point,
            normal: normalize_or_zero(normal),
        }
    }

    /// A point on the plane.
    pub fn point(&self) -> Vec3 {
        self.point
    }

    /// The (unit) plane normal.
    pub fn normal(&self) -> Vec3 {
        self.normal
    }

    /// Set the point on the plane.
    pub fn set_point(&mut self, point: Vec3) {
        self.point = point;
    }

    /// Set the plane normal (normalized if non-zero).
    pub fn set_normal(&mut self, normal: Vec3) {
        self.normal = normalize_or_zero(normal);
    }

    /// Plane-equation parameters `[a, b, c, d]` for `a·x + b·y + c·z + d = 0`
    /// (`(a,b,c)` the unit normal, `d = -normal·point`). silx `Plane.parameters`.
    pub fn parameters(&self) -> [f32; 4] {
        let n = self.normal;
        [n.x, n.y, n.z, -n.dot(self.point)]
    }

    /// `true` when a plane is defined (`‖normal‖ ≠ 0`). silx `Plane.isPlane`.
    pub fn is_plane(&self) -> bool {
        self.normal.x != 0.0 || self.normal.y != 0.0 || self.normal.z != 0.0
    }

    /// Move the plane `step` along its normal (silx `Plane.move`).
    pub fn move_along(&mut self, step: f32) {
        self.point += self.normal * step;
    }
}

/// Normalize `v`, or return the zero vector when `v` has zero length.
fn normalize_or_zero(v: Vec3) -> Vec3 {
    let len = v.length();
    if len == 0.0 {
        Vec3::new(0.0, 0.0, 0.0)
    } else {
        v * (1.0 / len)
    }
}

/// Intersect the segment `s0–s1` with the plane `(plane_norm, plane_pt)`. Returns
/// 0 points (no intersection), 1 (a crossing), or 2 (segment lies in the plane).
/// Port of silx `segmentPlaneIntersect`.
pub fn segment_plane_intersect(s0: Vec3, s1: Vec3, plane_norm: Vec3, plane_pt: Vec3) -> Vec<Vec3> {
    let segdir = s1 - s0;
    let dot_norm_seg = plane_norm.dot(segdir);
    if dot_norm_seg == 0.0 {
        // Segment parallel to the plane.
        if plane_norm.dot(plane_pt - s0) == 0.0 {
            return vec![s0, s1]; // Segment lies in the plane.
        }
        return Vec::new();
    }
    let alpha = -plane_norm.dot(s0 - plane_pt) / dot_norm_seg;
    if (0.0..=1.0).contains(&alpha) {
        vec![s0 + segdir * alpha]
    } else {
        Vec::new()
    }
}

/// Oriented angles (radians) from `ref_vector` to each of `vectors`, in `[0, 2π)`
/// using `norm` to break the `[0, π]` ambiguity by sign of the cross product.
/// Port of silx `angleBetweenVectors` (the `norm`-oriented branch). Zero-length
/// inputs yield NaN for that entry (as numpy's divide-by-zero would).
pub fn angle_between_vectors(ref_vector: Vec3, vectors: &[Vec3], norm: Vec3) -> Vec<f32> {
    let r = normalize_or_zero(ref_vector);
    vectors
        .iter()
        .map(|&v| {
            let vn = normalize_or_zero(v);
            let dot = (r.dot(vn)).clamp(-1.0, 1.0);
            let angle = dot.acos();
            if norm.dot(r.cross(vn)) < 0.0 {
                std::f32::consts::TAU - angle
            } else {
                angle
            }
        })
        .collect()
}

/// Ordered contour polygon where the plane `(plane_norm, plane_pt)` cuts the
/// axis-aligned box `bounds = (lo, hi)`. Port of silx `boxPlaneIntersect`.
///
/// Returns an empty vector when the plane misses the box (≤ 2 crossings); 3
/// points as-is; > 3 points ordered around their centroid (a convex polygon on
/// the box faces). Vertices are de-duplicated within [`DEDUP_EPS`].
pub fn box_plane_intersect(bounds: (Vec3, Vec3), plane_norm: Vec3, plane_pt: Vec3) -> Vec<Vec3> {
    let (lo, hi) = bounds;
    let span = hi - lo;
    let corner = |c: [f32; 3]| {
        Vec3::new(
            lo.x + c[0] * span.x,
            lo.y + c[1] * span.y,
            lo.z + c[2] * span.z,
        )
    };

    // Gather unique crossings over the 12 edges.
    let mut points: Vec<Vec3> = Vec::new();
    for [a, b] in BOX_EDGES {
        let s0 = corner(BOX_CORNERS[a]);
        let s1 = corner(BOX_CORNERS[b]);
        for p in segment_plane_intersect(s0, s1, plane_norm, plane_pt) {
            if !points.iter().any(|q| (*q - p).length() <= DEDUP_EPS) {
                points.push(p);
            }
        }
    }

    if points.len() <= 2 {
        return Vec::new();
    }
    if points.len() == 3 {
        return points;
    }

    // Order points to form a polyline lying on the box faces (silx).
    let mut centroid = Vec3::new(0.0, 0.0, 0.0);
    for &p in &points {
        centroid += p;
    }
    centroid = centroid * (1.0 / points.len() as f32);
    let vectors: Vec<Vec3> = points.iter().map(|&p| p - centroid).collect();
    let angles = angle_between_vectors(vectors[0], &vectors, plane_norm);

    let mut order: Vec<usize> = (0..points.len()).collect();
    order.sort_by(|&i, &j| angles[i].total_cmp(&angles[j]));
    order.into_iter().map(|i| points[i]).collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn approx(a: Vec3, b: Vec3) -> bool {
        (a - b).length() < 1e-5
    }

    /// Area of a planar polygon in 3D via the summed edge cross products
    /// (magnitude / 2). Correct ordering gives the true area; a self-crossing
    /// ("bowtie") order collapses it.
    fn polygon_area(poly: &[Vec3]) -> f32 {
        let mut acc = Vec3::new(0.0, 0.0, 0.0);
        for i in 0..poly.len() {
            let a = poly[i];
            let b = poly[(i + 1) % poly.len()];
            acc += a.cross(b);
        }
        acc.length() * 0.5
    }

    #[test]
    fn plane_normalizes_and_reports_parameters() {
        let p = Plane::new(Vec3::new(0.0, 0.0, 2.0), Vec3::new(0.0, 0.0, 5.0));
        assert!(approx(p.normal(), Vec3::new(0.0, 0.0, 1.0)), "unit normal");
        // a·x+b·y+c·z+d=0 through (0,0,2) with normal (0,0,1) → d = -2.
        let [a, b, c, d] = p.parameters();
        assert_eq!([a, b, c], [0.0, 0.0, 1.0]);
        assert!((d + 2.0).abs() < 1e-6, "d = -2, got {d}");
        assert!(p.is_plane());
    }

    #[test]
    fn zero_normal_is_not_a_plane() {
        let p = Plane::new(Vec3::new(0.0, 0.0, 0.0), Vec3::new(0.0, 0.0, 0.0));
        assert!(!p.is_plane());
    }

    #[test]
    fn move_along_translates_point_by_step_times_normal() {
        let mut p = Plane::new(Vec3::new(0.0, 0.0, 0.0), Vec3::new(0.0, 1.0, 0.0));
        p.move_along(3.0);
        assert!(approx(p.point(), Vec3::new(0.0, 3.0, 0.0)));
    }

    #[test]
    fn segment_intersect_crossing_parallel_and_in_plane() {
        let n = Vec3::new(0.0, 0.0, 1.0);
        let pt = Vec3::new(0.0, 0.0, 0.5);
        // Crossing at the midpoint.
        let r = segment_plane_intersect(Vec3::new(0.0, 0.0, 0.0), Vec3::new(0.0, 0.0, 1.0), n, pt);
        assert_eq!(r.len(), 1);
        assert!(approx(r[0], Vec3::new(0.0, 0.0, 0.5)));
        // Parallel, off the plane → none.
        let r = segment_plane_intersect(Vec3::new(0.0, 0.0, 0.0), Vec3::new(1.0, 0.0, 0.0), n, pt);
        assert!(r.is_empty());
        // In the plane → both ends.
        let r = segment_plane_intersect(Vec3::new(0.0, 0.0, 0.5), Vec3::new(1.0, 0.0, 0.5), n, pt);
        assert_eq!(r.len(), 2);
    }

    #[test]
    fn box_plane_axis_aligned_square() {
        let bounds = (Vec3::new(0.0, 0.0, 0.0), Vec3::new(1.0, 1.0, 1.0));
        let poly = box_plane_intersect(bounds, Vec3::new(0.0, 0.0, 1.0), Vec3::new(0.0, 0.0, 0.5));
        assert_eq!(poly.len(), 4, "z-plane cuts 4 vertical edges");
        for v in &poly {
            assert!((v.z - 0.5).abs() < 1e-6, "all at z=0.5: {v:?}");
        }
        // Ordered → full unit-square area 1.0 (a bowtie would give 0).
        assert!(
            (polygon_area(&poly) - 1.0).abs() < 1e-5,
            "ordered square area 1.0, got {}",
            polygon_area(&poly)
        );
    }

    #[test]
    fn box_plane_scaled_bounds() {
        // Box (0,0,0)..(4,2,6), y-plane at y=1.
        let bounds = (Vec3::new(0.0, 0.0, 0.0), Vec3::new(4.0, 2.0, 6.0));
        let poly = box_plane_intersect(bounds, Vec3::new(0.0, 1.0, 0.0), Vec3::new(0.0, 1.0, 0.0));
        assert_eq!(poly.len(), 4);
        for v in &poly {
            assert!((v.y - 1.0).abs() < 1e-6, "all at y=1: {v:?}");
        }
        // The y=1 slice is the x×z face: 4 (width) × 6 (depth) = 24.
        assert!(
            (polygon_area(&poly) - 24.0).abs() < 1e-4,
            "area 24, got {}",
            polygon_area(&poly)
        );
    }

    #[test]
    fn box_plane_miss_returns_empty() {
        let bounds = (Vec3::new(0.0, 0.0, 0.0), Vec3::new(1.0, 1.0, 1.0));
        // z-plane at z=5, outside the box.
        let poly = box_plane_intersect(bounds, Vec3::new(0.0, 0.0, 1.0), Vec3::new(0.0, 0.0, 5.0));
        assert!(poly.is_empty());
    }

    #[test]
    fn box_plane_diagonal_is_convex_hexagon() {
        // A plane with normal (1,1,1) through the box centre cuts a hexagon.
        let bounds = (Vec3::new(0.0, 0.0, 0.0), Vec3::new(1.0, 1.0, 1.0));
        let poly = box_plane_intersect(bounds, Vec3::new(1.0, 1.0, 1.0), Vec3::new(0.5, 0.5, 0.5));
        assert_eq!(poly.len(), 6, "main diagonal plane → regular hexagon");
        // Ordered hexagon area: the regular hexagon of the unit cube's diagonal
        // cut has area sqrt(3)*3/4 ≈ 1.2990.
        let area = polygon_area(&poly);
        assert!(
            (area - 3.0_f32.sqrt() * 0.75).abs() < 1e-3,
            "hexagon area ≈ 1.299, got {area}"
        );
    }
}
