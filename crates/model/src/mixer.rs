//! Per-track mixer state: inserts, sends, routing, fader, pan, mute/solo.

use crate::{AutomationMode, BusId};
use std::collections::BTreeMap;

pub const INSERT_SLOTS: usize = 10;
pub const SEND_SLOTS: usize = 10;

/// Channel formats (track widths).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Default, serde::Serialize, serde::Deserialize)]
pub enum ChannelFormat {
    #[default]
    Mono,
    Stereo,
    Lcr,
    Quad,
    Lcrs,
    Surround50,
    Surround51,
    Surround70,
    Surround71,
    Atmos712,
    Atmos714,
}

impl ChannelFormat {
    pub const ALL: [ChannelFormat; 11] = [
        ChannelFormat::Mono,
        ChannelFormat::Stereo,
        ChannelFormat::Lcr,
        ChannelFormat::Quad,
        ChannelFormat::Lcrs,
        ChannelFormat::Surround50,
        ChannelFormat::Surround51,
        ChannelFormat::Surround70,
        ChannelFormat::Surround71,
        ChannelFormat::Atmos712,
        ChannelFormat::Atmos714,
    ];
    pub fn channels(self) -> usize {
        match self {
            ChannelFormat::Mono => 1,
            ChannelFormat::Stereo => 2,
            ChannelFormat::Lcr => 3,
            ChannelFormat::Quad | ChannelFormat::Lcrs => 4,
            ChannelFormat::Surround50 => 5,
            ChannelFormat::Surround51 => 6,
            ChannelFormat::Surround70 => 7,
            ChannelFormat::Surround71 => 8,
            ChannelFormat::Atmos712 => 10,
            ChannelFormat::Atmos714 => 12,
        }
    }
    pub fn label(self) -> &'static str {
        match self {
            ChannelFormat::Mono => "Mono",
            ChannelFormat::Stereo => "Stereo",
            ChannelFormat::Lcr => "LCR",
            ChannelFormat::Quad => "Quad",
            ChannelFormat::Lcrs => "LCRS",
            ChannelFormat::Surround50 => "5.0",
            ChannelFormat::Surround51 => "5.1",
            ChannelFormat::Surround70 => "7.0",
            ChannelFormat::Surround71 => "7.1",
            ChannelFormat::Atmos712 => "7.1.2",
            ChannelFormat::Atmos714 => "7.1.4",
        }
    }
    pub fn from_id(s: &str) -> Option<ChannelFormat> {
        ChannelFormat::ALL.into_iter().find(|f| f.label().eq_ignore_ascii_case(s))
    }
    /// Best format for a channel count.
    pub fn for_channels(n: usize) -> ChannelFormat {
        ChannelFormat::ALL.into_iter().find(|f| f.channels() == n).unwrap_or(if n <= 1 { ChannelFormat::Mono } else { ChannelFormat::Stereo })
    }
}

/// An internal mix bus (Pro Tools "bus" paths).
#[derive(Debug, Clone, PartialEq, serde::Serialize, serde::Deserialize)]
pub struct Bus {
    pub id: BusId,
    pub name: String,
    pub format: ChannelFormat,
}

/// Physical output paths (from the I/O setup).
#[derive(Debug, Clone, PartialEq, serde::Serialize, serde::Deserialize)]
pub struct OutputPath {
    pub name: String,
    /// First hardware channel (0-based).
    pub first_channel: u16,
    pub format: ChannelFormat,
}

/// Where a signal goes or comes from.
#[derive(Debug, Clone, PartialEq, Eq, Hash, Default, serde::Serialize, serde::Deserialize)]
pub enum Route {
    #[default]
    None,
    /// The main output (hardware outputs 1-2, or the bounce source).
    Main,
    Bus(BusId),
    /// Hardware input/output by path name.
    Hardware(String),
}

/// A plugin instance on an insert slot.
#[derive(Debug, Clone, PartialEq, serde::Serialize, serde::Deserialize)]
pub struct Insert {
    /// Registry id from `soundcraft-dsp` (e.g. `eq7`, `comp`).
    pub plugin: String,
    #[serde(default)]
    pub params: BTreeMap<String, f32>,
    #[serde(default)]
    pub bypass: bool,
    #[serde(default = "yes")]
    pub active: bool,
    /// Preset name shown in the plugin window header.
    #[serde(default)]
    pub preset: String,
}

fn yes() -> bool {
    true
}

impl Insert {
    pub fn new(plugin: impl Into<String>) -> Self {
        Insert { plugin: plugin.into(), params: BTreeMap::new(), bypass: false, active: true, preset: String::from("<factory default>") }
    }
}

/// A send to a bus.
#[derive(Debug, Clone, PartialEq, serde::Serialize, serde::Deserialize)]
pub struct SendSlot {
    pub target: Route,
    pub level_db: f32,
    pub pan: f32,
    pub mute: bool,
    pub pre_fader: bool,
    #[serde(default = "yes")]
    pub follow_main_pan: bool,
}

