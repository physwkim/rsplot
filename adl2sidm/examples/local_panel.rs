//! End-to-end adl2sidm example: a MEDM `.adl` screen, converted to Rust and run.
//!
//! `examples/local_panel.adl` is a MEDM screen whose channels are authored as
//! `loc://` / `fake://` addresses, so the converted display drives itself with
//! NO IOC and no network — the `.adl` analogue of `sidm`'s `sidm_local_panel`.
//! `examples/local_panel_screen.rs` is the committed converter output, produced
//! by:
//!
//! ```text
//! cargo run -p adl2sidm -- adl2sidm/examples/local_panel.adl \
//!     -o adl2sidm/examples/local_panel_screen.rs --protocol ""
//! ```
//!
//! (`--protocol ""` because the channels already carry their `loc://`/`fake://`
//! scheme; the default `ca://` prefix would need a live IOC.) This file wires the
//! generated `Screen` into a tiny `eframe` app — the same `new(cc)` / `ui(ui)`
//! shape the converter emits for every screen.
//!
//! The default responsive layout mode (adl2pydm `grid_layout` parity) scales
//! every widget's MEDM rect by `available / native` on each axis, so the panel
//! reflows to fill its window instead of leaving dead space when the window is
//! larger than the 360×460 native screen. (`--absolute` would pin the fixed
//! MEDM pixels instead.)
//!
//! The screen also demonstrates the z-order rule the converter enforces: the
//! grey border `rectangle` (a decoration) overlaps the line edit, slider, and
//! byte controls, yet renders behind them and never steals their clicks, because
//! decoration is placed at `egui::Order::Background` and controls at
//! `Foreground`. (Proportional reflow preserves this overlap and its layering —
//! the scaled rects still overlap, and each keeps its `egui::Order`.)
//!
//! Run with: `cargo run -p adl2sidm --example local_panel`

use eframe::egui;

// The committed converter output. Including it compiles the generated `Screen`
// against the real sidm/siplot APIs (the same gate as `tests/compiles.rs`); here
// we also instantiate and run it.
mod local_panel_screen {
    include!("local_panel_screen.rs");
}
use local_panel_screen::Screen;

/// A minimal `eframe::App` that owns one converted screen and draws it.
struct App {
    screen: Screen,
}

impl App {
    fn new(cc: &eframe::CreationContext<'_>) -> Self {
        // `Screen::new` installs siplot on the wgpu render state, builds the
        // Engine, and connects every widget — all the converter's scaffolding.
        Self {
            screen: Screen::new(cc),
        }
    }
}

impl eframe::App for App {
    fn ui(&mut self, ui: &mut egui::Ui, _frame: &mut eframe::Frame) {
        egui::CentralPanel::default().show_inside(ui, |ui| {
            self.screen.ui(ui);
        });
    }
}

fn main() -> eframe::Result {
    eframe::run_native(
        "adl2sidm — local_panel.adl",
        eframe::NativeOptions {
            renderer: eframe::Renderer::Wgpu,
            ..Default::default()
        },
        Box::new(|cc| Ok(Box::new(App::new(cc)) as Box<dyn eframe::App>)),
    )
}
