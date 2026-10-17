//! The TV's hardware decoder: access units go straight into StarfishMediaAPIs, which presents
//! them on the video plane beneath the transparent UI.

use std::sync::Arc;
use std::time::{Duration, Instant};

use gfn_starfish::{Event, FeedResult, LoadPayload, OpenConfig, Rect, StarfishVideo, VideoCodec};
use opennow_streamer_platform_webos::{EncodedFrame, MediaStreamConfig, MediaVideoCodec};

use crate::{Decoded, DecoderFactory, VideoDecoder};

struct StarfishDecoder {
    video: StarfishVideo,
    opened: Instant,
    presented: u64,
    stats: Stats,
    /// This second's plane-lag samples (start, sum, count), published for the stats overlay.
    lag_window: (Instant, i64, i64),
}

fn sample_lag(window: &mut (Instant, i64, i64), lag: i64) {
    let (since, sum, count) = window;
    *sum += lag;
    *count += 1;
    if since.elapsed() >= Duration::from_secs(1) {
        crate::set_display_lag_ms(Some(*sum / *count));
        *window = (Instant::now(), 0, 0);
    }
}

impl Drop for StarfishDecoder {
    fn drop(&mut self) {
        crate::set_display_lag_ms(None);
    }
}

/// Periodic feed-vs-presentation report: how far the plane trails what was fed is the
/// stream's display latency inside the TV, which nothing else measures.
struct Stats {
    since: Instant,
    fed: u64,
    full: u64,
    /// How long after feeding each position report showed a frame on the plane, in ms: the
    /// TV's display latency, which nothing else measures. Sampled at the report itself, since the
    /// position only arrives every 200 ms.
    lag_min: i64,
    lag_max: i64,
    lag_sum: i64,
    lag_samples: i64,
    /// The video sink's own counters (callback types 47 / 46, `streamQualityInfo`).
    sink_displayed: i64,
    sink_dropped: i64,
    max_feed_us: u128,
    max_gap_us: u128,
    last_feed: Option<Instant>,
}

impl Stats {
    fn new() -> Self {
        Self {
            since: Instant::now(),
            fed: 0,
            full: 0,
            lag_min: i64::MAX,
            lag_max: 0,
            lag_sum: 0,
            lag_samples: 0,
            sink_displayed: 0,
            sink_dropped: 0,
            max_feed_us: 0,
            max_gap_us: 0,
            last_feed: None,
        }
    }

    fn report(&mut self, presented: u64) {
        if self.since.elapsed() < Duration::from_secs(5) {
            return;
        }
        eprintln!(
            "starfish: fed={} presented={} buffer-full-drops={} plane-lag avg={}ms min={}ms max={}ms sink-displayed={} sink-dropped={} max-feed={}us max-gap={}us",
            self.fed,
            presented,
            self.full,
            self.lag_sum / self.lag_samples.max(1),
            if self.lag_samples > 0 { self.lag_min } else { 0 },
            self.lag_max,
            self.sink_displayed,
            self.sink_dropped,
            self.max_feed_us,
            self.max_gap_us,
        );
        *self = Self {
            last_feed: self.last_feed,
            ..Self::new()
        };
    }
}

