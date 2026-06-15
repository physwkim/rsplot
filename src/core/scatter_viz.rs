//! Scatter visualization algorithms (silx `Scatter` / `ScatterVisualizationMixIn`).
//!
//! Pure, GPU-free transforms that turn unstructured `(x, y, value)` points into
//! renderable structures that the existing render paths can later consume:
//!
//! - [`delaunay`] — Bowyer-Watson incremental Delaunay triangulation of 2D
//!   points, the basis for silx's `SOLID` and `IRREGULAR_GRID` visualizations
//!   (silx uses matplotlib's `Triangulation`; see
//!   `gui/plot/items/scatter.py::_getTriangulationFuture`).
//! - [`solid_triangles`] — `Visualization.SOLID`: per-vertex-colored
//!   [`Triangles`] (silx `scatter.py:610-625`, `backend.addTriangles`).
//! - [`irregular_grid_image`] — `Visualization.IRREGULAR_GRID`: the
//!   triangulation rasterized to a value image by barycentric linear
//!   interpolation (silx `LinearTriInterpolator`).
//! - [`detect_regular_grid`] — `Visualization.REGULAR_GRID` auto-detection
//!   (`scatter.py::_guess_grid` / `_guess_z_grid_shape`, `core.py:1303-1308`).
//! - [`binned_statistic`] — `Visualization.BINNED_STATISTIC`: 2D binning with
//!   per-bin mean/count/sum (`core.py:1325-1329`, `scatter.py::__getHistogramInfo`).
//! - [`PointsViz`] — `Visualization.POINTS` data carrier with the optional
//!   per-point alpha array (silx `scatter.py` per-point alpha).
//!
//! Faithful to the silx semantics; GPU wiring (feeding these into
//! `backend_wgpu.rs` / `high_level.rs`) is deferred to a later wave.
//!
//! Mode-specific picking (silx `Scatter.pick`, `scatter.py:804-861`) is provided
//! for the grid modes that map a rendered cell back to source points:
//! [`regular_grid_pick`] (REGULAR_GRID) and [`BinnedStatistic::pick`]
//! (BINNED_STATISTIC). POINTS/SOLID use plain nearest-point picking (silx's
//! default `super().pick()`); IRREGULAR_GRID has no 1:1 cell→point mapping in
//! siplot's interpolated-image render (silx picks Delaunay triangles), so it is
//! intentionally not covered here.

use egui::Color32;

use crate::core::triangles::Triangles;

/// Major order of points in a regular grid (silx
/// `VisualizationParameter.GRID_MAJOR_ORDER`, `core.py:1303-1308`).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum GridMajorOrder {
    /// `"row"`: X (column) is the fast dimension — points fill the first row
    /// left-to-right, then the next row, etc.
    Row,
    /// `"column"`: Y (row) is the fast dimension — points fill the first
    /// column, then the next column, etc.
    Column,
}

/// The reduction function applied to each bin in a binned statistic (silx
/// `VisualizationParameter.BINNED_STATISTIC_FUNCTION`, `core.py:1325-1329`).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum BinnedStatisticFunction {
    /// `"mean"` (default): per-bin `sum / count`, `NaN` for empty bins.
    Mean,
    /// `"count"`: number of points in the bin.
    Count,
    /// `"sum"`: sum of values in the bin.
    Sum,
}

// ===========================================================================
// 1. Delaunay triangulation (Bowyer-Watson)
// ===========================================================================

/// A planar Delaunay triangulation: `triangles[i]` holds three indices into the
/// input point array (silx's matplotlib `Triangulation.triangles`).
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Triangulation {
    /// Triangle vertex-index triples into the input `(x, y)` arrays.
    pub triangles: Vec<[usize; 3]>,
}

impl Triangulation {
    /// Number of triangles.
    #[must_use]
    pub fn len(&self) -> usize {
        self.triangles.len()
    }

    /// Whether the triangulation is empty (degenerate / collinear input).
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.triangles.is_empty()
    }
}

/// Twice the signed area of triangle `(a, b, c)`; positive when the vertices are
/// counter-clockwise. Used for orientation and degeneracy tests.
fn orient2d(a: [f64; 2], b: [f64; 2], c: [f64; 2]) -> f64 {
    (b[0] - a[0]) * (c[1] - a[1]) - (b[1] - a[1]) * (c[0] - a[0])
}

/// Returns `true` when point `p` lies strictly inside the circumcircle of the
/// triangle `(a, b, c)`, which is assumed counter-clockwise. This is the
/// classic 3x3 in-circle determinant; with CCW orientation a positive
/// determinant means `p` is inside.
fn in_circumcircle(a: [f64; 2], b: [f64; 2], c: [f64; 2], p: [f64; 2]) -> bool {
    let ax = a[0] - p[0];
    let ay = a[1] - p[1];
    let bx = b[0] - p[0];
    let by = b[1] - p[1];
    let cx = c[0] - p[0];
    let cy = c[1] - p[1];

    let a2 = ax * ax + ay * ay;
    let b2 = bx * bx + by * by;
    let c2 = cx * cx + cy * cy;

    let det = ax * (by * c2 - b2 * cy) - ay * (bx * c2 - b2 * cx) + a2 * (bx * cy - by * cx);
    det > 0.0
}

/// Bowyer-Watson incremental Delaunay triangulation of 2D points.
///
/// Points are inserted one at a time into a super-triangle large enough to
/// contain all of them; triangles whose circumcircle contains the new point are
/// removed and the resulting cavity is re-triangulated. Triangles touching the
/// super-triangle vertices are discarded at the end, leaving only triangles
/// over the input points.
///
/// Returns an empty triangulation when fewer than 3 finite points are given or
/// when all points are collinear/coincident (no triangle has nonzero area) —
/// silx surfaces this as "Cannot get a triangulation" and skips the renderer.
///
/// `x` and `y` must have the same length. Non-finite points are ignored, as
/// silx masks them out before triangulating
/// (`scatter.py::_getTriangulationFuture`).
#[must_use]
pub fn delaunay(x: &[f64], y: &[f64]) -> Triangulation {
    assert_eq!(x.len(), y.len(), "x and y must have the same length");

    // Collect finite points, remembering their original indices so the output
    // refers to the caller's arrays.
    let pts: Vec<(usize, [f64; 2])> = x
        .iter()
        .zip(y)
        .enumerate()
        .filter(|&(_, (&xi, &yi))| xi.is_finite() && yi.is_finite())
        .map(|(i, (&xi, &yi))| (i, [xi, yi]))
        .collect();

    if pts.len() < 3 {
        return Triangulation { triangles: vec![] };
    }

    // Reject fully-degenerate input (all collinear/coincident): no triangle can
    // have nonzero area, so a Delaunay triangulation does not exist.
    let p0 = pts[0].1;
    let has_area = pts.iter().enumerate().any(|(i, &(_, pi))| {
        pts[i + 1..]
            .iter()
            .any(|&(_, pj)| orient2d(p0, pi, pj).abs() > 0.0)
    });
    if !has_area {
        return Triangulation { triangles: vec![] };
    }

    // Build a super-triangle that strictly contains every point.
    let (mut min_x, mut min_y, mut max_x, mut max_y) = (
        f64::INFINITY,
        f64::INFINITY,
        f64::NEG_INFINITY,
        f64::NEG_INFINITY,
    );
    for &(_, [px, py]) in &pts {
        min_x = min_x.min(px);
        min_y = min_y.min(py);
        max_x = max_x.max(px);
        max_y = max_y.max(py);
    }
    let dx = max_x - min_x;
    let dy = max_y - min_y;
    let d = dx.max(dy).max(f64::MIN_POSITIVE);
    let mid_x = 0.5 * (min_x + max_x);
    let mid_y = 0.5 * (min_y + max_y);
    // Three vertices well outside the bounding box. The factor 20 gives ample
    // margin so the super-triangle contains the whole point set.
    let st0 = [mid_x - 20.0 * d, mid_y - d];
    let st1 = [mid_x, mid_y + 20.0 * d];
    let st2 = [mid_x + 20.0 * d, mid_y - d];

    // Working vertex list: input points first, then the three super vertices.
    let n = pts.len();
    let mut verts: Vec<[f64; 2]> = pts.iter().map(|&(_, p)| p).collect();
    verts.push(st0);
    verts.push(st1);
    verts.push(st2);
    let s0 = n;
    let s1 = n + 1;
    let s2 = n + 2;

    // Each triangle stored CCW.
    let mut tris: Vec<[usize; 3]> = vec![ccw(&verts, [s0, s1, s2])];

    for ip in 0..n {
        let p = verts[ip];

        // Find triangles whose circumcircle contains p ("bad" triangles).
        let mut bad: Vec<usize> = Vec::new();
        for (ti, t) in tris.iter().enumerate() {
            if in_circumcircle(verts[t[0]], verts[t[1]], verts[t[2]], p) {
                bad.push(ti);
            }
        }

        // Collect the boundary of the cavity: edges that belong to exactly one
        // bad triangle.
        let mut boundary: Vec<[usize; 2]> = Vec::new();
        for &bi in &bad {
            let t = tris[bi];
            for &(a, b) in &[(t[0], t[1]), (t[1], t[2]), (t[2], t[0])] {
                // Is edge (a,b) shared with another bad triangle?
                let shared = bad
                    .iter()
                    .any(|&oi| oi != bi && triangle_has_edge(&tris[oi], a, b));
                if !shared {
                    boundary.push([a, b]);
                }
            }
        }

        // Remove bad triangles (descending index order keeps indices valid).
        bad.sort_unstable();
        for &bi in bad.iter().rev() {
            tris.swap_remove(bi);
        }

        // Re-triangulate the cavity by connecting p to each boundary edge.
        for [a, b] in boundary {
            tris.push(ccw(&verts, [a, b, ip]));
        }
    }

    // Drop triangles that touch a super-triangle vertex and remap to original
    // input indices.
    let original: Vec<usize> = pts.iter().map(|&(i, _)| i).collect();
    let triangles: Vec<[usize; 3]> = tris
        .into_iter()
        .filter(|t| t.iter().all(|&v| v < n))
        .map(|t| [original[t[0]], original[t[1]], original[t[2]]])
        .collect();

    Triangulation { triangles }
}

