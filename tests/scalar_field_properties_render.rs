//! Behaviour checks for `ScalarFieldProperties` (plot3d P3.2): the egui
//! properties panel (a port of silx `GroupPropertiesWidget`) drives a
//! `ScalarFieldView` — toggling the cut-plane visibility, autoscaling the
//! colormap over the volume, and adding an iso-surface — through real
//! egui/AccessKit interactions, and rebuilds the view without error.

use egui_kittest::Harness;
use egui_kittest::kittest::Queryable;
use egui_kittest::wgpu::{WgpuTestRenderer, create_render_state, default_wgpu_setup};
use rsplot::egui;
use rsplot::egui_wgpu::RenderState;
use rsplot::{ScalarFieldProperties, ScalarFieldView};
use std::cell::RefCell;
use std::rc::Rc;

const WIN: f32 = 360.0;

/// A `5×5×5` field whose interior `3³` block is `1.0` and the rest `0.0`.
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

struct PanelApp {
    view: ScalarFieldView,
    panel: ScalarFieldProperties,
    rs: RenderState,
}

impl PanelApp {
    fn new(rs: &RenderState) -> Self {
        let mut view = ScalarFieldView::new(rs, 3);
        assert!(view.set_data(rs, &blob(), 5, 5, 5), "5³ blob is valid data");
        // Start the colormap at a non-default range so "Autoscale" visibly
        // changes it (the field data spans [0, 1]).
        {
            let cm = view.field_mut().cut_plane_mut().colormap_mut();
            cm.vmin = 5.0;
            cm.vmax = 10.0;
        }
        Self {
            view,
            panel: ScalarFieldProperties::new(),
            rs: rs.clone(),
        }
    }

    fn ui(&mut self, ui: &mut egui::Ui) {
        self.panel.ui(ui, &mut self.view, &self.rs);
    }
}

#[test]
fn scalar_field_properties_panel_drives_the_view() {
    let rs = create_render_state(default_wgpu_setup());
    let app = Rc::new(RefCell::new(PanelApp::new(&rs)));
    let renderer = WgpuTestRenderer::from_render_state(rs.clone());

    let app_ui = app.clone();
    let mut harness = Harness::builder()
        .with_size(egui::vec2(WIN, WIN))
        .with_pixels_per_point(1.0)
        .renderer(renderer)
        .build_ui(move |ui| app_ui.borrow_mut().ui(ui));

    harness.step();

    // The cut plane is hidden by default.
    assert!(
        !app.borrow().view.field().cut_plane().is_visible(),
        "cut plane starts hidden"
    );

    // Toggle the "Visible" checkbox.
    harness.get_by_label("Visible").click();
    harness.run();
    assert!(
        app.borrow().view.field().cut_plane().is_visible(),
        "the Visible checkbox must show the cut plane"
    );

    // Autoscale the colormap over the volume: [5, 10] -> the data range [0, 1].
    harness.get_by_label("Autoscale").click();
    harness.run();
    {
        let borrow = app.borrow();
        let cm = borrow.view.field().cut_plane().colormap();
        assert!(
            (cm.vmin - 0.0).abs() < 1e-6 && (cm.vmax - 1.0).abs() < 1e-6,
            "Autoscale must fit the colormap to the data range [0, 1]; got [{}, {}]",
            cm.vmin,
            cm.vmax
        );
    }

    // Add an iso-surface.
    assert_eq!(
        app.borrow().view.field().isosurfaces().len(),
        0,
        "no iso-surfaces initially"
    );
    harness.get_by_label("Add iso-surface").click();
    harness.run();
    assert_eq!(
        app.borrow().view.field().isosurfaces().len(),
        1,
        "the Add button must append an iso-surface"
    );
    // Its level defaults to the middle of the data range [0, 1].
    let level = app.borrow().view.field().isosurfaces()[0].level();
    assert!(
        (level - 0.5).abs() < 1e-6,
        "the new iso-surface level should default to the data-range midpoint; got {level}"
    );
}
