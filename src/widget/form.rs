//! Qt-style form-layout helpers, used by the dialog widgets and exported for
//! applications building silx-style forms on top of rsplot. silx's dialogs
//! are `QFormLayout`s — labels in one aligned column, fields stretching to
//! the dialog edge, action buttons hugging the right — which plain
//! left-to-right rows don't reproduce: every row's field starts where its
//! own label ends, and a field can't know how much width trailing buttons
//! will need.
//!
//! Layout model (one pass, no retained state): inside a `ui.horizontal` row,
//! [`label`] occupies a fixed-width cell so stacked rows align; [`fill`]
//! lays its trailing widgets from the row's right edge inward, then hands
//! the space left in the middle to the field closure as a left-to-right
//! `Ui` — size a widget to `ui.available_width()` there to stretch it up to
//! the trailing widgets (Qt's field stretch).

use egui::{Align, Layout, Ui};

/// A fixed-width, left-aligned, vertically centered cell: the generic column
/// building block ([`label`] is `cell` holding a `ui.label`). Content wider
/// than `width` widens the cell rather than truncating.
pub fn cell<R>(ui: &mut Ui, width: f32, add_contents: impl FnOnce(&mut Ui) -> R) -> R {
    let h = ui.spacing().interact_size.y;
    ui.allocate_ui_with_layout(
        egui::vec2(width, h),
        Layout::left_to_right(Align::Center),
        |ui| {
            ui.set_min_size(egui::vec2(width, h));
            add_contents(ui)
        },
    )
    .inner
}

/// The fixed label column of a form row: a `width`-wide [`cell`] holding a
/// label, so stacked rows align their fields like a `QFormLayout` label
/// column. Returns the label's response (for hover text).
pub fn label(ui: &mut Ui, width: f32, text: impl Into<egui::WidgetText>) -> egui::Response {
    cell(ui, width, |ui| ui.label(text))
}

/// Fill the rest of a form row: `trailing` is laid out from the row's right
/// edge inward (right-aligned buttons, delete boxes), then `field` receives
/// what is left in the middle as a left-to-right `Ui`. A field sized to
/// `ui.available_width()` stretches exactly up to the trailing widgets; a
/// fixed-size field simply sits left in the gap. Pass an empty `trailing`
/// closure when the row has no trailing widgets.
pub fn fill<R>(ui: &mut Ui, trailing: impl FnOnce(&mut Ui), field: impl FnOnce(&mut Ui) -> R) -> R {
    ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
        trailing(ui);
        ui.with_layout(Layout::left_to_right(Align::Center), field)
            .inner
    })
    .inner
}

/// A `ui.group` spanning the available width, like a `QGroupBox` stacked in
/// a dialog (egui groups shrink-wrap to their content, which leaves ragged
/// right edges when boxes stack).
pub fn group<R>(ui: &mut Ui, add_contents: impl FnOnce(&mut Ui) -> R) -> egui::InnerResponse<R> {
    ui.group(|ui| {
        ui.set_width(ui.available_width());
        add_contents(ui)
    })
}
