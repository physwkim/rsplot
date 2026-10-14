//! Marching-cubes isosurface extraction — the pure, headless port of silx's
//! `silx.math.marchingcubes` (C++ `mc.hpp` + the 256-case lookup tables in
//! `mc_lut.cpp`, driven by Cython `marchingcubes.pyx`).
//!
//! Given a 3D scalar field on a regular grid and an iso-level, [`MarchingCubes`]
//! builds the triangulated iso-surface: per-vertex positions, coarse gradient
//! normals, and triangle indices. The dimension convention matches silx exactly:
//! the field is `(depth, height, width)` with `width` contiguous in memory, and
//! the output vertices/normals are stored as `(z, y, x)` / `(nz, ny, nx)` — the
//! same order as the input array. The consumer ([`crate::render::scene3d_items`]
//! `ScalarField3D`) is responsible for the `zyx → xyz` axis swap and the +0.5
//! cell-centre offset that silx applies via the `_isogroup` transform.
//!
//! This is a line-for-line port of the C++ slice-by-slice algorithm (process two
//! consecutive slices at a time, caching the edge→vertex map of the previous
//! slice) so the vertex ordering, interpolation, and normal estimation are
//! identical to silx's. `sampling` (per-dimension stride) is carried through
//! because it is part of the silx API and entangled with the index math; the
//! `ScalarField3D` isosurface path uses the default `(1, 1, 1)`.

use std::collections::HashMap;

/// Dimension index of `depth` (dim 0) — matches `DEPTH_IDX` in `mc.hpp`.
const DEPTH_IDX: usize = 0;
/// Dimension index of `height` (dim 1) — matches `HEIGHT_IDX` in `mc.hpp`.
const HEIGHT_IDX: usize = 1;
/// Dimension index of `width` (dim 2, contiguous) — matches `WIDTH_IDX`.
const WIDTH_IDX: usize = 2;

/// Failure modes of [`MarchingCubes::process`].
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum MarchingCubesError {
    /// `data.len()` does not equal `depth * height * width`.
    ShapeMismatch {
        /// Number of elements actually supplied.
        got: usize,
        /// Number of elements expected (`depth * height * width`).
        expected: usize,
    },
    /// An internal invariant broke: a triangle referenced an edge whose vertex
    /// was never registered. Mirrors silx's `std::runtime_error` ("cannot build
    /// triangle indices"); should never occur for well-formed input.
    TriangleIndex,
}

impl std::fmt::Display for MarchingCubesError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            MarchingCubesError::ShapeMismatch { got, expected } => write!(
                f,
                "marching cubes: data length {got} does not match depth*height*width {expected}"
            ),
            MarchingCubesError::TriangleIndex => {
                f.write_str("marching cubes: internal error, cannot build triangle indices")
            }
        }
    }
}

impl std::error::Error for MarchingCubesError {}

/// Compute the marching-cubes iso-surface of a `(depth, height, width)` scalar
/// field in one call.
///
/// Returns `(vertices, normals, indices)` where `vertices`/`normals` are
/// `(z, y, x)` / `(nz, ny, nx)` triples and `indices` are flat triangle vertex
/// indices (3 per triangle), or `None` when the surface is empty (no edge
/// crosses the level) or the data shape is inconsistent. `invert_normals = true`
/// orients normals along gradient descent (silx's default).
#[allow(clippy::type_complexity)]
pub fn isosurface(
    data: &[f32],
    depth: usize,
    height: usize,
    width: usize,
    isolevel: f32,
    invert_normals: bool,
) -> Option<(Vec<[f32; 3]>, Vec<[f32; 3]>, Vec<u32>)> {
    let mut mc = MarchingCubes::new(isolevel);
    mc.invert_normals = invert_normals;
    mc.process(data, depth, height, width).ok()?;
    if mc.vertices.is_empty() {
        return None;
    }
    Some((mc.vertices_zyx(), mc.normals_zyx(), mc.indices.clone()))
}

/// Marching-cubes processor. Port of `MarchingCubes<float, float>` (`mc.hpp`).
///
/// Either call [`process`](Self::process) with a full 3D array, or drive it
/// slice-by-slice with [`process_slice`](Self::process_slice) (bracketed by
/// reading [`vertices`](Self::vertices) etc. and [`reset`](Self::reset)).
#[derive(Clone, Debug)]
pub struct MarchingCubes {
    /// Iso-surface vertices, flat `(z, y, x)` triples.
    pub vertices: Vec<f32>,
    /// Coarse gradient normals at the vertices, flat `(nz, ny, nx)` triples.
    pub normals: Vec<f32>,
    /// Triangle vertex indices (3 per triangle).
    pub indices: Vec<u32>,
    /// Number of slices processed so far (the running depth coordinate).
    pub depth: usize,
    /// Slice height in pixels.
    pub height: usize,
    /// Slice width in pixels (contiguous dimension).
    pub width: usize,
    /// Per-dimension sampling stride `(depth, height, width)`; default `(1,1,1)`.
    pub sampling: [usize; 3],
    /// Iso-level at which to build the surface.
    pub isolevel: f32,
    /// `true` to orient normals along gradient descent (negate the gradient).
    pub invert_normals: bool,
    /// Edge-index → vertex-index cache for the slice currently being built.
    /// `None` between processing runs (matches the C++ null pointer that
    /// bootstraps `first_slice`).
    edge_indices: Option<HashMap<usize, usize>>,
}

impl MarchingCubes {
    /// Create a processor for the given iso-level (silx default sampling
    /// `(1,1,1)`, `invert_normals = true`).
    pub fn new(isolevel: f32) -> Self {
        Self {
            vertices: Vec::new(),
            normals: Vec::new(),
            indices: Vec::new(),
            depth: 0,
            height: 0,
            width: 0,
            sampling: [1, 1, 1],
            isolevel,
            invert_normals: true,
            edge_indices: None,
        }
    }

    /// Builder: set the per-dimension sampling stride `(depth, height, width)`.
    pub fn with_sampling(mut self, sampling: [usize; 3]) -> Self {
        self.sampling = sampling;
        self
    }

    /// Builder: set whether normals follow gradient descent (default `true`).
    pub fn with_invert_normals(mut self, invert: bool) -> Self {
        self.invert_normals = invert;
        self
    }

    /// Reset all computed data and counters (port of `reset`).
    pub fn reset(&mut self) {
        self.depth = 0;
        self.vertices.clear();
        self.normals.clear();
        self.indices.clear();
        self.edge_indices = None;
    }

    /// Set the slice dimensions and reset (port of `set_slice_size`).
    pub fn set_slice_size(&mut self, height: usize, width: usize) {
        self.reset();
        self.height = height;
        self.width = width;
    }

    /// Vertices as `(z, y, x)` triples.
    pub fn vertices_zyx(&self) -> Vec<[f32; 3]> {
        self.vertices
            .as_chunks::<3>()
            .0
            .iter()
            .map(|c| [c[0], c[1], c[2]])
            .collect()
    }

    /// Normals as `(nz, ny, nx)` triples.
    pub fn normals_zyx(&self) -> Vec<[f32; 3]> {
        self.normals
            .as_chunks::<3>()
            .0
            .iter()
            .map(|c| [c[0], c[1], c[2]])
            .collect()
    }

