//! The SoundCraft mix engine.
//!
//! [`MixEngine::render`] produces one block of the main stereo output for a session at a timeline
//! position: clips (with clip gain and fades) → trim/phase → inserts → pre-fader sends → fader and
//! mute (with automation) → pan → post-fader sends → busses → aux inputs → main → master faders.
//! The same code drives realtime playback and offline bounces, so what you hear is what you get.
//! Steady-state rendering does not allocate.
#![deny(clippy::unwrap_used, clippy::expect_used, clippy::panic, clippy::unimplemented, clippy::todo, clippy::unreachable)]

use soundcraft_dsp::pan::{PanLaw, gains};
use soundcraft_dsp::{Plugin, db_to_gain};
use soundcraft_model::{AutoParam, BusId, Clip, ClipContent, Route, Session, Track, TrackId, TrackKind};
use soundcraft_time::{Range, Samples};
use std::collections::HashMap;

/// Main mix width.
pub const MAIN_CHANNELS: usize = 2;

/// Peak meter values for one strip (linear, per channel, max over the last block).
#[derive(Debug, Clone, Copy, Default, PartialEq)]
pub struct StripMeter {
    pub peak: [f32; 2],
    /// Gain reduction of the first dynamics insert, dB.
    pub gain_reduction_db: f32,
}

struct PluginSlot {
    id: String,
    plugin: Box<dyn Plugin>,
    /// Last values pushed to the plugin, in the insert's parameter-map order.
    applied: Vec<f32>,
}

struct Strip {
    buf: Vec<Vec<f32>>,
    /// Pre-fader copy for pre-fader sends.
    pre: Vec<Vec<f32>>,
    scratch: Vec<f32>,
    plugins: Vec<Option<PluginSlot>>,
    instrument: Option<PluginSlot>,
    /// MIDI notes currently sounding on the instrument (pitch), for note-offs at stops/seek.
    held: Vec<u8>,
    muted: bool,
    gr: f32,
    /// Per-clip effect chains (Clip Effects) and a scratch buffer to render clips into.
    clip_fx: HashMap<u64, ClipFx>,
    clipbuf: Vec<Vec<f32>>,
    /// Total latency of the active inserts (samples).
    latency: usize,
    /// Delay compensation: align this strip with the slowest one (post- and pre-fader paths),
    /// plus extra delay for direct-to-main outputs so they line up with aux returns.
    align: DelayLine,
    align_pre: DelayLine,
    out_extra: DelayLine,
    outbuf: Vec<Vec<f32>>,
}

/// A multichannel delay line (samples), resized only when the delay changes.
#[derive(Default)]
struct DelayLine {
    bufs: Vec<Vec<f32>>,
    idx: usize,
    len: usize,
}

impl DelayLine {
    fn set(&mut self, len: usize, ch: usize) {
        let len = len.min(1 << 20);
        if len != self.len || self.bufs.len() != ch {
            self.len = len;
            self.idx = 0;
            self.bufs = vec![vec![0.0; len]; ch];
        }
    }
    fn process(&mut self, io: &mut [Vec<f32>], frames: usize) {
        if self.len == 0 {
            return;
        }
        let start = self.idx;
        for (c, ch) in io.iter_mut().enumerate() {
            let Some(ring) = self.bufs.get_mut(c) else { continue };
            let mut i = start;
            for x in ch.iter_mut().take(frames) {
                if let Some(r) = ring.get_mut(i) {
                    std::mem::swap(r, x);
                }
                i += 1;
                if i >= self.len {
                    i = 0;
                }
            }
        }
        self.idx = (start + frames) % self.len.max(1);
    }
}

/// Clip Effects: an EQ and a compressor per clip, configured from `edit.values`
/// (`clip_fx.<clip>.eq.<param>`, `clip_fx.<clip>.comp.<param>`, `clip_fx.<clip>.gain`).
struct ClipFx {
    eq: Option<Box<dyn Plugin>>,
    comp: Option<Box<dyn Plugin>>,
    gain_db: f32,
    sr: f32,
    ch: usize,
}

impl Strip {
    fn new() -> Self {
        Strip {
            buf: Vec::new(),
            pre: Vec::new(),
            scratch: Vec::new(),
            plugins: Vec::new(),
            instrument: None,
            held: Vec::new(),
            muted: false,
            gr: 0.0,
            clip_fx: HashMap::new(),
            clipbuf: Vec::new(),
            latency: 0,
            align: DelayLine::default(),
            align_pre: DelayLine::default(),
            out_extra: DelayLine::default(),
            outbuf: Vec::new(),
        }
    }
}

/// Renders sessions. Keep one per playback stream; plugin state persists across blocks.
pub struct MixEngine {
    sample_rate: f32,
    max_block: usize,
    strips: HashMap<TrackId, Strip>,
    busses: HashMap<BusId, Vec<Vec<f32>>>,
    main: Vec<Vec<f32>>,
    pub meters: HashMap<TrackId, StripMeter>,
    pub main_meter: StripMeter,
    pub pan_law: PanLaw,
    /// Last rendered position end (to detect seeks).
    last_end: Samples,
    /// Snapshot key of the session the strips/order were synced to.
    synced: (usize, usize, usize),
    order: Vec<usize>,
    /// Current total compensation delay (samples) of the mix.
    pub latency: usize,
    /// Live input for the next block (planar), set by the audio host for input monitoring.
    pub input: Vec<Vec<f32>>,
    /// Transport stopped: only monitored inputs sound (no clips).
    pub monitor_only: bool,
    /// Recording: record-armed tracks hear their input (auto input).
    pub recording: bool,
}

impl MixEngine {
    pub fn new(sample_rate: f32, max_block: usize) -> Self {
        let max_block = max_block.clamp(16, 16_384);
        MixEngine {
            sample_rate: if sample_rate.is_finite() && sample_rate > 0.0 { sample_rate } else { 48_000.0 },
            max_block,
            strips: HashMap::new(),
            busses: HashMap::new(),
            main: vec![vec![0.0; max_block]; MAIN_CHANNELS],
            meters: HashMap::new(),
            main_meter: StripMeter::default(),
            pan_law: PanLaw::Minus3,
            last_end: 0,
            synced: (0, 0, 0),
            order: Vec::new(),
            latency: 0,
            input: Vec::new(),
            monitor_only: false,
            recording: false,
        }
    }

    pub fn max_block(&self) -> usize {
        self.max_block
    }

    /// Reset all plugin state (on stop / seek).
    pub fn reset(&mut self) {
        for s in self.strips.values_mut() {
            for p in s.plugins.iter_mut().flatten() {
                p.plugin.reset();
            }
            if let Some(i) = &mut s.instrument {
                i.plugin.all_notes_off();
                i.plugin.reset();
            }
            s.held.clear();
        }
        self.meters.clear();
        self.main_meter = StripMeter::default();
    }

