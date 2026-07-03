//! Per-item transform stack — the port of silx `plot3d.items.core.DataItem3D`
//! transforms (`items/core.py:288-315`).
//!
//! Every silx 3D data item owns the composed transform list
//! `[translate, rotateFwd(center), rotate, rotateBwd, [matrix, scale]]`; a
//! point is mapped by `translate · T(c) · rotate · T(−c) · matrix · scale`,
//! where the rotation centre `c` may be given per-axis as an absolute value or
//! as a tag resolved against the item's (matrix·scale)-transformed data bounds
//! (`core.py:376-405` `_updateRotationCenter`).
//!
//! In this port the composed matrix is applied CPU-side when an item bakes its
//! geometry (`scene3d_items::append_with_transform`), so rendering and CPU
//! picking read the same transformed positions by construction.

use crate::core::scene3d::mat4::{Mat4, Vec3, mat4_rotate, mat4_scale, mat4_translate};

/// One axis of the rotation centre (silx `setRotationCenter`,
/// `items/core.py:406-436`): an absolute scene value, or a tag resolved
/// against the item's bounding box.
#[derive(Clone, Copy, Debug, PartialEq)]
pub enum RotationCenter {
    /// Absolute position on this axis (the silx float form; default `0`).
    Absolute(f32),
    /// The lower bound of the item's bounding box on this axis (`'lower'`).
    Lower,
    /// The centre of the item's bounding box on this axis (`'center'`).
    Center,
    /// The upper bound of the item's bounding box on this axis (`'upper'`).
    Upper,
}

impl Default for RotationCenter {
    fn default() -> Self {
        RotationCenter::Absolute(0.0)
    }
}

/// The silx `DataItem3D` transform stack (`items/core.py:288-315`), with the
/// public setters of `core.py:335-485`. Compose with
/// [`Item3DTransform::composed_matrix`]; the identity by default.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Item3DTransform {
    translation: Vec3,
    /// Rotation angle in degrees (silx `transform.Rotate`).
    rotation_angle_deg: f32,
    /// Unit rotation axis; silx normalizes on set and resets a zero axis to
    /// `(0, 0, 1)` with angle 0 (`scene/transform.py:759-778`).
    rotation_axis: Vec3,
    rotation_center: [RotationCenter; 3],
    /// The 3×3 part of silx `setMatrix` (`core.py:459-473`), row-major.
    matrix3: [[f32; 3]; 3],
    scale: Vec3,
}

const MATRIX3_IDENTITY: [[f32; 3]; 3] = [[1.0, 0.0, 0.0], [0.0, 1.0, 0.0], [0.0, 0.0, 1.0]];

impl Default for Item3DTransform {
    fn default() -> Self {
        Item3DTransform {
            translation: Vec3::ZERO,
            rotation_angle_deg: 0.0,
            rotation_axis: Vec3::new(0.0, 0.0, 1.0),
            rotation_center: [RotationCenter::default(); 3],
            matrix3: MATRIX3_IDENTITY,
            scale: Vec3::new(1.0, 1.0, 1.0),
        }
    }
}

impl Item3DTransform {
    /// The identity transform (all silx defaults).
    pub fn new() -> Self {
        Self::default()
    }

    /// True when every component is at its default, i.e. the composed matrix
    /// is the identity — items skip the bake-time transform entirely then.
    pub fn is_identity(&self) -> bool {
        *self == Self::default()
    }

    /// Set the scale of the item in the scene (silx `setScale`,
    /// `items/core.py:335-345`).
    pub fn set_scale(&mut self, sx: f32, sy: f32, sz: f32) {
        self.scale = Vec3::new(sx, sy, sz);
    }

    /// The scale set by [`set_scale`](Self::set_scale) (silx `getScale`).
    pub fn scale(&self) -> Vec3 {
        self.scale
    }

    /// Set the translation of the item's origin in the scene (silx
    /// `setTranslation`, `items/core.py:353-364`).
    pub fn set_translation(&mut self, x: f32, y: f32, z: f32) {
        self.translation = Vec3::new(x, y, z);
    }

