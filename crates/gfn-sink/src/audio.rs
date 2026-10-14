use std::collections::VecDeque;
use std::sync::mpsc::Sender;
use std::sync::{Arc, Condvar, Mutex};
use std::thread::JoinHandle;

use gfn_sdl::AudioOut;
use opennow_streamer_platform_webos::{EncodedFrame, MediaCodec, MediaFeedback, PushOutcome};

const SAMPLE_RATE: u32 = 48_000;
/// Ten 20 ms packets: the official client's adaptive ceiling (OpenNOW media.rs).
const QUEUE_CAPACITY: usize = 10;
/// Output backlog beyond which packets are skipped: cloud gaming trades a click for latency.
const MAX_OUTPUT_BACKLOG_MS: u32 = 120;
/// Silence queued ahead of the first packet and after the device ran dry, so ordinary network
/// jitter between 20 ms packets is absorbed instead of heard as a gap per late packet.
const CUSHION_MS: u32 = 40;
const MAX_FRAME_SAMPLES_PER_CHANNEL: usize = 5_760;

#[derive(Default)]
struct State {
    packets: VecDeque<EncodedFrame>,
    paused: bool,
    stopped: bool,
}

struct Shared {
    state: Mutex<State>,
    wake: Condvar,
    feedback: Sender<MediaFeedback>,
}

impl Shared {
    fn lock(&self) -> std::sync::MutexGuard<'_, State> {
        self.state.lock().unwrap_or_else(std::sync::PoisonError::into_inner)
    }

    fn dropped(&self, count: usize) {
        let _ = self.feedback.send(MediaFeedback::QueueDropped { media: "audio", count });
    }
}

pub struct AudioPipeline {
    shared: Arc<Shared>,
    worker: Mutex<Option<JoinHandle<()>>>,
}

impl AudioPipeline {
    pub fn start(output: Arc<AudioOut>, feedback: Sender<MediaFeedback>) -> Self {
        let shared = Arc::new(Shared { state: Mutex::default(), wake: Condvar::new(), feedback });
        let worker_shared = Arc::clone(&shared);
        let worker = std::thread::Builder::new()
            .name("gfn-audio".into())
            .spawn(move || run(&worker_shared, &output))
            .ok();
        Self { shared, worker: Mutex::new(worker) }
    }

    pub fn push(&self, packet: EncodedFrame) -> PushOutcome {
        let mut state = self.shared.lock();
        if state.stopped {
            return PushOutcome::Closed;
        }
        if state.paused {
            return PushOutcome::Paused;
        }
        let mut outcome = PushOutcome::Queued;
        if state.packets.len() >= QUEUE_CAPACITY {
            state.packets.pop_front();
            self.shared.dropped(1);
            outcome = PushOutcome::DroppedOldest;
        }
        state.packets.push_back(packet);
        drop(state);
        self.shared.wake.notify_one();
        outcome
    }

    pub fn set_paused(&self, paused: bool) {
        let mut state = self.shared.lock();
        state.paused = paused;
        if paused {
            state.packets.clear();
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

fn run(shared: &Shared, output: &AudioOut) {
    let mut decoder: Option<(u8, opus::Decoder)> = None;
    let mut pcm = vec![0i16; MAX_FRAME_SAMPLES_PER_CHANNEL * 2];
    let mut consecutive_errors = 0u32;
    let (mut underruns, mut backlog_drops) = (0u64, 0u64);
    let mut last_report = std::time::Instant::now();
    output.clear();
    loop {
        let packet = {
            let mut state = shared.lock();
            loop {
                if state.stopped {
                    output.clear();
                    return;
                }
                if let Some(packet) = state.packets.pop_front() {
                    break packet;
                }
                state = shared.wake.wait(state).unwrap_or_else(std::sync::PoisonError::into_inner);
            }
        };
        let MediaCodec::Opus { channels } = packet.codec else { continue };
        if decoder.as_ref().is_none_or(|(c, _)| *c != channels) {
            let layout = match channels {
                1 => opus::Channels::Mono,
                2 => opus::Channels::Stereo,
                _ => {
                    let _ = shared.feedback.send(MediaFeedback::AudioUnavailable {
                        backend: "opus",
                        reason: format!("{channels}-channel Opus is not supported"),
                        rejected: 1,
                    });
                    continue;
                }
            };
            match opus::Decoder::new(SAMPLE_RATE, layout) {
                Ok(fresh) => decoder = Some((channels, fresh)),
                Err(error) => {
                    let _ = shared.feedback.send(MediaFeedback::AudioDecoderError {
                        message: error.to_string(),
                        consecutive: 1,
                    });
                    continue;
                }
            }
        }
        let Some((channels, opus)) = decoder.as_mut() else { continue };
        match opus.decode(&packet.data, &mut pcm, false) {
            Ok(samples_per_channel) => {
                consecutive_errors = 0;
                if output.queued_ms() > MAX_OUTPUT_BACKLOG_MS {
                    backlog_drops += 1;
                    shared.dropped(1);
                    continue;
                }
                if output.queued_ms() == 0 {
                    underruns += 1;
                    let cushion = vec![0i16; (SAMPLE_RATE * CUSHION_MS / 1000) as usize * 2];
                    let _ = output.queue(&cushion);
                }
                if last_report.elapsed() >= std::time::Duration::from_secs(10) {
                    eprintln!("audio: underruns={underruns} backlog-drops={backlog_drops} queued={}ms", output.queued_ms());
                    last_report = std::time::Instant::now();
                }
                let samples = samples_per_channel * usize::from(*channels);
                let block = if *channels == 1 { upmix(&pcm[..samples]) } else { pcm[..samples].to_vec() };
                if let Err(message) = output.queue(&block) {
                    let _ = shared.feedback.send(MediaFeedback::OutputError { message });
                }
            }
            Err(error) => {
                consecutive_errors += 1;
                let _ = shared.feedback.send(MediaFeedback::AudioDecoderError {
                    message: error.to_string(),
                    consecutive: consecutive_errors,
                });
            }
        }
    }
}

fn upmix(mono: &[i16]) -> Vec<i16> {
    mono.iter().flat_map(|&s| [s, s]).collect()
}