    /// Process a full 3D scalar field `(depth, height, width)` (port of
    /// `process`). `data` is row-major with `width` contiguous.
    pub fn process(
        &mut self,
        data: &[f32],
        depth: usize,
        height: usize,
        width: usize,
    ) -> Result<(), MarchingCubesError> {
        let expected = depth.saturating_mul(height).saturating_mul(width);
        if data.len() != expected {
            return Err(MarchingCubesError::ShapeMismatch {
                got: data.len(),
                expected,
            });
        }

        self.reset();
        self.set_slice_size(height, width);

        let hw = height * width;
        let size = hw * self.sampling[DEPTH_IDX];
        // Number of slice pairs to process.
        let nb_slices = if depth == 0 {
            0
        } else {
            (depth - 1) / self.sampling[DEPTH_IDX]
        };

        for index in 0..nb_slices {
            let base = index * size;
            let slice0 = &data[base..base + hw];
            let slice1 = &data[base + size..base + size + hw];
            self.process_slice(slice0, slice1)?;
        }
        self.finish_process();

        self.depth = depth; // Forced, as it might be < depth otherwise.
        Ok(())
    }

    /// Clear the slice cache after slice-by-slice processing (port of
    /// `finish_process`).
    pub fn finish_process(&mut self) {
        self.edge_indices = None;
    }

    /// Process two consecutive slices (port of `process_slice`). The `slice1` of
    /// one call must be passed as `slice0` of the next.
    pub fn process_slice(
        &mut self,
        slice0: &[f32],
        slice1: &[f32],
    ) -> Result<(), MarchingCubesError> {
        if self.edge_indices.is_none() {
            // No previously processed slice — bootstrap.
            self.first_slice(slice0, slice1);
        }

        // Keep the previous slice's cache, start a fresh one for this slice.
        let previous_edge_indices = self.edge_indices.take().unwrap();
        self.edge_indices = Some(HashMap::new());

        let (sd, sh, sw) = (
            self.sampling[DEPTH_IDX],
            self.sampling[HEIGHT_IDX],
            self.sampling[WIDTH_IDX],
        );
        let (height, width) = (self.height, self.width);

        // Loop over the slice to add vertices in the upper (slice1) plane and the
        // z-direction edges.
        let mut row = 0;
        while row < height {
            let line_index = row * width;
            let mut col = 0;
            while col < width {
                let item_index = line_index + col;
                let value0 = slice1[item_index];

                // Forward edges in the current slice plane.
                if col < width - sw {
                    let value = slice1[item_index + sw];
                    self.process_edge(
                        value0,
                        value,
                        self.depth,
                        row,
                        col,
                        0,
                        Some(slice0),
                        slice1,
                        None,
                    );
                }
                if row < height - sh {
                    let value = slice1[item_index + width * sh];
                    self.process_edge(
                        value0,
                        value,
                        self.depth,
                        row,
                        col,
                        1,
                        Some(slice0),
                        slice1,
                        None,
                    );
                }

                // Backward edge in the z direction.
                {
                    let value = slice0[item_index];
                    // Expect a forward edge, so pass: previous, current.
                    self.process_edge(
                        value,
                        value0,
                        self.depth - sd,
                        row,
                        col,
                        2,
                        None,
                        slice0,
                        Some(slice1),
                    );
                }

                col += sw;
            }
            row += sh;
        }

        // Loop over the cubes to add triangle indices.
        let mut row = 0;
        while row < height - sh {
            let mut col = 0;
            while col < width - sw {
                let code = self.get_cell_code(slice0, slice1, row, col);
                if code != 0 {
                    for &edge in &MC_TRIANGLE_TABLE[code as usize] {
                        if edge < 0 {
                            break;
                        }
                        let offsets = &MC_EDGE_INDEX_TO_COORD_OFFSETS[edge as usize];
                        let ei = self.edge_index(
                            self.depth - sd + offsets[DEPTH_IDX] * sd,
                            row + offsets[HEIGHT_IDX] * sh,
                            col + offsets[WIDTH_IDX] * sw,
                            offsets[3],
                        );

                        // Lower-plane x/y edges live in the previous slice cache;
                        // upper-plane and z edges in the current cache.
                        let vidx = if offsets[DEPTH_IDX] == 0 && offsets[3] != 2 {
                            previous_edge_indices.get(&ei).copied()
                        } else {
                            self.edge_indices.as_ref().unwrap().get(&ei).copied()
                        };
                        match vidx {
                            Some(v) => self.indices.push(v as u32),
                            None => return Err(MarchingCubesError::TriangleIndex),
                        }
                    }
                }
                col += sw;
            }
            row += sh;
        }

        self.depth += sd;
        Ok(())
    }

    /// Bootstrap the first slice's edge cache (port of `first_slice`): add the
    /// iso-surface vertices in this slice's plane (x and y edges only).
    fn first_slice(&mut self, slice: &[f32], next: &[f32]) {
        self.edge_indices = Some(HashMap::new());

        let (sh, sw) = (self.sampling[HEIGHT_IDX], self.sampling[WIDTH_IDX]);
        let (height, width) = (self.height, self.width);

        let mut row = 0;
        while row < height {
            let line_index = row * width;
            let mut col = 0;
            while col < width {
                let item_index = line_index + col;
                let value0 = slice[item_index];

                if col < width - sw {
                    let value = slice[item_index + sw];
                    self.process_edge(
                        value0,
                        value,
                        self.depth,
                        row,
                        col,
                        0,
                        None,
                        slice,
                        Some(next),
                    );
                }
                if row < height - sh {
                    let value = slice[item_index + width * sh];
                    self.process_edge(
                        value0,
                        value,
                        self.depth,
                        row,
                        col,
                        1,
                        None,
                        slice,
                        Some(next),
                    );
                }

                col += sw;
            }
            row += sh;
        }

        self.depth += self.sampling[DEPTH_IDX];
    }

    /// Linearized 4D edge index (port of `edge_index`): position in a grid of
    /// `(height+1) × (width+1)` per slice, times 3 directions plus the direction.
    #[inline]
    fn edge_index(&self, depth: usize, row: usize, col: usize, direction: usize) -> usize {
        ((depth * (self.height + 1) + row) * (self.width + 1) + col) * 3 + direction
    }

