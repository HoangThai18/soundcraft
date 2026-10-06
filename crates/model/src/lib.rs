//! The SoundCraft session document.
//!
//! A [`Session`] is pure data: tracks with playlists of clips, the mixer (inserts, sends, routing,
//! automation), tempo/meter, memory locations and groups. Audio sample data lives in
//! [`SourcePool`] behind `Arc`s, so cloning a session for undo is cheap.
#![deny(clippy::unwrap_used, clippy::expect_used, clippy::panic, clippy::unimplemented, clippy::todo, clippy::unreachable)]

mod automation;
mod clip;
mod ids;
mod markers;
mod mixer;
mod session;
mod source;
mod track;

pub use automation::{AutoParam, AutomationLane, AutomationMode, AutomationPoint};
pub use clip::{Clip, ClipContent, Fade, FadeShape};
pub use ids::{BusId, ClipId, GroupId, MarkerId, SourceId, TrackId};
pub use markers::{Group, MarkerKind, MemoryLocation};
pub use mixer::{Bus, ChannelFormat, INSERT_SLOTS, Insert, Mixer, OutputPath, Route, SEND_SLOTS, SendSlot, fader_db_to_pos, fader_pos_to_db};
pub use session::{BitDepthSetting, EditMode, EditState, SESSION_EXTENSION, Session, SessionError, Tool, TrackView, ZoomState};
pub use source::{Source, SourceAudio, SourcePool};
pub use track::{Playlist, TRACK_COLORS, Track, TrackHeight, TrackKind};

pub use soundcraft_time as time;
