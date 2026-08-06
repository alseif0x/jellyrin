use jellyrin_core::live_tv_stable_id;
use serde_json::{Value, json};
use time::{OffsetDateTime, format_description::well_known::Rfc3339};
use url::Url;

use crate::{MagstvEpgData, MagstvEpgProgram, MagstvProviderError, parse_portal_response};

/// Plain-GET EPG representation. The `md5` query value is deliberately an
/// opaque input: this crate does not pretend to know its derivation yet.
pub const MAGSTV_EPG_MD5_QUERY: &str = "md5";

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct MagstvProgram {
    pub id: String,
    pub name: String,
    pub channel_id: String,
    pub start_date: String,
    pub end_date: String,
    pub overview: String,
    pub is_live: bool,
}

impl MagstvProgram {
    pub fn into_jellyrin_json(self) -> Value {
        json!({
            "Id": self.id,
            "Name": self.name,
            "ChannelId": self.channel_id,
            "StartDate": self.start_date,
            "EndDate": self.end_date,
            "Overview": self.overview,
            "IsLive": self.is_live,
        })
    }
}

/// Builds the captured plain EPG endpoint without deriving or mutating the
/// server-provided `md5` value. Network I/O is intentionally left to the
/// transport layer (F3), so this function is safe to test offline.
pub fn build_epg_url(
    base_url: &str,
    epg_path: &str,
    md5: &str,
) -> Result<Url, MagstvProviderError> {
    let mut url = Url::parse(base_url).map_err(|_| MagstvProviderError::InvalidEpgRequest)?;
    if url.scheme() != "https"
        || url.host_str().is_none()
        || !url.username().is_empty()
        || url.password().is_some()
        || epg_path.is_empty()
        || !epg_path.starts_with('/')
        || epg_path.starts_with("//")
        || epg_path.contains("://")
        || epg_path.split('/').any(|segment| segment == "..")
        || epg_path.chars().any(char::is_control)
        || md5.trim().is_empty()
        || md5.chars().any(char::is_control)
    {
        return Err(MagstvProviderError::InvalidEpgRequest);
    }
    url.set_path(epg_path);
    url.set_query(None);
    url.query_pairs_mut().append_pair(MAGSTV_EPG_MD5_QUERY, md5);
    Ok(url)
}

/// Parses a single channel's EPG using a best-effort, fixture-friendly
/// adapter. Until the native payload is decoded, only shape normalization is
/// done here; no authentication or signature is guessed.
pub fn parse_epg_programs(channel_id: &str, payload: &Value) -> Vec<Value> {
    let channel_id = channel_id.trim();
    if channel_id.is_empty() {
        return Vec::new();
    }
    listings(payload)
        .into_iter()
        .enumerate()
        .filter_map(|(index, listing)| parse_program(channel_id, index, listing))
        .map(MagstvProgram::into_jellyrin_json)
        .collect()
}

/// Drop-in payload helper for the future API wiring. A single-channel payload
/// must carry `ChannelId`/`channel_id`; callers with a separate channel id use
/// `parse_epg_programs` directly.
pub fn programs_from_payload(payload: &Value) -> Option<Vec<Value>> {
    let channel_id = string_field(payload, &["ChannelId", "channel_id", "channelId"])?;
    let programs = parse_epg_programs(&channel_id, payload);
    (!programs.is_empty()).then_some(programs)
}

/// Converts the typed `getProgram` response recovered from the MAGSTV portal
/// contract into Jellyrin programs. This is deliberately separate from the
/// public raw EPG GET parser: the portal response is still behind the app
/// codec, while the public endpoint's body is not known to be JSON yet.
pub fn programs_from_portal_epg(
    channel_id: &str,
    data: &MagstvEpgData,
    now: OffsetDateTime,
) -> Vec<MagstvProgram> {
    let channel_id = channel_id.trim();
    if channel_id.is_empty() {
        return Vec::new();
    }

    data.program_list
        .iter()
        .enumerate()
        .filter_map(|(index, program)| portal_program(channel_id, index, program, now))
        .collect()
}

