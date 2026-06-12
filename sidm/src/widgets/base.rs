//! Shared channel-widget behaviour: alarm-severity styling, connection gating,
//! and the middle-click PV copy.
//!
//! Ports the cross-cutting parts of PyDM's `PyDMWidget` mixin
//! (`pydm/widgets/base.py`) plus the alarm palette from
//! `pydm/default_stylesheet.qss`. In Qt these are stylesheet selectors keyed on
//! the `alarmSeverity` / `alarmSensitiveBorder` / `alarmSensitiveContent`
//! dynamic properties; here they are plain functions a widget calls each frame
//! against its [`ChannelState`] snapshot.
//!
//! The palette and the border/content/enabled decisions are pure and unit
//! tested; [`ChannelBase::framed`] is the egui drawing helper that applies them
//! (its rendering is exercised by a headless wgpu readback test).

use siplot::egui::{self, Color32};

use crate::channel::{AlarmSeverity, Channel, ChannelState};

/// PyDM alarm colour for a severity (`default_stylesheet.qss`), or `None` for
/// [`AlarmSeverity::NoAlarm`] (no alarm styling is applied). The same colour
/// drives both the border and the content (text) override.
///
/// `MINOR` `#EBEB00`, `MAJOR` `#FF0000`, `INVALID` `#EB00EB`, `DISCONNECTED`
/// `#FFFFFF` — the values the qss border/content rules use (note the disconnected
/// rules use pure white `#FFFFFF`, not the `WHITE_ALARM = #EBEBEB` constant).
pub fn severity_color(severity: AlarmSeverity) -> Option<Color32> {
    match severity {
        AlarmSeverity::NoAlarm => None,
        AlarmSeverity::Minor => Some(Color32::from_rgb(0xEB, 0xEB, 0x00)),
        AlarmSeverity::Major => Some(Color32::from_rgb(0xFF, 0x00, 0x00)),
        AlarmSeverity::Invalid => Some(Color32::from_rgb(0xEB, 0x00, 0xEB)),
        AlarmSeverity::Disconnected => Some(Color32::WHITE),
    }
}

/// MEDM's alarm colour for a severity — the `alarmColorString` table
/// (medmWidget.c: `"Green3", "Yellow", "Red", "White", "Gray80"`) applied by
/// `alarmColor()` (utils.c). Unlike the PyDM palette it is TOTAL: MEDM's
/// `clrmod="alarm"` replaces the foreground for every severity, so `NO_ALARM`
/// paints Green3 rather than keeping the widget's static colour. The
/// out-of-table arm (`Gray80`) serves [`AlarmSeverity::Disconnected`], a
/// severity MEDM itself never draws (it blanks unconnected widgets).
pub fn severity_color_medm(severity: AlarmSeverity) -> Color32 {
    match severity {
        AlarmSeverity::NoAlarm => Color32::from_rgb(0x00, 0xCD, 0x00),
        AlarmSeverity::Minor => Color32::from_rgb(0xFF, 0xFF, 0x00),
        AlarmSeverity::Major => Color32::from_rgb(0xFF, 0x00, 0x00),
        AlarmSeverity::Invalid => Color32::WHITE,
        AlarmSeverity::Disconnected => Color32::from_rgb(0xCC, 0xCC, 0xCC),
    }
}

/// Which alarm palette drives a widget's severity styling: PyDM's qss palette
/// (tint only while in alarm, keep the configured colour at `NoAlarm`) or
/// MEDM's `alarmColor` table (total replacement, `NO_ALARM` = Green3 — what a
/// converted `.adl` widget with `clrmod="alarm"` needs).
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum AlarmPalette {
    /// PyDM `default_stylesheet.qss` ([`severity_color`]).
    #[default]
    Pydm,
    /// MEDM `alarmColor` ([`severity_color_medm`]).
    Medm,
}

/// The alarm border to draw around a widget: a 2px stroke in the severity
/// colour, dashed only for [`AlarmSeverity::Disconnected`] (qss
/// `2px dashed #FFFFFF`), solid otherwise. `None` for `NoAlarm`.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct BorderStyle {
    /// Stroke colour.
    pub color: Color32,
    /// Stroke width in points (PyDM's `2px`).
    pub width: f32,
    /// Whether the stroke is dashed (disconnected) rather than solid.
    pub dashed: bool,
}

