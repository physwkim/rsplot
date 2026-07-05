//! Empirical end-to-end check for the `high_level_mask_tools` pencil bug report
//! ("the pen draws to the LEFT of the click position").
//!
//! Static analysis of the coordinate chain (pixel_to_data → painted cell →
//! overlay upload → GPU ortho) shows the image, the cursor mapping, and the
//! mask overlay all share the single `plot.transform(area)`, so there should be
//! no offset. This test proves that empirically: it reproduces the example's
//! exact wiring (bare `Plot2D` + `MaskToolsWidget`, pencil tool, Select mode
//! flipped after `show`) inside `egui_kittest`'s headless wgpu renderer, injects
//! a primary click at the data-area centre, renders the frame that paints the
//! overlay, and measures where the opaque-red painted cell actually lands on
//! screen versus where the click was.
//!
//! Needs a GPU (real or software): it constructs a wgpu `RenderState` and reads
//! back the rendered texture, mirroring `examples/gallery.rs`.

use egui_kittest::Harness;
use egui_kittest::wgpu::{WgpuTestRenderer, create_render_state, default_wgpu_setup};
use rsplot::egui_wgpu::RenderState;
use rsplot::{Colormap, MaskTool, MaskToolsWidget, Plot2D, PlotInteractionMode, Transform, egui};
use std::cell::RefCell;
use std::rc::Rc;

const W: u32 = 128;
const H: u32 = 96;

/// The example's app, reduced to the data path under test (no toolbar UI — the
/// pencil tool and the opaque-red mask colour are set directly so the painted
/// cell is trivially findable in the rendered image).
struct App {
    plot: Plot2D,
    mask: MaskToolsWidget,
    /// The data area + axes of the most recent frame, captured for the test to
    /// pick a click target and project the painted cell back to screen.
    last_transform: Option<Transform>,
}

impl App {
    fn new(rs: &RenderState) -> Self {
        let mut plot = Plot2D::new(rs, 0);
        plot.set_default_colormap(Colormap::viridis(0.0, 1.0));
        let pixels = build_image();
        plot.try_add_default_image(W, H, &pixels)
            .expect("image dims match");

        let mut mask = MaskToolsWidget::new(W, H);
        // Pencil + opaque pure red: the painted level (1) has no per-level
        // override, so it renders with `color` at `alpha`. Pure red is absent
        // from viridis, so a simple threshold isolates the painted cell.
        mask.active_tool = MaskTool::Pencil;
        mask.color = egui::Color32::from_rgb(255, 0, 0);
        mask.alpha = 1.0;

        Self {
            plot,
            mask,
            last_transform: None,
        }
    }

    /// The shipped example's `ui()` body: reserve the primary drag with
    /// `MaskDraw` while a tool is active, show the plot, drive the active tool
    /// (`handle_draw`), then upload the overlay.
    fn ui(&mut self, ui: &mut egui::Ui) {
        let want = if self.mask.active_tool != MaskTool::None {
            PlotInteractionMode::MaskDraw
        } else {
            PlotInteractionMode::Zoom
        };
        if self.plot.interaction_mode() != want {
            self.plot.set_interaction_mode(want);
        }
        let resp = self.plot.show(ui);
        self.mask.handle_draw(ui, &resp);
        self.mask.apply(&mut self.plot);
        self.last_transform = Some(resp.transform);
    }
}

fn build_image() -> Vec<f32> {
    let mut pixels = Vec::with_capacity((W * H) as usize);
    for row in 0..H {
        for col in 0..W {
            let cx = (col as f32 - W as f32 / 2.0) / (W as f32 / 4.0);
            let cy = (row as f32 - H as f32 / 2.0) / (H as f32 / 4.0);
            pixels.push((-0.5 * (cx * cx + cy * cy)).exp());
        }
    }
    pixels
}

/// Centroid (in image pixels) of the strongly-red pixels — the painted overlay
/// cell. `None` when no red pixel is present (overlay not rendered).
fn red_centroid(img: &[u8], width: u32, height: u32) -> Option<(f64, f64)> {
    let (mut sx, mut sy, mut n) = (0.0f64, 0.0f64, 0u64);
    for y in 0..height {
        for x in 0..width {
            let i = ((y * width + x) * 4) as usize;
            let (r, g, b) = (img[i], img[i + 1], img[i + 2]);
            if r > 180 && g < 80 && b < 80 {
                sx += x as f64;
                sy += y as f64;
                n += 1;
            }
        }
    }
    (n > 0).then(|| (sx / n as f64, sy / n as f64))
}

