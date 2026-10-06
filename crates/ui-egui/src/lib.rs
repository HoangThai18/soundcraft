//! SoundCraft's egui user interface.
//!
//! The UI is a thin shell over [`soundcraft_engine::Engine`]: it draws engine state and acts by
//! executing commands. It can be replaced without touching anything below it.
#![deny(clippy::unwrap_used, clippy::expect_used, clippy::panic, clippy::unimplemented, clippy::todo, clippy::unreachable)]

pub mod control;
pub mod dialogs;
pub mod edit_window;
pub mod fonts;
pub mod icons;
pub mod menus;
pub mod midi_editor;
pub mod mix_window;
pub mod panels;
pub mod shortcuts;
pub mod theme;
pub mod toolbar;
pub mod widgets;
pub mod windows;

use serde_json::{Value, json};
use soundcraft_engine::{Engine, TransportRequest};
use soundcraft_model::TrackId;
use soundcraft_playback::Player;
use soundcraft_time::{Range, Samples};
use std::collections::HashMap;
use std::sync::mpsc::Receiver;

pub use control::ControlRequest;

/// Which main window is in front (Window › Mix / Edit).
#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub enum MainWindow {
    Edit,
    Mix,
}

/// UI state (serde, so agents can read and set it).
#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
#[serde(default)]
pub struct UiState {
    pub window: MainWindow,
    pub show_tracks_list: bool,
    pub show_clip_list: bool,
    pub show_transport: bool,
    pub show_memory_locations: bool,
    pub show_big_counter: bool,
    pub show_undo_history: bool,
    pub show_session_info: bool,
    pub show_about: bool,
    pub show_midi_editor: bool,
    pub show_universe: bool,
    pub narrow_mix: bool,
    /// Mix window sections: inserts_ae, inserts_fj, sends_ae, sends_fj, io, comments, eq_curve, color.
    pub mix_views: Vec<String>,
    /// Edit window track-header columns: io, inserts_ae, sends_ae, comments, color.
    pub edit_views: Vec<String>,
    /// Open plugin windows (track, slot).
    pub plugin_windows: Vec<(TrackId, usize)>,
    pub audiosuite: Option<String>,
    pub status: String,
    pub show_automation: bool,
    pub show_color_palette: bool,
    pub show_disk_usage: bool,
    pub show_system_usage: bool,
    pub show_task_manager: bool,
    pub show_metadata: bool,
    pub show_event_list: bool,
    pub show_midi_keyboard: bool,
    pub show_workspace: bool,
    pub show_configurations: bool,
    pub show_playback_engine: bool,
    pub show_io_setup: bool,
    pub show_shortcuts: bool,
    pub workspace_dir: String,
    pub configurations: Vec<(String, Value)>,
}

impl Default for UiState {
    fn default() -> Self {
        UiState {
            window: MainWindow::Edit,
            show_tracks_list: true,
            show_clip_list: true,
            show_transport: false,
            show_memory_locations: false,
            show_big_counter: false,
            show_undo_history: false,
            show_session_info: false,
            show_about: false,
            show_midi_editor: false,
            show_universe: false,
            narrow_mix: false,
            mix_views: vec!["inserts_ae".into(), "sends_ae".into(), "io".into(), "color".into()],
            edit_views: vec!["color".into()],
            plugin_windows: Vec::new(),
            audiosuite: None,
            status: String::new(),
            show_automation: false,
            show_color_palette: false,
            show_disk_usage: false,
            show_system_usage: false,
            show_task_manager: false,
            show_metadata: false,
            show_event_list: false,
            show_midi_keyboard: false,
            show_workspace: false,
            show_configurations: false,
            show_playback_engine: false,
            show_io_setup: false,
            show_shortcuts: false,
            workspace_dir: String::new(),
            configurations: Vec::new(),
        }
    }
}

/// Host services (file dialogs) injected by the app; absent in tests and on the web.
#[derive(Default)]
pub struct Services {
    pub pick_open: Option<Box<dyn Fn(&str, &[&str]) -> Option<String>>>,
    pub pick_save: Option<Box<dyn Fn(&str, &str) -> Option<String>>>,
}

/// Ballistic meter display state per strip.
#[derive(Debug, Clone, Copy, Default)]
pub struct MeterDisplay {
    pub level: [f32; 2],
    pub hold: [f32; 2],
    pub hold_age: f32,
    pub clip: bool,
    pub gr: f32,
}