/// Convenience adapter for the future verified portal connector. It parses a
/// `getProgram` wrapper and returns the typed Jellyrin-facing programs while
/// keeping the raw response bytes out of logs.
pub fn parse_portal_epg_programs(
    channel_id: &str,
    body: &[u8],
    now: OffsetDateTime,
) -> Result<Vec<MagstvProgram>, MagstvProviderError> {
    let response = parse_portal_response::<MagstvEpgData>(body)?;
    let data = response
        .data
        .ok_or(MagstvProviderError::InvalidPortalPayload)?;
    Ok(programs_from_portal_epg(channel_id, &data, now))
}

fn portal_program(
    channel_id: &str,
    index: usize,
    program: &MagstvEpgProgram,
    now: OffsetDateTime,
) -> Option<MagstvProgram> {
    let start_date = program.start_time.as_deref().and_then(parse_date_string)?;
    let end_date = program.end_time.as_deref().and_then(parse_date_string)?;
    let start = OffsetDateTime::parse(&start_date, &Rfc3339).ok()?;
    let end = OffsetDateTime::parse(&end_date, &Rfc3339).ok()?;
    let name = program
        .program_name
        .as_deref()
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .map(str::to_string)
        .unwrap_or_else(|| format!("Program {}", index + 1));
    let overview = program
        .desc
        .as_deref()
        .or(program.remark.as_deref())
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .unwrap_or_default()
        .to_string();
    let remote_id = program
        .content_id
        .as_deref()
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .unwrap_or_else(|| "unknown");

    Some(MagstvProgram {
        id: live_tv_stable_id(
            "magstv-program",
            &format!("{channel_id}-{remote_id}-{start_date}"),
        ),
        name,
        channel_id: channel_id.to_string(),
        start_date,
        end_date,
        overview,
        is_live: start <= now && now < end,
    })
}

fn parse_date_string(value: &str) -> Option<String> {
    let value = value.trim();
    if value.is_empty() {
        return None;
    }
    if let Ok(parsed) = OffsetDateTime::parse(value, &Rfc3339) {
        return parsed.format(&Rfc3339).ok();
    }
    value.parse::<i64>().ok().and_then(format_unix_time)
}

fn parse_program(channel_id: &str, index: usize, listing: &Value) -> Option<MagstvProgram> {
    let start_date = date_field(
        listing,
        &["StartDate", "start", "start_time", "startTime", "begin"],
    )?;
    let end_date = date_field(listing, &["EndDate", "end", "end_time", "endTime", "stop"])?;
    let name = string_field(listing, &["Name", "title", "name"])
        .filter(|value| !value.is_empty())
        .unwrap_or_else(|| format!("Program {}", index + 1));
    let overview =
        string_field(listing, &["Overview", "description", "desc", "summary"]).unwrap_or_default();
    let remote_id = string_field(listing, &["Id", "id", "program_id", "programId"])
        .unwrap_or_else(|| index.to_string());
    let is_live = listing
        .get("IsLive")
        .or_else(|| listing.get("is_live"))
        .and_then(Value::as_bool)
        .unwrap_or(true);

    Some(MagstvProgram {
        id: live_tv_stable_id(
            "magstv-program",
            &format!("{channel_id}-{remote_id}-{start_date}"),
        ),
        name,
        channel_id: channel_id.to_string(),
        start_date,
        end_date,
        overview,
        is_live,
    })
}

fn listings(payload: &Value) -> Vec<&Value> {
    if let Some(values) = payload.as_array() {
        return values.iter().collect();
    }
    for key in ["programs", "listings", "epg_listings", "data", "Programs"] {
        if let Some(values) = payload.get(key).and_then(Value::as_array) {
            return values.iter().collect();
        }
    }
    Vec::new()
}