/// Order a triangle's three vertices counter-clockwise (positive signed area).
fn ccw(verts: &[[f64; 2]], t: [usize; 3]) -> [usize; 3] {
    if orient2d(verts[t[0]], verts[t[1]], verts[t[2]]) < 0.0 {
        [t[0], t[2], t[1]]
    } else {
        t
    }
}

/// Whether triangle `t` has the undirected edge `(a, b)`.
fn triangle_has_edge(t: &[usize; 3], a: usize, b: usize) -> bool {
    let edges = [(t[0], t[1]), (t[1], t[2]), (t[2], t[0])];
    edges
        .iter()
        .any(|&(u, v)| (u == a && v == b) || (u == b && v == a))
}

// ===========================================================================
// 2. SOLID / IRREGULAR_GRID
// ===========================================================================

/// Build the per-vertex-colored [`Triangles`] for `Visualization.SOLID`.
///
/// Each input point keeps its color (the caller maps `value` through a colormap
/// before calling, as silx does in `__applyColormapToData`), and the Delaunay
/// triangulation over the points provides the mesh
/// (silx `scatter.py:610-625`, `backend.addTriangles`).
///
/// `x`, `y`, and `colors` must have equal length. Returns `None` when the
/// points cannot be triangulated (fewer than 3 finite points or collinear),
/// matching silx's "Cannot display as solid surface" early-out.
#[must_use]
pub fn solid_triangles(x: &[f64], y: &[f64], colors: &[Color32]) -> Option<Triangles> {
    assert_eq!(x.len(), y.len(), "x and y must have the same length");
    assert_eq!(
        colors.len(),
        x.len(),
        "colors must have one entry per vertex"
    );

    let tri = delaunay(x, y);
    if tri.is_empty() {
        return None;
    }

    let indices: Vec<[u32; 3]> = tri
        .triangles
        .iter()
        .map(|t| {
            [
                u32::try_from(t[0]).expect("vertex index fits in u32"),
                u32::try_from(t[1]).expect("vertex index fits in u32"),
                u32::try_from(t[2]).expect("vertex index fits in u32"),
            ]
        })
        .collect();

    Some(Triangles::new(
        x.to_vec(),
        y.to_vec(),
        indices,
        colors.to_vec(),
    ))
}

/// Compute the `(dim0+1) × (dim1+1)` grid of quadrilateral cell corners for a
/// `dim0 × dim1` row-major grid of points, faithful to silx
/// `_quadrilateral_grid_coords` (items/scatter.py:191-235). Each interior
/// corner is the mean of its four surrounding points; the edge midpoints and
/// the four outer corners are linearly extrapolated so every input point sits
/// inside its own cell. `points[r * dim1 + c]` is the point at grid row `r`,
/// column `c`. Requires `dim0 >= 2` and `dim1 >= 2`.
fn quadrilateral_grid_coords(points: &[[f64; 2]], dim0: usize, dim1: usize) -> Vec<[f64; 2]> {
    debug_assert!(dim0 >= 2 && dim1 >= 2);
    debug_assert_eq!(points.len(), dim0 * dim1);
    let gw = dim1 + 1; // corner-grid width
    let mut grid = vec![[0.0_f64; 2]; (dim0 + 1) * gw];
    let p = |r: usize, c: usize| points[r * dim1 + c];
    // Mean of the four points around interior corner (r+1, c+1) of the corner
    // grid (silx inner_points); `r`,`c` index the dim0-1 × dim1-1 inner block.
    let inner = |r: usize, c: usize| -> [f64; 2] {
        let (a, b, d, e) = (p(r, c), p(r, c + 1), p(r + 1, c), p(r + 1, c + 1));
        [
            (a[0] + b[0] + d[0] + e[0]) / 4.0,
            (a[1] + b[1] + d[1] + e[1]) / 4.0,
        ]
    };
    for r in 0..dim0 - 1 {
        for c in 0..dim1 - 1 {
            grid[(r + 1) * gw + (c + 1)] = inner(r, c);
        }
    }
    // Vertical sides: left corner column (index 0) and right (index dim1).
    // silx: x = points[r][cc] + points[r+1][cc] - inner.x ; y = inner.y.
    for r in 0..dim0 - 1 {
        let il = inner(r, 0);
        grid[(r + 1) * gw] = [p(r, 0)[0] + p(r + 1, 0)[0] - il[0], il[1]];
        let ir = inner(r, dim1 - 2);
        grid[(r + 1) * gw + dim1] = [p(r, dim1 - 1)[0] + p(r + 1, dim1 - 1)[0] - ir[0], ir[1]];
    }
    // Horizontal sides: top corner row (index 0) and bottom (index dim0).
    // silx: x = inner.x ; y = points[rr][c] + points[rr][c+1] - inner.y.
    for c in 0..dim1 - 1 {
        let it = inner(0, c);
        grid[c + 1] = [it[0], p(0, c)[1] + p(0, c + 1)[1] - it[1]];
        let ib = inner(dim0 - 2, c);
        grid[dim0 * gw + (c + 1)] = [ib[0], p(dim0 - 1, c)[1] + p(dim0 - 1, c + 1)[1] - ib[1]];
    }
    // Four outer corners: grid_corner = 2 * point_corner - inner_corner.
    let corner = |pr: usize, pc: usize, ir: usize, ic: usize| -> [f64; 2] {
        let (pp, ii) = (p(pr, pc), inner(ir, ic));
        [2.0 * pp[0] - ii[0], 2.0 * pp[1] - ii[1]]
    };
    grid[0] = corner(0, 0, 0, 0);
    grid[dim1] = corner(0, dim1 - 1, 0, dim1 - 2);
    grid[dim0 * gw + dim1] = corner(dim0 - 1, dim1 - 1, dim0 - 2, dim1 - 2);
    grid[dim0 * gw] = corner(dim0 - 1, 0, dim0 - 2, 0);
    grid
}

/// Build the quadrilateral grid as triangles for a `dim0 × dim1` row-major grid
/// of points, faithful to silx `_quadrilateral_grid_as_triangles`
/// (items/scatter.py:238-263). Returns `(coords, indices)` with `4 * N` corner
/// vertices and `2 * N` triangles (`N = dim0 * dim1`): point `k`'s quad owns
/// vertices `4k..4k+4` and triangles `2k`/`2k+1`, so a picked triangle `t` maps
/// to source point `t / 2` (silx's vertex `// 4`).
fn quadrilateral_grid_as_triangles(
    points: &[[f64; 2]],
    dim0: usize,
    dim1: usize,
) -> (Vec<[f64; 2]>, Vec<[u32; 3]>) {
    let nbpoints = dim0 * dim1;
    let grid = quadrilateral_grid_coords(points, dim0, dim1);
    let gw = dim1 + 1;
    let mut coords = vec![[0.0_f64; 2]; 4 * nbpoints];
    for r in 0..dim0 {
        for c in 0..dim1 {
            let k = r * dim1 + c;
            coords[4 * k] = grid[r * gw + c];
            coords[4 * k + 1] = grid[(r + 1) * gw + c];
            coords[4 * k + 2] = grid[r * gw + (c + 1)];
            coords[4 * k + 3] = grid[(r + 1) * gw + (c + 1)];
        }
    }
    let mut indices = Vec::with_capacity(2 * nbpoints);
    for k in 0..nbpoints {
        let b = u32::try_from(4 * k).expect("vertex index fits in u32");
        indices.push([b, b + 1, b + 2]);
        indices.push([b + 1, b + 2, b + 3]);
    }
    (coords, indices)
}

