//! Realtime playback through the system audio device (cpal: CoreAudio, WASAPI, ALSA/Pulse/
//! PipeWire via ALSA, JACK, WebAudio).
//!
//! The UI thread owns a [`Player`]; the audio callback owns the [`MixEngine`] and a snapshot of
//! the session (`Arc<Session>`) that the UI swaps whenever the document changes. Commands go to
//! the callback through a channel; position and meters come back through atomics and a
//! `try_lock`ed snapshot, so the callback never blocks. When no device is available the player
//! falls back to a silent clock thread so the transport, meters and automation still run.
#![deny(clippy::unwrap_used, clippy::expect_used, clippy::panic, clippy::unimplemented, clippy::todo, clippy::unreachable)]

use soundcraft_mix::{MixEngine, StripMeter};
use soundcraft_model::{Session, TrackId};
use soundcraft_time::{Range, Samples};
use std::collections::HashMap;
use std::sync::atomic::{AtomicBool, AtomicI64, AtomicU32, Ordering};
use std::sync::mpsc::{Receiver, Sender, channel};
use std::sync::{Arc, Mutex, PoisonError};

const BLOCK: usize = 512;

/// Meter values for the UI.
#[derive(Debug, Clone, Default)]
pub struct MeterSnapshot {
    pub tracks: HashMap<TrackId, StripMeter>,
    pub main: StripMeter,
}

enum Cmd {
    Session(Arc<Session>),
    Play { from: Samples, end: Option<Samples>, looped: Option<Range> },
    Stop,
    Locate(Samples),
}

struct Shared {
    position: AtomicI64,
    playing: AtomicBool,
    /// Speed × 1000.
    speed: AtomicU32,
    meters: Mutex<MeterSnapshot>,
}

/// State owned by the audio callback.
struct AudioState {
    rx: Receiver<Cmd>,
    shared: Arc<Shared>,
    session: Arc<Session>,
    mix: MixEngine,
    pos: Samples,
    playing: bool,
    end: Option<Samples>,
    looped: Option<Range>,
    block: Vec<Vec<f32>>,
    /// Rendered frames waiting to be consumed (stereo planar) and the fractional read index.
    pending: Vec<Vec<f32>>,
    pending_len: usize,
    read: f64,
    device_rate: f64,
}

impl AudioState {
    fn new(rx: Receiver<Cmd>, shared: Arc<Shared>, session: Arc<Session>, device_rate: f64) -> Self {
        let sr = session.sample_rate.as_f64() as f32;
        AudioState {
            rx,
            shared,
            mix: MixEngine::new(sr, BLOCK),
            session,
            pos: 0,
            playing: false,
            end: None,
            looped: None,
            block: vec![vec![0.0; BLOCK]; 2],
            pending: vec![vec![0.0; BLOCK]; 2],
            pending_len: 0,
            read: 0.0,
            device_rate,
        }
    }

    fn drain(&mut self) {
        while let Ok(c) = self.rx.try_recv() {
            match c {
                Cmd::Session(s) => {
                    if (s.sample_rate.as_f64() - self.session.sample_rate.as_f64()).abs() > 0.5 {
                        self.mix = MixEngine::new(s.sample_rate.as_f64() as f32, BLOCK);
                    }
                    self.session = s;
                }
                Cmd::Play { from, end, looped } => {
                    self.pos = from;
                    self.end = end;
                    self.looped = looped.filter(|r| r.len() > 64);
                    self.playing = true;
                    self.pending_len = 0;
                    self.read = 0.0;
                    self.mix.reset();
                }
                Cmd::Stop => {
                    self.playing = false;
                    self.mix.reset();
                }
                Cmd::Locate(at) => {
                    self.pos = at;
                    self.pending_len = 0;
                    self.read = 0.0;
                }
            }
        }
        self.shared.playing.store(self.playing, Ordering::Relaxed);
    }

