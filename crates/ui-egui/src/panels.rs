//! Side panels (Tracks, Groups, Clips) and floating windows.

use crate::SoundApp;
use crate::theme::{Tokens, bold, mono, regular, rgb};
use egui::{Align2, Color32, Rect, Sense, Stroke, Ui, pos2, vec2};
use serde_json::json;
use soundcraft_time::{TimeFormat, format_position};

fn panel_header(ui: &mut Ui, title: &str) {
    let t = Tokens::DARK;
    let (r, _) = ui.allocate_exact_size(vec2(ui.available_width(), 22.0), Sense::hover());
    ui.painter().rect_filled(r, 0.0, t.panel_bg2);
    ui.painter().text(pos2(r.min.x + 8.0, r.center().y), Align2::LEFT_CENTER, title, bold(11.5), t.header_text);
    ui.painter().line_segment([pos2(r.min.x, r.max.y), pos2(r.max.x, r.max.y)], Stroke::new(1.0, t.border));
}

pub fn tracks_and_groups(app: &mut SoundApp, ui: &mut Ui) {
    let t = Tokens::DARK;
    let total = ui.available_height();
    panel_header(ui, "TRACKS");
    let tracks: Vec<(u64, String, bool, [u8; 3], bool)> = {
        let s = app.engine.session();
        s.tracks.iter().map(|x| (x.id.0, x.name.clone(), !x.hidden, x.color, s.edit.selected_tracks.contains(&x.id))).collect()
    };
    egui::ScrollArea::vertical().id_salt("tracks_scroll").max_height(total * 0.62).auto_shrink([false, false]).show(ui, |ui| {
        for (id, name, shown, color, sel) in tracks {
            let (r, resp) = ui.allocate_exact_size(vec2(ui.available_width(), 17.0), Sense::click());
            if sel {
                ui.painter().rect_filled(r, 0.0, Color32::from_rgb(52, 70, 96));
            }
            let dot = Rect::from_center_size(pos2(r.min.x + 10.0, r.center().y), vec2(8.0, 8.0));
            let dresp = ui.interact(dot.expand(3.0), ui.id().with(("vis", id)), Sense::click());
            ui.painter().circle(
                dot.center(),
                3.5,
                if shown { Color32::from_rgb(200, 200, 200) } else { Color32::TRANSPARENT },
                Stroke::new(1.0, t.text_dim),
            );
            if dresp.clicked() {
                let _ = app.run("track.hide", json!({"tracks": [id], "hidden": shown}));
            }
            ui.painter().rect_filled(Rect::from_min_size(pos2(r.min.x + 20.0, r.min.y + 3.0), vec2(4.0, 11.0)), 0.0, rgb(color));
            ui.painter().text(pos2(r.min.x + 30.0, r.center().y), Align2::LEFT_CENTER, &name, regular(11.5), if shown { t.text } else { t.text_dim });
            if resp.clicked() {
                let _ = app.run("edit.select", json!({"tracks": [id]}));
            }
        }
    });
    ui.add_space(4.0);
    panel_header(ui, "GROUPS");
    let groups: Vec<(u64, char, String, bool, [u8; 3])> =
        app.engine.session().groups.iter().map(|g| (g.id.0, g.letter, g.name.clone(), g.active, g.color)).collect();
    egui::ScrollArea::vertical().id_salt("groups_scroll").auto_shrink([false, false]).show(ui, |ui| {
        let (r, _) = ui.allocate_exact_size(vec2(ui.available_width(), 17.0), Sense::hover());
        ui.painter().text(pos2(r.min.x + 30.0, r.center().y), Align2::LEFT_CENTER, "<ALL>", regular(11.5).clone(), t.text);
        for (id, letter, name, active, color) in groups {
            let (r, resp) = ui.allocate_exact_size(vec2(ui.available_width(), 17.0), Sense::click());
            if active {
                ui.painter().rect_filled(Rect::from_min_size(pos2(r.min.x + 4.0, r.min.y + 2.0), vec2(14.0, 13.0)), 1.0, rgb(color));
            }
            ui.painter().text(
                pos2(r.min.x + 11.0, r.center().y),
                Align2::CENTER_CENTER,
                letter.to_string(),
                bold(10.0),
                if active { t.text_dark } else { t.text_dim },
            );
            ui.painter().text(pos2(r.min.x + 30.0, r.center().y), Align2::LEFT_CENTER, &name, regular(11.5), t.text);
            if resp.clicked() {
                let _ = app.run("track.group_toggle", json!({"group": id}));
            }
            if resp.double_clicked() {
                let members: Vec<u64> =
                    app.engine.session().groups.iter().find(|g| g.id.0 == id).map(|g| g.members.iter().map(|m| m.0).collect()).unwrap_or_default();
                let _ = app.run("edit.select", json!({"tracks": members}));
            }
        }
    });
}

