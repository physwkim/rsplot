//! A SiDM control panel for the `epics-rs` **mini-beamline** IOC
//! (`epics-rs/examples/mini-beamline`, run with
//! `cargo run -p mini-beamline --features ioc --bin mini_ioc -- ioc/st.cmd`).
//!
//! Every channel is a live `ca://` PV under the IOC's `mini:` prefix, wired to
//! match the records the IOC actually loads (`db/*.template`, `ioc/st.cmd`):
//!
//! - **Beam current** — `mini:current` (ai, mA, I/O Intr): a [`SidmLabel`]
//!   readout and a scrolling [`SidmTimePlot`] strip chart of the sine source.
//! - **DCM monochromator** — `mini:BraggEAO` (energy setpoint, keV) edited from a
//!   [`SidmLineEdit`] + [`SidmSlider`], with [`SidmLabel`] readbacks for the
//!   energy/theta/wavelength (`mini:BraggERdbkAO`, `mini:BraggThetaRdbkAO`,
//!   `mini:BraggLambdaRdbkAO`) and a [`SidmEnumComboBox`] for the Manual/Auto
//!   mode (`mini:KohzuModeBO`).
//! - **Point detectors** — the three detector readouts `mini:ph:DetValue_RBV`,
//!   `mini:edge:DetValue_RBV`, `mini:slit:DetValue_RBV` trended together on one
//!   [`SidmTimePlot`], with the PinHole exposure time (`mini:ph:ExposureTime`,
//!   user-settable) on a [`SidmLineEdit`].
//! - **Bulk waveform** — `mini:wf1` (10000-element DOUBLE, refreshed at 1 Hz) on
//!   a [`SidmWaveformPlot`].
//! - **MovingDot camera** — the NDStdArrays image `mini:dot:image1:ArrayData`
//!   (640×480 DOUBLE) on a [`SidmImageView`], with Acquire/Stop
//!   [`SidmPushButton`]s and an ImageMode [`SidmEnumComboBox`]
//!   (`mini:dot:cam1:Acquire`, `mini:dot:cam1:ImageMode`).
//!
//! Run (after the IOC is up and reachable):
//! `cargo run -p sidm --example sidm_mini_beamline`
//! (`ca` is a default feature, so no `--features` flag is needed; the
//! `required-features = ["ca"]` gate only skips the example under
//! `--no-default-features`.)
//!
//! Point `EPICS_CA_ADDR_LIST` at the IOC's host if it is not on the local
//! broadcast domain. With no IOC reachable the channels stay disconnected and
//! every widget renders its disconnected state (the PV name in the readout, a
//! dashed border).

use eframe::egui;
use sidm::Engine;
use sidm::widgets::{
    DataMargins, SidmEnumComboBox, SidmImageView, SidmLabel, SidmLineEdit, SidmPushButton,
    SidmSlider, SidmTimePlot, SidmWaveformPlot, TimeAxisMode,
};

// Every PV lives under the IOC's `mini:` prefix (st.cmd `epicsEnvSet PREFIX`).
const PREFIX: &str = "ca://mini:";

// A 5% top/bottom data margin for the strip charts, so the autoscaled curve
// keeps a gap from the axis edges instead of touching them (silx
// `setDataMargins` / pyqtgraph autorange padding). X is left at 0 — the X axis
// is the scrolling time window, not autoscaled.
const STRIP_MARGINS: DataMargins = DataMargins {
    x_min: 0.0,
    x_max: 0.0,
    y_min: 0.05,
    y_max: 0.05,
};

/// Full `ca://` address for a PV under the `mini:` prefix.
fn pv(suffix: &str) -> String {
    format!("{PREFIX}{suffix}")
}

/// The screen's sections, shown one at a time via a tab bar (a QTabWidget-style
/// layout) instead of one tall vertical stack.
#[derive(Clone, Copy, PartialEq, Eq)]
enum Tab {
    BeamCurrent,
    Dcm,
    Detectors,
    Waveform,
    Camera,
}

struct MiniBeamline {
    // The engine owns the tokio runtime and the CA connections; it must outlive
    // the widgets that hold `Channel` handles.
    _engine: Engine,

    // Which section tab is currently shown.
    tab: Tab,

    // Beam current.
    beam_label: SidmLabel,
    beam_plot: SidmTimePlot,

    // DCM monochromator.
    energy_edit: SidmLineEdit,
    energy_slider: SidmSlider,
    energy_rbv: SidmLabel,
    theta_rbv: SidmLabel,
    lambda_rbv: SidmLabel,
    kohzu_mode: SidmEnumComboBox,

    // Point detectors.
    detectors_plot: SidmTimePlot,
    exposure_edit: SidmLineEdit,

    // Bulk waveform.
    waveform_plot: SidmWaveformPlot,

    // MovingDot camera.
    image_view: SidmImageView,
    acquire_start: SidmPushButton,
    acquire_stop: SidmPushButton,
    image_mode: SidmEnumComboBox,
}

