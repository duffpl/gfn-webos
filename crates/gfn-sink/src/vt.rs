//! Desktop simulator video on macOS: VideoToolbox hardware decode into the simulator's frame slot.
//!
//! OpenH264 cannot sustain GeForce NOW's 1080p60 at 40-60 Mbit/s, and its backlog turns into
//! keyframe-recovery stutter. VideoToolbox also decodes H.265, the codec the TV is offered. Frames
//! are copied out of the pixel buffer so presentation stays the simulator's job, as the TV's UI
//! plane is separate from its video plane.

use std::ffi::c_void;
use std::ptr::{self, null, null_mut};
use std::sync::Arc;

use opennow_streamer_platform_webos::{EncodedFrame, MediaStreamConfig, MediaVideoCodec};

use crate::sim::{FrameSlot, OwnedYuv};
use crate::{Decoded, DecoderFactory, VideoDecoder};

type OSStatus = i32;
type CFTypeRef = *const c_void;

#[repr(C)]
#[derive(Clone, Copy)]
struct CMTime {
    value: i64,
    timescale: i32,
    flags: u32,
    epoch: i64,
}

#[repr(C)]
struct VTDecompressionOutputCallbackRecord {
    callback: extern "C" fn(*mut c_void, *mut c_void, OSStatus, u32, CFTypeRef, CMTime, CMTime),
    refcon: *mut c_void,
}

const CODEC_H264: u32 = u32::from_be_bytes(*b"avc1");
const CODEC_HEVC: u32 = u32::from_be_bytes(*b"hvc1");
/// Bi-planar 4:2:0 video range: VideoToolbox's native output, so no conversion on its side.
const PIXEL_FORMAT_NV12: i32 = i32::from_be_bytes(*b"420v");
const CF_NUMBER_SINT32: isize = 3;
const LOCK_READ_ONLY: u64 = 1;
const NAL_LENGTH_SIZE: i32 = 4;

#[link(name = "CoreFoundation", kind = "framework")]
unsafe extern "C" {
    static kCFAllocatorDefault: CFTypeRef;
    static kCFTypeDictionaryKeyCallBacks: c_void;
    static kCFTypeDictionaryValueCallBacks: c_void;
    fn CFRelease(object: CFTypeRef);
    fn CFRetain(object: CFTypeRef) -> CFTypeRef;
    fn CFNumberCreate(allocator: CFTypeRef, kind: isize, value: *const c_void) -> CFTypeRef;
    fn CFDictionaryCreate(
        allocator: CFTypeRef,
        keys: *const CFTypeRef,
        values: *const CFTypeRef,
        count: isize,
        key_callbacks: *const c_void,
        value_callbacks: *const c_void,
    ) -> CFTypeRef;
}

#[link(name = "CoreMedia", kind = "framework")]
unsafe extern "C" {
    fn CMVideoFormatDescriptionCreateFromH264ParameterSets(
        allocator: CFTypeRef,
        count: usize,
        pointers: *const *const u8,
        sizes: *const usize,
        nal_length_size: i32,
        out: *mut CFTypeRef,
    ) -> OSStatus;
    fn CMVideoFormatDescriptionCreateFromHEVCParameterSets(
        allocator: CFTypeRef,
        count: usize,
        pointers: *const *const u8,
        sizes: *const usize,
        nal_length_size: i32,
        extensions: CFTypeRef,
        out: *mut CFTypeRef,
    ) -> OSStatus;
    fn CMBlockBufferCreateWithMemoryBlock(
        allocator: CFTypeRef,
        memory: *mut c_void,
        length: usize,
        block_allocator: CFTypeRef,
        custom_source: *const c_void,
        offset: usize,
        data_length: usize,
        flags: u32,
        out: *mut CFTypeRef,
    ) -> OSStatus;
    fn CMBlockBufferReplaceDataBytes(source: *const c_void, block: CFTypeRef, offset: usize, length: usize) -> OSStatus;
    fn CMSampleBufferCreateReady(
        allocator: CFTypeRef,
        data: CFTypeRef,
        format: CFTypeRef,
        samples: isize,
        timing_entries: isize,
        timing: *const c_void,
        size_entries: isize,
        sizes: *const usize,
        out: *mut CFTypeRef,
    ) -> OSStatus;
}

