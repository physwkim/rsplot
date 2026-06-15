//! 4×4 matrix and 3-vector math for the 3D scene.
//!
//! A direct port of silx `silx.gui.plot3d.scene.transform` (the matrix
//! constructors) onto a hand-rolled `f32` linear-algebra type — no new
//! dependency, mirroring siplot's no-extra-crate stance for the 2D core.
//!
//! ## Conventions (must match silx for the port to stay verifiable)
//!
//! - [`Mat4`] stores its 16 elements **row-major** (`rows[r][c]`), exactly like
//!   the `numpy` arrays silx builds, and is applied to a *column* vector as
//!   `M·v` ([`Mat4::transform_point`]). Translation lives in the 4th column
//!   (`rows[i][3]`), as in `mat4Translate`.
//! - [`Mat4::mul`] is the standard matrix product, matching `numpy.dot(a, b)`.
//! - Angles passed to [`mat4_rotate`] are in **radians** (silx
//!   `mat4RotateFromAngleAxis` takes radians; the degree→radian conversion lives
//!   in the camera layer, as in silx).
//!
//! ## GPU boundary
//!
//! WGSL `mat4x4<f32>` is **column-major** and applies `M * v`, and wgpu clip
//! space is depth `z ∈ [0, 1]` whereas silx's projections (ported verbatim for
//! parity) target OpenGL's `z ∈ [-1, 1]`. Both conversions are isolated in
//! [`Mat4::to_gpu_cols`] / [`Mat4::to_gpu_clip_cols`] so the rest of the port
//! reads identically to the silx source.

/// A 3-component `f32` vector.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Vec3 {
    pub x: f32,
    pub y: f32,
    pub z: f32,
}

impl Vec3 {
    pub const ZERO: Vec3 = Vec3 {
        x: 0.0,
        y: 0.0,
        z: 0.0,
    };

    pub const fn new(x: f32, y: f32, z: f32) -> Self {
        Vec3 { x, y, z }
    }

    /// Build from an index-addressable triple (`[x, y, z]`).
    pub const fn from_array(a: [f32; 3]) -> Self {
        Vec3 {
            x: a[0],
            y: a[1],
            z: a[2],
        }
    }

    pub const fn to_array(self) -> [f32; 3] {
        [self.x, self.y, self.z]
    }

    pub fn dot(self, o: Vec3) -> f32 {
        self.x * o.x + self.y * o.y + self.z * o.z
    }

    /// Right-handed cross product `self × o`, matching `numpy.cross`.
    pub fn cross(self, o: Vec3) -> Vec3 {
        Vec3::new(
            self.y * o.z - self.z * o.y,
            self.z * o.x - self.x * o.z,
            self.x * o.y - self.y * o.x,
        )
    }

    /// Euclidean length (`numpy.linalg.norm`).
    pub fn length(self) -> f32 {
        self.dot(self).sqrt()
    }

    /// Unit vector. Returns the input unchanged when its length is zero (callers
    /// that require a non-zero vector assert separately, as silx does).
    pub fn normalized(self) -> Vec3 {
        let n = self.length();
        if n == 0.0 { self } else { self * (1.0 / n) }
    }
}

impl std::ops::Add for Vec3 {
    type Output = Vec3;
    fn add(self, o: Vec3) -> Vec3 {
        Vec3::new(self.x + o.x, self.y + o.y, self.z + o.z)
    }
}

impl std::ops::Sub for Vec3 {
    type Output = Vec3;
    fn sub(self, o: Vec3) -> Vec3 {
        Vec3::new(self.x - o.x, self.y - o.y, self.z - o.z)
    }
}

impl std::ops::Neg for Vec3 {
    type Output = Vec3;
    fn neg(self) -> Vec3 {
        Vec3::new(-self.x, -self.y, -self.z)
    }
}

impl std::ops::Mul<f32> for Vec3 {
    type Output = Vec3;
    /// Scalar multiply (replaces silx's `vector * scalar`).
    fn mul(self, s: f32) -> Vec3 {
        Vec3::new(self.x * s, self.y * s, self.z * s)
    }
}

impl std::ops::AddAssign for Vec3 {
    fn add_assign(&mut self, o: Vec3) {
        self.x += o.x;
        self.y += o.y;
        self.z += o.z;
    }
}

/// A row-major 4×4 `f32` matrix (see module docs for conventions).
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Mat4 {
    /// `rows[r][c]` — row `r`, column `c`.
    pub rows: [[f32; 4]; 4],
}

