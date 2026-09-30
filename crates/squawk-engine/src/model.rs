//! Finding, downloading and loading the Parakeet model.
//!
//! The default model is Handy's tarball of the int8 ONNX export
//! (`squawk_core::config::DEFAULT_MODEL_URL`, ~480 MB). It unpacks to one
//! directory, `parakeet-tdt-0.6b-v3-int8/`, holding [`REQUIRED_FILES`]. The
//! tarball was made on a Mac and carries AppleDouble `._*` entries and
//! `PaxHeader` entries: skip them.
//!
//! Download: stream to `<models_dir>/<dir>.tar.gz.part` (resume with a Range
//! request if a partial file exists), then unpack into
//! `<models_dir>/<dir>.partial/` and rename to `<models_dir>/<dir>/` only once
//! every required file is there, so a half-unpacked model is never mistaken
//! for a good one. Delete the tarball afterwards. One of the two network
//! accesses squawk makes; the other is the cleanup model's download
//! (`normalizer`), which reuses [`fetch`] and the checksum.

use std::fs::{File, OpenOptions};
use std::io::{Read, Seek, Write};
use std::path::{Component, Path, PathBuf};
use std::time::{Duration, Instant};

use sha2::{Digest, Sha256};
use squawk_core::ModelStatus;
use transcribe_rs::onnx::parakeet::{ParakeetModel, ParakeetParams, TimestampGranularity};
use transcribe_rs::onnx::Quantization;

use crate::error::EngineError;
use crate::SAMPLE_RATE;

/// The files `transcribe_rs::onnx::parakeet::ParakeetModel::load` needs with
/// `Quantization::Int8`.
pub const REQUIRED_FILES: &[&str] = &[
    "encoder-model.int8.onnx",
    "decoder_joint-model.int8.onnx",
    "nemo128.onnx",
    "vocab.txt",
];

/// Size and SHA-256 of the tarball at the default URL, checked after the
/// download so a truncated or tampered file is never unpacked. Other URLs
/// are only checked against their Content-Length.
pub const DEFAULT_TARBALL_SIZE: u64 = 478_517_071;
pub const DEFAULT_TARBALL_SHA256: &str =
    "43d37191602727524a7d8c6da0eef11c4ba24320f5b4730f1a2497befc2efa77";

/// Every required file is present in `dir`.
pub fn is_installed(dir: &Path) -> bool {
    REQUIRED_FILES.iter().all(|f| dir.join(f).is_file())
}

/// Progress reports at most this often while downloading.
const PROGRESS_EVERY: Duration = Duration::from_millis(250);

/// Download and unpack the model into `models_dir/dir_name`, reporting
/// `Downloading`/`Extracting` through `progress`. Blocking. Returns the
/// model directory. Already installed: returns at once.
pub fn download(
    url: &str,
    models_dir: &Path,
    dir_name: &str,
    progress: &mut dyn FnMut(ModelStatus),
) -> Result<PathBuf, EngineError> {
    let final_dir = models_dir.join(dir_name);
    if is_installed(&final_dir) {
        return Ok(final_dir);
    }
    std::fs::create_dir_all(models_dir)?;
    // The app (on first launch) and `squawk model download` may both get
    // here; they share the .part file and the staging dir, so one waits for
    // the other and then finds the model installed.
    let lock = File::create(models_dir.join(format!("{dir_name}.lock")))?;
    lock.lock()?;
    if is_installed(&final_dir) {
        return Ok(final_dir);
    }
    let part = models_dir.join(format!("{dir_name}.tar.gz.part"));
    let is_default = url == squawk_core::config::DEFAULT_MODEL_URL;
    fetch(
        url,
        &part,
        is_default.then_some(DEFAULT_TARBALL_SIZE),
        progress,
    )?;
    progress(ModelStatus::Extracting);
    if is_default {
        if let Err(e) = verify_sha256(&part, DEFAULT_TARBALL_SHA256) {
            let _ = std::fs::remove_file(&part);
            return Err(e);
        }
    }
    install_tarball(&part, models_dir, dir_name)?;
    let _ = std::fs::remove_file(&part);
    Ok(final_dir)
}

