//! `StackView` Profile3D toolbar (silx `Profile3DToolBar` / `ProfileImageStack*ROI`).
//!
//! The profile extraction cores (`stack_aligned_profile` / `stack_line_profile`)
//! are unit-tested in `high_level`; these tests exercise the *wiring* that was
//! the remaining gap: arming the profile tool, a drag on the image, and the
//! resulting 1D-vs-2D profile window opening with the extracted profile. Building
//! a `StackView` and caching its transform both need a real rendered frame, so
//! this runs through the egui_kittest + wgpu harness like `compare_separator`.

use std::cell::RefCell;
use std::rc::Rc;

use egui_kittest::Harness;
use egui_kittest::wgpu::{WgpuTestRenderer, create_render_state, default_wgpu_setup};
use siplot::egui;
use siplot::{Colormap, ProfileMethod, ProfileMode, StackProfileDimension, StackView, YAxis};

/// A `[2, 3, 4]` volume (2 frames, each 3 rows × 4 cols under the default
/// `Axis0` perspective) whose element `(i, j, k)` encodes its indices as
/// `100*i + 10*j + k`, so a stacked row/column profile is verifiable by hand.
fn sample_volume() -> (Vec<f32>, [usize; 3]) {
    let shape = [2usize, 3, 4];
    let [d0, d1, d2] = shape;
    let mut data = vec![0.0f32; d0 * d1 * d2];
    for i in 0..d0 {
        for j in 0..d1 {
            for k in 0..d2 {
                data[(i * d1 + j) * d2 + k] = (100 * i + 10 * j + k) as f32;
            }
        }
    }
    (data, shape)
}

/// Build a harness around a `StackView` holding [`sample_volume`], render two
/// frames so the transform is cached, and return the shared widget + harness.
fn harness() -> (Rc<RefCell<StackView>>, Harness<'static>) {
    let rs = create_render_state(default_wgpu_setup());
    siplot::install(&rs);

    let mut view = StackView::new(&rs, 0);
    let (data, shape) = sample_volume();
    view.set_volume(data, shape, Colormap::viridis(0.0, 200.0))
        .expect("volume matches shape");

    let app = Rc::new(RefCell::new(view));
    let app_ui = app.clone();
    let renderer = WgpuTestRenderer::from_render_state(rs.clone());
    let mut harness = Harness::builder()
        .with_size(egui::vec2(400.0, 400.0))
        .with_pixels_per_point(1.0)
        .renderer(renderer)
        .build_ui(move |ui| {
            app_ui.borrow_mut().show(ui);
        });
    harness.step();
    harness.step();
    (app, harness)
}

/// Drag from `p0` to `p1` with an incremental ramp so egui reports a genuine
/// drag (a single jump makes `drag_started` fire already at the end point).
fn drag(harness: &mut Harness<'static>, p0: egui::Pos2, p1: egui::Pos2) {
    harness.drag_at(p0);
    harness.step();
    for t in [0.2f32, 0.5, 0.8, 1.0] {
        harness.hover_at(p0 + (p1 - p0) * t);
        harness.step();
    }
    harness.drop_at(p1);
    harness.step();
    harness.step();
}

#[test]
fn dragging_a_horizontal_line_opens_the_2d_stacked_profile_window() {
    let (app, mut harness) = harness();
    app.borrow_mut().set_profile_mode(ProfileMode::Horizontal);
    app.borrow_mut()
        .set_profile_dimension(StackProfileDimension::TwoD);

    // Neither window is open before any drag.
    assert!(!app.borrow().stack_profile_window().is_open());
    assert!(!app.borrow().profile_window().is_open());

    // Drag a horizontal segment at data row y = 1.5 (→ row 1). Only end.y matters
    // for a horizontal profile.
    let p0 = app
        .borrow()
        .data_to_pixel(0.5, 1.5, YAxis::Left)
        .expect("transform cached after a frame");
    let p1 = app
        .borrow()
        .data_to_pixel(3.5, 1.5, YAxis::Left)
        .expect("transform cached after a frame");
    drag(&mut harness, p0, p1);

    // The 2D stacked-profile window opened; the 1D window stayed closed.
    assert!(
        app.borrow().stack_profile_window().is_open(),
        "dragging in 2D mode must open the stacked-profile image window"
    );
    assert!(!app.borrow().profile_window().is_open());

    // The data fed to the window (the same extractor it calls) is the row-1
    // profile stacked over both frames: frame i, col k → 100*i + 10*1 + k.
    let profile = app
        .borrow()
        .stack_aligned_profile(1.0, 1, true, ProfileMethod::Mean)
        .expect("horizontal stack profile over the volume");
    assert_eq!(profile.frame_count, 2);
    assert_eq!(profile.profile_len, 4);
    assert_eq!(
        profile.values,
        vec![10.0, 11.0, 12.0, 13.0, 110.0, 111.0, 112.0, 113.0]
    );
}

