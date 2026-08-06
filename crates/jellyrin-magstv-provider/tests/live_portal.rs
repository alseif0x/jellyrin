//! Opt-in interoperability probe for an operator-owned MAGSTV account.
//!
//! It is ignored by default and reads all account/key material from the
//! process environment. CI therefore cannot contact the service accidentally.

use jellyrin_magstv_provider::{
    MAGSTV_PORTAL_KEY_METADATA_ENV, MagstvCommonParams, MagstvConfig, MagstvPortalClient,
    MagstvPortalCodec, MagstvSecret, PortalOperation, ReqwestMagstvTransport,
};
use serde_json::Value;
use url::Url;

fn required(name: &str) -> String {
    std::env::var(name).unwrap_or_else(|_| panic!("missing required runtime variable {name}"))
}

fn json_shape(value: &Value) -> String {
    match value {
        Value::Null => "null".to_string(),
        Value::Bool(_) => "bool".to_string(),
        Value::Number(_) => "number".to_string(),
        Value::String(value) => format!("string(len={})", value.len()),
        Value::Array(values) => {
            let first = values
                .first()
                .map(json_shape)
                .unwrap_or_else(|| "empty".to_string());
            format!("array(len={}, first={first})", values.len())
        }
        Value::Object(values) => {
            let mut keys = values.keys().cloned().collect::<Vec<_>>();
            keys.sort();
            format!("object(keys={})", keys.join(","))
        }
    }
}

fn opaque_string_shape(value: Option<&Value>) -> String {
    let Some(Value::String(value)) = value else {
        return "missing_or_non_string".to_string();
    };
    let url_shape = Url::parse(value).ok().map(|url| {
        let mut query_keys = url
            .query_pairs()
            .map(|(key, _)| key.into_owned())
            .collect::<Vec<_>>();
        query_keys.sort();
        format!(
            "url_scheme={} host_present={} path_len={} query_keys={}",
            url.scheme(),
            url.host_str().is_some(),
            url.path().len(),
            query_keys.join(",")
        )
    });
    let mut query_like = value
        .split('&')
        .filter_map(|pair| pair.split_once('=').map(|(key, value)| (key, value.len())))
        .collect::<Vec<_>>();
    query_like.sort_by(|left, right| left.0.cmp(right.0));
    format!(
        "string_len={} url_shape={} json_shape={} punctuation={{q:{},amp:{},eq:{},pct:{}}}",
        value.len(),
        url_shape.unwrap_or_else(|| "none".to_string()),
        serde_json::from_str::<Value>(value)
            .ok()
            .map(|value| json_shape(&value))
            .unwrap_or_else(|| "none".to_string()),
        value.contains('?'),
        value.contains('&'),
        value.contains('='),
        value.contains('%'),
    ) + &format!(
        " query_like_keys={}",
        query_like
            .iter()
            .map(|(key, length)| format!("{key}:len={length}"))
            .collect::<Vec<_>>()
            .join(",")
    )
}

