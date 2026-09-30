//! Latency bench: model load, warm transcription, and streaming tail latency.
//!
//! ```sh
//! cargo run --release -p squawk-engine --example bench -- [--download] [--speed X] FILE...
//! ```
//!
//! For each file (any format macOS reads; `say -o x.aiff "..."` makes good
//! test input): transcribes it whole three times and prints the median, then
//! replays it through a real dictation session (segmenting and transcribing
//! while it "talks", `speed` × real time, default 1) and prints how long
//! `finish` took — the release-to-text number the app logs as `tail_ms`.
//! `--download` fetches the model first if it is missing.

use std::path::PathBuf;
use std::time::{Duration, Instant};

use squawk_core::{Config, ModelStatus, Paths};
use squawk_engine::model::{self, LoadedModel};
use squawk_engine::{audio, Engine, EngineConfig};

struct Stderr;

impl log::Log for Stderr {
    fn enabled(&self, m: &log::Metadata) -> bool {
        m.target().starts_with("squawk_engine")
    }
    fn log(&self, r: &log::Record) {
        if self.enabled(r.metadata()) {
            eprintln!("    [{}] {}", r.level(), r.args());
        }
    }
    fn flush(&self) {}
}

fn main() {
    if std::env::var_os("SQUAWK_BENCH_LOG").is_some() {
        let _ = log::set_logger(&Stderr);
        log::set_max_level(log::LevelFilter::Debug);
    }
    let mut files = Vec::new();
    let mut download = false;
    let mut speed = 1.0f32;
    let mut args = std::env::args().skip(1);
    while let Some(a) = args.next() {
        match a.as_str() {
            "--download" => download = true,
            "--speed" => speed = args.next().and_then(|s| s.parse().ok()).unwrap_or(1.0),
            _ => files.push(PathBuf::from(a)),
        }
    }
    let paths = Paths::detect().expect("home directory");
    let (config, _) = Config::load(&paths.config_file);
    let paths = paths.with_config(&config);
    let ec = EngineConfig::from_config(&paths, &config);
    let dir = ec.model_dir();

    if !model::is_installed(&dir) {
        if !download {
            eprintln!("model missing at {}; pass --download", dir.display());
            std::process::exit(1);
        }
        let t = Instant::now();
        let mut last = 0;
        model::download(
            &config.model.url,
            &paths.models_dir,
            &config.model.dir,
            &mut |s| match s {
                ModelStatus::Downloading { downloaded, total } => {
                    let pct = total.map(|t| downloaded * 100 / t.max(1)).unwrap_or(0);
                    if pct != last {
                        last = pct;
                        eprint!("\rdownloading {pct}%  {} MB", downloaded / 1_000_000);
                    }
                }
                other => eprintln!("\n{}", other.label()),
            },
        )
        .expect("download");
        eprintln!("model ready in {:.1} s", t.elapsed().as_secs_f32());
    }

    // Load + warm-up on their own, then drop, so the numbers are clean.
    let t = Instant::now();
    let mut m = LoadedModel::load(&dir, 0).expect("load");
    let load = t.elapsed();
    let t = Instant::now();
    m.warm_up().expect("warm up");
    let warm = t.elapsed();
    let t = Instant::now();
    m.transcribe(&vec![0.0; 16_000]).expect("second run");
    let second = t.elapsed();
    drop(m);
    println!(
        "model load {} ms · first inference (warm-up, 1 s) {} ms · second 1 s inference {} ms",
        load.as_millis(),
        warm.as_millis(),
        second.as_millis()
    );

    let engine = Engine::new(ec);
    engine.load_model_blocking().expect("engine load");
    for file in files {
        let samples = audio::load_file(&file).expect("audio file");
        let secs = samples.len() as f32 / 16_000.0;
        let mut runs: Vec<Duration> = (0..3)
            .map(|_| {
                let t = Instant::now();
                engine.transcribe(&samples).expect("transcribe");
                t.elapsed()
            })
            .collect();
        runs.sort();
        let text = engine.transcribe(&samples).unwrap();
        println!(
            "\n{} · {secs:.1} s audio · whole-clip transcribe {} ms (median of 3, {:.0}× real time)",
            file.display(),
            runs[1].as_millis(),
            secs / runs[1].as_secs_f32()
        );
        println!("  whole: {text}");

        // Release right on the last word (worst case), and 300 ms after it
        // (typical: people let go of fn a beat after they stop talking).
        for pause_ms in [0u32, 300] {
            let mut clip = samples.clone();
            clip.extend(std::iter::repeat_n(0.0, pause_ms as usize * 16));
            let clip_secs = clip.len() as f32 / 16_000.0;
            let session = engine.start_dictation_replay(clip, speed).expect("session");
            std::thread::sleep(Duration::from_secs_f32(clip_secs / speed + 0.02));
            let t = session.finish().expect("finish");
            println!(
                "  streaming, release {pause_ms} ms after the last word: tail {} ms · {} segments",
                t.tail_latency.as_millis(),
                t.segments
            );
            if pause_ms == 0 {
                println!("  streamed: {}", t.text);
            }
        }
    }
}