    /// Bring plugin instances in line with the session (creates/destroys as needed). Allocates;
    /// call outside the realtime thread when the document changes, or let `render` do it.
    pub fn sync(&mut self, s: &Session) {
        let sr = self.sample_rate;
        let mb = self.max_block;
        self.strips.retain(|id, _| s.track(*id).is_some());
        for t in &s.tracks {
            let ch = strip_channels(t);
            let strip = self.strips.entry(t.id).or_insert_with(Strip::new);
            if strip.buf.len() != ch || strip.buf.first().map_or(0, Vec::len) != mb {
                strip.buf = vec![vec![0.0; mb]; ch];
            }
            strip.plugins.resize_with(t.mixer.inserts.len(), || None);
            for (slot, ins) in strip.plugins.iter_mut().zip(t.mixer.inserts.iter()) {
                match ins {
                    Some(i) if slot.as_ref().is_none_or(|p| p.id != i.plugin) => {
                        *slot = create_plugin(&i.plugin).map(|mut p| {
                            p.prepare(sr, mb, ch);
                            PluginSlot { id: i.plugin.clone(), plugin: p, applied: Vec::new() }
                        });
                    }
                    None => *slot = None,
                    _ => {}
                }
            }
            let want = t.instrument.as_ref().map(|i| i.plugin.as_str());
            if strip.instrument.as_ref().map(|p| p.id.as_str()) != want {
                strip.instrument = want.and_then(|id| {
                    create_plugin(id).map(|mut p| {
                        p.prepare(sr, mb, ch.max(2));
                        PluginSlot { id: id.to_string(), plugin: p, applied: Vec::new() }
                    })
                });
            }
        }
        for b in &s.busses {
            self.busses.entry(b.id).or_insert_with(|| vec![vec![0.0; mb]; MAIN_CHANNELS]);
        }
        self.busses.retain(|id, _| s.bus(*id).is_some());
    }

    /// Render `frames` (≤ max_block) samples starting at `pos` into `out` (≥ 2 channels).
    pub fn render(&mut self, s: &Session, pos: Samples, frames: usize, out: &mut [Vec<f32>]) {
        let frames = frames.min(self.max_block);
        if pos != self.last_end {
            // Seek: silence held notes and ringing plugins.
            for st in self.strips.values_mut() {
                if let Some(i) = &mut st.instrument {
                    i.plugin.all_notes_off();
                }
                st.held.clear();
            }
        }
        self.last_end = pos.saturating_add(frames as i64);
        // Re-sync plugin instances and routing order only when the session snapshot changes.
        let key = (structure_fingerprint(s), s.tracks.len(), s.busses.len());
        if key != self.synced {
            self.sync(s);
            self.order = processing_order(s);
            self.synced = key;
        }
        for b in self.busses.values_mut() {
            for c in b.iter_mut() {
                c.iter_mut().take(frames).for_each(|x| *x = 0.0);
            }
        }
        for c in &mut self.main {
            c.iter_mut().take(frames).for_each(|x| *x = 0.0);
        }
        let any_solo = s.tracks.iter().any(|t| t.mixer.solo && !t.inactive);
        let order = std::mem::take(&mut self.order);
        let live = |t: &&Track| !(t.kind == TrackKind::Master || t.inactive || t.kind == TrackKind::Vca);
        let is_aux = |t: &Track| matches!(t.kind, TrackKind::Aux | TrackKind::Folder);
        // Phase 1: independent strips (audio, instrument, MIDI) in parallel.
        let mut work: Vec<(&Track, Strip)> = order
            .iter()
            .filter_map(|&i| s.tracks.get(i))
            .filter(live)
            .filter(|t| !is_aux(t))
            .filter_map(|t| self.strips.remove(&t.id).map(|st| (t, st)))
            .collect();
        for (_, st) in work.iter_mut() {
            if st.scratch.len() < self.max_block {
                st.scratch.resize(self.max_block, 0.0);
            }
        }
        let busses = std::mem::take(&mut self.busses);
        let live_in = LiveInput { input: &self.input, monitor_only: self.monitor_only, recording: self.recording };
        run_strips(&mut work, |(t, st)| process_strip(s, t, st, &busses, &live_in, pos, frames, any_solo));
        self.busses = busses;
        // Delay compensation: align every strip to the slowest; direct-to-main outputs also wait
        // for the slowest aux return (aux latencies are from the previous block; they are stable).
        let pdc = s.edit.delay_compensation;
        let lmax_t = if pdc { work.iter().map(|(_, st)| st.latency).max().unwrap_or(0) } else { 0 };
        let lmax_a =
            if pdc { self.strips.iter().filter(|(id, _)| s.track(**id).is_some_and(is_aux)).map(|(_, st)| st.latency).max().unwrap_or(0) } else { 0 };
        self.latency = lmax_t + lmax_a;
        for (t, st) in work.iter_mut() {
            let ch = st.buf.len();
            st.align.set(lmax_t.saturating_sub(st.latency), ch);
            st.align.process(&mut st.buf, frames);
            if t.mixer.sends.iter().flatten().any(|x| x.pre_fader) {
                st.align_pre.set(lmax_t.saturating_sub(st.latency), ch);
                st.align_pre.process(&mut st.pre, frames);
            }
            st.out_extra.set(if t.mixer.output == Route::Main { lmax_a } else { 0 }, ch);
        }
        for (t, st) in work.iter_mut() {
            self.route_strip(t, st, pos, frames);
        }
        for (t, st) in work {
            self.strips.insert(t.id, st);
        }
        let is_aux = |t: &Track| matches!(t.kind, TrackKind::Aux | TrackKind::Folder);
        // Phase 2: auxes in dependency order (they read busses).
        for t in order.iter().filter_map(|&i| s.tracks.get(i)).filter(live).filter(|t| is_aux(t)) {
            let Some(mut st) = self.strips.remove(&t.id) else { continue };
            if st.scratch.len() < self.max_block {
                st.scratch.resize(self.max_block, 0.0);
            }
            let live_in = LiveInput { input: &self.input, monitor_only: self.monitor_only, recording: self.recording };
            process_strip(s, t, &mut st, &self.busses, &live_in, pos, frames, any_solo);
            let ch = st.buf.len();
            st.align.set(if pdc { lmax_a.saturating_sub(st.latency) } else { 0 }, ch);
            st.align.process(&mut st.buf, frames);
            st.out_extra.set(0, ch);
            self.route_strip(t, &mut st, pos, frames);
            self.strips.insert(t.id, st);
        }
        self.order = order;
        // Master faders process the main mix.
        for t in s.tracks.iter().filter(|t| t.kind == TrackKind::Master && !t.inactive) {
            if let Some(strip) = self.strips.get_mut(&t.id) {
                for (dst, src) in strip.buf.iter_mut().zip(self.main.iter()) {
                    if let (Some(d), Some(sr)) = (dst.get_mut(..frames), src.get(..frames)) {
                        d.copy_from_slice(sr);
                    }
                }
                for (i, ins) in t.mixer.inserts.iter().enumerate() {
                    if let (Some(ins), Some(Some(slot))) = (ins, strip.plugins.get_mut(i))
                        && ins.active
                        && !ins.bypass
                    {
                        apply_params(slot, ins, t, i, pos);
                        slot.plugin.process(&mut strip.buf, frames);
                    }
                }
                let v0 = volume_db_at(t, pos);
                let v1 = volume_db_at(t, pos + frames as i64);
                for (ch, src) in strip.buf.iter().enumerate().take(MAIN_CHANNELS) {
                    if let Some(m) = self.main.get_mut(ch) {
                        for i in 0..frames {
                            let g = lerp_gain(v0, v1, i, frames);
                            if let (Some(d), Some(x)) = (m.get_mut(i), src.get(i)) {
                                *d = x * g;
                            }
                        }
                    }
                }
                let mut pk = [0.0f32; 2];
                for (c, p) in pk.iter_mut().enumerate() {
                    *p = self.main.get(c).map_or(0.0, |m| peak(m.get(..frames).unwrap_or(&[])));
                }
                self.meters.insert(t.id, StripMeter { peak: pk, gain_reduction_db: 0.0 });
            }
        }
        let mut pk = [0.0f32; 2];
        for (c, o) in out.iter_mut().enumerate() {
            let src = self.main.get(c.min(MAIN_CHANNELS - 1));
            for i in 0..frames {
                let v = src.and_then(|m| m.get(i)).copied().unwrap_or(0.0);
                if let Some(d) = o.get_mut(i) {
                    *d = if v.is_finite() { v } else { 0.0 };
                }
            }
            if let Some(p) = pk.get_mut(c) {
                *p = peak(o.get(..frames).unwrap_or(&[]));
            }
        }
        self.main_meter = StripMeter { peak: pk, gain_reduction_db: 0.0 };
    }

