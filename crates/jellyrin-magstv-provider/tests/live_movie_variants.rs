//! Opt-in variant probe: which startPlayVOD parameter shape does the portal
//! accept for MOVIES on the operator-owned account? Prints only codes/shapes.

use jellyrin_magstv_provider::{
    MAGSTV_PORTAL_KEY_METADATA_ENV, MagstvCommonParams, MagstvConfig, MagstvPortalClient,
    MagstvPortalCodec, MagstvSecret, PortalOperation, ReqwestMagstvTransport,
};

fn required(name: &str) -> String {
    std::env::var(name).unwrap_or_else(|_| panic!("missing required runtime variable {name}"))
}

#[tokio::test]
#[ignore = "requires an operator-owned account and the MX egress"]
async fn movie_start_play_vod_parameter_variants() {

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
    let authenticated = client.authenticate(&secret).await.expect("login");

    let columns = client
        .get_column_contents(
            &authenticated,
            authenticated
                .identity()
                .column_contents_request(None, 1, Some(100)),
        )
        .await
        .expect("root columns");
    let movies_root = columns
        .child_column_list
        .iter()
        .find(|column| column.code.as_deref() == Some("masnew_movies"))
        .expect("movies root column");
    let child_columns = client
        .get_column_contents(
            &authenticated,
            authenticated
                .identity()
                .column_contents_request(movies_root.id, 1, Some(100)),
        )
        .await
        .expect("movie categories");
    let media_column = child_columns.child_column_list.first().unwrap();
    let shelf = client
        .get_shelve_data(
            &authenticated,
            PortalOperation::ListMovies,
            authenticated.identity().shelve_request(
                media_column.id.expect("media column id"),
                "2",
                1,
                200,
            ),
        )
        .await
        .expect("movie shelf");
    let asset = shelf.asset_list.first().expect("one movie asset");
    let content_id = asset.content_id.clone().expect("movie content id");
    let column_id = media_column.id.expect("media column id");
    eprintln!(
        "probe asset content_type={:?} type={:?} program_type={:?} column_id={}",
        asset.content_type, asset.r#type, asset.program_type, column_id
    );

    for (auth_type, request_type, series_id) in [
        (Some("1".to_string()), "vod", None),
        (Some(String::new()), "vod", None),
        (None, "vod", None),
        (Some(String::new()), "1", None),
        (Some("1".to_string()), "1", None),
        (Some(String::new()), "2", None),
        (Some(String::new()), "0", None),
        (Some(String::new()), "vod", Some(String::new())),
    ] {
        let result = client
            .start_play_vod(
                &authenticated,
                authenticated.identity().start_play_vod_request(
                    column_id,
                    content_id.clone(),
                    series_id.clone(),
                    request_type,
                    0,
                    auth_type.clone(),
                    None,
                ),
            )
            .await;
        match result {
            Ok(play) => eprintln!(
                "auth_type={auth_type:?} type={request_type} series={series_id:?} -> OK episodes={} groups={} subtitles={}",
                play.episode_list.len(),
                play.episode_list
                    .iter()
                    .map(|episode| episode.total_movie_list.len())
                    .sum::<usize>(),
                play.episode_list
                    .iter()
                    .map(|episode| episode.subtitle_list.len())
                    .sum::<usize>(),
            ),
            Err(error) => eprintln!(
                "auth_type={auth_type:?} type={request_type} series={series_id:?} -> {error:?}"
            ),
        }
    }
}

#[tokio::test]
#[ignore = "requires an operator-owned account and the MX egress"]
async fn movie_start_play_vod_with_catalog_content_id() {
    let metadata = required(MAGSTV_PORTAL_KEY_METADATA_ENV);
    let content_id = required("MAGSTV_PROBE_CONTENT_ID");
    let column_id: i32 = required("MAGSTV_PROBE_COLUMN_ID").parse().expect("numeric column");
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
    let authenticated = client.authenticate(&secret).await.expect("login");

    for auth_type in [None, Some(String::new()), Some("1".to_string())] {
        let result = client
            .start_play_vod(
                &authenticated,
                authenticated.identity().start_play_vod_request(
                    column_id,
                    content_id.clone(),
                    None,
                    "vod",
                    0,
                    auth_type.clone(),
                    None,
                ),
            )
            .await;
        match result {
            Ok(play) => eprintln!(
                "catalog-id auth_type={auth_type:?} -> OK episodes={} groups={}",
                play.episode_list.len(),
                play.episode_list
                    .iter()
                    .map(|episode| episode.total_movie_list.len())
                    .sum::<usize>(),
            ),
            Err(error) => eprintln!("catalog-id auth_type={auth_type:?} -> {error:?}"),
        }
    }
}
