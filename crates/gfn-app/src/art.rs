//! Game artwork: the core downloads and caches the file (`artwork.resolve`, then an
//! `artwork.ready` event when it was not cached yet); this thread decodes it, crops it to the tile
//! and rounds its corners, so the UI thread only uploads finished pixels.

use std::collections::{HashMap, HashSet, VecDeque};
use std::sync::Arc;
use std::sync::mpsc::{Receiver, RecvTimeoutError};
use std::time::{Duration, Instant};

use gfn_core_client::CoreClient;
use image::imageops::FilterType;
use serde_json::json;

use crate::{Update, Updates};

pub struct Pixels {
    pub width: u32,
    pub height: u32,
    pub rgba: Vec<u8>,
}

pub struct Job {
    pub url: String,
    pub width: u32,
    pub height: u32,
    pub radius: u32,
}

pub enum Message {
    Want(Job),
    /// The artwork on screen now. Queued work for anything else is dropped, so a fast scroll does
    /// not leave a backlog of tiles that were only passed over.
    Visible(HashSet<String>),
    /// From the core's `artwork.ready` event; `None` when the download failed.
    Ready { url: String, path: Option<String> },
}

const RETRY: Duration = Duration::from_millis(750);

/// Work for this thread, kept to what is on screen.
#[derive(Default)]
struct Pending {
    queue: VecDeque<Job>,
    /// Being downloaded by the core, which sends `artwork.ready` when done.
    waiting: HashMap<String, Job>,
    retry: Vec<(Instant, Job)>,
}

impl Pending {
    fn want(&mut self, job: Job) {
        let url = job.url.as_str();
        let known = self.queue.iter().any(|j| j.url == url)
            || self.waiting.contains_key(url)
            || self.retry.iter().any(|(_, j)| j.url == url);
        if !known {
            self.queue.push_back(job);
        }
    }

    /// A download already started still finishes into the core's cache; only its decode is
    /// skipped, and scrolling back finds it cached.
    fn keep_only(&mut self, visible: &HashSet<String>) {
        self.queue.retain(|job| visible.contains(&job.url));
        self.waiting.retain(|url, _| visible.contains(url));
        self.retry.retain(|(_, job)| visible.contains(&job.url));
    }

    fn next(&mut self) -> Option<Job> {
        let (due, later): (Vec<_>, Vec<_>) = self.retry.drain(..).partition(|(at, _)| at.elapsed() >= RETRY);
        self.retry = later;
        self.queue.extend(due.into_iter().map(|(_, job)| job));
        self.queue.pop_front()
    }

    fn idle(&self) -> bool {
        self.queue.is_empty()
    }
}

pub fn spawn(core: Arc<CoreClient>, messages: Receiver<Message>, updates: Updates) {
    let worker = move || {
        let mut pending = Pending::default();
        loop {
            // Every message is read before each job, so a scroll's Visible lands before the
            // jobs it makes stale are worked on.
            let first = match messages.recv_timeout(if pending.idle() { RETRY } else { Duration::ZERO }) {
                Ok(message) => Some(message),
                Err(RecvTimeoutError::Timeout) => None,
                Err(RecvTimeoutError::Disconnected) => return,
            };
            for message in first.into_iter().chain(messages.try_iter()) {
                match message {
                    Message::Want(job) => pending.want(job),
                    Message::Visible(visible) => pending.keep_only(&visible),
                    Message::Ready { url, path } => {
                        if let Some(job) = pending.waiting.remove(&url) {
                            let pixels = path.and_then(|path| load(&path, &job));
                            if updates.send(Update::Art { url, pixels }).is_err() {
                                return;
                            }
                        }
                    }
                }
            }
            let Some(job) = pending.next() else { continue };
            let resolved = match core.request("artwork.resolve", json!({"url": job.url}), Duration::from_secs(10)) {
                Ok(resolved) => resolved,
                Err(error) => {
                    eprintln!("artwork.resolve: {error}");
                    let _ = updates.send(Update::Art { url: job.url, pixels: None });
                    continue;
                }
            };
            match resolved["status"].as_str() {
                Some("ready") => {
                    let pixels = resolved["filePath"].as_str().and_then(|path| load(path, &job));
                    if updates.send(Update::Art { url: job.url, pixels }).is_err() {
                        return;
                    }
                }
                Some("pending") => {
                    pending.waiting.insert(job.url.clone(), job);
                }
                // A saturated download queue is temporary; ask again shortly.
                _ if matches!(resolved["fallbackReason"].as_str(), Some("queue-full" | "retry-later")) => {
                    pending.retry.push((Instant::now(), job));
                }
                _ => {
                    let _ = updates.send(Update::Art { url: job.url, pixels: None });
                }
            }
        }
    };
    if let Err(error) = std::thread::Builder::new().name("gfn-artwork".into()).spawn(worker) {
        eprintln!("artwork thread: {error}");
    }
}