    /// Process one edge (port of `process_edge`): if the iso-level crosses it,
    /// add the interpolated vertex and its coarse gradient normal.
    #[allow(clippy::too_many_arguments)]
    fn process_edge(
        &mut self,
        value0: f32,
        value: f32,
        depth: usize,
        row: usize,
        col: usize,
        direction: usize,
        previous: Option<&[f32]>,
        current: &[f32],
        next: Option<&[f32]>,
    ) {
        // Crossing test: exactly one endpoint is <= isolevel.
        if (value0 <= self.isolevel) == (value <= self.isolevel) {
            return;
        }

        let offset = (self.isolevel - value0) / (value - value0);

        let (sd, sh, sw) = (
            self.sampling[DEPTH_IDX],
            self.sampling[HEIGHT_IDX],
            self.sampling[WIDTH_IDX],
        );

        // Store edge → vertex-index correspondence.
        let ei = self.edge_index(depth, row, col, direction);
        let vidx = self.vertices.len() / 3;
        self.edge_indices.as_mut().unwrap().insert(ei, vidx);

        // Store the vertex as (z, y, x).
        match direction {
            0 => {
                self.vertices.push(depth as f32);
                self.vertices.push(row as f32);
                self.vertices.push(col as f32 + offset * sw as f32);
            }
            1 => {
                self.vertices.push(depth as f32);
                self.vertices.push(row as f32 + offset * sh as f32);
                self.vertices.push(col as f32);
            }
            _ => {
                self.vertices.push(depth as f32 + offset * sd as f32);
                self.vertices.push(row as f32);
                self.vertices.push(col as f32);
            }
        }

        // Coarse gradient normal as (nz, ny, nx).
        let slice0 = previous.unwrap_or(current);
        let slice1 = if previous.is_some() {
            current
        } else {
            next.unwrap_or(current)
        };
        let width = self.width;
        let height = self.height;
        let row_offset = width * sh;

        let (mut nz, mut ny, mut nx) = if direction == 0 {
            let nz = {
                let mut item = row * width + col;
                if col >= width - sw {
                    item -= sw; // For the last column, use the previous column.
                }
                let item_next_col = item + sw;
                (1.0 - offset) * (slice1[item] - slice0[item])
                    + offset * (slice1[item_next_col] - slice0[item_next_col])
            };
            let ny = {
                let mut item = row * width + col;
                if row >= height - sh {
                    item -= row_offset; // For the last row, use the previous row.
                }
                if col >= width - sw {
                    item -= sw;
                }
                let item_next_col = item + sw;
                (1.0 - offset) * (current[item + row_offset] - current[item])
                    + offset * (current[item_next_col + row_offset] - current[item_next_col])
            };
            let nx = value - value0;
            (nz, ny, nx)
        } else if direction == 1 {
            let nz = {
                let mut item = row * width + col;
                if row >= height - sh {
                    item -= row_offset;
                }
                let item_next_row = item + row_offset;
                (1.0 - offset) * (slice1[item] - slice0[item])
                    + offset * (slice1[item_next_row] - slice0[item_next_row])
            };
            let ny = value - value0;
            let nx = {
                let mut item = row * width + col;
                if row >= height - sh {
                    item -= row_offset;
                }
                if col >= width - sw {
                    item -= sw;
                }
                let item_next_row = item + row_offset;
                (1.0 - offset) * (current[item + sw] - current[item])
                    + offset * (current[item_next_row + sw] - current[item_next_row])
            };
            (nz, ny, nx)
        } else {
            // direction == 2
            // previous is always None here; kept for parity with the C++ guard.
            let other_slice = previous.or(next).unwrap_or(current);
            let nz = value - value0;
            let ny = {
                let mut item = row * width + col;
                if row >= height - sh {
                    item -= row_offset;
                }
                let item_next_row = item + row_offset;
                (1.0 - offset) * (current[item_next_row] - current[item])
                    + offset * (other_slice[item_next_row] - other_slice[item])
            };
            let nx = {
                let mut item = row * width + col;
                if col >= width - sw {
                    item -= sw;
                }
                let item_next_col = item + sw;
                (1.0 - offset) * (current[item_next_col] - current[item])
                    + offset * (other_slice[item_next_col] - other_slice[item])
            };
            (nz, ny, nx)
        };

        // Apply sampling scaling.
        nz /= sd as f32;
        ny /= sh as f32;
        nx /= sw as f32;

        // Normalisation (with optional inversion via a negated norm).
        let mut norm = (nz * nz + ny * ny + nx * nx).sqrt();
        if self.invert_normals {
            norm = -norm;
        }
        if norm != 0.0 {
            nz /= norm;
            ny /= norm;
            nx /= norm;
        }

        self.normals.push(nz);
        self.normals.push(ny);
        self.normals.push(nx);
    }

    /// Bit mask of the cube's corners `<= isolevel` (port of `get_cell_code`).
    /// `slice1`/`slice2` are the lower/upper data slices of the cube.
    fn get_cell_code(&self, slice1: &[f32], slice2: &[f32], row: usize, col: usize) -> u8 {
        let sh = self.sampling[HEIGHT_IDX];
        let sw = self.sampling[WIDTH_IDX];
        let item = row * self.width + col;
        let item_next_row = item + self.width * sh;
        let mut code: u8 = 0;

        // First (lower) slice.
        if slice1[item] <= self.isolevel {
            code |= 1 << 0;
        }
        if slice1[item + sw] <= self.isolevel {
            code |= 1 << 1;
        }
        if slice1[item_next_row + sw] <= self.isolevel {
            code |= 1 << 2;
        }
        if slice1[item_next_row] <= self.isolevel {
            code |= 1 << 3;
        }

        // Second (upper) slice.
        if slice2[item] <= self.isolevel {
            code |= 1 << 4;
        }
        if slice2[item + sw] <= self.isolevel {
            code |= 1 << 5;
        }
        if slice2[item_next_row + sw] <= self.isolevel {
            code |= 1 << 6;
        }
        if slice2[item_next_row] <= self.isolevel {
            code |= 1 << 7;
        }

        code
    }
}