/// Unpack `tarball` into `<models_dir>/<dir_name>` via a `.partial` staging
/// directory. Fails (leaving no model directory) if a required file is
/// missing from the archive.
pub fn install_tarball(
    tarball: &Path,
    models_dir: &Path,
    dir_name: &str,
) -> Result<PathBuf, EngineError> {
    let final_dir = models_dir.join(dir_name);
    let staging = models_dir.join(format!("{dir_name}.partial"));
    if staging.exists() {
        std::fs::remove_dir_all(&staging)?;
    }
    std::fs::create_dir_all(&staging)?;
    let file = File::open(tarball)?;
    if let Err(e) = extract(file, &staging) {
        let _ = std::fs::remove_dir_all(&staging);
        return Err(e);
    }
    if !is_installed(&staging) {
        let _ = std::fs::remove_dir_all(&staging);
        let _ = std::fs::remove_file(tarball);
        return Err(EngineError::Download(format!(
            "the archive does not contain the model files ({})",
            REQUIRED_FILES.join(", ")
        )));
    }
    if final_dir.exists() {
        std::fs::remove_dir_all(&final_dir)?;
    }
    std::fs::rename(&staging, &final_dir)?;
    Ok(final_dir)
}

/// Each request may spend at most this long reading the body. ureq has no
/// idle-read timeout, and without a limit a connection that goes silent
/// (sleep/wake, a Wi-Fi roam) blocks the read forever. When the window runs
/// out, the download resumes with a new Range request, so a slow link only
/// costs an extra request per window.
const BODY_WINDOW: Duration = Duration::from_secs(60);

/// Stream `url` into `part`, resuming from its current length, and keep
/// resuming as long as each attempt makes progress. An attempt that adds
/// nothing (a stall, an HTTP error) ends it with that attempt's error.
pub(crate) fn fetch(
    url: &str,
    part: &Path,
    expected: Option<u64>,
    progress: &mut dyn FnMut(ModelStatus),
) -> Result<(), EngineError> {
    let agent: ureq::Agent = ureq::Agent::config_builder()
        .http_status_as_error(false)
        .timeout_connect(Some(Duration::from_secs(20)))
        .timeout_recv_response(Some(Duration::from_secs(30)))
        .timeout_recv_body(Some(BODY_WINDOW))
        .user_agent(concat!("squawk/", env!("CARGO_PKG_VERSION")))
        .build()
        .into();
    loop {
        let before = part_len(part);
        match fetch_once(&agent, url, part, expected, progress) {
            Ok(()) => return Ok(()),
            Err(e) if part_len(part) > before => {
                log::info!("model: download interrupted ({e}); resuming");
            }
            Err(e) => return Err(e),
        }
    }
}

fn part_len(part: &Path) -> u64 {
    std::fs::metadata(part).map(|m| m.len()).unwrap_or(0)
}

