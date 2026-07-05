//! Headless render check for [`Scatter2D`]'s three visualization modes through
//! the real `render::gpu_scene3d` pipelines.
//!
//! The mode→channel mapping and the colour/normal math are unit-tested purely in
//! `render::scene3d_items`; this proves each mode's emitted geometry actually
//! rasterizes on a GPU, and that the channels differ as expected: the SOLID fill
//! covers the whole triangulated quad, while LINES draws only its ~1px edges, so
//! SOLID must cover far more pixels than LINES, and POINTS renders its markers.
//! The same four `(x, y, value)` points feed every mode; the offscreen clear is
//! black, so any non-black pixel is rendered Scatter2D content.
//!
//! Needs a GPU (real or software).

use egui_kittest::Harness;
use egui_kittest::wgpu::{WgpuTestRenderer, create_render_state, default_wgpu_setup};
use rsplot::egui::{self, Color32};
use rsplot::egui_wgpu::RenderState;
use rsplot::{
    Camera, Colormap, ColormapName, Scatter2D, Scatter2DVisualization, Scene3dGeometry, Vec3,
    install_scene3d, paint_scene3d, set_scene3d,
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
    fn new(rs: &RenderState, mode: Scatter2DVisualization) -> Self {
        install_scene3d(rs);

        // A unit square of four points centred on the optical axis, coloured by
        // value through viridis (the darkest corner, value 0, is still non-black).
        let scatter = Scatter2D::new()
            .with_colormap(Colormap::new(ColormapName::Viridis, 0.0, 3.0))
            .with_size(24.0)
            .with_data(
                &[-0.5, 0.5, -0.5, 0.5],
                &[-0.5, -0.5, 0.5, 0.5],
                &[0.0, 1.0, 2.0, 3.0],
            )
            .with_visualization(mode);
        let mut g = Scene3dGeometry::new();
        scatter.append_to(&mut g);
        set_scene3d(rs, SCENE_ID, &g);

        let camera = Camera::new(
            30.0,
            0.1,
            100.0,
            (1.0, 1.0),
            Vec3::new(0.0, 0.0, 5.0),
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

/// Render one Scatter2D visualization mode and return the count of non-black
/// (rendered-content) pixels inside the scene rect.
fn coloured_pixels(mode: Scatter2DVisualization) -> usize {
    let rs = create_render_state(default_wgpu_setup());
    let app = Rc::new(RefCell::new(App::new(&rs, mode)));
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

    let (x0, y0) = (rect.min.x.max(0.0) as usize, rect.min.y.max(0.0) as usize);
    let (x1, y1) = ((rect.max.x as usize).min(iw), (rect.max.y as usize).min(ih));
    let mut n = 0;
    for y in y0..y1 {
        for x in x0..x1 {
            let i = (y * iw + x) * 4;
            let (r, g, b) = (raw[i], raw[i + 1], raw[i + 2]);
            if r > 40 || g > 40 || b > 40 {
                n += 1;
            }
        }
    }
    n
}

#[test]
fn scatter2d_modes_render_with_solid_filling_more_than_lines() {
    let points = coloured_pixels(Scatter2DVisualization::Points);
    let lines = coloured_pixels(Scatter2DVisualization::Lines);
    let solid = coloured_pixels(Scatter2DVisualization::Solid);

    assert!(
        points > 200,
        "POINTS mode should render four sized markers; got {points} px"
    );
    assert!(
        lines > 0,
        "LINES mode should render the triangulation edges; got {lines} px"
    );
    // SOLID fills the whole triangulated quad; LINES draws only its ~1px edges.
    assert!(
        solid > lines * 3,
        "SOLID fill should cover far more than the LINES edges; solid={solid} lines={lines}"
    );
    assert!(
        solid > 1000,
        "SOLID should fill a large area, not a few stray pixels; got {solid} px"
    );
}