/// Triangle edge lists for each of the 256 cube codes (port of
/// `MCTriangleTable` in `mc_lut.cpp`; table from
/// <http://paulbourke.net/geometry/polygonise/>, author Cory Bloyd, MIT).
/// A `-1` entry terminates the list for that code.
#[rustfmt::skip]
const MC_TRIANGLE_TABLE: [[i32; 16]; 256] = [
    [-1, -1, -1, -1, -1, -1, -1, -1, -1, -1, -1, -1, -1, -1, -1, -1],
    [0, 8, 3, -1, -1, -1, -1, -1, -1, -1, -1, -1, -1, -1, -1, -1],
    [0, 1, 9, -1, -1, -1, -1, -1, -1, -1, -1, -1, -1, -1, -1, -1],
    [1, 8, 3, 9, 8, 1, -1, -1, -1, -1, -1, -1, -1, -1, -1, -1],
    [1, 2, 10, -1, -1, -1, -1, -1, -1, -1, -1, -1, -1, -1, -1, -1],
    [0, 8, 3, 1, 2, 10, -1, -1, -1, -1, -1, -1, -1, -1, -1, -1],
    [9, 2, 10, 0, 2, 9, -1, -1, -1, -1, -1, -1, -1, -1, -1, -1],
    [2, 8, 3, 2, 10, 8, 10, 9, 8, -1, -1, -1, -1, -1, -1, -1],
    [3, 11, 2, -1, -1, -1, -1, -1, -1, -1, -1, -1, -1, -1, -1, -1],
    [0, 11, 2, 8, 11, 0, -1, -1, -1, -1, -1, -1, -1, -1, -1, -1],
    [1, 9, 0, 2, 3, 11, -1, -1, -1, -1, -1, -1, -1, -1, -1, -1],
    [1, 11, 2, 1, 9, 11, 9, 8, 11, -1, -1, -1, -1, -1, -1, -1],
    [3, 10, 1, 11, 10, 3, -1, -1, -1, -1, -1, -1, -1, -1, -1, -1],
    [0, 10, 1, 0, 8, 10, 8, 11, 10, -1, -1, -1, -1, -1, -1, -1],
    [3, 9, 0, 3, 11, 9, 11, 10, 9, -1, -1, -1, -1, -1, -1, -1],
    [9, 8, 10, 10, 8, 11, -1, -1, -1, -1, -1, -1, -1, -1, -1, -1],
    [4, 7, 8, -1, -1, -1, -1, -1, -1, -1, -1, -1, -1, -1, -1, -1],
    [4, 3, 0, 7, 3, 4, -1, -1, -1, -1, -1, -1, -1, -1, -1, -1],
    [0, 1, 9, 8, 4, 7, -1, -1, -1, -1, -1, -1, -1, -1, -1, -1],
    [4, 1, 9, 4, 7, 1, 7, 3, 1, -1, -1, -1, -1, -1, -1, -1],
    [1, 2, 10, 8, 4, 7, -1, -1, -1, -1, -1, -1, -1, -1, -1, -1],
    [3, 4, 7, 3, 0, 4, 1, 2, 10, -1, -1, -1, -1, -1, -1, -1],
    [9, 2, 10, 9, 0, 2, 8, 4, 7, -1, -1, -1, -1, -1, -1, -1],
    [2, 10, 9, 2, 9, 7, 2, 7, 3, 7, 9, 4, -1, -1, -1, -1],
    [8, 4, 7, 3, 11, 2, -1, -1, -1, -1, -1, -1, -1, -1, -1, -1],
    [11, 4, 7, 11, 2, 4, 2, 0, 4, -1, -1, -1, -1, -1, -1, -1],
    [9, 0, 1, 8, 4, 7, 2, 3, 11, -1, -1, -1, -1, -1, -1, -1],
    [4, 7, 11, 9, 4, 11, 9, 11, 2, 9, 2, 1, -1, -1, -1, -1],
    [3, 10, 1, 3, 11, 10, 7, 8, 4, -1, -1, -1, -1, -1, -1, -1],
    [1, 11, 10, 1, 4, 11, 1, 0, 4, 7, 11, 4, -1, -1, -1, -1],
    [4, 7, 8, 9, 0, 11, 9, 11, 10, 11, 0, 3, -1, -1, -1, -1],
    [4, 7, 11, 4, 11, 9, 9, 11, 10, -1, -1, -1, -1, -1, -1, -1],
    [9, 5, 4, -1, -1, -1, -1, -1, -1, -1, -1, -1, -1, -1, -1, -1],
    [9, 5, 4, 0, 8, 3, -1, -1, -1, -1, -1, -1, -1, -1, -1, -1],
    [0, 5, 4, 1, 5, 0, -1, -1, -1, -1, -1, -1, -1, -1, -1, -1],
    [8, 5, 4, 8, 3, 5, 3, 1, 5, -1, -1, -1, -1, -1, -1, -1],
    [1, 2, 10, 9, 5, 4, -1, -1, -1, -1, -1, -1, -1, -1, -1, -1],
    [3, 0, 8, 1, 2, 10, 4, 9, 5, -1, -1, -1, -1, -1, -1, -1],
    [5, 2, 10, 5, 4, 2, 4, 0, 2, -1, -1, -1, -1, -1, -1, -1],
    [2, 10, 5, 3, 2, 5, 3, 5, 4, 3, 4, 8, -1, -1, -1, -1],
    [9, 5, 4, 2, 3, 11, -1, -1, -1, -1, -1, -1, -1, -1, -1, -1],
    [0, 11, 2, 0, 8, 11, 4, 9, 5, -1, -1, -1, -1, -1, -1, -1],
    [0, 5, 4, 0, 1, 5, 2, 3, 11, -1, -1, -1, -1, -1, -1, -1],
    [2, 1, 5, 2, 5, 8, 2, 8, 11, 4, 8, 5, -1, -1, -1, -1],
    [10, 3, 11, 10, 1, 3, 9, 5, 4, -1, -1, -1, -1, -1, -1, -1],
    [4, 9, 5, 0, 8, 1, 8, 10, 1, 8, 11, 10, -1, -1, -1, -1],
    [5, 4, 0, 5, 0, 11, 5, 11, 10, 11, 0, 3, -1, -1, -1, -1],
    [5, 4, 8, 5, 8, 10, 10, 8, 11, -1, -1, -1, -1, -1, -1, -1],
    [9, 7, 8, 5, 7, 9, -1, -1, -1, -1, -1, -1, -1, -1, -1, -1],
    [9, 3, 0, 9, 5, 3, 5, 7, 3, -1, -1, -1, -1, -1, -1, -1],
    [0, 7, 8, 0, 1, 7, 1, 5, 7, -1, -1, -1, -1, -1, -1, -1],
    [1, 5, 3, 3, 5, 7, -1, -1, -1, -1, -1, -1, -1, -1, -1, -1],
    [9, 7, 8, 9, 5, 7, 10, 1, 2, -1, -1, -1, -1, -1, -1, -1],
    [10, 1, 2, 9, 5, 0, 5, 3, 0, 5, 7, 3, -1, -1, -1, -1],
    [8, 0, 2, 8, 2, 5, 8, 5, 7, 10, 5, 2, -1, -1, -1, -1],
    [2, 10, 5, 2, 5, 3, 3, 5, 7, -1, -1, -1, -1, -1, -1, -1],
    [7, 9, 5, 7, 8, 9, 3, 11, 2, -1, -1, -1, -1, -1, -1, -1],
    [9, 5, 7, 9, 7, 2, 9, 2, 0, 2, 7, 11, -1, -1, -1, -1],
    [2, 3, 11, 0, 1, 8, 1, 7, 8, 1, 5, 7, -1, -1, -1, -1],
    [11, 2, 1, 11, 1, 7, 7, 1, 5, -1, -1, -1, -1, -1, -1, -1],
    [9, 5, 8, 8, 5, 7, 10, 1, 3, 10, 3, 11, -1, -1, -1, -1],
    [5, 7, 0, 5, 0, 9, 7, 11, 0, 1, 0, 10, 11, 10, 0, -1],
    [11, 10, 0, 11, 0, 3, 10, 5, 0, 8, 0, 7, 5, 7, 0, -1],
    [11, 10, 5, 7, 11, 5, -1, -1, -1, -1, -1, -1, -1, -1, -1, -1],
    [10, 6, 5, -1, -1, -1, -1, -1, -1, -1, -1, -1, -1, -1, -1, -1],
    [0, 8, 3, 5, 10, 6, -1, -1, -1, -1, -1, -1, -1, -1, -1, -1],
    [9, 0, 1, 5, 10, 6, -1, -1, -1, -1, -1, -1, -1, -1, -1, -1],
    [1, 8, 3, 1, 9, 8, 5, 10, 6, -1, -1, -1, -1, -1, -1, -1],
    [1, 6, 5, 2, 6, 1, -1, -1, -1, -1, -1, -1, -1, -1, -1, -1],
    [1, 6, 5, 1, 2, 6, 3, 0, 8, -1, -1, -1, -1, -1, -1, -1],
    [9, 6, 5, 9, 0, 6, 0, 2, 6, -1, -1, -1, -1, -1, -1, -1],
    [5, 9, 8, 5, 8, 2, 5, 2, 6, 3, 2, 8, -1, -1, -1, -1],
    [2, 3, 11, 10, 6, 5, -1, -1, -1, -1, -1, -1, -1, -1, -1, -1],
    [11, 0, 8, 11, 2, 0, 10, 6, 5, -1, -1, -1, -1, -1, -1, -1],
    [0, 1, 9, 2, 3, 11, 5, 10, 6, -1, -1, -1, -1, -1, -1, -1],
    [5, 10, 6, 1, 9, 2, 9, 11, 2, 9, 8, 11, -1, -1, -1, -1],
    [6, 3, 11, 6, 5, 3, 5, 1, 3, -1, -1, -1, -1, -1, -1, -1],
    [0, 8, 11, 0, 11, 5, 0, 5, 1, 5, 11, 6, -1, -1, -1, -1],
    [3, 11, 6, 0, 3, 6, 0, 6, 5, 0, 5, 9, -1, -1, -1, -1],
    [6, 5, 9, 6, 9, 11, 11, 9, 8, -1, -1, -1, -1, -1, -1, -1],
    [5, 10, 6, 4, 7, 8, -1, -1, -1, -1, -1, -1, -1, -1, -1, -1],
    [4, 3, 0, 4, 7, 3, 6, 5, 10, -1, -1, -1, -1, -1, -1, -1],
    [1, 9, 0, 5, 10, 6, 8, 4, 7, -1, -1, -1, -1, -1, -1, -1],
    [10, 6, 5, 1, 9, 7, 1, 7, 3, 7, 9, 4, -1, -1, -1, -1],
    [6, 1, 2, 6, 5, 1, 4, 7, 8, -1, -1, -1, -1, -1, -1, -1],
    [1, 2, 5, 5, 2, 6, 3, 0, 4, 3, 4, 7, -1, -1, -1, -1],
    [8, 4, 7, 9, 0, 5, 0, 6, 5, 0, 2, 6, -1, -1, -1, -1],
    [7, 3, 9, 7, 9, 4, 3, 2, 9, 5, 9, 6, 2, 6, 9, -1],
    [3, 11, 2, 7, 8, 4, 10, 6, 5, -1, -1, -1, -1, -1, -1, -1],
    [5, 10, 6, 4, 7, 2, 4, 2, 0, 2, 7, 11, -1, -1, -1, -1],
    [0, 1, 9, 4, 7, 8, 2, 3, 11, 5, 10, 6, -1, -1, -1, -1],
    [9, 2, 1, 9, 11, 2, 9, 4, 11, 7, 11, 4, 5, 10, 6, -1],
    [8, 4, 7, 3, 11, 5, 3, 5, 1, 5, 11, 6, -1, -1, -1, -1],
    [5, 1, 11, 5, 11, 6, 1, 0, 11, 7, 11, 4, 0, 4, 11, -1],
    [0, 5, 9, 0, 6, 5, 0, 3, 6, 11, 6, 3, 8, 4, 7, -1],
    [6, 5, 9, 6, 9, 11, 4, 7, 9, 7, 11, 9, -1, -1, -1, -1],
    [10, 4, 9, 6, 4, 10, -1, -1, -1, -1, -1, -1, -1, -1, -1, -1],
    [4, 10, 6, 4, 9, 10, 0, 8, 3, -1, -1, -1, -1, -1, -1, -1],
    [10, 0, 1, 10, 6, 0, 6, 4, 0, -1, -1, -1, -1, -1, -1, -1],
    [8, 3, 1, 8, 1, 6, 8, 6, 4, 6, 1, 10, -1, -1, -1, -1],
    [1, 4, 9, 1, 2, 4, 2, 6, 4, -1, -1, -1, -1, -1, -1, -1],
    [3, 0, 8, 1, 2, 9, 2, 4, 9, 2, 6, 4, -1, -1, -1, -1],
    [0, 2, 4, 4, 2, 6, -1, -1, -1, -1, -1, -1, -1, -1, -1, -1],
    [8, 3, 2, 8, 2, 4, 4, 2, 6, -1, -1, -1, -1, -1, -1, -1],
    [10, 4, 9, 10, 6, 4, 11, 2, 3, -1, -1, -1, -1, -1, -1, -1],
    [0, 8, 2, 2, 8, 11, 4, 9, 10, 4, 10, 6, -1, -1, -1, -1],
    [3, 11, 2, 0, 1, 6, 0, 6, 4, 6, 1, 10, -1, -1, -1, -1],
    [6, 4, 1, 6, 1, 10, 4, 8, 1, 2, 1, 11, 8, 11, 1, -1],
    [9, 6, 4, 9, 3, 6, 9, 1, 3, 11, 6, 3, -1, -1, -1, -1],
    [8, 11, 1, 8, 1, 0, 11, 6, 1, 9, 1, 4, 6, 4, 1, -1],
    [3, 11, 6, 3, 6, 0, 0, 6, 4, -1, -1, -1, -1, -1, -1, -1],
    [6, 4, 8, 11, 6, 8, -1, -1, -1, -1, -1, -1, -1, -1, -1, -1],
    [7, 10, 6, 7, 8, 10, 8, 9, 10, -1, -1, -1, -1, -1, -1, -1],
    [0, 7, 3, 0, 10, 7, 0, 9, 10, 6, 7, 10, -1, -1, -1, -1],
    [10, 6, 7, 1, 10, 7, 1, 7, 8, 1, 8, 0, -1, -1, -1, -1],
    [10, 6, 7, 10, 7, 1, 1, 7, 3, -1, -1, -1, -1, -1, -1, -1],
    [1, 2, 6, 1, 6, 8, 1, 8, 9, 8, 6, 7, -1, -1, -1, -1],
    [2, 6, 9, 2, 9, 1, 6, 7, 9, 0, 9, 3, 7, 3, 9, -1],
    [7, 8, 0, 7, 0, 6, 6, 0, 2, -1, -1, -1, -1, -1, -1, -1],
    [7, 3, 2, 6, 7, 2, -1, -1, -1, -1, -1, -1, -1, -1, -1, -1],
    [2, 3, 11, 10, 6, 8, 10, 8, 9, 8, 6, 7, -1, -1, -1, -1],
    [2, 0, 7, 2, 7, 11, 0, 9, 7, 6, 7, 10, 9, 10, 7, -1],
    [1, 8, 0, 1, 7, 8, 1, 10, 7, 6, 7, 10, 2, 3, 11, -1],
    [11, 2, 1, 11, 1, 7, 10, 6, 1, 6, 7, 1, -1, -1, -1, -1],
    [8, 9, 6, 8, 6, 7, 9, 1, 6, 11, 6, 3, 1, 3, 6, -1],
    [0, 9, 1, 11, 6, 7, -1, -1, -1, -1, -1, -1, -1, -1, -1, -1],
    [7, 8, 0, 7, 0, 6, 3, 11, 0, 11, 6, 0, -1, -1, -1, -1],
    [7, 11, 6, -1, -1, -1, -1, -1, -1, -1, -1, -1, -1, -1, -1, -1],
    [7, 6, 11, -1, -1, -1, -1, -1, -1, -1, -1, -1, -1, -1, -1, -1],
    [3, 0, 8, 11, 7, 6, -1, -1, -1, -1, -1, -1, -1, -1, -1, -1],
    [0, 1, 9, 11, 7, 6, -1, -1, -1, -1, -1, -1, -1, -1, -1, -1],
    [8, 1, 9, 8, 3, 1, 11, 7, 6, -1, -1, -1, -1, -1, -1, -1],
    [10, 1, 2, 6, 11, 7, -1, -1, -1, -1, -1, -1, -1, -1, -1, -1],
    [1, 2, 10, 3, 0, 8, 6, 11, 7, -1, -1, -1, -1, -1, -1, -1],
    [2, 9, 0, 2, 10, 9, 6, 11, 7, -1, -1, -1, -1, -1, -1, -1],
    [6, 11, 7, 2, 10, 3, 10, 8, 3, 10, 9, 8, -1, -1, -1, -1],
    [7, 2, 3, 6, 2, 7, -1, -1, -1, -1, -1, -1, -1, -1, -1, -1],
    [7, 0, 8, 7, 6, 0, 6, 2, 0, -1, -1, -1, -1, -1, -1, -1],
    [2, 7, 6, 2, 3, 7, 0, 1, 9, -1, -1, -1, -1, -1, -1, -1],
    [1, 6, 2, 1, 8, 6, 1, 9, 8, 8, 7, 6, -1, -1, -1, -1],
    [10, 7, 6, 10, 1, 7, 1, 3, 7, -1, -1, -1, -1, -1, -1, -1],
    [10, 7, 6, 1, 7, 10, 1, 8, 7, 1, 0, 8, -1, -1, -1, -1],
    [0, 3, 7, 0, 7, 10, 0, 10, 9, 6, 10, 7, -1, -1, -1, -1],
    [7, 6, 10, 7, 10, 8, 8, 10, 9, -1, -1, -1, -1, -1, -1, -1],
    [6, 8, 4, 11, 8, 6, -1, -1, -1, -1, -1, -1, -1, -1, -1, -1],
    [3, 6, 11, 3, 0, 6, 0, 4, 6, -1, -1, -1, -1, -1, -1, -1],
    [8, 6, 11, 8, 4, 6, 9, 0, 1, -1, -1, -1, -1, -1, -1, -1],
    [9, 4, 6, 9, 6, 3, 9, 3, 1, 11, 3, 6, -1, -1, -1, -1],
    [6, 8, 4, 6, 11, 8, 2, 10, 1, -1, -1, -1, -1, -1, -1, -1],
    [1, 2, 10, 3, 0, 11, 0, 6, 11, 0, 4, 6, -1, -1, -1, -1],
    [4, 11, 8, 4, 6, 11, 0, 2, 9, 2, 10, 9, -1, -1, -1, -1],
    [10, 9, 3, 10, 3, 2, 9, 4, 3, 11, 3, 6, 4, 6, 3, -1],
    [8, 2, 3, 8, 4, 2, 4, 6, 2, -1, -1, -1, -1, -1, -1, -1],
    [0, 4, 2, 4, 6, 2, -1, -1, -1, -1, -1, -1, -1, -1, -1, -1],
    [1, 9, 0, 2, 3, 4, 2, 4, 6, 4, 3, 8, -1, -1, -1, -1],
    [1, 9, 4, 1, 4, 2, 2, 4, 6, -1, -1, -1, -1, -1, -1, -1],
    [8, 1, 3, 8, 6, 1, 8, 4, 6, 6, 10, 1, -1, -1, -1, -1],
    [10, 1, 0, 10, 0, 6, 6, 0, 4, -1, -1, -1, -1, -1, -1, -1],
    [4, 6, 3, 4, 3, 8, 6, 10, 3, 0, 3, 9, 10, 9, 3, -1],
    [10, 9, 4, 6, 10, 4, -1, -1, -1, -1, -1, -1, -1, -1, -1, -1],
    [4, 9, 5, 7, 6, 11, -1, -1, -1, -1, -1, -1, -1, -1, -1, -1],
    [0, 8, 3, 4, 9, 5, 11, 7, 6, -1, -1, -1, -1, -1, -1, -1],
    [5, 0, 1, 5, 4, 0, 7, 6, 11, -1, -1, -1, -1, -1, -1, -1],
    [11, 7, 6, 8, 3, 4, 3, 5, 4, 3, 1, 5, -1, -1, -1, -1],
    [9, 5, 4, 10, 1, 2, 7, 6, 11, -1, -1, -1, -1, -1, -1, -1],
    [6, 11, 7, 1, 2, 10, 0, 8, 3, 4, 9, 5, -1, -1, -1, -1],
    [7, 6, 11, 5, 4, 10, 4, 2, 10, 4, 0, 2, -1, -1, -1, -1],
    [3, 4, 8, 3, 5, 4, 3, 2, 5, 10, 5, 2, 11, 7, 6, -1],
    [7, 2, 3, 7, 6, 2, 5, 4, 9, -1, -1, -1, -1, -1, -1, -1],
    [9, 5, 4, 0, 8, 6, 0, 6, 2, 6, 8, 7, -1, -1, -1, -1],
    [3, 6, 2, 3, 7, 6, 1, 5, 0, 5, 4, 0, -1, -1, -1, -1],
    [6, 2, 8, 6, 8, 7, 2, 1, 8, 4, 8, 5, 1, 5, 8, -1],
    [9, 5, 4, 10, 1, 6, 1, 7, 6, 1, 3, 7, -1, -1, -1, -1],
    [1, 6, 10, 1, 7, 6, 1, 0, 7, 8, 7, 0, 9, 5, 4, -1],
    [4, 0, 10, 4, 10, 5, 0, 3, 10, 6, 10, 7, 3, 7, 10, -1],
    [7, 6, 10, 7, 10, 8, 5, 4, 10, 4, 8, 10, -1, -1, -1, -1],
    [6, 9, 5, 6, 11, 9, 11, 8, 9, -1, -1, -1, -1, -1, -1, -1],
    [3, 6, 11, 0, 6, 3, 0, 5, 6, 0, 9, 5, -1, -1, -1, -1],
    [0, 11, 8, 0, 5, 11, 0, 1, 5, 5, 6, 11, -1, -1, -1, -1],
    [6, 11, 3, 6, 3, 5, 5, 3, 1, -1, -1, -1, -1, -1, -1, -1],
    [1, 2, 10, 9, 5, 11, 9, 11, 8, 11, 5, 6, -1, -1, -1, -1],
    [0, 11, 3, 0, 6, 11, 0, 9, 6, 5, 6, 9, 1, 2, 10, -1],
    [11, 8, 5, 11, 5, 6, 8, 0, 5, 10, 5, 2, 0, 2, 5, -1],
    [6, 11, 3, 6, 3, 5, 2, 10, 3, 10, 5, 3, -1, -1, -1, -1],
    [5, 8, 9, 5, 2, 8, 5, 6, 2, 3, 8, 2, -1, -1, -1, -1],
    [9, 5, 6, 9, 6, 0, 0, 6, 2, -1, -1, -1, -1, -1, -1, -1],
    [1, 5, 8, 1, 8, 0, 5, 6, 8, 3, 8, 2, 6, 2, 8, -1],
    [1, 5, 6, 2, 1, 6, -1, -1, -1, -1, -1, -1, -1, -1, -1, -1],
    [1, 3, 6, 1, 6, 10, 3, 8, 6, 5, 6, 9, 8, 9, 6, -1],
    [10, 1, 0, 10, 0, 6, 9, 5, 0, 5, 6, 0, -1, -1, -1, -1],
    [0, 3, 8, 5, 6, 10, -1, -1, -1, -1, -1, -1, -1, -1, -1, -1],
    [10, 5, 6, -1, -1, -1, -1, -1, -1, -1, -1, -1, -1, -1, -1, -1],
    [11, 5, 10, 7, 5, 11, -1, -1, -1, -1, -1, -1, -1, -1, -1, -1],
    [11, 5, 10, 11, 7, 5, 8, 3, 0, -1, -1, -1, -1, -1, -1, -1],
    [5, 11, 7, 5, 10, 11, 1, 9, 0, -1, -1, -1, -1, -1, -1, -1],
    [10, 7, 5, 10, 11, 7, 9, 8, 1, 8, 3, 1, -1, -1, -1, -1],
    [11, 1, 2, 11, 7, 1, 7, 5, 1, -1, -1, -1, -1, -1, -1, -1],
    [0, 8, 3, 1, 2, 7, 1, 7, 5, 7, 2, 11, -1, -1, -1, -1],
    [9, 7, 5, 9, 2, 7, 9, 0, 2, 2, 11, 7, -1, -1, -1, -1],
    [7, 5, 2, 7, 2, 11, 5, 9, 2, 3, 2, 8, 9, 8, 2, -1],
    [2, 5, 10, 2, 3, 5, 3, 7, 5, -1, -1, -1, -1, -1, -1, -1],
    [8, 2, 0, 8, 5, 2, 8, 7, 5, 10, 2, 5, -1, -1, -1, -1],
    [9, 0, 1, 5, 10, 3, 5, 3, 7, 3, 10, 2, -1, -1, -1, -1],
    [9, 8, 2, 9, 2, 1, 8, 7, 2, 10, 2, 5, 7, 5, 2, -1],
    [1, 3, 5, 3, 7, 5, -1, -1, -1, -1, -1, -1, -1, -1, -1, -1],
    [0, 8, 7, 0, 7, 1, 1, 7, 5, -1, -1, -1, -1, -1, -1, -1],
    [9, 0, 3, 9, 3, 5, 5, 3, 7, -1, -1, -1, -1, -1, -1, -1],
    [9, 8, 7, 5, 9, 7, -1, -1, -1, -1, -1, -1, -1, -1, -1, -1],
    [5, 8, 4, 5, 10, 8, 10, 11, 8, -1, -1, -1, -1, -1, -1, -1],
    [5, 0, 4, 5, 11, 0, 5, 10, 11, 11, 3, 0, -1, -1, -1, -1],
    [0, 1, 9, 8, 4, 10, 8, 10, 11, 10, 4, 5, -1, -1, -1, -1],
    [10, 11, 4, 10, 4, 5, 11, 3, 4, 9, 4, 1, 3, 1, 4, -1],
    [2, 5, 1, 2, 8, 5, 2, 11, 8, 4, 5, 8, -1, -1, -1, -1],
    [0, 4, 11, 0, 11, 3, 4, 5, 11, 2, 11, 1, 5, 1, 11, -1],
    [0, 2, 5, 0, 5, 9, 2, 11, 5, 4, 5, 8, 11, 8, 5, -1],
    [9, 4, 5, 2, 11, 3, -1, -1, -1, -1, -1, -1, -1, -1, -1, -1],
    [2, 5, 10, 3, 5, 2, 3, 4, 5, 3, 8, 4, -1, -1, -1, -1],
    [5, 10, 2, 5, 2, 4, 4, 2, 0, -1, -1, -1, -1, -1, -1, -1],
    [3, 10, 2, 3, 5, 10, 3, 8, 5, 4, 5, 8, 0, 1, 9, -1],
    [5, 10, 2, 5, 2, 4, 1, 9, 2, 9, 4, 2, -1, -1, -1, -1],
    [8, 4, 5, 8, 5, 3, 3, 5, 1, -1, -1, -1, -1, -1, -1, -1],
    [0, 4, 5, 1, 0, 5, -1, -1, -1, -1, -1, -1, -1, -1, -1, -1],
    [8, 4, 5, 8, 5, 3, 9, 0, 5, 0, 3, 5, -1, -1, -1, -1],
    [9, 4, 5, -1, -1, -1, -1, -1, -1, -1, -1, -1, -1, -1, -1, -1],
    [4, 11, 7, 4, 9, 11, 9, 10, 11, -1, -1, -1, -1, -1, -1, -1],
    [0, 8, 3, 4, 9, 7, 9, 11, 7, 9, 10, 11, -1, -1, -1, -1],
    [1, 10, 11, 1, 11, 4, 1, 4, 0, 7, 4, 11, -1, -1, -1, -1],
    [3, 1, 4, 3, 4, 8, 1, 10, 4, 7, 4, 11, 10, 11, 4, -1],
    [4, 11, 7, 9, 11, 4, 9, 2, 11, 9, 1, 2, -1, -1, -1, -1],
    [9, 7, 4, 9, 11, 7, 9, 1, 11, 2, 11, 1, 0, 8, 3, -1],
    [11, 7, 4, 11, 4, 2, 2, 4, 0, -1, -1, -1, -1, -1, -1, -1],
    [11, 7, 4, 11, 4, 2, 8, 3, 4, 3, 2, 4, -1, -1, -1, -1],
    [2, 9, 10, 2, 7, 9, 2, 3, 7, 7, 4, 9, -1, -1, -1, -1],
    [9, 10, 7, 9, 7, 4, 10, 2, 7, 8, 7, 0, 2, 0, 7, -1],
    [3, 7, 10, 3, 10, 2, 7, 4, 10, 1, 10, 0, 4, 0, 10, -1],
    [1, 10, 2, 8, 7, 4, -1, -1, -1, -1, -1, -1, -1, -1, -1, -1],
    [4, 9, 1, 4, 1, 7, 7, 1, 3, -1, -1, -1, -1, -1, -1, -1],
    [4, 9, 1, 4, 1, 7, 0, 8, 1, 8, 7, 1, -1, -1, -1, -1],
    [4, 0, 3, 7, 4, 3, -1, -1, -1, -1, -1, -1, -1, -1, -1, -1],
    [4, 8, 7, -1, -1, -1, -1, -1, -1, -1, -1, -1, -1, -1, -1, -1],
    [9, 10, 8, 10, 11, 8, -1, -1, -1, -1, -1, -1, -1, -1, -1, -1],
    [3, 0, 9, 3, 9, 11, 11, 9, 10, -1, -1, -1, -1, -1, -1, -1],
    [0, 1, 10, 0, 10, 8, 8, 10, 11, -1, -1, -1, -1, -1, -1, -1],
    [3, 1, 10, 11, 3, 10, -1, -1, -1, -1, -1, -1, -1, -1, -1, -1],
    [1, 2, 11, 1, 11, 9, 9, 11, 8, -1, -1, -1, -1, -1, -1, -1],
    [3, 0, 9, 3, 9, 11, 1, 2, 9, 2, 11, 9, -1, -1, -1, -1],
    [0, 2, 11, 8, 0, 11, -1, -1, -1, -1, -1, -1, -1, -1, -1, -1],
    [3, 2, 11, -1, -1, -1, -1, -1, -1, -1, -1, -1, -1, -1, -1, -1],
    [2, 3, 8, 2, 8, 10, 10, 8, 9, -1, -1, -1, -1, -1, -1, -1],
    [9, 10, 2, 0, 9, 2, -1, -1, -1, -1, -1, -1, -1, -1, -1, -1],
    [2, 3, 8, 2, 8, 10, 0, 1, 8, 1, 10, 8, -1, -1, -1, -1],
    [1, 10, 2, -1, -1, -1, -1, -1, -1, -1, -1, -1, -1, -1, -1, -1],
    [1, 3, 8, 9, 1, 8, -1, -1, -1, -1, -1, -1, -1, -1, -1, -1],
    [0, 9, 1, -1, -1, -1, -1, -1, -1, -1, -1, -1, -1, -1, -1, -1],
    [0, 3, 8, -1, -1, -1, -1, -1, -1, -1, -1, -1, -1, -1, -1, -1],
    [-1, -1, -1, -1, -1, -1, -1, -1, -1, -1, -1, -1, -1, -1, -1, -1],
];

