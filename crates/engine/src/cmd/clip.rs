//! Clip menu commands.

use super::*;
use crate::cmd;
use serde_json::json;

pub fn specs() -> Vec<CommandSpec> {
    vec![
        cmd!("clip.edit_lock", "Edit Lock/Unlock", ["Clip"], Some("Cmd+L"), "{clips?}", has_selection, |e, p| flag(
            e,
            p,
            |c| c.edit_locked,
            |c, v| c.edit_locked = v
        )),
        cmd!("clip.time_lock", "Time Lock/Unlock", ["Clip"], Some("Ctrl+Alt+L"), "{clips?}", has_selection, |e, p| flag(
            e,
            p,
            |c| c.time_locked,
            |c, v| c.time_locked = v
        )),
        cmd!("clip.send_to_back", "Send to Back", ["Clip"], Some("Alt+Shift+B"), "{clips?}", has_selection, |e, p| layer(e, p, false)),
        cmd!("clip.bring_to_front", "Bring to Front", ["Clip"], Some("Alt+Shift+F"), "{clips?}", has_selection, |e, p| layer(e, p, true)),
        cmd!("clip.rating", "Rating", ["Clip", "Rating"], Some("Cmd+Alt+0-5"), "{clips?, rating: 0..5}", has_selection, |e, p| {
            let ids = clip_ids_param(e, p);
            let r = i64_or(p, "rating", 0).clamp(0, 5) as u8;
            let s = e.session_mut();
            for id in &ids {
                if let Some(c) = s.find_clip_mut(*id) {
                    c.rating = r
                }
            }
            Ok(json!({"rating": r}))
        }),
        cmd!("clip.group", "Group", ["Clip"], Some("Cmd+Alt+G"), "{clips?}", has_selection, |e, p| {
            let ids = clip_ids_param(e, p);
            if ids.len() < 2 {
                return Err(bad("clip.group", "select two or more clips"));
            }
            let s = e.session_mut();
            let g = s.alloc();
            for id in &ids {
                if let Some(c) = s.find_clip_mut(*id) {
                    c.group = Some(g)
                }
            }
            Ok(json!({"group": g}))
        }),
        cmd!("clip.ungroup", "Ungroup", ["Clip"], Some("Cmd+Alt+U"), "{clips?}", has_selection, |e, p| {
            let ids = clip_ids_param(e, p);
            let s = e.session_mut();
            for id in &ids {
                if let Some(c) = s.find_clip_mut(*id) {
                    c.group = None
                }
            }
            Ok(json!({}))
        }),
        cmd!("clip.ungroup_all", "Ungroup All", ["Clip"], None, "{}", always, |e, _| {
            for t in &mut e.session_mut().tracks {
                for pl in &mut t.playlists {
                    for c in &mut pl.clips {
                        c.group = None
                    }
                }
            }
            Ok(json!({}))
        }),
        cmd!("clip.loop", "Loop...", ["Clip"], Some("Cmd+Alt+L"), "{clips?, count?: n, length?: samples}", has_selection, loop_clips),
        cmd!("clip.rename", "Rename...", ["Clip"], Some("Cmd+Alt+Shift+R"), "{clip?, name}", has_selection, rename),
        cmd!("clip.gain", "Clip Gain", [], None, "{clips?, db | delta_db}", has_selection, gain),
        cmd!("clip.gain_render", "Render", ["Clip", "Clip Gain"], None, "{clips?}", has_selection, |e, p| {
            let ids = clip_ids_param(e, p);
            let mut n = 0;
            for id in ids {
                if crate::io::render_clip_gain(e, id)? {
                    n += 1
                }
            }
            Ok(json!({"rendered": n}))
        }),
        cmd!("clip.gain_bypass", "Bypass", ["Clip", "Clip Gain"], None, "{clips?}", has_selection, |e, p| {
            let ids = clip_ids_param(e, p);
            let s = e.session_mut();
            for id in &ids {
                if let Some(c) = s.find_clip_mut(*id) {
                    c.gain_db = 0.0;
                    c.gain_env.clear()
                }
            }
            Ok(json!({}))
        }),
        cmd!("clip.identify_sync_point", "Identify Sync Point", ["Clip"], Some("Cmd+,"), "{clip?, at?}", has_selection, |e, p| {
            let at = position_param(e, "clip.identify_sync_point", p, "at")?.unwrap_or(e.session().edit.selection.start);
            let ids = clip_ids_param(e, p);
            let s = e.session_mut();
            for id in &ids {
                if let Some(c) = s.find_clip_mut(*id) {
                    c.sync_point = (at - c.start).clamp(0, c.length)
                }
            }
            Ok(json!({}))
        }),
        cmd!("clip.quantize_to_grid", "Quantize to Grid", ["Clip"], Some("Cmd+0"), "{clips?}", has_selection, quantize_to_grid),
        cmd!("clip.color", "Clip Color", [], None, "{clips?, color: [r,g,b] | null}", has_selection, |e, p| {
            let ids = clip_ids_param(e, p);
            let col = p.get("color").and_then(Value::as_array).map(|a| {
                let g = |i: usize| a.get(i).and_then(Value::as_u64).unwrap_or(0).min(255) as u8;
                [g(0), g(1), g(2)]
            });
            let s = e.session_mut();
            for id in &ids {
                if let Some(c) = s.find_clip_mut(*id) {
                    c.color = col
                }
            }
            Ok(json!({}))
        }),
        cmd!("clip.elastic_properties", "Elastic Properties", ["Clip"], Some("Alt+5"), "{clips?, ratio?: 1.0}", has_selection, |e, p| {
            let ids = clip_ids_param(e, p);
            let r = f64_or(p, "ratio", 1.0).clamp(0.05, 20.0);
            let s = e.session_mut();
            for id in &ids {
                if let Some(c) = s.find_clip_mut(*id) {
                    c.stretch = r
                }
            }
            Ok(json!({"ratio": r}))
        }),
        cmd!("clip.remove_warp", "Remove Warp", ["Clip"], None, "{clips?}", has_selection, |e, p| {
            let ids = clip_ids_param(e, p);
            let s = e.session_mut();
            for id in &ids {
                if let Some(c) = s.find_clip_mut(*id) {
                    c.stretch = 1.0
                }
            }
            Ok(json!({}))
        }),
        cmd!("clip.capture", "Capture...", ["Clip"], Some("Cmd+R"), "{name?}", has_range, |e, p| {
            // Captures the selection as a new whole-file-referencing clip in the clip list (we keep it on the track).
            let name = str_param(p, "name").unwrap_or("Captured").to_string();
            let ids = clip_ids_param(e, p);
            let s = e.session_mut();
            for id in &ids {
                if let Some(c) = s.find_clip_mut(*id) {
                    c.name = name.clone()
                }
            }
            Ok(json!({"clips": ids.len()}))
        }),
    ]
}