fn load(path: &str, job: &Job) -> Option<Pixels> {
    let decoded = match image::open(path) {
        Ok(decoded) => decoded.to_rgba8(),
        Err(error) => {
            eprintln!("artwork {path}: {error}");
            return None;
        }
    };
    let (width, height) = (job.width.max(1), job.height.max(1));
    let (src_w, src_h) = decoded.dimensions();
    // Cover: crop the source to the tile's aspect ratio around its centre, then scale.
    let scale = (width as f32 / src_w as f32).max(height as f32 / src_h as f32);
    let crop_w = ((width as f32 / scale).round() as u32).clamp(1, src_w);
    let crop_h = ((height as f32 / scale).round() as u32).clamp(1, src_h);
    let cropped = image::imageops::crop_imm(&decoded, (src_w - crop_w) / 2, (src_h - crop_h) / 2, crop_w, crop_h);
    let mut rgba = image::imageops::resize(&cropped.to_image(), width, height, FilterType::Triangle).into_raw();
    round_corners(&mut rgba, width, height, job.radius);
    Some(Pixels { width, height, rgba })
}

fn round_corners(rgba: &mut [u8], width: u32, height: u32, radius: u32) {
    let radius = radius.min(width / 2).min(height / 2);
    let r = radius as f32;
    for y in 0..radius {
        for x in 0..radius {
            let (dx, dy) = (r - x as f32 - 0.5, r - y as f32 - 0.5);
            let coverage = (r - (dx * dx + dy * dy).sqrt() + 0.5).clamp(0.0, 1.0);
            for (cx, cy) in [(x, y), (width - 1 - x, y), (x, height - 1 - y), (width - 1 - x, height - 1 - y)] {
                let alpha = &mut rgba[((cy * width + cx) * 4 + 3) as usize];
                *alpha = (f32::from(*alpha) * coverage).round() as u8;
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn job(url: &str) -> Job {
        Job { url: url.into(), width: 1, height: 1, radius: 0 }
    }

    #[test]
    fn scrolled_past_artwork_is_dropped_from_the_queue() {
        let mut pending = Pending::default();
        for url in ["a", "b", "c", "a"] {
            pending.want(job(url));
        }
        pending.waiting.insert("w".into(), job("w"));
        pending.keep_only(&HashSet::from(["c".to_owned(), "x".to_owned()]));
        pending.want(job("x"));
        let order: Vec<String> = std::iter::from_fn(|| pending.next()).map(|job| job.url).collect();
        assert_eq!(order, ["c", "x"]);
        assert!(pending.waiting.is_empty());
    }

    #[test]
    fn corners_are_cut_and_the_middle_kept() {
        let (w, h) = (20, 10);
        let mut rgba = vec![255u8; (w * h * 4) as usize];
        round_corners(&mut rgba, w, h, 4);
        let alpha = |x: u32, y: u32| rgba[((y * w + x) * 4 + 3) as usize];
        assert_eq!(alpha(0, 0), 0);
        assert_eq!(alpha(w - 1, h - 1), 0);
        assert_eq!(alpha(10, 5), 255);
        assert_eq!(alpha(4, 0), 255);
    }
}
