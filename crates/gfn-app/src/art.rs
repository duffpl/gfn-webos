//! Game artwork: the core downloads and caches the file (`artwork.resolve`, then an
//! `artwork.ready` event when it was not cached yet); this thread decodes it, crops it to the tile
//! and rounds its corners, so the UI thread only uploads finished pixels.

use std::collections::HashMap;
use std::sync::Arc;
use std::sync::mpsc::{Receiver, RecvTimeoutError, Sender};
use std::time::{Duration, Instant};

use gfn_core_client::CoreClient;
use image::imageops::FilterType;
use serde_json::json;

use crate::Update;

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
    /// From the core's `artwork.ready` event; `None` when the download failed.
    Ready { url: String, path: Option<String> },
}

const RETRY: Duration = Duration::from_millis(750);

pub fn spawn(core: Arc<CoreClient>, messages: Receiver<Message>, updates: Sender<Update>) {
    let worker = move || {
        let mut waiting: HashMap<String, Job> = HashMap::new();
        let mut retry: Vec<(Instant, Job)> = Vec::new();
        loop {
            let message = match messages.recv_timeout(RETRY) {
                Ok(message) => Some(message),
                Err(RecvTimeoutError::Timeout) => None,
                Err(RecvTimeoutError::Disconnected) => return,
            };
            let (expired, later): (Vec<_>, Vec<_>) = retry.drain(..).partition(|(at, _)| at.elapsed() >= RETRY);
            retry = later;
            let mut due: Vec<Job> = expired.into_iter().map(|(_, job)| job).collect();
            match message {
                Some(Message::Want(job)) => due.push(job),
                Some(Message::Ready { url, path }) => {
                    if let Some(job) = waiting.remove(&url) {
                        let pixels = path.and_then(|path| load(&path, &job));
                        if updates.send(Update::Art { url, pixels }).is_err() {
                            return;
                        }
                    }
                }
                None => {}
            }
            for job in due {
                let resolved = core.request("artwork.resolve", json!({"url": job.url}), Duration::from_secs(10));
                let resolved = match resolved {
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
                        waiting.insert(job.url.clone(), job);
                    }
                    // A saturated download queue is temporary; ask again shortly.
                    _ if matches!(resolved["fallbackReason"].as_str(), Some("queue-full" | "retry-later")) => {
                        retry.push((Instant::now(), job));
                    }
                    _ => {
                        let _ = updates.send(Update::Art { url: job.url, pixels: None });
                    }
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
