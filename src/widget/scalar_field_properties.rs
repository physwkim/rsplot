//! [`ScalarFieldProperties`] — an egui properties panel for a [`ScalarFieldView`].
//!
//! Port of silx `plot3d.tools.GroupPropertiesWidget` adapted to the
//! `ScalarField3D` item set: a form that sets the presentation properties of the
//! field's colormapped item (the cut plane) and its iso-surfaces, then rebuilds
//! the view. silx's `GroupPropertiesWidget` applies one property (colormap /
//! marker / marker size / line width) to *all* `ColormapMixIn` / `SymbolMixIn`
//! items in a group; a `ScalarFieldView` owns one colormapped item (the cut
//! plane) plus solid-colour iso-surfaces, so the panel exposes exactly those:
//!
//! - **Cut plane** — visibility, colormap name (silx `Colormap.setName`), value
//!   range, an autoscale-over-the-volume button (silx autoscales the cut-plane
//!   colormap), and a [`ColorBarWidget`] showing the colormap (the 3D colorbar:
//!   silx's `plot3d` package ships no colorbar of its own, so this reuses the 2D
//!   one — a siplot convenience, not silx parity).
//! - **Iso-surfaces** — per-surface level + colour, a remove button, and an add
//!   button (silx `addIsosurface` / `removeIsosurface`).
//!
//! The properties tree of silx's generic `plot3d._model` (a `QAbstractItemModel`
//! editor of the whole scene graph) is **not** ported: it is a generic
//! scene-graph editor whose faithful port would be speculative for the current
//! item set. This concrete per-field form covers the editable properties of a
//! `ScalarFieldView`.

use egui::Color32;
use egui_wgpu::RenderState;

use crate::core::colormap::{AutoscaleMode, ColormapName};
use crate::widget::colorbar::ColorBarWidget;
use crate::widget::scalar_field_view::ScalarFieldView;

/// A default colour for newly-added iso-surfaces (gold), matching the
/// distinct-from-chrome convention used elsewhere.
const DEFAULT_NEW_ISO_COLOR: Color32 = Color32::from_rgb(255, 215, 0);

/// Stateful properties panel for a [`ScalarFieldView`]. Construct with
/// [`ScalarFieldProperties::new`], then call [`ui`](ScalarFieldProperties::ui)
/// each frame with the view to control.
pub struct ScalarFieldProperties {
    /// The colormap name shown in the picker. Tracked here because
    /// [`Colormap`](crate::core::colormap::Colormap) stores the LUT, not the
    /// name, so it cannot report which catalog entry produced it.
    colormap_name: ColormapName,
    /// The autoscale mode used by the "Autoscale" button.
    autoscale_mode: AutoscaleMode,
}

impl Default for ScalarFieldProperties {
    fn default() -> Self {
        Self::new()
    }
}

impl ScalarFieldProperties {
    /// A panel defaulting to the viridis colormap name and min/max autoscale.
    pub fn new() -> Self {
        Self {
            colormap_name: ColormapName::Viridis,
            autoscale_mode: AutoscaleMode::MinMax,
        }
    }

    /// The colormap name currently shown in the picker.
    pub fn colormap_name(&self) -> ColormapName {
        self.colormap_name
    }