#[link(name = "CoreVideo", kind = "framework")]
unsafe extern "C" {
    static kCVPixelBufferPixelFormatTypeKey: CFTypeRef;
    fn CVPixelBufferLockBaseAddress(buffer: CFTypeRef, flags: u64) -> i32;
    fn CVPixelBufferUnlockBaseAddress(buffer: CFTypeRef, flags: u64) -> i32;
    fn CVPixelBufferGetPlaneCount(buffer: CFTypeRef) -> usize;
    fn CVPixelBufferGetWidthOfPlane(buffer: CFTypeRef, plane: usize) -> usize;
    fn CVPixelBufferGetHeightOfPlane(buffer: CFTypeRef, plane: usize) -> usize;
    fn CVPixelBufferGetBytesPerRowOfPlane(buffer: CFTypeRef, plane: usize) -> usize;
    fn CVPixelBufferGetBaseAddressOfPlane(buffer: CFTypeRef, plane: usize) -> *const u8;
}

#[link(name = "VideoToolbox", kind = "framework")]
unsafe extern "C" {
    fn VTIsHardwareDecodeSupported(codec: u32) -> u8;
    fn VTDecompressionSessionCreate(
        allocator: CFTypeRef,
        format: CFTypeRef,
        decoder_specification: CFTypeRef,
        image_buffer_attributes: CFTypeRef,
        callback: *const VTDecompressionOutputCallbackRecord,
        out: *mut CFTypeRef,
    ) -> OSStatus;
    fn VTDecompressionSessionDecodeFrame(
        session: CFTypeRef,
        sample: CFTypeRef,
        flags: u32,
        source_refcon: *mut c_void,
        info_flags: *mut u32,
    ) -> OSStatus;
    fn VTDecompressionSessionInvalidate(session: CFTypeRef);
}

/// Releases a Core Foundation object on drop.
struct Owned(CFTypeRef);

impl Owned {
    fn new(object: CFTypeRef) -> Option<Self> {
        (!object.is_null()).then_some(Self(object))
    }
}

impl Drop for Owned {
    fn drop(&mut self) {
        unsafe { CFRelease(self.0) };
    }
}

pub fn hardware_codecs() -> Vec<MediaVideoCodec> {
    let mut codecs = Vec::new();
    unsafe {
        if VTIsHardwareDecodeSupported(CODEC_HEVC) != 0 {
            codecs.push(MediaVideoCodec::H265);
        }
        if VTIsHardwareDecodeSupported(CODEC_H264) != 0 {
            codecs.push(MediaVideoCodec::H264);
        }
    }
    codecs
}

pub fn decoder_factory(slot: Arc<FrameSlot>) -> Arc<DecoderFactory> {
    Arc::new(move |config: &MediaStreamConfig| {
        let hevc = match config.codec {
            MediaVideoCodec::H264 => false,
            MediaVideoCodec::H265 => true,
            MediaVideoCodec::Av1 => return Err("the simulator does not decode AV1".into()),
        };
        Ok(Box::new(VtDecoder {
            hevc,
            output: Box::new(Output { slot: Arc::clone(&slot), decoded: 0, failed: false }),
            parameter_sets: Vec::new(),
            format: None,
            session: None,
        }) as Box<dyn VideoDecoder>)
    })
}

/// Written by the decode callback, which runs inside `DecodeFrame` because decoding is synchronous.
struct Output {
    slot: Arc<FrameSlot>,
    decoded: u64,
    failed: bool,
}

struct VtDecoder {
    hevc: bool,
    output: Box<Output>,
    /// Parameter sets the current session was built from; a change rebuilds it.
    parameter_sets: Vec<Vec<u8>>,
    format: Option<Owned>,
    session: Option<Owned>,
}

