# SoundCraft roadmap

SoundCraft is a clean-room, pure-Rust digital audio workstation that aims at full parity with
Avid Pro Tools, and then beyond it: faster, open, scriptable and agent-controllable. This file is
the honest status: what works today, what is missing, and how far we are.

## Where we are (2026-10-07)

| Measure | Value | How |
|---|---|---|
| Menu-catalog parity (engine + UI) | **462 / 512 menu items (90 %)**; engine alone 388 / 512 (76 %), see [`docs/parity.md`](docs/parity.md) | `cargo xtask parity` compares the incumbent's 512 menu leaves (names observed black-box) with our command registry; the remaining items are mostly video, Atmos, collaboration, notation and third-party services |
| Estimated overall feature parity | **~40 %** | judgement across the areas below, weighted by how much professional users rely on them |
| Estimated remaining effort | **~220–300 wall-clock hours** of Claude Opus 5.5 work (single agent; roughly 70–100 hours with 3–4 agents in parallel) | sum of the per-area estimates below |

### Area by area

| Area | Status | Parity | Remaining (h) |
|---|---|---:|---:|
| Session model, save/load, undo | Done: tracks, playlists, clips, fades, clip gain, automation, markers, groups, busses; JSON `.scraft` + Audio Files folder; COW undo with gesture coalescing | 80 % | 10 |
| Audio file I/O | WAV/BWF/RF64, AIFF/AIFC, FLAC (read/write); MP3, OGG, AAC, ALAC, CAF (read); peaks | 75 % | 10 |
| Editing | Slip/Shuffle/Spot/Grid, cut/copy/paste/clear/duplicate/repeat/shift, separate/heal/trim/consolidate/strip silence, fades & crossfades, nudge, playlists, grabber/trim/selector/smart/pencil/zoom/scrubber gestures, edit groups | 60 % | 35 |
| Mixer & routing | Faders, pan, mute/solo (SIP, implicit solo), sends pre/post, busses, aux inputs, master faders, VCAs, routing folders, 10 inserts, automation read | 60 % | 30 |
| Automation | Breakpoint lanes for volume/pan/mute/sends/plugin params; modes stored; write/thin/glide/convert commands | 45 % | 25 |
| Plugins (built-in) | 26 original processors + 2 instruments, AudioSuite offline processing | 45 % | 30 |
| Third-party plugins (CLAP/VST3/AU) | Not started | 0 % | 40 |
| MIDI | MIDI/instrument tracks, SMF import/export, MIDI editor (piano roll + velocity), event list, step input, quantize/transpose/velocity/duration ops | 40 % | 30 |
| Recording | Audio input capture, punch into selection; loop record, QuickPunch, input monitoring with latency compensation missing | 30 % | 20 |
| Elastic Audio / TCE / Beat Detective | Clip stretch ratio, offline time-stretch and pitch-shift, transient detection | 20 % | 25 |
| Video, surround panning, Atmos | Formats modelled; no video track, no surround panner | 5 % | 40 |
| UI fidelity | Edit + Mix windows, toolbar, rulers, track headers, menus for the whole catalog, floating windows, dialogs | 65 % | 25 |
| Agent control | CLI, JSON control channel, MCP server (headless + bridged), offscreen UI renders | 90 % (ahead of the incumbent) | 5 |
| Release engineering | Signed macOS universal, Windows x64/x86, Linux AppImage/deb/rpm/tar/Flatpak, FreeBSD, Web/WASM on every push to `release` | 80 % | 5 |

## Current focus

1. Play back the model-only state the engine already stores: clip effects, trim automation, MIDI real-time properties.
2. QuickPunch and input monitoring (loop record and Write/Touch/Latch automation passes have landed).
3. Pitch-preserving realtime Elastic Audio.
4. Plugin hosting (CLAP first, it is MIT-licensed and Rust-friendly).
5. Elastic Audio in realtime; Beat Detective workflow window.
6. README screenshots and the first alpha release.

## Milestones

- **M0–M3 (done):** workspace, gates, model, IO, DSP, mix engine, command engine, Edit/Mix UI.
- **M4 (done):** realtime playback, metering, plugins, bounce.
- **M5 (in progress):** editing depth (tools, modes, playlists, fades) — most items landed.
- **M6 (in progress):** recording.
- **M7 (in progress):** MIDI editing.
- **M8 (done for the first cut):** CLI, control channel, MCP, agent acceptance test.
- **M9:** AudioSuite breadth, Elastic Audio, Beat Detective.
- **M10:** alpha release on all platforms.
- **M11:** third-party plugin hosting, video, surround.
