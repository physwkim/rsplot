# High-level Plot APIs

`egui-silx` exposes two plotting layers:

- `PlotView`: stateless chrome and interaction around a `Plot` model. Existing
  low-level examples use this when they want direct control of GPU item uploads.
- `PlotWidget`, `PlotWindow`, `Plot1D`, `Plot2D`: retained high-level widgets
  that own a `WgpuBackend`, item handles, labels, limits, legend metadata, item
  stats, events, and toolbar helpers.

The high-level examples mirror common silx examples from `silx/examples/`:

| silx example | egui-silx example | Covered APIs |
| --- | --- | --- |
| `plotWidget.py` | `cargo run --example high_level_plot_widget` | toolbar, image, scatter, histogram, legend, active stats |
| `plotLegendsWidget.py` | `cargo run --example high_level_legend`, `high_level_plot_widget`, and `high_level_plot1d` | silx-like legend rows, legend labels, legend selection, item lookup by legend |
| `plotStats.py` | `cargo run --example high_level_plot1d` | curve/scatter/histogram stats and active item stats |
| `plotProfile.py` | `cargo run --example high_level_plot2d` | image display, mask overlay, row/column profile extraction |
| `plotClearAction.py` | `cargo run --example high_level_clear_action` | clear/repopulate actions and item count feedback |
| `plotUpdateCurveFromThread.py` | `cargo run --example high_level_live_update` | retained curve handle updates without creating new items |
| `plotROIStats.py` | `cargo run --example high_level_roi_stats` | editable ROIs and image-pixel statistics |
| `plotItemsSelector.py` | `cargo run --example high_level_items_selector` | retained item table, multi-selection, active item stats |
| `plotContextMenu.py` | `cargo run --example high_level_context_menu` | plot-area context menu, reset/cursor/grid/save actions |
| `shiftPlotAction.py` | `cargo run --example high_level_shift_action` | custom action mutating the active curve in place |
| `plotUpdateImageFromThread.py` and `plotUpdateImageFromGevent.py` | `cargo run --example high_level_live_image` | retained image handle updates without resetting zoom |

`syncPlotLocation.py` is not ported yet. It needs multiple independent
high-level plots visible in one window, while the current wgpu resource path is
still documented as a single-item-set implementation; the backend notes call out
future `HashMap<PlotId, _>` storage for multi-plot state.

## Choosing a Type

Use `PlotWidget` when an application needs the general silx-style item API:

```rust
let mut plot = PlotWidget::new(render_state, 0);
plot.show_toolbar(ui);
plot.show(ui);
```

`PlotWindow` is the silx-style standalone name. In egui the application owns the
native OS window, so it is an alias for `PlotWidget`:

```rust
let mut plot = PlotWindow::new(render_state, 0);
plot.show_with_toolbar(ui);
```

Use `Plot1D` for curve-first views. It sets `X`/`Y` labels and starts with a
major grid:

```rust
let mut plot = Plot1D::new(render_state, 0);
plot.add_curve_with_legend(&x, &y, egui::Color32::YELLOW, "signal");
plot.add_scatter_with_legend(&sx, &sy, egui::Color32::LIGHT_BLUE, "samples");
plot.add_histogram_with_legend(&edges, &counts, egui::Color32::LIGHT_GREEN, "counts")?;
```

Use `Plot2D` for image-first views. It sets image-style labels, keeps data
aspect ratio, and inverts Y so row coordinates read as image rows:

```rust
let mut plot = Plot2D::new(render_state, 0);
plot.set_default_colormap(Colormap::viridis(0.0, 1.0));
let image = plot.try_add_default_image(width, height, &data)?;
plot.set_item_legend(image, "intensity");
plot.add_mask(width, height, &mask, egui::Color32::from_rgba_unmultiplied(255, 80, 80, 96))?;
```

## Items, Legend, and Active State

All added items return an `ItemHandle`. Legend labels are stored in the
high-level widget, not in the renderer:

```rust
let handle = plot.add_curve(&x, &y, egui::Color32::WHITE);
plot.set_item_legend(handle, "reference");
assert_eq!(plot.curve_by_legend("reference"), Some(handle));
plot.set_active_item(Some(handle));
```

The active item drives `show_active_stats` and emits an event:

```rust
for event in plot.drain_events() {
    eprintln!("{event:?}");
}
```

Typed helpers keep item families explicit:

```rust
plot.remove_scatter(scatter_handle);
plot.clear_histograms();
plot.get_all_masks();
```

Updating an existing histogram, scatter, or mask through the generic curve/image
update path preserves its high-level item kind.

## Fallible Image APIs

`ImageData` constructors panic on length mismatch because they are low-level
data types. The high-level API provides fallible wrappers for application input:

```rust
plot.try_add_image(width, height, &pixels, Colormap::viridis(0.0, 1.0))?;
plot.try_add_rgba_image(width, height, &rgba)?;
plot.try_update_image(handle, width, height, &pixels, colormap)?;
```

For non-unit placement, pass `ImageGeometry`:

```rust
plot.add_image_with_geometry(
    width,
    height,
    &pixels,
    colormap,
    ImageGeometry {
        origin: (10.0, 20.0),
        scale: (0.5, 0.5),
        alpha: 0.8,
    },
)?;
```

## Profiles and Masks

`Plot2D` profile helpers return row/column values from scalar image data. They
validate the row-major image shape and index:

```rust
let row = plot.horizontal_profile(width, height, &pixels, row_index)?;
let col = plot.vertical_profile(width, height, &pixels, column_index)?;
```

Masks are rendered as transparent RGBA image overlays:

```rust
plot.add_mask_with_geometry(
    width,
    height,
    &mask,
    egui::Color32::from_rgba_unmultiplied(255, 80, 80, 96),
    ImageGeometry::default(),
)?;
```

## Grid and Toolbar

`show_toolbar` exposes compact icon buttons for reset zoom, select/zoom/pan
interaction mode, cursor, grid, minor grid, aspect ratio, log axes, and axis
inversion. Plot clicks select the topmost pickable item as the active item; in
select mode, primary drags edit ROI handles without starting a box zoom:

```rust
plot.set_graph_grid_mode(GraphGrid::MajorAndMinor);
plot.set_interaction_mode(PlotInteractionMode::Select);
let toolbar = plot.show_toolbar(ui);
let response = plot.show(ui);
```

Use `show_toolbar_with` for application actions that should sit in the same
toolbar row instead of building an ad hoc `ui.horizontal` around every example:

```rust
let (_, clear_clicked) = plot.show_toolbar_with(ui, |ui, _plot| {
    ui.button("Clear").clicked()
});
if clear_clicked {
    plot.clear();
}
plot.show(ui);
```

`show_with_toolbar_with` draws the toolbar, custom controls, and plot in one
call when the extra controls do not need to change the data before the plot is
drawn. The toolbar is intentionally egui-native rather than a Qt action system:
icons are drawn by the widget, and tooltips provide the full action names. Its
return value reports which controls changed during the frame.
