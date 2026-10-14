//! On-device probes. `play-file` checks the Starfish live path (Phase 2) without GeForce NOW:
//!
//!     gfn-probe play-file <stream.h264|stream.h265> [--fps 60] [--loops 3] [--app-id <id>]
//!
//! The app id must be the installed package's id: ACB binds the video plane to that application.
//!
//!     gfn-probe core-hello <opennow-core> [--data-dir <dir>]
//!
//! Spawns the core, completes the protocol handshake and prints its capabilities and auth state.
//!
//! A full GeForce NOW session is the app itself (`gfn-app`).

mod annexb;

use std::time::{Duration, Instant};

use gfn_starfish::{Event, FeedResult, LoadPayload, OpenConfig, Rect, StarfishVideo, VideoCodec};

const EVENT_LOG: &str = "/tmp/gfn-webos-events.log";

fn main() {
    let args: Vec<String> = std::env::args().collect();
    let result = match args.get(1).map(String::as_str) {
        Some("play-file") => play_file(&args[2..]),
        Some("core-hello") => core_hello(&args[2..]),
        _ => Err(format!(
            "usage: {0} play-file <file> [--fps N] [--loops N] [--app-id ID]\n       {0} core-hello <opennow-core> [--data-dir DIR]",
            args[0]
        )),
    };
    if let Err(error) = result {
        eprintln!("gfn-probe: {error}");
        std::process::exit(1);
    }
}

fn flag<'a>(args: &'a [String], name: &str) -> Option<&'a str> {
    args.iter().position(|a| a == name).and_then(|i| args.get(i + 1)).map(String::as_str)
}

fn core_hello(args: &[String]) -> Result<(), String> {
    use gfn_core_client::{CoreClient, SpawnOptions};
    let executable = std::path::Path::new(args.first().ok_or("missing opennow-core path")?);
    let data_dir = flag(args, "--data-dir").map(std::path::Path::new);
    let started = Instant::now();
    let (core, _events) = CoreClient::spawn(SpawnOptions {
        executable,
        data_dir,
        shell_version: env!("CARGO_PKG_VERSION"),
        diagnostics: Some(Box::new(|line| eprintln!("core: {line}"))),
    })
    .map_err(|e| e.to_string())?;
    println!("handshake ok in {} ms", started.elapsed().as_millis());
    let session = core
        .request("auth.session.get", serde_json::json!({}), Duration::from_secs(10))
        .map_err(|e| e.to_string())?;
    let signed_in = session.get("session").is_some_and(|s| !s.is_null());
    println!("signed_in={signed_in} persistence={}", session.get("persistence").unwrap_or(&serde_json::Value::Null));
    Ok(())
}

fn play_file(args: &[String]) -> Result<(), String> {
    let path = args.first().ok_or("missing input file")?;
    let fps: u32 = flag(args, "--fps").map_or(Ok(60), str::parse).map_err(|_| "bad --fps")?;
    let loops: u32 = flag(args, "--loops").map_or(Ok(3), str::parse).map_err(|_| "bad --loops")?;
    let app_id = flag(args, "--app-id")
        .map(str::to_owned)
        .or_else(|| std::env::var("APPID").ok())
        .ok_or("no --app-id and APPID is not set")?;
    let codec = if path.ends_with(".h265") || path.ends_with(".hevc") { VideoCodec::H265 } else { VideoCodec::H264 };

    let data = std::fs::read(path).map_err(|e| format!("{path}: {e}"))?;
    let units = annexb::access_units(codec, &data);
    if units.is_empty() {
        return Err("no access units found".into());
    }
    gfn_starfish::open_event_log(Some(EVENT_LOG));
    let sdl = gfn_sdl::Sdl::init(gfn_sdl::INIT_VIDEO)?;
    let mut window = gfn_sdl::Window::open(&sdl, "gfn-probe", 1920, 1080, true, false)?;
    window.present_transparent();
    println!("plane={:?} codec={codec:?} units={} fps={fps}", gfn_starfish::video_plane(), units.len());

    let opened = Instant::now();
    let mut video = StarfishVideo::open(OpenConfig {
        app_id,
        payload: LoadPayload::live(codec, 1920, 1080, fps),
        display: Rect::FULL_HD,
    })?;
    println!("load+play in {} ms", opened.elapsed().as_millis());

    let interval = Duration::from_nanos(1_000_000_000 / u64::from(fps.max(1)));
    let start = Instant::now();
    let (mut fed, mut full_retries, mut errors) = (0u64, 0u64, 0u64);
    let mut first_frame: Option<Duration> = None;
    for unit in (0..loops).flat_map(|_| units.iter()) {
        let due = start + interval * fed as u32;
        if let Some(wait) = due.checked_duration_since(Instant::now()) {
            std::thread::sleep(wait);
        }
        let pts = i64::try_from(interval.as_nanos()).unwrap_or(0) * fed as i64;
        loop {
            match video.feed(unit, pts) {
                FeedResult::Accepted => break,
                FeedResult::BufferFull => {
                    full_retries += 1;
                    std::thread::sleep(Duration::from_millis(1));
                }
                FeedResult::Error => {
                    errors += 1;
                    break;
                }
            }
        }
        fed += 1;
        if fed % u64::from((fps / 10).max(1)) == 0 {
            window.present_transparent();
        }
        for event in video.events().try_iter() {
            match event {
                Event::FrameReady { .. } if first_frame.is_none() => first_frame = Some(start.elapsed()),
                Event::Error { code, message } => eprintln!("pipeline error {code}: {message}"),
                _ => {}
            }
        }
    }
    let elapsed = start.elapsed().as_secs_f64();
    println!(
        "fed={fed} presented={} feed_fps={:.1} first_frame_ms={} buffer_full_retries={full_retries} feed_errors={errors}",
        video.frames_presented(),
        fed as f64 / elapsed,
        first_frame.map_or("none".into(), |d| d.as_millis().to_string()),
    );
    Ok(())
}
