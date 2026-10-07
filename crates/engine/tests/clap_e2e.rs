//! A hosted CLAP plugin through the whole stack: `mix.insert` / `mix.insert_param` accept a
//! `clap:` id and the mix engine renders through it. Uses the test plugin built from
//! `crates/clap-host/tests/fixture` (a gain whose param "7" is linear gain).

use serde_json::json;
use soundcraft_audio_io::AudioBuffer;
use soundcraft_engine::Engine;
use soundcraft_model::{ChannelFormat, Clip, SourceAudio, SourceId, TrackKind};
use soundcraft_time::Range;
use std::path::Path;
use std::process::Command;
use std::sync::Arc;

fn install_fixture() {
    let manifest = Path::new(env!("CARGO_MANIFEST_DIR")).join("../clap-host/tests/fixture/Cargo.toml");
    let target = Path::new(env!("CARGO_TARGET_TMPDIR")).join("clap-fixture-target");
    let cargo = std::env::var("CARGO").unwrap_or_else(|_| "cargo".into());
    let st = Command::new(cargo)
        .args(["build", "--quiet", "--manifest-path"])
        .arg(&manifest)
        .arg("--target-dir")
        .arg(&target)
        .env_remove("RUSTFLAGS")
        .status()
        .unwrap();
    assert!(st.success());
    let lib = if cfg!(target_os = "macos") {
        "libclap_fixture.dylib"
    } else if cfg!(windows) {
        "clap_fixture.dll"
    } else {
        "libclap_fixture.so"
    };
    let dir = Path::new(env!("CARGO_TARGET_TMPDIR")).join(format!("engine-clap-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    std::fs::copy(target.join("debug").join(lib), dir.join("Fixture.clap")).unwrap();
    assert_eq!(soundcraft_clap_host::add_search_dir(&dir), 2);
}

#[test]
fn clap_insert_renders_through_the_mix() {
    install_fixture();
    let mut e = Engine::default();
    let list = e.execute("engine.clap_plugins", &json!({})).unwrap();
    assert!(list.as_array().unwrap().iter().any(|d| d["id"] == "clap:org.soundcraft.test.gain"));

    let s = e.session_mut();
    let t = s.add_track(TrackKind::Audio, ChannelFormat::Mono, None);
    s.pool.insert(SourceId(77), Arc::new(SourceAudio::new(AudioBuffer { sample_rate: 48_000, channels: vec![vec![0.25; 4800]] })));
    let id = s.new_clip_id();
    s.track_mut(t).unwrap().playlist_mut().unwrap().clips.push(Clip::audio(id, "dc", SourceId(77), 0, 0, 4800));
    let dry = soundcraft_mix::render_range(e.session(), Range::new(0, 4800), 512)[0][2000];
    assert!(dry.abs() > 0.01);

    let r = e.execute("mix.insert", &json!({"track": t.0, "plugin": "clap:org.soundcraft.test.gain", "params": {"7": 2.0}})).unwrap();
    assert_eq!(r["plugin"], "clap:org.soundcraft.test.gain");
    let wet = soundcraft_mix::render_range(e.session(), Range::new(0, 4800), 512)[0][2000];
    assert!((wet - 2.0 * dry).abs() < 1e-4, "dry {dry} wet {wet}");

    e.execute("mix.insert_param", &json!({"track": t.0, "slot": 0, "param": "7", "value": 0.5})).unwrap();
    let half = soundcraft_mix::render_range(e.session(), Range::new(0, 4800), 512)[0][2000];
    assert!((half - 0.5 * dry).abs() < 1e-4, "dry {dry} half {half}");
    assert!(e.execute("mix.insert_param", &json!({"track": t.0, "slot": 0, "param": "Gain", "value": 1.0})).is_err());
}
