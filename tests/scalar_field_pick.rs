//! CPU pick for `ScalarFieldView::pick` (plot3d PK4): a screen-centre ray is
//! unprojected (no GPU readback) and intersected against the field's cut plane;
//! the returned `FieldPick` carries the world hit position and the field value
//! sampled there — the data silx `PositionInfoWidget` shows under the cursor.
//! Render-free — the `RenderState` is only needed to build/upload the view's
//! geometry at construction; `pick` reads the view's own camera + field.

use egui_kittest::wgpu::{create_render_state, default_wgpu_setup};
use siplot::{CameraFace, ScalarFieldView, Vec3};

const N: usize = 5;

/// An `N×N×N` ramp where `data[z][y][x] = z` (row-major `(depth, height,
/// width)`, width contiguous). The value depends only on `z`, so the field value
/// at any in-box hit on the `z = k` plane is determined by `k` alone.
fn ramp_z() -> Vec<f32> {
    let mut data = vec![0.0f32; N * N * N];
    for z in 0..N {
        for y in 0..N {
            for x in 0..N {
                data[(z * N + y) * N + x] = z as f32;
            }
        }
    }
    data
}

/// A Front-framed `ScalarFieldView` over the ramp field, with the cut plane set
/// to the `z = cut_z` plane (normal `(0, 0, 1)`) and `visible`. The camera is
/// framed to the `(0,0,0)..(5,5,5)` box and sized square so the centre ray is
/// unambiguous. `pick` reads the view's own camera + field (pure CPU), so the
/// `RenderState` — needed only to build/upload the geometry here — need not
/// outlive this helper.
fn front_field_view(id: u64, cut_z: f32, visible: bool) -> ScalarFieldView {
    let rs = create_render_state(default_wgpu_setup());
    let mut view = ScalarFieldView::new(&rs, id);
    assert!(
        view.set_data(&rs, &ramp_z(), N, N, N),
        "5³ ramp is valid data"
    );

    {
        let plane = view.field_mut().cut_plane_mut();
        plane.set_normal(Vec3::new(0.0, 0.0, 1.0));
        plane.set_point(Vec3::new(0.0, 0.0, cut_z));
        plane.set_visible(visible);
    }
    view.rebuild(&rs);

    view.scene_mut().set_viewpoint(CameraFace::Front); // look along -Z through the box centre
    view.scene_mut().camera_mut().set_size((200.0, 200.0));
    view
}

#[test]
fn pick_hits_the_visible_cut_plane_and_samples_its_value() {
    // Cut plane at z = 2.5 — a voxel-centre slice (world z = 2.5 ⇒ z-index 2),
    // so the sampled value is exactly the ramp value 2.0.
    let view = front_field_view(31, 2.5, true);

    let pick = view
        .pick((0.0, 0.0))
        .expect("centre ray crosses the visible cut plane inside the box");
    assert!(
        (pick.position.z - 2.5).abs() < 1e-3,
        "hit lies on the z = 2.5 plane, got z = {}",
        pick.position.z
    );
    // The hit is near the box centre (2.5, 2.5) in x/y.
    assert!(
        (pick.position.x - 2.5).abs() < 0.6 && (pick.position.y - 2.5).abs() < 0.6,
        "hit near box centre, got ({}, {})",
        pick.position.x,
        pick.position.y
    );
    let value = pick.value.expect("hit is inside the box → has a value");
    assert!(
        (value - 2.0).abs() < 1e-4,
        "ramp value on the z = 2.5 slice is 2.0, got {value}"
    );
}

#[test]
fn pick_returns_none_when_nothing_is_pickable() {
    // Same geometry, but the cut plane is hidden and no iso-surface is added, so
    // the ray has nothing to hit — pick must not fabricate a result.
    let view = front_field_view(32, 2.5, false);
    assert!(
        view.pick((0.0, 0.0)).is_none(),
        "a hidden cut plane with no iso-surface leaves nothing to pick"
    );
}