/// The alarm border for a severity, or `None` when no border is drawn.
pub fn alarm_border(severity: AlarmSeverity) -> Option<BorderStyle> {
    severity_color(severity).map(|color| BorderStyle {
        color,
        width: BORDER_WIDTH,
        dashed: severity == AlarmSeverity::Disconnected,
    })
}

/// Which severities draw a widget border. PyDM's `alarmSensitiveBorder` is a
/// boolean (all severities or none); MEDM draws no severity border at all, so
/// a converted screen wants only the SiDM disconnect marker — the dashed
/// outline that stands in for MEDM's white-blanking of unconnected widgets.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum BorderMode {
    /// Severity rings plus the disconnected dash (PyDM
    /// `alarmSensitiveBorder = true`, the default).
    #[default]
    Alarm,
    /// Only the disconnected dash — no ring while connected, whatever the
    /// severity (MEDM-converted screens).
    DisconnectedOnly,
    /// No border ever (PyDM `alarmSensitiveBorder = false`).
    Off,
}

/// Resolve a numeric widget's range: the user override when set, otherwise the
/// PV's control limits (`DRVL`/`DRVH`). `None` when neither is available — the
/// widget then cannot establish a range (PyDM `reset_limits` /
/// `userDefinedLimits`).
pub fn control_range(state: &ChannelState, user_limits: Option<(f64, f64)>) -> Option<(f64, f64)> {
    user_limits.or(state.ctrl_limits)
}

/// The justify flags of `ui`'s layout, captured for [`justified_size`]. Capture
/// them *before* entering a nested `ui.vertical`/`ui.horizontal`, which resets
/// the layout and would hide the caller's justify intent.
pub(crate) fn layout_justify(ui: &egui::Ui) -> (bool, bool) {
    (
        ui.layout().horizontal_justify(),
        ui.layout().vertical_justify(),
    )
}

/// The size a fixed-size widget should allocate: a justified axis fills the
/// available space — growing *or* shrinking, the stock-egui rule (e.g.
/// `ProgressBar` reads `available_width` when justified) — while a
/// non-justified axis keeps `desired`. egui's own justification only ever
/// *expands* the response rect around an exact allocation
/// (`allocate_exact_size` returns the desired size aligned inside it), so a
/// widget that paints a fixed `size` must consult the layout itself or it
/// stays at its native size inside justified containers.
pub(crate) fn justified_size(
    (justify_h, justify_v): (bool, bool),
    ui: &egui::Ui,
    desired: egui::Vec2,
) -> egui::Vec2 {
    egui::vec2(
        if justify_h {
            ui.available_width()
        } else {
            desired.x
        },
        if justify_v {
            ui.available_height()
        } else {
            desired.y
        },
    )
}

/// Arbitrates the MEDM-style middle-click PV copy across overlapping widgets:
/// every widget under the pointer registers a candidate during the pass, and at
/// pass end the SMALLEST rect wins — MEDM's `findSmallestTouchedExecuteElement`
/// (utils.c), which picks the innermost element for `StartDrag` (actions.c).
/// egui's hit-testing cannot arbitrate this for us: the copy responses are
/// hover-sensed (clicks must keep flowing to the widgets' own faces), and
/// `Response::contains_pointer` reports every stacked widget, back-to-front.
#[derive(Default)]
struct MiddleClickCopy {
    /// Best candidate this pass: `(rect area, widget id, clipboard text)`. On
    /// equal areas the later registration wins, so the front-most of two
    /// stacked equal rects provides the names (registration follows draw
    /// order, back-to-front).
    best: Option<(f32, egui::Id, String)>,
    /// The widget whose copy is being announced while the middle button stays
    /// down — it shows the address tooltip (PyDM `show_address_tooltip`; MEDM
    /// keeps its drag icon up while Btn2 is held).
    holding: Option<egui::Id>,
    /// Lazily created PRIMARY-selection owner. The X11 selection serves other
    /// clients only while the owning `Clipboard` lives, so the plugin (alive
    /// for the app's lifetime) keeps it.
    #[cfg(target_os = "linux")]
    primary: Option<arboard::Clipboard>,
    /// Initialisation failed (no X11 display) — logged once, never retried.
    #[cfg(target_os = "linux")]
    primary_unavailable: bool,
}

