//! Channel value model and per-frame state snapshot.
//!
//! These are the pure data types every plugin produces and every widget reads.
//! They mirror the payload PyDM's `PyDMChannel` carries across its per-slot
//! signals (`pydm/widgets/channel.py`, `pydm/data_plugins/plugin.py`):
//! value, alarm severity, connection/write-access state, enum strings, units,
//! precision, control/alarm/warning limits, and a timestamp. In an immediate
//! mode GUI these collapse into one [`ChannelState`] snapshot that the tokio
//! side updates and widgets read each frame.
//!
//! The live [`Channel`]/`Connection`/engine types that own and publish a
//! [`ChannelState`] are added in a later commit (they require the async
//! runtime); this module stays pure and headlessly testable.

use std::sync::Arc;
use std::time::SystemTime;

/// EPICS alarm severity. Variants 0..=3 are the wire severities
/// (`NO_ALARM`/`MINOR`/`MAJOR`/`INVALID`); [`AlarmSeverity::Disconnected`] is a
/// widget-only state PyDM derives when a channel is not connected
/// (`ALARM_DISCONNECTED = 4`), never sent by a backend.
#[derive(Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Debug, Default, Hash)]
pub enum AlarmSeverity {
    /// `NO_ALARM` (0).
    #[default]
    NoAlarm,
    /// `MINOR` (1).
    Minor,
    /// `MAJOR` (2).
    Major,
    /// `INVALID` (3).
    Invalid,
    /// Engine-derived disconnected state (4).
    Disconnected,
}

impl AlarmSeverity {
    /// Map an EPICS wire severity (0..=3) to a variant. Values `>= 3` clamp to
    /// [`AlarmSeverity::Invalid`] (the highest wire severity); `Disconnected`
    /// is never produced here.
    pub fn from_epics(severity: u16) -> Self {
        match severity {
            0 => Self::NoAlarm,
            1 => Self::Minor,
            2 => Self::Major,
            _ => Self::Invalid,
        }
    }

    /// The PyDM numeric code (`NO_ALARM=0` … `DISCONNECTED=4`).
    pub fn as_code(self) -> u16 {
        match self {
            Self::NoAlarm => 0,
            Self::Minor => 1,
            Self::Major => 2,
            Self::Invalid => 3,
            Self::Disconnected => 4,
        }
    }
}

/// A normalized channel value. Backends (`EpicsValue`, pvAccess `PvField`)
/// convert into this once, on the tokio side, so the per-frame path is
/// allocation-free: cloning a `PvValue` only bumps `Arc` refcounts.
#[derive(Clone, Debug, PartialEq)]
pub enum PvValue {
    /// Integer scalar.
    Int(i64),
    /// Floating-point scalar.
    Float(f64),
    /// Boolean scalar.
    Bool(bool),
    /// String scalar.
    Str(Arc<str>),
    /// Enumeration: the index plus its resolved label (when enum strings are
    /// known).
    Enum {
        /// Selected index.
        index: u16,
        /// Resolved label for `index`, if the enum strings are cached.
        label: Option<Arc<str>>,
    },
    /// Floating-point waveform.
    FloatArray(Arc<[f64]>),
    /// Integer waveform.
    IntArray(Arc<[i64]>),
    /// String array.
    StrArray(Arc<[String]>),
    /// Raw byte array (EPICS `CHAR` waveform; may carry a NUL-terminated
    /// string).
    Bytes(Arc<[u8]>),
}

impl PvValue {
    /// Numeric view of a scalar value: `Int`/`Float`/`Bool`/`Enum.index`.
    /// Arrays and strings yield `None`.
    pub fn as_f64(&self) -> Option<f64> {
        match self {
            Self::Int(v) => Some(*v as f64),
            Self::Float(v) => Some(*v),
            Self::Bool(v) => Some(if *v { 1.0 } else { 0.0 }),
            Self::Enum { index, .. } => Some(f64::from(*index)),
            _ => None,
        }
    }

    /// Integer view of a scalar value (truncating a float toward zero).
    pub fn as_i64(&self) -> Option<i64> {
        match self {
            Self::Int(v) => Some(*v),
            Self::Float(v) => Some(*v as i64),
            Self::Bool(v) => Some(i64::from(*v)),
            Self::Enum { index, .. } => Some(i64::from(*index)),
            _ => None,
        }
    }

    /// Whether this value is an array (waveform) rather than a scalar.
    pub fn is_array(&self) -> bool {
        matches!(
            self,
            Self::FloatArray(_) | Self::IntArray(_) | Self::StrArray(_) | Self::Bytes(_)
        )
    }

    /// Borrow a float waveform without copying. Returns `None` for non-float
    /// arrays and scalars.
    pub fn as_f64_slice(&self) -> Option<&[f64]> {
        match self {
            Self::FloatArray(a) => Some(a),
            _ => None,
        }
    }

    /// Element count for arrays; `1` for scalars.
    pub fn len(&self) -> usize {
        match self {
            Self::FloatArray(a) => a.len(),
            Self::IntArray(a) => a.len(),
            Self::StrArray(a) => a.len(),
            Self::Bytes(a) => a.len(),
            _ => 1,
        }
    }