    /// Render the next block at the session rate into `pending`.
    fn render_next(&mut self) {
        let mut n = BLOCK;
        if let Some(l) = self.looped
            && self.pos >= l.end
        {
            self.pos = l.start;
        }
        if let Some(l) = self.looped {
            n = n.min(usize::try_from((l.end - self.pos).max(1)).unwrap_or(BLOCK));
        } else if let Some(e) = self.end {
            if self.pos >= e {
                self.playing = false;
                self.shared.playing.store(false, Ordering::Relaxed);
                n = 0;
            } else {
                n = n.min(usize::try_from(e - self.pos).unwrap_or(BLOCK));
            }
        }
        if n == 0 {
            for c in &mut self.pending {
                c.iter_mut().for_each(|x| *x = 0.0);
            }
            self.pending_len = BLOCK;
            return;
        }
        self.mix.render(&self.session, self.pos, n, &mut self.block);
        for (d, s) in self.pending.iter_mut().zip(self.block.iter()) {
            if let (Some(dd), Some(ss)) = (d.get_mut(..n), s.get(..n)) {
                dd.copy_from_slice(ss);
            }
        }
        self.pending_len = n;
        self.pos += n as i64;
        self.shared.position.store(self.pos, Ordering::Relaxed);
        if let Ok(mut m) = self.shared.meters.try_lock() {
            m.main = self.mix.main_meter;
            for (k, v) in &self.mix.meters {
                m.tracks.insert(*k, *v);
            }
        }
    }

    /// Fill an interleaved device buffer.
    fn fill(&mut self, out: &mut [f32], channels: usize) {
        self.drain();
        let channels = channels.max(1);
        if !self.playing {
            out.iter_mut().for_each(|x| *x = 0.0);
            if let Ok(mut m) = self.shared.meters.try_lock() {
                m.main = StripMeter::default();
                m.tracks.clear();
            }
            return;
        }
        let speed = f64::from(self.shared.speed.load(Ordering::Relaxed)) / 1000.0;
        let step = self.session.sample_rate.as_f64() / self.device_rate.max(1.0) * speed.clamp(0.1, 4.0);
        for frame in out.chunks_mut(channels) {
            while self.read >= self.pending_len as f64 {
                self.read -= self.pending_len as f64;
                if !self.playing {
                    break;
                }
                self.render_next();
                if self.pending_len == 0 {
                    break;
                }
            }
            let i0 = self.read.floor() as usize;
            let fr = (self.read - i0 as f64) as f32;
            for (c, o) in frame.iter_mut().enumerate() {
                let ch = self.pending.get(c.min(1));
                let a = ch.and_then(|v| v.get(i0)).copied().unwrap_or(0.0);
                let b = ch.and_then(|v| v.get(i0 + 1)).copied().unwrap_or(a);
                *o = if c < 2 && self.playing { (a + (b - a) * fr).clamp(-1.0, 1.0) } else { 0.0 };
            }
            self.read += step;
        }
    }
}

/// The transport and audio device.
pub struct Player {
    shared: Arc<Shared>,
    tx: Sender<Cmd>,
    #[cfg(not(target_arch = "wasm32"))]
    _stream: Option<cpal::Stream>,
    #[cfg(target_arch = "wasm32")]
    _stream: Option<cpal::Stream>,
    pub device_name: String,
    pub device_rate: u32,
    /// True when no audio device could be opened (silent clock).
    pub silent: bool,
}

