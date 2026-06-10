use axum::{
    body::Body,
    http::{Method, Request, StatusCode, header},
};
use http_body_util::BodyExt as _;
use jellyrin_api::{AppState, router};
use jellyrin_db::Database;
use serde_json::{Value, json};
use std::env;
use tower::ServiceExt as _;

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    let mut args = env::args().skip(1);
    let database_url = args.next().expect("database url argument is required");
    let username = args
        .next()
        .or_else(|| env::var("JELLYRIN_VALIDATE_USER").ok())
        .unwrap_or_else(|| "joe".to_string());
    let password = args
        .next()
        .or_else(|| env::var("JELLYRIN_VALIDATE_PASSWORD").ok())
        .expect("password argument or JELLYRIN_VALIDATE_PASSWORD is required");
    let expected_views = env_usize("JELLYRIN_VALIDATE_EXPECTED_VIEWS", 3);
    let expected_episodes = env_i64("JELLYRIN_VALIDATE_EXPECTED_EPISODES", 53);

    let db = Database::connect(&database_url).await?;
    let app = router(AppState {
        db,
        web_dir: ".".into(),
        log_dir: env::var("JELLYRIN_VALIDATE_LOG_DIR")
            .unwrap_or_else(|_| "/tmp/jellyrin-validate-api-log".to_string())
            .into(),
        local_address: env::var("JELLYRIN_VALIDATE_LOCAL_ADDRESS")
            .unwrap_or_else(|_| "http://127.0.0.1:8097".to_string()),
    });

    let auth = request_json(
        app.clone(),
        Request::builder()
            .method(Method::POST)
            .uri("/Users/AuthenticateByName")
            .header(header::CONTENT_TYPE, "application/json")
            .body(Body::from(
                json!({
                    "Username": username,
                    "Pw": password
                })
                .to_string(),
            ))?,
        StatusCode::OK,
    )
    .await?;
    let token = auth["AccessToken"]
        .as_str()
        .expect("auth response must include AccessToken")
        .to_string();
    let user_id = auth["User"]["Id"]
        .as_str()
        .expect("auth response must include user id")
        .to_string();

    let views = get_json(
        app.clone(),
        &format!("/UserViews?UserId={user_id}"),
        &token,
        StatusCode::OK,
    )
    .await?;
    anyhow::ensure!(
        views["Items"].as_array().unwrap().len() == expected_views,
        "expected {expected_views} views"
    );

    let tv = get_json(
        app.clone(),
        &format!("/Items?UserId={user_id}&IncludeItemTypes=Episode&Recursive=true&Limit=60"),
        &token,
        StatusCode::OK,
    )
    .await?;
    anyhow::ensure!(
        tv["TotalRecordCount"] == expected_episodes,
        "expected {expected_episodes} episodes"
    );
    let episode_id = tv["Items"][0]["Id"]
        .as_str()
        .expect("episode must include id")
        .to_string();

    let detail = get_json(
        app.clone(),
        &format!("/Items/{episode_id}?UserId={user_id}"),
        &token,
        StatusCode::OK,
    )
    .await?;
    anyhow::ensure!(
        detail["MediaStreams"].as_array().unwrap().len() >= 5,
        "expected streams"
    );
    anyhow::ensure!(
        detail["ImageTags"]["Primary"].as_str().is_some(),
        "expected primary image tag"
    );

    let images = get_json(
        app.clone(),
        &format!("/Items/{episode_id}/Images"),
        &token,
        StatusCode::OK,
    )
    .await?;
    anyhow::ensure!(
        !images.as_array().unwrap().is_empty(),
        "expected image infos"
    );

    let playback = get_json(
        app,
        &format!("/Items/{episode_id}/PlaybackInfo?UserId={user_id}"),
        &token,
        StatusCode::OK,
    )
    .await?;
    anyhow::ensure!(
        playback["MediaSources"][0]["MediaStreams"]
            .as_array()
            .unwrap()
            .iter()
            .any(|stream| stream["Type"] == "Audio"),
        "expected audio streams in PlaybackInfo"
    );

    println!(
        "ok token={} views={} episodes={} episode={} streams={} images={}",
        !token.is_empty(),
        expected_views,
        expected_episodes,
        episode_id,
        detail["MediaStreams"].as_array().unwrap().len(),
        images.as_array().unwrap().len()
    );
    Ok(())
}

async fn get_json(
    app: axum::Router,
    uri: &str,
    token: &str,
    expected: StatusCode,
) -> anyhow::Result<Value> {
    request_json(
        app,
        Request::builder()
            .uri(uri)
            .header("X-Emby-Token", token)
            .body(Body::empty())?,
        expected,
    )
    .await
}

async fn request_json(
    app: axum::Router,
    request: Request<Body>,
    expected: StatusCode,
) -> anyhow::Result<Value> {
    let response = app.oneshot(request).await?;
    let status = response.status();
    let body = response.into_body().collect().await?.to_bytes();
    anyhow::ensure!(
        status == expected,
        "expected status {expected}, got {status}: {}",
        String::from_utf8_lossy(&body)
    );
    Ok(serde_json::from_slice(&body)?)
}

fn env_usize(name: &str, default: usize) -> usize {
    env::var(name)
        .ok()
        .and_then(|value| value.parse().ok())
        .unwrap_or(default)
}

fn env_i64(name: &str, default: i64) -> i64 {
    env::var(name)
        .ok()
        .and_then(|value| value.parse().ok())
        .unwrap_or(default)
}
