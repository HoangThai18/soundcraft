//! Test-only CLAP plugins for soundcraft-clap-host: a gain effect and a "DC synth".
//!
//! - `org.soundcraft.test.gain`: stereo in/out, param 7 "Gain" (0..2, default 1), param 3 "Mode"
//!   (stepped enum A/B/C), hidden param 9, latency 3 samples.
//! - `org.soundcraft.test.synth`: instrument, stereo out, CLAP note port; outputs a constant equal
//!   to the velocity of the held note (0 when none), switching at the event's sample offset.
#![allow(clippy::missing_safety_doc, non_upper_case_globals)]

use clap_sys::audio_buffer::clap_audio_buffer;
use clap_sys::entry::clap_plugin_entry;
use clap_sys::events::*;
use clap_sys::ext::audio_ports::*;
use clap_sys::ext::latency::*;
use clap_sys::ext::log::*;
use clap_sys::ext::note_ports::*;
use clap_sys::ext::params::*;
use clap_sys::factory::plugin_factory::*;
use clap_sys::host::clap_host;
use clap_sys::id::clap_id;
use clap_sys::plugin::*;
use clap_sys::process::*;
use clap_sys::version::CLAP_VERSION;
use std::ffi::{CStr, c_char, c_void};

struct Features<const N: usize>([*const c_char; N]);
unsafe impl<const N: usize> Sync for Features<N> {}

static GAIN_FEATURES: Features<3> = Features([c"audio-effect".as_ptr(), c"stereo".as_ptr(), std::ptr::null()]);
static SYNTH_FEATURES: Features<3> = Features([c"instrument".as_ptr(), c"synthesizer".as_ptr(), std::ptr::null()]);

static GAIN_DESC: clap_plugin_descriptor = clap_plugin_descriptor {
    clap_version: CLAP_VERSION,
    id: c"org.soundcraft.test.gain".as_ptr(),
    name: c"Fixture Gain".as_ptr(),
    vendor: c"SoundCraft tests".as_ptr(),
    url: c"".as_ptr(),
    manual_url: c"".as_ptr(),
    support_url: c"".as_ptr(),
    version: c"1.0.0".as_ptr(),
    description: c"test gain".as_ptr(),
    features: GAIN_FEATURES.0.as_ptr(),
};

static SYNTH_DESC: clap_plugin_descriptor = clap_plugin_descriptor {
    clap_version: CLAP_VERSION,
    id: c"org.soundcraft.test.synth".as_ptr(),
    name: c"Fixture Synth".as_ptr(),
    vendor: c"SoundCraft tests".as_ptr(),
    url: std::ptr::null(),
    manual_url: std::ptr::null(),
    support_url: std::ptr::null(),
    version: c"1.0.0".as_ptr(),
    description: std::ptr::null(),
    features: SYNTH_FEATURES.0.as_ptr(),
};

struct Fx {
    plugin: clap_plugin,
    host: *const clap_host,
    synth: bool,
    gain: f64,
    mode: f64,
    level: f64,
    active: bool,
    processing: bool,
}

unsafe fn fx<'a>(p: *const clap_plugin) -> &'a mut Fx {
    unsafe { &mut *((*p).plugin_data as *mut Fx) }
}

unsafe extern "C" fn init(p: *const clap_plugin) -> bool {
    let f = unsafe { fx(p) };
    // Exercise the host's extension lookup + log callback.
    let host = unsafe { &*f.host };
    if let Some(ge) = host.get_extension {
        let log = unsafe { ge(f.host, CLAP_EXT_LOG.as_ptr()) } as *const clap_host_log;
        if let Some(l) = unsafe { log.as_ref() }.and_then(|l| l.log) {
            unsafe { l(f.host, CLAP_LOG_INFO, c"fixture init".as_ptr()) };
        }
        let none = unsafe { ge(f.host, c"clap.does-not-exist".as_ptr()) };
        if !none.is_null() {
            return false;
        }
    }
    true
}