pub fn clip_list(app: &mut SoundApp, ui: &mut Ui) {
    let t = Tokens::DARK;
    panel_header(ui, "CLIPS");
    let items: Vec<(Option<u64>, String, bool, [u8; 3])> = {
        let s = app.engine.session();
        let mut v: Vec<(Option<u64>, String, bool, [u8; 3])> = Vec::new();
        // Whole files first (bold), then clips on tracks.
        for src in &s.sources {
            v.push((None, format!("{} ({}ch)", src.name, src.channels), true, [150, 150, 150]));
        }
        for tr in &s.tracks {
            for c in tr.clips() {
                v.push((Some(c.id.0), c.name.clone(), false, c.color.unwrap_or(tr.color)));
            }
        }
        v
    };
    let selected: Vec<u64> = app.engine.session().edit.selected_clips.iter().map(|c| c.0).collect();
    egui::ScrollArea::vertical().id_salt("clips_scroll").auto_shrink([false, false]).show(ui, |ui| {
        for (id, name, whole, color) in items {
            let (r, resp) = ui.allocate_exact_size(vec2(ui.available_width(), 17.0), Sense::click());
            if id.is_some_and(|i| selected.contains(&i)) {
                ui.painter().rect_filled(r, 0.0, Color32::from_rgb(52, 70, 96));
            }
            ui.painter().rect_filled(Rect::from_min_size(pos2(r.min.x + 6.0, r.min.y + 4.0), vec2(9.0, 9.0)), 1.0, rgb(color));
            ui.painter().with_clip_rect(r).text(
                pos2(r.min.x + 22.0, r.center().y),
                Align2::LEFT_CENTER,
                &name,
                if whole { bold(11.0) } else { regular(11.0) },
                t.text,
            );
            if resp.clicked()
                && let Some(i) = id
            {
                let _ = app.run("edit.select", json!({"clips": [i]}));
            }
        }
    });
}

/// Floating windows.
pub fn floating(app: &mut SoundApp, ctx: &egui::Context) {
    transport_window(app, ctx);
    memory_locations(app, ctx);
    big_counter(app, ctx);
    undo_history(app, ctx);
    plugin_windows(app, ctx);
    about(app, ctx);
    session_info(app, ctx);
}

fn transport_window(app: &mut SoundApp, ctx: &egui::Context) {
    let mut open = app.ui.show_transport;
    if !open {
        return;
    }
    egui::Window::new("Transport").open(&mut open).resizable(false).default_pos(pos2(400.0, 500.0)).show(ctx, |ui| {
        ui.horizontal(|ui| {
            for (icon, cmd) in [
                ("rtz", "transport.rtz"),
                ("rewind", "transport.rewind"),
                ("stop", "transport.stop"),
                ("play", "transport.play"),
                ("ffwd", "transport.fast_forward"),
                ("end", "transport.go_to_end"),
                ("record", "transport.record"),
            ] {
                if crate::widgets::icon_button(ui, vec2(34.0, 26.0), icon, false, cmd).clicked() {
                    let _ = app.run(cmd, json!({}));
                }
            }
        });
        let s = app.engine.session();
        let txt = format_position(app.position(), s.edit.main_counter, s.sample_rate, &s.tempo, s.frame_rate, s.timecode_start);
        ui.label(egui::RichText::new(txt).font(mono(20.0)).color(Tokens::DARK.counter_text));
    });
    app.ui.show_transport = open;
}

fn big_counter(app: &mut SoundApp, ctx: &egui::Context) {
    let mut open = app.ui.show_big_counter;
    if !open {
        return;
    }
    egui::Window::new("Big Counter").open(&mut open).default_size(vec2(520.0, 110.0)).show(ctx, |ui| {
        let s = app.engine.session();
        let txt = format_position(app.position(), s.edit.main_counter, s.sample_rate, &s.tempo, s.frame_rate, s.timecode_start);
        let r = ui.available_rect_before_wrap();
        ui.painter().rect_filled(r, 4.0, Color32::BLACK);
        ui.painter().text(r.center(), Align2::CENTER_CENTER, txt, mono((r.height() * 0.6).clamp(20.0, 120.0)), Tokens::DARK.counter_text);
    });
    app.ui.show_big_counter = open;
}