impl Mat4 {
    pub const IDENTITY: Mat4 = Mat4 {
        rows: [
            [1.0, 0.0, 0.0, 0.0],
            [0.0, 1.0, 0.0, 0.0],
            [0.0, 0.0, 1.0, 0.0],
            [0.0, 0.0, 0.0, 1.0],
        ],
    };

    pub const fn from_rows(rows: [[f32; 4]; 4]) -> Self {
        Mat4 { rows }
    }

    /// Apply the transform to a 3D point (`w = 1`), with optional perspective
    /// divide. Mirrors `Transform.transformPoint`.
    pub fn transform_point(&self, p: Vec3, perspective_divide: bool) -> Vec3 {
        let v = [p.x, p.y, p.z, 1.0];
        let mut r = [0.0f32; 4];
        for (i, ri) in r.iter_mut().enumerate() {
            *ri = (0..4).map(|j| self.rows[i][j] * v[j]).sum();
        }
        if perspective_divide && r[3] != 0.0 {
            Vec3::new(r[0] / r[3], r[1] / r[3], r[2] / r[3])
        } else {
            Vec3::new(r[0], r[1], r[2])
        }
    }

    /// Apply the linear (upper-left 3×3) part to a direction vector. Mirrors
    /// `Transform.transformDir`.
    pub fn transform_dir(&self, d: Vec3) -> Vec3 {
        let v = [d.x, d.y, d.z];
        let mut r = [0.0f32; 3];
        for (i, ri) in r.iter_mut().enumerate() {
            *ri = (0..3).map(|j| self.rows[i][j] * v[j]).sum();
        }
        Vec3::new(r[0], r[1], r[2])
    }

    /// Column-major copy for upload to a WGSL `mat4x4<f32>` (the transpose of the
    /// row-major storage). Use for model/view matrices that carry no projection.
    pub fn to_gpu_cols(&self) -> [[f32; 4]; 4] {
        let mut cols = [[0.0f32; 4]; 4];
        for (c, col) in cols.iter_mut().enumerate() {
            for (r, cell) in col.iter_mut().enumerate() {
                *cell = self.rows[r][c];
            }
        }
        cols
    }

    /// Column-major copy for upload, with the OpenGL→wgpu depth-range
    /// correction baked in. `self` is a full clip-space matrix `P·V·M` built from
    /// the verbatim silx projection (NDC `z ∈ [-1, 1]`); the returned matrix maps
    /// `z` into wgpu's `[0, 1]` via `C·self` where `C` rewrites the z-row to
    /// `0.5·zrow + 0.5·wrow`. Matrix multiplication is associative, so applying
    /// `C` to the whole product equals applying it to the projection alone.
    pub fn to_gpu_clip_cols(&self) -> [[f32; 4]; 4] {
        let mut corrected = *self;
        for c in 0..4 {
            corrected.rows[2][c] = 0.5 * self.rows[2][c] + 0.5 * self.rows[3][c];
        }
        corrected.to_gpu_cols()
    }
}

impl std::ops::Mul for Mat4 {
    type Output = Mat4;
    /// Standard matrix product `self · other` (matches `numpy.dot`).
    fn mul(self, other: Mat4) -> Mat4 {
        let mut out = [[0.0f32; 4]; 4];
        for (i, out_row) in out.iter_mut().enumerate() {
            for (j, out_cell) in out_row.iter_mut().enumerate() {
                let mut acc = 0.0;
                for k in 0..4 {
                    acc += self.rows[i][k] * other.rows[k][j];
                }
                *out_cell = acc;
            }
        }
        Mat4 { rows: out }
    }
}

// Matrix constructors — direct ports of silx `transform.py` module functions.

/// `mat4Translate`: 4×4 translation matrix.
pub fn mat4_translate(tx: f32, ty: f32, tz: f32) -> Mat4 {
    Mat4::from_rows([
        [1.0, 0.0, 0.0, tx],
        [0.0, 1.0, 0.0, ty],
        [0.0, 0.0, 1.0, tz],
        [0.0, 0.0, 0.0, 1.0],
    ])
}

/// `mat4Scale`: 4×4 scale matrix.
pub fn mat4_scale(sx: f32, sy: f32, sz: f32) -> Mat4 {
    Mat4::from_rows([
        [sx, 0.0, 0.0, 0.0],
        [0.0, sy, 0.0, 0.0],
        [0.0, 0.0, sz, 0.0],
        [0.0, 0.0, 0.0, 1.0],
    ])
}