/// An in-progress mouse gesture in the Edit window.
#[derive(Debug, Clone)]
pub enum Gesture {
    Select { track: TrackId, anchor: Samples, tracks: Vec<TrackId> },
    MoveClips { clips: Vec<soundcraft_model::ClipId>, grab_at: Samples, delta: Samples, from_track: TrackId, to_track: TrackId },
    TrimStart { clip: soundcraft_model::ClipId, track: TrackId, to: Samples },
    TrimEnd { clip: soundcraft_model::ClipId, track: TrackId, to: Samples },
    Fade { clip: soundcraft_model::ClipId, track: TrackId, fade_in: bool, to: Samples },
    Scrub { last: Samples },
    Fader { track: TrackId, start_db: f32 },
    Pencil { track: TrackId, points: Vec<(Samples, f32)> },
    ZoomBox { start: f32 },
}

pub struct SoundApp {
    pub engine: Engine,
    pub player: Option<Player>,
    /// Input capture, opened on first record.
    pub recorder: Option<soundcraft_playback::record::Recorder>,
    record_start: Samples,
    pub ui: UiState,
    pub services: Services,
    pub dialogs: dialogs::Dialogs,
    pub meters: HashMap<TrackId, MeterDisplay>,
    pub main_meter: MeterDisplay,
    pub gesture: Option<Gesture>,
    pub edit_layout: edit_window::EditLayout,
    pub midi: midi_editor::MidiEditorState,
    pub synthetic: Vec<egui::Event>,
    control_rx: Option<Receiver<ControlRequest>>,
    pending_shots: Vec<control::PendingShot>,
    last_rev: u64,
    fonts_ready: bool,
    fonts_installed: bool,
    /// Transport simulation when there is no player (tests, offscreen renders).
    sim: Option<(Samples, Option<Samples>, Option<Range>)>,
    pub quit_requested: bool,
    /// Reset floating-window positions next frame (Window › Arrange).
    pub arrange_request: bool,
    /// Tracks whose automation was touched during this pass (Touch/Latch writing).
    touched: std::collections::HashSet<TrackId>,
    last_write_at: Samples,
    pub frame_ms: f32,
    last_frame: Option<f64>,
}

impl SoundApp {
    pub fn new(engine: Engine, player: Option<Player>, services: Services) -> Self {
        SoundApp {
            engine,
            player,
            recorder: None,
            record_start: 0,
            ui: UiState::default(),
            services,
            dialogs: dialogs::Dialogs::default(),
            meters: HashMap::new(),
            main_meter: MeterDisplay::default(),
            gesture: None,
            edit_layout: edit_window::EditLayout::default(),
            midi: midi_editor::MidiEditorState::default(),
            synthetic: Vec::new(),
            control_rx: None,
            pending_shots: Vec::new(),
            last_rev: 0,
            fonts_ready: false,
            fonts_installed: false,
            sim: None,
            quit_requested: false,
            arrange_request: false,
            touched: std::collections::HashSet::new(),
            last_write_at: i64::MIN,
            frame_ms: 0.0,
            last_frame: None,
        }
    }

    pub fn with_control(mut self, rx: Receiver<ControlRequest>) -> Self {
        self.control_rx = Some(rx);
        self
    }

    /// Run a command; errors go to the status line. Returns the result.
    pub fn run(&mut self, id: &str, params: Value) -> Result<Value, String> {
        if let Some(r) = menus::run_ui_command(self, id, &params) {
            return r;
        }
        match self.engine.execute(id, &params) {
            Ok(v) => {
                if id == "app.quit" {
                    self.quit_requested = true;
                }
                Ok(v)
            }
            Err(e) => {
                self.ui.status = e.to_string();
                log::warn!("{id}: {e}");
                Err(e.to_string())
            }
        }
    }

    pub fn is_playing(&self) -> bool {
        self.engine.transport.playing
    }

    /// Current transport position (playhead while playing, else the edit insertion).
    pub fn position(&self) -> Samples {
        if self.engine.transport.playing { self.engine.transport.position } else { self.engine.session().edit.selection.start }
    }

    fn start_play(&mut self, from: Samples, end: Option<Samples>, looped: Option<Range>) {
        if let Some(p) = &self.player {
            p.update_session(self.engine.session_arc());
            p.play(from, end, looped);
        } else {
            self.sim = Some((from, end, looped));
        }
        self.engine.transport.playing = true;
        self.engine.transport.position = from;
    }

