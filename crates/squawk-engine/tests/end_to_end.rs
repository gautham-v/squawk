//! The whole dictation path without a microphone or a keyboard: speech made
//! with `say` is fed through the real streaming session in real time (as if
//! from the mic), then through cleanup, Claude Code mode (a temp git repo as
//! the session's cwd) and the dictionary, exactly as the app does on fn-up.
//!
//! Needs the downloaded model, so ignored by default:
//! `cargo test --release -p squawk-engine --test end_to_end -- --ignored --nocapture`

use std::path::{Path, PathBuf};
use std::process::Command;
use std::sync::Arc;
use std::time::{Duration, Instant};

use squawk_core::cleanup::CleanupOptions;
use squawk_core::context::{Agent, Context, RepoVocab, Session};
use squawk_core::{pipeline, Config, Dictionary, Paths};
use squawk_engine::{audio, model, Engine, EngineConfig, SAMPLE_RATE};

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
    let engine = Engine::new(ec);
    engine.load_model_blocking().unwrap();
    Some(engine)
}

fn say(dir: &Path, name: &str, text: &str) -> Vec<f32> {
    let path = dir.join(format!("{name}.aiff"));
    let ok = Command::new("/usr/bin/say")
        .arg("-o")
        .arg(&path)
        .arg(text)
        .status()
        .expect("say")
        .success();
    assert!(ok, "say failed");
    audio::load_file(&path).unwrap()
}

/// A small Rust project in a fresh git repo.
fn repo(dir: &Path) -> PathBuf {
    let root = dir.join("demo");
    for (file, body) in [
        ("Cargo.toml", "[package]\nname = \"demo\"\n"),
        ("README.md", "# demo\n\nA small demo.\n"),
        ("src/main.rs", "fn main() {}\n"),
        ("src/audio.rs", "\n"),
        ("src/segmenter.rs", "\n"),
    ] {
        let path = root.join(file);
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        std::fs::write(path, body).unwrap();
    }
    for args in [&["init", "-q"][..], &["add", "."][..]] {
        let ok = Command::new("git")
            .args(args)
            .current_dir(&root)
            .status()
            .unwrap()
            .success();
        assert!(ok, "git {args:?}");
    }
    root
}

struct Run {
    text: String,
    audio_secs: f32,
    segments: usize,
    finish: Duration,
    pipeline: Duration,
}

/// Feed `samples` in real time, let go of fn `release_after` past the end of
/// the audio, and time fn-up to final text.
fn dictate(
    engine: &Engine,
    mut samples: Vec<f32>,
    release_after: Duration,
    context: Option<&Context>,
) -> Run {
    // The replay stops when the samples run out, so a late release is
    // silence in the clip, the same as a mic in a quiet room.
    let pad = (release_after.as_secs_f32() * SAMPLE_RATE as f32) as usize;
    samples.extend(vec![0.0; pad]);
    let secs = samples.len() as f32 / SAMPLE_RATE as f32;
    let session = engine.start_dictation_replay(samples, 1.0).unwrap();
    std::thread::sleep(Duration::from_secs_f32(secs + 0.03));

    let released = Instant::now();
    let t = session.finish().unwrap();
    let finish = released.elapsed();
    let dict = Dictionary::parse("cloud code -> Claude Code\n");
    let text = pipeline::finish(&t.text, &dict, context, &CleanupOptions::default());
    let pipeline = released.elapsed() - finish;
    eprintln!(
        "audio {:.1}s  segments {}  release {:?}  finish {} ms  pipeline {} µs\n  raw:   {}\n  final: {}",
        t.audio_secs,
        t.segments,
        release_after,
        finish.as_millis(),
        pipeline.as_micros(),
        t.text,
        text
    );
    Run {
        text,
        audio_secs: t.audio_secs,
        segments: t.segments,
        finish,
        pipeline,
    }
}

#[test]
#[ignore = "needs the downloaded model"]
fn claude_code_dictation_end_to_end() {
    let Some(engine) = engine() else { return };
    let tmp = tempfile::tempdir().unwrap();
    let root = repo(tmp.path());
    let vocab = Arc::new(RepoVocab::build(&root));
    let context = Context {
        session: Session {
            agent: Agent::Claude,
            pid: 0,
            cwd: root.clone(),
            tty: None,
        },
        vocab,
    };

    let speech = say(
        tmp.path(),
        "short",
        "Um, can you look at the audio dot rs file and the readme, and then ask \
         cloud code to run the tests.",
    );
    let run = dictate(&engine, speech, Duration::from_millis(300), Some(&context));
    for want in [
        "@src/audio.rs",
        "@README.md",
        "Claude Code",
        "run the tests",
    ] {
        assert!(run.text.contains(want), "{want:?} not in {:?}", run.text);
    }
    assert!(!run.text.to_lowercase().starts_with("um"), "{:?}", run.text);
    assert!(run.pipeline < Duration::from_millis(20));
}

#[test]
#[ignore = "needs the downloaded model"]
fn release_latency_for_a_twenty_second_dictation() {
    let Some(engine) = engine() else { return };
    let tmp = tempfile::tempdir().unwrap();
    let speech = say(
        tmp.path(),
        "long",
        "Okay, here is what I want to do next. First, read through the segmenter and \
         check how it picks the cut points, because I think the forced cut sometimes \
         lands in the middle of a word. Then look at the resampler tests and add one \
         for forty four point one kilohertz input. After that, run the whole suite, and \
         if everything passes, write a short summary of what changed.",
    );
    // Warm the recognizer the way a first dictation would.
    let _ = dictate(
        &engine,
        speech[..SAMPLE_RATE as usize].to_vec(),
        Duration::ZERO,
        None,
    );

    for release in [Duration::ZERO, Duration::from_millis(300)] {
        let run = dictate(&engine, speech.clone(), release, None);
        assert!(run.audio_secs > 15.0, "{}", run.audio_secs);
        assert!(run.segments >= 2, "cut while talking");
        let lower = run.text.to_lowercase();
        for want in ["segmenter", "cut points", "summary of what changed"] {
            assert!(lower.contains(want), "{want:?} not in {:?}", run.text);
        }
        // The target is 250 ms; leave room for a busy machine.
        assert!(run.finish < Duration::from_millis(1000), "{:?}", run.finish);
    }
}