/// Arrange unstructured `(x, y)` points onto a detected grid for
/// `Visualization.IRREGULAR_GRID`, faithful to silx's points-array construction
/// (items/scatter.py:682-775). Returns `(points, dim0, dim1, swap_xy)`: a flat
/// row-major `dim0 × dim1` grid whose first `x.len()` entries are the input
/// points in data order (later entries pad an incomplete grid), and `swap_xy`
/// telling the caller to swap the resulting coordinates back (silx stores a
/// column-major grid transposed as `(y, x)`).
///
/// Returns `None` when fewer than two points are given (silx renders a single
/// point as a square marker, handled by the caller) or no grid can be guessed.
fn arrange_irregular_grid_points(
    x: &[f64],
    y: &[f64],
) -> Option<(Vec<[f64; 2]>, usize, usize, bool)> {
    let nbpoints = x.len();
    if nbpoints < 2 {
        return None;
    }
    let grid = detect_regular_grid(x, y)?;
    let (mut s0, mut s1) = grid.shape; // (rows, cols)
    let order = grid.order;
    // silx: grow the shape so it includes every point.
    if nbpoints != s0 * s1 {
        match order {
            GridMajorOrder::Row => s0 = nbpoints.div_ceil(s1),
            GridMajorOrder::Column => s1 = nbpoints.div_ceil(s0),
        }
    }

    // Single-line case (silx 721-741): a grid dimension collapses to < 2.
    if s0 < 2 || s1 < 2 {
        let row_order = s0 == 1;
        // First line in silx's (a, b) convention: (x, y) row, (y, x) column.
        let line: Vec<[f64; 2]> = (0..nbpoints)
            .map(|i| {
                if row_order {
                    [x[i], y[i]]
                } else {
                    [y[i], x[i]]
                }
            })
            .collect();
        // Second line: each point offset by the perpendicular of the local
        // segment direction — silx's cross with the +z axis, (dx, dy) ↦ (dy, -dx)
        // — so the swept cells have area. The last point reuses the prior step.
        let mut points = Vec::with_capacity(2 * nbpoints);
        points.extend_from_slice(&line);
        for i in 0..nbpoints {
            let (dx, dy) = if i + 1 < nbpoints {
                (line[i + 1][0] - line[i][0], line[i + 1][1] - line[i][1])
            } else {
                (line[i][0] - line[i - 1][0], line[i][1] - line[i - 1][1])
            };
            points.push([line[i][0] + dy, line[i][1] - dx]);
        }
        return Some((points, 2, nbpoints, !row_order));
    }

    // Full / partial 2D grid (silx 743-775).
    let total = s0 * s1;
    let mut points = vec![[0.0_f64; 2]; total];
    match order {
        GridMajorOrder::Row => {
            for i in 0..nbpoints {
                points[i] = [x[i], y[i]];
            }
            if nbpoints != total {
                // Pad the incomplete last row with a tail slice of x and the
                // last y (silx 744-755).
                let index = (nbpoints / s1) * s1; // start of last full row
                let pad = total - nbpoints;
                let last_y = y[nbpoints - 1];
                for j in 0..pad {
                    points[nbpoints + j] = [x[index - pad + j], last_y];
                }
            }
            Some((points, s0, s1, false))
        }
        GridMajorOrder::Column => {
            // silx stores column-major as (y, x) with dims transposed.
            for i in 0..nbpoints {
                points[i] = [y[i], x[i]];
            }
            if nbpoints != total {
                let index = (nbpoints / s0) * s0; // start of last full column
                let pad = total - nbpoints;
                let last_x = x[nbpoints - 1];
                for j in 0..pad {
                    points[nbpoints + j] = [y[index - pad + j], last_x];
                }
            }
            Some((points, s1, s0, true))
        }
    }
}

/// Build the per-vertex-colored [`Triangles`] for `Visualization.IRREGULAR_GRID`,
/// faithful to silx's quadrilateral-grid render (items/scatter.py:682-797).
///
/// The points are arranged onto the detected grid
/// ([`arrange_irregular_grid_points`]), expanded into a dual grid of cell
/// corners, and emitted as `2` flat-shaded triangles per input point: every
/// point owns four consecutive vertices carrying its colormapped `colors[k]`
/// (silx `gridcolors[first::4] = rgbacolors[:nbpoints]`). The caller maps
/// `value` through a colormap before calling, as silx's `__applyColormapToData`
/// does.
///
/// `x`, `y`, and `colors` must have equal length. Returns `None` when the points
/// do not form a guessable grid or fewer than two are given (silx renders a lone
/// point as a square marker).
#[must_use]
pub fn irregular_grid_triangles(x: &[f64], y: &[f64], colors: &[Color32]) -> Option<Triangles> {
    assert_eq!(x.len(), y.len(), "x and y must have the same length");
    assert_eq!(
        colors.len(),
        x.len(),
        "colors must have one entry per point"
    );
    let nbpoints = x.len();
    let (points, dim0, dim1, swap_xy) = arrange_irregular_grid_points(x, y)?;
    let (mut coords, mut indices) = quadrilateral_grid_as_triangles(&points, dim0, dim1);
    // Keep only the real points' quads (silx coords[:4*nb], indices[:2*nb]).
    coords.truncate(4 * nbpoints);
    indices.truncate(2 * nbpoints);
    let (vx, vy): (Vec<f64>, Vec<f64>) = if swap_xy {
        coords.iter().map(|c| (c[1], c[0])).unzip()
    } else {
        coords.iter().map(|c| (c[0], c[1])).unzip()
    };
    // Flat-shade each cell: point k's four vertices share colors[k].
    let mut vcolors = Vec::with_capacity(4 * nbpoints);
    for &c in colors {
        vcolors.extend_from_slice(&[c; 4]);
    }
    Some(Triangles::new(vx, vy, indices, vcolors))
}

/// The scatter point index for an `IRREGULAR_GRID` pick at data coordinates
/// `(px, py)`, mirroring silx `Scatter.pick`'s IRREGULAR_GRID branch
/// (items/scatter.py:810-813: picked vertex `// 4`).
///
/// `mesh` is the triangle mesh built by [`irregular_grid_triangles`]. The first
/// triangle whose interior (with a tiny edge tolerance) contains the cursor maps
/// to its source point via `triangle_index / 2` (each point owns two triangles).
/// Returns `None` when the cursor lies outside every cell.
#[must_use]
pub fn irregular_grid_pick(mesh: &Triangles, px: f64, py: f64) -> Option<usize> {
    for (t, tri) in mesh.indices.iter().enumerate() {
        let v = |i: usize| [mesh.x[i], mesh.y[i]];
        let (a, b, c) = (v(tri[0] as usize), v(tri[1] as usize), v(tri[2] as usize));
        if barycentric(a, b, c, [px, py]).is_some() {
            return Some(t / 2);
        }
    }
    None
}

/// Barycentric coordinates of point `p` within triangle `(a, b, c)`.
///
/// Returns `None` when `p` lies outside the triangle (any coordinate negative
/// beyond `eps`) or the triangle is degenerate (zero area). Coordinates sum to
/// `1` and weight `a`, `b`, `c` respectively.
fn barycentric(a: [f64; 2], b: [f64; 2], c: [f64; 2], p: [f64; 2]) -> Option<[f64; 3]> {
    let det = orient2d(a, b, c);
    if det == 0.0 {
        return None;
    }
    // w_a uses (b, c), w_b uses (c, a), w_c uses (a, b); each is the sub-triangle
    // signed area over the total, all divided by the same det.
    let wa = orient2d(b, c, p) / det;
    let wb = orient2d(c, a, p) / det;
    let wc = orient2d(a, b, p) / det;
    // Allow a tiny tolerance on edges so vertices/edges sample as inside.
    let eps = -1e-9;
    if wa >= eps && wb >= eps && wc >= eps {
        Some([wa, wb, wc])
    } else {
        None
    }
}

/// Linearly interpolate the value at `p` over a triangulation by barycentric
/// weighting of the three triangle-vertex values (silx `LinearTriInterpolator`).
///
/// Returns `None` when `p` lies outside every triangle of the triangulation —
/// matching `LinearTriInterpolator`, which yields masked (no) values outside the
/// convex hull.
///
/// `values[i]` is the value at point `(x[i], y[i])`. The three arrays must have
/// equal length and match the indices stored in `tri`.
#[must_use]
pub fn interpolate(
    tri: &Triangulation,
    x: &[f64],
    y: &[f64],
    values: &[f64],
    px: f64,
    py: f64,
) -> Option<f64> {
    let p = [px, py];
    for t in &tri.triangles {
        let a = [x[t[0]], y[t[0]]];
        let b = [x[t[1]], y[t[1]]];
        let c = [x[t[2]], y[t[2]]];
        if let Some([wa, wb, wc]) = barycentric(a, b, c, p) {
            return Some(wa * values[t[0]] + wb * values[t[1]] + wc * values[t[2]]);
        }
    }
    None
}

/// A line profile sampled across scattered data — the result of
/// [`scatter_line_profile`] (silx `ScatterProfileToolBar` profile).
#[derive(Clone, Debug, PartialEq, Default)]
pub struct ScatterLineProfile {
    /// Sample positions `[x, y]` evenly spaced along the profile segment
    /// (silx `points` from `numpy.linspace`).
    pub points: Vec<[f64; 2]>,
    /// Interpolated value at each sample, index-aligned with `points`. `None`
    /// where the sample falls outside the scatter's convex hull (silx `NaN`).
    pub values: Vec<Option<f64>>,
}

impl ScatterLineProfile {
    /// Convert the profile to a `(distance, value)` curve for plotting against
    /// distance along the segment — the form silx `ScatterProfileToolBar` shows
    /// in its profile window. `distance[i]` is the Euclidean distance from the
    /// first sample to `points[i]` (`0` at the start, increasing along the line);
    /// `value[i]` is the interpolated value with out-of-hull samples (`None`)
    /// mapped to `f64::NAN` so they render as gaps (silx keeps them `NaN`).
    /// Returns empty vectors for an empty profile.
    #[must_use]
    pub fn distance_value_curve(&self) -> (Vec<f64>, Vec<f64>) {
        let Some(&[x0, y0]) = self.points.first() else {
            return (Vec::new(), Vec::new());
        };
        let distance = self
            .points
            .iter()
            .map(|&[x, y]| (x - x0).hypot(y - y0))
            .collect();
        let value = self.values.iter().map(|v| v.unwrap_or(f64::NAN)).collect();
        (distance, value)
    }
}