/// Edge origin (3 coordinate offsets) and direction (4th value) for each of the
/// 12 cube edges (port of `MCEdgeIndexToCoordOffsets` in `mc_lut.cpp`).
#[rustfmt::skip]
const MC_EDGE_INDEX_TO_COORD_OFFSETS: [[usize; 4]; 12] = [
    [0, 0, 0, 0],
    [0, 0, 1, 1],
    [0, 1, 0, 0],
    [0, 0, 0, 1],
    [1, 0, 0, 0],
    [1, 0, 1, 1],
    [1, 1, 0, 0],
    [1, 0, 0, 1],
    [0, 0, 0, 2],
    [0, 0, 1, 2],
    [0, 1, 1, 2],
    [0, 1, 0, 2],
];

#[cfg(test)]
mod tests {
    use super::*;

    /// A uniform field never crosses the level → no vertices, no triangles.
    #[test]
    fn uniform_field_has_no_surface() {
        let data = [0.0f32; 8]; // 2×2×2 all zero
        let mut mc = MarchingCubes::new(0.5);
        mc.process(&data, 2, 2, 2).unwrap();
        assert!(mc.vertices.is_empty(), "no crossings → no vertices");
        assert!(mc.indices.is_empty(), "no crossings → no triangles");
    }

    /// A 2×2×2 cube with exactly one corner above the level produces exactly one
    /// triangle clipping that corner, with vertices at the analytic edge
    /// midpoints. Corner (z=0,y=0,x=0)=1 (> 0.5), the rest 0 (<= 0.5).
    #[test]
    fn single_corner_above_level_one_triangle() {
        // Layout (depth, height, width) = (2,2,2), width contiguous.
        // index = z*4 + y*2 + x. Corner (0,0,0) = index 0.
        let mut data = [0.0f32; 8];
        data[0] = 1.0;
        let mut mc = MarchingCubes::new(0.5);
        mc.invert_normals = false;
        mc.process(&data, 2, 2, 2).unwrap();

        let verts = mc.vertices_zyx();
        assert_eq!(verts.len(), 3, "exactly one triangle = 3 vertices");
        assert_eq!(mc.indices, vec![0, 1, 2]);

        // Edges crossing from corner 0: x (edge 0), y (edge 3), z (edge 8), each
        // at midpoint 0.5. Registration order: first slice x then y, then z.
        // (z,y,x): x-edge → (0,0,0.5); y-edge → (0,0.5,0); z-edge → (0.5,0,0).
        let approx = |a: [f32; 3], b: [f32; 3]| (0..3).all(|i| (a[i] - b[i]).abs() < 1e-6);
        assert!(
            approx(verts[0], [0.0, 0.0, 0.5]),
            "x-edge vertex: {:?}",
            verts[0]
        );
        assert!(
            approx(verts[1], [0.0, 0.5, 0.0]),
            "y-edge vertex: {:?}",
            verts[1]
        );
        assert!(
            approx(verts[2], [0.5, 0.0, 0.0]),
            "z-edge vertex: {:?}",
            verts[2]
        );
    }

