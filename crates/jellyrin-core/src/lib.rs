use serde::{Deserialize, Serialize};
use std::cmp::Ordering;
use std::path::Path;
use std::sync::OnceLock;
use time::OffsetDateTime;
use uuid::Uuid;

pub const DEFAULT_HLS_SEGMENT_TIME_SECONDS: u32 = 3;
pub const DEFAULT_HLS_SEGMENT_PATTERN: &str = "segment_%05d.ts";
const HLS_TEXT_SUBTITLE_MAX_INTERLEAVE_DELTA_US: u32 = 1_000_000;

pub const LIVE_TV_REMOTE_USER_AGENT: &str = "VLC/3.0.20 LibVLC/3.0.20";
pub const LIVE_TV_XTREAM_DEFAULT_EPG_LIMIT: usize = 6;
pub const LIVE_TV_XTREAM_MAX_EPG_CHANNELS: usize = 12;
pub const LIVE_TV_XTREAM_MAX_IMPORT_LIMIT: usize = 100_000;

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
    pub sync_play_access: String,
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

/// Whether a TV-library video lives below one of Jellyfin's conventional extras directories.
/// Directory matching is ASCII case-insensitive and ignores surrounding whitespace.
pub fn is_tv_extra_media_item(item: &MediaItem) -> bool {
    Path::new(item.path.strip_prefix("file://").unwrap_or(&item.path))
        .parent()
        .is_some_and(|parent| {
            parent.components().any(|component| {
                component.as_os_str().to_str().is_some_and(|value| {
                    matches!(
                        value.trim().to_ascii_lowercase().as_str(),
                        "extras"
                            | "featurettes"
                            | "special features"
                            | "behind the scenes"
                            | "deleted scenes"
                            | "interviews"
                            | "trailers"
                    )
                })
            })
        })
}

/// Public Jellyfin item type derived from the neutral persisted media fields.
pub fn effective_media_item_type(item: &MediaItem) -> &'static str {
    match (item.media_type.as_str(), item.collection_type.as_deref()) {
        ("Series", _) => "Series",
        ("Video", Some("movies")) => "Movie",
        ("Video", Some("musicvideos" | "musicvideo")) => "MusicVideo",
        ("Video", Some("tvshows" | "tvshow" | "series")) if is_tv_extra_media_item(item) => "Video",
        ("Video", Some("tvshows" | "tvshow" | "series")) => "Episode",
        ("Video", _) => "Video",
        ("Audio", _) => "Audio",
        ("Photo", _) => "Photo",
        ("Book", _) => "Book",
        _ => "BaseItem",
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TvEpisodePathInfo {
    pub series_name: String,
    pub season_number: Option<i32>,
    pub episode_number: Option<i32>,
}

/// Derive Jellyfin's synthetic TV hierarchy solely from the persisted episode name and path.
///
/// Classification (including exclusion of extras) remains the caller's responsibility. Keeping
/// this path parser in core lets catalogue counting and the API use one exact implementation.
pub fn tv_episode_path_info(name: &str, path: &str) -> TvEpisodePathInfo {
    let components = Path::new(path)
        .parent()
        .map(|parent| {
            parent
                .components()
                .filter_map(|component| component.as_os_str().to_str())
                .filter(|component| !component.is_empty())
                .map(str::to_string)
                .collect::<Vec<_>>()
        })
        .unwrap_or_default();
    let season_component_index = components
        .iter()
        .rposition(|component| parse_season_component(component).is_some());
    let season_number = season_component_index
        .and_then(|index| components.get(index))
        .and_then(|component| parse_season_component(component))
        .or_else(|| parse_sxe_numbers(name).map(|(season, _)| season));
    let episode_number = parse_sxe_numbers(name).map(|(_, episode)| episode);
    let series_name = season_component_index
        .and_then(|index| index.checked_sub(1))
        .and_then(|index| components.get(index))
        .cloned()
        .or_else(|| components.last().cloned())
        .unwrap_or_else(|| name.to_string());
    TvEpisodePathInfo {
        series_name,
        season_number,
        episode_number,
    }
}

/// Stable grouping key used when selecting one visible episode per TV series.
pub fn tv_episode_series_key(item: &MediaItem) -> String {
    if effective_media_item_type(item) == "Episode" {
        tv_episode_path_info(&item.name, &item.path)
            .series_name
            .to_ascii_lowercase()
    } else {
        item.virtual_folder_id.to_string()
    }
}

/// Jellyfin-compatible ordering for episodes when metadata is not part of the query.
///
/// Keeping this in core makes streamed database selection and API sorting use the exact same
/// tie-breakers.
pub fn compare_tv_episode_items(left: &MediaItem, right: &MediaItem) -> Ordering {
    let left_info = (effective_media_item_type(left) == "Episode")
        .then(|| tv_episode_path_info(&left.name, &left.path));
    let right_info = (effective_media_item_type(right) == "Episode")
        .then(|| tv_episode_path_info(&right.name, &right.path));
    let left_series = left_info
        .as_ref()
        .map(|info| info.series_name.to_ascii_lowercase())
        .unwrap_or_else(|| left.name.to_ascii_lowercase());
    let right_series = right_info
        .as_ref()
        .map(|info| info.series_name.to_ascii_lowercase())
        .unwrap_or_else(|| right.name.to_ascii_lowercase());
    left_series
        .cmp(&right_series)
        .then_with(|| {
            left_info
                .as_ref()
                .and_then(|info| info.season_number)
                .unwrap_or(i32::MAX)
                .cmp(
                    &right_info
                        .as_ref()
                        .and_then(|info| info.season_number)
                        .unwrap_or(i32::MAX),
                )
        })
        .then_with(|| {
            left_info
                .as_ref()
                .and_then(|info| info.episode_number)
                .unwrap_or(i32::MAX)
                .cmp(
                    &right_info
                        .as_ref()
                        .and_then(|info| info.episode_number)
                        .unwrap_or(i32::MAX),
                )
        })
        .then_with(|| {
            left.name
                .to_ascii_lowercase()
                .cmp(&right.name.to_ascii_lowercase())
        })
        .then_with(|| left.id.cmp(&right.id))
}

fn parse_season_component(value: &str) -> Option<i32> {
    let value = value.trim().to_ascii_lowercase();
    let digits = value
        .strip_prefix("season")
        .or_else(|| value.strip_prefix("series"))
        .map(str::trim)?
        .chars()
        .take_while(|character| character.is_ascii_digit())
        .collect::<String>();
    digits.parse().ok()
}

fn parse_sxe_numbers(value: &str) -> Option<(i32, i32)> {
    let bytes = value.as_bytes();
    for index in 0..bytes.len().saturating_sub(3) {
        if !bytes[index].eq_ignore_ascii_case(&b's') {
            continue;
        }
        let mut cursor = index + 1;
        let season_start = cursor;
        while cursor < bytes.len() && bytes[cursor].is_ascii_digit() {
            cursor += 1;
        }
        if season_start == cursor
            || cursor >= bytes.len()
            || !bytes[cursor].eq_ignore_ascii_case(&b'e')
        {
            continue;
        }
        let episode_start = cursor + 1;
        cursor = episode_start;
        while cursor < bytes.len() && bytes[cursor].is_ascii_digit() {
            cursor += 1;
        }
        if episode_start == cursor {
            continue;
        }
        let season = value[season_start..episode_start - 1].parse().ok()?;
        let episode = value[episode_start..cursor].parse().ok()?;
        return Some((season, episode));
    }
    None
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
    pub is_favorite: bool,
    pub rating: Option<f64>,
    pub updated_at: OffsetDateTime,
}

#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct TranscodeStreamSelection {
    pub video_stream_index: Option<i64>,
    pub audio_stream_index: Option<i64>,
    pub subtitle_stream_index: Option<i64>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum HlsStreamMode {
    Copy,
    Encode,
    Drop,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum H264Preset {
    Ultrafast,
    Superfast,
    Veryfast,
    Faster,
    Fast,
    Medium,
}

/// Process-wide defaults applied when an HLS request is created.
///
/// Keeping the encoder settings together gives the application one value to
/// load at startup and pass explicitly. [`HlsTranscodeRequest::new`] remains a
/// compatibility convenience and caches that complete value once.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct HlsEncodingConfig {
    pub video_threads: Option<u16>,
    pub video_preset: H264Preset,
}

impl Default for HlsEncodingConfig {
    fn default() -> Self {
        Self {
            video_threads: Some(2),
            video_preset: H264Preset::Ultrafast,
        }
    }
}

impl HlsEncodingConfig {
    pub fn from_values(preset: Option<&str>, threads: Option<&str>) -> Self {
        let defaults = Self::default();
        Self {
            video_preset: preset
                .and_then(|value| match value.trim().to_ascii_lowercase().as_str() {
                    "ultrafast" => Some(H264Preset::Ultrafast),
                    "superfast" => Some(H264Preset::Superfast),
                    "veryfast" => Some(H264Preset::Veryfast),
                    "faster" => Some(H264Preset::Faster),
                    "fast" => Some(H264Preset::Fast),
                    "medium" => Some(H264Preset::Medium),
                    _ => None,
                })
                .unwrap_or(defaults.video_preset),
            video_threads: match threads {
                Some(value) if value.eq_ignore_ascii_case("auto") => None,
                Some(value) => value
                    .parse::<u16>()
                    .ok()
                    .filter(|threads| (1..=64).contains(threads))
                    .or(defaults.video_threads),
                None => defaults.video_threads,
            },
        }
    }

    pub fn from_env() -> Self {
        let preset = std::env::var("JELLYRIN_TRANSCODE_PRESET").ok();
        let threads = std::env::var("JELLYRIN_TRANSCODE_THREADS").ok();
        Self::from_values(preset.as_deref(), threads.as_deref())
    }
}

static HLS_ENCODING_CONFIG: OnceLock<HlsEncodingConfig> = OnceLock::new();

/// Loads and returns the process-wide HLS encoder defaults.
///
/// The server calls this during startup so environment validation is complete
/// before requests are accepted. Other embedders can pass an explicit
/// [`HlsEncodingConfig`] to [`HlsTranscodeRequest::new_with_encoding_config`].
pub fn configured_hls_encoding_config() -> &'static HlsEncodingConfig {
    HLS_ENCODING_CONFIG.get_or_init(HlsEncodingConfig::from_env)
}

