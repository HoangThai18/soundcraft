//! The Mix window: one channel strip per track.

use crate::theme::{Tokens, bold, regular, rgb};
use crate::widgets::{db_text, fader, meter, pan_knob, pan_text, rec_toggle, selector_box, text_toggle};
use crate::{SoundApp, panels};
use egui::{Align2, Color32, CornerRadius, Rect, Sense, Stroke, StrokeKind, Ui, pos2, vec2};
use serde_json::json;
use soundcraft_model::{Route, Track, TrackId, TrackKind};

pub fn strip_width(narrow: bool) -> f32 {
    if narrow { 64.0 } else { 98.0 }
}

pub fn show(app: &mut SoundApp, ui: &mut Ui) {
    let t = Tokens::DARK;
    if app.ui.show_tracks_list {
        egui::Panel::left("mix_tracks_list")
            .exact_size(168.0)
            .frame(egui::Frame::NONE.fill(t.panel_bg))
            .show(ui, |ui| panels::tracks_and_groups(app, ui));
    }
    egui::CentralPanel::default().frame(egui::Frame::NONE.fill(t.window_bg)).show(ui, |ui| {
        let ids: Vec<TrackId> = app.engine.session().tracks.iter().filter(|x| !x.hidden).map(|x| x.id).collect();
        egui::ScrollArea::horizontal().auto_shrink([false, false]).show(ui, |ui| {
            ui.horizontal_top(|ui| {
                ui.spacing_mut().item_spacing.x = 1.0;
                for id in ids {
                    strip(app, ui, id);
                }
            });
        });
    });
}

fn section_label(ui: &Ui, r: Rect, text: &str) {
    ui.painter().text(pos2(r.center().x, r.min.y + 7.0), Align2::CENTER_CENTER, text, bold(10.0), Color32::from_rgb(200, 200, 200));
}

fn route_name(app: &SoundApp, r: &Route) -> String {
    match r {
        Route::None => "no output".into(),
        Route::Main => "Out 1-2".into(),
        Route::Bus(b) => app.engine.session().bus(*b).map_or_else(|| "bus?".into(), |b| b.name.clone()),
        Route::Hardware(h) => h.clone(),
    }
}

