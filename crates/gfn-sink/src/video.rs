use std::collections::VecDeque;
use std::sync::mpsc::{Sender, channel};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Condvar, Mutex};
use std::thread::JoinHandle;
use std::time::{Duration, Instant};

use opennow_streamer_platform_webos::{EncodedFrame, MediaFeedback, MediaStreamConfig, PushOutcome};

use crate::{Decoded, DecoderFactory, VideoDecoder};

/// Frames queued ahead of the decoder. A quarter second at the stream rate absorbs a recovery
/// burst; anything older is worth less than a fresh keyframe (same policy as OpenNOW's desktop).
fn queue_capacity(fps: u32) -> usize {
    (fps.max(1).div_ceil(4) as usize).clamp(2, 60)
}

/// Running totals the embedder samples for diagnostics; `take` resets the window.
#[derive(Debug, Default)]
pub struct VideoStats {
    decoded: AtomicU64,
    decode_ns: AtomicU64,
    decode_ns_max: AtomicU64,
    /// Time from `push` to the decoder picking the frame up.
    wait_ns: AtomicU64,
    depth_max: AtomicU64,
    dropped: AtomicU64,
}

#[derive(Debug, Clone, Copy, Default, PartialEq)]
pub struct VideoStatsWindow {
    pub decoded: u64,
    pub decode_avg_ms: f64,
    pub decode_max_ms: f64,
    pub queue_wait_avg_ms: f64,
    pub queue_depth_max: u64,
    pub dropped: u64,
}

impl VideoStats {
    pub fn take(&self) -> VideoStatsWindow {
        let decoded = self.decoded.swap(0, Ordering::Relaxed);
        let per_frame = |ns: u64| if decoded == 0 { 0.0 } else { ns as f64 / decoded as f64 / 1e6 };
        VideoStatsWindow {
            decoded,
            decode_avg_ms: per_frame(self.decode_ns.swap(0, Ordering::Relaxed)),
            decode_max_ms: self.decode_ns_max.swap(0, Ordering::Relaxed) as f64 / 1e6,
            queue_wait_avg_ms: per_frame(self.wait_ns.swap(0, Ordering::Relaxed)),
            queue_depth_max: self.depth_max.swap(0, Ordering::Relaxed),
            dropped: self.dropped.swap(0, Ordering::Relaxed),
        }
    }

    fn record(&self, wait_ns: u64, decode_ns: u64) {
        self.decoded.fetch_add(1, Ordering::Relaxed);
        self.wait_ns.fetch_add(wait_ns, Ordering::Relaxed);
        self.decode_ns.fetch_add(decode_ns, Ordering::Relaxed);
        self.decode_ns_max.fetch_max(decode_ns, Ordering::Relaxed);
    }
}

#[derive(Default)]
struct State {
    frames: VecDeque<(EncodedFrame, Instant)>,
    paused: bool,
    stopped: bool,
    /// Set when a reference was lost; non-key frames are discarded until a keyframe arrives so
    /// corrupt pictures are never presented.
    awaiting_keyframe: bool,
}

struct Shared {
    state: Mutex<State>,
    wake: Condvar,
    feedback: Sender<MediaFeedback>,
    capacity: usize,
    stats: Arc<VideoStats>,
}

impl Shared {
    fn lock(&self) -> std::sync::MutexGuard<'_, State> {
        self.state.lock().unwrap_or_else(std::sync::PoisonError::into_inner)
    }

    fn request_keyframe(&self, mid: &str, reason: &str) {
        let _ = self.feedback.send(MediaFeedback::RequestKeyframe { mid: mid.to_owned(), reason: reason.to_owned() });
    }

    fn dropped(&self, count: usize) {
        self.stats.dropped.fetch_add(count as u64, Ordering::Relaxed);
        let _ = self.feedback.send(MediaFeedback::QueueDropped { media: "video", count });
    }
}

pub struct VideoPipeline {
    shared: Arc<Shared>,
    worker: Mutex<Option<JoinHandle<()>>>,
}

impl VideoPipeline {
    pub fn start(
        backend: &'static str,
        config: &MediaStreamConfig,
        factory: Arc<DecoderFactory>,
        feedback: Sender<MediaFeedback>,
        stats: Arc<VideoStats>,
    ) -> Result<Self, String> {
        let shared = Arc::new(Shared {
            state: Mutex::new(State { awaiting_keyframe: true, ..State::default() }),
            wake: Condvar::new(),
            feedback,
            capacity: queue_capacity(config.fps),
            stats,
        });
        let (ready_tx, ready_rx) = channel();
        let worker_shared = Arc::clone(&shared);
        let config = *config;
        let worker = std::thread::Builder::new()
            .name("gfn-video".into())
            .spawn(move || match factory(&config) {
                Ok(decoder) => {
                    let _ = ready_tx.send(Ok(()));
                    run(backend, &worker_shared, factory.as_ref(), decoder, &config);
                }
                Err(error) => {
                    let _ = ready_tx.send(Err(error));
                }
            })
            .map_err(|e| e.to_string())?;
        match ready_rx.recv() {
            Ok(Ok(())) => Ok(Self { shared, worker: Mutex::new(Some(worker)) }),
            Ok(Err(error)) => {
                let _ = worker.join();
                Err(error)
            }
            Err(_) => Err("video thread exited during decoder start".into()),
        }
    }