impl MiddleClickCopy {
    /// Own the X11 PRIMARY selection with the copied names: PyDM sets
    /// `QClipboard.Selection` alongside the clipboard on Linux
    /// (pydm/widgets/base.py), and MEDM's Btn2 delivers the copy through
    /// PRIMARY exclusively (actions.c `selectionConvertProc`) — middle-pasting
    /// PV names into a terminal needs it. egui's `copy_text` reaches only the
    /// CLIPBOARD selection.
    #[cfg(target_os = "linux")]
    fn own_primary_selection(&mut self, text: &str) {
        use arboard::{LinuxClipboardKind, SetExtLinux};
        if self.primary.is_none() && !self.primary_unavailable {
            match arboard::Clipboard::new() {
                Ok(clipboard) => self.primary = Some(clipboard),
                Err(err) => {
                    self.primary_unavailable = true;
                    log::warn!("middle-click copy: PRIMARY selection unavailable: {err}");
                }
            }
        }
        if let Some(clipboard) = &mut self.primary
            && let Err(err) = clipboard
                .set()
                .clipboard(LinuxClipboardKind::Primary)
                .text(text)
        {
            log::warn!("middle-click copy: failed to own the PRIMARY selection: {err}");
        }
    }
}

impl egui::plugin::Plugin for MiddleClickCopy {
    fn debug_name(&self) -> &'static str {
        "sidm_middle_click_copy"
    }

    fn on_end_pass(&mut self, ui: &mut egui::Ui) {
        if let Some((_, id, text)) = self.best.take() {
            #[cfg(target_os = "linux")]
            self.own_primary_selection(&text);
            ui.ctx().copy_text(text);
            self.holding = Some(id);
        }
        if self.holding.is_some()
            && !ui.input(|i| i.pointer.button_down(egui::PointerButton::Middle))
        {
            self.holding = None;
        }
    }
}

/// MEDM Btn2 / PyDM middle-click copy: pressing the middle button over a
/// channel widget copies its PV name(s) to the clipboard, protocol-stripped and
/// space-joined exactly as both references produce them (MEDM `StartDrag` owns
/// the selection with the space-joined record names, actions.c; PyDM
/// `show_address_tooltip` joins `remove_protocol`'d addresses,
/// pydm/widgets/base.py). While the button stays held, the winning widget shows
/// the full address(es) as a tooltip, newline-joined like PyDM's. On Linux the
/// copy also owns the X11 PRIMARY selection (both references do — middle-paste
/// into a terminal). Overlapping widgets resolve through the
/// [`MiddleClickCopy`] plugin.
///
/// Every framed channel widget gets this through [`ChannelBase`]; a custom
/// widget that draws its own response wires it explicitly (the plots do).
pub fn middle_click_copy<'a>(
    ui: &egui::Ui,
    response: &egui::Response,
    addresses: impl IntoIterator<Item = &'a str>,
) {
    let ctx = ui.ctx();
    // Register up-front, NOT on press: `Context::run_ui` snapshots the plugin
    // list before the pass runs, so a plugin added mid-pass only gets its
    // `on_end_pass` from the NEXT pass — registering here (idempotent by type)
    // guarantees the plugin is live by the time a press arrives.
    ctx.add_plugin(MiddleClickCopy::default());
    if !response.contains_pointer() {
        return;
    }
    let (pressed, down) = ui.input(|i| {
        (
            i.pointer.button_pressed(egui::PointerButton::Middle),
            i.pointer.button_down(egui::PointerButton::Middle),
        )
    });
    if !pressed && !down {
        return;
    }
    let raw: Vec<&str> = addresses.into_iter().collect();
    if raw.is_empty() {
        return;
    }
    if pressed {
        let text = raw
            .iter()
            .map(|a| a.split_once("://").map_or(*a, |(_, rest)| rest))
            .collect::<Vec<_>>()
            .join(" ");
        let area = response.rect.area();
        ctx.with_plugin(|p: &mut MiddleClickCopy| {
            if p.best.as_ref().is_none_or(|(best, ..)| area <= *best) {
                p.best = Some((area, response.id, text));
            }
        });
    }
    if down
        && ctx.with_plugin(|p: &mut MiddleClickCopy| p.holding == Some(response.id)) == Some(true)
    {
        response.show_tooltip_text(raw.join("\n"));
    }
}

/// Border stroke width (PyDM `2px`).
const BORDER_WIDTH: f32 = 2.0;
/// Uniform inset reserved around content so the border has room and the content
/// geometry does not shift when an alarm border appears or clears.
const BORDER_INSET: i8 = 3;
/// Dash/gap lengths (points) for the disconnected dashed border.
const DASH_LEN: f32 = 4.0;
const DASH_GAP: f32 = 3.0;