/// Sample a line profile across scattered `(x, y, values)` data — silx
/// `ScatterProfileToolBar` / `_computeProfile` (`tools/profile/rois.py:737-762`).
///
/// Places `n_points` samples evenly along the segment `start`..`end`
/// (`numpy.linspace(.., endpoint=True)`) and interpolates each through the
/// scatter's Delaunay triangulation (silx `LinearNDInterpolator`, via
/// [`delaunay`] + [`interpolate`]): a sample outside the convex hull (no
/// containing triangle) yields `None`, mirroring silx's `NaN`. Returns the
/// sample positions paired with their interpolated values.
///
/// Fewer than 3 input points — or collinear points — build no triangles, so
/// every value is `None`. With `n_points == 1` the `start` point is the sole
/// sample; `n_points == 0` returns empty vectors. `values` must be index-aligned
/// with `x`/`y` (same length), like [`interpolate`].
pub fn scatter_line_profile(
    x: &[f64],
    y: &[f64],
    values: &[f64],
    start: (f64, f64),
    end: (f64, f64),
    n_points: usize,
) -> ScatterLineProfile {
    let tri = delaunay(x, y);
    let mut points = Vec::with_capacity(n_points);
    let mut profile = Vec::with_capacity(n_points);
    for i in 0..n_points {
        let t = if n_points <= 1 {
            0.0
        } else {
            i as f64 / (n_points - 1) as f64
        };
        let px = start.0 + (end.0 - start.0) * t;
        let py = start.1 + (end.1 - start.1) * t;
        points.push([px, py]);
        profile.push(interpolate(&tri, x, y, values, px, py));
    }
    ScatterLineProfile {
        points,
        values: profile,
    }
}

/// An image grid of interpolated/binned values with an affine data placement
/// (silx `addImage(data, origin, scale)`).
#[derive(Clone, Debug, PartialEq)]
pub struct GridImage {
    /// Row-major value grid, `shape.0` rows by `shape.1` columns. Cells with no
    /// value hold `NaN`.
    pub data: Vec<f64>,
    /// `(rows, cols)` of `data` (silx `(height, width)`).
    pub shape: (usize, usize),
    /// Data coordinate of the lower-left pixel center origin `(x, y)`
    /// (silx `origin`).
    pub origin: (f64, f64),
    /// Data-space size of one pixel `(sx, sy)` (silx `scale`).
    pub scale: (f64, f64),
}

impl GridImage {
    /// Value at row `r`, column `c` (row-major), or `None` if out of bounds.
    #[must_use]
    pub fn get(&self, r: usize, c: usize) -> Option<f64> {
        if r < self.shape.0 && c < self.shape.1 {
            Some(self.data[r * self.shape.1 + c])
        } else {
            None
        }
    }

    /// The grid cell `(row, col)` containing data coordinates `(x, y)`, or `None`
    /// when the point falls outside the image. Mirrors a backend image pick:
    /// `col = ⌊(x − ox) / sx⌋`, `row = ⌊(y − oy) / sy⌋`, bounds-checked against
    /// the shape. A negative scale (a reversed-bounds regular grid) is handled —
    /// numerator and scale flip sign together so the cell index stays in range.
    #[must_use]
    pub fn cell(&self, x: f64, y: f64) -> Option<(usize, usize)> {
        grid_cell(self.shape, self.origin, self.scale, x, y)
    }
}

/// Map data coordinates `(x, y)` to the row-major grid cell `(row, col)` for a
/// grid of `shape` (rows, cols) placed at `origin` with per-axis `scale`, or
/// `None` when the point is outside the grid or `scale` is zero / non-finite.
/// Shared by [`GridImage::cell`] and [`BinnedStatistic::pick`].
fn grid_cell(
    shape: (usize, usize),
    origin: (f64, f64),
    scale: (f64, f64),
    x: f64,
    y: f64,
) -> Option<(usize, usize)> {
    let (sx, sy) = scale;
    if sx == 0.0 || sy == 0.0 {
        return None;
    }
    let cf = (x - origin.0) / sx;
    let rf = (y - origin.1) / sy;
    if !cf.is_finite() || !rf.is_finite() || cf < 0.0 || rf < 0.0 {
        return None;
    }
    let col = cf.floor() as usize;
    let row = rf.floor() as usize;
    if row < shape.0 && col < shape.1 {
        Some((row, col))
    } else {
        None
    }
}

/// The scatter point index for a [`crate::core::scatter_viz`]
/// `Visualization.REGULAR_GRID` pick at data coordinates `(x, y)`, mirroring silx
/// `Scatter.pick` REGULAR_GRID branch (items/scatter.py:815-835).
///
/// `image` is the rendered grid (its `shape`/`origin`/`scale`), `order` the grid
/// major order, and `point_count` the number of scatter points. The picked image
/// cell `(row, col)` maps to a source index by the major order — `row * cols +
/// col` for [`GridMajorOrder::Row`], `row + col * rows` for
/// [`GridMajorOrder::Column`] (siplot stores column-major points transposed into
/// the row-major image, so this inverts that placement). Returns `None` when the
/// cursor is off the grid, or when the cell maps past the last point (silx: "image
/// can be larger than scatter").
#[must_use]
pub fn regular_grid_pick(
    image: &GridImage,
    order: GridMajorOrder,
    point_count: usize,
    x: f64,
    y: f64,
) -> Option<usize> {
    let (row, col) = image.cell(x, y)?;
    let (rows, cols) = image.shape;
    let index = match order {
        GridMajorOrder::Row => row * cols + col,
        GridMajorOrder::Column => row + col * rows,
    };
    if index < point_count {
        Some(index)
    } else {
        None
    }
}

/// Rasterize a triangulation to a `rows x cols` value image by barycentric
/// linear interpolation, for `Visualization.IRREGULAR_GRID`.
///
/// The image covers the axis-aligned bounding box of the finite input points.
/// Pixel `(r, c)` samples the value at the pixel center; pixels whose center
/// falls outside the triangulated convex hull are left `NaN` (no value), like
/// `LinearTriInterpolator`'s masked output.
///
/// Returns `None` when the points cannot be triangulated or `rows`/`cols` is 0.
#[must_use]
pub fn irregular_grid_image(
    x: &[f64],
    y: &[f64],
    values: &[f64],
    rows: usize,
    cols: usize,
) -> Option<GridImage> {
    assert_eq!(x.len(), y.len(), "x and y must have the same length");
    assert_eq!(
        values.len(),
        x.len(),
        "values must have one entry per point"
    );
    if rows == 0 || cols == 0 {
        return None;
    }

    let tri = delaunay(x, y);
    if tri.is_empty() {
        return None;
    }

    let (mut min_x, mut min_y, mut max_x, mut max_y) = (
        f64::INFINITY,
        f64::INFINITY,
        f64::NEG_INFINITY,
        f64::NEG_INFINITY,
    );
    for (&xi, &yi) in x.iter().zip(y) {
        if xi.is_finite() && yi.is_finite() {
            min_x = min_x.min(xi);
            min_y = min_y.min(yi);
            max_x = max_x.max(xi);
            max_y = max_y.max(yi);
        }
    }

    // Pixel size: span divided by the number of pixels.
    let sx = if cols > 0 {
        (max_x - min_x) / cols as f64
    } else {
        1.0
    };
    let sy = if rows > 0 {
        (max_y - min_y) / rows as f64
    } else {
        1.0
    };

    let mut data = vec![f64::NAN; rows * cols];
    for r in 0..rows {
        // Pixel-center Y, row 0 at the bottom (data min).
        let py = min_y + (r as f64 + 0.5) * sy;
        for c in 0..cols {
            let px = min_x + (c as f64 + 0.5) * sx;
            if let Some(v) = interpolate(&tri, x, y, values, px, py) {
                data[r * cols + c] = v;
            }
        }
    }

    Some(GridImage {
        data,
        shape: (rows, cols),
        origin: (min_x, min_y),
        scale: (sx, sy),
    })
}

// ===========================================================================
// 3. REGULAR_GRID detection
// ===========================================================================

/// The detected shape and ordering of a regular grid (silx `_guess_grid`
/// result + `_RegularGridInfo.order`).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct RegularGrid {
    /// `(height, width)` = `(rows, cols)` of the grid (silx shape convention).
    pub shape: (usize, usize),
    /// Major order of the points (silx `GRID_MAJOR_ORDER`).
    pub order: GridMajorOrder,
}

/// Length of one line of a Z-like (snake-free, same-direction) regular grid
/// from a coordinate array, faithful to silx `_get_z_line_length`.
///
/// Looks at the sign of consecutive differences: a regular grid scanned the
/// same way each line shows the fast coordinate stepping in one sign, then a
/// single reversal (the opposite sign) at each line boundary. The line length
/// is the constant spacing between those reversals. Returns 0 when no constant
/// line length is found.
fn get_z_line_length(array: &[f64]) -> usize {
    if array.len() < 2 {
        return 0;
    }
    let sign: Vec<i8> = array
        .windows(2)
        .map(|w| {
            let d = w[1] - w[0];
            if d > 0.0 {
                1
            } else if d < 0.0 {
                -1
            } else {
                0
            }
        })
        .collect();
    // silx: if no diffs or the first diff is flat, give up.
    if sign.is_empty() || sign[0] == 0 {
        return 0;
    }
    let first = sign[0];
    // Indices (in the original array) where the coordinate reverses direction:
    // these mark the beginning of a new line. silx uses `where(sign == -sign[0]) + 1`.
    let beginnings: Vec<usize> = sign
        .iter()
        .enumerate()
        .filter(|&(_, &s)| s == -first)
        .map(|(i, _)| i + 1)
        .collect();
    if beginnings.is_empty() {
        return 0;
    }
    let length = beginnings[0];
    // All inter-beginning gaps must equal the first line length.
    let uniform = beginnings.windows(2).all(|w| w[1] - w[0] == length);
    if uniform { length } else { 0 }
}

/// Guess a Z-like regular grid shape from `(x, y)` coordinates, faithful to silx
/// `_guess_z_grid_shape`.
///
/// Tries X as the fast (row-major) dimension first; if X yields a line length,
/// the grid is row-major with that width. Otherwise tries Y as the fast
/// (column-major) dimension. Returns `None` when neither yields a line length.
fn guess_z_grid_shape(x: &[f64], y: &[f64]) -> Option<RegularGrid> {
    let n = x.len();
    let width = get_z_line_length(x);
    if width != 0 {
        let height = n.div_ceil(width);
        return Some(RegularGrid {
            shape: (height, width),
            order: GridMajorOrder::Row,
        });
    }
    let height = get_z_line_length(y);
    if height != 0 {
        let width = n.div_ceil(height);
        return Some(RegularGrid {
            shape: (height, width),
            order: GridMajorOrder::Column,
        });
    }
    None
}

