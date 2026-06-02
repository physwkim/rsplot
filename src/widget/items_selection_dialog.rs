//! A reusable widget to select a subset of plot items, grouped by kind.
//!
//! Ports silx `ItemsSelectionDialog.py`: a modal that lists a plot's items in a
//! table of (legend, kind) and lets the user pick a subset, with a side
//! `KindsSelector` that filters which item *kinds* are shown. silx splits the
//! work across `KindsSelector` (a multi-select list of item kinds, all selected
//! by default) and `PlotItemsSelector` (the legend/kind table whose rows are
//! selectable, filtered by the active kinds); `ItemsSelectionDialog` wires them
//! together and exposes `getSelectedItems`.
//!
//! This port is a standalone, GPU-free widget over a caller-owned slice of
//! item entries `(label, PlotItemKind, selected)` — it does *not* reach into a
//! live `Plot2D`/`PlotWidget`, so it composes with any item list (the
//! high-level `examples/high_level_items_selector.rs` builds an ad-hoc flat
//! checkbox list; this widget adds the silx grouping-by-kind and kind filter it
//! lacks). All selection / filtering / grouping logic lives in pure methods so
//! it is unit-testable without an egui context; [`ItemsSelectionDialog::ui`]
//! renders them.

use crate::widget::high_level::PlotItemKind;

/// One selectable item entry: a display label, its [`PlotItemKind`], and
/// whether it is currently selected (silx `PlotItemsSelector` row: legend +
/// kind + row-selection state).
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct SelectableItem {
    /// Display label / legend shown in the row (silx `item.getName()`).
    pub label: String,
    /// The item family this entry belongs to (silx item kind string).
    pub kind: PlotItemKind,
    /// Whether the row is currently selected (silx row selection state).
    pub selected: bool,
}

impl SelectableItem {
    /// Create an item entry with the given label, kind, and initial selection.
    pub fn new(label: impl Into<String>, kind: PlotItemKind, selected: bool) -> Self {
        Self {
            label: label.into(),
            kind,
            selected,
        }
    }
}

/// All item kinds, in the order silx lists them in `PlotWidget.ITEM_KINDS`
/// (curve, image, scatter, histogram, marker, …). Used to render the kind
/// filter and to group rows deterministically.
const KIND_ORDER: [PlotItemKind; 8] = [
    PlotItemKind::Curve,
    PlotItemKind::Image,
    PlotItemKind::Scatter,
    PlotItemKind::Histogram,
    PlotItemKind::Marker,
    PlotItemKind::Mask,
    PlotItemKind::Triangles,
    PlotItemKind::Shape,
];

/// The distinct kinds present in `items`, in [`KIND_ORDER`] order (the initial
/// shown-kinds set: silx `KindsSelector.selectAll()` over the plot's kinds).
fn distinct_kinds(items: &[SelectableItem]) -> Vec<PlotItemKind> {
    KIND_ORDER
        .into_iter()
        .filter(|k| items.iter().any(|it| it.kind == *k))
        .collect()
}

/// A widget to select a subset of plot items, grouped by kind, with a per-kind
/// filter (silx `ItemsSelectionDialog`).
///
/// Holds the caller-supplied item entries plus the set of kinds currently shown
/// (the `KindsSelector` state; every kind present in the entries is shown by
/// default, matching silx's `selectAll()` on construction). Render with
/// [`Self::ui`]; read the chosen subset with [`Self::selected_items`].
///
/// ```ignore
/// let mut dialog = ItemsSelectionDialog::new(vec![
///     SelectableItem::new("A curve", PlotItemKind::Curve, true),
///     SelectableItem::new("An image", PlotItemKind::Image, false),
/// ]);
///
/// // frame loop
/// dialog.ui(ui);
/// let chosen: Vec<&str> = dialog.selected_items().map(|it| it.label.as_str()).collect();
/// ```
pub struct ItemsSelectionDialog {
    items: Vec<SelectableItem>,
    /// Kinds currently shown by the kind filter (silx `KindsSelector`
    /// selection). A kind absent from this list hides its rows. Held as a
    /// `Vec` (deduplicated, [`KIND_ORDER`]-ordered) because [`PlotItemKind`]
    /// implements neither `Ord` nor `Hash`.
    shown_kinds: Vec<PlotItemKind>,
}

impl ItemsSelectionDialog {
    /// Create a dialog over `items`, with every kind that appears in `items`
    /// shown by default (silx `KindsSelector.selectAll()` on construction).
    pub fn new(items: Vec<SelectableItem>) -> Self {
        let shown_kinds = distinct_kinds(&items);
        Self { items, shown_kinds }
    }