impl VideoDecoder for VtDecoder {
    fn decode(&mut self, frame: &EncodedFrame) -> Result<Decoded, String> {
        let mut parameter_sets = Vec::new();
        let mut sample = Vec::with_capacity(frame.data.len() + 16);
        for nal in nal_units(&frame.data) {
            match nal_kind(nal, self.hevc) {
                NalKind::ParameterSet => parameter_sets.push(nal.to_vec()),
                NalKind::Skip => {}
                NalKind::Slice => {
                    sample.extend_from_slice(&(nal.len() as u32).to_be_bytes());
                    sample.extend_from_slice(nal);
                }
            }
        }
        if !parameter_sets.is_empty() && parameter_sets != self.parameter_sets {
            self.rebuild(parameter_sets)?;
        }
        let (Some(session), Some(format)) = (&self.session, &self.format) else {
            return Err("no parameter sets received yet".into());
        };
        if sample.is_empty() {
            return Ok(Decoded::Accepted);
        }
        unsafe {
            let block = create_block(&sample)?;
            let mut buffer = null();
            let size = sample.len();
            check(
                CMSampleBufferCreateReady(kCFAllocatorDefault, block.0, format.0, 1, 0, null(), 1, &size, &mut buffer),
                "CMSampleBufferCreateReady",
            )?;
            let buffer = Owned(buffer);
            self.output.failed = false;
            let mut info = 0;
            check(VTDecompressionSessionDecodeFrame(session.0, buffer.0, 0, null_mut(), &mut info), "DecodeFrame")?;
        }
        if self.output.failed { Err("VideoToolbox rejected the access unit".into()) } else { Ok(Decoded::Accepted) }
    }

    fn presented_frames(&self) -> u64 {
        self.output.decoded
    }
}

impl VtDecoder {
    fn rebuild(&mut self, parameter_sets: Vec<Vec<u8>>) -> Result<(), String> {
        self.close();
        let pointers: Vec<*const u8> = parameter_sets.iter().map(|p| p.as_ptr()).collect();
        let sizes: Vec<usize> = parameter_sets.iter().map(Vec::len).collect();
        unsafe {
            let mut format = null();
            let status = if self.hevc {
                CMVideoFormatDescriptionCreateFromHEVCParameterSets(
                    kCFAllocatorDefault,
                    pointers.len(),
                    pointers.as_ptr(),
                    sizes.as_ptr(),
                    NAL_LENGTH_SIZE,
                    null(),
                    &mut format,
                )
            } else {
                CMVideoFormatDescriptionCreateFromH264ParameterSets(
                    kCFAllocatorDefault,
                    pointers.len(),
                    pointers.as_ptr(),
                    sizes.as_ptr(),
                    NAL_LENGTH_SIZE,
                    &mut format,
                )
            };
            check(status, "CMVideoFormatDescriptionCreate")?;
            let format = Owned(format);
            let nv12 = PIXEL_FORMAT_NV12;
            let pixel_format = Owned::new(CFNumberCreate(kCFAllocatorDefault, CF_NUMBER_SINT32, (&raw const nv12).cast()))
            .ok_or("CFNumberCreate failed")?;
            let attributes = Owned::new(CFDictionaryCreate(
                kCFAllocatorDefault,
                &kCVPixelBufferPixelFormatTypeKey,
                &pixel_format.0,
                1,
                (&raw const kCFTypeDictionaryKeyCallBacks).cast(),
                (&raw const kCFTypeDictionaryValueCallBacks).cast(),
            ))
            .ok_or("CFDictionaryCreate failed")?;
            let record = VTDecompressionOutputCallbackRecord {
                callback: on_decoded,
                refcon: ptr::from_mut(self.output.as_mut()).cast(),
            };
            let mut session = null();
            check(
                VTDecompressionSessionCreate(kCFAllocatorDefault, format.0, null(), attributes.0, &record, &mut session),
                "VTDecompressionSessionCreate",
            )?;
            self.session = Some(Owned(session));
            self.format = Some(format);
        }
        self.parameter_sets = parameter_sets;
        Ok(())
    }

    fn close(&mut self) {
        if let Some(session) = self.session.take() {
            unsafe { VTDecompressionSessionInvalidate(session.0) };
        }
        self.format = None;
        self.parameter_sets.clear();
    }
}

impl Drop for VtDecoder {
    fn drop(&mut self) {
        self.close();
    }
}

extern "C" fn on_decoded(
    refcon: *mut c_void,
    _source: *mut c_void,
    status: OSStatus,
    _info: u32,
    image: CFTypeRef,
    _pts: CMTime,
    _duration: CMTime,
) {
    // SAFETY: `refcon` is the boxed `Output` of the decoder whose synchronous DecodeFrame call
    // is running this callback; the box outlives the session.
    let output = unsafe { &mut *refcon.cast::<Output>() };
    if status != 0 || image.is_null() {
        output.failed = true;
        return;
    }
    match unsafe { copy_nv12(image) } {
        Some(frame) => {
            output.slot.publish(frame);
            output.decoded += 1;
        }
        None => output.failed = true,
    }
}