/// Whether `array` is monotonic: `1` increasing, `-1` decreasing, `0` neither
/// (silx `is_monotonic`). Equal consecutive elements count as both directions.
fn is_monotonic(array: &[f64]) -> i8 {
    if array.len() < 2 {
        // numpy.diff of length<2 is empty; all() of empty is True for both.
        return 1;
    }
    let diffs: Vec<f64> = array.windows(2).map(|w| w[1] - w[0]).collect();
    if diffs.iter().all(|&d| d >= 0.0) {
        1
    } else if diffs.iter().all(|&d| d <= 0.0) {
        -1
    } else {
        0
    }
}

/// Auto-detect that the points lie on a regular grid, faithful to silx
/// `_guess_grid` (`scatter.py:148-188`).
///
/// First tries a Z-like 2D grid via [`guess_z_grid_shape`]. Failing that, falls
/// back to a single line when either coordinate is monotonic (the line runs
/// along whichever axis varies more), reported as row-major. Returns `None`
/// when the points form neither a grid nor a guessable line.
///
/// `x` and `y` must have equal length.
#[must_use]
pub fn detect_regular_grid(x: &[f64], y: &[f64]) -> Option<RegularGrid> {
    assert_eq!(x.len(), y.len(), "x and y must have the same length");
    if x.is_empty() {
        return None;
    }

    if let Some(grid) = guess_z_grid_shape(x, y) {
        return Some(grid);
    }

    // Fallback: a single line if either coordinate is monotonic.
    let y_monotonic = is_monotonic(y) != 0;
    let x_monotonic = is_monotonic(x) != 0;
    if x_monotonic || y_monotonic {
        let (x_min, x_max) = min_max(x);
        let (y_min, y_max) = min_max(y);
        let shape = if !y_monotonic || (x_max - x_min) >= (y_max - y_min) {
            // line along X
            (1, x.len())
        } else {
            // line along Y
            (y.len(), 1)
        };
        Some(RegularGrid {
            shape,
            order: GridMajorOrder::Row, // order does not matter for a single line
        })
    } else {
        None
    }
}

/// Min and max of `array`, ignoring non-finite values (silx `min_max`).
/// Returns `(NaN, NaN)` for an all-non-finite or empty array.
fn min_max(array: &[f64]) -> (f64, f64) {
    let mut min = f64::INFINITY;
    let mut max = f64::NEG_INFINITY;
    for &v in array {
        if v.is_finite() {
            min = min.min(v);
            max = max.max(v);
        }
    }
    if min > max {
        (f64::NAN, f64::NAN)
    } else {
        (min, max)
    }
}

// ===========================================================================
// 4. BINNED_STATISTIC
// ===========================================================================

/// Per-bin statistics over a 2D binning of scatter points (silx
/// `_HistogramInfo` / `Histogramnd`, `core.py:1325-1329`).
///
/// All three grids are row-major, `shape.0` rows (Y) by `shape.1` columns (X),
/// matching silx's `(height, width)` convention.
#[derive(Clone, Debug, PartialEq)]
pub struct BinnedStatistic {
    /// Per-bin mean `sum / count`; `NaN` where `count == 0` (silx
    /// `numpy.errstate(divide="ignore")` then `sums / counts`).
    pub mean: Vec<f64>,
    /// Per-bin point count.
    pub count: Vec<u64>,
    /// Per-bin value sum; `0.0` for an empty bin.
    pub sum: Vec<f64>,
    /// `(rows, cols)` = `(height, width)` of the binning (silx shape).
    pub shape: (usize, usize),
    /// Data coordinate of the lower-left bin-edge origin `(x, y)`
    /// (silx `origin = xEdges[0], yEdges[0]`).
    pub origin: (f64, f64),
    /// Data-space bin width `(sx, sy)` (silx `scale`).
    pub scale: (f64, f64),
}

impl BinnedStatistic {
    /// The reduction grid selected by `func`, as `f64` (count promoted),
    /// row-major (silx `getattr(histoInfo, function)`).
    #[must_use]
    pub fn select(&self, func: BinnedStatisticFunction) -> Vec<f64> {
        match func {
            BinnedStatisticFunction::Mean => self.mean.clone(),
            BinnedStatisticFunction::Count => self.count.iter().map(|&c| c as f64).collect(),
            BinnedStatisticFunction::Sum => self.sum.clone(),
        }
    }

    /// The indices of all scatter points falling in the bin under data
    /// coordinates `(px, py)`, mirroring silx `Scatter.pick` BINNED_STATISTIC
    /// branch (items/scatter.py:837-859).
    ///
    /// The picked bin `(row, col)` expands back to its data range
    /// `[ox + sx·col, ox + sx·(col+1)) × [oy + sy·row, oy + sy·(row+1))` and
    /// every point inside it is returned (silx
    /// `numpy.nonzero(logical_and(...))`). The upper bin edges are exclusive, as
    /// in silx. `x` and `y` are the scatter point coordinates; pairs beyond the
    /// shorter slice are ignored. Returns `None` when the cursor is off the grid
    /// or no point lies in the bin (silx returns no pick).
    #[must_use]
    pub fn pick(&self, x: &[f64], y: &[f64], px: f64, py: f64) -> Option<Vec<usize>> {
        let (row, col) = grid_cell(self.shape, self.origin, self.scale, px, py)?;
        let (ox, oy) = self.origin;
        let (sx, sy) = self.scale;
        let x_lo = ox + sx * col as f64;
        let x_hi = ox + sx * (col + 1) as f64;
        let y_lo = oy + sy * row as f64;
        let y_hi = oy + sy * (row + 1) as f64;
        let indices: Vec<usize> = x
            .iter()
            .zip(y.iter())
            .enumerate()
            .filter(|&(_, (&xi, &yi))| xi >= x_lo && xi < x_hi && yi >= y_lo && yi < y_hi)
            .map(|(i, _)| i)
            .collect();
        if indices.is_empty() {
            None
        } else {
            Some(indices)
        }
    }
}

/// Bin `(x, y, value)` points into a `rows x cols` grid over the data extent and
/// compute per-bin mean/count/sum (silx `Visualization.BINNED_STATISTIC`,
/// `scatter.py::__getHistogramInfo`).
///
/// The grid spans the finite-point bounding box of X and Y. A point is assigned
/// to the bin `floor((coord - min) / binsize)`, with the upper edge clamped into
/// the last bin so the maximum point is included (matching `Histogramnd`'s
/// inclusive last edge). Non-finite points and points with a non-finite value
/// are skipped.
///
/// Returns `None` when there are no finite points or `rows`/`cols` is 0.
///
/// `x`, `y`, and `values` must have equal length.
#[must_use]
pub fn binned_statistic(
    x: &[f64],
    y: &[f64],
    values: &[f64],
    rows: usize,
    cols: usize,
) -> Option<BinnedStatistic> {
    assert_eq!(x.len(), y.len(), "x and y must have the same length");
    assert_eq!(
        values.len(),
        x.len(),
        "values must have one entry per point"
    );
    if rows == 0 || cols == 0 {
        return None;
    }

    let (x_min, x_max) = min_max(x);
    let (y_min, y_max) = min_max(y);
    if !x_min.is_finite() || !y_min.is_finite() {
        return None; // no finite points
    }

    // Bin sizes; degenerate (single-valued) ranges get a unit bin to avoid /0.
    let sx = {
        let span = x_max - x_min;
        if span > 0.0 { span / cols as f64 } else { 1.0 }
    };
    let sy = {
        let span = y_max - y_min;
        if span > 0.0 { span / rows as f64 } else { 1.0 }
    };

    let mut count = vec![0u64; rows * cols];
    let mut sum = vec![0.0f64; rows * cols];

    for ((&xi, &yi), &vi) in x.iter().zip(y).zip(values) {
        if !xi.is_finite() || !yi.is_finite() || !vi.is_finite() {
            continue;
        }
        // Column from X, row from Y.
        let mut c = ((xi - x_min) / sx).floor() as isize;
        let mut r = ((yi - y_min) / sy).floor() as isize;
        // Clamp the inclusive upper edge into the last bin.
        if c >= cols as isize {
            c = cols as isize - 1;
        }
        if r >= rows as isize {
            r = rows as isize - 1;
        }
        if c < 0 || r < 0 {
            continue; // outside the lower edge (only non-finite would do this)
        }
        let idx = r as usize * cols + c as usize;
        count[idx] += 1;
        sum[idx] += vi;
    }

    let mean: Vec<f64> = count
        .iter()
        .zip(&sum)
        .map(|(&c, &s)| if c == 0 { f64::NAN } else { s / c as f64 })
        .collect();

    Some(BinnedStatistic {
        mean,
        count,
        sum,
        shape: (rows, cols),
        origin: (x_min, y_min),
        scale: (sx, sy),
    })
}

// ===========================================================================
// 5. Per-point alpha (POINTS-mode data carrier)
// ===========================================================================

/// `Visualization.POINTS` data carrier with optional per-point alpha (silx
/// `Scatter` per-point alpha array, applied in `__applyColormapToData`).
///
/// Pure data only; GPU blending of the per-point alpha is deferred to a later
/// wave. Colors are pre-mapped through the colormap by the caller, as in silx.
#[derive(Clone, Debug, PartialEq)]
pub struct PointsViz {
    /// Point X coordinates.
    pub x: Vec<f64>,
    /// Point Y coordinates (same length as `x`).
    pub y: Vec<f64>,
    /// Per-point value (same length as `x`).
    pub values: Vec<f64>,
    /// Per-point colormap colors (same length as `x`).
    pub colors: Vec<Color32>,
    /// Optional per-point alpha in `[0, 1]` (silx `__alpha`), same length as `x`
    /// when present. `None` means a uniform global alpha applies instead.
    pub alpha: Option<Vec<f64>>,
}