fn memory_locations(app: &mut SoundApp, ctx: &egui::Context) {
    let mut open = app.ui.show_memory_locations;
    if !open {
        return;
    }
    egui::Window::new("Memory Locations").open(&mut open).default_size(vec2(420.0, 260.0)).show(ctx, |ui| {
        ui.horizontal(|ui| {
            if ui.button("+ New").clicked() {
                let _ = app.run("markers.add", json!({}));
            }
        });
        let markers: Vec<(u32, String, i64, String)> = {
            let s = app.engine.session();
            s.markers
                .iter()
                .map(|m| {
                    (m.number, m.name.clone(), m.start, format_position(m.start, TimeFormat::BarsBeats, s.sample_rate, &s.tempo, s.frame_rate, 0))
                })
                .collect()
        };
        egui::Grid::new("mem_grid").striped(true).num_columns(4).show(ui, |ui| {
            ui.label(egui::RichText::new("#").strong());
            ui.label(egui::RichText::new("Name").strong());
            ui.label(egui::RichText::new("Location").strong());
            ui.label("");
            ui.end_row();
            for (n, name, _, loc) in markers {
                if ui.button(n.to_string()).clicked() {
                    let _ = app.run("markers.recall", json!({"number": n}));
                }
                ui.label(name);
                ui.label(loc);
                if ui.small_button("✕").clicked() {
                    let _ = app.run("markers.delete", json!({"number": n}));
                }
                ui.end_row();
            }
        });
    });
    app.ui.show_memory_locations = open;
}

fn undo_history(app: &mut SoundApp, ctx: &egui::Context) {
    let mut open = app.ui.show_undo_history;
    if !open {
        return;
    }
    egui::Window::new("Undo History").open(&mut open).default_size(vec2(260.0, 300.0)).show(ctx, |ui| {
        let hist = app.engine.undo_history();
        let n = hist.len();
        egui::ScrollArea::vertical().show(ui, |ui| {
            for (i, h) in hist.iter().enumerate() {
                if ui.selectable_label(i + 1 == n, format!("{}  {h}", i + 1)).clicked() {
                    for _ in 0..(n - i - 1) {
                        let _ = app.run("edit.undo", json!({}));
                    }
                }
            }
            if let Some(r) = app.engine.redo_label() {
                ui.label(egui::RichText::new(format!("(redo) {r}")).italics().color(Tokens::DARK.text_dim));
            }
        });
    });
    app.ui.show_undo_history = open;
}

fn plugin_windows(app: &mut SoundApp, ctx: &egui::Context) {
    let wins = app.ui.plugin_windows.clone();
    let mut keep = Vec::new();
    for (tid, slot) in wins {
        let Some((tname, ins)) =
            app.engine.session().track(tid).and_then(|t| t.mixer.inserts.get(slot).cloned().flatten().map(|i| (t.name.clone(), i)))
        else {
            continue;
        };
        let Some(info) = soundcraft_dsp::plugin_info(&ins.plugin) else { continue };
        let mut open = true;
        egui::Window::new(format!("{tname} · {} · {}", (b'a' + slot as u8) as char, info.name))
            .id(egui::Id::new(("plugin", tid.0, slot)))
            .open(&mut open)
            .default_width(340.0)
            .show(ctx, |ui| {
                ui.horizontal(|ui| {
                    ui.label(egui::RichText::new(&ins.preset).color(Tokens::DARK.text_dim));
                    let label = if ins.bypass { "BYPASSED" } else { "Bypass" };
                    if ui.button(label).clicked() {
                        let _ = app.run("mix.insert_bypass", json!({"track": tid.0, "slot": slot}));
                    }
                });
                if info.id == "eq_7band" || info.id == "eq_1band" {
                    eq_curve(ui, info.id, &ins);
                }
                egui::Grid::new(("pg", tid.0, slot)).num_columns(3).show(ui, |ui| {
                    for p in info.params {
                        let v = ins.params.get(p.id).copied().unwrap_or(p.default);
                        ui.label(p.name);
                        let mut x = v;
                        let resp = if !p.choices.is_empty() {
                            let mut idx = x.round().max(0.0) as usize;
                            let r = egui::ComboBox::from_id_salt(("pc", tid.0, slot, p.id))
                                .selected_text(p.choices.get(idx).copied().unwrap_or("?"))
                                .show_ui(ui, |ui| {
                                    for (i, c) in p.choices.iter().enumerate() {
                                        ui.selectable_value(&mut idx, i, *c);
                                    }
                                });
                            x = idx as f32;
                            r.response
                        } else {
                            let mut sl = egui::Slider::new(&mut x, p.min..=p.max).show_value(false);
                            if p.taper == soundcraft_dsp::Taper::Log && p.min > 0.0 {
                                sl = sl.logarithmic(true);
                            }
                            ui.add(sl)
                        };
                        if (x - v).abs() > f32::EPSILON {
                            let _ = app.engine.execute("mix.insert_param", &json!({"track": tid.0, "slot": slot, "param": p.id, "value": x}));
                        }
                        let _ = resp;
                        ui.label(egui::RichText::new(p.format(x)).font(mono(11.0)));
                        ui.end_row();
                    }
                });
            });
        if open {
            keep.push((tid, slot));
        }
    }
    app.ui.plugin_windows = keep;
}

