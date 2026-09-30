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
//! for a good one. Delete the tarball afterwards. The only network access
//! squawk ever makes.

use std::path::{Path, PathBuf};

use squawk_core::ModelStatus;

use crate::error::EngineError;

/// The files `transcribe_rs::onnx::parakeet::ParakeetModel::load` needs with
/// `Quantization::Int8`.
pub const REQUIRED_FILES: &[&str] = &[
    "encoder-model.int8.onnx",
    "decoder_joint-model.int8.onnx",
    "nemo128.onnx",
    "vocab.txt",
];

/// Every required file is present in `dir`.
pub fn is_installed(dir: &Path) -> bool {
    REQUIRED_FILES.iter().all(|f| dir.join(f).is_file())
}

/// Download and unpack the model into `models_dir/dir_name`, reporting
/// `Downloading`/`Extracting` through `progress`. Blocking. Returns the
/// model directory. Already installed: returns at once.
pub fn download(
    url: &str,
    models_dir: &Path,
    dir_name: &str,
    progress: &mut dyn FnMut(ModelStatus),
) -> Result<PathBuf, EngineError> {
    let _ = (url, models_dir, dir_name, progress);
    todo!("engine agent")
}

/// A loaded model. Not `Sync`; lives on the recognizer thread.
pub struct LoadedModel {
    _private: (),
}

impl LoadedModel {
    /// Load from `dir` with int8 quantization. `threads` = 0 lets ORT pick.
    /// Takes ~1–2 s on an M4.
    pub fn load(dir: &Path, threads: usize) -> Result<LoadedModel, EngineError> {
        let _ = (dir, threads);
        todo!("engine agent")
    }

    /// Transcribe 16 kHz mono samples with segment timestamps (seconds from
    /// the start of `samples`).
    pub fn transcribe(&mut self, samples: &[f32]) -> Result<Recognized, EngineError> {
        let _ = samples;
        todo!("engine agent")
    }
}

/// Model output for one piece of audio.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct Recognized {
    pub text: String,
    /// (start_secs, end_secs, text), relative to the piece.
    pub segments: Vec<(f32, f32, String)>,
}