    pub fn push(&self, frame: EncodedFrame) -> PushOutcome {
        let mut state = self.shared.lock();
        if state.stopped {
            return PushOutcome::Closed;
        }
        if state.paused {
            return PushOutcome::Paused;
        }
        let mut outcome = PushOutcome::Queued;
        if !frame.contiguous && !frame.keyframe && !state.awaiting_keyframe {
            state.awaiting_keyframe = true;
            self.shared.request_keyframe(&frame.mid, "compressed video discontinuity");
        }
        if state.frames.len() >= self.shared.capacity {
            let evicted = state.frames.len() + 1 - self.shared.capacity;
            state.frames.drain(..evicted);
            self.shared.dropped(evicted);
            if !state.awaiting_keyframe {
                state.awaiting_keyframe = true;
                self.shared.request_keyframe(&frame.mid, "video queue overflow");
            }
            outcome = PushOutcome::DroppedOldest;
        }
        state.frames.push_back((frame, Instant::now()));
        self.shared.stats.depth_max.fetch_max(state.frames.len() as u64, Ordering::Relaxed);
        drop(state);
        self.shared.wake.notify_one();
        outcome
    }

    pub fn set_paused(&self, paused: bool) {
        let mut state = self.shared.lock();
        state.paused = paused;
        if paused {
            let dropped = state.frames.len();
            state.frames.clear();
            if dropped > 0 {
                self.shared.dropped(dropped);
            }
        } else {
            // Resuming needs a fresh reference; the engine relays this as an NVST keyframe request.
            state.awaiting_keyframe = true;
            self.shared.request_keyframe("", "resume after pause");
        }
    }

    pub fn stop(&self) {
        self.shared.lock().stopped = true;
        self.shared.wake.notify_all();
        let worker = self.worker.lock().unwrap_or_else(std::sync::PoisonError::into_inner).take();
        if let Some(worker) = worker {
            let _ = worker.join();
        }
    }
}

/// How often a closed decoder is reopened; the TV may hand the video resources back a little
/// after the app is in front again.
const REOPEN_RETRY: Duration = Duration::from_millis(500);
/// A decoder error ends the session (the core reports it as a stream error), so it is reported
/// only once reopening has kept failing this long.
const REOPEN_GIVE_UP: Duration = Duration::from_secs(10);