fn eq_curve(ui: &mut Ui, id: &str, ins: &soundcraft_model::Insert) {
    let t = Tokens::DARK;
    let (r, _) = ui.allocate_exact_size(vec2(ui.available_width().max(300.0), 110.0), Sense::hover());
    ui.painter().rect_filled(r, 4.0, Color32::from_rgb(14, 18, 22));
    let n = 160;
    let freqs: Vec<f32> = (0..n).map(|i| 20.0 * 1000f32.powf(i as f32 / (n - 1) as f32)).collect();
    let params: Vec<(&str, f32)> = ins.params.iter().map(|(k, v)| (k.as_str(), *v)).collect();
    let resp = if id == "eq_7band" {
        soundcraft_dsp::eq7_response(&params, &freqs, 48_000.0)
    } else {
        soundcraft_dsp::eq1_response(&params, &freqs, 48_000.0)
    };
    for db in [-12.0f32, 0.0, 12.0] {
        let y = r.center().y - db / 18.0 * r.height() * 0.5;
        ui.painter().line_segment([pos2(r.min.x, y), pos2(r.max.x, y)], Stroke::new(1.0, Color32::from_rgb(40, 50, 60)));
    }
    let pts: Vec<egui::Pos2> = resp
        .iter()
        .enumerate()
        .map(|(i, db)| pos2(r.min.x + r.width() * i as f32 / (n - 1) as f32, r.center().y - db.clamp(-18.0, 18.0) / 18.0 * r.height() * 0.5))
        .collect();
    ui.painter().add(egui::Shape::line(pts, Stroke::new(2.0, t.counter_text)));
}

fn about(app: &mut SoundApp, ctx: &egui::Context) {
    let mut open = app.ui.show_about;
    if !open {
        return;
    }
    egui::Window::new("About SoundCraft").open(&mut open).collapsible(false).resizable(false).show(ctx, |ui| {
        ui.label(egui::RichText::new("SoundCraft").font(bold(22.0)));
        ui.label(format!("Version {}", env!("CARGO_PKG_VERSION")));
        ui.label("An open-source digital audio workstation from the ArtCraft team.");
        ui.label("Dual-licensed MIT OR Apache-2.0. Made with Rust and egui.");
        ui.hyperlink_to("getartcraft.com/apps/soundcraft", "https://getartcraft.com/apps/soundcraft");
        ui.hyperlink_to("Join us on Discord", "https://discord.gg/artcraft");
    });
    app.ui.show_about = open;
}

fn session_info(app: &mut SoundApp, ctx: &egui::Context) {
    let mut open = app.ui.show_session_info;
    if !open {
        return;
    }
    egui::Window::new("Session Info").open(&mut open).default_size(vec2(480.0, 360.0)).show(ctx, |ui| {
        let text = soundcraft_engine::inspect::session_text(&app.engine);
        egui::ScrollArea::vertical().show(ui, |ui| {
            ui.label(egui::RichText::new(text).font(mono(11.0)));
        });
    });
    app.ui.show_session_info = open;
}
