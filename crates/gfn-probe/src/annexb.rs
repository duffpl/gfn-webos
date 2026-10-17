//! Splits an Annex-B elementary stream into access units, the unit Starfish's Feed expects.

use gfn_starfish::VideoCodec;

/// Byte ranges of each NAL unit including its start code.
fn nal_ranges(data: &[u8]) -> Vec<(usize, usize)> {
    let mut starts = Vec::new();
    let mut i = 0;
    while i + 3 <= data.len() {
        if data[i] == 0 && data[i + 1] == 0 && data[i + 2] == 1 {
            let start = if i > 0 && data[i - 1] == 0 { i - 1 } else { i };
            starts.push((start, i + 3));
            i += 3;
        } else {
            i += 1;
        }
    }
    starts
        .iter()
        .enumerate()
        .map(|(n, &(start, _))| (start, starts.get(n + 1).map_or(data.len(), |next| next.0)))
        .collect()
}

fn payload_offset(data: &[u8], range: (usize, usize)) -> usize {
    let mut p = range.0;
    while data[p] == 0 {
        p += 1;
    }
    p + 1
}

/// Whether this NAL starts a new access unit (H.264 7.4.1.2.3 / H.265 7.4.2.4.4, first-slice form).
fn starts_access_unit(codec: VideoCodec, nal: &[u8]) -> bool {
    match codec {
        VideoCodec::H264 => {
            let Some(&header) = nal.first() else { return false };
            match header & 0x1f {
                6..=9 | 14..=18 => true,
                1 | 5 => nal.get(1).is_some_and(|b| b & 0x80 != 0),
                _ => false,
            }
        }
        VideoCodec::H265 => {
            let Some(&header) = nal.first() else { return false };
            match (header >> 1) & 0x3f {
                32..=35 | 39 | 41..=44 | 48..=55 => true,
                0..=31 => nal.get(2).is_some_and(|b| b & 0x80 != 0),
                _ => false,
            }
        }
        // AV1 has no NAL units; the probe only reads .h264 and .h265 files.
        VideoCodec::Av1 => unreachable!("AV1 is not Annex B"),
    }
}

pub fn access_units(codec: VideoCodec, data: &[u8]) -> Vec<&[u8]> {
    let mut units = Vec::new();
    let mut current: Option<usize> = None;
    let mut has_vcl = false;
    for range in nal_ranges(data) {
        let nal = &data[payload_offset(data, range)..range.1];
        let vcl = match codec {
            VideoCodec::H264 => matches!(nal.first().map(|h| h & 0x1f), Some(1 | 5)),
            VideoCodec::H265 => nal.first().is_some_and(|h| (h >> 1) & 0x3f <= 31),
            VideoCodec::Av1 => unreachable!("AV1 is not Annex B"),
        };
        if has_vcl && starts_access_unit(codec, nal) {
            if let Some(start) = current {
                units.push(&data[start..range.0]);
            }
            current = None;
            has_vcl = false;
        }
        current.get_or_insert(range.0);
        has_vcl |= vcl;
    }
    if let Some(start) = current {
        units.push(&data[start..]);
    }
    units
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn groups_parameter_sets_with_the_following_picture() {
        let sps = [0, 0, 0, 1, 0x67, 0x42];
        let pps = [0, 0, 0, 1, 0x68, 0xce];
        let idr = [0, 0, 1, 0x65, 0x88, 0x01];
        let p1_slice0 = [0, 0, 1, 0x41, 0x9a, 0x02];
        let p1_slice1 = [0, 0, 1, 0x41, 0x40, 0x03];
        let p2 = [0, 0, 1, 0x41, 0x9a, 0x04];
        let stream: Vec<u8> = [&sps[..], &pps, &idr, &p1_slice0, &p1_slice1, &p2].concat();
        let units = access_units(VideoCodec::H264, &stream);
        assert_eq!(units.len(), 3);
        assert_eq!(units[0], [&sps[..], &pps, &idr].concat());
        assert_eq!(units[1], [&p1_slice0[..], &p1_slice1].concat());
        assert_eq!(units[2], p2);
    }

    #[test]
    fn splits_hevc_on_first_slice_segment() {
        let vps = [0, 0, 0, 1, 0x40, 0x01, 0x0c];
        let idr = [0, 0, 1, 0x26, 0x01, 0xaf];
        let trail0 = [0, 0, 1, 0x02, 0x01, 0xd0];
        let trail0_seg = [0, 0, 1, 0x02, 0x01, 0x40];
        let stream: Vec<u8> = [&vps[..], &idr, &trail0, &trail0_seg].concat();
        let units = access_units(VideoCodec::H265, &stream);
        assert_eq!(units.len(), 2);
        assert_eq!(units[1], [&trail0[..], &trail0_seg].concat());
    }
}
