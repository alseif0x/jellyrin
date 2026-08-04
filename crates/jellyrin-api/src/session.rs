use jellyrin_db::{ActivePlaybackSession, ActiveSessionUser, ActiveViewingSession, DeviceSession};

use crate::{format_time_for_json, media_item_to_json};

pub(crate) fn session_to_json(
    session: &DeviceSession,
    active_playback: Option<&ActivePlaybackSession>,
    active_viewing: Option<&ActiveViewingSession>,
    additional_users: &[ActiveSessionUser],
    server_id: &str,
) -> serde_json::Value {
    let capabilities = session.capabilities.as_ref();
    let additional_users = additional_users
        .iter()
        .map(|user| {
            serde_json::json!({
                "UserId": user.user_id,
                "UserName": user.user_name,
            })
        })
        .collect::<Vec<_>>();
    let last_activity_date = format_time_for_json(session.last_activity_at);
    let supports_media_control = capability_bool(capabilities, "SupportsMediaControl");
    serde_json::json!({
        "Id": session.access_token,
        "UserId": session.user_id,
        "UserName": session.user_name,
        "Client": session.client,
        "LastActivityDate": last_activity_date,
        "LastPlaybackCheckIn": last_activity_date,
        "LastPausedDate": null,
        "DeviceName": session.device_name,
        "DeviceType": null,
        "DeviceId": session.device_id,
        "ApplicationVersion": session.version,
        "IsActive": true,
        "SupportsMediaControl": supports_media_control,
        "SupportsRemoteControl": capability_bool(capabilities, "SupportsRemoteControl")
            || supports_media_control,
        "PlayableMediaTypes": capability_array(capabilities, "PlayableMediaTypes"),
        "SupportedCommands": capability_array(capabilities, "SupportedCommands"),
        "Capabilities": capabilities.cloned(),
        "RemoteEndPoint": null,
        "NowPlayingItem": active_playback.map(|playback| media_item_to_json(&playback.item, server_id)),
        "PlayState": active_playback.map(active_playback_state_json),
        "NowViewingItem": active_viewing.map(|viewing| media_item_to_json(&viewing.item, server_id)),
        "TranscodingInfo": null,
        "NowPlayingQueue": null,
        "HasCustomDeviceName": false,
        "PlaylistItemId": null,
        "ServerId": server_id,
        "UserPrimaryImageTag": null,
        "AdditionalUsers": additional_users,
    })
}

pub(crate) fn capability_bool(capabilities: Option<&serde_json::Value>, key: &str) -> bool {
    capabilities
        .and_then(|capabilities| capabilities.get(key))
        .and_then(serde_json::Value::as_bool)
        .unwrap_or(false)
}

pub(crate) fn capability_array(
    capabilities: Option<&serde_json::Value>,
    key: &str,
) -> serde_json::Value {
    capabilities
        .and_then(|capabilities| capabilities.get(key))
        .filter(|value| value.is_array())
        .cloned()
        .unwrap_or_else(|| serde_json::json!([]))
}

pub(crate) fn active_playback_state_json(playback: &ActivePlaybackSession) -> serde_json::Value {
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
        "PlaybackOrder": "Default",
    })
}