/// One request: stream `url` into `part`, resuming from its current length.
fn fetch_once(
    agent: &ureq::Agent,
    url: &str,
    part: &Path,
    expected: Option<u64>,
    progress: &mut dyn FnMut(ModelStatus),
) -> Result<(), EngineError> {
    let mut have = part_len(part);
    if expected.is_some_and(|e| have == e) {
        return Ok(());
    }
    if expected.is_some_and(|e| have > e) {
        std::fs::remove_file(part)?;
        have = 0;
    }
    let mut req = agent.get(url);
    if have > 0 {
        req = req.header("Range", &format!("bytes={have}-"));
    }
    let resp = req
        .call()
        .map_err(|e| EngineError::Download(e.to_string()))?;
    let status = resp.status().as_u16();
    let header = |name: &str| {
        resp.headers()
            .get(name)
            .and_then(|v| v.to_str().ok())
            .map(str::to_string)
    };
    let content_length = header("content-length").and_then(|v| v.parse::<u64>().ok());
    let content_range = header("content-range");
    let (append, total) = match status {
        206 => {
            let total = content_range
                .as_deref()
                .and_then(parse_content_range_total)
                .or(content_length.map(|l| l + have));
            (true, total)
        }
        200 => (false, content_length),
        416 if have > 0 => {
            // Nothing past what we have: the partial file is complete.
            let total = content_range.as_deref().and_then(parse_content_range_total);
            if total.is_none_or(|t| t == have) {
                return Ok(());
            }
            std::fs::remove_file(part)?;
            return Err(EngineError::Download(
                "the partial download does not match the server; try again".into(),
            ));
        }
        s => return Err(EngineError::Download(format!("HTTP {s} from {url}"))),
    };
    if let (Some(e), Some(t)) = (expected, total) {
        if e != t {
            return Err(EngineError::Download(format!(
                "unexpected size {t} bytes (expected {e})"
            )));
        }
    }
    let mut file = OpenOptions::new()
        .create(true)
        .write(true)
        .truncate(!append)
        .open(part)?;
    let mut done = if append {
        file.seek(std::io::SeekFrom::End(0))?
    } else {
        0
    };
    let mut body = resp.into_body().into_reader();
    let mut buf = vec![0u8; 256 * 1024];
    let mut last = Instant::now() - PROGRESS_EVERY;
    loop {
        let n = body
            .read(&mut buf)
            .map_err(|e| EngineError::Download(format!("connection lost: {e}")))?;
        if n == 0 {
            break;
        }
        file.write_all(&buf[..n])?;
        done += n as u64;
        if last.elapsed() >= PROGRESS_EVERY {
            last = Instant::now();
            progress(ModelStatus::Downloading {
                downloaded: done,
                total,
            });
        }
    }
    file.sync_all()?;
    progress(ModelStatus::Downloading {
        downloaded: done,
        total,
    });
    if let Some(t) = total {
        if done != t {
            return Err(EngineError::Download(format!(
                "incomplete download ({done} of {t} bytes); try again to resume"
            )));
        }
    }
    Ok(())
}

/// `bytes 100-199/1000` or `bytes */1000` → 1000.
fn parse_content_range_total(v: &str) -> Option<u64> {
    v.rsplit('/').next()?.trim().parse().ok()
}

pub(crate) fn verify_sha256(path: &Path, want: &str) -> Result<(), EngineError> {
    let mut file = File::open(path)?;
    let mut hasher = Sha256::new();
    let mut buf = vec![0u8; 1 << 20];
    loop {
        let n = file.read(&mut buf)?;
        if n == 0 {
            break;
        }
        hasher.update(&buf[..n]);
    }
    let got: String = hasher
        .finalize()
        .iter()
        .map(|b| format!("{b:02x}"))
        .collect();
    if got != want {
        return Err(EngineError::Download(format!(
            "checksum mismatch (got {got}); the download was discarded, try again"
        )));
    }
    Ok(())
}

/// Unpack a `.tar.gz` into `dest`. The archive's single top-level
/// directory is stripped; macOS junk (`._*` AppleDouble files, `PaxHeader`
/// directories) and anything that is not a regular file is skipped, and no
/// entry may escape `dest`.
fn extract(reader: impl Read, dest: &Path) -> Result<(), EngineError> {
    let bad = |e: std::io::Error| EngineError::Download(format!("could not unpack: {e}"));
    let mut archive = tar::Archive::new(flate2::read::GzDecoder::new(reader));
    for entry in archive.entries().map_err(bad)? {
        let mut entry = entry.map_err(bad)?;
        if !entry.header().entry_type().is_file() {
            continue;
        }
        let path = entry.path().map_err(bad)?.into_owned();
        let Some(rel) = model_relative_path(&path) else {
            continue;
        };
        let target = dest.join(rel);
        if let Some(dir) = target.parent() {
            std::fs::create_dir_all(dir)?;
        }
        entry.unpack(&target).map_err(bad)?;
    }
    Ok(())
}