    /// The translation set by [`set_translation`](Self::set_translation).
    pub fn translation(&self) -> Vec3 {
        self.translation
    }

    /// Set the centre of rotation, per axis absolute or bounding-box-relative
    /// (silx `setRotationCenter`, `items/core.py:406-436`).
    pub fn set_rotation_center(&mut self, x: RotationCenter, y: RotationCenter, z: RotationCenter) {
        self.rotation_center = [x, y, z];
    }

    /// The rotation centre set by
    /// [`set_rotation_center`](Self::set_rotation_center).
    pub fn rotation_center(&self) -> [RotationCenter; 3] {
        self.rotation_center
    }

    /// Set the rotation as an angle (degrees) about `axis` (silx `setRotation`,
    /// `items/core.py:445-458`). A zero axis resets to angle 0 about `(0,0,1)`,
    /// as silx `Rotate.setAngleAxis` (`scene/transform.py:773-778`).
    pub fn set_rotation(&mut self, angle_deg: f32, axis: Vec3) {
        if axis.length() == 0.0 {
            self.rotation_angle_deg = 0.0;
            self.rotation_axis = Vec3::new(0.0, 0.0, 1.0);
        } else {
            self.rotation_angle_deg = angle_deg;
            self.rotation_axis = axis.normalized();
        }
    }

    /// The `(angle_degrees, unit_axis)` set by
    /// [`set_rotation`](Self::set_rotation) (silx `getRotation`).
    pub fn rotation(&self) -> (f32, Vec3) {
        (self.rotation_angle_deg, self.rotation_axis)
    }

    /// Set the 3×3 transform matrix, `None` for identity (silx `setMatrix`,
    /// `items/core.py:459-473` — the 3×3 is embedded in a 4×4).
    pub fn set_matrix(&mut self, matrix: Option<[[f32; 3]; 3]>) {
        self.matrix3 = matrix.unwrap_or(MATRIX3_IDENTITY);
    }

    /// The 3×3 matrix set by [`set_matrix`](Self::set_matrix) (silx
    /// `getMatrix`).
    pub fn matrix(&self) -> [[f32; 3]; 3] {
        self.matrix3
    }

    /// `matrix · scale` — silx `_transformObjectToRotate`
    /// (`items/core.py:297-300`), the part of the stack applied *before*
    /// rotation, against which bounding-box-relative rotation centres resolve.
    fn object_to_rotate_matrix(&self) -> Mat4 {
        let m = &self.matrix3;
        let matrix4 = Mat4::from_rows([
            [m[0][0], m[0][1], m[0][2], 0.0],
            [m[1][0], m[1][1], m[1][2], 0.0],
            [m[2][0], m[2][1], m[2][2], 0.0],
            [0.0, 0.0, 0.0, 1.0],
        ]);
        matrix4 * mat4_scale(self.scale.x, self.scale.y, self.scale.z)
    }

    /// Resolve the rotation centre against `data_bounds` (the item's **raw**
    /// data bounds) — silx `_updateRotationCenter` (`items/core.py:376-405`):
    /// tagged axes read the bounds transformed by `[matrix, scale]`; a missing
    /// bounds resolves tags to `0`.
    pub fn resolve_rotation_center(&self, data_bounds: Option<(Vec3, Vec3)>) -> Vec3 {
        let bounds = data_bounds.map(|b| transform_aabb(&self.object_to_rotate_matrix(), b));
        let mut out = [0.0f32; 3];
        for (index, (slot, center)) in out.iter_mut().zip(self.rotation_center).enumerate() {
            *slot = match (center, bounds) {
                (RotationCenter::Absolute(v), _) => v,
                (_, None) => 0.0,
                (RotationCenter::Lower, Some((lo, _))) => lo.to_array()[index],
                (RotationCenter::Center, Some((lo, hi))) => {
                    0.5 * (lo.to_array()[index] + hi.to_array()[index])
                }
                (RotationCenter::Upper, Some((_, hi))) => hi.to_array()[index],
            };
        }
        Vec3::from_array(out)
    }

