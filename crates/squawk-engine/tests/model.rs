//! Real-model tests. They need the downloaded model, so they are ignored by
//! default: `cargo test -p squawk-engine --release -- --ignored`. Test speech
//! is generated on the spot with `say`, so no audio lives in the repo.

use std::path::{Path, PathBuf};
use std::time::Duration;

use squawk_core::{Config, Paths};
use squawk_engine::{audio, model, Engine, EngineConfig};

fn engine() -> Option<Engine> {
    let paths = Paths::detect().ok()?;
    let (config, _) = Config::load(&paths.config_file);
    let ec = EngineConfig::from_config(&paths.with_config(&config), &config);
    if !model::is_installed(&ec.model_dir()) {
        eprintln!(
            "model not installed at {}; skipping",
            ec.model_dir().display()
        );
        return None;
    }
    Some(Engine::new(ec))
}

fn say(dir: &Path, name: &str, text: &str) -> PathBuf {
    let path = dir.join(format!("{name}.aiff"));
    let ok = std::process::Command::new("/usr/bin/say")
        .arg("-o")
        .arg(&path)
        .arg(text)
        .status()
        .expect("say")
        .success();
    assert!(ok, "say failed");
    path
}

fn words(text: &str) -> String {
    text.to_lowercase()
        .chars()
        .filter(|c| c.is_alphanumeric() || c.is_whitespace())
        .collect()
}

#[test]
#[ignore = "needs the downloaded model"]
fn transcribes_generated_speech() {
    let Some(engine) = engine() else { return };
    let tmp = tempfile::tempdir().unwrap();
    let file = say(
        tmp.path(),
        "one",
        "Please open the settings file and turn off dark mode. Then restart the app.",
    );
    let samples = audio::load_file(&file).unwrap();
    engine.load_model_blocking().unwrap();
    let text = engine.transcribe(&samples).unwrap();
    let w = words(&text);
    for want in ["open the settings file", "dark mode", "restart the app"] {
        assert!(w.contains(want), "{want:?} not in {text:?}");
    }
}

#[test]
#[ignore = "needs the downloaded model"]
fn streaming_dictation_matches_and_is_quick() {
    let Some(engine) = engine() else { return };
    let tmp = tempfile::tempdir().unwrap();
    let file = say(
        tmp.path(),
        "two",
        "Okay, here is the plan. First check the resampler. Then look at the segmenter \
         tests, because the forced cut sometimes lands in the middle of a word. After \
         that, run the whole test suite and push the branch.",
    );
    let mut samples = audio::load_file(&file).unwrap();
    samples.extend(vec![0.0; 16_000 * 3 / 10]);
    engine.load_model_blocking().unwrap();
    let secs = samples.len() as f32 / 16_000.0;
    let speed = 4.0;
    let session = engine.start_dictation_replay(samples, speed).unwrap();
    std::thread::sleep(Duration::from_secs_f32(secs / speed + 0.05));
    let t = session.finish().unwrap();
    eprintln!(
        "{:.1} s, {} segments, tail {} ms: {}",
        t.audio_secs,
        t.segments,
        t.tail_latency.as_millis(),
        t.text
    );
    let w = words(&t.text);
    for want in [
        "here is the plan",
        "segmenter",
        "middle of a word",
        "push the branch",
    ] {
        assert!(w.contains(want), "{want:?} not in {:?}", t.text);
    }
    assert!(t.segments >= 2, "cut while talking");
    // Generous: other work may share the CPU; the bench measures properly.
    assert!(t.tail_latency < Duration::from_millis(1500));
}

#[test]
#[ignore = "needs the downloaded model"]
fn silence_gives_empty_text() {
    let Some(engine) = engine() else { return };
    engine.load_model_blocking().unwrap();
    let session = engine
        .start_dictation_replay(vec![0.0; 16_000], 10.0)
        .unwrap();
    std::thread::sleep(Duration::from_millis(150));
    let t = session.finish().unwrap();
    assert_eq!(t.text, "");
    assert_eq!(t.segments, 0);
}

#[test]
#[ignore = "needs the downloaded model"]
fn meeting_segments_keep_their_spaces_and_times() {
    let Some(engine) = engine() else { return };
    let tmp = tempfile::tempdir().unwrap();
    let file = say(
        tmp.path(),
        "three",
        "The model is about 700 megabytes. Each track is cut into 30 second chunks.",
    );
    let samples = audio::load_file(&file).unwrap();
    let mut m = model::LoadedModel::load(&engine.config().model_dir(), 0).unwrap();
    let r = m.transcribe(&samples).unwrap();
    assert!(!r.segments.is_empty(), "{:?}", r.segments);
    let joined: Vec<&str> = r.segments.iter().map(|s| s.2.as_str()).collect();
    let joined = joined.join(" ");
    assert!(joined.contains("about 700"), "{joined}");
    let secs = samples.len() as f32 / 16_000.0;
    assert!(r.segments.windows(2).all(|w| w[0].0 <= w[1].0));
    assert!(r.segments.iter().all(|s| s.0 <= s.1 && s.1 <= secs));
}
