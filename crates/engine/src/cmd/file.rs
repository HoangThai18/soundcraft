//! File menu commands.

use super::*;
use crate::cmd;
use serde_json::json;
use soundcraft_model::Session;
use soundcraft_time::SampleRate;

pub fn specs() -> Vec<CommandSpec> {
    vec![
        cmd!(noundo "session.new", "New...", ["File"], Some("Cmd+N"), "{name?: 'Untitled', sample_rate?: 48000, bit_depth?: 24, template?: blank|demo}", always, new_session),
        cmd!(noundo "session.open", "Open Session...", ["File"], Some("Cmd+O"), "{path}", always, |e, p| {
            let path = str_param(p, "path").ok_or_else(|| bad("session.open", "`path` required"))?.to_string();
            crate::io::open_session(e, &path)?;
            Ok(json!({"name": e.session().name, "tracks": e.session().tracks.len()}))
        }),
        cmd!(noundo "session.close", "Close Session", ["File"], Some("Cmd+Shift+W"), "{}", always, |e, _| { e.replace_session(Session::default()); e.path = None; Ok(json!({})) }),
        cmd!(noundo "session.save", "Save Session", ["File"], Some("Cmd+S"), "{path?}", always, |e, p| {
            let path = str_param(p, "path").map(str::to_string).or_else(|| e.path.clone()).ok_or_else(|| bad("session.save", "no path yet: pass `path` (Save As)"))?;
            let written = crate::io::save_session(e, &path)?;
            Ok(json!({"path": path, "audio_files_written": written}))
        }),
        cmd!(noundo "session.save_as", "Save Session As...", ["File"], Some("Cmd+Shift+S"), "{path}", always, |e, p| {
            let path = str_param(p, "path").ok_or_else(|| bad("session.save_as", "`path` required"))?.to_string();
            let written = crate::io::save_session(e, &path)?;
            Ok(json!({"path": path, "audio_files_written": written}))
        }),
        cmd!(noundo "session.save_copy", "Save Session Copy In...", ["File"], None, "{path}", always, |e, p| {
            let path = str_param(p, "path").ok_or_else(|| bad("session.save_copy", "`path` required"))?.to_string();
            let keep = e.path.clone();
            let written = crate::io::save_session(e, &path)?;
            e.path = keep;
            Ok(json!({"path": path, "audio_files_written": written}))
        }),
        cmd!(noundo "session.save_template", "Save As Template...", ["File"], None, "{path}", always, |e, p| {
            let path = str_param(p, "path").ok_or_else(|| bad("session.save_template", "`path` required"))?.to_string();
            let mut s = e.session().clone();
            for t in &mut s.tracks { for pl in &mut t.playlists { pl.clips.clear() } }
            s.sources.clear();
            let text = s.to_json().map_err(|err| EngineError::Io(err.to_string()))?;
            std::fs::write(&path, text).map_err(|err| EngineError::Io(format!("{path}: {err}")))?;
            Ok(json!({"path": path}))
        }),
        cmd!(noundo "session.revert", "Revert Session to Saved...", ["File"], None, "{}", always, |e, _| {
            let path = e.path.clone().ok_or_else(|| bad("session.revert", "the session has not been saved"))?;
            crate::io::open_session(e, &path)?;
            Ok(json!({}))
        }),
        cmd!(
            "file.import_audio",
            "Audio...",
            ["File", "Import"],
            Some("Cmd+Shift+I"),
            "{path | paths: [..], track?: target track, at?: position, new_tracks?: true}",
            always,
            import_audio
        ),
        cmd!("file.import_midi", "MIDI...", ["File", "Import"], Some("Cmd+Alt+I"), "{path, at?}", always, |e, p| {
            let path = str_param(p, "path").ok_or_else(|| bad("file.import_midi", "`path` required"))?.to_string();
            let at = position_param(e, "file.import_midi", p, "at")?.unwrap_or(0);
            let bytes = std::fs::read(&path).map_err(|err| EngineError::Io(format!("{path}: {err}")))?;
            let tracks = crate::io::import_midi_bytes(e, &bytes, at, bool_or(p, "tempo_map", true))?;
            Ok(json!({"tracks": tracks}))
        }),
        cmd!(noundo "file.bounce_mix", "Bounce Mix...", ["File"], Some("Cmd+Alt+B"), "{path, format?: wav|aiff|flac, bit_depth?: 16|24|32f, start?, end?, source?: main|bus name, normalize?: false, dither?: true}", always, bounce),
        cmd!(noundo "file.export_midi", "MIDI...", ["File", "Export"], None, "{path, tracks?}", always, |e, p| {
            let path = str_param(p, "path").ok_or_else(|| bad("file.export_midi", "`path` required"))?.to_string();
            let bytes = crate::io::export_midi(e)?;
            std::fs::write(&path, &bytes).map_err(|err| EngineError::Io(format!("{path}: {err}")))?;
            Ok(json!({"path": path, "bytes": bytes.len()}))
        }),
        cmd!(noundo "file.export_session_text", "Session Info as Text...", ["File", "Export"], None, "{path?}", always, |e, p| {
            let text = crate::inspect::session_text(e);
            if let Some(path) = str_param(p, "path") {
                std::fs::write(path, &text).map_err(|err| EngineError::Io(format!("{path}: {err}")))?;
            }
            Ok(json!({"text": text}))
        }),
        cmd!(noundo "file.export_clips", "Export Clips as Files...", [], None, "{dir, clips?, format?: wav}", always, |e, p| {
            let dir = str_param(p, "dir").ok_or_else(|| bad("file.export_clips", "`dir` required"))?.to_string();
            let ids = clip_ids_param(e, p);
            let n = crate::io::export_clips(e, &ids, &dir)?;
            Ok(json!({"written": n}))
        }),
        cmd!(noundo "file.export_selected_tracks_as_session", "Selected Tracks as New Session...", ["File", "Export"], None, "{path, tracks?}", has_selection, |e, p| {
            let path = str_param(p, "path").ok_or_else(|| bad("file.export_selected_tracks_as_session", "`path` required"))?.to_string();
            let keep = tracks_required(e, "file.export_selected_tracks_as_session", p)?;
            let mut s = e.session().clone();
            s.tracks.retain(|t| keep.contains(&t.id) || t.kind == soundcraft_model::TrackKind::Master);
            let mut tmp = crate::Engine::new(s);
            crate::io::save_session(&mut tmp, &path)?;
            Ok(json!({"path": path}))
        }),
        cmd!(query "file.get_info", "Get Info...", ["File"], None, "{}", always, |e, _| Ok(crate::inspect::session(e, false))),
        cmd!(noundo "app.quit", "Quit", [], Some("Cmd+Q"), "{}", always, |_, _| Ok(json!({"quit": true}))),
    ]
}

