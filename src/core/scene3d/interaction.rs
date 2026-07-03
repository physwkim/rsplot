//! Pure camera-interaction helpers — the orbit/pan drag state machines from
//! silx `silx.gui.plot3d.scene.interaction`, ported off Qt so they are unit
//! testable. The egui [`crate::widget::scene_widget::SceneWidget`] drives these
//! from pointer events; zoom and depth-extent adjustment live on
//! [`crate::core::scene3d::camera::Camera`] (they need its projection internals).
//!
//! Window coordinates are pixels with the origin at the top-left (egui's
//! convention, matching silx's `winY origin top`).

use crate::core::scene3d::camera::{Camera, CameraExtrinsic};
use crate::core::scene3d::mat4::{Vec3, mat4_rotate};

/// Window pixel → normalized device coordinates in `[-1, 1]`, origin centre, x
/// right, y up. Port of `Viewport.windowToNdc` (no viewport origin offset: the
/// widget passes coordinates already relative to the scene rect).
pub fn window_to_ndc(win: (f32, f32), size: (f32, f32)) -> (f32, f32) {
    let (x, y) = win;
    let (w, h) = size;
    (2.0 * x / w - 1.0, 1.0 - 2.0 * y / h)
}

/// Arcball-like camera rotation. Port of `CameraSelectRotate`: a drag rotates
/// the view direction (and orbits the position around a fixed centre) by an
/// angle proportional to the drag distance over the smaller viewport dimension.
#[derive(Clone, Copy, Debug)]
pub struct OrbitDrag {
    /// Window pixel where the drag began.
    origin: (f32, f32),
    /// Camera pose captured at drag start (the rotation is always relative to it,
    /// so the motion is absolute to the press point — no per-frame drift).
    start: CameraExtrinsic,
    /// Fixed centre of rotation (scene-space).
    center: Vec3,
}

impl OrbitDrag {
    /// Begin an orbit at window pixel `origin`, rotating around `center`. With
    /// silx's `orbitAroundCenter=False` (the mode `Plot3DWidget` uses,
    /// `Plot3DWidget.py:189-205`) the caller passes the **picked object point**
    /// under the press as `center`, falling back to the scene bounds centre on a
    /// miss (`interaction.py:150-161` `CameraSelectRotate.beginDrag`).
    pub fn begin(camera: &Camera, origin: (f32, f32), center: Vec3) -> Self {
        OrbitDrag {
            origin,
            start: camera.extrinsic,
            center,
        }
    }

    /// Apply the rotation for the current cursor `win` (viewport `size` in
    /// pixels), relative to the captured start pose.
    pub fn update(&self, camera: &mut Camera, win: (f32, f32), size: (f32, f32)) {
        let dx = self.origin.0 - win.0;
        let dy = self.origin.1 - win.1;

        let (direction, up, position) = if dx == 0.0 && dy == 0.0 {
            (
                self.start.direction(),
                self.start.up(),
                self.start.position(),
            )
        } else {
            let minsize = size.0.min(size.1);
            let distance = (dx * dx + dy * dy).sqrt();
            let angle = distance / minsize * std::f32::consts::PI;

            // Drag vector in the image plane (note y inversion via -up).
            let drag = (self.start.side() * dx - self.start.up() * dy).normalized();
            let axis = drag.cross(self.start.direction()).normalized();

            let rotation = mat4_rotate(angle, axis.x, axis.y, axis.z);
            let direction = rotation.transform_dir(self.start.direction());
            let up = rotation.transform_dir(self.start.up());
            // Orbit position around centre: T(c)·R·T(-c) applied to start pos.
            let position =
                self.center + rotation.transform_dir(self.start.position() - self.center);
            (direction, up, position)
        };

        camera.extrinsic.set_orientation(Some(direction), Some(up));
        camera.extrinsic.set_position(position);
    }
}