    /// The composed matrix `translate · T(c) · rotate · T(−c) · matrix · scale`
    /// (silx `__transforms`, `items/core.py:305-313`), with the rotation centre
    /// `c` resolved against `data_bounds` (the item's raw data bounds).
    pub fn composed_matrix(&self, data_bounds: Option<(Vec3, Vec3)>) -> Mat4 {
        let c = self.resolve_rotation_center(data_bounds);
        let t = self.translation;
        let a = self.rotation_axis;
        mat4_translate(t.x, t.y, t.z)
            * mat4_translate(c.x, c.y, c.z)
            * mat4_rotate(self.rotation_angle_deg.to_radians(), a.x, a.y, a.z)
            * mat4_translate(-c.x, -c.y, -c.z)
            * self.object_to_rotate_matrix()
    }

    /// The axis-aligned bounds of `raw` under the composed matrix — the
    /// transformed-corner AABB, silx `Transform.transformBounds`. Identity
    /// passes `raw` through unchanged.
    pub fn transform_bounds(&self, raw: Option<(Vec3, Vec3)>) -> Option<(Vec3, Vec3)> {
        let raw = raw?;
        if self.is_identity() {
            return Some(raw);
        }
        Some(transform_aabb(&self.composed_matrix(Some(raw)), raw))
    }
}