    /// Send, meter and pan a processed strip into the busses / main mix.
    fn route_strip(&mut self, t: &Track, strip: &mut Strip, pos: Samples, frames: usize) {
        let mut pk = [0.0f32; 2];
        for (c, p) in pk.iter_mut().enumerate() {
            let ch = c.min(strip.buf.len().saturating_sub(1));
            *p = strip.buf.get(ch).map_or(0.0, |b| peak(b.get(..frames).unwrap_or(&[])));
        }
        self.meters.insert(t.id, StripMeter { peak: pk, gain_reduction_db: strip.gr });
        for (i, snd) in t.mixer.sends.iter().enumerate() {
            let Some(snd) = snd else { continue };
            if snd.mute || strip.muted {
                continue;
            }
            let lvl = send_level(t, i, snd.level_db, pos);
            let src = if snd.pre_fader { &strip.pre } else { &strip.buf };
            mix_to_bus(src, frames, &mut self.busses, &snd.target, db_to_gain(lvl), snd.pan, self.pan_law, &t.mixer.pan);
        }
        let pans: [f32; 2] = [pan_at(t, 0, pos), pan_at(t, 1, pos)];
        match &t.mixer.output {
            Route::Main if strip.out_extra.len > 0 => {
                if strip.outbuf.len() != strip.buf.len() {
                    strip.outbuf = vec![Vec::new(); strip.buf.len()];
                }
                for (d, src) in strip.outbuf.iter_mut().zip(strip.buf.iter()) {
                    d.clear();
                    d.extend_from_slice(src.get(..frames).unwrap_or(&[]));
                }
                strip.out_extra.process(&mut strip.outbuf, frames);
                pan_into(&strip.outbuf, frames, &mut self.main, &pans, self.pan_law);
            }
            Route::Main => pan_into(&strip.buf, frames, &mut self.main, &pans, self.pan_law),
            Route::Bus(b) => {
                if let Some(dst) = self.busses.get_mut(b) {
                    pan_into(&strip.buf, frames, dst, &pans, self.pan_law);
                }
            }
            _ => {}
        }
    }
}

/// Source → trim → inserts → (pre-fader copy) → mute/fader for one strip. Independent of other
/// strips except that aux inputs read `busses`, so non-aux strips can run in parallel.
/// Live input routed to monitored tracks.
struct LiveInput<'a> {
    input: &'a [Vec<f32>],
    monitor_only: bool,
    recording: bool,
}