fn strip(app: &mut SoundApp, ui: &mut Ui, id: TrackId) {
    let t = Tokens::DARK;
    let Some(track) = app.engine.session().track(id).cloned() else { return };
    let narrow = app.ui.narrow_mix;
    let w = strip_width(narrow);
    let h = ui.available_height().max(560.0);
    let (r, _) = ui.allocate_exact_size(vec2(w, h), Sense::hover());
    let selected = app.engine.session().edit.selected_tracks.contains(&id);
    ui.painter().rect_filled(r, 0.0, if selected { Color32::from_rgb(54, 58, 64) } else { t.strip_bg });
    let mut y = r.min.y + 4.0;
    let inner_w = w - 8.0;
    let x0 = r.min.x + 4.0;
    let views = app.ui.mix_views.clone();
    let has = |v: &str| views.iter().any(|x| x == v);
    let all = has("all");
    for (key, title, off, is_send) in [
        ("inserts_ae", "INSERTS A-E", 0usize, false),
        ("inserts_fj", "INSERTS F-J", 5, false),
        ("sends_ae", "SENDS A-E", 0, true),
        ("sends_fj", "SENDS F-J", 5, true),
    ] {
        if !(all || has(key)) {
            continue;
        }
        let sec = Rect::from_min_size(pos2(x0, y), vec2(inner_w, 16.0 + 5.0 * 17.0));
        ui.painter().rect_filled(sec, 2.0, t.strip_section);
        section_label(ui, sec, title);
        for k in 0..5 {
            let sr = Rect::from_min_size(pos2(x0 + 2.0, sec.min.y + 15.0 + k as f32 * 17.0), vec2(inner_w - 4.0, 15.0));
            if is_send {
                send_slot(app, ui, &track, off + k, sr);
            } else {
                insert_slot(app, ui, &track, off + k, sr);
            }
        }
        y = sec.max.y + 4.0;
    }
    if all || has("eq_curve") {
        let sec = Rect::from_min_size(pos2(x0, y), vec2(inner_w, 40.0));
        ui.painter().rect_filled(sec, 2.0, Color32::from_rgb(14, 18, 22));
        let eq = track.mixer.inserts.iter().flatten().find(|i| i.plugin == "eq_7band" || i.plugin == "eq_1band");
        if let Some(ins) = eq {
            let n = 40;
            let freqs: Vec<f32> = (0..n).map(|i| 20.0 * 1000f32.powf(i as f32 / (n - 1) as f32)).collect();
            let params: Vec<(&str, f32)> = ins.params.iter().map(|(k, v)| (k.as_str(), *v)).collect();
            let resp = if ins.plugin == "eq_7band" {
                soundcraft_dsp::eq7_response(&params, &freqs, 48_000.0)
            } else {
                soundcraft_dsp::eq1_response(&params, &freqs, 48_000.0)
            };
            let pts: Vec<egui::Pos2> = resp
                .iter()
                .enumerate()
                .map(|(i, db)| {
                    pos2(sec.min.x + sec.width() * i as f32 / (n - 1) as f32, sec.center().y - db.clamp(-18.0, 18.0) / 18.0 * sec.height() * 0.45)
                })
                .collect();
            ui.painter().add(egui::Shape::line(pts, Stroke::new(1.5, t.counter_text)));
        } else {
            ui.painter().line_segment(
                [pos2(sec.min.x + 2.0, sec.center().y), pos2(sec.max.x - 2.0, sec.center().y)],
                Stroke::new(1.0, Color32::from_rgb(60, 70, 80)),
            );
        }
        y = sec.max.y + 4.0;
    }
    if all || has("comments") {
        let sec = Rect::from_min_size(pos2(x0, y), vec2(inner_w, 44.0));
        let mut c = track.comments.clone();
        let mut child = ui.new_child(egui::UiBuilder::new().max_rect(sec));
        if child.add(egui::TextEdit::multiline(&mut c).desired_width(inner_w).desired_rows(2).hint_text("comments").font(regular(10.0))).changed() {
            let _ = app.engine.execute_merged("track.comments", &json!({"track": id.0, "comments": c}), &format!("comments:{}", id.0));
        }
        y = sec.max.y + 4.0;
    }
    if all || has("io") {
        let sec = Rect::from_min_size(pos2(x0, y), vec2(inner_w, 16.0 + 2.0 * 18.0 + 30.0));
        ui.painter().rect_filled(sec, 2.0, t.strip_section);
        section_label(ui, sec, "I / O");
        let in_r = Rect::from_min_size(pos2(x0 + 2.0, sec.min.y + 15.0), vec2(inner_w - 4.0, 16.0));
        let out_r = Rect::from_min_size(pos2(x0 + 2.0, sec.min.y + 33.0), vec2(inner_w - 4.0, 16.0));
        let input =
            if track.kind.has_playlist() && track.mixer.input == Route::None { "In 1".to_string() } else { route_name(app, &track.mixer.input) };
        let mut c = ui.new_child(egui::UiBuilder::new().max_rect(in_r));
        let resp = selector_box(&mut c, in_r.width(), in_r.height(), &input, t.text);
        egui::Popup::menu(&resp).show(|ui| route_menu(app, ui, id, true));
        let mut c = ui.new_child(egui::UiBuilder::new().max_rect(out_r));
        let resp = selector_box(&mut c, out_r.width(), out_r.height(), &route_name(app, &track.mixer.output), t.text);
        egui::Popup::menu(&resp).show(|ui| route_menu(app, ui, id, false));
        // Automation mode.
        let am_r = Rect::from_min_size(pos2(x0 + 2.0, sec.min.y + 51.0), vec2(inner_w - 4.0, 16.0));
        ui.painter().text(pos2(am_r.center().x, am_r.min.y - 1.0), Align2::CENTER_BOTTOM, "", regular(9.0), t.text_dim);
        let mut c = ui.new_child(egui::UiBuilder::new().max_rect(am_r));
        let mode = track.mixer.automation_mode;
        let col = if mode == soundcraft_model::AutomationMode::Read {
            t.auto_read
        } else if mode == soundcraft_model::AutomationMode::Off {
            t.text_dim
        } else {
            t.auto_write
        };
        let resp = selector_box(&mut c, am_r.width(), am_r.height(), &format!("auto {}", mode.label()), col);
        egui::Popup::menu(&resp).show(|ui| {
            for m in soundcraft_model::AutomationMode::ALL {
                if ui.selectable_label(m == mode, m.label()).clicked() {
                    let _ = app.run("mix.automation_mode", json!({"track": id.0, "mode": m.label()}));
                }
            }
        });
        y = sec.max.y + 4.0;
    }
    // Group selector.
    let gname = app.engine.session().groups.iter().find(|g| g.members.contains(&id)).map_or("no group".to_string(), |g| g.name.clone());
    let gr = Rect::from_min_size(pos2(x0 + 2.0, y), vec2(inner_w - 4.0, 16.0));
    let mut c = ui.new_child(egui::UiBuilder::new().max_rect(gr));
    let _ = selector_box(&mut c, gr.width(), gr.height(), &gname, t.text_dim);
    y += 22.0;
    // Pan.
    if !track.mixer.pan.is_empty() && track.kind != TrackKind::Vca {
        let n = track.mixer.pan.len().min(2);
        let ks = if n == 2 { (inner_w / 2.0 - 6.0).min(32.0) } else { 34.0 };
        for i in 0..n {
            let cx = if n == 2 { x0 + inner_w * (0.25 + 0.5 * i as f32) } else { x0 + inner_w * 0.5 };
            let kr = Rect::from_center_size(pos2(cx, y + ks * 0.5), vec2(ks, ks));
            let mut c = ui.new_child(egui::UiBuilder::new().max_rect(kr));
            let mut v = track.mixer.pan.get(i).copied().unwrap_or(0.0);
            let before = v;
            let resp = pan_knob(&mut c, ks, &mut v, "Pan (drag; double-click centres)");
            if (v - before).abs() > f32::EPSILON {
                let _ = app.engine.execute_merged("mix.pan", &json!({"track": id.0, "pan": v, "index": i}), &format!("pan:{}:{i}", id.0));
            }
            let _ = resp;
            let pr = Rect::from_center_size(pos2(cx, y + ks + 8.0), vec2(ks + 8.0, 13.0));
            ui.painter().rect_filled(pr, 1.0, t.counter_bg);
            ui.painter().text(pr.center(), Align2::CENTER_CENTER, pan_text(v), regular(10.0), t.counter_text);
        }
        y += ks + 20.0;
    }
    // Rec / Input / Solo / Mute.
    let bw = (inner_w - 6.0) / 2.0;
    let mut row = ui.new_child(
        egui::UiBuilder::new().max_rect(Rect::from_min_size(pos2(x0, y), vec2(inner_w, 20.0))).layout(egui::Layout::left_to_right(egui::Align::Min)),
    );
    row.spacing_mut().item_spacing.x = 6.0;
    if track.kind.has_playlist() {
        if text_toggle(&mut row, vec2(bw, 18.0), "I", track.mixer.input_monitor, t.input, "Input monitoring").clicked() {
            let _ = app.run("mix.input_monitor", json!({"track": id.0}));
        }
        if rec_toggle(&mut row, vec2(bw, 18.0), track.mixer.record_arm, "Record enable").clicked() {
            let _ = app.run("mix.record_arm", json!({"track": id.0}));
        }
    }
    y += 22.0;
    let mut row = ui.new_child(
        egui::UiBuilder::new().max_rect(Rect::from_min_size(pos2(x0, y), vec2(inner_w, 20.0))).layout(egui::Layout::left_to_right(egui::Align::Min)),
    );
    row.spacing_mut().item_spacing.x = 6.0;
    if track.kind != TrackKind::Master {
        if text_toggle(&mut row, vec2(bw, 18.0), "S", track.mixer.solo, t.solo, "Solo").clicked() {
            let _ = app.run("mix.solo", json!({"track": id.0}));
        }
        if text_toggle(&mut row, vec2(bw, 18.0), "M", track.mixer.mute, t.mute, "Mute").clicked() {
            let _ = app.run("mix.mute", json!({"track": id.0}));
        }
    }
    y += 26.0;
    // Fader + meter.
    let bottom_h = 64.0;
    let fh = (r.max.y - bottom_h - y).max(120.0);
    let fr = Rect::from_min_size(pos2(x0, y), vec2(inner_w * 0.6, fh));
    if let Some(db) = fader(ui, fr, track.mixer.volume_db, ui.id().with(("fader", id.0))) {
        app.gesture = Some(crate::Gesture::Fader { track: id, start_db: track.mixer.volume_db });
        let _ = app.engine.execute_merged("mix.volume", &json!({"track": id.0, "db": db}), &format!("fader:{}", id.0));
    }
    let md = app.meters.get(&id).copied().unwrap_or_default();
    let mr = Rect::from_min_max(pos2(fr.max.x + 4.0, fr.min.y + 6.0), pos2(x0 + inner_w - 2.0, fr.max.y - 6.0));
    let chans = if track.kind == TrackKind::Master { 2 } else { track.channels().min(2) };
    if chans >= 2 {
        let mw = mr.width() / 2.0 - 1.0;
        meter(ui, Rect::from_min_size(mr.min, vec2(mw, mr.height())), md.level[0], md.hold[0], md.clip);
        meter(ui, Rect::from_min_size(pos2(mr.min.x + mw + 2.0, mr.min.y), vec2(mw, mr.height())), md.level[1], md.hold[1], md.clip);
    } else {
        meter(ui, mr, md.level[0], md.hold[0], md.clip);
    }
    if md.gr > 0.1 {
        ui.painter().text(pos2(mr.center().x, mr.min.y - 3.0), Align2::CENTER_BOTTOM, format!("-{:.0}", md.gr), regular(8.0), t.meter_yellow);
    }
    // Volume readout and name.
    let vr = Rect::from_min_size(pos2(x0 + 4.0, r.max.y - bottom_h + 4.0), vec2(inner_w - 8.0, 15.0));
    ui.painter().rect_filled(vr, 1.0, t.counter_bg);
    ui.painter().text(vr.center(), Align2::CENTER_CENTER, db_text(track.mixer.volume_db), regular(11.0), t.counter_text);
    let kind = match track.kind {
        TrackKind::Audio => "audio",
        TrackKind::Aux => "aux",
        TrackKind::Master => "master",
        TrackKind::Midi => "midi",
        TrackKind::Instrument => "inst",
        TrackKind::Vca => "vca",
        TrackKind::Folder => "folder",
    };
    ui.painter().text(pos2(vr.center().x, vr.max.y + 9.0), Align2::CENTER_CENTER, kind, regular(9.5), t.text_dim);
    let nr = Rect::from_min_size(pos2(x0, r.max.y - 26.0), vec2(inner_w, 18.0));
    ui.painter().rect(
        nr,
        CornerRadius::same(2),
        if selected { t.name_field_sel } else { t.name_field },
        Stroke::new(1.0, Color32::BLACK),
        StrokeKind::Inside,
    );
    ui.painter().with_clip_rect(nr).text(nr.center(), Align2::CENTER_CENTER, &track.name, bold(12.0), t.text_dark);
    let nresp = ui.interact(nr, ui.id().with(("strip_name", id.0)), Sense::click());
    if nresp.clicked() {
        let _ = app.run("edit.select", json!({"tracks": [id.0]}));
    }
    if nresp.double_clicked() {
        app.dialogs.open_rename_track(id, &track.name);
    }
    if app.ui.mix_views.iter().any(|v| v == "color") {
        ui.painter().rect_filled(Rect::from_min_size(pos2(r.min.x, r.max.y - 5.0), vec2(w, 5.0)), 0.0, rgb(track.color));
    }
    ui.painter().line_segment([pos2(r.max.x, r.min.y), pos2(r.max.x, r.max.y)], Stroke::new(1.0, t.border));
}