impl VideoDecoder for StarfishDecoder {
    fn decode(&mut self, frame: &EncodedFrame) -> Result<Decoded, String> {
        // Arrival time, not the stream's RTP clock: the pipeline paces presentation by PTS, and
        // stream timestamps carry whatever head start the stream had over Load/Play, which it then
        // holds as a constant delay (measured 9 s). Arrival-stamped frames are always due now
        // (moonlight-tv's ss4s does the same).
        let pts = i64::try_from(self.opened.elapsed().as_nanos()).unwrap_or(i64::MAX);
        // A full source buffer means the decoder is behind; waiting only adds latency.
        let started = Instant::now();
        if let Some(last) = self.stats.last_feed {
            self.stats.max_gap_us = self.stats.max_gap_us.max((started - last).as_micros());
        }
        self.stats.last_feed = Some(started);
        let result = self.video.feed(&frame.data, pts);
        self.stats.max_feed_us = self.stats.max_feed_us.max(started.elapsed().as_micros());
        self.stats.fed += 1;
        if result == FeedResult::BufferFull {
            self.stats.full += 1;
        }
        let mut pipeline_error = None;
        for event in self.video.events().try_iter() {
            match event {
                Event::FrameReady { pts } => {
                    self.presented += 1;
                    let now = i64::try_from(self.opened.elapsed().as_nanos()).unwrap_or(i64::MAX);
                    let lag = (now - pts) / 1_000_000;
                    let stats = &mut self.stats;
                    (stats.lag_min, stats.lag_max) = (stats.lag_min.min(lag), stats.lag_max.max(lag));
                    stats.lag_sum += lag;
                    stats.lag_samples += 1;
                    sample_lag(&mut self.lag_window, lag);
                }
                Event::Error { code, message } => {
                    pipeline_error.get_or_insert(format!("pipeline error {code}: {message}"));
                }
                Event::Other { kind: 47, num, .. } => self.stats.sink_displayed += num,
                Event::Other { kind: 46, num, .. } => self.stats.sink_dropped += num,
                Event::Other { .. } => {}
            }
        }
        self.stats.report(self.presented);
        if let Some(error) = pipeline_error {
            return Err(error);
        }
        match result {
            FeedResult::Accepted => Ok(Decoded::Accepted),
            FeedResult::BufferFull => Ok(Decoded::Skipped),
            FeedResult::Error => Err("Feed rejected the access unit".into()),
        }
    }

    fn presented_frames(&self) -> u64 {
        self.presented
    }
}

pub fn decoder_factory(app_id: String, display: Rect) -> Arc<DecoderFactory> {
    Arc::new(move |config: &MediaStreamConfig| {
        let codec = match config.codec {
            MediaVideoCodec::H264 => VideoCodec::H264,
            MediaVideoCodec::H265 => VideoCodec::H265,
            MediaVideoCodec::Av1 => return Err("AV1 has no Starfish path".into()),
        };
        let video = StarfishVideo::open(OpenConfig {
            app_id: app_id.clone(),
            payload: tuned(LoadPayload::live(codec, config.width, config.height, config.fps)),
            display,
        })?;
        Ok(Box::new(StarfishDecoder { video, opened: Instant::now(), presented: 0, stats: Stats::new(), lag_window: (Instant::now(), 0, 0) }) as Box<dyn VideoDecoder>)
    })
}

/// Developer aid for latency experiments: `key=value` lines in this file override the load
/// payload at each stream start, so variants are compared without reinstalling. The app jail's
/// `/tmp` is the TV's.
const TUNING_FILE: &str = "/tmp/io.github.gfnwebos.starfish";

fn tuned(mut payload: LoadPayload) -> LoadPayload {
    let Ok(tuning) = std::fs::read_to_string(TUNING_FILE) else { return payload };
    for line in tuning.lines() {
        let Some((key, value)) = line.split_once('=') else { continue };
        let (key, value) = (key.trim(), value.trim());
        let flag = value == "true";
        match key {
            "provider" => payload.provider = value.to_owned(),
            "contents_type" => payload.contents_type = if value == "LIVE" { "LIVE" } else { "WEBRTC" },
            "video_buffer_max" => payload.video_buffer_max = value.parse().unwrap_or(payload.video_buffer_max),
            "low_delay" => payload.low_delay = flag,
            "audio_sync" => payload.audio_sync = flag,
            "pause_at_decode_time" => payload.pause_at_decode_time = flag,
            "corrupted_frame_info" => payload.corrupted_frame_info = flag,
            _ => eprintln!("starfish: {TUNING_FILE}: unknown key {key}"),
        }
    }
    eprintln!("starfish: tuned payload {payload:?}");
    payload
}