fn process_strip(
    s: &Session,
    t: &Track,
    strip: &mut Strip,
    busses: &HashMap<BusId, Vec<Vec<f32>>>,
    live: &LiveInput<'_>,
    pos: Samples,
    frames: usize,
    any_solo: bool,
) {
    for c in strip.buf.iter_mut() {
        c.iter_mut().take(frames).for_each(|x| *x = 0.0);
    }
    match t.kind {
        TrackKind::Audio if !live.input.is_empty() && (t.mixer.input_monitor || (t.mixer.record_arm && (live.monitor_only || live.recording))) => {
            // Input monitoring: the track hears its hardware input instead of its clips.
            let first = match &t.mixer.input {
                Route::Hardware(h) => {
                    h.trim_start_matches("In ").split(['-', ' ']).next().and_then(|n| n.parse::<usize>().ok()).map_or(0, |n| n.saturating_sub(1))
                }
                _ => 0,
            };
            let n_in = live.input.len().max(1);
            for (k, dst) in strip.buf.iter_mut().enumerate() {
                if let Some(src) = live.input.get((first + k) % n_in) {
                    for (d, x) in dst.iter_mut().zip(src.iter()).take(frames) {
                        *d = *x;
                    }
                }
            }
        }
        TrackKind::Audio if live.monitor_only => {}
        TrackKind::Instrument | TrackKind::Midi if live.monitor_only => {}
        TrackKind::Audio => {
            let block = Range { start: pos, end: pos.saturating_add(frames as i64) };
            for clip in t.clips() {
                if clip.muted || !clip.is_audio() || !clip.range().overlaps(&block) {
                    continue;
                }
                match clip_fx_params(s, clip.id.0) {
                    None => render_clip(s, clip, pos, frames, &mut strip.buf, &mut strip.scratch),
                    Some(params) => {
                        let nch = strip.buf.len();
                        if strip.clipbuf.len() != nch || strip.clipbuf.first().map_or(0, Vec::len) < frames {
                            strip.clipbuf = vec![vec![0.0; frames.max(strip.scratch.len())]; nch];
                        }
                        for c in strip.clipbuf.iter_mut() {
                            c.iter_mut().take(frames).for_each(|x| *x = 0.0);
                        }
                        render_clip(s, clip, pos, frames, &mut strip.clipbuf, &mut strip.scratch);
                        let fx = strip.clip_fx.entry(clip.id.0).or_insert_with(|| ClipFx::new(s.sample_rate.as_f64() as f32, nch));
                        fx.configure(&params);
                        fx.process(&mut strip.clipbuf, frames);
                        for (d, c) in strip.buf.iter_mut().zip(strip.clipbuf.iter()) {
                            for (x, y) in d.iter_mut().zip(c.iter()).take(frames) {
                                *x += *y;
                            }
                        }
                    }
                }
            }
        }
        TrackKind::Aux | TrackKind::Folder => {
            if let Route::Bus(b) = &t.mixer.input
                && let Some(src) = busses.get(b)
            {
                let nch = strip.buf.len();
                for (ch, dst) in strip.buf.iter_mut().enumerate() {
                    for i in 0..frames {
                        let v = if nch == 1 {
                            0.5 * (src.first().and_then(|c| c.get(i)).copied().unwrap_or(0.0)
                                + src.get(1).and_then(|c| c.get(i)).copied().unwrap_or(0.0))
                        } else {
                            src.get(ch.min(1)).and_then(|c| c.get(i)).copied().unwrap_or(0.0)
                        };
                        if let Some(d) = dst.get_mut(i) {
                            *d = v;
                        }
                    }
                }
            }
        }
        TrackKind::Instrument | TrackKind::Midi => {
            if let Some(inst) = &mut strip.instrument {
                schedule_notes(s, t, pos, frames, inst, &mut strip.held);
                if strip.buf.len() >= 2 {
                    inst.plugin.process(&mut strip.buf, frames);
                } else if let Some(c0) = strip.buf.first_mut() {
                    // Mono MIDI track: render the instrument in stereo and fold.
                    let mut st = [std::mem::take(c0), std::mem::take(&mut strip.scratch)];
                    inst.plugin.process(&mut st, frames);
                    let [a, b] = st;
                    *c0 = a;
                    strip.scratch = b;
                    for (x, y) in c0.iter_mut().zip(strip.scratch.iter()).take(frames) {
                        *x = 0.5 * (*x + *y);
                    }
                }
            }
        }
        _ => {}
    }
    let trim = db_to_gain(t.mixer.trim_db) * if t.mixer.phase_invert { -1.0 } else { 1.0 };
    if (trim - 1.0).abs() > f32::EPSILON {
        for c in strip.buf.iter_mut() {
            c.iter_mut().take(frames).for_each(|x| *x *= trim);
        }
    }
    let mut gr = 0.0f32;
    let mut latency = 0usize;
    for (i, ins) in t.mixer.inserts.iter().enumerate() {
        let (Some(ins), Some(Some(slot))) = (ins, strip.plugins.get_mut(i)) else { continue };
        if !ins.active || ins.bypass {
            continue;
        }
        apply_params(slot, ins, t, i, pos);
        slot.plugin.process(&mut strip.buf, frames);
        latency = latency.saturating_add(slot.plugin.latency());
        if gr == 0.0 {
            gr = slot.plugin.gain_reduction_db();
        }
    }
    strip.gr = gr;
    strip.latency = latency;
    let auto_mute = t.mixer.automation_mode.reads() && t.lane(&AutoParam::Mute).is_some_and(|l| l.value_at(pos, 0.0) >= 0.5);
    let soloed_out = any_solo && !t.mixer.solo && !t.mixer.solo_safe && !receives_solo(s, t);
    strip.muted = t.mixer.mute || auto_mute || soloed_out;
    if t.mixer.sends.iter().flatten().any(|x| x.pre_fader) {
        strip.pre.resize_with(strip.buf.len(), Vec::new);
        for (d, src) in strip.pre.iter_mut().zip(strip.buf.iter()) {
            d.clear();
            d.extend_from_slice(src.get(..frames).unwrap_or(&[]));
        }
    }
    let mut v0 = volume_db_at(t, pos);
    let mut v1 = volume_db_at(t, pos + frames as i64);
    // Trim automation (an offset on top of the volume curve).
    if let Some(l) = t
        .automation
        .iter()
        .find(|l| matches!(&l.param, AutoParam::Plugin { slot: u8::MAX, param } if param == "trim"))
        .filter(|l| !l.points.is_empty())
    {
        v0 += l.value_at(pos, 0.0);
        v1 += l.value_at(pos + frames as i64, 0.0);
    }
    // VCA master: its fader offsets the member's, and its mute mutes the member.
    if let Some(vid) = t.mixer.vca
        && let Some(vca) = s.tracks.iter().find(|x| x.id.0 == vid && x.kind == TrackKind::Vca)
    {
        v0 += volume_db_at(vca, pos);
        v1 += volume_db_at(vca, pos + frames as i64);
        if vca.mixer.mute {
            strip.muted = true;
        }
    }
    let (g0, g1) = if strip.muted { (-144.0, -144.0) } else { (v0.min(12.0), v1.min(12.0)) };
    for c in strip.buf.iter_mut() {
        for i in 0..frames {
            if let Some(x) = c.get_mut(i) {
                *x *= lerp_gain(g0, g1, i, frames);
            }
        }
    }
}

/// Clip-effect parameters of a clip, or None when it has none or they are bypassed.
fn clip_fx_params(s: &Session, clip: u64) -> Option<Vec<(String, f32)>> {
    if s.edit.values.is_empty() {
        return None;
    }
    let pre = format!("clip_fx.{clip}.");
    let v: Vec<(String, f32)> = s
        .edit
        .values
        .range(pre.clone()..)
        .take_while(|(k, _)| k.starts_with(&pre))
        .map(|(k, v)| (k.get(pre.len()..).unwrap_or("").to_string(), *v as f32))
        .collect();
    if v.is_empty() || s.edit.flag(&format!("clip_fx.bypass.{clip}")) { None } else { Some(v) }
}

impl ClipFx {
    fn new(sr: f32, ch: usize) -> ClipFx {
        ClipFx { eq: None, comp: None, gain_db: 0.0, sr, ch }
    }
    fn make(&self, id: &str) -> Option<Box<dyn Plugin>> {
        soundcraft_dsp::create(id).map(|mut p| {
            p.prepare(self.sr, 16_384, self.ch.max(1));
            p
        })
    }
    /// Modules exist only when one of their parameters is set.
    fn configure(&mut self, params: &[(String, f32)]) {
        self.gain_db = 0.0;
        let has_eq = params.iter().any(|(k, _)| k.starts_with("eq."));
        let has_comp = params.iter().any(|(k, _)| k.starts_with("comp."));
        if has_eq && self.eq.is_none() {
            self.eq = self.make("eq_7band");
        } else if !has_eq {
            self.eq = None;
        }
        if has_comp && self.comp.is_none() {
            self.comp = self.make("compressor");
        } else if !has_comp {
            self.comp = None;
        }
        for (k, v) in params {
            if let Some(p) = k.strip_prefix("eq.") {
                if let Some(eq) = &mut self.eq
                    && eq.param(p) != Some(*v)
                {
                    eq.set_param(p, *v);
                }
            } else if let Some(p) = k.strip_prefix("comp.") {
                if let Some(c) = &mut self.comp
                    && c.param(p) != Some(*v)
                {
                    c.set_param(p, *v);
                }
            } else if k == "gain" {
                self.gain_db = *v;
            }
        }
    }
    fn process(&mut self, io: &mut [Vec<f32>], frames: usize) {
        if let Some(eq) = &mut self.eq {
            eq.process(io, frames);
        }
        if let Some(c) = &mut self.comp {
            c.process(io, frames);
        }
        if self.gain_db.abs() > 1e-6 {
            let g = db_to_gain(self.gain_db);
            for ch in io.iter_mut() {
                ch.iter_mut().take(frames).for_each(|x| *x *= g);
            }
        }
    }
}

/// Hash of everything `sync`/`processing_order` depend on (tracks, kinds, routing, plugins, sends).
fn structure_fingerprint(s: &Session) -> usize {
    use std::hash::{Hash, Hasher};
    let mut h = std::collections::hash_map::DefaultHasher::new();
    for t in &s.tracks {
        t.id.hash(&mut h);
        t.kind.hash(&mut h);
        t.format.hash(&mut h);
        t.inactive.hash(&mut h);
        t.mixer.input.hash(&mut h);
        t.mixer.output.hash(&mut h);
        for i in &t.mixer.inserts {
            i.as_ref().map(|x| x.plugin.as_str()).hash(&mut h);
        }
        for snd in t.mixer.sends.iter().flatten() {
            snd.target.hash(&mut h);
        }
        t.instrument.as_ref().map(|x| x.plugin.as_str()).hash(&mut h);
    }
    for b in &s.busses {
        b.id.hash(&mut h);
    }
    h.finish() as usize
}