/// Where an archive entry goes inside the model directory, or `None` to
/// skip it.
fn model_relative_path(path: &Path) -> Option<PathBuf> {
    let mut parts = Vec::new();
    for c in path.components() {
        match c {
            Component::Normal(p) => {
                let p = p.to_str()?;
                if p.starts_with("._") || p.starts_with("PaxHeader") || p == ".DS_Store" {
                    return None;
                }
                parts.push(p.to_string());
            }
            Component::CurDir => {}
            _ => return None,
        }
    }
    // Strip the top-level directory the tarball wraps everything in.
    if parts.len() > 1 {
        parts.remove(0);
    }
    if parts.is_empty() {
        return None;
    }
    Some(parts.iter().collect())
}

/// Trailing silence: a clip that ends mid-word (fn released right on the
/// last syllable) otherwise sometimes drops that word. (The leading pad
/// Parakeet needs, 250 ms, `transcribe_with` adds itself.)
const TRAIL_PAD_MS: usize = 150;

/// A loaded model. Not `Sync`; lives on the recognizer thread.
pub struct LoadedModel {
    model: ParakeetModel,
}

impl LoadedModel {
    /// Load from `dir` with int8 quantization. Takes ~1–2 s on an M4.
    /// `threads` is accepted for the config's sake, but `transcribe-rs`
    /// builds its ONNX Runtime sessions itself, so ORT always picks.
    pub fn load(dir: &Path, threads: usize) -> Result<LoadedModel, EngineError> {
        if !is_installed(dir) {
            return Err(EngineError::ModelMissing);
        }
        if threads > 0 {
            log::debug!("model: threads = {threads} ignored (ORT decides)");
        }
        let model =
            ParakeetModel::load(dir, &Quantization::Int8).map_err(|e| EngineError::ModelLoad {
                path: dir.to_path_buf(),
                message: e.to_string(),
            })?;
        Ok(LoadedModel { model })
    }

    /// One short inference so ONNX Runtime allocates its buffers now rather
    /// than during the first dictation.
    pub fn warm_up(&mut self) -> Result<(), EngineError> {
        let n = SAMPLE_RATE as usize;
        let noise: Vec<f32> = (0..n)
            .map(|i| ((i * 7919) % 200) as f32 / 200_000.0 - 0.0005)
            .collect();
        self.transcribe(&noise).map(|_| ())
    }

    /// Transcribe 16 kHz mono samples with segment timestamps (seconds from
    /// the start of `samples`).
    pub fn transcribe(&mut self, samples: &[f32]) -> Result<Recognized, EngineError> {
        let pad = TRAIL_PAD_MS * SAMPLE_RATE as usize / 1000;
        let mut padded = Vec::with_capacity(samples.len() + pad);
        padded.extend_from_slice(samples);
        padded.resize(samples.len() + pad, 0.0);
        let params = ParakeetParams {
            timestamp_granularity: Some(TimestampGranularity::Segment),
            ..ParakeetParams::default()
        };
        let result = self
            .model
            .transcribe_with(&padded, &params)
            .map_err(|e| EngineError::Transcribe(e.to_string()))?;
        let len = samples.len() as f32 / SAMPLE_RATE as f32;
        let text = result.text.trim().to_string();
        let segments: Vec<(f32, f32, String)> = result
            .segments
            .unwrap_or_default()
            .into_iter()
            .map(|s| (s.start.min(len), s.end.min(len), s.text.trim().to_string()))
            .filter(|(_, _, t)| !t.is_empty())
            .collect();
        Ok(Recognized {
            segments: respace_segments(&text, segments),
            text,
        })
    }
}