/// The axis-aligned bounding box of `(min, max)`'s eight corners mapped
/// through `m` (silx `Transform.transformBounds`).
pub fn transform_aabb(m: &Mat4, (mn, mx): (Vec3, Vec3)) -> (Vec3, Vec3) {
    let mut lo = Vec3::new(f32::INFINITY, f32::INFINITY, f32::INFINITY);
    let mut hi = Vec3::new(f32::NEG_INFINITY, f32::NEG_INFINITY, f32::NEG_INFINITY);
    for corner in [
        Vec3::new(mn.x, mn.y, mn.z),
        Vec3::new(mx.x, mn.y, mn.z),
        Vec3::new(mn.x, mx.y, mn.z),
        Vec3::new(mx.x, mx.y, mn.z),
        Vec3::new(mn.x, mn.y, mx.z),
        Vec3::new(mx.x, mn.y, mx.z),
        Vec3::new(mn.x, mx.y, mx.z),
        Vec3::new(mx.x, mx.y, mx.z),
    ] {
        let p = m.transform_point(corner, false);
        lo = Vec3::new(lo.x.min(p.x), lo.y.min(p.y), lo.z.min(p.z));
        hi = Vec3::new(hi.x.max(p.x), hi.y.max(p.y), hi.z.max(p.z));
    }
    (lo, hi)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn close(a: Vec3, b: Vec3) -> bool {
        (a.x - b.x).abs() < 1e-4 && (a.y - b.y).abs() < 1e-4 && (a.z - b.z).abs() < 1e-4
    }

    #[test]
    fn default_is_identity() {
        let t = Item3DTransform::new();
        assert!(t.is_identity());
        let p = Vec3::new(1.5, -2.0, 3.0);
        assert!(close(t.composed_matrix(None).transform_point(p, false), p));
    }

    #[test]
    fn compose_order_matches_silx_stack() {
        // silx applies scale, then matrix, then the centred rotation, then the
        // translation (core.py:305-313). Hand-check with scale (2,3,4),
        // rotation 90° about +z at centre 0, translation (10, 20, 30):
        // p = (1, 0, 0) → scale (2, 0, 0) → rotate (0, 2, 0) → (10, 22, 30).
        let mut t = Item3DTransform::new();
        t.set_scale(2.0, 3.0, 4.0);
        t.set_rotation(90.0, Vec3::new(0.0, 0.0, 1.0));
        t.set_translation(10.0, 20.0, 30.0);
        let m = t.composed_matrix(None);
        assert!(close(
            m.transform_point(Vec3::new(1.0, 0.0, 0.0), false),
            Vec3::new(10.0, 22.0, 30.0)
        ));

        // Matrix applies AFTER scale (matrix · scale, core.py:297-300):
        // p = (1, 1, 0) → scale (2, 3, 0) → matrix swap x/y (3, 2, 0).
        let mut t = Item3DTransform::new();
        t.set_scale(2.0, 3.0, 4.0);
        t.set_matrix(Some([[0.0, 1.0, 0.0], [1.0, 0.0, 0.0], [0.0, 0.0, 1.0]]));
        let m = t.composed_matrix(None);
        assert!(close(
            m.transform_point(Vec3::new(1.0, 1.0, 0.0), false),
            Vec3::new(3.0, 2.0, 0.0)
        ));
    }

    #[test]
    fn rotation_center_absolute_offsets_the_pivot() {
        // 180° about +z at absolute centre (1, 0, 0): p = (2, 0, 0) → (0, 0, 0).
        let mut t = Item3DTransform::new();
        t.set_rotation(180.0, Vec3::new(0.0, 0.0, 1.0));
        t.set_rotation_center(
            RotationCenter::Absolute(1.0),
            RotationCenter::Absolute(0.0),
            RotationCenter::Absolute(0.0),
        );
        let m = t.composed_matrix(None);
        assert!(close(
            m.transform_point(Vec3::new(2.0, 0.0, 0.0), false),
            Vec3::new(0.0, 0.0, 0.0)
        ));
    }

    #[test]
    fn rotation_center_tags_resolve_against_scaled_bounds() {
        // Raw bounds (0,0,0)..(2,4,6), scale (2,1,1) → transformed bounds
        // (0,0,0)..(4,4,6) (silx resolves tags against [matrix, scale]-mapped
        // bounds, core.py:381-395). 'center' x = 2, 'lower' y = 0, 'upper' z=6.
        let mut t = Item3DTransform::new();
        t.set_scale(2.0, 1.0, 1.0);
        t.set_rotation_center(
            RotationCenter::Center,
            RotationCenter::Lower,
            RotationCenter::Upper,
        );
        let bounds = Some((Vec3::ZERO, Vec3::new(2.0, 4.0, 6.0)));
        assert!(close(
            t.resolve_rotation_center(bounds),
            Vec3::new(2.0, 0.0, 6.0)
        ));

        // Missing bounds → tags resolve to 0 (core.py:387-388).
        assert!(close(t.resolve_rotation_center(None), Vec3::ZERO));

        // 180° about +z pivoting on the scaled bbox centre (2, 2, ·): raw
        // corner (0, 0, 0) → scaled (0, 0, 0) → rotated to (4, 4, 0).
        t.set_rotation_center(
            RotationCenter::Center,
            RotationCenter::Center,
            RotationCenter::Absolute(0.0),
        );
        t.set_rotation(180.0, Vec3::new(0.0, 0.0, 1.0));
        let m = t.composed_matrix(bounds);
        assert!(close(
            m.transform_point(Vec3::ZERO, false),
            Vec3::new(4.0, 4.0, 0.0)
        ));
    }

    #[test]
    fn zero_rotation_axis_resets_like_silx() {
        let mut t = Item3DTransform::new();
        t.set_rotation(45.0, Vec3::ZERO);
        assert_eq!(t.rotation(), (0.0, Vec3::new(0.0, 0.0, 1.0)));
        assert!(t.is_identity());
    }

    #[test]
    fn transform_bounds_is_the_transformed_corner_aabb() {
        let mut t = Item3DTransform::new();
        t.set_rotation(90.0, Vec3::new(0.0, 0.0, 1.0));
        t.set_translation(10.0, 0.0, 0.0);
        // (0,0,0)..(2,1,3) rotated 90° about z → x ∈ [-1,0], y ∈ [0,2]; then
        // translated by (10,0,0).
        let out = t
            .transform_bounds(Some((Vec3::ZERO, Vec3::new(2.0, 1.0, 3.0))))
            .unwrap();
        assert!(close(out.0, Vec3::new(9.0, 0.0, 0.0)));
        assert!(close(out.1, Vec3::new(10.0, 2.0, 3.0)));

        // Identity passes through; None stays None.
        let id = Item3DTransform::new();
        let raw = (Vec3::ZERO, Vec3::new(1.0, 1.0, 1.0));
        assert_eq!(id.transform_bounds(Some(raw)), Some(raw));
        assert_eq!(id.transform_bounds(None), None);
    }
}