impl PointsViz {
    /// Build a POINTS carrier with no per-point alpha. Panics if `y`, `values`,
    /// or `colors` do not match `x` in length.
    #[must_use]
    pub fn new(x: Vec<f64>, y: Vec<f64>, values: Vec<f64>, colors: Vec<Color32>) -> Self {
        assert_eq!(x.len(), y.len(), "x and y must have the same length");
        assert_eq!(
            values.len(),
            x.len(),
            "values must have one entry per point"
        );
        assert_eq!(
            colors.len(),
            x.len(),
            "colors must have one entry per point"
        );
        Self {
            x,
            y,
            values,
            colors,
            alpha: None,
        }
    }

    /// Attach a per-point alpha array (silx `setData(..., alpha=...)`), each
    /// clamped to `[0, 1]`. Panics if `alpha` does not match `x` in length.
    #[must_use]
    pub fn with_alpha(mut self, alpha: Vec<f64>) -> Self {
        assert_eq!(
            alpha.len(),
            self.x.len(),
            "alpha must have one entry per point"
        );
        self.alpha = Some(alpha.into_iter().map(|a| a.clamp(0.0, 1.0)).collect());
        self
    }

    /// Number of points.
    #[must_use]
    pub fn len(&self) -> usize {
        self.x.len()
    }

    /// Whether there are no points.
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.x.is_empty()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    // --- Delaunay triangulation ---------------------------------------------

    #[test]
    fn delaunay_three_points_one_triangle() {
        let tri = delaunay(&[0.0, 1.0, 0.0], &[0.0, 0.0, 1.0]);
        assert_eq!(tri.len(), 1);
        // The single triangle references all three input points.
        let mut refs = tri.triangles[0];
        refs.sort_unstable();
        assert_eq!(refs, [0, 1, 2]);
    }

    #[test]
    fn delaunay_four_convex_points_two_triangles() {
        // Unit square, convex position -> exactly two triangles.
        let x = [0.0, 1.0, 1.0, 0.0];
        let y = [0.0, 0.0, 1.0, 1.0];
        let tri = delaunay(&x, &y);
        assert_eq!(tri.len(), 2);
        // Every input point is referenced by at least one triangle.
        let mut seen = [false; 4];
        for t in &tri.triangles {
            for &v in t {
                seen[v] = true;
            }
        }
        assert!(seen.iter().all(|&s| s), "every input point referenced");
    }

    #[test]
    fn delaunay_collinear_points_empty() {
        // All on the line y = x: no triangle has area.
        let tri = delaunay(&[0.0, 1.0, 2.0, 3.0], &[0.0, 1.0, 2.0, 3.0]);
        assert!(tri.is_empty(), "collinear input -> empty triangulation");
    }

    #[test]
    fn delaunay_fewer_than_three_points_empty() {
        assert!(delaunay(&[0.0, 1.0], &[0.0, 1.0]).is_empty());
        assert!(delaunay(&[], &[]).is_empty());
    }

    #[test]
    fn delaunay_ignores_non_finite_points() {
        let x = [0.0, 1.0, 0.0, f64::NAN];
        let y = [0.0, 0.0, 1.0, 5.0];
        let tri = delaunay(&x, &y);
        // The NaN point is dropped, leaving a single triangle over indices 0,1,2.
        assert_eq!(tri.len(), 1);
        for t in &tri.triangles {
            assert!(
                t.iter().all(|&v| v < 3),
                "no triangle references the NaN point"
            );
        }
    }

    #[test]
    fn delaunay_property_no_point_inside_circumcircle() {
        // Fixed small set in general position.
        let x = [0.0, 1.0, 2.0, 0.5, 1.5, 1.0];
        let y = [0.0, 0.2, 0.0, 1.0, 1.1, 2.0];
        let tri = delaunay(&x, &y);
        assert!(!tri.is_empty());
        // Delaunay property: no input point lies strictly inside any triangle's
        // circumcircle (excluding the triangle's own vertices).
        for t in &tri.triangles {
            let a = [x[t[0]], y[t[0]]];
            let b = [x[t[1]], y[t[1]]];
            let c = [x[t[2]], y[t[2]]];
            let (a, b, c) = if orient2d(a, b, c) < 0.0 {
                (a, c, b)
            } else {
                (a, b, c)
            };
            for i in 0..x.len() {
                if i == t[0] || i == t[1] || i == t[2] {
                    continue;
                }
                let p = [x[i], y[i]];
                assert!(
                    !in_circumcircle(a, b, c, p),
                    "point {i} inside circumcircle of triangle {t:?}"
                );
            }
        }
    }

    // --- SOLID --------------------------------------------------------------

    #[test]
    fn solid_triangles_colors_each_vertex() {
        let x = [0.0, 1.0, 0.0];
        let y = [0.0, 0.0, 1.0];
        let colors = [Color32::RED, Color32::GREEN, Color32::BLUE];
        let t = solid_triangles(&x, &y, &colors).expect("triangulable");
        assert_eq!(t.indices.len(), 1);
        assert_eq!(t.colors, colors);
        assert_eq!(t.x, x);
        assert_eq!(t.y, y);
    }

    #[test]
    fn solid_triangles_none_for_collinear() {
        let x = [0.0, 1.0, 2.0];
        let y = [0.0, 1.0, 2.0];
        let colors = [Color32::RED; 3];
        assert!(solid_triangles(&x, &y, &colors).is_none());
    }

    // --- Barycentric interpolation ------------------------------------------

    #[test]
    fn interpolate_at_vertices_returns_vertex_value() {
        let x = [0.0, 1.0, 0.0];
        let y = [0.0, 0.0, 1.0];
        let values = [10.0, 20.0, 30.0];
        let tri = delaunay(&x, &y);
        for i in 0..3 {
            let v = interpolate(&tri, &x, &y, &values, x[i], y[i]).expect("inside");
            assert!((v - values[i]).abs() < 1e-9, "vertex {i}: {v}");
        }
    }

    #[test]
    fn interpolate_at_centroid_returns_mean() {
        let x = [0.0, 1.0, 0.0];
        let y = [0.0, 0.0, 1.0];
        let values = [10.0, 20.0, 30.0];
        let tri = delaunay(&x, &y);
        let cx = (x[0] + x[1] + x[2]) / 3.0;
        let cy = (y[0] + y[1] + y[2]) / 3.0;
        let v = interpolate(&tri, &x, &y, &values, cx, cy).expect("inside");
        let mean = (10.0 + 20.0 + 30.0) / 3.0;
        assert!((v - mean).abs() < 1e-9, "centroid value {v} != mean {mean}");
    }

    #[test]
    fn interpolate_outside_returns_none() {
        let x = [0.0, 1.0, 0.0];
        let y = [0.0, 0.0, 1.0];
        let values = [10.0, 20.0, 30.0];
        let tri = delaunay(&x, &y);
        // Well outside the triangle.
        assert!(interpolate(&tri, &x, &y, &values, 5.0, 5.0).is_none());
        assert!(interpolate(&tri, &x, &y, &values, -1.0, -1.0).is_none());
    }

    #[test]
    fn scatter_line_profile_interpolates_affine_field_along_line() {
        // Triangle with an affine field v = x + 2y: (0,0)=0, (2,0)=2, (0,2)=4.
        // Linear (barycentric) interpolation reproduces it exactly, so a line
        // from (0,0) to (1,1) samples 0, 1.5, 3.0 at t = 0, 0.5, 1.
        let x = [0.0, 2.0, 0.0];
        let y = [0.0, 0.0, 2.0];
        let values = [0.0, 2.0, 4.0];
        let prof = scatter_line_profile(&x, &y, &values, (0.0, 0.0), (1.0, 1.0), 3);
        assert_eq!(prof.points, vec![[0.0, 0.0], [0.5, 0.5], [1.0, 1.0]]);
        let got: Vec<f64> = prof
            .values
            .iter()
            .map(|v| v.expect("inside hull"))
            .collect();
        for (g, want) in got.iter().zip([0.0, 1.5, 3.0]) {
            assert!((g - want).abs() < 1e-9, "got {g}, want {want}");
        }
    }

    #[test]
    fn scatter_line_profile_outside_hull_is_none() {
        // A segment entirely outside the triangle's convex hull: every sample
        // falls in no triangle, so every value is None (silx NaN).
        let x = [0.0, 2.0, 0.0];
        let y = [0.0, 0.0, 2.0];
        let values = [0.0, 2.0, 4.0];
        let prof = scatter_line_profile(&x, &y, &values, (5.0, 5.0), (9.0, 9.0), 4);
        assert!(prof.values.iter().all(Option::is_none), "{:?}", prof.values);
    }

    #[test]
    fn scatter_line_profile_too_few_points_yields_no_values() {
        // Fewer than 3 input points build no triangles -> all None.
        let x = [0.0, 1.0];
        let y = [0.0, 1.0];
        let values = [1.0, 2.0];
        let prof = scatter_line_profile(&x, &y, &values, (0.0, 0.0), (1.0, 1.0), 2);
        assert_eq!(prof.points.len(), 2);
        assert!(prof.values.iter().all(Option::is_none));
    }