/// Take each segment's text from the full text instead. `transcribe-rs`
/// builds segment text by gluing word tokens, and drops the space before a
/// number ("about700 megabytes"); the full text is decoded properly. The
/// two hold the same characters apart from spaces, so each segment is the
/// next stretch of the full text with as many non-space characters. If they
/// ever disagree, the segments are returned as they were.
fn respace_segments(text: &str, segments: Vec<(f32, f32, String)>) -> Vec<(f32, f32, String)> {
    let squash = |s: &str| s.chars().filter(|c| !c.is_whitespace()).collect::<String>();
    let joined: String = segments.iter().map(|(_, _, t)| squash(t)).collect();
    if joined != squash(text) {
        return segments;
    }
    let mut rest = text.chars().peekable();
    segments
        .into_iter()
        .map(|(start, end, seg)| {
            let mut need = seg.chars().filter(|c| !c.is_whitespace()).count();
            let mut out = String::new();
            while need > 0 {
                let Some(c) = rest.next() else { break };
                if !c.is_whitespace() {
                    need -= 1;
                }
                out.push(c);
            }
            (start, end, out.trim().to_string())
        })
        .collect()
}

/// Model output for one piece of audio.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct Recognized {
    pub text: String,
    /// (start_secs, end_secs, text), relative to the piece.
    pub segments: Vec<(f32, f32, String)>,
}

#[cfg(test)]
mod tests {
    use super::*;

    fn add_file(b: &mut tar::Builder<Vec<u8>>, path: &str, body: &[u8]) {
        let mut h = tar::Header::new_gnu();
        h.set_size(body.len() as u64);
        h.set_mode(0o644);
        h.set_entry_type(tar::EntryType::Regular);
        h.set_cksum();
        b.append_data(&mut h, path, body).unwrap();
    }

    fn tarball(entries: &[(&str, &[u8])]) -> Vec<u8> {
        let mut b = tar::Builder::new(Vec::new());
        for (p, body) in entries {
            add_file(&mut b, p, body);
        }
        let tar = b.into_inner().unwrap();
        let mut gz = flate2::write::GzEncoder::new(Vec::new(), flate2::Compression::fast());
        gz.write_all(&tar).unwrap();
        gz.finish().unwrap()
    }

    fn model_entries() -> Vec<(String, Vec<u8>)> {
        let mut v: Vec<(String, Vec<u8>)> = REQUIRED_FILES
            .iter()
            .map(|f| {
                (
                    format!("parakeet-tdt-0.6b-v3-int8/{f}"),
                    f.as_bytes().to_vec(),
                )
            })
            .collect();
        v.push(("._parakeet-tdt-0.6b-v3-int8".into(), b"junk".to_vec()));
        v.push((
            "parakeet-tdt-0.6b-v3-int8/._encoder-model.int8.onnx".into(),
            b"apple double".to_vec(),
        ));
        v.push((
            "PaxHeader/parakeet-tdt-0.6b-v3-int8".into(),
            b"pax".to_vec(),
        ));
        v.push((
            "parakeet-tdt-0.6b-v3-int8/config.json".into(),
            b"{}".to_vec(),
        ));
        v
    }

    fn write_tarball(dir: &Path, entries: &[(String, Vec<u8>)]) -> PathBuf {
        let refs: Vec<(&str, &[u8])> = entries
            .iter()
            .map(|(p, b)| (p.as_str(), b.as_slice()))
            .collect();
        let path = dir.join("m.tar.gz.part");
        std::fs::write(&path, tarball(&refs)).unwrap();
        path
    }

    #[test]
    fn extracts_and_skips_macos_junk() {
        let tmp = tempfile::tempdir().unwrap();
        let tgz = write_tarball(tmp.path(), &model_entries());
        let dir = install_tarball(&tgz, tmp.path(), "model").unwrap();
        assert!(is_installed(&dir));
        assert_eq!(
            std::fs::read(dir.join("vocab.txt")).unwrap(),
            b"vocab.txt".to_vec()
        );
        assert!(dir.join("config.json").is_file());
        let names: Vec<String> = std::fs::read_dir(&dir)
            .unwrap()
            .map(|e| e.unwrap().file_name().to_string_lossy().into_owned())
            .collect();
        assert!(names.iter().all(|n| !n.starts_with("._")), "{names:?}");
        assert!(!tmp.path().join("model.partial").exists());
        assert!(!tmp.path().join("PaxHeader").exists());
    }