    /// Normals are unit length, and `invert_normals` negates them.
    #[test]
    fn normals_unit_length_and_invertible() {
        let mut data = [0.0f32; 8];
        data[0] = 1.0;

        let mut up = MarchingCubes::new(0.5);
        up.invert_normals = false;
        up.process(&data, 2, 2, 2).unwrap();
        let n_up = up.normals_zyx();

        let mut inv = MarchingCubes::new(0.5);
        inv.invert_normals = true;
        inv.process(&data, 2, 2, 2).unwrap();
        let n_inv = inv.normals_zyx();

        assert_eq!(n_up.len(), 3);
        for n in &n_up {
            let len = (n[0] * n[0] + n[1] * n[1] + n[2] * n[2]).sqrt();
            assert!((len - 1.0).abs() < 1e-5, "unit normal, got len {len}");
        }
        for (a, b) in n_up.iter().zip(&n_inv) {
            for i in 0..3 {
                assert!((a[i] + b[i]).abs() < 1e-5, "invert negates: {a:?} vs {b:?}");
            }
        }
    }

    /// Interpolation respects an off-centre crossing: corner 0 = 0.25, its +x
    /// neighbour = 1.0, level 0.5 → the x crossing sits at offset
    /// (0.5-0.25)/(1.0-0.25) = 1/3 along x.
    #[test]
    fn vertex_interpolates_off_centre() {
        let mut data = [0.0f32; 8];
        data[0] = 0.25; // (0,0,0)
        data[1] = 1.0; // (0,0,1)  +x neighbour
        // Everything else 0 (<= 0.5); corner 1 is the only one > 0.5.
        let mut mc = MarchingCubes::new(0.5);
        mc.process(&data, 2, 2, 2).unwrap();
        let verts = mc.vertices_zyx();
        // The x-edge crossing between (0,0,0) and (0,0,1): value0=0.25, value=1.0
        // → offset 1/3 → x = 0 + 1/3.
        let x_edge = verts
            .iter()
            .find(|v| v[0].abs() < 1e-6 && v[1].abs() < 1e-6 && v[2] > 1e-6);
        let v = x_edge.expect("x-edge crossing vertex present");
        assert!(
            (v[2] - 1.0 / 3.0).abs() < 1e-6,
            "x offset 1/3, got {}",
            v[2]
        );
    }

