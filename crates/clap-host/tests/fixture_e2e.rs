//! End to end: build the test CLAP plugin in `tests/fixture` (a cdylib outside the workspace),
//! install it as a `.clap` in a temp folder, then scan → instantiate → process → params → notes.

use soundcraft_dsp::{Category, Unit};
use std::path::{Path, PathBuf};
use std::process::Command;
use std::sync::OnceLock;

/// Builds the fixture once and returns a folder containing `Fixture.clap`.
fn fixture_dir() -> &'static Path {
    static DIR: OnceLock<PathBuf> = OnceLock::new();
    DIR.get_or_init(|| {
        let manifest = Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixture/Cargo.toml");
        let target = Path::new(env!("CARGO_TARGET_TMPDIR")).join("clap-fixture-target");
        let cargo = std::env::var("CARGO").unwrap_or_else(|_| "cargo".into());
        let st = Command::new(cargo)
            .args(["build", "--quiet", "--manifest-path"])
            .arg(&manifest)
            .arg("--target-dir")
            .arg(&target)
            .env_remove("RUSTFLAGS")
            .status()
            .expect("run cargo");
        assert!(st.success(), "building the CLAP fixture failed");
        let lib = if cfg!(target_os = "macos") {
            "libclap_fixture.dylib"
        } else if cfg!(windows) {
            "clap_fixture.dll"
        } else {
            "libclap_fixture.so"
        };
        let built = target.join("debug").join(lib);
        let dir = Path::new(env!("CARGO_TARGET_TMPDIR")).join(format!("clap-fixture-install-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        if cfg!(target_os = "macos") {
            // Install as a real macOS bundle to exercise bundle resolution.
            let exe = dir.join("vendor/Fixture.clap/Contents/MacOS");
            std::fs::create_dir_all(&exe).unwrap();
            std::fs::copy(&built, exe.join("Fixture")).unwrap();
        } else {
            std::fs::create_dir_all(dir.join("vendor")).unwrap();
            std::fs::copy(&built, dir.join("vendor/Fixture.clap")).unwrap();
        }
        assert_eq!(soundcraft_clap_host::add_search_dir(&dir), 2);
        dir
    })
}

const GAIN: &str = "clap:org.soundcraft.test.gain";
const SYNTH: &str = "clap:org.soundcraft.test.synth";

#[test]
fn scan_lists_both_fixture_plugins() {
    let dir = fixture_dir();
    let found = soundcraft_clap_host::scan_paths(&[dir.to_path_buf()]);
    assert_eq!(found.len(), 2);
    let g = found.iter().find(|d| d.id == GAIN).unwrap();
    assert_eq!(g.name, "Fixture Gain");
    assert_eq!(g.vendor, "SoundCraft tests");
    assert_eq!(g.features, vec!["audio-effect", "stereo"]);
    assert_eq!(g.category, Category::Other);
    assert!(!g.is_instrument);
    let s = found.iter().find(|d| d.id == SYNTH).unwrap();
    assert_eq!(s.category, Category::Instrument);
    assert!(s.is_instrument);
    let global = soundcraft_clap_host::scan();
    assert!(global.iter().any(|d| d.id == GAIN) && global.iter().any(|d| d.id == SYNTH));
}

#[test]
fn info_exposes_visible_params_with_names() {
    fixture_dir();
    let info = soundcraft_clap_host::plugin_info(GAIN).unwrap();
    assert_eq!(info.id, GAIN);
    assert_eq!(info.short_name, "Fixture ");
    let ids: Vec<&str> = info.params.iter().map(|p| p.id).collect();
    assert_eq!(ids, vec!["7", "3"], "hidden param 9 is skipped");
    let gain = info.param("7").unwrap();
    assert_eq!((gain.name, gain.min, gain.max, gain.default), ("Gain", 0.0, 2.0, 1.0));
    let mode = info.param("3").unwrap();
    assert_eq!(mode.unit, Unit::Choice);
    assert_eq!(mode.choices, &["A", "B", "C"]);
    // Cached: the same leaked info every time.
    assert!(std::ptr::eq(info, soundcraft_clap_host::plugin_info(GAIN).unwrap()));
}