    #[test]
    fn missing_files_leave_no_model_dir() {
        let tmp = tempfile::tempdir().unwrap();
        let mut entries = model_entries();
        entries.retain(|(p, _)| !p.ends_with("nemo128.onnx"));
        let tgz = write_tarball(tmp.path(), &entries);
        let err = install_tarball(&tgz, tmp.path(), "model").unwrap_err();
        assert!(matches!(err, EngineError::Download(_)));
        assert!(!tmp.path().join("model").exists());
        assert!(!tmp.path().join("model.partial").exists());
    }

    #[test]
    fn garbage_is_a_download_error() {
        let tmp = tempfile::tempdir().unwrap();
        let p = tmp.path().join("x.part");
        std::fs::write(&p, b"definitely not gzip").unwrap();
        assert!(matches!(
            install_tarball(&p, tmp.path(), "model"),
            Err(EngineError::Download(_))
        ));
        assert!(!tmp.path().join("model").exists());
    }

    #[test]
    fn a_stale_partial_dir_is_replaced() {
        let tmp = tempfile::tempdir().unwrap();
        std::fs::create_dir_all(tmp.path().join("model.partial/old")).unwrap();
        let tgz = write_tarball(tmp.path(), &model_entries());
        let dir = install_tarball(&tgz, tmp.path(), "model").unwrap();
        assert!(!dir.join("old").exists());
    }

    #[test]
    fn relative_paths() {
        let p = |s: &str| model_relative_path(Path::new(s));
        assert_eq!(p("top/vocab.txt"), Some(PathBuf::from("vocab.txt")));
        assert_eq!(p("./top/sub/a.onnx"), Some(PathBuf::from("sub/a.onnx")));
        assert_eq!(p("vocab.txt"), Some(PathBuf::from("vocab.txt")));
        assert_eq!(p("top/._vocab.txt"), None);
        assert_eq!(p("PaxHeader/top"), None);
        assert_eq!(p("top/../../etc/passwd"), None);
        assert_eq!(p("/abs/path"), None);
    }

    #[test]
    fn content_range_total() {
        assert_eq!(parse_content_range_total("bytes 100-199/1000"), Some(1000));
        assert_eq!(
            parse_content_range_total("bytes */478517071"),
            Some(478_517_071)
        );
        assert_eq!(parse_content_range_total("bytes 0-1/*"), None);
    }

    #[test]
    fn already_installed_needs_no_network() {
        let tmp = tempfile::tempdir().unwrap();
        let dir = tmp.path().join("m");
        std::fs::create_dir_all(&dir).unwrap();
        for f in REQUIRED_FILES {
            std::fs::write(dir.join(f), b"x").unwrap();
        }
        let mut calls = 0;
        let got = download("http://127.0.0.1:9/nope", tmp.path(), "m", &mut |_| {
            calls += 1
        })
        .unwrap();
        assert_eq!(got, dir);
        assert_eq!(calls, 0);
    }

    #[test]
    fn segments_get_the_full_text_spacing() {
        let text = "It is about 700 megabytes. Each track is cut into 30-second chunks.";
        let segs = vec![
            (0.0, 2.0, "It is about700 megabytes.".to_string()),
            (
                2.5,
                5.0,
                "Each track is cut into30-second chunks.".to_string(),
            ),
        ];
        let fixed = respace_segments(text, segs);
        assert_eq!(fixed[0].2, "It is about 700 megabytes.");
        assert_eq!(fixed[1].2, "Each track is cut into 30-second chunks.");
        assert_eq!((fixed[1].0, fixed[1].1), (2.5, 5.0));
        // Disagreeing text leaves the segments alone.
        let odd = vec![(0.0, 1.0, "Something else.".to_string())];
        assert_eq!(respace_segments(text, odd.clone()), odd);
    }