impl SendSlot {
    pub fn new(target: Route) -> Self {
        SendSlot { target, level_db: -144.0, pan: 0.0, mute: false, pre_fader: false, follow_main_pan: true }
    }
}

/// The channel strip.
#[derive(Debug, Clone, PartialEq, serde::Serialize, serde::Deserialize)]
pub struct Mixer {
    pub volume_db: f32,
    /// One panner per source channel for mono/stereo (stereo tracks: two panners, default hard L/R).
    pub pan: Vec<f32>,
    pub mute: bool,
    pub solo: bool,
    #[serde(default)]
    pub solo_safe: bool,
    #[serde(default)]
    pub record_arm: bool,
    #[serde(default)]
    pub input_monitor: bool,
    #[serde(default)]
    pub phase_invert: bool,
    pub input: Route,
    pub output: Route,
    pub inserts: Vec<Option<Insert>>,
    pub sends: Vec<Option<SendSlot>>,
    pub automation_mode: AutomationMode,
    /// Input gain / trim, dB.
    #[serde(default)]
    pub trim_db: f32,
    /// VCA master that controls this track, if any (track id of the VCA).
    #[serde(default)]
    pub vca: Option<u64>,
}

impl Mixer {
    pub fn new(format: ChannelFormat) -> Self {
        let pan = match format {
            ChannelFormat::Mono => vec![0.0],
            ChannelFormat::Stereo => vec![-1.0, 1.0],
            _ => Vec::new(),
        };
        Mixer {
            volume_db: 0.0,
            pan,
            mute: false,
            solo: false,
            solo_safe: false,
            record_arm: false,
            input_monitor: false,
            phase_invert: false,
            input: Route::None,
            output: Route::Main,
            inserts: vec![None; INSERT_SLOTS],
            sends: vec![None; SEND_SLOTS],
            automation_mode: AutomationMode::Read,
            trim_db: 0.0,
            vca: None,
        }
    }

    /// Clamp everything into range (after deserialising untrusted files).
    pub fn sanitize(&mut self) {
        self.volume_db = finite_or(self.volume_db, 0.0).clamp(-144.0, 12.0);
        for p in &mut self.pan {
            *p = finite_or(*p, 0.0).clamp(-1.0, 1.0);
        }
        self.inserts.resize(INSERT_SLOTS, None);
        self.sends.resize(SEND_SLOTS, None);
        for s in self.sends.iter_mut().flatten() {
            s.level_db = finite_or(s.level_db, -144.0).clamp(-144.0, 12.0);
            s.pan = finite_or(s.pan, 0.0).clamp(-1.0, 1.0);
        }
        self.trim_db = finite_or(self.trim_db, 0.0).clamp(-144.0, 24.0);
    }
}

pub(crate) fn finite_or(v: f32, d: f32) -> f32 {
    if v.is_finite() { v } else { d }
}

/// Fader taper used by Pro Tools-style faders: position 0..1 ↔ dB (-inf..+12).
pub fn fader_pos_to_db(pos: f32) -> f32 {
    let p = finite_or(pos, 0.0).clamp(0.0, 1.0);
    if p <= 0.0 {
        return -144.0;
    }
    // Piecewise: top 75 % covers -24..+12 linearly-ish, the bottom compresses to -inf.
    if p >= 0.25 { -24.0 + (p - 0.25) / 0.75 * 36.0 } else { -24.0 - (1.0 - p / 0.25).powf(1.5) * 120.0 }
}

pub fn fader_db_to_pos(db: f32) -> f32 {
    let db = finite_or(db, -144.0);
    if db <= -144.0 {
        return 0.0;
    }
    if db >= -24.0 {
        (0.25 + (db.min(12.0) + 24.0) / 36.0 * 0.75).min(1.0)
    } else {
        let x = ((-24.0 - db) / 120.0).clamp(0.0, 1.0).powf(1.0 / 1.5);
        ((1.0 - x) * 0.25).max(0.0)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn fader_taper_round_trips() {
        for db in [-100.0f32, -60.0, -24.0, -10.0, 0.0, 6.0, 12.0] {
            let p = fader_db_to_pos(db);
            assert!((fader_pos_to_db(p) - db).abs() < 0.05, "{db} → {p}");
        }
        assert_eq!(fader_pos_to_db(0.0), -144.0);
        assert_eq!(fader_db_to_pos(f32::NAN), 0.0);
    }

    #[test]
    fn mixer_defaults_and_sanitize() {
        let mut m = Mixer::new(ChannelFormat::Stereo);
        assert_eq!(m.pan, vec![-1.0, 1.0]);
        m.volume_db = f32::INFINITY;
        m.inserts.truncate(2);
        m.sanitize();
        assert_eq!(m.volume_db, 0.0);
        assert_eq!(m.inserts.len(), INSERT_SLOTS);
    }

    #[test]
    fn formats() {
        assert_eq!(ChannelFormat::for_channels(6), ChannelFormat::Surround51);
        assert_eq!(ChannelFormat::from_id("stereo"), Some(ChannelFormat::Stereo));
    }
}
