//! Desktop simulator video: a latest-frame slot the main thread presents, filled by VideoToolbox
//! on macOS (`vt`) or by OpenH264 software decode elsewhere.
//! It stands in for the TV's hardware plane so UI and protocol work can happen off-device; its
//! timings say nothing about LG's decoder.

use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Condvar, Mutex};
use std::time::Duration;

use openh264::OpenH264API;
use openh264::decoder::{Decoder, DecoderConfig};
use openh264::formats::YUVSource;
use opennow_streamer_platform_webos::{EncodedFrame, MediaStreamConfig, MediaVideoCodec};

use crate::{Decoded, DecoderFactory, VideoDecoder};

pub struct OwnedYuv {
    pub width: u32,
    pub height: u32,
    pub y: Vec<u8>,
    pub u: Vec<u8>,
    pub v: Vec<u8>,
    pub y_stride: usize,
    pub uv_stride: usize,
}

impl OwnedYuv {
    pub fn as_frame(&self) -> gfn_sdl::YuvFrame<'_> {
        gfn_sdl::YuvFrame {
            width: self.width,
            height: self.height,
            y: &self.y,
            u: &self.u,
            v: &self.v,
            y_stride: self.y_stride,
            uv_stride: self.uv_stride,
        }
    }
}

/// Latest decoded picture. Only the newest matters: an unpresented frame is replaced, not queued.
#[derive(Default)]
pub struct FrameSlot {
    latest: Mutex<Option<OwnedYuv>>,
    ready: Condvar,
    replaced: AtomicU64,
}

impl FrameSlot {
    pub fn take(&self) -> Option<OwnedYuv> {
        self.latest.lock().unwrap_or_else(std::sync::PoisonError::into_inner).take()
    }

    /// Waits up to `timeout` for a picture, so the presenter wakes the moment one is decoded
    /// instead of polling (polling skips frames whenever two land inside one interval).
    pub fn wait_take(&self, timeout: Duration) -> Option<OwnedYuv> {
        let latest = self.latest.lock().unwrap_or_else(std::sync::PoisonError::into_inner);
        let (mut latest, _) = self
            .ready
            .wait_timeout_while(latest, timeout, |latest| latest.is_none())
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        latest.take()
    }

    pub fn replaced_unpresented(&self) -> u64 {
        self.replaced.load(Ordering::Relaxed)
    }

    pub(crate) fn publish(&self, frame: OwnedYuv) {
        let previous = self.latest.lock().unwrap_or_else(std::sync::PoisonError::into_inner).replace(frame);
        if previous.is_some() {
            self.replaced.fetch_add(1, Ordering::Relaxed);
        }
        self.ready.notify_one();
    }
}

struct SimDecoder {
    decoder: Decoder,
    slot: Arc<FrameSlot>,
    decoded: u64,
}

impl VideoDecoder for SimDecoder {
    fn decode(&mut self, frame: &EncodedFrame) -> Result<Decoded, String> {
        let Some(yuv) = self.decoder.decode(&frame.data).map_err(|e| e.to_string())? else {
            return Ok(Decoded::Accepted);
        };
        let (width, height) = yuv.dimensions();
        let (y_stride, uv_stride, _) = yuv.strides();
        self.slot.publish(OwnedYuv {
            width: width as u32,
            height: height as u32,
            y: yuv.y().to_vec(),
            u: yuv.u().to_vec(),
            v: yuv.v().to_vec(),
            y_stride,
            uv_stride,
        });
        self.decoded += 1;
        Ok(Decoded::Accepted)
    }

    fn presented_frames(&self) -> u64 {
        self.decoded
    }
}

/// H.264 only: OpenH264 is the one decoder that builds identically on every development host.
pub fn decoder_factory(slot: Arc<FrameSlot>) -> Arc<DecoderFactory> {
    Arc::new(move |config: &MediaStreamConfig| {
        if config.codec != MediaVideoCodec::H264 {
            return Err(format!("the simulator decodes H.264 only, not {}", config.codec.label()));
        }
        let decoder = Decoder::with_api_config(OpenH264API::from_source(), DecoderConfig::new().debug(false))
            .map_err(|e| format!("OpenH264: {e}"))?;
        Ok(Box::new(SimDecoder { decoder, slot: Arc::clone(&slot), decoded: 0 }) as Box<dyn VideoDecoder>)
    })
}
