//! Printer-selection dialog for the toolbar Print action.
//!
//! silx's `PrintAction` opens Qt's `QPrintDialog`, where the user picks a
//! system printer (or "Print to File"). [`PrintDialog`] is the siplot
//! analogue: a small window listing the system printers with the default
//! preselected, a Print button, and a "Save to file…" button that diverts to
//! the figure save dialog (format choice — PNG/JPEG/TIFF/PDF/… — happens in
//! that dialog's file-type filters).
//!
//! The dialog only *signals* the choice via [`PrintDialogAction`]; the owning
//! [`PlotWidget`](crate::PlotWidget) performs the print / save (the same
//! single-owner pattern as the plot context menu and the colorbar drag).

use egui::vec2;

/// What the user chose in the print dialog this frame. The owner acts on it;
/// the dialog never prints or saves itself.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum PrintDialogAction {
    /// Print the figure to the named system printer.
    Print {
        /// The chosen printer's system name.
        printer: String,
    },
    /// Open the figure save dialog instead (QPrintDialog's "Print to File").
    SaveToFile,
}

/// The index the printer combo preselects: the system default when present,
/// else the first printer, clamped to the list (0 for an empty list).
pub fn preselected_printer_index(count: usize, default_index: Option<usize>) -> usize {
    default_index
        .filter(|&i| i < count)
        .unwrap_or(0)
        .min(count.saturating_sub(1))
}

/// Printer-selection dialog state (see the module docs). Owned by
/// [`PlotWidget`](crate::PlotWidget); opened by the toolbar Print button via
/// [`Self::open_with_system_printers`].
pub struct PrintDialog {
    open: bool,
    /// System printer names captured when the dialog opened (a native
    /// enumeration; not refreshed per frame).
    printer_names: Vec<String>,
    /// Index into [`Self::printer_names`] of the combo selection.
    selected: usize,
    win: crate::widget::detached::DetachedWindow,
}

impl Default for PrintDialog {
    fn default() -> Self {
        Self::new()
    }
}

impl PrintDialog {
    /// A closed dialog with no printers captured.
    pub fn new() -> Self {
        Self {
            open: false,
            printer_names: Vec::new(),
            selected: 0,
            win: crate::widget::detached::DetachedWindow::new(
                egui::Id::new("siplot_print_dialog"),
                vec2(300.0, 110.0),
            ),
        }
    }

    /// Whether the dialog is currently open.
    pub fn is_open(&self) -> bool {
        self.open
    }

    /// Open the dialog over the given printer list, preselecting
    /// `default_index` (the system default) when valid. The list may be empty;
    /// the dialog then disables Print and only offers "Save to file…".
    pub fn open_with(&mut self, printer_names: Vec<String>, default_index: Option<usize>) {
        self.selected = preselected_printer_index(printer_names.len(), default_index);
        self.printer_names = printer_names;
        self.open = true;
    }

    /// Open the dialog over the live system printer list (native enumeration,
    /// untestable shim — the list/selection logic it feeds is [`Self::open_with`]).
    pub fn open_with_system_printers(&mut self) {
        let printers = printers::get_printers();
        let names = printers.iter().map(|p| p.system_name.clone()).collect();
        let default = printers.iter().position(|p| p.is_default);
        self.open_with(names, default);
    }

    /// Render the dialog (when open) and report the user's choice this frame.
    /// Print and "Save to file…" both close the dialog; so do Cancel and the
    /// window close button.
    pub fn show(&mut self, ctx: &egui::Context) -> Option<PrintDialogAction> {
        if !self.open {
            return None;
        }
        let mut action = None;
        let pos = self.win.position(ctx);
        let id = self.win.id();
        let size = self.win.size();

        let signals = crate::widget::detached::show_detached(ctx, id, "Print", size, pos, |ui| {
            ui.horizontal(|ui| {
                ui.label("Printer:");
                if self.printer_names.is_empty() {
                    ui.label("No printers found");
                } else {
                    let current = self
                        .printer_names
                        .get(self.selected)
                        .cloned()
                        .unwrap_or_default();
                    egui::ComboBox::from_id_salt("print_dialog_printer")
                        .selected_text(current)
                        .show_ui(ui, |ui| {
                            for (i, name) in self.printer_names.iter().enumerate() {
                                ui.selectable_value(&mut self.selected, i, name);
                            }
                        });
                }
            });

            ui.separator();

            ui.horizontal(|ui| {
                let can_print = !self.printer_names.is_empty();
                if ui
                    .add_enabled(can_print, egui::Button::new("Print"))
                    .clicked()
                    && let Some(name) = self.printer_names.get(self.selected)
                {
                    action = Some(PrintDialogAction::Print {
                        printer: name.clone(),
                    });
                }
                if ui
                    .button("Save to file…")
                    .on_hover_text("Save the figure as PNG / JPEG / TIFF / PDF / … instead")
                    .clicked()
                {
                    action = Some(PrintDialogAction::SaveToFile);
                }
                if ui.button("Cancel").clicked() {
                    action = None;
                    self.open = false;
                }
            });
        });

        if action.is_some() {
            self.open = false;
        }
        let mut open = self.open;
        self.win.apply_signals(&signals, &mut open);
        self.open = open;
        action
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn preselected_index_uses_default_when_valid() {
        assert_eq!(preselected_printer_index(3, Some(2)), 2);
    }

    #[test]
    fn preselected_index_falls_back_to_first_when_default_missing_or_invalid() {
        assert_eq!(preselected_printer_index(3, None), 0);
        assert_eq!(preselected_printer_index(3, Some(7)), 0);
    }

    #[test]
    fn preselected_index_empty_list_is_zero() {
        assert_eq!(preselected_printer_index(0, None), 0);
        assert_eq!(preselected_printer_index(0, Some(1)), 0);
    }

    #[test]
    fn open_with_sets_state_and_clamps_selection() {
        let mut dlg = PrintDialog::new();
        assert!(!dlg.is_open());
        dlg.open_with(vec!["a".into(), "b".into()], Some(1));
        assert!(dlg.is_open());
        assert_eq!(dlg.selected, 1);
        dlg.open_with(vec!["a".into()], Some(5));
        assert_eq!(dlg.selected, 0);
    }

    #[test]
    fn show_closed_returns_none_without_rendering() {
        let ctx = egui::Context::default();
        let mut dlg = PrintDialog::new();
        let _ = ctx.run_ui(egui::RawInput::default(), |ui| {
            assert!(dlg.show(ui.ctx()).is_none());
        });
    }

    #[test]
    fn show_open_renders_without_panic_and_stays_open() {
        // No input this frame: nothing is clicked, so no action and the dialog
        // stays open (exercises the full render path incl. the empty-list arm).
        let ctx = egui::Context::default();
        let mut dlg = PrintDialog::new();
        dlg.open_with(Vec::new(), None);
        let _ = ctx.run_ui(egui::RawInput::default(), |ui| {
            assert!(dlg.show(ui.ctx()).is_none());
        });
        assert!(dlg.is_open());

        let mut dlg = PrintDialog::new();
        dlg.open_with(vec!["Office".into(), "Lab".into()], Some(1));
        let _ = ctx.run_ui(egui::RawInput::default(), |ui| {
            assert!(dlg.show(ui.ctx()).is_none());
        });
        assert!(dlg.is_open());
    }
}