/// Process strips, in parallel on native targets when there are enough of them.
#[cfg(not(target_arch = "wasm32"))]
fn run_strips<T: Send>(work: &mut [T], f: impl Fn(&mut T) + Sync + Send) {
    use rayon::prelude::*;
    if work.len() >= 4 {
        work.par_iter_mut().for_each(f);
    } else {
        work.iter_mut().for_each(f);
    }
}

#[cfg(target_arch = "wasm32")]
fn run_strips<T: Send>(work: &mut [T], f: impl Fn(&mut T) + Sync + Send) {
    work.iter_mut().for_each(f);
}

fn strip_channels(t: &Track) -> usize {
    match t.kind {
        TrackKind::Master => MAIN_CHANNELS,
        TrackKind::Instrument => t.channels().max(2),
        _ => t.channels().clamp(1, 16),
    }
}

/// Audio/instrument tracks first, then auxes ordered so bus producers come before consumers.
fn processing_order(s: &Session) -> Vec<usize> {
    let mut order: Vec<usize> =
        (0..s.tracks.len()).filter(|&i| s.tracks.get(i).is_some_and(|t| !matches!(t.kind, TrackKind::Aux | TrackKind::Folder))).collect();
    let mut auxes: Vec<usize> =
        (0..s.tracks.len()).filter(|&i| s.tracks.get(i).is_some_and(|t| matches!(t.kind, TrackKind::Aux | TrackKind::Folder))).collect();
    // Depth = how many aux hops feed this aux's input bus (bounded; cycles cap out).
    let depth = |i: usize| -> usize {
        let mut d = 0;
        let mut cur = vec![i];
        for _ in 0..8 {
            let inputs: Vec<&Route> = cur.iter().filter_map(|&k| s.tracks.get(k)).map(|t| &t.mixer.input).collect();
            let feeders: Vec<usize> = auxes_feeding(s, &inputs);
            if feeders.is_empty() {
                break;
            }
            d += 1;
            cur = feeders;
        }
        d
    };
    auxes.sort_by_key(|&i| depth(i));
    order.extend(auxes);
    order
}

fn auxes_feeding(s: &Session, inputs: &[&Route]) -> Vec<usize> {
    s.tracks
        .iter()
        .enumerate()
        .filter(|(_, t)| matches!(t.kind, TrackKind::Aux | TrackKind::Folder))
        .filter(|(_, t)| {
            inputs.iter().any(|r| matches!(r, Route::Bus(_)) && (&&t.mixer.output == r || t.mixer.sends.iter().flatten().any(|sn| &&sn.target == r)))
        })
        .map(|(i, _)| i)
        .collect()
}

/// Implicit solo: an aux is not muted when a soloed track feeds its input bus.
fn receives_solo(s: &Session, t: &Track) -> bool {
    let Route::Bus(b) = &t.mixer.input else { return false };
    s.tracks
        .iter()
        .any(|o| o.mixer.solo && (o.mixer.output == Route::Bus(*b) || o.mixer.sends.iter().flatten().any(|sn| sn.target == Route::Bus(*b))))
}

fn render_clip(s: &Session, clip: &Clip, pos: Samples, frames: usize, buf: &mut [Vec<f32>], _scratch: &mut [f32]) {
    let ClipContent::Audio { source, offset } = clip.content else { return };
    let block = Range { start: pos, end: pos.saturating_add(frames as i64) };
    let Some(isect) = clip.range().intersect(&block) else { return };
    let Some(audio) = s.pool.get(source) else { return };
    let nsrc = audio.buffer.channels.len();
    if nsrc == 0 {
        return;
    }
    let stretch = if clip.stretch.is_finite() && clip.stretch > 0.0 { clip.stretch } else { 1.0 };
    let i0 = usize::try_from(isect.start - pos).unwrap_or(0);
    let i1 = usize::try_from(isect.end - pos).unwrap_or(0).min(frames);
    // Gain is evaluated every 32 samples and interpolated (fades and envelopes stay smooth).
    const STEP: usize = 32;
    for (ch, dst) in buf.iter_mut().enumerate() {
        let src = audio.buffer.channels.get(ch % nsrc).map(Vec::as_slice).unwrap_or(&[]);
        let mut i = i0;
        while i < i1 {
            let seg_end = (i + STEP).min(i1);
            let rel0 = pos + i as i64 - clip.start;
            let rel1 = pos + seg_end as i64 - clip.start;
            let g0 = clip.gain_at(rel0);
            let g1 = clip.gain_at(rel1.min(clip.length));
            let n = (seg_end - i) as f32;
            for k in i..seg_end {
                let rel = pos + k as i64 - clip.start;
                let v = if (stretch - 1.0).abs() < 1e-9 {
                    usize::try_from(offset + rel).ok().and_then(|x| src.get(x)).copied().unwrap_or(0.0)
                } else {
                    let f = offset as f64 + rel as f64 / stretch;
                    let x0 = f.floor();
                    let fr = (f - x0) as f32;
                    let a = if x0 >= 0.0 { src.get(x0 as usize).copied().unwrap_or(0.0) } else { 0.0 };
                    let b = if x0 + 1.0 >= 0.0 { src.get(x0 as usize + 1).copied().unwrap_or(0.0) } else { 0.0 };
                    a + (b - a) * fr
                };
                let t = (k - i) as f32 / n;
                if let Some(d) = dst.get_mut(k) {
                    *d += v * (g0 + (g1 - g0) * t);
                }
            }
            i = seg_end;
        }
    }
}

/// Send note on/offs for MIDI clips overlapping the block.
fn schedule_notes(s: &Session, t: &Track, pos: Samples, frames: usize, inst: &mut PluginSlot, held: &mut Vec<u8>) {
    let sr = s.sample_rate;
    let end = pos.saturating_add(frames as i64);
    for clip in t.clips() {
        if clip.muted {
            continue;
        }
        let ClipContent::Midi { sequence } = &clip.content else { continue };
        if !clip.range().overlaps(&Range { start: pos, end }) {
            continue;
        }
        let base = s.tempo.samples_to_ticks(clip.start, sr);
        // MIDI Real-Time Properties (non-destructive velocity/transpose/duration/delay).
        let rtp = |name: &str, d: f64| s.edit.value(&format!("rtp.{}.{name}", t.id.0), d);
        let (dv, dp, dur, delay) = if s.edit.values.is_empty() {
            (0i64, 0i64, 1.0f64, 0i64)
        } else {
            (
                rtp("velocity", 0.0).clamp(-127.0, 127.0) as i64,
                rtp("transpose", 0.0).clamp(-127.0, 127.0) as i64,
                rtp("duration", 100.0).clamp(1.0, 1000.0) / 100.0,
                rtp("delay", 0.0).clamp(-1e7, 1e7) as i64,
            )
        };
        for n0 in &sequence.notes {
            let mut n = *n0;
            n.velocity = (i64::from(n.velocity) + dv).clamp(1, 127) as u8;
            n.pitch = (i64::from(n.pitch) + dp).clamp(0, 127) as u8;
            n.length = ((n.length as f64 * dur) as i64).max(1);
            n.start = (n.start + delay).max(0);
            let on = s.tempo.tick_to_samples(base + n.start, sr);
            let off = s.tempo.tick_to_samples(base + n.start + n.length, sr).min(clip.end());
            if on >= clip.end() {
                continue;
            }
            if on >= pos && on < end {
                inst.plugin.note_on(usize::try_from(on - pos).unwrap_or(0), n.pitch, n.velocity);
                if !held.contains(&n.pitch) && held.len() < 128 {
                    held.push(n.pitch);
                }
            }
            if off >= pos && off < end {
                inst.plugin.note_off(usize::try_from(off - pos).unwrap_or(0), n.pitch);
                held.retain(|p| *p != n.pitch);
            }
        }
    }
}

