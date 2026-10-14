//! Test-only helpers shared by the widget unit tests.

/// Headless-frame runner for the unit tests: [`egui::Context::run_ui`] whose
/// output is safe to drop. No renderer applies texture deltas in a headless
/// test, and epaint 0.36 panics when an unapplied [`egui::TexturesDelta`] is
/// dropped, so every test frame comes through here, which clears the delta
/// before handing the output back.
pub trait HeadlessRun {
    fn run_headless<F: FnMut(&mut egui::Ui)>(&self, raw: egui::RawInput, f: F) -> egui::FullOutput;
}

impl HeadlessRun for egui::Context {
    fn run_headless<F: FnMut(&mut egui::Ui)>(&self, raw: egui::RawInput, f: F) -> egui::FullOutput {
        let mut out = self.run_ui(raw, f);
        out.textures_delta.clear();
        out
    }
}