impl MiniBeamline {
    fn new(cc: &eframe::CreationContext<'_>) -> Self {
        let rs = cc
            .wgpu_render_state
            .as_ref()
            .expect("eframe must use the wgpu renderer (NativeOptions.renderer = Wgpu)");
        // Required before any siplot GPU widget (Plot1D/TimePlot/ImageView).
        siplot::install(rs);

        let engine = Engine::new();
        // Repaint the window whenever a channel value changes.
        engine.attach_repaint(cc.egui_ctx.clone());

        // --- Beam current ---
        let beam_label = SidmLabel::new(&engine, &pv("current"))
            .expect("connect beam current label")
            .with_precision(2)
            .with_show_units(true);
        let mut beam_plot = SidmTimePlot::new(rs, 0)
            .with_time_span(30.0)
            .with_data_margins(STRIP_MARGINS);
        beam_plot
            .add_channel(
                &engine,
                &pv("current"),
                egui::Color32::from_rgb(0, 200, 255),
                "beam current",
            )
            .expect("connect beam current curve");

        // --- DCM monochromator ---
        let energy_edit = SidmLineEdit::new(&engine, &pv("BraggEAO")).expect("connect energy edit");
        let energy_slider = SidmSlider::new(&engine, &pv("BraggEAO"))
            .expect("connect energy slider")
            // The DCM covers ~5–20 keV (st.cmd widens DCM Z/Y limits for this).
            .with_limits(5.0, 20.0)
            .with_precision(3);
        let energy_rbv = SidmLabel::new(&engine, &pv("BraggERdbkAO"))
            .expect("connect energy readback")
            .with_precision(3);
        let theta_rbv = SidmLabel::new(&engine, &pv("BraggThetaRdbkAO"))
            .expect("connect theta readback")
            .with_precision(4);
        let lambda_rbv = SidmLabel::new(&engine, &pv("BraggLambdaRdbkAO"))
            .expect("connect wavelength readback")
            .with_precision(4);
        let kohzu_mode =
            SidmEnumComboBox::new(&engine, &pv("KohzuModeBO")).expect("connect Kohzu mode");

        // --- Point detectors: three readouts on one strip chart ---
        let mut detectors_plot = SidmTimePlot::new(rs, 1)
            .with_time_span(30.0)
            .with_data_margins(STRIP_MARGINS);
        detectors_plot
            .add_channel(
                &engine,
                &pv("ph:DetValue_RBV"),
                egui::Color32::from_rgb(0, 220, 120),
                "PinHole",
            )
            .expect("connect PinHole curve");
        detectors_plot
            .add_channel(
                &engine,
                &pv("edge:DetValue_RBV"),
                egui::Color32::from_rgb(255, 180, 0),
                "Edge",
            )
            .expect("connect Edge curve");
        detectors_plot
            .add_channel(
                &engine,
                &pv("slit:DetValue_RBV"),
                egui::Color32::from_rgb(230, 80, 200),
                "Slit",
            )
            .expect("connect Slit curve");
        let exposure_edit =
            SidmLineEdit::new(&engine, &pv("ph:ExposureTime")).expect("connect exposure edit");

        // --- Bulk waveform ---
        let mut waveform_plot = SidmWaveformPlot::new(rs, 2);
        waveform_plot
            .add_channel(
                &engine,
                &pv("wf1"),
                egui::Color32::from_rgb(180, 180, 255),
                "wf1",
            )
            .expect("connect waveform curve");

        // --- MovingDot camera: 640×480 DOUBLE image from the NDStdArrays plugin ---
        let image_view = SidmImageView::new(
            &engine,
            rs,
            3,
            &pv("dot:image1:ArrayData"),
            None, // fixed width below; the IOC has no separate width PV in this panel
        )
        .expect("connect camera image")
        .with_width(640)
        .with_normalize(true);
        let acquire_start = SidmPushButton::new(&engine, &pv("dot:cam1:Acquire"), "Acquire", "1")
            .expect("connect acquire start");
        let acquire_stop = SidmPushButton::new(&engine, &pv("dot:cam1:Acquire"), "Stop", "0")
            .expect("connect acquire stop");
        let image_mode =
            SidmEnumComboBox::new(&engine, &pv("dot:cam1:ImageMode")).expect("connect image mode");

        Self {
            _engine: engine,
            tab: Tab::BeamCurrent,
            beam_label,
            beam_plot,
            energy_edit,
            energy_slider,
            energy_rbv,
            theta_rbv,
            lambda_rbv,
            kohzu_mode,
            detectors_plot,
            exposure_edit,
            waveform_plot,
            image_view,
            acquire_start,
            acquire_stop,
            image_mode,
        }
    }
}