fn apply_params(slot: &mut PluginSlot, ins: &soundcraft_model::Insert, t: &Track, i: usize, pos: Samples) {
    let slot_u8 = u8::try_from(i).unwrap_or(0);
    let automated = t.mixer.automation_mode.reads()
        && t.automation.iter().any(|l| matches!(&l.param, AutoParam::Plugin { slot, .. } if *slot == slot_u8) && !l.points.is_empty());
    if slot.applied.len() != ins.params.len() {
        slot.applied = vec![f32::NAN; ins.params.len()];
    }
    for ((k, v), last) in ins.params.iter().zip(slot.applied.iter_mut()) {
        let mut val = *v;
        if automated
            && let Some(l) = t.automation.iter().find(|l| matches!(&l.param, AutoParam::Plugin { slot, param } if *slot == slot_u8 && param == k))
            && !l.points.is_empty()
        {
            val = l.value_at(pos, val);
        }
        if val.to_bits() != last.to_bits() {
            slot.plugin.set_param(k, val);
            *last = val;
        }
    }
}

fn volume_db_at(t: &Track, pos: Samples) -> f32 {
    if t.mixer.automation_mode.reads()
        && t.mixer.automation_mode != soundcraft_model::AutomationMode::Write
        && let Some(l) = t.lane(&AutoParam::Volume)
        && !l.points.is_empty()
    {
        return l.value_at(pos, t.mixer.volume_db);
    }
    t.mixer.volume_db
}

fn pan_at(t: &Track, idx: usize, pos: Samples) -> f32 {
    let base = t.mixer.pan.get(idx).copied().unwrap_or(if idx == 0 { 0.0 } else { 1.0 });
    if t.mixer.automation_mode.reads()
        && let Some(l) = t.lane(&AutoParam::Pan(u8::try_from(idx).unwrap_or(0)))
        && !l.points.is_empty()
    {
        return l.value_at(pos, base);
    }
    base
}

fn send_level(t: &Track, i: usize, base: f32, pos: Samples) -> f32 {
    if t.mixer.automation_mode.reads()
        && let Some(l) = t.lane(&AutoParam::SendLevel(u8::try_from(i).unwrap_or(0)))
        && !l.points.is_empty()
    {
        return l.value_at(pos, base);
    }
    base
}

fn lerp_gain(db0: f32, db1: f32, i: usize, n: usize) -> f32 {
    let g0 = db_to_gain(db0);
    let g1 = db_to_gain(db1);
    if n == 0 { g0 } else { g0 + (g1 - g0) * (i as f32 / n as f32) }
}

fn peak(x: &[f32]) -> f32 {
    x.iter().fold(0.0f32, |m, v| if v.is_finite() { m.max(v.abs()) } else { m })
}

/// Pan a strip (1, 2 or N channels) into a stereo destination.
fn pan_into(src: &[Vec<f32>], frames: usize, dst: &mut [Vec<f32>], pans: &[f32; 2], law: PanLaw) {
    let (l, r) = dst.split_at_mut(1);
    let (Some(dl), Some(dr)) = (l.first_mut(), r.first_mut()) else { return };
    match src.len() {
        0 => {}
        1 => {
            let (gl, gr) = gains(pans[0], law);
            if let Some(s0) = src.first() {
                for i in 0..frames {
                    let v = s0.get(i).copied().unwrap_or(0.0);
                    if let (Some(a), Some(b)) = (dl.get_mut(i), dr.get_mut(i)) {
                        *a += v * gl;
                        *b += v * gr;
                    }
                }
            }
        }
        n => {
            // Stereo: each side has its own panner. Wider formats fold extra channels to centre.
            let (a_l, a_r) = gains(pans[0], PanLaw::Zero);
            let (b_l, b_r) = gains(pans[1], PanLaw::Zero);
            for i in 0..frames {
                let x0 = src.first().and_then(|c| c.get(i)).copied().unwrap_or(0.0);
                let x1 = src.get(1).and_then(|c| c.get(i)).copied().unwrap_or(0.0);
                let mut c = 0.0;
                for ch in src.iter().skip(2).take(n.saturating_sub(2)) {
                    c += ch.get(i).copied().unwrap_or(0.0) * std::f32::consts::FRAC_1_SQRT_2;
                }
                if let (Some(a), Some(b)) = (dl.get_mut(i), dr.get_mut(i)) {
                    *a += x0 * a_l + x1 * b_l + c;
                    *b += x0 * a_r + x1 * b_r + c;
                }
            }
        }
    }
}

fn mix_to_bus(
    src: &[Vec<f32>],
    frames: usize,
    busses: &mut HashMap<BusId, Vec<Vec<f32>>>,
    target: &Route,
    gain: f32,
    send_pan: f32,
    law: PanLaw,
    track_pan: &[f32],
) {
    let Route::Bus(b) = target else { return };
    let Some(dst) = busses.get_mut(b) else { return };
    let pans = if src.len() == 1 { [send_pan, 0.0] } else { [track_pan.first().copied().unwrap_or(-1.0), track_pan.get(1).copied().unwrap_or(1.0)] };
    if gain <= 0.0 {
        return;
    }
    // Scale via a temporary pass over the destination: add gain*src.
    let (l, r) = dst.split_at_mut(1);
    let (Some(dl), Some(dr)) = (l.first_mut(), r.first_mut()) else { return };
    if src.len() == 1 {
        let (gl, gr) = gains(pans[0], law);
        if let Some(s0) = src.first() {
            for i in 0..frames {
                let v = s0.get(i).copied().unwrap_or(0.0) * gain;
                if let (Some(a), Some(bb)) = (dl.get_mut(i), dr.get_mut(i)) {
                    *a += v * gl;
                    *bb += v * gr;
                }
            }
        }
    } else {
        for i in 0..frames {
            let x0 = src.first().and_then(|c| c.get(i)).copied().unwrap_or(0.0) * gain;
            let x1 = src.get(1).and_then(|c| c.get(i)).copied().unwrap_or(0.0) * gain;
            if let (Some(a), Some(bb)) = (dl.get_mut(i), dr.get_mut(i)) {
                *a += x0;
                *bb += x1;
            }
        }
    }
}

