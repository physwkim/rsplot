//! Headless wgpu readback proving the 2D image path writes its colours in the
//! render target's encoding: a colormapped texel and a direct-RGBA texel must
//! come back as the colormap's / the image's own sRGB bytes.
//!
//! `egui_kittest`'s renderer targets a non-sRGB `Rgba8Unorm` texture — the same
//! kind of target egui-wgpu's swapchain is — and reads the stored bytes back
//! verbatim. The colormap LUT and the RGBA tiles used to be `Rgba8UnormSrgb`
//! textures, so the shader wrote *linearised* values into that target and every
//! image came out darker than its colormap (viridis' mid teal `(33, 145, 140)`
//! read back as `(4, 71, 66)`). Now the texel formats follow the target
//! (`gpu_image::color_texel_format`), so the bytes round-trip exactly.
//!
//! Boundaries covered: the scalar (LUT) path and the direct-RGBA path, each
//! sampled over a block of pixels around the window centre, which the image
//! fills (view limits pinned to the image extent, no grid, no colorbar). Needs
//! a GPU (real or software).

use std::cell::RefCell;
use std::rc::Rc;

use egui_kittest::Harness;
use egui_kittest::wgpu::{WgpuTestRenderer, create_render_state, default_wgpu_setup};
use rsplot::egui;
use rsplot::egui_wgpu::RendererOptions;
use rsplot::{Colormap, ImageData, Plot, PlotView, install, set_image};

const W: usize = 400;
const H: usize = 300;
const WD: u32 = 16;
const HD: u32 = 16;
/// Half-side of the sampled block around the window centre.
const BLOCK: usize = 4;

/// Render `image` filling the data area and return the frame's RGBA bytes.
fn render(image: &ImageData) -> Vec<u8> {
    let rs = create_render_state(default_wgpu_setup(), RendererOptions::default());
    install(&rs);
    set_image(&rs, 0, image);

    let mut plot = Plot::new(0);
    plot.limits = (0.0, f64::from(WD), 0.0, f64::from(HD));

    let app = Rc::new(RefCell::new(plot));
    let app_ui = app.clone();
    let renderer = WgpuTestRenderer::from_render_state(rs);
    let mut harness = Harness::builder()
        .with_size(egui::vec2(W as f32, H as f32))
        .with_pixels_per_point(1.0)
        .renderer(renderer)
        .build_ui(move |ui| {
            PlotView::new().show(ui, &mut app_ui.borrow_mut());
        });

    harness.step();
    harness.step();
    let frame = harness.render().expect("headless wgpu render");
    assert_eq!((frame.width() as usize, frame.height() as usize), (W, H));
    frame.as_raw().to_vec()
}

/// Every pixel of the `(2·BLOCK)²` block around the window centre must be
/// `expected` to within one 8-bit step per channel.
fn assert_centre_block_is(raw: &[u8], expected: [u8; 3], what: &str) {
    let (cx, cy) = (W / 2, H / 2);
    for y in cy - BLOCK..cy + BLOCK {
        for x in cx - BLOCK..cx + BLOCK {
            let i = (y * W + x) * 4;
            let got = [raw[i], raw[i + 1], raw[i + 2]];
            let close = got.iter().zip(expected).all(|(&g, e)| g.abs_diff(e) <= 1);
            assert!(
                close,
                "{what}: pixel ({x},{y}) is rgb{got:?}, expected rgb{expected:?} ± 1 \
                 (the image's own sRGB bytes on the non-sRGB target)"
            );
        }
    }
}

#[test]
fn colormapped_texel_reads_back_as_the_colormap_byte() {
    // A uniform field at the colormap's midpoint: every texel is one LUT entry,
    // a saturated teal whose linearised bytes would be far darker.
    let colormap = Colormap::viridis(0.0, 1.0);
    let expected = colormap.color_at(0.5);
    assert!(
        expected[0] > 20 && expected[1] > 100,
        "viridis mid should be a mid-intensity teal; got {expected:?}"
    );
    let image = ImageData::new(WD, HD, vec![0.5f32; (WD * HD) as usize], colormap);

    let raw = render(&image);
    assert_centre_block_is(
        &raw,
        [expected[0], expected[1], expected[2]],
        "scalar (LUT) image",
    );
}

#[test]
fn rgba_texel_reads_back_as_its_own_bytes() {
    // Three distinct mid-range channels: each would land on a different, darker
    // byte if the tile were sRGB-decoded on the way to the non-sRGB target.
    let texel = [128u8, 64, 32, 255];
    let image = ImageData::rgba(WD, HD, vec![texel; (WD * HD) as usize]);

    let raw = render(&image);
    assert_centre_block_is(&raw, [texel[0], texel[1], texel[2]], "direct RGBA image");
}
