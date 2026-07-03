//! CPU pick traversal for `SceneWidget::pick` (plot3d PK2): a click is
//! unprojected into a ray (no GPU readback) and intersected with the scene's
//! data geometry. These tests are render-free — they build a `SceneWidget`
//! (which needs a headless wgpu `RenderState` only to install the scene
//! resources), set a known triangle / point, frame a fixed viewpoint, and assert
//! the pick lands where the geometry is.

use egui_kittest::wgpu::{create_render_state, default_wgpu_setup};
use siplot::egui::Color32;
use siplot::{
    CameraFace, ImageInterpolation, PointMarker, Scatter2D, Scatter2DVisualization, Scatter3D,
    Scene3dGeometry, Scene3dImageLayer, ScenePickKind, SceneWidget, Vec3,
};

/// Frame a unit-box scene from the Front viewpoint, with the camera sized
/// square so the centre-screen ray is unambiguous. `pick` reads the widget's own
/// CPU geometry + camera, so the `RenderState` (only needed to install the scene
/// resources at construction) need not outlive this helper.
fn front_view_widget(id: u64, geometry: Scene3dGeometry) -> SceneWidget {
    let rs = create_render_state(default_wgpu_setup());
    let mut w = SceneWidget::new(&rs, id);
    w.set_geometry(&rs, geometry);
    w.set_viewpoint(CameraFace::Front); // look along -Z through the box centre
    w.camera_mut().set_size((200.0, 200.0));
    w
}

#[test]
fn pick_hits_surface_under_screen_centre() {
    // A triangle in the z = 0.5 plane covering the box centre (0.5, 0.5).
    let mut geo = Scene3dGeometry::new();
    geo.add_triangle(
        [0.0, 0.0, 0.5],
        [1.0, 0.0, 0.5],
        [0.5, 1.0, 0.5],
        Color32::WHITE,
    );
    let w = front_view_widget(21, geo);

    let pick = w.pick((0.0, 0.0)).expect("centre ray hits the triangle");
    assert_eq!(pick.kind, ScenePickKind::Surface);
    // The hit lies on the z = 0.5 plane near the box centre.
    assert!(
        (pick.position.z - 0.5).abs() < 1e-3,
        "hit z = {} (want 0.5)",
        pick.position.z
    );
    assert!(
        (pick.position.x - 0.5).abs() < 0.1,
        "hit x = {}",
        pick.position.x
    );
    assert!(
        (pick.position.y - 0.5).abs() < 0.25,
        "hit y = {}",
        pick.position.y
    );
}

#[test]
fn pick_misses_when_ray_clears_the_geometry() {
    // A small triangle near one corner; the centre ray does not cross it and
    // there are no points, so the pick is empty.
    let mut geo = Scene3dGeometry::new();
    geo.add_triangle(
        [0.0, 0.0, 0.5],
        [0.1, 0.0, 0.5],
        [0.0, 0.1, 0.5],
        Color32::WHITE,
    );
    let w = front_view_widget(22, geo);
    assert!(
        w.pick((0.0, 0.0)).is_none(),
        "centre ray must miss the corner triangle"
    );
}

#[test]
fn pick_selects_scatter_point_under_the_cursor() {
    // A single scatter point at the box centre; the centre ray hits it.
    let mut geo = Scene3dGeometry::new();
    geo.add_point([0.5, 0.5, 0.5], Color32::WHITE, 12.0, PointMarker::Circle);
    let w = front_view_widget(23, geo);

    let pick = w.pick((0.0, 0.0)).expect("centre ray hits the point");
    assert_eq!(pick.kind, ScenePickKind::Point { index: 0 });
    assert!((pick.position.x - 0.5).abs() < 1e-6);
    assert!((pick.position.y - 0.5).abs() < 1e-6);
    assert!((pick.position.z - 0.5).abs() < 1e-6);
}

#[test]
fn pick_prefers_the_nearer_surface() {
    // Two centre-covering triangles at z = 0.2 and z = 0.8. From the Front view
    // (camera on the +Z side looking toward -Z) the z = 0.8 plane is nearer, so
    // it must win.
    let mut geo = Scene3dGeometry::new();
    geo.add_triangle(
        [0.0, 0.0, 0.2],
        [1.0, 0.0, 0.2],
        [0.5, 1.0, 0.2],
        Color32::WHITE,
    );
    geo.add_triangle(
        [0.0, 0.0, 0.8],
        [1.0, 0.0, 0.8],
        [0.5, 1.0, 0.8],
        Color32::WHITE,
    );
    let w = front_view_widget(24, geo);

    let pick = w.pick((0.0, 0.0)).expect("centre ray hits a triangle");
    assert_eq!(pick.kind, ScenePickKind::Surface);
    assert!(
        (pick.position.z - 0.8).abs() < 1e-2,
        "nearer plane (z=0.8) should win, got z = {}",
        pick.position.z
    );
}

/// The NDC (x, y) where `world` projects with the widget's current camera —
/// used to aim a pick exactly at a known data point.
fn ndc_of(w: &SceneWidget, world: Vec3) -> (f32, f32) {
    let p = w.camera().matrix().transform_point(world, true);
    (p.x, p.y)
}