    /// Whether an array value is empty. Scalars are never empty.
    pub fn is_empty(&self) -> bool {
        self.len() == 0
    }
}

/// The full snapshot of a channel, read by widgets each frame.
///
/// One field per PyDM channel slot. The tokio side replaces this behind a lock
/// and bumps [`ChannelState::stamp`] on every change so consumers can detect
/// updates (e.g. to skip a GPU re-upload) without comparing the whole struct.
#[derive(Clone, Debug, Default)]
pub struct ChannelState {
    /// Whether the underlying connection is established.
    pub connected: bool,
    /// Whether writes are permitted (CA access rights / pvAccess put).
    pub write_access: bool,
    /// Latest value, if one has arrived.
    pub value: Option<PvValue>,
    /// Wire alarm severity (see [`ChannelState::effective_severity`] for the
    /// value to use when styling).
    pub severity: AlarmSeverity,
    /// Enumeration strings (CA `DBR_GR/CTRL_ENUM`, pvAccess enum), if any.
    pub enum_strings: Option<Arc<[String]>>,
    /// Engineering units (`EGU`).
    pub units: Option<Arc<str>>,
    /// Display precision (`PREC`).
    pub precision: Option<i32>,
    /// Display low/high limits.
    pub display_limits: Option<(f64, f64)>,
    /// Control low/high limits (`DRVL`/`DRVH`).
    pub ctrl_limits: Option<(f64, f64)>,
    /// Warning low/high limits (`LOW`/`HIGH`).
    pub warn_limits: Option<(f64, f64)>,
    /// Alarm low/high limits (`LOLO`/`HIHI`).
    pub alarm_limits: Option<(f64, f64)>,
    /// Timestamp of the latest value.
    pub timestamp: Option<SystemTime>,
    /// Monotonic update counter, bumped on every change.
    pub stamp: u64,
}

impl ChannelState {
    /// Severity to use for styling: [`AlarmSeverity::Disconnected`] overrides
    /// the PV severity whenever the channel is not connected (PyDM
    /// `base.py` `connection_changed`).
    pub fn effective_severity(&self) -> AlarmSeverity {
        if self.connected {
            self.severity
        } else {
            AlarmSeverity::Disconnected
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn epics_severity_maps_and_clamps() {
        assert_eq!(AlarmSeverity::from_epics(0), AlarmSeverity::NoAlarm);
        assert_eq!(AlarmSeverity::from_epics(1), AlarmSeverity::Minor);
        assert_eq!(AlarmSeverity::from_epics(2), AlarmSeverity::Major);
        assert_eq!(AlarmSeverity::from_epics(3), AlarmSeverity::Invalid);
        // Out-of-range wire severities clamp to INVALID, never Disconnected.
        assert_eq!(AlarmSeverity::from_epics(99), AlarmSeverity::Invalid);
    }

    #[test]
    fn severity_codes_match_pydm() {
        assert_eq!(AlarmSeverity::NoAlarm.as_code(), 0);
        assert_eq!(AlarmSeverity::Minor.as_code(), 1);
        assert_eq!(AlarmSeverity::Major.as_code(), 2);
        assert_eq!(AlarmSeverity::Invalid.as_code(), 3);
        assert_eq!(AlarmSeverity::Disconnected.as_code(), 4);
    }

    #[test]
    fn scalar_numeric_views() {
        assert_eq!(PvValue::Int(7).as_f64(), Some(7.0));
        assert_eq!(PvValue::Float(1.5).as_i64(), Some(1));
        assert_eq!(PvValue::Bool(true).as_f64(), Some(1.0));
        assert_eq!(
            PvValue::Enum {
                index: 2,
                label: None
            }
            .as_f64(),
            Some(2.0)
        );
        assert_eq!(PvValue::Str(Arc::from("x")).as_f64(), None);
    }

    #[test]
    fn array_views() {
        let a = PvValue::FloatArray(Arc::from([1.0, 2.0, 3.0].as_slice()));
        assert!(a.is_array());
        assert_eq!(a.len(), 3);
        assert!(!a.is_empty());
        assert_eq!(a.as_f64_slice(), Some([1.0, 2.0, 3.0].as_slice()));
        assert_eq!(a.as_f64(), None);

        let scalar = PvValue::Int(1);
        assert!(!scalar.is_array());
        assert_eq!(scalar.len(), 1);
        assert_eq!(scalar.as_f64_slice(), None);

        let empty = PvValue::IntArray(Arc::from([].as_slice()));
        assert!(empty.is_empty());
    }

    #[test]
    fn effective_severity_is_disconnected_when_not_connected() {
        let mut s = ChannelState {
            connected: false,
            severity: AlarmSeverity::Minor,
            ..Default::default()
        };
        assert_eq!(s.effective_severity(), AlarmSeverity::Disconnected);
        s.connected = true;
        assert_eq!(s.effective_severity(), AlarmSeverity::Minor);
    }

    #[test]
    fn default_state_is_disconnected_no_alarm() {
        let s = ChannelState::default();
        assert!(!s.connected);
        assert_eq!(s.severity, AlarmSeverity::NoAlarm);
        assert_eq!(s.effective_severity(), AlarmSeverity::Disconnected);
        assert_eq!(s.stamp, 0);
    }
}
