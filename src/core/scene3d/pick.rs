//! CPU ray–geometry picking for the 3D scene.
//!
//! Port of silx's scene picking (`gui/plot3d/items/_pick.py` +
//! `gui/_glutils/utils.py`). silx picks **geometrically on the CPU**: a click is
//! unprojected into a near→far segment through the inverse camera matrix
//! ([`picking_segment`], silx `PickContext.getPickingSegment`), and each item's
//! `_pickFull` intersects that segment with its geometry — there is no GPU
//! colour-id readback. This module owns the two primitives every item picker
//! needs: the segment builder and segment/triangle intersection
//! ([`segment_triangles_intersection`], silx `_glutils.utils`); per-item
//! traversal (points, meshes, volumes) builds on these.
//!
//! Coordinates: [`picking_segment`] returns the segment in **scene/world**
//! space (the frame the [`crate::render::gpu_scene3d::Scene3dGeometry`] vertices
//! live in), matching how silx requests the segment in the picked primitive's
//! object frame.

use super::camera::Camera;
use super::mat4::Vec3;

/// One segment/triangle intersection, faithful to silx
/// `segmentTrianglesIntersection`'s return tuple.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct TriangleHit {
    /// Index of the intersected triangle in the input slice.
    pub triangle: usize,
    /// Segment parameter `t ∈ [0, 1]` of the intersection point: the hit is at
    /// `s0 + t·(s1 − s0)`. Lower `t` is nearer the segment start (the camera).
    pub t: f32,
    /// Barycentric coordinates of the hit within the triangle (`[w0, w1, w2]`,
    /// weights of vertices 0/1/2).
    pub barycentric: [f32; 3],
}

impl TriangleHit {
    /// The intersection point in the segment's coordinate frame, given the same
    /// segment endpoints passed to [`segment_triangles_intersection`].
    pub fn position(&self, s0: Vec3, s1: Vec3) -> Vec3 {
        s0 + (s1 - s0) * self.t
    }
}

/// Build the picking segment for a click at normalized device coordinates `ndc`
/// (`x, y ∈ [-1, 1]`), returning its near and far points in **scene/world**
/// space, or `None` if the camera matrix is singular.
///
/// Port of silx `PickContext.getPickingSegment(frame='scene')`: the near/far
/// points are the NDC ray endpoints at `z = -1` and `z = +1` (the full depth
/// range — silx's depth-buffer narrowing is an optional optimisation, a no-op
/// stub in the reference) unprojected through the inverse camera matrix. The
/// `true` perspective-divide matches silx `transformPoints(perspectiveDivide=True)`,
/// the same un-projection the pan/zoom interaction uses
/// ([`Camera::pan`](super::camera::Camera::pan)).
pub fn picking_segment(camera: &Camera, ndc: (f32, f32)) -> Option<(Vec3, Vec3)> {
    let inv = camera.matrix().inverse()?;
    let near = inv.transform_point(Vec3::new(ndc.0, ndc.1, -1.0), true);
    let far = inv.transform_point(Vec3::new(ndc.0, ndc.1, 1.0), true);
    Some((near, far))
}

/// Intersect the segment `(s0, s1)` with each triangle in `triangles`
/// (`[v0, v1, v2]` per triangle), returning the hits within the segment sorted
/// by increasing depth `t` (nearest first).
///
/// Line-for-line port of silx `segmentTrianglesIntersection`
/// (`gui/_glutils/utils.py`), the signed-tetrahedron-volume test of Kensler &
/// Shirley (2006): a triangle is hit when the three sub-volumes share a sign
/// (the line crosses the triangle) and the segment parameter `t` lands in
/// `[0, 1]`. Degenerate triangles (`volume == 0`, i.e. the segment is parallel
/// to the triangle plane or the triangle is collinear) are skipped — silx lets
/// the division produce `NaN`, which then fails the `0 ≤ t ≤ 1` mask; the
/// explicit guard here yields the same outcome without relying on `NaN`
/// comparisons.
pub fn segment_triangles_intersection(
    segment: (Vec3, Vec3),
    triangles: &[[Vec3; 3]],
) -> Vec<TriangleHit> {
    let (s0, s1) = segment;
    let d = s1 - s0;

    let mut hits: Vec<TriangleHit> = Vec::new();
    for (index, tri) in triangles.iter().enumerate() {
        let t0s0 = s0 - tri[0];
        let edge01 = tri[1] - tri[0];
        let edge02 = tri[2] - tri[0];

        let d_cross_edge02 = d.cross(edge02);
        let t0s0_cross_edge01 = t0s0.cross(edge01);

        let volume = d_cross_edge02.dot(edge01);
        if volume == 0.0 {
            continue;
        }
        let sub1 = d_cross_edge02.dot(t0s0);
        let sub2 = t0s0_cross_edge01.dot(d);
        let sub0 = volume - sub1 - sub2;

        let all_pos = sub0 >= 0.0 && sub1 >= 0.0 && sub2 >= 0.0;
        let all_neg = sub0 <= 0.0 && sub1 <= 0.0 && sub2 <= 0.0;
        if !(all_pos || all_neg) {
            continue;
        }

        let t = t0s0_cross_edge01.dot(edge02) / volume;
        if !(0.0..=1.0).contains(&t) {
            continue;
        }

        hits.push(TriangleHit {
            triangle: index,
            t,
            barycentric: [sub0 / volume, sub1 / volume, sub2 / volume],
        });
    }

    hits.sort_by(|a, b| a.t.total_cmp(&b.t));
    hits
}

