#![recursion_limit = "256"]

use std::{
    cmp::Ordering,
    collections::{BTreeSet, HashMap, HashSet},
    fs,
    path::{Path as FsPath, PathBuf},
    sync::{Arc, OnceLock},
    time::SystemTime,
};

use axum::{
    Json, Router,
    body::Body,
    extract::ws::{Message, WebSocket, WebSocketUpgrade, rejection::WebSocketUpgradeRejection},
    extract::{Path, Query, RawQuery, State},
    http::{HeaderMap, StatusCode, header},
    response::{IntoResponse, Redirect},
    routing::{delete, get, head, post},
};
use jellyrin_compat::{
    AuthenticateUserByNameDto, AuthenticationResultDto, CountryDto, CultureDto, HealthResponse,
    LocalizationOptionDto, PublicSystemInfo, SessionInfoDto, StartupConfigurationDto,
    StartupRemoteAccessDto, StartupUserDto, UserDto, UserPolicyDto,
};
use jellyrin_core::{
    DeviceToken, HlsTranscodeRequest, MediaItem, PlaybackState, StartupConfig,
    TranscodeStreamSelection, User, VirtualFolder, build_hls_ffmpeg_command,
};
use jellyrin_db::{
    ActivePlaybackSession, ActivityLogEntry, ActivityLogFilter, ActivityLogSortField, ApiKey,
    BackupManifest, BrandingConfig, Database, DeviceSession, SortDirection,
    SystemConfigurationPayloads, TaskRun, TranscodeSession, UpsertActivePlaybackSession,
    UpsertPlaybackState, UpsertTranscodeSession,
};
use jellyrin_transcode::{
    HLS_MEDIA_PLAYLIST_NAME, HlsTranscodeLayout, HlsVariantInfo, render_hls_master_playlist,
    spawn_transcode_process, wait_for_hls_readiness,
};
use serde::{Deserialize, Deserializer, de};
use time::{Duration, OffsetDateTime, format_description::well_known::Rfc3339};
use tokio::io::{AsyncReadExt, AsyncSeekExt};
use tokio::sync::{Mutex, broadcast, oneshot};
use tokio_util::io::ReaderStream;
use tower_http::{services::ServeDir, trace::TraceLayer};
use uuid::Uuid;

const COMPATIBLE_SERVER_VERSION: &str = "12.0.0";
const COMPATIBLE_PRODUCT_NAME: &str = "Jellyfin Server";
const DEFAULT_AUTHENTICATION_PROVIDER_ID: &str =
    "Jellyfin.Server.Implementations.Users.DefaultAuthenticationProvider";
const DEFAULT_PASSWORD_RESET_PROVIDER_ID: &str =
    "Jellyfin.Server.Implementations.Users.DefaultPasswordResetProvider";
const ISO_639_2_DATA: &str = include_str!("localization/iso6392.txt");
const COUNTRIES_DATA: &str = include_str!("localization/countries.json");
const TERMINAL_TRANSCODE_CLEANUP_RETENTION_HOURS: i64 = 24;
const TERMINAL_TRANSCODE_CLEANUP_INTERVAL_SECONDS: u64 = 60 * 60;
static PLAYBACK_EVENTS: OnceLock<broadcast::Sender<PlaybackEvent>> = OnceLock::new();
static TRANSCODE_STOPS: OnceLock<Mutex<HashMap<String, oneshot::Sender<()>>>> = OnceLock::new();
static TRANSCODE_DEDUPE_LOCKS: OnceLock<Mutex<HashMap<String, Arc<Mutex<()>>>>> = OnceLock::new();

#[derive(Clone)]
pub struct AppState {
    pub db: Database,
    pub web_dir: PathBuf,
    pub log_dir: PathBuf,
    pub local_address: String,
}

#[derive(Debug, Clone)]
struct PlaybackEvent {
    session_id: String,
    message: serde_json::Value,
}

pub fn router(state: AppState) -> Router {
    let web_dir = state.web_dir.clone();

    Router::new()
        .route("/", get(|| async { Redirect::temporary("/web/") }))
        .route("/health", get(health))
        .route("/healthz", get(health))
        .route("/readyz", get(ready))
        .route("/System/Info/Public", get(system_info_public))
        .route("/System/Info", get(system_info))
        .route("/System/Ping", get(ping))
        .route("/System/Ping", post(ping))
        .route("/system/ping", get(ping))
        .route("/system/ping", post(ping))
        .route("/System/Restart", post(system_admin_noop))
        .route("/system/restart", post(system_admin_noop))
        .route("/System/Shutdown", post(system_admin_noop))
        .route("/system/shutdown", post(system_admin_noop))
        .route("/Backup", get(backups))
        .route("/backup", get(backups))
        .route("/Backup/Create", post(create_backup))
        .route("/backup/create", post(create_backup))
        .route("/Backup/Manifest", get(backup_manifest))
        .route("/backup/manifest", get(backup_manifest))
        .route("/Backup/Restore", post(restore_backup))
        .route("/backup/restore", post(restore_backup))
        .route("/System/Info/Storage", get(system_storage))
        .route("/system/info/storage", get(system_storage))
        .route("/System/Logs", get(system_logs))
        .route("/system/logs", get(system_logs))
        .route("/System/Logs/Log", get(system_log_file))
        .route("/system/logs/log", get(system_log_file))
        .route("/System/ActivityLog/Entries", get(activity_log_entries))
        .route("/system/activitylog/entries", get(activity_log_entries))
        .route("/System/Configuration", get(system_configuration))
        .route("/system/configuration", get(system_configuration))
        .route("/System/Configuration", post(update_system_configuration))
        .route("/system/configuration", post(update_system_configuration))
        .route(
            "/System/Configuration/MetadataOptions/Default",
            get(default_metadata_options),
        )
        .route(
            "/system/configuration/metadataoptions/default",
            get(default_metadata_options),
        )
        .route(
            "/System/Configuration/Branding",
            post(update_branding_configuration),
        )
        .route(
            "/system/configuration/branding",
            post(update_branding_configuration),
        )
        .route("/System/Configuration/{key}", get(named_configuration))
        .route("/system/configuration/{key}", get(named_configuration))
        .route(
            "/System/Configuration/{key}",
            post(update_named_configuration),
        )
        .route(
            "/system/configuration/{key}",
            post(update_named_configuration),
        )
        .route(
            "/Dashboard/web/ConfigurationPages",
            get(dashboard_configuration_pages),
        )
        .route(
            "/web/ConfigurationPages",
            get(dashboard_configuration_pages),
        )
        .route(
            "/dashboard/web/configurationpages",
            get(dashboard_configuration_pages),
        )
        .route(
            "/web/configurationpages",
            get(dashboard_configuration_pages),
        )
        .route("/Dashboard/web/ConfigurationPage", get(empty_text))
        .route("/web/ConfigurationPage", get(empty_text))
        .route("/dashboard/web/configurationpage", get(empty_text))
        .route("/web/configurationpage", get(empty_text))
        .route("/Devices", get(devices))
        .route("/devices", get(devices))
        .route("/Devices/Info", get(device_info))
        .route("/devices/info", get(device_info))
        .route("/Devices/Options", get(device_options))
        .route("/devices/options", get(device_options))
        .route("/Devices/Options", post(update_device_options))
        .route("/devices/options", post(update_device_options))
        .route("/Devices", delete(delete_device))
        .route("/devices", delete(delete_device))
        .route("/Session/Sessions", get(session_sessions))
        .route("/session/sessions", get(session_sessions))
        .route("/Sessions", get(session_sessions))
        .route("/sessions", get(session_sessions))
        .route("/Plugins", get(installed_plugins))
        .route("/plugins", get(installed_plugins))
        .route("/Plugins/{plugin_id}/{version}/Enable", post(enable_plugin))
        .route("/plugins/{plugin_id}/{version}/enable", post(enable_plugin))
        .route(
            "/Plugins/{plugin_id}/{version}/Disable",
            post(disable_plugin),
        )
        .route(
            "/plugins/{plugin_id}/{version}/disable",
            post(disable_plugin),
        )
        .route(
            "/Plugins/{plugin_id}/{version}",
            delete(uninstall_plugin_by_version),
        )
        .route(
            "/plugins/{plugin_id}/{version}",
            delete(uninstall_plugin_by_version),
        )
        .route("/Plugins/{plugin_id}", delete(uninstall_plugin))
        .route("/plugins/{plugin_id}", delete(uninstall_plugin))
        .route(
            "/Plugins/{plugin_id}/Configuration",
            get(plugin_configuration).post(update_plugin_configuration),
        )
        .route(
            "/plugins/{plugin_id}/configuration",
            get(plugin_configuration).post(update_plugin_configuration),
        )
        .route("/Plugins/{plugin_id}/Manifest", get(plugin_manifest))
        .route("/plugins/{plugin_id}/manifest", get(plugin_manifest))
        .route("/Packages", get(available_packages))
        .route("/packages", get(available_packages))
        .route("/Packages/{name}", get(package_info))
        .route("/packages/{name}", get(package_info))
        .route("/Packages/Installed/{name}", post(install_package))
        .route("/packages/installed/{name}", post(install_package))
        .route(
            "/Packages/Installing/{package_id}",
            delete(cancel_package_installation),
        )
        .route(
            "/packages/installing/{package_id}",
            delete(cancel_package_installation),
        )
        .route(
            "/Repositories",
            get(package_repositories).post(update_package_repositories),
        )
        .route(
            "/repositories",
            get(package_repositories).post(update_package_repositories),
        )
        .route("/ScheduledTasks", get(scheduled_tasks))
        .route("/scheduledtasks", get(scheduled_tasks))
        .route("/ScheduledTasks/{task_id}", get(scheduled_task))
        .route("/scheduledtasks/{task_id}", get(scheduled_task))
        .route(
            "/ScheduledTasks/Running/{task_id}",
            post(start_scheduled_task).delete(stop_scheduled_task),
        )
        .route(
            "/scheduledtasks/running/{task_id}",
            post(start_scheduled_task).delete(stop_scheduled_task),
        )
        .route(
            "/ScheduledTasks/{task_id}/Triggers",
            post(update_scheduled_task_triggers),
        )
        .route(
            "/scheduledtasks/{task_id}/triggers",
            post(update_scheduled_task_triggers),
        )
        .route("/Library/Refresh", post(refresh_library))
        .route("/library/refresh", post(refresh_library))
        .route("/Library/MediaFolders", get(user_views_result))
        .route("/library/mediafolders", get(user_views_result))
        .route("/Startup/Configuration", get(get_startup_configuration))
        .route("/Startup/Configuration", post(post_startup_configuration))
        .route("/Startup/RemoteAccess", post(post_startup_remote_access))
        .route("/Startup/User", get(get_startup_user))
        .route("/Startup/FirstUser", get(get_startup_user))
        .route("/Startup/User", post(post_startup_user))
        .route("/Startup/Complete", post(post_startup_complete))
        .route("/Users/Public", get(get_public_users))
        .route("/users/public", get(get_public_users))
        .route("/Auth/Providers", get(authentication_providers))
        .route("/auth/providers", get(authentication_providers))
        .route("/Auth/Keys", get(api_keys).post(create_api_key))
        .route("/auth/keys", get(api_keys).post(create_api_key))
        .route("/Auth/Keys/{key}", delete(revoke_api_key))
        .route("/auth/keys/{key}", delete(revoke_api_key))
        .route(
            "/Auth/PasswordResetProviders",
            get(password_reset_providers),
        )
        .route(
            "/auth/passwordresetproviders",
            get(password_reset_providers),
        )
        .route(
            "/Session/Auth/PasswordResetProviders",
            get(password_reset_providers),
        )
        .route(
            "/session/auth/passwordresetproviders",
            get(password_reset_providers),
        )
        .route("/Users", get(get_users).post(update_user))
        .route("/users", get(get_users).post(update_user))
        .route("/Users/Configuration", post(update_user_configuration))
        .route("/users/configuration", post(update_user_configuration))
        .route("/Users/AuthenticateByName", post(authenticate_by_name))
        .route("/Users/authenticatebyname", post(authenticate_by_name))
        .route("/users/authenticatebyname", post(authenticate_by_name))
        .route("/users/AuthenticateByName", post(authenticate_by_name))
        .route("/Users/Me", get(get_current_user))
        .route("/users/me", get(get_current_user))
        .route(
            "/Users/{user_id}/Configuration",
            post(update_user_configuration_for_path),
        )
        .route(
            "/users/{user_id}/configuration",
            post(update_user_configuration_for_path),
        )
        .route("/Users/{user_id}/Views", get(user_views_result))
        .route("/users/{user_id}/views", get(user_views_result))
        .route("/Users/{user_id}/Items", get(user_items_result))
        .route("/users/{user_id}/items", get(user_items_result))
        .route("/Users/{user_id}/Items/Counts", get(user_item_counts))
        .route("/users/{user_id}/items/counts", get(user_item_counts))
        .route("/Users/{user_id}/Items/Latest", get(user_latest_items))
        .route("/users/{user_id}/items/latest", get(user_latest_items))
        .route("/Users/{user_id}/Items/Resume", get(user_resume_items))
        .route("/users/{user_id}/items/resume", get(user_resume_items))
        .route("/Users/{user_id}", get(get_user_by_id))
        .route("/users/{user_id}", get(get_user_by_id))
        .route("/Users/Password", post(update_user_password))
        .route("/users/password", post(update_user_password))
        .route(
            "/Users/{user_id}/Password",
            post(update_user_password_legacy),
        )
        .route(
            "/users/{user_id}/password",
            post(update_user_password_legacy),
        )
        .route("/Users/{user_id}/Policy", post(update_user_policy))
        .route("/users/{user_id}/policy", post(update_user_policy))
        .route("/Sessions/Logout", post(logout))
        .route("/sessions/logout", post(logout))
        .route("/Sessions/Playing", post(report_playback_start))
        .route("/sessions/playing", post(report_playback_start))
        .route("/Playstate/Sessions/Playing", post(report_playback_start))
        .route("/playstate/sessions/playing", post(report_playback_start))
        .route("/Sessions/{session_id}/Playing", post(send_play_command))
        .route("/sessions/{session_id}/playing", post(send_play_command))
        .route(
            "/Sessions/{session_id}/Playing/{command}",
            post(send_playstate_command),
        )
        .route(
            "/sessions/{session_id}/playing/{command}",
            post(send_playstate_command),
        )
        .route("/Sessions/Playing/Progress", post(report_playback_progress))
        .route("/sessions/playing/progress", post(report_playback_progress))
        .route(
            "/Playstate/Sessions/Playing/Progress",
            post(report_playback_progress),
        )
        .route(
            "/playstate/sessions/playing/progress",
            post(report_playback_progress),
        )
        .route("/Sessions/Playing/Stopped", post(report_playback_stopped))
        .route("/sessions/playing/stopped", post(report_playback_stopped))
        .route(
            "/Playstate/Sessions/Playing/Stopped",
            post(report_playback_stopped),
        )
        .route(
            "/playstate/sessions/playing/stopped",
            post(report_playback_stopped),
        )
        .route(
            "/Playstate/Sessions/Playing/Ping",
            post(ping_playback_session),
        )
        .route(
            "/playstate/sessions/playing/ping",
            post(ping_playback_session),
        )
        .route("/Sessions/Capabilities", post(update_session_capabilities))
        .route(
            "/Sessions/Capabilities/Full",
            post(update_session_capabilities),
        )
        .route("/sessions/capabilities", post(update_session_capabilities))
        .route(
            "/sessions/capabilities/full",
            post(update_session_capabilities),
        )
        .route("/QuickConnect/Enabled", get(quick_connect_enabled))
        .route("/quickconnect/enabled", get(quick_connect_enabled))
        .route("/SyncPlay/List", get(empty_json_array))
        .route("/syncplay/list", get(empty_json_array))
        .route("/LiveTv/Info", get(live_tv_info))
        .route("/livetv/info", get(live_tv_info))
        .route("/LiveTv/GuideInfo", get(live_tv_guide_info))
        .route("/livetv/guideinfo", get(live_tv_guide_info))
        .route(
            "/LiveTv/ChannelMappingOptions",
            get(live_tv_channel_mapping_options),
        )
        .route(
            "/livetv/channelmappingoptions",
            get(live_tv_channel_mapping_options),
        )
        .route("/LiveTv/ChannelMappings", post(set_live_tv_channel_mapping))
        .route("/livetv/channelmappings", post(set_live_tv_channel_mapping))
        .route("/LiveTv/TunerHosts/Types", get(live_tv_tuner_host_types))
        .route("/livetv/tunerhosts/types", get(live_tv_tuner_host_types))
        .route("/LiveTv/TunerHosts", post(add_live_tv_tuner_host))
        .route("/livetv/tunerhosts", post(add_live_tv_tuner_host))
        .route("/LiveTv/TunerHosts", delete(delete_live_tv_tuner_host))
        .route("/livetv/tunerhosts", delete(delete_live_tv_tuner_host))
        .route(
            "/LiveTv/ListingProviders/Default",
            get(default_live_tv_listing_provider),
        )
        .route(
            "/livetv/listingproviders/default",
            get(default_live_tv_listing_provider),
        )
        .route(
            "/LiveTv/ListingProviders",
            post(add_live_tv_listing_provider),
        )
        .route(
            "/livetv/listingproviders",
            post(add_live_tv_listing_provider),
        )
        .route(
            "/LiveTv/ListingProviders",
            delete(delete_live_tv_listing_provider),
        )
        .route(
            "/livetv/listingproviders",
            delete(delete_live_tv_listing_provider),
        )
        .route(
            "/LiveTv/ListingProviders/Lineups",
            get(live_tv_listing_provider_lineups),
        )
        .route(
            "/livetv/listingproviders/lineups",
            get(live_tv_listing_provider_lineups),
        )
        .route(
            "/LiveTv/ListingProviders/SchedulesDirect/Countries",
            get(live_tv_schedules_direct_countries),
        )
        .route(
            "/livetv/listingproviders/schedulesdirect/countries",
            get(live_tv_schedules_direct_countries),
        )
        .route("/LiveTv/Channels", get(empty_items_result))
        .route("/livetv/channels", get(empty_items_result))
        .route("/LiveTv/Programs", get(empty_items_result))
        .route("/livetv/programs", get(empty_items_result))
        .route("/LiveTv/RecommendedPrograms", get(empty_items_result))
        .route("/livetv/recommendedprograms", get(empty_items_result))
        .route("/LiveTv/Programs/Recommended", get(empty_items_result))
        .route("/livetv/programs/recommended", get(empty_items_result))
        .route("/LiveTv/Recordings", get(empty_items_result))
        .route("/livetv/recordings", get(empty_items_result))
        .route("/LiveTv/RecordingGroups", get(empty_items_result))
        .route("/livetv/recordinggroups", get(empty_items_result))
        .route("/LiveTv/Timers", get(empty_result_json))
        .route("/livetv/timers", get(empty_result_json))
        .route("/LiveTv/SeriesTimers", get(empty_result_json))
        .route("/livetv/seriestimers", get(empty_result_json))
        .route("/Branding/Configuration", get(branding_configuration))
        .route("/branding/configuration", get(branding_configuration))
        .route("/Branding/Css", get(branding_css))
        .route("/Branding/Css.css", get(branding_css))
        .route("/Branding/Splashscreen", get(empty_text))
        .route("/branding/splashscreen", get(empty_text))
        .route("/Library/VirtualFolders", get(get_virtual_folders))
        .route("/Library/VirtualFolders", post(add_virtual_folder))
        .route("/Library/VirtualFolders", delete(delete_virtual_folder))
        .route(
            "/Library/VirtualFolders/Paths",
            post(add_virtual_folder_path),
        )
        .route(
            "/Library/VirtualFolders/Paths",
            delete(delete_virtual_folder_path),
        )
        .route("/library/virtualfolders", get(get_virtual_folders))
        .route("/library/virtualfolders", post(add_virtual_folder))
        .route("/library/virtualfolders", delete(delete_virtual_folder))
        .route(
            "/library/virtualfolders/paths",
            post(add_virtual_folder_path),
        )
        .route(
            "/library/virtualfolders/paths",
            delete(delete_virtual_folder_path),
        )
        .route("/Environment/Drives", get(environment_drives))
        .route("/environment/drives", get(environment_drives))
        .route(
            "/Environment/DefaultDirectoryBrowser",
            get(environment_default_directory_browser),
        )
        .route(
            "/environment/defaultdirectorybrowser",
            get(environment_default_directory_browser),
        )
        .route(
            "/Environment/DirectoryContents",
            get(environment_directory_contents),
        )
        .route(
            "/environment/directorycontents",
            get(environment_directory_contents),
        )
        .route("/Environment/ParentPath", get(environment_parent_path))
        .route("/environment/parentpath", get(environment_parent_path))
        .route("/Environment/ValidatePath", post(environment_validate_path))
        .route("/environment/validatepath", post(environment_validate_path))
        .route("/DisplayPreferences/usersettings", get(display_preferences))
        .route("/displaypreferences/usersettings", get(display_preferences))
        .route(
            "/DisplayPreferences/usersettings",
            post(update_display_preferences),
        )
        .route(
            "/displaypreferences/usersettings",
            post(update_display_preferences),
        )
        .route("/System/Endpoint", get(system_endpoint))
        .route("/system/endpoint", get(system_endpoint))
        .route("/Playback/BitrateTest", get(bitrate_test))
        .route("/playback/bitratetest", get(bitrate_test))
        .route("/MediaInfo/Playback/BitrateTest", get(bitrate_test))
        .route("/mediainfo/playback/bitratetest", get(bitrate_test))
        .route("/MediaInfo/LiveStreams/Open", post(open_live_stream))
        .route("/mediainfo/livestreams/open", post(open_live_stream))
        .route("/MediaInfo/LiveStreams/Close", post(close_live_stream))
        .route("/mediainfo/livestreams/close", post(close_live_stream))
        .route("/Videos/ActiveEncodings", get(active_encodings))
        .route("/videos/activeencodings", get(active_encodings))
        .route("/Videos/ActiveEncodings", delete(stop_active_encoding))
        .route("/videos/activeencodings", delete(stop_active_encoding))
        .route(
            "/HlsSegment/Videos/ActiveEncodings",
            get(active_encodings).delete(stop_active_encoding),
        )
        .route(
            "/hlssegment/videos/activeencodings",
            get(active_encodings).delete(stop_active_encoding),
        )
        .route(
            "/HlsSegment/Videos/{item_id}/master.m3u8",
            get(hls_master_playlist),
        )
        .route(
            "/hlssegment/videos/{item_id}/master.m3u8",
            get(hls_master_playlist),
        )
        .route(
            "/HlsSegment/Videos/{item_id}/master.m3u8",
            head(hls_master_playlist_head),
        )
        .route(
            "/hlssegment/videos/{item_id}/master.m3u8",
            head(hls_master_playlist_head),
        )
        .route(
            "/HlsSegment/Videos/{item_id}/main.m3u8",
            get(hls_media_playlist),
        )
        .route(
            "/hlssegment/videos/{item_id}/main.m3u8",
            get(hls_media_playlist),
        )
        .route(
            "/HlsSegment/Videos/{item_id}/main.m3u8",
            head(hls_media_playlist_head),
        )
        .route(
            "/hlssegment/videos/{item_id}/main.m3u8",
            head(hls_media_playlist_head),
        )
        .route(
            "/HlsSegment/Videos/{item_id}/hls1/{playlist_id}/{segment_file}",
            get(hls_segment),
        )
        .route(
            "/hlssegment/videos/{item_id}/hls1/{playlist_id}/{segment_file}",
            get(hls_segment),
        )
        .route(
            "/HlsSegment/Videos/{item_id}/hls1/{playlist_id}/{segment_file}",
            head(hls_segment_head),
        )
        .route(
            "/hlssegment/videos/{item_id}/hls1/{playlist_id}/{segment_file}",
            head(hls_segment_head),
        )
        .route(
            "/DynamicHls/Videos/{item_id}/master.m3u8",
            get(hls_master_playlist),
        )
        .route(
            "/dynamichls/videos/{item_id}/master.m3u8",
            get(hls_master_playlist),
        )
        .route(
            "/DynamicHls/Videos/{item_id}/master.m3u8",
            head(hls_master_playlist_head),
        )
        .route(
            "/dynamichls/videos/{item_id}/master.m3u8",
            head(hls_master_playlist_head),
        )
        .route(
            "/DynamicHls/Videos/{item_id}/main.m3u8",
            get(hls_media_playlist),
        )
        .route(
            "/dynamichls/videos/{item_id}/main.m3u8",
            get(hls_media_playlist),
        )
        .route(
            "/DynamicHls/Videos/{item_id}/live.m3u8",
            get(hls_media_playlist),
        )
        .route(
            "/dynamichls/videos/{item_id}/live.m3u8",
            get(hls_media_playlist),
        )
        .route(
            "/DynamicHls/Videos/{item_id}/hls1/{playlist_id}/{segment_file}",
            get(hls_segment),
        )
        .route(
            "/dynamichls/videos/{item_id}/hls1/{playlist_id}/{segment_file}",
            get(hls_segment),
        )
        .route(
            "/HlsSegment/Videos/{item_id}/hls/{playlist_id}/stream.m3u8",
            get(hls_legacy_media_playlist),
        )
        .route(
            "/hlssegment/videos/{item_id}/hls/{playlist_id}/stream.m3u8",
            get(hls_legacy_media_playlist),
        )
        .route(
            "/HlsSegment/Videos/{item_id}/hls/{playlist_id}/{segment_file}",
            get(hls_segment),
        )
        .route(
            "/hlssegment/videos/{item_id}/hls/{playlist_id}/{segment_file}",
            get(hls_segment),
        )
        .route("/UserViews", get(user_views_result))
        .route("/userviews", get(user_views_result))
        .route("/Items/Counts", get(item_counts))
        .route("/items/counts", get(item_counts))
        .route("/Items", get(items_result))
        .route("/items", get(items_result))
        .route("/Items/Latest", get(latest_items))
        .route("/items/latest", get(latest_items))
        .route("/Items/Filters", get(item_filters))
        .route("/items/filters", get(item_filters))
        .route("/Items/Filters2", get(query_filters))
        .route("/items/filters2", get(query_filters))
        .route("/Filter/Items/Filters", get(item_filters))
        .route("/filter/items/filters", get(item_filters))
        .route("/Filter/Items/Filters2", get(query_filters))
        .route("/filter/items/filters2", get(query_filters))
        .route("/Items/{item_id}/Ancestors", get(item_ancestors))
        .route("/items/{item_id}/ancestors", get(item_ancestors))
        .route(
            "/Items/{item_id}/Similar",
            get(authenticated_item_empty_items),
        )
        .route(
            "/items/{item_id}/similar",
            get(authenticated_item_empty_items),
        )
        .route(
            "/Users/{user_id}/Items/{item_id}/Intros",
            get(user_item_empty_items),
        )
        .route(
            "/users/{user_id}/items/{item_id}/intros",
            get(user_item_empty_items),
        )
        .route(
            "/Items/{item_id}/Images",
            get(authenticated_item_empty_json_array),
        )
        .route(
            "/items/{item_id}/images",
            get(authenticated_item_empty_json_array),
        )
        .route(
            "/Items/{item_id}/ThemeMedia",
            get(authenticated_item_theme_media),
        )
        .route(
            "/items/{item_id}/thememedia",
            get(authenticated_item_theme_media),
        )
        .route(
            "/Items/{item_id}/ThemeSongs",
            get(authenticated_item_theme_items),
        )
        .route(
            "/items/{item_id}/themesongs",
            get(authenticated_item_theme_items),
        )
        .route(
            "/Items/{item_id}/ThemeVideos",
            get(authenticated_item_theme_items),
        )
        .route(
            "/items/{item_id}/themevideos",
            get(authenticated_item_theme_items),
        )
        .route("/Items/{item_id}/PlaybackInfo", get(item_playback_info))
        .route("/items/{item_id}/playbackinfo", get(item_playback_info))
        .route(
            "/MediaInfo/Items/{item_id}/PlaybackInfo",
            get(item_playback_info),
        )
        .route(
            "/mediainfo/items/{item_id}/playbackinfo",
            get(item_playback_info),
        )
        .route(
            "/Items/{item_id}/PlaybackInfo",
            post(post_item_playback_info),
        )
        .route(
            "/items/{item_id}/playbackinfo",
            post(post_item_playback_info),
        )
        .route(
            "/MediaInfo/Items/{item_id}/PlaybackInfo",
            post(post_item_playback_info),
        )
        .route(
            "/mediainfo/items/{item_id}/playbackinfo",
            post(post_item_playback_info),
        )
        .route("/Items/{item_id}/InstantMix", get(instant_mix_from_item))
        .route("/items/{item_id}/instantmix", get(instant_mix_from_item))
        .route("/Songs/{item_id}/InstantMix", get(instant_mix_from_item))
        .route("/songs/{item_id}/instantmix", get(instant_mix_from_item))
        .route("/Videos/{item_id}/stream", get(direct_stream_item))
        .route("/videos/{item_id}/stream", get(direct_stream_item))
        .route("/Videos/{item_id}/stream", head(direct_stream_item_head))
        .route("/videos/{item_id}/stream", head(direct_stream_item_head))
        .route("/Videos/{item_id}/master.m3u8", get(hls_master_playlist))
        .route("/videos/{item_id}/master.m3u8", get(hls_master_playlist))
        .route(
            "/Videos/{item_id}/master.m3u8",
            head(hls_master_playlist_head),
        )
        .route(
            "/videos/{item_id}/master.m3u8",
            head(hls_master_playlist_head),
        )
        .route("/Videos/{item_id}/main.m3u8", get(hls_media_playlist))
        .route("/videos/{item_id}/main.m3u8", get(hls_media_playlist))
        .route("/Videos/{item_id}/main.m3u8", head(hls_media_playlist_head))
        .route("/videos/{item_id}/main.m3u8", head(hls_media_playlist_head))
        .route(
            "/Videos/{item_id}/hls1/{playlist_id}/{segment_file}",
            get(hls_segment),
        )
        .route(
            "/videos/{item_id}/hls1/{playlist_id}/{segment_file}",
            get(hls_segment),
        )
        .route(
            "/Videos/{item_id}/hls1/{playlist_id}/{segment_file}",
            head(hls_segment_head),
        )
        .route(
            "/videos/{item_id}/hls1/{playlist_id}/{segment_file}",
            head(hls_segment_head),
        )
        .route(
            "/Videos/{item_id}/hls/{playlist_id}/stream.m3u8",
            get(hls_legacy_media_playlist),
        )
        .route(
            "/videos/{item_id}/hls/{playlist_id}/stream.m3u8",
            get(hls_legacy_media_playlist),
        )
        .route(
            "/Videos/{item_id}/hls/{playlist_id}/{segment_file}",
            get(hls_segment),
        )
        .route(
            "/videos/{item_id}/hls/{playlist_id}/{segment_file}",
            get(hls_segment),
        )
        .route(
            "/Videos/{item_id}/stream.{container}",
            get(direct_stream_item_by_container),
        )
        .route(
            "/videos/{item_id}/stream.{container}",
            get(direct_stream_item_by_container),
        )
        .route(
            "/Videos/{item_id}/stream.{container}",
            head(direct_stream_item_by_container_head),
        )
        .route(
            "/videos/{item_id}/stream.{container}",
            head(direct_stream_item_by_container_head),
        )
        .route("/Videos/MergeVersions", post(merge_video_versions))
        .route("/videos/mergeversions", post(merge_video_versions))
        .route(
            "/Videos/{item_id}/AdditionalParts",
            get(video_additional_parts),
        )
        .route(
            "/videos/{item_id}/additionalparts",
            get(video_additional_parts),
        )
        .route(
            "/Videos/{item_id}/AlternateSources",
            delete(delete_video_alternate_sources),
        )
        .route(
            "/videos/{item_id}/alternatesources",
            delete(delete_video_alternate_sources),
        )
        .route("/Audio/{item_id}/stream", get(direct_stream_audio))
        .route("/audio/{item_id}/stream", get(direct_stream_audio))
        .route("/Audio/{item_id}/stream", head(direct_stream_audio_head))
        .route("/audio/{item_id}/stream", head(direct_stream_audio_head))
        .route("/Audio/{item_id}/universal", get(universal_audio))
        .route("/audio/{item_id}/universal", get(universal_audio))
        .route(
            "/UniversalAudio/Audio/{item_id}/universal",
            get(universal_audio),
        )
        .route(
            "/universalaudio/audio/{item_id}/universal",
            get(universal_audio),
        )
        .route("/Audio/{item_id}/universal", head(universal_audio_head))
        .route("/audio/{item_id}/universal", head(universal_audio_head))
        .route(
            "/UniversalAudio/Audio/{item_id}/universal",
            head(universal_audio_head),
        )
        .route(
            "/universalaudio/audio/{item_id}/universal",
            head(universal_audio_head),
        )
        .route(
            "/Audio/{item_id}/stream.{container}",
            get(direct_stream_audio_by_container),
        )
        .route(
            "/audio/{item_id}/stream.{container}",
            get(direct_stream_audio_by_container),
        )
        .route(
            "/Audio/{item_id}/stream.{container}",
            head(direct_stream_audio_by_container_head),
        )
        .route(
            "/audio/{item_id}/stream.{container}",
            head(direct_stream_audio_by_container_head),
        )
        .route("/Items/{item_id}", get(item_detail))
        .route("/items/{item_id}", get(item_detail))
        .route("/Users/{user_id}/Items/{item_id}", get(user_item_detail))
        .route("/users/{user_id}/items/{item_id}", get(user_item_detail))
        .route(
            "/Users/{user_id}/PlayedItems/{item_id}",
            post(mark_item_played),
        )
        .route(
            "/users/{user_id}/playeditems/{item_id}",
            post(mark_item_played),
        )
        .route(
            "/Playstate/UserPlayedItems/{item_id}",
            post(mark_authenticated_item_played),
        )
        .route(
            "/playstate/userplayeditems/{item_id}",
            post(mark_authenticated_item_played),
        )
        .route(
            "/Playstate/Users/{user_id}/PlayedItems/{item_id}",
            post(mark_item_played),
        )
        .route(
            "/playstate/users/{user_id}/playeditems/{item_id}",
            post(mark_item_played),
        )
        .route(
            "/Users/{user_id}/PlayedItems/{item_id}",
            delete(mark_item_unplayed),
        )
        .route(
            "/users/{user_id}/playeditems/{item_id}",
            delete(mark_item_unplayed),
        )
        .route(
            "/Playstate/UserPlayedItems/{item_id}",
            delete(mark_authenticated_item_unplayed),
        )
        .route(
            "/playstate/userplayeditems/{item_id}",
            delete(mark_authenticated_item_unplayed),
        )
        .route(
            "/Playstate/Users/{user_id}/PlayedItems/{item_id}",
            delete(mark_item_unplayed),
        )
        .route(
            "/playstate/users/{user_id}/playeditems/{item_id}",
            delete(mark_item_unplayed),
        )
        .route(
            "/Playstate/PlayingItems/{item_id}",
            post(report_path_playback_start),
        )
        .route(
            "/playstate/playingitems/{item_id}",
            post(report_path_playback_start),
        )
        .route(
            "/Playstate/Users/{user_id}/PlayingItems/{item_id}",
            post(report_path_playback_start_legacy),
        )
        .route(
            "/playstate/users/{user_id}/playingitems/{item_id}",
            post(report_path_playback_start_legacy),
        )
        .route(
            "/Playstate/PlayingItems/{item_id}/Progress",
            post(report_path_playback_progress),
        )
        .route(
            "/playstate/playingitems/{item_id}/progress",
            post(report_path_playback_progress),
        )
        .route(
            "/Playstate/Users/{user_id}/PlayingItems/{item_id}/Progress",
            post(report_path_playback_progress_legacy),
        )
        .route(
            "/playstate/users/{user_id}/playingitems/{item_id}/progress",
            post(report_path_playback_progress_legacy),
        )
        .route(
            "/Playstate/PlayingItems/{item_id}",
            delete(report_path_playback_stopped),
        )
        .route(
            "/playstate/playingitems/{item_id}",
            delete(report_path_playback_stopped),
        )
        .route(
            "/Playstate/Users/{user_id}/PlayingItems/{item_id}",
            delete(report_path_playback_stopped_legacy),
        )
        .route(
            "/playstate/users/{user_id}/playingitems/{item_id}",
            delete(report_path_playback_stopped_legacy),
        )
        .route(
            "/Items/{item_id}/Images/{image_type}",
            get(item_placeholder_image),
        )
        .route(
            "/items/{item_id}/images/{image_type}",
            get(item_placeholder_image),
        )
        .route(
            "/Users/{user_id}/Images/{image_type}",
            get(user_placeholder_image),
        )
        .route(
            "/users/{user_id}/images/{image_type}",
            get(user_placeholder_image),
        )
        .route("/Shows/NextUp", get(authenticated_empty_items))
        .route("/shows/nextup", get(authenticated_empty_items))
        .route("/Shows/Upcoming", get(authenticated_empty_items))
        .route("/shows/upcoming", get(authenticated_empty_items))
        .route(
            "/Movies/Recommendations",
            get(authenticated_empty_json_array),
        )
        .route(
            "/movies/recommendations",
            get(authenticated_empty_json_array),
        )
        .route("/Genres", get(authenticated_empty_items))
        .route("/genres", get(authenticated_empty_items))
        .route("/Persons", get(authenticated_empty_items))
        .route("/persons", get(authenticated_empty_items))
        .route("/Studios", get(authenticated_empty_items))
        .route("/studios", get(authenticated_empty_items))
        .route("/Years", get(authenticated_empty_items))
        .route("/years", get(authenticated_empty_items))
        .route("/Channels", get(channels))
        .route("/channels", get(channels))
        .route("/Channels/Features", get(authenticated_empty_json_array))
        .route("/channels/features", get(authenticated_empty_json_array))
        .route("/Artists", get(authenticated_empty_items))
        .route("/artists", get(authenticated_empty_items))
        .route("/AlbumArtists", get(authenticated_empty_items))
        .route("/albumartists", get(authenticated_empty_items))
        .route("/Albums", get(authenticated_empty_items))
        .route("/albums", get(authenticated_empty_items))
        .route("/UserItems/Resume", get(resume_items))
        .route("/useritems/resume", get(resume_items))
        .route("/Localization/Options", get(localization_options))
        .route("/Localization/Cultures", get(localization_cultures))
        .route("/Localization/cultures", get(localization_cultures))
        .route("/Localization/Countries", get(localization_countries))
        .route("/Localization/countries", get(localization_countries))
        .route("/Localization/ParentalRatings", get(parental_ratings))
        .route("/Localization/parentalratings", get(parental_ratings))
        .route("/localization/parentalratings", get(parental_ratings))
        .route("/socket", get(websocket))
        .nest_service(
            "/web",
            ServeDir::new(web_dir).append_index_html_on_directories(true),
        )
        .layer(TraceLayer::new_for_http())
        .with_state(state)
}

pub async fn reconcile_transcode_sessions_on_startup(db: &Database) -> anyhow::Result<usize> {
    let sessions = db.stale_transcode_sessions_on_startup().await?;
    let count = sessions.len();
    for session in sessions {
        db.update_transcode_session_status(&session.play_session_id, "stopped")
            .await?;
        cleanup_hls_transcode_files(&session.output_path).await;
    }
    Ok(count)
}

pub fn spawn_periodic_transcode_cleanup(db: Database) -> tokio::task::JoinHandle<()> {
    tokio::spawn(async move {
        loop {
            match cleanup_stale_hls_transcodes(&db).await {
                Ok(cleaned_count) if cleaned_count > 0 => {
                    tracing::info!(count = cleaned_count, "cleaned stale HLS transcode outputs");
                }
                Ok(_) => {}
                Err(error) => {
                    tracing::warn!(%error, "failed to clean stale HLS transcode outputs");
                }
            }
            tokio::time::sleep(std::time::Duration::from_secs(
                TERMINAL_TRANSCODE_CLEANUP_INTERVAL_SECONDS,
            ))
            .await;
        }
    })
}

pub async fn cleanup_stale_hls_transcodes(db: &Database) -> anyhow::Result<usize> {
    let retention = Duration::hours(TERMINAL_TRANSCODE_CLEANUP_RETENTION_HOURS);
    let terminal_count = cleanup_terminal_hls_transcodes(db, retention).await?;
    let orphan_count =
        cleanup_orphan_hls_transcode_dirs(db, transcode_temp_root(), retention).await?;
    Ok(terminal_count + orphan_count)
}

pub async fn cleanup_terminal_hls_transcodes(
    db: &Database,
    older_than: Duration,
) -> anyhow::Result<usize> {
    let sessions = db
        .terminal_transcode_sessions_older_than(older_than)
        .await?;
    let mut count = 0;
    for session in sessions {
        if cleanup_hls_transcode_files(&session.output_path).await {
            count += 1;
        }
    }
    Ok(count)
}

pub async fn cleanup_orphan_hls_transcode_dirs(
    db: &Database,
    root: impl AsRef<FsPath>,
    older_than: Duration,
) -> anyhow::Result<usize> {
    let root = root.as_ref();
    let mut known_session_dirs = HashSet::new();
    for output_path in db.transcode_session_output_paths().await? {
        known_session_dirs
            .insert(HlsTranscodeLayout::from_media_playlist_path(output_path).session_dir);
    }

    let mut cleaned_count = 0;
    let mut entries = match tokio::fs::read_dir(root).await {
        Ok(entries) => entries,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(0),
        Err(error) => return Err(error.into()),
    };
    while let Some(entry) = entries.next_entry().await? {
        let path = entry.path();
        let file_type = entry.file_type().await?;
        if !file_type.is_dir() || known_session_dirs.contains(&path) {
            continue;
        }
        if !hls_transcode_dir_is_cleanup_candidate(&path).await {
            continue;
        }
        if !hls_transcode_dir_is_old_enough(&path, older_than).await {
            continue;
        }
        if cleanup_hls_transcode_dir(&path).await {
            cleaned_count += 1;
        }
    }
    Ok(cleaned_count)
}

async fn hls_transcode_dir_is_cleanup_candidate(path: &FsPath) -> bool {
    let mut entries = match tokio::fs::read_dir(path).await {
        Ok(entries) => entries,
        Err(_) => return false,
    };
    let mut is_empty = true;
    while let Ok(Some(entry)) = entries.next_entry().await {
        is_empty = false;
        let file_name = entry.file_name();
        let file_name = file_name.to_string_lossy();
        if file_name == HLS_MEDIA_PLAYLIST_NAME
            || (file_name.starts_with("segment_") && file_name.ends_with(".ts"))
        {
            return true;
        }
    }
    is_empty
}

async fn hls_transcode_dir_is_old_enough(path: &FsPath, older_than: Duration) -> bool {
    if older_than <= Duration::ZERO {
        return true;
    }
    let Ok(metadata) = tokio::fs::metadata(path).await else {
        return false;
    };
    let Ok(modified_at) = metadata.modified() else {
        return false;
    };
    SystemTime::now()
        .duration_since(modified_at)
        .is_ok_and(|age| age >= older_than.unsigned_abs())
}

async fn health() -> Json<HealthResponse> {
    Json(HealthResponse { status: "Healthy" })
}

async fn ready(State(state): State<AppState>) -> Result<Json<HealthResponse>, ApiError> {
    sqlx::query("SELECT 1").execute(state.db.pool()).await?;
    Ok(Json(HealthResponse { status: "Ready" }))
}

async fn system_info_public(
    State(state): State<AppState>,
) -> Result<Json<PublicSystemInfo>, ApiError> {
    let server = state.db.server_state().await?;
    Ok(Json(PublicSystemInfo {
        id: server.server_id,
        server_name: server.server_name,
        version: COMPATIBLE_SERVER_VERSION.to_string(),
        product_name: COMPATIBLE_PRODUCT_NAME.to_string(),
        operating_system: "Linux".to_string(),
        local_address: state.local_address,
        startup_wizard_completed: server.startup_wizard_completed,
    }))
}

async fn system_info(
    State(state): State<AppState>,
    headers: HeaderMap,
    Query(query): Query<AuthQuery>,
) -> Result<Json<PublicSystemInfo>, ApiError> {
    require_user(&state.db, &headers, query.api_key.as_deref()).await?;
    system_info_public(State(state)).await
}

async fn get_startup_configuration(
    State(state): State<AppState>,
) -> Result<Json<StartupConfigurationDto>, ApiError> {
    Ok(Json(startup_config_to_dto(
        state.db.startup_config().await?,
    )))
}

async fn post_startup_configuration(
    State(state): State<AppState>,
    Json(payload): Json<StartupConfigurationDto>,
) -> Result<StatusCode, ApiError> {
    require_startup_wizard_incomplete(&state.db).await?;
    let current = state.db.startup_config().await?;
    state
        .db
        .update_startup_config(StartupConfig {
            server_name: payload.server_name,
            ui_culture: payload.ui_culture,
            metadata_country_code: payload.metadata_country_code,
            preferred_metadata_language: payload.preferred_metadata_language,
            dummy_chapter_duration: current.dummy_chapter_duration,
            chapter_image_resolution: current.chapter_image_resolution,
            enable_remote_access: current.enable_remote_access,
        })
        .await?;
    Ok(StatusCode::NO_CONTENT)
}

async fn post_startup_remote_access(
    State(state): State<AppState>,
    Json(payload): Json<StartupRemoteAccessDto>,
) -> Result<StatusCode, ApiError> {
    require_startup_wizard_incomplete(&state.db).await?;
    state
        .db
        .set_remote_access(payload.enable_remote_access)
        .await?;
    Ok(StatusCode::NO_CONTENT)
}

async fn get_startup_user(State(state): State<AppState>) -> Result<Json<StartupUserDto>, ApiError> {
    let user = state.db.first_user().await?;
    Ok(Json(StartupUserDto {
        name: Some(user.name),
        password: None,
    }))
}

async fn post_startup_user(
    State(state): State<AppState>,
    Json(payload): Json<StartupUserDto>,
) -> Result<StatusCode, ApiError> {
    require_startup_wizard_incomplete(&state.db).await?;
    let name = payload
        .name
        .as_deref()
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .unwrap_or("admin")
        .to_string();
    let password = payload
        .password
        .as_deref()
        .filter(|value| !value.is_empty())
        .ok_or_else(|| ApiError::bad_request("Password must not be empty"))?;

    state.db.update_first_user(name, password).await?;
    Ok(StatusCode::NO_CONTENT)
}

async fn post_startup_complete(State(state): State<AppState>) -> Result<StatusCode, ApiError> {
    require_startup_wizard_incomplete(&state.db).await?;
    state.db.complete_startup_wizard().await?;
    Ok(StatusCode::NO_CONTENT)
}

async fn get_public_users(State(state): State<AppState>) -> Result<Json<Vec<UserDto>>, ApiError> {
    let server = state.db.server_state().await?;
    let users = state.db.users().await?;
    let mut dtos = Vec::with_capacity(users.len());
    for user in &users {
        dtos.push(user_to_dto(&state.db, user, server.server_id).await?);
    }
    Ok(Json(dtos))
}

async fn get_users(
    State(state): State<AppState>,
    headers: HeaderMap,
    Query(query): Query<AuthQuery>,
) -> Result<Json<Vec<UserDto>>, ApiError> {
    require_admin(&state.db, &headers, query.api_key.as_deref()).await?;
    let server = state.db.server_state().await?;
    let users = state.db.users().await?;
    let mut dtos = Vec::with_capacity(users.len());
    for user in &users {
        dtos.push(user_to_dto(&state.db, user, server.server_id).await?);
    }
    Ok(Json(dtos))
}

async fn authentication_providers(
    State(state): State<AppState>,
    headers: HeaderMap,
    Query(query): Query<AuthQuery>,
) -> Result<Json<Vec<serde_json::Value>>, ApiError> {
    require_admin(&state.db, &headers, query.api_key.as_deref()).await?;
    Ok(Json(vec![serde_json::json!({
        "Name": "Default",
        "Id": DEFAULT_AUTHENTICATION_PROVIDER_ID
    })]))
}

async fn password_reset_providers(
    State(state): State<AppState>,
    headers: HeaderMap,
    Query(query): Query<AuthQuery>,
) -> Result<Json<Vec<serde_json::Value>>, ApiError> {
    require_admin(&state.db, &headers, query.api_key.as_deref()).await?;
    Ok(Json(vec![serde_json::json!({
        "Name": "Default",
        "Id": DEFAULT_PASSWORD_RESET_PROVIDER_ID
    })]))
}

#[derive(Debug, Deserialize)]
struct BackupManifestQuery {
    #[serde(flatten)]
    auth: AuthQuery,
    path: String,
}

#[derive(Debug, Deserialize)]
struct BackupOptionsBody {
    #[serde(alias = "Metadata")]
    metadata: Option<bool>,
    #[serde(alias = "Trickplay")]
    trickplay: Option<bool>,
    #[serde(alias = "Subtitles")]
    subtitles: Option<bool>,
    #[serde(alias = "Database")]
    database: Option<bool>,
}

#[derive(Debug, Deserialize)]
struct BackupRestoreBody {
    #[serde(alias = "ArchiveFileName")]
    archive_file_name: String,
}

async fn backups(
    State(state): State<AppState>,
    headers: HeaderMap,
    Query(query): Query<AuthQuery>,
) -> Result<Json<Vec<serde_json::Value>>, ApiError> {
    require_admin(&state.db, &headers, query.api_key.as_deref()).await?;
    let manifests = state.db.backup_manifests().await?;
    Ok(Json(manifests.iter().map(backup_manifest_json).collect()))
}

async fn create_backup(
    State(state): State<AppState>,
    headers: HeaderMap,
    Query(query): Query<AuthQuery>,
    Json(payload): Json<Option<BackupOptionsBody>>,
) -> Result<Json<serde_json::Value>, ApiError> {
    let user = require_admin(&state.db, &headers, query.api_key.as_deref()).await?;
    let manifest = state
        .db
        .create_backup_manifest(COMPATIBLE_SERVER_VERSION, "1", backup_options_json(payload))
        .await?;
    record_activity(
        &state.db,
        "Backup manifest created",
        Some("A backup manifest was created."),
        "System",
        Some(user.id),
    )
    .await?;
    Ok(Json(backup_manifest_json(&manifest)))
}

async fn backup_manifest(
    State(state): State<AppState>,
    headers: HeaderMap,
    Query(query): Query<BackupManifestQuery>,
) -> Result<Json<serde_json::Value>, ApiError> {
    require_admin(&state.db, &headers, query.auth.api_key.as_deref()).await?;
    let manifest = state
        .db
        .backup_manifest(&query.path)
        .await?
        .ok_or_else(|| ApiError::not_found("Backup manifest not found"))?;
    Ok(Json(backup_manifest_json(&manifest)))
}

async fn restore_backup(
    State(state): State<AppState>,
    headers: HeaderMap,
    Query(query): Query<AuthQuery>,
    Json(payload): Json<BackupRestoreBody>,
) -> Result<StatusCode, ApiError> {
    require_admin(&state.db, &headers, query.api_key.as_deref()).await?;
    let archive = payload.archive_file_name.trim();
    if archive.is_empty() {
        return Err(ApiError::bad_request("ArchiveFileName must not be empty"));
    }
    state
        .db
        .backup_manifest(archive)
        .await?
        .ok_or_else(|| ApiError::not_found("Backup manifest not found"))?;
    Err(ApiError::conflict(
        "Backup restore is not implemented in Jellyrin yet",
    ))
}

fn backup_options_json(payload: Option<BackupOptionsBody>) -> serde_json::Value {
    let payload = payload.unwrap_or(BackupOptionsBody {
        metadata: None,
        trickplay: None,
        subtitles: None,
        database: None,
    });
    serde_json::json!({
        "Metadata": payload.metadata.unwrap_or(false),
        "Trickplay": payload.trickplay.unwrap_or(false),
        "Subtitles": payload.subtitles.unwrap_or(false),
        "Database": payload.database.unwrap_or(true)
    })
}

fn backup_manifest_json(manifest: &BackupManifest) -> serde_json::Value {
    serde_json::json!({
        "ServerVersion": manifest.server_version,
        "BackupEngineVersion": manifest.backup_engine_version,
        "DateCreated": format_time_for_json(manifest.created_at),
        "Path": manifest.path,
        "Options": manifest.options
    })
}

#[derive(Debug, Deserialize)]
struct CreateApiKeyQuery {
    #[serde(flatten)]
    auth: AuthQuery,
    #[serde(alias = "App")]
    app: Option<String>,
}

async fn api_keys(
    State(state): State<AppState>,
    headers: HeaderMap,
    Query(query): Query<AuthQuery>,
) -> Result<Json<serde_json::Value>, ApiError> {
    require_admin(&state.db, &headers, query.api_key.as_deref()).await?;
    let keys = state.db.api_keys().await?;
    let items = keys
        .iter()
        .enumerate()
        .map(|(index, key)| api_key_to_json(index, key))
        .collect::<Vec<_>>();

    Ok(Json(serde_json::json!({
        "Items": items,
        "TotalRecordCount": keys.len(),
        "StartIndex": 0
    })))
}

async fn create_api_key(
    State(state): State<AppState>,
    headers: HeaderMap,
    Query(query): Query<CreateApiKeyQuery>,
) -> Result<StatusCode, ApiError> {
    let user = require_admin(&state.db, &headers, query.auth.api_key.as_deref()).await?;
    let app = query
        .app
        .as_deref()
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .ok_or_else(|| ApiError::bad_request("App must not be empty"))?;
    state.db.issue_api_key_for_user(user.id, app).await?;
    Ok(StatusCode::NO_CONTENT)
}

async fn revoke_api_key(
    State(state): State<AppState>,
    headers: HeaderMap,
    Query(query): Query<AuthQuery>,
    Path(key): Path<String>,
) -> Result<StatusCode, ApiError> {
    require_admin(&state.db, &headers, query.api_key.as_deref()).await?;
    state.db.revoke_api_key(&key).await?;
    Ok(StatusCode::NO_CONTENT)
}

fn api_key_to_json(index: usize, key: &ApiKey) -> serde_json::Value {
    serde_json::json!({
        "Id": index + 1,
        "AccessToken": key.access_token,
        "DeviceId": null,
        "AppName": key.name,
        "AppVersion": null,
        "DeviceName": null,
        "UserId": key.user_id,
        "UserName": key.user_name,
        "IsActive": true,
        "DateCreated": format_time_for_json(key.created_at),
        "DateRevoked": null,
        "DateLastActivity": format_time_for_json(key.last_activity_at)
    })
}

#[derive(Debug, Deserialize)]
struct UpdateUserQuery {
    #[serde(flatten)]
    auth: AuthQuery,
    #[serde(alias = "UserId", alias = "userId")]
    user_id: Option<String>,
}

async fn update_user(
    State(state): State<AppState>,
    headers: HeaderMap,
    Query(query): Query<UpdateUserQuery>,
    Json(payload): Json<serde_json::Value>,
) -> Result<StatusCode, ApiError> {
    require_admin(&state.db, &headers, query.auth.api_key.as_deref()).await?;
    let user_id = query
        .user_id
        .as_deref()
        .or_else(|| payload.get("Id").and_then(serde_json::Value::as_str))
        .ok_or_else(|| ApiError::bad_request("User id is required"))?;
    update_user_profile_from_payload(&state.db, resolve_user_id(user_id)?, payload).await?;
    Ok(StatusCode::NO_CONTENT)
}

#[derive(Debug, Deserialize)]
struct UpdateUserPasswordQuery {
    #[serde(flatten)]
    auth: AuthQuery,
    #[serde(alias = "UserId", alias = "userId")]
    user_id: Option<String>,
}

#[derive(Debug, Deserialize)]
struct UpdateUserPasswordBody {
    #[serde(alias = "CurrentPw")]
    current_pw: Option<String>,
    #[serde(alias = "NewPw")]
    new_pw: Option<String>,
    #[serde(alias = "ResetPassword")]
    reset_password: Option<bool>,
}

async fn update_user_password(
    State(state): State<AppState>,
    headers: HeaderMap,
    Query(query): Query<UpdateUserPasswordQuery>,
    Json(payload): Json<UpdateUserPasswordBody>,
) -> Result<StatusCode, ApiError> {
    let (auth_user, token) =
        require_user(&state.db, &headers, query.auth.api_key.as_deref()).await?;
    let requested_user_id = match query.user_id.as_deref() {
        Some(user_id) => resolve_user_id(user_id)?,
        None => auth_user.id,
    };
    update_user_password_inner(&state, &auth_user, &token, requested_user_id, payload).await
}

async fn update_user_password_legacy(
    State(state): State<AppState>,
    headers: HeaderMap,
    Query(query): Query<AuthQuery>,
    Path(user_id): Path<Uuid>,
    Json(payload): Json<UpdateUserPasswordBody>,
) -> Result<StatusCode, ApiError> {
    let (auth_user, token) = require_user(&state.db, &headers, query.api_key.as_deref()).await?;
    update_user_password_inner(&state, &auth_user, &token, user_id, payload).await
}

async fn update_user_password_inner(
    state: &AppState,
    auth_user: &User,
    token: &DeviceToken,
    requested_user_id: Uuid,
    payload: UpdateUserPasswordBody,
) -> Result<StatusCode, ApiError> {
    let target = state
        .db
        .users()
        .await?
        .into_iter()
        .find(|user| user.id == requested_user_id)
        .ok_or_else(|| ApiError::not_found("User not found"))?;
    ensure_user_access(auth_user, target.id)?;

    if payload.reset_password.unwrap_or(false) {
        state.db.reset_user_password(target.id).await?;
        return Ok(StatusCode::NO_CONTENT);
    }

    if !auth_user.is_administrator || auth_user.id == target.id {
        state
            .db
            .verify_user_password(target.id, payload.current_pw.as_deref().unwrap_or_default())
            .await
            .map_err(|_| ApiError::forbidden("Invalid user or password entered"))?;
    }

    state
        .db
        .set_user_password(target.id, payload.new_pw.as_deref().unwrap_or_default())
        .await?;
    state
        .db
        .revoke_user_tokens_except(target.id, &token.access_token)
        .await?;
    Ok(StatusCode::NO_CONTENT)
}

async fn update_user_policy(
    State(state): State<AppState>,
    headers: HeaderMap,
    Query(query): Query<AuthQuery>,
    Path(user_id): Path<Uuid>,
    Json(payload): Json<serde_json::Value>,
) -> Result<StatusCode, ApiError> {
    require_admin(&state.db, &headers, query.api_key.as_deref()).await?;
    update_user_profile_from_payload(&state.db, user_id, serde_json::json!({ "Policy": payload }))
        .await?;
    Ok(StatusCode::NO_CONTENT)
}

async fn update_user_profile_from_payload(
    db: &Database,
    user_id: Uuid,
    payload: serde_json::Value,
) -> Result<User, ApiError> {
    let current = db
        .users()
        .await?
        .into_iter()
        .find(|user| user.id == user_id)
        .ok_or_else(|| ApiError::not_found("User not found"))?;
    let policy = payload.get("Policy");
    let name = payload
        .get("Name")
        .and_then(serde_json::Value::as_str)
        .unwrap_or(&current.name);
    let is_administrator =
        bool_value(policy, "IsAdministrator").unwrap_or(current.is_administrator);
    let is_disabled = bool_value(policy, "IsDisabled").unwrap_or(current.is_disabled);
    db.update_user_profile(user_id, name, is_administrator, is_disabled)
        .await
        .map_err(ApiError::from)
}

fn bool_value(payload: Option<&serde_json::Value>, key: &str) -> Option<bool> {
    payload
        .and_then(|payload| payload.get(key))
        .and_then(serde_json::Value::as_bool)
}

async fn authenticate_by_name(
    State(state): State<AppState>,
    headers: HeaderMap,
    Json(payload): Json<AuthenticateUserByNameDto>,
) -> Result<Json<AuthenticationResultDto>, ApiError> {
    let auth = client_auth_from_headers(&headers);
    let username = payload
        .username
        .as_deref()
        .filter(|value| !value.is_empty())
        .ok_or_else(|| ApiError::bad_request("Username must not be empty"))?;
    let password = payload.pw.as_deref().unwrap_or("");
    let (user, token) = state
        .db
        .authenticate_user_by_name(
            username,
            password,
            &auth.device_id,
            &auth.device,
            &auth.client,
            &auth.version,
        )
        .await
        .map_err(|_| ApiError::unauthorized("Invalid username or password"))?;
    let server = state.db.server_state().await?;
    record_activity(
        &state.db,
        &format!("{} signed in", user.name),
        Some(&format!("{} signed in from {}", user.name, auth.client)),
        "Authentication",
        Some(user.id),
    )
    .await?;

    Ok(Json(
        authentication_result_to_dto(&state.db, &user, &token, server.server_id).await?,
    ))
}

async fn get_current_user(
    State(state): State<AppState>,
    headers: HeaderMap,
    Query(query): Query<AuthQuery>,
) -> Result<Json<UserDto>, ApiError> {
    let (user, _) = require_user(&state.db, &headers, query.api_key.as_deref()).await?;
    let server = state.db.server_state().await?;
    Ok(Json(user_to_dto(&state.db, &user, server.server_id).await?))
}

async fn get_user_by_id(
    State(state): State<AppState>,
    headers: HeaderMap,
    Query(query): Query<AuthQuery>,
    Path(user_id): Path<Uuid>,
) -> Result<Json<UserDto>, ApiError> {
    let (user, _) = require_user(&state.db, &headers, query.api_key.as_deref()).await?;
    if user.id != user_id && !user.is_administrator {
        return Err(ApiError::forbidden("User access denied"));
    }
    let server = state.db.server_state().await?;
    let requested_user = if user.id == user_id {
        user
    } else {
        state
            .db
            .users()
            .await?
            .into_iter()
            .find(|candidate| candidate.id == user_id)
            .ok_or_else(|| ApiError::not_found("User not found"))?
    };
    Ok(Json(
        user_to_dto(&state.db, &requested_user, server.server_id).await?,
    ))
}

#[derive(Debug, Deserialize)]
struct UserConfigurationQuery {
    #[serde(flatten)]
    auth: AuthQuery,
    #[serde(alias = "UserId", alias = "userId")]
    user_id: Option<String>,
}

async fn update_user_configuration(
    State(state): State<AppState>,
    headers: HeaderMap,
    Query(query): Query<UserConfigurationQuery>,
    Json(payload): Json<serde_json::Value>,
) -> Result<StatusCode, ApiError> {
    update_user_configuration_for_id(
        state,
        headers,
        query.auth,
        query.user_id.as_deref(),
        payload,
    )
    .await
}

async fn update_user_configuration_for_path(
    State(state): State<AppState>,
    headers: HeaderMap,
    Query(auth): Query<AuthQuery>,
    Path(user_id): Path<String>,
    Json(payload): Json<serde_json::Value>,
) -> Result<StatusCode, ApiError> {
    update_user_configuration_for_id(state, headers, auth, Some(&user_id), payload).await
}

async fn update_user_configuration_for_id(
    state: AppState,
    headers: HeaderMap,
    auth: AuthQuery,
    user_id: Option<&str>,
    payload: serde_json::Value,
) -> Result<StatusCode, ApiError> {
    let auth_user = require_request_user(&state.db, &headers, auth.api_key.as_deref()).await?;
    let user_id = match user_id {
        Some(user_id) => resolve_user_id(user_id)?,
        None => auth_user.id,
    };
    ensure_user_access(&auth_user, user_id)?;
    let current = state
        .db
        .user_configuration(user_id)
        .await?
        .unwrap_or_else(default_user_configuration);
    let merged = merge_user_configuration(current, payload)?;
    state.db.update_user_configuration(user_id, merged).await?;
    Ok(StatusCode::NO_CONTENT)
}

fn merge_user_configuration(
    current: serde_json::Value,
    update: serde_json::Value,
) -> Result<serde_json::Value, ApiError> {
    let serde_json::Value::Object(mut current) = current else {
        return Err(ApiError::bad_request(
            "Stored user configuration is invalid",
        ));
    };
    let serde_json::Value::Object(update) = update else {
        return Err(ApiError::bad_request(
            "User configuration body must be an object",
        ));
    };
    for (key, value) in update {
        current.insert(key, value);
    }
    Ok(serde_json::Value::Object(current))
}

fn default_user_configuration() -> serde_json::Value {
    serde_json::json!({
        "AudioLanguagePreference": null,
        "PlayDefaultAudioTrack": true,
        "SubtitleLanguagePreference": null,
        "DisplayMissingEpisodes": false,
        "GroupedFolders": [],
        "SubtitleMode": "Default",
        "DisplayCollectionsView": false,
        "EnableLocalPassword": false,
        "OrderedViews": [],
        "LatestItemsExcludes": [],
        "MyMediaExcludes": [],
        "HidePlayedInLatest": true,
        "RememberAudioSelections": true,
        "RememberSubtitleSelections": true,
        "EnableNextEpisodeAutoPlay": true,
        "CastReceiverId": null
    })
}

async fn logout(
    State(state): State<AppState>,
    headers: HeaderMap,
    Query(query): Query<AuthQuery>,
) -> Result<StatusCode, ApiError> {
    let token = bearer_token(&headers)
        .or_else(|| query.api_key.clone())
        .ok_or_else(|| ApiError::unauthorized("Missing token"))?;
    let (user, _) = require_user(&state.db, &headers, query.api_key.as_deref()).await?;
    record_activity(
        &state.db,
        &format!("{} signed out", user.name),
        Some(&format!("{} signed out", user.name)),
        "Authentication",
        Some(user.id),
    )
    .await?;
    state.db.revoke_token(&token).await?;
    Ok(StatusCode::NO_CONTENT)
}

async fn update_session_capabilities(
    State(state): State<AppState>,
    headers: HeaderMap,
    Query(query): Query<SessionCapabilitiesQuery>,
    body: Option<Json<serde_json::Value>>,
) -> Result<StatusCode, ApiError> {
    let (_, token) = require_user(&state.db, &headers, query.auth.api_key.as_deref()).await?;
    let capabilities = normalize_session_capabilities(body.map(|Json(value)| value), &query)?;
    state
        .db
        .update_device_capabilities(&token.access_token, capabilities)
        .await
        .map_err(|_| ApiError::not_found("Device session not found"))?;
    Ok(StatusCode::NO_CONTENT)
}

#[derive(Debug, Deserialize)]
struct SessionCapabilitiesQuery {
    #[serde(flatten)]
    auth: AuthQuery,
    #[serde(alias = "PlayableMediaTypes", alias = "playableMediaTypes")]
    playable_media_types: Option<String>,
    #[serde(alias = "SupportedCommands", alias = "supportedCommands")]
    supported_commands: Option<String>,
    #[serde(alias = "SupportsRemoteControl", alias = "supportsRemoteControl")]
    supports_remote_control: Option<bool>,
    #[serde(alias = "SupportsMediaControl", alias = "supportsMediaControl")]
    supports_media_control: Option<bool>,
    #[serde(
        alias = "SupportsPersistentIdentifier",
        alias = "supportsPersistentIdentifier"
    )]
    supports_persistent_identifier: Option<bool>,
    #[serde(alias = "SupportsSync", alias = "supportsSync")]
    supports_sync: Option<bool>,
}

fn normalize_session_capabilities(
    payload: Option<serde_json::Value>,
    query: &SessionCapabilitiesQuery,
) -> Result<serde_json::Value, ApiError> {
    let mut object = match payload {
        Some(serde_json::Value::Object(object)) => object,
        Some(_) => {
            return Err(ApiError::bad_request(
                "Session capabilities body must be an object",
            ));
        }
        None => serde_json::Map::new(),
    };
    let serde_json::Value::Object(defaults) = default_session_capabilities() else {
        unreachable!("default session capabilities must be an object");
    };
    for (key, value) in defaults {
        object.entry(key).or_insert(value);
    }
    if let Some(values) = query.playable_media_types.as_deref() {
        object.insert(
            "PlayableMediaTypes".to_string(),
            serde_json::Value::Array(parse_capability_list(values)),
        );
    }
    if let Some(values) = query.supported_commands.as_deref() {
        object.insert(
            "SupportedCommands".to_string(),
            serde_json::Value::Array(parse_capability_list(values)),
        );
    }
    if let Some(value) = query.supports_remote_control {
        object.insert(
            "SupportsRemoteControl".to_string(),
            serde_json::json!(value),
        );
    }
    if let Some(value) = query.supports_media_control {
        object.insert("SupportsMediaControl".to_string(), serde_json::json!(value));
    }
    if let Some(value) = query.supports_persistent_identifier {
        object.insert(
            "SupportsPersistentIdentifier".to_string(),
            serde_json::json!(value),
        );
    }
    if let Some(value) = query.supports_sync {
        object.insert("SupportsSync".to_string(), serde_json::json!(value));
    }
    Ok(serde_json::Value::Object(object))
}

fn parse_capability_list(values: &str) -> Vec<serde_json::Value> {
    values
        .split(',')
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .map(|value| serde_json::Value::String(value.to_string()))
        .collect()
}

fn default_session_capabilities() -> serde_json::Value {
    serde_json::json!({
        "PlayableMediaTypes": [],
        "SupportedCommands": [],
        "SupportsRemoteControl": false,
        "SupportsMediaControl": false
    })
}

async fn ping() -> &'static str {
    "Jellyfin Server"
}

async fn system_admin_noop(
    State(state): State<AppState>,
    headers: HeaderMap,
    Query(query): Query<AuthQuery>,
) -> Result<StatusCode, ApiError> {
    require_admin(&state.db, &headers, query.api_key.as_deref()).await?;
    Ok(StatusCode::NO_CONTENT)
}

async fn system_storage(
    State(state): State<AppState>,
    headers: HeaderMap,
    Query(query): Query<AuthQuery>,
) -> Result<Json<serde_json::Value>, ApiError> {
    require_admin(&state.db, &headers, query.api_key.as_deref()).await?;
    let current_dir = std::env::current_dir().unwrap_or_else(|_| PathBuf::from("."));
    let temp_dir = std::env::temp_dir();
    let libraries = state
        .db
        .virtual_folders()
        .await?
        .iter()
        .map(|folder| {
            serde_json::json!({
                "Id": folder.id,
                "Name": folder.name,
                "Folders": folder
                    .locations
                    .iter()
                    .map(|path| folder_storage_json(PathBuf::from(path)))
                    .collect::<Vec<serde_json::Value>>()
            })
        })
        .collect::<Vec<serde_json::Value>>();

    Ok(Json(serde_json::json!({
        "ProgramDataFolder": folder_storage_json(current_dir.clone()),
        "WebFolder": folder_storage_json(state.web_dir),
        "ImageCacheFolder": folder_storage_json(temp_dir.join("jellyrin").join("images")),
        "CacheFolder": folder_storage_json(temp_dir.join("jellyrin").join("cache")),
        "LogFolder": folder_storage_json(state.log_dir),
        "InternalMetadataFolder": folder_storage_json(current_dir.join("metadata")),
        "TranscodingTempFolder": folder_storage_json(temp_dir.join("jellyrin").join("transcodes")),
        "Libraries": libraries
    })))
}

fn folder_storage_json(path: PathBuf) -> serde_json::Value {
    serde_json::json!({
        "Path": path.to_string_lossy(),
        "FreeSpace": null,
        "UsedSpace": null,
        "StorageType": null,
        "DeviceId": null
    })
}

async fn system_logs(
    State(state): State<AppState>,
    headers: HeaderMap,
    Query(auth_query): Query<AuthQuery>,
) -> Result<Json<Vec<serde_json::Value>>, ApiError> {
    require_admin(&state.db, &headers, auth_query.api_key.as_deref()).await?;

    let mut read_dir = match tokio::fs::read_dir(&state.log_dir).await {
        Ok(read_dir) => read_dir,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(Json(Vec::new())),
        Err(error) => return Err(ApiError::from(error)),
    };

    let mut logs = Vec::new();
    while let Some(entry) = read_dir.next_entry().await.map_err(ApiError::from)? {
        let metadata = tokio::fs::symlink_metadata(entry.path())
            .await
            .map_err(ApiError::from)?;
        if metadata.file_type().is_symlink() || !metadata.is_file() {
            continue;
        }

        let name = entry.file_name().to_string_lossy().into_owned();
        if !is_server_log_file_name(&name) {
            continue;
        }

        let modified = metadata
            .modified()
            .ok()
            .map(OffsetDateTime::from)
            .unwrap_or_else(OffsetDateTime::now_utc);
        let created = metadata
            .created()
            .ok()
            .map(OffsetDateTime::from)
            .unwrap_or(modified);
        logs.push(serde_json::json!({
            "Name": name,
            "Size": metadata.len(),
            "DateCreated": format_time_for_json(created),
            "DateModified": format_time_for_json(modified)
        }));
    }

    logs.sort_by(|left, right| {
        right["DateModified"]
            .as_str()
            .cmp(&left["DateModified"].as_str())
            .then_with(|| left["Name"].as_str().cmp(&right["Name"].as_str()))
    });

    Ok(Json(logs))
}

fn is_server_log_file_name(name: &str) -> bool {
    FsPath::new(name)
        .extension()
        .and_then(|extension| extension.to_str())
        .map(|extension| matches!(extension.to_ascii_lowercase().as_str(), "log" | "txt"))
        .unwrap_or(false)
}

#[derive(Debug, Deserialize)]
struct SystemLogFileQuery {
    #[serde(alias = "Name")]
    name: String,
}

async fn system_log_file(
    State(state): State<AppState>,
    headers: HeaderMap,
    Query(auth_query): Query<AuthQuery>,
    Query(query): Query<SystemLogFileQuery>,
) -> Result<impl IntoResponse, ApiError> {
    require_admin(&state.db, &headers, auth_query.api_key.as_deref()).await?;
    let Some(path) = safe_log_file_path(&state.log_dir, &query.name) else {
        return Err(ApiError::bad_request("invalid log file name"));
    };
    let metadata = tokio::fs::symlink_metadata(&path).await.map_err(|error| {
        if error.kind() == std::io::ErrorKind::NotFound {
            ApiError::not_found("Log file not found")
        } else {
            ApiError::from(error)
        }
    })?;
    if metadata.file_type().is_symlink() || !metadata.is_file() {
        return Err(ApiError::not_found("Log file not found"));
    }

    let contents = tokio::fs::read_to_string(path).await.map_err(|error| {
        if error.kind() == std::io::ErrorKind::NotFound {
            ApiError::not_found("Log file not found")
        } else {
            ApiError::from(error)
        }
    })?;

    Ok((
        [(header::CONTENT_TYPE, "text/plain; charset=utf-8")],
        contents,
    ))
}

fn safe_log_file_path(log_dir: &FsPath, name: &str) -> Option<PathBuf> {
    let name = name.trim();
    if name.is_empty() || name.contains('/') || name.contains('\\') {
        return None;
    }

    let path = FsPath::new(name);
    if path.file_name().and_then(|file_name| file_name.to_str()) != Some(name) {
        return None;
    }

    Some(log_dir.join(name))
}

#[derive(Debug, Deserialize)]
struct ActivityLogQuery {
    #[serde(alias = "startIndex")]
    #[serde(alias = "StartIndex")]
    start_index: Option<i64>,
    #[serde(alias = "limit")]
    #[serde(alias = "Limit")]
    limit: Option<i64>,
    #[serde(alias = "hasUserId")]
    #[serde(alias = "HasUserId")]
    has_user_id: Option<bool>,
    #[serde(alias = "name")]
    #[serde(alias = "Name")]
    name: Option<String>,
    #[serde(alias = "overview")]
    #[serde(alias = "Overview")]
    overview: Option<String>,
    #[serde(alias = "shortOverview")]
    #[serde(alias = "ShortOverview")]
    short_overview: Option<String>,
    #[serde(alias = "type")]
    #[serde(alias = "Type")]
    entry_type: Option<String>,
    #[serde(alias = "itemId")]
    #[serde(alias = "ItemId")]
    item_id: Option<String>,
    #[serde(alias = "username")]
    #[serde(alias = "Username")]
    username: Option<String>,
    #[serde(alias = "severity")]
    #[serde(alias = "Severity")]
    severity: Option<String>,
    #[serde(default, deserialize_with = "deserialize_query_string_list")]
    #[serde(alias = "sortBy")]
    #[serde(alias = "SortBy")]
    sort_by: Vec<String>,
    #[serde(default, deserialize_with = "deserialize_query_string_list")]
    #[serde(alias = "sortOrder")]
    #[serde(alias = "SortOrder")]
    sort_order: Vec<String>,
    #[serde(alias = "minDate")]
    #[serde(alias = "MinDate")]
    min_date: Option<String>,
    #[serde(alias = "maxDate")]
    #[serde(alias = "MaxDate")]
    max_date: Option<String>,
}

fn deserialize_query_string_list<'de, D>(deserializer: D) -> Result<Vec<String>, D::Error>
where
    D: Deserializer<'de>,
{
    struct StringListVisitor;

    impl<'de> de::Visitor<'de> for StringListVisitor {
        type Value = Vec<String>;

        fn expecting(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
            formatter.write_str("a string or a list of strings")
        }

        fn visit_str<E>(self, value: &str) -> Result<Self::Value, E>
        where
            E: de::Error,
        {
            Ok(vec![value.to_string()])
        }

        fn visit_string<E>(self, value: String) -> Result<Self::Value, E>
        where
            E: de::Error,
        {
            Ok(vec![value])
        }

        fn visit_seq<A>(self, mut seq: A) -> Result<Self::Value, A::Error>
        where
            A: de::SeqAccess<'de>,
        {
            let mut values = Vec::new();
            while let Some(value) = seq.next_element::<String>()? {
                values.push(value);
            }
            Ok(values)
        }
    }

    deserializer.deserialize_any(StringListVisitor)
}

async fn activity_log_entries(
    State(state): State<AppState>,
    headers: HeaderMap,
    Query(auth_query): Query<AuthQuery>,
    Query(query): Query<ActivityLogQuery>,
) -> Result<Json<serde_json::Value>, ApiError> {
    require_admin(&state.db, &headers, auth_query.api_key.as_deref()).await?;
    let start_index = query.start_index.unwrap_or_default().max(0);
    let limit = query.limit.unwrap_or(100).clamp(0, 1000);
    let filter = activity_log_filter_from_query(&query)?;
    let (entries, total) = state
        .db
        .activity_log_entries(start_index, limit, filter)
        .await?;
    Ok(Json(serde_json::json!({
        "Items": entries.iter().map(activity_log_entry_json).collect::<Vec<_>>(),
        "TotalRecordCount": total,
        "StartIndex": start_index
    })))
}

fn activity_log_filter_from_query(query: &ActivityLogQuery) -> Result<ActivityLogFilter, ApiError> {
    if query
        .item_id
        .as_deref()
        .is_some_and(|value| !value.trim().is_empty())
    {
        return Err(ApiError::bad_request(
            "activity log item filtering is not implemented",
        ));
    }

    Ok(ActivityLogFilter {
        has_user_id: query.has_user_id,
        name: query.name.clone(),
        overview: query.overview.clone(),
        short_overview: query.short_overview.clone(),
        entry_type: query.entry_type.clone(),
        username: query.username.clone(),
        severity: query
            .severity
            .as_deref()
            .map(normalize_activity_log_severity)
            .transpose()?,
        min_date: query
            .min_date
            .as_deref()
            .map(parse_activity_log_date)
            .transpose()?,
        max_date: query
            .max_date
            .as_deref()
            .map(parse_activity_log_date)
            .transpose()?,
        sort: parse_activity_log_sort(&query.sort_by, &query.sort_order),
    })
}

fn normalize_activity_log_severity(value: &str) -> Result<String, ApiError> {
    let value = value.trim();
    let severity = match value.to_ascii_lowercase().as_str() {
        "trace" => "Trace",
        "debug" => "Debug",
        "information" => "Information",
        "warning" => "Warning",
        "error" => "Error",
        "critical" => "Critical",
        "none" => "None",
        _ => return Err(ApiError::bad_request("invalid activity log severity")),
    };
    Ok(severity.to_string())
}

fn parse_activity_log_date(value: &str) -> Result<OffsetDateTime, ApiError> {
    OffsetDateTime::parse(value.trim(), &Rfc3339)
        .map_err(|_| ApiError::bad_request("invalid activity log date"))
}

fn parse_activity_log_sort(
    sort_by: &[String],
    sort_order: &[String],
) -> Vec<(ActivityLogSortField, SortDirection)> {
    let fields = expand_query_values(sort_by);
    let orders = expand_query_values(sort_order);
    fields
        .iter()
        .enumerate()
        .filter_map(|(index, field)| {
            let field = parse_activity_log_sort_field(field)?;
            let direction = orders
                .get(index)
                .or_else(|| orders.first())
                .and_then(|order| parse_sort_direction(order))
                .unwrap_or(SortDirection::Ascending);
            Some((field, direction))
        })
        .collect()
}

fn expand_query_values(values: &[String]) -> Vec<String> {
    values
        .iter()
        .flat_map(|value| value.split(','))
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .map(ToOwned::to_owned)
        .collect()
}

fn parse_activity_log_sort_field(value: &str) -> Option<ActivityLogSortField> {
    match value.trim().to_ascii_lowercase().as_str() {
        "name" => Some(ActivityLogSortField::Name),
        "overview" | "overiew" => Some(ActivityLogSortField::Overview),
        "shortoverview" | "short_overview" => Some(ActivityLogSortField::ShortOverview),
        "type" => Some(ActivityLogSortField::Type),
        "datecreated" | "date" | "createdat" => Some(ActivityLogSortField::DateCreated),
        "username" | "user" => Some(ActivityLogSortField::Username),
        "logseverity" | "severity" => Some(ActivityLogSortField::LogSeverity),
        _ => None,
    }
}

fn parse_sort_direction(value: &str) -> Option<SortDirection> {
    match value.trim().to_ascii_lowercase().as_str() {
        "ascending" | "asc" => Some(SortDirection::Ascending),
        "descending" | "desc" => Some(SortDirection::Descending),
        _ => None,
    }
}

fn activity_log_entry_json(entry: &ActivityLogEntry) -> serde_json::Value {
    serde_json::json!({
        "Id": entry.id,
        "Name": entry.name,
        "Overview": entry.overview,
        "ShortOverview": entry.short_overview,
        "Type": entry.entry_type,
        "Severity": entry.severity,
        "Date": format_time_for_json(entry.created_at),
        "UserId": entry.user_id.map(|id| id.to_string()),
        "ItemId": null,
        "UserPrimaryImageTag": null
    })
}

async fn record_activity(
    db: &Database,
    name: &str,
    overview: Option<&str>,
    entry_type: &str,
    user_id: Option<Uuid>,
) -> Result<(), ApiError> {
    db.add_activity_log_entry(name, overview, overview, entry_type, user_id)
        .await?;
    Ok(())
}

async fn session_sessions(
    State(state): State<AppState>,
    headers: HeaderMap,
    Query(query): Query<AuthQuery>,
) -> Result<Json<Vec<serde_json::Value>>, ApiError> {
    let user = require_request_user(&state.db, &headers, query.api_key.as_deref()).await?;
    Ok(Json(session_list_json(&state.db, &user).await?))
}

async fn session_list_json(db: &Database, user: &User) -> Result<Vec<serde_json::Value>, ApiError> {
    let sessions = if user.is_administrator {
        db.device_sessions().await?
    } else {
        db.device_sessions_for_user(user.id).await?
    };
    let active_playback = db
        .active_playback_sessions()
        .await?
        .into_iter()
        .map(|session| (session.session_id.clone(), session))
        .collect::<HashMap<_, _>>();
    let server_id = db.server_state().await?.server_id.to_string();
    Ok(sessions
        .iter()
        .map(|session| {
            session_to_json(
                session,
                active_playback.get(&session.access_token),
                &server_id,
            )
        })
        .collect::<Vec<serde_json::Value>>())
}

async fn devices(
    State(state): State<AppState>,
    headers: HeaderMap,
    Query(query): Query<AuthQuery>,
) -> Result<Json<serde_json::Value>, ApiError> {
    require_admin(&state.db, &headers, query.api_key.as_deref()).await?;
    let items = state
        .db
        .device_sessions()
        .await?
        .iter()
        .map(device_to_json)
        .collect::<Vec<serde_json::Value>>();
    Ok(Json(query_result(items)))
}

async fn installed_plugins(
    State(state): State<AppState>,
    headers: HeaderMap,
    Query(query): Query<AuthQuery>,
) -> Result<Json<Vec<serde_json::Value>>, ApiError> {
    require_admin(&state.db, &headers, query.api_key.as_deref()).await?;
    Ok(Json(Vec::new()))
}

async fn available_packages(
    State(state): State<AppState>,
    headers: HeaderMap,
    Query(query): Query<AuthQuery>,
) -> Result<Json<Vec<serde_json::Value>>, ApiError> {
    require_admin(&state.db, &headers, query.api_key.as_deref()).await?;
    Ok(Json(Vec::new()))
}

#[derive(Debug, Deserialize)]
struct PackageQuery {
    #[serde(flatten)]
    auth: AuthQuery,
    #[serde(alias = "AssemblyGuid")]
    _assembly_guid: Option<String>,
    #[serde(alias = "Version")]
    _version: Option<String>,
    #[serde(alias = "RepositoryUrl")]
    _repository_url: Option<String>,
}

async fn package_info(
    State(state): State<AppState>,
    headers: HeaderMap,
    Query(query): Query<PackageQuery>,
    Path(_name): Path<String>,
) -> Result<Json<serde_json::Value>, ApiError> {
    require_admin(&state.db, &headers, query.auth.api_key.as_deref()).await?;
    Err(ApiError::not_found("Package not found"))
}

async fn install_package(
    State(state): State<AppState>,
    headers: HeaderMap,
    Query(query): Query<PackageQuery>,
    Path(_name): Path<String>,
) -> Result<StatusCode, ApiError> {
    require_admin(&state.db, &headers, query.auth.api_key.as_deref()).await?;
    Err(ApiError::not_found("Package not found"))
}

async fn cancel_package_installation(
    State(state): State<AppState>,
    headers: HeaderMap,
    Query(query): Query<AuthQuery>,
    Path(_package_id): Path<String>,
) -> Result<StatusCode, ApiError> {
    require_admin(&state.db, &headers, query.api_key.as_deref()).await?;
    Ok(StatusCode::NO_CONTENT)
}

async fn package_repositories(
    State(state): State<AppState>,
    headers: HeaderMap,
    Query(query): Query<AuthQuery>,
) -> Result<Json<Vec<serde_json::Value>>, ApiError> {
    require_admin(&state.db, &headers, query.api_key.as_deref()).await?;
    Ok(Json(repository_infos_from_config(
        state
            .db
            .system_configuration_payloads()
            .await?
            .plugin_repositories,
    )))
}

async fn update_package_repositories(
    State(state): State<AppState>,
    headers: HeaderMap,
    Query(query): Query<AuthQuery>,
    Json(payload): Json<serde_json::Value>,
) -> Result<StatusCode, ApiError> {
    let user = require_admin(&state.db, &headers, query.api_key.as_deref()).await?;
    let mut current_payloads = state.db.system_configuration_payloads().await?;
    current_payloads.plugin_repositories =
        serde_json::Value::Array(repository_infos_from_config(payload));
    state
        .db
        .update_system_configuration_payloads(current_payloads)
        .await?;
    record_activity(
        &state.db,
        "Plugin repositories updated",
        Some("Plugin repository list was updated."),
        "System",
        Some(user.id),
    )
    .await?;
    Ok(StatusCode::NO_CONTENT)
}

fn repository_infos_from_config(value: serde_json::Value) -> Vec<serde_json::Value> {
    let serde_json::Value::Array(repositories) = value else {
        return Vec::new();
    };

    repositories
        .into_iter()
        .filter_map(normalize_repository_info)
        .collect()
}

fn normalize_repository_info(value: serde_json::Value) -> Option<serde_json::Value> {
    let serde_json::Value::Object(repository) = value else {
        return None;
    };
    let name = repository
        .get("Name")
        .or_else(|| repository.get("name"))
        .and_then(serde_json::Value::as_str)
        .map(str::trim)
        .filter(|name| !name.is_empty())?;
    let url = repository
        .get("Url")
        .or_else(|| repository.get("URL"))
        .or_else(|| repository.get("url"))
        .and_then(serde_json::Value::as_str)
        .map(str::trim)
        .filter(|url| !url.is_empty())?;
    let enabled = repository
        .get("Enabled")
        .or_else(|| repository.get("enabled"))
        .and_then(serde_json::Value::as_bool)
        .unwrap_or(true);

    Some(serde_json::json!({
        "Name": name,
        "Url": url,
        "Enabled": enabled
    }))
}

async fn plugin_configuration(
    State(state): State<AppState>,
    headers: HeaderMap,
    Query(query): Query<AuthQuery>,
    Path(_plugin_id): Path<String>,
) -> Result<Json<serde_json::Value>, ApiError> {
    require_admin(&state.db, &headers, query.api_key.as_deref()).await?;
    Err(ApiError::not_found("Plugin not found"))
}

async fn update_plugin_configuration(
    State(state): State<AppState>,
    headers: HeaderMap,
    Query(query): Query<AuthQuery>,
    Path(_plugin_id): Path<String>,
    Json(_payload): Json<serde_json::Value>,
) -> Result<StatusCode, ApiError> {
    require_admin(&state.db, &headers, query.api_key.as_deref()).await?;
    Err(ApiError::not_found("Plugin not found"))
}

async fn enable_plugin(
    State(state): State<AppState>,
    headers: HeaderMap,
    Query(query): Query<AuthQuery>,
    Path((_plugin_id, _version)): Path<(String, String)>,
) -> Result<StatusCode, ApiError> {
    require_admin(&state.db, &headers, query.api_key.as_deref()).await?;
    Err(ApiError::not_found("Plugin not found"))
}

async fn disable_plugin(
    State(state): State<AppState>,
    headers: HeaderMap,
    Query(query): Query<AuthQuery>,
    Path((_plugin_id, _version)): Path<(String, String)>,
) -> Result<StatusCode, ApiError> {
    require_admin(&state.db, &headers, query.api_key.as_deref()).await?;
    Err(ApiError::not_found("Plugin not found"))
}

async fn uninstall_plugin_by_version(
    State(state): State<AppState>,
    headers: HeaderMap,
    Query(query): Query<AuthQuery>,
    Path((_plugin_id, _version)): Path<(String, String)>,
) -> Result<StatusCode, ApiError> {
    require_admin(&state.db, &headers, query.api_key.as_deref()).await?;
    Err(ApiError::not_found("Plugin not found"))
}

async fn uninstall_plugin(
    State(state): State<AppState>,
    headers: HeaderMap,
    Query(query): Query<AuthQuery>,
    Path(_plugin_id): Path<String>,
) -> Result<StatusCode, ApiError> {
    require_admin(&state.db, &headers, query.api_key.as_deref()).await?;
    Err(ApiError::not_found("Plugin not found"))
}

async fn plugin_manifest(
    State(state): State<AppState>,
    headers: HeaderMap,
    Query(query): Query<AuthQuery>,
    Path(plugin_id): Path<String>,
) -> Result<Json<serde_json::Value>, ApiError> {
    require_admin(&state.db, &headers, query.api_key.as_deref()).await?;
    Ok(Json(serde_json::json!({
        "Guid": plugin_id,
        "Name": plugin_id,
        "Overview": "Plugin manifests are not supported by Jellyrin yet.",
        "Description": "Plugin compatibility is intentionally disabled in this milestone.",
        "Owner": "Jellyrin",
        "Category": "General",
        "Versions": []
    })))
}

async fn device_info(
    State(state): State<AppState>,
    headers: HeaderMap,
    Query(auth_query): Query<AuthQuery>,
    Query(query): Query<DeviceOptionsQuery>,
) -> Result<Json<serde_json::Value>, ApiError> {
    require_admin(&state.db, &headers, auth_query.api_key.as_deref()).await?;
    let device = match trimmed_optional(query.id) {
        Some(id) => state.db.device_session_by_id(&id).await?,
        None => None,
    };
    Ok(Json(
        device
            .as_ref()
            .map(device_to_json)
            .unwrap_or_else(empty_device_info_json),
    ))
}

fn empty_device_info_json() -> serde_json::Value {
    serde_json::json!({
        "Name": null,
        "Id": null,
        "LastUserName": null,
        "AppName": null,
        "AppVersion": null,
        "LastUserId": null,
        "DateLastActivity": null,
        "Capabilities": null
    })
}

fn device_to_json(device: &DeviceSession) -> serde_json::Value {
    serde_json::json!({
        "Name": device.device_name,
        "Id": device.device_id,
        "LastUserName": device.user_name,
        "AppName": device.client,
        "AppVersion": device.version,
        "LastUserId": device.user_id,
        "DateLastActivity": format_time_for_json(device.last_activity_at),
        "Capabilities": device.capabilities.clone()
    })
}

#[derive(Debug, Deserialize)]
struct DeviceOptionsQuery {
    #[serde(alias = "Id", alias = "id")]
    id: Option<String>,
}

#[derive(Debug, Deserialize)]
struct DeviceOptionsUpdate {
    #[serde(alias = "CustomName")]
    custom_name: Option<String>,
}

async fn device_options(
    State(state): State<AppState>,
    headers: HeaderMap,
    Query(auth_query): Query<AuthQuery>,
    Query(query): Query<DeviceOptionsQuery>,
) -> Result<Json<serde_json::Value>, ApiError> {
    require_admin(&state.db, &headers, auth_query.api_key.as_deref()).await?;
    let custom_name = if let Some(id) = trimmed_optional(query.id) {
        state
            .db
            .device_session_by_id(&id)
            .await?
            .map(|session| session.device_name)
    } else {
        None
    };
    Ok(Json(serde_json::json!({
        "CustomName": custom_name
    })))
}

async fn update_device_options(
    State(state): State<AppState>,
    headers: HeaderMap,
    Query(auth_query): Query<AuthQuery>,
    Query(query): Query<DeviceOptionsQuery>,
    Json(payload): Json<DeviceOptionsUpdate>,
) -> Result<StatusCode, ApiError> {
    let user = require_admin(&state.db, &headers, auth_query.api_key.as_deref()).await?;
    let Some(id) = trimmed_optional(query.id) else {
        return Err(ApiError::bad_request("Device id is required"));
    };
    let Some(custom_name) = trimmed_optional(payload.custom_name) else {
        return Err(ApiError::bad_request("CustomName is required"));
    };
    if state.db.device_session_by_id(&id).await?.is_none() {
        return Err(ApiError::not_found("Device not found"));
    }
    state.db.update_device_name(&id, &custom_name).await?;
    record_activity(
        &state.db,
        "Device options updated",
        Some("A device custom name was updated."),
        "Device",
        Some(user.id),
    )
    .await?;
    Ok(StatusCode::NO_CONTENT)
}

fn trimmed_optional(value: Option<String>) -> Option<String> {
    value
        .map(|value| value.trim().to_string())
        .filter(|value| !value.is_empty())
}

#[derive(Debug, Deserialize)]
struct DeleteDeviceQuery {
    #[serde(alias = "Id", alias = "id")]
    id: Option<String>,
}

async fn delete_device(
    State(state): State<AppState>,
    headers: HeaderMap,
    Query(auth_query): Query<AuthQuery>,
    Query(query): Query<DeleteDeviceQuery>,
) -> Result<StatusCode, ApiError> {
    let user = require_admin(&state.db, &headers, auth_query.api_key.as_deref()).await?;
    if let Some(id) = query
        .id
        .as_deref()
        .map(str::trim)
        .filter(|id| !id.is_empty())
    {
        state.db.revoke_device(id).await?;
        record_activity(
            &state.db,
            "Device deleted",
            Some("A device was revoked."),
            "Device",
            Some(user.id),
        )
        .await?;
    }
    Ok(StatusCode::NO_CONTENT)
}

async fn system_configuration(
    State(state): State<AppState>,
) -> Result<Json<serde_json::Value>, ApiError> {
    Ok(Json(system_configuration_json(
        state.db.startup_config().await?,
        state.db.server_state().await?.startup_wizard_completed,
        state.db.system_configuration_payloads().await?,
    )))
}

#[derive(Debug, Deserialize)]
struct SystemConfigurationUpdate {
    #[serde(alias = "ServerName")]
    server_name: Option<String>,
    #[serde(rename = "UICulture", alias = "uiCulture")]
    ui_culture: Option<String>,
    #[serde(alias = "MetadataCountryCode")]
    metadata_country_code: Option<String>,
    #[serde(alias = "PreferredMetadataLanguage")]
    preferred_metadata_language: Option<String>,
    #[serde(alias = "DummyChapterDuration")]
    dummy_chapter_duration: Option<i64>,
    #[serde(alias = "ChapterImageResolution")]
    chapter_image_resolution: Option<String>,
    #[serde(alias = "EnableRemoteAccess")]
    enable_remote_access: Option<bool>,
    #[serde(alias = "CachePath")]
    cache_path: Option<String>,
    #[serde(alias = "MetadataPath")]
    metadata_path: Option<String>,
    #[serde(alias = "QuickConnectAvailable")]
    quick_connect_available: Option<bool>,
    #[serde(alias = "LibraryScanFanoutConcurrency")]
    library_scan_fanout_concurrency: Option<i64>,
    #[serde(alias = "ParallelImageEncodingLimit")]
    parallel_image_encoding_limit: Option<i64>,
    #[serde(rename = "ContentTypes", alias = "contentTypes")]
    content_types: Option<serde_json::Value>,
    #[serde(rename = "MetadataOptions", alias = "metadataOptions")]
    metadata_options: Option<serde_json::Value>,
    #[serde(rename = "PathSubstitutions", alias = "pathSubstitutions")]
    path_substitutions: Option<serde_json::Value>,
    #[serde(rename = "PluginRepositories", alias = "pluginRepositories")]
    plugin_repositories: Option<serde_json::Value>,
    #[serde(alias = "RemoteClientBitrateLimit")]
    remote_client_bitrate_limit: Option<i64>,
    #[serde(rename = "MinResumePct", alias = "minResumePct")]
    min_resume_pct: Option<i64>,
    #[serde(rename = "MaxResumePct", alias = "maxResumePct")]
    max_resume_pct: Option<i64>,
    #[serde(alias = "MinResumeDurationSeconds")]
    min_resume_duration_seconds: Option<i64>,
    #[serde(alias = "MinAudiobookResume")]
    min_audiobook_resume: Option<i64>,
    #[serde(alias = "MaxAudiobookResume")]
    max_audiobook_resume: Option<i64>,
    #[serde(rename = "TrickplayOptions", alias = "trickplayOptions")]
    trickplay_options: Option<serde_json::Value>,
    #[serde(alias = "EnableSlowResponseWarning")]
    enable_slow_response_warning: Option<bool>,
    #[serde(alias = "SlowResponseThresholdMs")]
    slow_response_threshold_ms: Option<i64>,
}

async fn update_system_configuration(
    State(state): State<AppState>,
    headers: HeaderMap,
    Query(query): Query<AuthQuery>,
    Json(payload): Json<SystemConfigurationUpdate>,
) -> Result<StatusCode, ApiError> {
    let user = require_admin(&state.db, &headers, query.api_key.as_deref()).await?;
    let current = state.db.startup_config().await?;
    let chapter_image_resolution = match payload.chapter_image_resolution.as_deref() {
        Some(value) => validate_chapter_image_resolution(value, &current.chapter_image_resolution)?,
        None => current.chapter_image_resolution,
    };
    let current_payloads = state.db.system_configuration_payloads().await?;
    let server_options =
        server_configuration_options_json(&payload, current_payloads.server_options);
    state
        .db
        .update_startup_config(StartupConfig {
            server_name: non_empty_or_current(payload.server_name, current.server_name),
            ui_culture: non_empty_or_current(payload.ui_culture, current.ui_culture),
            metadata_country_code: non_empty_or_current(
                payload.metadata_country_code,
                current.metadata_country_code,
            ),
            preferred_metadata_language: non_empty_or_current(
                payload.preferred_metadata_language,
                current.preferred_metadata_language,
            ),
            dummy_chapter_duration: payload
                .dummy_chapter_duration
                .unwrap_or(current.dummy_chapter_duration),
            chapter_image_resolution,
            enable_remote_access: payload
                .enable_remote_access
                .unwrap_or(current.enable_remote_access),
        })
        .await?;
    state
        .db
        .update_system_configuration_payloads(SystemConfigurationPayloads {
            content_types: array_update_or_current(
                payload.content_types,
                current_payloads.content_types,
            ),
            metadata_options: array_update_or_current(
                payload.metadata_options,
                current_payloads.metadata_options,
            ),
            path_substitutions: array_update_or_current(
                payload.path_substitutions,
                current_payloads.path_substitutions,
            ),
            plugin_repositories: array_update_or_current(
                payload.plugin_repositories,
                current_payloads.plugin_repositories,
            ),
            server_options,
        })
        .await?;
    record_activity(
        &state.db,
        "Server configuration updated",
        Some("Server configuration was updated."),
        "System",
        Some(user.id),
    )
    .await?;
    Ok(StatusCode::NO_CONTENT)
}

fn non_empty_or_current(update: Option<String>, current: String) -> String {
    update
        .map(|value| value.trim().to_string())
        .filter(|value| !value.is_empty())
        .unwrap_or(current)
}

fn validate_chapter_image_resolution(update: &str, current: &str) -> Result<String, ApiError> {
    let value = update.trim();
    if value.is_empty() {
        return Ok(current.to_string());
    }
    match value {
        "MatchSource" | "P144" | "P240" | "P360" | "P480" | "P720" | "P1080" | "P1440"
        | "P2160" => Ok(value.to_string()),
        _ => Err(ApiError::bad_request("Invalid ChapterImageResolution")),
    }
}

fn array_update_or_current(
    update: Option<serde_json::Value>,
    current: serde_json::Value,
) -> serde_json::Value {
    match update {
        Some(value @ serde_json::Value::Array(_)) => value,
        _ => current,
    }
}

fn server_configuration_options_json(
    payload: &SystemConfigurationUpdate,
    current: serde_json::Value,
) -> serde_json::Value {
    let mut config = default_server_configuration_options();
    if let serde_json::Value::Object(current) = current {
        for (key, value) in current {
            config[key] = value;
        }
    }
    merge_i64_option(
        &mut config,
        "RemoteClientBitrateLimit",
        payload.remote_client_bitrate_limit,
    );
    merge_i64_option(&mut config, "MinResumePct", payload.min_resume_pct);
    merge_i64_option(&mut config, "MaxResumePct", payload.max_resume_pct);
    merge_i64_option(
        &mut config,
        "MinResumeDurationSeconds",
        payload.min_resume_duration_seconds,
    );
    merge_i64_option(
        &mut config,
        "MinAudiobookResume",
        payload.min_audiobook_resume,
    );
    merge_i64_option(
        &mut config,
        "MaxAudiobookResume",
        payload.max_audiobook_resume,
    );
    if let Some(trickplay_options) = &payload.trickplay_options {
        config["TrickplayOptions"] = trickplay_options_json(trickplay_options.clone());
    }
    merge_bool_option(
        &mut config,
        "EnableSlowResponseWarning",
        payload.enable_slow_response_warning,
    );
    merge_i64_option(
        &mut config,
        "SlowResponseThresholdMs",
        payload.slow_response_threshold_ms,
    );
    merge_string_option(&mut config, "CachePath", payload.cache_path.as_deref());
    merge_string_option(
        &mut config,
        "MetadataPath",
        payload.metadata_path.as_deref(),
    );
    merge_bool_option(
        &mut config,
        "QuickConnectAvailable",
        payload.quick_connect_available,
    );
    merge_i64_option(
        &mut config,
        "LibraryScanFanoutConcurrency",
        payload.library_scan_fanout_concurrency,
    );
    merge_i64_option(
        &mut config,
        "ParallelImageEncodingLimit",
        payload.parallel_image_encoding_limit,
    );
    config
}

fn default_server_configuration_options() -> serde_json::Value {
    serde_json::json!({
        "RemoteClientBitrateLimit": 0,
        "MinResumePct": 5,
        "MaxResumePct": 90,
        "MinResumeDurationSeconds": 300,
        "MinAudiobookResume": 5,
        "MaxAudiobookResume": 5,
        "EnableSlowResponseWarning": true,
        "SlowResponseThresholdMs": 500,
        "CachePath": null,
        "MetadataPath": "",
        "QuickConnectAvailable": true,
        "LibraryScanFanoutConcurrency": 0,
        "ParallelImageEncodingLimit": 0,
        "TrickplayOptions": default_trickplay_options()
    })
}

fn default_trickplay_options() -> serde_json::Value {
    serde_json::json!({
        "EnableHwAcceleration": false,
        "EnableHwEncoding": false,
        "EnableKeyFrameOnlyExtraction": false,
        "ScanBehavior": "NonBlocking",
        "ProcessPriority": "BelowNormal",
        "Interval": 10000,
        "WidthResolutions": [320],
        "TileWidth": 10,
        "TileHeight": 10,
        "Qscale": 4,
        "JpegQuality": 90,
        "ProcessThreads": 1
    })
}

fn trickplay_options_json(payload: serde_json::Value) -> serde_json::Value {
    let mut config = default_trickplay_options();
    for key in [
        "EnableHwAcceleration",
        "EnableHwEncoding",
        "EnableKeyFrameOnlyExtraction",
        "ScanBehavior",
        "ProcessPriority",
        "Interval",
        "WidthResolutions",
        "TileWidth",
        "TileHeight",
        "Qscale",
        "JpegQuality",
        "ProcessThreads",
    ] {
        merge_known_network_value(&mut config, &payload, key);
    }
    config
}

fn merge_i64_option(config: &mut serde_json::Value, key: &'static str, value: Option<i64>) {
    if let Some(value) = value {
        config[key] = serde_json::json!(value);
    }
}

fn merge_bool_option(config: &mut serde_json::Value, key: &'static str, value: Option<bool>) {
    if let Some(value) = value {
        config[key] = serde_json::json!(value);
    }
}

fn merge_string_option(config: &mut serde_json::Value, key: &'static str, value: Option<&str>) {
    if let Some(value) = value {
        config[key] = serde_json::json!(value);
    }
}

fn system_configuration_json(
    config: StartupConfig,
    startup_wizard_completed: bool,
    payloads: SystemConfigurationPayloads,
) -> serde_json::Value {
    let mut server_options = default_server_configuration_options();
    if let serde_json::Value::Object(current) = payloads.server_options {
        for (key, value) in current {
            server_options[key] = value;
        }
    }
    serde_json::json!({
        "ServerName": config.server_name,
        "UICulture": config.ui_culture,
        "MetadataCountryCode": config.metadata_country_code,
        "PreferredMetadataLanguage": config.preferred_metadata_language,
        "DummyChapterDuration": config.dummy_chapter_duration,
        "ChapterImageResolution": config.chapter_image_resolution,
        "EnableRemoteAccess": config.enable_remote_access,
        "EnableUPnP": false,
        "IsStartupWizardCompleted": startup_wizard_completed,
        "LibraryMonitorDelay": 60,
        "EnableRealtimeMonitor": false,
        "EnableCaseSensitiveItemIds": true,
        "ImageSavingConvention": "Compatible",
        "SkipDeserializationForBasicTypes": false,
        "SkipDeserializationForPrograms": false,
        "SaveMetadataHidden": false,
        "ContentTypes": payloads.content_types,
        "MetadataOptions": payloads.metadata_options,
        "PathSubstitutions": payloads.path_substitutions,
        "PluginRepositories": payloads.plugin_repositories,
        "CachePath": server_options["CachePath"],
        "MetadataPath": server_options["MetadataPath"],
        "QuickConnectAvailable": server_options["QuickConnectAvailable"],
        "LibraryScanFanoutConcurrency": server_options["LibraryScanFanoutConcurrency"],
        "ParallelImageEncodingLimit": server_options["ParallelImageEncodingLimit"],
        "RemoteClientBitrateLimit": server_options["RemoteClientBitrateLimit"],
        "MinResumePct": server_options["MinResumePct"],
        "MaxResumePct": server_options["MaxResumePct"],
        "MinResumeDurationSeconds": server_options["MinResumeDurationSeconds"],
        "MinAudiobookResume": server_options["MinAudiobookResume"],
        "MaxAudiobookResume": server_options["MaxAudiobookResume"],
        "TrickplayOptions": server_options["TrickplayOptions"],
        "LogFileRetentionDays": 3,
        "EnableSlowResponseWarning": server_options["EnableSlowResponseWarning"],
        "SlowResponseThresholdMs": server_options["SlowResponseThresholdMs"],
        "RunAtStartup": false
    })
}

async fn named_configuration(
    State(state): State<AppState>,
    headers: HeaderMap,
    Query(query): Query<AuthQuery>,
    Path(key): Path<String>,
) -> Result<Json<serde_json::Value>, ApiError> {
    let key = key.to_ascii_lowercase();
    let value = match key.as_str() {
        "branding" => branding_configuration_json(state.db.branding_config().await?),
        "network" => {
            require_admin(&state.db, &headers, query.api_key.as_deref()).await?;
            state
                .db
                .named_configuration(&key)
                .await?
                .unwrap_or_else(default_network_configuration)
        }
        "livetv" => {
            require_admin(&state.db, &headers, query.api_key.as_deref()).await?;
            state
                .db
                .named_configuration(&key)
                .await?
                .unwrap_or_else(default_live_tv_configuration)
        }
        "metadata" => {
            require_admin(&state.db, &headers, query.api_key.as_deref()).await?;
            state
                .db
                .named_configuration(&key)
                .await?
                .unwrap_or_else(default_metadata_configuration)
        }
        "xbmcmetadata" => {
            require_admin(&state.db, &headers, query.api_key.as_deref()).await?;
            state
                .db
                .named_configuration(&key)
                .await?
                .unwrap_or_else(default_xbmc_metadata_configuration)
        }
        "encoding" => {
            require_admin(&state.db, &headers, query.api_key.as_deref()).await?;
            state
                .db
                .named_configuration(&key)
                .await?
                .unwrap_or_else(default_encoding_configuration)
        }
        _ => serde_json::json!({}),
    };
    Ok(Json(value))
}

async fn update_named_configuration(
    State(state): State<AppState>,
    headers: HeaderMap,
    Query(query): Query<AuthQuery>,
    Path(key): Path<String>,
    Json(payload): Json<serde_json::Value>,
) -> Result<StatusCode, ApiError> {
    let user = require_admin(&state.db, &headers, query.api_key.as_deref()).await?;
    let key = key.to_ascii_lowercase();
    let (normalized, activity_name) = match key.as_str() {
        "network" => {
            let normalized = network_configuration_json(payload);
            if let Some(enable_remote_access) = normalized
                .get("EnableRemoteAccess")
                .and_then(serde_json::Value::as_bool)
            {
                state.db.set_remote_access(enable_remote_access).await?;
            }
            (normalized, "Network configuration updated")
        }
        "livetv" => (
            live_tv_configuration_json(payload),
            "Live TV configuration updated",
        ),
        "metadata" => (
            metadata_configuration_json(payload),
            "Metadata configuration updated",
        ),
        "xbmcmetadata" => (
            xbmc_metadata_configuration_json(payload),
            "NFO metadata configuration updated",
        ),
        "encoding" => (
            encoding_configuration_json(payload),
            "Encoding configuration updated",
        ),
        _ => return Ok(StatusCode::NOT_IMPLEMENTED),
    };
    state
        .db
        .update_named_configuration(&key, normalized)
        .await?;
    record_activity(
        &state.db,
        activity_name,
        Some("Named configuration was updated."),
        "System",
        Some(user.id),
    )
    .await?;
    Ok(StatusCode::NO_CONTENT)
}

fn default_network_configuration() -> serde_json::Value {
    serde_json::json!({
        "BaseUrl": "",
        "EnableHttps": false,
        "RequireHttps": false,
        "CertificatePath": "",
        "CertificatePassword": "",
        "InternalHttpPort": 8097,
        "InternalHttpsPort": 8920,
        "PublicHttpPort": 8097,
        "PublicHttpsPort": 8920,
        "AutoDiscovery": true,
        "EnableUPnP": false,
        "EnableIPv4": true,
        "EnableIPv6": false,
        "EnableRemoteAccess": true,
        "LocalNetworkSubnets": [],
        "LocalNetworkAddresses": [],
        "KnownProxies": [],
        "IgnoreVirtualInterfaces": true,
        "VirtualInterfaceNames": ["vEthernet*", "utun*", "docker*", "veth*"],
        "EnablePublishedServerUriByRequest": false,
        "PublishedServerUriBySubnet": [],
        "RemoteIPFilter": [],
        "IsRemoteIPFilterBlacklist": false
    })
}

fn network_configuration_json(payload: serde_json::Value) -> serde_json::Value {
    let mut config = default_network_configuration();
    merge_known_network_value(&mut config, &payload, "BaseUrl");
    merge_known_network_value(&mut config, &payload, "CertificatePath");
    merge_known_network_value(&mut config, &payload, "CertificatePassword");
    merge_known_network_value(&mut config, &payload, "EnableHttps");
    merge_known_network_value(&mut config, &payload, "RequireHttps");
    merge_known_network_value(&mut config, &payload, "InternalHttpPort");
    merge_known_network_value(&mut config, &payload, "InternalHttpsPort");
    merge_known_network_value(&mut config, &payload, "PublicHttpPort");
    merge_known_network_value(&mut config, &payload, "PublicHttpsPort");
    merge_known_network_value(&mut config, &payload, "AutoDiscovery");
    merge_known_network_value(&mut config, &payload, "EnableUPnP");
    merge_known_network_value(&mut config, &payload, "EnableIPv4");
    merge_known_network_value(&mut config, &payload, "EnableIPv6");
    merge_known_network_value(&mut config, &payload, "EnableRemoteAccess");
    merge_known_network_value(&mut config, &payload, "LocalNetworkSubnets");
    merge_known_network_value(&mut config, &payload, "LocalNetworkAddresses");
    merge_known_network_value(&mut config, &payload, "KnownProxies");
    merge_known_network_value(&mut config, &payload, "IgnoreVirtualInterfaces");
    merge_known_network_value(&mut config, &payload, "VirtualInterfaceNames");
    merge_known_network_value(&mut config, &payload, "EnablePublishedServerUriByRequest");
    merge_known_network_value(&mut config, &payload, "PublishedServerUriBySubnet");
    merge_known_network_value(&mut config, &payload, "RemoteIPFilter");
    merge_known_network_value(&mut config, &payload, "IsRemoteIPFilterBlacklist");
    config
}

fn merge_known_network_value(
    config: &mut serde_json::Value,
    payload: &serde_json::Value,
    key: &'static str,
) {
    if let Some(value) = payload.get(key) {
        config[key] = value.clone();
    }
}

fn default_live_tv_configuration() -> serde_json::Value {
    serde_json::json!({
        "GuideDays": null,
        "RecordingPath": "",
        "MovieRecordingPath": "",
        "SeriesRecordingPath": "",
        "EnableRecordingSubfolders": false,
        "EnableOriginalAudioWithEncodedRecordings": false,
        "TunerHosts": [],
        "ListingProviders": [],
        "PrePaddingSeconds": 180,
        "PostPaddingSeconds": 180,
        "MediaLocationsCreated": [],
        "RecordingPostProcessor": "",
        "RecordingPostProcessorArguments": "",
        "SaveRecordingNFO": false,
        "SaveRecordingImages": false
    })
}

fn default_listing_provider_info() -> serde_json::Value {
    serde_json::json!({
        "Id": null,
        "Type": null,
        "Username": null,
        "Password": null,
        "ListingsId": null,
        "ZipCode": null,
        "Country": null,
        "Path": null,
        "EnabledTuners": [],
        "EnableAllTuners": true,
        "NewsCategories": ["news", "journalism", "documentary", "current affairs"],
        "SportsCategories": ["sports", "basketball", "baseball", "football"],
        "KidsCategories": ["kids", "family", "children", "childrens", "disney"],
        "MovieCategories": ["movie"],
        "ChannelMappings": [],
        "MoviePrefix": null,
        "PreferredLanguage": null,
        "UserAgent": null
    })
}

fn live_tv_configuration_json(payload: serde_json::Value) -> serde_json::Value {
    let mut config = default_live_tv_configuration();
    merge_known_network_value(&mut config, &payload, "GuideDays");
    merge_known_network_value(&mut config, &payload, "RecordingPath");
    merge_known_network_value(&mut config, &payload, "MovieRecordingPath");
    merge_known_network_value(&mut config, &payload, "SeriesRecordingPath");
    merge_known_network_value(&mut config, &payload, "EnableRecordingSubfolders");
    merge_known_network_value(
        &mut config,
        &payload,
        "EnableOriginalAudioWithEncodedRecordings",
    );
    merge_known_network_value(&mut config, &payload, "TunerHosts");
    merge_known_network_value(&mut config, &payload, "ListingProviders");
    merge_known_network_value(&mut config, &payload, "PrePaddingSeconds");
    merge_known_network_value(&mut config, &payload, "PostPaddingSeconds");
    merge_known_network_value(&mut config, &payload, "MediaLocationsCreated");
    merge_known_network_value(&mut config, &payload, "RecordingPostProcessor");
    merge_known_network_value(&mut config, &payload, "RecordingPostProcessorArguments");
    merge_known_network_value(&mut config, &payload, "SaveRecordingNFO");
    merge_known_network_value(&mut config, &payload, "SaveRecordingImages");
    config
}

fn default_metadata_configuration() -> serde_json::Value {
    serde_json::json!({
        "UseFileCreationTimeForDateAdded": false
    })
}

fn metadata_configuration_json(payload: serde_json::Value) -> serde_json::Value {
    let mut config = default_metadata_configuration();
    merge_known_network_value(&mut config, &payload, "UseFileCreationTimeForDateAdded");
    config
}

fn default_xbmc_metadata_configuration() -> serde_json::Value {
    serde_json::json!({
        "UserId": null,
        "ReleaseDateFormat": "yyyy-MM-dd",
        "SaveImagePathsInNfo": true,
        "EnablePathSubstitution": true,
        "EnableExtraThumbsDuplication": false
    })
}

fn xbmc_metadata_configuration_json(payload: serde_json::Value) -> serde_json::Value {
    let mut config = default_xbmc_metadata_configuration();
    merge_known_network_value(&mut config, &payload, "UserId");
    merge_known_network_value(&mut config, &payload, "ReleaseDateFormat");
    merge_known_network_value(&mut config, &payload, "SaveImagePathsInNfo");
    merge_known_network_value(&mut config, &payload, "EnablePathSubstitution");
    merge_known_network_value(&mut config, &payload, "EnableExtraThumbsDuplication");
    config
}

fn default_encoding_configuration() -> serde_json::Value {
    serde_json::json!({
        "EncodingThreadCount": -1,
        "TranscodingTempPath": null,
        "FallbackFontPath": null,
        "EnableFallbackFont": false,
        "EnableAudioVbr": false,
        "DownMixAudioBoost": 2.0,
        "DownMixStereoAlgorithm": "None",
        "MaxMuxingQueueSize": 2048,
        "EnableThrottling": false,
        "ThrottleDelaySeconds": 180,
        "EnableSegmentDeletion": false,
        "SegmentKeepSeconds": 720,
        "HardwareAccelerationType": "none",
        "EncoderAppPath": null,
        "EncoderAppPathDisplay": null,
        "VaapiDevice": "/dev/dri/renderD128",
        "QsvDevice": "",
        "EnableTonemapping": false,
        "EnableVppTonemapping": false,
        "EnableVideoToolboxTonemapping": false,
        "TonemappingAlgorithm": "bt2390",
        "TonemappingMode": "auto",
        "TonemappingRange": "auto",
        "TonemappingDesat": 0.0,
        "TonemappingPeak": 100.0,
        "TonemappingParam": 0.0,
        "VppTonemappingBrightness": 16.0,
        "VppTonemappingContrast": 1.0,
        "H264Crf": 23,
        "H265Crf": 28,
        "EncoderPreset": null,
        "DeinterlaceDoubleRate": false,
        "DeinterlaceMethod": "yadif",
        "EnableDecodingColorDepth10Hevc": true,
        "EnableDecodingColorDepth10Vp9": true,
        "EnableDecodingColorDepth10HevcRext": false,
        "EnableDecodingColorDepth12HevcRext": false,
        "EnableEnhancedNvdecDecoder": true,
        "PreferSystemNativeHwDecoder": true,
        "EnableIntelLowPowerH264HwEncoder": false,
        "EnableIntelLowPowerHevcHwEncoder": false,
        "EnableHardwareEncoding": true,
        "AllowHevcEncoding": false,
        "AllowAv1Encoding": false,
        "EnableSubtitleExtraction": true,
        "SubtitleExtractionTimeoutMinutes": 30,
        "HardwareDecodingCodecs": ["h264", "vc1"],
        "AllowOnDemandMetadataBasedKeyframeExtractionForExtensions": ["mkv"],
        "HlsAudioSeekStrategy": "DisableAccurateSeek"
    })
}

fn encoding_configuration_json(payload: serde_json::Value) -> serde_json::Value {
    let mut config = default_encoding_configuration();
    for key in [
        "EncodingThreadCount",
        "TranscodingTempPath",
        "FallbackFontPath",
        "EnableFallbackFont",
        "EnableAudioVbr",
        "DownMixAudioBoost",
        "DownMixStereoAlgorithm",
        "MaxMuxingQueueSize",
        "EnableThrottling",
        "ThrottleDelaySeconds",
        "EnableSegmentDeletion",
        "SegmentKeepSeconds",
        "HardwareAccelerationType",
        "EncoderAppPath",
        "EncoderAppPathDisplay",
        "VaapiDevice",
        "QsvDevice",
        "EnableTonemapping",
        "EnableVppTonemapping",
        "EnableVideoToolboxTonemapping",
        "TonemappingAlgorithm",
        "TonemappingMode",
        "TonemappingRange",
        "TonemappingDesat",
        "TonemappingPeak",
        "TonemappingParam",
        "VppTonemappingBrightness",
        "VppTonemappingContrast",
        "H264Crf",
        "H265Crf",
        "EncoderPreset",
        "DeinterlaceDoubleRate",
        "DeinterlaceMethod",
        "EnableDecodingColorDepth10Hevc",
        "EnableDecodingColorDepth10Vp9",
        "EnableDecodingColorDepth10HevcRext",
        "EnableDecodingColorDepth12HevcRext",
        "EnableEnhancedNvdecDecoder",
        "PreferSystemNativeHwDecoder",
        "EnableIntelLowPowerH264HwEncoder",
        "EnableIntelLowPowerHevcHwEncoder",
        "EnableHardwareEncoding",
        "AllowHevcEncoding",
        "AllowAv1Encoding",
        "EnableSubtitleExtraction",
        "SubtitleExtractionTimeoutMinutes",
        "HardwareDecodingCodecs",
        "AllowOnDemandMetadataBasedKeyframeExtractionForExtensions",
        "HlsAudioSeekStrategy",
    ] {
        merge_known_network_value(&mut config, &payload, key);
    }
    config
}

async fn update_branding_configuration(
    State(state): State<AppState>,
    headers: HeaderMap,
    Query(query): Query<AuthQuery>,
    Json(payload): Json<serde_json::Value>,
) -> Result<StatusCode, ApiError> {
    let user = require_admin(&state.db, &headers, query.api_key.as_deref()).await?;
    let current = state.db.branding_config().await?;
    state
        .db
        .update_branding_config(BrandingConfig {
            login_disclaimer: optional_nullable_string(
                payload.get("LoginDisclaimer").cloned(),
                current.login_disclaimer,
            ),
            custom_css: optional_nullable_string(
                payload.get("CustomCss").cloned(),
                current.custom_css,
            ),
            splashscreen_enabled: payload
                .get("SplashscreenEnabled")
                .and_then(serde_json::Value::as_bool)
                .unwrap_or(current.splashscreen_enabled),
        })
        .await?;
    record_activity(
        &state.db,
        "Branding configuration updated",
        Some("Branding configuration was updated."),
        "System",
        Some(user.id),
    )
    .await?;
    Ok(StatusCode::NO_CONTENT)
}

fn optional_nullable_string(
    update: Option<serde_json::Value>,
    current: Option<String>,
) -> Option<String> {
    match update {
        None => current,
        Some(serde_json::Value::Null) => None,
        Some(serde_json::Value::String(value)) => {
            let trimmed = value.trim().to_string();
            if trimmed.is_empty() {
                None
            } else {
                Some(trimmed)
            }
        }
        Some(value) => Some(value.to_string()),
    }
}

fn branding_configuration_json(config: BrandingConfig) -> serde_json::Value {
    serde_json::json!({
        "LoginDisclaimer": config.login_disclaimer,
        "CustomCss": config.custom_css,
        "SplashscreenEnabled": config.splashscreen_enabled
    })
}

async fn default_metadata_options() -> Json<serde_json::Value> {
    Json(serde_json::json!({
        "ItemType": null,
        "DisabledMetadataSavers": [],
        "LocalMetadataReaderOrder": [],
        "DisabledMetadataFetchers": [],
        "MetadataFetcherOrder": [],
        "DisabledImageFetchers": [],
        "ImageFetcherOrder": [],
        "DisabledSubtitleFetchers": [],
        "SubtitleFetcherOrder": []
    }))
}

async fn dashboard_configuration_pages(
    State(state): State<AppState>,
    headers: HeaderMap,
    Query(query): Query<AuthQuery>,
) -> Result<Json<Vec<serde_json::Value>>, ApiError> {
    require_admin(&state.db, &headers, query.api_key.as_deref()).await?;
    Ok(Json(Vec::new()))
}

fn empty_result() -> serde_json::Value {
    serde_json::json!({
        "Items": [],
        "TotalRecordCount": 0,
        "StartIndex": 0
    })
}

const LIBRARY_SCAN_TASK_ID: &str = "scan-media-library";
const LIBRARY_SCAN_TASK_KEY: &str = "RefreshLibrary";
const STALE_TASK_HOURS: i64 = 24;

async fn scheduled_tasks(
    State(state): State<AppState>,
    headers: HeaderMap,
    Query(query): Query<AuthQuery>,
) -> Result<Json<Vec<serde_json::Value>>, ApiError> {
    require_admin(&state.db, &headers, query.api_key.as_deref()).await?;
    recover_stale_library_scan_runs(&state.db).await?;
    Ok(Json(vec![library_scan_task_json(&state.db).await?]))
}

async fn scheduled_task(
    State(state): State<AppState>,
    headers: HeaderMap,
    Query(query): Query<AuthQuery>,
    Path(task_id): Path<String>,
) -> Result<Json<serde_json::Value>, ApiError> {
    require_admin(&state.db, &headers, query.api_key.as_deref()).await?;
    recover_stale_library_scan_runs(&state.db).await?;
    if is_library_scan_task(&task_id) {
        return Ok(Json(library_scan_task_json(&state.db).await?));
    }

    Err(ApiError::not_found("Scheduled task not found"))
}

async fn start_scheduled_task(
    State(state): State<AppState>,
    headers: HeaderMap,
    Query(query): Query<AuthQuery>,
    Path(task_id): Path<String>,
) -> Result<StatusCode, ApiError> {
    require_admin(&state.db, &headers, query.api_key.as_deref()).await?;
    if is_library_scan_task(&task_id) {
        recover_stale_library_scan_runs(&state.db).await?;
        let run = match state.db.start_task_run(LIBRARY_SCAN_TASK_KEY).await {
            Ok(run) => run,
            Err(error) if format!("{error:#}").contains("task is already running") => {
                return Ok(StatusCode::NO_CONTENT);
            }
            Err(error)
                if state
                    .db
                    .current_task_run(LIBRARY_SCAN_TASK_KEY)
                    .await?
                    .is_some() =>
            {
                let _ = error;
                return Ok(StatusCode::NO_CONTENT);
            }
            Err(error) => return Err(error.into()),
        };
        let db = state.db.clone();
        tokio::spawn(async move {
            match scan_all_library_items(&db).await {
                Ok(scanned_count) => {
                    let _ = db
                        .complete_task_run(
                            run.id,
                            serde_json::json!({
                                "ItemsScanned": scanned_count,
                            }),
                        )
                        .await;
                }
                Err(error) => {
                    let message = format!("{error:?}");
                    let _ = db.fail_task_run(run.id, &message).await;
                }
            }
        });
        return Ok(StatusCode::NO_CONTENT);
    }

    Err(ApiError::not_found("Scheduled task not found"))
}

async fn stop_scheduled_task(
    State(state): State<AppState>,
    headers: HeaderMap,
    Query(query): Query<AuthQuery>,
    Path(task_id): Path<String>,
) -> Result<StatusCode, ApiError> {
    require_admin(&state.db, &headers, query.api_key.as_deref()).await?;
    if is_library_scan_task(&task_id) {
        state
            .db
            .fail_current_task_run(LIBRARY_SCAN_TASK_KEY, "Task run cancelled.")
            .await?;
        return Ok(StatusCode::NO_CONTENT);
    }

    Err(ApiError::not_found("Scheduled task not found"))
}

async fn update_scheduled_task_triggers(
    State(state): State<AppState>,
    headers: HeaderMap,
    Query(query): Query<AuthQuery>,
    Path(task_id): Path<String>,
    Json(_triggers): Json<serde_json::Value>,
) -> Result<StatusCode, ApiError> {
    require_admin(&state.db, &headers, query.api_key.as_deref()).await?;
    if is_library_scan_task(&task_id) {
        return Ok(StatusCode::NO_CONTENT);
    }

    Err(ApiError::not_found("Scheduled task not found"))
}

async fn recover_stale_library_scan_runs(db: &Database) -> Result<(), ApiError> {
    db.fail_stale_task_runs(
        LIBRARY_SCAN_TASK_KEY,
        Duration::hours(STALE_TASK_HOURS),
        "Task run expired before completion.",
    )
    .await?;
    Ok(())
}

async fn refresh_library(
    State(state): State<AppState>,
    headers: HeaderMap,
    Query(query): Query<AuthQuery>,
) -> Result<StatusCode, ApiError> {
    let user = require_request_user(&state.db, &headers, query.api_key.as_deref()).await?;
    if !user.is_administrator {
        return Err(ApiError::forbidden("Administrator access required"));
    }
    let scanned = scan_all_library_items(&state.db).await?;
    record_activity(
        &state.db,
        "Library scan completed",
        Some(&format!("Library refresh scanned {scanned} item(s).")),
        "Library",
        Some(user.id),
    )
    .await?;
    Ok(StatusCode::NO_CONTENT)
}

async fn scan_all_library_items(db: &Database) -> Result<usize, ApiError> {
    let folders = db.virtual_folders().await?;
    let mut scanned = 0usize;
    for folder in folders {
        scanned += db.scan_virtual_folder_items(folder.id).await?;
    }
    Ok(scanned)
}

async fn library_scan_task_json(db: &Database) -> Result<serde_json::Value, ApiError> {
    let current_run = db.current_task_run(LIBRARY_SCAN_TASK_KEY).await?;
    let last_result = db.last_task_result(LIBRARY_SCAN_TASK_KEY).await?;
    let state = if current_run.is_some() {
        "Running"
    } else {
        "Idle"
    };

    Ok(serde_json::json!({
        "Name": "Scan Media Library",
        "State": state,
        "CurrentProgressPercentage": null,
        "Id": LIBRARY_SCAN_TASK_ID,
        "LastExecutionResult": last_result.as_ref().map(task_run_result_json),
        "Triggers": [
            {
                "Type": "IntervalTrigger",
                "TimeOfDayTicks": null,
                "IntervalTicks": 43_200_000_000_i64,
                "DayOfWeek": null,
                "MaxRuntimeTicks": null,
            }
        ],
        "Description": "Scans configured Jellyrin media libraries.",
        "Category": "Library",
        "IsHidden": false,
        "Key": LIBRARY_SCAN_TASK_KEY,
    }))
}

fn is_library_scan_task(task_id: &str) -> bool {
    task_id == LIBRARY_SCAN_TASK_ID || task_id == LIBRARY_SCAN_TASK_KEY
}

fn task_run_result_json(run: &TaskRun) -> serde_json::Value {
    let status = match run.status.as_str() {
        "completed" => "Completed",
        "failed" => "Failed",
        _ => "Running",
    };

    serde_json::json!({
        "Name": "Scan Media Library",
        "Key": run.task_key.clone(),
        "Id": run.id.to_string(),
        "Status": status,
        "StartTimeUtc": format_time_for_json(run.started_at),
        "EndTimeUtc": run.completed_at.map(format_time_for_json),
        "ErrorMessage": run.error_message.clone(),
        "Result": run.result_json.clone(),
    })
}

fn format_time_for_json(value: OffsetDateTime) -> String {
    value
        .format(&Rfc3339)
        .unwrap_or_else(|_| "1970-01-01T00:00:00Z".to_string())
}

#[derive(Debug, Deserialize)]
struct PlaybackReportBody {
    #[serde(alias = "ItemId")]
    item_id: String,
    #[serde(alias = "MediaSourceId")]
    media_source_id: Option<String>,
    #[serde(alias = "PlaySessionId")]
    _play_session_id: Option<String>,
    #[serde(alias = "PlayMethod")]
    _play_method: Option<String>,
    #[serde(alias = "CanSeek")]
    _can_seek: Option<bool>,
    #[serde(alias = "AudioStreamIndex")]
    audio_stream_index: Option<i64>,
    #[serde(alias = "SubtitleStreamIndex")]
    subtitle_stream_index: Option<i64>,
    #[serde(alias = "PlaylistItemId")]
    _playlist_item_id: Option<String>,
    #[serde(alias = "SessionId")]
    _session_id: Option<String>,
    #[serde(alias = "VolumeLevel")]
    _volume_level: Option<i32>,
    #[serde(alias = "IsMuted")]
    _is_muted: Option<bool>,
    #[serde(alias = "PositionTicks")]
    position_ticks: Option<i64>,
    #[serde(alias = "IsPaused")]
    is_paused: Option<bool>,
}

async fn report_playback_start(
    State(state): State<AppState>,
    headers: HeaderMap,
    Query(query): Query<AuthQuery>,
    Json(payload): Json<PlaybackReportBody>,
) -> Result<StatusCode, ApiError> {
    report_playback(state, headers, query, payload, true).await
}

async fn report_playback_progress(
    State(state): State<AppState>,
    headers: HeaderMap,
    Query(query): Query<AuthQuery>,
    Json(payload): Json<PlaybackReportBody>,
) -> Result<StatusCode, ApiError> {
    report_playback(state, headers, query, payload, true).await
}

async fn report_playback_stopped(
    State(state): State<AppState>,
    headers: HeaderMap,
    Query(query): Query<AuthQuery>,
    Json(payload): Json<PlaybackReportBody>,
) -> Result<StatusCode, ApiError> {
    report_playback(state, headers, query, payload, false).await
}

#[derive(Debug, Deserialize)]
struct PathPlaybackReportQuery {
    #[serde(alias = "api_key", alias = "ApiKey")]
    api_key: Option<String>,
    #[serde(alias = "MediaSourceId")]
    media_source_id: Option<String>,
    #[serde(alias = "AudioStreamIndex")]
    audio_stream_index: Option<i64>,
    #[serde(alias = "SubtitleStreamIndex")]
    subtitle_stream_index: Option<i64>,
    #[serde(alias = "PositionTicks")]
    position_ticks: Option<i64>,
    #[serde(alias = "StartPositionTicks")]
    start_position_ticks: Option<i64>,
    #[serde(alias = "IsPaused")]
    is_paused: Option<bool>,
}

impl PathPlaybackReportQuery {
    fn auth_query(&self) -> AuthQuery {
        AuthQuery {
            api_key: self.api_key.clone(),
        }
    }

    fn playback_body(&self, item_id: String) -> PlaybackReportBody {
        PlaybackReportBody {
            item_id,
            media_source_id: self.media_source_id.clone(),
            _play_session_id: None,
            _play_method: None,
            _can_seek: None,
            audio_stream_index: self.audio_stream_index,
            subtitle_stream_index: self.subtitle_stream_index,
            _playlist_item_id: None,
            _session_id: None,
            _volume_level: None,
            _is_muted: None,
            position_ticks: self.position_ticks.or(self.start_position_ticks),
            is_paused: self.is_paused,
        }
    }
}

async fn report_path_playback_start(
    State(state): State<AppState>,
    headers: HeaderMap,
    Query(query): Query<PathPlaybackReportQuery>,
    Path(item_id): Path<String>,
) -> Result<StatusCode, ApiError> {
    let payload = query.playback_body(item_id);
    report_playback(state, headers, query.auth_query(), payload, true).await
}

async fn report_path_playback_start_legacy(
    State(state): State<AppState>,
    headers: HeaderMap,
    Query(query): Query<PathPlaybackReportQuery>,
    Path((_user_id, item_id)): Path<(String, String)>,
) -> Result<StatusCode, ApiError> {
    let payload = query.playback_body(item_id);
    report_playback(state, headers, query.auth_query(), payload, true).await
}

async fn report_path_playback_progress(
    State(state): State<AppState>,
    headers: HeaderMap,
    Query(query): Query<PathPlaybackReportQuery>,
    Path(item_id): Path<String>,
) -> Result<StatusCode, ApiError> {
    let payload = query.playback_body(item_id);
    report_playback(state, headers, query.auth_query(), payload, true).await
}

async fn report_path_playback_progress_legacy(
    State(state): State<AppState>,
    headers: HeaderMap,
    Query(query): Query<PathPlaybackReportQuery>,
    Path((_user_id, item_id)): Path<(String, String)>,
) -> Result<StatusCode, ApiError> {
    let payload = query.playback_body(item_id);
    report_playback(state, headers, query.auth_query(), payload, true).await
}

async fn report_path_playback_stopped(
    State(state): State<AppState>,
    headers: HeaderMap,
    Query(query): Query<PathPlaybackReportQuery>,
    Path(item_id): Path<String>,
) -> Result<StatusCode, ApiError> {
    let payload = query.playback_body(item_id);
    report_playback(state, headers, query.auth_query(), payload, false).await
}

async fn report_path_playback_stopped_legacy(
    State(state): State<AppState>,
    headers: HeaderMap,
    Query(query): Query<PathPlaybackReportQuery>,
    Path((_user_id, item_id)): Path<(String, String)>,
) -> Result<StatusCode, ApiError> {
    let payload = query.playback_body(item_id);
    report_playback(state, headers, query.auth_query(), payload, false).await
}

async fn ping_playback_session(
    State(state): State<AppState>,
    headers: HeaderMap,
    Query(query): Query<AuthQuery>,
) -> Result<StatusCode, ApiError> {
    require_user(&state.db, &headers, query.api_key.as_deref()).await?;
    Ok(StatusCode::NO_CONTENT)
}

async fn report_playback(
    state: AppState,
    headers: HeaderMap,
    query: AuthQuery,
    payload: PlaybackReportBody,
    playback_active: bool,
) -> Result<StatusCode, ApiError> {
    let (user, token) = require_user(&state.db, &headers, query.api_key.as_deref()).await?;
    let item = media_item_by_id(&state.db, &payload.item_id).await?;
    let position_ticks = payload.position_ticks.unwrap_or_default();
    let is_paused = payload.is_paused.unwrap_or(false);
    state
        .db
        .upsert_playback_state(UpsertPlaybackState {
            user_id: user.id,
            item_id: item.id,
            media_source_id: payload.media_source_id.clone(),
            audio_stream_index: payload.audio_stream_index,
            subtitle_stream_index: payload.subtitle_stream_index,
            position_ticks,
            is_paused,
            played: false,
        })
        .await?;
    if playback_active {
        state
            .db
            .upsert_active_playback_session(UpsertActivePlaybackSession {
                session_id: token.access_token.clone(),
                user_id: user.id,
                item_id: item.id,
                media_source_id: payload.media_source_id,
                audio_stream_index: payload.audio_stream_index,
                subtitle_stream_index: payload.subtitle_stream_index,
                position_ticks,
                is_paused,
            })
            .await?;
    } else {
        state
            .db
            .clear_active_playback_session(&token.access_token)
            .await?;
    }
    Ok(StatusCode::NO_CONTENT)
}

#[derive(Debug, Default)]
struct SendPlayCommandQuery {
    play_command: Option<String>,
    item_ids: Option<String>,
    start_position_ticks: Option<i64>,
    media_source_id: Option<String>,
    audio_stream_index: Option<i64>,
    subtitle_stream_index: Option<i64>,
    start_index: Option<usize>,
}

async fn send_play_command(
    State(state): State<AppState>,
    headers: HeaderMap,
    Query(auth_query): Query<AuthQuery>,
    RawQuery(raw_query): RawQuery,
    Path(session_id): Path<String>,
) -> Result<StatusCode, ApiError> {
    let query = parse_send_play_command_query(raw_query.as_deref());
    let (auth_user, _) = require_user(&state.db, &headers, auth_query.api_key.as_deref()).await?;
    let target_session = state
        .db
        .device_session_by_id(&session_id)
        .await?
        .ok_or_else(|| ApiError::not_found("Device session not found"))?;
    ensure_user_access(&auth_user, target_session.user_id)?;

    let play_command = query
        .play_command
        .as_deref()
        .filter(|command| !command.trim().is_empty())
        .ok_or_else(|| ApiError::bad_request("PlayCommand is required"))?;
    let item_ids = parse_play_command_item_ids(query.item_ids.as_deref())?;
    let start_index = query.start_index.unwrap_or_default();

    let (selected_item, event_item_ids, event_play_command) =
        match play_command.to_ascii_lowercase().as_str() {
            "playinstantmix" => {
                let mix_items = audio_instant_mix_items(&state.db, &item_ids[0]).await?;
                let selected_item = mix_items.get(start_index).cloned();
                let event_item_ids = mix_items
                    .iter()
                    .take(200)
                    .map(|item| item.id.simple().to_string())
                    .collect::<Vec<_>>();
                (selected_item, event_item_ids, "PlayNow")
            }
            "playnow" | "playnext" | "playlast" | "playshuffle" => {
                let selected_id = item_ids
                    .get(start_index)
                    .or_else(|| item_ids.first())
                    .ok_or_else(|| ApiError::bad_request("ItemIds is required"))?;
                (
                    Some(media_item_by_id(&state.db, selected_id).await?),
                    item_ids.clone(),
                    canonical_play_command(play_command),
                )
            }
            _ => return Err(ApiError::bad_request("Unsupported PlayCommand")),
        };

    if let Some(item) = selected_item {
        let media_source_id = query
            .media_source_id
            .clone()
            .unwrap_or_else(|| item.id.simple().to_string());
        state
            .db
            .upsert_active_playback_session(UpsertActivePlaybackSession {
                session_id: target_session.access_token.clone(),
                user_id: target_session.user_id,
                item_id: item.id,
                media_source_id: Some(media_source_id),
                audio_stream_index: query.audio_stream_index,
                subtitle_stream_index: query.subtitle_stream_index,
                position_ticks: query.start_position_ticks.unwrap_or_default(),
                is_paused: false,
            })
            .await?;
        broadcast_session_message(
            &target_session.access_token,
            serde_json::json!({
                "MessageType": "Play",
                "Data": {
                    "ItemIds": event_item_ids,
                    "StartPositionTicks": query.start_position_ticks,
                    "PlayCommand": event_play_command,
                    "MediaSourceId": query.media_source_id,
                    "AudioStreamIndex": query.audio_stream_index,
                    "SubtitleStreamIndex": query.subtitle_stream_index,
                    "StartIndex": query.start_index,
                }
            }),
        );
    } else {
        state
            .db
            .clear_active_playback_session(&target_session.access_token)
            .await?;
    }
    broadcast_sessions_message(&state.db, &target_session.access_token, &auth_user).await?;

    Ok(StatusCode::NO_CONTENT)
}

fn canonical_play_command(command: &str) -> &'static str {
    match command.to_ascii_lowercase().as_str() {
        "playnext" => "PlayNext",
        "playlast" => "PlayLast",
        "playshuffle" => "PlayShuffle",
        "playnow" => "PlayNow",
        _ => "PlayNow",
    }
}

fn parse_play_command_item_ids(value: Option<&str>) -> Result<Vec<String>, ApiError> {
    let item_ids = value
        .ok_or_else(|| ApiError::bad_request("ItemIds is required"))?
        .split(',')
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .map(ToOwned::to_owned)
        .collect::<Vec<_>>();
    if item_ids.is_empty() {
        Err(ApiError::bad_request("ItemIds is required"))
    } else {
        Ok(item_ids)
    }
}

fn parse_send_play_command_query(raw_query: Option<&str>) -> SendPlayCommandQuery {
    let mut query = SendPlayCommandQuery::default();
    let Some(raw_query) = raw_query else {
        return query;
    };

    for part in raw_query.split('&').filter(|part| !part.is_empty()) {
        let (key, value) = part.split_once('=').unwrap_or((part, ""));
        let key = percent_decode_query_component(key).to_ascii_lowercase();
        let value = percent_decode_query_component(value);
        match key.as_str() {
            "playcommand" => query.play_command = Some(value),
            "itemids" => set_query_scalar(&mut query.item_ids, value),
            "startpositionticks" => query.start_position_ticks = value.parse().ok(),
            "mediasourceid" => query.media_source_id = Some(value),
            "audiostreamindex" => query.audio_stream_index = value.parse().ok(),
            "subtitlestreamindex" => query.subtitle_stream_index = value.parse().ok(),
            "startindex" => query.start_index = value.parse().ok(),
            _ => {}
        }
    }

    query
}

#[derive(Debug, Default)]
struct SendPlaystateCommandQuery {
    seek_position_ticks: Option<i64>,
}

async fn send_playstate_command(
    State(state): State<AppState>,
    headers: HeaderMap,
    Query(auth_query): Query<AuthQuery>,
    RawQuery(raw_query): RawQuery,
    Path((session_id, command)): Path<(String, String)>,
) -> Result<StatusCode, ApiError> {
    let query = parse_send_playstate_command_query(raw_query.as_deref());
    let (auth_user, _) = require_user(&state.db, &headers, auth_query.api_key.as_deref()).await?;
    let target_session = state
        .db
        .device_session_by_id(&session_id)
        .await?
        .ok_or_else(|| ApiError::not_found("Device session not found"))?;
    ensure_user_access(&auth_user, target_session.user_id)?;

    let current_playback = state
        .db
        .active_playback_sessions()
        .await?
        .into_iter()
        .find(|playback| playback.session_id == target_session.access_token);

    let canonical_command = canonical_playstate_command(&command)?;
    match canonical_command {
        "Stop" => {
            state
                .db
                .clear_active_playback_session(&target_session.access_token)
                .await?;
        }
        "Pause" | "Unpause" | "PlayPause" | "Seek" => {
            if let Some(playback) = current_playback {
                let is_paused = match canonical_command {
                    "Pause" => true,
                    "Unpause" => false,
                    "PlayPause" => !playback.is_paused,
                    _ => playback.is_paused,
                };
                state
                    .db
                    .upsert_active_playback_session(UpsertActivePlaybackSession {
                        session_id: target_session.access_token.clone(),
                        user_id: target_session.user_id,
                        item_id: playback.item.id,
                        media_source_id: playback.media_source_id,
                        audio_stream_index: playback.audio_stream_index,
                        subtitle_stream_index: playback.subtitle_stream_index,
                        position_ticks: query
                            .seek_position_ticks
                            .unwrap_or(playback.position_ticks),
                        is_paused,
                    })
                    .await?;
            }
        }
        "NextTrack" | "PreviousTrack" | "Rewind" | "FastForward" => {}
        _ => unreachable!("canonical playstate command must be exhaustive"),
    }
    broadcast_session_message(
        &target_session.access_token,
        serde_json::json!({
            "MessageType": "Playstate",
            "Data": {
                "Command": canonical_command,
                "SeekPositionTicks": query.seek_position_ticks,
            }
        }),
    );
    broadcast_sessions_message(&state.db, &target_session.access_token, &auth_user).await?;

    Ok(StatusCode::NO_CONTENT)
}

fn canonical_playstate_command(command: &str) -> Result<&'static str, ApiError> {
    match command.to_ascii_lowercase().as_str() {
        "stop" => Ok("Stop"),
        "pause" => Ok("Pause"),
        "unpause" => Ok("Unpause"),
        "playpause" => Ok("PlayPause"),
        "seek" => Ok("Seek"),
        "nexttrack" => Ok("NextTrack"),
        "previoustrack" => Ok("PreviousTrack"),
        "rewind" => Ok("Rewind"),
        "fastforward" => Ok("FastForward"),
        _ => Err(ApiError::bad_request("Unsupported PlaystateCommand")),
    }
}

fn parse_send_playstate_command_query(raw_query: Option<&str>) -> SendPlaystateCommandQuery {
    let mut query = SendPlaystateCommandQuery::default();
    let Some(raw_query) = raw_query else {
        return query;
    };

    for part in raw_query.split('&').filter(|part| !part.is_empty()) {
        let (key, value) = part.split_once('=').unwrap_or((part, ""));
        let key = percent_decode_query_component(key).to_ascii_lowercase();
        let value = percent_decode_query_component(value);
        if key == "seekpositionticks" {
            query.seek_position_ticks = value.parse().ok();
        }
    }

    query
}

async fn quick_connect_enabled() -> Json<bool> {
    Json(false)
}

async fn empty_json_array() -> Json<Vec<serde_json::Value>> {
    Json(Vec::new())
}

async fn empty_result_json() -> Json<serde_json::Value> {
    Json(empty_result())
}

async fn live_tv_info() -> Json<serde_json::Value> {
    Json(serde_json::json!({
        "Services": [],
        "IsEnabled": false,
        "EnabledUsers": [],
        "TunerHosts": [],
        "ListingsProviders": []
    }))
}

async fn live_tv_guide_info() -> Json<serde_json::Value> {
    Json(serde_json::json!({
        "StartDate": null,
        "EndDate": null
    }))
}

#[derive(Debug, Deserialize)]
struct LiveTvChannelMappingOptionsQuery {
    #[serde(alias = "providerId", alias = "ProviderId")]
    provider_id: Option<String>,
    #[serde(alias = "api_key", alias = "ApiKey")]
    api_key: Option<String>,
}

async fn live_tv_channel_mapping_options(
    State(state): State<AppState>,
    headers: HeaderMap,
    Query(query): Query<LiveTvChannelMappingOptionsQuery>,
) -> Result<Json<serde_json::Value>, ApiError> {
    require_admin(&state.db, &headers, query.api_key.as_deref()).await?;
    let provider_id = query
        .provider_id
        .as_deref()
        .map(str::trim)
        .filter(|id| !id.is_empty())
        .ok_or_else(|| ApiError::bad_request("Provider id is required"))?;
    let config = state
        .db
        .named_configuration("livetv")
        .await?
        .unwrap_or_else(default_live_tv_configuration);
    let provider = live_tv_listing_provider(&config, provider_id)
        .ok_or_else(|| ApiError::not_found("Listing provider not found"))?;
    let mappings = provider
        .get("ChannelMappings")
        .and_then(serde_json::Value::as_array)
        .cloned()
        .unwrap_or_default();
    Ok(Json(serde_json::json!({
        "TunerChannels": [],
        "ProviderChannels": [],
        "Mappings": mappings,
        "ProviderName": live_tv_provider_name(provider)
    })))
}

#[derive(Debug, Deserialize)]
struct SetLiveTvChannelMappingBody {
    #[serde(alias = "ProviderId")]
    provider_id: String,
    #[serde(alias = "TunerChannelId")]
    tuner_channel_id: String,
    #[serde(alias = "ProviderChannelId")]
    provider_channel_id: String,
}

async fn set_live_tv_channel_mapping(
    State(state): State<AppState>,
    headers: HeaderMap,
    Query(query): Query<AuthQuery>,
    Json(payload): Json<SetLiveTvChannelMappingBody>,
) -> Result<Json<serde_json::Value>, ApiError> {
    require_admin(&state.db, &headers, query.api_key.as_deref()).await?;
    let provider_id = payload.provider_id.trim();
    let tuner_channel_id = payload.tuner_channel_id.trim();
    let provider_channel_id = payload.provider_channel_id.trim();
    if provider_id.is_empty() || tuner_channel_id.is_empty() || provider_channel_id.is_empty() {
        return Err(ApiError::bad_request(
            "Provider, tuner channel and provider channel ids are required",
        ));
    }

    let mut config = state
        .db
        .named_configuration("livetv")
        .await?
        .unwrap_or_else(default_live_tv_configuration);
    let mut listing_providers = config
        .get("ListingProviders")
        .and_then(serde_json::Value::as_array)
        .cloned()
        .unwrap_or_default();
    let provider = listing_providers
        .iter_mut()
        .find(|provider| {
            provider
                .get("Id")
                .and_then(serde_json::Value::as_str)
                .is_some_and(|id| id.eq_ignore_ascii_case(provider_id))
        })
        .ok_or_else(|| ApiError::not_found("Listing provider not found"))?;
    let mut mappings = provider
        .get("ChannelMappings")
        .and_then(serde_json::Value::as_array)
        .cloned()
        .unwrap_or_default();
    mappings.retain(|mapping| {
        mapping
            .get("Name")
            .and_then(serde_json::Value::as_str)
            .is_none_or(|name| !name.eq_ignore_ascii_case(tuner_channel_id))
    });
    if !tuner_channel_id.eq_ignore_ascii_case(provider_channel_id) {
        mappings.push(serde_json::json!({
            "Name": tuner_channel_id,
            "Value": provider_channel_id
        }));
    }
    provider["ChannelMappings"] = serde_json::json!(mappings);
    config["ListingProviders"] = serde_json::json!(listing_providers);
    state
        .db
        .update_named_configuration("livetv", live_tv_configuration_json(config))
        .await?;
    Ok(Json(serde_json::json!({
        "Id": tuner_channel_id,
        "Name": tuner_channel_id,
        "ProviderChannelId": provider_channel_id,
        "ProviderChannelName": provider_channel_id
    })))
}

fn live_tv_listing_provider<'a>(
    config: &'a serde_json::Value,
    provider_id: &str,
) -> Option<&'a serde_json::Value> {
    config
        .get("ListingProviders")
        .and_then(serde_json::Value::as_array)
        .and_then(|providers| {
            providers.iter().find(|provider| {
                provider
                    .get("Id")
                    .and_then(serde_json::Value::as_str)
                    .is_some_and(|id| id.eq_ignore_ascii_case(provider_id))
            })
        })
}

fn live_tv_provider_name(provider: &serde_json::Value) -> String {
    for key in ["Name", "ListingsId", "Type", "Id"] {
        if let Some(value) = provider
            .get(key)
            .and_then(serde_json::Value::as_str)
            .map(str::trim)
            .filter(|value| !value.is_empty())
        {
            return value.to_string();
        }
    }
    String::new()
}

async fn live_tv_tuner_host_types(
    State(state): State<AppState>,
    headers: HeaderMap,
    Query(query): Query<AuthQuery>,
) -> Result<Json<Vec<serde_json::Value>>, ApiError> {
    require_user(&state.db, &headers, query.api_key.as_deref()).await?;
    Ok(Json(vec![
        serde_json::json!({ "Id": "hdhomerun", "Name": "HDHomeRun" }),
        serde_json::json!({ "Id": "m3u", "Name": "M3U Tuner" }),
    ]))
}

async fn add_live_tv_tuner_host(
    State(state): State<AppState>,
    headers: HeaderMap,
    Query(query): Query<AuthQuery>,
    Json(mut payload): Json<serde_json::Value>,
) -> Result<Json<serde_json::Value>, ApiError> {
    require_admin(&state.db, &headers, query.api_key.as_deref()).await?;
    if !payload.is_object() {
        return Err(ApiError::bad_request("Tuner host must be an object"));
    }

    let tuner_id = payload
        .get("Id")
        .and_then(serde_json::Value::as_str)
        .map(str::trim)
        .filter(|id| !id.is_empty())
        .map(ToOwned::to_owned)
        .unwrap_or_else(|| Uuid::new_v4().simple().to_string());
    payload["Id"] = serde_json::json!(tuner_id);

    let mut config = state
        .db
        .named_configuration("livetv")
        .await?
        .unwrap_or_else(default_live_tv_configuration);
    let mut tuner_hosts = config
        .get("TunerHosts")
        .and_then(serde_json::Value::as_array)
        .cloned()
        .unwrap_or_default();
    if let Some(existing) = tuner_hosts.iter_mut().find(|tuner| {
        tuner.get("Id").and_then(serde_json::Value::as_str)
            == payload.get("Id").and_then(serde_json::Value::as_str)
    }) {
        *existing = payload.clone();
    } else {
        tuner_hosts.push(payload.clone());
    }
    config["TunerHosts"] = serde_json::json!(tuner_hosts);
    state
        .db
        .update_named_configuration("livetv", live_tv_configuration_json(config))
        .await?;
    Ok(Json(payload))
}

#[derive(Debug, Deserialize)]
struct LiveTvTunerHostQuery {
    #[serde(alias = "Id")]
    id: Option<String>,
    #[serde(alias = "api_key", alias = "ApiKey")]
    api_key: Option<String>,
}

async fn delete_live_tv_tuner_host(
    State(state): State<AppState>,
    headers: HeaderMap,
    Query(query): Query<LiveTvTunerHostQuery>,
) -> Result<StatusCode, ApiError> {
    require_admin(&state.db, &headers, query.api_key.as_deref()).await?;
    let tuner_id = query.id.as_deref().unwrap_or_default();
    let mut config = state
        .db
        .named_configuration("livetv")
        .await?
        .unwrap_or_else(default_live_tv_configuration);
    let tuner_hosts = config
        .get("TunerHosts")
        .and_then(serde_json::Value::as_array)
        .map(|hosts| {
            hosts
                .iter()
                .filter(|tuner| {
                    tuner.get("Id").and_then(serde_json::Value::as_str) != Some(tuner_id)
                })
                .cloned()
                .collect::<Vec<_>>()
        })
        .unwrap_or_default();
    config["TunerHosts"] = serde_json::json!(tuner_hosts);
    state
        .db
        .update_named_configuration("livetv", live_tv_configuration_json(config))
        .await?;
    Ok(StatusCode::NO_CONTENT)
}

async fn default_live_tv_listing_provider(
    State(state): State<AppState>,
    headers: HeaderMap,
    Query(query): Query<AuthQuery>,
) -> Result<Json<serde_json::Value>, ApiError> {
    require_user(&state.db, &headers, query.api_key.as_deref()).await?;
    Ok(Json(default_listing_provider_info()))
}

#[derive(Debug, Deserialize)]
struct LiveTvListingProviderPostQuery {
    #[serde(alias = "api_key", alias = "ApiKey")]
    api_key: Option<String>,
    #[serde(alias = "pw", alias = "Pw")]
    _password: Option<String>,
    #[serde(alias = "ValidateListings")]
    _validate_listings: Option<bool>,
    #[serde(alias = "ValidateLogin")]
    _validate_login: Option<bool>,
}

async fn add_live_tv_listing_provider(
    State(state): State<AppState>,
    headers: HeaderMap,
    Query(query): Query<LiveTvListingProviderPostQuery>,
    Json(payload): Json<serde_json::Value>,
) -> Result<Json<serde_json::Value>, ApiError> {
    require_admin(&state.db, &headers, query.api_key.as_deref()).await?;
    if !payload.is_object() {
        return Err(ApiError::bad_request("Listing provider must be an object"));
    }

    let mut provider = default_listing_provider_info();
    if let serde_json::Value::Object(fields) = payload {
        for (key, value) in fields {
            provider[key] = value;
        }
    }
    let provider_id = provider
        .get("Id")
        .and_then(serde_json::Value::as_str)
        .map(str::trim)
        .filter(|id| !id.is_empty())
        .map(ToOwned::to_owned)
        .unwrap_or_else(|| Uuid::new_v4().simple().to_string());
    provider["Id"] = serde_json::json!(provider_id);

    let mut config = state
        .db
        .named_configuration("livetv")
        .await?
        .unwrap_or_else(default_live_tv_configuration);
    let mut listing_providers = config
        .get("ListingProviders")
        .and_then(serde_json::Value::as_array)
        .cloned()
        .unwrap_or_default();
    if let Some(existing) = listing_providers.iter_mut().find(|listing_provider| {
        listing_provider
            .get("Id")
            .and_then(serde_json::Value::as_str)
            == provider.get("Id").and_then(serde_json::Value::as_str)
    }) {
        *existing = provider.clone();
    } else {
        listing_providers.push(provider.clone());
    }
    config["ListingProviders"] = serde_json::json!(listing_providers);
    state
        .db
        .update_named_configuration("livetv", live_tv_configuration_json(config))
        .await?;
    Ok(Json(provider))
}

#[derive(Debug, Deserialize)]
struct LiveTvListingProviderDeleteQuery {
    #[serde(alias = "Id")]
    id: Option<String>,
    #[serde(alias = "api_key", alias = "ApiKey")]
    api_key: Option<String>,
}

async fn delete_live_tv_listing_provider(
    State(state): State<AppState>,
    headers: HeaderMap,
    Query(query): Query<LiveTvListingProviderDeleteQuery>,
) -> Result<StatusCode, ApiError> {
    require_admin(&state.db, &headers, query.api_key.as_deref()).await?;
    let provider_id = query.id.as_deref().unwrap_or_default();
    let mut config = state
        .db
        .named_configuration("livetv")
        .await?
        .unwrap_or_else(default_live_tv_configuration);
    let listing_providers = config
        .get("ListingProviders")
        .and_then(serde_json::Value::as_array)
        .map(|providers| {
            providers
                .iter()
                .filter(|provider| {
                    provider.get("Id").and_then(serde_json::Value::as_str) != Some(provider_id)
                })
                .cloned()
                .collect::<Vec<_>>()
        })
        .unwrap_or_default();
    config["ListingProviders"] = serde_json::json!(listing_providers);
    state
        .db
        .update_named_configuration("livetv", live_tv_configuration_json(config))
        .await?;
    Ok(StatusCode::NO_CONTENT)
}

#[derive(Debug, Deserialize)]
struct LiveTvListingProviderLineupsQuery {
    #[serde(alias = "api_key", alias = "ApiKey")]
    api_key: Option<String>,
    #[serde(alias = "Id")]
    _id: Option<String>,
    #[serde(alias = "Type")]
    _provider_type: Option<String>,
    #[serde(alias = "Location")]
    _location: Option<String>,
    #[serde(alias = "Country")]
    _country: Option<String>,
}

async fn live_tv_listing_provider_lineups(
    State(state): State<AppState>,
    headers: HeaderMap,
    Query(query): Query<LiveTvListingProviderLineupsQuery>,
) -> Result<Json<Vec<serde_json::Value>>, ApiError> {
    require_user(&state.db, &headers, query.api_key.as_deref()).await?;
    Ok(Json(Vec::new()))
}

async fn live_tv_schedules_direct_countries(
    State(state): State<AppState>,
    headers: HeaderMap,
    Query(query): Query<AuthQuery>,
) -> Result<Json<serde_json::Value>, ApiError> {
    require_admin(&state.db, &headers, query.api_key.as_deref()).await?;
    Ok(Json(serde_json::json!({
        "North America": [
            { "fullName": "Canada", "shortName": "CAN" },
            { "fullName": "United States", "shortName": "USA" }
        ],
        "ZZZ": []
    })))
}

async fn branding_configuration(
    State(state): State<AppState>,
) -> Result<Json<serde_json::Value>, ApiError> {
    Ok(Json(branding_configuration_json(
        state.db.branding_config().await?,
    )))
}

async fn branding_css(State(state): State<AppState>) -> Result<String, ApiError> {
    Ok(state
        .db
        .branding_config()
        .await?
        .custom_css
        .unwrap_or_default())
}

async fn empty_text() -> &'static str {
    ""
}

async fn get_virtual_folders(
    State(state): State<AppState>,
    headers: HeaderMap,
    Query(query): Query<AuthQuery>,
) -> Result<Json<Vec<serde_json::Value>>, ApiError> {
    require_admin_or_startup_incomplete(&state.db, &headers, query.api_key.as_deref()).await?;
    let folders = state.db.virtual_folders().await?;
    Ok(Json(folders.iter().map(virtual_folder_to_json).collect()))
}

#[derive(Debug, Deserialize)]
struct AddVirtualFolderQuery {
    #[serde(alias = "Name", alias = "name")]
    name: String,
    #[serde(alias = "CollectionType", alias = "collectionType")]
    collection_type: Option<String>,
    #[serde(alias = "Paths", alias = "paths")]
    paths: Option<String>,
}

#[derive(Debug, Deserialize)]
struct AddVirtualFolderBody {
    #[serde(alias = "LibraryOptions")]
    library_options: Option<LibraryOptionsBody>,
}

#[derive(Debug, Deserialize)]
struct LibraryOptionsBody {
    #[serde(alias = "PathInfos")]
    path_infos: Option<Vec<MediaPathInfoBody>>,
}

#[derive(Debug, Deserialize)]
struct MediaPathInfoBody {
    #[serde(alias = "Path")]
    path: Option<String>,
}

async fn add_virtual_folder(
    State(state): State<AppState>,
    headers: HeaderMap,
    Query(auth_query): Query<AuthQuery>,
    Query(query): Query<AddVirtualFolderQuery>,
    body: Option<Json<AddVirtualFolderBody>>,
) -> Result<StatusCode, ApiError> {
    let user = require_admin_or_startup_incomplete_user(
        &state.db,
        &headers,
        auth_query.api_key.as_deref(),
    )
    .await?;
    let mut locations = comma_delimited_paths(query.paths.as_deref());
    if let Some(Json(body)) = body
        && let Some(library_options) = body.library_options
        && let Some(path_infos) = library_options.path_infos
    {
        locations.extend(
            path_infos
                .into_iter()
                .filter_map(|path_info| path_info.path),
        );
    }

    let folder = state
        .db
        .upsert_virtual_folder(&query.name, query.collection_type.as_deref(), locations)
        .await?;
    state.db.scan_virtual_folder_items(folder.id).await?;
    record_activity(
        &state.db,
        "Library added",
        Some(&format!("Library {} was added or updated.", folder.name)),
        "Library",
        user.map(|user| user.id),
    )
    .await?;
    Ok(StatusCode::NO_CONTENT)
}

#[derive(Debug, Deserialize)]
struct AddMediaPathBody {
    #[serde(alias = "Name")]
    name: String,
    #[serde(alias = "Path")]
    path: Option<String>,
    #[serde(alias = "PathInfo")]
    path_info: Option<MediaPathInfoBody>,
}

async fn add_virtual_folder_path(
    State(state): State<AppState>,
    headers: HeaderMap,
    Query(query): Query<AuthQuery>,
    Json(payload): Json<AddMediaPathBody>,
) -> Result<StatusCode, ApiError> {
    let user =
        require_admin_or_startup_incomplete_user(&state.db, &headers, query.api_key.as_deref())
            .await?;
    let path = payload
        .path_info
        .and_then(|path_info| path_info.path)
        .or(payload.path)
        .ok_or_else(|| ApiError::bad_request("PathInfo and Path can't both be null"))?;
    state
        .db
        .add_virtual_folder_path(&payload.name, &path)
        .await?;
    let folder = state
        .db
        .virtual_folders()
        .await?
        .into_iter()
        .find(|folder| folder.name.eq_ignore_ascii_case(&payload.name))
        .ok_or_else(|| ApiError::bad_request("Virtual folder not found"))?;
    state.db.scan_virtual_folder_items(folder.id).await?;
    record_activity(
        &state.db,
        "Library path added",
        Some(&format!("A path was added to library {}.", folder.name)),
        "Library",
        user.map(|user| user.id),
    )
    .await?;
    Ok(StatusCode::NO_CONTENT)
}

#[derive(Debug, Deserialize)]
struct VirtualFolderNameQuery {
    #[serde(alias = "Name", alias = "name")]
    name: String,
}

async fn delete_virtual_folder(
    State(state): State<AppState>,
    headers: HeaderMap,
    Query(auth_query): Query<AuthQuery>,
    Query(query): Query<VirtualFolderNameQuery>,
) -> Result<StatusCode, ApiError> {
    let user = require_admin_or_startup_incomplete_user(
        &state.db,
        &headers,
        auth_query.api_key.as_deref(),
    )
    .await?;
    if state.db.delete_virtual_folder(&query.name).await? {
        record_activity(
            &state.db,
            "Library deleted",
            Some(&format!("Library {} was deleted.", query.name)),
            "Library",
            user.map(|user| user.id),
        )
        .await?;
        Ok(StatusCode::NO_CONTENT)
    } else {
        Err(ApiError::not_found("Virtual folder not found"))
    }
}

#[derive(Debug, Deserialize)]
struct DeleteMediaPathQuery {
    #[serde(alias = "Name", alias = "name")]
    name: String,
    #[serde(alias = "Path", alias = "path")]
    path: String,
}

async fn delete_virtual_folder_path(
    State(state): State<AppState>,
    headers: HeaderMap,
    Query(auth_query): Query<AuthQuery>,
    Query(query): Query<DeleteMediaPathQuery>,
) -> Result<StatusCode, ApiError> {
    let user = require_admin_or_startup_incomplete_user(
        &state.db,
        &headers,
        auth_query.api_key.as_deref(),
    )
    .await?;
    if state
        .db
        .remove_virtual_folder_path(&query.name, &query.path)
        .await?
    {
        record_activity(
            &state.db,
            "Library path deleted",
            Some(&format!("A path was removed from library {}.", query.name)),
            "Library",
            user.map(|user| user.id),
        )
        .await?;
        Ok(StatusCode::NO_CONTENT)
    } else {
        Err(ApiError::not_found("Virtual folder path not found"))
    }
}

fn comma_delimited_paths(paths: Option<&str>) -> Vec<String> {
    paths
        .into_iter()
        .flat_map(|paths| paths.split(','))
        .map(str::trim)
        .filter(|path| !path.is_empty())
        .map(ToOwned::to_owned)
        .collect()
}

fn virtual_folder_to_json(folder: &VirtualFolder) -> serde_json::Value {
    serde_json::json!({
        "Name": folder.name,
        "Locations": folder.locations,
        "CollectionType": folder.collection_type,
        "LibraryOptions": {
            "Enabled": true,
            "EnablePhotos": true,
            "EnableRealtimeMonitor": false,
            "EnableLUFSScan": false,
            "EnableChapterImageExtraction": false,
            "ExtractChapterImagesDuringLibraryScan": false,
            "EnableTrickplayImageExtraction": false,
            "ExtractTrickplayImagesDuringLibraryScan": false,
            "PathInfos": folder.locations.iter().map(|path| {
                serde_json::json!({ "Path": path })
            }).collect::<Vec<_>>(),
            "SaveLocalMetadata": false,
            "EnableInternetProviders": false,
            "EnableAutomaticSeriesGrouping": true,
            "EnableEmbeddedTitles": false,
            "EnableEmbeddedExtrasTitles": false,
            "EnableEmbeddedEpisodeInfos": false,
            "AutomaticRefreshIntervalDays": 0,
            "PreferredMetadataLanguage": null,
            "MetadataCountryCode": null,
            "SeasonZeroDisplayName": "Specials"
        },
        "ItemId": folder.id.simple().to_string(),
        "PrimaryImageItemId": null,
        "RefreshProgress": null,
        "RefreshStatus": "Idle"
    })
}

async fn environment_drives(
    State(state): State<AppState>,
    headers: HeaderMap,
    Query(query): Query<AuthQuery>,
) -> Result<Json<Vec<serde_json::Value>>, ApiError> {
    require_admin_or_startup_incomplete(&state.db, &headers, query.api_key.as_deref()).await?;
    Ok(Json(vec![
        serde_json::json!({
            "Name": "/",
            "Path": "/",
            "Type": "Network",
        }),
        serde_json::json!({
            "Name": "Home",
            "Path": "/home/cdmonio",
            "Type": "Network",
        }),
    ]))
}

async fn environment_default_directory_browser(
    State(state): State<AppState>,
    headers: HeaderMap,
    Query(query): Query<AuthQuery>,
) -> Result<Json<serde_json::Value>, ApiError> {
    require_admin_or_startup_incomplete(&state.db, &headers, query.api_key.as_deref()).await?;
    Ok(Json(serde_json::json!({ "Path": null })))
}

#[derive(Debug, Deserialize)]
struct DirectoryQuery {
    #[serde(alias = "Path", alias = "path")]
    path: Option<PathBuf>,
    #[serde(alias = "IncludeFiles")]
    include_files: Option<bool>,
}

async fn environment_directory_contents(
    State(state): State<AppState>,
    headers: HeaderMap,
    Query(auth_query): Query<AuthQuery>,
    Query(query): Query<DirectoryQuery>,
) -> Result<Json<Vec<serde_json::Value>>, ApiError> {
    require_admin_or_startup_incomplete(&state.db, &headers, auth_query.api_key.as_deref()).await?;
    let path = query.path.unwrap_or_else(|| PathBuf::from("/"));
    let include_files = query.include_files.unwrap_or(false);

    let entries = fs::read_dir(path)
        .map(|read_dir| {
            read_dir
                .filter_map(Result::ok)
                .filter_map(|entry| {
                    let metadata = entry.metadata().ok()?;
                    if metadata.is_file() && !include_files {
                        return None;
                    }

                    let path = entry.path();
                    let name = entry.file_name().to_string_lossy().into_owned();
                    Some(serde_json::json!({
                        "Name": name,
                        "Path": path,
                        "Type": if metadata.is_dir() { "Directory" } else { "File" },
                    }))
                })
                .collect::<Vec<_>>()
        })
        .unwrap_or_default();

    Ok(Json(entries))
}

#[derive(Debug, Deserialize)]
struct PathQuery {
    #[serde(alias = "Path", alias = "path")]
    path: Option<PathBuf>,
}

async fn environment_parent_path(
    State(state): State<AppState>,
    headers: HeaderMap,
    Query(auth_query): Query<AuthQuery>,
    Query(query): Query<PathQuery>,
) -> Result<Json<Option<String>>, ApiError> {
    require_admin_or_startup_incomplete(&state.db, &headers, auth_query.api_key.as_deref()).await?;
    let parent = query
        .path
        .as_deref()
        .and_then(|path| path.parent())
        .map(|path| path.to_string_lossy().into_owned());
    Ok(Json(parent))
}

#[derive(Debug, Deserialize)]
struct ValidatePathRequest {
    #[serde(alias = "Path", alias = "path")]
    path: Option<PathBuf>,
}

async fn environment_validate_path(
    State(state): State<AppState>,
    headers: HeaderMap,
    Query(query): Query<AuthQuery>,
    Json(payload): Json<ValidatePathRequest>,
) -> Result<StatusCode, ApiError> {
    require_admin_or_startup_incomplete(&state.db, &headers, query.api_key.as_deref()).await?;
    if payload.path.as_deref().is_some_and(|path| path.exists()) {
        Ok(StatusCode::NO_CONTENT)
    } else {
        Ok(StatusCode::BAD_REQUEST)
    }
}

async fn user_views_result(
    State(state): State<AppState>,
    headers: HeaderMap,
    Query(query): Query<AuthQuery>,
) -> Result<Json<serde_json::Value>, ApiError> {
    require_request_user(&state.db, &headers, query.api_key.as_deref()).await?;
    let folders = state.db.virtual_folders().await?;
    let server_id = state.db.server_state().await?.server_id.to_string();
    let items = folders
        .iter()
        .map(|folder| user_view_to_json(folder, &server_id))
        .collect::<Vec<_>>();
    Ok(Json(query_result(items)))
}

#[derive(Debug, Deserialize, Default)]
struct ItemsQuery {
    #[serde(alias = "UserId", alias = "userId")]
    user_id: Option<String>,
    #[serde(alias = "Ids", alias = "ids")]
    ids: Option<String>,
    #[serde(alias = "ParentId", alias = "parentId")]
    parent_id: Option<String>,
    #[serde(
        default,
        deserialize_with = "deserialize_csv_values",
        alias = "IncludeItemTypes",
        alias = "includeItemTypes"
    )]
    include_item_types: Vec<String>,
    #[serde(
        default,
        deserialize_with = "deserialize_csv_values",
        alias = "ExcludeItemTypes",
        alias = "excludeItemTypes"
    )]
    exclude_item_types: Vec<String>,
    #[serde(
        default,
        deserialize_with = "deserialize_csv_values",
        alias = "MediaTypes",
        alias = "mediaTypes"
    )]
    media_types: Vec<String>,
    #[serde(alias = "SearchTerm", alias = "searchTerm")]
    search_term: Option<String>,
    #[serde(alias = "IsPlayed", alias = "isPlayed")]
    is_played: Option<bool>,
    #[serde(alias = "IsFolder", alias = "isFolder")]
    is_folder: Option<bool>,
    #[serde(
        default,
        deserialize_with = "deserialize_csv_values",
        alias = "Filters",
        alias = "filters"
    )]
    filters: Vec<String>,
    #[serde(alias = "NameStartsWith", alias = "nameStartsWith")]
    name_starts_with: Option<String>,
    #[serde(alias = "NameStartsWithOrGreater", alias = "nameStartsWithOrGreater")]
    name_starts_with_or_greater: Option<String>,
    #[serde(alias = "NameLessThan", alias = "nameLessThan")]
    name_less_than: Option<String>,
    #[serde(alias = "Recursive", alias = "recursive")]
    _recursive: Option<String>,
    #[serde(alias = "StartIndex", alias = "startIndex")]
    start_index: Option<usize>,
    #[serde(alias = "Limit", alias = "limit")]
    limit: Option<usize>,
    #[serde(alias = "SortBy", alias = "sortBy")]
    sort_by: Option<String>,
    #[serde(alias = "SortOrder", alias = "sortOrder")]
    sort_order: Option<String>,
    #[serde(alias = "Fields", alias = "fields")]
    _fields: Option<String>,
    #[serde(alias = "ImageTypeLimit", alias = "imageTypeLimit")]
    _image_type_limit: Option<String>,
    #[serde(alias = "EnableImages", alias = "enableImages")]
    _enable_images: Option<String>,
    #[serde(alias = "EnableUserData", alias = "enableUserData")]
    _enable_user_data: Option<String>,
    #[serde(alias = "CollapseBoxSetItems", alias = "collapseBoxSetItems")]
    _collapse_box_set_items: Option<String>,
    #[serde(alias = "ImageTypes", alias = "imageTypes")]
    _image_types: Option<String>,
    #[serde(alias = "EnableImageTypes", alias = "enableImageTypes")]
    _enable_image_types: Option<String>,
    #[serde(alias = "EnableTotalRecordCount", alias = "enableTotalRecordCount")]
    _enable_total_record_count: Option<String>,
    #[serde(alias = "ExcludeLocationTypes", alias = "excludeLocationTypes")]
    _exclude_location_types: Option<String>,
}

fn parse_items_query(raw_query: Option<&str>) -> ItemsQuery {
    let mut query = ItemsQuery::default();
    let Some(raw_query) = raw_query else {
        return query;
    };

    for part in raw_query.split('&').filter(|part| !part.is_empty()) {
        let (key, value) = part.split_once('=').unwrap_or((part, ""));
        let key = percent_decode_query_component(key).to_ascii_lowercase();
        let value = percent_decode_query_component(value);
        match key.as_str() {
            "userid" => query.user_id = Some(value),
            "ids" => set_query_scalar(&mut query.ids, value),
            "parentid" => query.parent_id = Some(value),
            "includeitemtypes" => query.include_item_types.push(value),
            "excludeitemtypes" => query.exclude_item_types.push(value),
            "mediatypes" => query.media_types.push(value),
            "searchterm" => query.search_term = Some(value),
            "isplayed" => query.is_played = parse_query_bool(&value),
            "isfolder" => query.is_folder = parse_query_bool(&value),
            "filters" => query.filters.push(value),
            "namestartswith" => query.name_starts_with = Some(value),
            "namestartswithorgreater" => query.name_starts_with_or_greater = Some(value),
            "namelessthan" => query.name_less_than = Some(value),
            "startindex" => query.start_index = value.parse().ok(),
            "limit" => query.limit = value.parse().ok(),
            "sortby" => set_query_scalar(&mut query.sort_by, value),
            "sortorder" => set_query_scalar(&mut query.sort_order, value),
            _ => {}
        }
    }

    query
}

fn set_query_scalar(target: &mut Option<String>, value: String) {
    if let Some(existing) = target {
        existing.push(',');
        existing.push_str(&value);
    } else {
        *target = Some(value);
    }
}

fn parse_query_bool(value: &str) -> Option<bool> {
    match value.to_ascii_lowercase().as_str() {
        "true" => Some(true),
        "false" => Some(false),
        _ => None,
    }
}

fn percent_decode_query_component(value: &str) -> String {
    let bytes = value.as_bytes();
    let mut output = Vec::with_capacity(bytes.len());
    let mut index = 0;
    while index < bytes.len() {
        match bytes[index] {
            b'+' => {
                output.push(b' ');
                index += 1;
            }
            b'%' if index + 2 < bytes.len() => {
                if let (Some(high), Some(low)) =
                    (hex_value(bytes[index + 1]), hex_value(bytes[index + 2]))
                {
                    output.push((high << 4) | low);
                    index += 3;
                } else {
                    output.push(bytes[index]);
                    index += 1;
                }
            }
            byte => {
                output.push(byte);
                index += 1;
            }
        }
    }
    String::from_utf8_lossy(&output).into_owned()
}

fn hex_value(byte: u8) -> Option<u8> {
    match byte {
        b'0'..=b'9' => Some(byte - b'0'),
        b'a'..=b'f' => Some(byte - b'a' + 10),
        b'A'..=b'F' => Some(byte - b'A' + 10),
        _ => None,
    }
}

async fn items_result(
    State(state): State<AppState>,
    headers: HeaderMap,
    Query(auth_query): Query<AuthQuery>,
    RawQuery(raw_query): RawQuery,
) -> Result<Json<serde_json::Value>, ApiError> {
    let query = parse_items_query(raw_query.as_deref());
    let auth_user =
        require_request_user(&state.db, &headers, auth_query.api_key.as_deref()).await?;
    let requested_user_id = query.user_id.as_deref().map(resolve_user_id).transpose()?;
    if let Some(requested_user_id) = requested_user_id {
        ensure_user_access(&auth_user, requested_user_id)?;
    }
    let server_id = state.db.server_state().await?.server_id.to_string();
    let filtered_items = filtered_media_items(
        state.db.media_items().await?,
        &query,
        requested_user_id,
        &state.db,
    )
    .await?;
    let total_record_count = filtered_items.len();
    let items = items_to_json(
        &state.db,
        paged_media_items(filtered_items, &query),
        &server_id,
        requested_user_id,
    )
    .await?;
    Ok(Json(query_result_with_total(
        items,
        total_record_count,
        query.start_index.unwrap_or(0),
    )))
}

async fn user_items_result(
    State(state): State<AppState>,
    headers: HeaderMap,
    Query(auth_query): Query<AuthQuery>,
    Path(user_id): Path<String>,
    RawQuery(raw_query): RawQuery,
) -> Result<Json<serde_json::Value>, ApiError> {
    let query = parse_items_query(raw_query.as_deref());
    let auth_user =
        require_request_user(&state.db, &headers, auth_query.api_key.as_deref()).await?;
    let requested_user_id = resolve_user_id(&user_id)?;
    ensure_user_access(&auth_user, requested_user_id)?;
    let server_id = state.db.server_state().await?.server_id.to_string();
    let filtered_items = filtered_media_items(
        state.db.media_items().await?,
        &query,
        Some(requested_user_id),
        &state.db,
    )
    .await?;
    let total_record_count = filtered_items.len();
    let items = items_to_json(
        &state.db,
        paged_media_items(filtered_items, &query),
        &server_id,
        Some(requested_user_id),
    )
    .await?;
    Ok(Json(query_result_with_total(
        items,
        total_record_count,
        query.start_index.unwrap_or(0),
    )))
}

async fn item_counts(
    State(state): State<AppState>,
    headers: HeaderMap,
    Query(auth_query): Query<AuthQuery>,
    RawQuery(raw_query): RawQuery,
) -> Result<Json<serde_json::Value>, ApiError> {
    let query = parse_items_query(raw_query.as_deref());
    let auth_user =
        require_request_user(&state.db, &headers, auth_query.api_key.as_deref()).await?;
    let requested_user_id = query.user_id.as_deref().map(resolve_user_id).transpose()?;
    if let Some(requested_user_id) = requested_user_id {
        ensure_user_access(&auth_user, requested_user_id)?;
    }
    let filtered_items = filtered_media_items(
        state.db.media_items().await?,
        &query,
        requested_user_id,
        &state.db,
    )
    .await?;
    Ok(Json(item_counts_json(&filtered_items)))
}

async fn user_item_counts(
    State(state): State<AppState>,
    headers: HeaderMap,
    Query(auth_query): Query<AuthQuery>,
    Path(user_id): Path<String>,
    RawQuery(raw_query): RawQuery,
) -> Result<Json<serde_json::Value>, ApiError> {
    let query = parse_items_query(raw_query.as_deref());
    let auth_user =
        require_request_user(&state.db, &headers, auth_query.api_key.as_deref()).await?;
    let requested_user_id = resolve_user_id(&user_id)?;
    ensure_user_access(&auth_user, requested_user_id)?;
    let filtered_items = filtered_media_items(
        state.db.media_items().await?,
        &query,
        Some(requested_user_id),
        &state.db,
    )
    .await?;
    Ok(Json(item_counts_json(&filtered_items)))
}

async fn latest_items(
    State(state): State<AppState>,
    headers: HeaderMap,
    Query(auth_query): Query<AuthQuery>,
    RawQuery(raw_query): RawQuery,
) -> Result<Json<Vec<serde_json::Value>>, ApiError> {
    let query = parse_items_query(raw_query.as_deref());
    let auth_user =
        require_request_user(&state.db, &headers, auth_query.api_key.as_deref()).await?;
    let requested_user_id = query.user_id.as_deref().map(resolve_user_id).transpose()?;
    if let Some(requested_user_id) = requested_user_id {
        ensure_user_access(&auth_user, requested_user_id)?;
    }
    let server_id = state.db.server_state().await?.server_id.to_string();
    let mut filtered_items = filtered_media_items(
        state.db.media_items().await?,
        &query,
        requested_user_id,
        &state.db,
    )
    .await?;
    filtered_items.sort_by(|left, right| {
        compare_media_items(
            right,
            left,
            &[SortField::DateLastMediaAdded, SortField::SortName],
        )
    });
    let limit = query.limit.unwrap_or(20).min(100);
    Ok(Json(
        items_to_json(
            &state.db,
            filtered_items.into_iter().take(limit).collect(),
            &server_id,
            requested_user_id,
        )
        .await?,
    ))
}

async fn user_latest_items(
    State(state): State<AppState>,
    headers: HeaderMap,
    Query(auth_query): Query<AuthQuery>,
    Path(user_id): Path<String>,
    RawQuery(raw_query): RawQuery,
) -> Result<Json<Vec<serde_json::Value>>, ApiError> {
    let query = parse_items_query(raw_query.as_deref());
    let auth_user =
        require_request_user(&state.db, &headers, auth_query.api_key.as_deref()).await?;
    let requested_user_id = resolve_user_id(&user_id)?;
    ensure_user_access(&auth_user, requested_user_id)?;
    let server_id = state.db.server_state().await?.server_id.to_string();
    let mut filtered_items = filtered_media_items(
        state.db.media_items().await?,
        &query,
        Some(requested_user_id),
        &state.db,
    )
    .await?;
    filtered_items.sort_by(|left, right| {
        compare_media_items(
            right,
            left,
            &[SortField::DateLastMediaAdded, SortField::SortName],
        )
    });
    let limit = query.limit.unwrap_or(20).min(100);
    Ok(Json(
        items_to_json(
            &state.db,
            filtered_items.into_iter().take(limit).collect(),
            &server_id,
            Some(requested_user_id),
        )
        .await?,
    ))
}

async fn resume_items(
    State(state): State<AppState>,
    headers: HeaderMap,
    Query(query): Query<AuthQuery>,
) -> Result<Json<serde_json::Value>, ApiError> {
    let user = require_request_user(&state.db, &headers, query.api_key.as_deref()).await?;
    let server_id = state.db.server_state().await?.server_id.to_string();
    let items = state
        .db
        .resume_items_for_user(user.id, 20)
        .await?
        .iter()
        .map(|(item, playback)| media_item_to_json_with_playback(item, &server_id, Some(playback)))
        .collect();
    Ok(Json(query_result(items)))
}

async fn user_resume_items(
    State(state): State<AppState>,
    headers: HeaderMap,
    Query(query): Query<AuthQuery>,
    Path(user_id): Path<String>,
) -> Result<Json<serde_json::Value>, ApiError> {
    let auth_user = require_request_user(&state.db, &headers, query.api_key.as_deref()).await?;
    let requested_user_id = resolve_user_id(&user_id)?;
    ensure_user_access(&auth_user, requested_user_id)?;
    let server_id = state.db.server_state().await?.server_id.to_string();
    let items = state
        .db
        .resume_items_for_user(requested_user_id, 20)
        .await?
        .iter()
        .map(|(item, playback)| media_item_to_json_with_playback(item, &server_id, Some(playback)))
        .collect();
    Ok(Json(query_result(items)))
}

async fn item_detail(
    State(state): State<AppState>,
    headers: HeaderMap,
    Query(query): Query<AuthQuery>,
    Path(item_id): Path<String>,
) -> Result<Json<serde_json::Value>, ApiError> {
    require_request_user(&state.db, &headers, query.api_key.as_deref()).await?;
    let requested_id = parse_jellyfin_uuid(&item_id)?;
    let server_id = state.db.server_state().await?.server_id.to_string();
    if let Some(folder) = state
        .db
        .virtual_folders()
        .await?
        .into_iter()
        .find(|folder| folder.id == requested_id)
    {
        return Ok(Json(user_view_to_json(&folder, &server_id)));
    }

    let item = state
        .db
        .media_items()
        .await?
        .into_iter()
        .find(|item| item.id == requested_id)
        .ok_or_else(|| ApiError::not_found("Item not found"))?;
    Ok(Json(media_item_to_json(&item, &server_id)))
}

async fn user_item_detail(
    State(state): State<AppState>,
    headers: HeaderMap,
    Query(query): Query<AuthQuery>,
    Path((user_id, item_id)): Path<(String, String)>,
) -> Result<Json<serde_json::Value>, ApiError> {
    let auth_user = require_request_user(&state.db, &headers, query.api_key.as_deref()).await?;
    let user_id = resolve_user_id(&user_id)?;
    ensure_user_access(&auth_user, user_id)?;
    let item = media_item_by_id(&state.db, &item_id).await?;
    let playback = state.db.playback_state_for_item(user_id, item.id).await?;
    let server_id = state.db.server_state().await?.server_id.to_string();
    Ok(Json(media_item_to_json_with_playback(
        &item,
        &server_id,
        playback.as_ref(),
    )))
}

async fn mark_item_played(
    State(state): State<AppState>,
    headers: HeaderMap,
    Query(query): Query<AuthQuery>,
    Path((user_id, item_id)): Path<(String, String)>,
) -> Result<StatusCode, ApiError> {
    set_item_played(state, headers, query, user_id, item_id, true).await
}

async fn mark_item_unplayed(
    State(state): State<AppState>,
    headers: HeaderMap,
    Query(query): Query<AuthQuery>,
    Path((user_id, item_id)): Path<(String, String)>,
) -> Result<StatusCode, ApiError> {
    set_item_played(state, headers, query, user_id, item_id, false).await
}

async fn mark_authenticated_item_played(
    State(state): State<AppState>,
    headers: HeaderMap,
    Query(query): Query<AuthQuery>,
    Path(item_id): Path<String>,
) -> Result<StatusCode, ApiError> {
    let user = require_request_user(&state.db, &headers, query.api_key.as_deref()).await?;
    set_item_played(
        state,
        headers,
        query,
        user.id.simple().to_string(),
        item_id,
        true,
    )
    .await
}

async fn mark_authenticated_item_unplayed(
    State(state): State<AppState>,
    headers: HeaderMap,
    Query(query): Query<AuthQuery>,
    Path(item_id): Path<String>,
) -> Result<StatusCode, ApiError> {
    let user = require_request_user(&state.db, &headers, query.api_key.as_deref()).await?;
    set_item_played(
        state,
        headers,
        query,
        user.id.simple().to_string(),
        item_id,
        false,
    )
    .await
}

async fn set_item_played(
    state: AppState,
    headers: HeaderMap,
    query: AuthQuery,
    user_id: String,
    item_id: String,
    played: bool,
) -> Result<StatusCode, ApiError> {
    let auth_user = require_request_user(&state.db, &headers, query.api_key.as_deref()).await?;
    let user_id = resolve_user_id(&user_id)?;
    ensure_user_access(&auth_user, user_id)?;
    let item = media_item_by_id(&state.db, &item_id).await?;
    state
        .db
        .upsert_playback_state(UpsertPlaybackState {
            user_id,
            item_id: item.id,
            media_source_id: None,
            audio_stream_index: None,
            subtitle_stream_index: None,
            position_ticks: 0,
            is_paused: false,
            played,
        })
        .await?;
    Ok(StatusCode::NO_CONTENT)
}

fn resolve_user_id(user_id: &str) -> Result<Uuid, ApiError> {
    parse_jellyfin_uuid(user_id).map_err(|_| ApiError::bad_request("Invalid user id"))
}

async fn item_ancestors(
    State(state): State<AppState>,
    headers: HeaderMap,
    Query(query): Query<AuthQuery>,
    Path(item_id): Path<String>,
) -> Result<Json<Vec<serde_json::Value>>, ApiError> {
    require_request_user(&state.db, &headers, query.api_key.as_deref()).await?;
    let item = media_item_by_id(&state.db, &item_id).await?;
    let server_id = state.db.server_state().await?.server_id.to_string();
    let folder = state
        .db
        .virtual_folders()
        .await?
        .into_iter()
        .find(|folder| folder.id == item.virtual_folder_id)
        .ok_or_else(|| ApiError::not_found("Parent folder not found"))?;
    Ok(Json(vec![user_view_to_json(&folder, &server_id)]))
}

async fn item_playback_info(
    State(state): State<AppState>,
    headers: HeaderMap,
    Query(query): Query<AuthQuery>,
    RawQuery(raw_query): RawQuery,
    Path(item_id): Path<String>,
) -> Result<Json<serde_json::Value>, ApiError> {
    let (user, token) = require_user(&state.db, &headers, query.api_key.as_deref()).await?;
    let options = parse_playback_info_options(raw_query.as_deref(), None);
    playback_info_response(&state, &user, &token, &item_id, options).await
}

async fn post_item_playback_info(
    State(state): State<AppState>,
    headers: HeaderMap,
    Query(query): Query<AuthQuery>,
    RawQuery(raw_query): RawQuery,
    Path(item_id): Path<String>,
    body: Option<Json<serde_json::Value>>,
) -> Result<Json<serde_json::Value>, ApiError> {
    let (user, token) = require_user(&state.db, &headers, query.api_key.as_deref()).await?;
    let options =
        parse_playback_info_options(raw_query.as_deref(), body.as_ref().map(|body| &body.0));
    playback_info_response(&state, &user, &token, &item_id, options).await
}

async fn instant_mix_from_item(
    State(state): State<AppState>,
    headers: HeaderMap,
    Query(auth_query): Query<AuthQuery>,
    RawQuery(raw_query): RawQuery,
    Path(item_id): Path<String>,
) -> Result<Json<serde_json::Value>, ApiError> {
    let query = parse_items_query(raw_query.as_deref());
    let auth_user =
        require_request_user(&state.db, &headers, auth_query.api_key.as_deref()).await?;
    let requested_user_id = query.user_id.as_deref().map(resolve_user_id).transpose()?;
    if let Some(requested_user_id) = requested_user_id {
        ensure_user_access(&auth_user, requested_user_id)?;
    }

    let mix_items = audio_instant_mix_items(&state.db, &item_id).await?;
    if mix_items.is_empty() {
        return Ok(Json(query_result(Vec::new())));
    }

    let server_id = state.db.server_state().await?.server_id.to_string();
    let total_record_count = mix_items.len();
    let limit = query.limit.unwrap_or(usize::MAX);
    let items = items_to_json(
        &state.db,
        mix_items.into_iter().take(limit).collect(),
        &server_id,
        requested_user_id,
    )
    .await?;
    Ok(Json(query_result_with_total(items, total_record_count, 0)))
}

async fn audio_instant_mix_items(db: &Database, item_id: &str) -> Result<Vec<MediaItem>, ApiError> {
    let item = media_item_by_id(db, item_id).await?;
    if item.media_type != "Audio" {
        return Ok(Vec::new());
    }

    let mut mix_items = db
        .media_items()
        .await?
        .into_iter()
        .filter(|candidate| {
            candidate.media_type == "Audio" && candidate.virtual_folder_id == item.virtual_folder_id
        })
        .collect::<Vec<_>>();
    mix_items.sort_by(|left, right| compare_media_items(left, right, &[SortField::SortName]));
    mix_items.sort_by_key(|candidate| candidate.id != item.id);
    Ok(mix_items)
}

#[derive(Debug, Clone)]
struct PlaybackInfoOptions {
    enable_direct_play: bool,
    enable_direct_stream: bool,
    enable_transcoding: bool,
    media_source_id: Option<String>,
    audio_stream_index: Option<i64>,
    subtitle_stream_index: Option<i64>,
    start_position_ticks: i64,
    direct_play_profiles: Option<Vec<DirectPlayProfileMatcher>>,
}

#[derive(Debug, Clone)]
struct DirectPlayProfileMatcher {
    profile_type: Option<String>,
    containers: Vec<String>,
    video_codecs: Vec<String>,
    audio_codecs: Vec<String>,
}

impl Default for PlaybackInfoOptions {
    fn default() -> Self {
        Self {
            enable_direct_play: true,
            enable_direct_stream: true,
            enable_transcoding: true,
            media_source_id: None,
            audio_stream_index: None,
            subtitle_stream_index: None,
            start_position_ticks: 0,
            direct_play_profiles: None,
        }
    }
}

fn parse_playback_info_options(
    raw_query: Option<&str>,
    body: Option<&serde_json::Value>,
) -> PlaybackInfoOptions {
    let mut options = body
        .map(playback_info_options_from_body)
        .unwrap_or_default();
    let Some(raw_query) = raw_query else {
        return options;
    };

    for part in raw_query.split('&').filter(|part| !part.is_empty()) {
        let (key, value) = part.split_once('=').unwrap_or((part, ""));
        let key = percent_decode_query_component(key).to_ascii_lowercase();
        let value = percent_decode_query_component(value);
        match key.as_str() {
            "enabledirectplay" => {
                if let Some(value) = parse_query_bool(&value) {
                    options.enable_direct_play = value;
                }
            }
            "enabledirectstream" => {
                if let Some(value) = parse_query_bool(&value) {
                    options.enable_direct_stream = value;
                }
            }
            "enabletranscoding" => {
                if let Some(value) = parse_query_bool(&value) {
                    options.enable_transcoding = value;
                }
            }
            "mediasourceid" => {
                if !value.trim().is_empty() {
                    options.media_source_id = Some(value.trim().to_string());
                }
            }
            "audiostreamindex" => {
                if let Ok(value) = value.parse::<i64>() {
                    options.audio_stream_index = Some(value);
                }
            }
            "subtitlestreamindex" => {
                if let Ok(value) = value.parse::<i64>() {
                    options.subtitle_stream_index = Some(value);
                }
            }
            "starttimeticks" | "startpositionticks" => {
                if let Ok(value) = value.parse::<i64>() {
                    options.start_position_ticks = value.max(0);
                }
            }
            _ => {}
        }
    }

    options
}

fn playback_info_options_from_body(body: &serde_json::Value) -> PlaybackInfoOptions {
    let mut options = PlaybackInfoOptions::default();
    if let Some(value) = json_bool_field(body, "EnableDirectPlay") {
        options.enable_direct_play = value;
    }
    if let Some(value) = json_bool_field(body, "EnableDirectStream") {
        options.enable_direct_stream = value;
    }
    if let Some(value) = json_bool_field(body, "EnableTranscoding") {
        options.enable_transcoding = value;
    }
    options.media_source_id = json_string_field(body, "MediaSourceId");
    options.audio_stream_index = json_i64_field(body, "AudioStreamIndex");
    options.subtitle_stream_index = json_i64_field(body, "SubtitleStreamIndex");
    options.start_position_ticks = json_i64_field(body, "StartTimeTicks")
        .or_else(|| json_i64_field(body, "StartPositionTicks"))
        .unwrap_or_default()
        .max(0);
    options.direct_play_profiles = parse_direct_play_profiles(body);
    options
}

fn json_bool_field(value: &serde_json::Value, field: &str) -> Option<bool> {
    value.as_object()?.iter().find_map(|(key, value)| {
        if key.eq_ignore_ascii_case(field) {
            value.as_bool()
        } else {
            None
        }
    })
}

fn json_field_case_insensitive<'a>(
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

fn json_string_field(value: &serde_json::Value, field: &str) -> Option<String> {
    json_field_case_insensitive(value, field).and_then(|value| match value {
        serde_json::Value::String(value) if !value.trim().is_empty() => {
            Some(value.trim().to_string())
        }
        _ => None,
    })
}

fn json_i64_field(value: &serde_json::Value, field: &str) -> Option<i64> {
    json_field_case_insensitive(value, field).and_then(json_value_i64)
}

fn parse_direct_play_profiles(body: &serde_json::Value) -> Option<Vec<DirectPlayProfileMatcher>> {
    let device_profile = json_field_case_insensitive(body, "DeviceProfile")?;
    let profiles = json_field_case_insensitive(device_profile, "DirectPlayProfiles")?
        .as_array()?
        .iter()
        .filter_map(parse_direct_play_profile)
        .collect::<Vec<_>>();
    Some(profiles)
}

fn parse_direct_play_profile(profile: &serde_json::Value) -> Option<DirectPlayProfileMatcher> {
    if !profile.is_object() {
        return None;
    }

    Some(DirectPlayProfileMatcher {
        profile_type: json_string_field(profile, "Type"),
        containers: json_csv_field(profile, "Container"),
        video_codecs: json_csv_field(profile, "VideoCodec"),
        audio_codecs: json_csv_field(profile, "AudioCodec"),
    })
}

fn json_csv_field(value: &serde_json::Value, field: &str) -> Vec<String> {
    json_string_field(value, field)
        .map(|value| {
            value
                .split(',')
                .map(str::trim)
                .filter(|value| !value.is_empty())
                .map(|value| value.to_ascii_lowercase())
                .collect()
        })
        .unwrap_or_default()
}

async fn playback_info_response(
    state: &AppState,
    user: &User,
    token: &DeviceToken,
    item_id: &str,
    options: PlaybackInfoOptions,
) -> Result<Json<serde_json::Value>, ApiError> {
    let item = media_item_by_id(&state.db, item_id).await?;
    let server_id = state.db.server_state().await?.server_id.to_string();
    let item_json = media_item_to_json(&item, &server_id);
    let play_session_id = Uuid::new_v4().simple().to_string();
    let direct_play_supported = playback_direct_play_supported(&item, &options);
    let direct_stream_supported = options.enable_direct_stream || direct_play_supported;
    if !playback_selection_supported(&item, &options) {
        return Ok(Json(serde_json::json!({
            "MediaSources": [],
            "PlaySessionId": play_session_id,
            "ErrorCode": "NoCompatibleStream",
        })));
    }
    if !direct_play_supported && !direct_stream_supported {
        if options.enable_transcoding && item.media_type == "Video" {
            return playback_transcode_info_response(state, user, token, &item, item_json, options)
                .await;
        }
        return Ok(Json(serde_json::json!({
            "MediaSources": [],
            "PlaySessionId": play_session_id,
            "ErrorCode": "NoCompatibleStream",
        })));
    }

    let mut media_sources = item_json["MediaSources"].clone();
    if let Some(media_source) = media_sources
        .as_array_mut()
        .and_then(|sources| sources.first_mut())
        .and_then(serde_json::Value::as_object_mut)
    {
        media_source.insert(
            "SupportsDirectPlay".to_string(),
            serde_json::json!(direct_play_supported),
        );
        media_source.insert(
            "SupportsDirectStream".to_string(),
            serde_json::json!(direct_stream_supported),
        );
        media_source.insert("SupportsTranscoding".to_string(), serde_json::json!(false));
        apply_playback_stream_selection(media_source, &options);
    }

    Ok(Json(serde_json::json!({
        "MediaSources": media_sources,
        "PlaySessionId": play_session_id,
        "ErrorCode": null,
    })))
}

async fn playback_transcode_info_response(
    state: &AppState,
    user: &User,
    token: &DeviceToken,
    item: &MediaItem,
    item_json: serde_json::Value,
    options: PlaybackInfoOptions,
) -> Result<Json<serde_json::Value>, ApiError> {
    let play_session_id = Uuid::new_v4().simple().to_string();
    let item_id = item.id.simple().to_string();

    let streams = media_item_streams(item);
    let selection = TranscodeStreamSelection {
        video_stream_index: first_stream_index(&streams, "Video"),
        audio_stream_index: options
            .audio_stream_index
            .or_else(|| default_audio_stream_index(&streams)),
        subtitle_stream_index: options.subtitle_stream_index,
    };
    let dedupe_key =
        hls_transcode_dedupe_key(user.id, item, &selection, options.start_position_ticks);
    let dedupe_lock = transcode_dedupe_lock(&dedupe_key).await;
    let _dedupe_guard = dedupe_lock.lock().await;

    if let Some(session) = reusable_hls_transcode_session(&state.db, &dedupe_key).await? {
        return playback_transcode_session_info_response(
            item_json,
            &token.access_token,
            &item_id,
            &session.play_session_id,
            &options,
        );
    }

    let layout = HlsTranscodeLayout::new(transcode_temp_root(), &play_session_id);
    tokio::fs::create_dir_all(&layout.session_dir).await?;
    let mut request = HlsTranscodeRequest::new(
        item.path.clone(),
        layout.media_playlist_path.to_string_lossy().to_string(),
        layout.segment_pattern_string(),
        selection.clone(),
    );
    request.start_position_ticks = options.start_position_ticks;
    let command = build_hls_ffmpeg_command(&request);

    let (session, claimed_new_session) = state
        .db
        .claim_transcode_session(
            &dedupe_key,
            UpsertTranscodeSession {
                play_session_id: play_session_id.clone(),
                dedupe_key: Some(dedupe_key.clone()),
                device_id: Some(token.device_id.clone()),
                user_id: user.id,
                item_id: item.id,
                media_source_id: Some(item_id.clone()),
                audio_stream_index: selection.audio_stream_index,
                subtitle_stream_index: selection.subtitle_stream_index,
                video_stream_index: selection.video_stream_index,
                output_path: layout.media_playlist_path.to_string_lossy().to_string(),
                process_id: None,
                status: "starting".to_string(),
                progress_percent: None,
                position_ticks: request.start_position_ticks,
            },
        )
        .await?;
    let play_session_id = session.play_session_id;
    if claimed_new_session {
        spawn_hls_transcode_task(state.db.clone(), play_session_id.clone(), command).await;
    } else {
        cleanup_hls_transcode_dir(&layout.session_dir).await;
    }

    playback_transcode_session_info_response(
        item_json,
        &token.access_token,
        &item_id,
        &play_session_id,
        &options,
    )
}

fn playback_transcode_session_info_response(
    item_json: serde_json::Value,
    access_token: &str,
    item_id: &str,
    play_session_id: &str,
    options: &PlaybackInfoOptions,
) -> Result<Json<serde_json::Value>, ApiError> {
    let mut media_sources = item_json["MediaSources"].clone();
    if let Some(media_source) = media_sources
        .as_array_mut()
        .and_then(|sources| sources.first_mut())
        .and_then(serde_json::Value::as_object_mut)
    {
        media_source.insert("SupportsDirectPlay".to_string(), serde_json::json!(false));
        media_source.insert("SupportsDirectStream".to_string(), serde_json::json!(false));
        media_source.insert("SupportsTranscoding".to_string(), serde_json::json!(true));
        media_source.insert(
            "TranscodingSubProtocol".to_string(),
            serde_json::json!("hls"),
        );
        media_source.insert("TranscodingContainer".to_string(), serde_json::json!("ts"));
        media_source.insert("Container".to_string(), serde_json::json!("ts"));
        media_source.insert(
            "TranscodingUrl".to_string(),
            serde_json::json!(hls_master_url(item_id, play_session_id, access_token)),
        );
        media_source.insert("DirectStreamUrl".to_string(), serde_json::Value::Null);
        apply_playback_stream_selection(media_source, options);
    }

    Ok(Json(serde_json::json!({
        "MediaSources": media_sources,
        "PlaySessionId": play_session_id,
        "ErrorCode": null,
    })))
}

fn hls_transcode_dedupe_key(
    user_id: Uuid,
    item: &MediaItem,
    selection: &TranscodeStreamSelection,
    start_position_ticks: i64,
) -> String {
    format!(
        "hls:ts:{}:{}:{}:{}:{}:{}:{}",
        user_id.simple(),
        item.id.simple(),
        item.id.simple(),
        selection
            .video_stream_index
            .map_or_else(|| "-".to_string(), |index| index.to_string()),
        selection
            .audio_stream_index
            .map_or_else(|| "-".to_string(), |index| index.to_string()),
        selection
            .subtitle_stream_index
            .map_or_else(|| "-".to_string(), |index| index.to_string()),
        start_position_ticks.max(0)
    )
}

async fn transcode_dedupe_lock(key: &str) -> Arc<Mutex<()>> {
    let mut locks = transcode_dedupe_registry().lock().await;
    locks
        .entry(key.to_string())
        .or_insert_with(|| Arc::new(Mutex::new(())))
        .clone()
}

fn transcode_dedupe_registry() -> &'static Mutex<HashMap<String, Arc<Mutex<()>>>> {
    TRANSCODE_DEDUPE_LOCKS.get_or_init(|| Mutex::new(HashMap::new()))
}

async fn reusable_hls_transcode_session(
    db: &Database,
    dedupe_key: &str,
) -> Result<Option<TranscodeSession>, ApiError> {
    let sessions = db.transcode_sessions().await?;
    for session in sessions {
        if session.dedupe_key.as_deref() != Some(dedupe_key)
            || !matches!(
                session.status.as_str(),
                "starting" | "running" | "completed"
            )
        {
            continue;
        }
        if session.status == "completed"
            && !HlsTranscodeLayout::from_media_playlist_path(&session.output_path)
                .media_playlist_path
                .exists()
        {
            continue;
        }
        return Ok(Some(session));
    }
    Ok(None)
}

async fn spawn_hls_transcode_task(
    db: Database,
    play_session_id: String,
    command: jellyrin_core::FfmpegCommandSpec,
) {
    let (stop_tx, stop_rx) = oneshot::channel();
    if let Some(previous_stop) = transcode_stop_registry()
        .lock()
        .await
        .insert(play_session_id.clone(), stop_tx)
    {
        let _ = previous_stop.send(());
    }

    tokio::spawn(async move {
        let mut process = match spawn_transcode_process(&command) {
            Ok(process) => process,
            Err(error) => {
                let _ = db
                    .update_transcode_session_status(&play_session_id, "failed")
                    .await;
                tracing::error!(%play_session_id, %error, "failed to spawn HLS transcode process");
                return;
            }
        };

        let progress_rx = process.subscribe_progress();
        let process_id = process.process_id().map(i64::from);
        let mut runtime_ticks = None;
        match db
            .transcode_session_by_play_session_id(&play_session_id)
            .await
        {
            Ok(Some(session)) => {
                runtime_ticks = session.item.runtime_ticks;
                let _ = db
                    .upsert_transcode_session(UpsertTranscodeSession {
                        play_session_id: play_session_id.clone(),
                        dedupe_key: session.dedupe_key,
                        device_id: session.device_id,
                        user_id: session.user_id,
                        item_id: session.item.id,
                        media_source_id: session.media_source_id,
                        audio_stream_index: session.audio_stream_index,
                        subtitle_stream_index: session.subtitle_stream_index,
                        video_stream_index: session.video_stream_index,
                        output_path: session.output_path,
                        process_id,
                        status: "running".to_string(),
                        progress_percent: session.progress_percent,
                        position_ticks: session.position_ticks,
                    })
                    .await;
            }
            Ok(None) => {
                tracing::warn!(%play_session_id, "transcode session disappeared before process start");
            }
            Err(error) => {
                tracing::error!(%play_session_id, %error, "failed to load transcode session before process start");
            }
        }

        let progress_task = spawn_transcode_progress_persistence_task(
            db.clone(),
            play_session_id.clone(),
            progress_rx,
            runtime_ticks,
        );

        let mut stopped = false;
        let exit = tokio::select! {
            exit = process.wait() => exit,
            _ = stop_rx => {
                stopped = true;
                process.stop().await
            }
        };
        let final_status = match exit {
            Ok(_) if stopped => "stopped",
            Ok(exit) if exit.success => "completed",
            Ok(_) => "failed",
            Err(error) => {
                tracing::error!(%play_session_id, %error, "HLS transcode process wait failed");
                "failed"
            }
        };
        drop(process);
        let _ = progress_task.await;
        let session = db
            .transcode_session_by_play_session_id(&play_session_id)
            .await
            .ok()
            .flatten();
        let _ = db
            .update_transcode_session_status(&play_session_id, final_status)
            .await;
        transcode_stop_registry()
            .lock()
            .await
            .remove(&play_session_id);
        if stopped && let Some(session) = session {
            cleanup_hls_transcode_files(&session.output_path).await;
        }
    });
}

fn spawn_transcode_progress_persistence_task(
    db: Database,
    play_session_id: String,
    mut progress_rx: tokio::sync::broadcast::Receiver<jellyrin_core::FfmpegProgress>,
    runtime_ticks: Option<i64>,
) -> tokio::task::JoinHandle<()> {
    tokio::spawn(async move {
        let mut last_position_ticks = None;
        while let Ok(progress) = progress_rx.recv().await {
            let Some(position_ticks) = progress.position_ticks() else {
                continue;
            };
            if last_position_ticks == Some(position_ticks) {
                continue;
            }
            last_position_ticks = Some(position_ticks);
            let progress_percent = transcode_progress_percent(position_ticks, runtime_ticks);
            if let Err(error) = db
                .update_transcode_session_progress(
                    &play_session_id,
                    progress_percent,
                    position_ticks,
                )
                .await
            {
                tracing::warn!(%play_session_id, %error, "failed to persist HLS transcode progress");
            }
        }
    })
}

fn transcode_progress_percent(position_ticks: i64, runtime_ticks: Option<i64>) -> Option<f64> {
    let runtime_ticks = runtime_ticks?;
    if runtime_ticks <= 0 || position_ticks < 0 {
        return None;
    }
    Some(((position_ticks as f64 / runtime_ticks as f64) * 100.0).clamp(0.0, 100.0))
}

fn transcode_stop_registry() -> &'static Mutex<HashMap<String, oneshot::Sender<()>>> {
    TRANSCODE_STOPS.get_or_init(|| Mutex::new(HashMap::new()))
}

fn playback_selection_supported(item: &MediaItem, options: &PlaybackInfoOptions) -> bool {
    if let Some(media_source_id) = options.media_source_id.as_deref()
        && parse_jellyfin_uuid(media_source_id).ok() != Some(item.id)
    {
        return false;
    }

    if let Some(audio_stream_index) = options.audio_stream_index
        && !media_item_has_stream_index(item, "Audio", audio_stream_index)
    {
        return false;
    }

    if let Some(subtitle_stream_index) = options.subtitle_stream_index {
        if subtitle_stream_index < 0 {
            return true;
        }
        if !media_item_has_stream_index(item, "Subtitle", subtitle_stream_index) {
            return false;
        }
    }

    true
}

fn apply_playback_stream_selection(
    media_source: &mut serde_json::Map<String, serde_json::Value>,
    options: &PlaybackInfoOptions,
) {
    if let Some(audio_stream_index) = options.audio_stream_index {
        media_source.insert(
            "DefaultAudioStreamIndex".to_string(),
            serde_json::json!(audio_stream_index),
        );
    }

    if let Some(subtitle_stream_index) = options.subtitle_stream_index {
        let value = if subtitle_stream_index < 0 {
            serde_json::Value::Null
        } else {
            serde_json::json!(subtitle_stream_index)
        };
        media_source.insert("DefaultSubtitleStreamIndex".to_string(), value);
    }
}

fn media_item_has_stream_index(item: &MediaItem, stream_type: &str, index: i64) -> bool {
    media_item_streams(item).iter().any(|stream| {
        let Some(stream) = stream.as_object() else {
            return false;
        };
        let Some(type_name) = stream.get("Type").and_then(serde_json::Value::as_str) else {
            return false;
        };
        if !type_name.eq_ignore_ascii_case(stream_type) {
            return false;
        }

        stream
            .get("Index")
            .and_then(json_value_i64)
            .is_some_and(|stream_index| stream_index == index)
    })
}

fn playback_direct_play_supported(item: &MediaItem, options: &PlaybackInfoOptions) -> bool {
    if !options.enable_direct_play {
        return false;
    }

    let Some(profiles) = options.direct_play_profiles.as_ref() else {
        return true;
    };

    profiles
        .iter()
        .any(|profile| direct_play_profile_matches(item, profile))
}

fn direct_play_profile_matches(item: &MediaItem, profile: &DirectPlayProfileMatcher) -> bool {
    if let Some(profile_type) = profile.profile_type.as_deref()
        && !profile_type.eq_ignore_ascii_case(item.media_type.as_str())
    {
        return false;
    }

    let Some(container) = media_item_container(item) else {
        return false;
    };
    if !profile.containers.is_empty()
        && !profile
            .containers
            .iter()
            .any(|candidate| candidate.eq_ignore_ascii_case(&container))
    {
        return false;
    }

    if !profile.video_codecs.is_empty() {
        let Some(video_codec) = media_item_stream_codec(item, "Video") else {
            return false;
        };
        if !profile
            .video_codecs
            .iter()
            .any(|candidate| candidate.eq_ignore_ascii_case(&video_codec))
        {
            return false;
        }
    }

    if !profile.audio_codecs.is_empty() {
        let Some(audio_codec) = media_item_stream_codec(item, "Audio") else {
            return false;
        };
        if !profile
            .audio_codecs
            .iter()
            .any(|candidate| candidate.eq_ignore_ascii_case(&audio_codec))
        {
            return false;
        }
    }

    true
}

fn media_item_stream_codec(item: &MediaItem, stream_type: &str) -> Option<String> {
    item.media_streams.iter().find_map(|stream| {
        let stream = stream.as_object()?;
        let type_matches = stream.get("Type").and_then(serde_json::Value::as_str)?;
        if !type_matches.eq_ignore_ascii_case(stream_type) {
            return None;
        }
        stream
            .get("Codec")
            .and_then(serde_json::Value::as_str)
            .filter(|codec| !codec.trim().is_empty())
            .map(|codec| codec.trim().to_string())
    })
}

async fn direct_stream_item(
    State(state): State<AppState>,
    Path(item_id): Path<String>,
    headers: HeaderMap,
    Query(query): Query<StreamQuery>,
) -> Result<axum::response::Response, ApiError> {
    direct_stream_media(
        &state,
        &headers,
        query.api_key.as_deref(),
        &item_id,
        "Video",
        true,
    )
    .await
}

async fn direct_stream_item_head(
    State(state): State<AppState>,
    Path(item_id): Path<String>,
    headers: HeaderMap,
    Query(query): Query<StreamQuery>,
) -> Result<axum::response::Response, ApiError> {
    direct_stream_media(
        &state,
        &headers,
        query.api_key.as_deref(),
        &item_id,
        "Video",
        false,
    )
    .await
}

async fn hls_master_playlist(
    State(state): State<AppState>,
    Path(item_id): Path<String>,
    headers: HeaderMap,
    RawQuery(raw_query): RawQuery,
    Query(query): Query<HlsQuery>,
) -> Result<axum::response::Response, ApiError> {
    hls_master_playlist_response(
        &state,
        &headers,
        &item_id,
        query,
        raw_query.as_deref(),
        true,
    )
    .await
}

async fn hls_master_playlist_head(
    State(state): State<AppState>,
    Path(item_id): Path<String>,
    headers: HeaderMap,
    RawQuery(raw_query): RawQuery,
    Query(query): Query<HlsQuery>,
) -> Result<axum::response::Response, ApiError> {
    hls_master_playlist_response(
        &state,
        &headers,
        &item_id,
        query,
        raw_query.as_deref(),
        false,
    )
    .await
}

async fn hls_media_playlist(
    State(state): State<AppState>,
    Path(item_id): Path<String>,
    headers: HeaderMap,
    RawQuery(raw_query): RawQuery,
    Query(query): Query<HlsQuery>,
) -> Result<axum::response::Response, ApiError> {
    hls_media_playlist_response(
        &state,
        &headers,
        &item_id,
        query,
        raw_query.as_deref(),
        true,
    )
    .await
}

async fn hls_media_playlist_head(
    State(state): State<AppState>,
    Path(item_id): Path<String>,
    headers: HeaderMap,
    RawQuery(raw_query): RawQuery,
    Query(query): Query<HlsQuery>,
) -> Result<axum::response::Response, ApiError> {
    hls_media_playlist_response(
        &state,
        &headers,
        &item_id,
        query,
        raw_query.as_deref(),
        false,
    )
    .await
}

async fn hls_legacy_media_playlist(
    State(state): State<AppState>,
    Path((item_id, _playlist_id)): Path<(String, String)>,
    headers: HeaderMap,
    RawQuery(raw_query): RawQuery,
    Query(query): Query<HlsQuery>,
) -> Result<axum::response::Response, ApiError> {
    hls_media_playlist_response(
        &state,
        &headers,
        &item_id,
        query,
        raw_query.as_deref(),
        true,
    )
    .await
}

async fn hls_segment(
    State(state): State<AppState>,
    Path((item_id, playlist_id, segment_file)): Path<(String, String, String)>,
    headers: HeaderMap,
    Query(query): Query<HlsQuery>,
) -> Result<axum::response::Response, ApiError> {
    let (segment_id, container) = parse_hls_segment_file(&segment_file)?;
    hls_segment_response(
        &state,
        &headers,
        &item_id,
        HlsSegmentRequest {
            playlist_id: &playlist_id,
            segment_id,
            container,
        },
        query,
        true,
    )
    .await
}

async fn hls_segment_head(
    State(state): State<AppState>,
    Path((item_id, playlist_id, segment_file)): Path<(String, String, String)>,
    headers: HeaderMap,
    Query(query): Query<HlsQuery>,
) -> Result<axum::response::Response, ApiError> {
    let (segment_id, container) = parse_hls_segment_file(&segment_file)?;
    hls_segment_response(
        &state,
        &headers,
        &item_id,
        HlsSegmentRequest {
            playlist_id: &playlist_id,
            segment_id,
            container,
        },
        query,
        false,
    )
    .await
}

async fn direct_stream_item_by_container(
    State(state): State<AppState>,
    Path((item_id, _container)): Path<(String, String)>,
    headers: HeaderMap,
    Query(query): Query<StreamQuery>,
) -> Result<axum::response::Response, ApiError> {
    direct_stream_media(
        &state,
        &headers,
        query.api_key.as_deref(),
        &item_id,
        "Video",
        true,
    )
    .await
}

async fn direct_stream_item_by_container_head(
    State(state): State<AppState>,
    Path((item_id, _container)): Path<(String, String)>,
    headers: HeaderMap,
    Query(query): Query<StreamQuery>,
) -> Result<axum::response::Response, ApiError> {
    direct_stream_media(
        &state,
        &headers,
        query.api_key.as_deref(),
        &item_id,
        "Video",
        false,
    )
    .await
}

#[derive(Debug, Deserialize)]
struct MergeVersionsBody {
    #[serde(default, alias = "Ids", alias = "ItemIds", alias = "ItemIdList")]
    ids: Vec<String>,
}

async fn merge_video_versions(
    State(state): State<AppState>,
    headers: HeaderMap,
    Query(query): Query<AuthQuery>,
    body: Option<Json<MergeVersionsBody>>,
) -> Result<StatusCode, ApiError> {
    require_request_user(&state.db, &headers, query.api_key.as_deref()).await?;
    if let Some(Json(body)) = body {
        for item_id in body.ids {
            let item = media_item_by_id(&state.db, &item_id).await?;
            if item.media_type != "Video" {
                return Err(ApiError::bad_request(
                    "MergeVersions only supports video items",
                ));
            }
        }
    }
    Ok(StatusCode::NO_CONTENT)
}

#[derive(Debug, Deserialize)]
struct OpenLiveStreamBody {
    #[serde(alias = "ItemId")]
    item_id: Option<String>,
    #[serde(alias = "OpenToken")]
    open_token: Option<String>,
    #[serde(alias = "MediaSourceId")]
    media_source_id: Option<String>,
}

#[derive(Debug, Deserialize)]
struct CloseLiveStreamBody {
    #[serde(alias = "LiveStreamId")]
    live_stream_id: Option<String>,
    #[serde(alias = "MediaSourceId")]
    media_source_id: Option<String>,
}

async fn open_live_stream(
    State(state): State<AppState>,
    headers: HeaderMap,
    Query(query): Query<AuthQuery>,
    body: Option<Json<OpenLiveStreamBody>>,
) -> Result<Json<serde_json::Value>, ApiError> {
    require_request_user(&state.db, &headers, query.api_key.as_deref()).await?;
    let body = body.map(|body| body.0).unwrap_or(OpenLiveStreamBody {
        item_id: None,
        open_token: None,
        media_source_id: None,
    });
    let item_id = body
        .item_id
        .or(body.media_source_id)
        .or(body.open_token)
        .ok_or_else(|| ApiError::bad_request("ItemId is required"))?;
    let item = media_item_by_id(&state.db, &item_id).await?;
    let server_id = state.db.server_state().await?.server_id.to_string();
    let item_json = media_item_to_json(&item, &server_id);
    let media_source = item_json["MediaSources"]
        .as_array()
        .and_then(|sources| sources.first())
        .cloned()
        .ok_or_else(|| ApiError::not_found("Media source not found"))?;
    let live_stream_id = item.id.simple().to_string();
    Ok(Json(serde_json::json!({
        "MediaSource": media_source,
        "LiveStreamId": live_stream_id,
        "MediaSourceId": live_stream_id,
    })))
}

async fn close_live_stream(
    State(state): State<AppState>,
    headers: HeaderMap,
    Query(query): Query<AuthQuery>,
    body: Option<Json<CloseLiveStreamBody>>,
) -> Result<StatusCode, ApiError> {
    require_request_user(&state.db, &headers, query.api_key.as_deref()).await?;
    if let Some(Json(body)) = body
        && let Some(item_id) = body.live_stream_id.or(body.media_source_id)
    {
        media_item_by_id(&state.db, &item_id).await?;
    }
    Ok(StatusCode::NO_CONTENT)
}

async fn video_additional_parts(
    State(state): State<AppState>,
    headers: HeaderMap,
    Query(query): Query<AuthQuery>,
    Path(item_id): Path<String>,
) -> Result<Json<serde_json::Value>, ApiError> {
    require_request_user(&state.db, &headers, query.api_key.as_deref()).await?;
    let item = media_item_by_id(&state.db, &item_id).await?;
    if item.media_type != "Video" {
        return Err(ApiError::not_found("Video item not found"));
    }
    Ok(empty_items_result().await)
}

async fn delete_video_alternate_sources(
    State(state): State<AppState>,
    headers: HeaderMap,
    Query(query): Query<AuthQuery>,
    Path(item_id): Path<String>,
) -> Result<StatusCode, ApiError> {
    require_request_user(&state.db, &headers, query.api_key.as_deref()).await?;
    let item = media_item_by_id(&state.db, &item_id).await?;
    if item.media_type != "Video" {
        return Err(ApiError::not_found("Video item not found"));
    }
    Ok(StatusCode::NO_CONTENT)
}

async fn direct_stream_audio(
    State(state): State<AppState>,
    Path(item_id): Path<String>,
    headers: HeaderMap,
    Query(query): Query<StreamQuery>,
) -> Result<axum::response::Response, ApiError> {
    direct_stream_media(
        &state,
        &headers,
        query.api_key.as_deref(),
        &item_id,
        "Audio",
        true,
    )
    .await
}

async fn direct_stream_audio_head(
    State(state): State<AppState>,
    Path(item_id): Path<String>,
    headers: HeaderMap,
    Query(query): Query<StreamQuery>,
) -> Result<axum::response::Response, ApiError> {
    direct_stream_media(
        &state,
        &headers,
        query.api_key.as_deref(),
        &item_id,
        "Audio",
        false,
    )
    .await
}

async fn universal_audio(
    State(state): State<AppState>,
    Path(item_id): Path<String>,
    headers: HeaderMap,
    Query(query): Query<StreamQuery>,
) -> Result<axum::response::Response, ApiError> {
    direct_stream_media(
        &state,
        &headers,
        query.api_key.as_deref(),
        &item_id,
        "Audio",
        true,
    )
    .await
}

async fn universal_audio_head(
    State(state): State<AppState>,
    Path(item_id): Path<String>,
    headers: HeaderMap,
    Query(query): Query<StreamQuery>,
) -> Result<axum::response::Response, ApiError> {
    direct_stream_media(
        &state,
        &headers,
        query.api_key.as_deref(),
        &item_id,
        "Audio",
        false,
    )
    .await
}

async fn direct_stream_audio_by_container(
    State(state): State<AppState>,
    Path((item_id, _container)): Path<(String, String)>,
    headers: HeaderMap,
    Query(query): Query<StreamQuery>,
) -> Result<axum::response::Response, ApiError> {
    direct_stream_media(
        &state,
        &headers,
        query.api_key.as_deref(),
        &item_id,
        "Audio",
        true,
    )
    .await
}

async fn direct_stream_audio_by_container_head(
    State(state): State<AppState>,
    Path((item_id, _container)): Path<(String, String)>,
    headers: HeaderMap,
    Query(query): Query<StreamQuery>,
) -> Result<axum::response::Response, ApiError> {
    direct_stream_media(
        &state,
        &headers,
        query.api_key.as_deref(),
        &item_id,
        "Audio",
        false,
    )
    .await
}

async fn hls_master_playlist_response(
    state: &AppState,
    headers: &HeaderMap,
    item_id: &str,
    query: HlsQuery,
    raw_query: Option<&str>,
    include_body: bool,
) -> Result<axum::response::Response, ApiError> {
    let session = active_hls_transcode_session(state, headers, item_id, &query).await?;
    let playlist = render_hls_master_playlist(&HlsVariantInfo {
        uri: append_query(HLS_MEDIA_PLAYLIST_NAME, raw_query),
        bandwidth: session
            .item
            .bitrate
            .and_then(positive_u32)
            .unwrap_or(1_000_000),
        resolution: session
            .item
            .width
            .and_then(|value| positive_u32(i64::from(value)))
            .zip(
                session
                    .item
                    .height
                    .and_then(|value| positive_u32(i64::from(value))),
            ),
        codecs: hls_codecs(&session.item),
    });
    playlist_response(playlist, include_body)
}

async fn hls_media_playlist_response(
    state: &AppState,
    headers: &HeaderMap,
    item_id: &str,
    query: HlsQuery,
    raw_query: Option<&str>,
    include_body: bool,
) -> Result<axum::response::Response, ApiError> {
    let session = active_hls_transcode_session(state, headers, item_id, &query).await?;
    let layout = HlsTranscodeLayout::from_media_playlist_path(&session.output_path);
    let ready = wait_for_hls_readiness(
        &layout.media_playlist_path,
        layout.segment_path(0),
        std::time::Duration::from_secs(5),
    )
    .await?;
    if !ready {
        return Err(ApiError::not_found("HLS playlist is not ready"));
    }

    let playlist = tokio::fs::read_to_string(&layout.media_playlist_path).await?;
    let playlist = rewrite_hls_media_playlist(&playlist, item_id, raw_query);
    playlist_response(playlist, include_body)
}

struct HlsSegmentRequest<'a> {
    playlist_id: &'a str,
    segment_id: i64,
    container: &'a str,
}

async fn hls_segment_response(
    state: &AppState,
    headers: &HeaderMap,
    item_id: &str,
    segment: HlsSegmentRequest<'_>,
    query: HlsQuery,
    include_body: bool,
) -> Result<axum::response::Response, ApiError> {
    if segment.playlist_id != "main"
        || !segment.container.eq_ignore_ascii_case("ts")
        || segment.segment_id < 0
    {
        return Err(ApiError::not_found("HLS segment not found"));
    }

    let session = active_hls_transcode_session(state, headers, item_id, &query).await?;
    let layout = HlsTranscodeLayout::from_media_playlist_path(&session.output_path);
    let segment_id = u32::try_from(segment.segment_id)
        .map_err(|_| ApiError::not_found("HLS segment not found"))?;
    stream_path(
        layout.segment_path(segment_id),
        "video/mp2t".to_string(),
        headers,
        include_body,
    )
    .await
}

async fn active_hls_transcode_session(
    state: &AppState,
    headers: &HeaderMap,
    item_id: &str,
    query: &HlsQuery,
) -> Result<TranscodeSession, ApiError> {
    require_request_user(&state.db, headers, query.api_key.as_deref()).await?;
    let requested_item_id = parse_jellyfin_uuid(item_id)?;
    let play_session_id = query
        .play_session_id
        .as_deref()
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .ok_or_else(|| ApiError::bad_request("PlaySessionId is required"))?;
    let session = state
        .db
        .transcode_session_by_play_session_id(play_session_id)
        .await?
        .ok_or_else(|| ApiError::not_found("HLS transcode session not found"))?;
    if session.item.id != requested_item_id
        || session.item.media_type != "Video"
        || !matches!(
            session.status.as_str(),
            "starting" | "running" | "completed"
        )
    {
        return Err(ApiError::not_found("HLS transcode session not found"));
    }
    Ok(session)
}

fn playlist_response(
    playlist: String,
    include_body: bool,
) -> Result<axum::response::Response, ApiError> {
    let content_length = playlist.len().to_string();
    let body = if include_body {
        Body::from(playlist)
    } else {
        Body::empty()
    };
    Ok((
        StatusCode::OK,
        [
            (
                header::CONTENT_TYPE,
                "application/vnd.apple.mpegurl".to_string(),
            ),
            (header::CONTENT_LENGTH, content_length),
        ],
        body,
    )
        .into_response())
}

async fn stream_path(
    path: PathBuf,
    content_type: String,
    headers: &HeaderMap,
    include_body: bool,
) -> Result<axum::response::Response, ApiError> {
    let mut file = tokio::fs::File::open(path).await?;
    let total_len = file.metadata().await?.len();
    let range = match parse_range_header(headers, total_len) {
        Ok(range) => range,
        Err(()) => {
            return Ok((
                StatusCode::RANGE_NOT_SATISFIABLE,
                [
                    (header::CONTENT_TYPE, content_type),
                    (header::ACCEPT_RANGES, "bytes".to_string()),
                    (header::CONTENT_RANGE, format!("bytes */{total_len}")),
                ],
                Body::empty(),
            )
                .into_response());
        }
    };

    if let Some((start, end)) = range {
        file.seek(std::io::SeekFrom::Start(start)).await?;
        let content_length = end - start + 1;
        let stream = ReaderStream::new(file.take(content_length));
        let body = if include_body {
            Body::from_stream(stream)
        } else {
            Body::empty()
        };
        return Ok((
            StatusCode::PARTIAL_CONTENT,
            [
                (header::CONTENT_TYPE, content_type),
                (header::ACCEPT_RANGES, "bytes".to_string()),
                (header::CONTENT_LENGTH, content_length.to_string()),
                (
                    header::CONTENT_RANGE,
                    format!("bytes {start}-{end}/{total_len}"),
                ),
            ],
            body,
        )
            .into_response());
    }

    let stream = ReaderStream::new(file);
    let body = if include_body {
        Body::from_stream(stream)
    } else {
        Body::empty()
    };
    Ok((
        StatusCode::OK,
        [
            (header::CONTENT_TYPE, content_type),
            (header::ACCEPT_RANGES, "bytes".to_string()),
            (header::CONTENT_LENGTH, total_len.to_string()),
        ],
        body,
    )
        .into_response())
}

async fn direct_stream_media(
    state: &AppState,
    headers: &HeaderMap,
    api_key: Option<&str>,
    item_id: &str,
    media_type: &str,
    include_body: bool,
) -> Result<axum::response::Response, ApiError> {
    require_request_user(&state.db, headers, api_key).await?;
    let item = media_item_by_id(&state.db, item_id).await?;
    if item.media_type != media_type {
        return Err(ApiError::not_found(format!(
            "{media_type} stream not found"
        )));
    }

    stream_media_item(item, headers, include_body).await
}

async fn stream_media_item(
    item: MediaItem,
    headers: &HeaderMap,
    include_body: bool,
) -> Result<axum::response::Response, ApiError> {
    let mut file = tokio::fs::File::open(media_item_path(&item)).await?;
    let total_len = file.metadata().await?.len();
    let content_type = media_item_content_type(&item);

    let range = match parse_range_header(headers, total_len) {
        Ok(range) => range,
        Err(()) => {
            return Ok((
                StatusCode::RANGE_NOT_SATISFIABLE,
                [
                    (header::CONTENT_TYPE, content_type),
                    (header::ACCEPT_RANGES, "bytes".to_string()),
                    (header::CONTENT_RANGE, format!("bytes */{total_len}")),
                ],
                Body::empty(),
            )
                .into_response());
        }
    };

    if let Some((start, end)) = range {
        file.seek(std::io::SeekFrom::Start(start)).await?;
        let content_length = end - start + 1;
        let stream = ReaderStream::new(file.take(content_length));
        let body = if include_body {
            Body::from_stream(stream)
        } else {
            Body::empty()
        };
        return Ok((
            StatusCode::PARTIAL_CONTENT,
            [
                (header::CONTENT_TYPE, content_type),
                (header::ACCEPT_RANGES, "bytes".to_string()),
                (header::CONTENT_LENGTH, content_length.to_string()),
                (
                    header::CONTENT_RANGE,
                    format!("bytes {start}-{end}/{total_len}"),
                ),
            ],
            body,
        )
            .into_response());
    }

    let stream = ReaderStream::new(file);
    let body = if include_body {
        Body::from_stream(stream)
    } else {
        Body::empty()
    };
    Ok((
        StatusCode::OK,
        [
            (header::CONTENT_TYPE, content_type),
            (header::ACCEPT_RANGES, "bytes".to_string()),
            (header::CONTENT_LENGTH, total_len.to_string()),
        ],
        body,
    )
        .into_response())
}

async fn media_item_by_id(db: &Database, item_id: &str) -> Result<MediaItem, ApiError> {
    let requested_id = parse_jellyfin_uuid(item_id)?;
    db.media_items()
        .await?
        .into_iter()
        .find(|item| item.id == requested_id)
        .ok_or_else(|| ApiError::not_found("Item not found"))
}

async fn empty_items_result() -> Json<serde_json::Value> {
    Json(query_result(Vec::new()))
}

async fn authenticated_empty_items(
    State(state): State<AppState>,
    headers: HeaderMap,
    Query(query): Query<AuthQuery>,
) -> Result<Json<serde_json::Value>, ApiError> {
    require_request_user(&state.db, &headers, query.api_key.as_deref()).await?;
    Ok(empty_items_result().await)
}

#[derive(Debug, Deserialize)]
struct ChannelsQuery {
    #[serde(flatten)]
    auth: AuthQuery,
    #[serde(alias = "UserId")]
    _user_id: Option<String>,
    #[serde(alias = "startIndex", alias = "StartIndex")]
    start_index: Option<usize>,
    #[serde(alias = "limit", alias = "Limit")]
    _limit: Option<usize>,
    #[serde(alias = "supportsLatestItems", alias = "SupportsLatestItems")]
    _supports_latest_items: Option<bool>,
    #[serde(alias = "supportsMediaDeletion", alias = "SupportsMediaDeletion")]
    _supports_media_deletion: Option<bool>,
    #[serde(alias = "isFavorite", alias = "IsFavorite")]
    _is_favorite: Option<bool>,
}

async fn channels(
    State(state): State<AppState>,
    headers: HeaderMap,
    Query(query): Query<ChannelsQuery>,
) -> Result<Json<serde_json::Value>, ApiError> {
    require_request_user(&state.db, &headers, query.auth.api_key.as_deref()).await?;
    Ok(Json(query_result_with_total(
        Vec::new(),
        0,
        query.start_index.unwrap_or(0),
    )))
}

async fn authenticated_item_empty_items(
    State(state): State<AppState>,
    headers: HeaderMap,
    Query(query): Query<AuthQuery>,
    Path(item_id): Path<String>,
) -> Result<Json<serde_json::Value>, ApiError> {
    require_request_user(&state.db, &headers, query.api_key.as_deref()).await?;
    media_item_by_id(&state.db, &item_id).await?;
    Ok(empty_items_result().await)
}

async fn user_item_empty_items(
    State(state): State<AppState>,
    headers: HeaderMap,
    Query(query): Query<AuthQuery>,
    Path((user_id, item_id)): Path<(String, String)>,
) -> Result<Json<serde_json::Value>, ApiError> {
    let auth_user = require_request_user(&state.db, &headers, query.api_key.as_deref()).await?;
    let user_id = resolve_user_id(&user_id)?;
    ensure_user_access(&auth_user, user_id)?;
    media_item_by_id(&state.db, &item_id).await?;
    Ok(empty_items_result().await)
}

async fn authenticated_empty_json_array(
    State(state): State<AppState>,
    headers: HeaderMap,
    Query(query): Query<AuthQuery>,
) -> Result<Json<Vec<serde_json::Value>>, ApiError> {
    require_request_user(&state.db, &headers, query.api_key.as_deref()).await?;
    Ok(empty_json_array().await)
}

async fn active_encodings(
    State(state): State<AppState>,
    headers: HeaderMap,
    Query(query): Query<AuthQuery>,
) -> Result<Json<Vec<serde_json::Value>>, ApiError> {
    require_request_user(&state.db, &headers, query.api_key.as_deref()).await?;
    let sessions = state.db.active_transcode_sessions().await?;
    Ok(Json(sessions.iter().map(transcode_session_json).collect()))
}

async fn stop_active_encoding(
    State(state): State<AppState>,
    headers: HeaderMap,
    Query(query): Query<StopEncodingQuery>,
) -> Result<StatusCode, ApiError> {
    let user = require_request_user(&state.db, &headers, query.api_key.as_deref()).await?;
    let play_session_id = query
        .play_session_id
        .as_deref()
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .ok_or_else(|| ApiError::bad_request("PlaySessionId is required"))?;
    let session = state
        .db
        .transcode_session_by_play_session_id(play_session_id)
        .await?
        .ok_or_else(|| ApiError::not_found("Transcode session not found"))?;
    if session.user_id != user.id && !user.is_administrator {
        return Err(ApiError::forbidden("Transcode session access denied"));
    }
    if let Some(query_device_id) = query
        .device_id
        .as_deref()
        .map(str::trim)
        .filter(|value| !value.is_empty())
        && session
            .device_id
            .as_deref()
            .is_some_and(|session_device_id| session_device_id != query_device_id)
    {
        return Err(ApiError::forbidden("Transcode session device mismatch"));
    }

    let already_terminal = matches!(session.status.as_str(), "stopped" | "completed" | "failed");
    let stop_sender = transcode_stop_registry()
        .lock()
        .await
        .remove(play_session_id);
    if let Some(stop_sender) = stop_sender {
        state
            .db
            .update_transcode_session_status(play_session_id, "stopping")
            .await?;
        let _ = stop_sender.send(());
    } else if !already_terminal {
        state
            .db
            .update_transcode_session_status(play_session_id, "stopped")
            .await?;
        cleanup_hls_transcode_files(&session.output_path).await;
    } else {
        cleanup_hls_transcode_files(&session.output_path).await;
    }

    Ok(StatusCode::OK)
}

async fn authenticated_item_empty_json_array(
    State(state): State<AppState>,
    headers: HeaderMap,
    Query(query): Query<AuthQuery>,
    Path(item_id): Path<String>,
) -> Result<Json<Vec<serde_json::Value>>, ApiError> {
    require_request_user(&state.db, &headers, query.api_key.as_deref()).await?;
    media_item_by_id(&state.db, &item_id).await?;
    Ok(empty_json_array().await)
}

async fn authenticated_item_theme_media(
    State(state): State<AppState>,
    headers: HeaderMap,
    Query(query): Query<AuthQuery>,
    Path(item_id): Path<String>,
) -> Result<Json<serde_json::Value>, ApiError> {
    require_request_user(&state.db, &headers, query.api_key.as_deref()).await?;
    media_item_by_id(&state.db, &item_id).await?;
    Ok(Json(serde_json::json!({
        "ThemeVideosResult": query_result(Vec::new()),
        "ThemeSongsResult": query_result(Vec::new()),
        "SoundtrackSongsResult": query_result(Vec::new())
    })))
}

async fn authenticated_item_theme_items(
    State(state): State<AppState>,
    headers: HeaderMap,
    Query(query): Query<AuthQuery>,
    Path(item_id): Path<String>,
) -> Result<Json<serde_json::Value>, ApiError> {
    require_request_user(&state.db, &headers, query.api_key.as_deref()).await?;
    media_item_by_id(&state.db, &item_id).await?;
    Ok(empty_items_result().await)
}

async fn item_filters(
    State(state): State<AppState>,
    headers: HeaderMap,
    Query(auth_query): Query<AuthQuery>,
    RawQuery(raw_query): RawQuery,
) -> Result<Json<serde_json::Value>, ApiError> {
    let query = parse_items_query(raw_query.as_deref());
    let items =
        filtered_items_for_query(&state, &headers, auth_query.api_key.as_deref(), &query).await?;
    let media_types = items
        .iter()
        .map(|item| item.media_type.clone())
        .collect::<BTreeSet<_>>()
        .into_iter()
        .collect::<Vec<_>>();
    let containers = items
        .iter()
        .filter_map(media_item_container)
        .collect::<BTreeSet<_>>()
        .into_iter()
        .collect::<Vec<_>>();

    Ok(Json(serde_json::json!({
        "Genres": [],
        "Tags": [],
        "OfficialRatings": [],
        "Years": [],
        "Containers": containers,
        "MediaTypes": media_types,
        "VideoTypes": [],
        "SeriesStatuses": [],
        "Staff": [],
        "Artists": [],
        "Albums": [],
        "Studios": [],
        "Trailers": [],
        "Features": []
    })))
}

async fn query_filters(
    State(state): State<AppState>,
    headers: HeaderMap,
    Query(auth_query): Query<AuthQuery>,
    RawQuery(raw_query): RawQuery,
) -> Result<Json<serde_json::Value>, ApiError> {
    let query = parse_items_query(raw_query.as_deref());
    filtered_items_for_query(&state, &headers, auth_query.api_key.as_deref(), &query).await?;
    Ok(Json(serde_json::json!({
        "Genres": [],
        "Tags": [],
        "AudioLanguages": [],
        "SubtitleLanguages": []
    })))
}

async fn filtered_items_for_query(
    state: &AppState,
    headers: &HeaderMap,
    api_key: Option<&str>,
    query: &ItemsQuery,
) -> Result<Vec<MediaItem>, ApiError> {
    let auth_user = require_request_user(&state.db, headers, api_key).await?;
    let requested_user_id = query.user_id.as_deref().map(resolve_user_id).transpose()?;
    if let Some(requested_user_id) = requested_user_id {
        ensure_user_access(&auth_user, requested_user_id)?;
    }
    filtered_media_items(
        state.db.media_items().await?,
        query,
        requested_user_id,
        &state.db,
    )
    .await
}

fn query_result(items: Vec<serde_json::Value>) -> serde_json::Value {
    let total_record_count = items.len();
    query_result_with_total(items, total_record_count, 0)
}

fn query_result_with_total(
    items: Vec<serde_json::Value>,
    total_record_count: usize,
    start_index: usize,
) -> serde_json::Value {
    serde_json::json!({
        "TotalRecordCount": total_record_count,
        "StartIndex": start_index,
        "Items": items,
    })
}

async fn filtered_media_items(
    items: Vec<MediaItem>,
    query: &ItemsQuery,
    user_id: Option<Uuid>,
    db: &Database,
) -> Result<Vec<MediaItem>, ApiError> {
    let parent_id = query
        .parent_id
        .as_deref()
        .map(parse_jellyfin_uuid)
        .transpose()?;
    let ids = query.ids.as_deref().map(parse_uuid_list).transpose()?;
    let include_types = csv_values_lowercase(&query.include_item_types);
    let exclude_types = csv_values_lowercase(&query.exclude_item_types);
    let media_types = csv_values_lowercase(&query.media_types);
    let filters = csv_values_lowercase(&query.filters);
    let search_term = query
        .search_term
        .as_deref()
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .map(str::to_ascii_lowercase);
    let name_starts_with = normalized_prefix(query.name_starts_with.as_deref());
    let name_starts_with_or_greater =
        normalized_prefix(query.name_starts_with_or_greater.as_deref());
    let name_less_than = normalized_prefix(query.name_less_than.as_deref());
    let is_folder_filter = query
        .is_folder
        .or_else(|| filters.as_deref().and_then(folder_filter_value));
    let is_played_filter = query
        .is_played
        .or_else(|| filters.as_deref().and_then(played_filter_value));

    let mut items = items
        .into_iter()
        .filter(|item| ids.as_ref().is_none_or(|ids| ids.contains(&item.id)))
        .filter(|item| parent_id.is_none_or(|parent_id| item.virtual_folder_id == parent_id))
        .filter(|item| {
            include_types.as_ref().is_none_or(|types| {
                let item_type = media_item_type(item).to_ascii_lowercase();
                types.iter().any(|allowed| allowed == &item_type)
            })
        })
        .filter(|item| {
            exclude_types.as_ref().is_none_or(|types| {
                let item_type = media_item_type(item).to_ascii_lowercase();
                !types.iter().any(|excluded| excluded == &item_type)
            })
        })
        .filter(|item| {
            media_types.as_ref().is_none_or(|types| {
                let media_type = item.media_type.to_ascii_lowercase();
                types.iter().any(|allowed| allowed == &media_type)
            })
        })
        .filter(|_| is_folder_filter.is_none_or(|is_folder| !is_folder))
        .filter(|item| {
            search_term
                .as_ref()
                .is_none_or(|term| item.name.to_ascii_lowercase().contains(term))
        })
        .filter(|item| {
            name_starts_with
                .as_ref()
                .is_none_or(|prefix| item.name.to_ascii_lowercase().starts_with(prefix))
        })
        .filter(|item| {
            name_starts_with_or_greater
                .as_ref()
                .is_none_or(|prefix| item.name.to_ascii_lowercase().as_str() >= prefix.as_str())
        })
        .filter(|item| {
            name_less_than
                .as_ref()
                .is_none_or(|prefix| item.name.to_ascii_lowercase().as_str() < prefix.as_str())
        })
        .collect::<Vec<_>>();

    if let Some(is_played) = is_played_filter {
        let Some(user_id) = user_id else {
            items.clear();
            return Ok(items);
        };
        let mut filtered = Vec::new();
        for item in items {
            let played = db
                .playback_state_for_item(user_id, item.id)
                .await?
                .is_some_and(|state| state.played);
            if played == is_played {
                filtered.push(item);
            }
        }
        items = filtered;
    }

    let sort_fields = sort_fields(query.sort_by.as_deref());
    if query
        .sort_order
        .as_deref()
        .is_some_and(|order| order.eq_ignore_ascii_case("Descending"))
    {
        items.sort_by(|left, right| compare_media_items(right, left, &sort_fields));
    } else {
        items.sort_by(|left, right| compare_media_items(left, right, &sort_fields));
    }

    Ok(items)
}

fn paged_media_items(items: Vec<MediaItem>, query: &ItemsQuery) -> Vec<MediaItem> {
    let start_index = query.start_index.unwrap_or(0);
    let limit = query.limit.unwrap_or(usize::MAX);
    items.into_iter().skip(start_index).take(limit).collect()
}

async fn items_to_json(
    db: &Database,
    items: Vec<MediaItem>,
    server_id: &str,
    user_id: Option<Uuid>,
) -> Result<Vec<serde_json::Value>, ApiError> {
    let mut values = Vec::with_capacity(items.len());
    for item in items {
        let playback = if let Some(user_id) = user_id {
            db.playback_state_for_item(user_id, item.id).await?
        } else {
            None
        };
        values.push(media_item_to_json_with_playback(
            &item,
            server_id,
            playback.as_ref(),
        ));
    }
    Ok(values)
}

fn csv_values_lowercase(values: &[String]) -> Option<Vec<String>> {
    let values = values
        .iter()
        .flat_map(|value| value.split(','))
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .map(str::to_ascii_lowercase)
        .collect::<Vec<_>>();
    if values.is_empty() {
        None
    } else {
        Some(values)
    }
}

#[derive(Debug, Deserialize)]
#[serde(untagged)]
enum QueryStringValues {
    One(String),
    Many(Vec<String>),
}

fn deserialize_csv_values<'de, D>(deserializer: D) -> Result<Vec<String>, D::Error>
where
    D: Deserializer<'de>,
{
    let values = Option::<QueryStringValues>::deserialize(deserializer)?;
    Ok(match values {
        Some(QueryStringValues::One(value)) => vec![value],
        Some(QueryStringValues::Many(values)) => values,
        None => Vec::new(),
    })
}

fn normalized_prefix(value: Option<&str>) -> Option<String> {
    value
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .map(str::to_ascii_lowercase)
}

fn folder_filter_value(filters: &[String]) -> Option<bool> {
    if filters.iter().any(|filter| filter == "isfolder") {
        Some(true)
    } else if filters.iter().any(|filter| filter == "isnotfolder") {
        Some(false)
    } else {
        None
    }
}

fn played_filter_value(filters: &[String]) -> Option<bool> {
    if filters.iter().any(|filter| filter == "isplayed") {
        Some(true)
    } else if filters.iter().any(|filter| filter == "isunplayed") {
        Some(false)
    } else {
        None
    }
}

#[derive(Debug, Clone, Copy)]
enum SortField {
    SortName,
    DateCreated,
    DateLastMediaAdded,
}

fn sort_fields(sort_by: Option<&str>) -> Vec<SortField> {
    let fields = sort_by
        .unwrap_or("SortName")
        .split(',')
        .filter_map(|field| match field.trim().to_ascii_lowercase().as_str() {
            "sortname" | "name" => Some(SortField::SortName),
            "datecreated" => Some(SortField::DateCreated),
            "datelastmediaadded" => Some(SortField::DateLastMediaAdded),
            _ => None,
        })
        .collect::<Vec<_>>();

    if fields.is_empty() {
        vec![SortField::SortName]
    } else {
        fields
    }
}

fn compare_media_items(left: &MediaItem, right: &MediaItem, fields: &[SortField]) -> Ordering {
    fields
        .iter()
        .map(|field| match field {
            SortField::SortName => left
                .name
                .to_ascii_lowercase()
                .cmp(&right.name.to_ascii_lowercase()),
            SortField::DateCreated => left.created_at.cmp(&right.created_at),
            SortField::DateLastMediaAdded => left.updated_at.cmp(&right.updated_at),
        })
        .find(|ordering| *ordering != Ordering::Equal)
        .unwrap_or_else(|| left.id.cmp(&right.id))
}

fn parse_uuid_list(value: &str) -> Result<Vec<Uuid>, ApiError> {
    value
        .split(',')
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .map(parse_jellyfin_uuid)
        .collect()
}

fn parse_jellyfin_uuid(value: &str) -> Result<Uuid, ApiError> {
    Uuid::parse_str(value)
        .or_else(|_| Uuid::parse_str(&hyphenate_uuid(value)))
        .map_err(|_| ApiError::bad_request("Invalid item id"))
}

fn hyphenate_uuid(value: &str) -> String {
    if value.len() == 32 {
        format!(
            "{}-{}-{}-{}-{}",
            &value[0..8],
            &value[8..12],
            &value[12..16],
            &value[16..20],
            &value[20..32]
        )
    } else {
        value.to_string()
    }
}

fn user_view_to_json(folder: &VirtualFolder, server_id: &str) -> serde_json::Value {
    serde_json::json!({
        "Name": folder.name,
        "ServerId": server_id,
        "Id": folder.id.simple().to_string(),
        "Etag": null,
        "DateCreated": folder.created_at.to_string(),
        "CanDelete": false,
        "CanDownload": false,
        "SortName": folder.name,
        "ExternalUrls": [],
        "Path": null,
        "EnableMediaSourceDisplay": true,
        "ChannelId": null,
        "Taglines": [],
        "Genres": [],
        "PlayAccess": "Full",
        "RemoteTrailers": [],
        "ProviderIds": {},
        "IsFolder": true,
        "ParentId": null,
        "Type": "CollectionFolder",
        "CollectionType": folder.collection_type,
        "UserData": { "PlaybackPositionTicks": 0, "PlayCount": 0, "IsFavorite": false, "Played": false },
        "ImageTags": { "Primary": "placeholder" },
        "PrimaryImageAspectRatio": 0.6666667,
        "BackdropImageTags": [],
        "LocationType": "FileSystem",
        "MediaType": null,
    })
}

fn media_item_to_json(item: &MediaItem, server_id: &str) -> serde_json::Value {
    media_item_to_json_with_playback(item, server_id, None)
}

fn transcode_session_json(session: &TranscodeSession) -> serde_json::Value {
    let item_id = session.item.id.simple().to_string();
    serde_json::json!({
        "PlaySessionId": session.play_session_id,
        "UserId": session.user_id.simple().to_string(),
        "ItemId": item_id,
        "MediaSourceId": session.media_source_id.clone().unwrap_or(item_id),
        "DeviceId": null,
        "Path": session.output_path,
        "OutputPath": session.output_path,
        "Status": session.status,
        "ProcessId": session.process_id,
        "ProgressPercentage": session.progress_percent,
        "CompletionPercentage": session.progress_percent,
        "TranscodingPositionTicks": session.position_ticks,
        "TranscodingStartPositionTicks": 0,
        "Container": media_item_container(&session.item),
        "VideoCodec": media_item_stream_codec(&session.item, "Video"),
        "AudioCodec": media_item_stream_codec(&session.item, "Audio"),
        "Width": session.item.width,
        "Height": session.item.height,
        "Bitrate": session.item.bitrate,
        "VideoStreamIndex": session.video_stream_index,
        "AudioStreamIndex": session.audio_stream_index,
        "SubtitleStreamIndex": session.subtitle_stream_index,
        "TranscodeReasons": [],
        "IsAudioDirect": false,
        "IsVideoDirect": false,
        "UpdatedAt": session.updated_at.to_string(),
    })
}

fn media_item_to_json_with_playback(
    item: &MediaItem,
    server_id: &str,
    playback: Option<&PlaybackState>,
) -> serde_json::Value {
    let item_type = media_item_type(item);
    let item_id = item.id.simple().to_string();
    let container = media_item_container(item);
    let file_name = media_item_file_name(item);
    let file_size = media_item_file_size(item);
    let playback_position_ticks = playback.map_or(0, |state| state.position_ticks);
    let played = playback.is_some_and(|state| state.played);
    let play_count = i32::from(played);
    let media_streams = media_item_streams(item);
    let default_audio_stream_index = default_audio_stream_index(&media_streams);
    let default_subtitle_stream_index = default_subtitle_stream_index(&media_streams);
    let selected_audio_stream_index = playback
        .and_then(|state| state.audio_stream_index)
        .or(default_audio_stream_index);
    let selected_subtitle_stream_index =
        selected_subtitle_stream_index(playback, default_subtitle_stream_index);
    let direct_stream_url = media_item_direct_stream_url(item, &item_id);
    let video_type = if item.media_type == "Video" {
        serde_json::json!("VideoFile")
    } else {
        serde_json::Value::Null
    };

    let media_source = serde_json::json!({
        "Protocol": "File",
        "Id": playback.and_then(|state| state.media_source_id.clone()).unwrap_or_else(|| item_id.clone()),
        "Path": item.path,
        "Type": "Default",
        "Container": container,
        "Size": file_size,
        "Name": item.name,
        "IsRemote": false,
        "DirectStreamUrl": direct_stream_url,
        "ETag": null,
        "RunTimeTicks": item.runtime_ticks,
        "ReadAtNativeFramerate": false,
        "IgnoreDts": false,
        "IgnoreIndex": false,
        "GenPtsInput": false,
        "SupportsTranscoding": false,
        "SupportsDirectStream": true,
        "SupportsDirectPlay": true,
        "IsInfiniteStream": false,
        "RequiresOpening": false,
        "RequiresClosing": false,
        "RequiresLooping": false,
        "SupportsProbing": true,
        "VideoType": video_type,
        "DefaultAudioStreamIndex": selected_audio_stream_index,
        "DefaultSubtitleStreamIndex": selected_subtitle_stream_index,
        "MediaStreams": media_streams.clone(),
        "Formats": [],
        "Bitrate": item.bitrate,
    });

    serde_json::json!({
        "Name": item.name,
        "OriginalTitle": null,
        "ServerId": server_id,
        "Id": item_id,
        "Etag": null,
        "DateCreated": item.created_at.to_string(),
        "DateLastMediaAdded": item.updated_at.to_string(),
        "PremiereDate": null,
        "ProductionYear": null,
        "CommunityRating": null,
        "CriticRating": null,
        "OfficialRating": null,
        "Overview": "",
        "CanDelete": false,
        "CanDownload": true,
        "SortName": item.name,
        "ForcedSortName": null,
        "ExternalUrls": [],
        "Path": item.path,
        "FileName": file_name,
        "Container": container,
        "Size": file_size,
        "EnableMediaSourceDisplay": true,
        "ChannelId": null,
        "Taglines": [],
        "Genres": [],
        "GenreItems": [],
        "Studios": [],
        "People": [],
        "Tags": [],
        "TagItems": [],
        "Chapters": [],
        "MediaStreams": media_streams,
        "PlayAccess": "Full",
        "RemoteTrailers": [],
        "ProviderIds": {},
        "IsFolder": false,
        "ParentId": item.virtual_folder_id.simple().to_string(),
        "Type": item_type,
        "MediaType": item.media_type,
        "RunTimeTicks": item.runtime_ticks,
        "UserData": { "PlaybackPositionTicks": playback_position_ticks, "PlayCount": play_count, "IsFavorite": false, "Played": played, "Key": item_id, "ItemId": item_id, "PlayedPercentage": null, "LastPlayedDate": null },
        "ImageTags": { "Primary": "placeholder" },
        "PrimaryImageAspectRatio": 0.6666667,
        "BackdropImageTags": [],
        "LocationType": "FileSystem",
        "MediaSources": [media_source],
    })
}

fn selected_subtitle_stream_index(
    playback: Option<&PlaybackState>,
    default_subtitle_stream_index: Option<i64>,
) -> serde_json::Value {
    match playback.and_then(|state| state.subtitle_stream_index) {
        Some(index) if index < 0 => serde_json::Value::Null,
        Some(index) => serde_json::json!(index),
        None => serde_json::json!(default_subtitle_stream_index),
    }
}

fn transcode_temp_root() -> PathBuf {
    std::env::temp_dir().join("jellyrin").join("transcodes")
}

fn hls_master_url(item_id: &str, play_session_id: &str, access_token: &str) -> String {
    format!("/Videos/{item_id}/master.m3u8?PlaySessionId={play_session_id}&api_key={access_token}")
}

async fn cleanup_hls_transcode_files(output_path: &str) -> bool {
    let session_dir = HlsTranscodeLayout::from_media_playlist_path(output_path).session_dir;
    cleanup_hls_transcode_dir(&session_dir).await
}

async fn cleanup_hls_transcode_dir(session_dir: &FsPath) -> bool {
    if session_dir.as_os_str().is_empty() {
        return false;
    }
    match tokio::fs::remove_dir_all(session_dir).await {
        Ok(()) => true,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => false,
        Err(error) => {
            tracing::warn!(
                path = %session_dir.display(),
                %error,
                "failed to remove HLS transcode directory"
            );
            false
        }
    }
}

fn first_stream_index(media_streams: &[serde_json::Value], stream_type: &str) -> Option<i64> {
    media_streams.iter().find_map(|stream| {
        let stream = stream.as_object()?;
        let type_name = stream.get("Type").and_then(serde_json::Value::as_str)?;
        if !type_name.eq_ignore_ascii_case(stream_type) {
            return None;
        }
        stream.get("Index").and_then(json_value_i64)
    })
}

fn default_audio_stream_index(media_streams: &[serde_json::Value]) -> Option<i64> {
    let mut first_audio_index = None;
    for stream in media_streams {
        let Some(stream) = stream.as_object() else {
            continue;
        };
        let Some(type_name) = stream.get("Type").and_then(serde_json::Value::as_str) else {
            continue;
        };
        if !type_name.eq_ignore_ascii_case("Audio") {
            continue;
        }

        let Some(index) = stream.get("Index").and_then(json_value_i64) else {
            continue;
        };
        if first_audio_index.is_none() {
            first_audio_index = Some(index);
        }
        if stream
            .get("IsDefault")
            .and_then(serde_json::Value::as_bool)
            .unwrap_or(false)
        {
            return Some(index);
        }
    }

    first_audio_index
}

fn default_subtitle_stream_index(media_streams: &[serde_json::Value]) -> Option<i64> {
    media_streams.iter().find_map(|stream| {
        let stream = stream.as_object()?;
        let type_name = stream.get("Type").and_then(serde_json::Value::as_str)?;
        if !type_name.eq_ignore_ascii_case("Subtitle") {
            return None;
        }
        if !stream
            .get("IsDefault")
            .and_then(serde_json::Value::as_bool)
            .unwrap_or(false)
        {
            return None;
        }

        stream.get("Index").and_then(json_value_i64)
    })
}

fn json_value_i64(value: &serde_json::Value) -> Option<i64> {
    match value {
        serde_json::Value::Number(value) => value.as_i64(),
        serde_json::Value::String(value) => value.parse::<i64>().ok(),
        _ => None,
    }
}

fn item_counts_json(items: &[MediaItem]) -> serde_json::Value {
    let mut movie_count = 0;
    let mut series_count = 0;
    let mut episode_count = 0;
    let mut song_count = 0;
    let mut album_count = 0;
    let mut book_count = 0;
    let mut music_video_count = 0;

    for item in items {
        match media_item_type(item) {
            "Movie" => movie_count += 1,
            "Series" => series_count += 1,
            "Episode" => episode_count += 1,
            "Audio" => song_count += 1,
            "MusicAlbum" => album_count += 1,
            "Book" => book_count += 1,
            "MusicVideo" => music_video_count += 1,
            _ => {}
        }
    }

    serde_json::json!({
        "MovieCount": movie_count,
        "SeriesCount": series_count,
        "EpisodeCount": episode_count,
        "ArtistCount": 0,
        "ProgramCount": 0,
        "TrailerCount": 0,
        "SongCount": song_count,
        "AlbumCount": album_count,
        "MusicVideoCount": music_video_count,
        "BoxSetCount": 0,
        "BookCount": book_count,
        "ItemCount": items.len(),
    })
}

fn media_item_path(item: &MediaItem) -> &FsPath {
    FsPath::new(&item.path)
}

fn media_item_container(item: &MediaItem) -> Option<String> {
    media_item_path(item)
        .extension()
        .and_then(|extension| extension.to_str())
        .map(|extension| extension.to_ascii_lowercase())
}

fn media_item_file_name(item: &MediaItem) -> Option<String> {
    media_item_path(item)
        .file_name()
        .and_then(|file_name| file_name.to_str())
        .map(ToOwned::to_owned)
}

fn media_item_file_size(item: &MediaItem) -> Option<u64> {
    item.file_size
        .and_then(|file_size| u64::try_from(file_size).ok())
}

fn media_item_content_type(item: &MediaItem) -> String {
    match media_item_container(item).as_deref() {
        Some("mp4" | "m4v") => "video/mp4",
        Some("webm") => "video/webm",
        Some("mkv") => "video/x-matroska",
        Some("mov") => "video/quicktime",
        Some("avi") => "video/x-msvideo",
        Some("mp3") => "audio/mpeg",
        Some("m4a") => "audio/mp4",
        Some("aac") => "audio/aac",
        Some("flac") => "audio/flac",
        Some("ogg") => "audio/ogg",
        Some("wav") => "audio/wav",
        _ => "application/octet-stream",
    }
    .to_string()
}

fn media_item_direct_stream_url(item: &MediaItem, item_id: &str) -> String {
    match item.media_type.as_str() {
        "Audio" => format!("/Audio/{item_id}/stream"),
        _ => format!("/Videos/{item_id}/stream"),
    }
}

fn hls_codecs(item: &MediaItem) -> Option<String> {
    let codecs = [
        media_item_stream_codec(item, "Video"),
        media_item_stream_codec(item, "Audio"),
    ]
    .into_iter()
    .flatten()
    .filter(|codec| !codec.trim().is_empty())
    .collect::<Vec<_>>();
    (!codecs.is_empty()).then(|| codecs.join(","))
}

fn rewrite_hls_media_playlist(playlist: &str, item_id: &str, raw_query: Option<&str>) -> String {
    let mut rewritten = String::with_capacity(playlist.len());
    for line in playlist.lines() {
        if line.trim().is_empty() || line.starts_with('#') {
            rewritten.push_str(line);
        } else if let Some(segment_id) = hls_segment_id_from_uri(line) {
            rewritten.push_str(&append_query(
                &format!("/Videos/{item_id}/hls1/main/{segment_id}.ts"),
                raw_query,
            ));
        } else {
            rewritten.push_str(line);
        }
        rewritten.push('\n');
    }
    rewritten
}

fn hls_segment_id_from_uri(uri: &str) -> Option<u32> {
    let path = uri.split_once('?').map_or(uri, |(path, _)| path);
    let file_name = FsPath::new(path).file_name()?.to_str()?;
    let segment_id = file_name.strip_prefix("segment_")?.strip_suffix(".ts")?;
    segment_id.parse().ok()
}

fn parse_hls_segment_file(segment_file: &str) -> Result<(i64, &str), ApiError> {
    let (segment_id, container) = segment_file
        .rsplit_once('.')
        .ok_or_else(|| ApiError::not_found("HLS segment not found"))?;
    let segment_id = segment_id
        .parse()
        .map_err(|_| ApiError::not_found("HLS segment not found"))?;
    Ok((segment_id, container))
}

fn append_query(path: &str, raw_query: Option<&str>) -> String {
    raw_query
        .filter(|query| !query.trim().is_empty())
        .map_or_else(|| path.to_string(), |query| format!("{path}?{query}"))
}

fn positive_u32(value: i64) -> Option<u32> {
    u32::try_from(value).ok().filter(|value| *value > 0)
}

fn media_item_streams(item: &MediaItem) -> Vec<serde_json::Value> {
    if !item.media_streams.is_empty() {
        return item.media_streams.clone();
    }

    if item.media_type == "Audio" {
        return vec![serde_json::json!({
            "Codec": media_item_container(item).unwrap_or_else(|| "unknown".to_string()),
            "Language": null,
            "DisplayTitle": "Audio",
            "IsInterlaced": false,
            "BitRate": item.bitrate,
            "BitDepth": null,
            "Channels": null,
            "SampleRate": null,
            "IsDefault": true,
            "IsForced": false,
            "Type": "Audio",
            "Index": 0,
            "IsExternal": false,
            "Path": null
        })];
    }

    if item.media_type == "Video" {
        return vec![serde_json::json!({
            "Codec": media_item_container(item).unwrap_or_else(|| "unknown".to_string()),
            "Language": null,
            "ColorTransfer": null,
            "ColorPrimaries": null,
            "ColorSpace": null,
            "TimeBase": null,
            "VideoRange": null,
            "DisplayTitle": "Video",
            "NalLengthSize": null,
            "IsInterlaced": false,
            "IsAVC": null,
            "BitRate": item.bitrate,
            "BitDepth": null,
            "RefFrames": null,
            "IsDefault": true,
            "IsForced": false,
            "Height": item.height,
            "Width": item.width,
            "AverageFrameRate": null,
            "RealFrameRate": null,
            "Profile": null,
            "Type": "Video",
            "AspectRatio": null,
            "Index": 0,
            "IsExternal": false,
            "IsTextSubtitleStream": false,
            "SupportsExternalStream": false,
            "Path": null,
            "PixelFormat": null,
            "Level": null,
            "IsAnamorphic": null
        })];
    }

    Vec::new()
}

fn parse_range_header(headers: &HeaderMap, total_len: u64) -> Result<Option<(u64, u64)>, ()> {
    let Some(range) = headers.get(header::RANGE) else {
        return Ok(None);
    };
    if total_len == 0 {
        return Err(());
    }

    let range = range.to_str().map_err(|_| ())?;
    let range = range.strip_prefix("bytes=").ok_or(())?;
    let (start, end) = range.split_once('-').ok_or(())?;
    let (start, end) = if start.is_empty() {
        let suffix_len = end.parse::<u64>().map_err(|_| ())?.min(total_len);
        (total_len - suffix_len, total_len - 1)
    } else {
        let start = start.parse::<u64>().map_err(|_| ())?;
        let end = if end.is_empty() {
            total_len - 1
        } else {
            end.parse::<u64>().map_err(|_| ())?.min(total_len - 1)
        };
        (start, end)
    };

    if start <= end && start < total_len {
        Ok(Some((start, end)))
    } else {
        Err(())
    }
}

fn media_item_type(item: &MediaItem) -> &'static str {
    match (item.media_type.as_str(), item.collection_type.as_deref()) {
        ("Video", Some("movies")) => "Movie",
        ("Video", _) => "Video",
        ("Audio", _) => "Audio",
        ("Photo", _) => "Photo",
        ("Book", _) => "Book",
        _ => "BaseItem",
    }
}

#[derive(Debug, Deserialize)]
struct DisplayPreferencesQuery {
    #[serde(flatten)]
    auth: AuthQuery,
    #[serde(alias = "UserId")]
    user_id: Option<String>,
    #[serde(alias = "Client")]
    client: Option<String>,
}

async fn display_preferences(
    State(state): State<AppState>,
    headers: HeaderMap,
    Query(query): Query<DisplayPreferencesQuery>,
) -> Result<Json<serde_json::Value>, ApiError> {
    let auth_user =
        require_request_user(&state.db, &headers, query.auth.api_key.as_deref()).await?;
    let user_id = match query.user_id.as_deref() {
        Some(user_id) => resolve_user_id(user_id)?,
        None => auth_user.id,
    };
    ensure_user_access(&auth_user, user_id)?;
    let client = display_preferences_client(query.client.as_deref());
    let preferences = state
        .db
        .display_preferences(user_id, &client, "usersettings")
        .await?
        .unwrap_or_else(|| default_display_preferences("usersettings"));
    Ok(Json(preferences))
}

async fn update_display_preferences(
    State(state): State<AppState>,
    headers: HeaderMap,
    Query(query): Query<DisplayPreferencesQuery>,
    Json(payload): Json<serde_json::Value>,
) -> Result<StatusCode, ApiError> {
    let auth_user =
        require_request_user(&state.db, &headers, query.auth.api_key.as_deref()).await?;
    let user_id = match query.user_id.as_deref() {
        Some(user_id) => resolve_user_id(user_id)?,
        None => auth_user.id,
    };
    ensure_user_access(&auth_user, user_id)?;
    let client = display_preferences_client(query.client.as_deref());
    let preferences = normalize_display_preferences_payload(payload, "usersettings")?;
    state
        .db
        .update_display_preferences(user_id, &client, "usersettings", preferences)
        .await?;
    Ok(StatusCode::NO_CONTENT)
}

fn display_preferences_client(client: Option<&str>) -> String {
    client
        .filter(|client| !client.trim().is_empty())
        .unwrap_or("emby")
        .to_string()
}

fn normalize_display_preferences_payload(
    payload: serde_json::Value,
    id: &str,
) -> Result<serde_json::Value, ApiError> {
    let mut preferences = match payload {
        serde_json::Value::Object(map) => serde_json::Value::Object(map),
        _ => {
            return Err(ApiError::bad_request(
                "Display preferences body must be an object",
            ));
        }
    };
    preferences["Id"] = serde_json::Value::String(id.to_string());
    Ok(preferences)
}

fn default_display_preferences(id: &str) -> serde_json::Value {
    serde_json::json!({
        "Id": id,
        "ViewType": "",
        "SortBy": "SortName",
        "IndexBy": "",
        "RememberIndexing": false,
        "PrimaryImageHeight": 0,
        "PrimaryImageWidth": 0,
        "CustomPrefs": {}
    })
}

async fn system_endpoint() -> Json<serde_json::Value> {
    Json(serde_json::json!({
        "IsLocal": true,
        "IsInNetwork": true
    }))
}

#[derive(Debug, Deserialize)]
struct BitrateQuery {
    #[serde(alias = "Size")]
    size: Option<usize>,
}

async fn bitrate_test(Query(query): Query<BitrateQuery>) -> impl IntoResponse {
    let size = query.size.unwrap_or_default().min(1_000_000);
    (
        [(header::CONTENT_TYPE, "application/octet-stream")],
        vec![0u8; size],
    )
}

async fn item_placeholder_image(
    State(state): State<AppState>,
    Path((item_id, _image_type)): Path<(String, String)>,
) -> Result<axum::response::Response, ApiError> {
    media_item_or_folder_by_id(&state.db, &item_id).await?;
    Ok(placeholder_png_response())
}

async fn user_placeholder_image(
    State(state): State<AppState>,
    Path((user_id, _image_type)): Path<(Uuid, String)>,
) -> Result<axum::response::Response, ApiError> {
    if !state
        .db
        .users()
        .await?
        .into_iter()
        .any(|user| user.id == user_id)
    {
        return Err(ApiError::not_found("User not found"));
    }
    Ok(placeholder_png_response())
}

fn placeholder_png_response() -> axum::response::Response {
    const TRANSPARENT_PNG: &[u8] = &[
        137, 80, 78, 71, 13, 10, 26, 10, 0, 0, 0, 13, 73, 72, 68, 82, 0, 0, 0, 1, 0, 0, 0, 1, 8, 6,
        0, 0, 0, 31, 21, 196, 137, 0, 0, 0, 13, 73, 68, 65, 84, 120, 156, 99, 0, 1, 0, 0, 5, 0, 1,
        13, 10, 45, 180, 0, 0, 0, 0, 73, 69, 78, 68, 174, 66, 96, 130,
    ];
    (
        [(header::CONTENT_TYPE, "image/png")],
        TRANSPARENT_PNG.to_vec(),
    )
        .into_response()
}

async fn media_item_or_folder_by_id(db: &Database, item_id: &str) -> Result<(), ApiError> {
    let requested_id = parse_jellyfin_uuid(item_id)?;
    if db
        .media_items()
        .await?
        .into_iter()
        .any(|item| item.id == requested_id)
    {
        return Ok(());
    }
    if db
        .virtual_folders()
        .await?
        .into_iter()
        .any(|folder| folder.id == requested_id)
    {
        return Ok(());
    }
    Err(ApiError::not_found("Item not found"))
}

fn playback_event_sender() -> &'static broadcast::Sender<PlaybackEvent> {
    PLAYBACK_EVENTS.get_or_init(|| {
        let (sender, _) = broadcast::channel(256);
        sender
    })
}

fn subscribe_playback_events() -> broadcast::Receiver<PlaybackEvent> {
    playback_event_sender().subscribe()
}

fn broadcast_session_message(session_id: &str, message: serde_json::Value) {
    let _ = playback_event_sender().send(PlaybackEvent {
        session_id: session_id.to_string(),
        message,
    });
}

async fn broadcast_sessions_message(
    db: &Database,
    session_id: &str,
    user: &User,
) -> Result<(), ApiError> {
    broadcast_session_message(
        session_id,
        serde_json::json!({
            "MessageType": "Sessions",
            "Data": session_list_json(db, user).await?
        }),
    );
    Ok(())
}

async fn websocket(
    State(state): State<AppState>,
    headers: HeaderMap,
    Query(query): Query<AuthQuery>,
    ws: Result<WebSocketUpgrade, WebSocketUpgradeRejection>,
) -> axum::response::Response {
    let (user, session_id) = match require_user(&state.db, &headers, query.api_key.as_deref()).await
    {
        Ok((user, token)) => (user, token.access_token),
        Err(error) => return error.into_response(),
    };

    match ws {
        Ok(ws) => ws
            .on_upgrade(move |socket| handle_websocket(socket, state, user, session_id))
            .into_response(),
        Err(rejection) => rejection.into_response(),
    }
}

async fn handle_websocket(mut socket: WebSocket, state: AppState, user: User, session_id: String) {
    let _ = socket
        .send(Message::Text(
            serde_json::json!({
                "MessageType": "ForceKeepAlive",
                "Data": 60
            })
            .to_string()
            .into(),
        ))
        .await;
    let mut receiver = subscribe_playback_events();
    loop {
        tokio::select! {
            event = receiver.recv() => {
                match event {
                    Ok(event) if event.session_id == session_id => {
                        if socket
                            .send(Message::Text(event.message.to_string().into()))
                            .await
                            .is_err()
                        {
                            break;
                        }
                    }
                    Ok(_) => {}
                    Err(broadcast::error::RecvError::Lagged(_)) => {}
                    Err(broadcast::error::RecvError::Closed) => break,
                }
            }
            message = socket.recv() => {
                match message {
                    Some(Ok(Message::Text(text))) => {
                        if should_send_sessions_message(&text) {
                            let sessions = match session_list_json(&state.db, &user).await {
                                Ok(sessions) => sessions,
                                Err(_) => break,
                            };
                            let message = serde_json::json!({
                                "MessageType": "Sessions",
                                "Data": sessions
                            });
                            if socket
                                .send(Message::Text(message.to_string().into()))
                                .await
                                .is_err()
                            {
                                break;
                            }
                        }
                    }
                    Some(Ok(Message::Close(_))) | None => break,
                    Some(Ok(_)) => {}
                    Some(Err(_)) => break,
                }
            }
        }
    }
}

fn should_send_sessions_message(text: &str) -> bool {
    serde_json::from_str::<serde_json::Value>(text)
        .ok()
        .and_then(|value| {
            value
                .get("MessageType")
                .and_then(serde_json::Value::as_str)
                .map(str::to_string)
        })
        .is_some_and(|message_type| message_type.eq_ignore_ascii_case("SessionsStart"))
}

async fn localization_options() -> Json<Vec<LocalizationOptionDto>> {
    Json(vec![
        LocalizationOptionDto {
            name: "English".to_string(),
            value: "en-US".to_string(),
        },
        LocalizationOptionDto {
            name: "Espanol".to_string(),
            value: "es-ES".to_string(),
        },
    ])
}

async fn localization_cultures() -> Json<Vec<CultureDto>> {
    Json(load_cultures())
}

async fn localization_countries() -> Json<Vec<CountryDto>> {
    Json(load_countries())
}

async fn parental_ratings(
    State(state): State<AppState>,
    headers: HeaderMap,
    Query(query): Query<AuthQuery>,
) -> Result<Json<Vec<serde_json::Value>>, ApiError> {
    require_user_or_startup_incomplete(&state.db, &headers, query.api_key.as_deref()).await?;
    Ok(Json(vec![
        parental_rating("US-G", 1),
        parental_rating("US-PG", 5),
        parental_rating("US-PG-13", 7),
        parental_rating("US-R", 9),
        parental_rating("US-NC-17", 10),
    ]))
}

fn parental_rating(name: &str, score: i32) -> serde_json::Value {
    serde_json::json!({
        "Name": name,
        "Value": name,
        "RatingScore": {
            "score": score,
            "subScore": null
        }
    })
}

fn load_cultures() -> Vec<CultureDto> {
    let mut cultures = ISO_639_2_DATA
        .lines()
        .filter_map(|line| {
            let parts: Vec<_> = line.split('|').collect();
            if parts.len() != 5 || parts[3].trim().is_empty() {
                return None;
            }

            let mut name = parts[3].to_string();
            let two_letter = parts[2].to_string();
            if two_letter.contains('-') {
                name = two_letter.clone();
            }

            let mut three_letter_names = vec![parts[0].to_string()];
            if !parts[1].is_empty() {
                three_letter_names.push(parts[1].to_string());
            }

            Some(CultureDto {
                name,
                display_name: parts[3].to_string(),
                two_letter_iso_language_name: two_letter,
                three_letter_iso_language_name: three_letter_names.first().cloned(),
                three_letter_iso_language_names: three_letter_names,
            })
        })
        .collect::<Vec<_>>();

    cultures.sort_by_key(|culture| culture.display_name.to_ascii_lowercase());
    cultures.dedup_by(|a, b| a.display_name.eq_ignore_ascii_case(&b.display_name));
    cultures
}

fn load_countries() -> Vec<CountryDto> {
    let mut countries: Vec<CountryDto> =
        serde_json::from_str(COUNTRIES_DATA).expect("bundled countries.json is valid");
    countries.sort_by_key(|country| country.display_name.to_ascii_lowercase());
    countries
}

#[derive(Debug, Deserialize)]
struct AuthQuery {
    #[serde(alias = "api_key", alias = "ApiKey")]
    api_key: Option<String>,
}

#[derive(Debug, Deserialize)]
struct StreamQuery {
    #[serde(alias = "api_key", alias = "ApiKey")]
    api_key: Option<String>,
    #[serde(alias = "Static", alias = "static", alias = "static_")]
    _static_file: Option<bool>,
    #[serde(alias = "MediaSourceId")]
    _media_source_id: Option<String>,
    #[serde(alias = "DeviceId")]
    _device_id: Option<String>,
    #[serde(alias = "PlaySessionId")]
    _play_session_id: Option<String>,
    #[serde(alias = "Tag")]
    _tag: Option<String>,
    #[serde(alias = "StartTimeTicks")]
    _start_time_ticks: Option<i64>,
    #[serde(alias = "AudioStreamIndex")]
    _audio_stream_index: Option<i32>,
    #[serde(alias = "SubtitleStreamIndex")]
    _subtitle_stream_index: Option<i32>,
}

#[derive(Debug, Deserialize)]
struct HlsQuery {
    #[serde(alias = "api_key", alias = "ApiKey")]
    api_key: Option<String>,
    #[serde(alias = "PlaySessionId", alias = "playSessionId")]
    play_session_id: Option<String>,
}

#[derive(Debug, Deserialize)]
struct StopEncodingQuery {
    #[serde(alias = "api_key", alias = "ApiKey")]
    api_key: Option<String>,
    #[serde(alias = "PlaySessionId", alias = "playSessionId")]
    play_session_id: Option<String>,
    #[serde(alias = "DeviceId", alias = "deviceId")]
    device_id: Option<String>,
}

#[derive(Debug)]
struct ClientAuth {
    client: String,
    device: String,
    device_id: String,
    version: String,
}

fn startup_config_to_dto(config: StartupConfig) -> StartupConfigurationDto {
    StartupConfigurationDto {
        server_name: config.server_name,
        ui_culture: config.ui_culture,
        metadata_country_code: config.metadata_country_code,
        preferred_metadata_language: config.preferred_metadata_language,
    }
}

async fn user_to_dto(db: &Database, user: &User, server_id: Uuid) -> Result<UserDto, ApiError> {
    let configuration = db
        .user_configuration(user.id)
        .await?
        .unwrap_or_else(default_user_configuration);
    let has_configured_password = db.user_has_password(user.id).await?;
    Ok(UserDto {
        id: user.id,
        name: user.name.clone(),
        server_id,
        has_password: has_configured_password,
        has_configured_password,
        has_configured_easy_password: false,
        enable_auto_login: false,
        configuration,
        policy: UserPolicyDto {
            is_administrator: user.is_administrator,
            is_disabled: user.is_disabled,
            is_hidden: false,
            enable_all_devices: true,
            enable_remote_control_of_other_users: user.is_administrator,
            enable_shared_device_control: true,
            enable_remote_access: true,
            enable_collection_management: user.is_administrator,
            enable_subtitle_management: user.is_administrator,
            enable_content_downloading: true,
            enable_live_tv_management: user.is_administrator,
            enable_live_tv_access: true,
            enable_media_playback: true,
            enable_audio_playback_transcoding: true,
            enable_video_playback_transcoding: true,
            enable_playback_remuxing: true,
            force_remote_source_transcoding: false,
            enable_content_deletion: false,
            enable_content_deletion_from_folders: Vec::new(),
            remote_client_bitrate_limit: 0,
            login_attempts_before_lockout: -1,
            max_active_sessions: 0,
            authentication_provider_id: DEFAULT_AUTHENTICATION_PROVIDER_ID.to_string(),
            password_reset_provider_id: DEFAULT_PASSWORD_RESET_PROVIDER_ID.to_string(),
            sync_play_access: "CreateAndJoinGroups".to_string(),
        },
    })
}

async fn authentication_result_to_dto(
    db: &Database,
    user: &User,
    token: &DeviceToken,
    server_id: Uuid,
) -> Result<AuthenticationResultDto, ApiError> {
    Ok(AuthenticationResultDto {
        user: user_to_dto(db, user, server_id).await?,
        session_info: SessionInfoDto {
            id: token.access_token.clone(),
            user_id: user.id,
            user_name: user.name.clone(),
            client: token.client.clone(),
            last_activity_date: OffsetDateTime::now_utc()
                .format(&Rfc3339)
                .unwrap_or_else(|_| "1970-01-01T00:00:00Z".to_string()),
            device_name: token.device_name.clone(),
            device_id: token.device_id.clone(),
            application_version: token.version.clone(),
            is_active: true,
        },
        access_token: token.access_token.clone(),
        server_id,
    })
}

fn session_to_json(
    session: &DeviceSession,
    active_playback: Option<&ActivePlaybackSession>,
    server_id: &str,
) -> serde_json::Value {
    let capabilities = session.capabilities.as_ref();
    serde_json::json!({
        "Id": session.access_token,
        "UserId": session.user_id,
        "UserName": session.user_name,
        "Client": session.client,
        "LastActivityDate": format_time_for_json(session.last_activity_at),
        "DeviceName": session.device_name,
        "DeviceId": session.device_id,
        "ApplicationVersion": session.version,
        "IsActive": true,
        "SupportsRemoteControl": capability_bool(capabilities, "SupportsRemoteControl")
            || capability_bool(capabilities, "SupportsMediaControl"),
        "PlayableMediaTypes": capability_array(capabilities, "PlayableMediaTypes"),
        "SupportedCommands": capability_array(capabilities, "SupportedCommands"),
        "NowPlayingItem": active_playback.map(|playback| media_item_to_json(&playback.item, server_id)),
        "PlayState": active_playback.map(active_playback_state_json),
        "NowViewingItem": null,
    })
}

fn capability_bool(capabilities: Option<&serde_json::Value>, key: &str) -> bool {
    capabilities
        .and_then(|capabilities| capabilities.get(key))
        .and_then(serde_json::Value::as_bool)
        .unwrap_or(false)
}

fn capability_array(capabilities: Option<&serde_json::Value>, key: &str) -> serde_json::Value {
    capabilities
        .and_then(|capabilities| capabilities.get(key))
        .filter(|value| value.is_array())
        .cloned()
        .unwrap_or_else(|| serde_json::json!([]))
}

fn active_playback_state_json(playback: &ActivePlaybackSession) -> serde_json::Value {
    serde_json::json!({
        "PositionTicks": playback.position_ticks,
        "CanSeek": true,
        "IsPaused": playback.is_paused,
        "IsMuted": false,
        "VolumeLevel": 100,
        "AudioStreamIndex": playback.audio_stream_index,
        "SubtitleStreamIndex": playback.subtitle_stream_index,
        "MediaSourceId": playback.media_source_id.clone(),
        "PlayMethod": "DirectPlay",
        "RepeatMode": "RepeatNone",
    })
}

async fn require_user(
    db: &Database,
    headers: &HeaderMap,
    query_token: Option<&str>,
) -> Result<(User, DeviceToken), ApiError> {
    let token = bearer_token(headers)
        .or_else(|| query_token.map(ToOwned::to_owned))
        .ok_or_else(|| ApiError::unauthorized("Missing token"))?;
    match db.user_by_token(&token).await {
        Ok(auth) => Ok(auth),
        Err(_) => db
            .user_by_api_key(&token)
            .await
            .map_err(|_| ApiError::unauthorized("Invalid token")),
    }
}

async fn require_request_user(
    db: &Database,
    headers: &HeaderMap,
    query_token: Option<&str>,
) -> Result<User, ApiError> {
    require_user(db, headers, query_token)
        .await
        .map(|(user, _)| user)
}

async fn require_admin(
    db: &Database,
    headers: &HeaderMap,
    query_token: Option<&str>,
) -> Result<User, ApiError> {
    let user = require_request_user(db, headers, query_token).await?;
    if user.is_administrator {
        Ok(user)
    } else {
        Err(ApiError::forbidden("Administrator access required"))
    }
}

fn ensure_user_access(auth_user: &User, requested_user_id: Uuid) -> Result<(), ApiError> {
    if auth_user.id == requested_user_id || auth_user.is_administrator {
        Ok(())
    } else {
        Err(ApiError::forbidden("User access denied"))
    }
}

async fn require_admin_or_startup_incomplete(
    db: &Database,
    headers: &HeaderMap,
    query_token: Option<&str>,
) -> Result<(), ApiError> {
    require_admin_or_startup_incomplete_user(db, headers, query_token)
        .await
        .map(|_| ())
}

async fn require_admin_or_startup_incomplete_user(
    db: &Database,
    headers: &HeaderMap,
    query_token: Option<&str>,
) -> Result<Option<User>, ApiError> {
    if !db.server_state().await?.startup_wizard_completed {
        return Ok(None);
    }

    require_admin(db, headers, query_token).await.map(Some)
}

async fn require_user_or_startup_incomplete(
    db: &Database,
    headers: &HeaderMap,
    query_token: Option<&str>,
) -> Result<(), ApiError> {
    if !db.server_state().await?.startup_wizard_completed {
        return Ok(());
    }

    require_request_user(db, headers, query_token)
        .await
        .map(|_| ())
}

async fn require_startup_wizard_incomplete(db: &Database) -> Result<(), ApiError> {
    let server = db.server_state().await?;
    if server.startup_wizard_completed {
        return Err(ApiError::forbidden("Startup wizard is already complete"));
    }
    Ok(())
}

fn bearer_token(headers: &HeaderMap) -> Option<String> {
    for name in ["x-emby-token", "x-mediabrowser-token"] {
        if let Some(value) = headers.get(name).and_then(|value| value.to_str().ok())
            && !value.is_empty()
        {
            return Some(value.to_string());
        }
    }

    headers
        .get("authorization")
        .or_else(|| headers.get("x-emby-authorization"))
        .and_then(|value| value.to_str().ok())
        .and_then(parse_authorization_token)
}

fn client_auth_from_headers(headers: &HeaderMap) -> ClientAuth {
    let mut auth = ClientAuth {
        client: "Jellyfin Web".to_string(),
        device: "Browser".to_string(),
        device_id: Uuid::new_v4().to_string(),
        version: env!("CARGO_PKG_VERSION").to_string(),
    };

    if let Some(header) = headers
        .get("authorization")
        .or_else(|| headers.get("x-emby-authorization"))
        .and_then(|value| value.to_str().ok())
    {
        for (key, value) in parse_media_browser_pairs(header) {
            match key.as_str() {
                "client" => auth.client = value,
                "device" => auth.device = value,
                "deviceid" => auth.device_id = value,
                "version" => auth.version = value,
                _ => {}
            }
        }
    }

    auth
}

fn parse_authorization_token(header: &str) -> Option<String> {
    parse_media_browser_pairs(header)
        .into_iter()
        .find_map(|(key, value)| (key == "token").then_some(value))
}

fn parse_media_browser_pairs(header: &str) -> Vec<(String, String)> {
    let payload = header
        .strip_prefix("MediaBrowser ")
        .or_else(|| header.strip_prefix("Emby "))
        .unwrap_or(header);

    payload
        .split(',')
        .filter_map(|part| {
            let (key, value) = part.trim().split_once('=')?;
            Some((
                key.trim().to_ascii_lowercase(),
                value.trim().trim_matches('"').to_string(),
            ))
        })
        .collect()
}

#[derive(Debug)]
pub struct ApiError {
    status: StatusCode,
    error: anyhow::Error,
}

impl<E> From<E> for ApiError
where
    E: Into<anyhow::Error>,
{
    fn from(error: E) -> Self {
        Self {
            status: StatusCode::INTERNAL_SERVER_ERROR,
            error: error.into(),
        }
    }
}

impl ApiError {
    fn bad_request(message: impl Into<String>) -> Self {
        Self {
            status: StatusCode::BAD_REQUEST,
            error: anyhow::anyhow!(message.into()),
        }
    }

    fn unauthorized(message: impl Into<String>) -> Self {
        Self {
            status: StatusCode::UNAUTHORIZED,
            error: anyhow::anyhow!(message.into()),
        }
    }

    fn forbidden(message: impl Into<String>) -> Self {
        Self {
            status: StatusCode::FORBIDDEN,
            error: anyhow::anyhow!(message.into()),
        }
    }

    fn not_found(message: impl Into<String>) -> Self {
        Self {
            status: StatusCode::NOT_FOUND,
            error: anyhow::anyhow!(message.into()),
        }
    }

    fn conflict(message: impl Into<String>) -> Self {
        Self {
            status: StatusCode::CONFLICT,
            error: anyhow::anyhow!(message.into()),
        }
    }
}

impl IntoResponse for ApiError {
    fn into_response(self) -> axum::response::Response {
        tracing::error!(error = %self.error, status = %self.status, "request failed");
        (
            self.status,
            Json(serde_json::json!({
                "Error": self.status.canonical_reason().unwrap_or("Error"),
                "Message": self.error.to_string(),
            })),
        )
            .into_response()
    }
}

#[cfg(test)]
mod tests {
    use std::fs;

    use super::{
        AppState, COMPATIBLE_SERVER_VERSION, DEFAULT_AUTHENTICATION_PROVIDER_ID,
        DEFAULT_PASSWORD_RESET_PROVIDER_ID, cleanup_orphan_hls_transcode_dirs,
        cleanup_terminal_hls_transcodes, default_audio_stream_index, default_subtitle_stream_index,
        hls_transcode_dedupe_key, load_countries, load_cultures, parse_authorization_token,
        parse_jellyfin_uuid, parse_media_browser_pairs, reconcile_transcode_sessions_on_startup,
        router, spawn_hls_transcode_task, subscribe_playback_events, transcode_dedupe_lock,
    };
    use axum::{
        body::Body,
        http::{Method, Request, StatusCode, header},
    };
    use http_body_util::BodyExt;
    use jellyrin_core::{FfmpegCommandSpec, MediaItem, TranscodeStreamSelection};
    use jellyrin_db::{Database, UpsertTranscodeSession};
    use serde_json::{Value, json};
    use std::sync::Arc;
    use tower::ServiceExt;

    fn assert_hls_transcode_playback_info(info: &Value, item_id: &str, api_key: &str) -> String {
        assert_eq!(info["ErrorCode"], Value::Null);
        assert!(
            info["PlaySessionId"]
                .as_str()
                .is_some_and(|id| !id.is_empty())
        );
        let media_source = &info["MediaSources"][0];
        assert_eq!(media_source["SupportsDirectPlay"], false);
        assert_eq!(media_source["SupportsDirectStream"], false);
        assert_eq!(media_source["SupportsTranscoding"], true);
        assert_eq!(media_source["TranscodingSubProtocol"], "hls");
        assert_eq!(media_source["TranscodingContainer"], "ts");
        assert_eq!(media_source["Container"], "ts");
        assert_eq!(media_source["DirectStreamUrl"], Value::Null);
        let play_session_id = info["PlaySessionId"].as_str().unwrap();
        assert_eq!(
            media_source["TranscodingUrl"],
            format!(
                "/Videos/{item_id}/master.m3u8?PlaySessionId={play_session_id}&api_key={api_key}"
            )
        );
        play_session_id.to_string()
    }

    #[test]
    fn parses_media_browser_authorization_header() {
        let pairs = parse_media_browser_pairs(
            r#"MediaBrowser Client="Jellyfin Web", Device="Firefox", DeviceId="abc", Version="1", Token="tok""#,
        );
        assert!(pairs.contains(&("client".to_string(), "Jellyfin Web".to_string())));
        assert!(pairs.contains(&("deviceid".to_string(), "abc".to_string())));
        assert_eq!(
            parse_authorization_token(
                r#"MediaBrowser Client="Jellyfin Web", DeviceId="abc", Token="tok""#
            ),
            Some("tok".to_string())
        );
    }

    #[test]
    fn hls_transcode_dedupe_key_tracks_user_item_and_stream_selection() {
        let user_id = uuid::Uuid::parse_str("aaaaaaaa-aaaa-aaaa-aaaa-aaaaaaaaaaaa").unwrap();
        let item = MediaItem {
            id: uuid::Uuid::parse_str("bbbbbbbb-bbbb-bbbb-bbbb-bbbbbbbbbbbb").unwrap(),
            virtual_folder_id: uuid::Uuid::parse_str("cccccccc-cccc-cccc-cccc-cccccccccccc")
                .unwrap(),
            name: "Movie".to_string(),
            path: "/media/Movie.mkv".to_string(),
            media_type: "Video".to_string(),
            collection_type: Some("movies".to_string()),
            file_size: None,
            runtime_ticks: None,
            bitrate: None,
            width: None,
            height: None,
            media_streams: Vec::new(),
            created_at: time::OffsetDateTime::UNIX_EPOCH,
            updated_at: time::OffsetDateTime::UNIX_EPOCH,
        };
        let selection = TranscodeStreamSelection {
            video_stream_index: Some(0),
            audio_stream_index: Some(1),
            subtitle_stream_index: Some(-1),
        };
        let key = hls_transcode_dedupe_key(user_id, &item, &selection, 12_345_000_000);
        assert_eq!(
            key,
            "hls:ts:aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa:bbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb:bbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb:0:1:-1:12345000000"
        );

        let changed_selection = TranscodeStreamSelection {
            audio_stream_index: Some(2),
            ..selection
        };
        assert_ne!(
            key,
            hls_transcode_dedupe_key(user_id, &item, &changed_selection, 12_345_000_000)
        );
        assert_ne!(
            key,
            hls_transcode_dedupe_key(user_id, &item, &selection, 12_346_000_000)
        );
    }

    #[tokio::test]
    async fn hls_transcode_dedupe_lock_is_shared_per_key() {
        let first = transcode_dedupe_lock("same-key").await;
        let second = transcode_dedupe_lock("same-key").await;
        let other = transcode_dedupe_lock("other-key").await;

        assert!(Arc::ptr_eq(&first, &second));
        assert!(!Arc::ptr_eq(&first, &other));
    }

    #[test]
    fn default_stream_indexes_follow_jellyfin_selection_basics() {
        let streams = vec![
            json!({ "Type": "Audio", "Index": 1, "IsDefault": false }),
            json!({ "Type": "Audio", "Index": 5, "IsDefault": true }),
            json!({ "Type": "Subtitle", "Index": 2, "IsDefault": false }),
            json!({ "Type": "Subtitle", "Index": "8", "IsDefault": true }),
        ];

        assert_eq!(default_audio_stream_index(&streams), Some(5));
        assert_eq!(default_subtitle_stream_index(&streams), Some(8));

        let streams = vec![
            json!({ "Type": "Audio", "Index": "3" }),
            json!({ "Type": "Subtitle", "Index": 4, "IsDefault": false }),
        ];
        assert_eq!(default_audio_stream_index(&streams), Some(3));
        assert_eq!(default_subtitle_stream_index(&streams), None);
    }

    #[test]
    fn bundled_localization_lists_match_jellyfin_scale() {
        let cultures = load_cultures();
        let countries = load_countries();

        assert!(cultures.len() > 400);
        assert!(countries.len() >= 140);
        assert!(cultures.iter().any(|culture| {
            culture.two_letter_iso_language_name == "es"
                && culture.display_name == "Spanish; Castilian"
        }));
        assert!(
            countries
                .iter()
                .any(|country| country.two_letter_iso_region_name == "ES")
        );
    }

    #[tokio::test]
    async fn startup_wizard_and_login_http_flow() {
        let db = Database::connect("sqlite::memory:").await.unwrap();
        let app = router(AppState {
            db,
            web_dir: ".".into(),
            log_dir: ".".into(),
            local_address: "http://127.0.0.1:8097".to_string(),
        });

        let response = app
            .clone()
            .oneshot(
                Request::builder()
                    .uri("/Startup/Configuration")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::OK);
        let response = app
            .clone()
            .oneshot(
                Request::builder()
                    .method(Method::POST)
                    .uri("/Startup/User")
                    .header(header::CONTENT_TYPE, "application/json")
                    .body(Body::from(
                        json!({ "Name": "admin", "Password": "secret" }).to_string(),
                    ))
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::NO_CONTENT);
        let response = app
            .clone()
            .oneshot(
                Request::builder()
                    .method(Method::POST)
                    .uri("/Startup/Complete")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::NO_CONTENT);

        let response = app
            .clone()
            .oneshot(
                Request::builder()
                    .method(Method::POST)
                    .uri("/Users/AuthenticateByName")
                    .header(header::CONTENT_TYPE, "application/json")
                    .header(
                        header::AUTHORIZATION,
                        r#"MediaBrowser Client="Jellyfin Web", Device="Firefox", DeviceId="test-device", Version="dev""#,
                    )
                    .body(Body::from(
                        json!({ "Username": "admin", "Pw": "secret" }).to_string(),
                    ))
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::OK);

        let body = response.into_body().collect().await.unwrap().to_bytes();
        let result: Value = serde_json::from_slice(&body).unwrap();
        let token = result["AccessToken"].as_str().unwrap();
        assert!(!token.is_empty());
        assert_eq!(result["User"]["Name"], "admin");

        let response = app
            .clone()
            .oneshot(
                Request::builder()
                    .uri("/System/Info")
                    .header(
                        header::AUTHORIZATION,
                        format!(
                            r#"MediaBrowser Client="Jellyfin Web", DeviceId="test-device", Token="{token}""#
                        ),
                    )
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::OK);

        let response = app
            .clone()
            .oneshot(
                Request::builder()
                    .uri("/Sessions")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::UNAUTHORIZED);

        let response = app
            .oneshot(
                Request::builder()
                    .uri("/Sessions")
                    .header("X-Emby-Token", token)
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::OK);
        let body = response.into_body().collect().await.unwrap().to_bytes();
        let sessions: Value = serde_json::from_slice(&body).unwrap();
        assert_eq!(sessions.as_array().unwrap().len(), 1);
        assert_eq!(sessions[0]["UserName"], "admin");
        assert_eq!(sessions[0]["DeviceId"], "test-device");
        assert_eq!(sessions[0]["Client"], "Jellyfin Web");
        assert_eq!(sessions[0]["IsActive"], true);
    }

    #[tokio::test]
    async fn startup_mutations_are_blocked_after_wizard_completion() {
        let db = Database::connect("sqlite::memory:").await.unwrap();
        let app = router(AppState {
            db,
            web_dir: ".".into(),
            log_dir: ".".into(),
            local_address: "http://127.0.0.1:8097".to_string(),
        });

        let response = app
            .clone()
            .oneshot(
                Request::builder()
                    .method(Method::POST)
                    .uri("/Startup/User")
                    .header(header::CONTENT_TYPE, "application/json")
                    .body(Body::from(
                        json!({ "Name": "admin", "Password": "secret" }).to_string(),
                    ))
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::NO_CONTENT);

        let response = app
            .clone()
            .oneshot(
                Request::builder()
                    .method(Method::POST)
                    .uri("/Startup/Complete")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::NO_CONTENT);

        let response = app
            .oneshot(
                Request::builder()
                    .method(Method::POST)
                    .uri("/Startup/User")
                    .header(header::CONTENT_TYPE, "application/json")
                    .body(Body::from(
                        json!({ "Name": "admin", "Password": "changed" }).to_string(),
                    ))
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::FORBIDDEN);
    }

    #[tokio::test]
    async fn authenticated_routes_accept_api_keys() {
        let db = Database::connect("sqlite::memory:").await.unwrap();
        let user = db
            .update_first_user("admin".to_string(), "secret")
            .await
            .unwrap();
        let api_key = db
            .issue_api_key_for_user(user.id, "test-key")
            .await
            .unwrap();
        let app = router(AppState {
            db,
            web_dir: ".".into(),
            log_dir: ".".into(),
            local_address: "http://127.0.0.1:8097".to_string(),
        });

        let response = app
            .oneshot(
                Request::builder()
                    .uri(format!("/System/Info?api_key={api_key}"))
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::OK);
    }

    #[tokio::test]
    async fn auth_keys_round_trip_with_admin_session_token() {
        let db = Database::connect("sqlite::memory:").await.unwrap();
        db.update_first_user("admin".to_string(), "secret")
            .await
            .unwrap();
        let app = router(AppState {
            db,
            web_dir: ".".into(),
            log_dir: ".".into(),
            local_address: "http://127.0.0.1:8097".to_string(),
        });

        let response = app
            .clone()
            .oneshot(
                Request::builder()
                    .uri("/Auth/Keys")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::UNAUTHORIZED);

        let response = app
            .clone()
            .oneshot(
                Request::builder()
                    .method(Method::POST)
                    .uri("/Users/AuthenticateByName")
                    .header(
                        header::AUTHORIZATION,
                        r#"MediaBrowser Client="Jellyfin Web", Device="Auth Keys Test", DeviceId="auth-keys-device", Version="dev""#,
                    )
                    .header(header::CONTENT_TYPE, "application/json")
                    .body(Body::from(
                        json!({ "Username": "admin", "Pw": "secret" }).to_string(),
                    ))
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::OK);
        let body = response.into_body().collect().await.unwrap().to_bytes();
        let login: Value = serde_json::from_slice(&body).unwrap();
        let session_token = login["AccessToken"].as_str().unwrap();

        let response = app
            .clone()
            .oneshot(
                Request::builder()
                    .uri("/Auth/Keys")
                    .header("X-Emby-Token", session_token)
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::OK);
        let body = response.into_body().collect().await.unwrap().to_bytes();
        let keys: Value = serde_json::from_slice(&body).unwrap();
        assert_eq!(keys["Items"].as_array().unwrap().len(), 0);
        assert_eq!(keys["TotalRecordCount"], 0);

        let response = app
            .clone()
            .oneshot(
                Request::builder()
                    .method(Method::POST)
                    .uri("/Auth/Keys?app=QA%20Client")
                    .header("X-Emby-Token", session_token)
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::NO_CONTENT);

        let response = app
            .clone()
            .oneshot(
                Request::builder()
                    .method(Method::POST)
                    .uri("/auth/keys?app=")
                    .header("X-Emby-Token", session_token)
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::BAD_REQUEST);

        let response = app
            .clone()
            .oneshot(
                Request::builder()
                    .uri("/auth/keys")
                    .header("X-Emby-Token", session_token)
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::OK);
        let body = response.into_body().collect().await.unwrap().to_bytes();
        let keys: Value = serde_json::from_slice(&body).unwrap();
        let item = keys["Items"]
            .as_array()
            .unwrap()
            .iter()
            .find(|item| item["AppName"] == "QA Client")
            .unwrap();
        let api_key = item["AccessToken"].as_str().unwrap();
        assert!(!api_key.is_empty());
        assert_eq!(item["UserName"], "admin");
        assert_eq!(item["IsActive"], true);
        assert!(item["DateCreated"].as_str().unwrap().ends_with('Z'));

        let response = app
            .clone()
            .oneshot(
                Request::builder()
                    .uri(format!("/System/Info?api_key={api_key}"))
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::OK);

        let response = app
            .clone()
            .oneshot(
                Request::builder()
                    .method(Method::DELETE)
                    .uri(format!("/Auth/Keys/{api_key}"))
                    .header("X-Emby-Token", session_token)
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::NO_CONTENT);

        let response = app
            .clone()
            .oneshot(
                Request::builder()
                    .uri(format!("/System/Info?api_key={api_key}"))
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::UNAUTHORIZED);

        let response = app
            .oneshot(
                Request::builder()
                    .uri("/Auth/Keys")
                    .header("X-Emby-Token", session_token)
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        let body = response.into_body().collect().await.unwrap().to_bytes();
        let keys: Value = serde_json::from_slice(&body).unwrap();
        assert_eq!(keys["Items"].as_array().unwrap().len(), 0);
        assert_eq!(keys["TotalRecordCount"], 0);
    }

    #[tokio::test]
    async fn backup_endpoints_list_create_manifest_and_reject_restore() {
        let db = Database::connect("sqlite::memory:").await.unwrap();
        let user = db
            .update_first_user("admin".to_string(), "secret")
            .await
            .unwrap();
        let api_key = db
            .issue_api_key_for_user(user.id, "test-key")
            .await
            .unwrap();
        let app = router(AppState {
            db,
            web_dir: ".".into(),
            log_dir: ".".into(),
            local_address: "http://127.0.0.1:8097".to_string(),
        });

        let response = app
            .clone()
            .oneshot(
                Request::builder()
                    .uri("/Backup")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::UNAUTHORIZED);

        let response = app
            .clone()
            .oneshot(
                Request::builder()
                    .uri("/backup")
                    .header("X-Emby-Token", &api_key)
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::OK);
        let body = response.into_body().collect().await.unwrap().to_bytes();
        let backups: Value = serde_json::from_slice(&body).unwrap();
        assert_eq!(backups.as_array().unwrap().len(), 0);

        let response = app
            .clone()
            .oneshot(
                Request::builder()
                    .method(Method::POST)
                    .uri("/Backup/Create")
                    .header("X-Emby-Token", &api_key)
                    .header(header::CONTENT_TYPE, "application/json")
                    .body(Body::from(
                        json!({
                            "Metadata": true,
                            "Subtitles": false,
                            "Trickplay": true
                        })
                        .to_string(),
                    ))
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::OK);
        let body = response.into_body().collect().await.unwrap().to_bytes();
        let created: Value = serde_json::from_slice(&body).unwrap();
        let path = created["Path"].as_str().unwrap();
        assert!(path.starts_with("jellyrin-backup-"));
        assert_eq!(created["ServerVersion"], COMPATIBLE_SERVER_VERSION);
        assert_eq!(created["BackupEngineVersion"], "1");
        assert!(created["DateCreated"].as_str().unwrap().ends_with('Z'));
        assert_eq!(created["Options"]["Metadata"], true);
        assert_eq!(created["Options"]["Trickplay"], true);
        assert_eq!(created["Options"]["Subtitles"], false);
        assert_eq!(created["Options"]["Database"], true);

        let response = app
            .clone()
            .oneshot(
                Request::builder()
                    .uri("/Backup")
                    .header("X-Emby-Token", &api_key)
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        let body = response.into_body().collect().await.unwrap().to_bytes();
        let backups: Value = serde_json::from_slice(&body).unwrap();
        assert_eq!(backups.as_array().unwrap().len(), 1);
        assert_eq!(backups[0]["Path"], path);

        let response = app
            .clone()
            .oneshot(
                Request::builder()
                    .uri(format!("/Backup/Manifest?path={path}"))
                    .header("X-Emby-Token", &api_key)
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::OK);
        let body = response.into_body().collect().await.unwrap().to_bytes();
        let manifest: Value = serde_json::from_slice(&body).unwrap();
        assert_eq!(manifest["Path"], path);

        let response = app
            .clone()
            .oneshot(
                Request::builder()
                    .uri("/Backup/Manifest?path=missing.zip")
                    .header("X-Emby-Token", &api_key)
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::NOT_FOUND);

        let response = app
            .clone()
            .oneshot(
                Request::builder()
                    .method(Method::POST)
                    .uri("/Backup/Restore")
                    .header("X-Emby-Token", &api_key)
                    .header(header::CONTENT_TYPE, "application/json")
                    .body(Body::from(
                        json!({ "ArchiveFileName": "missing.zip" }).to_string(),
                    ))
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::NOT_FOUND);

        let response = app
            .oneshot(
                Request::builder()
                    .method(Method::POST)
                    .uri("/backup/restore")
                    .header("X-Emby-Token", &api_key)
                    .header(header::CONTENT_TYPE, "application/json")
                    .body(Body::from(json!({ "ArchiveFileName": path }).to_string()))
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::CONFLICT);
    }

    #[tokio::test]
    async fn scheduled_tasks_endpoints_expose_library_scan_stub() {
        let db = Database::connect("sqlite::memory:").await.unwrap();
        let user = db
            .update_first_user("admin".to_string(), "secret")
            .await
            .unwrap();
        let api_key = db
            .issue_api_key_for_user(user.id, "test-key")
            .await
            .unwrap();
        let app = router(AppState {
            db,
            web_dir: ".".into(),
            log_dir: ".".into(),
            local_address: "http://127.0.0.1:8097".to_string(),
        });

        let response = app
            .clone()
            .oneshot(
                Request::builder()
                    .uri("/ScheduledTasks?IsEnabled=true")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::UNAUTHORIZED);

        let response = app
            .clone()
            .oneshot(
                Request::builder()
                    .uri(format!("/ScheduledTasks?IsEnabled=true&api_key={api_key}"))
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::OK);
        let body = response.into_body().collect().await.unwrap().to_bytes();
        let tasks: Value = serde_json::from_slice(&body).unwrap();
        assert_eq!(tasks.as_array().unwrap().len(), 1);
        assert_eq!(tasks[0]["Id"], "scan-media-library");
        assert_eq!(tasks[0]["Key"], "RefreshLibrary");
        assert_eq!(tasks[0]["Name"], "Scan Media Library");
        assert_eq!(tasks[0]["State"], "Idle");
        assert_eq!(tasks[0]["IsHidden"], false);
        assert_eq!(tasks[0]["Triggers"].as_array().unwrap().len(), 1);

        let response = app
            .clone()
            .oneshot(
                Request::builder()
                    .uri("/ScheduledTasks/scan-media-library")
                    .header("X-Emby-Token", &api_key)
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::OK);
        let body = response.into_body().collect().await.unwrap().to_bytes();
        let task: Value = serde_json::from_slice(&body).unwrap();
        assert_eq!(task["Id"], "scan-media-library");

        for endpoint in [
            "/ScheduledTasks/Running/scan-media-library",
            "/scheduledtasks/running/scan-media-library",
        ] {
            let response = app
                .clone()
                .oneshot(
                    Request::builder()
                        .method(Method::POST)
                        .uri(endpoint)
                        .header("X-Emby-Token", &api_key)
                        .body(Body::empty())
                        .unwrap(),
                )
                .await
                .unwrap();
            assert_eq!(response.status(), StatusCode::NO_CONTENT, "{endpoint}");
        }

        let mut task = json!({});
        for _ in 0..20 {
            let response = app
                .clone()
                .oneshot(
                    Request::builder()
                        .uri("/ScheduledTasks/scan-media-library")
                        .header("X-Emby-Token", &api_key)
                        .body(Body::empty())
                        .unwrap(),
                )
                .await
                .unwrap();
            assert_eq!(response.status(), StatusCode::OK);
            let body = response.into_body().collect().await.unwrap().to_bytes();
            task = serde_json::from_slice(&body).unwrap();
            if task["State"] == "Idle" && task["LastExecutionResult"]["Status"] == "Completed" {
                break;
            }
            tokio::time::sleep(std::time::Duration::from_millis(25)).await;
        }
        assert_eq!(task["State"], "Idle");
        assert_eq!(task["LastExecutionResult"]["Status"], "Completed");
        assert_eq!(task["LastExecutionResult"]["Key"], "RefreshLibrary");

        let response = app
            .clone()
            .oneshot(
                Request::builder()
                    .method(Method::DELETE)
                    .uri("/ScheduledTasks/Running/scan-media-library")
                    .header("X-Emby-Token", &api_key)
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::NO_CONTENT);

        let response = app
            .clone()
            .oneshot(
                Request::builder()
                    .method(Method::POST)
                    .uri("/ScheduledTasks/scan-media-library/Triggers")
                    .header("X-Emby-Token", &api_key)
                    .header(header::CONTENT_TYPE, "application/json")
                    .body(Body::from(json!([]).to_string()))
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::NO_CONTENT);

        let response = app
            .oneshot(
                Request::builder()
                    .method(Method::POST)
                    .uri("/ScheduledTasks/unknown/Triggers")
                    .header("X-Emby-Token", &api_key)
                    .header(header::CONTENT_TYPE, "application/json")
                    .body(Body::from(json!([]).to_string()))
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::NOT_FOUND);
    }

    #[tokio::test]
    async fn admin_dashboard_shell_stubs_avoid_404s() {
        let db = Database::connect("sqlite::memory:").await.unwrap();
        let user = db
            .update_first_user("admin".to_string(), "secret")
            .await
            .unwrap();
        let api_key = db
            .issue_api_key_for_user(user.id, "test-key")
            .await
            .unwrap();
        let storage_root = tempfile::tempdir().unwrap();
        let storage_path = storage_root.path().join("movies");
        let log_dir = storage_root.path().join("logs");
        std::fs::create_dir_all(&storage_path).unwrap();
        std::fs::create_dir_all(&log_dir).unwrap();
        std::fs::write(log_dir.join("jellyrin.log"), "first line\nsecond line\n").unwrap();
        std::fs::write(log_dir.join("older.log"), "older\n").unwrap();
        std::fs::write(log_dir.join("notes.json"), "{}\n").unwrap();
        std::fs::create_dir(log_dir.join("nested")).unwrap();
        #[cfg(unix)]
        std::os::unix::fs::symlink("/etc/passwd", log_dir.join("passwd.log")).unwrap();
        let storage_folder = db
            .upsert_virtual_folder(
                "Storage Movies",
                Some("movies"),
                vec![storage_path.to_string_lossy().to_string()],
            )
            .await
            .unwrap();
        let app = router(AppState {
            db,
            web_dir: ".".into(),
            log_dir: log_dir.clone(),
            local_address: "http://127.0.0.1:8097".to_string(),
        });

        for endpoint in [
            "/System/Ping",
            "/System/Info/Storage",
            "/System/ActivityLog/Entries",
            "/System/Configuration",
            "/System/Configuration/MetadataOptions/Default",
            "/System/Configuration/branding",
            "/Dashboard/web/ConfigurationPages",
            "/web/ConfigurationPages?enableInMainMenu=true",
            "/web/configurationpages?enableInMainMenu=true",
            "/Dashboard/web/ConfigurationPage?name=home.html",
            "/web/ConfigurationPage?name=home.html",
            "/Devices",
            "/Devices/Info?Id=test-device",
            "/Devices/Options?Id=test-device",
            "/Session/Sessions",
            "/Sessions",
            "/Plugins",
            "/plugins",
            "/Plugins/test-plugin/Manifest",
            "/Packages",
            "/packages",
            "/Repositories",
            "/repositories",
            "/LiveTv/Info",
            "/livetv/info",
            "/LiveTv/GuideInfo?UserId=test-user",
            "/LiveTv/Channels?UserId=test-user",
            "/LiveTv/Programs?UserId=test-user",
            "/LiveTv/RecommendedPrograms?UserId=test-user",
            "/LiveTv/Recordings?UserId=test-user",
            "/LiveTv/RecordingGroups?UserId=test-user",
            "/LiveTv/Timers",
            "/LiveTv/SeriesTimers",
            "/Channels?UserId=test-user&StartIndex=3&SupportsMediaDeletion=true",
            "/channels?supportsLatestItems=true&isFavorite=false",
            "/Channels/Features",
            "/Localization/ParentalRatings",
            "/localization/parentalratings",
        ] {
            let response = app
                .clone()
                .oneshot(
                    Request::builder()
                        .uri(endpoint)
                        .header("X-Emby-Token", &api_key)
                        .body(Body::empty())
                        .unwrap(),
                )
                .await
                .unwrap();
            assert_eq!(response.status(), StatusCode::OK, "{endpoint}");
        }

        let response = app
            .clone()
            .oneshot(
                Request::builder()
                    .uri("/System/Configuration")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        let body = response.into_body().collect().await.unwrap().to_bytes();
        let config: Value = serde_json::from_slice(&body).unwrap();
        assert_eq!(config["ServerName"], "Jellyrin");
        assert_eq!(config["EnableRemoteAccess"], false);
        assert_eq!(config["ContentTypes"].as_array().unwrap().len(), 0);

        for endpoint in ["/System/Restart", "/System/Shutdown"] {
            let response = app
                .clone()
                .oneshot(
                    Request::builder()
                        .method(Method::POST)
                        .uri(endpoint)
                        .body(Body::empty())
                        .unwrap(),
                )
                .await
                .unwrap();
            assert_eq!(response.status(), StatusCode::UNAUTHORIZED, "{endpoint}");
        }

        for endpoint in ["/System/Restart", "/system/shutdown"] {
            let response = app
                .clone()
                .oneshot(
                    Request::builder()
                        .method(Method::POST)
                        .uri(endpoint)
                        .header("X-Emby-Token", &api_key)
                        .body(Body::empty())
                        .unwrap(),
                )
                .await
                .unwrap();
            assert_eq!(response.status(), StatusCode::NO_CONTENT, "{endpoint}");
        }

        let response = app
            .clone()
            .oneshot(
                Request::builder()
                    .uri("/System/Info/Storage")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::UNAUTHORIZED);

        let response = app
            .clone()
            .oneshot(
                Request::builder()
                    .uri("/System/Info/Storage")
                    .header("X-Emby-Token", &api_key)
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::OK);
        let body = response.into_body().collect().await.unwrap().to_bytes();
        let storage: Value = serde_json::from_slice(&body).unwrap();
        assert!(
            !storage["ProgramDataFolder"]["Path"]
                .as_str()
                .unwrap()
                .is_empty()
        );
        assert_eq!(storage["WebFolder"]["Path"], ".");
        assert_eq!(
            storage["LogFolder"]["Path"],
            log_dir.to_string_lossy().to_string()
        );
        assert_eq!(storage["Libraries"][0]["Id"], storage_folder.id.to_string());
        assert_eq!(storage["Libraries"][0]["Name"], "Storage Movies");
        assert_eq!(
            storage["Libraries"][0]["Folders"][0]["Path"],
            storage_path.to_string_lossy().to_string()
        );

        let response = app
            .clone()
            .oneshot(
                Request::builder()
                    .uri("/web/ConfigurationPages")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::UNAUTHORIZED);

        let response = app
            .clone()
            .oneshot(
                Request::builder()
                    .uri("/System/ActivityLog/Entries?StartIndex=0&Limit=20")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::UNAUTHORIZED);

        let response = app
            .clone()
            .oneshot(
                Request::builder()
                    .uri("/System/ActivityLog/Entries?StartIndex=0&Limit=20")
                    .header("X-Emby-Token", &api_key)
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::OK);
        let body = response.into_body().collect().await.unwrap().to_bytes();
        let activity: Value = serde_json::from_slice(&body).unwrap();
        assert_eq!(activity["TotalRecordCount"], 0);
        assert_eq!(activity["Items"].as_array().unwrap().len(), 0);

        let response = app
            .clone()
            .oneshot(
                Request::builder()
                    .uri("/System/Logs")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::UNAUTHORIZED);

        let response = app
            .clone()
            .oneshot(
                Request::builder()
                    .uri("/System/Logs")
                    .header("X-Emby-Token", &api_key)
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::OK);
        let body = response.into_body().collect().await.unwrap().to_bytes();
        let logs: Value = serde_json::from_slice(&body).unwrap();
        let logs = logs.as_array().unwrap();
        assert_eq!(logs.len(), 2);
        assert!(logs.iter().any(|log| log["Name"] == "jellyrin.log"));
        assert!(!logs.iter().any(|log| log["Name"] == "notes.json"));
        assert!(!logs.iter().any(|log| log["Name"] == "passwd.log"));
        assert!(logs.iter().all(|log| log["Size"].as_u64().unwrap() > 0));
        assert!(logs.iter().all(|log| log["DateModified"].is_string()));

        let response = app
            .clone()
            .oneshot(
                Request::builder()
                    .uri("/System/Logs/Log?name=jellyrin.log")
                    .header("X-Emby-Token", &api_key)
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::OK);
        let content_type = response
            .headers()
            .get(header::CONTENT_TYPE)
            .and_then(|value| value.to_str().ok())
            .unwrap();
        assert!(content_type.starts_with("text/plain"));
        let body = response.into_body().collect().await.unwrap().to_bytes();
        assert_eq!(&body[..], b"first line\nsecond line\n");

        for uri in [
            "/System/Logs/Log?name=../jellyrin.log",
            "/System/Logs/Log?name=nested/file.log",
        ] {
            let response = app
                .clone()
                .oneshot(
                    Request::builder()
                        .uri(uri)
                        .header("X-Emby-Token", &api_key)
                        .body(Body::empty())
                        .unwrap(),
                )
                .await
                .unwrap();
            assert_eq!(response.status(), StatusCode::BAD_REQUEST, "{uri}");
        }

        let response = app
            .clone()
            .oneshot(
                Request::builder()
                    .uri("/System/Logs/Log?name=missing.log")
                    .header("X-Emby-Token", &api_key)
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::NOT_FOUND);

        #[cfg(unix)]
        {
            let response = app
                .clone()
                .oneshot(
                    Request::builder()
                        .uri("/System/Logs/Log?name=passwd.log")
                        .header("X-Emby-Token", &api_key)
                        .body(Body::empty())
                        .unwrap(),
                )
                .await
                .unwrap();
            assert_eq!(response.status(), StatusCode::NOT_FOUND);
        }

        for endpoint in ["/Plugins", "/Packages", "/Plugins/test-plugin/Manifest"] {
            let response = app
                .clone()
                .oneshot(
                    Request::builder()
                        .uri(endpoint)
                        .body(Body::empty())
                        .unwrap(),
                )
                .await
                .unwrap();
            assert_eq!(response.status(), StatusCode::UNAUTHORIZED, "{endpoint}");
        }

        let response = app
            .clone()
            .oneshot(
                Request::builder()
                    .uri("/Plugins")
                    .header("X-Emby-Token", &api_key)
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        let body = response.into_body().collect().await.unwrap().to_bytes();
        let plugins: Value = serde_json::from_slice(&body).unwrap();
        assert_eq!(plugins.as_array().unwrap().len(), 0);

        let response = app
            .clone()
            .oneshot(
                Request::builder()
                    .uri("/Plugins/test-plugin/Manifest")
                    .header("X-Emby-Token", &api_key)
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        let body = response.into_body().collect().await.unwrap().to_bytes();
        let manifest: Value = serde_json::from_slice(&body).unwrap();
        assert_eq!(manifest["Guid"], "test-plugin");
        assert_eq!(manifest["Versions"].as_array().unwrap().len(), 0);

        for endpoint in [
            "/Plugins/test-plugin/Configuration",
            "/Packages/MissingPlugin",
        ] {
            let response = app
                .clone()
                .oneshot(
                    Request::builder()
                        .uri(endpoint)
                        .header("X-Emby-Token", &api_key)
                        .body(Body::empty())
                        .unwrap(),
                )
                .await
                .unwrap();
            assert_eq!(response.status(), StatusCode::NOT_FOUND, "{endpoint}");
        }

        for endpoint in [
            "/Plugins/test-plugin/1.0.0.0/Enable",
            "/Plugins/test-plugin/1.0.0.0/Disable",
            "/Packages/Installed/MissingPlugin",
        ] {
            let response = app
                .clone()
                .oneshot(
                    Request::builder()
                        .method(Method::POST)
                        .uri(endpoint)
                        .header("X-Emby-Token", &api_key)
                        .body(Body::empty())
                        .unwrap(),
                )
                .await
                .unwrap();
            assert_eq!(response.status(), StatusCode::NOT_FOUND, "{endpoint}");
        }

        for endpoint in ["/Plugins/test-plugin", "/Plugins/test-plugin/1.0.0.0"] {
            let response = app
                .clone()
                .oneshot(
                    Request::builder()
                        .method(Method::DELETE)
                        .uri(endpoint)
                        .header("X-Emby-Token", &api_key)
                        .body(Body::empty())
                        .unwrap(),
                )
                .await
                .unwrap();
            assert_eq!(response.status(), StatusCode::NOT_FOUND, "{endpoint}");
        }

        let response = app
            .clone()
            .oneshot(
                Request::builder()
                    .method(Method::DELETE)
                    .uri("/Packages/Installing/00000000-0000-0000-0000-000000000000")
                    .header("X-Emby-Token", &api_key)
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::NO_CONTENT);

        let response = app
            .clone()
            .oneshot(
                Request::builder()
                    .uri("/Repositories")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::UNAUTHORIZED);

        let response = app
            .clone()
            .oneshot(
                Request::builder()
                    .uri("/Repositories")
                    .header("X-Emby-Token", &api_key)
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::OK);
        let body = response.into_body().collect().await.unwrap().to_bytes();
        let repositories: Value = serde_json::from_slice(&body).unwrap();
        assert_eq!(repositories.as_array().unwrap().len(), 0);

        let response = app
            .clone()
            .oneshot(
                Request::builder()
                    .uri("/LiveTv/Info")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        let body = response.into_body().collect().await.unwrap().to_bytes();
        let live_tv_info: Value = serde_json::from_slice(&body).unwrap();
        assert_eq!(live_tv_info["IsEnabled"], false);
        assert_eq!(live_tv_info["Services"].as_array().unwrap().len(), 0);

        let response = app
            .clone()
            .oneshot(
                Request::builder()
                    .uri("/LiveTv/Channels")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        let body = response.into_body().collect().await.unwrap().to_bytes();
        let live_tv_channels: Value = serde_json::from_slice(&body).unwrap();
        assert_eq!(live_tv_channels["TotalRecordCount"], 0);
        assert_eq!(live_tv_channels["Items"].as_array().unwrap().len(), 0);

        let response = app
            .clone()
            .oneshot(
                Request::builder()
                    .method(Method::POST)
                    .uri("/System/Configuration")
                    .header(header::CONTENT_TYPE, "application/json")
                    .body(Body::from("{}"))
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::UNAUTHORIZED);

        let response = app
            .clone()
            .oneshot(
                Request::builder()
                    .method(Method::POST)
                    .uri("/System/Configuration")
                    .header("X-Emby-Token", &api_key)
                    .header(header::CONTENT_TYPE, "application/json")
                    .body(Body::from("{}"))
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::NO_CONTENT);

        for endpoint in [
            "/System/Configuration/Branding",
            "/system/configuration/branding",
        ] {
            let response = app
                .clone()
                .oneshot(
                    Request::builder()
                        .method(Method::POST)
                        .uri(endpoint)
                        .header(header::CONTENT_TYPE, "application/json")
                        .body(Body::from("{}"))
                        .unwrap(),
                )
                .await
                .unwrap();
            assert_eq!(response.status(), StatusCode::UNAUTHORIZED, "{endpoint}");

            let response = app
                .clone()
                .oneshot(
                    Request::builder()
                        .method(Method::POST)
                        .uri(endpoint)
                        .header("X-Emby-Token", &api_key)
                        .header(header::CONTENT_TYPE, "application/json")
                        .body(Body::from("{}"))
                        .unwrap(),
                )
                .await
                .unwrap();
            assert_eq!(response.status(), StatusCode::NO_CONTENT, "{endpoint}");
        }

        let response = app
            .clone()
            .oneshot(
                Request::builder()
                    .method(Method::POST)
                    .uri("/Devices/Options")
                    .header(header::CONTENT_TYPE, "application/json")
                    .body(Body::from("{}"))
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::UNAUTHORIZED);

        let response = app
            .clone()
            .oneshot(
                Request::builder()
                    .method(Method::POST)
                    .uri("/Devices/Options")
                    .header("X-Emby-Token", &api_key)
                    .header(header::CONTENT_TYPE, "application/json")
                    .body(Body::from("{}"))
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::BAD_REQUEST);

        for endpoint in [
            "/System/Configuration/unsupported",
            "/system/configuration/unsupported",
        ] {
            let response = app
                .clone()
                .oneshot(
                    Request::builder()
                        .method(Method::POST)
                        .uri(endpoint)
                        .header(header::CONTENT_TYPE, "application/json")
                        .body(Body::from("{}"))
                        .unwrap(),
                )
                .await
                .unwrap();
            assert_eq!(response.status(), StatusCode::UNAUTHORIZED, "{endpoint}");

            let response = app
                .clone()
                .oneshot(
                    Request::builder()
                        .method(Method::POST)
                        .uri(endpoint)
                        .header("X-Emby-Token", &api_key)
                        .header(header::CONTENT_TYPE, "application/json")
                        .body(Body::from(
                            json!({ "Sentinel": "not-persisted" }).to_string(),
                        ))
                        .unwrap(),
                )
                .await
                .unwrap();
            assert_eq!(response.status(), StatusCode::NOT_IMPLEMENTED, "{endpoint}");
        }

        let response = app
            .clone()
            .oneshot(
                Request::builder()
                    .method(Method::DELETE)
                    .uri("/Devices")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::UNAUTHORIZED);

        let response = app
            .clone()
            .oneshot(
                Request::builder()
                    .method(Method::DELETE)
                    .uri("/Devices")
                    .header("X-Emby-Token", &api_key)
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::NO_CONTENT);
    }

    #[tokio::test]
    async fn system_configuration_post_persists_startup_config() {
        let db = Database::connect("sqlite::memory:").await.unwrap();
        let user = db
            .update_first_user("admin".to_string(), "secret")
            .await
            .unwrap();
        let api_key = db
            .issue_api_key_for_user(user.id, "test-key")
            .await
            .unwrap();
        let app = router(AppState {
            db: db.clone(),
            web_dir: ".".into(),
            log_dir: ".".into(),
            local_address: "http://127.0.0.1:8097".to_string(),
        });

        let response = app
            .clone()
            .oneshot(
                Request::builder()
                    .uri("/System/Configuration")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::OK);
        let body = response.into_body().collect().await.unwrap().to_bytes();
        let default_config: Value = serde_json::from_slice(&body).unwrap();
        assert_eq!(default_config["DummyChapterDuration"], 0);
        assert_eq!(default_config["ChapterImageResolution"], "MatchSource");
        assert_eq!(default_config["RemoteClientBitrateLimit"], 0);
        assert_eq!(default_config["MinResumePct"], 5);
        assert_eq!(default_config["MaxResumePct"], 90);
        assert_eq!(default_config["MinResumeDurationSeconds"], 300);
        assert_eq!(default_config["MinAudiobookResume"], 5);
        assert_eq!(default_config["MaxAudiobookResume"], 5);
        assert_eq!(default_config["EnableSlowResponseWarning"], true);
        assert_eq!(default_config["SlowResponseThresholdMs"], 500);
        assert_eq!(default_config["CachePath"], Value::Null);
        assert_eq!(default_config["MetadataPath"], "");
        assert_eq!(default_config["QuickConnectAvailable"], true);
        assert_eq!(default_config["LibraryScanFanoutConcurrency"], 0);
        assert_eq!(default_config["ParallelImageEncodingLimit"], 0);
        assert_eq!(
            default_config["TrickplayOptions"]["EnableHwAcceleration"],
            false
        );
        assert_eq!(
            default_config["TrickplayOptions"]["ScanBehavior"],
            "NonBlocking"
        );
        assert_eq!(
            default_config["TrickplayOptions"]["ProcessPriority"],
            "BelowNormal"
        );
        assert_eq!(
            default_config["TrickplayOptions"]["WidthResolutions"],
            json!([320])
        );

        let response = app
            .clone()
            .oneshot(
                Request::builder()
                    .method(Method::POST)
                    .uri("/System/Configuration")
                    .header(header::CONTENT_TYPE, "application/json")
                    .body(Body::from(
                        json!({
                            "ServerName": "Jellyrin Admin QA",
                            "UICulture": "es-ES",
                            "MetadataCountryCode": "ES",
                            "PreferredMetadataLanguage": "es",
                            "DummyChapterDuration": 300,
                            "ChapterImageResolution": "P720",
                            "RemoteClientBitrateLimit": 1_500_000,
                            "MinResumePct": 10,
                            "MaxResumePct": 80,
                            "MinResumeDurationSeconds": 120,
                            "MinAudiobookResume": 7,
                            "MaxAudiobookResume": 12,
                            "EnableSlowResponseWarning": false,
                            "SlowResponseThresholdMs": 750,
                            "CachePath": "/tmp/jellyrin-cache",
                            "MetadataPath": "/tmp/jellyrin-metadata",
                            "QuickConnectAvailable": false,
                            "LibraryScanFanoutConcurrency": 3,
                            "ParallelImageEncodingLimit": 4,
                            "TrickplayOptions": {
                                "EnableHwAcceleration": true,
                                "EnableHwEncoding": true,
                                "EnableKeyFrameOnlyExtraction": true,
                                "ScanBehavior": "Blocking",
                                "ProcessPriority": "High",
                                "Interval": 5000,
                                "WidthResolutions": [320, 640],
                                "TileWidth": 8,
                                "TileHeight": 9,
                                "Qscale": 6,
                                "JpegQuality": 85,
                                "ProcessThreads": 2,
                                "UnknownTrickplayField": "ignored"
                            },
                            "EnableRemoteAccess": true,
                            "UnknownJellyfinWebSetting": "ignored",
                            "MetadataOptions": [{ "ItemType": "Movie", "DisabledMetadataFetchers": ["Test"] }],
                            "ContentTypes": [{ "Name": "Movies", "Value": "movies" }],
                            "PathSubstitutions": [{ "From": "/mnt/a", "To": "/mnt/b" }],
                            "PluginRepositories": [{ "Name": "Example", "Url": "https://example.invalid" }]
                        })
                        .to_string(),
                    ))
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::UNAUTHORIZED);

        let response = app
            .clone()
            .oneshot(
                Request::builder()
                    .method(Method::POST)
                    .uri("/System/Configuration")
                    .header("X-Emby-Token", &api_key)
                    .header(header::CONTENT_TYPE, "application/json")
                    .body(Body::from(
                        json!({
                            "ServerName": "Jellyrin Admin QA",
                            "UICulture": "es-ES",
                            "MetadataCountryCode": "ES",
                            "PreferredMetadataLanguage": "es",
                            "DummyChapterDuration": 300,
                            "ChapterImageResolution": "P720",
                            "RemoteClientBitrateLimit": 1_500_000,
                            "MinResumePct": 10,
                            "MaxResumePct": 80,
                            "MinResumeDurationSeconds": 120,
                            "MinAudiobookResume": 7,
                            "MaxAudiobookResume": 12,
                            "EnableSlowResponseWarning": false,
                            "SlowResponseThresholdMs": 750,
                            "CachePath": "/tmp/jellyrin-cache",
                            "MetadataPath": "/tmp/jellyrin-metadata",
                            "QuickConnectAvailable": false,
                            "LibraryScanFanoutConcurrency": 3,
                            "ParallelImageEncodingLimit": 4,
                            "TrickplayOptions": {
                                "EnableHwAcceleration": true,
                                "EnableHwEncoding": true,
                                "EnableKeyFrameOnlyExtraction": true,
                                "ScanBehavior": "Blocking",
                                "ProcessPriority": "High",
                                "Interval": 5000,
                                "WidthResolutions": [320, 640],
                                "TileWidth": 8,
                                "TileHeight": 9,
                                "Qscale": 6,
                                "JpegQuality": 85,
                                "ProcessThreads": 2,
                                "UnknownTrickplayField": "ignored"
                            },
                            "EnableRemoteAccess": true,
                            "UnknownJellyfinWebSetting": "ignored",
                            "MetadataOptions": [{ "ItemType": "Movie", "DisabledMetadataFetchers": ["Test"] }],
                            "ContentTypes": [{ "Name": "Movies", "Value": "movies" }],
                            "PathSubstitutions": [{ "From": "/mnt/a", "To": "/mnt/b" }],
                            "PluginRepositories": [{ "Name": "Example", "Url": "https://example.invalid" }]
                        })
                        .to_string(),
                    ))
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::NO_CONTENT);

        let response = app
            .clone()
            .oneshot(
                Request::builder()
                    .uri("/system/configuration")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::OK);
        let body = response.into_body().collect().await.unwrap().to_bytes();
        let updated_config: Value = serde_json::from_slice(&body).unwrap();
        assert_eq!(updated_config["ServerName"], "Jellyrin Admin QA");
        assert_eq!(updated_config["UICulture"], "es-ES");
        assert_eq!(updated_config["MetadataCountryCode"], "ES");
        assert_eq!(updated_config["PreferredMetadataLanguage"], "es");
        assert_eq!(updated_config["DummyChapterDuration"], 300);
        assert_eq!(updated_config["ChapterImageResolution"], "P720");
        assert_eq!(updated_config["RemoteClientBitrateLimit"], 1_500_000);
        assert_eq!(updated_config["MinResumePct"], 10);
        assert_eq!(updated_config["MaxResumePct"], 80);
        assert_eq!(updated_config["MinResumeDurationSeconds"], 120);
        assert_eq!(updated_config["MinAudiobookResume"], 7);
        assert_eq!(updated_config["MaxAudiobookResume"], 12);
        assert_eq!(updated_config["EnableSlowResponseWarning"], false);
        assert_eq!(updated_config["SlowResponseThresholdMs"], 750);
        assert_eq!(updated_config["CachePath"], "/tmp/jellyrin-cache");
        assert_eq!(updated_config["MetadataPath"], "/tmp/jellyrin-metadata");
        assert_eq!(updated_config["QuickConnectAvailable"], false);
        assert_eq!(updated_config["LibraryScanFanoutConcurrency"], 3);
        assert_eq!(updated_config["ParallelImageEncodingLimit"], 4);
        assert_eq!(
            updated_config["TrickplayOptions"]["EnableHwAcceleration"],
            true
        );
        assert_eq!(updated_config["TrickplayOptions"]["EnableHwEncoding"], true);
        assert_eq!(
            updated_config["TrickplayOptions"]["EnableKeyFrameOnlyExtraction"],
            true
        );
        assert_eq!(
            updated_config["TrickplayOptions"]["ScanBehavior"],
            "Blocking"
        );
        assert_eq!(
            updated_config["TrickplayOptions"]["ProcessPriority"],
            "High"
        );
        assert_eq!(updated_config["TrickplayOptions"]["Interval"], 5000);
        assert_eq!(
            updated_config["TrickplayOptions"]["WidthResolutions"],
            json!([320, 640])
        );
        assert_eq!(updated_config["TrickplayOptions"]["TileWidth"], 8);
        assert_eq!(updated_config["TrickplayOptions"]["TileHeight"], 9);
        assert_eq!(updated_config["TrickplayOptions"]["Qscale"], 6);
        assert_eq!(updated_config["TrickplayOptions"]["JpegQuality"], 85);
        assert_eq!(updated_config["TrickplayOptions"]["ProcessThreads"], 2);
        assert!(
            updated_config["TrickplayOptions"]
                .get("UnknownTrickplayField")
                .is_none()
        );
        assert_eq!(updated_config["EnableRemoteAccess"], true);
        assert_eq!(updated_config["IsStartupWizardCompleted"], false);
        assert_eq!(
            updated_config["MetadataOptions"],
            json!([{ "ItemType": "Movie", "DisabledMetadataFetchers": ["Test"] }])
        );
        assert_eq!(
            updated_config["ContentTypes"],
            json!([{ "Name": "Movies", "Value": "movies" }])
        );
        assert_eq!(
            updated_config["PathSubstitutions"],
            json!([{ "From": "/mnt/a", "To": "/mnt/b" }])
        );
        assert_eq!(
            updated_config["PluginRepositories"],
            json!([{ "Name": "Example", "Url": "https://example.invalid" }])
        );

        let persisted_config = db.startup_config().await.unwrap();
        assert_eq!(persisted_config.server_name, "Jellyrin Admin QA");
        assert_eq!(persisted_config.ui_culture, "es-ES");
        assert_eq!(persisted_config.metadata_country_code, "ES");
        assert_eq!(persisted_config.preferred_metadata_language, "es");
        assert_eq!(persisted_config.dummy_chapter_duration, 300);
        assert_eq!(persisted_config.chapter_image_resolution, "P720");
        assert!(persisted_config.enable_remote_access);

        let response = app
            .clone()
            .oneshot(
                Request::builder()
                    .method(Method::POST)
                    .uri("/system/configuration")
                    .header("X-Emby-Token", &api_key)
                    .header(header::CONTENT_TYPE, "application/json")
                    .body(Body::from(
                        json!({
                            "EnableSlowResponseWarning": true,
                            "SlowResponseThresholdMs": 1_234
                        })
                        .to_string(),
                    ))
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::NO_CONTENT);

        let response = app
            .clone()
            .oneshot(
                Request::builder()
                    .uri("/system/configuration")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::OK);
        let body = response.into_body().collect().await.unwrap().to_bytes();
        let reenabled_config: Value = serde_json::from_slice(&body).unwrap();
        assert_eq!(reenabled_config["EnableSlowResponseWarning"], true);
        assert_eq!(reenabled_config["SlowResponseThresholdMs"], 1_234);

        let response = app
            .clone()
            .oneshot(
                Request::builder()
                    .method(Method::POST)
                    .uri("/system/configuration")
                    .header("X-Emby-Token", &api_key)
                    .header(header::CONTENT_TYPE, "application/json")
                    .body(Body::from(json!({ "ServerName": " " }).to_string()))
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::NO_CONTENT);

        let response = app
            .clone()
            .oneshot(
                Request::builder()
                    .method(Method::POST)
                    .uri("/system/configuration")
                    .header("X-Emby-Token", &api_key)
                    .header(header::CONTENT_TYPE, "application/json")
                    .body(Body::from(
                        json!({ "ChapterImageResolution": "InvalidResolution" }).to_string(),
                    ))
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::BAD_REQUEST);

        let response = app
            .clone()
            .oneshot(
                Request::builder()
                    .method(Method::POST)
                    .uri("/system/configuration")
                    .header("X-Emby-Token", &api_key)
                    .header(header::CONTENT_TYPE, "application/json")
                    .body(Body::from(
                        json!({ "EnableRemoteAccess": false }).to_string(),
                    ))
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::NO_CONTENT);

        let response = app
            .clone()
            .oneshot(
                Request::builder()
                    .method(Method::POST)
                    .uri("/system/configuration")
                    .header("X-Emby-Token", &api_key)
                    .header(header::CONTENT_TYPE, "application/json")
                    .body(Body::from(
                        json!({
                            "ContentTypes": { "Name": "Not an array" },
                            "MetadataOptions": "invalid",
                            "PathSubstitutions": null
                        })
                        .to_string(),
                    ))
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::NO_CONTENT);

        let response = app
            .oneshot(
                Request::builder()
                    .uri("/System/Configuration")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        let body = response.into_body().collect().await.unwrap().to_bytes();
        let preserved_config: Value = serde_json::from_slice(&body).unwrap();
        assert_eq!(preserved_config["ServerName"], "Jellyrin Admin QA");
        assert_eq!(preserved_config["UICulture"], "es-ES");
        assert_eq!(preserved_config["DummyChapterDuration"], 300);
        assert_eq!(preserved_config["ChapterImageResolution"], "P720");
        assert_eq!(preserved_config["RemoteClientBitrateLimit"], 1_500_000);
        assert_eq!(preserved_config["MinResumePct"], 10);
        assert_eq!(preserved_config["MaxResumePct"], 80);
        assert_eq!(preserved_config["MinResumeDurationSeconds"], 120);
        assert_eq!(preserved_config["MinAudiobookResume"], 7);
        assert_eq!(preserved_config["MaxAudiobookResume"], 12);
        assert_eq!(preserved_config["EnableSlowResponseWarning"], true);
        assert_eq!(preserved_config["SlowResponseThresholdMs"], 1_234);
        assert_eq!(preserved_config["CachePath"], "/tmp/jellyrin-cache");
        assert_eq!(preserved_config["MetadataPath"], "/tmp/jellyrin-metadata");
        assert_eq!(preserved_config["QuickConnectAvailable"], false);
        assert_eq!(preserved_config["LibraryScanFanoutConcurrency"], 3);
        assert_eq!(preserved_config["ParallelImageEncodingLimit"], 4);
        assert_eq!(
            preserved_config["TrickplayOptions"]["ProcessPriority"],
            "High"
        );
        assert_eq!(
            preserved_config["TrickplayOptions"]["WidthResolutions"],
            json!([320, 640])
        );
        assert_eq!(preserved_config["EnableRemoteAccess"], false);
        assert_eq!(
            preserved_config["MetadataOptions"],
            json!([{ "ItemType": "Movie", "DisabledMetadataFetchers": ["Test"] }])
        );
        assert_eq!(
            preserved_config["ContentTypes"],
            json!([{ "Name": "Movies", "Value": "movies" }])
        );
        assert_eq!(
            preserved_config["PathSubstitutions"],
            json!([{ "From": "/mnt/a", "To": "/mnt/b" }])
        );
        assert_eq!(
            preserved_config["PluginRepositories"],
            json!([{ "Name": "Example", "Url": "https://example.invalid" }])
        );
    }

    #[tokio::test]
    async fn network_named_configuration_round_trips_dashboard_contract() {
        let db = Database::connect("sqlite::memory:").await.unwrap();
        let user = db
            .update_first_user("admin".to_string(), "secret")
            .await
            .unwrap();
        let api_key = db
            .issue_api_key_for_user(user.id, "test-key")
            .await
            .unwrap();
        let app = router(AppState {
            db: db.clone(),
            web_dir: ".".into(),
            log_dir: ".".into(),
            local_address: "http://127.0.0.1:8097".to_string(),
        });

        let response = app
            .clone()
            .oneshot(
                Request::builder()
                    .uri("/System/Configuration/network")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::UNAUTHORIZED);

        let response = app
            .clone()
            .oneshot(
                Request::builder()
                    .uri("/System/Configuration/network")
                    .header("X-Emby-Token", &api_key)
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::OK);
        let body = response.into_body().collect().await.unwrap().to_bytes();
        let defaults: Value = serde_json::from_slice(&body).unwrap();
        assert_eq!(defaults["InternalHttpPort"], 8097);
        assert_eq!(defaults["InternalHttpsPort"], 8920);
        assert_eq!(defaults["EnableIPv4"], true);
        assert_eq!(defaults["EnableIPv6"], false);
        assert_eq!(defaults["EnableRemoteAccess"], true);
        assert!(defaults["LocalNetworkSubnets"].is_array());
        assert!(defaults["KnownProxies"].is_array());
        assert!(defaults["PublishedServerUriBySubnet"].is_array());

        let response = app
            .clone()
            .oneshot(
                Request::builder()
                    .method(Method::POST)
                    .uri("/System/Configuration/network")
                    .header(header::CONTENT_TYPE, "application/json")
                    .body(Body::from("{}"))
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::UNAUTHORIZED);

        let payload = json!({
            "BaseUrl": "/jellyrin",
            "EnableHttps": true,
            "RequireHttps": false,
            "CertificatePath": "/tmp/cert.pfx",
            "CertificatePassword": "secret",
            "InternalHttpPort": 18097,
            "InternalHttpsPort": 18920,
            "PublicHttpPort": 80,
            "PublicHttpsPort": 443,
            "AutoDiscovery": false,
            "EnableIPv4": true,
            "EnableIPv6": true,
            "EnableRemoteAccess": false,
            "LocalNetworkSubnets": ["192.168.1.0/24"],
            "LocalNetworkAddresses": ["0.0.0.0"],
            "KnownProxies": ["10.0.0.1"],
            "RemoteIPFilter": ["203.0.113.10"],
            "IsRemoteIPFilterBlacklist": true,
            "PublishedServerUriBySubnet": ["all=https://media.example.test"],
            "UnknownNetworkField": "ignored"
        });
        let response = app
            .clone()
            .oneshot(
                Request::builder()
                    .method(Method::POST)
                    .uri("/system/configuration/network")
                    .header("X-Emby-Token", &api_key)
                    .header(header::CONTENT_TYPE, "application/json")
                    .body(Body::from(payload.to_string()))
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::NO_CONTENT);

        let response = app
            .clone()
            .oneshot(
                Request::builder()
                    .uri("/system/configuration/network")
                    .header("X-Emby-Token", &api_key)
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        let body = response.into_body().collect().await.unwrap().to_bytes();
        let config: Value = serde_json::from_slice(&body).unwrap();
        assert_eq!(config["BaseUrl"], "/jellyrin");
        assert_eq!(config["EnableHttps"], true);
        assert_eq!(config["InternalHttpPort"], 18097);
        assert_eq!(config["InternalHttpsPort"], 18920);
        assert_eq!(config["PublicHttpPort"], 80);
        assert_eq!(config["PublicHttpsPort"], 443);
        assert_eq!(config["AutoDiscovery"], false);
        assert_eq!(config["EnableIPv6"], true);
        assert_eq!(config["EnableRemoteAccess"], false);
        assert_eq!(config["LocalNetworkSubnets"], json!(["192.168.1.0/24"]));
        assert_eq!(config["KnownProxies"], json!(["10.0.0.1"]));
        assert_eq!(config["RemoteIPFilter"], json!(["203.0.113.10"]));
        assert_eq!(config["IsRemoteIPFilterBlacklist"], true);
        assert_eq!(
            config["PublishedServerUriBySubnet"],
            json!(["all=https://media.example.test"])
        );
        assert!(config.get("UnknownNetworkField").is_none());

        let startup = db.startup_config().await.unwrap();
        assert!(!startup.enable_remote_access);
    }

    #[tokio::test]
    async fn live_tv_named_configuration_round_trips_dashboard_contract() {
        let db = Database::connect("sqlite::memory:").await.unwrap();
        let user = db
            .update_first_user("admin".to_string(), "secret")
            .await
            .unwrap();
        let api_key = db
            .issue_api_key_for_user(user.id, "test-key")
            .await
            .unwrap();
        let app = router(AppState {
            db,
            web_dir: ".".into(),
            log_dir: ".".into(),
            local_address: "http://127.0.0.1:8097".to_string(),
        });

        let response = app
            .clone()
            .oneshot(
                Request::builder()
                    .uri("/System/Configuration/livetv")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::UNAUTHORIZED);

        let response = app
            .clone()
            .oneshot(
                Request::builder()
                    .uri("/System/Configuration/livetv")
                    .header("X-Emby-Token", &api_key)
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::OK);
        let body = response.into_body().collect().await.unwrap().to_bytes();
        let defaults: Value = serde_json::from_slice(&body).unwrap();
        assert_eq!(defaults["GuideDays"], Value::Null);
        assert_eq!(defaults["RecordingPath"], "");
        assert_eq!(defaults["PrePaddingSeconds"], 180);
        assert_eq!(defaults["PostPaddingSeconds"], 180);
        assert_eq!(defaults["SaveRecordingNFO"], false);
        assert!(defaults["TunerHosts"].is_array());
        assert!(defaults["ListingProviders"].is_array());

        let payload = json!({
            "GuideDays": 7,
            "RecordingPath": "/srv/recordings",
            "MovieRecordingPath": "/srv/recordings/movies",
            "SeriesRecordingPath": "/srv/recordings/series",
            "EnableRecordingSubfolders": true,
            "EnableOriginalAudioWithEncodedRecordings": true,
            "TunerHosts": [{ "Id": "tuner-1", "Url": "http://192.0.2.10" }],
            "ListingProviders": [{ "Id": "provider-1", "Type": "xmltv" }],
            "PrePaddingSeconds": 300,
            "PostPaddingSeconds": 600,
            "MediaLocationsCreated": ["/srv/recordings"],
            "RecordingPostProcessor": "/usr/local/bin/process-recording",
            "RecordingPostProcessorArguments": "{path}",
            "SaveRecordingNFO": true,
            "SaveRecordingImages": true,
            "UnknownLiveTvField": "ignored"
        });
        let response = app
            .clone()
            .oneshot(
                Request::builder()
                    .method(Method::POST)
                    .uri("/system/configuration/livetv")
                    .header("X-Emby-Token", &api_key)
                    .header(header::CONTENT_TYPE, "application/json")
                    .body(Body::from(payload.to_string()))
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::NO_CONTENT);

        let response = app
            .clone()
            .oneshot(
                Request::builder()
                    .uri("/system/configuration/livetv")
                    .header("X-Emby-Token", &api_key)
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        let body = response.into_body().collect().await.unwrap().to_bytes();
        let config: Value = serde_json::from_slice(&body).unwrap();
        assert_eq!(config["GuideDays"], 7);
        assert_eq!(config["RecordingPath"], "/srv/recordings");
        assert_eq!(config["MovieRecordingPath"], "/srv/recordings/movies");
        assert_eq!(config["SeriesRecordingPath"], "/srv/recordings/series");
        assert_eq!(config["EnableRecordingSubfolders"], true);
        assert_eq!(config["EnableOriginalAudioWithEncodedRecordings"], true);
        assert_eq!(config["PrePaddingSeconds"], 300);
        assert_eq!(config["PostPaddingSeconds"], 600);
        assert_eq!(
            config["RecordingPostProcessor"],
            "/usr/local/bin/process-recording"
        );
        assert_eq!(config["RecordingPostProcessorArguments"], "{path}");
        assert_eq!(config["SaveRecordingNFO"], true);
        assert_eq!(config["SaveRecordingImages"], true);
        assert_eq!(config["TunerHosts"].as_array().unwrap().len(), 1);
        assert_eq!(config["ListingProviders"].as_array().unwrap().len(), 1);
        assert!(config.get("UnknownLiveTvField").is_none());

        let response = app
            .clone()
            .oneshot(
                Request::builder()
                    .uri("/LiveTv/TunerHosts/Types")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::UNAUTHORIZED);

        let response = app
            .clone()
            .oneshot(
                Request::builder()
                    .uri("/LiveTv/TunerHosts/Types")
                    .header("X-Emby-Token", &api_key)
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::OK);
        let body = response.into_body().collect().await.unwrap().to_bytes();
        let types: Value = serde_json::from_slice(&body).unwrap();
        assert!(
            types
                .as_array()
                .unwrap()
                .iter()
                .any(|tuner_type| tuner_type["Id"] == "m3u")
        );

        let response = app
            .clone()
            .oneshot(
                Request::builder()
                    .method(Method::POST)
                    .uri("/LiveTv/TunerHosts")
                    .header(header::CONTENT_TYPE, "application/json")
                    .body(Body::from(
                        json!({
                            "Id": "tuner-2",
                            "Type": "m3u",
                            "Url": "http://example.test/playlist.m3u",
                            "FriendlyName": "Test tuner"
                        })
                        .to_string(),
                    ))
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::UNAUTHORIZED);

        let response = app
            .clone()
            .oneshot(
                Request::builder()
                    .method(Method::POST)
                    .uri("/LiveTv/TunerHosts")
                    .header("X-Emby-Token", &api_key)
                    .header(header::CONTENT_TYPE, "application/json")
                    .body(Body::from(
                        json!({
                            "Id": "tuner-2",
                            "Type": "m3u",
                            "Url": "http://example.test/playlist.m3u",
                            "FriendlyName": "Test tuner",
                            "TunerCount": "2",
                            "AllowStreamSharing": true
                        })
                        .to_string(),
                    ))
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::OK);
        let body = response.into_body().collect().await.unwrap().to_bytes();
        let created_tuner: Value = serde_json::from_slice(&body).unwrap();
        assert_eq!(created_tuner["Id"], "tuner-2");
        assert_eq!(created_tuner["FriendlyName"], "Test tuner");

        let response = app
            .clone()
            .oneshot(
                Request::builder()
                    .method(Method::POST)
                    .uri("/livetv/tunerhosts")
                    .header("X-Emby-Token", &api_key)
                    .header(header::CONTENT_TYPE, "application/json")
                    .body(Body::from(
                        json!({
                            "Id": "tuner-2",
                            "Type": "m3u",
                            "Url": "http://example.test/replaced.m3u",
                            "FriendlyName": "Updated tuner"
                        })
                        .to_string(),
                    ))
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::OK);

        let response = app
            .clone()
            .oneshot(
                Request::builder()
                    .uri("/System/Configuration/livetv")
                    .header("X-Emby-Token", &api_key)
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        let body = response.into_body().collect().await.unwrap().to_bytes();
        let config: Value = serde_json::from_slice(&body).unwrap();
        let tuner_hosts = config["TunerHosts"].as_array().unwrap();
        assert_eq!(tuner_hosts.len(), 2);
        let tuner = tuner_hosts
            .iter()
            .find(|tuner| tuner["Id"] == "tuner-2")
            .unwrap();
        assert_eq!(tuner["Url"], "http://example.test/replaced.m3u");
        assert_eq!(tuner["FriendlyName"], "Updated tuner");

        let response = app
            .clone()
            .oneshot(
                Request::builder()
                    .method(Method::DELETE)
                    .uri("/LiveTv/TunerHosts?id=tuner-2")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::UNAUTHORIZED);

        let response = app
            .clone()
            .oneshot(
                Request::builder()
                    .method(Method::DELETE)
                    .uri("/LiveTv/TunerHosts?id=tuner-2")
                    .header("X-Emby-Token", &api_key)
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::NO_CONTENT);

        let response = app
            .clone()
            .oneshot(
                Request::builder()
                    .uri("/System/Configuration/livetv")
                    .header("X-Emby-Token", &api_key)
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        let body = response.into_body().collect().await.unwrap().to_bytes();
        let config: Value = serde_json::from_slice(&body).unwrap();
        let tuner_hosts = config["TunerHosts"].as_array().unwrap();
        assert_eq!(tuner_hosts.len(), 1);
        assert!(tuner_hosts.iter().all(|tuner| tuner["Id"] != "tuner-2"));

        let response = app
            .clone()
            .oneshot(
                Request::builder()
                    .uri("/LiveTv/ListingProviders/Default")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::UNAUTHORIZED);

        let response = app
            .clone()
            .oneshot(
                Request::builder()
                    .uri("/livetv/listingproviders/default")
                    .header("X-Emby-Token", &api_key)
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::OK);
        let body = response.into_body().collect().await.unwrap().to_bytes();
        let default_provider: Value = serde_json::from_slice(&body).unwrap();
        assert_eq!(default_provider["EnableAllTuners"], true);
        assert_eq!(default_provider["NewsCategories"][0], "news");
        assert!(
            default_provider["EnabledTuners"]
                .as_array()
                .unwrap()
                .is_empty()
        );

        let response = app
            .clone()
            .oneshot(
                Request::builder()
                    .method(Method::POST)
                    .uri("/LiveTv/ListingProviders")
                    .header(header::CONTENT_TYPE, "application/json")
                    .body(Body::from(
                        json!({
                            "Id": "provider-2",
                            "Type": "xmltv",
                            "Path": "/srv/xmltv.xml",
                            "EnableAllTuners": false,
                            "EnabledTuners": ["tuner-1"]
                        })
                        .to_string(),
                    ))
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::UNAUTHORIZED);

        let response = app
            .clone()
            .oneshot(
                Request::builder()
                    .method(Method::POST)
                    .uri("/LiveTv/ListingProviders?ValidateListings=true")
                    .header("X-Emby-Token", &api_key)
                    .header(header::CONTENT_TYPE, "application/json")
                    .body(Body::from(
                        json!({
                            "Id": "provider-2",
                            "Type": "xmltv",
                            "Path": "/srv/xmltv.xml",
                            "EnableAllTuners": false,
                            "EnabledTuners": ["tuner-1"],
                            "MovieCategories": ["film"]
                        })
                        .to_string(),
                    ))
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::OK);
        let body = response.into_body().collect().await.unwrap().to_bytes();
        let created_provider: Value = serde_json::from_slice(&body).unwrap();
        assert_eq!(created_provider["Id"], "provider-2");
        assert_eq!(created_provider["Type"], "xmltv");
        assert_eq!(created_provider["MovieCategories"], json!(["film"]));

        let response = app
            .clone()
            .oneshot(
                Request::builder()
                    .method(Method::POST)
                    .uri("/livetv/listingproviders")
                    .header("X-Emby-Token", &api_key)
                    .header(header::CONTENT_TYPE, "application/json")
                    .body(Body::from(
                        json!({
                            "Id": "provider-2",
                            "Type": "xmltv",
                            "Path": "/srv/updated.xml",
                            "EnableAllTuners": true
                        })
                        .to_string(),
                    ))
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::OK);

        let response = app
            .clone()
            .oneshot(
                Request::builder()
                    .uri("/System/Configuration/livetv")
                    .header("X-Emby-Token", &api_key)
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        let body = response.into_body().collect().await.unwrap().to_bytes();
        let config: Value = serde_json::from_slice(&body).unwrap();
        let providers = config["ListingProviders"].as_array().unwrap();
        assert_eq!(providers.len(), 2);
        let provider = providers
            .iter()
            .find(|provider| provider["Id"] == "provider-2")
            .unwrap();
        assert_eq!(provider["Path"], "/srv/updated.xml");
        assert_eq!(provider["EnableAllTuners"], true);

        let response = app
            .clone()
            .oneshot(
                Request::builder()
                    .method(Method::DELETE)
                    .uri("/LiveTv/ListingProviders?id=provider-2")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::UNAUTHORIZED);

        let response = app
            .clone()
            .oneshot(
                Request::builder()
                    .method(Method::DELETE)
                    .uri("/LiveTv/ListingProviders?id=provider-2")
                    .header("X-Emby-Token", &api_key)
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::NO_CONTENT);

        let response = app
            .clone()
            .oneshot(
                Request::builder()
                    .uri("/System/Configuration/livetv")
                    .header("X-Emby-Token", &api_key)
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        let body = response.into_body().collect().await.unwrap().to_bytes();
        let config: Value = serde_json::from_slice(&body).unwrap();
        let providers = config["ListingProviders"].as_array().unwrap();
        assert_eq!(providers.len(), 1);
        assert!(
            providers
                .iter()
                .all(|provider| provider["Id"] != "provider-2")
        );

        let response = app
            .clone()
            .oneshot(
                Request::builder()
                    .uri("/LiveTv/ListingProviders/SchedulesDirect/Countries")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::UNAUTHORIZED);

        let response = app
            .clone()
            .oneshot(
                Request::builder()
                    .uri("/livetv/listingproviders/schedulesdirect/countries")
                    .header("X-Emby-Token", &api_key)
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::OK);
        let body = response.into_body().collect().await.unwrap().to_bytes();
        let countries: Value = serde_json::from_slice(&body).unwrap();
        assert_eq!(
            countries["North America"][0],
            json!({ "fullName": "Canada", "shortName": "CAN" })
        );
        assert!(countries["ZZZ"].as_array().unwrap().is_empty());

        let response = app
            .clone()
            .oneshot(
                Request::builder()
                    .uri(
                        "/LiveTv/ListingProviders/Lineups?Id=provider-1&Location=90210&Country=USA",
                    )
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::UNAUTHORIZED);

        let response = app
            .clone()
            .oneshot(
                Request::builder()
                    .uri(
                        "/livetv/listingproviders/lineups?Id=provider-1&Location=90210&Country=USA",
                    )
                    .header("X-Emby-Token", &api_key)
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::OK);
        let body = response.into_body().collect().await.unwrap().to_bytes();
        let lineups: Value = serde_json::from_slice(&body).unwrap();
        assert!(lineups.as_array().unwrap().is_empty());

        let response = app
            .clone()
            .oneshot(
                Request::builder()
                    .uri("/LiveTv/ChannelMappingOptions?providerId=provider-1")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::UNAUTHORIZED);

        let response = app
            .clone()
            .oneshot(
                Request::builder()
                    .uri("/livetv/channelmappingoptions?ProviderId=provider-1")
                    .header("X-Emby-Token", &api_key)
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::OK);
        let body = response.into_body().collect().await.unwrap().to_bytes();
        let options: Value = serde_json::from_slice(&body).unwrap();
        assert!(options["TunerChannels"].as_array().unwrap().is_empty());
        assert!(options["ProviderChannels"].as_array().unwrap().is_empty());
        assert!(options["Mappings"].as_array().unwrap().is_empty());
        assert_eq!(options["ProviderName"], "xmltv");

        let response = app
            .clone()
            .oneshot(
                Request::builder()
                    .method(Method::POST)
                    .uri("/LiveTv/ChannelMappings")
                    .header(header::CONTENT_TYPE, "application/json")
                    .body(Body::from(
                        json!({
                            "ProviderId": "provider-1",
                            "TunerChannelId": "1",
                            "ProviderChannelId": "101"
                        })
                        .to_string(),
                    ))
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::UNAUTHORIZED);

        let response = app
            .clone()
            .oneshot(
                Request::builder()
                    .method(Method::POST)
                    .uri("/livetv/channelmappings")
                    .header("X-Emby-Token", &api_key)
                    .header(header::CONTENT_TYPE, "application/json")
                    .body(Body::from(
                        json!({
                            "ProviderId": "provider-1",
                            "TunerChannelId": "1",
                            "ProviderChannelId": "101"
                        })
                        .to_string(),
                    ))
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::OK);
        let body = response.into_body().collect().await.unwrap().to_bytes();
        let mapping: Value = serde_json::from_slice(&body).unwrap();
        assert_eq!(mapping["Id"], "1");
        assert_eq!(mapping["Name"], "1");
        assert_eq!(mapping["ProviderChannelId"], "101");
        assert_eq!(mapping["ProviderChannelName"], "101");

        let response = app
            .clone()
            .oneshot(
                Request::builder()
                    .uri("/System/Configuration/livetv")
                    .header("X-Emby-Token", &api_key)
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        let body = response.into_body().collect().await.unwrap().to_bytes();
        let config: Value = serde_json::from_slice(&body).unwrap();
        let providers = config["ListingProviders"].as_array().unwrap();
        let provider = providers
            .iter()
            .find(|provider| provider["Id"] == "provider-1")
            .unwrap();
        assert_eq!(
            provider["ChannelMappings"],
            json!([{ "Name": "1", "Value": "101" }])
        );

        let response = app
            .clone()
            .oneshot(
                Request::builder()
                    .method(Method::POST)
                    .uri("/LiveTv/ChannelMappings")
                    .header("X-Emby-Token", &api_key)
                    .header(header::CONTENT_TYPE, "application/json")
                    .body(Body::from(
                        json!({
                            "ProviderId": "provider-1",
                            "TunerChannelId": "1",
                            "ProviderChannelId": "1"
                        })
                        .to_string(),
                    ))
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::OK);

        let response = app
            .clone()
            .oneshot(
                Request::builder()
                    .uri("/LiveTv/ChannelMappingOptions?providerId=provider-1")
                    .header("X-Emby-Token", &api_key)
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::OK);
        let body = response.into_body().collect().await.unwrap().to_bytes();
        let options: Value = serde_json::from_slice(&body).unwrap();
        assert!(options["Mappings"].as_array().unwrap().is_empty());

        let response = app
            .oneshot(
                Request::builder()
                    .method(Method::POST)
                    .uri("/System/Configuration/unsupported")
                    .header("X-Emby-Token", &api_key)
                    .header(header::CONTENT_TYPE, "application/json")
                    .body(Body::from(
                        json!({ "Sentinel": "not-persisted" }).to_string(),
                    ))
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::NOT_IMPLEMENTED);
    }

    #[tokio::test]
    async fn metadata_named_configuration_round_trips_dashboard_contract() {
        let db = Database::connect("sqlite::memory:").await.unwrap();
        let user = db
            .update_first_user("admin".to_string(), "secret")
            .await
            .unwrap();
        let api_key = db
            .issue_api_key_for_user(user.id, "test-key")
            .await
            .unwrap();
        let app = router(AppState {
            db,
            web_dir: ".".into(),
            log_dir: ".".into(),
            local_address: "http://127.0.0.1:8097".to_string(),
        });

        let response = app
            .clone()
            .oneshot(
                Request::builder()
                    .uri("/System/Configuration/metadata")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::UNAUTHORIZED);

        let response = app
            .clone()
            .oneshot(
                Request::builder()
                    .uri("/System/Configuration/metadata")
                    .header("X-Emby-Token", &api_key)
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::OK);
        let body = response.into_body().collect().await.unwrap().to_bytes();
        let defaults: Value = serde_json::from_slice(&body).unwrap();
        assert_eq!(defaults["UseFileCreationTimeForDateAdded"], false);

        let response = app
            .clone()
            .oneshot(
                Request::builder()
                    .method(Method::POST)
                    .uri("/System/Configuration/metadata")
                    .header(header::CONTENT_TYPE, "application/json")
                    .body(Body::from("{}"))
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::UNAUTHORIZED);

        let payload = json!({
            "UseFileCreationTimeForDateAdded": true,
            "UnknownMetadataField": "ignored"
        });
        let response = app
            .clone()
            .oneshot(
                Request::builder()
                    .method(Method::POST)
                    .uri("/System/Configuration/metadata")
                    .header("X-Emby-Token", &api_key)
                    .header(header::CONTENT_TYPE, "application/json")
                    .body(Body::from(payload.to_string()))
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::NO_CONTENT);

        let response = app
            .clone()
            .oneshot(
                Request::builder()
                    .uri("/system/configuration/metadata")
                    .header("X-Emby-Token", &api_key)
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::OK);
        let body = response.into_body().collect().await.unwrap().to_bytes();
        let config: Value = serde_json::from_slice(&body).unwrap();
        assert_eq!(config["UseFileCreationTimeForDateAdded"], true);
        assert!(config.get("UnknownMetadataField").is_none());

        let response = app
            .oneshot(
                Request::builder()
                    .method(Method::POST)
                    .uri("/system/configuration/metadata")
                    .header("X-Emby-Token", &api_key)
                    .header(header::CONTENT_TYPE, "application/json")
                    .body(Body::from(
                        json!({ "UseFileCreationTimeForDateAdded": false }).to_string(),
                    ))
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::NO_CONTENT);
    }

    #[tokio::test]
    async fn xbmc_metadata_named_configuration_round_trips_dashboard_contract() {
        let db = Database::connect("sqlite::memory:").await.unwrap();
        let user = db
            .update_first_user("admin".to_string(), "secret")
            .await
            .unwrap();
        let api_key = db
            .issue_api_key_for_user(user.id, "test-key")
            .await
            .unwrap();
        let app = router(AppState {
            db,
            web_dir: ".".into(),
            log_dir: ".".into(),
            local_address: "http://127.0.0.1:8097".to_string(),
        });

        let response = app
            .clone()
            .oneshot(
                Request::builder()
                    .uri("/System/Configuration/xbmcmetadata")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::UNAUTHORIZED);

        let response = app
            .clone()
            .oneshot(
                Request::builder()
                    .uri("/System/Configuration/xbmcmetadata")
                    .header("X-Emby-Token", &api_key)
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::OK);
        let body = response.into_body().collect().await.unwrap().to_bytes();
        let defaults: Value = serde_json::from_slice(&body).unwrap();
        assert_eq!(defaults["UserId"], Value::Null);
        assert_eq!(defaults["ReleaseDateFormat"], "yyyy-MM-dd");
        assert_eq!(defaults["SaveImagePathsInNfo"], true);
        assert_eq!(defaults["EnablePathSubstitution"], true);
        assert_eq!(defaults["EnableExtraThumbsDuplication"], false);

        let response = app
            .clone()
            .oneshot(
                Request::builder()
                    .method(Method::POST)
                    .uri("/System/Configuration/xbmcmetadata")
                    .header(header::CONTENT_TYPE, "application/json")
                    .body(Body::from("{}"))
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::UNAUTHORIZED);

        let payload = json!({
            "UserId": user.id.to_string(),
            "ReleaseDateFormat": "dd/MM/yyyy",
            "SaveImagePathsInNfo": false,
            "EnablePathSubstitution": false,
            "EnableExtraThumbsDuplication": true,
            "UnknownNfoField": "ignored"
        });
        let response = app
            .clone()
            .oneshot(
                Request::builder()
                    .method(Method::POST)
                    .uri("/System/Configuration/xbmcmetadata")
                    .header("X-Emby-Token", &api_key)
                    .header(header::CONTENT_TYPE, "application/json")
                    .body(Body::from(payload.to_string()))
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::NO_CONTENT);

        let response = app
            .clone()
            .oneshot(
                Request::builder()
                    .uri("/system/configuration/xbmcmetadata")
                    .header("X-Emby-Token", &api_key)
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::OK);
        let body = response.into_body().collect().await.unwrap().to_bytes();
        let config: Value = serde_json::from_slice(&body).unwrap();
        assert_eq!(config["UserId"], user.id.to_string());
        assert_eq!(config["ReleaseDateFormat"], "dd/MM/yyyy");
        assert_eq!(config["SaveImagePathsInNfo"], false);
        assert_eq!(config["EnablePathSubstitution"], false);
        assert_eq!(config["EnableExtraThumbsDuplication"], true);
        assert!(config.get("UnknownNfoField").is_none());

        let response = app
            .oneshot(
                Request::builder()
                    .method(Method::POST)
                    .uri("/system/configuration/xbmcmetadata")
                    .header("X-Emby-Token", &api_key)
                    .header(header::CONTENT_TYPE, "application/json")
                    .body(Body::from(
                        json!({ "SaveImagePathsInNfo": true }).to_string(),
                    ))
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::NO_CONTENT);
    }

    #[tokio::test]
    async fn encoding_named_configuration_round_trips_dashboard_contract() {
        let db = Database::connect("sqlite::memory:").await.unwrap();
        let user = db
            .update_first_user("admin".to_string(), "secret")
            .await
            .unwrap();
        let api_key = db
            .issue_api_key_for_user(user.id, "test-key")
            .await
            .unwrap();
        let app = router(AppState {
            db,
            web_dir: ".".into(),
            log_dir: ".".into(),
            local_address: "http://127.0.0.1:8097".to_string(),
        });

        let response = app
            .clone()
            .oneshot(
                Request::builder()
                    .uri("/System/Configuration/encoding")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::UNAUTHORIZED);

        let response = app
            .clone()
            .oneshot(
                Request::builder()
                    .uri("/System/Configuration/encoding")
                    .header("X-Emby-Token", &api_key)
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::OK);
        let body = response.into_body().collect().await.unwrap().to_bytes();
        let defaults: Value = serde_json::from_slice(&body).unwrap();
        assert_eq!(defaults["HardwareAccelerationType"], "none");
        assert_eq!(defaults["EncodingThreadCount"], -1);
        assert_eq!(defaults["TranscodingTempPath"], Value::Null);
        assert_eq!(defaults["FallbackFontPath"], Value::Null);
        assert_eq!(defaults["EncoderAppPath"], Value::Null);
        assert_eq!(defaults["EncoderAppPathDisplay"], Value::Null);
        assert_eq!(defaults["VaapiDevice"], "/dev/dri/renderD128");
        assert_eq!(defaults["EncoderPreset"], Value::Null);
        assert_eq!(defaults["EnableHardwareEncoding"], true);
        assert_eq!(defaults["EnableSubtitleExtraction"], true);
        assert_eq!(defaults["HardwareDecodingCodecs"], json!(["h264", "vc1"]));
        assert_eq!(
            defaults["AllowOnDemandMetadataBasedKeyframeExtractionForExtensions"],
            json!(["mkv"])
        );
        assert_eq!(defaults["HlsAudioSeekStrategy"], "DisableAccurateSeek");

        let response = app
            .clone()
            .oneshot(
                Request::builder()
                    .method(Method::POST)
                    .uri("/System/Configuration/encoding")
                    .header(header::CONTENT_TYPE, "application/json")
                    .body(Body::from("{}"))
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::UNAUTHORIZED);

        let payload = json!({
            "HardwareAccelerationType": "vaapi",
            "VaapiDevice": "/dev/dri/renderD129",
            "EncodingThreadCount": 4,
            "TranscodingTempPath": "/tmp/jellyrin-transcode",
            "FallbackFontPath": "/usr/share/fonts",
            "EnableFallbackFont": true,
            "EnableAudioVbr": true,
            "DownMixAudioBoost": 1.5,
            "DownMixStereoAlgorithm": "Rfc7845",
            "MaxMuxingQueueSize": 4096,
            "EnableThrottling": true,
            "ThrottleDelaySeconds": 60,
            "EnableSegmentDeletion": true,
            "SegmentKeepSeconds": 300,
            "EnableTonemapping": true,
            "EnableVppTonemapping": true,
            "TonemappingAlgorithm": "mobius",
            "TonemappingMode": "max",
            "TonemappingRange": "pc",
            "TonemappingPeak": 120.0,
            "VppTonemappingBrightness": 20.0,
            "VppTonemappingContrast": 1.2,
            "H264Crf": 20,
            "H265Crf": 24,
            "EncoderPreset": "fast",
            "DeinterlaceMethod": "bwdif",
            "DeinterlaceDoubleRate": true,
            "EnableDecodingColorDepth10Hevc": false,
            "EnableDecodingColorDepth10Vp9": false,
            "EnableHardwareEncoding": false,
            "AllowHevcEncoding": true,
            "AllowAv1Encoding": true,
            "EnableSubtitleExtraction": false,
            "SubtitleExtractionTimeoutMinutes": 15,
            "HardwareDecodingCodecs": ["h264", "hevc"],
            "AllowOnDemandMetadataBasedKeyframeExtractionForExtensions": ["mkv", "mp4"],
            "HlsAudioSeekStrategy": "TranscodeAudio",
            "UnknownEncodingField": "ignored"
        });
        let response = app
            .clone()
            .oneshot(
                Request::builder()
                    .method(Method::POST)
                    .uri("/System/Configuration/encoding")
                    .header("X-Emby-Token", &api_key)
                    .header(header::CONTENT_TYPE, "application/json")
                    .body(Body::from(payload.to_string()))
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::NO_CONTENT);

        let response = app
            .clone()
            .oneshot(
                Request::builder()
                    .uri("/system/configuration/encoding")
                    .header("X-Emby-Token", &api_key)
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::OK);
        let body = response.into_body().collect().await.unwrap().to_bytes();
        let config: Value = serde_json::from_slice(&body).unwrap();
        assert_eq!(config["HardwareAccelerationType"], "vaapi");
        assert_eq!(config["VaapiDevice"], "/dev/dri/renderD129");
        assert_eq!(config["EncodingThreadCount"], 4);
        assert_eq!(config["TranscodingTempPath"], "/tmp/jellyrin-transcode");
        assert_eq!(config["EnableFallbackFont"], true);
        assert_eq!(config["DownMixAudioBoost"], 1.5);
        assert_eq!(config["DownMixStereoAlgorithm"], "Rfc7845");
        assert_eq!(config["EnableThrottling"], true);
        assert_eq!(config["EnableSegmentDeletion"], true);
        assert_eq!(config["EnableTonemapping"], true);
        assert_eq!(config["EnableVppTonemapping"], true);
        assert_eq!(config["TonemappingAlgorithm"], "mobius");
        assert_eq!(config["TonemappingMode"], "max");
        assert_eq!(config["TonemappingRange"], "pc");
        assert_eq!(config["H264Crf"], 20);
        assert_eq!(config["H265Crf"], 24);
        assert_eq!(config["EncoderPreset"], "fast");
        assert_eq!(config["DeinterlaceMethod"], "bwdif");
        assert_eq!(config["DeinterlaceDoubleRate"], true);
        assert_eq!(config["EnableHardwareEncoding"], false);
        assert_eq!(config["AllowHevcEncoding"], true);
        assert_eq!(config["AllowAv1Encoding"], true);
        assert_eq!(config["EnableSubtitleExtraction"], false);
        assert_eq!(config["HardwareDecodingCodecs"], json!(["h264", "hevc"]));
        assert_eq!(
            config["AllowOnDemandMetadataBasedKeyframeExtractionForExtensions"],
            json!(["mkv", "mp4"])
        );
        assert_eq!(config["HlsAudioSeekStrategy"], "TranscodeAudio");
        assert!(config.get("UnknownEncodingField").is_none());

        let response = app
            .oneshot(
                Request::builder()
                    .method(Method::POST)
                    .uri("/system/configuration/encoding")
                    .header("X-Emby-Token", &api_key)
                    .header(header::CONTENT_TYPE, "application/json")
                    .body(Body::from(
                        json!({ "HardwareAccelerationType": "none" }).to_string(),
                    ))
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::NO_CONTENT);
    }

    #[tokio::test]
    async fn activity_log_requires_admin_and_returns_persisted_events() {
        let db = Database::connect("sqlite::memory:").await.unwrap();
        let user = db
            .update_first_user("admin".to_string(), "secret")
            .await
            .unwrap();
        let api_key = db
            .issue_api_key_for_user(user.id, "test-key")
            .await
            .unwrap();
        let app = router(AppState {
            db,
            web_dir: ".".into(),
            log_dir: ".".into(),
            local_address: "http://127.0.0.1:8097".to_string(),
        });

        let response = app
            .clone()
            .oneshot(
                Request::builder()
                    .uri("/System/ActivityLog/Entries")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::UNAUTHORIZED);

        let response = app
            .clone()
            .oneshot(
                Request::builder()
                    .method(Method::POST)
                    .uri("/Users/AuthenticateByName")
                    .header(header::CONTENT_TYPE, "application/json")
                    .header(
                        header::AUTHORIZATION,
                        r#"MediaBrowser Client="Jellyfin Web", Device="Firefox", DeviceId="activity-device", Version="dev""#,
                    )
                    .body(Body::from(
                        json!({ "Username": "admin", "Pw": "secret" }).to_string(),
                    ))
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::OK);
        let body = response.into_body().collect().await.unwrap().to_bytes();
        let login: Value = serde_json::from_slice(&body).unwrap();
        let session_token = login["AccessToken"].as_str().unwrap();

        let response = app
            .clone()
            .oneshot(
                Request::builder()
                    .method(Method::POST)
                    .uri("/Sessions/Logout")
                    .header("X-Emby-Token", session_token)
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::NO_CONTENT);

        let response = app
            .clone()
            .oneshot(
                Request::builder()
                    .method(Method::POST)
                    .uri("/System/Configuration")
                    .header("X-Emby-Token", &api_key)
                    .header(header::CONTENT_TYPE, "application/json")
                    .body(Body::from(
                        json!({ "ServerName": "Activity Server" }).to_string(),
                    ))
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::NO_CONTENT);

        let response = app
            .clone()
            .oneshot(
                Request::builder()
                    .uri("/System/ActivityLog/Entries?StartIndex=0&Limit=1")
                    .header("X-Emby-Token", &api_key)
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::OK);
        let body = response.into_body().collect().await.unwrap().to_bytes();
        let activity: Value = serde_json::from_slice(&body).unwrap();
        assert_eq!(activity["TotalRecordCount"], 3);
        assert_eq!(activity["StartIndex"], 0);
        assert_eq!(activity["Items"].as_array().unwrap().len(), 1);
        assert_eq!(activity["Items"][0]["Name"], "Server configuration updated");
        assert_eq!(activity["Items"][0]["Type"], "System");
        assert_eq!(activity["Items"][0]["Severity"], "Information");
        assert_eq!(activity["Items"][0]["UserId"], user.id.to_string());

        let response = app
            .oneshot(
                Request::builder()
                    .uri("/System/ActivityLog/Entries?startIndex=0&limit=10&hasUserId=true&username=adm&name=Server&severity=Information&sortBy=Name&sortOrder=Ascending")
                    .header("X-Emby-Token", &api_key)
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::OK);
        let body = response.into_body().collect().await.unwrap().to_bytes();
        let filtered: Value = serde_json::from_slice(&body).unwrap();
        assert_eq!(filtered["TotalRecordCount"], 1);
        assert_eq!(filtered["Items"].as_array().unwrap().len(), 1);
        assert_eq!(filtered["Items"][0]["Name"], "Server configuration updated");
    }

    #[tokio::test]
    async fn package_repositories_round_trip_system_configuration_payload() {
        let db = Database::connect("sqlite::memory:").await.unwrap();
        let user = db
            .update_first_user("admin".to_string(), "secret")
            .await
            .unwrap();
        let api_key = db
            .issue_api_key_for_user(user.id, "test-key")
            .await
            .unwrap();
        let app = router(AppState {
            db,
            web_dir: ".".into(),
            log_dir: ".".into(),
            local_address: "http://127.0.0.1:8097".to_string(),
        });

        let response = app
            .clone()
            .oneshot(
                Request::builder()
                    .uri("/Repositories")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::UNAUTHORIZED);

        let response = app
            .clone()
            .oneshot(
                Request::builder()
                    .method(Method::POST)
                    .uri("/Repositories")
                    .header("X-Emby-Token", &api_key)
                    .header(header::CONTENT_TYPE, "application/json")
                    .body(Body::from(
                        json!([
                            { "Name": "Stable", "Url": "https://repo.example/manifest.json", "Enabled": true },
                            { "name": "Disabled", "url": "https://disabled.example/manifest.json", "enabled": false },
                            { "Name": "Missing URL" },
                            "invalid"
                        ])
                        .to_string(),
                    ))
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::NO_CONTENT);

        let response = app
            .clone()
            .oneshot(
                Request::builder()
                    .uri("/repositories")
                    .header("X-Emby-Token", &api_key)
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::OK);
        let body = response.into_body().collect().await.unwrap().to_bytes();
        let repositories: Value = serde_json::from_slice(&body).unwrap();
        assert_eq!(repositories.as_array().unwrap().len(), 2);
        assert_eq!(repositories[0]["Name"], "Stable");
        assert_eq!(repositories[0]["Url"], "https://repo.example/manifest.json");
        assert_eq!(repositories[0]["Enabled"], true);
        assert_eq!(repositories[1]["Name"], "Disabled");
        assert_eq!(repositories[1]["Enabled"], false);

        let response = app
            .oneshot(
                Request::builder()
                    .uri("/System/Configuration")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        let body = response.into_body().collect().await.unwrap().to_bytes();
        let config: Value = serde_json::from_slice(&body).unwrap();
        assert_eq!(config["PluginRepositories"], repositories);
    }

    #[tokio::test]
    async fn branding_configuration_round_trips_supported_fields() {
        let db = Database::connect("sqlite::memory:").await.unwrap();
        let user = db
            .update_first_user("admin".to_string(), "secret")
            .await
            .unwrap();
        let api_key = db
            .issue_api_key_for_user(user.id, "test-key")
            .await
            .unwrap();
        let app = router(AppState {
            db,
            web_dir: ".".into(),
            log_dir: ".".into(),
            local_address: "http://127.0.0.1:8097".to_string(),
        });

        let response = app
            .clone()
            .oneshot(
                Request::builder()
                    .uri("/Branding/Configuration")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::OK);
        let body = response.into_body().collect().await.unwrap().to_bytes();
        let defaults: Value = serde_json::from_slice(&body).unwrap();
        assert_eq!(defaults["LoginDisclaimer"], Value::Null);
        assert_eq!(defaults["CustomCss"], Value::Null);
        assert_eq!(defaults["SplashscreenEnabled"], true);

        let response = app
            .clone()
            .oneshot(
                Request::builder()
                    .method(Method::POST)
                    .uri("/System/Configuration/Branding")
                    .header(header::CONTENT_TYPE, "application/json")
                    .body(Body::from(
                        json!({
                            "LoginDisclaimer": "Private server",
                            "CustomCss": "body { color: rgb(1, 2, 3); }",
                            "SplashscreenEnabled": false,
                            "UnknownBrandingField": "ignored"
                        })
                        .to_string(),
                    ))
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::UNAUTHORIZED);

        let response = app
            .clone()
            .oneshot(
                Request::builder()
                    .method(Method::POST)
                    .uri("/System/Configuration/Branding")
                    .header("X-Emby-Token", &api_key)
                    .header(header::CONTENT_TYPE, "application/json")
                    .body(Body::from(
                        json!({
                            "LoginDisclaimer": "Private server",
                            "CustomCss": "body { color: rgb(1, 2, 3); }",
                            "SplashscreenEnabled": false,
                            "UnknownBrandingField": "ignored"
                        })
                        .to_string(),
                    ))
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::NO_CONTENT);

        for endpoint in ["/Branding/Configuration", "/System/Configuration/branding"] {
            let response = app
                .clone()
                .oneshot(
                    Request::builder()
                        .uri(endpoint)
                        .body(Body::empty())
                        .unwrap(),
                )
                .await
                .unwrap();
            assert_eq!(response.status(), StatusCode::OK, "{endpoint}");
            let body = response.into_body().collect().await.unwrap().to_bytes();
            let branding: Value = serde_json::from_slice(&body).unwrap();
            assert_eq!(branding["LoginDisclaimer"], "Private server");
            assert_eq!(branding["CustomCss"], "body { color: rgb(1, 2, 3); }");
            assert_eq!(branding["SplashscreenEnabled"], false);
        }

        let response = app
            .clone()
            .oneshot(
                Request::builder()
                    .uri("/Branding/Css")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::OK);
        let body = response.into_body().collect().await.unwrap().to_bytes();
        assert_eq!(body.as_ref(), b"body { color: rgb(1, 2, 3); }");

        let response = app
            .clone()
            .oneshot(
                Request::builder()
                    .method(Method::POST)
                    .uri("/system/configuration/branding")
                    .header("X-Emby-Token", &api_key)
                    .header(header::CONTENT_TYPE, "application/json")
                    .body(Body::from(json!({ "LoginDisclaimer": null }).to_string()))
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::NO_CONTENT);

        let response = app
            .oneshot(
                Request::builder()
                    .uri("/branding/configuration")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::OK);
        let body = response.into_body().collect().await.unwrap().to_bytes();
        let branding: Value = serde_json::from_slice(&body).unwrap();
        assert_eq!(branding["LoginDisclaimer"], Value::Null);
        assert_eq!(branding["CustomCss"], "body { color: rgb(1, 2, 3); }");
        assert_eq!(branding["SplashscreenEnabled"], false);
    }

    #[tokio::test]
    async fn display_preferences_round_trip_per_user_and_client() {
        let db = Database::connect("sqlite::memory:").await.unwrap();
        let user = db
            .update_first_user("admin".to_string(), "secret")
            .await
            .unwrap();
        let api_key = db
            .issue_api_key_for_user(user.id, "test-key")
            .await
            .unwrap();
        let app = router(AppState {
            db,
            web_dir: ".".into(),
            log_dir: ".".into(),
            local_address: "http://127.0.0.1:8097".to_string(),
        });
        let endpoint = format!(
            "/DisplayPreferences/usersettings?UserId={}&Client=emby",
            user.id
        );

        let response = app
            .clone()
            .oneshot(
                Request::builder()
                    .uri(&endpoint)
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::UNAUTHORIZED);

        let response = app
            .clone()
            .oneshot(
                Request::builder()
                    .uri(&endpoint)
                    .header("X-Emby-Token", &api_key)
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::OK);
        let body = response.into_body().collect().await.unwrap().to_bytes();
        let defaults: Value = serde_json::from_slice(&body).unwrap();
        assert_eq!(defaults["Id"], "usersettings");
        assert_eq!(defaults["SortBy"], "SortName");

        let response = app
            .clone()
            .oneshot(
                Request::builder()
                    .method(Method::POST)
                    .uri(&endpoint)
                    .header("X-Emby-Token", &api_key)
                    .header(header::CONTENT_TYPE, "application/json")
                    .body(Body::from(
                        json!({
                            "Id": "ignored-client-id",
                            "ViewType": "Poster",
                            "SortBy": "DateCreated,SortName",
                            "IndexBy": "SortName",
                            "RememberIndexing": true,
                            "PrimaryImageHeight": 320,
                            "PrimaryImageWidth": 213,
                            "CustomPrefs": { "landing-livetv": "false" }
                        })
                        .to_string(),
                    ))
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::NO_CONTENT);

        let response = app
            .clone()
            .oneshot(
                Request::builder()
                    .uri(&endpoint)
                    .header("X-Emby-Token", &api_key)
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        let body = response.into_body().collect().await.unwrap().to_bytes();
        let preferences: Value = serde_json::from_slice(&body).unwrap();
        assert_eq!(preferences["Id"], "usersettings");
        assert_eq!(preferences["ViewType"], "Poster");
        assert_eq!(preferences["SortBy"], "DateCreated,SortName");
        assert_eq!(preferences["RememberIndexing"], true);
        assert_eq!(preferences["CustomPrefs"]["landing-livetv"], "false");

        let response = app
            .oneshot(
                Request::builder()
                    .uri(format!(
                        "/displaypreferences/usersettings?UserId={}&Client=other",
                        user.id
                    ))
                    .header("X-Emby-Token", &api_key)
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        let body = response.into_body().collect().await.unwrap().to_bytes();
        let other_client: Value = serde_json::from_slice(&body).unwrap();
        assert_eq!(other_client["SortBy"], "SortName");
    }

    #[tokio::test]
    async fn device_delete_revokes_existing_session() {
        let db = Database::connect("sqlite::memory:").await.unwrap();
        let user = db
            .update_first_user("admin".to_string(), "secret")
            .await
            .unwrap();
        let api_key = db
            .issue_api_key_for_user(user.id, "test-key")
            .await
            .unwrap();
        let app = router(AppState {
            db,
            web_dir: ".".into(),
            log_dir: ".".into(),
            local_address: "http://127.0.0.1:8097".to_string(),
        });

        let response = app
            .clone()
            .oneshot(
                Request::builder()
                    .method(Method::POST)
                    .uri("/Users/AuthenticateByName")
                    .header(header::CONTENT_TYPE, "application/json")
                    .header(
                        header::AUTHORIZATION,
                        r#"MediaBrowser Client="Jellyfin Web", Device="Delete Test", DeviceId="delete-device", Version="dev""#,
                    )
                    .body(Body::from(
                        json!({ "Username": "admin", "Pw": "secret" }).to_string(),
                    ))
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::OK);
        let body = response.into_body().collect().await.unwrap().to_bytes();
        let login: Value = serde_json::from_slice(&body).unwrap();
        let token = login["AccessToken"].as_str().unwrap();
        assert_eq!(login["SessionInfo"]["DeviceId"], "delete-device");

        let response = app
            .clone()
            .oneshot(
                Request::builder()
                    .uri("/Sessions")
                    .header("X-Emby-Token", &api_key)
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::OK);
        let body = response.into_body().collect().await.unwrap().to_bytes();
        let sessions: Value = serde_json::from_slice(&body).unwrap();
        assert_eq!(sessions.as_array().unwrap().len(), 1);
        assert_eq!(sessions[0]["DeviceId"], "delete-device");

        let response = app
            .clone()
            .oneshot(
                Request::builder()
                    .method(Method::DELETE)
                    .uri("/devices?Id=delete-device")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::UNAUTHORIZED);

        let response = app
            .clone()
            .oneshot(
                Request::builder()
                    .method(Method::DELETE)
                    .uri("/devices?Id=delete-device")
                    .header("X-Emby-Token", &api_key)
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::NO_CONTENT);

        let response = app
            .clone()
            .oneshot(
                Request::builder()
                    .uri("/Sessions")
                    .header("X-Emby-Token", &api_key)
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::OK);
        let body = response.into_body().collect().await.unwrap().to_bytes();
        let sessions_after_delete: Value = serde_json::from_slice(&body).unwrap();
        assert_eq!(sessions_after_delete.as_array().unwrap().len(), 0);

        let response = app
            .clone()
            .oneshot(
                Request::builder()
                    .uri("/System/Info")
                    .header("X-Emby-Token", token)
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::UNAUTHORIZED);

        let response = app
            .oneshot(
                Request::builder()
                    .method(Method::DELETE)
                    .uri("/Devices")
                    .header("X-Emby-Token", &api_key)
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::NO_CONTENT);
    }

    #[tokio::test]
    async fn devices_list_and_info_return_persisted_sessions() {
        let db = Database::connect("sqlite::memory:").await.unwrap();
        let user = db
            .update_first_user("admin".to_string(), "secret")
            .await
            .unwrap();
        let api_key = db
            .issue_api_key_for_user(user.id, "test-key")
            .await
            .unwrap();
        let app = router(AppState {
            db,
            web_dir: ".".into(),
            log_dir: ".".into(),
            local_address: "http://127.0.0.1:8097".to_string(),
        });

        let response = app
            .clone()
            .oneshot(
                Request::builder()
                    .method(Method::POST)
                    .uri("/Users/AuthenticateByName")
                    .header(header::CONTENT_TYPE, "application/json")
                    .header(
                        header::AUTHORIZATION,
                        r#"MediaBrowser Client="Jellyfin Web", Device="Admin Browser", DeviceId="admin-browser", Version="dev""#,
                    )
                    .body(Body::from(
                        json!({ "Username": "admin", "Pw": "secret" }).to_string(),
                    ))
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::OK);

        let response = app
            .clone()
            .oneshot(
                Request::builder()
                    .uri("/Devices")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::UNAUTHORIZED);

        let response = app
            .clone()
            .oneshot(
                Request::builder()
                    .uri("/Devices")
                    .header("X-Emby-Token", &api_key)
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::OK);
        let body = response.into_body().collect().await.unwrap().to_bytes();
        let devices: Value = serde_json::from_slice(&body).unwrap();
        assert_eq!(devices["TotalRecordCount"], 1);
        assert_eq!(devices["Items"][0]["Id"], "admin-browser");
        assert_eq!(devices["Items"][0]["Name"], "Admin Browser");
        assert_eq!(devices["Items"][0]["AppName"], "Jellyfin Web");
        assert_eq!(devices["Items"][0]["LastUserName"], "admin");

        let response = app
            .clone()
            .oneshot(
                Request::builder()
                    .uri("/devices/info?Id=admin-browser")
                    .header("X-Emby-Token", &api_key)
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::OK);
        let body = response.into_body().collect().await.unwrap().to_bytes();
        let info: Value = serde_json::from_slice(&body).unwrap();
        assert_eq!(info["Id"], "admin-browser");
        assert_eq!(info["Name"], "Admin Browser");

        let response = app
            .clone()
            .oneshot(
                Request::builder()
                    .uri("/Devices/Info?Id=missing-device")
                    .header("X-Emby-Token", &api_key)
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::OK);
        let body = response.into_body().collect().await.unwrap().to_bytes();
        let missing_info: Value = serde_json::from_slice(&body).unwrap();
        assert_eq!(missing_info["Id"], Value::Null);

        let response = app
            .clone()
            .oneshot(
                Request::builder()
                    .method(Method::DELETE)
                    .uri("/Devices?Id=admin-browser")
                    .header("X-Emby-Token", &api_key)
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::NO_CONTENT);

        let response = app
            .oneshot(
                Request::builder()
                    .uri("/Devices")
                    .header("X-Emby-Token", &api_key)
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        let body = response.into_body().collect().await.unwrap().to_bytes();
        let devices_after_delete: Value = serde_json::from_slice(&body).unwrap();
        assert_eq!(devices_after_delete["TotalRecordCount"], 0);
    }

    #[tokio::test]
    async fn session_capabilities_round_trip_to_sessions_and_devices() {
        let db = Database::connect("sqlite::memory:").await.unwrap();
        let user = db
            .update_first_user("admin".to_string(), "secret")
            .await
            .unwrap();
        let api_key = db
            .issue_api_key_for_user(user.id, "test-key")
            .await
            .unwrap();
        let app = router(AppState {
            db,
            web_dir: ".".into(),
            log_dir: ".".into(),
            local_address: "http://127.0.0.1:8097".to_string(),
        });

        let response = app
            .clone()
            .oneshot(
                Request::builder()
                    .method(Method::POST)
                    .uri("/Users/AuthenticateByName")
                    .header(header::CONTENT_TYPE, "application/json")
                    .header(
                        header::AUTHORIZATION,
                        r#"MediaBrowser Client="Jellyfin Web", Device="Capable Browser", DeviceId="capable-device", Version="dev""#,
                    )
                    .body(Body::from(
                        json!({ "Username": "admin", "Pw": "secret" }).to_string(),
                    ))
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::OK);
        let body = response.into_body().collect().await.unwrap().to_bytes();
        let login: Value = serde_json::from_slice(&body).unwrap();
        let token = login["AccessToken"].as_str().unwrap();

        let response = app
            .clone()
            .oneshot(
                Request::builder()
                    .method(Method::POST)
                    .uri("/Sessions/Capabilities/Full")
                    .header(header::CONTENT_TYPE, "application/json")
                    .body(Body::from("{}"))
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::UNAUTHORIZED);

        let response = app
            .clone()
            .oneshot(
                Request::builder()
                    .method(Method::POST)
                    .uri("/Sessions/Capabilities/Full")
                    .header("X-Emby-Token", token)
                    .header(header::CONTENT_TYPE, "application/json")
                    .body(Body::from(
                        json!({
                            "PlayableMediaTypes": ["Audio", "Video"],
                            "SupportedCommands": ["DisplayContent", "Play", "Seek"],
                            "SupportsRemoteControl": true,
                            "SupportsMediaControl": true,
                            "SupportsPersistentIdentifier": true
                        })
                        .to_string(),
                    ))
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::NO_CONTENT);

        let response = app
            .clone()
            .oneshot(
                Request::builder()
                    .uri("/Sessions")
                    .header("X-Emby-Token", &api_key)
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::OK);
        let body = response.into_body().collect().await.unwrap().to_bytes();
        let sessions: Value = serde_json::from_slice(&body).unwrap();
        assert_eq!(sessions[0]["DeviceId"], "capable-device");
        assert_eq!(sessions[0]["SupportsRemoteControl"], true);
        assert_eq!(sessions[0]["PlayableMediaTypes"], json!(["Audio", "Video"]));
        assert_eq!(
            sessions[0]["SupportedCommands"],
            json!(["DisplayContent", "Play", "Seek"])
        );

        let response = app
            .clone()
            .oneshot(
                Request::builder()
                    .uri("/Devices/Info?Id=capable-device")
                    .header("X-Emby-Token", &api_key)
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::OK);
        let body = response.into_body().collect().await.unwrap().to_bytes();
        let device: Value = serde_json::from_slice(&body).unwrap();
        assert_eq!(device["Capabilities"]["SupportsPersistentIdentifier"], true);
        assert_eq!(
            device["Capabilities"]["SupportedCommands"],
            json!(["DisplayContent", "Play", "Seek"])
        );

        let response = app
            .oneshot(
                Request::builder()
                    .method(Method::POST)
                    .uri("/sessions/capabilities")
                    .header("X-Emby-Token", token)
                    .header(header::CONTENT_TYPE, "application/json")
                    .body(Body::from("[]"))
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::BAD_REQUEST);
    }

    #[tokio::test]
    async fn session_capabilities_query_params_round_trip() {
        let db = Database::connect("sqlite::memory:").await.unwrap();
        let user = db
            .update_first_user("admin".to_string(), "secret")
            .await
            .unwrap();
        let api_key = db
            .issue_api_key_for_user(user.id, "test-key")
            .await
            .unwrap();
        let app = router(AppState {
            db,
            web_dir: ".".into(),
            log_dir: ".".into(),
            local_address: "http://127.0.0.1:8097".to_string(),
        });

        let response = app
            .clone()
            .oneshot(
                Request::builder()
                    .method(Method::POST)
                    .uri("/Users/AuthenticateByName")
                    .header(header::CONTENT_TYPE, "application/json")
                    .header(
                        header::AUTHORIZATION,
                        r#"MediaBrowser Client="Jellyfin Web", Device="Query Capable", DeviceId="query-capable", Version="dev""#,
                    )
                    .body(Body::from(
                        json!({ "Username": "admin", "Pw": "secret" }).to_string(),
                    ))
                    .unwrap(),
            )
            .await
            .unwrap();
        let body = response.into_body().collect().await.unwrap().to_bytes();
        let login: Value = serde_json::from_slice(&body).unwrap();
        let token = login["AccessToken"].as_str().unwrap();

        let response = app
            .clone()
            .oneshot(
                Request::builder()
                    .method(Method::POST)
                    .uri("/sessions/capabilities?playableMediaTypes=Video,Audio&supportedCommands=DisplayMessage,GoHome&supportsMediaControl=true&supportsPersistentIdentifier=false")
                    .header("X-Emby-Token", token)
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::NO_CONTENT);

        let response = app
            .oneshot(
                Request::builder()
                    .uri("/Sessions")
                    .header("X-Emby-Token", &api_key)
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        let body = response.into_body().collect().await.unwrap().to_bytes();
        let sessions: Value = serde_json::from_slice(&body).unwrap();
        assert_eq!(sessions[0]["DeviceId"], "query-capable");
        assert_eq!(sessions[0]["SupportsRemoteControl"], true);
        assert_eq!(sessions[0]["PlayableMediaTypes"], json!(["Video", "Audio"]));
        assert_eq!(
            sessions[0]["SupportedCommands"],
            json!(["DisplayMessage", "GoHome"])
        );
    }

    #[tokio::test]
    async fn device_options_round_trip_custom_name() {
        let db = Database::connect("sqlite::memory:").await.unwrap();
        let user = db
            .update_first_user("admin".to_string(), "secret")
            .await
            .unwrap();
        let api_key = db
            .issue_api_key_for_user(user.id, "test-key")
            .await
            .unwrap();
        let app = router(AppState {
            db,
            web_dir: ".".into(),
            log_dir: ".".into(),
            local_address: "http://127.0.0.1:8097".to_string(),
        });

        let response = app
            .clone()
            .oneshot(
                Request::builder()
                    .method(Method::POST)
                    .uri("/Users/AuthenticateByName")
                    .header(header::CONTENT_TYPE, "application/json")
                    .header(
                        header::AUTHORIZATION,
                        r#"MediaBrowser Client="Jellyfin Web", Device="Original Device", DeviceId="options-device", Version="dev""#,
                    )
                    .body(Body::from(
                        json!({ "Username": "admin", "Pw": "secret" }).to_string(),
                    ))
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::OK);

        let response = app
            .clone()
            .oneshot(
                Request::builder()
                    .uri("/Devices/Options?Id=options-device")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::UNAUTHORIZED);

        let response = app
            .clone()
            .oneshot(
                Request::builder()
                    .uri("/Devices/Options?Id=options-device")
                    .header("X-Emby-Token", &api_key)
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::OK);
        let body = response.into_body().collect().await.unwrap().to_bytes();
        let options: Value = serde_json::from_slice(&body).unwrap();
        assert_eq!(options["CustomName"], "Original Device");

        let response = app
            .clone()
            .oneshot(
                Request::builder()
                    .method(Method::POST)
                    .uri("/devices/options?Id=options-device")
                    .header("X-Emby-Token", &api_key)
                    .header(header::CONTENT_TYPE, "application/json")
                    .body(Body::from(
                        json!({ "CustomName": "Living Room TV" }).to_string(),
                    ))
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::NO_CONTENT);

        let response = app
            .clone()
            .oneshot(
                Request::builder()
                    .uri("/Sessions")
                    .header("X-Emby-Token", &api_key)
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::OK);
        let body = response.into_body().collect().await.unwrap().to_bytes();
        let sessions: Value = serde_json::from_slice(&body).unwrap();
        assert_eq!(sessions[0]["DeviceName"], "Living Room TV");

        let response = app
            .clone()
            .oneshot(
                Request::builder()
                    .uri("/devices/options?Id=options-device")
                    .header("X-Emby-Token", &api_key)
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::OK);
        let body = response.into_body().collect().await.unwrap().to_bytes();
        let updated_options: Value = serde_json::from_slice(&body).unwrap();
        assert_eq!(updated_options["CustomName"], "Living Room TV");

        let response = app
            .clone()
            .oneshot(
                Request::builder()
                    .method(Method::POST)
                    .uri("/Devices/Options?Id=missing-device")
                    .header("X-Emby-Token", &api_key)
                    .header(header::CONTENT_TYPE, "application/json")
                    .body(Body::from(
                        json!({ "CustomName": "Missing Device" }).to_string(),
                    ))
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::NOT_FOUND);

        let response = app
            .oneshot(
                Request::builder()
                    .method(Method::POST)
                    .uri("/Devices/Options")
                    .header("X-Emby-Token", &api_key)
                    .header(header::CONTENT_TYPE, "application/json")
                    .body(Body::from(json!({}).to_string()))
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::BAD_REQUEST);
    }

    #[tokio::test]
    async fn library_and_environment_routes_require_admin_after_startup() {
        let db = Database::connect("sqlite::memory:").await.unwrap();
        let user = db
            .update_first_user("admin".to_string(), "secret")
            .await
            .unwrap();
        db.server_state().await.unwrap();
        db.complete_startup_wizard().await.unwrap();
        let api_key = db
            .issue_api_key_for_user(user.id, "test-key")
            .await
            .unwrap();
        let app = router(AppState {
            db,
            web_dir: ".".into(),
            log_dir: ".".into(),
            local_address: "http://127.0.0.1:8097".to_string(),
        });

        for endpoint in [
            "/Library/VirtualFolders",
            "/Environment/Drives",
            "/Environment/DefaultDirectoryBrowser",
        ] {
            let response = app
                .clone()
                .oneshot(
                    Request::builder()
                        .uri(endpoint)
                        .body(Body::empty())
                        .unwrap(),
                )
                .await
                .unwrap();
            assert_eq!(response.status(), StatusCode::UNAUTHORIZED, "{endpoint}");

            let response = app
                .clone()
                .oneshot(
                    Request::builder()
                        .uri(endpoint)
                        .header("X-Emby-Token", &api_key)
                        .body(Body::empty())
                        .unwrap(),
                )
                .await
                .unwrap();
            assert_eq!(response.status(), StatusCode::OK, "{endpoint}");
        }

        let response = app
            .clone()
            .oneshot(
                Request::builder()
                    .uri("/environment/defaultdirectorybrowser")
                    .header("X-Emby-Token", &api_key)
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::OK);
        let body = response.into_body().collect().await.unwrap().to_bytes();
        let default_browser: Value = serde_json::from_slice(&body).unwrap();
        assert_eq!(default_browser["Path"], Value::Null);

        let response = app
            .clone()
            .oneshot(
                Request::builder()
                    .uri("/Environment/ParentPath?Path=/tmp/jellyrin-child")
                    .header("X-Emby-Token", &api_key)
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::OK);
        let body = response.into_body().collect().await.unwrap().to_bytes();
        let parent_path: Value = serde_json::from_slice(&body).unwrap();
        assert_eq!(parent_path, json!("/tmp"));

        let response = app
            .clone()
            .oneshot(
                Request::builder()
                    .uri("/Environment/Drives")
                    .header("X-Emby-Token", &api_key)
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::OK);
        let body = response.into_body().collect().await.unwrap().to_bytes();
        let drives: Value = serde_json::from_slice(&body).unwrap();
        let drive = drives.as_array().unwrap().first().unwrap();
        assert!(drive["Name"].as_str().is_some_and(|name| !name.is_empty()));
        assert!(drive["Path"].as_str().is_some_and(|path| !path.is_empty()));
        assert!(drive["Type"].as_str().is_some_and(|kind| !kind.is_empty()));

        let tmp = tempfile::tempdir().unwrap();
        let child_dir = tmp.path().join("child-dir");
        let child_file = tmp.path().join("child-file.txt");
        fs::create_dir(&child_dir).unwrap();
        fs::write(&child_file, b"test").unwrap();
        let response = app
            .clone()
            .oneshot(
                Request::builder()
                    .uri(format!(
                        "/Environment/DirectoryContents?Path={}&IncludeFiles=false",
                        tmp.path().to_string_lossy()
                    ))
                    .header("X-Emby-Token", &api_key)
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::OK);
        let body = response.into_body().collect().await.unwrap().to_bytes();
        let entries: Value = serde_json::from_slice(&body).unwrap();
        let entries = entries.as_array().unwrap();
        let child_dir_path = child_dir.to_string_lossy();
        assert!(entries.iter().any(|entry| {
            entry["Name"] == "child-dir"
                && entry["Path"] == *child_dir_path
                && entry["Type"] == "Directory"
        }));
        assert!(
            entries
                .iter()
                .all(|entry| entry["Name"] != "child-file.txt")
        );

        let response = app
            .clone()
            .oneshot(
                Request::builder()
                    .uri(format!(
                        "/Environment/DirectoryContents?Path={}&IncludeFiles=true",
                        tmp.path().to_string_lossy()
                    ))
                    .header("X-Emby-Token", &api_key)
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::OK);
        let body = response.into_body().collect().await.unwrap().to_bytes();
        let entries: Value = serde_json::from_slice(&body).unwrap();
        let entries = entries.as_array().unwrap();
        let child_file_path = child_file.to_string_lossy();
        assert!(entries.iter().any(|entry| {
            entry["Name"] == "child-file.txt"
                && entry["Path"] == *child_file_path
                && entry["Type"] == "File"
        }));

        let response = app
            .clone()
            .oneshot(
                Request::builder()
                    .method(Method::POST)
                    .uri("/Environment/ValidatePath")
                    .header(header::CONTENT_TYPE, "application/json")
                    .body(Body::from(json!({ "Path": "/" }).to_string()))
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::UNAUTHORIZED);

        let response = app
            .clone()
            .oneshot(
                Request::builder()
                    .method(Method::POST)
                    .uri("/Environment/ValidatePath")
                    .header("X-Emby-Token", &api_key)
                    .header(header::CONTENT_TYPE, "application/json")
                    .body(Body::from(json!({ "Path": "/" }).to_string()))
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::NO_CONTENT);

        let response = app
            .oneshot(
                Request::builder()
                    .method(Method::POST)
                    .uri("/Environment/ValidatePath")
                    .header("X-Emby-Token", &api_key)
                    .header(header::CONTENT_TYPE, "application/json")
                    .body(Body::from(
                        json!({ "Path": tmp.path().join("missing").to_string_lossy() }).to_string(),
                    ))
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::BAD_REQUEST);
    }

    #[tokio::test]
    async fn websocket_requires_auth_and_accepts_query_or_header_token() {
        let db = Database::connect("sqlite::memory:").await.unwrap();
        let app = router(AppState {
            db,
            web_dir: ".".into(),
            log_dir: ".".into(),
            local_address: "http://127.0.0.1:8097".to_string(),
        });

        let response = app
            .clone()
            .oneshot(websocket_request("/socket").body(Body::empty()).unwrap())
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::UNAUTHORIZED);

        let response = app
            .clone()
            .oneshot(
                Request::builder()
                    .method(Method::POST)
                    .uri("/Startup/User")
                    .header(header::CONTENT_TYPE, "application/json")
                    .body(Body::from(
                        json!({ "Name": "admin", "Password": "secret" }).to_string(),
                    ))
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::NO_CONTENT);

        let response = app
            .clone()
            .oneshot(
                Request::builder()
                    .method(Method::POST)
                    .uri("/Users/AuthenticateByName")
                    .header(header::CONTENT_TYPE, "application/json")
                    .header(
                        header::AUTHORIZATION,
                        r#"MediaBrowser Client="Jellyfin Web", Device="Firefox", DeviceId="socket-test", Version="dev""#,
                    )
                    .body(Body::from(
                        json!({ "Username": "admin", "Pw": "secret" }).to_string(),
                    ))
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::OK);
        let body = response.into_body().collect().await.unwrap().to_bytes();
        let result: Value = serde_json::from_slice(&body).unwrap();
        let token = result["AccessToken"].as_str().unwrap();

        let response = app
            .clone()
            .oneshot(
                websocket_request(&format!("/socket?api_key={token}&deviceId=socket-test"))
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::UPGRADE_REQUIRED);

        let response = app
            .oneshot(
                websocket_request("/socket")
                    .header("X-Emby-Token", token)
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::UPGRADE_REQUIRED);
    }

    fn websocket_request(uri: &str) -> axum::http::request::Builder {
        Request::builder()
            .uri(uri)
            .header(header::CONNECTION, "Upgrade")
            .header(header::UPGRADE, "websocket")
            .header("Sec-WebSocket-Version", "13")
            .header("Sec-WebSocket-Key", "x3JJHMbDL1EzLkh9GBhXDw==")
    }

    async fn next_playback_event(
        receiver: &mut tokio::sync::broadcast::Receiver<super::PlaybackEvent>,
        session_id: &str,
    ) -> Value {
        for _ in 0..10 {
            let event = tokio::time::timeout(std::time::Duration::from_secs(2), receiver.recv())
                .await
                .expect("timed out waiting for playback websocket event")
                .expect("playback event channel closed");
            if event.session_id == session_id {
                return event.message;
            }
        }
        panic!("no playback websocket event for session {session_id}");
    }

    async fn next_playback_event_type(
        receiver: &mut tokio::sync::broadcast::Receiver<super::PlaybackEvent>,
        session_id: &str,
        message_type: &str,
    ) -> Value {
        for _ in 0..20 {
            let event = next_playback_event(receiver, session_id).await;
            if event["MessageType"]
                .as_str()
                .is_some_and(|value| value == message_type)
            {
                return event;
            }
        }
        panic!("no {message_type} playback websocket event for session {session_id}");
    }

    #[tokio::test]
    async fn m1_environment_library_and_image_compat_endpoints_exist() {
        let db = Database::connect("sqlite::memory:").await.unwrap();
        let app = router(AppState {
            db,
            web_dir: ".".into(),
            log_dir: ".".into(),
            local_address: "http://127.0.0.1:8097".to_string(),
        });

        let response = app
            .clone()
            .oneshot(
                Request::builder()
                    .uri("/Environment/Drives")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::OK);

        let response = app
            .clone()
            .oneshot(
                Request::builder()
                    .uri("/Environment/DefaultDirectoryBrowser")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::OK);

        let response = app
            .clone()
            .oneshot(
                Request::builder()
                    .uri("/environment/directorycontents?Path=/&IncludeFiles=false")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::OK);

        let response = app
            .clone()
            .oneshot(
                Request::builder()
                    .method(Method::POST)
                    .uri("/Library/VirtualFolders?name=Movies&collectionType=movies&paths=/media/movies")
                    .header(header::CONTENT_TYPE, "application/json")
                    .body(Body::from("{}"))
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::NO_CONTENT);

        let response = app
            .clone()
            .oneshot(
                Request::builder()
                    .uri("/library/virtualfolders")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::OK);
        let body = response.into_body().collect().await.unwrap().to_bytes();
        let folders: Value = serde_json::from_slice(&body).unwrap();
        assert_eq!(folders[0]["Name"], "Movies");
        assert_eq!(folders[0]["Locations"][0], "/media/movies");

        let response = app
            .clone()
            .oneshot(
                Request::builder()
                    .method(Method::POST)
                    .uri("/Library/VirtualFolders/Paths")
                    .header(header::CONTENT_TYPE, "application/json")
                    .body(Body::from(
                        json!({ "Name": "Movies", "Path": "/media/more-movies" }).to_string(),
                    ))
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::NO_CONTENT);

        let response = app
            .clone()
            .oneshot(
                Request::builder()
                    .method(Method::DELETE)
                    .uri("/Library/VirtualFolders/Paths?Name=Movies&Path=/media/more-movies")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::NO_CONTENT);

        let response = app
            .clone()
            .oneshot(
                Request::builder()
                    .method(Method::DELETE)
                    .uri("/Library/VirtualFolders?Name=Movies")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::NO_CONTENT);

        let response = app
            .clone()
            .oneshot(
                Request::builder()
                    .uri("/library/virtualfolders")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::OK);
        let body = response.into_body().collect().await.unwrap().to_bytes();
        let folders: Value = serde_json::from_slice(&body).unwrap();
        assert!(folders.as_array().unwrap().is_empty());

        let response = app
            .clone()
            .oneshot(
                Request::builder()
                    .method(Method::POST)
                    .uri("/environment/validatepath")
                    .header(header::CONTENT_TYPE, "application/json")
                    .body(Body::from(json!({ "Path": "/" }).to_string()))
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::NO_CONTENT);

        let response = app
            .clone()
            .oneshot(
                Request::builder()
                    .uri("/SyncPlay/List")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::OK);
        let body = response.into_body().collect().await.unwrap().to_bytes();
        let syncplay: Value = serde_json::from_slice(&body).unwrap();
        assert_eq!(syncplay.as_array().unwrap().len(), 0);

        let response = app
            .oneshot(
                Request::builder()
                    .uri("/items/00000000-0000-0000-0000-000000000000/images/Primary")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::NOT_FOUND);
    }

    #[tokio::test]
    async fn virtual_folder_scan_populates_items_endpoint() {
        let tmp = tempfile::tempdir().unwrap();
        let movie = tmp.path().join("Example Movie.mp4");
        tokio::fs::write(&movie, b"fake video").await.unwrap();

        let db = Database::connect("sqlite::memory:").await.unwrap();
        let user = db
            .update_first_user("admin".to_string(), "secret")
            .await
            .unwrap();
        let api_key = db
            .issue_api_key_for_user(user.id, "test-key")
            .await
            .unwrap();
        let user_id = user.id.simple().to_string();
        let test_db = db.clone();
        let app = router(AppState {
            db,
            web_dir: ".".into(),
            log_dir: ".".into(),
            local_address: "http://127.0.0.1:8097".to_string(),
        });

        let response = app
            .clone()
            .oneshot(
                Request::builder()
                    .method(Method::POST)
                    .uri(format!(
                        "/Library/VirtualFolders?name=Movies&collectionType=movies&paths={}",
                        tmp.path().to_string_lossy()
                    ))
                    .header(header::CONTENT_TYPE, "application/json")
                    .body(Body::from("{}"))
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::NO_CONTENT);

        let response = app
            .clone()
            .oneshot(
                Request::builder()
                    .uri("/Items")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::UNAUTHORIZED);

        let response = app
            .clone()
            .oneshot(
                Request::builder()
                    .uri("/Items")
                    .header("X-Emby-Token", &api_key)
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::OK);
        let body = response.into_body().collect().await.unwrap().to_bytes();
        let result: Value = serde_json::from_slice(&body).unwrap();

        assert_eq!(result["TotalRecordCount"], 1);
        assert_eq!(result["Items"][0]["Name"], "Example Movie");
        assert_eq!(result["Items"][0]["Type"], "Movie");
        assert_eq!(result["Items"][0]["Path"], movie.to_string_lossy().as_ref());
        let item_id = result["Items"][0]["Id"].as_str().unwrap();
        let parent_id = result["Items"][0]["ParentId"].as_str().unwrap();
        test_db
            .update_media_item_media_info(
                parse_jellyfin_uuid(item_id).unwrap(),
                Some(987_650_000),
                Some(2_500_000),
                Some(1920),
                Some(1080),
                vec![
                    json!({
                        "Type": "Video",
                        "Index": 0,
                        "Codec": "h264",
                        "Width": 1920,
                        "Height": 1080,
                        "BitRate": 2_500_000,
                        "AverageFrameRate": 24.0,
                        "PixelFormat": "yuv420p"
                    }),
                    json!({
                        "Type": "Audio",
                        "Index": 1,
                        "Codec": "aac",
                        "IsDefault": true,
                        "Channels": 2,
                        "SampleRate": 48000
                    }),
                    json!({
                        "Type": "Subtitle",
                        "Index": 2,
                        "Codec": "subrip",
                        "IsDefault": true,
                        "IsForced": false,
                        "IsTextSubtitleStream": true
                    }),
                ],
            )
            .await
            .unwrap();

        let response = app
            .clone()
            .oneshot(
                Request::builder()
                    .uri("/Items/Counts")
                    .header("X-Emby-Token", &api_key)
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::OK);
        let body = response.into_body().collect().await.unwrap().to_bytes();
        let counts: Value = serde_json::from_slice(&body).unwrap();
        assert_eq!(counts["MovieCount"], 1);
        assert_eq!(counts["ItemCount"], 1);
        assert_eq!(counts["SongCount"], 0);
        assert_eq!(counts["BookCount"], 0);

        let response = app
            .clone()
            .oneshot(
                Request::builder()
                    .uri(format!(
                        "/Items/Counts?ParentId={parent_id}&IncludeItemTypes=Movie"
                    ))
                    .header("X-Emby-Token", &api_key)
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::OK);
        let body = response.into_body().collect().await.unwrap().to_bytes();
        let filtered_counts: Value = serde_json::from_slice(&body).unwrap();
        assert_eq!(filtered_counts["MovieCount"], 1);
        assert_eq!(filtered_counts["ItemCount"], 1);

        let response = app
            .clone()
            .oneshot(
                Request::builder()
                    .uri("/Items/Counts?SearchTerm=missing-title")
                    .header("X-Emby-Token", &api_key)
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::OK);
        let body = response.into_body().collect().await.unwrap().to_bytes();
        let empty_counts: Value = serde_json::from_slice(&body).unwrap();
        assert_eq!(empty_counts["MovieCount"], 0);
        assert_eq!(empty_counts["ItemCount"], 0);

        let response = app
            .clone()
            .oneshot(
                Request::builder()
                    .uri(format!(
                        "/Items/Latest?ParentId={parent_id}&IncludeItemTypes=Movie&Limit=1"
                    ))
                    .header("X-Emby-Token", &api_key)
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::OK);
        let body = response.into_body().collect().await.unwrap().to_bytes();
        let latest: Value = serde_json::from_slice(&body).unwrap();
        assert_eq!(latest.as_array().unwrap().len(), 1);
        assert_eq!(latest[0]["Name"], "Example Movie");

        let response = app
            .clone()
            .oneshot(
                Request::builder()
                    .uri(format!(
                        "/items?ParentId={parent_id}&IncludeItemTypes=Movie&StartIndex=0&Limit=1&SortBy=SortName"
                    ))
                    .header("X-Emby-Token", &api_key)
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::OK);
        let body = response.into_body().collect().await.unwrap().to_bytes();
        let filtered: Value = serde_json::from_slice(&body).unwrap();
        assert_eq!(filtered["TotalRecordCount"], 1);
        assert_eq!(filtered["StartIndex"], 0);
        assert_eq!(filtered["Items"].as_array().unwrap().len(), 1);
        assert_eq!(filtered["Items"][0]["Name"], "Example Movie");

        let response = app
            .clone()
            .oneshot(
                Request::builder()
                    .uri("/Items?ParentId=not-a-valid-id")
                    .header("X-Emby-Token", &api_key)
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::BAD_REQUEST);

        let response = app
            .clone()
            .oneshot(
                Request::builder()
                    .uri(format!("/Users/{user_id}/Items?IncludeItemTypes=Movie"))
                    .header("X-Emby-Token", &api_key)
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::OK);
        let body = response.into_body().collect().await.unwrap().to_bytes();
        let user_items: Value = serde_json::from_slice(&body).unwrap();
        assert_eq!(user_items["TotalRecordCount"], 1);
        assert_eq!(user_items["Items"][0]["Name"], "Example Movie");

        let response = app
            .clone()
            .oneshot(
                Request::builder()
                    .uri("/Items/Filters")
                    .header("X-Emby-Token", &api_key)
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::OK);
        let body = response.into_body().collect().await.unwrap().to_bytes();
        let filters: Value = serde_json::from_slice(&body).unwrap();
        assert_eq!(filters["Genres"].as_array().unwrap().len(), 0);
        assert_eq!(filters["MediaTypes"], json!(["Video"]));
        assert_eq!(filters["Containers"], json!(["mp4"]));

        let response = app
            .clone()
            .oneshot(
                Request::builder()
                    .uri(format!(
                        "/Items/Filters2?userId={user_id}&parentId={parent_id}&includeItemTypes=Movie"
                    ))
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::UNAUTHORIZED);

        let response = app
            .clone()
            .oneshot(
                Request::builder()
                    .uri(format!(
                        "/Items/Filters2?userId={user_id}&parentId={parent_id}&includeItemTypes=Movie"
                    ))
                    .header("X-Emby-Token", &api_key)
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::OK);
        let body = response.into_body().collect().await.unwrap().to_bytes();
        let filters2: Value = serde_json::from_slice(&body).unwrap();
        assert_eq!(filters2["Genres"].as_array().unwrap().len(), 0);
        assert_eq!(filters2["Tags"].as_array().unwrap().len(), 0);
        assert_eq!(filters2["AudioLanguages"].as_array().unwrap().len(), 0);
        assert_eq!(filters2["SubtitleLanguages"].as_array().unwrap().len(), 0);

        let response = app
            .clone()
            .oneshot(
                Request::builder()
                    .uri(format!(
                        "/Filter/Items/Filters?userId={user_id}&parentId={parent_id}&includeItemTypes=Movie&mediaTypes=Video"
                    ))
                    .header("X-Emby-Token", &api_key)
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::OK);
        let body = response.into_body().collect().await.unwrap().to_bytes();
        let filter_alias: Value = serde_json::from_slice(&body).unwrap();
        assert_eq!(filter_alias["MediaTypes"], json!(["Video"]));
        assert_eq!(filter_alias["Containers"], json!(["mp4"]));

        let response = app
            .clone()
            .oneshot(
                Request::builder()
                    .uri(format!(
                        "/Items?userId={user_id}&parentId={parent_id}&includeItemTypes=Movie&includeItemTypes=BoxSet"
                    ))
                    .header("X-Emby-Token", &api_key)
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::OK);
        let body = response.into_body().collect().await.unwrap().to_bytes();
        let repeated_include_types: Value = serde_json::from_slice(&body).unwrap();
        assert_eq!(repeated_include_types["TotalRecordCount"], 1);
        assert_eq!(repeated_include_types["Items"][0]["Id"], item_id);

        let response = app
            .clone()
            .oneshot(
                Request::builder()
                    .uri(format!(
                        "/Filter/Items/Filters?userId={user_id}&parentId={parent_id}&includeItemTypes=Audio&mediaTypes=Audio"
                    ))
                    .header("X-Emby-Token", &api_key)
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::OK);
        let body = response.into_body().collect().await.unwrap().to_bytes();
        let empty_audio_filter_alias: Value = serde_json::from_slice(&body).unwrap();
        assert_eq!(empty_audio_filter_alias["MediaTypes"], json!([]));
        assert_eq!(empty_audio_filter_alias["Containers"], json!([]));

        let response = app
            .clone()
            .oneshot(
                Request::builder()
                    .uri("/System/ActivityLog/Entries")
                    .header("X-Emby-Token", &api_key)
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::OK);
        let body = response.into_body().collect().await.unwrap().to_bytes();
        let activity: Value = serde_json::from_slice(&body).unwrap();
        assert_eq!(activity["TotalRecordCount"], 1);
        assert_eq!(activity["Items"][0]["Name"], "Library added");
        assert_eq!(activity["Items"][0]["Type"], "Library");
        assert_eq!(activity["Items"][0]["UserId"], Value::Null);

        let response = app
            .clone()
            .oneshot(
                Request::builder()
                    .uri("/Genres")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::UNAUTHORIZED);

        let response = app
            .clone()
            .oneshot(
                Request::builder()
                    .uri("/Genres")
                    .header("X-Emby-Token", &api_key)
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::OK);
        let body = response.into_body().collect().await.unwrap().to_bytes();
        let genres: Value = serde_json::from_slice(&body).unwrap();
        assert_eq!(genres["TotalRecordCount"], 0);

        let response = app
            .clone()
            .oneshot(
                Request::builder()
                    .uri("/Persons")
                    .header("X-Emby-Token", &api_key)
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::OK);
        let body = response.into_body().collect().await.unwrap().to_bytes();
        let persons: Value = serde_json::from_slice(&body).unwrap();
        assert_eq!(persons["StartIndex"], 0);
        assert_eq!(persons["TotalRecordCount"], 0);
        assert_eq!(persons["Items"].as_array().unwrap().len(), 0);

        let response = app
            .clone()
            .oneshot(
                Request::builder()
                    .uri("/Movies/Recommendations")
                    .header("X-Emby-Token", &api_key)
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::OK);
        let body = response.into_body().collect().await.unwrap().to_bytes();
        let recommendations: Value = serde_json::from_slice(&body).unwrap();
        assert_eq!(recommendations.as_array().unwrap().len(), 0);

        let response = app
            .clone()
            .oneshot(
                Request::builder()
                    .uri(format!("/Items/{item_id}"))
                    .header("X-Emby-Token", &api_key)
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::OK);
        let body = response.into_body().collect().await.unwrap().to_bytes();
        let detail: Value = serde_json::from_slice(&body).unwrap();
        assert_eq!(detail["Id"], item_id);
        assert_eq!(detail["Name"], "Example Movie");
        assert_eq!(detail["Container"], "mp4");
        assert_eq!(detail["Size"], 10);
        assert_eq!(detail["RunTimeTicks"], 987_650_000);
        assert_eq!(detail["FileName"], "Example Movie.mp4");
        assert_eq!(detail["ImageTags"]["Primary"], "placeholder");
        assert_eq!(detail["PrimaryImageAspectRatio"], 0.6666667);
        assert_eq!(detail["MediaSources"][0]["Id"], item_id);
        assert_eq!(
            detail["MediaSources"][0]["Path"],
            movie.to_string_lossy().as_ref()
        );
        assert_eq!(detail["MediaSources"][0]["Container"], "mp4");
        assert_eq!(detail["MediaSources"][0]["Size"], 10);
        assert_eq!(detail["MediaSources"][0]["RunTimeTicks"], 987_650_000);
        assert_eq!(detail["MediaSources"][0]["Bitrate"], 2_500_000);
        assert_eq!(detail["MediaSources"][0]["DefaultAudioStreamIndex"], 1);
        assert_eq!(detail["MediaSources"][0]["DefaultSubtitleStreamIndex"], 2);
        assert_eq!(
            detail["MediaSources"][0]["DirectStreamUrl"],
            format!("/Videos/{item_id}/stream")
        );
        assert_eq!(
            detail["MediaSources"][0]["MediaStreams"][0]["Type"],
            "Video"
        );
        assert_eq!(
            detail["MediaSources"][0]["MediaStreams"][0]["Codec"],
            "h264"
        );
        assert_eq!(detail["MediaSources"][0]["MediaStreams"][0]["Width"], 1920);
        assert_eq!(detail["MediaSources"][0]["MediaStreams"][0]["Height"], 1080);
        assert_eq!(
            detail["MediaSources"][0]["MediaStreams"][0]["PixelFormat"],
            "yuv420p"
        );
        assert_eq!(detail["MediaStreams"][0]["Index"], 0);
        assert_eq!(detail["MediaStreams"][1]["Type"], "Audio");
        assert_eq!(detail["MediaStreams"][1]["Codec"], "aac");
        assert_eq!(detail["MediaStreams"][1]["Channels"], 2);
        assert_eq!(detail["MediaStreams"][1]["SampleRate"], 48000);
        assert_eq!(detail["MediaStreams"][2]["Type"], "Subtitle");
        assert_eq!(detail["MediaStreams"][2]["Codec"], "subrip");
        assert_eq!(detail["MediaStreams"][2]["Index"], 2);
        assert_eq!(detail["People"].as_array().unwrap().len(), 0);
        assert_eq!(detail["Studios"].as_array().unwrap().len(), 0);
        assert_eq!(detail["GenreItems"].as_array().unwrap().len(), 0);

        let response = app
            .clone()
            .oneshot(
                Request::builder()
                    .uri(format!("/Videos/{item_id}/AdditionalParts"))
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::UNAUTHORIZED);

        let response = app
            .clone()
            .oneshot(
                Request::builder()
                    .uri(format!("/Videos/{item_id}/AdditionalParts"))
                    .header("X-Emby-Token", &api_key)
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::OK);
        let body = response.into_body().collect().await.unwrap().to_bytes();
        let additional_parts: Value = serde_json::from_slice(&body).unwrap();
        assert_eq!(additional_parts["TotalRecordCount"], 0);
        assert_eq!(additional_parts["Items"].as_array().unwrap().len(), 0);

        let response = app
            .clone()
            .oneshot(
                Request::builder()
                    .method(Method::DELETE)
                    .uri(format!("/Videos/{item_id}/AlternateSources"))
                    .header("X-Emby-Token", &api_key)
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::NO_CONTENT);

        let response = app
            .clone()
            .oneshot(
                Request::builder()
                    .method(Method::POST)
                    .uri("/Videos/MergeVersions")
                    .header("X-Emby-Token", &api_key)
                    .header(header::CONTENT_TYPE, "application/json")
                    .body(Body::from(json!({ "Ids": [item_id] }).to_string()))
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::NO_CONTENT);

        let response = app
            .clone()
            .oneshot(
                Request::builder()
                    .uri(format!("/Items/{item_id}/Images"))
                    .header("X-Emby-Token", &api_key)
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::OK);
        let body = response.into_body().collect().await.unwrap().to_bytes();
        let item_images: Value = serde_json::from_slice(&body).unwrap();
        assert_eq!(item_images.as_array().unwrap().len(), 0);

        let response = app
            .clone()
            .oneshot(
                Request::builder()
                    .uri(format!("/Items/{item_id}/Images/Primary"))
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::OK);
        assert_eq!(
            response.headers().get(header::CONTENT_TYPE).unwrap(),
            "image/png"
        );

        let response = app
            .clone()
            .oneshot(
                Request::builder()
                    .uri(format!("/Items/{parent_id}/Images/Primary"))
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::OK);
        assert_eq!(
            response.headers().get(header::CONTENT_TYPE).unwrap(),
            "image/png"
        );

        let response = app
            .clone()
            .oneshot(
                Request::builder()
                    .uri(format!("/Items/{item_id}/Images/Primary"))
                    .header("X-Emby-Token", &api_key)
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::OK);
        assert_eq!(
            response.headers().get(header::CONTENT_TYPE).unwrap(),
            "image/png"
        );

        let response = app
            .clone()
            .oneshot(
                Request::builder()
                    .uri("/Items/not-a-valid-id/Images/Primary")
                    .header("X-Emby-Token", &api_key)
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::BAD_REQUEST);

        let response = app
            .clone()
            .oneshot(
                Request::builder()
                    .uri("/Items/00000000-0000-0000-0000-000000000000/Images/Primary")
                    .header("X-Emby-Token", &api_key)
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::NOT_FOUND);

        let response = app
            .clone()
            .oneshot(
                Request::builder()
                    .uri(format!("/Users/{user_id}/Images/Primary"))
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::OK);
        assert_eq!(
            response.headers().get(header::CONTENT_TYPE).unwrap(),
            "image/png"
        );

        let response = app
            .clone()
            .oneshot(
                Request::builder()
                    .uri(format!("/Users/{user_id}/Images/Primary"))
                    .header("X-Emby-Token", &api_key)
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::OK);
        assert_eq!(
            response.headers().get(header::CONTENT_TYPE).unwrap(),
            "image/png"
        );

        let response = app
            .clone()
            .oneshot(
                Request::builder()
                    .uri("/Users/00000000-0000-0000-0000-000000000000/Images/Primary")
                    .header("X-Emby-Token", &api_key)
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::NOT_FOUND);

        let response = app
            .clone()
            .oneshot(
                Request::builder()
                    .uri(format!("/Items/{item_id}/ThemeMedia"))
                    .header("X-Emby-Token", &api_key)
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::OK);
        let body = response.into_body().collect().await.unwrap().to_bytes();
        let theme_media: Value = serde_json::from_slice(&body).unwrap();
        assert_eq!(theme_media["ThemeVideosResult"]["TotalRecordCount"], 0);
        assert_eq!(
            theme_media["ThemeSongsResult"]["Items"]
                .as_array()
                .unwrap()
                .len(),
            0
        );
        assert_eq!(theme_media["SoundtrackSongsResult"]["StartIndex"], 0);

        for endpoint in [
            format!("/Items/{item_id}/ThemeSongs"),
            format!("/Items/{item_id}/ThemeVideos"),
        ] {
            let response = app
                .clone()
                .oneshot(
                    Request::builder()
                        .uri(endpoint)
                        .header("X-Emby-Token", &api_key)
                        .body(Body::empty())
                        .unwrap(),
                )
                .await
                .unwrap();
            assert_eq!(response.status(), StatusCode::OK);
            let body = response.into_body().collect().await.unwrap().to_bytes();
            let theme_items: Value = serde_json::from_slice(&body).unwrap();
            assert_eq!(theme_items["TotalRecordCount"], 0);
            assert_eq!(theme_items["StartIndex"], 0);
            assert_eq!(theme_items["Items"].as_array().unwrap().len(), 0);
        }

        let response = app
            .clone()
            .oneshot(
                Request::builder()
                    .uri("/Items/00000000-0000-0000-0000-000000000000/ThemeSongs")
                    .header("X-Emby-Token", &api_key)
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::NOT_FOUND);

        let response = app
            .clone()
            .oneshot(
                Request::builder()
                    .uri(format!("/Users/{user_id}/Items/{item_id}"))
                    .header("X-Emby-Token", &api_key)
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::OK);
        let body = response.into_body().collect().await.unwrap().to_bytes();
        let user_detail: Value = serde_json::from_slice(&body).unwrap();
        assert_eq!(user_detail["Id"], item_id);
        assert_eq!(user_detail["Name"], "Example Movie");

        let response = app
            .clone()
            .oneshot(
                Request::builder()
                    .uri(format!("/Items/{item_id}/Ancestors"))
                    .header("X-Emby-Token", &api_key)
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::OK);
        let body = response.into_body().collect().await.unwrap().to_bytes();
        let ancestors: Value = serde_json::from_slice(&body).unwrap();
        assert_eq!(ancestors.as_array().unwrap().len(), 1);
        assert_eq!(ancestors[0]["Id"], parent_id);
        assert_eq!(ancestors[0]["Type"], "CollectionFolder");
        assert_eq!(ancestors[0]["ImageTags"]["Primary"], "placeholder");

        let response = app
            .clone()
            .oneshot(
                Request::builder()
                    .uri(format!("/Items/{item_id}/PlaybackInfo"))
                    .header("X-Emby-Token", &api_key)
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::OK);
        let body = response.into_body().collect().await.unwrap().to_bytes();
        let playback_info: Value = serde_json::from_slice(&body).unwrap();
        assert_eq!(playback_info["ErrorCode"], Value::Null);
        assert!(
            playback_info["PlaySessionId"]
                .as_str()
                .is_some_and(|id| !id.is_empty())
        );
        assert_eq!(playback_info["MediaSources"][0]["SupportsDirectPlay"], true);
        assert_eq!(
            playback_info["MediaSources"][0]["SupportsTranscoding"],
            false
        );
        assert_eq!(
            playback_info["MediaSources"][0]["MediaStreams"][0]["Type"],
            "Video"
        );
        assert_eq!(
            playback_info["MediaSources"][0]["MediaStreams"][0]["Codec"],
            "h264"
        );
        assert_eq!(
            playback_info["MediaSources"][0]["RunTimeTicks"],
            987_650_000
        );
        assert_eq!(playback_info["MediaSources"][0]["Bitrate"], 2_500_000);
        assert_eq!(
            playback_info["MediaSources"][0]["DefaultAudioStreamIndex"],
            1
        );
        assert_eq!(
            playback_info["MediaSources"][0]["DefaultSubtitleStreamIndex"],
            2
        );
        assert_eq!(
            playback_info["MediaSources"][0]["MediaStreams"][0]["Width"],
            1920
        );
        assert_eq!(
            playback_info["MediaSources"][0]["MediaStreams"][0]["Height"],
            1080
        );
        assert_eq!(
            playback_info["MediaSources"][0]["MediaStreams"][1]["Type"],
            "Audio"
        );
        assert_eq!(
            playback_info["MediaSources"][0]["MediaStreams"][1]["Channels"],
            2
        );
        assert_eq!(
            playback_info["MediaSources"][0]["MediaStreams"][2]["Type"],
            "Subtitle"
        );

        let response = app
            .clone()
            .oneshot(
                Request::builder()
                    .uri(format!("/MediaInfo/Items/{item_id}/PlaybackInfo"))
                    .header("X-Emby-Token", &api_key)
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::OK);
        let body = response.into_body().collect().await.unwrap().to_bytes();
        let media_info_playback_info: Value = serde_json::from_slice(&body).unwrap();
        assert_eq!(media_info_playback_info["ErrorCode"], Value::Null);
        assert_eq!(
            media_info_playback_info["MediaSources"][0]["SupportsDirectPlay"],
            true
        );

        let response = app
            .clone()
            .oneshot(
                Request::builder()
                    .uri("/MediaInfo/Playback/BitrateTest?Size=8")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::OK);
        let body = response.into_body().collect().await.unwrap().to_bytes();
        assert_eq!(body.len(), 8);

        let response = app
            .clone()
            .oneshot(
                Request::builder()
                    .method(Method::POST)
                    .uri("/MediaInfo/LiveStreams/Open")
                    .header("X-Emby-Token", &api_key)
                    .header(header::CONTENT_TYPE, "application/json")
                    .body(Body::from(json!({ "ItemId": item_id }).to_string()))
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::OK);
        let body = response.into_body().collect().await.unwrap().to_bytes();
        let live_stream: Value = serde_json::from_slice(&body).unwrap();
        assert_eq!(live_stream["LiveStreamId"], item_id);
        assert_eq!(live_stream["MediaSourceId"], item_id);
        assert_eq!(live_stream["MediaSource"]["Id"], item_id);
        assert_eq!(live_stream["MediaSource"]["SupportsDirectPlay"], true);

        let response = app
            .clone()
            .oneshot(
                Request::builder()
                    .method(Method::POST)
                    .uri("/MediaInfo/LiveStreams/Close")
                    .header("X-Emby-Token", &api_key)
                    .header(header::CONTENT_TYPE, "application/json")
                    .body(Body::from(json!({ "LiveStreamId": item_id }).to_string()))
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::NO_CONTENT);

        let response = app
            .clone()
            .oneshot(
                Request::builder()
                    .method(Method::POST)
                    .uri(format!(
                        "/Playstate/PlayingItems/{item_id}?PositionTicks=123&AudioStreamIndex=1&SubtitleStreamIndex=-1"
                    ))
                    .header("X-Emby-Token", &api_key)
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::NO_CONTENT);
        let playback_state = test_db
            .playback_state_for_item(user.id, parse_jellyfin_uuid(item_id).unwrap())
            .await
            .unwrap()
            .unwrap();
        assert_eq!(playback_state.position_ticks, 123);
        assert_eq!(playback_state.audio_stream_index, Some(1));
        assert_eq!(playback_state.subtitle_stream_index, Some(-1));

        let response = app
            .clone()
            .oneshot(
                Request::builder()
                    .method(Method::POST)
                    .uri(format!(
                        "/Playstate/PlayingItems/{item_id}/Progress?PositionTicks=456"
                    ))
                    .header("X-Emby-Token", &api_key)
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::NO_CONTENT);
        let playback_state = test_db
            .playback_state_for_item(user.id, parse_jellyfin_uuid(item_id).unwrap())
            .await
            .unwrap()
            .unwrap();
        assert_eq!(playback_state.position_ticks, 456);

        let response = app
            .clone()
            .oneshot(
                Request::builder()
                    .method(Method::DELETE)
                    .uri(format!(
                        "/Playstate/PlayingItems/{item_id}?PositionTicks=789"
                    ))
                    .header("X-Emby-Token", &api_key)
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::NO_CONTENT);
        assert!(test_db.active_playback_sessions().await.unwrap().is_empty());

        let response = app
            .clone()
            .oneshot(
                Request::builder()
                    .method(Method::POST)
                    .uri(format!("/Playstate/UserPlayedItems/{item_id}"))
                    .header("X-Emby-Token", &api_key)
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::NO_CONTENT);
        let playback_state = test_db
            .playback_state_for_item(user.id, parse_jellyfin_uuid(item_id).unwrap())
            .await
            .unwrap()
            .unwrap();
        assert!(playback_state.played);

        let response = app
            .clone()
            .oneshot(
                Request::builder()
                    .method(Method::DELETE)
                    .uri(format!("/Playstate/UserPlayedItems/{item_id}"))
                    .header("X-Emby-Token", &api_key)
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::NO_CONTENT);
        let playback_state = test_db
            .playback_state_for_item(user.id, parse_jellyfin_uuid(item_id).unwrap())
            .await
            .unwrap()
            .unwrap();
        assert!(!playback_state.played);

        let response = app
            .clone()
            .oneshot(
                Request::builder()
                    .method(Method::POST)
                    .uri(format!("/Items/{item_id}/PlaybackInfo"))
                    .header("X-Emby-Token", &api_key)
                    .header(header::CONTENT_TYPE, "application/json")
                    .body(Body::from(
                        json!({
                            "MediaSourceId": item_id,
                            "AudioStreamIndex": 1,
                            "SubtitleStreamIndex": -1
                        })
                        .to_string(),
                    ))
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::OK);
        let body = response.into_body().collect().await.unwrap().to_bytes();
        let selected_streams_info: Value = serde_json::from_slice(&body).unwrap();
        assert_eq!(selected_streams_info["ErrorCode"], Value::Null);
        assert_eq!(
            selected_streams_info["MediaSources"][0]["DefaultAudioStreamIndex"],
            1
        );
        assert_eq!(
            selected_streams_info["MediaSources"][0]["DefaultSubtitleStreamIndex"],
            Value::Null
        );

        let response = app
            .clone()
            .oneshot(
                Request::builder()
                    .method(Method::POST)
                    .uri(format!("/Items/{item_id}/PlaybackInfo"))
                    .header("X-Emby-Token", &api_key)
                    .header(header::CONTENT_TYPE, "application/json")
                    .body(Body::from(
                        json!({
                            "MediaSourceId": parse_jellyfin_uuid(item_id).unwrap().to_string(),
                            "AudioStreamIndex": 1
                        })
                        .to_string(),
                    ))
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::OK);
        let body = response.into_body().collect().await.unwrap().to_bytes();
        let hyphenated_media_source_info: Value = serde_json::from_slice(&body).unwrap();
        assert_eq!(hyphenated_media_source_info["ErrorCode"], Value::Null);
        assert_eq!(
            hyphenated_media_source_info["MediaSources"][0]["DefaultAudioStreamIndex"],
            1
        );

        let response = app
            .clone()
            .oneshot(
                Request::builder()
                    .uri(format!(
                        "/Items/{item_id}/PlaybackInfo?AudioStreamIndex=999"
                    ))
                    .header("X-Emby-Token", &api_key)
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::OK);
        let body = response.into_body().collect().await.unwrap().to_bytes();
        let invalid_audio_selection_info: Value = serde_json::from_slice(&body).unwrap();
        assert_eq!(
            invalid_audio_selection_info["ErrorCode"],
            "NoCompatibleStream"
        );
        assert_eq!(
            invalid_audio_selection_info["MediaSources"]
                .as_array()
                .unwrap()
                .len(),
            0
        );

        let response = app
            .clone()
            .oneshot(
                Request::builder()
                    .uri(format!(
                        "/Items/{item_id}/PlaybackInfo?SubtitleStreamIndex=999"
                    ))
                    .header("X-Emby-Token", &api_key)
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::OK);
        let body = response.into_body().collect().await.unwrap().to_bytes();
        let invalid_subtitle_selection_info: Value = serde_json::from_slice(&body).unwrap();
        assert_eq!(
            invalid_subtitle_selection_info["ErrorCode"],
            "NoCompatibleStream"
        );
        assert_eq!(
            invalid_subtitle_selection_info["MediaSources"]
                .as_array()
                .unwrap()
                .len(),
            0
        );

        let response = app
            .clone()
            .oneshot(
                Request::builder()
                    .method(Method::POST)
                    .uri(format!("/Items/{item_id}/PlaybackInfo"))
                    .header("X-Emby-Token", &api_key)
                    .header(header::CONTENT_TYPE, "application/json")
                    .body(Body::from(
                        json!({
                            "MediaSourceId": "00000000000000000000000000000000",
                            "AudioStreamIndex": 1
                        })
                        .to_string(),
                    ))
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::OK);
        let body = response.into_body().collect().await.unwrap().to_bytes();
        let invalid_media_source_info: Value = serde_json::from_slice(&body).unwrap();
        assert_eq!(invalid_media_source_info["ErrorCode"], "NoCompatibleStream");
        assert_eq!(
            invalid_media_source_info["MediaSources"]
                .as_array()
                .unwrap()
                .len(),
            0
        );

        let response = app
            .clone()
            .oneshot(
                Request::builder()
                    .uri(format!(
                        "/Items/{item_id}/PlaybackInfo?enableDirectPlay=false&enableDirectStream=true&enableTranscoding=true"
                    ))
                    .header("X-Emby-Token", &api_key)
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::OK);
        let body = response.into_body().collect().await.unwrap().to_bytes();
        let direct_stream_only_info: Value = serde_json::from_slice(&body).unwrap();
        assert_eq!(direct_stream_only_info["ErrorCode"], Value::Null);
        assert_eq!(
            direct_stream_only_info["MediaSources"][0]["SupportsDirectPlay"],
            false
        );
        assert_eq!(
            direct_stream_only_info["MediaSources"][0]["SupportsDirectStream"],
            true
        );
        assert_eq!(
            direct_stream_only_info["MediaSources"][0]["SupportsTranscoding"],
            false
        );

        let response = app
            .clone()
            .oneshot(
                Request::builder()
                    .uri(format!(
                        "/Items/{item_id}/PlaybackInfo?EnableDirectPlay=false&EnableDirectStream=false&EnableTranscoding=true"
                    ))
                    .header("X-Emby-Token", &api_key)
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::OK);
        let body = response.into_body().collect().await.unwrap().to_bytes();
        let transcode_only_info: Value = serde_json::from_slice(&body).unwrap();
        let transcode_play_session_id =
            assert_hls_transcode_playback_info(&transcode_only_info, item_id, &api_key);
        let transcode_session = test_db
            .transcode_session_by_play_session_id(&transcode_play_session_id)
            .await
            .unwrap()
            .unwrap();
        assert_eq!(
            transcode_session.device_id.as_deref(),
            Some("api-key:test-key")
        );
        assert_eq!(transcode_session.item.id.simple().to_string(), item_id);
        assert_eq!(transcode_session.audio_stream_index, Some(1));
        assert_eq!(transcode_session.video_stream_index, Some(0));
        assert!(
            transcode_session
                .output_path
                .ends_with(&format!("{transcode_play_session_id}/main.m3u8"))
        );
        assert_eq!(transcode_session.position_ticks, 0);

        let response = app
            .clone()
            .oneshot(
                Request::builder()
                    .uri(format!(
                        "/Items/{item_id}/PlaybackInfo?EnableDirectPlay=false&EnableDirectStream=false&EnableTranscoding=true&StartTimeTicks=12345000000"
                    ))
                    .header("X-Emby-Token", &api_key)
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::OK);
        let body = response.into_body().collect().await.unwrap().to_bytes();
        let seek_transcode_info: Value = serde_json::from_slice(&body).unwrap();
        let seek_play_session_id =
            assert_hls_transcode_playback_info(&seek_transcode_info, item_id, &api_key);
        assert_ne!(seek_play_session_id, transcode_play_session_id);
        let seek_transcode_session = test_db
            .transcode_session_by_play_session_id(&seek_play_session_id)
            .await
            .unwrap()
            .unwrap();
        assert_eq!(seek_transcode_session.position_ticks, 12_345_000_000);

        let response = app
            .clone()
            .oneshot(
                Request::builder()
                    .method(Method::POST)
                    .uri(format!("/Items/{item_id}/PlaybackInfo"))
                    .header("X-Emby-Token", &api_key)
                    .header(header::CONTENT_TYPE, "application/json")
                    .body(Body::from(json!({ "DeviceProfile": {} }).to_string()))
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::OK);
        let body = response.into_body().collect().await.unwrap().to_bytes();
        let posted_playback_info: Value = serde_json::from_slice(&body).unwrap();
        assert!(
            posted_playback_info["PlaySessionId"]
                .as_str()
                .is_some_and(|id| !id.is_empty())
        );
        assert_eq!(
            posted_playback_info["MediaSources"][0]["SupportsDirectPlay"],
            true
        );

        let response = app
            .clone()
            .oneshot(
                Request::builder()
                    .method(Method::POST)
                    .uri(format!("/Items/{item_id}/PlaybackInfo"))
                    .header("X-Emby-Token", &api_key)
                    .header(header::CONTENT_TYPE, "application/json")
                    .body(Body::from(
                        json!({
                            "DeviceProfile": {
                                "DirectPlayProfiles": [
                                    {
                                        "Type": "Video",
                                        "Container": "mp4,m4v",
                                        "VideoCodec": "h264",
                                        "AudioCodec": "aac"
                                    }
                                ]
                            }
                        })
                        .to_string(),
                    ))
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::OK);
        let body = response.into_body().collect().await.unwrap().to_bytes();
        let matching_profile_info: Value = serde_json::from_slice(&body).unwrap();
        assert_eq!(matching_profile_info["ErrorCode"], Value::Null);
        assert_eq!(
            matching_profile_info["MediaSources"][0]["SupportsDirectPlay"],
            true
        );
        assert_eq!(
            matching_profile_info["MediaSources"][0]["SupportsDirectStream"],
            true
        );

        let response = app
            .clone()
            .oneshot(
                Request::builder()
                    .method(Method::POST)
                    .uri(format!("/Items/{item_id}/PlaybackInfo"))
                    .header("X-Emby-Token", &api_key)
                    .header(header::CONTENT_TYPE, "application/json")
                    .body(Body::from(
                        json!({
                            "EnableDirectStream": false,
                            "DeviceProfile": {
                                "DirectPlayProfiles": [
                                    {
                                        "Type": "Video",
                                        "Container": "mkv",
                                        "VideoCodec": "hevc",
                                        "AudioCodec": "opus"
                                    }
                                ]
                            }
                        })
                        .to_string(),
                    ))
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::OK);
        let body = response.into_body().collect().await.unwrap().to_bytes();
        let mismatching_profile_info: Value = serde_json::from_slice(&body).unwrap();
        assert_hls_transcode_playback_info(&mismatching_profile_info, item_id, &api_key);

        let response = app
            .clone()
            .oneshot(
                Request::builder()
                    .method(Method::POST)
                    .uri(format!("/Items/{item_id}/PlaybackInfo"))
                    .header("X-Emby-Token", &api_key)
                    .header(header::CONTENT_TYPE, "application/json")
                    .body(Body::from(
                        json!({
                            "EnableDirectStream": false,
                            "DeviceProfile": {
                                "DirectPlayProfiles": []
                            }
                        })
                        .to_string(),
                    ))
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::OK);
        let body = response.into_body().collect().await.unwrap().to_bytes();
        let empty_profiles_info: Value = serde_json::from_slice(&body).unwrap();
        assert_hls_transcode_playback_info(&empty_profiles_info, item_id, &api_key);

        let response = app
            .clone()
            .oneshot(
                Request::builder()
                    .method(Method::POST)
                    .uri(format!("/Items/{item_id}/PlaybackInfo"))
                    .header("X-Emby-Token", &api_key)
                    .header(header::CONTENT_TYPE, "application/json")
                    .body(Body::from(
                        json!({
                            "DeviceProfile": {
                                "DirectPlayProfiles": [
                                    {
                                        "Type": "Video",
                                        "Container": "mkv",
                                        "VideoCodec": "hevc",
                                        "AudioCodec": "opus"
                                    }
                                ]
                            }
                        })
                        .to_string(),
                    ))
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::OK);
        let body = response.into_body().collect().await.unwrap().to_bytes();
        let mismatching_but_direct_stream_info: Value = serde_json::from_slice(&body).unwrap();
        assert_eq!(mismatching_but_direct_stream_info["ErrorCode"], Value::Null);
        assert_eq!(
            mismatching_but_direct_stream_info["MediaSources"][0]["SupportsDirectPlay"],
            false
        );
        assert_eq!(
            mismatching_but_direct_stream_info["MediaSources"][0]["SupportsDirectStream"],
            true
        );

        let response = app
            .clone()
            .oneshot(
                Request::builder()
                    .method(Method::POST)
                    .uri(format!("/Items/{item_id}/PlaybackInfo"))
                    .header("X-Emby-Token", &api_key)
                    .header(header::CONTENT_TYPE, "application/json")
                    .body(Body::from(
                        json!({
                            "EnableDirectPlay": false,
                            "EnableDirectStream": false,
                            "EnableTranscoding": true,
                            "DeviceProfile": {}
                        })
                        .to_string(),
                    ))
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::OK);
        let body = response.into_body().collect().await.unwrap().to_bytes();
        let posted_transcode_only_info: Value = serde_json::from_slice(&body).unwrap();
        assert_hls_transcode_playback_info(&posted_transcode_only_info, item_id, &api_key);

        let response = app
            .clone()
            .oneshot(
                Request::builder()
                    .method(Method::POST)
                    .uri(format!(
                        "/Items/{item_id}/PlaybackInfo?EnableDirectStream=true"
                    ))
                    .header("X-Emby-Token", &api_key)
                    .header(header::CONTENT_TYPE, "application/json")
                    .body(Body::from(
                        json!({
                            "EnableDirectPlay": false,
                            "EnableDirectStream": false,
                            "EnableTranscoding": true
                        })
                        .to_string(),
                    ))
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::OK);
        let body = response.into_body().collect().await.unwrap().to_bytes();
        let query_overrides_body_info: Value = serde_json::from_slice(&body).unwrap();
        assert_eq!(query_overrides_body_info["ErrorCode"], Value::Null);
        assert_eq!(
            query_overrides_body_info["MediaSources"][0]["SupportsDirectStream"],
            true
        );

        let response = app
            .clone()
            .oneshot(
                Request::builder()
                    .method(Method::POST)
                    .uri("/Users/AuthenticateByName")
                    .header(header::CONTENT_TYPE, "application/json")
                    .header(
                        header::AUTHORIZATION,
                        r#"MediaBrowser Client="Jellyfin Web", Device="Firefox", DeviceId="playback-device", Version="dev""#,
                    )
                    .body(Body::from(
                        json!({ "Username": "admin", "Pw": "secret" }).to_string(),
                    ))
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::OK);
        let body = response.into_body().collect().await.unwrap().to_bytes();
        let login: Value = serde_json::from_slice(&body).unwrap();
        let playback_token = login["AccessToken"].as_str().unwrap();
        let response = app
            .clone()
            .oneshot(
                Request::builder()
                    .method(Method::POST)
                    .uri("/Sessions/Playing/Progress")
                    .header("X-Emby-Token", playback_token)
                    .header(header::CONTENT_TYPE, "application/json")
                    .body(Body::from(
                        json!({
                            "ItemId": item_id,
                            "MediaSourceId": item_id,
                            "AudioStreamIndex": 1,
                            "SubtitleStreamIndex": -1,
                            "PositionTicks": 25_000_000,
                            "CanSeek": true,
                            "IsPaused": false,
                        })
                        .to_string(),
                    ))
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::NO_CONTENT);

        let response = app
            .clone()
            .oneshot(
                Request::builder()
                    .uri("/Sessions")
                    .header("X-Emby-Token", playback_token)
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::OK);
        let body = response.into_body().collect().await.unwrap().to_bytes();
        let sessions: Value = serde_json::from_slice(&body).unwrap();
        assert_eq!(sessions[0]["DeviceId"], "playback-device");
        assert_eq!(sessions[0]["NowPlayingItem"]["Id"], item_id);
        assert_eq!(sessions[0]["PlayState"]["PositionTicks"], 25_000_000);
        assert_eq!(sessions[0]["PlayState"]["IsPaused"], false);
        assert_eq!(sessions[0]["PlayState"]["AudioStreamIndex"], 1);
        assert_eq!(sessions[0]["PlayState"]["SubtitleStreamIndex"], -1);

        let response = app
            .clone()
            .oneshot(
                Request::builder()
                    .method(Method::POST)
                    .uri("/Sessions/Playing/Stopped")
                    .header("X-Emby-Token", playback_token)
                    .header(header::CONTENT_TYPE, "application/json")
                    .body(Body::from(
                        json!({
                            "ItemId": item_id,
                            "MediaSourceId": item_id,
                            "AudioStreamIndex": 1,
                            "SubtitleStreamIndex": -1,
                            "PositionTicks": 25_000_000,
                            "IsPaused": false,
                        })
                        .to_string(),
                    ))
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::NO_CONTENT);

        let response = app
            .clone()
            .oneshot(
                Request::builder()
                    .uri("/Sessions")
                    .header("X-Emby-Token", playback_token)
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::OK);
        let body = response.into_body().collect().await.unwrap().to_bytes();
        let stopped_sessions: Value = serde_json::from_slice(&body).unwrap();
        assert_eq!(stopped_sessions[0]["NowPlayingItem"], Value::Null);

        for (endpoint, position_ticks) in [
            ("/Sessions/Playing", 0_i64),
            ("/Sessions/Playing/Progress", 50_000_000_i64),
            ("/Sessions/Playing/Stopped", 50_000_000_i64),
        ] {
            let response = app
                .clone()
                .oneshot(
                    Request::builder()
                        .method(Method::POST)
                        .uri(endpoint)
                        .header("X-Emby-Token", &api_key)
                        .header(header::CONTENT_TYPE, "application/json")
                        .body(Body::from(
                            json!({
                                "ItemId": item_id,
                                "MediaSourceId": item_id,
                                "AudioStreamIndex": 1,
                                "SubtitleStreamIndex": -1,
                                "PositionTicks": position_ticks,
                                "CanSeek": true,
                                "IsPaused": false,
                            })
                            .to_string(),
                        ))
                        .unwrap(),
                )
                .await
                .unwrap();
            assert_eq!(response.status(), StatusCode::NO_CONTENT, "{endpoint}");
        }

        let response = app
            .clone()
            .oneshot(
                Request::builder()
                    .uri("/UserItems/Resume")
                    .header("X-Emby-Token", &api_key)
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::OK);
        let body = response.into_body().collect().await.unwrap().to_bytes();
        let resume: Value = serde_json::from_slice(&body).unwrap();
        assert_eq!(resume["TotalRecordCount"], 1);
        assert_eq!(resume["Items"][0]["Id"], item_id);
        assert_eq!(
            resume["Items"][0]["UserData"]["PlaybackPositionTicks"],
            50_000_000
        );
        assert_eq!(resume["Items"][0]["UserData"]["Played"], false);
        assert_eq!(
            resume["Items"][0]["MediaSources"][0]["DefaultAudioStreamIndex"],
            1
        );
        assert_eq!(
            resume["Items"][0]["MediaSources"][0]["DefaultSubtitleStreamIndex"],
            Value::Null
        );

        let response = app
            .clone()
            .oneshot(
                Request::builder()
                    .method(Method::POST)
                    .uri(format!("/Users/{user_id}/PlayedItems/{item_id}"))
                    .header("X-Emby-Token", &api_key)
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::NO_CONTENT);

        let response = app
            .clone()
            .oneshot(
                Request::builder()
                    .uri(format!("/Users/{user_id}/Items/{item_id}"))
                    .header("X-Emby-Token", &api_key)
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::OK);
        let body = response.into_body().collect().await.unwrap().to_bytes();
        let user_item_after_played: Value = serde_json::from_slice(&body).unwrap();
        assert_eq!(user_item_after_played["UserData"]["Played"], true);
        assert_eq!(user_item_after_played["UserData"]["PlayCount"], 1);

        let response = app
            .clone()
            .oneshot(
                Request::builder()
                    .uri(format!("/Users/{user_id}/Items?IsPlayed=true"))
                    .header("X-Emby-Token", &api_key)
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::OK);
        let body = response.into_body().collect().await.unwrap().to_bytes();
        let played_items: Value = serde_json::from_slice(&body).unwrap();
        assert_eq!(played_items["TotalRecordCount"], 1);
        assert_eq!(played_items["Items"][0]["Id"], item_id);
        assert_eq!(played_items["Items"][0]["UserData"]["Played"], true);

        let response = app
            .clone()
            .oneshot(
                Request::builder()
                    .uri(format!("/Items?UserId={user_id}&IsPlayed=true"))
                    .header("X-Emby-Token", &api_key)
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::OK);
        let body = response.into_body().collect().await.unwrap().to_bytes();
        let user_context_items: Value = serde_json::from_slice(&body).unwrap();
        assert_eq!(user_context_items["TotalRecordCount"], 1);
        assert_eq!(user_context_items["Items"][0]["UserData"]["Played"], true);

        let response = app
            .clone()
            .oneshot(
                Request::builder()
                    .uri(format!(
                        "/Users/{user_id}/Items/Counts?IncludeItemTypes=Movie&Filters=IsPlayed"
                    ))
                    .header("X-Emby-Token", &api_key)
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::OK);
        let body = response.into_body().collect().await.unwrap().to_bytes();
        let user_played_counts: Value = serde_json::from_slice(&body).unwrap();
        assert_eq!(user_played_counts["MovieCount"], 1);
        assert_eq!(user_played_counts["ItemCount"], 1);

        let response = app
            .clone()
            .oneshot(
                Request::builder()
                    .uri(format!(
                        "/Users/{user_id}/Items/Latest?IncludeItemTypes=Movie&Limit=1"
                    ))
                    .header("X-Emby-Token", &api_key)
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::OK);
        let body = response.into_body().collect().await.unwrap().to_bytes();
        let user_latest: Value = serde_json::from_slice(&body).unwrap();
        assert_eq!(user_latest.as_array().unwrap().len(), 1);
        assert_eq!(user_latest[0]["Id"], item_id);
        assert_eq!(user_latest[0]["UserData"]["Played"], true);

        let response = app
            .clone()
            .oneshot(
                Request::builder()
                    .uri("/UserItems/Resume")
                    .header("X-Emby-Token", &api_key)
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::OK);
        let body = response.into_body().collect().await.unwrap().to_bytes();
        let resume_after_played: Value = serde_json::from_slice(&body).unwrap();
        assert_eq!(resume_after_played["TotalRecordCount"], 0);
        assert_eq!(resume_after_played["Items"].as_array().unwrap().len(), 0);

        let response = app
            .clone()
            .oneshot(
                Request::builder()
                    .method(Method::DELETE)
                    .uri(format!("/users/{user_id}/playeditems/{item_id}"))
                    .header("X-Emby-Token", &api_key)
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::NO_CONTENT);

        let response = app
            .clone()
            .oneshot(
                Request::builder()
                    .uri(format!("/Videos/{item_id}/stream"))
                    .header("X-Emby-Token", &api_key)
                    .header(header::RANGE, "bytes=0-3")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::PARTIAL_CONTENT);
        assert_eq!(
            response.headers().get(header::CONTENT_RANGE).unwrap(),
            "bytes 0-3/10"
        );
        assert_eq!(
            response.headers().get(header::CONTENT_TYPE).unwrap(),
            "video/mp4"
        );
        let body = response.into_body().collect().await.unwrap().to_bytes();
        assert_eq!(&body[..], b"fake");

        let response = app
            .clone()
            .oneshot(
                Request::builder()
                    .uri(format!("/Videos/{item_id}/stream"))
                    .header("X-Emby-Token", &api_key)
                    .header(header::RANGE, "bytes=-5")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::PARTIAL_CONTENT);
        assert_eq!(
            response.headers().get(header::CONTENT_RANGE).unwrap(),
            "bytes 5-9/10"
        );
        let body = response.into_body().collect().await.unwrap().to_bytes();
        assert_eq!(&body[..], b"video");

        let response = app
            .clone()
            .oneshot(
                Request::builder()
                    .method(Method::HEAD)
                    .uri(format!("/Videos/{item_id}/stream"))
                    .header("X-Emby-Token", &api_key)
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::OK);
        assert_eq!(
            response.headers().get(header::CONTENT_LENGTH).unwrap(),
            "10"
        );
        assert_eq!(
            response.headers().get(header::ACCEPT_RANGES).unwrap(),
            "bytes"
        );
        let body = response.into_body().collect().await.unwrap().to_bytes();
        assert!(body.is_empty());

        let response = app
            .clone()
            .oneshot(
                Request::builder()
                    .uri(format!("/Videos/{item_id}/stream.mp4?Static=true"))
                    .header("X-Emby-Token", &api_key)
                    .header(header::RANGE, "bytes=0-3")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::PARTIAL_CONTENT);
        assert_eq!(
            response.headers().get(header::CONTENT_RANGE).unwrap(),
            "bytes 0-3/10"
        );
        assert_eq!(
            response.headers().get(header::CONTENT_TYPE).unwrap(),
            "video/mp4"
        );
        let body = response.into_body().collect().await.unwrap().to_bytes();
        assert_eq!(&body[..], b"fake");

        let response = app
            .clone()
            .oneshot(
                Request::builder()
                    .method(Method::HEAD)
                    .uri(format!("/videos/{item_id}/stream.mp4?Static=true"))
                    .header("X-Emby-Token", &api_key)
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::OK);
        assert_eq!(
            response.headers().get(header::CONTENT_LENGTH).unwrap(),
            "10"
        );
        assert_eq!(
            response.headers().get(header::CONTENT_TYPE).unwrap(),
            "video/mp4"
        );
        let body = response.into_body().collect().await.unwrap().to_bytes();
        assert!(body.is_empty());

        let response = app
            .clone()
            .oneshot(
                Request::builder()
                    .uri(format!("/Videos/{item_id}/stream"))
                    .header("X-Emby-Token", &api_key)
                    .header(header::RANGE, "bytes=99-100")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::RANGE_NOT_SATISFIABLE);
        assert_eq!(
            response.headers().get(header::CONTENT_RANGE).unwrap(),
            "bytes */10"
        );

        let response = app
            .clone()
            .oneshot(
                Request::builder()
                    .uri(format!(
                        "/Videos/{item_id}/stream?Static=true&MediaSourceId={item_id}&DeviceId=test-device&PlaySessionId=test-play-session&Tag=test-tag"
                    ))
                    .header("X-Emby-Token", &api_key)
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::OK);
        assert_eq!(
            response.headers().get(header::CONTENT_TYPE).unwrap(),
            "video/mp4"
        );

        let extra_movie = tmp.path().join("Second Movie.mp4");
        tokio::fs::write(&extra_movie, b"second video")
            .await
            .unwrap();
        let response = app
            .clone()
            .oneshot(
                Request::builder()
                    .method(Method::POST)
                    .uri("/ScheduledTasks/Running/scan-media-library")
                    .header("X-Emby-Token", &api_key)
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::NO_CONTENT);

        let mut rescanned = json!({});
        for _ in 0..20 {
            let response = app
                .clone()
                .oneshot(
                    Request::builder()
                        .uri("/Items")
                        .header("X-Emby-Token", &api_key)
                        .body(Body::empty())
                        .unwrap(),
                )
                .await
                .unwrap();
            assert_eq!(response.status(), StatusCode::OK);
            let body = response.into_body().collect().await.unwrap().to_bytes();
            rescanned = serde_json::from_slice(&body).unwrap();
            if rescanned["TotalRecordCount"] == 2 {
                break;
            }
            tokio::time::sleep(std::time::Duration::from_millis(25)).await;
        }
        assert_eq!(rescanned["TotalRecordCount"], 2);

        let response = app
            .clone()
            .oneshot(
                Request::builder()
                    .uri("/Items?IncludeItemTypes=Movie&StartIndex=1&Limit=1&SortBy=SortName")
                    .header("X-Emby-Token", &api_key)
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::OK);
        let body = response.into_body().collect().await.unwrap().to_bytes();
        let paged: Value = serde_json::from_slice(&body).unwrap();
        assert_eq!(paged["TotalRecordCount"], 2);
        assert_eq!(paged["StartIndex"], 1);
        assert_eq!(paged["Items"].as_array().unwrap().len(), 1);
        assert_eq!(paged["Items"][0]["Name"], "Second Movie");

        let second_item_id = paged["Items"][0]["Id"].as_str().unwrap();
        let response = app
            .clone()
            .oneshot(
                Request::builder()
                    .uri(format!("/Items?Ids={second_item_id}&SearchTerm=second&MediaTypes=Video&ExcludeItemTypes=Audio&IsFolder=false"))
                    .header("X-Emby-Token", &api_key)
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::OK);
        let body = response.into_body().collect().await.unwrap().to_bytes();
        let filtered_items: Value = serde_json::from_slice(&body).unwrap();
        assert_eq!(filtered_items["TotalRecordCount"], 1);
        assert_eq!(filtered_items["Items"][0]["Id"], second_item_id);

        let response = app
            .clone()
            .oneshot(
                Request::builder()
                    .uri("/Items?ExcludeItemTypes=Movie")
                    .header("X-Emby-Token", &api_key)
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::OK);
        let body = response.into_body().collect().await.unwrap().to_bytes();
        let excluded_movies: Value = serde_json::from_slice(&body).unwrap();
        assert_eq!(excluded_movies["TotalRecordCount"], 0);

        let response = app
            .clone()
            .oneshot(
                Request::builder()
                    .uri("/Items?IsFolder=true")
                    .header("X-Emby-Token", &api_key)
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::OK);
        let body = response.into_body().collect().await.unwrap().to_bytes();
        let folders_only: Value = serde_json::from_slice(&body).unwrap();
        assert_eq!(folders_only["TotalRecordCount"], 0);

        let response = app
            .clone()
            .oneshot(
                Request::builder()
                    .uri("/Items?Filters=IsNotFolder&NameStartsWith=Sec&ImageTypeLimit=1&EnableImages=true&EnableUserData=true&CollapseBoxSetItems=false")
                    .header("X-Emby-Token", &api_key)
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::OK);
        let body = response.into_body().collect().await.unwrap().to_bytes();
        let prefixed: Value = serde_json::from_slice(&body).unwrap();
        assert_eq!(prefixed["TotalRecordCount"], 1);
        assert_eq!(prefixed["Items"][0]["Id"], second_item_id);

        let response = app
            .clone()
            .oneshot(
                Request::builder()
                    .uri("/Items?Filters=IsFolder")
                    .header("X-Emby-Token", &api_key)
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::OK);
        let body = response.into_body().collect().await.unwrap().to_bytes();
        let filtered_folders: Value = serde_json::from_slice(&body).unwrap();
        assert_eq!(filtered_folders["TotalRecordCount"], 0);

        let response = app
            .clone()
            .oneshot(
                Request::builder()
                    .uri(format!(
                        "/Users/{user_id}/Items?Filters=IsUnplayed&NameStartsWith=Sec"
                    ))
                    .header("X-Emby-Token", &api_key)
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::OK);
        let body = response.into_body().collect().await.unwrap().to_bytes();
        let unplayed_filter_alias: Value = serde_json::from_slice(&body).unwrap();
        assert_eq!(unplayed_filter_alias["TotalRecordCount"], 1);
        assert_eq!(unplayed_filter_alias["Items"][0]["Id"], second_item_id);

        let response = app
            .clone()
            .oneshot(
                Request::builder()
                    .uri("/Items?NameStartsWithOrGreater=Second&NameLessThan=Third&SortBy=DateCreated,SortName&SortOrder=Descending&Fields=PrimaryImageAspectRatio,MediaSources&ImageTypes=Primary&EnableTotalRecordCount=true")
                    .header("X-Emby-Token", &api_key)
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::OK);
        let body = response.into_body().collect().await.unwrap().to_bytes();
        let name_window: Value = serde_json::from_slice(&body).unwrap();
        assert_eq!(name_window["TotalRecordCount"], 1);
        assert_eq!(name_window["Items"][0]["Id"], second_item_id);

        let response = app
            .clone()
            .oneshot(
                Request::builder()
                    .uri("/Items?SearchTerm=missing-title")
                    .header("X-Emby-Token", &api_key)
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::OK);
        let body = response.into_body().collect().await.unwrap().to_bytes();
        let no_matches: Value = serde_json::from_slice(&body).unwrap();
        assert_eq!(no_matches["TotalRecordCount"], 0);

        let response = app
            .clone()
            .oneshot(
                Request::builder()
                    .uri(format!("/Items/{item_id}/Similar"))
                    .header("X-Emby-Token", &api_key)
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::OK);
        let body = response.into_body().collect().await.unwrap().to_bytes();
        let similar: Value = serde_json::from_slice(&body).unwrap();
        assert_eq!(similar["TotalRecordCount"], 0);
        assert_eq!(similar["Items"].as_array().unwrap().len(), 0);

        let response = app
            .oneshot(
                Request::builder()
                    .uri(format!("/Users/{user_id}/Items/{item_id}/Intros"))
                    .header("X-Emby-Token", &api_key)
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::OK);
        let body = response.into_body().collect().await.unwrap().to_bytes();
        let intros: Value = serde_json::from_slice(&body).unwrap();
        assert_eq!(intros["TotalRecordCount"], 0);
        assert_eq!(intros["Items"].as_array().unwrap().len(), 0);
    }

    #[tokio::test]
    async fn audio_items_support_direct_stream_routes() {
        let tmp = tempfile::tempdir().unwrap();
        let song = tmp.path().join("Example Song.mp3");
        tokio::fs::write(&song, b"fake audio").await.unwrap();
        let other_song = tmp.path().join("Second Song.mp3");
        tokio::fs::write(&other_song, b"other audio").await.unwrap();

        let db = Database::connect("sqlite::memory:").await.unwrap();
        let user = db
            .update_first_user("admin".to_string(), "secret")
            .await
            .unwrap();
        let api_key = db
            .issue_api_key_for_user(user.id, "test-key")
            .await
            .unwrap();
        let user_id = user.id.simple().to_string();
        let app = router(AppState {
            db,
            web_dir: ".".into(),
            log_dir: ".".into(),
            local_address: "http://127.0.0.1:8097".to_string(),
        });

        let response = app
            .clone()
            .oneshot(
                Request::builder()
                    .method(Method::POST)
                    .uri(format!(
                        "/Library/VirtualFolders?name=Music&collectionType=music&paths={}",
                        tmp.path().to_string_lossy()
                    ))
                    .header(header::CONTENT_TYPE, "application/json")
                    .body(Body::from("{}"))
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::NO_CONTENT);

        let response = app
            .clone()
            .oneshot(
                Request::builder()
                    .uri("/Items?IncludeItemTypes=Audio&Limit=1&SortBy=SortName")
                    .header("X-Emby-Token", &api_key)
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::OK);
        let body = response.into_body().collect().await.unwrap().to_bytes();
        let result: Value = serde_json::from_slice(&body).unwrap();
        assert_eq!(result["TotalRecordCount"], 2);
        assert_eq!(result["Items"].as_array().unwrap().len(), 1);
        assert_eq!(result["Items"][0]["Name"], "Example Song");
        assert_eq!(result["Items"][0]["Type"], "Audio");
        let item_id = result["Items"][0]["Id"].as_str().unwrap();
        assert_eq!(
            result["Items"][0]["MediaSources"][0]["DirectStreamUrl"],
            format!("/Audio/{item_id}/stream")
        );
        assert_eq!(
            result["Items"][0]["MediaSources"][0]["MediaStreams"][0]["Type"],
            "Audio"
        );
        assert_eq!(
            result["Items"][0]["MediaSources"][0]["VideoType"],
            Value::Null
        );

        let response = app
            .clone()
            .oneshot(
                Request::builder()
                    .uri("/Items/Counts")
                    .header("X-Emby-Token", &api_key)
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::OK);
        let body = response.into_body().collect().await.unwrap().to_bytes();
        let counts: Value = serde_json::from_slice(&body).unwrap();
        assert_eq!(counts["SongCount"], 2);
        assert_eq!(counts["ItemCount"], 2);

        let response = app
            .clone()
            .oneshot(
                Request::builder()
                    .uri(format!(
                        "/Items/{item_id}/InstantMix?UserId={}&Limit=1",
                        user.id
                    ))
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::UNAUTHORIZED);

        let response = app
            .clone()
            .oneshot(
                Request::builder()
                    .uri(format!(
                        "/Items/{item_id}/InstantMix?UserId={}&Limit=1",
                        user.id
                    ))
                    .header("X-Emby-Token", &api_key)
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::OK);
        let body = response.into_body().collect().await.unwrap().to_bytes();
        let instant_mix: Value = serde_json::from_slice(&body).unwrap();
        assert_eq!(instant_mix["TotalRecordCount"], 2);
        assert_eq!(instant_mix["StartIndex"], 0);
        assert_eq!(instant_mix["Items"].as_array().unwrap().len(), 1);
        assert_eq!(instant_mix["Items"][0]["Id"], item_id);
        assert_eq!(instant_mix["Items"][0]["Type"], "Audio");
        assert_eq!(
            instant_mix["Items"][0]["MediaSources"][0]["DirectStreamUrl"],
            format!("/Audio/{item_id}/stream")
        );
        assert_eq!(instant_mix["Items"][0]["UserData"]["Played"], false);

        let response = app
            .clone()
            .oneshot(
                Request::builder()
                    .uri(format!("/Songs/{item_id}/InstantMix?limit=5"))
                    .header("X-Emby-Token", &api_key)
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::OK);
        let body = response.into_body().collect().await.unwrap().to_bytes();
        let song_mix: Value = serde_json::from_slice(&body).unwrap();
        assert_eq!(song_mix["TotalRecordCount"], 2);
        assert_eq!(song_mix["Items"][0]["Id"], item_id);

        let response = app
            .clone()
            .oneshot(
                Request::builder()
                    .uri("/Items/not-a-uuid/InstantMix")
                    .header("X-Emby-Token", &api_key)
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::BAD_REQUEST);

        let response = app
            .clone()
            .oneshot(
                Request::builder()
                    .uri("/Items/00000000-0000-0000-0000-000000000000/InstantMix")
                    .header("X-Emby-Token", &api_key)
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::NOT_FOUND);

        let response = app
            .clone()
            .oneshot(
                Request::builder()
                    .method(Method::POST)
                    .uri("/Users/AuthenticateByName")
                    .header(header::CONTENT_TYPE, "application/json")
                    .header(
                        header::AUTHORIZATION,
                        r#"MediaBrowser Client="Jellyfin Web", Device="Firefox", DeviceId="audio-remote-device", Version="dev""#,
                    )
                    .body(Body::from(
                        json!({ "Username": "admin", "Pw": "secret" }).to_string(),
                    ))
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::OK);
        let body = response.into_body().collect().await.unwrap().to_bytes();
        let login: Value = serde_json::from_slice(&body).unwrap();
        let playback_token = login["AccessToken"].as_str().unwrap();
        let mut playback_events = subscribe_playback_events();

        let response = app
            .clone()
            .oneshot(
                Request::builder()
                    .method(Method::POST)
                    .uri(format!(
                        "/Sessions/{playback_token}/Playing?PlayCommand=PlayInstantMix&ItemIds={item_id}&StartPositionTicks=123&AudioStreamIndex=1&SubtitleStreamIndex=-1"
                    ))
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::UNAUTHORIZED);

        let response = app
            .clone()
            .oneshot(
                Request::builder()
                    .method(Method::POST)
                    .uri(format!(
                        "/Sessions/{playback_token}/Playing?PlayCommand=PlayInstantMix&ItemIds={item_id}&StartPositionTicks=123&AudioStreamIndex=1&SubtitleStreamIndex=-1"
                    ))
                    .header("X-Emby-Token", &api_key)
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::NO_CONTENT);
        let play_event =
            next_playback_event_type(&mut playback_events, playback_token, "Play").await;
        assert_eq!(play_event["MessageType"], "Play");
        assert_eq!(play_event["Data"]["PlayCommand"], "PlayNow");
        assert_eq!(play_event["Data"]["StartPositionTicks"], 123);
        assert_eq!(play_event["Data"]["AudioStreamIndex"], 1);
        assert_eq!(play_event["Data"]["SubtitleStreamIndex"], -1);
        assert_eq!(play_event["Data"]["ItemIds"].as_array().unwrap().len(), 2);
        assert_eq!(play_event["Data"]["ItemIds"][0], item_id);

        let response = app
            .clone()
            .oneshot(
                Request::builder()
                    .uri("/Sessions")
                    .header("X-Emby-Token", playback_token)
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::OK);
        let body = response.into_body().collect().await.unwrap().to_bytes();
        let sessions: Value = serde_json::from_slice(&body).unwrap();
        assert_eq!(sessions[0]["DeviceId"], "audio-remote-device");
        assert_eq!(sessions[0]["NowPlayingItem"]["Id"], item_id);
        assert_eq!(sessions[0]["PlayState"]["PositionTicks"], 123);
        assert_eq!(
            sessions[0]["PlayState"]["MediaSourceId"]
                .as_str()
                .unwrap()
                .replace('-', ""),
            item_id
        );
        assert_eq!(sessions[0]["PlayState"]["AudioStreamIndex"], 1);
        assert_eq!(sessions[0]["PlayState"]["SubtitleStreamIndex"], -1);

        let response = app
            .clone()
            .oneshot(
                Request::builder()
                    .method(Method::POST)
                    .uri(format!(
                        "/sessions/{playback_token}/playing?playCommand=PlayNow&itemIds={item_id}&startPositionTicks=456&audioStreamIndex=1&subtitleStreamIndex=-1"
                    ))
                    .header("X-Emby-Token", playback_token)
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::NO_CONTENT);
        let play_now_event =
            next_playback_event_type(&mut playback_events, playback_token, "Play").await;
        assert_eq!(play_now_event["MessageType"], "Play");
        assert_eq!(play_now_event["Data"]["PlayCommand"], "PlayNow");
        assert_eq!(play_now_event["Data"]["ItemIds"][0], item_id);
        assert_eq!(play_now_event["Data"]["StartPositionTicks"], 456);
        assert_eq!(play_now_event["Data"]["AudioStreamIndex"], 1);
        assert_eq!(play_now_event["Data"]["SubtitleStreamIndex"], -1);

        let response = app
            .clone()
            .oneshot(
                Request::builder()
                    .uri("/Sessions")
                    .header("X-Emby-Token", playback_token)
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::OK);
        let body = response.into_body().collect().await.unwrap().to_bytes();
        let play_now_sessions: Value = serde_json::from_slice(&body).unwrap();
        assert_eq!(play_now_sessions[0]["NowPlayingItem"]["Id"], item_id);
        assert_eq!(play_now_sessions[0]["PlayState"]["PositionTicks"], 456);
        assert_eq!(play_now_sessions[0]["PlayState"]["AudioStreamIndex"], 1);
        assert_eq!(play_now_sessions[0]["PlayState"]["SubtitleStreamIndex"], -1);

        let response = app
            .clone()
            .oneshot(
                Request::builder()
                    .method(Method::POST)
                    .uri(format!("/Sessions/{playback_token}/Playing/Pause"))
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::UNAUTHORIZED);

        let response = app
            .clone()
            .oneshot(
                Request::builder()
                    .method(Method::POST)
                    .uri(format!("/Sessions/{playback_token}/Playing/Pause"))
                    .header("X-Emby-Token", &api_key)
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::NO_CONTENT);
        let pause_event =
            next_playback_event_type(&mut playback_events, playback_token, "Playstate").await;
        assert_eq!(pause_event["MessageType"], "Playstate");
        assert_eq!(pause_event["Data"]["Command"], "Pause");

        let response = app
            .clone()
            .oneshot(
                Request::builder()
                    .method(Method::POST)
                    .uri(format!(
                        "/sessions/{playback_token}/playing/seek?seekpositionticks=999"
                    ))
                    .header("X-Emby-Token", playback_token)
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::NO_CONTENT);
        let seek_event =
            next_playback_event_type(&mut playback_events, playback_token, "Playstate").await;
        assert_eq!(seek_event["MessageType"], "Playstate");
        assert_eq!(seek_event["Data"]["Command"], "Seek");
        assert_eq!(seek_event["Data"]["SeekPositionTicks"], 999);

        let response = app
            .clone()
            .oneshot(
                Request::builder()
                    .uri("/Sessions")
                    .header("X-Emby-Token", playback_token)
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::OK);
        let body = response.into_body().collect().await.unwrap().to_bytes();
        let paused_sessions: Value = serde_json::from_slice(&body).unwrap();
        assert_eq!(paused_sessions[0]["NowPlayingItem"]["Id"], item_id);
        assert_eq!(paused_sessions[0]["PlayState"]["PositionTicks"], 999);
        assert_eq!(paused_sessions[0]["PlayState"]["IsPaused"], true);
        assert_eq!(paused_sessions[0]["PlayState"]["AudioStreamIndex"], 1);
        assert_eq!(paused_sessions[0]["PlayState"]["SubtitleStreamIndex"], -1);

        let response = app
            .clone()
            .oneshot(
                Request::builder()
                    .method(Method::POST)
                    .uri(format!("/Sessions/{playback_token}/Playing/PlayPause"))
                    .header("X-Emby-Token", playback_token)
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::NO_CONTENT);

        let response = app
            .clone()
            .oneshot(
                Request::builder()
                    .uri("/Sessions")
                    .header("X-Emby-Token", playback_token)
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::OK);
        let body = response.into_body().collect().await.unwrap().to_bytes();
        let toggled_sessions: Value = serde_json::from_slice(&body).unwrap();
        assert_eq!(toggled_sessions[0]["PlayState"]["IsPaused"], false);

        for command in ["NextTrack", "PreviousTrack", "Rewind", "FastForward"] {
            let response = app
                .clone()
                .oneshot(
                    Request::builder()
                        .method(Method::POST)
                        .uri(format!("/Sessions/{playback_token}/Playing/{command}"))
                        .header("X-Emby-Token", playback_token)
                        .body(Body::empty())
                        .unwrap(),
                )
                .await
                .unwrap();
            assert_eq!(response.status(), StatusCode::NO_CONTENT, "{command}");
        }

        let response = app
            .clone()
            .oneshot(
                Request::builder()
                    .method(Method::POST)
                    .uri("/Sessions/missing-session/Playing/Stop")
                    .header("X-Emby-Token", &api_key)
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::NOT_FOUND);

        let response = app
            .clone()
            .oneshot(
                Request::builder()
                    .method(Method::POST)
                    .uri(format!("/Sessions/{playback_token}/Playing/Unsupported"))
                    .header("X-Emby-Token", playback_token)
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::BAD_REQUEST);

        let response = app
            .clone()
            .oneshot(
                Request::builder()
                    .method(Method::POST)
                    .uri(format!("/Sessions/{playback_token}/Playing/Stop"))
                    .header("X-Emby-Token", playback_token)
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::NO_CONTENT);

        let response = app
            .clone()
            .oneshot(
                Request::builder()
                    .uri("/Sessions")
                    .header("X-Emby-Token", playback_token)
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::OK);
        let body = response.into_body().collect().await.unwrap().to_bytes();
        let stopped_remote_sessions: Value = serde_json::from_slice(&body).unwrap();
        assert_eq!(stopped_remote_sessions[0]["NowPlayingItem"], Value::Null);

        let response = app
            .clone()
            .oneshot(
                Request::builder()
                    .method(Method::POST)
                    .uri(format!(
                        "/Sessions/{playback_token}/Playing?PlayCommand=PlayInstantMix&ItemIds=not-a-uuid"
                    ))
                    .header("X-Emby-Token", &api_key)
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::BAD_REQUEST);

        let response = app
            .clone()
            .oneshot(
                Request::builder()
                    .method(Method::POST)
                    .uri(format!(
                        "/Sessions/missing-session/Playing?PlayCommand=PlayInstantMix&ItemIds={item_id}"
                    ))
                    .header("X-Emby-Token", &api_key)
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::NOT_FOUND);

        let response = app
            .clone()
            .oneshot(
                Request::builder()
                    .uri(format!("/Audio/{item_id}/stream"))
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::UNAUTHORIZED);

        let response = app
            .clone()
            .oneshot(
                Request::builder()
                    .uri(format!("/Audio/{item_id}/stream"))
                    .header("X-Emby-Token", &api_key)
                    .header(header::RANGE, "bytes=0-3")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::PARTIAL_CONTENT);
        assert_eq!(
            response.headers().get(header::CONTENT_RANGE).unwrap(),
            "bytes 0-3/10"
        );
        assert_eq!(
            response.headers().get(header::CONTENT_TYPE).unwrap(),
            "audio/mpeg"
        );
        let body = response.into_body().collect().await.unwrap().to_bytes();
        assert_eq!(&body[..], b"fake");

        let response = app
            .clone()
            .oneshot(
                Request::builder()
                    .uri(format!(
                        "/Audio/{item_id}/universal?UserId={user_id}&DeviceId=test-device&MaxStreamingBitrate=128000"
                    ))
                    .header("X-Emby-Token", &api_key)
                    .header(header::RANGE, "bytes=5-9")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::PARTIAL_CONTENT);
        assert_eq!(
            response.headers().get(header::CONTENT_RANGE).unwrap(),
            "bytes 5-9/10"
        );
        assert_eq!(
            response.headers().get(header::CONTENT_TYPE).unwrap(),
            "audio/mpeg"
        );
        let body = response.into_body().collect().await.unwrap().to_bytes();
        assert_eq!(&body[..], b"audio");

        let response = app
            .clone()
            .oneshot(
                Request::builder()
                    .method(Method::HEAD)
                    .uri(format!("/audio/{item_id}/universal?Static=true"))
                    .header("X-Emby-Token", &api_key)
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::OK);
        assert_eq!(
            response.headers().get(header::CONTENT_LENGTH).unwrap(),
            "10"
        );
        assert_eq!(
            response.headers().get(header::CONTENT_TYPE).unwrap(),
            "audio/mpeg"
        );
        let body = response.into_body().collect().await.unwrap().to_bytes();
        assert!(body.is_empty());

        let response = app
            .clone()
            .oneshot(
                Request::builder()
                    .method(Method::HEAD)
                    .uri(format!("/Audio/{item_id}/stream.mp3?Static=true"))
                    .header("X-Emby-Token", &api_key)
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::OK);
        assert_eq!(
            response.headers().get(header::CONTENT_LENGTH).unwrap(),
            "10"
        );
        assert_eq!(
            response.headers().get(header::CONTENT_TYPE).unwrap(),
            "audio/mpeg"
        );
        let body = response.into_body().collect().await.unwrap().to_bytes();
        assert!(body.is_empty());

        let response = app
            .oneshot(
                Request::builder()
                    .uri(format!("/Videos/{item_id}/stream"))
                    .header("X-Emby-Token", &api_key)
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::NOT_FOUND);
    }

    #[tokio::test]
    async fn active_encodings_returns_documented_contract() {
        let tmp = tempfile::tempdir().unwrap();
        let media = tmp.path().join("Transcode Me.mkv");
        tokio::fs::write(&media, b"fake video").await.unwrap();

        let db = Database::connect("sqlite::memory:").await.unwrap();
        let user = db
            .update_first_user("admin".to_string(), "secret")
            .await
            .unwrap();
        let api_key = db
            .issue_api_key_for_user(user.id, "active-encoding-test-key")
            .await
            .unwrap();
        let folder = db
            .upsert_virtual_folder(
                "Movies",
                Some("movies"),
                vec![tmp.path().to_string_lossy().to_string()],
            )
            .await
            .unwrap();
        db.scan_virtual_folder_items(folder.id).await.unwrap();
        let item = db.media_items().await.unwrap().remove(0);
        let app = router(AppState {
            db: db.clone(),
            web_dir: ".".into(),
            log_dir: ".".into(),
            local_address: "http://127.0.0.1:8097".to_string(),
        });

        let response = app
            .clone()
            .oneshot(
                Request::builder()
                    .uri("/Videos/ActiveEncodings")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::UNAUTHORIZED);

        let response = app
            .clone()
            .oneshot(
                Request::builder()
                    .uri("/videos/activeencodings")
                    .header("X-Emby-Token", &api_key)
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::OK);
        let body = response.into_body().collect().await.unwrap().to_bytes();
        let active_encodings: Value = serde_json::from_slice(&body).unwrap();
        assert_eq!(active_encodings, json!([]));

        db.upsert_transcode_session(UpsertTranscodeSession {
            play_session_id: "play-session-active".to_string(),
            dedupe_key: None,
            device_id: None,
            user_id: user.id,
            item_id: item.id,
            media_source_id: Some(item.id.simple().to_string()),
            audio_stream_index: Some(1),
            subtitle_stream_index: Some(-1),
            video_stream_index: Some(0),
            output_path: "/tmp/jellyrin-transcodes/play-session-active/main.m3u8".to_string(),
            process_id: Some(321),
            status: "starting".to_string(),
            progress_percent: Some(3.5),
            position_ticks: 99,
        })
        .await
        .unwrap();

        let response = app
            .clone()
            .oneshot(
                Request::builder()
                    .uri("/Videos/ActiveEncodings")
                    .header("X-Emby-Token", &api_key)
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::OK);
        let body = response.into_body().collect().await.unwrap().to_bytes();
        let active_encodings: Value = serde_json::from_slice(&body).unwrap();
        let active_encodings = active_encodings.as_array().unwrap();
        assert_eq!(active_encodings.len(), 1);
        let encoding = &active_encodings[0];
        let updated_at = encoding["UpdatedAt"].as_str().unwrap();
        assert!(!updated_at.is_empty());
        assert_eq!(
            encoding,
            &json!({
                "PlaySessionId": "play-session-active",
                "UserId": user.id.simple().to_string(),
                "ItemId": item.id.simple().to_string(),
                "MediaSourceId": item.id.simple().to_string(),
                "DeviceId": null,
                "Path": "/tmp/jellyrin-transcodes/play-session-active/main.m3u8",
                "OutputPath": "/tmp/jellyrin-transcodes/play-session-active/main.m3u8",
                "Status": "starting",
                "ProcessId": 321,
                "ProgressPercentage": 3.5,
                "CompletionPercentage": 3.5,
                "TranscodingPositionTicks": 99,
                "TranscodingStartPositionTicks": 0,
                "Container": "mkv",
                "VideoCodec": null,
                "AudioCodec": null,
                "Width": null,
                "Height": null,
                "Bitrate": null,
                "VideoStreamIndex": 0,
                "AudioStreamIndex": 1,
                "SubtitleStreamIndex": -1,
                "TranscodeReasons": [],
                "IsAudioDirect": false,
                "IsVideoDirect": false,
                "UpdatedAt": updated_at,
            })
        );
        let response = app
            .clone()
            .oneshot(
                Request::builder()
                    .uri("/HlsSegment/Videos/ActiveEncodings")
                    .header("X-Emby-Token", &api_key)
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::OK);
        let body = response.into_body().collect().await.unwrap().to_bytes();
        let active_encodings_alias: Value = serde_json::from_slice(&body).unwrap();
        assert_eq!(active_encodings_alias.as_array().unwrap().len(), 1);

        db.update_transcode_session_status("play-session-active", "Stopped")
            .await
            .unwrap();
        let response = app
            .oneshot(
                Request::builder()
                    .uri("/Videos/ActiveEncodings")
                    .header("X-Emby-Token", &api_key)
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::OK);
        let body = response.into_body().collect().await.unwrap().to_bytes();
        let active_encodings: Value = serde_json::from_slice(&body).unwrap();
        assert_eq!(active_encodings, json!([]));
    }

    #[tokio::test]
    async fn stop_active_encoding_marks_session_stopped_and_removes_hls_files() {
        let media_root = tempfile::tempdir().unwrap();
        let movie = media_root.path().join("Stop Me.mkv");
        tokio::fs::write(&movie, b"fake video").await.unwrap();

        let transcode_root = tempfile::tempdir().unwrap();
        let transcode_dir = transcode_root.path().join("play-session-stop");
        tokio::fs::create_dir_all(&transcode_dir).await.unwrap();
        let main_playlist = transcode_dir.join("main.m3u8");
        tokio::fs::write(&main_playlist, b"#EXTM3U\n")
            .await
            .unwrap();
        tokio::fs::write(transcode_dir.join("segment_00000.ts"), b"zero")
            .await
            .unwrap();

        let db = Database::connect("sqlite::memory:").await.unwrap();
        let user = db
            .update_first_user("admin".to_string(), "secret")
            .await
            .unwrap();
        let api_key = db
            .issue_api_key_for_user(user.id, "stop-encoding-test-key")
            .await
            .unwrap();
        let folder = db
            .upsert_virtual_folder(
                "Movies",
                Some("movies"),
                vec![media_root.path().to_string_lossy().to_string()],
            )
            .await
            .unwrap();
        db.scan_virtual_folder_items(folder.id).await.unwrap();
        let item = db.media_items().await.unwrap().remove(0);

        db.upsert_transcode_session(UpsertTranscodeSession {
            play_session_id: "play-session-stop".to_string(),
            dedupe_key: None,
            device_id: Some("device-1".to_string()),
            user_id: user.id,
            item_id: item.id,
            media_source_id: Some(item.id.simple().to_string()),
            audio_stream_index: Some(0),
            subtitle_stream_index: Some(-1),
            video_stream_index: Some(0),
            output_path: main_playlist.to_string_lossy().to_string(),
            process_id: Some(999),
            status: "running".to_string(),
            progress_percent: Some(12.0),
            position_ticks: 42,
        })
        .await
        .unwrap();

        let app = router(AppState {
            db: db.clone(),
            web_dir: ".".into(),
            log_dir: ".".into(),
            local_address: "http://127.0.0.1:8097".to_string(),
        });

        let response = app
            .clone()
            .oneshot(
                Request::builder()
                    .method(Method::DELETE)
                    .uri(
                        "/Videos/ActiveEncodings?PlaySessionId=play-session-stop&DeviceId=device-1",
                    )
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::UNAUTHORIZED);

        let response = app
            .clone()
            .oneshot(
                Request::builder()
                    .method(Method::DELETE)
                    .uri("/Videos/ActiveEncodings")
                    .header("X-Emby-Token", &api_key)
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::BAD_REQUEST);

        let response = app
            .clone()
            .oneshot(
                Request::builder()
                    .method(Method::DELETE)
                    .uri(
                        "/Videos/ActiveEncodings?PlaySessionId=play-session-stop&DeviceId=other-device",
                    )
                    .header("X-Emby-Token", &api_key)
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::FORBIDDEN);
        assert!(transcode_dir.exists());

        let response = app
            .clone()
            .oneshot(
                Request::builder()
                    .method(Method::DELETE)
                    .uri(
                        "/Videos/ActiveEncodings?playSessionId=play-session-stop&deviceId=device-1",
                    )
                    .header("X-Emby-Token", &api_key)
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::OK);
        assert!(!transcode_dir.exists());

        let session = db
            .transcode_session_by_play_session_id("play-session-stop")
            .await
            .unwrap()
            .unwrap();
        assert_eq!(session.status, "stopped");

        let response = app
            .clone()
            .oneshot(
                Request::builder()
                    .method(Method::DELETE)
                    .uri("/Videos/ActiveEncodings?PlaySessionId=play-session-stop")
                    .header("X-Emby-Token", &api_key)
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::OK);

        let response = app
            .clone()
            .oneshot(
                Request::builder()
                    .uri("/Videos/ActiveEncodings")
                    .header("X-Emby-Token", &api_key)
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::OK);
        let body = response.into_body().collect().await.unwrap().to_bytes();
        let active_encodings: Value = serde_json::from_slice(&body).unwrap();
        assert_eq!(active_encodings, json!([]));

        let response = app
            .oneshot(
                Request::builder()
                    .method(Method::DELETE)
                    .uri("/videos/activeencodings?PlaySessionId=missing")
                    .header("X-Emby-Token", &api_key)
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::NOT_FOUND);
    }

    #[tokio::test]
    async fn startup_reconciliation_stops_stale_transcodes_and_cleans_hls_files() {
        let media_root = tempfile::tempdir().unwrap();
        let movie = media_root.path().join("Stale.mkv");
        tokio::fs::write(&movie, b"fake video").await.unwrap();

        let transcode_root = tempfile::tempdir().unwrap();
        let stale_dir = transcode_root.path().join("play-session-stale");
        tokio::fs::create_dir_all(&stale_dir).await.unwrap();
        let stale_playlist = stale_dir.join("main.m3u8");
        tokio::fs::write(&stale_playlist, b"#EXTM3U\n")
            .await
            .unwrap();
        tokio::fs::write(stale_dir.join("segment_00000.ts"), b"zero")
            .await
            .unwrap();

        let completed_dir = transcode_root.path().join("play-session-completed");
        tokio::fs::create_dir_all(&completed_dir).await.unwrap();
        let completed_playlist = completed_dir.join("main.m3u8");
        tokio::fs::write(&completed_playlist, b"#EXTM3U\n")
            .await
            .unwrap();

        let db = Database::connect("sqlite::memory:").await.unwrap();
        let user = db
            .update_first_user("admin".to_string(), "secret")
            .await
            .unwrap();
        let folder = db
            .upsert_virtual_folder(
                "Movies",
                Some("movies"),
                vec![media_root.path().to_string_lossy().to_string()],
            )
            .await
            .unwrap();
        db.scan_virtual_folder_items(folder.id).await.unwrap();
        let item = db.media_items().await.unwrap().remove(0);

        for (play_session_id, output_path, status) in [
            (
                "play-session-stale",
                stale_playlist.to_string_lossy().to_string(),
                "running",
            ),
            (
                "play-session-completed",
                completed_playlist.to_string_lossy().to_string(),
                "completed",
            ),
        ] {
            db.upsert_transcode_session(UpsertTranscodeSession {
                play_session_id: play_session_id.to_string(),
                dedupe_key: None,
                device_id: None,
                user_id: user.id,
                item_id: item.id,
                media_source_id: Some(item.id.simple().to_string()),
                audio_stream_index: Some(0),
                subtitle_stream_index: Some(-1),
                video_stream_index: Some(0),
                output_path,
                process_id: Some(222),
                status: status.to_string(),
                progress_percent: Some(10.0),
                position_ticks: 0,
            })
            .await
            .unwrap();
        }

        let stopped = reconcile_transcode_sessions_on_startup(&db).await.unwrap();
        assert_eq!(stopped, 1);
        assert!(!stale_dir.exists());
        assert!(completed_dir.exists());

        let stale = db
            .transcode_session_by_play_session_id("play-session-stale")
            .await
            .unwrap()
            .unwrap();
        assert_eq!(stale.status, "stopped");
        let completed = db
            .transcode_session_by_play_session_id("play-session-completed")
            .await
            .unwrap()
            .unwrap();
        assert_eq!(completed.status, "completed");
        assert!(db.active_transcode_sessions().await.unwrap().is_empty());
    }

    #[tokio::test]
    async fn hls_transcode_task_persists_incremental_progress() {
        let media_root = tempfile::tempdir().unwrap();
        let movie = media_root.path().join("Progress.mkv");
        tokio::fs::write(&movie, b"fake video").await.unwrap();

        let db = Database::connect("sqlite::memory:").await.unwrap();
        let user = db
            .update_first_user("admin".to_string(), "secret")
            .await
            .unwrap();
        let folder = db
            .upsert_virtual_folder(
                "Movies",
                Some("movies"),
                vec![media_root.path().to_string_lossy().to_string()],
            )
            .await
            .unwrap();
        db.scan_virtual_folder_items(folder.id).await.unwrap();
        let item = db.media_items().await.unwrap().remove(0);
        db.update_media_item_media_info(item.id, Some(20_000), None, None, None, vec![])
            .await
            .unwrap();

        let transcode_root = tempfile::tempdir().unwrap();
        let playlist = transcode_root
            .path()
            .join("play-session-progress/main.m3u8");
        tokio::fs::create_dir_all(playlist.parent().unwrap())
            .await
            .unwrap();
        db.upsert_transcode_session(UpsertTranscodeSession {
            play_session_id: "play-session-progress".to_string(),
            dedupe_key: None,
            device_id: None,
            user_id: user.id,
            item_id: item.id,
            media_source_id: Some(item.id.simple().to_string()),
            audio_stream_index: Some(0),
            subtitle_stream_index: Some(-1),
            video_stream_index: Some(0),
            output_path: playlist.to_string_lossy().to_string(),
            process_id: None,
            status: "starting".to_string(),
            progress_percent: None,
            position_ticks: 0,
        })
        .await
        .unwrap();

        let command = FfmpegCommandSpec::new(
            "sh",
            vec![
                "-c".to_string(),
                "printf 'out_time_us=1000\\nprogress=continue\\nout_time_us=2000\\nprogress=end\\n' >&2"
                    .to_string(),
            ],
        );
        spawn_hls_transcode_task(db.clone(), "play-session-progress".to_string(), command).await;

        let mut session = None;
        for _ in 0..100 {
            let current = db
                .transcode_session_by_play_session_id("play-session-progress")
                .await
                .unwrap()
                .unwrap();
            if current.status == "completed" {
                session = Some(current);
                break;
            }
            tokio::time::sleep(std::time::Duration::from_millis(25)).await;
        }
        let session = session.expect("transcode process did not complete");
        assert_eq!(session.position_ticks, 20_000);
        assert_eq!(session.progress_percent, Some(100.0));
    }

    #[tokio::test]
    async fn terminal_transcode_cleanup_removes_old_hls_outputs_only() {
        let media_root = tempfile::tempdir().unwrap();
        let movie = media_root.path().join("Cleanup.mkv");
        tokio::fs::write(&movie, b"fake video").await.unwrap();

        let transcode_root = tempfile::tempdir().unwrap();
        let completed_dir = transcode_root.path().join("play-session-completed");
        tokio::fs::create_dir_all(&completed_dir).await.unwrap();
        let completed_playlist = completed_dir.join("main.m3u8");
        tokio::fs::write(&completed_playlist, b"#EXTM3U\n")
            .await
            .unwrap();
        tokio::fs::write(completed_dir.join("segment_00000.ts"), b"zero")
            .await
            .unwrap();

        let running_dir = transcode_root.path().join("play-session-running");
        tokio::fs::create_dir_all(&running_dir).await.unwrap();
        let running_playlist = running_dir.join("main.m3u8");
        tokio::fs::write(&running_playlist, b"#EXTM3U\n")
            .await
            .unwrap();

        let db = Database::connect("sqlite::memory:").await.unwrap();
        let user = db
            .update_first_user("admin".to_string(), "secret")
            .await
            .unwrap();
        let folder = db
            .upsert_virtual_folder(
                "Movies",
                Some("movies"),
                vec![media_root.path().to_string_lossy().to_string()],
            )
            .await
            .unwrap();
        db.scan_virtual_folder_items(folder.id).await.unwrap();
        let item = db.media_items().await.unwrap().remove(0);

        for (play_session_id, output_path, status) in [
            (
                "play-session-completed",
                completed_playlist.to_string_lossy().to_string(),
                "completed",
            ),
            (
                "play-session-running",
                running_playlist.to_string_lossy().to_string(),
                "running",
            ),
        ] {
            db.upsert_transcode_session(UpsertTranscodeSession {
                play_session_id: play_session_id.to_string(),
                dedupe_key: None,
                device_id: None,
                user_id: user.id,
                item_id: item.id,
                media_source_id: Some(item.id.simple().to_string()),
                audio_stream_index: Some(0),
                subtitle_stream_index: Some(-1),
                video_stream_index: Some(0),
                output_path,
                process_id: None,
                status: status.to_string(),
                progress_percent: None,
                position_ticks: 0,
            })
            .await
            .unwrap();
        }

        let cleaned = cleanup_terminal_hls_transcodes(&db, time::Duration::seconds(-1))
            .await
            .unwrap();
        assert_eq!(cleaned, 1);
        assert!(!completed_dir.exists());
        assert!(running_dir.exists());
    }

    #[tokio::test]
    async fn orphan_transcode_cleanup_preserves_db_backed_dirs() {
        let media_root = tempfile::tempdir().unwrap();
        let movie = media_root.path().join("Known.mkv");
        tokio::fs::write(&movie, b"fake video").await.unwrap();

        let transcode_root = tempfile::tempdir().unwrap();
        let known_dir = transcode_root.path().join("play-session-known");
        tokio::fs::create_dir_all(&known_dir).await.unwrap();
        let known_playlist = known_dir.join("main.m3u8");
        tokio::fs::write(&known_playlist, b"#EXTM3U\n")
            .await
            .unwrap();
        let orphan_dir = transcode_root.path().join("play-session-orphan");
        tokio::fs::create_dir_all(&orphan_dir).await.unwrap();
        tokio::fs::write(orphan_dir.join("main.m3u8"), b"#EXTM3U\n")
            .await
            .unwrap();
        let non_hls_dir = transcode_root.path().join("debug-not-hls");
        tokio::fs::create_dir_all(&non_hls_dir).await.unwrap();
        tokio::fs::write(non_hls_dir.join("notes.txt"), b"keep")
            .await
            .unwrap();
        let root_file = transcode_root.path().join("README.txt");
        tokio::fs::write(&root_file, b"keep").await.unwrap();

        let db = Database::connect("sqlite::memory:").await.unwrap();
        let user = db
            .update_first_user("admin".to_string(), "secret")
            .await
            .unwrap();
        let folder = db
            .upsert_virtual_folder(
                "Movies",
                Some("movies"),
                vec![media_root.path().to_string_lossy().to_string()],
            )
            .await
            .unwrap();
        db.scan_virtual_folder_items(folder.id).await.unwrap();
        let item = db.media_items().await.unwrap().remove(0);
        db.upsert_transcode_session(UpsertTranscodeSession {
            play_session_id: "play-session-known".to_string(),
            dedupe_key: None,
            device_id: None,
            user_id: user.id,
            item_id: item.id,
            media_source_id: Some(item.id.simple().to_string()),
            audio_stream_index: Some(0),
            subtitle_stream_index: Some(-1),
            video_stream_index: Some(0),
            output_path: known_playlist.to_string_lossy().to_string(),
            process_id: None,
            status: "completed".to_string(),
            progress_percent: None,
            position_ticks: 0,
        })
        .await
        .unwrap();

        let cleaned =
            cleanup_orphan_hls_transcode_dirs(&db, transcode_root.path(), time::Duration::ZERO)
                .await
                .unwrap();
        assert_eq!(cleaned, 1);
        assert!(known_dir.exists());
        assert!(!orphan_dir.exists());
        assert!(non_hls_dir.exists());
        assert!(root_file.exists());
    }

    #[tokio::test]
    async fn orphan_transcode_cleanup_ignores_missing_and_recent_roots() {
        let transcode_root = tempfile::tempdir().unwrap();
        let missing_root = transcode_root.path().join("missing");
        let recent_dir = transcode_root.path().join("recent-orphan");
        tokio::fs::create_dir_all(&recent_dir).await.unwrap();
        tokio::fs::write(recent_dir.join("main.m3u8"), b"#EXTM3U\n")
            .await
            .unwrap();

        let db = Database::connect("sqlite::memory:").await.unwrap();
        let missing_cleaned =
            cleanup_orphan_hls_transcode_dirs(&db, &missing_root, time::Duration::ZERO)
                .await
                .unwrap();
        assert_eq!(missing_cleaned, 0);

        let recent_cleaned = cleanup_orphan_hls_transcode_dirs(
            &db,
            transcode_root.path(),
            time::Duration::hours(24),
        )
        .await
        .unwrap();
        assert_eq!(recent_cleaned, 0);
        assert!(recent_dir.exists());
    }

    #[tokio::test]
    async fn playback_info_reuses_matching_active_hls_transcode_session() {
        let media_root = tempfile::tempdir().unwrap();
        let movie = media_root.path().join("Retry Me.mkv");
        tokio::fs::write(&movie, b"fake video").await.unwrap();

        let transcode_root = tempfile::tempdir().unwrap();
        let transcode_dir = transcode_root.path().join("play-session-reuse");
        tokio::fs::create_dir_all(&transcode_dir).await.unwrap();
        let main_playlist = transcode_dir.join("main.m3u8");
        tokio::fs::write(&main_playlist, b"#EXTM3U\n")
            .await
            .unwrap();

        let db = Database::connect("sqlite::memory:").await.unwrap();
        let user = db
            .update_first_user("admin".to_string(), "secret")
            .await
            .unwrap();
        let api_key = db
            .issue_api_key_for_user(user.id, "reuse-transcode-test-key")
            .await
            .unwrap();
        let folder = db
            .upsert_virtual_folder(
                "Movies",
                Some("movies"),
                vec![media_root.path().to_string_lossy().to_string()],
            )
            .await
            .unwrap();
        db.scan_virtual_folder_items(folder.id).await.unwrap();
        let item = db.media_items().await.unwrap().remove(0);
        let item_id = item.id.simple().to_string();
        let selection = TranscodeStreamSelection {
            video_stream_index: Some(0),
            audio_stream_index: None,
            subtitle_stream_index: None,
        };
        let dedupe_key = hls_transcode_dedupe_key(user.id, &item, &selection, 0);

        db.upsert_transcode_session(UpsertTranscodeSession {
            play_session_id: "play-session-reuse".to_string(),
            dedupe_key: Some(dedupe_key),
            device_id: None,
            user_id: user.id,
            item_id: item.id,
            media_source_id: Some(item_id.clone()),
            audio_stream_index: selection.audio_stream_index,
            subtitle_stream_index: selection.subtitle_stream_index,
            video_stream_index: selection.video_stream_index,
            output_path: main_playlist.to_string_lossy().to_string(),
            process_id: Some(111),
            status: "running".to_string(),
            progress_percent: Some(7.0),
            position_ticks: 0,
        })
        .await
        .unwrap();

        let app = router(AppState {
            db: db.clone(),
            web_dir: ".".into(),
            log_dir: ".".into(),
            local_address: "http://127.0.0.1:8097".to_string(),
        });

        let response = app
            .oneshot(
                Request::builder()
                    .uri(format!(
                        "/Items/{item_id}/PlaybackInfo?EnableDirectPlay=false&EnableDirectStream=false&EnableTranscoding=true"
                    ))
                    .header("X-Emby-Token", &api_key)
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::OK);
        let body = response.into_body().collect().await.unwrap().to_bytes();
        let playback_info: Value = serde_json::from_slice(&body).unwrap();
        assert_eq!(playback_info["PlaySessionId"], "play-session-reuse");
        assert_eq!(
            playback_info["MediaSources"][0]["TranscodingUrl"],
            format!(
                "/Videos/{item_id}/master.m3u8?PlaySessionId=play-session-reuse&api_key={api_key}"
            )
        );

        let sessions = db.transcode_sessions().await.unwrap();
        assert_eq!(sessions.len(), 1);
        assert_eq!(sessions[0].play_session_id, "play-session-reuse");
        assert_eq!(sessions[0].process_id, Some(111));
    }

    #[tokio::test]
    async fn hls_routes_serve_active_transcode_files() {
        let media_root = tempfile::tempdir().unwrap();
        let movie = media_root.path().join("Transcode Me.mkv");
        tokio::fs::write(&movie, b"fake video").await.unwrap();

        let transcode_root = tempfile::tempdir().unwrap();
        let transcode_dir = transcode_root.path().join("play-session-hls");
        tokio::fs::create_dir_all(&transcode_dir).await.unwrap();
        let main_playlist = transcode_dir.join("main.m3u8");
        let first_segment = transcode_dir.join("segment_00000.ts");
        let second_segment = transcode_dir.join("segment_00001.ts");
        tokio::fs::write(
            &main_playlist,
            format!(
                "#EXTM3U\n#EXT-X-TARGETDURATION:3\n#EXTINF:3.000,\n{}\n#EXTINF:2.000,\nsegment_00001.ts\n",
                first_segment.to_string_lossy()
            ),
        )
        .await
        .unwrap();
        tokio::fs::write(&first_segment, b"zero").await.unwrap();
        tokio::fs::write(&second_segment, b"one").await.unwrap();

        let db = Database::connect("sqlite::memory:").await.unwrap();
        let user = db
            .update_first_user("admin".to_string(), "secret")
            .await
            .unwrap();
        let api_key = db
            .issue_api_key_for_user(user.id, "hls-route-test-key")
            .await
            .unwrap();
        let folder = db
            .upsert_virtual_folder(
                "Movies",
                Some("movies"),
                vec![media_root.path().to_string_lossy().to_string()],
            )
            .await
            .unwrap();
        db.scan_virtual_folder_items(folder.id).await.unwrap();
        let item = db.media_items().await.unwrap().remove(0);
        let item_id = item.id.simple().to_string();

        db.upsert_transcode_session(UpsertTranscodeSession {
            play_session_id: "play-session-hls".to_string(),
            dedupe_key: None,
            device_id: None,
            user_id: user.id,
            item_id: item.id,
            media_source_id: Some(item_id.clone()),
            audio_stream_index: Some(0),
            subtitle_stream_index: Some(-1),
            video_stream_index: Some(0),
            output_path: main_playlist.to_string_lossy().to_string(),
            process_id: Some(456),
            status: "running".to_string(),
            progress_percent: Some(50.0),
            position_ticks: 123,
        })
        .await
        .unwrap();

        let app = router(AppState {
            db: db.clone(),
            web_dir: ".".into(),
            log_dir: ".".into(),
            local_address: "http://127.0.0.1:8097".to_string(),
        });

        let response = app
            .clone()
            .oneshot(
                Request::builder()
                    .uri(format!(
                        "/Videos/{item_id}/master.m3u8?PlaySessionId=play-session-hls"
                    ))
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::UNAUTHORIZED);

        let response = app
            .clone()
            .oneshot(
                Request::builder()
                    .uri(format!(
                        "/Videos/{item_id}/master.m3u8?PlaySessionId=play-session-hls"
                    ))
                    .header("X-Emby-Token", &api_key)
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::OK);
        assert_eq!(
            response.headers().get(header::CONTENT_TYPE).unwrap(),
            "application/vnd.apple.mpegurl"
        );
        let body = response.into_body().collect().await.unwrap().to_bytes();
        let master = String::from_utf8(body.to_vec()).unwrap();
        assert_eq!(
            master,
            "#EXTM3U\n#EXT-X-VERSION:3\n#EXT-X-STREAM-INF:BANDWIDTH=1000000\nmain.m3u8?PlaySessionId=play-session-hls\n"
        );
        let response = app
            .clone()
            .oneshot(
                Request::builder()
                    .method(Method::HEAD)
                    .uri(format!(
                        "/HlsSegment/Videos/{item_id}/master.m3u8?PlaySessionId=play-session-hls"
                    ))
                    .header("X-Emby-Token", &api_key)
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::OK);
        assert_eq!(
            response.headers().get(header::CONTENT_TYPE).unwrap(),
            "application/vnd.apple.mpegurl"
        );
        let body = response.into_body().collect().await.unwrap().to_bytes();
        assert!(body.is_empty());

        let response = app
            .clone()
            .oneshot(
                Request::builder()
                    .uri(format!(
                        "/videos/{item_id}/main.m3u8?PlaySessionId=play-session-hls"
                    ))
                    .header("X-Emby-Token", &api_key)
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::OK);
        let body = response.into_body().collect().await.unwrap().to_bytes();
        let media_playlist = String::from_utf8(body.to_vec()).unwrap();
        assert!(!media_playlist.contains(transcode_root.path().to_str().unwrap()));
        assert!(media_playlist.contains(&format!(
            "/Videos/{item_id}/hls1/main/0.ts?PlaySessionId=play-session-hls"
        )));
        assert!(media_playlist.contains(&format!(
            "/Videos/{item_id}/hls1/main/1.ts?PlaySessionId=play-session-hls"
        )));

        let response = app
            .clone()
            .oneshot(
                Request::builder()
                    .uri(format!(
                        "/DynamicHls/Videos/{item_id}/master.m3u8?PlaySessionId=play-session-hls"
                    ))
                    .header("X-Emby-Token", &api_key)
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::OK);
        let body = response.into_body().collect().await.unwrap().to_bytes();
        let dynamic_master = String::from_utf8(body.to_vec()).unwrap();
        assert_eq!(
            dynamic_master,
            "#EXTM3U\n#EXT-X-VERSION:3\n#EXT-X-STREAM-INF:BANDWIDTH=1000000\nmain.m3u8?PlaySessionId=play-session-hls\n"
        );

        let response = app
            .clone()
            .oneshot(
                Request::builder()
                    .method(Method::HEAD)
                    .uri(format!(
                        "/DynamicHls/Videos/{item_id}/master.m3u8?PlaySessionId=play-session-hls"
                    ))
                    .header("X-Emby-Token", &api_key)
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::OK);
        let body = response.into_body().collect().await.unwrap().to_bytes();
        assert!(body.is_empty());

        let response = app
            .clone()
            .oneshot(
                Request::builder()
                    .uri(format!(
                        "/DynamicHls/Videos/{item_id}/main.m3u8?PlaySessionId=play-session-hls"
                    ))
                    .header("X-Emby-Token", &api_key)
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::OK);

        let response = app
            .clone()
            .oneshot(
                Request::builder()
                    .uri(format!(
                        "/DynamicHls/Videos/{item_id}/live.m3u8?PlaySessionId=play-session-hls"
                    ))
                    .header("X-Emby-Token", &api_key)
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::OK);

        let response = app
            .clone()
            .oneshot(
                Request::builder()
                    .uri(format!(
                        "/hlssegment/videos/{item_id}/main.m3u8?PlaySessionId=play-session-hls"
                    ))
                    .header("X-Emby-Token", &api_key)
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::OK);

        let response = app
            .clone()
            .oneshot(
                Request::builder()
                    .uri(format!(
                        "/Videos/{item_id}/hls1/main/0.ts?PlaySessionId=play-session-hls"
                    ))
                    .header("X-Emby-Token", &api_key)
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::OK);
        assert_eq!(
            response.headers().get(header::CONTENT_TYPE).unwrap(),
            "video/mp2t"
        );
        let body = response.into_body().collect().await.unwrap().to_bytes();
        assert_eq!(&body[..], b"zero");
        let response = app
            .clone()
            .oneshot(
                Request::builder()
                    .uri(format!(
                        "/HlsSegment/Videos/{item_id}/hls1/main/0.ts?PlaySessionId=play-session-hls"
                    ))
                    .header("X-Emby-Token", &api_key)
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::OK);
        let body = response.into_body().collect().await.unwrap().to_bytes();
        assert_eq!(&body[..], b"zero");

        let response = app
            .clone()
            .oneshot(
                Request::builder()
                    .uri(format!(
                        "/Videos/{item_id}/hls/main/stream.m3u8?PlaySessionId=play-session-hls"
                    ))
                    .header("X-Emby-Token", &api_key)
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::OK);

        let response = app
            .clone()
            .oneshot(
                Request::builder()
                    .uri(format!(
                        "/videos/{item_id}/hls/main/0.ts?PlaySessionId=play-session-hls"
                    ))
                    .header("X-Emby-Token", &api_key)
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::OK);
        let body = response.into_body().collect().await.unwrap().to_bytes();
        assert_eq!(&body[..], b"zero");

        let response = app
            .clone()
            .oneshot(
                Request::builder()
                    .uri(format!(
                        "/DynamicHls/Videos/{item_id}/hls1/main/0.ts?PlaySessionId=play-session-hls"
                    ))
                    .header("X-Emby-Token", &api_key)
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::OK);
        let body = response.into_body().collect().await.unwrap().to_bytes();
        assert_eq!(&body[..], b"zero");

        let response = app
            .clone()
            .oneshot(
                Request::builder()
                    .uri(format!(
                        "/HlsSegment/Videos/{item_id}/hls/main/stream.m3u8?PlaySessionId=play-session-hls"
                    ))
                    .header("X-Emby-Token", &api_key)
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::OK);

        let response = app
            .clone()
            .oneshot(
                Request::builder()
                    .uri(format!(
                        "/HlsSegment/Videos/{item_id}/hls/main/0.ts?PlaySessionId=play-session-hls"
                    ))
                    .header("X-Emby-Token", &api_key)
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::OK);
        let body = response.into_body().collect().await.unwrap().to_bytes();
        assert_eq!(&body[..], b"zero");

        let response = app
            .clone()
            .oneshot(
                Request::builder()
                    .method(Method::HEAD)
                    .uri(format!(
                        "/videos/{item_id}/hls1/main/1.ts?PlaySessionId=play-session-hls"
                    ))
                    .header("X-Emby-Token", &api_key)
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::OK);
        assert_eq!(response.headers().get(header::CONTENT_LENGTH).unwrap(), "3");
        let body = response.into_body().collect().await.unwrap().to_bytes();
        assert!(body.is_empty());

        let response = app
            .clone()
            .oneshot(
                Request::builder()
                    .uri(format!(
                        "/Videos/{item_id}/hls1/main/0.mp4?PlaySessionId=play-session-hls"
                    ))
                    .header("X-Emby-Token", &api_key)
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::NOT_FOUND);

        db.update_transcode_session_status("play-session-hls", "stopped")
            .await
            .unwrap();
        let response = app
            .oneshot(
                Request::builder()
                    .uri(format!(
                        "/Videos/{item_id}/master.m3u8?PlaySessionId=play-session-hls"
                    ))
                    .header("X-Emby-Token", &api_key)
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::NOT_FOUND);
    }

    #[tokio::test]
    async fn public_users_lists_all_configured_users() {
        let db = Database::connect("sqlite::memory:").await.unwrap();
        let admin = db
            .update_first_user("admin".to_string(), "admin-secret")
            .await
            .unwrap();
        let api_key = db
            .issue_api_key_for_user(admin.id, "test-key")
            .await
            .unwrap();
        db.upsert_admin_user("jellyrin-e2e-admin", "e2e-secret")
            .await
            .unwrap();
        let app = router(AppState {
            db,
            web_dir: ".".into(),
            log_dir: ".".into(),
            local_address: "http://127.0.0.1:8097".to_string(),
        });

        let response = app
            .clone()
            .oneshot(
                Request::builder()
                    .uri("/Users/Public")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::OK);
        let body = response.into_body().collect().await.unwrap().to_bytes();
        let users: Value = serde_json::from_slice(&body).unwrap();
        assert_eq!(users.as_array().unwrap().len(), 2);
        assert!(
            users
                .as_array()
                .unwrap()
                .iter()
                .any(|user| user["Name"] == "jellyrin-e2e-admin")
        );

        let response = app
            .clone()
            .oneshot(
                Request::builder()
                    .uri("/Users")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::UNAUTHORIZED);

        for endpoint in ["/Users", "/users"] {
            let response = app
                .clone()
                .oneshot(
                    Request::builder()
                        .uri(endpoint)
                        .header("X-Emby-Token", &api_key)
                        .body(Body::empty())
                        .unwrap(),
                )
                .await
                .unwrap();
            assert_eq!(response.status(), StatusCode::OK, "{endpoint}");
            let body = response.into_body().collect().await.unwrap().to_bytes();
            let users: Value = serde_json::from_slice(&body).unwrap();
            assert_eq!(users.as_array().unwrap().len(), 2);
            assert!(
                users
                    .as_array()
                    .unwrap()
                    .iter()
                    .all(|user| user["Policy"]["IsAdministrator"] == true)
            );
            assert_eq!(
                users[0]["Policy"]["AuthenticationProviderId"],
                DEFAULT_AUTHENTICATION_PROVIDER_ID
            );
            assert_eq!(
                users[0]["Policy"]["PasswordResetProviderId"],
                DEFAULT_PASSWORD_RESET_PROVIDER_ID
            );
        }
    }

    #[tokio::test]
    async fn user_admin_provider_and_policy_routes_support_dashboard_editing() {
        let db = Database::connect("sqlite::memory:").await.unwrap();
        let admin = db
            .update_first_user("admin".to_string(), "secret")
            .await
            .unwrap();
        let api_key = db
            .issue_api_key_for_user(admin.id, "test-key")
            .await
            .unwrap();
        let managed = db
            .upsert_admin_user("managed", "managed-secret")
            .await
            .unwrap();
        let app = router(AppState {
            db,
            web_dir: ".".into(),
            log_dir: ".".into(),
            local_address: "http://127.0.0.1:8097".to_string(),
        });

        for endpoint in ["/Auth/Providers", "/Auth/PasswordResetProviders"] {
            let response = app
                .clone()
                .oneshot(
                    Request::builder()
                        .uri(endpoint)
                        .body(Body::empty())
                        .unwrap(),
                )
                .await
                .unwrap();
            assert_eq!(response.status(), StatusCode::UNAUTHORIZED, "{endpoint}");
        }

        let response = app
            .clone()
            .oneshot(
                Request::builder()
                    .uri("/Auth/Providers")
                    .header("X-Emby-Token", &api_key)
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::OK);
        let body = response.into_body().collect().await.unwrap().to_bytes();
        let providers: Value = serde_json::from_slice(&body).unwrap();
        assert_eq!(providers[0]["Name"], "Default");
        assert_eq!(providers[0]["Id"], DEFAULT_AUTHENTICATION_PROVIDER_ID);

        let response = app
            .clone()
            .oneshot(
                Request::builder()
                    .uri("/Auth/PasswordResetProviders")
                    .header("X-Emby-Token", &api_key)
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::OK);
        let body = response.into_body().collect().await.unwrap().to_bytes();
        let providers: Value = serde_json::from_slice(&body).unwrap();
        assert_eq!(providers[0]["Name"], "Default");
        assert_eq!(providers[0]["Id"], DEFAULT_PASSWORD_RESET_PROVIDER_ID);

        let response = app
            .clone()
            .oneshot(
                Request::builder()
                    .uri("/Channels?UserId=00000000-0000-0000-0000-000000000000&StartIndex=2&Limit=10&SupportsMediaDeletion=true&IsFavorite=false")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::UNAUTHORIZED);

        let response = app
            .clone()
            .oneshot(
                Request::builder()
                    .uri("/Channels?UserId=00000000-0000-0000-0000-000000000000&StartIndex=2&Limit=10&SupportsMediaDeletion=true&IsFavorite=false")
                    .header("X-Emby-Token", &api_key)
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::OK);
        let body = response.into_body().collect().await.unwrap().to_bytes();
        let channels: Value = serde_json::from_slice(&body).unwrap();
        assert_eq!(channels["Items"], json!([]));
        assert_eq!(channels["TotalRecordCount"], 0);
        assert_eq!(channels["StartIndex"], 2);

        let response = app
            .clone()
            .oneshot(
                Request::builder()
                    .uri(format!(
                        "/Channels?startIndex=7&limit=1&supportsLatestItems=true&api_key={api_key}"
                    ))
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::OK);
        let body = response.into_body().collect().await.unwrap().to_bytes();
        let channels: Value = serde_json::from_slice(&body).unwrap();
        assert_eq!(channels["Items"], json!([]));
        assert_eq!(channels["TotalRecordCount"], 0);
        assert_eq!(channels["StartIndex"], 7);

        let response = app
            .clone()
            .oneshot(
                Request::builder()
                    .uri("/localization/parentalratings")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::OK);
        let body = response.into_body().collect().await.unwrap().to_bytes();
        let ratings: Value = serde_json::from_slice(&body).unwrap();
        assert_eq!(ratings[0]["Name"], "US-G");
        assert_eq!(ratings[0]["Value"], "US-G");
        assert_eq!(ratings[0]["RatingScore"]["score"], 1);
        assert_eq!(ratings[0]["RatingScore"]["subScore"], Value::Null);

        let response = app
            .clone()
            .oneshot(
                Request::builder()
                    .method(Method::POST)
                    .uri(format!("/Users/Password?userId={}", admin.id))
                    .header(header::CONTENT_TYPE, "application/json")
                    .body(Body::from(
                        json!({ "CurrentPw": "secret", "NewPw": "changed-secret" }).to_string(),
                    ))
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::UNAUTHORIZED);

        let response = app
            .clone()
            .oneshot(
                Request::builder()
                    .method(Method::POST)
                    .uri(format!("/Users/Password?userId={}", admin.id))
                    .header("X-Emby-Token", &api_key)
                    .header(header::CONTENT_TYPE, "application/json")
                    .body(Body::from(
                        json!({ "CurrentPw": "wrong", "NewPw": "changed-secret" }).to_string(),
                    ))
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::FORBIDDEN);

        let response = app
            .clone()
            .oneshot(
                Request::builder()
                    .method(Method::POST)
                    .uri(format!("/Users/Password?userId={}", admin.id))
                    .header("X-Emby-Token", &api_key)
                    .header(header::CONTENT_TYPE, "application/json")
                    .body(Body::from(
                        json!({
                            "CurrentPw": "secret",
                            "NewPw": "changed-secret",
                            "ResetPassword": false
                        })
                        .to_string(),
                    ))
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::NO_CONTENT);

        let response = app
            .clone()
            .oneshot(
                Request::builder()
                    .method(Method::POST)
                    .uri("/Users/AuthenticateByName")
                    .header(header::CONTENT_TYPE, "application/json")
                    .header(
                        header::AUTHORIZATION,
                        r#"MediaBrowser Client="Jellyfin Web", Device="Firefox", DeviceId="old-password-test", Version="dev""#,
                    )
                    .body(Body::from(
                        json!({ "Username": "admin", "Pw": "secret" }).to_string(),
                    ))
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::UNAUTHORIZED);

        let response = app
            .clone()
            .oneshot(
                Request::builder()
                    .method(Method::POST)
                    .uri("/Users/AuthenticateByName")
                    .header(header::CONTENT_TYPE, "application/json")
                    .header(
                        header::AUTHORIZATION,
                        r#"MediaBrowser Client="Jellyfin Web", Device="Firefox", DeviceId="new-password-test", Version="dev""#,
                    )
                    .body(Body::from(
                        json!({ "Username": "admin", "Pw": "changed-secret" }).to_string(),
                    ))
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::OK);

        let response = app
            .clone()
            .oneshot(
                Request::builder()
                    .method(Method::POST)
                    .uri(format!("/Users/{}/Password", managed.id))
                    .header("X-Emby-Token", &api_key)
                    .header(header::CONTENT_TYPE, "application/json")
                    .body(Body::from(
                        json!({ "NewPw": "managed-changed" }).to_string(),
                    ))
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::NO_CONTENT);

        let response = app
            .clone()
            .oneshot(
                Request::builder()
                    .method(Method::POST)
                    .uri("/Users/AuthenticateByName")
                    .header(header::CONTENT_TYPE, "application/json")
                    .header(
                        header::AUTHORIZATION,
                        r#"MediaBrowser Client="Jellyfin Web", Device="Firefox", DeviceId="managed-password-test", Version="dev""#,
                    )
                    .body(Body::from(
                        json!({ "Username": "managed", "Pw": "managed-changed" }).to_string(),
                    ))
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::OK);

        let response = app
            .clone()
            .oneshot(
                Request::builder()
                    .method(Method::POST)
                    .uri("/Users/00000000-0000-0000-0000-000000000000/Password")
                    .header("X-Emby-Token", &api_key)
                    .header(header::CONTENT_TYPE, "application/json")
                    .body(Body::from(json!({ "NewPw": "missing" }).to_string()))
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::NOT_FOUND);

        let response = app
            .clone()
            .oneshot(
                Request::builder()
                    .method(Method::POST)
                    .uri(format!("/Users/{}/Password", managed.id))
                    .header("X-Emby-Token", &api_key)
                    .header(header::CONTENT_TYPE, "application/json")
                    .body(Body::from(json!({ "ResetPassword": true }).to_string()))
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::NO_CONTENT);

        let response = app
            .clone()
            .oneshot(
                Request::builder()
                    .uri(format!("/Users/{}", managed.id))
                    .header("X-Emby-Token", &api_key)
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::OK);
        let body = response.into_body().collect().await.unwrap().to_bytes();
        let user: Value = serde_json::from_slice(&body).unwrap();
        assert_eq!(user["HasConfiguredPassword"], false);
        assert_eq!(user["HasPassword"], false);

        let response = app
            .clone()
            .oneshot(
                Request::builder()
                    .method(Method::POST)
                    .uri("/Users/AuthenticateByName")
                    .header(header::CONTENT_TYPE, "application/json")
                    .header(
                        header::AUTHORIZATION,
                        r#"MediaBrowser Client="Jellyfin Web", Device="Firefox", DeviceId="managed-reset-test", Version="dev""#,
                    )
                    .body(Body::from(
                        json!({ "Username": "managed", "Pw": "managed-changed" }).to_string(),
                    ))
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::UNAUTHORIZED);

        let response = app
            .clone()
            .oneshot(
                Request::builder()
                    .method(Method::POST)
                    .uri(format!("/Users?userId={}", managed.id))
                    .header("X-Emby-Token", &api_key)
                    .header(header::CONTENT_TYPE, "application/json")
                    .body(Body::from(
                        json!({
                            "Id": managed.id,
                            "Name": "managed-renamed",
                            "Policy": {
                                "IsAdministrator": false,
                                "IsDisabled": true
                            }
                        })
                        .to_string(),
                    ))
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::NO_CONTENT);

        let response = app
            .clone()
            .oneshot(
                Request::builder()
                    .uri(format!("/Users/{}", managed.id))
                    .header("X-Emby-Token", &api_key)
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::OK);
        let body = response.into_body().collect().await.unwrap().to_bytes();
        let user: Value = serde_json::from_slice(&body).unwrap();
        assert_eq!(user["Name"], "managed-renamed");
        assert_eq!(user["Policy"]["IsAdministrator"], false);
        assert_eq!(user["Policy"]["IsDisabled"], true);

        let response = app
            .clone()
            .oneshot(
                Request::builder()
                    .method(Method::POST)
                    .uri(format!("/Users/{}/Policy", managed.id))
                    .header("X-Emby-Token", &api_key)
                    .header(header::CONTENT_TYPE, "application/json")
                    .body(Body::from(
                        json!({
                            "IsAdministrator": true,
                            "IsDisabled": false,
                            "AuthenticationProviderId": DEFAULT_AUTHENTICATION_PROVIDER_ID,
                            "PasswordResetProviderId": DEFAULT_PASSWORD_RESET_PROVIDER_ID
                        })
                        .to_string(),
                    ))
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::NO_CONTENT);

        let response = app
            .oneshot(
                Request::builder()
                    .uri(format!("/Users/{}", managed.id))
                    .header("X-Emby-Token", &api_key)
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::OK);
        let body = response.into_body().collect().await.unwrap().to_bytes();
        let user: Value = serde_json::from_slice(&body).unwrap();
        assert_eq!(user["Name"], "managed-renamed");
        assert_eq!(user["Policy"]["IsAdministrator"], true);
        assert_eq!(user["Policy"]["IsDisabled"], false);
    }

    #[tokio::test]
    async fn parental_ratings_follow_first_time_setup_auth() {
        let db = Database::connect("sqlite::memory:").await.unwrap();
        let admin = db
            .update_first_user("admin".to_string(), "secret")
            .await
            .unwrap();
        let api_key = db
            .issue_api_key_for_user(admin.id, "test-key")
            .await
            .unwrap();
        let app = router(AppState {
            db: db.clone(),
            web_dir: ".".into(),
            log_dir: ".".into(),
            local_address: "http://127.0.0.1:8097".to_string(),
        });

        let response = app
            .clone()
            .oneshot(
                Request::builder()
                    .uri("/Localization/ParentalRatings")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::OK);

        db.complete_startup_wizard().await.unwrap();

        let response = app
            .clone()
            .oneshot(
                Request::builder()
                    .uri("/Localization/ParentalRatings")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::UNAUTHORIZED);

        let response = app
            .oneshot(
                Request::builder()
                    .uri("/Localization/ParentalRatings")
                    .header("X-Emby-Token", &api_key)
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::OK);
        let body = response.into_body().collect().await.unwrap().to_bytes();
        let ratings: Value = serde_json::from_slice(&body).unwrap();
        assert!(ratings.as_array().unwrap().len() >= 5);
    }

    #[tokio::test]
    async fn user_configuration_round_trips_partial_updates() {
        let db = Database::connect("sqlite::memory:").await.unwrap();
        let user = db
            .update_first_user("admin".to_string(), "secret")
            .await
            .unwrap();
        let api_key = db
            .issue_api_key_for_user(user.id, "test-key")
            .await
            .unwrap();
        let app = router(AppState {
            db,
            web_dir: ".".into(),
            log_dir: ".".into(),
            local_address: "http://127.0.0.1:8097".to_string(),
        });

        let response = app
            .clone()
            .oneshot(
                Request::builder()
                    .uri("/Users/Me")
                    .header("X-Emby-Token", &api_key)
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::OK);
        let body = response.into_body().collect().await.unwrap().to_bytes();
        let me: Value = serde_json::from_slice(&body).unwrap();
        assert_eq!(me["Configuration"]["SubtitleMode"], "Default");
        assert_eq!(me["Configuration"]["EnableNextEpisodeAutoPlay"], true);
        assert_eq!(me["Configuration"]["HidePlayedInLatest"], true);
        assert_eq!(me["Configuration"]["DisplayCollectionsView"], false);
        assert_eq!(me["Configuration"]["LatestItemsExcludes"], json!([]));

        let response = app
            .clone()
            .oneshot(
                Request::builder()
                    .method(Method::POST)
                    .uri(format!("/Users/Configuration?userId={}", user.id))
                    .header(header::CONTENT_TYPE, "application/json")
                    .body(Body::from("{}"))
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::UNAUTHORIZED);

        let response = app
            .clone()
            .oneshot(
                Request::builder()
                    .method(Method::POST)
                    .uri(format!("/Users/Configuration?userId={}", user.id))
                    .header("X-Emby-Token", &api_key)
                    .header(header::CONTENT_TYPE, "application/json")
                    .body(Body::from(
                        json!({
                            "DisplayMissingEpisodes": true,
                            "EnableNextEpisodeAutoPlay": false,
                            "LatestItemsExcludes": ["movies"],
                            "UnknownFutureSetting": "kept"
                        })
                        .to_string(),
                    ))
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::NO_CONTENT);

        let response = app
            .clone()
            .oneshot(
                Request::builder()
                    .method(Method::POST)
                    .uri(format!("/users/{}/configuration", user.id))
                    .header("X-Emby-Token", &api_key)
                    .header(header::CONTENT_TYPE, "application/json")
                    .body(Body::from(
                        json!({
                            "SubtitleMode": "OnlyForced",
                            "RememberAudioSelections": false
                        })
                        .to_string(),
                    ))
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::NO_CONTENT);

        let response = app
            .clone()
            .oneshot(
                Request::builder()
                    .uri("/Users/Me")
                    .header("X-Emby-Token", &api_key)
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        let body = response.into_body().collect().await.unwrap().to_bytes();
        let updated_me: Value = serde_json::from_slice(&body).unwrap();
        assert_eq!(updated_me["Configuration"]["DisplayMissingEpisodes"], true);
        assert_eq!(
            updated_me["Configuration"]["EnableNextEpisodeAutoPlay"],
            false
        );
        assert_eq!(
            updated_me["Configuration"]["LatestItemsExcludes"],
            json!(["movies"])
        );
        assert_eq!(updated_me["Configuration"]["SubtitleMode"], "OnlyForced");
        assert_eq!(
            updated_me["Configuration"]["RememberAudioSelections"],
            false
        );
        assert_eq!(updated_me["Configuration"]["PlayDefaultAudioTrack"], true);
        assert_eq!(updated_me["Configuration"]["UnknownFutureSetting"], "kept");

        let response = app
            .clone()
            .oneshot(
                Request::builder()
                    .method(Method::POST)
                    .uri("/Users/Configuration")
                    .header("X-Emby-Token", &api_key)
                    .header(header::CONTENT_TYPE, "application/json")
                    .body(Body::from("{}"))
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::NO_CONTENT);

        let response = app
            .oneshot(
                Request::builder()
                    .uri("/Users/Me")
                    .header("X-Emby-Token", &api_key)
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        let body = response.into_body().collect().await.unwrap().to_bytes();
        let after_no_user_id: Value = serde_json::from_slice(&body).unwrap();
        assert_eq!(
            after_no_user_id["Configuration"]["SubtitleMode"],
            "OnlyForced"
        );
    }
}