fn insert_slot(app: &mut SoundApp, ui: &mut Ui, track: &Track, slot: usize, r: Rect) {
    let t = Tokens::DARK;
    let id = track.id;
    let ins = track.mixer.inserts.get(slot).cloned().flatten();
    let resp = ui.interact(r, ui.id().with(("ins", id.0, slot)), Sense::click());
    let fill = match &ins {
        Some(i) if i.bypass => Color32::from_rgb(70, 56, 30),
        Some(_) => Color32::from_rgb(52, 62, 80),
        None => t.slot_bg,
    };
    ui.painter().rect(
        r,
        CornerRadius::same(2),
        if resp.hovered() { fill.gamma_multiply(1.3) } else { fill },
        Stroke::new(1.0, Color32::from_rgb(16, 16, 16)),
        StrokeKind::Inside,
    );
    let label = ins.as_ref().and_then(|i| soundcraft_dsp::plugin_info(&i.plugin)).map_or("", |p| p.short_name);
    if label.is_empty() {
        ui.painter().circle_filled(pos2(r.min.x + 6.0, r.center().y), 1.5, t.text_dim);
    } else {
        ui.painter().with_clip_rect(r).text(r.center(), Align2::CENTER_CENTER, label, regular(10.5), t.text);
    }
    if resp.clicked() && ins.is_some() && !app.ui.plugin_windows.contains(&(id, slot)) {
        app.ui.plugin_windows.push((id, slot));
    }
    let menu_resp = if ins.is_none() { resp.clone() } else { resp.clone().on_hover_text("Click: open plugin · right-click: change") };
    let popup = if ins.is_none() { egui::Popup::menu(&menu_resp) } else { egui::Popup::context_menu(&menu_resp) };
    popup.show(|ui| plugin_menu(app, ui, id, slot, ins.is_some()));
}