unsafe extern "C" fn destroy(p: *const clap_plugin) {
    let data = unsafe { (*p).plugin_data } as *mut Fx;
    drop(unsafe { Box::from_raw(data) });
}

unsafe extern "C" fn activate(p: *const clap_plugin, sr: f64, min: u32, max: u32) -> bool {
    let f = unsafe { fx(p) };
    if f.active || !(sr > 0.0) || min == 0 || max < min {
        return false;
    }
    f.active = true;
    true
}

unsafe extern "C" fn deactivate(p: *const clap_plugin) {
    let f = unsafe { fx(p) };
    assert!(!f.processing, "deactivate while processing");
    f.active = false;
}

unsafe extern "C" fn start_processing(p: *const clap_plugin) -> bool {
    let f = unsafe { fx(p) };
    assert!(f.active, "start_processing while inactive");
    f.processing = true;
    true
}

unsafe extern "C" fn stop_processing(p: *const clap_plugin) {
    unsafe { fx(p) }.processing = false;
}

unsafe extern "C" fn reset(p: *const clap_plugin) {
    unsafe { fx(p) }.level = 0.0;
}

fn apply(f: &mut Fx, h: &clap_event_header) {
    if h.space_id != CLAP_CORE_EVENT_SPACE_ID {
        return;
    }
    match h.type_ {
        CLAP_EVENT_PARAM_VALUE => {
            let e = unsafe { &*(h as *const clap_event_header as *const clap_event_param_value) };
            match e.param_id {
                7 => f.gain = e.value,
                3 => f.mode = e.value,
                _ => {}
            }
        }
        CLAP_EVENT_NOTE_ON => {
            let e = unsafe { &*(h as *const clap_event_header as *const clap_event_note) };
            f.level = e.velocity;
        }
        CLAP_EVENT_NOTE_OFF => f.level = 0.0,
        _ => {}
    }
}

unsafe extern "C" fn process(p: *const clap_plugin, pr: *const clap_process) -> clap_process_status {
    let f = unsafe { fx(p) };
    let pr = unsafe { &*pr };
    assert!(f.processing, "process while not processing");
    let n = pr.frames_count as usize;
    let ev = unsafe { &*pr.in_events };
    let count = unsafe { (ev.size.unwrap())(pr.in_events) };
    if pr.audio_outputs_count != 1 {
        return CLAP_PROCESS_ERROR;
    }
    let out: &clap_audio_buffer = unsafe { &*pr.audio_outputs };
    let input: Option<&clap_audio_buffer> = if f.synth {
        if pr.audio_inputs_count != 0 {
            return CLAP_PROCESS_ERROR;
        }
        None
    } else {
        if pr.audio_inputs_count != 1 {
            return CLAP_PROCESS_ERROR;
        }
        Some(unsafe { &*pr.audio_inputs })
    };
    let mut next = 0u32;
    let mut last_time = 0u32;
    for i in 0..n {
        while next < count {
            let h = unsafe { &*(ev.get.unwrap())(pr.in_events, next) };
            assert!(h.time >= last_time, "events not sorted");
            assert!((h.time as usize) < n, "event time outside the block");
            if h.time as usize > i {
                break;
            }
            last_time = h.time;
            apply(f, h);
            next += 1;
        }
        for c in 0..out.channel_count as usize {
            let o = unsafe { *out.data32.add(c) };
            let v = match input {
                Some(inp) => {
                    let x = unsafe { *(*inp.data32.add(c.min(inp.channel_count as usize - 1))).add(i) };
                    x * f.gain as f32
                }
                None => f.level as f32,
            };
            unsafe { *o.add(i) = v };
        }
    }
    CLAP_PROCESS_CONTINUE
}

// ---- extensions ----

unsafe extern "C" fn params_count(p: *const clap_plugin) -> u32 {
    if unsafe { fx(p) }.synth { 0 } else { 3 }
}

fn write_name(dst: &mut [c_char], s: &CStr) {
    for (d, b) in dst.iter_mut().zip(s.to_bytes_with_nul()) {
        *d = *b as c_char;
    }
}