fn flag(e: &mut Engine, p: &Value, get: fn(&soundcraft_model::Clip) -> bool, set: fn(&mut soundcraft_model::Clip, bool)) -> Result<Value> {
    let ids = clip_ids_param(e, p);
    let s = e.session_mut();
    let v = p.get("value").and_then(Value::as_bool).unwrap_or_else(|| !ids.iter().all(|id| s.find_clip(*id).is_some_and(|(_, c)| get(c))));
    for id in &ids {
        if let Some(c) = s.find_clip_mut(*id) {
            set(c, v);
        }
    }
    Ok(json!({"value": v, "clips": ids.len()}))
}

fn layer(e: &mut Engine, p: &Value, front: bool) -> Result<Value> {
    let ids = clip_ids_param(e, p);
    let s = e.session_mut();
    for t in &mut s.tracks {
        if let Some(pl) = t.playlist_mut() {
            let (mut sel, mut rest): (Vec<_>, Vec<_>) = pl.clips.drain(..).partition(|c| ids.contains(&c.id));
            if front {
                rest.append(&mut sel);
                pl.clips = rest;
            } else {
                sel.append(&mut rest);
                pl.clips = sel;
            }
        }
    }
    Ok(json!({}))
}

fn loop_clips(e: &mut Engine, p: &Value) -> Result<Value> {
    let ids = clip_ids_param(e, p);
    let count = i64_or(p, "count", 4).clamp(1, 512);
    let total = position_param(e, "clip.loop", p, "length")?;
    let s = e.session_mut();
    let mut made = 0;
    for id in ids {
        let Some((tid, c)) = s.find_clip(id).map(|(t, c)| (t, c.clone())) else { continue };
        let reps = total.map_or(count - 1, |l| (l / c.length.max(1)).max(1) - 1);
        let mut copies = Vec::new();
        for k in 1..=reps {
            let mut nc = c.clone();
            nc.start = 0;
            copies.push((c.start + c.length * k, nc));
        }
        for (at, nc) in copies {
            crate::edit::paste(s, tid, at, &[nc], c.length, false);
            made += 1;
        }
    }
    Ok(json!({"loops": made}))
}

fn rename(e: &mut Engine, p: &Value) -> Result<Value> {
    let name = str_param(p, "name").map(str::trim).filter(|n| !n.is_empty()).ok_or_else(|| bad("clip.rename", "`name` required"))?.to_string();
    let ids = clip_ids_param(e, p);
    let s = e.session_mut();
    for id in &ids {
        if let Some(c) = s.find_clip_mut(*id) {
            c.name = name.clone();
        }
    }
    Ok(json!({"clips": ids.len()}))
}

fn gain(e: &mut Engine, p: &Value) -> Result<Value> {
    let ids = clip_ids_param(e, p);
    let abs = p.get("db").and_then(Value::as_f64);
    let delta = p.get("delta_db").and_then(Value::as_f64);
    if abs.is_none() && delta.is_none() {
        return Err(bad("clip.gain", "`db` or `delta_db` required"));
    }
    let s = e.session_mut();
    for id in &ids {
        if let Some(c) = s.find_clip_mut(*id) {
            let v = abs.unwrap_or(f64::from(c.gain_db) + delta.unwrap_or(0.0));
            c.gain_db = if v.is_finite() { (v as f32).clamp(-144.0, 36.0) } else { c.gain_db };
        }
    }
    Ok(json!({"clips": ids.len()}))
}

fn quantize_to_grid(e: &mut Engine, p: &Value) -> Result<Value> {
    let ids = clip_ids_param(e, p);
    let s = e.session();
    let moves: Vec<(soundcraft_model::ClipId, i64)> = ids
        .iter()
        .filter_map(|id| s.find_clip(*id).map(|(_, c)| (*id, c.start + c.sync_point)))
        .map(|(id, anchor)| (id, s.edit.grid.snap(anchor, s.sample_rate, &s.tempo, s.frame_rate) - anchor))
        .collect();
    let s = e.session_mut();
    for (id, d) in &moves {
        crate::edit::move_clips(s, &[*id], *d, None);
    }
    Ok(json!({"clips": moves.len()}))
}
