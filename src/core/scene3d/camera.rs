//! Camera: projection (intrinsic) × position/orientation (extrinsic).
//!
//! A port of silx `silx.gui.plot3d.scene.camera`. silx caches matrices behind a
//! change-notifier; here every matrix is rebuilt on demand (a 4×4 build per
//! frame is negligible), so the observer machinery is dropped.

use super::mat4::{
    Mat4, Vec3, mat4_look_at_dir, mat4_orthographic, mat4_perspective, mat4_rotate, mat4_translate,
};

/// Rotation direction relative to the image plane, for [`CameraExtrinsic::orbit`]
/// and [`CameraExtrinsic::rotate`]. Mirrors silx's `'up'/'down'/'left'/'right'`.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum CameraDirection {
    Up,
    Down,
    Left,
    Right,
}

/// Translation direction relative to the image plane, for
/// [`CameraExtrinsic::move_to`]. Mirrors silx's six `move` directions.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum CameraMove {
    Up,
    Down,
    Left,
    Right,
    Forward,
    Backward,
}

/// Pre-defined camera orientations, for [`CameraExtrinsic::reset`]. Mirrors
/// `_RESET_CAMERA_ORIENTATIONS`.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum CameraFace {
    Side,
    Front,
    Back,
    Top,
    Bottom,
    Right,
    Left,
}

impl CameraFace {
    /// `(direction, up)` for this face, as in silx `_RESET_CAMERA_ORIENTATIONS`.
    fn direction_up(self) -> (Vec3, Vec3) {
        match self {
            CameraFace::Side => (Vec3::new(-1.0, -1.0, -1.0), Vec3::new(0.0, 1.0, 0.0)),
            CameraFace::Front => (Vec3::new(0.0, 0.0, -1.0), Vec3::new(0.0, 1.0, 0.0)),
            CameraFace::Back => (Vec3::new(0.0, 0.0, 1.0), Vec3::new(0.0, 1.0, 0.0)),
            CameraFace::Top => (Vec3::new(0.0, -1.0, 0.0), Vec3::new(0.0, 0.0, -1.0)),
            CameraFace::Bottom => (Vec3::new(0.0, 1.0, 0.0), Vec3::new(0.0, 0.0, 1.0)),
            CameraFace::Right => (Vec3::new(-1.0, 0.0, 0.0), Vec3::new(0.0, 1.0, 0.0)),
            CameraFace::Left => (Vec3::new(1.0, 0.0, 0.0), Vec3::new(0.0, 1.0, 0.0)),
        }
    }
}

/// Camera position and orientation (the view matrix). Port of `CameraExtrinsic`.
#[derive(Clone, Copy, Debug)]
pub struct CameraExtrinsic {
    position: Vec3,
    /// Normalized sight direction.
    direction: Vec3,
    /// Normalized up vector (orthogonal to `direction`).
    up: Vec3,
    /// Normalized side vector (`direction × up`).
    side: Vec3,
}

impl Default for CameraExtrinsic {
    fn default() -> Self {
        CameraExtrinsic::new(
            Vec3::ZERO,
            Vec3::new(0.0, 0.0, -1.0),
            Vec3::new(0.0, 1.0, 0.0),
        )
    }
}

impl CameraExtrinsic {
    pub fn new(position: Vec3, direction: Vec3, up: Vec3) -> Self {
        let mut e = CameraExtrinsic {
            position,
            direction: Vec3::new(0.0, 0.0, -1.0),
            up: Vec3::new(0.0, 1.0, 0.0),
            side: Vec3::new(1.0, 0.0, 0.0),
        };
        e.set_orientation(Some(direction), Some(up));
        e
    }

    /// Set the rotation of the point of view, re-deriving an orthonormal
    /// `(side, up, direction)` basis. `None` keeps the current vector.
    ///
    /// Returns `false` and leaves the orientation unchanged when `direction` and
    /// `up` are parallel (silx raises `RuntimeError`; siplot no-ops so the
    /// interactive widget cannot crash at the gimbal pole).
    pub fn set_orientation(&mut self, direction: Option<Vec3>, up: Option<Vec3>) -> bool {
        let direction = match direction {
            Some(d) => d.normalized(),
            None => self.direction,
        };
        let up = up.unwrap_or(self.up);

        let side = direction.cross(up);
        let sidenormal = side.length();
        if sidenormal == 0.0 {
            return false;
        }
        let side = side * (1.0 / sidenormal);
        let up = side.cross(direction).normalized();

        self.side = side;
        self.up = up;
        self.direction = direction;
        true
    }