/// Like [`red_centroid`] but only counts pixels inside the physical rect
/// `(x0, y0, x1, y1)` — used to exclude toolbar chrome (e.g. the red mask-color
/// swatch) from the measurement of the painted overlay cell.
fn red_centroid_in(
    img: &[u8],
    width: u32,
    height: u32,
    (x0, y0, x1, y1): (f64, f64, f64, f64),
) -> Option<(f64, f64)> {
    let (mut sx, mut sy, mut n) = (0.0f64, 0.0f64, 0u64);
    for y in 0..height {
        for x in 0..width {
            if (x as f64) < x0 || (x as f64) >= x1 || (y as f64) < y0 || (y as f64) >= y1 {
                continue;
            }
            let i = ((y * width + x) * 4) as usize;
            let (r, g, b) = (img[i], img[i + 1], img[i + 2]);
            if r > 180 && g < 80 && b < 80 {
                sx += x as f64;
                sy += y as f64;
                n += 1;
            }
        }
    }
    (n > 0).then(|| (sx / n as f64, sy / n as f64))
}

#[test]
fn pencil_paints_under_the_click_not_left_of_it() {
    // ppp 1.0 (standard) and 2.0 (macOS Retina, the reporter's display): a HiDPI
    // coordinate mismatch would only surface at ppp != 1.0.
    run_case(1.0);
    run_case(2.0);
}

fn run_case(ppp: f32) {
    let rs = create_render_state(default_wgpu_setup());
    rsplot::install(&rs);

    let app = Rc::new(RefCell::new(App::new(&rs)));
    let renderer = WgpuTestRenderer::from_render_state(rs);

    let app_ui = app.clone();
    let mut harness = Harness::builder()
        .with_size(egui::vec2(900.0, 600.0))
        .with_pixels_per_point(ppp)
        .renderer(renderer)
        .build_ui(move |ui| app_ui.borrow_mut().ui(ui));

    // Frame 1: lay out, capture the transform/data area.
    harness.step();
    let transform = app.borrow().last_transform.expect("transform captured");
    let area = transform.area;
    let click = area.center();

    // Inject a primary click (press + release at the same point) at the centre.
    harness.hover_at(click);
    harness.drag_at(click);
    harness.step(); // press frame
    harness.drop_at(click);
    harness.step(); // release frame: clicked_by → paint cell, apply() adds overlay
    harness.step(); // overlay item now renders
    harness.step(); // settle

    // The pencil must have painted exactly one cell (brush_size 1).
    let painted: Vec<usize> = app
        .borrow()
        .mask
        .mask
        .iter()
        .enumerate()
        .filter_map(|(i, &lvl)| (lvl != 0).then_some(i))
        .collect();
    assert!(
        !painted.is_empty(),
        "pencil click painted no mask cell at all"
    );

    // Cell the click maps to, via the SAME transform the render used.
    let (dx, dy) = transform.pixel_to_data(click);
    let expected_cell = (dy.floor() as i64, dx.floor() as i64); // (row, col)
    let painted_cell = {
        let idx = painted[0];
        ((idx as u32 / W) as i64, (idx as u32 % W) as i64)
    };
    assert_eq!(
        painted_cell, expected_cell,
        "painted cell {painted_cell:?} != cell under the click {expected_cell:?}"
    );

    // Render and locate the painted overlay on screen.
    let image = harness.render().expect("headless wgpu render");
    let (iw, ih) = (image.width(), image.height());
    let centroid = red_centroid(image.as_raw(), iw, ih)
        .expect("painted red overlay cell must be visible in the rendered image");

    // The click is in logical points; the rendered image is physical pixels.
    let click_px = (click.x as f64 * ppp as f64, click.y as f64 * ppp as f64);
    let off_x = centroid.0 - click_px.0;
    let off_y = centroid.1 - click_px.1;

    // One data cell in screen pixels, the natural alignment tolerance: the
    // painted cell centre is within half a cell of the click, plus AA spread.
    let cell_px = (area.width() as f64 / W as f64) * ppp as f64;
    let tol = cell_px * 2.0 + 4.0;

    eprintln!(
        "ppp={ppp} click_px=({:.1},{:.1}) overlay_centroid=({:.1},{:.1}) offset=({:.1},{:.1}) cell_px={:.2} tol={:.2}",
        click_px.0, click_px.1, centroid.0, centroid.1, off_x, off_y, cell_px, tol
    );

    assert!(
        off_x.abs() <= tol && off_y.abs() <= tol,
        "ppp={ppp}: rendered pencil overlay is offset from the click by ({off_x:.1},{off_y:.1}) px \
         (tolerance {tol:.1}); negative x means drawn LEFT of the click"
    );
}