fn string_field(value: &Value, keys: &[&str]) -> Option<String> {
    keys.iter().find_map(|key| {
        let value = value.get(*key)?;
        match value {
            Value::String(value) => {
                let value = value.trim();
                (!value.is_empty()).then_some(value.to_string())
            }
            Value::Number(value) => Some(value.to_string()),
            _ => None,
        }
    })
}

fn date_field(value: &Value, keys: &[&str]) -> Option<String> {
    keys.iter()
        .find_map(|key| value.get(*key).and_then(format_date))
}

fn format_date(value: &Value) -> Option<String> {
    match value {
        Value::String(value) => {
            let value = value.trim();
            if value.is_empty() {
                return None;
            }
            if let Ok(parsed) = OffsetDateTime::parse(value, &Rfc3339) {
                return parsed.format(&Rfc3339).ok();
            }
            value.parse::<i64>().ok().and_then(format_unix_time)
        }
        Value::Number(value) => value.as_i64().and_then(format_unix_time).or_else(|| {
            value
                .as_f64()
                .and_then(|value| format_unix_time(value as i64))
        }),
        _ => None,
    }
}

fn format_unix_time(timestamp: i64) -> Option<String> {
    // Accommodate the common millisecond representation without changing the
    // public output contract (RFC3339 strings).
    let seconds = if timestamp.unsigned_abs() > 10_000_000_000 {
        timestamp / 1_000
    } else {
        timestamp
    };
    OffsetDateTime::from_unix_timestamp(seconds)
        .ok()?
        .format(&Rfc3339)
        .ok()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn epg_url_preserves_opaque_md5_and_rejects_path_escape() {
        let url = build_epg_url(
            "https://epg.example.invalid/bootstrap",
            "/epg/v2/live/app/utc2/26",
            "opaque-md5-fixture",
        )
        .expect("valid EPG request");
        assert_eq!(url.path(), "/epg/v2/live/app/utc2/26");
        assert_eq!(url.query(), Some("md5=opaque-md5-fixture"));
        assert_eq!(
            build_epg_url("https://epg.example.invalid", "/epg/../secret", "md5"),
            Err(MagstvProviderError::InvalidEpgRequest)
        );
    }

    #[test]
    fn hypothetical_epg_shape_maps_to_jellyrin_programs() {
        let payload = json!({
            "ChannelId": "26",
            "programs": [{
                "id": "p1",
                "title": "Morning fixture",
                "start": "2026-07-30T08:00:00Z",
                "end": "2026-07-30T09:00:00Z",
                "description": "Offline fixture"
            }]
        });
        let programs = programs_from_payload(&payload).expect("fixture has a program");
        assert_eq!(programs.len(), 1);
        assert_eq!(programs[0]["Name"], "Morning fixture");
        assert_eq!(programs[0]["ChannelId"], "26");
        assert_eq!(programs[0]["Overview"], "Offline fixture");
    }

    #[test]
    fn typed_portal_epg_maps_dates_and_live_state() {
        let body = json!({
            "returnCode": "0",
            "data": {
                "channelCode": "mx-news",
                "programList": [{
                    "contentId": "program-1",
                    "programName": "Noon fixture",
                    "startTime": "2026-07-30T11:00:00Z",
                    "endTime": "2026-07-30T13:00:00Z",
                    "desc": "Sanitised portal fixture"
                }]
            }
        });
        let programs = parse_portal_epg_programs(
            "mx-news",
            &serde_json::to_vec(&body).unwrap(),
            OffsetDateTime::parse("2026-07-30T12:00:00Z", &Rfc3339).unwrap(),
        )
        .unwrap();
        assert_eq!(programs.len(), 1);
        assert_eq!(programs[0].name, "Noon fixture");
        assert!(programs[0].is_live);
        assert!(programs[0].id.starts_with("magstv-program-"));
    }
}