    pub fn position(&self) -> Vec3 {
        self.position
    }

    pub fn set_position(&mut self, position: Vec3) {
        self.position = position;
    }

    pub fn direction(&self) -> Vec3 {
        self.direction
    }

    pub fn set_direction(&mut self, direction: Vec3) -> bool {
        self.set_orientation(Some(direction), None)
    }

    pub fn up(&self) -> Vec3 {
        self.up
    }

    pub fn set_up(&mut self, up: Vec3) -> bool {
        self.set_orientation(None, Some(up))
    }

    pub fn side(&self) -> Vec3 {
        self.side
    }

    /// The view matrix (`mat4LookAtDir`).
    pub fn matrix(&self) -> Mat4 {
        mat4_look_at_dir(self.position, self.direction, self.up)
    }

    /// Move the camera relative to the image plane. Port of `move`.
    pub fn move_to(&mut self, direction: CameraMove, step: f32) {
        let vector = match direction {
            CameraMove::Up => self.up,
            CameraMove::Down => -self.up,
            CameraMove::Right => self.side,
            CameraMove::Left => -self.side,
            CameraMove::Forward => self.direction,
            CameraMove::Backward => -self.direction,
        };
        self.position += vector * step;
    }

    /// First-person rotation toward `direction`. Port of `rotate`. `angle` is in
    /// degrees.
    pub fn rotate(&mut self, direction: CameraDirection, angle: f32) -> bool {
        let axis = match direction {
            CameraDirection::Up => self.side,
            CameraDirection::Down => -self.side,
            CameraDirection::Left => self.up,
            CameraDirection::Right => -self.up,
        };
        let m = mat4_rotate(angle.to_radians(), axis.x, axis.y, axis.z);
        let newdir = m.transform_dir(self.direction);
        match direction {
            CameraDirection::Up | CameraDirection::Down => {
                // Rotate up too so up and the new direction stay non-colinear.
                let newup = m.transform_dir(self.up);
                self.set_orientation(Some(newdir), Some(newup))
            }
            // Up is the rotation axis here, no need to rotate it.
            CameraDirection::Left | CameraDirection::Right => self.set_direction(newdir),
        }
    }

    /// Rotate the camera around `center`. Port of `orbit`. `angle` is in degrees.
    pub fn orbit(&mut self, direction: CameraDirection, center: Vec3, angle: f32) -> bool {
        let axis = match direction {
            CameraDirection::Down => self.side,
            CameraDirection::Up => -self.side,
            CameraDirection::Right => self.up,
            CameraDirection::Left => -self.up,
        };
        let rotmatrix = mat4_rotate(angle.to_radians(), axis.x, axis.y, axis.z);

        // Rotate the viewing direction first (recomputes side/up from old up).
        let newdir = rotmatrix.transform_dir(self.direction);
        if !self.set_direction(newdir) {
            return false;
        }

        // Rotate position around center: T(center) · R · T(-center).
        let matrix = mat4_translate(center.x, center.y, center.z)
            * rotmatrix
            * mat4_translate(-center.x, -center.y, -center.z);
        self.position = matrix.transform_point(self.position, false);
        true
    }

    /// Reset the camera to a pre-defined face, preserving its distance to the
    /// origin. Port of `reset`.
    pub fn reset(&mut self, face: CameraFace) {
        let distance = self.position.length();
        let (direction, up) = face.direction_up();
        self.set_orientation(Some(direction), Some(up));
        self.position = (-self.direction) * distance;
    }
}

/// `numpy.sign` for f32.
fn signum0(v: f32) -> f32 {
    if v > 0.0 {
        1.0
    } else if v < 0.0 {
        -1.0
    } else {
        0.0
    }
}

/// Orthographic projection with optional aspect-ratio preservation. Port of
/// `transform.Orthographic`.
#[derive(Clone, Copy, Debug)]
pub struct Orthographic {
    left: f32,
    right: f32,
    bottom: f32,
    top: f32,
    near: f32,
    far: f32,
    size: (f32, f32),
    keepaspect: bool,
}