    fn stop_play(&mut self) {
        if let Some(p) = &self.player {
            p.stop();
        }
        if self.engine.transport.recording {
            self.finish_recording();
        }
        self.sim = None;
        self.engine.transport.playing = false;
        self.engine.transport.recording = false;
        let s = self.engine.session();
        if s.edit.insertion_follows_playback {
            let pos = self.engine.transport.position;
            self.engine.session_mut().edit.selection = Range::point(pos);
        }
    }

    fn start_recording(&mut self) {
        let armed = self.engine.session().tracks.iter().any(|t| t.mixer.record_arm);
        if !armed {
            self.ui.status = "Record-enable a track first (the red button in its header).".into();
            return;
        }
        if self.recorder.is_none() && self.player.is_some() {
            match soundcraft_playback::record::Recorder::open() {
                Ok(r) => self.recorder = Some(r),
                Err(e) => {
                    self.ui.status = format!("Cannot record: {e}");
                    return;
                }
            }
        }
        self.record_start = self.engine.session().edit.selection.start;
        if let Some(r) = &self.recorder {
            r.arm();
        }
        self.engine.transport.recording = true;
        if !self.is_playing() {
            let sel = self.engine.session().edit.selection;
            if self.engine.session().edit.loop_record && !sel.is_empty() {
                self.start_play(sel.start, None, Some(sel));
            } else {
                // Record into the selection when there is one (punch), else open-ended.
                let end = (!sel.is_empty()).then_some(sel.end);
                self.start_play(sel.start, end, None);
            }
        }
    }

    fn finish_recording(&mut self) {
        self.engine.transport.recording = false;
        let Some(r) = &self.recorder else { return };
        let take = r.take();
        let rate = take.sample_rate;
        let sel = self.engine.session().edit.selection;
        if self.engine.session().edit.loop_record && !sel.is_empty() {
            // Split the capture into one take per loop pass; each pass gets its own playlist.
            let pass = usize::try_from(soundcraft_time::to_samples(sel.len() as f64 * f64::from(rate) / self.engine.session().sample_rate.as_f64()))
                .unwrap_or(0)
                .max(1);
            let total = take.channels.first().map_or(0, Vec::len);
            let passes = total.div_ceil(pass).max(1);
            let mut made = 0;
            for k in 0..passes {
                let chunk: Vec<Vec<f32>> =
                    take.channels.iter().map(|c| c.get(k * pass..((k + 1) * pass).min(c.len())).map(<[f32]>::to_vec).unwrap_or_default()).collect();
                if chunk.first().is_none_or(|c| c.len() < pass / 8) {
                    continue;
                }
                if made > 0 {
                    let armed: Vec<u64> = self.engine.session().tracks.iter().filter(|t| t.mixer.record_arm).map(|t| t.id.0).collect();
                    let _ = self.engine.execute("track.playlist_new", &serde_json::json!({"tracks": armed}));
                }
                if soundcraft_engine::io::add_recording(&mut self.engine, sel.start, chunk, rate).is_ok() {
                    made += 1;
                }
            }
            self.ui.status = format!("Loop-recorded {made} take(s)");
            return;
        }
        match soundcraft_engine::io::add_recording(&mut self.engine, self.record_start, take.channels, rate) {
            Ok(ids) => self.ui.status = format!("Recorded {} clip(s)", ids.len()),
            Err(e) => self.ui.status = e.to_string(),
        }
    }

    /// While playing, record volume automation from fader positions for tracks in Write mode, and
    /// for Touch/Latch tracks whose fader is (or, for Latch, was) being moved. One undo step per pass.
    fn write_automation(&mut self, ctx: &egui::Context) {
        use soundcraft_model::AutomationMode as M;
        if !self.is_playing() {
            if !self.touched.is_empty() {
                self.touched.clear();
                self.engine.end_merge();
            }
            self.last_write_at = i64::MIN;
            return;
        }
        let pos = self.position();
        let step = self.engine.session().sample_rate.samples(0.02);
        if (pos - self.last_write_at).abs() < step {
            return;
        }
        self.last_write_at = pos;
        let dragging = ctx.input(|i| i.pointer.any_down());
        let fader_track = match &self.gesture {
            Some(Gesture::Fader { track, .. }) => Some(*track),
            _ => None,
        };
        let writes: Vec<(TrackId, f32)> = self
            .engine
            .session()
            .tracks
            .iter()
            .filter_map(|t| {
                let m = t.mixer.automation_mode;
                let touched_now = dragging && fader_track == Some(t.id);
                let write = match m {
                    M::Write => true,
                    M::Touch => touched_now,
                    M::Latch | M::TouchLatch => touched_now || self.touched.contains(&t.id),
                    _ => false,
                };
                write.then_some((t.id, t.mixer.volume_db))
            })
            .collect();
        for (t, v) in writes {
            self.touched.insert(t);
            let _ = self.engine.execute_merged(
                "automation.set_point",
                &serde_json::json!({"track": t.0, "param": "volume", "at": pos, "value": v}),
                "automation_pass",
            );
        }
    }

