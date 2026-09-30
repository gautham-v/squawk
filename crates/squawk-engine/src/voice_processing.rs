//! Echo-cancelled mic for meetings: Apple's voice processing I/O through
//! `AVAudioEngine` (`inputNode.setVoiceProcessingEnabled(true)`).
//!
//! On speakers the other side of a call comes back in through the mic. The
//! voice processing unit takes out whatever the output device is playing,
//! whichever app plays it, so the call audio never needs to pass through
//! squawk. Measured on a MacBook Pro's built-in speakers and mic with `say`
//! at a normal volume: the leaked speech drops by about 30 dB and no longer
//! transcribes.
//!
//! The costs, and why dictation does not use it:
//! - macOS ducks other audio while voice processing runs. At the minimum
//!   level set here (advanced ducking off) the call is ~8 dB quieter, both
//!   in the speakers and in the ScreenCaptureKit "Them" track (the default
//!   level is ~30 dB).
//! - Every other client of the mic gets a signal ~40 dB down meanwhile;
//!   `audio::MicShare` lends this stream to a dictation instead.
//! - It uses the system default input and output devices. A named
//!   `input_device` that is not the default gets plain capture instead (see
//!   `audio::open_stream`).
//! - Opening takes ~0.6–0.7 s (a plain stream ~0.1 s), and it costs ~22 % of
//!   one core in-process plus ~8 % in coreaudiod on an M4 (plain: ~3 %).
//! - The tap delivers 100 ms blocks.
//!
//! Only channel 0 of the input node is used: with voice processing on, the
//! node reports several channels (9 on a MacBook Pro) and the processed
//! voice is the first.
//!
//! The engine stops itself when the hardware changes (a headset connects,
//! the default device changes) and posts
//! `AVAudioEngineConfigurationChangeNotification`; that is reported through
//! `on_change` so the caller reopens, exactly as for a lost cpal stream.

use std::ptr::NonNull;

use block2::RcBlock;
use crossbeam_channel::Sender;
use objc2::rc::Retained;
use objc2::runtime::{AnyObject, NSObjectProtocol, ProtocolObject};
use objc2_avf_audio::{
    AVAudioEngine, AVAudioEngineConfigurationChangeNotification, AVAudioPCMBuffer, AVAudioTime,
    AVAudioVoiceProcessingOtherAudioDuckingConfiguration,
    AVAudioVoiceProcessingOtherAudioDuckingLevel,
};
use objc2_foundation::{NSNotification, NSNotificationCenter};

use crate::error::EngineError;

/// A running voice-processed input. Not `Send`: it lives on the mic capture
/// thread, like a cpal stream. Dropping it stops the engine.
pub(crate) struct VoiceInput {
    engine: Retained<AVAudioEngine>,
    observer: Option<Retained<ProtocolObject<dyn NSObjectProtocol>>>,
    rate: u32,
}

impl VoiceInput {
    /// Start the default input with echo cancellation. Mono f32 blocks at
    /// [`VoiceInput::rate`] go to `raw_tx`; `on_change` is called (on an
    /// AVFAudio queue) when the engine has stopped because the hardware
    /// changed.
    pub(crate) fn open(
        raw_tx: Sender<Vec<f32>>,
        on_change: impl Fn() + Send + Sync + 'static,
    ) -> Result<VoiceInput, EngineError> {
        let fail = |what: &str, e: &dyn std::fmt::Display| {
            EngineError::Mic(format!("voice processing: {what}: {e}"))
        };
        // SAFETY: plain AVFAudio calls on objects this thread owns; the tap
        // block only reads the buffer it is handed, for the duration of the
        // call.
        unsafe {
            let engine = AVAudioEngine::new();
            // The output side has to exist before voice processing is
            // switched on, or starting fails with -10875
            // (kAudioUnitErr_FailedInitialization).
            let _ = engine.mainMixerNode();
            let input = engine.inputNode();
            input
                .setVoiceProcessingEnabled_error(true)
                .map_err(|e| fail("enable", &*e))?;
            // Keep the call as loud as the unit allows (macOS 14+).
            input.setVoiceProcessingOtherAudioDuckingConfiguration(
                AVAudioVoiceProcessingOtherAudioDuckingConfiguration {
                    enableAdvancedDucking: false.into(),
                    duckingLevel: AVAudioVoiceProcessingOtherAudioDuckingLevel::Min,
                },
            );
            let format = input.outputFormatForBus(0);
            let rate = format.sampleRate().round() as u32;
            if rate == 0 || format.channelCount() == 0 {
                return Err(EngineError::Mic("voice processing: no input format".into()));
            }
            let tap = RcBlock::new(
                move |buffer: NonNull<AVAudioPCMBuffer>, _: NonNull<AVAudioTime>| {
                    let buffer = buffer.as_ref();
                    let frames = buffer.frameLength() as usize;
                    let channels = buffer.floatChannelData();
                    if frames == 0 || channels.is_null() {
                        return;
                    }
                    let first = std::slice::from_raw_parts((*channels).as_ptr(), frames);
                    let _ = raw_tx.send(first.to_vec());
                },
            );
            input.installTapOnBus_bufferSize_format_block(
                0,
                // A hint; macOS delivers ~100 ms blocks whatever is asked.
                1024,
                None,
                RcBlock::as_ptr(&tap),
            );
            let changed = RcBlock::new(move |_: NonNull<NSNotification>| on_change());
            let observer = NSNotificationCenter::defaultCenter()
                .addObserverForName_object_queue_usingBlock(
                    Some(AVAudioEngineConfigurationChangeNotification),
                    Some(&engine),
                    None,
                    &changed,
                );
            let mut voice = VoiceInput {
                engine,
                observer: Some(observer),
                rate,
            };
            voice.engine.prepare();
            if let Err(e) = voice.engine.startAndReturnError() {
                voice.shutdown();
                return Err(fail("start", &*e));
            }
            Ok(voice)
        }
    }

    /// Sample rate of the blocks sent to `raw_tx`.
    pub(crate) fn rate(&self) -> u32 {
        self.rate
    }

    fn shutdown(&mut self) {
        // SAFETY: as in `open`; the observer was registered by `open`.
        unsafe {
            if let Some(observer) = self.observer.take() {
                let observer: &AnyObject = (*observer).as_ref();
                NSNotificationCenter::defaultCenter().removeObserver(observer);
            }
            self.engine.inputNode().removeTapOnBus(0);
            self.engine.stop();
        }
    }
}

impl Drop for VoiceInput {
    fn drop(&mut self) {
        self.shutdown();
    }
}
