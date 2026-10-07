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
                        *slot = soundcraft_dsp::create(&i.plugin).map(|mut p| {
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
                    soundcraft_dsp::create(id).map(|mut p| {
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
        run_strips(&mut work, |(t, st)| process_strip(s, t, st, &busses, pos, frames, any_solo));
        self.busses = busses;
        for (t, st) in &work {
            self.route_strip(t, st, pos, frames);
        }
        for (t, st) in work {
            self.strips.insert(t.id, st);
        }
        // Phase 2: auxes in dependency order (they read busses).
        for t in order.iter().filter_map(|&i| s.tracks.get(i)).filter(live).filter(|t| is_aux(t)) {
            let Some(mut st) = self.strips.remove(&t.id) else { continue };
            if st.scratch.len() < self.max_block {
                st.scratch.resize(self.max_block, 0.0);
            }
            process_strip(s, t, &mut st, &self.busses, pos, frames, any_solo);
            self.route_strip(t, &st, pos, frames);
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
    fn route_strip(&mut self, t: &Track, strip: &Strip, pos: Samples, frames: usize) {
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
fn process_strip(s: &Session, t: &Track, strip: &mut Strip, busses: &HashMap<BusId, Vec<Vec<f32>>>, pos: Samples, frames: usize, any_solo: bool) {
    for c in strip.buf.iter_mut() {
        c.iter_mut().take(frames).for_each(|x| *x = 0.0);
    }
    match t.kind {
        TrackKind::Audio => {
            for clip in t.clips() {
                if clip.muted || !clip.is_audio() {
                    continue;
                }
                render_clip(s, clip, pos, frames, &mut strip.buf, &mut strip.scratch);
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
    for (i, ins) in t.mixer.inserts.iter().enumerate() {
        let (Some(ins), Some(Some(slot))) = (ins, strip.plugins.get_mut(i)) else { continue };
        if !ins.active || ins.bypass {
            continue;
        }
        apply_params(slot, ins, t, i, pos);
        slot.plugin.process(&mut strip.buf, frames);
        if gr == 0.0 {
            gr = slot.plugin.gain_reduction_db();
        }
    }
    strip.gr = gr;
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
        for n in &sequence.notes {
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

/// Offline render of the main mix over `range` (stereo, planar).
pub fn render_range(s: &Session, range: Range, block: usize) -> Vec<Vec<f32>> {
    let len = usize::try_from(range.len().max(0)).unwrap_or(0);
    let mut out = vec![vec![0.0f32; len]; MAIN_CHANNELS];
    let mut eng = MixEngine::new(s.sample_rate.as_f64() as f32, block);
    let b = eng.max_block();
    let mut tmp = vec![vec![0.0f32; b]; MAIN_CHANNELS];
    let mut done = 0usize;
    while done < len {
        let n = (len - done).min(b);
        eng.render(s, range.start + done as i64, n, &mut tmp);
        for (o, t) in out.iter_mut().zip(tmp.iter()) {
            if let (Some(d), Some(sr)) = (o.get_mut(done..done + n), t.get(..n)) {
                d.copy_from_slice(sr);
            }
        }
        done += n;
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
        render_clip(s, c, range.start, len, &mut out, &mut scratch);
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
    fn empty_session_is_silent() {
        let s = Session::default();
        let out = render_range(&s, Range::new(0, 1000), 128);
        assert!(out.iter().all(|c| c.iter().all(|x| *x == 0.0)));
    }
}