impl MiniBeamline {
    /// Beam-current tab: a readout label and the scrolling strip chart.
    fn beam_current_tab(&mut self, ui: &mut egui::Ui) {
        ui.label("Beam current (mini:current):");
        ui.horizontal(|ui| {
            ui.label("Value:");
            self.beam_label.show(ui);
        });
        // Runtime X-axis toggle (PyDM's relative vs absolute time axis): flip the
        // strip chart between seconds-since-start and the local wall-clock time.
        ui.horizontal(|ui| {
            ui.label("X axis:");
            let mut mode = self.beam_plot.time_axis_mode();
            ui.selectable_value(&mut mode, TimeAxisMode::SinceStart, "Since start (s)");
            ui.selectable_value(&mut mode, TimeAxisMode::WallClock, "Wall clock");
            if mode != self.beam_plot.time_axis_mode() {
                self.beam_plot.set_time_axis_mode(mode);
            }
        });
        // Fill the rest of the tab so the strip chart grows with the window.
        ui.allocate_ui(ui.available_size(), |ui| {
            self.beam_plot.show(ui);
        });
    }

    /// DCM monochromator tab: energy entry/slider, read-backs, and mode.
    fn dcm_tab(&mut self, ui: &mut egui::Ui) {
        ui.label("DCM monochromator:");
        ui.horizontal(|ui| {
            ui.label("Energy setpoint (keV):");
            self.energy_edit.show(ui);
        });
        self.energy_slider.show(ui);
        egui::Grid::new("dcm_readbacks")
            .striped(true)
            .show(ui, |ui| {
                ui.label("Energy RBV (keV):");
                self.energy_rbv.show(ui);
                ui.end_row();
                ui.label("Theta RBV (deg):");
                self.theta_rbv.show(ui);
                ui.end_row();
                ui.label("Wavelength RBV (A):");
                self.lambda_rbv.show(ui);
                ui.end_row();
            });
        ui.horizontal(|ui| {
            ui.label("Mode:");
            self.kohzu_mode.show(ui);
        });
    }

    /// Point-detectors tab: the three-pen strip chart and PinHole exposure.
    fn detectors_tab(&mut self, ui: &mut egui::Ui) {
        ui.label("Point detectors (PinHole / Edge / Slit):");
        ui.horizontal(|ui| {
            ui.label("PinHole exposure (s):");
            self.exposure_edit.show(ui);
        });
        // Plot drawn last so it fills the remaining height (exposure sits above it).
        ui.allocate_ui(ui.available_size(), |ui| {
            self.detectors_plot.show(ui);
        });
    }

    /// Bulk-waveform tab: the 10k-point array plot.
    fn waveform_tab(&mut self, ui: &mut egui::Ui) {
        ui.label("Bulk waveform (mini:wf1, 10000 pts @ 1 Hz):");
        // Fill the rest of the tab so the array plot grows with the window.
        ui.allocate_ui(ui.available_size(), |ui| {
            self.waveform_plot.show(ui);
        });
    }

    /// Camera tab: acquire controls, image mode, and the image view.
    fn camera_tab(&mut self, ui: &mut egui::Ui) {
        ui.label("MovingDot camera (mini:dot:image1:ArrayData, 640x480):");
        ui.horizontal(|ui| {
            self.acquire_start.show(ui);
            self.acquire_stop.show(ui);
            ui.label("Image mode:");
            self.image_mode.show(ui);
        });
        // Fill the rest of the tab so the camera image grows with the window.
        ui.allocate_ui(ui.available_size(), |ui| {
            self.image_view.show(ui);
        });
    }
}

impl eframe::App for MiniBeamline {
    fn ui(&mut self, ui: &mut egui::Ui, _frame: &mut eframe::Frame) {
        ui.heading("SiDM — mini-beamline");
        ui.label(
            "Live ca:// PVs from the epics-rs mini-beamline IOC (mini: prefix). \
             Disconnected PVs show a dashed border.",
        );

        // QTabWidget-style tab bar: show one section at a time instead of
        // stacking all five down a long scrolling column.
        ui.separator();
        ui.horizontal(|ui| {
            ui.selectable_value(&mut self.tab, Tab::BeamCurrent, "Beam current");
            ui.selectable_value(&mut self.tab, Tab::Dcm, "DCM monochromator");
            ui.selectable_value(&mut self.tab, Tab::Detectors, "Point detectors");
            ui.selectable_value(&mut self.tab, Tab::Waveform, "Bulk waveform");
            ui.selectable_value(&mut self.tab, Tab::Camera, "Camera");
        });
        ui.separator();

        // Fill the remaining height: each tab's plot/image expands vertically (and
        // horizontally) to the window, so resizing grows the graph instead of
        // leaving dead space below it. No outer ScrollArea — a vertical scroll area
        // hands its child infinite height, which would defeat the vertical fill.
        match self.tab {
            Tab::BeamCurrent => self.beam_current_tab(ui),
            Tab::Dcm => self.dcm_tab(ui),
            Tab::Detectors => self.detectors_tab(ui),
            Tab::Waveform => self.waveform_tab(ui),
            Tab::Camera => self.camera_tab(ui),
        }
    }
}

fn main() -> eframe::Result {
    eframe::run_native(
        "SiDM — mini-beamline",
        eframe::NativeOptions {
            renderer: eframe::Renderer::Wgpu,
            ..Default::default()
        },
        Box::new(|cc| Ok(Box::new(MiniBeamline::new(cc)) as Box<dyn eframe::App>)),
    )
}