impl Player {
    /// Open the default output device. Never fails: without a device it runs a silent clock.
    pub fn new(session: Arc<Session>) -> Player {
        let shared = Arc::new(Shared {
            position: AtomicI64::new(0),
            playing: AtomicBool::new(false),
            speed: AtomicU32::new(1000),
            meters: Mutex::new(MeterSnapshot::default()),
        });
        let (tx, rx) = channel();
        let want = session.sample_rate.hz();
        match open_device(want) {
            Ok((device, config, name)) => {
                let rate = config.sample_rate.0;
                let channels = usize::from(config.channels);
                let mut state = AudioState::new(rx, Arc::clone(&shared), session, f64::from(rate));
                let err_fn = |e: cpal::StreamError| log::warn!("audio stream error: {e}");
                use cpal::traits::{DeviceTrait, StreamTrait};
                match device.build_output_stream(
                    &config,
                    move |data: &mut [f32], _: &cpal::OutputCallbackInfo| state.fill(data, channels),
                    err_fn,
                    None,
                ) {
                    Ok(stream) => {
                        if let Err(e) = stream.play() {
                            log::warn!("audio stream failed to start: {e}");
                        }
                        log::info!("audio device: {name} @ {rate} Hz, {channels} ch");
                        Player { shared, tx, _stream: Some(stream), device_name: name, device_rate: rate, silent: false }
                    }
                    Err(e) => {
                        log::warn!("cannot open audio stream: {e}; using a silent clock");
                        let (tx2, rx2) = channel();
                        // The state (and its receiver) moved into the failed closure; rebuild.
                        Player::silent_clock(shared, tx2, rx2, want)
                    }
                }
            }
            Err(e) => {
                log::warn!("no audio output: {e}; using a silent clock");
                let s2 = Arc::clone(&shared);
                Player::silent_clock_with(s2, tx, rx, session)
            }
        }
    }

    fn silent_clock(shared: Arc<Shared>, tx: Sender<Cmd>, rx: Receiver<Cmd>, rate: u32) -> Player {
        let s = Arc::new(Session::new("Untitled", soundcraft_time::SampleRate::new(rate).unwrap_or_default()));
        Player::silent_clock_with(shared, tx, rx, s)
    }

    #[cfg(not(target_arch = "wasm32"))]
    fn silent_clock_with(shared: Arc<Shared>, tx: Sender<Cmd>, rx: Receiver<Cmd>, session: Arc<Session>) -> Player {
        let rate = session.sample_rate.hz();
        let mut state = AudioState::new(rx, Arc::clone(&shared), session, f64::from(rate));
        let spawn = std::thread::Builder::new().name("soundcraft-clock".into()).spawn(move || {
            let mut buf = vec![0.0f32; 4096];
            let mut last = std::time::Instant::now();
            loop {
                std::thread::sleep(std::time::Duration::from_millis(10));
                let now = std::time::Instant::now();
                let frames = ((now - last).as_secs_f64() * state.device_rate) as usize;
                last = now;
                let n = (frames * 2).min(buf.len());
                if let Some(b) = buf.get_mut(..n) {
                    state.fill(b, 2);
                }
                if Arc::strong_count(&state.shared) <= 1 {
                    break;
                }
            }
        });
        if let Err(e) = spawn {
            log::warn!("clock thread failed: {e}");
        }
        Player { shared, tx, _stream: None, device_name: "No audio device (silent)".into(), device_rate: rate, silent: true }
    }

    #[cfg(target_arch = "wasm32")]
    fn silent_clock_with(shared: Arc<Shared>, tx: Sender<Cmd>, _rx: Receiver<Cmd>, session: Arc<Session>) -> Player {
        Player { shared, tx, _stream: None, device_name: "No audio device".into(), device_rate: session.sample_rate.hz(), silent: true }
    }

    pub fn update_session(&self, s: Arc<Session>) {
        let _ = self.tx.send(Cmd::Session(s));
    }

    /// Start playback at `from`, stopping at `end`, or looping `looped`.
    pub fn play(&self, from: Samples, end: Option<Samples>, looped: Option<Range>) {
        self.shared.position.store(from, Ordering::Relaxed);
        self.shared.playing.store(true, Ordering::Relaxed);
        let _ = self.tx.send(Cmd::Play { from, end, looped });
    }