/// Faithful mirror of `examples/high_level_mask_tools.rs`: the real `ui()` body
/// (`ui.vertical` → `show_toolbar` → `separator` → `show` → `handle_draw` →
/// `apply`), the example's title + graph cursor, on a Retina (ppp 2.0) surface.
/// Measures the painted overlay against the CHROME position the user actually
/// compares it to (`data_to_pixel`), not the click — directly testing the report
/// "the mask is drawn ~20px LEFT of the mouse / polygon points".
struct MirrorApp {
    plot: Plot2D,
    mask: MaskToolsWidget,
    last_transform: Option<Transform>,
}

impl MirrorApp {
    fn new(rs: &RenderState) -> Self {
        let mut plot = Plot2D::new(rs, 0);
        plot.set_graph_title("Interactive Masking (Hover and Drag)");
        plot.set_graph_cursor(true);
        plot.set_default_colormap(Colormap::viridis(0.0, 1.0));
        plot.try_add_default_image(W, H, &build_image())
            .expect("image dims match");

        let mut mask = MaskToolsWidget::new(W, H);
        mask.active_tool = MaskTool::Pencil;
        mask.color = egui::Color32::from_rgb(255, 0, 0);
        mask.alpha = 1.0;

        Self {
            plot,
            mask,
            last_transform: None,
        }
    }

    fn ui(&mut self, ui: &mut egui::Ui) {
        ui.vertical(|ui| {
            self.mask.show_toolbar(ui);
            ui.separator();
            let want = if self.mask.active_tool != MaskTool::None {
                PlotInteractionMode::MaskDraw
            } else {
                PlotInteractionMode::Zoom
            };
            if self.plot.interaction_mode() != want {
                self.plot.set_interaction_mode(want);
            }
            let resp = self.plot.show(ui);
            self.mask.handle_draw(ui, &resp);
            self.mask.apply(&mut self.plot);
            self.last_transform = Some(resp.transform);
        });
    }
}