impl H264Preset {
    fn as_ffmpeg_value(self) -> &'static str {
        match self {
            Self::Ultrafast => "ultrafast",
            Self::Superfast => "superfast",
            Self::Veryfast => "veryfast",
            Self::Faster => "faster",
            Self::Fast => "fast",
            Self::Medium => "medium",
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum H264Profile {
    Baseline,
    Main,
    High,
}

impl H264Profile {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Baseline => "baseline",
            Self::Main => "main",
            Self::High => "high",
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum AacProfile {
    LowComplexity,
}

impl AacProfile {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::LowComplexity => "lc",
        }
    }

    fn as_ffmpeg_value(self) -> &'static str {
        match self {
            Self::LowComplexity => "aac_low",
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct HlsTranscodeRequest {
    pub input_path: String,
    pub output_playlist_path: String,
    pub segment_pattern_path: String,
    pub selection: TranscodeStreamSelection,
    pub include_video: bool,
    pub video_mode: HlsStreamMode,
    pub audio_mode: HlsStreamMode,
    pub start_position_ticks: i64,
    pub duration_ticks: Option<i64>,
    pub hls_start_number: Option<u32>,
    pub output_ts_offset_ticks: Option<i64>,
    pub event_playlist: bool,
    pub hls_list_size: u32,
    pub hls_delete_threshold: Option<u32>,
    pub hls_delete_segments: bool,
    pub hls_omit_endlist: bool,
    pub hls_temp_file: bool,
    pub max_video_width: Option<u32>,
    pub max_video_height: Option<u32>,
    pub video_bitrate: Option<u32>,
    pub video_profile: Option<H264Profile>,
    /// H.264 level encoded as Jellyfin's decimal-tenths integer (`41` = level 4.1).
    pub video_level: Option<u8>,
    /// Maximum encoded frame rate in thousandths of a frame per second.
    pub max_video_frame_rate_millihertz: Option<u32>,
    /// Maximum number of worker threads used by the video encoder.
    ///
    /// A small default prevents one software encode from consuming every host CPU. Set this to
    /// `None` only when an operator deliberately wants FFmpeg's automatic thread selection.
    pub video_threads: Option<u16>,
    pub video_preset: H264Preset,
    pub audio_bitrate: Option<u32>,
    pub audio_profile: Option<AacProfile>,
    pub audio_channels: Option<u8>,
    pub segment_time_seconds: u32,
    pub burn_in_subtitle: bool,
    /// DVB teletext page decoded as text for the selected subtitle stream.
    ///
    /// FFmpeg treats this as an input decoder option, so the command builder must place it before
    /// `-i`. Keeping the value numeric and validating the broadcast page range prevents callers
    /// from injecting libzvbi's comma-separated page expression into the process arguments.
    pub teletext_page: Option<u16>,
    /// Percentage of native input rate used for a finite remote VOD source (110 = 1.10x).
    pub input_readrate_percent: Option<u16>,
    pub input_readrate_initial_burst_seconds: Option<u16>,
}

impl HlsTranscodeRequest {
    pub fn new(
        input_path: impl Into<String>,
        output_playlist_path: impl Into<String>,
        segment_pattern_path: impl Into<String>,
        selection: TranscodeStreamSelection,
    ) -> Self {
        Self::new_with_encoding_config(
            input_path,
            output_playlist_path,
            segment_pattern_path,
            selection,
            *configured_hls_encoding_config(),
        )
    }

    pub fn new_with_encoding_config(
        input_path: impl Into<String>,
        output_playlist_path: impl Into<String>,
        segment_pattern_path: impl Into<String>,
        selection: TranscodeStreamSelection,
        encoding: HlsEncodingConfig,
    ) -> Self {
        Self {
            input_path: input_path.into(),
            output_playlist_path: output_playlist_path.into(),
            segment_pattern_path: segment_pattern_path.into(),
            selection,
            include_video: true,
            video_mode: HlsStreamMode::Encode,
            audio_mode: HlsStreamMode::Encode,
            start_position_ticks: 0,
            duration_ticks: None,
            hls_start_number: None,
            output_ts_offset_ticks: None,
            event_playlist: false,
            hls_list_size: 0,
            hls_delete_threshold: None,
            hls_delete_segments: false,
            hls_omit_endlist: false,
            hls_temp_file: false,
            max_video_width: None,
            max_video_height: None,
            video_bitrate: None,
            video_profile: None,
            video_level: None,
            max_video_frame_rate_millihertz: None,
            video_threads: encoding.video_threads,
            video_preset: encoding.video_preset,
            audio_bitrate: None,
            audio_profile: None,
            audio_channels: None,
            segment_time_seconds: DEFAULT_HLS_SEGMENT_TIME_SECONDS,
            burn_in_subtitle: false,
            teletext_page: None,
            input_readrate_percent: None,
            input_readrate_initial_burst_seconds: None,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct FfmpegCommandSpec {
    program: String,
    args: Vec<String>,
    #[serde(skip)]
    workload: Option<FfmpegWorkload>,
}

/// Workload intent assigned by a trusted FFmpeg command builder.
///
/// An absent intent is deliberately untrusted: callers that assemble arbitrary arguments cannot
/// opt themselves into the cheap remux lane merely by choosing a convenient FFmpeg spelling.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum FfmpegWorkload {
    Remux,
    AudioEncode,
    VideoEncode,
}

impl FfmpegCommandSpec {
    pub fn new(program: impl Into<String>, args: Vec<String>) -> Self {
        Self {
            program: program.into(),
            args,
            workload: None,
        }
    }

    fn with_workload(mut self, workload: FfmpegWorkload) -> Self {
        self.workload = Some(workload);
        self
    }

    pub fn program(&self) -> &str {
        &self.program
    }

    pub fn args(&self) -> &[String] {
        &self.args
    }

    pub fn workload(&self) -> Option<FfmpegWorkload> {
        self.workload
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

    /// Returns FFmpeg's numeric frame rate without retaining arbitrary stderr text.
    pub fn fps_value(&self) -> Option<f64> {
        parse_ffmpeg_nonnegative_metric(self.fps.as_deref()?, false)
    }

    /// Returns FFmpeg's processing speed as a realtime ratio (`1.0` means realtime).
    pub fn speed_ratio(&self) -> Option<f64> {
        parse_ffmpeg_nonnegative_metric(self.speed.as_deref()?, true)
    }
}

fn parse_ffmpeg_nonnegative_metric(value: &str, requires_x_suffix: bool) -> Option<f64> {
    let value = value.trim();
    if value.is_empty() || value.len() > 32 || !value.is_ascii() {
        return None;
    }
    let value = if requires_x_suffix {
        value.strip_suffix('x')?
    } else {
        value
    };
    let parsed = value.parse::<f64>().ok()?;
    (parsed.is_finite() && (0.0..=10_000.0).contains(&parsed)).then_some(parsed)
}

pub fn build_hls_ffmpeg_command(request: &HlsTranscodeRequest) -> FfmpegCommandSpec {
    let should_burn_subtitle = request.include_video
        && request.burn_in_subtitle
        && request
            .selection
            .subtitle_stream_index
            .is_some_and(|index| index >= 0);
    let video_mode = if should_burn_subtitle {
        HlsStreamMode::Encode
    } else {
        request.video_mode
    };
    let decodes_teletext_page = request
        .teletext_page
        .is_some_and(|page| (100..=899).contains(&page))
        && request
            .selection
            .subtitle_stream_index
            .is_some_and(|index| index >= 0)
        && !should_burn_subtitle;

    let mut args = vec![
        "-hide_banner".to_string(),
        "-nostdin".to_string(),
        "-y".to_string(),
        "-loglevel".to_string(),
        "warning".to_string(),
        "-nostats".to_string(),
        "-stats_period".to_string(),
        "2".to_string(),
    ];

    if request.include_video
        && video_mode == HlsStreamMode::Encode
        && let Some(video_threads) = request.video_threads.filter(|threads| *threads > 0)
    {
        // Encoder thread limits do not constrain FFmpeg's filter graphs. Apply the same budget
        // to simple and complex graphs before the input so automatic scaling and subtitle burn-in
        // cannot independently consume all host CPUs.
        args.push("-filter_threads".to_string());
        args.push(video_threads.to_string());
        args.push("-filter_complex_threads".to_string());
        args.push(video_threads.to_string());
    }

    if request.start_position_ticks > 0 {
        args.push("-ss".to_string());
        args.push(format_ticks_as_seconds(request.start_position_ticks));
    }

    if let Some(readrate_percent) = request
        .input_readrate_percent
        .filter(|percent| (1..=1000).contains(percent))
    {
        args.push("-readrate".to_string());
        args.push(format!("{:.2}", f64::from(readrate_percent) / 100.0));
        if let Some(initial_burst_seconds) = request
            .input_readrate_initial_burst_seconds
            .filter(|seconds| *seconds > 0)
        {
            args.push("-readrate_initial_burst".to_string());
            args.push(initial_burst_seconds.to_string());
        }
    }

    if decodes_teletext_page {
        // These private decoder options must precede the input. Text output is substantially
        // cheaper than bitmap teletext and can be encoded directly as WebVTT without touching the
        // video stream.
        args.push("-txt_page".to_string());
        args.push(request.teletext_page.unwrap_or_default().to_string());
        args.push("-txt_format".to_string());
        args.push("text".to_string());
    }

    args.push("-i".to_string());
    args.push(request.input_path.clone());

    if let Some(duration_ticks) = request.duration_ticks
        && duration_ticks > 0
    {
        args.push("-t".to_string());
        args.push(format_ticks_as_seconds(duration_ticks));
    }

    if should_burn_subtitle {
        args.push("-filter_complex".to_string());
        args.push(format!(
            "{}{}overlay=eof_action=pass:repeatlast=0[v]",
            ffmpeg_stream_filter_input("v", request.selection.video_stream_index),
            ffmpeg_stream_filter_input("s", request.selection.subtitle_stream_index),
        ));
        args.push("-map".to_string());
        args.push("[v]".to_string());
    } else if request.include_video && video_mode != HlsStreamMode::Drop {
        push_selected_stream_map(&mut args, "v", request.selection.video_stream_index, true);
    } else {
        args.push("-vn".to_string());
    }
    if request.audio_mode == HlsStreamMode::Drop {
        args.push("-an".to_string());
    } else {
        push_selected_stream_map(&mut args, "a", request.selection.audio_stream_index, true);
    }
    if should_burn_subtitle {
        args.push("-sn".to_string());
    } else if request
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
        if decodes_teletext_page {
            args.push("-threads:s".to_string());
            args.push("1".to_string());
        }
    } else {
        args.push("-sn".to_string());
    }

    if request.include_video && video_mode == HlsStreamMode::Copy {
        args.push("-c:v".to_string());
        args.push("copy".to_string());
    } else if request.include_video && video_mode == HlsStreamMode::Encode {
        args.push("-c:v".to_string());
        args.push("libx264".to_string());
        args.push("-preset".to_string());
        args.push(request.video_preset.as_ffmpeg_value().to_string());
        args.push("-profile:v".to_string());
        args.push(
            request
                .video_profile
                .unwrap_or(H264Profile::Main)
                .as_str()
                .to_string(),
        );
        if let Some(video_level) = request
            .video_level
            .filter(|level| (10..=62).contains(level))
        {
            args.push("-level:v".to_string());
            args.push(format_h264_level(video_level));
        }
        args.push("-pix_fmt".to_string());
        args.push("yuv420p".to_string());
        args.push("-force_key_frames".to_string());
        args.push(format!(
            "expr:gte(t,n_forced*{})",
            request.segment_time_seconds.max(1)
        ));
        args.push("-sc_threshold".to_string());
        args.push("0".to_string());
        if let Some(video_threads) = request.video_threads.filter(|threads| *threads > 0) {
            args.push("-threads:v".to_string());
            args.push(video_threads.to_string());
        }

        if let Some(video_bitrate) = request.video_bitrate {
            args.push("-b:v".to_string());
            args.push(video_bitrate.to_string());
            args.push("-maxrate".to_string());
            args.push(video_bitrate.to_string());
            args.push("-bufsize".to_string());
            args.push(video_bitrate.saturating_mul(2).to_string());
        }

        let mut video_filters = Vec::new();
        if request.max_video_width.is_some() || request.max_video_height.is_some() {
            video_filters.push(scale_filter(
                request.max_video_width,
                request.max_video_height,
            ));
        }
        if let Some(frame_rate) = request
            .max_video_frame_rate_millihertz
            .filter(|frame_rate| *frame_rate > 0)
        {
            video_filters.push(format!("fps={}", format_millihertz(frame_rate)));
        }
        if !video_filters.is_empty() {
            args.push("-vf".to_string());
            args.push(video_filters.join(","));
        }
    }

    match request.audio_mode {
        HlsStreamMode::Copy => {
            args.push("-c:a".to_string());
            args.push("copy".to_string());
        }
        HlsStreamMode::Encode => {
            args.push("-c:a".to_string());
            args.push("aac".to_string());
            if let Some(audio_profile) = request.audio_profile {
                args.push("-profile:a".to_string());
                args.push(audio_profile.as_ffmpeg_value().to_string());
            }
            args.push("-ac".to_string());
            args.push(
                request
                    .audio_channels
                    .filter(|channels| (1..=8).contains(channels))
                    .unwrap_or(2)
                    .to_string(),
            );
            if let Some(audio_bitrate) = request.audio_bitrate {
                args.push("-b:a".to_string());
                args.push(audio_bitrate.to_string());
            }
        }
        HlsStreamMode::Drop => {}
    }

    if !should_burn_subtitle
        && request
            .selection
            .subtitle_stream_index
            .is_some_and(|index| index >= 0)
    {
        // FFmpeg otherwise buffers encoded video until every mapped stream has produced a packet
        // or its ten-second interleave window expires. Sparse text tracks whose first cue appears
        // later make the first HLS segment miss the player's deadline. A one-second mux window
        // publishes empty WebVTT segments normally and does not change the media timestamps.
        args.push("-max_interleave_delta".to_string());
        args.push(HLS_TEXT_SUBTITLE_MAX_INTERLEAVE_DELTA_US.to_string());
    }

    if let Some(output_ts_offset_ticks) = request.output_ts_offset_ticks
        && output_ts_offset_ticks > 0
    {
        args.push("-output_ts_offset".to_string());
        args.push(format_ticks_as_seconds(output_ts_offset_ticks));
    }

    // MPEG-TS otherwise applies its historical 1.4-second mux delay. WebVTT uses the original
    // media timeline, so keeping that delay would make every subtitle appear about 1.4 seconds
    // late and would compound after an HLS seek. Start both streams on the same clock instead.
    args.push("-muxdelay".to_string());
    args.push("0".to_string());
    args.push("-muxpreload".to_string());
    args.push("0".to_string());

    args.push("-f".to_string());
    args.push("hls".to_string());
    args.push("-hls_time".to_string());
    args.push(request.segment_time_seconds.max(1).to_string());
    args.push("-hls_playlist_type".to_string());
    args.push(if request.event_playlist {
        "event".to_string()
    } else {
        "vod".to_string()
    });
    args.push("-hls_list_size".to_string());
    args.push(request.hls_list_size.to_string());
    if let Some(delete_threshold) = request.hls_delete_threshold.filter(|value| *value > 0) {
        args.push("-hls_delete_threshold".to_string());
        args.push(delete_threshold.to_string());
    }
    let mut hls_flags = Vec::new();
    if request.hls_delete_segments {
        hls_flags.push("delete_segments");
    }
    if request.hls_omit_endlist {
        hls_flags.push("omit_endlist");
    }
    if request.hls_temp_file {
        hls_flags.push("temp_file");
    }
    if request.include_video && video_mode == HlsStreamMode::Encode {
        hls_flags.push("independent_segments");
    }
    if !hls_flags.is_empty() {
        args.push("-hls_flags".to_string());
        args.push(hls_flags.join("+"));
    }
    if let Some(start_number) = request.hls_start_number {
        args.push("-start_number".to_string());
        args.push(start_number.to_string());
    }
    args.push("-hls_segment_filename".to_string());
    args.push(request.segment_pattern_path.clone());
    args.push("-progress".to_string());
    args.push("pipe:2".to_string());
    args.push(request.output_playlist_path.clone());

    let workload = if request.include_video && video_mode == HlsStreamMode::Encode {
        FfmpegWorkload::VideoEncode
    } else if request.audio_mode == HlsStreamMode::Encode
        || (request
            .selection
            .subtitle_stream_index
            .is_some_and(|index| index >= 0)
            && !decodes_teletext_page)
    {
        FfmpegWorkload::AudioEncode
    } else {
        FfmpegWorkload::Remux
    };

    FfmpegCommandSpec::new("ffmpeg", args).with_workload(workload)
}

pub fn build_hls_ffmpeg_command_from_stdin(request: &HlsTranscodeRequest) -> FfmpegCommandSpec {
    let mut request = request.clone();
    request.input_path = "pipe:0".to_string();
    let mut command = build_hls_ffmpeg_command(&request);
    command.args.retain(|arg| arg != "-nostdin");
    if let Some(input_index) = command.args.iter().position(|arg| arg == "-i") {
        command.args.splice(
            input_index..input_index,
            ["-f".to_string(), "mpegts".to_string()],
        );
    }
    command
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

fn ffmpeg_stream_filter_input(stream_type: &str, stream_index: Option<i64>) -> String {
    match stream_index {
        Some(index) if index >= 0 => format!("[0:{index}]"),
        _ => format!("[0:{stream_type}:0]"),
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

fn format_h264_level(level: u8) -> String {
    format!("{}.{}", level / 10, level % 10)
}

fn format_millihertz(value: u32) -> String {
    let whole = value / 1_000;
    let fractional = value % 1_000;
    if fractional == 0 {
        return whole.to_string();
    }
    format!("{whole}.{fractional:03}")
        .trim_end_matches('0')
        .to_string()
}

fn format_ticks_as_seconds(ticks: i64) -> String {
    let seconds = (ticks.max(0) as f64) / 10_000_000.0;
    format!("{seconds:.3}")
}

fn non_empty(value: &str) -> Option<String> {
    (!value.is_empty()).then(|| value.to_string())
}

/// Case-insensitive JSON field lookup.
pub fn json_field_case_insensitive<'a>(
    value: &'a serde_json::Value,
    field: &str,
) -> Option<&'a serde_json::Value> {
    value.as_object()?.iter().find_map(|(key, value)| {
        if key.eq_ignore_ascii_case(field) {
            Some(value)
        } else {
            None
        }
    })
}

/// Extract a trimmed, non-empty string from a JSON object field (case-insensitive key).
pub fn json_string_field(value: &serde_json::Value, field: &str) -> Option<String> {
    json_field_case_insensitive(value, field).and_then(|value| match value {
        serde_json::Value::String(value) if !value.trim().is_empty() => {
            Some(value.trim().to_string())
        }
        _ => None,
    })
}

fn comma_delimited_values(value: &str) -> Vec<String> {
    value
        .split(',')
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .map(ToOwned::to_owned)
        .collect()
}

/// Extract a list of strings from a JSON field (supports array or comma-separated string).
pub fn json_string_list_field(value: &serde_json::Value, field: &str) -> Option<Vec<String>> {
    let value = json_field_case_insensitive(value, field)?;
    match value {
        serde_json::Value::Array(values) => Some(
            values
                .iter()
                .filter_map(serde_json::Value::as_str)
                .map(str::trim)
                .filter(|value| !value.is_empty())
                .map(ToOwned::to_owned)
                .collect(),
        ),
        serde_json::Value::String(value) => Some(comma_delimited_values(value)),
        _ => None,
    }
}

/// Parse a u64 from a JSON field (supports number or string).
pub fn live_tv_u64_field(value: &serde_json::Value, key: &str) -> Option<u64> {
    match value.get(key)? {
        serde_json::Value::Number(number) => number.as_u64(),
        serde_json::Value::String(value) => value.parse::<u64>().ok(),
        _ => None,
    }
}

/// Generate a stable lowercase hyphenated ID from a prefix and value.
pub fn live_tv_stable_id(prefix: &str, value: &str) -> String {
    let normalized = value
        .to_ascii_lowercase()
        .chars()
        .map(|ch| if ch.is_ascii_alphanumeric() { ch } else { '-' })
        .collect::<String>()
        .trim_matches('-')
        .to_string();
    if normalized.is_empty() {
        prefix.to_string()
    } else {
        format!("{prefix}-{normalized}")
    }
}

fn fnv1a64(bytes: &[u8], seed: u64) -> u64 {
    bytes.iter().fold(seed, |hash, byte| {
        hash.wrapping_mul(0x100000001b3) ^ u64::from(*byte)
    })
}

/// Generate a stable 32-char hex entity ID from a type and name.
pub fn stable_entity_id(item_type: &str, name: &str) -> String {
    let key = format!("{}:{}", item_type, name.trim().to_ascii_lowercase());
    format!(
        "{:016x}{:016x}",
        fnv1a64(key.as_bytes(), 0xcbf29ce484222325),
        fnv1a64(key.as_bytes(), 0x84222325cbf29ce4)
    )
}

/// Format an OffsetDateTime as RFC 3339 for JSON responses.
pub fn format_time_for_json(value: OffsetDateTime) -> String {
    use time::format_description::well_known::Rfc3339;
    value
        .format(&Rfc3339)
        .unwrap_or_else(|_| "1970-01-01T00:00:00Z".to_string())
}

#[cfg(test)]
mod tests {
    use super::{
        AacProfile, FfmpegCommandSpec, FfmpegProgress, FfmpegWorkload, H264Preset, H264Profile,
        HlsEncodingConfig, HlsStreamMode, HlsTranscodeRequest, MediaItem, TranscodeStreamSelection,
        build_hls_ffmpeg_command, build_hls_ffmpeg_command_from_stdin, compare_tv_episode_items,
        effective_media_item_type, is_tv_extra_media_item, parse_ffmpeg_progress,
        tv_episode_path_info, tv_episode_series_key,
    };

    #[test]
    fn ffmpeg_workload_intent_never_crosses_a_serialization_boundary() {
        let command =
            FfmpegCommandSpec::new("ffmpeg", vec!["-c:v".to_string(), "copy".to_string()])
                .with_workload(FfmpegWorkload::Remux);
        let encoded = serde_json::to_value(&command).unwrap();
        assert!(encoded.get("workload").is_none());
        let decoded: FfmpegCommandSpec = serde_json::from_value(encoded).unwrap();
        assert_eq!(decoded.workload, None);
    }

    #[test]
    fn hls_encoding_config_parses_preset_and_thread_values_together() {
        assert_eq!(
            HlsEncodingConfig::from_values(Some(" veryfast "), Some("4")),
            HlsEncodingConfig {
                video_threads: Some(4),
                video_preset: H264Preset::Veryfast,
            }
        );
        assert_eq!(
            HlsEncodingConfig::from_values(Some("invalid"), Some("auto")),
            HlsEncodingConfig {
                video_threads: None,
                video_preset: H264Preset::Ultrafast,
            }
        );
        assert_eq!(
            HlsEncodingConfig::from_values(None, Some("65")),
            HlsEncodingConfig::default()
        );
        assert_eq!(
            HlsEncodingConfig::from_values(None, Some(" auto ")),
            HlsEncodingConfig::default()
        );
    }

    #[test]
    fn effective_media_item_type_handles_tv_extra_directory_variants() {
        let item = |path: &str| MediaItem {
            id: uuid::Uuid::nil(),
            virtual_folder_id: uuid::Uuid::nil(),
            name: "Clip".to_string(),
            path: path.to_string(),
            media_type: "Video".to_string(),
            collection_type: Some("tvshows".to_string()),
            file_size: None,
            runtime_ticks: None,
            bitrate: None,
            width: None,
            height: None,
            media_streams: Vec::new(),
            created_at: time::OffsetDateTime::UNIX_EPOCH,
            updated_at: time::OffsetDateTime::UNIX_EPOCH,
        };

        for path in [
            "/media/Show/Featurettes/clip.mkv",
            "/media/Show/ Special Features /clip.mkv",
            "/media/Show/Season 01/Extras/clip.mkv",
            "file:///media/Show/Trailers/clip.mkv",
        ] {
            let item = item(path);
            assert!(is_tv_extra_media_item(&item), "path={path}");
            assert_eq!(effective_media_item_type(&item), "Video", "path={path}");
        }

        let episode = item("/media/Show/Season 01/episode.mkv");
        assert!(!is_tv_extra_media_item(&episode));
        assert_eq!(effective_media_item_type(&episode), "Episode");

        let mut persisted_series = item("plugin-vod://catalog/series");
        persisted_series.media_type = "Series".to_string();
        assert_eq!(effective_media_item_type(&persisted_series), "Series");
    }

    #[test]
    fn tv_episode_path_parser_preserves_synthetic_hierarchy_semantics() {
        let nested = tv_episode_path_info(
            "Example S02E03",
            "/media/Example Show/Season 02/Example S02E03.mkv",
        );
        assert_eq!(nested.series_name, "Example Show");
        assert_eq!(nested.season_number, Some(2));
        assert_eq!(nested.episode_number, Some(3));

        let name_fallback = tv_episode_path_info("Fallback S04E05", "");
        assert_eq!(name_fallback.series_name, "Fallback S04E05");
        assert_eq!(name_fallback.season_number, Some(4));
        assert_eq!(name_fallback.episode_number, Some(5));
    }

    #[test]
    fn tv_episode_grouping_and_ordering_share_the_path_parser() {
        let episode = |id, name: &str, path: &str| MediaItem {
            id,
            virtual_folder_id: uuid::Uuid::nil(),
            name: name.to_string(),
            path: path.to_string(),
            media_type: "Video".to_string(),
            collection_type: Some("tvshows".to_string()),
            file_size: None,
            runtime_ticks: None,
            bitrate: None,
            width: None,
            height: None,
            media_streams: Vec::new(),
            created_at: time::OffsetDateTime::UNIX_EPOCH,
            updated_at: time::OffsetDateTime::UNIX_EPOCH,
        };
        let first = episode(
            uuid::Uuid::from_u128(1),
            "Example S01E02",
            "/media/Example Show/Season 01/Example S01E02.mkv",
        );
        let later = episode(
            uuid::Uuid::from_u128(2),
            "Example S02E01",
            "/media/Example Show/Season 02/Example S02E01.mkv",
        );

        assert_eq!(tv_episode_series_key(&first), "example show");
        assert_eq!(tv_episode_series_key(&first), tv_episode_series_key(&later));
        assert!(compare_tv_episode_items(&first, &later).is_lt());
    }

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
                "-loglevel",
                "warning",
                "-nostats",
                "-stats_period",
                "2",
                "-filter_threads",
                "2",
                "-filter_complex_threads",
                "2",
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
                "ultrafast",
                "-profile:v",
                "main",
                "-pix_fmt",
                "yuv420p",
                "-force_key_frames",
                "expr:gte(t,n_forced*3)",
                "-sc_threshold",
                "0",
                "-threads:v",
                "2",
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
                "-ac",
                "2",
                "-b:a",
                "192000",
                "-muxdelay",
                "0",
                "-muxpreload",
                "0",
                "-f",
                "hls",
                "-hls_time",
                "3",
                "-hls_playlist_type",
                "vod",
                "-hls_list_size",
                "0",
                "-hls_flags",
                "independent_segments",
                "-hls_segment_filename",
                "/tmp/jellyrin/transcodes/play-1/segment_%05d.ts",
                "-progress",
                "pipe:2",
                "/tmp/jellyrin/transcodes/play-1/main.m3u8",
            ]
        );
    }

    #[test]
    fn hls_ffmpeg_command_applies_validated_codec_profile_limits() {
        let mut request = HlsTranscodeRequest::new(
            "/media/Movie.mkv",
            "/tmp/main.m3u8",
            "/tmp/segment_%05d.ts",
            TranscodeStreamSelection::default(),
        );
        request.max_video_width = Some(1920);
        request.video_profile = Some(H264Profile::High);
        request.video_level = Some(41);
        request.max_video_frame_rate_millihertz = Some(29_970);
        request.audio_profile = Some(AacProfile::LowComplexity);
        request.audio_channels = Some(6);

        let command = build_hls_ffmpeg_command(&request);

        for pair in [
            ["-profile:v", "high"],
            ["-level:v", "4.1"],
            ["-profile:a", "aac_low"],
            ["-ac", "6"],
        ] {
            assert!(command.args.windows(2).any(|actual| actual == pair));
        }
        assert!(command.args.windows(2).any(|pair| {
            pair[0] == "-vf" && pair[1].contains("min(1920,iw)") && pair[1].contains("fps=29.97")
        }));
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
        assert!(
            command
                .args
                .windows(2)
                .any(|pair| pair == ["-max_interleave_delta", "1000000"])
        );
    }

    #[test]
    fn hls_ffmpeg_command_decodes_one_valid_teletext_page_as_bounded_webvtt() {
        let mut request = HlsTranscodeRequest::new(
            "https://provider.invalid/live.ts",
            "/tmp/main.m3u8",
            "/tmp/segment_%05d.ts",
            TranscodeStreamSelection {
                video_stream_index: None,
                audio_stream_index: None,
                subtitle_stream_index: Some(2),
            },
        );
        request.video_mode = HlsStreamMode::Copy;
        request.audio_mode = HlsStreamMode::Copy;
        request.teletext_page = Some(801);

        let command = build_hls_ffmpeg_command(&request);
        let input_position = command.args.iter().position(|arg| arg == "-i").unwrap();
        let page_position = command
            .args
            .windows(2)
            .position(|pair| pair == ["-txt_page", "801"])
            .unwrap();
        let format_position = command
            .args
            .windows(2)
            .position(|pair| pair == ["-txt_format", "text"])
            .unwrap();

        assert!(page_position < input_position);
        assert!(format_position < input_position);
        assert!(command.args.windows(2).any(|pair| pair == ["-map", "0:2"]));
        assert!(
            command
                .args
                .windows(2)
                .any(|pair| pair == ["-c:s", "webvtt"])
        );
        assert!(
            command
                .args
                .windows(2)
                .any(|pair| pair == ["-threads:s", "1"])
        );
        // Video/audio remain stream-copy and libzvbi's single-page text decoding is bounded to one
        // subtitle thread, so this stays admissible under Jellyrin's production remux-only mode.
        assert_eq!(command.workload(), Some(FfmpegWorkload::Remux));
    }

    #[test]
    fn hls_ffmpeg_command_ignores_invalid_or_unselected_teletext_page() {
        for (page, subtitle_stream_index) in
            [(Some(99), Some(2)), (Some(900), Some(2)), (Some(801), None)]
        {
            let mut request = HlsTranscodeRequest::new(
                "/media/live.ts",
                "/tmp/main.m3u8",
                "/tmp/segment_%05d.ts",
                TranscodeStreamSelection {
                    video_stream_index: None,
                    audio_stream_index: None,
                    subtitle_stream_index,
                },
            );
            request.teletext_page = page;
            let command = build_hls_ffmpeg_command(&request);
            assert!(!command.args.iter().any(|arg| arg == "-txt_page"));
            assert!(!command.args.iter().any(|arg| arg == "-txt_format"));
        }
    }

    #[test]
    fn hls_ffmpeg_command_burns_image_subtitles_into_video() {
        let mut request = HlsTranscodeRequest::new(
            "/media/Movie.mkv",
            "/tmp/main.m3u8",
            "/tmp/segment_%05d.ts",
            TranscodeStreamSelection {
                video_stream_index: Some(0),
                audio_stream_index: Some(1),
                subtitle_stream_index: Some(3),
            },
        );
        request.burn_in_subtitle = true;

        let command = build_hls_ffmpeg_command(&request);

        assert!(command.args.windows(2).any(|pair| {
            pair == [
                "-filter_complex",
                "[0:0][0:3]overlay=eof_action=pass:repeatlast=0[v]",
            ]
        }));
        assert!(command.args.windows(2).any(|pair| pair == ["-map", "[v]"]));
        assert!(command.args.windows(2).any(|pair| pair == ["-map", "0:1?"]));
        assert!(command.args.iter().any(|arg| arg == "-sn"));
        assert!(!command.args.windows(2).any(|pair| pair == ["-map", "0:3"]));
        assert!(
            !command
                .args
                .windows(2)
                .any(|pair| pair == ["-c:s", "webvtt"])
        );
        assert!(
            !command
                .args
                .iter()
                .any(|arg| arg == "-max_interleave_delta")
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
        assert!(!command.args.iter().any(|arg| arg == "-threads:v"));
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
    fn hls_ffmpeg_command_can_disable_or_override_video_thread_limits() {
        let mut request = HlsTranscodeRequest::new(
            "/media/Movie.mkv",
            "/tmp/main.m3u8",
            "/tmp/segment_%05d.ts",
            TranscodeStreamSelection::default(),
        );
        request.video_threads = Some(1);
        let limited = build_hls_ffmpeg_command(&request);
        for pair in [
            ["-threads:v", "1"],
            ["-filter_threads", "1"],
            ["-filter_complex_threads", "1"],
        ] {
            assert!(limited.args.windows(2).any(|actual| actual == pair));
        }

        request.video_threads = None;
        let automatic = build_hls_ffmpeg_command(&request);
        for option in ["-threads:v", "-filter_threads", "-filter_complex_threads"] {
            assert!(!automatic.args.iter().any(|arg| arg == option));
        }
    }

    #[test]
    fn hls_ffmpeg_command_supports_remux_and_partial_transcode() {
        let mut request = HlsTranscodeRequest::new(
            "https://media.example/movie.mkv",
            "/tmp/main.m3u8",
            "/tmp/segment_%05d.ts",
            TranscodeStreamSelection::default(),
        );
        request.video_mode = HlsStreamMode::Copy;
        request.audio_mode = HlsStreamMode::Copy;
        let remux = build_hls_ffmpeg_command(&request);
        assert!(remux.args.windows(2).any(|pair| pair == ["-c:v", "copy"]));
        assert!(remux.args.windows(2).any(|pair| pair == ["-c:a", "copy"]));
        assert!(!remux.args.iter().any(|argument| argument == "libx264"));
        assert!(
            !remux
                .args
                .iter()
                .any(|argument| argument == "-filter_threads"
                    || argument == "-filter_complex_threads")
        );

        request.audio_mode = HlsStreamMode::Encode;
        let partial = build_hls_ffmpeg_command(&request);
        assert!(partial.args.windows(2).any(|pair| pair == ["-c:v", "copy"]));
        assert!(partial.args.windows(2).any(|pair| pair == ["-c:a", "aac"]));
        assert!(!partial.args.iter().any(|argument| argument == "libx264"));
        assert!(
            !partial
                .args
                .iter()
                .any(|argument| argument == "-filter_threads"
                    || argument == "-filter_complex_threads")
        );
    }

    #[test]
    fn hls_ffmpeg_command_rate_limits_remote_vod_before_input() {
        let mut request = HlsTranscodeRequest::new(
            "https://provider.example/movie/42.mkv",
            "/tmp/rate/main.m3u8",
            "/tmp/rate/segment_%05d.ts",
            TranscodeStreamSelection::default(),
        );
        request.input_readrate_percent = Some(110);
        request.input_readrate_initial_burst_seconds = Some(15);

        let command = build_hls_ffmpeg_command(&request);
        let input_index = command.args.iter().position(|arg| arg == "-i").unwrap();
        let readrate_index = command
            .args
            .iter()
            .position(|arg| arg == "-readrate")
            .unwrap();
        let burst_index = command
            .args
            .iter()
            .position(|arg| arg == "-readrate_initial_burst")
            .unwrap();

        assert!(readrate_index < input_index);
        assert!(burst_index < input_index);
        assert_eq!(command.args[readrate_index + 1], "1.10");
        assert_eq!(command.args[burst_index + 1], "15");
        assert!(
            command
                .args
                .windows(2)
                .any(|pair| pair == ["-nostats", "-stats_period"])
        );
    }

    #[test]
    fn hls_ffmpeg_command_supports_bounded_live_window() {
        let mut request = HlsTranscodeRequest::new(
            "https://provider.example/live/42.ts",
            "/tmp/live/main.m3u8",
            "/tmp/live/segment_%05d.ts",
            TranscodeStreamSelection::default(),
        );
        request.event_playlist = true;
        request.hls_list_size = 20;
        request.hls_delete_threshold = Some(2);
        request.hls_delete_segments = true;
        request.hls_omit_endlist = true;
        request.hls_temp_file = true;

        let command = build_hls_ffmpeg_command(&request);
        assert!(
            command
                .args
                .windows(2)
                .any(|pair| pair == ["-hls_list_size", "20"])
        );
        assert!(
            command
                .args
                .windows(2)
                .any(|pair| pair == ["-hls_delete_threshold", "2"])
        );
        assert!(command.args.windows(2).any(|pair| {
            pair == [
                "-hls_flags",
                "delete_segments+omit_endlist+temp_file+independent_segments",
            ]
        }));
    }

    #[test]
    fn hls_ffmpeg_command_can_start_at_specific_segment_number() {
        let mut request = HlsTranscodeRequest::new(
            "/media/Movie.mkv",
            "/tmp/seek/segment_00042.m3u8",
            "/tmp/seek/segment_%05d.ts",
            TranscodeStreamSelection {
                video_stream_index: Some(0),
                audio_stream_index: Some(1),
                subtitle_stream_index: Some(-1),
            },
        );
        request.start_position_ticks = 120_000_000;
        request.duration_ticks = Some(30_000_000);
        request.hls_start_number = Some(42);
        request.output_ts_offset_ticks = Some(1_260_000_000);

        let command = build_hls_ffmpeg_command(&request);

        assert!(
            command
                .args
                .windows(2)
                .any(|pair| pair == ["-ss", "12.000"])
        );
        assert!(command.args.windows(2).any(|pair| pair == ["-t", "3.000"]));
        assert!(
            command
                .args
                .windows(2)
                .any(|pair| pair == ["-start_number", "42"])
        );
        assert!(
            command
                .args
                .windows(2)
                .any(|pair| pair == ["-output_ts_offset", "126.000"])
        );
    }

    #[test]
    fn hls_ffmpeg_command_from_stdin_uses_mpegts_pipe_input() {
        let request = HlsTranscodeRequest::new(
            "hdhomerun://103B4218-0/ch7-3",
            "/tmp/live/main.m3u8",
            "/tmp/live/segment_%05d.ts",
            TranscodeStreamSelection {
                video_stream_index: Some(0),
                audio_stream_index: Some(0),
                subtitle_stream_index: None,
            },
        );

        let command = build_hls_ffmpeg_command_from_stdin(&request);

        assert!(!command.args.iter().any(|arg| arg == "-nostdin"));
        assert!(command.args.windows(2).any(|pair| pair == ["-i", "pipe:0"]));
        assert!(
            command
                .args
                .windows(4)
                .any(|pair| pair == ["-f", "mpegts", "-i", "pipe:0"]),
            "stdin HLS command must force the input demuxer before -i: {:?}",
            command.args
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
        assert_eq!(progress.fps_value(), Some(25.0));
        assert_eq!(progress.speed_ratio(), Some(1.25));
        assert_eq!(progress.progress.as_deref(), Some("continue"));
        assert!(!progress.is_complete());
    }

    #[test]
    fn rejects_unbounded_or_non_numeric_ffmpeg_progress_metrics() {
        for value in [
            "",
            "N/A",
            "NaN",
            "inf",
            "-1",
            "10001",
            "💥",
            "123456789012345678901234567890123",
        ] {
            let progress = FfmpegProgress {
                fps: Some(value.to_string()),
                ..FfmpegProgress::default()
            };
            assert_eq!(progress.fps_value(), None, "unexpected fps value: {value}");
        }

        for value in ["", "1", "1X", "N/Ax", "NaNx", "-1x", "10001x"] {
            let progress = FfmpegProgress {
                speed: Some(value.to_string()),
                ..FfmpegProgress::default()
            };
            assert_eq!(
                progress.speed_ratio(),
                None,
                "unexpected speed value: {value}"
            );
        }
    }

    #[test]
    fn parses_ffmpeg_progress_completion() {
        let progress = parse_ffmpeg_progress("out_time_ms=5000000\nprogress=end\n");

        assert_eq!(progress.position_ticks(), Some(50000000));
        assert!(progress.is_complete());
    }
}