/// Copies an NV12 pixel buffer into the slot's I420 layout (the chroma plane is de-interleaved).
unsafe fn copy_nv12(image: CFTypeRef) -> Option<OwnedYuv> {
    unsafe {
        if CVPixelBufferGetPlaneCount(image) != 2 || CVPixelBufferLockBaseAddress(image, LOCK_READ_ONLY) != 0 {
            return None;
        }
        CFRetain(image);
        let image = Owned(image);
        let (width, height) = (CVPixelBufferGetWidthOfPlane(image.0, 0), CVPixelBufferGetHeightOfPlane(image.0, 0));
        let (chroma_w, chroma_h) = (CVPixelBufferGetWidthOfPlane(image.0, 1), CVPixelBufferGetHeightOfPlane(image.0, 1));
        let luma_stride = CVPixelBufferGetBytesPerRowOfPlane(image.0, 0);
        let chroma_stride = CVPixelBufferGetBytesPerRowOfPlane(image.0, 1);
        let luma_base = CVPixelBufferGetBaseAddressOfPlane(image.0, 0);
        let chroma_base = CVPixelBufferGetBaseAddressOfPlane(image.0, 1);
        let frame = (!luma_base.is_null() && !chroma_base.is_null()).then(|| {
            let mut y = Vec::with_capacity(width * height);
            for row in 0..height {
                y.extend_from_slice(std::slice::from_raw_parts(luma_base.add(row * luma_stride), width));
            }
            let (mut u, mut v) = (Vec::with_capacity(chroma_w * chroma_h), Vec::with_capacity(chroma_w * chroma_h));
            for row in 0..chroma_h {
                let line = std::slice::from_raw_parts(chroma_base.add(row * chroma_stride), chroma_w * 2);
                for pair in line.chunks_exact(2) {
                    u.push(pair[0]);
                    v.push(pair[1]);
                }
            }
            OwnedYuv { width: width as u32, height: height as u32, y, u, v, y_stride: width, uv_stride: chroma_w }
        });
        CVPixelBufferUnlockBaseAddress(image.0, LOCK_READ_ONLY);
        frame
    }
}

unsafe fn create_block(data: &[u8]) -> Result<Owned, String> {
    unsafe {
        let mut block = null();
        check(
            CMBlockBufferCreateWithMemoryBlock(
                kCFAllocatorDefault,
                null_mut(),
                data.len(),
                kCFAllocatorDefault,
                null(),
                0,
                data.len(),
                0,
                &mut block,
            ),
            "CMBlockBufferCreateWithMemoryBlock",
        )?;
        let block = Owned(block);
        check(CMBlockBufferReplaceDataBytes(data.as_ptr().cast(), block.0, 0, data.len()), "CMBlockBufferReplaceDataBytes")?;
        Ok(block)
    }
}

fn check(status: OSStatus, call: &str) -> Result<(), String> {
    if status == 0 { Ok(()) } else { Err(format!("{call} failed: OSStatus {status}")) }
}

#[derive(Debug, PartialEq, Eq)]
enum NalKind {
    ParameterSet,
    Slice,
    /// Access unit delimiters and end-of-sequence markers have no place in a sample buffer.
    Skip,
}

fn nal_kind(nal: &[u8], hevc: bool) -> NalKind {
    let Some(&header) = nal.first() else { return NalKind::Skip };
    if hevc {
        match (header >> 1) & 0x3f {
            32..=34 => NalKind::ParameterSet,
            35..=37 => NalKind::Skip,
            _ => NalKind::Slice,
        }
    } else {
        match header & 0x1f {
            7 | 8 => NalKind::ParameterSet,
            9 | 10 | 11 => NalKind::Skip,
            _ => NalKind::Slice,
        }
    }
}