/// Camera panning. Port of `CameraSelectPan`: a drag translates the camera so
/// the scene point on a fixed depth plane stays under the cursor. silx reads the
/// plane depth from the depth buffer under the press (`interaction.py:226-235`
/// `_pickNdcZGL(x, y)`); the widget supplies the same datum from the CPU pick
/// ([`crate::SceneWidget::pick`]'s `ndc_depth`), falling back to the far plane
/// (`z = 1`, what an empty depth buffer reads) on a miss.
#[derive(Clone, Copy, Debug)]
pub struct PanDrag {
    /// Last cursor position as NDC `(x, y, z)`; `z` is the fixed pan-plane depth.
    last: Vec3,
}

impl PanDrag {
    /// Begin a pan at window pixel `win`, with the pan plane at NDC depth
    /// `plane_ndc_z` — the picked depth under the press (silx
    /// `CameraSelectPan.beginDrag`: `ndcZ = _pickNdcZGL(x, y)`), or `1.0` (the
    /// far plane) when nothing was hit.
    pub fn begin(win: (f32, f32), size: (f32, f32), plane_ndc_z: f32) -> Self {
        let (nx, ny) = window_to_ndc(win, size);
        PanDrag {
            last: Vec3::new(nx, ny, plane_ndc_z),
        }
    }

