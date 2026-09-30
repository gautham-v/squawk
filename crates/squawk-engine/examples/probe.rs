//! Hardware probe for the parts unit tests cannot reach.
//!
//! ```sh
//! cargo run --release -p squawk-engine --example probe -- devices
//! cargo run --release -p squawk-engine --example probe -- mic 5       # dictate for 5 s
//! cargo run --release -p squawk-engine --example probe -- system 5    # capture system audio
//! cargo run --release -p squawk-engine --example probe -- meeting 60  # record a meeting
//! cargo run --release -p squawk-engine --example probe -- meeting 20 no-aec keep
//! ```
//!
//! `meeting` takes optional words: `no-aec` records with
//! `echo_cancellation = false` (plain mic, no echo filter), `keep` keeps
//! both tracks as WAVs (`keep_audio`), `dictate` also dictates from 3 s to
//! 10 s into the meeting and prints what that heard. It prints the engine's
//! log and the transcript.
//!
//! `mic` and `meeting` need the Microphone permission for the terminal,
//! `system` and `meeting` need Screen & System Audio Recording (macOS asks
//! on first use). The meeting file goes to the usual meetings folder.

use std::sync::{Arc, Mutex};
use std::time::Duration;

use squawk_core::{Config, Paths, Store};
use squawk_engine::system_audio::{self, SystemAudioCapture};
use squawk_engine::{audio, Engine, EngineConfig, MeetingOptions};

fn main() {
    let mut args = std::env::args().skip(1);
    let what = args.next().unwrap_or_else(|| "devices".into());
    let secs: u64 = args.next().and_then(|s| s.parse().ok()).unwrap_or(5);
    let flags: Vec<String> = args.collect();
    let paths = Paths::detect().expect("home");
    let (mut config, _) = Config::load(&paths.config_file);
    if flags.iter().any(|f| f == "no-aec") {
        config.meeting.echo_cancellation = false;
    }
    if flags.iter().any(|f| f == "keep") {
        config.keep_audio = true;
    }
    let _ = log::set_logger(&StderrLog).map(|()| log::set_max_level(log::LevelFilter::Info));
    let paths = paths.with_config(&config);
    let engine = Engine::new(EngineConfig::from_config(&paths, &config));
    engine.set_event_sink(|e| eprintln!("event: {e:?}"));

    match what.as_str() {
        "devices" => {
            for d in audio::input_devices() {
                println!("{d}");
            }
            println!(
                "screen & system audio permission: {}",
                system_audio::has_permission()
            );
        }
        "mic" => {
            engine.load_model_blocking().expect("model");
            let session = engine.start_dictation().expect("start");
            eprintln!("talk for {secs} s…");
            for _ in 0..secs * 10 {
                std::thread::sleep(Duration::from_millis(100));
                let bars = (session.level() * 30.0) as usize;
                eprint!("\r{:<30}", "#".repeat(bars));
            }
            let t = session.finish().expect("finish");
            eprintln!(
                "\n{:.1} s audio, {} segments, tail {} ms",
                t.audio_secs,
                t.segments,
                t.tail_latency.as_millis()
            );
            println!("{}", t.text);
        }
        "system" => {
            let got = Arc::new(Mutex::new(Vec::<f32>::new()));
            let sink = got.clone();
            let cap = SystemAudioCapture::start(move |b| sink.lock().unwrap().extend_from_slice(b))
                .expect("system audio");
            eprintln!("play something for {secs} s…");
            std::thread::sleep(Duration::from_secs(secs));
            cap.stop();
            let samples = std::mem::take(&mut *got.lock().unwrap());
            let peak = samples.chunks(320).map(audio::rms).fold(0f32, f32::max);
            println!(
                "{:.1} s of system audio received, peak rms {peak:.4}",
                samples.len() as f32 / 16_000.0
            );
            if peak > 0.001 && engine.load_model_blocking().is_ok() {
                println!("heard: {}", engine.transcribe(&samples).unwrap_or_default());
            }
        }
        "meeting" => {
            engine.load_model_blocking().expect("model");
            let now = chrono::Local::now();
            let store = Store::new(&paths);
            let opts = MeetingOptions {
                title: "Probe meeting".into(),
                path: store.new_meeting_path(&now, "Probe meeting"),
                started_at: now,
                chunk_secs: config.meeting.chunk_secs,
                system_audio: config.meeting.system_audio,
            };
            let handle = engine.start_meeting(opts).expect("start meeting");
            eprintln!("recording {secs} s to {}", handle.info().path);
            if flags.iter().any(|f| f == "dictate") && secs > 10 {
                std::thread::sleep(Duration::from_secs(3));
                let session = engine.start_dictation().expect("start dictation");
                std::thread::sleep(Duration::from_secs(7));
                let t = session.finish().expect("finish dictation");
                println!("dictation ({:.1} s): {:?}", t.audio_secs, t.text);
                std::thread::sleep(Duration::from_secs(secs - 10));
            } else {
                std::thread::sleep(Duration::from_secs(secs));
            }
            let r = handle.stop().expect("stop");
            println!(
                "saved {} ({} s, system audio: {})",
                r.path.display(),
                r.length_secs,
                r.had_system_audio
            );
            if let Ok(m) = store.read_meeting(&r.path) {
                for u in m.utterances {
                    println!("{:>4}  {:<4}  {}", u.start_secs, u.speaker.label(), u.text);
                }
            }
        }
        other => eprintln!("unknown probe {other:?}: devices | mic | system | meeting"),
    }
}

/// The engine logs through `log`; print it so a probe run shows what the
/// mic and system audio did.
struct StderrLog;

impl log::Log for StderrLog {
    fn enabled(&self, m: &log::Metadata) -> bool {
        m.level() <= log::Level::Info && m.target().starts_with("squawk")
    }

    fn log(&self, r: &log::Record) {
        if self.enabled(r.metadata()) {
            eprintln!("{} {}: {}", r.level(), r.target(), r.args());
        }
    }

    fn flush(&self) {}
}