fn log_play_shape(label: &str, play: &jellyrin_magstv_provider::MagstvStartPlayVodData) {
    for (episode_index, episode) in play.episode_list.iter().enumerate().take(2) {
        let movies = episode
            .total_movie_list
            .iter()
            .flat_map(|group| group.movie_list.iter());
        let movie_count = episode
            .total_movie_list
            .iter()
            .map(|group| group.movie_list.len())
            .sum::<usize>();
        for (movie_index, movie) in movies.enumerate().take(4) {
            let license_shapes = movie
                .license_list
                .iter()
                .map(json_shape)
                .collect::<Vec<_>>();
            let license_field_shapes = movie
                .license_list
                .iter()
                .map(|license| {
                    let object = license.as_object();
                    let value = object.and_then(|object| object.get("license"));
                    let tag = object.and_then(|object| object.get("tag"));
                    format!(
                        "license={} tag={}",
                        opaque_string_shape(value),
                        tag.map(json_shape).unwrap_or_else(|| "missing".to_string()),
                    )
                })
                .collect::<Vec<_>>();
            eprintln!(
                "{label} episode={} movie_count={} movie={} content_id_present={} license_count={} license_shapes={} license_fields={}",
                episode_index,
                movie_count,
                movie_index,
                movie
                    .content_id
                    .as_deref()
                    .is_some_and(|value| !value.trim().is_empty()),
                movie.license_list.len(),
                license_shapes.join("|"),
                license_field_shapes.join("|")
            );
        }
        for (subtitle_index, subtitle) in episode.subtitle_list.iter().enumerate().take(4) {
            eprintln!(
                "{label} episode={} subtitle={} language_present={} files={} file_shapes={}",
                episode_index,
                subtitle_index,
                subtitle
                    .language
                    .as_deref()
                    .is_some_and(|value| !value.trim().is_empty()),
                subtitle.file.len(),
                subtitle
                    .file
                    .iter()
                    .map(|file| {
                        let url_len = file.url.as_deref().map(str::len).unwrap_or(0);
                        format!(
                            "url_len={url_len},type_present={}",
                            file.file_type.is_some()
                        )
                    })
                    .collect::<Vec<_>>()
                    .join("|")
            );
        }
    }
}