unsafe extern "C" fn params_get_info(_p: *const clap_plugin, index: u32, info: *mut clap_param_info) -> bool {
    let info = unsafe { &mut *info };
    let (id, name, flags, min, max, def): (clap_id, &CStr, u32, f64, f64, f64) = match index {
        0 => (7, c"Gain", CLAP_PARAM_IS_AUTOMATABLE, 0.0, 2.0, 1.0),
        1 => (3, c"Mode", CLAP_PARAM_IS_STEPPED | CLAP_PARAM_IS_ENUM, 0.0, 2.0, 0.0),
        2 => (9, c"Secret", CLAP_PARAM_IS_HIDDEN, 0.0, 1.0, 0.0),
        _ => return false,
    };
    info.id = id;
    info.flags = flags;
    info.cookie = std::ptr::null_mut();
    write_name(&mut info.name, name);
    write_name(&mut info.module, c"");
    info.min_value = min;
    info.max_value = max;
    info.default_value = def;
    true
}

unsafe extern "C" fn params_get_value(p: *const clap_plugin, id: clap_id, out: *mut f64) -> bool {
    let f = unsafe { fx(p) };
    let v = match id {
        7 => f.gain,
        3 => f.mode,
        9 => 0.0,
        _ => return false,
    };
    unsafe { *out = v };
    true
}

unsafe extern "C" fn params_value_to_text(_p: *const clap_plugin, id: clap_id, v: f64, buf: *mut c_char, cap: u32) -> bool {
    if id != 3 || cap < 2 {
        return false;
    }
    let label = [b'A', b'B', b'C'].get(v.round() as usize).copied().unwrap_or(b'?');
    unsafe {
        *buf = label as c_char;
        *buf.add(1) = 0;
    }
    true
}

unsafe extern "C" fn params_flush(p: *const clap_plugin, inp: *const clap_input_events, _out: *const clap_output_events) {
    let f = unsafe { fx(p) };
    let ev = unsafe { &*inp };
    let n = unsafe { (ev.size.unwrap())(inp) };
    for i in 0..n {
        apply(f, unsafe { &*(ev.get.unwrap())(inp, i) });
    }
}

static PARAMS: clap_plugin_params = clap_plugin_params {
    count: Some(params_count),
    get_info: Some(params_get_info),
    get_value: Some(params_get_value),
    value_to_text: Some(params_value_to_text),
    text_to_value: None,
    flush: Some(params_flush),
};

unsafe extern "C" fn ports_count(p: *const clap_plugin, is_input: bool) -> u32 {
    if is_input && unsafe { fx(p) }.synth { 0 } else { 1 }
}

unsafe extern "C" fn ports_get(p: *const clap_plugin, index: u32, is_input: bool, info: *mut clap_audio_port_info) -> bool {
    if index != 0 || (is_input && unsafe { fx(p) }.synth) {
        return false;
    }
    let info = unsafe { &mut *info };
    info.id = if is_input { 0 } else { 1 };
    write_name(&mut info.name, c"main");
    info.flags = CLAP_AUDIO_PORT_IS_MAIN;
    info.channel_count = 2;
    info.port_type = CLAP_PORT_STEREO.as_ptr();
    info.in_place_pair = clap_sys::id::CLAP_INVALID_ID;
    true
}

static PORTS: clap_plugin_audio_ports = clap_plugin_audio_ports { count: Some(ports_count), get: Some(ports_get) };

unsafe extern "C" fn latency_get(_p: *const clap_plugin) -> u32 {
    3
}

static LATENCY: clap_plugin_latency = clap_plugin_latency { get: Some(latency_get) };

unsafe extern "C" fn notes_count(p: *const clap_plugin, is_input: bool) -> u32 {
    if is_input && unsafe { fx(p) }.synth { 1 } else { 0 }
}