    /// Replace the item entries, re-showing every kind present in the new list
    /// (mirrors silx rebuilding the selector when the plot's items change).
    pub fn set_items(&mut self, items: Vec<SelectableItem>) {
        self.shown_kinds = distinct_kinds(&items);
        self.items = items;
    }

    /// All item entries (selected or not), in insertion order.
    pub fn items(&self) -> &[SelectableItem] {
        &self.items
    }

    /// The distinct kinds present in the entries, in [`KIND_ORDER`] order
    /// (silx `KindsSelector` available kinds derived from the plot's items).
    pub fn available_kinds(&self) -> Vec<PlotItemKind> {
        distinct_kinds(&self.items)
    }

    /// Whether `kind` is currently shown by the kind filter.
    pub fn is_kind_shown(&self, kind: PlotItemKind) -> bool {
        self.shown_kinds.contains(&kind)
    }

    /// Show or hide all rows of `kind` via the kind filter (silx
    /// `KindsSelector` selecting / deselecting a kind). Hiding a kind does not
    /// change the per-item selection state — silx's filter only governs
    /// visibility — so re-showing it restores the prior selection.
    pub fn set_kind_shown(&mut self, kind: PlotItemKind, shown: bool) {
        let present = self.shown_kinds.iter().position(|k| *k == kind);
        match (shown, present) {
            (true, None) => self.shown_kinds.push(kind),
            (false, Some(i)) => {
                self.shown_kinds.remove(i);
            }
            _ => {}
        }
    }

    /// Set the selection state of the item at `index` (out-of-range is a
    /// no-op). Mirrors silx row selection in `PlotItemsSelector`.
    pub fn set_selected(&mut self, index: usize, selected: bool) {
        if let Some(item) = self.items.get_mut(index) {
            item.selected = selected;
        }
    }

    /// Toggle the selection state of the item at `index` (out-of-range is a
    /// no-op).
    pub fn toggle_selected(&mut self, index: usize) {
        if let Some(item) = self.items.get_mut(index) {
            item.selected = !item.selected;
        }
    }

    /// The chosen subset: every selected entry **whose kind is currently
    /// shown** by the filter (silx `getSelectedItems` reads the rows of the
    /// filtered table, so a selection hidden by the kind filter is not
    /// returned).
    pub fn selected_items(&self) -> impl Iterator<Item = &SelectableItem> {
        self.items
            .iter()
            .filter(|it| it.selected && self.shown_kinds.contains(&it.kind))
    }

    /// The indices, into [`Self::items`], of the rows that are visible under
    /// the current kind filter, grouped by kind in [`KIND_ORDER`] order. Used
    /// by [`Self::ui`] to render grouped sections; exposed for testing the
    /// grouping/filtering logic without an egui context.
    pub fn visible_groups(&self) -> Vec<(PlotItemKind, Vec<usize>)> {
        let mut groups = Vec::new();
        for kind in KIND_ORDER {
            if !self.shown_kinds.contains(&kind) {
                continue;
            }
            let indices: Vec<usize> = self
                .items
                .iter()
                .enumerate()
                .filter(|(_, it)| it.kind == kind)
                .map(|(i, _)| i)
                .collect();
            if !indices.is_empty() {
                groups.push((kind, indices));
            }
        }
        groups
    }