#[tokio::test]
#[ignore = "requires an operator-owned account and the isolated MX egress sidecar"]
async fn owned_account_can_authenticate_through_the_verified_contract() {
    let metadata = required(MAGSTV_PORTAL_KEY_METADATA_ENV);
    let config = MagstvConfig {
        bootstrap_url: required("MAGSTV_BOOTSTRAP_URL"),
        secret_reference: "MAGSTV_LIVE_PROBE".to_string(),
        category_ids: Default::default(),
        excluded_category_ids: Default::default(),
        channel_limit: None,
        cdn_edge_host: None,
    };
    let secret = MagstvSecret::new(
        required("MAGSTV_LIVE_PROBE_USERNAME"),
        required("MAGSTV_LIVE_PROBE_PASSWORD"),
    );
    let common = MagstvCommonParams::from_environment().expect("valid runtime device identity");
    let codec =
        MagstvPortalCodec::from_manifest_hex(&metadata, common).expect("valid local APK metadata");
    let transport = ReqwestMagstvTransport::new().expect("MX egress transport available");
    let client = MagstvPortalClient::new(config, transport, codec).expect("valid portal client");

    let authenticated = client
        .authenticate(&secret)
        .await
        .expect("owned account authenticates");
    assert!(!authenticated.identity().user_id().is_empty());
    assert!(!authenticated.identity().user_token().is_empty());
    eprintln!(
        "portal identity portal_code_present={}",
        authenticated.identity().portal_code().is_some()
    );

    let columns = client
        .get_column_contents(
            &authenticated,
            authenticated
                .identity()
                .column_contents_request(None, 1, Some(100)),
        )
        .await
        .expect("owned account can enumerate root columns");
    assert!(!columns.child_column_list.is_empty());
    for column in &columns.child_column_list {
        eprintln!(
            "root-column id={:?} code={:?} name={:?} type={:?}",
            column.id, column.code, column.name, column.r#type
        );
    }

    let mut movie_probe: Option<(String, String, i32)> = None;
    let mut series_probe: Option<(String, String, i32)> = None;
    for (code, operation) in [
        ("masnew_movies", PortalOperation::ListMovies),
        ("masnew_series", PortalOperation::ListSeries),
    ] {
        let column = columns
            .child_column_list
            .iter()
            .find(|column| column.code.as_deref() == Some(code))
            .expect("expected root media column");
        let child_columns = client
            .get_column_contents(
                &authenticated,
                authenticated
                    .identity()
                    .column_contents_request(column.id, 1, Some(100)),
            )
            .await
            .expect("owned account can enumerate media categories");
        eprintln!(
            "shelf {code} categories={}",
            child_columns.child_column_list.len()
        );
        for child in child_columns.child_column_list.iter().take(5) {
            eprintln!(
                "media-column id={:?} code={:?} name={:?} type={:?}",
                child.id, child.code, child.name, child.r#type
            );
        }
        let media_column = child_columns.child_column_list.first().unwrap_or(column);
        let shelf = client
            .get_shelve_data(
                &authenticated,
                operation,
                authenticated.identity().shelve_request(
                    media_column.id.expect("media column id"),
                    // tc/{h0,l,r,u} all use the literal "2" for VOD shelves;
                    // ChildColumn.type describes the column, not this request.
                    "2",
                    1,
                    200,
                ),
            )
            .await
            .expect("owned account can enumerate media shelf");
        eprintln!("shelf {code} assets={}", shelf.asset_list.len());
        if let Some(asset) = shelf.asset_list.first() {
            eprintln!(
                "shelf {code} sample content_type={:?} type={:?} program_type={:?}",
                asset.content_type, asset.r#type, asset.program_type
            );
            if code == "masnew_series" {
                if let (Some(content_id), Some(request_type)) =
                    (asset.content_id.clone(), asset.r#type.clone())
                {
                    series_probe = Some((
                        content_id,
                        request_type,
                        media_column.id.expect("media column id"),
                    ));
                }
            } else if code == "masnew_movies"
                && let (Some(content_id), Some(request_type)) =
                    (asset.content_id.clone(), asset.r#type.clone())
            {
                movie_probe = Some((
                    content_id,
                    request_type,
                    media_column.id.expect("media column id"),
                ));
            }
        }
        assert!(!shelf.asset_list.is_empty());
    }

    let language = std::env::var("MAGSTV_APP_LANGUAGE").unwrap_or_else(|_| "es".to_string());
    let mac_addr =
        std::env::var("MAGSTV_MAC_ADDR").unwrap_or_else(|_| "02:00:00:00:00:01".to_string());
    let (has_pay, user_identity) = match client
        .get_auth_info(
            &authenticated,
            authenticated
                .identity()
                .auth_info_request("1", language.clone()),
        )
        .await
    {
        Ok(auth_info) => {
            eprintln!(
                "auth info entries={} has_pay={}",
                auth_info.auth_info_list.len(),
                auth_info.has_pay.is_some()
            );
            (auth_info.has_pay, auth_info.user_identity)
        }
        Err(error) => {
            eprintln!("auth info failed={error:?}");
            (None, None)
        }
    };

    let app_version = std::env::var("MAGSTV_APP_VERSION").unwrap_or_else(|_| "49905".to_string());
    for request_type in ["vod", "live"] {
        match client
            .get_slb_info(
                &authenticated,
                authenticated.identity().slb_info_request(
                    app_version.clone(),
                    has_pay.clone(),
                    language.clone(),
                    user_identity.clone(),
                    request_type,
                    None,
                ),
            )
            .await
        {
            Ok(slb) => eprintln!(
                "slb type={request_type} cdn_count={} play_params_present={} play_params_len={} error_code={:?} rst_status={:?} merge_status={:?}",
                slb.cdn_list.len(),
                slb.play_params
                    .as_deref()
                    .is_some_and(|value| !value.is_empty()),
                slb.play_params.as_deref().map(str::len).unwrap_or(0),
                slb.error_code,
                slb.rst_status,
                slb.merge_rst_status,
            ),
            Err(error) => eprintln!("slb type={request_type} failed={error:?}"),
        }
    }

    let (series_content_id, series_request_type, series_column_id) =
        series_probe.expect("series shelf exposes a detail request type");

    // The player does not use a null/"0" auth type here.  The VOD screen
    // passes the series id again as seriesContentId and uses authType "1";
    // the episode list is optional for the initial series negotiation.
    for episode_number_list in [None, Some(vec![0, 1])] {
        let episode_list_present = episode_number_list.is_some();
        match client
            .start_play_vod(
                &authenticated,
                authenticated.identity().start_play_vod_request(
                    series_column_id,
                    series_content_id.clone(),
                    Some(series_content_id.clone()),
                    "vod",
                    0,
                    Some("1".to_string()),
                    episode_number_list,
                ),
            )
            .await
        {
            Ok(play) => {
                eprintln!(
                    "series startPlayVOD episode_list_present={} episodes={} movie_groups={} subtitles={}",
                    episode_list_present,
                    play.episode_list.len(),
                    play.episode_list
                        .iter()
                        .map(|episode| episode.total_movie_list.len())
                        .sum::<usize>(),
                    play.episode_list
                        .iter()
                        .map(|episode| episode.subtitle_list.len())
                        .sum::<usize>()
                );
                log_play_shape("series startPlayVOD", &play);

                // Optional operator-only handoff for the local Ranger oracle.
                // The file is deliberately outside the repository and is
                // never printed: it contains a short-lived playback licence.
                if let Some(path) = std::env::var_os("MAGSTV_LIVE_PROBE_SECRET_OUT")
                    && !std::path::Path::new(&path).exists()
                    && let Some(episode) = play.episode_list.first()
                    && let Some(group) = episode.total_movie_list.first()
                    && let Some(movie) = group.movie_list.first()
                    && let Some(license) = movie.license_list.first()
                    && let Some(license_object) = license.as_object()
                    && let (Some(content_id), Some(license_value), Some(tag)) = (
                        movie.content_id.as_deref(),
                        license_object.get("license").and_then(Value::as_str),
                        license_object.get("tag").and_then(Value::as_str),
                    )
                {
                    let handoff = serde_json::json!({
                        "content_id": content_id,
                        "license": license_value,
                        "tag": tag,
                        "quality": group.quality,
                        "audio_info": movie.audio_info,
                        "encode_format": movie.encode_format,
                        "video_format": movie.video_format,
                        "volume": movie.volume,
                    });
                    if let Some(parent) = std::path::Path::new(&path).parent() {
                        std::fs::create_dir_all(parent).expect("create secret handoff directory");
                    }
                    std::fs::write(&path, serde_json::to_vec(&handoff).expect("serialize handoff"))
                        .expect("write secret handoff");
                }
            }
            Err(error) => eprintln!(
                "series startPlayVOD episode_list_present={} failed={error:?}",
                episode_list_present
            ),
        }
    }

    let (movie_content_id, _movie_request_type, movie_column_id) =
        movie_probe.expect("movie shelf exposes a detail request type");
    match client
        .start_play_vod(
            &authenticated,
            authenticated.identity().start_play_vod_request(
                movie_column_id,
                movie_content_id,
                None,
                "vod",
                0,
                Some("1".to_string()),
                None,
            ),
        )
        .await
    {
        Ok(play) => {
            eprintln!(
                "movie startPlayVOD episodes={} movie_groups={} subtitles={}",
                play.episode_list.len(),
                play.episode_list
                    .iter()
                    .map(|episode| episode.total_movie_list.len())
                    .sum::<usize>(),
                play.episode_list
                    .iter()
                    .map(|episode| episode.subtitle_list.len())
                    .sum::<usize>()
            );
            log_play_shape("movie startPlayVOD", &play);
        }
        Err(error) => eprintln!("movie startPlayVOD failed={error:?}"),
    }

    match client
        .get_item_data(
            &authenticated,
            authenticated.identity().item_data_request(
                series_content_id,
                series_request_type,
                Some("0".to_string()),
                Some(language.clone()),
                mac_addr,
            ),
        )
        .await
    {
        Ok(detail) => {
            eprintln!(
                "series detail episodes={} audio_info={} subtitle_info={} more_audio={} more_subtitle={}",
                detail.simple_program_list.len(),
                detail.audio_info.is_some(),
                detail.subs_info.is_some(),
                detail.more_audio.unwrap_or_default(),
                detail.more_subtitle.unwrap_or_default()
            );
        }
        Err(error) => eprintln!("series detail failed={error:?}"),
    }
}