pub fn plugin_menu(app: &mut SoundApp, ui: &mut Ui, id: TrackId, slot: usize, occupied: bool) {
    if occupied {
        if ui.button("Bypass").clicked() {
            let _ = app.run("mix.insert_bypass", json!({"track": id.0, "slot": slot}));
        }
        if ui.button("no insert").clicked() {
            let _ = app.run("mix.insert_remove", json!({"track": id.0, "slot": slot}));
        }
        ui.separator();
    }
    let mut cats: Vec<soundcraft_dsp::Category> = Vec::new();
    for p in soundcraft_dsp::plugins() {
        if !p.is_instrument && !cats.contains(&p.category) {
            cats.push(p.category);
        }
    }
    for c in cats {
        ui.menu_button(format!("{c:?}"), |ui| {
            for p in soundcraft_dsp::plugins().iter().filter(|p| p.category == c && !p.is_instrument) {
                if ui.button(p.name).clicked() {
                    let _ = app.run("mix.insert", json!({"track": id.0, "slot": slot, "plugin": p.id}));
                }
            }
        });
    }
}

fn send_slot(app: &mut SoundApp, ui: &mut Ui, track: &Track, slot: usize, r: Rect) {
    let t = Tokens::DARK;
    let id = track.id;
    let snd = track.mixer.sends.get(slot).cloned().flatten();
    let resp = ui.interact(r, ui.id().with(("snd", id.0, slot)), Sense::click_and_drag());
    let fill = if snd.is_some() { Color32::from_rgb(46, 66, 56) } else { t.slot_bg };
    ui.painter().rect(r, CornerRadius::same(2), fill, Stroke::new(1.0, Color32::from_rgb(16, 16, 16)), StrokeKind::Inside);
    match &snd {
        Some(s) => {
            let name = route_name(app, &s.target);
            // Level bar.
            let k = soundcraft_model::fader_db_to_pos(s.level_db);
            ui.painter().rect_filled(
                Rect::from_min_max(pos2(r.min.x + 1.0, r.max.y - 3.0), pos2(r.min.x + 1.0 + (r.width() - 2.0) * k, r.max.y - 1.0)),
                0.0,
                t.counter_text,
            );
            ui.painter().with_clip_rect(r).text(
                r.center() - vec2(0.0, 1.0),
                Align2::CENTER_CENTER,
                name,
                regular(10.0),
                if s.mute { t.mute } else { t.text },
            );
            if resp.dragged() {
                let db = (s.level_db.max(-60.0) - resp.drag_delta().y * 0.3).clamp(-144.0, 12.0);
                let _ =
                    app.engine.execute_merged("mix.send_level", &json!({"track": id.0, "slot": slot, "db": db}), &format!("send:{}:{slot}", id.0));
            }
            if resp.double_clicked() {
                let _ = app.run("mix.send_level", json!({"track": id.0, "slot": slot, "db": 0.0}));
            }
            let _ = resp.clone().on_hover_text(format!("Send {}: {} dB (drag to change)", (b'A' + slot as u8) as char, db_text(s.level_db)));
        }
        None => {
            ui.painter().circle_filled(pos2(r.min.x + 6.0, r.center().y), 1.5, t.text_dim);
        }
    }
    let popup = if snd.is_none() { egui::Popup::menu(&resp) } else { egui::Popup::context_menu(&resp) };
    popup.show(|ui| {
        if snd.is_some() && ui.button("no send").clicked() {
            let _ = app.run("mix.send_remove", json!({"track": id.0, "slot": slot}));
        }
        let busses: Vec<String> = app.engine.session().busses.iter().map(|b| b.name.clone()).collect();
        for b in busses {
            if ui.button(&b).clicked() {
                let _ = app.run("mix.send", json!({"track": id.0, "slot": slot, "bus": b}));
            }
        }
        ui.separator();
        if ui.button("new bus...").clicked() {
            let n = app.engine.session().busses.len() + 1;
            let _ = app.run("mix.send", json!({"track": id.0, "slot": slot, "bus": format!("Bus {n}")}));
        }
    });
}

fn route_menu(app: &mut SoundApp, ui: &mut Ui, id: TrackId, input: bool) {
    let key = if input { "input" } else { "output" };
    let cmd = if input { "track.input" } else { "track.output" };
    if ui.button(if input { "no input" } else { "no output" }).clicked() {
        let _ = app.run(cmd, json!({"track": id.0, key: "none"}));
    }
    if !input && ui.button("Out 1-2").clicked() {
        let _ = app.run(cmd, json!({"track": id.0, key: "main"}));
    }
    if input {
        for i in 1..=8 {
            if ui.button(format!("In {i}")).clicked() {
                let _ = app.run(cmd, json!({"track": id.0, key: format!("In {i}")}));
            }
        }
    }
    ui.separator();
    let busses: Vec<String> = app.engine.session().busses.iter().map(|b| b.name.clone()).collect();
    ui.menu_button("bus", |ui| {
        for b in &busses {
            if ui.button(b).clicked() {
                let _ = app.run(cmd, json!({"track": id.0, key: b}));
            }
        }
        if ui.button("new bus...").clicked() {
            let n = busses.len() + 1;
            let _ = app.run(cmd, json!({"track": id.0, key: format!("Bus {n}")}));
        }
    });
}