impl Orthographic {
    /// `clip` is `[left, right, bottom, top]` (grouped to keep the constructor
    /// within the argument-count budget; it is the natural unit anyway — the four
    /// planes are always set together and adjusted as a group by `keepaspect`).
    pub fn new(clip: [f32; 4], near: f32, far: f32, size: (f32, f32), keepaspect: bool) -> Self {
        let [left, right, bottom, top] = clip;
        let mut o = Orthographic {
            left,
            right,
            bottom,
            top,
            near,
            far,
            size,
            keepaspect,
        };
        o.update(left, right, bottom, top);
        o
    }

    fn update(&mut self, mut left: f32, mut right: f32, mut bottom: f32, mut top: f32) {
        if self.keepaspect {
            let (width, height) = self.size;
            let aspect = width / height;
            let orthoaspect = (left - right).abs() / (bottom - top).abs();
            if orthoaspect >= aspect {
                // Keep width, enlarge height.
                let newheight = signum0(top - bottom) * (left - right).abs() / aspect;
                bottom = 0.5 * (bottom + top) - 0.5 * newheight;
                top = bottom + newheight;
            } else {
                // Keep height, enlarge width.
                let newwidth = signum0(right - left) * (bottom - top).abs() * aspect;
                left = 0.5 * (left + right) - 0.5 * newwidth;
                right = left + newwidth;
            }
        }
        self.left = left;
        self.right = right;
        self.bottom = bottom;
        self.top = top;
    }

    pub fn set_clipping(&mut self, left: f32, right: f32, bottom: f32, top: f32) {
        self.update(left, right, bottom, top);
    }

    pub fn set_size(&mut self, size: (f32, f32)) {
        if size != self.size {
            self.size = size;
            self.update(self.left, self.right, self.bottom, self.top);
        }
    }

    pub fn matrix(&self) -> Mat4 {
        mat4_orthographic(
            self.left,
            self.right,
            self.bottom,
            self.top,
            self.near,
            self.far,
        )
    }
}

/// Perspective projection by field-of-view + aspect. Port of
/// `transform.Perspective`.
#[derive(Clone, Copy, Debug)]
pub struct Perspective {
    fovy: f32,
    near: f32,
    far: f32,
    size: (f32, f32),
}

impl Perspective {
    pub fn new(fovy: f32, near: f32, far: f32, size: (f32, f32)) -> Self {
        Perspective {
            fovy,
            near,
            far,
            size,
        }
    }

    pub fn set_size(&mut self, size: (f32, f32)) {
        self.size = size;
    }

    pub fn matrix(&self) -> Mat4 {
        let (w, h) = self.size;
        mat4_perspective(self.fovy, w, h, self.near, self.far)
    }
}

/// Camera intrinsic: either perspective or orthographic projection.
#[derive(Clone, Copy, Debug)]
pub enum Projection {
    Perspective(Perspective),
    Orthographic(Orthographic),
}

impl Projection {
    pub fn matrix(&self) -> Mat4 {
        match self {
            Projection::Perspective(p) => p.matrix(),
            Projection::Orthographic(o) => o.matrix(),
        }
    }

    fn set_size(&mut self, size: (f32, f32)) {
        match self {
            Projection::Perspective(p) => p.set_size(size),
            Projection::Orthographic(o) => o.set_size(size),
        }
    }

    fn set_depth_extent(&mut self, near: f32, far: f32) {
        match self {
            Projection::Perspective(p) => {
                p.near = near;
                p.far = far;
            }
            Projection::Orthographic(o) => {
                o.near = near;
                o.far = far;
            }
        }
    }

    fn size(&self) -> (f32, f32) {
        match self {
            Projection::Perspective(p) => p.size,
            Projection::Orthographic(o) => o.size,
        }
    }
}

/// Combination of camera projection and extrinsic pose. Port of `Camera`.
#[derive(Clone, Copy, Debug)]
pub struct Camera {
    pub intrinsic: Projection,
    pub extrinsic: CameraExtrinsic,
}

impl Camera {
    /// A perspective camera, matching silx `Camera` defaults (fovy 30°).
    pub fn new(
        fovy: f32,
        near: f32,
        far: f32,
        size: (f32, f32),
        position: Vec3,
        direction: Vec3,
        up: Vec3,
    ) -> Self {
        Camera {
            intrinsic: Projection::Perspective(Perspective::new(fovy, near, far, size)),
            extrinsic: CameraExtrinsic::new(position, direction, up),
        }
    }

