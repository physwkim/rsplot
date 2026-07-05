//! Headless render check for the plot3d P2.2c cut plane (`ScalarField3D` +
//! `CutPlane` → a colormapped `Scene3dTexturedMesh`). A 5×5×5 field with a solid
//! 3×3×3 high block at its centre is sliced by the z=2.5 plane (normal `(0,0,1)`,
//! through the volume centre). The slice samples the block in the middle and the
//! zero background at the edges.
//!
//! A two-stop blue→red colormap over `[0,1]` then maps background→blue and
//! block→red. Looking straight down `−z` at the volume centre, the slice fills
//! the view (its box face spans world `[0,5]²`, ≈ the visible extent); the centre
//! ray hits world `(2.5, 2.5, 2.5)` — voxel `(2,2,2)` = 1.0 → red — while the
//! corners hit the zero background → blue. Asserting red centre + blue corners
//! proves the whole chain: the plane∩box contour is built, the slice samples the
//! field, the colormap colours it, and the textured mesh renders.

use egui_kittest::Harness;
use egui_kittest::wgpu::{WgpuTestRenderer, create_render_state, default_wgpu_setup};
use rsplot::egui::{self, Color32};
use rsplot::egui_wgpu::RenderState;
use rsplot::{
    Camera, Colormap, ScalarField3D, Scene3dGeometry, Vec3, install_scene3d, paint_scene3d,
    set_scene3d,
};
use std::cell::RefCell;
use std::rc::Rc;

const SCENE_ID: u64 = 0;
const WIN: f32 = 300.0;

struct App {
    camera: Camera,
    last_rect: Option<egui::Rect>,
}

impl App {
    fn new(rs: &RenderState) -> Self {
        install_scene3d(rs);

        // 5×5×5 field, central 3×3×3 block = 1.0, rest 0.0.
        let (d, h, w) = (5usize, 5usize, 5usize);
        let mut data = vec![0.0f32; d * h * w];
        for z in 1..4 {
            for y in 1..4 {
                for x in 1..4 {
                    data[(z * h + y) * w + x] = 1.0;
                }
            }
        }
        let mut sf = ScalarField3D::new().with_data(&data, d, h, w);
        // Two-stop colormap: 0 → blue, 1 → red, over [0, 1].
        let cmap = Colormap::from_colors(&[[0, 0, 255, 255], [255, 0, 0, 255]], 0.0, 1.0)
            .expect("two-stop colormap");
        {
            let cp = sf.cut_plane_mut();
            cp.set_colormap(cmap);
            cp.set_normal(Vec3::new(0.0, 0.0, 1.0));
            cp.set_point(Vec3::new(2.5, 2.5, 2.5));
            cp.set_visible(true);
        }

        let mut g = Scene3dGeometry::new();
        sf.append_to(&mut g);
        set_scene3d(rs, SCENE_ID, &g);

        // Look straight down −z at the volume centre (2.5, 2.5, 2.5).
        let camera = Camera::new(
            30.0,
            0.1,
            100.0,
            (1.0, 1.0),
            Vec3::new(2.5, 2.5, 12.0),
            Vec3::new(0.0, 0.0, -1.0),
            Vec3::new(0.0, 1.0, 0.0),
        );
        Self {
            camera,
            last_rect: None,
        }
    }

    fn ui(&mut self, ui: &mut egui::Ui) {
        let (rect, _resp) = ui.allocate_exact_size(ui.available_size(), egui::Sense::hover());
        paint_scene3d(ui, rect, SCENE_ID, &self.camera, Color32::BLACK);
        self.last_rect = Some(rect);
    }
}

#[test]
fn scene3d_cutplane_renders_colormapped_slice() {
    let rs = create_render_state(default_wgpu_setup());
    let app = Rc::new(RefCell::new(App::new(&rs)));
    let renderer = WgpuTestRenderer::from_render_state(rs);

    let app_ui = app.clone();
    let mut harness = Harness::builder()
        .with_size(egui::vec2(WIN, WIN))
        .with_pixels_per_point(1.0)
        .renderer(renderer)
        .build_ui(move |ui| app_ui.borrow_mut().ui(ui));

    harness.step();
    let rect = app.borrow().last_rect.expect("scene rect captured");

    let image = harness.render().expect("headless wgpu render");
    let (iw, ih) = (image.width() as usize, image.height() as usize);
    let raw = image.as_raw();

    let at = |fx: f32, fy: f32| -> (u8, u8, u8) {
        let x = ((rect.min.x + fx * rect.width()).round() as usize).min(iw - 1);
        let y = ((rect.min.y + fy * rect.height()).round() as usize).min(ih - 1);
        let i = (y * iw + x) * 4;
        (raw[i], raw[i + 1], raw[i + 2])
    };

    // Centre: the ray hits world (2.5, 2.5, 2.5) = voxel (2,2,2) = 1.0 → red.
    let (r, g, b) = at(0.5, 0.5);
    assert!(
        r > 150 && b < 80 && r > b,
        "slice centre should be red (block, value 1.0); got rgb({r},{g},{b})"
    );

    // Corners: the slice fills the view (box face ≈ visible extent), but the
    // corners hit the zero background → blue.
    for (fx, fy) in [(0.03, 0.03), (0.97, 0.97), (0.03, 0.97), (0.97, 0.03)] {
        let (r, g, b) = at(fx, fy);
        assert!(
            b > 150 && r < 80 && b > r,
            "slice corner ({fx},{fy}) should be blue (background, value 0.0); \
             got rgb({r},{g},{b})"
        );
    }
}