/// `mat4RotateFromAngleAxis`: 4×4 rotation from `angle` (radians) about an axis
/// `(x, y, z)`. The axis is assumed normalized by the caller, as in silx (the
/// camera layer normalizes before calling).
pub fn mat4_rotate(angle: f32, x: f32, y: f32, z: f32) -> Mat4 {
    let ca = angle.cos();
    let sa = angle.sin();
    let omca = 1.0 - ca;
    Mat4::from_rows([
        [
            omca * x * x + ca,
            omca * x * y - sa * z,
            omca * x * z + sa * y,
            0.0,
        ],
        [
            omca * x * y + sa * z,
            omca * y * y + ca,
            omca * y * z - sa * x,
            0.0,
        ],
        [
            omca * x * z - sa * y,
            omca * y * z + sa * x,
            omca * z * z + ca,
            0.0,
        ],
        [0.0, 0.0, 0.0, 1.0],
    ])
}

/// `mat4LookAtDir`: view matrix looking in `direction` from `position` with the
/// given `up`. Panics (debug) on a zero direction / parallel up, matching silx's
/// asserts.
pub fn mat4_look_at_dir(position: Vec3, direction: Vec3, up: Vec3) -> Mat4 {
    let dirnorm = direction.length();
    debug_assert!(dirnorm != 0.0, "look-at direction must be non-zero");
    let direction = direction * (1.0 / dirnorm);

    let side = direction.cross(up);
    let sidenorm = side.length();
    debug_assert!(sidenorm != 0.0, "look-at direction and up are parallel");
    let up = (side * (1.0 / sidenorm)).cross(direction).normalized();

    // Rotation-only matrix: rows are side / up / -direction.
    let rot = Mat4::from_rows([
        [side.x, side.y, side.z, 0.0],
        [up.x, up.y, up.z, 0.0],
        [-direction.x, -direction.y, -direction.z, 0.0],
        [0.0, 0.0, 0.0, 1.0],
    ]);
    rot * mat4_translate(-position.x, -position.y, -position.z)
}

/// `mat4Perspective`: perspective projection (gluPerspective-like). `fovy` is in
/// degrees. Produces an OpenGL clip-space matrix (NDC `z ∈ [-1, 1]`).
pub fn mat4_perspective(fovy: f32, width: f32, height: f32, near: f32, far: f32) -> Mat4 {
    debug_assert!(fovy != 0.0 && width != 0.0 && height != 0.0);
    debug_assert!(near > 0.0 && far > near);
    let aspect = width / height;
    let f = 1.0 / (fovy.to_radians() / 2.0).tan();
    Mat4::from_rows([
        [f / aspect, 0.0, 0.0, 0.0],
        [0.0, f, 0.0, 0.0],
        [
            0.0,
            0.0,
            (far + near) / (near - far),
            2.0 * far * near / (near - far),
        ],
        [0.0, 0.0, -1.0, 0.0],
    ])
}