/// Composition base shared by every channel-driven widget: the [`Channel`] plus
/// the two PyDM alarm-sensitivity flags. Widgets embed one of these and call its
/// helpers each frame.
pub struct ChannelBase {
    channel: Channel,
    /// Which severities draw a border (PyDM `alarmSensitiveBorder` defaults to
    /// every severity; see [`BorderMode`]).
    pub border_mode: BorderMode,
    /// Recolour the widget content by alarm severity (PyDM
    /// `alarmSensitiveContent`, default `false`).
    pub alarm_sensitive_content: bool,
    /// The palette severity-sensitive styling draws from (default PyDM).
    pub alarm_palette: AlarmPalette,
    /// The channel is an internal placeholder rather than a user-named PV
    /// (adl2sidm binds synthetic `loc://` addresses to MEDM widgets that carry
    /// no channel, because every sidm widget requires one). Suppresses the
    /// PV-facing surface — the middle-click PV copy and its held address
    /// tooltip — matching MEDM, where Btn2 acts only on elements with records
    /// (`StartDrag` returns without an update task), and PyDM, where a
    /// channel-less widget shows neither.
    pub placeholder_channel: bool,
}

impl ChannelBase {
    /// Wrap a channel with PyDM's default sensitivities (border on, content
    /// off).
    pub fn new(channel: Channel) -> Self {
        Self {
            channel,
            border_mode: BorderMode::default(),
            alarm_sensitive_content: false,
            alarm_palette: AlarmPalette::default(),
            placeholder_channel: false,
        }
    }

    /// The palette's severity colour for `state` — the single owner of the
    /// palette choice. `None` means "no severity override" (PyDM palette at
    /// `NoAlarm`); the MEDM palette always overrides.
    pub fn severity_override(&self, state: &ChannelState) -> Option<Color32> {
        match self.alarm_palette {
            AlarmPalette::Pydm => severity_color(state.effective_severity()),
            AlarmPalette::Medm => Some(severity_color_medm(state.effective_severity())),
        }
    }

    /// The underlying channel (for reading state, writing, the address).
    pub fn channel(&self) -> &Channel {
        &self.channel
    }

    /// Border to draw for `state`, honouring [`Self::border_mode`]. Uses
    /// [`ChannelState::effective_severity`] so a disconnected channel gets the
    /// dashed border regardless of its last wire severity.
    pub fn border(&self, state: &ChannelState) -> Option<BorderStyle> {
        let severity = state.effective_severity();
        match self.border_mode {
            BorderMode::Alarm => alarm_border(severity),
            BorderMode::DisconnectedOnly if severity == AlarmSeverity::Disconnected => {
                alarm_border(severity)
            }
            BorderMode::DisconnectedOnly | BorderMode::Off => None,
        }
    }

    /// Content (text) colour override for `state`, honouring
    /// [`Self::alarm_sensitive_content`]. `None` means "keep the default
    /// colour"; a widget applies this to its text (PyDM `PyDMLabel` /
    /// `PyDMLineEdit` content colour).
    pub fn content_color(&self, state: &ChannelState) -> Option<Color32> {
        if self.alarm_sensitive_content {
            self.severity_override(state)
        } else {
            None
        }
    }

    /// Whether an interactive widget should be enabled: the channel is connected
    /// and, for a writable widget, the PV grants write access (PyDM
    /// `check_enable_state`). A read-only widget (`writable == false`) only needs
    /// the connection.
    pub fn enabled(&self, state: &ChannelState, writable: bool) -> bool {
        state.connected && (!writable || state.write_access)
    }

    /// Lay out `add` inside a severity-styled border, gating the content's
    /// enabled state on connection + write access and wiring the middle-click
    /// PV copy. The returned [`egui::InnerResponse`] carries the content's
    /// value and the frame's response.
    pub fn framed<R>(
        &self,
        ui: &mut egui::Ui,
        state: &ChannelState,
        writable: bool,
        add: impl FnOnce(&mut egui::Ui) -> R,
    ) -> egui::InnerResponse<R> {
        self.framed_with_enabled(ui, state, self.enabled(state, writable), add)
    }

