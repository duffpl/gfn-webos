//! The NVST engine's media backend for webOS and the desktop simulator.
//!
//! The engine pushes compressed access units and Opus packets; this crate queues them (bounded,
//! drop-oldest), decodes them on dedicated threads and reports the decoder state the engine turns
//! into NVST protocol traffic (frame acks, keyframe requests, drop counters).
//!
//! Video decoders are created ON the video thread from a factory, because the TV's Starfish
//! session must be driven entirely by the thread that loaded it.

mod audio;
#[cfg(unix)]
#[cfg_attr(not(all(target_os = "linux", target_arch = "arm")), allow(dead_code))]
pub mod ndl;
mod video;
#[cfg(feature = "sim-video")]
pub mod sim;
#[cfg(all(feature = "sim-video", target_os = "macos"))]
pub mod vt;
#[cfg(all(target_os = "linux", target_arch = "arm"))]
pub mod starfish;

use std::sync::Arc;
use std::sync::atomic::{AtomicBool, AtomicI64, Ordering};
use std::sync::mpsc::Sender;

use opennow_streamer_platform_webos::{
    EncodedFrame, MediaBackend, MediaBackendSession, MediaFeedback, PushOutcome,
};
use opennow_streamer_protocol::{CodecCapability, VideoBackendCapability};

pub use gfn_sdl::AudioOut;
pub use video::{VideoStats, VideoStatsWindow};
pub use opennow_streamer_platform_webos::{MediaRuntime, MediaStreamConfig, MediaVideoCodec, create_runtime};

/// Milliseconds from handing a frame to the decoder to seeing it on screen, averaged over the last
/// second; negative while nothing measures it (only the TV's decoder reports presentation).
static DISPLAY_LAG_MS: AtomicI64 = AtomicI64::new(-1);

pub fn display_lag_ms() -> Option<i64> {
    Some(DISPLAY_LAG_MS.load(Ordering::Relaxed)).filter(|ms| *ms >= 0)
}

#[cfg_attr(not(all(target_os = "linux", target_arch = "arm")), allow(dead_code))]
fn set_display_lag_ms(ms: Option<i64>) {
    DISPLAY_LAG_MS.store(ms.unwrap_or(-1), Ordering::Relaxed);
}

/// Whether the next stream on the TV plays through NDL instead of Starfish. Chosen per session in
/// Settings, while the backend is built once at startup.
static USE_NDL: AtomicBool = AtomicBool::new(false);

pub fn set_use_ndl(enabled: bool) {
    USE_NDL.store(enabled, Ordering::Relaxed);
}

pub fn use_ndl() -> bool {
    USE_NDL.load(Ordering::Relaxed)
}

/// While another app has the screen the TV takes the video plane away, and a player fed through
/// that keeps showing black after the app returns; the video thread closes its decoder for the
/// duration and opens a fresh one afterwards (PlxNative's suspend/reload on the same events).
static BACKGROUNDED: AtomicBool = AtomicBool::new(false);

pub fn set_backgrounded(backgrounded: bool) {
    BACKGROUNDED.store(backgrounded, Ordering::Relaxed);
}