fn new_session(e: &mut Engine, p: &Value) -> Result<Value> {
    let name = str_param(p, "name").unwrap_or("Untitled").to_string();
    let sr =
        SampleRate::new(i64_or(p, "sample_rate", 48_000).clamp(0, i64::from(u32::MAX)) as u32).map_err(|err| bad("session.new", err.to_string()))?;
    let mut s = if str_param(p, "template") == Some("demo") { crate::demo::demo_session() } else { Session::new(name.clone(), sr) };
    s.name = name;
    if let Some(b) = p.get("bit_depth") {
        s.bit_depth = match b.to_string().trim_matches('"') {
            "16" => soundcraft_model::BitDepthSetting::Int16,
            "32" | "32f" => soundcraft_model::BitDepthSetting::Float32,
            _ => soundcraft_model::BitDepthSetting::Int24,
        };
    }
    e.replace_session(s);
    e.path = None;
    Ok(json!({"name": e.session().name, "sample_rate": e.session().sample_rate.hz()}))
}

fn import_audio(e: &mut Engine, p: &Value) -> Result<Value> {
    let mut paths: Vec<String> =
        p.get("paths").and_then(Value::as_array).map(|a| a.iter().filter_map(Value::as_str).map(str::to_string).collect()).unwrap_or_default();
    if let Some(one) = str_param(p, "path") {
        paths.push(one.to_string());
    }
    if paths.is_empty() {
        return Err(bad("file.import_audio", "`path` or `paths` required"));
    }
    let at = position_param(e, "file.import_audio", p, "at")?.unwrap_or(0).max(0);
    let target = track_param(e, "file.import_audio", p, "track")?;
    let mut out = Vec::new();
    for path in &paths {
        let bytes = std::fs::read(path).map_err(|err| EngineError::Io(format!("{path}: {err}")))?;
        let name = std::path::Path::new(path).file_name().and_then(|n| n.to_str()).unwrap_or("Audio").to_string();
        let r = crate::io::import_audio_bytes(e, &name, &bytes, Some(path.as_str()), target, at)?;
        out.push(r);
    }
    Ok(json!({"imported": out}))
}

fn bounce(e: &mut Engine, p: &Value) -> Result<Value> {
    let path = str_param(p, "path").ok_or_else(|| bad("file.bounce_mix", "`path` required"))?.to_string();
    let r = range_param(e, "file.bounce_mix", p)?;
    let r = if r.is_empty() { soundcraft_time::Range::new(0, e.session().content_end().max(1)) } else { r };
    let ext = std::path::Path::new(&path).extension().and_then(|x| x.to_str()).unwrap_or("wav").to_ascii_lowercase();
    let format = match str_param(p, "format").unwrap_or(ext.as_str()) {
        "aif" | "aiff" => soundcraft_audio_io::FileFormat::Aiff,
        "flac" => soundcraft_audio_io::FileFormat::Flac,
        _ => soundcraft_audio_io::FileFormat::Wav,
    };
    let bit_depth = match p.get("bit_depth").map(|b| b.to_string().trim_matches('"').to_string()).as_deref() {
        Some("16") => soundcraft_audio_io::BitDepth::Int16,
        Some("32") | Some("32f") => soundcraft_audio_io::BitDepth::Float32,
        _ => soundcraft_audio_io::BitDepth::Int24,
    };
    let opts = soundcraft_audio_io::EncodeOptions { format, bit_depth, dither: bool_or(p, "dither", true), bwf: None };
    let normalize = bool_or(p, "normalize", false);
    let (bytes, stats) = crate::io::bounce_bytes(e, r, &opts, normalize)?;
    std::fs::write(&path, &bytes).map_err(|err| EngineError::Io(format!("{path}: {err}")))?;
    Ok(json!({"path": path, "bytes": bytes.len(), "seconds": e.session().sample_rate.seconds(r.len()), "peak_db": stats.0, "lufs": stats.1}))
}