#[test]
fn gain_processes_audio_and_follows_params() {
    fixture_dir();
    let mut p = soundcraft_clap_host::create(GAIN).unwrap();
    p.prepare(44_100.0, 256, 2);
    assert_eq!(p.latency(), 3);
    assert_eq!(p.param("7"), Some(1.0));
    let mut io = vec![vec![0.5f32; 256], vec![-0.25f32; 256]];
    p.process(&mut io, 256);
    assert!(io[0].iter().all(|&x| (x - 0.5).abs() < 1e-6));
    assert!(p.set_param("7", 2.0));
    assert!(!p.set_param("9", 1.0), "hidden params are not exposed");
    assert!(!p.set_param("Gain", 1.0), "ids are CLAP numbers");
    assert!(p.set_param("7", 5.0), "clamped");
    assert_eq!(p.param("7"), Some(2.0));
    // A block larger than max_block is chunked.
    let mut io = vec![vec![0.5f32; 700], vec![-0.25f32; 700]];
    p.process(&mut io, 700);
    assert!(io[0].iter().all(|&x| (x - 1.0).abs() < 1e-6));
    assert!(io[1].iter().all(|&x| (x + 0.5).abs() < 1e-6));
    // Mono strip: input duplicated into the stereo port, stereo output averaged back.
    p.prepare(48_000.0, 128, 1);
    let mut mono = vec![vec![0.25f32; 128]];
    p.process(&mut mono, 128);
    assert!(mono[0].iter().all(|&x| (x - 0.5).abs() < 1e-6));
    // Hostile shapes don't crash.
    p.process(&mut [], 64);
    let mut short = vec![vec![1.0f32; 4]];
    p.process(&mut short, 1_000_000);
    assert!(p.set_param("7", f32::NAN), "NaN becomes the default");
    assert_eq!(p.param("7"), Some(1.0));
    p.reset();
    p.prepare(f32::NAN, 0, 0);
    let mut io = vec![vec![0.1f32; 8]];
    p.process(&mut io, 8);
}

#[test]
fn synth_plays_notes_at_their_offsets() {
    fixture_dir();
    let info = soundcraft_clap_host::plugin_info(SYNTH).unwrap();
    assert!(info.is_instrument);
    let mut p = soundcraft_clap_host::create(SYNTH).unwrap();
    p.prepare(48_000.0, 64, 2);
    let mut io = vec![vec![9.0f32; 64], vec![9.0f32; 64]];
    p.note_on(10, 60, 127);
    p.note_off(40, 60);
    p.process(&mut io, 64);
    for (i, &x) in io[0].iter().enumerate() {
        let want = if (10..40).contains(&i) { 1.0 } else { 0.0 };
        assert!((x - want).abs() < 1e-6, "frame {i}: {x}");
    }
    assert_eq!(io[0], io[1]);
    // Offsets past the block are clamped into it; all_notes_off releases held notes.
    p.note_on(1000, 61, 64);
    let mut io = vec![vec![0.0f32; 64], vec![0.0f32; 64]];
    p.process(&mut io, 64);
    assert!(io[0][62].abs() < 1e-6 && (io[0][63] - 64.0 / 127.0).abs() < 1e-6);
    p.all_notes_off();
    p.process(&mut io, 64);
    assert!(io[0].iter().all(|x| x.abs() < 1e-6));
}

#[test]
fn many_instances_create_and_drop_cleanly() {
    fixture_dir();
    for _ in 0..50 {
        let mut a = soundcraft_clap_host::create(GAIN).unwrap();
        let b = soundcraft_clap_host::create(SYNTH).unwrap();
        let mut io = vec![vec![0.0f32; 32]; 2];
        a.process(&mut io, 32);
        drop(b);
    }
}