    /// A z-only step field produces a planar surface between the crossing slices,
    /// and `sampling[depth] = 2` moves the crossing because the strided slices
    /// straddle a wider gap. depth=3: plane 0 = 1.0, planes 1,2 = 0.0.
    #[test]
    fn sampling_depth_shifts_z_crossing() {
        let (d, h, w) = (3usize, 2usize, 2usize);
        let mut data = vec![0.0f32; d * h * w];
        for v in data.iter_mut().take(h * w) {
            *v = 1.0; // z=0 plane
        }

        // Default sampling: crossing between z=0 (1.0) and z=1 (0.0) at z=0.5.
        let mut mc1 = MarchingCubes::new(0.5);
        mc1.process(&data, d, h, w).unwrap();
        let z1: Vec<f32> = mc1
            .vertices_zyx()
            .iter()
            .filter(|v| v[0] > 1e-6) // z-direction crossings have non-integer z
            .map(|v| v[0])
            .collect();
        assert!(!z1.is_empty(), "default sampling yields a z crossing");
        for z in &z1 {
            assert!((z - 0.5).abs() < 1e-6, "default crossing at z=0.5, got {z}");
        }

        // sampling depth=2: processes slices 0 and 2; crossing between z=0 (1.0)
        // and z=2 (0.0) → z = 0 + 0.5*2 = 1.0.
        let mut mc2 = MarchingCubes::new(0.5).with_sampling([2, 1, 1]);
        mc2.process(&data, d, h, w).unwrap();
        let z2: Vec<f32> = mc2
            .vertices_zyx()
            .iter()
            .filter(|v| v[0] > 1e-6)
            .map(|v| v[0])
            .collect();
        assert!(!z2.is_empty(), "strided sampling yields a z crossing");
        for z in &z2 {
            assert!((z - 1.0).abs() < 1e-6, "strided crossing at z=1.0, got {z}");
        }
    }