    /// Play from the selection, honouring loop playback and selection end.
    pub fn play_from_selection(&mut self) {
        let s = self.engine.session();
        let sel = s.edit.selection;
        let preroll = if s.edit.pre_post_roll { s.edit.pre_roll } else { 0 };
        let postroll = if s.edit.pre_post_roll { s.edit.post_roll } else { 0 };
        if s.edit.loop_playback && !sel.is_empty() {
            self.start_play(sel.start, None, Some(sel));
        } else if !sel.is_empty() {
            self.start_play((sel.start - preroll).max(0), Some(sel.end + postroll), None);
        } else {
            self.start_play((sel.start - preroll).max(0), None, None);
        }
    }

    fn handle_transport(&mut self, dt: f32) {
        let reqs: Vec<TransportRequest> = std::mem::take(&mut self.engine.transport_requests);
        for r in reqs {
            match r {
                TransportRequest::Play => {
                    if !self.is_playing() {
                        self.play_from_selection();
                    }
                }
                TransportRequest::Stop | TransportRequest::Pause => self.stop_play(),
                TransportRequest::TogglePlay => {
                    if self.is_playing() {
                        self.stop_play();
                    } else {
                        self.play_from_selection();
                    }
                }
                TransportRequest::Record => {
                    if self.engine.transport.recording {
                        self.stop_play();
                    } else {
                        self.start_recording();
                    }
                }
                TransportRequest::Locate(at) => {
                    if self.is_playing() {
                        if let Some(p) = &self.player {
                            p.locate(at);
                        }
                        if let Some(sim) = &mut self.sim {
                            sim.0 = at;
                        }
                    }
                    self.engine.transport.position = at;
                }
                TransportRequest::PlaySelection => {
                    let sel = self.engine.session().edit.selection;
                    self.start_play(sel.start, Some(sel.end.max(sel.start + 1)), None);
                }
                TransportRequest::HalfSpeed => {
                    if let Some(p) = &self.player {
                        p.set_speed(0.5);
                    }
                    self.play_from_selection();
                }
                TransportRequest::Scrub(at) => self.engine.transport.position = at,
                TransportRequest::AllNotesOff => {}
            }
        }
        // Read back the position.
        if let Some(p) = &self.player {
            let playing = p.is_playing();
            let ended = self.engine.transport.playing && !playing;
            if ended {
                p.set_speed(1.0);
            }
            if self.engine.transport.playing {
                self.engine.transport.position = p.position();
            }
            let snap = p.meters();
            for (id, m) in &snap.tracks {
                let d = self.meters.entry(*id).or_default();
                feed_meter(d, m.peak, m.gain_reduction_db, dt);
            }
            let main = snap.main.peak;
            feed_meter(&mut self.main_meter, main, 0.0, dt);
            if ended {
                self.stop_play();
            }
        } else if let Some((pos, end, looped)) = &mut self.sim {
            let sr = self.engine.session().sample_rate.as_f64();
            *pos += (f64::from(dt) * sr) as i64;
            if let Some(l) = looped
                && *pos >= l.end
            {
                *pos = l.start;
            }
            let stop = end.is_some_and(|e| *pos >= e);
            self.engine.transport.position = *pos;
            if stop {
                self.stop_play();
            }
        }
        if !self.engine.transport.playing {
            for d in self.meters.values_mut() {
                feed_meter(d, [0.0, 0.0], 0.0, dt);
            }
            feed_meter(&mut self.main_meter, [0.0, 0.0], 0.0, dt);
        }
    }

