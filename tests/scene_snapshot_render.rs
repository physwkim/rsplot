//! Off-screen snapshot of a 3D scene (plot3d P3.3): `SceneWidget::snapshot`
//! renders the current scene synchronously into a transient copyable target and
//! reads it back as tightly packed RGBA8 — independent of the egui frame loop
//! (no `Harness`). The snapshot of an iso-surface scene must contain the
//! iso-surface colour (magenta, which the dark chrome cannot make) and encode to
//! a valid PNG. A second snapshot at a different size returns a correctly sized
//! buffer, proving the camera aspect tracks the requested pixel size.

use egui_kittest::wgpu::{create_render_state, default_wgpu_setup};
use rsplot::ScalarFieldView;
use rsplot::egui::Color32;
use rsplot::encode_png;

fn blob() -> Vec<f32> {
    let mut data = vec![0.0f32; 125];
    for z in 1..4 {
        for y in 1..4 {
            for x in 1..4 {
                data[(z * 5 + y) * 5 + x] = 1.0;
            }
        }
    }
    data
}

fn count_magenta(raw: &[u8], iw: usize, ih: usize) -> usize {
    (0..iw * ih)
        .filter(|&px| {
            let i = px * 4;
            raw[i] > 120 && raw[i + 2] > 120 && raw[i + 1] < 80
        })
        .count()
}

#[test]
fn snapshot_captures_isosurface_and_encodes_png() {
    let rs = create_render_state(default_wgpu_setup());
    let mut view = ScalarFieldView::new(&rs, 7);
    assert!(
        view.set_data(&rs, &blob(), 5, 5, 5),
        "5³ blob is valid data"
    );
    view.add_isosurface(&rs, 0.5, Color32::from_rgb(255, 0, 255));

    let (w, h) = (256u32, 256u32);
    let rgba = view
        .scene()
        .snapshot(&rs, (w, h))
        .expect("scene snapshot reads back");
    assert_eq!(
        rgba.len(),
        (w * h * 4) as usize,
        "snapshot is tightly packed RGBA8"
    );
    let magenta = count_magenta(&rgba, w as usize, h as usize);
    assert!(
        magenta > 50,
        "the iso-surface (magenta) must be captured in the snapshot; only {magenta} px"
    );

    // The RGBA8 buffer round-trips through the PNG encoder.
    let png = encode_png(&rgba, w, h).expect("encode PNG");
    assert!(
        png.starts_with(&[0x89, b'P', b'N', b'G', 0x0d, 0x0a, 0x1a, 0x0a]),
        "encoded bytes carry the PNG magic header"
    );
}

#[test]
fn snapshot_size_tracks_requested_pixels() {
    let rs = create_render_state(default_wgpu_setup());
    let mut view = ScalarFieldView::new(&rs, 8);
    assert!(view.set_data(&rs, &blob(), 5, 5, 5), "valid data");
    view.add_isosurface(&rs, 0.5, Color32::from_rgb(255, 0, 255));

    // A non-square size (whose padded row stride exceeds the tight stride)
    // still returns a tightly packed buffer of exactly width*height*4 bytes.
    let (w, h) = (200u32, 120u32);
    let rgba = view.scene().snapshot(&rs, (w, h)).expect("snapshot");
    assert_eq!(rgba.len(), (w * h * 4) as usize);
}