    /// Like [`Self::framed`] but with an explicit `enabled` gate instead of the
    /// connection/write-access rule. Used by container widgets whose enabled
    /// state follows a different rule (e.g. `PyDMFrame`'s `disableOnDisconnect`,
    /// which leaves the frame enabled while disconnected unless opted in).
    pub fn framed_with_enabled<R>(
        &self,
        ui: &mut egui::Ui,
        state: &ChannelState,
        enabled: bool,
        add: impl FnOnce(&mut egui::Ui) -> R,
    ) -> egui::InnerResponse<R> {
        let border = self.border(state);
        let justify = layout_justify(ui);

        let (value, response) = if justify.0 && justify.1 {
            // A both-axis-justified layout is the MEDM-cell contract (the
            // converted screens wrap every widget in one): the widget owns
            // exactly this rect. Flow placement cannot honour that — egui
            // floors rows and buttons at `interact_size`, and the justified
            // parent re-centres an overflowing frame a few px into the clip
            // (measured) — so reserve the rect with one constant allocation
            // and run the content in a child pinned to it. Motif widgets have
            // no minimum size and no margins (MEDM createToggleButtons et
            // al.), so the egui floors are scoped away; the content then fits
            // the cell and the inherited centred layout truly centres it.
            let outer = ui.available_rect_before_wrap();
            let response = ui.allocate_rect(outer, egui::Sense::hover());
            let mut content =
                ui.new_child(egui::UiBuilder::new().max_rect(outer).layout(*ui.layout()));
            {
                let spacing = content.spacing_mut();
                spacing.interact_size = egui::Vec2::ZERO;
                spacing.button_padding = egui::Vec2::ZERO;
            }
            let value = egui::Frame::NONE
                .inner_margin(egui::Margin::same(BORDER_INSET))
                .show(&mut content, |ui| ui.add_enabled_ui(enabled, add).inner)
                .inner;
            (value, response)
        } else {
            let egui::InnerResponse { inner, response } = egui::Frame::NONE
                .inner_margin(egui::Margin::same(BORDER_INSET))
                .show(ui, |ui| ui.add_enabled_ui(enabled, add).inner);
            (inner, response)
        };

        if let Some(style) = border {
            paint_border(ui.painter(), response.rect, &style);
        }
        if !self.placeholder_channel {
            middle_click_copy(ui, &response, [self.channel.address().raw()]);
        }
        egui::InnerResponse::new(value, response)
    }
}