/// Offline render of the main mix over `range` (stereo, planar). The mix's delay-compensation
/// latency is removed, so the result lines up with the timeline.
pub fn render_range(s: &Session, range: Range, block: usize) -> Vec<Vec<f32>> {
    let len = usize::try_from(range.len().max(0)).unwrap_or(0);
    // Probe the latency with one block, then render `len + latency` and drop the head.
    let latency = {
        let mut probe = MixEngine::new(s.sample_rate.as_f64() as f32, block);
        let b = probe.max_block();
        let mut tmp = vec![vec![0.0f32; b]; MAIN_CHANNELS];
        probe.render(s, range.start, b.min(len.max(1)), &mut tmp);
        probe.latency
    };
    let total = len.saturating_add(latency);
    let mut out = vec![vec![0.0f32; total]; MAIN_CHANNELS];
    let mut eng = MixEngine::new(s.sample_rate.as_f64() as f32, block);
    let b = eng.max_block();
    let mut tmp = vec![vec![0.0f32; b]; MAIN_CHANNELS];
    let mut done = 0usize;
    while done < total {
        let n = (total - done).min(b);
        eng.render(s, range.start + done as i64, n, &mut tmp);
        for (o, t) in out.iter_mut().zip(tmp.iter()) {
            if let (Some(d), Some(sr)) = (o.get_mut(done..done + n), t.get(..n)) {
                d.copy_from_slice(sr);
            }
        }
        done += n;
    }
    if latency > 0 {
        for c in &mut out {
            c.drain(..latency.min(c.len()));
        }
    }
    out
}

/// Render only one track's clips (no mixer processing) — Consolidate.
pub fn render_clips(s: &Session, track: TrackId, range: Range) -> Vec<Vec<f32>> {
    let Some(t) = s.track(track) else { return Vec::new() };
    let len = usize::try_from(range.len().max(0)).unwrap_or(0);
    let mut out = vec![vec![0.0f32; len]; t.channels().max(1)];
    let mut scratch = Vec::new();
    for c in t.clips() {
        if c.muted {
            continue;
        }
        match clip_fx_params(s, c.id.0) {
            None => render_clip(s, c, range.start, len, &mut out, &mut scratch),
            Some(params) => {
                let mut tmp = vec![vec![0.0f32; len]; out.len()];
                render_clip(s, c, range.start, len, &mut tmp, &mut scratch);
                let mut fx = ClipFx::new(s.sample_rate.as_f64() as f32, tmp.len());
                fx.configure(&params);
                let mut done = 0;
                while done < len {
                    let n = (len - done).min(4096);
                    let mut block: Vec<Vec<f32>> = tmp.iter().map(|ch| ch.get(done..done + n).map(<[f32]>::to_vec).unwrap_or_default()).collect();
                    fx.process(&mut block, n);
                    for (o, b) in out.iter_mut().zip(block.iter()) {
                        if let Some(dst) = o.get_mut(done..done + n) {
                            for (x, y) in dst.iter_mut().zip(b.iter()) {
                                *x += *y;
                            }
                        }
                    }
                    done += n;
                }
            }
        }
    }
    out
}

/// Render one track through its inserts (pre-fader) — Commit / Freeze.
pub fn render_track_pre_fader(s: &Session, track: TrackId, range: Range) -> Vec<Vec<f32>> {
    let mut solo = s.clone();
    for t in &mut solo.tracks {
        if t.id == track {
            t.mixer.volume_db = 0.0;
            t.mixer.mute = false;
            t.mixer.solo = false;
            t.mixer.output = Route::Main;
            t.mixer.pan = match t.channels() {
                1 => vec![0.0],
                _ => vec![-1.0, 1.0],
            };
            t.automation.retain(|l| matches!(l.param, AutoParam::Plugin { .. }));
            t.mixer.sends.iter_mut().for_each(|x| *x = None);
        } else {
            t.inactive = true;
        }
    }
    let mut out = render_range(&solo, range, 1024);
    // Mono tracks: fold back to one channel (undo the centre pan law).
    if s.track(track).is_some_and(|t| t.channels() == 1)
        && let Some(l) = out.first().cloned()
    {
        let g = std::f32::consts::SQRT_2;
        out = vec![l.iter().map(|x| x * g).collect()];
    }
    out
}

/// A built-in plugin, else a hosted CLAP plugin (`clap:<id>`).
fn create_plugin(id: &str) -> Option<Box<dyn Plugin>> {
    soundcraft_dsp::create(id).or_else(|| soundcraft_clap_host::create(id))
}

#[cfg(test)]
mod tests {
    use super::*;
    use soundcraft_audio_io::AudioBuffer;
    use soundcraft_model::{ChannelFormat, Insert, SourceAudio, SourceId};
    use std::sync::Arc;

    fn session_with_dc(level: f32, frames: usize) -> (Session, TrackId) {
        let mut s = Session::default();
        let t = s.add_track(TrackKind::Audio, ChannelFormat::Mono, None);
        let buf = AudioBuffer { sample_rate: 48_000, channels: vec![vec![level; frames]] };
        s.pool.insert(SourceId(900), Arc::new(SourceAudio::new(buf)));
        let id = s.new_clip_id();
        if let Some(pl) = s.track_mut(t).and_then(|t| t.playlist_mut()) {
            pl.clips.push(Clip::audio(id, "dc", SourceId(900), 0, 0, frames as i64));
        }
        (s, t)
    }

    #[test]
    fn mono_center_pan_is_minus_3db() {
        let (s, _) = session_with_dc(0.5, 4800);
        let out = render_range(&s, Range::new(0, 4800), 512);
        let expect = 0.5 * std::f32::consts::FRAC_1_SQRT_2;
        assert!((out[0][1000] - expect).abs() < 1e-3, "{}", out[0][1000]);
        assert!((out[1][1000] - expect).abs() < 1e-3);
    }

    #[test]
    fn fader_mute_and_solo() {
        let (mut s, t) = session_with_dc(0.5, 4800);
        s.track_mut(t).unwrap().mixer.volume_db = -6.0;
        let out = render_range(&s, Range::new(0, 4800), 512);
        let expect = 0.5 * std::f32::consts::FRAC_1_SQRT_2 * db_to_gain(-6.0);
        assert!((out[0][2000] - expect).abs() < 1e-3);
        s.track_mut(t).unwrap().mixer.mute = true;
        let out = render_range(&s, Range::new(0, 4800), 512);
        assert!(out[0][2000].abs() < 1e-6);
        // Soloing another track mutes this one.
        s.track_mut(t).unwrap().mixer.mute = false;
        let other = s.add_track(TrackKind::Audio, ChannelFormat::Mono, None);
        s.track_mut(other).unwrap().mixer.solo = true;
        let out = render_range(&s, Range::new(0, 4800), 512);
        assert!(out[0][2000].abs() < 1e-6);
    }

    #[test]
    fn hard_pans() {
        let (mut s, t) = session_with_dc(0.5, 4800);
        s.track_mut(t).unwrap().mixer.pan = vec![-1.0];
        let out = render_range(&s, Range::new(0, 4800), 256);
        assert!(out[0][100] > 0.49 && out[1][100].abs() < 1e-4);
    }

