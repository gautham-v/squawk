//! Hardware probe for the parts unit tests cannot reach.
//!
//! ```sh
//! cargo run --release -p squawk-engine --example probe -- devices
//! cargo run --release -p squawk-engine --example probe -- mic 5       # dictate for 5 s
//! cargo run --release -p squawk-engine --example probe -- system 5    # capture system audio
//! cargo run --release -p squawk-engine --example probe -- meeting 60  # record a meeting
//! ```
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
    let paths = Paths::detect().expect("home");
    let (config, _) = Config::load(&paths.config_file);
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
            std::thread::sleep(Duration::from_secs(secs));
            let r = handle.stop().expect("stop");
            println!(
                "saved {} ({} s, system audio: {})",
                r.path.display(),
                r.length_secs,
                r.had_system_audio
            );
        }
        other => eprintln!("unknown probe {other:?}: devices | mic | system | meeting"),
    }
}