    /// The full clip-space matrix `projection · view`.
    pub fn matrix(&self) -> Mat4 {
        self.intrinsic.matrix() * self.extrinsic.matrix()
    }

    /// Update the viewport size used for the projection aspect ratio.
    pub fn set_size(&mut self, size: (f32, f32)) {
        self.intrinsic.set_size(size);
    }

    /// Position the camera so the axes-aligned `bounds` fit the frustum, and set
    /// the near/far depth extent to bracket them. Sight direction and up are
    /// preserved. Port of `Camera.resetCamera`.
    pub fn reset_camera(&mut self, bounds: (Vec3, Vec3)) {
        let (min, max) = bounds;
        let center = (min + max) * 0.5;
        let mut radius = ((max - min) * 0.5).length();
        if radius == 0.0 {
            radius = 1.0;
        }

        match &mut self.intrinsic {
            Projection::Perspective(p) => {
                let mut minfov = p.fovy.to_radians();
                let (width, height) = p.size;
                if width < height {
                    minfov *= width / height;
                }
                let offset = radius / (0.5 * minfov).sin();
                self.extrinsic.position = center - self.extrinsic.direction * offset;
                self.intrinsic
                    .set_depth_extent(offset - radius, offset + radius);
            }
            Projection::Orthographic(o) => {
                o.set_clipping(
                    center.x - radius,
                    center.x + radius,
                    center.y - radius,
                    center.y + radius,
                );
                self.extrinsic.position = Vec3::ZERO;
                self.intrinsic
                    .set_depth_extent(center.z - radius, center.z + radius);
            }
        }
    }