    #[test]
    fn distance_value_curve_is_distance_from_start_with_nan_gaps() {
        // Samples at (0,0),(3,4),(6,8): distances from the first are 0, 5, 10.
        // The middle value is None (out of hull) -> NaN in the curve.
        let prof = ScatterLineProfile {
            points: vec![[0.0, 0.0], [3.0, 4.0], [6.0, 8.0]],
            values: vec![Some(1.0), None, Some(2.0)],
        };
        let (distance, value) = prof.distance_value_curve();
        assert_eq!(distance, vec![0.0, 5.0, 10.0]);
        assert_eq!(value[0], 1.0);
        assert!(value[1].is_nan(), "out-of-hull sample maps to NaN");
        assert_eq!(value[2], 2.0);
    }

    #[test]
    fn distance_value_curve_empty_profile_is_empty() {
        let prof = ScatterLineProfile::default();
        assert_eq!(prof.distance_value_curve(), (Vec::new(), Vec::new()));
    }

    // --- IRREGULAR_GRID image -----------------------------------------------

    #[test]
    fn irregular_grid_image_interpolates_inside_nan_outside() {
        // Right triangle with a plane value field z = x (value equals x coord).
        let x = [0.0, 4.0, 0.0];
        let y = [0.0, 0.0, 4.0];
        let values = [0.0, 4.0, 0.0];
        let img = irregular_grid_image(&x, &y, &values, 4, 4).expect("triangulable");
        assert_eq!(img.shape, (4, 4));
        // Bottom-left pixel center (0.5, 0.5) is inside; value ~= x = 0.5.
        let v = img.get(0, 0).unwrap();
        assert!((v - 0.5).abs() < 1e-9, "interior value {v}");
        // Top-right pixel center (3.5, 3.5) is outside the triangle -> NaN.
        let outside = img.get(3, 3).unwrap();
        assert!(
            outside.is_nan(),
            "exterior pixel should be NaN, got {outside}"
        );
        assert_eq!(img.origin, (0.0, 0.0));
        assert_eq!(img.scale, (1.0, 1.0));
    }

    #[test]
    fn irregular_grid_image_none_for_degenerate() {
        assert!(
            irregular_grid_image(&[0.0, 1.0, 2.0], &[0.0, 1.0, 2.0], &[1.0, 2.0, 3.0], 4, 4)
                .is_none()
        );
        assert!(
            irregular_grid_image(&[0.0, 1.0, 0.0], &[0.0, 0.0, 1.0], &[1.0, 2.0, 3.0], 0, 4)
                .is_none()
        );
    }

    // --- REGULAR_GRID detection ---------------------------------------------

    /// Build a 3 rows x 4 cols row-major grid (X fast).
    fn grid_3x4_row_major() -> (Vec<f64>, Vec<f64>) {
        let (rows, cols) = (3usize, 4usize);
        let mut x = Vec::new();
        let mut y = Vec::new();
        for r in 0..rows {
            for c in 0..cols {
                x.push(c as f64);
                y.push(r as f64);
            }
        }
        (x, y)
    }

    #[test]
    fn detect_regular_grid_row_major_3x4() {
        let (x, y) = grid_3x4_row_major();
        let grid = detect_regular_grid(&x, &y).expect("grid detected");
        assert_eq!(grid.shape, (3, 4));
        assert_eq!(grid.order, GridMajorOrder::Row);
    }

    #[test]
    fn detect_regular_grid_column_major_3x4() {
        // Y fast: fill column 0 top-to-bottom, then column 1, etc. (height=3).
        let (rows, cols) = (3usize, 4usize);
        let mut x = Vec::new();
        let mut y = Vec::new();
        for c in 0..cols {
            for r in 0..rows {
                x.push(c as f64);
                y.push(r as f64);
            }
        }
        let grid = detect_regular_grid(&x, &y).expect("grid detected");
        assert_eq!(grid.shape, (3, 4));
        assert_eq!(grid.order, GridMajorOrder::Column);
    }

    #[test]
    fn detect_regular_grid_rejects_random_scatter() {
        // Both coords have irregular direction reversals (no Z-grid line length)
        // and neither is monotonic -> no grid, no line.
        // x signs: + - + + -  (reversals at idx 2,5: gaps 2 then 3, non-uniform)
        // y signs: - + + - +  (reversals at idx 1,5: gaps 1 then 4, non-uniform)
        let x = [0.0, 1.0, 0.5, 2.0, 3.0, 1.0];
        let y = [2.0, 0.0, 1.0, 3.0, 0.5, 4.0];
        assert!(detect_regular_grid(&x, &y).is_none());
    }

    #[test]
    fn detect_regular_grid_single_line_along_x() {
        // X strictly increasing (monotonic, no Z-grid line length), Y has
        // irregular reversals (no Z-grid, not monotonic) -> falls to the line
        // branch; Y not monotonic forces the line along X -> shape (1, N).
        let x = [0.0, 1.0, 2.0, 3.0, 4.0, 5.0];
        let y = [2.0, 0.0, 1.0, 3.0, 0.5, 4.0];
        let grid = detect_regular_grid(&x, &y).expect("line detected");
        assert_eq!(grid.shape, (1, 6));
    }

    // --- BINNED_STATISTIC ---------------------------------------------------

    #[test]
    fn binned_statistic_2x2_mean_count_sum() {
        // Points in [0,2]x[0,2], 2x2 bins -> bin size 1x1.
        // Bin layout (row=Y, col=X):
        //   (r0,c0): x in [0,1), y in [0,1)
        //   (r0,c1): x in [1,2], y in [0,1)
        //   (r1,c0): x in [0,1), y in [1,2]
        //   (r1,c1): x in [1,2], y in [1,2]
        // Place: two points in (r0,c0); one in (r1,c1); leave (r0,c1),(r1,c0) empty.
        // (0,0) and (2,2) pin the data extent to [0,2]x[0,2] so bins are 1x1.
        let x = [0.0, 0.5, 2.0];
        let y = [0.0, 0.5, 2.0];
        let v = [10.0, 30.0, 7.0];
        let bs = binned_statistic(&x, &y, &v, 2, 2).expect("binned");
        assert_eq!(bs.shape, (2, 2));

        // (r0,c0) = index 0: count 2, sum 40, mean 20.
        assert_eq!(bs.count[0], 2);
        assert!((bs.sum[0] - 40.0).abs() < 1e-12);
        assert!((bs.mean[0] - 20.0).abs() < 1e-12);

        // (r0,c1) = index 1: empty.
        assert_eq!(bs.count[1], 0);
        assert_eq!(bs.sum[1], 0.0);
        assert!(bs.mean[1].is_nan(), "empty bin mean is NaN");

        // (r1,c0) = index 2: empty.
        assert_eq!(bs.count[2], 0);

        // (r1,c1) = index 3: count 1, sum 7, mean 7.
        assert_eq!(bs.count[3], 1);
        assert!((bs.sum[3] - 7.0).abs() < 1e-12);
        assert!((bs.mean[3] - 7.0).abs() < 1e-12);

        // Geometry.
        assert_eq!(bs.origin, (0.0, 0.0));
        assert_eq!(bs.scale, (1.0, 1.0));
    }

    #[test]
    fn binned_statistic_max_point_clamped_into_last_bin() {
        // Point exactly at the max edge must land in the last bin, not overflow.
        let x = [0.0, 2.0];
        let y = [0.0, 2.0];
        let v = [1.0, 2.0];
        let bs = binned_statistic(&x, &y, &v, 2, 2).expect("binned");
        // (2,2) is the upper corner -> last bin (r1,c1) = index 3.
        assert_eq!(bs.count[3], 1);
        assert!((bs.sum[3] - 2.0).abs() < 1e-12);
        // (0,0) -> first bin.
        assert_eq!(bs.count[0], 1);
    }

    #[test]
    fn binned_statistic_select_returns_chosen_grid() {
        let x = [0.2, 1.5];
        let y = [0.2, 1.5];
        let v = [10.0, 7.0];
        let bs = binned_statistic(&x, &y, &v, 2, 2).expect("binned");
        let counts = bs.select(BinnedStatisticFunction::Count);
        assert_eq!(counts, vec![1.0, 0.0, 0.0, 1.0]);
        let sums = bs.select(BinnedStatisticFunction::Sum);
        assert_eq!(sums, vec![10.0, 0.0, 0.0, 7.0]);
        let means = bs.select(BinnedStatisticFunction::Mean);
        assert!((means[0] - 10.0).abs() < 1e-12);
        assert!(means[1].is_nan());
    }

    #[test]
    fn binned_statistic_none_for_empty_or_zero_shape() {
        assert!(binned_statistic(&[], &[], &[], 2, 2).is_none());
        assert!(binned_statistic(&[0.0], &[0.0], &[1.0], 0, 2).is_none());
    }

    #[test]
    fn binned_statistic_skips_non_finite_value() {
        let x = [0.2, 0.4];
        let y = [0.2, 0.4];
        let v = [10.0, f64::NAN];
        let bs = binned_statistic(&x, &y, &v, 2, 2).expect("binned");
        // Only the finite-valued point is counted in bin 0.
        assert_eq!(bs.count[0], 1);
        assert!((bs.sum[0] - 10.0).abs() < 1e-12);
    }

    // --- mode-specific picking (silx Scatter.pick) --------------------------

    #[test]
    fn regular_grid_pick_row_major_maps_cell_to_index() {
        // 2 rows x 3 cols, row-major, unit scale at origin (0,0); 6 points.
        let image = GridImage {
            data: vec![0.0; 6],
            shape: (2, 3),
            origin: (0.0, 0.0),
            scale: (1.0, 1.0),
        };
        // (col 2, row 0) -> 0*3 + 2 = 2.
        assert_eq!(
            regular_grid_pick(&image, GridMajorOrder::Row, 6, 2.5, 0.5),
            Some(2)
        );
        // (col 0, row 1) -> 1*3 + 0 = 3.
        assert_eq!(
            regular_grid_pick(&image, GridMajorOrder::Row, 6, 0.5, 1.5),
            Some(3)
        );
        // Off the grid (col 3 >= cols).
        assert_eq!(
            regular_grid_pick(&image, GridMajorOrder::Row, 6, 3.5, 0.5),
            None
        );
    }