#[cfg(test)]
mod tests {
    use super::*;

    fn approx(a: f32, b: f32) {
        assert!((a - b).abs() < 1e-5, "{a} != {b}");
    }

    fn approx_vec(a: Vec3, b: Vec3) {
        approx(a.x, b.x);
        approx(a.y, b.y);
        approx(a.z, b.z);
    }

    #[test]
    fn segment_through_triangle_centre_hits_at_midpoint() {
        // A unit triangle in the z = 0 plane; a segment from +z to -z through its
        // centroid crosses it at t = 0.5 with equal barycentric weights.
        let tri = [
            Vec3::new(0.0, 0.0, 0.0),
            Vec3::new(1.0, 0.0, 0.0),
            Vec3::new(0.0, 1.0, 0.0),
        ];
        let centroid = Vec3::new(1.0 / 3.0, 1.0 / 3.0, 0.0);
        let s0 = centroid + Vec3::new(0.0, 0.0, 1.0);
        let s1 = centroid + Vec3::new(0.0, 0.0, -1.0);
        let hits = segment_triangles_intersection((s0, s1), &[tri]);
        assert_eq!(hits.len(), 1);
        approx(hits[0].t, 0.5);
        approx_vec(hits[0].position(s0, s1), centroid);
        for w in hits[0].barycentric {
            approx(w, 1.0 / 3.0);
        }
    }

    #[test]
    fn segment_missing_triangle_returns_no_hit() {
        let tri = [
            Vec3::new(0.0, 0.0, 0.0),
            Vec3::new(1.0, 0.0, 0.0),
            Vec3::new(0.0, 1.0, 0.0),
        ];
        // Crosses the z = 0 plane well outside the triangle.
        let s0 = Vec3::new(5.0, 5.0, 1.0);
        let s1 = Vec3::new(5.0, 5.0, -1.0);
        assert!(segment_triangles_intersection((s0, s1), &[tri]).is_empty());
    }

    #[test]
    fn segment_short_of_triangle_returns_no_hit() {
        // The plane crossing is beyond the segment's far end (t > 1).
        let tri = [
            Vec3::new(0.0, 0.0, 0.0),
            Vec3::new(1.0, 0.0, 0.0),
            Vec3::new(0.0, 1.0, 0.0),
        ];
        let c = Vec3::new(0.25, 0.25, 0.0);
        let s0 = c + Vec3::new(0.0, 0.0, 2.0);
        let s1 = c + Vec3::new(0.0, 0.0, 1.0); // both on +z side, never reaches z=0
        assert!(segment_triangles_intersection((s0, s1), &[tri]).is_empty());
    }

    #[test]
    fn parallel_segment_is_skipped() {
        let tri = [
            Vec3::new(0.0, 0.0, 0.0),
            Vec3::new(1.0, 0.0, 0.0),
            Vec3::new(0.0, 1.0, 0.0),
        ];
        // Segment lies in a plane parallel to the triangle (z = 1): volume == 0.
        let s0 = Vec3::new(0.0, 0.0, 1.0);
        let s1 = Vec3::new(1.0, 1.0, 1.0);
        assert!(segment_triangles_intersection((s0, s1), &[tri]).is_empty());
    }

    #[test]
    fn picking_segment_round_trips_to_screen_centre_and_depth_ends() {
        // A perspective camera framing the unit box; the centre-screen ray must
        // unproject so that re-projecting its ends lands back at NDC (0, 0) with
        // z = −1 (near) and z = +1 (far) — i.e. the inverse inverts the projection.
        let mut cam = Camera::new(
            30.0,
            0.1,
            100.0,
            (4.0, 3.0),
            Vec3::new(0.0, 0.0, 5.0),
            Vec3::new(0.0, 0.0, -1.0),
            Vec3::new(0.0, 1.0, 0.0),
        );
        cam.reset_camera((Vec3::ZERO, Vec3::new(1.0, 1.0, 1.0)));

        let (near, far) = picking_segment(&cam, (0.0, 0.0)).expect("camera is non-singular");
        let m = cam.matrix();
        let pn = m.transform_point(near, true);
        let pf = m.transform_point(far, true);
        approx(pn.x, 0.0);
        approx(pn.y, 0.0);
        approx(pn.z, -1.0);
        approx(pf.x, 0.0);
        approx(pf.y, 0.0);
        approx(pf.z, 1.0);
    }

    #[test]
    fn hits_are_sorted_near_to_far() {
        // Two parallel triangles at z = 0 and z = 1; a +z→-z segment hits the
        // farther one (z = 1) first (smaller t) then the nearer (z = 0).
        let tri_lo = [
            Vec3::new(-1.0, -1.0, 0.0),
            Vec3::new(1.0, -1.0, 0.0),
            Vec3::new(0.0, 1.0, 0.0),
        ];
        let tri_hi = [
            Vec3::new(-1.0, -1.0, 1.0),
            Vec3::new(1.0, -1.0, 1.0),
            Vec3::new(0.0, 1.0, 1.0),
        ];
        let s0 = Vec3::new(0.0, 0.0, 2.0);
        let s1 = Vec3::new(0.0, 0.0, -1.0);
        let hits = segment_triangles_intersection((s0, s1), &[tri_lo, tri_hi]);
        assert_eq!(hits.len(), 2);
        // tri_hi (z=1) is nearer the segment start, so it sorts first.
        assert_eq!(hits[0].triangle, 1);
        assert_eq!(hits[1].triangle, 0);
        assert!(hits[0].t < hits[1].t);
    }
}