/// NAL unit payloads of an Annex-B access unit (start codes stripped).
fn nal_units(data: &[u8]) -> impl Iterator<Item = &[u8]> {
    let mut starts = Vec::new();
    let mut i = 0;
    while i + 3 <= data.len() {
        if data[i] == 0 && data[i + 1] == 0 && data[i + 2] == 1 {
            starts.push(i + 3);
            i += 3;
        } else {
            i += 1;
        }
    }
    let ends: Vec<usize> = starts.iter().skip(1).map(|&s| s - 3).chain(std::iter::once(data.len())).collect();
    starts.into_iter().zip(ends).map(move |(start, end)| {
        let mut end = end;
        // A four-byte start code leaves its leading zero on the previous unit.
        while end > start && data[end - 1] == 0 {
            end -= 1;
        }
        &data[start..end]
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn splits_annex_b_and_classifies_h264() {
        let au = [0, 0, 0, 1, 0x67, 1, 2, 0, 0, 1, 0x68, 3, 0, 0, 0, 1, 0x09, 0xf0, 0, 0, 1, 0x65, 4, 5];
        let units: Vec<&[u8]> = nal_units(&au).collect();
        assert_eq!(units, vec![&[0x67, 1, 2][..], &[0x68, 3], &[0x09, 0xf0], &[0x65, 4, 5]]);
        let kinds: Vec<NalKind> = units.iter().map(|n| nal_kind(n, false)).collect();
        assert_eq!(kinds, vec![NalKind::ParameterSet, NalKind::ParameterSet, NalKind::Skip, NalKind::Slice]);
    }

    #[test]
    fn classifies_hevc_parameter_sets() {
        assert_eq!(nal_kind(&[0x40, 1], true), NalKind::ParameterSet); // VPS
        assert_eq!(nal_kind(&[0x42, 1], true), NalKind::ParameterSet); // SPS
        assert_eq!(nal_kind(&[0x44, 1], true), NalKind::ParameterSet); // PPS
        assert_eq!(nal_kind(&[0x46, 1], true), NalKind::Skip); // AUD
        assert_eq!(nal_kind(&[0x26, 1], true), NalKind::Slice); // IDR_W_RADL
    }

    /// Decodes an Annex-B file written with access unit delimiters, e.g.
    /// `GFN_VT_SAMPLE=clip.h265 cargo test -p gfn-sink --features sim-video --release -- --ignored --nocapture`.
    #[test]
    #[ignore = "needs GFN_VT_SAMPLE"]
    fn decodes_a_sample_file_in_real_time() {
        let path = std::env::var("GFN_VT_SAMPLE").expect("GFN_VT_SAMPLE");
        let hevc = path.ends_with(".h265") || path.ends_with(".hevc");
        let data = std::fs::read(&path).unwrap();
        let is_aud = |at: usize| {
            data[at..].starts_with(&[0, 0, 1]) && data.get(at + 3).is_some_and(|&h| nal_kind(&[h], hevc) == NalKind::Skip)
        };
        let mut cuts: Vec<usize> = (0..data.len().saturating_sub(4)).filter(|&i| is_aud(i)).collect();
        cuts.push(data.len());
        let codec = if hevc { MediaVideoCodec::H265 } else { MediaVideoCodec::H264 };
        let slot = Arc::new(FrameSlot::default());
        let config = MediaStreamConfig { codec, ..MediaStreamConfig::default() };
        let mut decoder = decoder_factory(Arc::clone(&slot))(&config).unwrap();
        let started = std::time::Instant::now();
        for (index, window) in cuts.windows(2).enumerate() {
            let frame = EncodedFrame {
                mid: "1".into(),
                codec: opennow_streamer_platform_webos::MediaCodec::H264,
                data: Arc::from(&data[window[0]..window[1]]),
                frame_index: Some(index as u32),
                timestamp: index as u64 * 1500,
                clock_rate_hz: 90_000,
                keyframe: index == 0,
                contiguous: true,
                ssrc: None,
            };
            decoder.decode(&frame).unwrap_or_else(|e| panic!("access unit {index}: {e}"));
        }
        let units = cuts.len() - 1;
        let per_frame = started.elapsed().as_secs_f64() * 1000.0 / units as f64;
        let picture = slot.take().expect("a decoded picture");
        println!("{units} access units, {} decoded, {per_frame:.2} ms/frame, {}x{}", decoder.presented_frames(), picture.width, picture.height);
        assert_eq!(decoder.presented_frames(), units as u64);
        assert!(per_frame < 16.6, "slower than 60 fps");
    }
}