    #[test]
    fn fades_and_clip_gain_apply() {
        let (mut s, t) = session_with_dc(1.0, 48_000);
        {
            let c = &mut s.track_mut(t).unwrap().playlist_mut().unwrap().clips[0];
            c.fade_in = soundcraft_model::Fade { len: 1000, shape: soundcraft_model::FadeShape::Linear };
            c.gain_db = -6.0;
        }
        let out = render_range(&s, Range::new(0, 2000), 512);
        let g = std::f32::consts::FRAC_1_SQRT_2 * db_to_gain(-6.0);
        assert!(out[0][0].abs() < 1e-3);
        assert!((out[0][500] - 0.5 * g).abs() < 0.02, "{}", out[0][500]);
        assert!((out[0][1500] - g).abs() < 1e-3);
    }

    #[test]
    fn sends_feed_aux_via_bus() {
        let (mut s, t) = session_with_dc(0.5, 4800);
        let bus = s.add_bus("Verb", ChannelFormat::Stereo);
        let aux = s.add_track(TrackKind::Aux, ChannelFormat::Stereo, Some("Verb"));
        s.track_mut(aux).unwrap().mixer.input = Route::Bus(bus);
        {
            let tr = s.track_mut(t).unwrap();
            tr.mixer.output = Route::None;
            let mut snd = soundcraft_model::SendSlot::new(Route::Bus(bus));
            snd.level_db = 0.0;
            tr.mixer.sends[0] = Some(snd);
        }
        let out = render_range(&s, Range::new(0, 4800), 512);
        assert!(out[0][1000] > 0.1, "aux should carry the send: {}", out[0][1000]);
    }

    #[test]
    fn volume_automation_ramps() {
        let (mut s, t) = session_with_dc(0.5, 48_000);
        {
            let tr = s.track_mut(t).unwrap();
            let l = tr.lane_mut(&AutoParam::Volume);
            l.set_point(0, 0.0);
            l.set_point(24_000, -144.0);
        }
        let out = render_range(&s, Range::new(0, 48_000), 512);
        assert!(out[0][100] > 0.3);
        assert!(out[0][30_000].abs() < 1e-4);
    }

    #[test]
    fn inserts_process_and_survive_unknown_plugins() {
        let (mut s, t) = session_with_dc(0.5, 4800);
        s.track_mut(t).unwrap().mixer.inserts[0] = Some(Insert::new("does-not-exist"));
        let out = render_range(&s, Range::new(0, 4800), 512);
        assert!(out[0][100] > 0.3);
        let id = soundcraft_dsp::plugins().iter().find(|p| p.id.contains("gain")).map(|p| p.id).unwrap_or("gain");
        let mut ins = Insert::new(id);
        ins.params.insert("gain".into(), -6.0);
        s.track_mut(t).unwrap().mixer.inserts[0] = Some(ins);
        let _ = render_range(&s, Range::new(0, 4800), 512);
    }

    #[test]
    fn clip_effects_gain_and_trim_automation_apply() {
        let (mut s, t) = session_with_dc(0.5, 4800);
        let cid = s.track(t).unwrap().clips()[0].id.0;
        s.edit.values.insert(format!("clip_fx.{cid}.gain"), -6.0);
        let out = render_range(&s, Range::new(0, 4800), 512);
        let expect = 0.5 * std::f32::consts::FRAC_1_SQRT_2 * db_to_gain(-6.0);
        assert!((out[0][2000] - expect).abs() < 0.01, "{}", out[0][2000]);
        s.edit.set_flag(&format!("clip_fx.bypass.{cid}"), true);
        let out = render_range(&s, Range::new(0, 4800), 512);
        assert!((out[0][2000] - 0.5 * std::f32::consts::FRAC_1_SQRT_2).abs() < 0.01);
        s.track_mut(t).unwrap().lane_mut(&AutoParam::Plugin { slot: u8::MAX, param: "trim".into() }).set_point(0, -12.0);
        let out = render_range(&s, Range::new(0, 4800), 512);
        assert!(out[0][2000] < 0.2, "{}", out[0][2000]);
    }

    #[test]
    fn delay_compensation_aligns_latent_tracks() {
        let mut s = Session::default();
        let mut imp = vec![0.0f32; 48_000];
        imp[1000] = 1.0;
        s.pool.insert(SourceId(900), Arc::new(SourceAudio::new(AudioBuffer { sample_rate: 48_000, channels: vec![imp] })));
        let mut ids = Vec::new();
        for (pan, fx) in [(-1.0f32, true), (1.0, false)] {
            let t = s.add_track(TrackKind::Audio, ChannelFormat::Mono, None);
            let cid = s.new_clip_id();
            let tr = s.track_mut(t).unwrap();
            tr.playlist_mut().unwrap().clips.push(Clip::audio(cid, "i", SourceId(900), 0, 0, 48_000));
            tr.mixer.pan = vec![pan];
            if fx {
                tr.mixer.inserts[0] = Some(Insert::new("maximizer"));
            }
            ids.push(t);
        }
        let argmax = |c: &[f32]| c.iter().enumerate().max_by(|a, b| a.1.abs().total_cmp(&b.1.abs())).map(|(i, _)| i).unwrap();
        let lat = soundcraft_dsp::create("maximizer").unwrap().latency();
        assert!(lat > 0, "test needs a latent plugin");
        let out = render_range(&s, Range::new(0, 8000), 512);
        assert_eq!(argmax(&out[0]), argmax(&out[1]), "PDC should align L and R");
        s.edit.delay_compensation = false;
        let out = render_range(&s, Range::new(0, 8000), 512);
        assert_ne!(argmax(&out[0]), argmax(&out[1]));
    }

    #[test]
    fn input_monitoring_replaces_clips() {
        let (mut s, t) = session_with_dc(0.5, 4800);
        let mut eng = MixEngine::new(48_000.0, 512);
        eng.input = vec![vec![0.25; 512]];
        let mut out = vec![vec![0.0; 512]; 2];
        // Not monitored: the clip plays.
        eng.render(&s, 0, 512, &mut out);
        assert!((out[0][100] - 0.5 * std::f32::consts::FRAC_1_SQRT_2).abs() < 1e-3);
        // Monitored: the input replaces it, and it still sounds when stopped.
        s.track_mut(t).unwrap().mixer.input_monitor = true;
        eng.render(&s, 512, 512, &mut out);
        assert!((out[0][100] - 0.25 * std::f32::consts::FRAC_1_SQRT_2).abs() < 1e-3, "{}", out[0][100]);
        eng.monitor_only = true;
        eng.render(&s, 1024, 512, &mut out);
        assert!(out[0][100] > 0.1);
        s.track_mut(t).unwrap().mixer.input_monitor = false;
        eng.render(&s, 1024, 512, &mut out);
        assert!(out[0][100].abs() < 1e-6, "stopped, unmonitored: silence");
    }

    #[test]
    fn empty_session_is_silent() {
        let s = Session::default();
        let out = render_range(&s, Range::new(0, 1000), 128);
        assert!(out.iter().all(|c| c.iter().all(|x| *x == 0.0)));
    }
}