    /// Draw the panel, mutating `view` and rebuilding its geometry when any
    /// property changes. Returns `true` if a change was applied this frame.
    pub fn ui(
        &mut self,
        ui: &mut egui::Ui,
        view: &mut ScalarFieldView,
        render_state: &RenderState,
    ) -> bool {
        let mut changed = false;

        ui.label("Cut plane");

        // Visibility.
        let mut visible = view.field().cut_plane().is_visible();
        if ui.checkbox(&mut visible, "Visible").changed() {
            view.field_mut().cut_plane_mut().set_visible(visible);
            changed = true;
        }

        // Colormap name (silx Colormap.setName: rebuild the LUT, keep the range).
        let mut name = self.colormap_name;
        egui::ComboBox::from_id_salt("scalar_field_cmap")
            .selected_text(name.label())
            .show_ui(ui, |ui| {
                for n in ColormapName::ALL {
                    ui.selectable_value(&mut name, n, n.label());
                }
            });
        if name != self.colormap_name {
            self.colormap_name = name;
            view.field_mut()
                .cut_plane_mut()
                .colormap_mut()
                .set_name(name);
            changed = true;
        }

        // Value range.
        let (mut vmin, mut vmax) = {
            let cm = view.field().cut_plane().colormap();
            (cm.vmin, cm.vmax)
        };
        ui.horizontal(|ui| {
            ui.label("Range");
            let r_min = ui.add(egui::DragValue::new(&mut vmin).speed(0.01));
            let r_max = ui.add(egui::DragValue::new(&mut vmax).speed(0.01));
            if r_min.changed() || r_max.changed() {
                let cm = view.field_mut().cut_plane_mut().colormap_mut();
                cm.vmin = vmin;
                cm.vmax = vmax;
                changed = true;
            }
        });

        // Autoscale the colormap over the volume data.
        ui.horizontal(|ui| {
            egui::ComboBox::from_id_salt("scalar_field_autoscale")
                .selected_text(self.autoscale_mode.label())
                .show_ui(ui, |ui| {
                    for mode in AutoscaleMode::ALL {
                        ui.selectable_value(&mut self.autoscale_mode, mode, mode.label());
                    }
                });
            if ui.button("Autoscale").clicked() {
                view.field_mut()
                    .autoscale_cut_plane_colormap(self.autoscale_mode);
                changed = true;
            }
        });

        // The 3D colorbar: the cut-plane colormap.
        let colorbar =
            ColorBarWidget::new(view.field().cut_plane().colormap().clone()).with_legend("Data");
        colorbar.ui(ui, egui::vec2(64.0, 160.0));

        ui.separator();
        ui.label("Iso-surfaces");

        // Snapshot the iso-surfaces, render editable rows, then apply the edits
        // (the rows borrow the field immutably; the edits need it mutably).
        let isos: Vec<(f32, Color32)> = view
            .field()
            .isosurfaces()
            .iter()
            .map(|iso| (iso.level(), iso.color()))
            .collect();

        let mut level_edits: Vec<(usize, f32)> = Vec::new();
        let mut color_edits: Vec<(usize, Color32)> = Vec::new();
        let mut remove: Option<usize> = None;
        for (i, (level, color)) in isos.iter().enumerate() {
            ui.horizontal(|ui| {
                ui.label(format!("#{i}"));
                let mut lvl = *level;
                if ui
                    .add(egui::DragValue::new(&mut lvl).speed(0.01))
                    .on_hover_text("Iso-level")
                    .changed()
                {
                    level_edits.push((i, lvl));
                }
                let mut col = *color;
                if ui.color_edit_button_srgba(&mut col).changed() {
                    color_edits.push((i, col));
                }
                if ui.button("Remove").clicked() {
                    remove = Some(i);
                }
            });
        }

        for (i, lvl) in level_edits {
            if let Some(iso) = view.field_mut().isosurface_mut(i) {
                iso.set_level(lvl);
                changed = true;
            }
        }
        for (i, col) in color_edits {
            if let Some(iso) = view.field_mut().isosurface_mut(i) {
                iso.set_color(col);
                changed = true;
            }
        }
        if let Some(i) = remove {
            view.field_mut().remove_isosurface(i);
            changed = true;
        }

        if ui.button("Add iso-surface").clicked() {
            // Default the level to the middle of the data range (silx's auto
            // levels centre on the data; a fixed midpoint is the simplest stable
            // default), falling back to 0.5 for an empty field.
            let level = view
                .field()
                .data_range()
                .map(|(min, _, max)| 0.5 * (min + max))
                .unwrap_or(0.5);
            view.field_mut()
                .add_isosurface(level, DEFAULT_NEW_ISO_COLOR);
            changed = true;
        }

        if changed {
            view.rebuild(render_state);
        }
        changed
    }
}
