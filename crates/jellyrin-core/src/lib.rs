use serde::{Deserialize, Serialize};
use time::OffsetDateTime;
use uuid::Uuid;

pub const DEFAULT_HLS_SEGMENT_TIME_SECONDS: u32 = 3;
pub const DEFAULT_HLS_SEGMENT_PATTERN: &str = "segment_%05d.ts";

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ServerState {
    pub server_id: Uuid,
    pub server_name: String,
    pub startup_wizard_completed: bool,
    pub created_at: OffsetDateTime,
    pub updated_at: OffsetDateTime,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct StartupConfig {
    pub server_name: String,
    pub ui_culture: String,
    pub metadata_country_code: String,
    pub preferred_metadata_language: String,
    pub dummy_chapter_duration: i64,
    pub chapter_image_resolution: String,
    pub enable_remote_access: bool,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct User {
    pub id: Uuid,
    pub name: String,
    pub is_administrator: bool,
    pub is_disabled: bool,
    pub created_at: OffsetDateTime,
    pub updated_at: OffsetDateTime,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct DeviceToken {
    pub access_token: String,
    pub user_id: Uuid,
    pub device_id: String,
    pub device_name: String,
    pub client: String,
    pub version: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct VirtualFolder {
    pub id: Uuid,
    pub name: String,
    pub collection_type: Option<String>,
    pub locations: Vec<String>,
    pub created_at: OffsetDateTime,
    pub updated_at: OffsetDateTime,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct MediaItem {
    pub id: Uuid,
    pub virtual_folder_id: Uuid,
    pub name: String,
    pub path: String,
    pub media_type: String,
    pub collection_type: Option<String>,
    pub file_size: Option<i64>,
    pub runtime_ticks: Option<i64>,
    pub bitrate: Option<i64>,
    pub width: Option<i32>,
    pub height: Option<i32>,
    pub media_streams: Vec<serde_json::Value>,
    pub created_at: OffsetDateTime,
    pub updated_at: OffsetDateTime,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PlaybackState {
    pub user_id: Uuid,
    pub item_id: Uuid,
    pub media_source_id: Option<String>,
    pub audio_stream_index: Option<i64>,
    pub subtitle_stream_index: Option<i64>,
    pub position_ticks: i64,
    pub is_paused: bool,
    pub played: bool,
    pub updated_at: OffsetDateTime,
}

#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct TranscodeStreamSelection {
    pub video_stream_index: Option<i64>,
    pub audio_stream_index: Option<i64>,
    pub subtitle_stream_index: Option<i64>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct HlsTranscodeRequest {
    pub input_path: String,
    pub output_playlist_path: String,
    pub segment_pattern_path: String,
    pub selection: TranscodeStreamSelection,
    pub include_video: bool,
    pub start_position_ticks: i64,
    pub max_video_width: Option<u32>,
    pub max_video_height: Option<u32>,
    pub video_bitrate: Option<u32>,
    pub audio_bitrate: Option<u32>,
    pub segment_time_seconds: u32,
}

impl HlsTranscodeRequest {
    pub fn new(
        input_path: impl Into<String>,
        output_playlist_path: impl Into<String>,
        segment_pattern_path: impl Into<String>,
        selection: TranscodeStreamSelection,
    ) -> Self {
        Self {
            input_path: input_path.into(),
            output_playlist_path: output_playlist_path.into(),
            segment_pattern_path: segment_pattern_path.into(),
            selection,
            include_video: true,
            start_position_ticks: 0,
            max_video_width: None,
            max_video_height: None,
            video_bitrate: None,
            audio_bitrate: None,
            segment_time_seconds: DEFAULT_HLS_SEGMENT_TIME_SECONDS,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct FfmpegCommandSpec {
    pub program: String,
    pub args: Vec<String>,
}

impl FfmpegCommandSpec {
    pub fn new(program: impl Into<String>, args: Vec<String>) -> Self {
        Self {
            program: program.into(),
            args,
        }
    }
}

#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct FfmpegProgress {
    pub frame: Option<u64>,
    pub fps: Option<String>,
    pub bitrate: Option<String>,
    pub total_size: Option<u64>,
    pub out_time_us: Option<u64>,
    pub out_time_ms: Option<u64>,
    pub out_time: Option<String>,
    pub speed: Option<String>,
    pub progress: Option<String>,
}

impl FfmpegProgress {
    pub fn position_ticks(&self) -> Option<i64> {
        self.out_time_us
            .or(self.out_time_ms)
            .and_then(|value| value.checked_mul(10))
            .and_then(|value| i64::try_from(value).ok())
    }

    pub fn is_complete(&self) -> bool {
        self.progress
            .as_deref()
            .is_some_and(|value| value.eq_ignore_ascii_case("end"))
    }
}

pub fn build_hls_ffmpeg_command(request: &HlsTranscodeRequest) -> FfmpegCommandSpec {
    let mut args = vec![
        "-hide_banner".to_string(),
        "-nostdin".to_string(),
        "-y".to_string(),
    ];

    if request.start_position_ticks > 0 {
        args.push("-ss".to_string());
        args.push(format_ticks_as_seconds(request.start_position_ticks));
    }

    args.push("-i".to_string());
    args.push(request.input_path.clone());

    if request.include_video {
        push_selected_stream_map(&mut args, "v", request.selection.video_stream_index, true);
    } else {
        args.push("-vn".to_string());
    }
    push_selected_stream_map(&mut args, "a", request.selection.audio_stream_index, true);
    if request
        .selection
        .subtitle_stream_index
        .is_some_and(|index| index >= 0)
    {
        push_selected_stream_map(
            &mut args,
            "s",
            request.selection.subtitle_stream_index,
            false,
        );
        args.push("-c:s".to_string());
        args.push("webvtt".to_string());
    } else {
        args.push("-sn".to_string());
    }

    if request.include_video {
        args.push("-c:v".to_string());
        args.push("libx264".to_string());
        args.push("-preset".to_string());
        args.push("veryfast".to_string());
        args.push("-profile:v".to_string());
        args.push("main".to_string());
        args.push("-pix_fmt".to_string());
        args.push("yuv420p".to_string());

        if let Some(video_bitrate) = request.video_bitrate {
            args.push("-b:v".to_string());
            args.push(video_bitrate.to_string());
            args.push("-maxrate".to_string());
            args.push(video_bitrate.to_string());
            args.push("-bufsize".to_string());
            args.push(video_bitrate.saturating_mul(2).to_string());
        }

        if request.max_video_width.is_some() || request.max_video_height.is_some() {
            args.push("-vf".to_string());
            args.push(scale_filter(
                request.max_video_width,
                request.max_video_height,
            ));
        }
    }

    args.push("-c:a".to_string());
    args.push("aac".to_string());
    if let Some(audio_bitrate) = request.audio_bitrate {
        args.push("-b:a".to_string());
        args.push(audio_bitrate.to_string());
    }

    args.push("-f".to_string());
    args.push("hls".to_string());
    args.push("-hls_time".to_string());
    args.push(request.segment_time_seconds.max(1).to_string());
    args.push("-hls_playlist_type".to_string());
    args.push("event".to_string());
    args.push("-hls_segment_filename".to_string());
    args.push(request.segment_pattern_path.clone());
    args.push("-progress".to_string());
    args.push("pipe:2".to_string());
    args.push(request.output_playlist_path.clone());

    FfmpegCommandSpec::new("ffmpeg", args)
}

pub fn parse_ffmpeg_progress(input: &str) -> FfmpegProgress {
    let mut progress = FfmpegProgress::default();
    for line in input.lines() {
        parse_ffmpeg_progress_line(&mut progress, line);
    }
    progress
}

pub fn parse_ffmpeg_progress_line(progress: &mut FfmpegProgress, line: &str) {
    let Some((key, value)) = line.trim().split_once('=') else {
        return;
    };
    let value = value.trim();
    match key.trim() {
        "frame" => progress.frame = value.parse().ok(),
        "fps" => progress.fps = non_empty(value),
        "bitrate" => progress.bitrate = non_empty(value),
        "total_size" => progress.total_size = value.parse().ok(),
        "out_time_us" => progress.out_time_us = value.parse().ok(),
        "out_time_ms" => progress.out_time_ms = value.parse().ok(),
        "out_time" => progress.out_time = non_empty(value),
        "speed" => progress.speed = non_empty(value),
        "progress" => progress.progress = non_empty(value),
        _ => {}
    }
}

fn push_selected_stream_map(
    args: &mut Vec<String>,
    stream_type: &str,
    stream_index: Option<i64>,
    optional: bool,
) {
    args.push("-map".to_string());
    let optional_suffix = if optional { "?" } else { "" };
    match stream_index {
        Some(index) if index >= 0 => args.push(format!("0:{index}{optional_suffix}")),
        _ => args.push(format!("0:{stream_type}:0{optional_suffix}")),
    }
}

fn scale_filter(max_width: Option<u32>, max_height: Option<u32>) -> String {
    let width = max_width
        .map(|value| value.to_string())
        .unwrap_or_else(|| "-2".to_string());
    let height = max_height
        .map(|value| value.to_string())
        .unwrap_or_else(|| "-2".to_string());
    format!("scale='min({width},iw)':'min({height},ih)':force_original_aspect_ratio=decrease")
}

fn format_ticks_as_seconds(ticks: i64) -> String {
    let seconds = (ticks.max(0) as f64) / 10_000_000.0;
    format!("{seconds:.3}")
}

fn non_empty(value: &str) -> Option<String> {
    (!value.is_empty()).then(|| value.to_string())
}

#[cfg(test)]
mod tests {
    use super::{
        HlsTranscodeRequest, TranscodeStreamSelection, build_hls_ffmpeg_command,
        parse_ffmpeg_progress,
    };

    #[test]
    fn hls_ffmpeg_command_preserves_selected_streams_and_output_paths() {
        let mut request = HlsTranscodeRequest::new(
            "/media/Movie.mkv",
            "/tmp/jellyrin/transcodes/play-1/main.m3u8",
            "/tmp/jellyrin/transcodes/play-1/segment_%05d.ts",
            TranscodeStreamSelection {
                video_stream_index: Some(0),
                audio_stream_index: Some(2),
                subtitle_stream_index: Some(-1),
            },
        );
        request.start_position_ticks = 12_345_000_000;
        request.max_video_width = Some(1280);
        request.max_video_height = Some(720);
        request.video_bitrate = Some(4_000_000);
        request.audio_bitrate = Some(192_000);

        let command = build_hls_ffmpeg_command(&request);

        assert_eq!(command.program, "ffmpeg");
        assert_eq!(
            command.args,
            vec![
                "-hide_banner",
                "-nostdin",
                "-y",
                "-ss",
                "1234.500",
                "-i",
                "/media/Movie.mkv",
                "-map",
                "0:0?",
                "-map",
                "0:2?",
                "-sn",
                "-c:v",
                "libx264",
                "-preset",
                "veryfast",
                "-profile:v",
                "main",
                "-pix_fmt",
                "yuv420p",
                "-b:v",
                "4000000",
                "-maxrate",
                "4000000",
                "-bufsize",
                "8000000",
                "-vf",
                "scale='min(1280,iw)':'min(720,ih)':force_original_aspect_ratio=decrease",
                "-c:a",
                "aac",
                "-b:a",
                "192000",
                "-f",
                "hls",
                "-hls_time",
                "3",
                "-hls_playlist_type",
                "event",
                "-hls_segment_filename",
                "/tmp/jellyrin/transcodes/play-1/segment_%05d.ts",
                "-progress",
                "pipe:2",
                "/tmp/jellyrin/transcodes/play-1/main.m3u8",
            ]
        );
    }

    #[test]
    fn hls_ffmpeg_command_maps_default_streams_and_optional_subtitles() {
        let request = HlsTranscodeRequest::new(
            "/media/Movie.mkv",
            "/tmp/main.m3u8",
            "/tmp/segment_%05d.ts",
            TranscodeStreamSelection {
                video_stream_index: None,
                audio_stream_index: None,
                subtitle_stream_index: Some(3),
            },
        );

        let command = build_hls_ffmpeg_command(&request);

        assert!(
            command
                .args
                .windows(2)
                .any(|pair| pair == ["-map", "0:v:0?"])
        );
        assert!(
            command
                .args
                .windows(2)
                .any(|pair| pair == ["-map", "0:a:0?"])
        );
        assert!(command.args.windows(2).any(|pair| pair == ["-map", "0:3"]));
        assert!(
            command
                .args
                .windows(2)
                .any(|pair| pair == ["-c:s", "webvtt"])
        );
    }

    #[test]
    fn hls_ffmpeg_command_can_transcode_audio_only() {
        let mut request = HlsTranscodeRequest::new(
            "/media/Song.flac",
            "/tmp/audio/main.m3u8",
            "/tmp/audio/segment_%05d.ts",
            TranscodeStreamSelection {
                video_stream_index: None,
                audio_stream_index: Some(1),
                subtitle_stream_index: None,
            },
        );
        request.include_video = false;
        request.audio_bitrate = Some(128_000);

        let command = build_hls_ffmpeg_command(&request);

        assert!(command.args.iter().any(|arg| arg == "-vn"));
        assert!(!command.args.iter().any(|arg| arg == "-c:v"));
        assert!(command.args.windows(2).any(|pair| pair == ["-map", "0:1?"]));
        assert!(command.args.windows(2).any(|pair| pair == ["-c:a", "aac"]));
        assert!(
            command
                .args
                .windows(2)
                .any(|pair| pair == ["-b:a", "128000"])
        );
    }

    #[test]
    fn parses_ffmpeg_progress_protocol() {
        let progress = parse_ffmpeg_progress(
            r#"
frame=42
fps=25.0
bitrate=4000.0kbits/s
total_size=123456
out_time_us=12345678
out_time=00:00:12.345678
speed=1.25x
progress=continue
"#,
        );

        assert_eq!(progress.frame, Some(42));
        assert_eq!(progress.total_size, Some(123456));
        assert_eq!(progress.position_ticks(), Some(123456780));
        assert_eq!(progress.progress.as_deref(), Some("continue"));
        assert!(!progress.is_complete());
    }

    #[test]
    fn parses_ffmpeg_progress_completion() {
        let progress = parse_ffmpeg_progress("out_time_ms=5000000\nprogress=end\n");

        assert_eq!(progress.position_ticks(), Some(50000000));
        assert!(progress.is_complete());
    }
}