    /// Apply the pan translation for the current cursor `win`.
    pub fn update(&mut self, camera: &mut Camera, win: (f32, f32), size: (f32, f32)) {
        let (nx, ny) = window_to_ndc(win, size);
        let cur = Vec3::new(nx, ny, self.last.z);
        camera.pan(self.last, cur);
        self.last = cur;
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::core::scene3d::camera::Camera;

    fn approx(a: f32, b: f32, eps: f32) {
        assert!((a - b).abs() < eps, "{a} != {b}");
    }

    fn test_camera() -> Camera {
        // Perspective at (0,0,5) looking down -z, up +y; square viewport.
        Camera::new(
            30.0,
            0.1,
            100.0,
            (300.0, 300.0),
            Vec3::new(0.0, 0.0, 5.0),
            Vec3::new(0.0, 0.0, -1.0),
            Vec3::new(0.0, 1.0, 0.0),
        )
    }

    #[test]
    fn window_to_ndc_maps_centre_and_corners() {
        let size = (200.0, 100.0);
        let (cx, cy) = window_to_ndc((100.0, 50.0), size);
        approx(cx, 0.0, 1e-6);
        approx(cy, 0.0, 1e-6);
        // Top-left pixel → (-1, +1); bottom-right → (+1, -1).
        let (lx, ly) = window_to_ndc((0.0, 0.0), size);
        approx(lx, -1.0, 1e-6);
        approx(ly, 1.0, 1e-6);
        let (rx, ry) = window_to_ndc((200.0, 100.0), size);
        approx(rx, 1.0, 1e-6);
        approx(ry, -1.0, 1e-6);
    }

    #[test]
    fn orbit_preserves_radius_and_keeps_looking_at_centre() {
        let mut camera = test_camera();
        let size = (300.0, 300.0);
        let center = Vec3::ZERO;
        let start_radius = camera.extrinsic.position().length();

        // Horizontal drag from centre, a quarter-width to the right.
        let drag = OrbitDrag::begin(&camera, (150.0, 150.0), center);
        drag.update(&mut camera, (225.0, 150.0), size);

        let pos = camera.extrinsic.position();
        let dir = camera.extrinsic.direction();
        // Radius to the centre of rotation is preserved.
        approx((pos - center).length(), start_radius, 1e-3);
        // The camera still looks at the centre: pos + dir*radius ≈ centre.
        let looked_at = pos + dir * start_radius;
        approx(looked_at.x, center.x, 1e-2);
        approx(looked_at.y, center.y, 1e-2);
        approx(looked_at.z, center.z, 1e-2);
        // The rotation actually happened (not the identity branch).
        assert!(pos.x.abs() > 0.1, "horizontal orbit should move x: {pos:?}");
        // Basis stays orthonormal.
        approx(dir.length(), 1.0, 1e-4);
        approx(camera.extrinsic.up().length(), 1.0, 1e-4);
    }

    #[test]
    fn orbit_zero_drag_is_identity() {
        let mut camera = test_camera();
        let before = camera.extrinsic.position();
        let drag = OrbitDrag::begin(&camera, (150.0, 150.0), Vec3::ZERO);
        drag.update(&mut camera, (150.0, 150.0), (300.0, 300.0));
        let after = camera.extrinsic.position();
        approx(after.x, before.x, 1e-6);
        approx(after.y, before.y, 1e-6);
        approx(after.z, before.z, 1e-6);
    }

    #[test]
    fn pan_keeps_grabbed_point_under_the_cursor() {
        let mut camera = test_camera();
        let size = (300.0, 300.0);

        // Pan plane depth = the scene centre's NDC z (a centre-depth anchor).
        let plane_z = camera.matrix().transform_point(Vec3::ZERO, true).z;
        let a = (120.0, 160.0);
        let b = (180.0, 130.0);
        let (nax, nay) = window_to_ndc(a, size);
        let (nbx, nby) = window_to_ndc(b, size);

        // The scene point under cursor A on the pan plane, before the drag.
        let inv0 = camera.matrix().inverse().expect("invertible");
        let p_a = inv0.transform_point(Vec3::new(nax, nay, plane_z), true);

        let mut pan = PanDrag::begin(a, size, plane_z);
        pan.update(&mut camera, b, size);

        // After the pan, P_A must project to cursor B (defining pan property).
        let projected = camera.matrix().transform_point(p_a, true);
        approx(projected.x, nbx, 1e-3);
        approx(projected.y, nby, 1e-3);
    }

    #[test]
    fn pan_anchored_at_picked_depth_tracks_the_picked_point() {
        // The pan plane sits at the *picked* geometry depth (silx
        // interaction.py:226-235), not the scene-centre depth: a point well off
        // the centre plane must track the cursor 1:1.
        let mut camera = test_camera();
        let size = (300.0, 300.0);

        // Picked object point off the centre plane (z = 2 is 3 units from the
        // camera at (0,0,5); the centre plane is 5 units away).
        let picked = Vec3::new(0.4, -0.3, 2.0);
        let picked_ndc = camera.matrix().transform_point(picked, true);

        // Grab exactly where the picked point projects, drag to B.
        let a_ndc = (picked_ndc.x, picked_ndc.y);
        let a_win = (
            (a_ndc.0 + 1.0) * 0.5 * size.0,
            (1.0 - a_ndc.1) * 0.5 * size.1,
        );
        let b_win = (a_win.0 + 60.0, a_win.1 - 45.0);
        let (nbx, nby) = window_to_ndc(b_win, size);

        let mut pan = PanDrag::begin(a_win, size, picked_ndc.z);
        pan.update(&mut camera, b_win, size);

        // The picked point itself must land under cursor B.
        let projected = camera.matrix().transform_point(picked, true);
        approx(projected.x, nbx, 1e-3);
        approx(projected.y, nby, 1e-3);
    }

    #[test]
    fn orbit_pivots_on_the_picked_point() {
        // With a picked anchor (silx orbitAroundCenter=False,
        // interaction.py:150-161), the orbit preserves the camera's distance to
        // the *picked* point and keeps looking at it — not at the scene centre.
        let mut camera = test_camera();
        let size = (300.0, 300.0);
        let pivot = Vec3::new(1.0, 0.5, 1.0); // off-centre picked object point
        let start_radius = (camera.extrinsic.position() - pivot).length();

        let drag = OrbitDrag::begin(&camera, (150.0, 150.0), pivot);
        drag.update(&mut camera, (210.0, 170.0), size);

        let pos = camera.extrinsic.position();
        // Radius to the picked pivot is preserved (the defining orbit property).
        approx((pos - pivot).length(), start_radius, 1e-3);
        // The rotation happened (the camera moved).
        assert!(
            (pos - Vec3::new(0.0, 0.0, 5.0)).length() > 0.1,
            "orbit should move the camera: {pos:?}"
        );
        // The distance to the *scene centre* is NOT preserved in general — the
        // pivot is what anchors the motion.
        let centre_radius = pos.length();
        assert!(
            (centre_radius - 5.0).abs() > 1e-3,
            "off-centre pivot must not orbit the origin (radius stayed {centre_radius})"
        );
    }
}
