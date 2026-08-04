use serde_json::Value;

pub(crate) fn hdhomerun_bool_field(entry: &Value, key: &str) -> bool {
    match entry.get(key) {
        Some(Value::Bool(b)) => *b,
        Some(Value::Number(n)) => n.as_u64().is_some_and(|v| v != 0),
        _ => false,
    }
}

pub(crate) fn parse_live_tv_hdhomerun_channels(lineup: &[Value]) -> Vec<Value> {
    lineup
        .iter()
        .filter(|entry| !hdhomerun_bool_field(entry, "DRM"))
        .filter_map(|entry| {
            let guide_number = entry.get("GuideNumber")?.as_str()?.to_string();
            let guide_name = entry
                .get("GuideName")
                .and_then(Value::as_str)
                .unwrap_or(&guide_number)
                .to_string();
            let url = entry
                .get("URL")
                .and_then(Value::as_str)
                .unwrap_or("")
                .to_string();
            Some(serde_json::json!({
                "Id": format!("hdhr_{guide_number}"),
                "Name": guide_name,
                "Number": guide_number,
                "Path": url,
                "ChannelType": "TV",
                "IsHD": hdhomerun_bool_field(entry, "HD"),
                "IsFavorite": hdhomerun_bool_field(entry, "Favorite"),
                "IsLegacyTuner": url.to_ascii_lowercase().starts_with("hdhomerun"),
            }))
        })
        .collect()
}

pub(crate) fn parse_live_tv_m3u_channels(contents: &str) -> Vec<Value> {
    let mut channels = Vec::new();
    let mut pending: Option<Value> = None;
    for line in contents
        .lines()
        .map(str::trim)
        .filter(|line| !line.is_empty())
    {
        if line.starts_with("#EXTINF") {
            let name = line
                .rsplit_once(',')
                .map(|(_, name)| name.trim())
                .filter(|name| !name.is_empty())
                .unwrap_or("Channel");
            let id = live_tv_m3u_attribute(line, "tvg-id")
                .or_else(|| live_tv_m3u_attribute(line, "channel-id"))
                .unwrap_or_else(|| live_tv_stable_id("channel", name));
            let display_name = live_tv_m3u_attribute(line, "tvg-name")
                .filter(|value| !value.trim().is_empty())
                .unwrap_or_else(|| name.to_string());
            let number = live_tv_m3u_attribute(line, "tvg-chno")
                .or_else(|| live_tv_m3u_attribute(line, "channel-number"));
            pending = Some(serde_json::json!({
                "Id": id,
                "Name": display_name,
                "Number": number,
                "ChannelType": "TV"
            }));
            continue;
        }
        if line.starts_with('#') {
            continue;
        }
        if let Some(mut channel) = pending.take() {
            channel["Path"] = serde_json::json!(line);
            channels.push(channel);
        }
    }
    channels
}

fn live_tv_m3u_attribute(line: &str, name: &str) -> Option<String> {
    let lower = line.to_ascii_lowercase();
    let attr = format!("{}=", name.to_ascii_lowercase());
    let start = lower.find(&attr)? + attr.len();
    let quote = line[start..].chars().next()?;
    if quote != '"' && quote != '\'' {
        return None;
    }
    let value_start = start + quote.len_utf8();
    let value_end = line[value_start..].find(quote)? + value_start;
    Some(line[value_start..value_end].trim().to_string())
}

pub(crate) fn parse_live_tv_xmltv_programs(contents: &str) -> Vec<Value> {
    live_tv_xml_elements(contents, "programme")
        .into_iter()
        .enumerate()
        .map(|(index, element)| {
            let channel_id = live_tv_xml_attribute(&element, "channel").unwrap_or_default();
            let start = live_tv_xml_attribute(&element, "start")
                .map(|value| live_tv_xmltv_datetime(&value))
                .unwrap_or(Value::Null);
            let end = live_tv_xml_attribute(&element, "stop")
                .map(|value| live_tv_xmltv_datetime(&value))
                .unwrap_or(Value::Null);
            let name = live_tv_xml_first_text(&element, "title")
                .unwrap_or_else(|| format!("Program {}", index + 1));
            let overview = live_tv_xml_first_text(&element, "desc").unwrap_or_default();
            serde_json::json!({
                "Id": live_tv_stable_id("program", &format!("{channel_id}-{index}-{name}")),
                "Name": name,
                "ChannelId": channel_id,
                "StartDate": start,
                "EndDate": end,
                "Overview": overview
            })
        })
        .collect()
}

fn live_tv_xmltv_datetime(value: &str) -> Value {
    let compact = value.split_whitespace().next().unwrap_or(value);
    if compact.len() >= 14 && compact[..14].chars().all(|ch| ch.is_ascii_digit()) {
        return serde_json::json!(format!(
            "{}-{}-{}T{}:{}:{}Z",
            &compact[0..4],
            &compact[4..6],
            &compact[6..8],
            &compact[8..10],
            &compact[10..12],
            &compact[12..14]
        ));
    }
    serde_json::json!(value)
}

fn live_tv_xml_first_text(contents: &str, tag: &str) -> Option<String> {
    live_tv_xml_elements(contents, tag)
        .into_iter()
        .map(|element| live_tv_xml_decode(&live_tv_strip_xml_tags(&element)))
        .find(|value| !value.is_empty())
}

fn live_tv_xml_elements(contents: &str, tag: &str) -> Vec<String> {
    let mut values = Vec::new();
    let open = format!("<{tag}");
    let close = format!("</{tag}>");
    let lower = contents.to_ascii_lowercase();
    let mut offset = 0usize;
    while let Some(start) = lower[offset..].find(&open) {
        let start = offset + start;
        let after_tag = start + open.len();
        if !lower[after_tag..]
            .chars()
            .next()
            .is_some_and(|ch| ch == '>' || ch.is_ascii_whitespace())
        {
            offset = after_tag;
            continue;
        }
        let Some(open_end) = lower[start..].find('>').map(|index| start + index + 1) else {
            break;
        };
        let Some(end) = lower[open_end..]
            .find(&close)
            .map(|index| open_end + index + close.len())
        else {
            break;
        };
        values.push(contents[start..end].to_string());
        offset = end;
    }
    values
}

fn live_tv_xml_attribute(element: &str, name: &str) -> Option<String> {
    let lower = element.to_ascii_lowercase();
    let attr = format!("{}=", name.to_ascii_lowercase());
    let start = lower.find(&attr)? + attr.len();
    let quote = element[start..].chars().next()?;
    if quote != '"' && quote != '\'' {
        return None;
    }
    let value_start = start + quote.len_utf8();
    let value_end = element[value_start..].find(quote)? + value_start;
    Some(live_tv_xml_decode(&element[value_start..value_end]))
}

fn live_tv_strip_xml_tags(value: &str) -> String {
    let mut output = String::new();
    let mut in_tag = false;
    for ch in value.chars() {
        match ch {
            '<' => in_tag = true,
            '>' => in_tag = false,
            _ if !in_tag => output.push(ch),
            _ => {}
        }
    }
    output
}

fn live_tv_xml_decode(value: &str) -> String {
    value
        .replace("&amp;", "&")
        .replace("&lt;", "<")
        .replace("&gt;", ">")
        .replace("&quot;", "\"")
        .replace("&apos;", "'")
        .trim()
        .to_string()
}

pub(crate) fn live_tv_stable_id(prefix: &str, value: &str) -> String {
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
