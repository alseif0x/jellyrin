use std::collections::BTreeMap;

use time::OffsetDateTime;
use uuid::Uuid;

#[derive(Debug, Clone)]
pub(crate) struct PlaybackEvent {
    pub(crate) session_id: String,
    pub(crate) message: serde_json::Value,
}

#[derive(Debug, Clone)]
pub(crate) struct SyncPlayGroup {
    pub(crate) id: String,
    pub(crate) name: String,
    pub(crate) owner_user_id: Uuid,
    pub(crate) participants: BTreeMap<String, SyncPlayParticipant>,
    pub(crate) state: serde_json::Value,
    pub(crate) command_sequence: u64,
    pub(crate) updated_at: OffsetDateTime,
}

#[derive(Debug, Clone)]
pub(crate) struct SyncPlayParticipant {
    pub(crate) user_id: Uuid,
    pub(crate) user_name: String,
    pub(crate) session_id: String,
    pub(crate) device_id: String,
    pub(crate) is_ready: bool,
    pub(crate) is_buffering: bool,
    pub(crate) last_seen_at: OffsetDateTime,
}