    /// Per-frame logic: control channel, transport, document sync.
    pub fn logic(&mut self, ctx: &egui::Context) {
        // Fonts set now take effect next frame, so draw only from the frame after.
        if !self.fonts_ready {
            if self.fonts_installed {
                self.fonts_ready = true;
            } else {
                fonts::install(ctx);
                theme::apply(ctx);
                self.fonts_installed = true;
                ctx.request_repaint();
            }
        }
        let now = ctx.input(|i| i.time);
        let dt = self.last_frame.map_or(1.0 / 60.0, |t| (now - t) as f32).clamp(0.0, 0.25);
        self.last_frame = Some(now);
        self.frame_ms = self.frame_ms * 0.9 + dt * 1000.0 * 0.1;
        control::drain(self, ctx);
        if !ctx.input(|i| i.pointer.any_down()) {
            if matches!(self.gesture, Some(Gesture::Fader { .. })) {
                self.gesture = None;
            }
            // Keep a running automation pass merged until playback stops.
            if !ctx.egui_wants_keyboard_input() && self.touched.is_empty() {
                self.engine.end_merge();
            }
        }
        if self.engine.revision != self.last_rev {
            self.last_rev = self.engine.revision;
            if let Some(p) = &self.player {
                p.update_session(self.engine.session_arc());
            }
        }
        self.handle_transport(dt);
        self.write_automation(ctx);
        if self.is_playing() || self.meters.values().any(|m| m.level[0] > 0.0001) || !self.pending_shots.is_empty() {
            ctx.request_repaint();
        }
        control::collect_screenshots(self, ctx);
    }

    /// Feed queued synthetic input (from the control channel) into egui.
    pub fn raw_input_hook(&mut self, raw: &mut egui::RawInput) {
        if self.synthetic.is_empty() {
            return;
        }
        let mut n = 0;
        for e in &self.synthetic {
            n += 1;
            if matches!(e, egui::Event::PointerButton { pressed: false, .. } | egui::Event::Key { pressed: false, .. }) {
                break;
            }
            if matches!(e, egui::Event::PointerButton { pressed: true, .. }) {
                break;
            }
        }
        raw.events.extend(self.synthetic.drain(..n.min(self.synthetic.len())));
    }

    /// Lay out the whole window.
    pub fn ui(&mut self, ui: &mut egui::Ui) {
        let ctx = ui.ctx().clone();
        if !self.fonts_ready {
            ctx.request_repaint();
            return;
        }
        if self.arrange_request {
            self.arrange_request = false;
            ctx.memory_mut(|m| m.reset_areas());
        }
        shortcuts::handle(self, &ctx);
        menus::menu_bar(self, ui);
        match self.ui.window {
            MainWindow::Edit => edit_window::show(self, ui),
            MainWindow::Mix => mix_window::show(self, ui),
        }
        panels::floating(self, &ctx);
        windows::show(self, &ctx);
        dialogs::show(self, &ctx);
    }

    /// UI state as JSON (control channel `ui.inspect`).
    pub fn inspect(&self, ctx: &egui::Context) -> Value {
        let size = ctx.content_rect().size();
        json!({
            "ui": self.ui,
            "window_size": [size.x, size.y],
            "playing": self.is_playing(),
            "position": self.position(),
            "dialog": self.dialogs.open_name(),
            "frame_ms": self.frame_ms,
            "audio_device": self.player.as_ref().map(|p| p.device_name.clone()),
            "status": self.ui.status,
        })
    }
}

fn feed_meter(d: &mut MeterDisplay, peak: [f32; 2], gr: f32, dt: f32) {
    // Instant attack, ~26 dB/s release, 2 s peak hold.
    let fall = 10f32.powf(-26.0 * dt / 20.0);
    for c in 0..2 {
        let v = peak.get(c).copied().unwrap_or(0.0);
        let lvl = d.level.get(c).copied().unwrap_or(0.0);
        let nv = if v >= lvl { v } else { (lvl * fall).max(v) };
        if let Some(x) = d.level.get_mut(c) {
            *x = if nv < 1e-5 { 0.0 } else { nv };
        }
        if v >= d.hold.get(c).copied().unwrap_or(0.0) {
            if let Some(h) = d.hold.get_mut(c) {
                *h = v;
            }
            d.hold_age = 0.0;
        }
        if v >= 1.0 {
            d.clip = true;
        }
    }
    d.hold_age += dt;
    if d.hold_age > 2.0 {
        d.hold = d.level;
    }
    d.gr = if gr > d.gr { gr } else { d.gr * fall };
}