fn run(
    backend: &'static str,
    shared: &Shared,
    factory: &DecoderFactory,
    decoder: Box<dyn VideoDecoder>,
    config: &MediaStreamConfig,
) {
    let codec = config.codec.label();
    let mut started = false;
    let mut decoder = Some(decoder);
    let mut last_reopen: Option<Instant> = None;
    // Since when the decoder has been needed but missing, and the error that took it away.
    let mut broken: Option<(Instant, String)> = None;
    loop {
        let (frame, queued_at) = {
            let mut state = shared.lock();
            loop {
                if state.stopped {
                    return;
                }
                if let Some((frame, queued_at)) = state.frames.pop_front() {
                    // Frames while backgrounded are discarded unreported: they are not losses the
                    // server should adapt its bitrate to.
                    if crate::backgrounded() || decoder.is_none() {
                        state.awaiting_keyframe = true;
                        break (frame, queued_at);
                    }
                    if state.awaiting_keyframe && !frame.keyframe {
                        shared.dropped(1);
                        continue;
                    }
                    state.awaiting_keyframe = false;
                    break (frame, queued_at);
                }
                state = shared.wake.wait(state).unwrap_or_else(std::sync::PoisonError::into_inner);
            }
        };
        if crate::backgrounded() {
            if decoder.take().is_some() {
                eprintln!("video: app in background, {backend} decoder closed");
            }
            broken = None;
            continue;
        }
        if decoder.is_none() {
            let (since, _) = broken.get_or_insert_with(|| (Instant::now(), "decoder closed in background".into()));
            let since = *since;
            if last_reopen.is_some_and(|at| at.elapsed() < REOPEN_RETRY) {
                continue;
            }
            last_reopen = Some(Instant::now());
            match factory(config) {
                Ok(reopened) => {
                    eprintln!("video: {backend} decoder reopened");
                    decoder = Some(reopened);
                    broken = None;
                }
                Err(message) => {
                    if since.elapsed() >= REOPEN_GIVE_UP {
                        let cause = broken.take().map(|(_, cause)| cause).unwrap_or_default();
                        let message = format!("{cause}; reopening failed: {message}");
                        let _ = shared.feedback.send(MediaFeedback::DecoderError { codec, message });
                    }
                    continue;
                }
            }
            if !frame.keyframe {
                // awaiting_keyframe is still set, so the queue skips to the keyframe asked for here.
                shared.request_keyframe(&frame.mid, "decoder reopened");
                continue;
            }
            shared.lock().awaiting_keyframe = false;
        }
        let Some(active) = decoder.as_mut() else { continue };
        let decode_started = Instant::now();
        let decoded = active.decode(&frame);
        let nanos = |d: std::time::Duration| u64::try_from(d.as_nanos()).unwrap_or(u64::MAX);
        shared.stats.record(nanos(decode_started - queued_at), nanos(decode_started.elapsed()));
        match decoded {
            Ok(Decoded::Skipped) => {
                shared.dropped(1);
                shared.lock().awaiting_keyframe = true;
                shared.request_keyframe(&frame.mid, "decoder buffer full");
            }
            Ok(Decoded::Accepted) => {
                let _ = shared.feedback.send(MediaFeedback::VideoFrameAccepted {
                    frame_index: frame.frame_index,
                    timestamp: frame.timestamp,
                    bytes: u32::try_from(frame.data.len()).unwrap_or(u32::MAX),
                    keyframe: frame.keyframe,
                });
            }
            Err(message) => {
                // Reported only if reopening fails too: a fresh pipeline usually recovers, and the
                // report would end the session.
                eprintln!("video: {backend} decoder failed, reopening: {message}");
                shared.lock().awaiting_keyframe = true;
                broken = Some((Instant::now(), message));
            }
        }
        if broken.is_some() {
            decoder = None;
            continue;
        }
        if !started && active.presented_frames() > 0 {
            started = true;
            let _ = shared.feedback.send(MediaFeedback::PlaybackStarted { backend });
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use opennow_streamer_platform_webos::{MediaCodec, MediaFeedback};
    use std::sync::atomic::{AtomicU64, Ordering};
    use std::sync::mpsc::Receiver;
    use std::time::Duration;

    /// While the gate is closed `decode` blocks, which pins the worker so queue states are exact.
    type Gate = Arc<(Mutex<bool>, Condvar)>;

    struct Recorder(Arc<Mutex<Vec<u32>>>, Arc<AtomicU64>, Gate);

    impl VideoDecoder for Recorder {
        fn decode(&mut self, frame: &EncodedFrame) -> Result<Decoded, String> {
            self.0.lock().unwrap().push(frame.frame_index.unwrap());
            self.1.fetch_add(1, Ordering::Relaxed);
            let (open, changed) = &*self.2;
            let _open = changed.wait_while(open.lock().unwrap(), |open| !*open).unwrap();
            Ok(Decoded::Accepted)
        }
        fn presented_frames(&self) -> u64 {
            self.1.load(Ordering::Relaxed)
        }
    }

    fn frame(index: u32, keyframe: bool) -> EncodedFrame {
        EncodedFrame {
            mid: "1".into(),
            codec: MediaCodec::H264,
            data: Arc::from(vec![0u8; 4]),
            frame_index: Some(index),
            timestamp: u64::from(index) * 1500,
            clock_rate_hz: 90_000,
            keyframe,
            contiguous: true,
            ssrc: None,
        }
    }

    fn gated_pipeline(fps: u32, gate: Gate) -> (VideoPipeline, Arc<Mutex<Vec<u32>>>, Receiver<MediaFeedback>) {
        let decoded = Arc::new(Mutex::new(Vec::new()));
        let sink = Arc::clone(&decoded);
        let factory: Arc<DecoderFactory> = Arc::new(move |_| {
            Ok(Box::new(Recorder(Arc::clone(&sink), Arc::new(AtomicU64::new(0)), Arc::clone(&gate))) as Box<dyn VideoDecoder>)
        });
        let (tx, rx) = channel();
        let config = MediaStreamConfig { fps, ..MediaStreamConfig::default() };
        (VideoPipeline::start("test", &config, factory, tx, Arc::default()).unwrap(), decoded, rx)
    }

    fn pipeline(fps: u32) -> (VideoPipeline, Arc<Mutex<Vec<u32>>>, Receiver<MediaFeedback>) {
        gated_pipeline(fps, Arc::new((Mutex::new(true), Condvar::new())))
    }

    fn open(gate: &Gate) {
        *gate.0.lock().unwrap() = true;
        gate.1.notify_all();
    }

    fn settle(decoded: &Mutex<Vec<u32>>, len: usize) {
        for _ in 0..200 {
            if decoded.lock().unwrap().len() >= len {
                return;
            }
            std::thread::sleep(Duration::from_millis(5));
        }
    }

    #[test]
    fn waits_for_the_first_keyframe_and_acks_what_it_decodes() {
        let (video, decoded, feedback) = pipeline(60);
        video.push(frame(1, false));
        video.push(frame(2, true));
        video.push(frame(3, false));
        settle(&decoded, 2);
        video.stop();
        assert_eq!(*decoded.lock().unwrap(), vec![2, 3]);
        let events: Vec<_> = feedback.try_iter().collect();
        let acked: Vec<_> = events
            .iter()
            .filter_map(|e| match e {
                MediaFeedback::VideoFrameAccepted { frame_index, .. } => *frame_index,
                _ => None,
            })
            .collect();
        assert_eq!(acked, vec![2, 3]);
        assert!(events.contains(&MediaFeedback::QueueDropped { media: "video", count: 1 }));
        assert!(events.iter().any(|e| matches!(e, MediaFeedback::PlaybackStarted { backend: "test" })));
    }

    #[test]
    fn overflow_evicts_oldest_and_requests_one_keyframe() {
        let gate: Gate = Arc::new((Mutex::new(false), Condvar::new()));
        let (video, decoded, feedback) = gated_pipeline(1, Arc::clone(&gate));
        video.push(frame(1, true));
        settle(&decoded, 1);
        assert_eq!(video.push(frame(2, false)), PushOutcome::Queued);
        assert_eq!(video.push(frame(3, false)), PushOutcome::Queued);
        assert_eq!(video.push(frame(4, false)), PushOutcome::DroppedOldest);
        assert_eq!(video.push(frame(5, false)), PushOutcome::DroppedOldest);
        open(&gate);
        video.stop();
        let events: Vec<_> = feedback.try_iter().collect();
        let requests = events.iter().filter(|e| matches!(e, MediaFeedback::RequestKeyframe { .. })).count();
        assert_eq!(requests, 1, "{events:?}");
        assert_eq!(video.push(frame(6, true)), PushOutcome::Closed);
    }

    #[test]
    fn discontinuity_requests_a_keyframe() {
        let (video, decoded, feedback) = pipeline(60);
        video.push(frame(1, true));
        settle(&decoded, 1);
        let mut gap = frame(3, false);
        gap.contiguous = false;
        video.push(gap);
        video.stop();
        assert!(feedback.try_iter().any(|e| matches!(e, MediaFeedback::RequestKeyframe { .. })));
        assert_eq!(*decoded.lock().unwrap(), vec![1]);
    }

    /// Fails on frame 2, as a dead Starfish pipeline would.
    struct Failing(Arc<Mutex<Vec<u32>>>);

    impl VideoDecoder for Failing {
        fn decode(&mut self, frame: &EncodedFrame) -> Result<Decoded, String> {
            let index = frame.frame_index.unwrap();
            if index == 2 {
                return Err("pipeline error".into());
            }
            self.0.lock().unwrap().push(index);
            Ok(Decoded::Accepted)
        }
        fn presented_frames(&self) -> u64 {
            0
        }
    }

    #[test]
    fn a_failed_decoder_is_reopened_without_ending_the_session() {
        let decoded = Arc::new(Mutex::new(Vec::new()));
        let opens = Arc::new(AtomicU64::new(0));
        let (sink, counter) = (Arc::clone(&decoded), Arc::clone(&opens));
        let factory: Arc<DecoderFactory> = Arc::new(move |_| {
            counter.fetch_add(1, Ordering::Relaxed);
            Ok(Box::new(Failing(Arc::clone(&sink))) as Box<dyn VideoDecoder>)
        });
        let (tx, feedback) = channel();
        let video = VideoPipeline::start("test", &MediaStreamConfig::default(), factory, tx, Arc::default()).unwrap();
        video.push(frame(1, true));
        settle(&decoded, 1);
        video.push(frame(2, false));
        video.push(frame(3, false));
        video.push(frame(4, true));
        video.push(frame(5, false));
        settle(&decoded, 3);
        video.stop();
        assert_eq!(*decoded.lock().unwrap(), vec![1, 4, 5]);
        assert_eq!(opens.load(Ordering::Relaxed), 2);
        let events: Vec<_> = feedback.try_iter().collect();
        assert!(!events.iter().any(|e| matches!(e, MediaFeedback::DecoderError { .. })), "{events:?}");
        assert!(events.iter().any(|e| matches!(e, MediaFeedback::RequestKeyframe { .. })), "{events:?}");
    }

    #[test]
    fn capacity_tracks_a_quarter_second() {
        assert_eq!(queue_capacity(60), 15);
        assert_eq!(queue_capacity(1), 2);
        assert_eq!(queue_capacity(1000), 60);
    }
}
