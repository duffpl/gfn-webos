//! Streaming choices the user makes on the Settings screen. They live in our own file rather than
//! only in the core's settings so the TV can keep defaults different from the core's, and they are
//! handed to the core at each session start, where a blocking core call costs the UI nothing.

use std::path::{Path, PathBuf};

use serde_json::{Value, json};

use crate::TV;

pub const RESOLUTIONS: [(&str, &str); 4] =
    [("1280x720", "720p"), ("1920x1080", "1080p"), ("2560x1440", "1440p"), ("3840x2160", "4K")];
pub const FRAME_RATES: [u32; 3] = [30, 60, 120];
pub const BITRATES: [u32; 9] = [10, 15, 20, 25, 30, 40, 50, 60, 75];
/// No "auto": the core starts an embedded stream with "auto" as H.264, which would hide H.265.
pub const CODECS: [(&str, &str); 2] = [("h264", "H.264"), ("h265", "H.265")];
/// The TV's video paths: Starfish (media pipeline, H.264/H.265, HDR) or NDL (direct decoder,
/// H.264 only), which moonlight-tv uses on webOS 4 for latency.
pub const PLAYERS: [(&str, &str); 2] = [("starfish", "Starfish"), ("ndl", "NDL (H.264 only)")];

#[derive(Debug, Clone, PartialEq)]
pub struct StreamSettings {
    pub resolution: String,
    pub fps: u32,
    pub max_bitrate_mbps: u32,
    pub codec: String,
    /// HDR10, which needs H.265; turning it on picks H.265.
    pub hdr: bool,
    /// One of [`PLAYERS`]; NDL implies H.264 without HDR.
    pub player: String,
}

impl Default for StreamSettings {
    fn default() -> Self {
        // The TV caps an unprivileged UDP receive buffer at 512 KiB (net.core.rmem_max), ~100 ms
        // at the core's 75 Mbps default; any scheduling hiccup then overflows it and frames are lost.
        let max_bitrate_mbps = if TV { 20 } else { 75 };
        // Only the OpenH264 fallback (non-macOS simulator) is limited to H.264.
        let codec = if cfg!(all(feature = "sim", not(target_os = "macos"))) { "h264" } else { "h265" };
        Self { resolution: "1920x1080".into(), fps: 60, max_bitrate_mbps, codec: codec.into(), hdr: false, player: "starfish".into() }
    }
}

impl StreamSettings {
    pub fn path(data_dir: &Path) -> PathBuf {
        data_dir.join("stream-settings.json")
    }

    /// Unknown or damaged values fall back to the defaults one by one.
    pub fn load(path: &Path) -> Self {
        let mut settings = Self::default();
        let Some(stored) = std::fs::read(path).ok().and_then(|bytes| serde_json::from_slice::<Value>(&bytes).ok()) else {
            return settings;
        };
        if let Some(resolution) = stored["resolution"].as_str().filter(|r| RESOLUTIONS.iter().any(|(v, _)| v == r)) {
            settings.resolution = resolution.to_owned();
        }
        if let Some(fps) = stored["fps"].as_u64().and_then(|f| u32::try_from(f).ok()).filter(|f| FRAME_RATES.contains(f)) {
            settings.fps = fps;
        }
        if let Some(rate) = stored["maxBitrateMbps"].as_u64().and_then(|r| u32::try_from(r).ok()).filter(|r| BITRATES.contains(r)) {
            settings.max_bitrate_mbps = rate;
        }
        if let Some(codec) = stored["codec"].as_str().filter(|c| CODECS.iter().any(|(v, _)| v == c)) {
            settings.codec = codec.to_owned();
        }
        settings.hdr = stored["hdr"].as_bool().unwrap_or(false) && settings.codec == "h265";
        if TV && stored["player"].as_str() == Some("ndl") && settings.codec == "h264" {
            settings.player = "ndl".into();
        }
        settings
    }

    pub fn save(&self, path: &Path) -> Result<(), String> {
        let json = serde_json::to_vec_pretty(&self.to_json()).map_err(|e| e.to_string())?;
        std::fs::write(path, json).map_err(|e| format!("{}: {e}", path.display()))
    }

    fn to_json(&self) -> Value {
        json!({
            "resolution": self.resolution,
            "fps": self.fps,
            "maxBitrateMbps": self.max_bitrate_mbps,
            "codec": self.codec,
            "hdr": self.hdr,
            "player": self.player,
        })
    }

    /// `(key, value)` pairs for the core's `settings.set`.
    pub fn core_settings(&self) -> Vec<(&'static str, Value)> {
        // The core resolves an HDR session to 10-bit itself; the stored color mode stays 8-bit so an
        // H.264 choice is never refused for it. Color goes first: the core rejects a codec the
        // saved color mode cannot use.
        vec![
            ("colorQuality", json!("8bit_420")),
            ("resolution", json!(self.resolution)),
            ("fps", json!(self.fps)),
            ("maxBitrateMbps", json!(self.max_bitrate_mbps)),
            ("codec", json!(self.codec)),
            ("enableHdr", json!(self.hdr)),
            // CloudMatch accepts HDR from a "Linux" client but never turns it on in the cloud PC;
            // presenting as Windows (a setting in our OpenNOW fork) gets a real HDR session.
            ("reportedPlatform", json!(if self.hdr { "windows" } else { "auto" })),
        ]
    }
}

/// Moves `current` one step through `choices`, stopping at either end.
pub fn step<T: PartialEq + Copy>(choices: &[T], current: T, forward: bool) -> T {
    let index = choices.iter().position(|c| *c == current).unwrap_or(0);
    let next = if forward { (index + 1).min(choices.len() - 1) } else { index.saturating_sub(1) };
    choices[next]
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn round_trips_and_ignores_values_it_does_not_offer() {
        let dir = std::env::temp_dir().join(format!("gfn-settings-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let path = StreamSettings::path(&dir);
        let chosen =
            StreamSettings {
            resolution: "2560x1440".into(),
            fps: 120,
            max_bitrate_mbps: 40,
            codec: "h265".into(),
            hdr: true,
            player: "starfish".into(),
        };
        chosen.save(&path).unwrap();
        assert_eq!(StreamSettings::load(&path), chosen);

        std::fs::write(&path, r#"{"resolution":"999x1","fps":61,"maxBitrateMbps":40,"codec":"vp9"}"#).unwrap();
        let loaded = StreamSettings::load(&path);
        assert_eq!(loaded.max_bitrate_mbps, 40);
        assert_eq!((loaded.resolution.as_str(), loaded.fps), ("1920x1080", 60));
        assert_eq!(loaded.codec, StreamSettings::default().codec);
        std::fs::remove_dir_all(dir).unwrap();
    }

    #[test]
    fn steps_stop_at_the_ends() {
        assert_eq!(step(&BITRATES, 75, true), 75);
        assert_eq!(step(&BITRATES, 10, false), 10);
        assert_eq!(step(&BITRATES, 20, true), 25);
    }
}