    #[test]
    fn regular_grid_pick_column_major_inverts_transposed_placement() {
        // Column-major points are stored transposed -> index = row + col*rows.
        let image = GridImage {
            data: vec![0.0; 6],
            shape: (2, 3),
            origin: (0.0, 0.0),
            scale: (1.0, 1.0),
        };
        // (col 2, row 1) -> 1 + 2*2 = 5.
        assert_eq!(
            regular_grid_pick(&image, GridMajorOrder::Column, 6, 2.5, 1.5),
            Some(5)
        );
        // (col 1, row 0) -> 0 + 1*2 = 2.
        assert_eq!(
            regular_grid_pick(&image, GridMajorOrder::Column, 6, 1.5, 0.5),
            Some(2)
        );
    }

    #[test]
    fn regular_grid_pick_none_past_last_point() {
        // 6-cell image but only 5 points: the trailing cell maps past the data
        // (silx "image can be larger than scatter").
        let image = GridImage {
            data: vec![0.0; 6],
            shape: (2, 3),
            origin: (0.0, 0.0),
            scale: (1.0, 1.0),
        };
        // Last cell (col 2, row 1) -> index 5 >= point_count 5 -> None.
        assert_eq!(
            regular_grid_pick(&image, GridMajorOrder::Row, 5, 2.5, 1.5),
            None
        );
        // (col 1, row 1) -> index 4 < 5 -> Some(4).
        assert_eq!(
            regular_grid_pick(&image, GridMajorOrder::Row, 5, 1.5, 1.5),
            Some(4)
        );
    }

    #[test]
    fn regular_grid_pick_handles_negative_scale() {
        // Reversed-bounds grid: x descending 10,8,6 -> scale -2, origin 11.
        let image = GridImage {
            data: vec![0.0; 3],
            shape: (1, 3),
            origin: (11.0, -0.5),
            scale: (-2.0, 1.0),
        };
        // x=10 -> col 0; x=6 -> col 2.
        assert_eq!(
            regular_grid_pick(&image, GridMajorOrder::Row, 3, 10.0, 0.0),
            Some(0)
        );
        assert_eq!(
            regular_grid_pick(&image, GridMajorOrder::Row, 3, 6.0, 0.0),
            Some(2)
        );
        // x beyond the begin edge (12 > 11) -> negative cell -> None.
        assert_eq!(
            regular_grid_pick(&image, GridMajorOrder::Row, 3, 12.0, 0.0),
            None
        );
    }

    #[test]
    fn binned_statistic_pick_returns_points_in_bin() {
        // 4 points pin [0,2]x[0,2] into 2x2 unit bins.
        let x = [0.0, 0.5, 1.5, 2.0];
        let y = [0.0, 0.5, 1.5, 2.0];
        let v = [10.0, 30.0, 5.0, 7.0];
        let bs = binned_statistic(&x, &y, &v, 2, 2).expect("binned");
        assert_eq!(bs.origin, (0.0, 0.0));
        assert_eq!(bs.scale, (1.0, 1.0));
        // Bin (row0,col0) range x[0,1) y[0,1): points 0 and 1.
        assert_eq!(bs.pick(&x, &y, 0.5, 0.5), Some(vec![0, 1]));
        // Bin (row1,col1) range x[1,2) y[1,2): point 2 in; point 3 at the max
        // edge is excluded by the strict upper bound (silx behaviour).
        assert_eq!(bs.pick(&x, &y, 1.5, 1.5), Some(vec![2]));
    }

    #[test]
    fn binned_statistic_pick_none_off_grid_or_empty_bin() {
        let x = [0.0, 0.5, 2.0];
        let y = [0.0, 0.5, 2.0];
        let v = [10.0, 30.0, 7.0];
        let bs = binned_statistic(&x, &y, &v, 2, 2).expect("binned");
        // Empty bin (row0,col1): x[1,2) y[0,1) holds no point.
        assert_eq!(bs.pick(&x, &y, 1.5, 0.5), None);
        // Cursor off the grid (right of, and left of).
        assert_eq!(bs.pick(&x, &y, 2.5, 0.5), None);
        assert_eq!(bs.pick(&x, &y, -0.5, 0.5), None);
    }

    // --- Per-point alpha ----------------------------------------------------

    #[test]
    fn points_viz_default_no_alpha() {
        let p = PointsViz::new(
            vec![0.0, 1.0],
            vec![0.0, 1.0],
            vec![5.0, 6.0],
            vec![Color32::RED, Color32::BLUE],
        );
        assert_eq!(p.len(), 2);
        assert!(p.alpha.is_none());
    }

    #[test]
    fn points_viz_with_alpha_clamps() {
        let p = PointsViz::new(
            vec![0.0, 1.0],
            vec![0.0, 1.0],
            vec![5.0, 6.0],
            vec![Color32::RED, Color32::BLUE],
        )
        .with_alpha(vec![-0.5, 2.0]);
        assert_eq!(p.alpha, Some(vec![0.0, 1.0]));
    }

    #[test]
    #[should_panic(expected = "alpha must have one entry per point")]
    fn points_viz_alpha_length_mismatch_panics() {
        let _ = PointsViz::new(
            vec![0.0, 1.0],
            vec![0.0, 1.0],
            vec![5.0, 6.0],
            vec![Color32::RED, Color32::BLUE],
        )
        .with_alpha(vec![0.5]);
    }

    // --- IRREGULAR_GRID quadrilateral mesh ----------------------------------

    #[test]
    fn quadrilateral_grid_coords_unit_2x2_offsets_by_half() {
        // Points on a unit grid: row 0 at y=0, row 1 at y=1; x fast.
        let points = [[0.0, 0.0], [1.0, 0.0], [0.0, 1.0], [1.0, 1.0]];
        let grid = quadrilateral_grid_coords(&points, 2, 2);
        // 3x3 corner grid, each cell a unit square centered on its point.
        let expect = [
            [-0.5, -0.5],
            [0.5, -0.5],
            [1.5, -0.5],
            [-0.5, 0.5],
            [0.5, 0.5],
            [1.5, 0.5],
            [-0.5, 1.5],
            [0.5, 1.5],
            [1.5, 1.5],
        ];
        assert_eq!(grid.len(), 9);
        for (got, want) in grid.iter().zip(&expect) {
            assert!((got[0] - want[0]).abs() < 1e-12, "x: {got:?} vs {want:?}");
            assert!((got[1] - want[1]).abs() < 1e-12, "y: {got:?} vs {want:?}");
        }
    }

    #[test]
    fn irregular_grid_triangles_builds_one_cell_per_point() {
        let x = [0.0, 1.0, 0.0, 1.0];
        let y = [0.0, 0.0, 1.0, 1.0];
        let colors = [Color32::RED, Color32::GREEN, Color32::BLUE, Color32::WHITE];
        let mesh = irregular_grid_triangles(&x, &y, &colors).expect("buildable grid");
        // 4 points -> 16 vertices, 8 triangles (2 per point).
        assert_eq!(mesh.x.len(), 16);
        assert_eq!(mesh.colors.len(), 16);
        assert_eq!(mesh.indices.len(), 8);
        // silx flat-shades: each point's 4 vertices share its color.
        for (k, &c) in colors.iter().enumerate() {
            for v in 0..4 {
                assert_eq!(mesh.colors[4 * k + v], c, "point {k} vertex {v}");
            }
        }
    }

    #[test]
    fn irregular_grid_pick_maps_cursor_to_owning_cell() {
        let x = [0.0, 1.0, 0.0, 1.0];
        let y = [0.0, 0.0, 1.0, 1.0];
        let mesh = irregular_grid_triangles(&x, &y, &[Color32::RED; 4]).expect("buildable grid");
        // Each unit cell is centered on its data point (silx vertex // 4).
        assert_eq!(irregular_grid_pick(&mesh, 0.0, 0.0), Some(0));
        assert_eq!(irregular_grid_pick(&mesh, 1.0, 0.0), Some(1));
        assert_eq!(irregular_grid_pick(&mesh, 0.0, 1.0), Some(2));
        assert_eq!(irregular_grid_pick(&mesh, 1.0, 1.0), Some(3));
        // Far outside every cell -> no pick.
        assert_eq!(irregular_grid_pick(&mesh, 10.0, 10.0), None);
    }

    #[test]
    fn irregular_grid_single_line_builds_one_cell_per_point_and_picks() {
        // Collinear points fall back to silx's single-line grid (a 2xN strip).
        let x = [0.0, 1.0, 2.0, 3.0];
        let y = [0.0, 0.0, 0.0, 0.0];
        let mesh = irregular_grid_triangles(&x, &y, &[Color32::RED; 4]).expect("buildable line");
        assert_eq!(mesh.x.len(), 16, "4 points -> 16 vertices");
        assert_eq!(mesh.indices.len(), 8, "4 points -> 8 triangles");
        // Each data point lies inside its own cell.
        assert_eq!(irregular_grid_pick(&mesh, 1.0, 0.0), Some(1));
        assert_eq!(irregular_grid_pick(&mesh, 2.0, 0.0), Some(2));
    }

    #[test]
    fn irregular_grid_triangles_none_for_too_few_points() {
        // A single point cannot form a quadrilateral mesh (silx renders a square).
        assert!(irregular_grid_triangles(&[0.0], &[0.0], &[Color32::RED]).is_none());
    }
}