/// Paint the alarm border on `rect`: a solid inside stroke, or a hand-drawn
/// dashed rectangle outline (egui `Frame` strokes are solid only).
fn paint_border(painter: &egui::Painter, rect: egui::Rect, style: &BorderStyle) {
    let stroke = egui::Stroke::new(style.width, style.color);
    if style.dashed {
        let pts = [
            rect.left_top(),
            rect.right_top(),
            rect.right_bottom(),
            rect.left_bottom(),
            rect.left_top(),
        ];
        for shape in egui::Shape::dashed_line(&pts, stroke, DASH_LEN, DASH_GAP) {
            painter.add(shape);
        }
    } else {
        painter.rect_stroke(
            rect,
            egui::CornerRadius::ZERO,
            stroke,
            egui::StrokeKind::Inside,
        );
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn st(connected: bool, write_access: bool, severity: AlarmSeverity) -> ChannelState {
        ChannelState {
            connected,
            write_access,
            severity,
            ..ChannelState::default()
        }
    }

    #[test]
    fn severity_color_matches_pydm_palette() {
        assert_eq!(severity_color(AlarmSeverity::NoAlarm), None);
        assert_eq!(
            severity_color(AlarmSeverity::Minor),
            Some(Color32::from_rgb(0xEB, 0xEB, 0x00))
        );
        assert_eq!(
            severity_color(AlarmSeverity::Major),
            Some(Color32::from_rgb(0xFF, 0x00, 0x00))
        );
        assert_eq!(
            severity_color(AlarmSeverity::Invalid),
            Some(Color32::from_rgb(0xEB, 0x00, 0xEB))
        );
        assert_eq!(
            severity_color(AlarmSeverity::Disconnected),
            Some(Color32::WHITE)
        );
    }

    #[test]
    fn alarm_border_is_solid_except_disconnected() {
        assert_eq!(alarm_border(AlarmSeverity::NoAlarm), None);
        let minor = alarm_border(AlarmSeverity::Minor).expect("minor has a border");
        assert!(!minor.dashed);
        assert_eq!(minor.width, BORDER_WIDTH);
        assert_eq!(minor.color, Color32::from_rgb(0xEB, 0xEB, 0x00));

        let disc = alarm_border(AlarmSeverity::Disconnected).expect("disconnected has a border");
        assert!(disc.dashed);
        assert_eq!(disc.color, Color32::WHITE);
    }

    // The ChannelBase decisions are tested through the pure functions above plus
    // a small live channel for the state-driven helpers below.

    fn base() -> ChannelBase {
        let engine = crate::Engine::new();
        let channel = engine
            .connect("loc://widget_base_test")
            .expect("connect loc channel");
        ChannelBase::new(channel)
    }

    #[test]
    fn border_off_when_alarm_sensitive_border_disabled() {
        let mut b = base();
        b.border_mode = BorderMode::Off;
        // Even a major alarm yields no border when the mode is off.
        assert_eq!(b.border(&st(true, true, AlarmSeverity::Major)), None);
    }

    #[test]
    fn disconnected_only_mode_draws_no_ring_but_keeps_the_dash() {
        let mut b = base();
        b.border_mode = BorderMode::DisconnectedOnly;
        // No ring while connected — whatever the severity (MEDM draws no
        // severity border).
        assert_eq!(b.border(&st(true, true, AlarmSeverity::Minor)), None);
        assert_eq!(b.border(&st(true, true, AlarmSeverity::Major)), None);
        assert_eq!(b.border(&st(true, true, AlarmSeverity::Invalid)), None);
        // The disconnect marker stays.
        let dash = b
            .border(&st(false, false, AlarmSeverity::NoAlarm))
            .expect("disconnected still draws the dash");
        assert!(dash.dashed);
    }

    #[test]
    fn disconnected_state_forces_dashed_border_over_wire_severity() {
        let b = base();
        // Not connected but last wire severity was NoAlarm → effective severity
        // is Disconnected, so the border is the dashed white one.
        let border = b
            .border(&st(false, false, AlarmSeverity::NoAlarm))
            .expect("disconnected draws a border");
        assert!(border.dashed);
        assert_eq!(border.color, Color32::WHITE);
    }

    #[test]
    fn content_color_only_when_sensitive() {
        let mut b = base();
        let major = st(true, true, AlarmSeverity::Major);
        assert_eq!(b.content_color(&major), None);
        b.alarm_sensitive_content = true;
        assert_eq!(b.content_color(&major), Some(Color32::from_rgb(0xFF, 0, 0)));
    }

    #[test]
    fn severity_color_medm_is_total_and_green_at_no_alarm() {
        // MEDM alarmColorString {"Green3","Yellow","Red","White","Gray80"}
        // (medmWidget.c) applied unconditionally under clrmod="alarm"
        // (utils.c alarmColor) — NO_ALARM paints Green3, not the static colour.
        assert_eq!(
            severity_color_medm(AlarmSeverity::NoAlarm),
            Color32::from_rgb(0x00, 0xCD, 0x00)
        );
        assert_eq!(
            severity_color_medm(AlarmSeverity::Minor),
            Color32::from_rgb(0xFF, 0xFF, 0x00)
        );
        assert_eq!(
            severity_color_medm(AlarmSeverity::Major),
            Color32::from_rgb(0xFF, 0x00, 0x00)
        );
        assert_eq!(severity_color_medm(AlarmSeverity::Invalid), Color32::WHITE);
        assert_eq!(
            severity_color_medm(AlarmSeverity::Disconnected),
            Color32::from_rgb(0xCC, 0xCC, 0xCC)
        );
    }

    #[test]
    fn medm_palette_overrides_content_at_no_alarm() {
        let mut b = base();
        b.alarm_sensitive_content = true;
        b.alarm_palette = AlarmPalette::Medm;
        assert_eq!(
            b.content_color(&st(true, true, AlarmSeverity::NoAlarm)),
            Some(Color32::from_rgb(0x00, 0xCD, 0x00))
        );
        assert_eq!(
            b.content_color(&st(true, true, AlarmSeverity::Minor)),
            Some(Color32::from_rgb(0xFF, 0xFF, 0x00))
        );
    }

    #[test]
    fn enabled_requires_connection_and_write_access_when_writable() {
        let b = base();
        // Read-only widget: connection alone suffices.
        assert!(b.enabled(&st(true, false, AlarmSeverity::NoAlarm), false));
        assert!(!b.enabled(&st(false, true, AlarmSeverity::NoAlarm), false));
        // Writable widget: needs connection AND write access.
        assert!(b.enabled(&st(true, true, AlarmSeverity::NoAlarm), true));
        assert!(!b.enabled(&st(true, false, AlarmSeverity::NoAlarm), true));
        assert!(!b.enabled(&st(false, true, AlarmSeverity::NoAlarm), true));
    }
}