    pub fn stop(&self) {
        self.shared.playing.store(false, Ordering::Relaxed);
        let _ = self.tx.send(Cmd::Stop);
    }

    pub fn locate(&self, at: Samples) {
        self.shared.position.store(at, Ordering::Relaxed);
        let _ = self.tx.send(Cmd::Locate(at));
    }

    pub fn set_speed(&self, speed: f32) {
        let v = if speed.is_finite() { (speed.clamp(0.1, 4.0) * 1000.0) as u32 } else { 1000 };
        self.shared.speed.store(v, Ordering::Relaxed);
    }

    pub fn position(&self) -> Samples {
        self.shared.position.load(Ordering::Relaxed)
    }

    pub fn is_playing(&self) -> bool {
        self.shared.playing.load(Ordering::Relaxed)
    }

    pub fn meters(&self) -> MeterSnapshot {
        self.shared.meters.lock().unwrap_or_else(PoisonError::into_inner).clone()
    }
}

fn open_device(want_rate: u32) -> Result<(cpal::Device, cpal::StreamConfig, String), String> {
    use cpal::traits::{DeviceTrait, HostTrait};
    let host = cpal::default_host();
    let device = host.default_output_device().ok_or_else(|| "no default output device".to_string())?;
    #[allow(deprecated)]
    let name = device.name().unwrap_or_else(|_| "Audio device".into());
    // Prefer an f32 config at the session rate; else the device default (we resample).
    let mut chosen: Option<cpal::StreamConfig> = None;
    if let Ok(configs) = device.supported_output_configs() {
        for c in configs {
            if c.sample_format() == cpal::SampleFormat::F32
                && c.min_sample_rate().0 <= want_rate
                && c.max_sample_rate().0 >= want_rate
                && c.channels() >= 2
            {
                chosen = Some(c.with_sample_rate(cpal::SampleRate(want_rate)).config());
                break;
            }
        }
    }
    let config = match chosen {
        Some(c) => c,
        None => {
            let d = device.default_output_config().map_err(|e| e.to_string())?;
            if d.sample_format() != cpal::SampleFormat::F32 {
                return Err(format!("unsupported device sample format {:?}", d.sample_format()));
            }
            d.config()
        }
    };
    Ok((device, config, name))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn audio_state_plays_and_stops_at_end() {
        let s = Arc::new(soundcraft_model::Session::default());
        let shared = Arc::new(Shared {
            position: AtomicI64::new(0),
            playing: AtomicBool::new(false),
            speed: AtomicU32::new(1000),
            meters: Mutex::new(MeterSnapshot::default()),
        });
        let (tx, rx) = channel();
        let mut st = AudioState::new(rx, Arc::clone(&shared), s, 48_000.0);
        tx.send(Cmd::Play { from: 0, end: Some(1000), looped: None }).unwrap();
        let mut out = vec![0.0f32; 4096];
        st.fill(&mut out, 2);
        assert!(shared.position.load(Ordering::Relaxed) >= 1000);
        assert!(!shared.playing.load(Ordering::Relaxed));
    }

    #[test]
    fn looping_wraps() {
        let s = Arc::new(soundcraft_model::Session::default());
        let shared = Arc::new(Shared {
            position: AtomicI64::new(0),
            playing: AtomicBool::new(false),
            speed: AtomicU32::new(1000),
            meters: Mutex::new(MeterSnapshot::default()),
        });
        let (tx, rx) = channel();
        let mut st = AudioState::new(rx, Arc::clone(&shared), s, 48_000.0);
        tx.send(Cmd::Play { from: 0, end: None, looped: Some(Range::new(0, 1000)) }).unwrap();
        let mut out = vec![0.0f32; 20_000];
        st.fill(&mut out, 2);
        let p = shared.position.load(Ordering::Relaxed);
        assert!(p <= 1000, "position {p} should stay inside the loop");
        assert!(shared.playing.load(Ordering::Relaxed));
    }
}