    /// Current viewport size used for the projection.
    pub fn size(&self) -> (f32, f32) {
        self.intrinsic.size()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn approx(a: f32, b: f32) {
        assert!((a - b).abs() < 1e-4, "{a} != {b}");
    }

    fn approx_vec(a: Vec3, b: Vec3) {
        approx(a.x, b.x);
        approx(a.y, b.y);
        approx(a.z, b.z);
    }

    #[test]
    fn extrinsic_builds_orthonormal_basis() {
        let e = CameraExtrinsic::new(
            Vec3::new(0.0, 0.0, 5.0),
            Vec3::new(0.0, 0.0, -1.0),
            Vec3::new(0.0, 1.0, 0.0),
        );
        // side = direction × up = (-z) × (y) = (0,0,-1)×(0,1,0) = (1,0,0)
        approx_vec(e.side(), Vec3::new(1.0, 0.0, 0.0));
        approx(e.direction().length(), 1.0);
        approx(e.up().length(), 1.0);
        // Orthogonality.
        approx(e.direction().dot(e.up()), 0.0);
        approx(e.direction().dot(e.side()), 0.0);
        approx(e.up().dot(e.side()), 0.0);
    }

    #[test]
    fn set_orientation_rejects_parallel() {
        let mut e = CameraExtrinsic::default();
        // direction == up → side is zero → rejected, state unchanged.
        let before = e.direction();
        assert!(!e.set_orientation(
            Some(Vec3::new(0.0, 1.0, 0.0)),
            Some(Vec3::new(0.0, 1.0, 0.0))
        ));
        approx_vec(e.direction(), before);
    }

    #[test]
    fn orbit_full_turn_returns_to_start() {
        let mut e = CameraExtrinsic::new(
            Vec3::new(0.0, 0.0, 5.0),
            Vec3::new(0.0, 0.0, -1.0),
            Vec3::new(0.0, 1.0, 0.0),
        );
        let start = e.position();
        for _ in 0..36 {
            assert!(e.orbit(CameraDirection::Left, Vec3::ZERO, 10.0));
        }
        approx_vec(e.position(), start);
    }

    #[test]
    fn orbit_left_90_moves_camera_to_side() {
        // Camera at +z looking -z. Orbit 'right' 90° about origin should swing
        // the camera onto the x axis (distance preserved).
        let mut e = CameraExtrinsic::new(
            Vec3::new(0.0, 0.0, 5.0),
            Vec3::new(0.0, 0.0, -1.0),
            Vec3::new(0.0, 1.0, 0.0),
        );
        assert!(e.orbit(CameraDirection::Right, Vec3::ZERO, 90.0));
        approx(e.position().length(), 5.0);
        // Still looking roughly at the origin.
        approx(e.direction().dot(e.position().normalized()), -1.0);
    }

    #[test]
    fn reset_top_looks_down() {
        let mut e = CameraExtrinsic::new(
            Vec3::new(0.0, 0.0, 10.0),
            Vec3::new(0.0, 0.0, -1.0),
            Vec3::new(0.0, 1.0, 0.0),
        );
        e.reset(CameraFace::Top);
        approx_vec(e.direction(), Vec3::new(0.0, -1.0, 0.0));
        // Distance preserved, camera above the origin.
        approx_vec(e.position(), Vec3::new(0.0, 10.0, 0.0));
    }

    #[test]
    fn reset_camera_perspective_frames_unit_box() {
        let mut cam = Camera::new(
            30.0,
            0.1,
            10.0,
            (100.0, 100.0),
            Vec3::ZERO,
            Vec3::new(0.0, 0.0, -1.0),
            Vec3::new(0.0, 1.0, 0.0),
        );
        let bounds = (Vec3::new(-1.0, -1.0, -1.0), Vec3::new(1.0, 1.0, 1.0));
        cam.reset_camera(bounds);

        // Camera pulled back along +z (opposite the -z sight direction).
        assert!(cam.extrinsic.position().z > 0.0);
        // The box center projects to the NDC origin (in front of the camera).
        let mvp = cam.matrix();
        let center = mvp.transform_point(Vec3::ZERO, true);
        approx(center.x, 0.0);
        approx(center.y, 0.0);
        // All 8 corners are within the clip cube after perspective divide.
        for &x in &[-1.0f32, 1.0] {
            for &y in &[-1.0f32, 1.0] {
                for &z in &[-1.0f32, 1.0] {
                    let ndc = mvp.transform_point(Vec3::new(x, y, z), true);
                    assert!(ndc.x.abs() <= 1.001, "x ndc {} out of range", ndc.x);
                    assert!(ndc.y.abs() <= 1.001, "y ndc {} out of range", ndc.y);
                    assert!(ndc.z.abs() <= 1.001, "z ndc {} out of range", ndc.z);
                }
            }
        }
    }

    #[test]
    fn reset_camera_collapsed_bounds_uses_unit_radius() {
        let mut cam = Camera::new(
            30.0,
            0.1,
            10.0,
            (100.0, 100.0),
            Vec3::ZERO,
            Vec3::new(0.0, 0.0, -1.0),
            Vec3::new(0.0, 1.0, 0.0),
        );
        // Degenerate (single point) bounds must not divide by zero.
        cam.reset_camera((Vec3::ZERO, Vec3::ZERO));
        assert!(cam.extrinsic.position().z.is_finite());
        assert!(cam.extrinsic.position().z > 0.0);
    }

    #[test]
    fn orthographic_keepaspect_enlarges_to_match_viewport() {
        // Square clip on a 2:1 viewport: keepaspect enlarges the x-range to
        // [-2, 2] so the unit square is not stretched. x=2 then maps to NDC 1.
        let o = Orthographic::new([-1.0, 1.0, -1.0, 1.0], -1.0, 1.0, (200.0, 100.0), true);
        let m = o.matrix();
        approx(m.transform_point(Vec3::new(2.0, 0.0, 0.0), false).x, 1.0);
        approx(m.transform_point(Vec3::new(0.0, 1.0, 0.0), false).y, 1.0);
    }

    #[test]
    fn reset_camera_orthographic_branch() {
        let mut cam = Camera {
            intrinsic: Projection::Orthographic(Orthographic::new(
                [-1.0, 1.0, -1.0, 1.0],
                -1.0,
                1.0,
                (100.0, 100.0),
                true,
            )),
            extrinsic: CameraExtrinsic::default(),
        };
        cam.reset_camera((Vec3::new(-1.0, -1.0, -1.0), Vec3::new(1.0, 1.0, 1.0)));
        // Orthographic reset places the camera at the origin.
        approx_vec(cam.extrinsic.position(), Vec3::ZERO);
        // The bounds center projects to the NDC origin.
        let ndc = cam.matrix().transform_point(Vec3::ZERO, false);
        approx(ndc.x, 0.0);
        approx(ndc.y, 0.0);
    }
}