/// `mat4Orthographic`: orthographic projection (glOrtho-like). Produces an
/// OpenGL clip-space matrix (NDC `z ∈ [-1, 1]`).
pub fn mat4_orthographic(
    left: f32,
    right: f32,
    bottom: f32,
    top: f32,
    near: f32,
    far: f32,
) -> Mat4 {
    Mat4::from_rows([
        [
            2.0 / (right - left),
            0.0,
            0.0,
            -(right + left) / (right - left),
        ],
        [
            0.0,
            2.0 / (top - bottom),
            0.0,
            -(top + bottom) / (top - bottom),
        ],
        [0.0, 0.0, -2.0 / (far - near), -(far + near) / (far - near)],
        [0.0, 0.0, 0.0, 1.0],
    ])
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
    fn cross_is_right_handed() {
        // x × y = z
        approx_vec(
            Vec3::new(1.0, 0.0, 0.0).cross(Vec3::new(0.0, 1.0, 0.0)),
            Vec3::new(0.0, 0.0, 1.0),
        );
    }

    #[test]
    fn identity_is_multiplicative_unit() {
        let m = mat4_translate(3.0, -2.0, 5.0);
        assert_eq!(Mat4::IDENTITY * m, m);
        assert_eq!(m * Mat4::IDENTITY, m);
    }

    #[test]
    fn translate_moves_point() {
        let m = mat4_translate(1.0, 2.0, 3.0);
        approx_vec(
            m.transform_point(Vec3::new(10.0, 20.0, 30.0), false),
            Vec3::new(11.0, 22.0, 33.0),
        );
    }

    #[test]
    fn scale_scales_point() {
        let m = mat4_scale(2.0, 3.0, 4.0);
        approx_vec(
            m.transform_point(Vec3::new(1.0, 1.0, 1.0), false),
            Vec3::new(2.0, 3.0, 4.0),
        );
    }

    #[test]
    fn rotate_90_about_z_maps_x_to_y() {
        let m = mat4_rotate(std::f32::consts::FRAC_PI_2, 0.0, 0.0, 1.0);
        approx_vec(
            m.transform_point(Vec3::new(1.0, 0.0, 0.0), false),
            Vec3::new(0.0, 1.0, 0.0),
        );
    }

    #[test]
    fn rotate_direction_ignores_translation_column() {
        // A rotation matrix has no translation; transform_dir == transform_point
        // for the linear part. Check the 3×3 path explicitly.
        let m = mat4_rotate(std::f32::consts::FRAC_PI_2, 0.0, 1.0, 0.0);
        // 90° about +y maps +z → +x.
        approx_vec(
            m.transform_dir(Vec3::new(0.0, 0.0, 1.0)),
            Vec3::new(1.0, 0.0, 0.0),
        );
    }

    #[test]
    fn look_at_dir_puts_camera_at_origin_of_view_space() {
        // Camera at (0,0,5), looking toward -z, up +y. The world origin should
        // land at (0,0,-5) in view space (5 units in front along view -z).
        let view = mat4_look_at_dir(
            Vec3::new(0.0, 0.0, 5.0),
            Vec3::new(0.0, 0.0, -1.0),
            Vec3::new(0.0, 1.0, 0.0),
        );
        approx_vec(
            view.transform_point(Vec3::ZERO, false),
            Vec3::new(0.0, 0.0, -5.0),
        );
        // The camera position maps to the view-space origin.
        approx_vec(
            view.transform_point(Vec3::new(0.0, 0.0, 5.0), false),
            Vec3::ZERO,
        );
    }

    #[test]
    fn orthographic_maps_box_to_ndc_cube() {
        let m = mat4_orthographic(-2.0, 2.0, -2.0, 2.0, -2.0, 2.0);
        // Corner (right, top, far-as-negative-z) → NDC (1, 1, ?).
        approx_vec(
            m.transform_point(Vec3::new(2.0, 2.0, 0.0), false),
            Vec3::new(1.0, 1.0, 0.0),
        );
        approx_vec(
            m.transform_point(Vec3::new(-2.0, -2.0, 0.0), false),
            Vec3::new(-1.0, -1.0, 0.0),
        );
    }

    #[test]
    fn perspective_matches_silx_values() {
        // fovy=30, square 1×1 viewport, near=0.1 far=10.
        let m = mat4_perspective(30.0, 1.0, 1.0, 0.1, 10.0);
        let f = 1.0 / (30.0f32.to_radians() / 2.0).tan();
        approx(m.rows[0][0], f);
        approx(m.rows[1][1], f);
        approx(m.rows[2][2], (10.0 + 0.1) / (0.1 - 10.0));
        approx(m.rows[2][3], 2.0 * 10.0 * 0.1 / (0.1 - 10.0));
        approx(m.rows[3][2], -1.0);
        approx(m.rows[3][3], 0.0);
    }

    #[test]
    fn gpu_clip_correction_maps_minus_one_to_zero() {
        // An OpenGL ortho maps near→-1, far→+1 in NDC z. After the wgpu clip
        // correction, near→0, far→1.
        let m = mat4_orthographic(-1.0, 1.0, -1.0, 1.0, 0.0, 10.0);
        // A clip-space point at near plane (z corresponds to world z=0).
        let near_ndc_z = m.transform_point(Vec3::new(0.0, 0.0, 0.0), false).z; // GL NDC
        approx(near_ndc_z, -1.0);

        // Apply the same correction the GPU helper does and check 0.
        let cols = m.to_gpu_clip_cols();
        // cols is column-major: corrected z for world (0,0,0,1) is
        // sum over columns of col[c].z * v[c]; v = (0,0,0,1) → col[3].z.
        approx(cols[3][2], 0.0);

        // Far plane world z = -10 (looking down -z), maps to +1 in GL, → 1 wgpu.
        // Build the world point (0,0,-10): corrected z = col2.z*(-10) + col3.z.
        let z_far = cols[2][2] * (-10.0) + cols[3][2];
        approx(z_far, 1.0);
    }

    #[test]
    fn to_gpu_cols_is_transpose() {
        let m = mat4_translate(1.0, 2.0, 3.0);
        let cols = m.to_gpu_cols();
        // Row-major translation has tx at rows[0][3]; column-major stores it at
        // cols[3][0].
        approx(cols[3][0], 1.0);
        approx(cols[3][1], 2.0);
        approx(cols[3][2], 3.0);
    }
}