#[test]
fn pick_scatter2d_lines_mode_hits_data_points_not_segments() {
    // silx picks LINES mode at the data points with a 5 px square threshold
    // (items/scatter.py:509-511), never along the segments.
    let scatter = Scatter2D::new()
        .with_data(&[0.25, 0.75, 0.5], &[0.25, 0.25, 0.75], &[0.0, 1.0, 2.0])
        .with_visualization(Scatter2DVisualization::Lines);
    let mut geo = Scene3dGeometry::new();
    scatter.append_to(&mut geo);
    assert_eq!(
        geo.line_pick_anchors().len(),
        3,
        "one anchor per data point"
    );
    let w = front_view_widget(25, geo);

    // Aim exactly at data point 1 → LinePoint with that index.
    let target = Vec3::new(0.75, 0.25, 0.0);
    let pick = w.pick(ndc_of(&w, target)).expect("hits the data point");
    assert_eq!(pick.kind, ScenePickKind::LinePoint { index: 1 });
    assert!((pick.position.x - 0.75).abs() < 1e-6);
    assert!((pick.position.y - 0.25).abs() < 1e-6);

    // Aim at the midpoint of the edge between points 0 and 1: on the drawn
    // segment but ~50 px from either endpoint at this zoom → no pick.
    let mid = Vec3::new(0.5, 0.25, 0.0);
    assert!(
        w.pick(ndc_of(&w, mid)).is_none(),
        "silx picks the points, not the line segments"
    );
}

#[test]
fn pick_image_quad_returns_row_and_column() {
    // A 3×2-pixel image layer at origin (0,0,0.5), 0.25 world units per pixel
    // → spans (0..0.75, 0..0.5) in the z = 0.5 plane.
    let mut geo = Scene3dGeometry::new();
    geo.add_image_layer(Scene3dImageLayer {
        pixels: vec![255; 3 * 2 * 4],
        width: 3,
        height: 2,
        origin: [0.0, 0.0, 0.5],
        scale: [0.25, 0.25],
        interpolation: ImageInterpolation::Nearest,
    });
    let w = front_view_widget(26, geo);

    // Aim inside pixel (row 1, col 2): world (0.6, 0.3) → col 0.6/0.25 = 2.4,
    // row 0.3/0.25 = 1.2 (silx _pickFull floors to ints, image.py:72-77).
    let target = Vec3::new(0.6, 0.3, 0.5);
    let pick = w.pick(ndc_of(&w, target)).expect("ray crosses the image");
    assert_eq!(
        pick.kind,
        ScenePickKind::Image {
            image: 0,
            row: 1,
            col: 2
        }
    );
    // The hit position is the world-space plane intersection.
    assert!((pick.position.x - 0.6).abs() < 1e-3);
    assert!((pick.position.y - 0.3).abs() < 1e-3);
    assert!((pick.position.z - 0.5).abs() < 1e-3);

    // Just past the last column (x = 0.9 → col 3.6 ≥ width 3): outside image
    // (silx rejects row/column past the data shape, image.py:78-84).
    assert!(w.pick(ndc_of(&w, Vec3::new(0.9, 0.3, 0.5))).is_none());
    // Behind the origin corner (negative image coordinates): rejected too.
    assert!(w.pick(ndc_of(&w, Vec3::new(-0.2, 0.3, 0.5))).is_none());
}

#[test]
fn pick_follows_the_item_transform() {
    // silx applies the DataItem3D transform stack (items/core.py:288-315) to
    // rendering AND picking through the shared scene graph. Here the composed
    // matrix is baked into the geometry at append time, so the pick traversal
    // reads exactly the positions the renderer draws — assert that a
    // transformed point picks at its transformed location and no longer at
    // its raw one.
    let mut scatter = Scatter3D::new().with_data(&[0.2], &[0.2], &[0.5], &[1.0]);
    scatter.transform_mut().set_translation(0.4, 0.2, 0.0);
    let mut geo = Scene3dGeometry::new();
    scatter.append_to(&mut geo);
    let w = front_view_widget(27, geo);

    let target = Vec3::new(0.6, 0.4, 0.5);
    let pick = w.pick(ndc_of(&w, target)).expect("transformed point picks");
    assert_eq!(pick.kind, ScenePickKind::Point { index: 0 });
    assert!((pick.position.x - 0.6).abs() < 1e-6);
    assert!((pick.position.y - 0.4).abs() < 1e-6);
    assert!((pick.position.z - 0.5).abs() < 1e-6);

    // The raw (untransformed) location no longer picks anything.
    assert!(w.pick(ndc_of(&w, Vec3::new(0.2, 0.2, 0.5))).is_none());

    // Item bounds report the same composed transform.
    let (lo, hi) = scatter.bounds().expect("has data");
    assert!((lo.x - 0.6).abs() < 1e-6 && (lo.y - 0.4).abs() < 1e-6);
    assert!((hi.x - 0.6).abs() < 1e-6 && (hi.z - 0.5).abs() < 1e-6);
}