#[test]
fn dragging_in_1d_mode_opens_the_curve_profile_window() {
    let (app, mut harness) = harness();
    // Default dimension is 1D (silx profileType default).
    assert_eq!(
        app.borrow().profile_dimension(),
        StackProfileDimension::OneD
    );
    app.borrow_mut().set_profile_mode(ProfileMode::Horizontal);

    let p0 = app
        .borrow()
        .data_to_pixel(0.5, 1.5, YAxis::Left)
        .expect("transform cached");
    let p1 = app
        .borrow()
        .data_to_pixel(3.5, 1.5, YAxis::Left)
        .expect("transform cached");
    drag(&mut harness, p0, p1);

    assert!(
        app.borrow().profile_window().is_open(),
        "dragging in 1D mode must open the current-frame curve profile window"
    );
    assert!(!app.borrow().stack_profile_window().is_open());
}

#[test]
fn switching_dimension_closes_the_inactive_window() {
    let (app, mut harness) = harness();
    app.borrow_mut().set_profile_mode(ProfileMode::Horizontal);
    app.borrow_mut()
        .set_profile_dimension(StackProfileDimension::TwoD);

    let p0 = app
        .borrow()
        .data_to_pixel(0.5, 1.5, YAxis::Left)
        .expect("transform cached");
    let p1 = app
        .borrow()
        .data_to_pixel(3.5, 1.5, YAxis::Left)
        .expect("transform cached");
    drag(&mut harness, p0, p1);
    assert!(app.borrow().stack_profile_window().is_open());

    // Toggling back to 1D closes the 2D window (silx shows one profile at a time).
    app.borrow_mut()
        .set_profile_dimension(StackProfileDimension::OneD);
    assert!(!app.borrow().stack_profile_window().is_open());
}

#[test]
fn show_profile_returns_true_for_a_line_over_the_volume() {
    let (app, _harness) = harness();
    app.borrow_mut().set_profile_mode(ProfileMode::Line);
    app.borrow_mut()
        .set_profile_dimension(StackProfileDimension::TwoD);

    // A diagonal line within the 4×3 frame yields a stacked profile.
    assert!(
        app.borrow_mut().show_profile((0.5, 0.5), (3.5, 2.5)),
        "a line profile over a loaded volume must produce a 2D stacked profile"
    );
    assert!(app.borrow().stack_profile_window().is_open());
}

#[test]
fn two_d_profile_requires_a_volume_not_flat_frames() {
    // In flat-frames mode (`set_stack`, no 3D volume) the 2D stacked profile has
    // no source, so it produces nothing (silx ImageStack always carries the 3D
    // array; our flat-frames convenience does not).
    let rs = create_render_state(default_wgpu_setup());
    siplot::install(&rs);
    let mut view = StackView::new(&rs, 0);
    let frames = vec![vec![1.0f32; 4 * 3], vec![2.0f32; 4 * 3]];
    view.set_stack(4, 3, frames, Colormap::viridis(0.0, 2.0))
        .expect("uniform frames");
    view.set_profile_mode(ProfileMode::Horizontal);
    view.set_profile_dimension(StackProfileDimension::TwoD);

    assert!(
        !view.show_profile((0.5, 1.5), (3.5, 1.5)),
        "2D stacked profile must be unavailable without a loaded volume"
    );
    assert!(!view.stack_profile_window().is_open());
}