    /// Render the dialog body: a row of per-kind filter checkboxes (silx
    /// `KindsSelector`) followed by the shown items grouped by kind, each a
    /// selectable checkbox row (silx `PlotItemsSelector`).
    pub fn ui(&mut self, ui: &mut egui::Ui) {
        ui.label("Filter item kinds:");
        ui.horizontal_wrapped(|ui| {
            for kind in self.available_kinds() {
                let mut shown = self.is_kind_shown(kind);
                if ui.checkbox(&mut shown, kind.as_str()).changed() {
                    self.set_kind_shown(kind, shown);
                }
            }
        });

        ui.separator();
        ui.label("Select items:");

        let groups = self.visible_groups();
        if groups.is_empty() {
            ui.weak("No items");
            return;
        }
        egui::ScrollArea::vertical().show(ui, |ui| {
            for (kind, indices) in groups {
                ui.strong(kind.as_str());
                for index in indices {
                    let mut selected = self.items[index].selected;
                    let label = self.items[index].label.clone();
                    if ui.checkbox(&mut selected, label).changed() {
                        self.set_selected(index, selected);
                    }
                }
            }
        });
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn sample() -> ItemsSelectionDialog {
        ItemsSelectionDialog::new(vec![
            SelectableItem::new("curve A", PlotItemKind::Curve, true),
            SelectableItem::new("curve B", PlotItemKind::Curve, false),
            SelectableItem::new("image A", PlotItemKind::Image, true),
            SelectableItem::new("scatter A", PlotItemKind::Scatter, false),
        ])
    }

    #[test]
    fn all_kinds_shown_by_default() {
        let d = sample();
        assert!(d.is_kind_shown(PlotItemKind::Curve));
        assert!(d.is_kind_shown(PlotItemKind::Image));
        assert!(d.is_kind_shown(PlotItemKind::Scatter));
        // A kind absent from the entries is not shown (nothing to show).
        assert!(!d.is_kind_shown(PlotItemKind::Marker));
    }

    #[test]
    fn available_kinds_are_distinct_in_kind_order() {
        // Curve appears twice in the entries but once in available_kinds, in
        // KIND_ORDER order (Curve before Image before Scatter).
        let d = sample();
        assert_eq!(
            d.available_kinds(),
            vec![
                PlotItemKind::Curve,
                PlotItemKind::Image,
                PlotItemKind::Scatter
            ]
        );
    }

    #[test]
    fn selected_items_returns_only_selected_and_shown() {
        let d = sample();
        let labels: Vec<&str> = d.selected_items().map(|it| it.label.as_str()).collect();
        // curve A and image A are selected; curve B and scatter A are not.
        assert_eq!(labels, vec!["curve A", "image A"]);
    }

    #[test]
    fn toggling_selection_changes_the_returned_subset() {
        let mut d = sample();
        // Select curve B (index 1); deselect image A (index 2).
        d.toggle_selected(1);
        d.set_selected(2, false);
        let labels: Vec<&str> = d.selected_items().map(|it| it.label.as_str()).collect();
        assert_eq!(labels, vec!["curve A", "curve B"]);
    }

    #[test]
    fn set_and_toggle_out_of_range_are_noops() {
        let mut d = sample();
        d.set_selected(99, true); // no panic, no change.
        d.toggle_selected(99);
        let labels: Vec<&str> = d.selected_items().map(|it| it.label.as_str()).collect();
        assert_eq!(labels, vec!["curve A", "image A"]);
    }

    #[test]
    fn hiding_a_kind_drops_its_items_from_the_subset_without_clearing_selection() {
        let mut d = sample();
        // Hide curves: curve A is selected but now filtered out of the subset.
        d.set_kind_shown(PlotItemKind::Curve, false);
        let labels: Vec<&str> = d.selected_items().map(|it| it.label.as_str()).collect();
        assert_eq!(labels, vec!["image A"]);
        // Per-item selection is untouched: re-showing curves restores curve A.
        d.set_kind_shown(PlotItemKind::Curve, true);
        let labels: Vec<&str> = d.selected_items().map(|it| it.label.as_str()).collect();
        assert_eq!(labels, vec!["curve A", "image A"]);
    }

    #[test]
    fn visible_groups_filters_and_groups_in_kind_order() {
        let mut d = sample();
        // All kinds shown: three groups (Curve with two rows, Image, Scatter).
        let groups = d.visible_groups();
        assert_eq!(
            groups,
            vec![
                (PlotItemKind::Curve, vec![0, 1]),
                (PlotItemKind::Image, vec![2]),
                (PlotItemKind::Scatter, vec![3]),
            ]
        );

        // Hide Image: it disappears from the groups, others keep their indices.
        d.set_kind_shown(PlotItemKind::Image, false);
        let groups = d.visible_groups();
        assert_eq!(
            groups,
            vec![
                (PlotItemKind::Curve, vec![0, 1]),
                (PlotItemKind::Scatter, vec![3]),
            ]
        );
    }

    #[test]
    fn set_items_reshows_every_new_kind() {
        let mut d = sample();
        d.set_kind_shown(PlotItemKind::Curve, false);
        d.set_items(vec![
            SelectableItem::new("m", PlotItemKind::Marker, true),
            SelectableItem::new("c", PlotItemKind::Curve, true),
        ]);
        // Both new kinds are shown again after set_items.
        assert!(d.is_kind_shown(PlotItemKind::Marker));
        assert!(d.is_kind_shown(PlotItemKind::Curve));
        let labels: Vec<&str> = d.selected_items().map(|it| it.label.as_str()).collect();
        assert_eq!(labels, vec!["m", "c"]);
    }
}