fn backgrounded() -> bool {
    BACKGROUNDED.load(Ordering::Relaxed)
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Decoded {
    Accepted,
    /// The decoder had no room and the access unit was dropped; the reference chain is broken
    /// until the next keyframe, but the decoder itself is healthy.
    Skipped,
}

/// A decoder driven by the video thread. `decode` must not block for long: the queue in front of
/// it is what keeps latency bounded.
pub trait VideoDecoder {
    /// Hands one Annex-B access unit to the decoder. `Err` is a decoder failure.
    fn decode(&mut self, frame: &EncodedFrame) -> Result<Decoded, String>;

    /// Pictures the decoder has put on screen so far.
    fn presented_frames(&self) -> u64;
}

pub type DecoderFactory = dyn Fn(&MediaStreamConfig) -> Result<Box<dyn VideoDecoder>, String> + Send + Sync;

pub struct SinkBackend {
    name: &'static str,
    platform: &'static str,
    codecs: Vec<MediaVideoCodec>,
    /// Codecs whose 10-bit 4:2:0 HDR10 the decoder and display can show.
    hdr_codecs: Vec<MediaVideoCodec>,
    decoder: Arc<DecoderFactory>,
    audio: Option<Arc<AudioOut>>,
    stats: Arc<VideoStats>,
}

impl SinkBackend {
    /// `name` is what the engine reports as the playback backend; `codecs` what the decoder can
    /// reproduce exactly at 8-bit 4:2:0 SDR. `audio` is `None` when there is no output device.
    pub fn new(
        name: &'static str,
        platform: &'static str,
        codecs: Vec<MediaVideoCodec>,
        decoder: Arc<DecoderFactory>,
        audio: Option<Arc<AudioOut>>,
    ) -> Self {
        Self { name, platform, codecs, hdr_codecs: Vec::new(), decoder, audio, stats: Arc::default() }
    }

    pub fn with_hdr(mut self, codecs: Vec<MediaVideoCodec>) -> Self {
        self.hdr_codecs = codecs;
        self
    }

    /// Decode timing and queue depth across every session this backend starts.
    pub fn video_stats(&self) -> Arc<VideoStats> {
        Arc::clone(&self.stats)
    }
}

impl MediaBackend for SinkBackend {
    fn video_backends(&self) -> Vec<VideoBackendCapability> {
        let codecs = [MediaVideoCodec::H264, MediaVideoCodec::H265, MediaVideoCodec::Av1]
            .into_iter()
            .map(|codec| {
                let available = self.codecs.contains(&codec);
                let hdr = available && self.hdr_codecs.contains(&codec);
                let color_qualities = match (available, hdr) {
                    (false, _) => Vec::new(),
                    (true, false) => vec!["8bit_420"],
                    (true, true) => vec!["8bit_420", "10bit_420"],
                };
                CodecCapability {
                    codec: codec.label(),
                    available,
                    color_qualities: Some(color_qualities),
                    hdr_color_qualities: hdr.then(|| vec!["10bit_420"]),
                    hdr_supported: Some(hdr),
                    reason: (!available).then_some("not supported by this decoder"),
                }
            })
            .collect();
        vec![VideoBackendCapability {
            backend: self.name,
            platform: self.platform,
            codecs,
            zero_copy_modes: Vec::new(),
            available: !self.codecs.is_empty(),
            reason: None,
        }]
    }

    fn start(
        &self,
        config: &MediaStreamConfig,
        feedback: Sender<MediaFeedback>,
    ) -> Result<Box<dyn MediaBackendSession>, String> {
        if !self.codecs.contains(&config.codec) {
            return Err(format!("{} decode is unavailable", config.codec.label()));
        }
        let ten_bit_hdr = self.hdr_codecs.contains(&config.codec) && config.color_quality.bit_depth() == 10;
        if config.color_quality.is_444() || (config.color_quality.bit_depth() != 8 && !ten_bit_hdr) || (config.hdr && !ten_bit_hdr) {
            return Err(format!("unsupported color mode {}", config.color_quality.protocol_name()));
        }
        let video = video::VideoPipeline::start(
            self.name,
            config,
            Arc::clone(&self.decoder),
            feedback.clone(),
            Arc::clone(&self.stats),
        )?;
        let audio = match &self.audio {
            Some(output) => Some(audio::AudioPipeline::start(Arc::clone(output), feedback)),
            None => {
                let _ = feedback.send(MediaFeedback::AudioUnavailable {
                    backend: self.name,
                    reason: "no audio output device".into(),
                    rejected: 0,
                });
                None
            }
        };
        Ok(Box::new(Session { video, audio }))
    }
}

struct Session {
    video: video::VideoPipeline,
    audio: Option<audio::AudioPipeline>,
}

impl MediaBackendSession for Session {
    fn push_video(&self, frame: EncodedFrame) -> PushOutcome {
        self.video.push(frame)
    }

    fn push_audio(&self, frame: EncodedFrame) -> PushOutcome {
        match &self.audio {
            Some(audio) => audio.push(frame),
            None => PushOutcome::Unsupported,
        }
    }

    fn set_paused(&self, paused: bool) {
        self.video.set_paused(paused);
        if let Some(audio) = &self.audio {
            audio.set_paused(paused);
        }
    }

    fn stop(&self) {
        self.video.stop();
        if let Some(audio) = &self.audio {
            audio.stop();
        }
    }
}
