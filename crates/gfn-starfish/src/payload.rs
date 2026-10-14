/// BUFFERSTREAM Load payload for a live, video-only, low-delay stream.
///
/// The template is PlxNative's device-verified video-only payload (`PAYLOAD_V`, engine.rs) with
/// the envelope, app id and provider made variable. Key order is kept as a string template rather
/// than a serde map: the firmware's parser has not been read, so the bytes stay as close to the
/// proven ones as possible. The declared envelope matters: webOS 10 allocates decoder resources
/// against `adaptiveStreaming`, not the bitstream (PlxNative docs/webos10-resource-allocation.md).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LoadPayload {
    pub codec: VideoCodec,
    pub width: u32,
    pub height: u32,
    pub fps: u32,
    pub provider: String,
    pub low_delay: bool,
    pub audio_sync: bool,
    /// `esInfo.pauseAtDecodeTime`; moonlight-tv sets it.
    pub pause_at_decode_time: bool,
    /// Adds `streamQualityInfoCorruptedFrame`, as moonlight-tv does.
    pub corrupted_frame_info: bool,
    /// `transmission.contentsType`. moonlight-tv measured WEBRTC as markedly lower video latency
    /// than LIVE on webOS 4 (ss4s smp_player.c).
    pub contents_type: &'static str,
    /// Ceiling of the pipeline's appsrc video queue in bytes; everything queued there is delay.
    pub video_buffer_max: u32,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum VideoCodec {
    H264,
    H265,
}

impl VideoCodec {
    const fn label(self) -> &'static str {
        match self {
            Self::H264 => "H264",
            Self::H265 => "H265",
        }
    }
}

impl LoadPayload {
    pub fn live(codec: VideoCodec, width: u32, height: u32, fps: u32) -> Self {
        Self {
            codec,
            width,
            height,
            fps,
            provider: "gfnwebos".into(),
            low_delay: true,
            audio_sync: true,
            pause_at_decode_time: false,
            corrupted_frame_info: false,
            contents_type: "WEBRTC",
            video_buffer_max: 1024 * 1024,
        }
    }

    pub fn to_json(&self, app_id: &str, window_id: Option<&str>) -> String {
        let corrupted = if self.corrupted_frame_info { r#""streamQualityInfoCorruptedFrame":true,"# } else { "" };
        let window = window_id.map_or(String::new(), |id| format!(r#","windowId":"{}""#, escape(id)));
        format!(
            concat!(
                r#"{{"args":[{{"mediaTransportType":"BUFFERSTREAM","option":{{"appId":"{app}"{window},"#,
                r#""externalStreamingInfo":{{"contents":{{"codec":{{"video":"{codec}"}},"#,
                r#""esInfo":{{"pauseAtDecodeTime":{pause},"ptsToDecode":0,"seperatedPTS":true}},"#,
                r#""format":"RAW","provider":"{provider}"}},"#,
                r#""streamQualityInfo":true,"streamQualityInfoNonFlushable":true,{corrupted}"#,
                r#""audioSync":{audio_sync},"restartStreaming":false,"#,
                r#""bufferingCtrInfo":{{"bufferMaxLevel":0,"bufferMinLevel":0,"preBufferByte":0,"#,
                r#""qBufferLevelAudio":0,"qBufferLevelVideo":0,"#,
                r#""srcBufferLevelAudio":{{"minimum":1,"maximum":32768}},"#,
                r#""srcBufferLevelVideo":{{"minimum":1,"maximum":{video_buffer_max}}}}}}},"#,
                r#""needAudio":false,"queryPosition":false,"lowDelayMode":{low_delay},"#,
                r#""transmission":{{"contentsType":"{contents_type}"}},"#,
                r#""adaptiveStreaming":{{"audioOnly":false,"maxWidth":{w},"maxHeight":{h},"maxFrameRate":{fps}}}}}}}]}}"#,
            ),
            app = escape(app_id),
            window = window,
            codec = self.codec.label(),
            provider = escape(&self.provider),
            audio_sync = self.audio_sync,
            pause = self.pause_at_decode_time,
            corrupted = corrupted,
            low_delay = self.low_delay,
            video_buffer_max = self.video_buffer_max,
            contents_type = self.contents_type,
            w = self.width,
            h = self.height,
            fps = self.fps,
        )
    }
}

fn escape(value: &str) -> String {
    serde_json::to_string(value).map(|s| s[1..s.len() - 1].to_owned()).unwrap_or_default()
}

#[cfg(test)]
mod tests {
    use super::*;

    /// PlxNative's PAYLOAD_V with only the variable fields substituted.
    const PLX_PAYLOAD_V: &str = r#"{"args":[{"mediaTransportType":"BUFFERSTREAM","option":{"appId":"@APPID@","externalStreamingInfo":{"contents":{"codec":{"video":"H264"},"esInfo":{"pauseAtDecodeTime":false,"ptsToDecode":0,"seperatedPTS":true},"format":"RAW","provider":"plxnative"},"streamQualityInfo":true,"streamQualityInfoNonFlushable":true,"audioSync":true,"restartStreaming":false,"bufferingCtrInfo":{"bufferMaxLevel":0,"bufferMinLevel":0,"preBufferByte":0,"qBufferLevelAudio":0,"qBufferLevelVideo":0,"srcBufferLevelAudio":{"minimum":1,"maximum":32768},"srcBufferLevelVideo":{"minimum":1,"maximum":8388608}}},"needAudio":false,"queryPosition":false,"lowDelayMode":true,"transmission":{"contentsType":"LIVE"},"adaptiveStreaming":{"audioOnly":false,"maxWidth":1920,"maxHeight":1080,"maxFrameRate":30}}}]}"#;

    #[test]
    fn matches_the_device_verified_template() {
        let mut payload = LoadPayload::live(VideoCodec::H264, 1920, 1080, 30);
        payload.provider = "plxnative".into();
        payload.contents_type = "LIVE";
        payload.video_buffer_max = 8_388_608;
        assert_eq!(payload.to_json("@APPID@", None), PLX_PAYLOAD_V);
    }

    #[test]
    fn window_id_follows_app_id_and_output_is_valid_json() {
        let json = LoadPayload::live(VideoCodec::H265, 3840, 2160, 60).to_json("com.x", Some("_Window_Id_1"));
        assert!(json.contains(r#""appId":"com.x","windowId":"_Window_Id_1","externalStreamingInfo""#));
        let parsed: serde_json::Value = serde_json::from_str(&json).unwrap();
        let option = &parsed["args"][0]["option"];
        assert_eq!(option["externalStreamingInfo"]["contents"]["codec"]["video"], "H265");
        assert_eq!(option["adaptiveStreaming"]["maxFrameRate"], 60);
    }
}