    /// Shape mismatch is reported, not silently truncated.
    #[test]
    fn shape_mismatch_errors() {
        let data = [0.0f32; 7]; // not 2*2*2
        let mut mc = MarchingCubes::new(0.5);
        let err = mc.process(&data, 2, 2, 2).unwrap_err();
        assert_eq!(
            err,
            MarchingCubesError::ShapeMismatch {
                got: 7,
                expected: 8
            }
        );
    }

    /// The high-level `isosurface` helper returns `None` for an empty surface and
    /// `Some` with consistent lengths for a real one.
    #[test]
    fn isosurface_helper_none_and_some() {
        let flat = [0.0f32; 8];
        assert!(isosurface(&flat, 2, 2, 2, 0.5, true).is_none());

        let mut data = [0.0f32; 8];
        data[0] = 1.0;
        let (v, n, idx) = isosurface(&data, 2, 2, 2, 0.5, true).expect("non-empty surface");
        assert_eq!(v.len(), n.len(), "one normal per vertex");
        assert_eq!(idx.len() % 3, 0, "indices come in triangles");
        assert!(
            idx.iter().all(|&i| (i as usize) < v.len()),
            "indices in range"
        );
    }

    /// The triangle table and offsets tables match the C++ source dimensions.
    #[test]
    fn lookup_tables_well_formed() {
        assert_eq!(MC_TRIANGLE_TABLE.len(), 256);
        assert_eq!(MC_EDGE_INDEX_TO_COORD_OFFSETS.len(), 12);
        // Every non-terminator entry is a valid edge index in [0, 12).
        for row in &MC_TRIANGLE_TABLE {
            for &e in row {
                assert!(
                    e == -1 || (0..12).contains(&e),
                    "edge index {e} out of range"
                );
            }
        }
        // Code 0 (all corners above level) and code 255 (all below) are empty.
        assert_eq!(MC_TRIANGLE_TABLE[0][0], -1);
        assert_eq!(MC_TRIANGLE_TABLE[255][0], -1);
    }
}
