//! The plot's built-in default image colormap is gray with linear
//! normalization — silx `PlotWidget.setDefaultColormap(None)` builds
//! `Colormap(name=silx.config.DEFAULT_COLORMAP_NAME, normalization="linear")`
//! with `DEFAULT_COLORMAP_NAME = "gray"` (PlotWidget.py:3056-3062,
//! _config.py:58). Constructing a `PlotWidget` needs a wgpu render state
//! (real or software).

use egui_kittest::wgpu::{create_render_state, default_wgpu_setup};
use rsplot::{Colormap, ColormapName, Normalization, Plot2D, PlotWidget};

#[test]
fn plot_default_image_colormap_is_gray_linear() {
    let rs = create_render_state(default_wgpu_setup());
    rsplot::install(&rs);

    let plot = PlotWidget::new(&rs, 0);
    let cm = plot.default_colormap();
    assert_eq!(cm.normalization, Normalization::Linear);
    // The LUT is the gray black-to-white ramp (silx `_create_colormap_lut`
    // for "gray": [i, i, i, 255]) — not viridis.
    assert_eq!(cm.lut, Colormap::new(ColormapName::Gray, 0.0, 1.0).lut);
    assert_eq!(cm.lut[0], [0u8, 0, 0, 255]);
    assert_eq!(cm.lut[255], [255u8, 255, 255, 255]);
    assert_ne!(cm.lut, Colormap::viridis(0.0, 1.0).lut);

    // Plot2D inherits the same default (silx Plot2D also uses
    // DEFAULT_COLORMAP_NAME).
    let plot2d = Plot2D::new(&rs, 1);
    assert_eq!(
        plot2d.default_colormap().lut,
        Colormap::new(ColormapName::Gray, 0.0, 1.0).lut
    );
}