unsafe extern "C" fn notes_get(_p: *const clap_plugin, index: u32, is_input: bool, info: *mut clap_note_port_info) -> bool {
    if index != 0 || !is_input {
        return false;
    }
    let info = unsafe { &mut *info };
    info.id = 0;
    info.supported_dialects = CLAP_NOTE_DIALECT_CLAP | CLAP_NOTE_DIALECT_MIDI;
    info.preferred_dialect = CLAP_NOTE_DIALECT_CLAP;
    write_name(&mut info.name, c"notes");
    true
}

static NOTES: clap_plugin_note_ports = clap_plugin_note_ports { count: Some(notes_count), get: Some(notes_get) };

unsafe extern "C" fn get_extension(p: *const clap_plugin, id: *const c_char) -> *const c_void {
    let id = unsafe { CStr::from_ptr(id) };
    let synth = unsafe { fx(p) }.synth;
    if id == CLAP_EXT_PARAMS {
        &PARAMS as *const _ as *const c_void
    } else if id == CLAP_EXT_AUDIO_PORTS {
        &PORTS as *const _ as *const c_void
    } else if id == CLAP_EXT_LATENCY && !synth {
        &LATENCY as *const _ as *const c_void
    } else if id == CLAP_EXT_NOTE_PORTS && synth {
        &NOTES as *const _ as *const c_void
    } else {
        std::ptr::null()
    }
}

unsafe extern "C" fn on_main_thread(_p: *const clap_plugin) {}

// ---- factory + entry ----

unsafe extern "C" fn factory_count(_f: *const clap_plugin_factory) -> u32 {
    2
}

unsafe extern "C" fn factory_desc(_f: *const clap_plugin_factory, i: u32) -> *const clap_plugin_descriptor {
    match i {
        0 => &GAIN_DESC,
        1 => &SYNTH_DESC,
        _ => std::ptr::null(),
    }
}

unsafe extern "C" fn factory_create(_f: *const clap_plugin_factory, host: *const clap_host, id: *const c_char) -> *const clap_plugin {
    let id = unsafe { CStr::from_ptr(id) };
    let (desc, synth) = if id == c"org.soundcraft.test.gain" {
        (&GAIN_DESC as *const _, false)
    } else if id == c"org.soundcraft.test.synth" {
        (&SYNTH_DESC as *const _, true)
    } else {
        return std::ptr::null();
    };
    let b = Box::new(Fx {
        plugin: clap_plugin {
            desc,
            plugin_data: std::ptr::null_mut(),
            init: Some(init),
            destroy: Some(destroy),
            activate: Some(activate),
            deactivate: Some(deactivate),
            start_processing: Some(start_processing),
            stop_processing: Some(stop_processing),
            reset: Some(reset),
            process: Some(process),
            get_extension: Some(get_extension),
            on_main_thread: Some(on_main_thread),
        },
        host,
        synth,
        gain: 1.0,
        mode: 0.0,
        level: 0.0,
        active: false,
        processing: false,
    });
    let raw = Box::into_raw(b);
    unsafe {
        (*raw).plugin.plugin_data = raw as *mut c_void;
        &(*raw).plugin
    }
}

static FACTORY: clap_plugin_factory =
    clap_plugin_factory { get_plugin_count: Some(factory_count), get_plugin_descriptor: Some(factory_desc), create_plugin: Some(factory_create) };

unsafe extern "C" fn entry_init(_path: *const c_char) -> bool {
    true
}
unsafe extern "C" fn entry_deinit() {}
unsafe extern "C" fn entry_get_factory(id: *const c_char) -> *const c_void {
    if unsafe { CStr::from_ptr(id) } == CLAP_PLUGIN_FACTORY_ID { &FACTORY as *const _ as *const c_void } else { std::ptr::null() }
}

#[unsafe(no_mangle)]
pub static clap_entry: clap_plugin_entry =
    clap_plugin_entry { clap_version: CLAP_VERSION, init: Some(entry_init), deinit: Some(entry_deinit), get_factory: Some(entry_get_factory) };