    #[test]
    fn sha256_check() {
        let tmp = tempfile::tempdir().unwrap();
        let p = tmp.path().join("f");
        std::fs::write(&p, b"abc").unwrap();
        verify_sha256(
            &p,
            "ba7816bf8f01cfea414140de5dae2223b00361a396177a9cb410ff61f20015ad",
        )
        .unwrap();
        assert!(verify_sha256(&p, "00").is_err());
    }

    /// A one-file HTTP server: the first response is cut off after `cut`
    /// bytes (a dropped connection); later ones honour `Range`.
    fn flaky_server(data: Vec<u8>, cut: usize) -> String {
        use std::io::{BufRead, BufReader};
        let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        let addr = listener.local_addr().unwrap();
        std::thread::spawn(move || {
            for (i, conn) in listener.incoming().enumerate() {
                let Ok(mut conn) = conn else { return };
                let mut from = 0usize;
                let mut reader = BufReader::new(conn.try_clone().unwrap());
                loop {
                    let mut line = String::new();
                    if reader.read_line(&mut line).unwrap_or(0) == 0 || line == "\r\n" {
                        break;
                    }
                    let lower = line.to_ascii_lowercase();
                    if let Some(r) = lower.strip_prefix("range: bytes=") {
                        from = r.trim().trim_end_matches('-').parse().unwrap();
                    }
                }
                let body = &data[from..];
                let head = if from > 0 {
                    format!(
                        "HTTP/1.1 206 Partial Content\r\nContent-Length: {}\r\nContent-Range: bytes {from}-{}/{}\r\nConnection: close\r\n\r\n",
                        body.len(),
                        data.len() - 1,
                        data.len()
                    )
                } else {
                    format!(
                        "HTTP/1.1 200 OK\r\nContent-Length: {}\r\nConnection: close\r\n\r\n",
                        body.len()
                    )
                };
                let _ = conn.write_all(head.as_bytes());
                let body = if i == 0 { &body[..cut] } else { body };
                let _ = conn.write_all(body);
            }
        });
        format!("http://{addr}/model.tar.gz")
    }

    #[test]
    fn an_interrupted_download_resumes_by_itself() {
        let data: Vec<u8> = (0..200_000u32).map(|i| (i % 251) as u8).collect();
        let url = flaky_server(data.clone(), 70_000);
        let tmp = tempfile::tempdir().unwrap();
        let part = tmp.path().join("m.tar.gz.part");
        let mut last = None;
        fetch(&url, &part, Some(data.len() as u64), &mut |s| {
            last = Some(s)
        })
        .unwrap();
        assert_eq!(std::fs::read(&part).unwrap(), data);
        assert_eq!(
            last,
            Some(ModelStatus::Downloading {
                downloaded: 200_000,
                total: Some(200_000)
            })
        );
    }

    #[test]
    fn a_download_that_makes_no_progress_fails() {
        let tmp = tempfile::tempdir().unwrap();
        let part = tmp.path().join("m.tar.gz.part");
        // Nothing listens on port 9.
        assert!(matches!(
            fetch("http://127.0.0.1:9/nope", &part, None, &mut |_| {}),
            Err(EngineError::Download(_))
        ));
    }

    #[test]
    fn a_second_downloader_waits_and_finds_the_model() {
        let tmp = tempfile::tempdir().unwrap();
        let root = tmp.path().to_path_buf();
        let held = File::create(root.join("m.lock")).unwrap();
        held.lock().unwrap();
        let waiter = {
            let root = root.clone();
            std::thread::spawn(move || download("http://127.0.0.1:9/nope", &root, "m", &mut |_| {}))
        };
        std::thread::sleep(Duration::from_millis(100));
        assert!(!waiter.is_finished());
        // The first downloader finishes installing, then lets go.
        let dir = root.join("m");
        std::fs::create_dir_all(&dir).unwrap();
        for f in REQUIRED_FILES {
            std::fs::write(dir.join(f), b"x").unwrap();
        }
        drop(held);
        assert_eq!(waiter.join().unwrap().unwrap(), dir);
    }
}
