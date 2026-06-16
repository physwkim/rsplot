//! HDF5 "Select a 2D dataset" picker modal in `MaskToolsWidget::show_toolbar`
//! (silx `DatasetDialog`, opened by `_selectDataset`), verified through the
//! egui_kittest harness.
//!
//! The choice logic (`H5DatasetPicker` + `apply_h5_pick`) and the file codecs
//! are unit-tested in `mask_tools.rs`; this exercises the live modal: with a
//! pick parked (`begin_h5_load`/`begin_h5_save`, what the native file dialog's
//! HDF5 branch calls), the rendered modal lists the datasets and its OK button
//! drives the actual read/write. `show_toolbar` is pure egui, so no wgpu render
//! state is needed.

use std::cell::RefCell;
use std::rc::Rc;

use egui_kittest::Harness;
use egui_kittest::kittest::Queryable;
use siplot::MaskToolsWidget;
use siplot::egui;

fn temp_h5(tag: &str) -> std::path::PathBuf {
    let mut path = std::env::temp_dir();
    path.push(format!(
        "siplot_h5_picker_{}_{}.h5",
        tag,
        std::process::id()
    ));
    let _ = std::fs::remove_file(&path);
    path
}

#[test]
fn load_modal_loads_the_chosen_dataset() {
    let path = temp_h5("load");
    // Seed a file with two distinct 2D datasets.
    let mut seed = MaskToolsWidget::new(2, 2);
    seed.mask = vec![1, 2, 3, 4];
    seed.save_h5(&path).expect("write mask dataset");
    seed.mask = vec![9, 9, 9, 9];
    seed.save_h5_dataset(&path, "other")
        .expect("write other dataset");

    let widget = Rc::new(RefCell::new(MaskToolsWidget::new(2, 2)));
    // Open the picker for the file (the rfd file dialog's HDF5 branch does this).
    widget
        .borrow_mut()
        .begin_h5_load(path.clone())
        .expect("begin load");

    let widget_ui = widget.clone();
    let mut harness = Harness::builder()
        .with_size(egui::vec2(1000.0, 400.0))
        .with_pixels_per_point(1.0)
        .build_ui(move |ui| {
            widget_ui.borrow_mut().show_toolbar(ui);
        });
    harness.step();
    harness.step();

    // The modal is up: its load prompt and both dataset choices render.
    let _ = harness.get_by_label("Select a dataset");
    let _ = harness.get_by_label("other");

    // Choose the non-default "other" dataset, then confirm.
    harness.get_by_label("other").click();
    harness.step();
    harness.get_by_label("OK").click();
    harness.step();
    harness.step();

    // The chosen dataset (not the default first one) loaded.
    assert_eq!(
        widget.borrow().mask,
        vec![9, 9, 9, 9],
        "the modal's dataset choice must drive which dataset is read"
    );
    let _ = std::fs::remove_file(&path);
}

#[test]
fn save_modal_writes_the_named_dataset() {
    let path = temp_h5("save");
    // Pre-existing file with a `mask` dataset (so the save modal shows it).
    let mut seed = MaskToolsWidget::new(2, 2);
    seed.mask = vec![1, 1, 1, 1];
    seed.save_h5(&path).expect("seed mask dataset");

    let widget = Rc::new(RefCell::new(MaskToolsWidget::new(2, 2)));
    widget.borrow_mut().mask = vec![5, 6, 7, 8];
    widget
        .borrow_mut()
        .begin_h5_save(path.clone())
        .expect("begin save");

    let widget_ui = widget.clone();
    let mut harness = Harness::builder()
        .with_size(egui::vec2(1000.0, 400.0))
        .with_pixels_per_point(1.0)
        .build_ui(move |ui| {
            widget_ui.borrow_mut().show_toolbar(ui);
        });
    harness.step();
    harness.step();

    // The modal is up in save mode (its prompt mentions typing a new name).
    let _ = harness.get_by_label("Select a dataset or type a new dataset name");

    // The name defaults to silx's `mask`; confirm to overwrite it.
    harness.get_by_label("OK").click();
    harness.step();
    harness.step();

    // Read the file back: the `mask` dataset now holds the widget's mask.
    let mut reader = MaskToolsWidget::new(2, 2);
    reader.load_h5(&path).expect("load saved mask");
    assert_eq!(
        reader.mask,
        vec![5, 6, 7, 8],
        "the save modal must write the current mask to the chosen dataset"
    );
    let _ = std::fs::remove_file(&path);
}
