//! `RsdmFrame` — a channel-connected container.
//!
//! Ports `pydm/widgets/frame.py` (`PyDMFrame`): a grouping container that can
//! optionally disable its children when the channel disconnects
//! (`disableOnDisconnect`, default off) and optionally draw the alarm-severity
//! border (`alarmSensitiveBorder`, default *off* for the frame — unlike the
//! value widgets, whose default is on).
//!
//! In immediate mode there is no retained child tree to enable/disable; the
//! frame instead wraps a content closure each frame, gating it with
//! [`egui::Ui::add_enabled_ui`] through [`ChannelBase::framed_with_enabled`]. The
//! one piece of real logic — the enable decision — is the pure
//! [`RsdmFrame::frame_enabled`], unit-tested; the border/inset rendering is the
//! same primitive the other widgets use (verified by the base widget's
//! readback test).

use rsplot::egui;

use crate::engine::{Engine, EngineError};
use crate::widgets::base::{BorderMode, ChannelBase};

/// A channel-connected grouping container (PyDM `PyDMFrame`).
pub struct RsdmFrame {
    base: ChannelBase,
    disable_on_disconnect: bool,
}

impl RsdmFrame {
    /// Connect `address` and wrap it as a frame. The alarm border is off by
    /// default (PyDM `PyDMFrame.alarmSensitiveBorder = False`).
    pub fn new(engine: &Engine, address: &str) -> Result<Self, EngineError> {
        let channel = engine.connect(address)?;
        let mut base = ChannelBase::new(channel);
        base.border_mode = BorderMode::Off;
        Ok(Self {
            base,
            disable_on_disconnect: false,
        })
    }

    /// Disable the frame's contents while the channel is disconnected (builder
    /// style; PyDM `disableOnDisconnect`).
    pub fn with_disable_on_disconnect(mut self, on: bool) -> Self {
        self.disable_on_disconnect = on;
        self
    }

    /// Draw the alarm-severity border (builder style; PyDM
    /// `alarmSensitiveBorder`). Off by default for the frame.
    pub fn with_alarm_sensitive_border(mut self, on: bool) -> Self {
        self.base.border_mode = if on {
            BorderMode::Alarm
        } else {
            BorderMode::Off
        };
        self
    }

    /// Mark the channel as an internal placeholder, not a user-named PV
    /// (builder style; suppresses the middle-click PV copy — adl2rsdm uses
    /// this for MEDM composites that carry no channel).
    pub fn with_placeholder_channel(mut self) -> Self {
        self.base.placeholder_channel = true;
        self
    }

    /// The underlying channel base, for reading state / styling.
    pub fn base(&self) -> &ChannelBase {
        &self.base
    }

    /// Whether the frame's contents should be enabled (PyDM
    /// `check_enable_state`): always enabled unless `disable_on_disconnect` is
    /// set and the channel is disconnected.
    pub fn frame_enabled(disable_on_disconnect: bool, connected: bool) -> bool {
        !disable_on_disconnect || connected
    }

    /// Lay out `add` inside the frame, applying the enable gate and (if enabled)
    /// the alarm border. Returns the content's value alongside the frame
    /// response.
    pub fn show<R>(
        &mut self,
        ui: &mut egui::Ui,
        add: impl FnOnce(&mut egui::Ui) -> R,
    ) -> egui::InnerResponse<R> {
        let state = self.base.channel().state();
        let enabled = Self::frame_enabled(self.disable_on_disconnect, state.connected);
        self.base.framed_with_enabled(ui, &state, enabled, add)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn enabled_unless_disconnected_and_opted_in() {
        // Default (disable_on_disconnect = false): always enabled.
        assert!(RsdmFrame::frame_enabled(false, true));
        assert!(RsdmFrame::frame_enabled(false, false));
        // Opted in: enabled iff connected.
        assert!(RsdmFrame::frame_enabled(true, true));
        assert!(!RsdmFrame::frame_enabled(true, false));
    }

    #[test]
    fn frame_default_border_is_off() {
        let engine = crate::Engine::new();
        let frame = RsdmFrame::new(&engine, "loc://frame_test").expect("connect");
        // PyDM frame defaults alarmSensitiveBorder off (the value widgets are on).
        assert_eq!(frame.base().border_mode, BorderMode::Off);
        assert!(!frame.disable_on_disconnect);
    }
}