#[test]
fn overlay_aligns_with_chrome_in_mirror_of_example() {
    // Retina ppp (the reporter's display): the overlay drift only surfaced at
    // ppp != 1.0 in the toolbar-overflow regime.
    let ppp = 2.0f32;
    let rs = create_render_state(default_wgpu_setup());
    rsplot::install(&rs);

    let app = Rc::new(RefCell::new(MirrorApp::new(&rs)));
    let renderer = WgpuTestRenderer::from_render_state(rs);

    let app_ui = app.clone();
    let mut harness = Harness::builder()
        .with_size(egui::vec2(800.0, 600.0))
        .with_pixels_per_point(ppp)
        .renderer(renderer)
        .build_ui(move |ui| app_ui.borrow_mut().ui(ui));

    // Two frames: the wrapped toolbar's height settles, so the plot's data area
    // is stable before we click.
    harness.step();
    harness.step();
    let transform = app.borrow().last_transform.expect("transform captured");
    let area = transform.area;
    // The mask toolbar (`show_toolbar`) is far wider than the 800-pt window. A
    // non-wrapping `ui.horizontal` grows the surrounding ui past the window, so
    // the plot over-allocates and `area` exceeds the window; egui-wgpu then
    // clamps the image viewport to the window while the chrome's `data_to_pixel`
    // is unclamped, drifting the overlay LEFT of the cursor/polygon points.
    // `horizontal_wrapped` keeps the toolbar (and thus `area`) within the window.
    assert!(
        area.max.x <= 800.0,
        "data area right edge {:.0} exceeds the 800-pt window — the toolbar still \
         overflows and the wgpu overlay will diverge from the chrome",
        area.max.x
    );

    let click = area.center();
    harness.hover_at(click);
    harness.drag_at(click);
    harness.step();
    harness.drop_at(click);
    harness.step();
    harness.step();
    harness.step();

    let painted: Vec<usize> = app
        .borrow()
        .mask
        .mask
        .iter()
        .enumerate()
        .filter_map(|(i, &lvl)| (lvl != 0).then_some(i))
        .collect();
    assert!(!painted.is_empty(), "pencil click painted no mask cell");

    let image = harness.render().expect("headless wgpu render");
    let (iw, ih) = (image.width(), image.height());
    // Restrict the red search to the physical data-area rect so the toolbar's
    // red mask-colour swatch is not mistaken for the painted overlay cell.
    let centroid = red_centroid_in(
        image.as_raw(),
        iw,
        ih,
        (
            area.min.x as f64 * ppp as f64,
            area.min.y as f64 * ppp as f64,
            area.max.x as f64 * ppp as f64,
            area.max.y as f64 * ppp as f64,
        ),
    )
    .expect("painted red overlay cell must be visible in the data area");

    // CHROME ground truth: the centre of the painted cell, projected to the
    // screen the SAME way the chrome (axes, polygon points, crosshair) is —
    // `data_to_pixel` (logical) × ppp. This is exactly what the user compares
    // the overlay against ("the mouse / polygon point positions").
    let idx = painted[0];
    let (cell_col, cell_row) = ((idx as u32 % W) as f64, (idx as u32 / W) as f64);
    let chrome_pt = transform.data_to_pixel(cell_col + 0.5, cell_row + 0.5);
    let chrome_px = (
        chrome_pt.x as f64 * ppp as f64,
        chrome_pt.y as f64 * ppp as f64,
    );

    let off_x = centroid.0 - chrome_px.0;
    let off_y = centroid.1 - chrome_px.1;
    let cell_px = (area.width() as f64 / W as f64) * ppp as f64;

    // The overlay cell centre must land on the chrome projection of that same
    // cell centre, within AA + half-cell spread. A ~20px left drift fails here.
    let tol = cell_px + 4.0;
    assert!(
        off_x.abs() <= tol && off_y.abs() <= tol,
        "wgpu overlay is offset from the CHROME position by ({off_x:.1},{off_y:.1}) px \
         (tol {tol:.1}); negative x = drawn LEFT of the chrome/mouse/polygon points"
    );
}

/// The rectangle tool must actually mask the dragged region (the example
/// previously never drove the shape-draw path, so shapes were invisible).
#[test]
fn rectangle_tool_masks_the_dragged_region() {
    let rs = create_render_state(default_wgpu_setup());
    rsplot::install(&rs);

    let app = Rc::new(RefCell::new(App::new(&rs)));
    app.borrow_mut().mask.active_tool = MaskTool::Rectangle;
    let renderer = WgpuTestRenderer::from_render_state(rs);

    let app_ui = app.clone();
    let mut harness = Harness::builder()
        .with_size(egui::vec2(900.0, 600.0))
        .with_pixels_per_point(1.0)
        .renderer(renderer)
        .build_ui(move |ui| app_ui.borrow_mut().ui(ui));

    harness.step();
    let transform = app.borrow().last_transform.expect("transform captured");
    let area = transform.area;
    // Drag a rectangle inside the data area.
    let p0 = area.center() - egui::vec2(60.0, 40.0);
    let p1 = area.center() + egui::vec2(60.0, 40.0);

    harness.hover_at(p0);
    harness.drag_at(p0);
    harness.step(); // press
    // Drag incrementally toward the far corner. A real drag moves continuously,
    // so egui reports drag_started on the first move that clears its click-vs-drag
    // threshold — which must be near p0, hence the small first waypoint (a single
    // jump straight to p1 would make egui report the start already at p1, drawing
    // a degenerate one-cell rectangle).
    for t in [0.15f32, 0.4, 0.7, 1.0] {
        harness.hover_at(p0 + (p1 - p0) * t);
        harness.step();
    }
    harness.drop_at(p1);
    harness.step(); // release: finish → fill the rectangle
    harness.step(); // settle

    let masked = app.borrow().mask.mask.iter().filter(|&&l| l != 0).count();
    assert!(
        masked > 0,
        "rectangle tool masked no cells — the shape-draw path is not wired"
    );

    // The rectangle's centre cell must be masked.
    let (mx, my) = transform.pixel_to_data(area.center());
    let (col, row) = (mx.floor() as i64, my.floor() as i64);
    let idx = (row as usize) * (W as usize) + col as usize;
    assert!(
        app.borrow().mask.mask[idx] != 0,
        "rectangle centre cell ({row},{col}) is not masked"
    );
}
