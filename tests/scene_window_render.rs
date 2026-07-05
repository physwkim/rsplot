//! Composition checks for `SceneWindow` (plot3d P3.3): the window lays out a
//! viewpoint toolbar, a `ScalarFieldView` scene, and a toggleable
//! `ScalarFieldProperties` side panel. The scene renders an iso-surface through
//! the window (magenta, a colour the chrome cannot make), the "View" toolbar and
//! the properties controls are present, and toggling "Properties" hides the
//! panel.

use egui_kittest::Harness;
use egui_kittest::kittest::Queryable;
use egui_kittest::wgpu::{WgpuTestRenderer, create_render_state, default_wgpu_setup};
use rsplot::SceneWindow;
use rsplot::egui;
use rsplot::egui::Color32;
use rsplot::egui_wgpu::RenderState;
use std::cell::RefCell;
use std::rc::Rc;

const WIN: f32 = 480.0;

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

struct WindowApp {
    window: SceneWindow,
    rs: RenderState,
}

impl WindowApp {
    fn new(rs: &RenderState) -> Self {
        let mut window = SceneWindow::new(rs, 4);
        assert!(
            window.view_mut().set_data(rs, &blob(), 5, 5, 5),
            "5³ blob is valid data"
        );
        window
            .view_mut()
            .add_isosurface(rs, 0.5, Color32::from_rgb(255, 0, 255));
        Self {
            window,
            rs: rs.clone(),
        }
    }

    fn ui(&mut self, ui: &mut egui::Ui) {
        self.window.show(ui, &self.rs);
    }
}

#[test]
fn scene_window_composes_toolbar_scene_and_properties() {
    let rs = create_render_state(default_wgpu_setup());
    let app = Rc::new(RefCell::new(WindowApp::new(&rs)));
    let renderer = WgpuTestRenderer::from_render_state(rs.clone());

    let app_ui = app.clone();
    let mut harness = Harness::builder()
        .with_size(egui::vec2(WIN, WIN))
        .with_pixels_per_point(1.0)
        .renderer(renderer)
        .build_ui(move |ui| app_ui.borrow_mut().ui(ui));

    // Frame 1: scene renders the iso-surface; toolbar + properties controls exist.
    harness.step();
    let image = harness.render().expect("headless wgpu render");
    let (iw, ih) = (image.width() as usize, image.height() as usize);
    let magenta = count_magenta(image.as_raw(), iw, ih);
    assert!(
        magenta > 50,
        "the iso-surface (magenta) must render through the window scene; only {magenta} px"
    );
    assert!(
        harness.query_by_label("View").is_some(),
        "the viewpoint toolbar must be present"
    );
    assert!(
        harness.query_by_label("Autoscale").is_some(),
        "the properties panel must be shown by default"
    );
    // The position/value readout (silx PositionInfoWidget) is present; with no
    // cursor over the scene it reads "-" for the data value.
    assert!(
        harness.query_by_label("Data: -").is_some(),
        "the position/value readout must be present, empty when nothing is picked"
    );

    // Hide the properties panel via the toolbar toggle.
    assert!(app.borrow().window.properties_visible());
    harness.get_by_label("Properties").click();
    harness.run();
    assert!(
        !app.borrow().window.properties_visible(),
        "the Properties toggle must hide the panel"
    );
    assert!(
        harness.query_by_label("Autoscale").is_none(),
        "the properties controls must be gone once the panel is hidden"
    );
    assert!(
        harness.query_by_label("View").is_some(),
        "the viewpoint toolbar stays after hiding the properties panel"
    );
}
