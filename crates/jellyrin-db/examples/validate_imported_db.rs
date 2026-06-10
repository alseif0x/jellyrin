use jellyrin_db::Database;
use std::env;

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
    let expected_folders = env_usize("JELLYRIN_VALIDATE_EXPECTED_FOLDERS", 3);
    let expected_items = env_usize("JELLYRIN_VALIDATE_EXPECTED_ITEMS", 81);
    let expected_tv_items = env_usize("JELLYRIN_VALIDATE_EXPECTED_TV_ITEMS", 53);
    let expected_items_with_streams =
        env_usize("JELLYRIN_VALIDATE_EXPECTED_ITEMS_WITH_STREAMS", 78);

    let db = Database::connect(&database_url).await?;

    let users = db.users().await?;
    let user = users
        .iter()
        .find(|user| user.name.eq_ignore_ascii_case(&username))
        .expect("expected validation user to exist");
    db.verify_user_password(user.id, &password).await?;

    let folders = db.virtual_folders().await?;
    let items = db.media_items().await?;
    let items_with_streams = items
        .iter()
        .filter(|item| !item.media_streams.is_empty())
        .count();
    let tv_items = items
        .iter()
        .filter(|item| item.collection_type.as_deref() == Some("tvshows"))
        .count();

    anyhow::ensure!(
        folders.len() == expected_folders,
        "expected {expected_folders} folders, got {}",
        folders.len()
    );
    anyhow::ensure!(
        items.len() == expected_items,
        "expected {expected_items} items, got {}",
        items.len()
    );
    anyhow::ensure!(
        tv_items == expected_tv_items,
        "expected {expected_tv_items} tv items, got {tv_items}"
    );
    anyhow::ensure!(
        items_with_streams >= expected_items_with_streams,
        "expected at least {expected_items_with_streams} items with streams, got {items_with_streams}"
    );

    println!(
        "ok users={} folders={} items={} tv_items={} items_with_streams={}",
        users.len(),
        folders.len(),
        items.len(),
        tv_items,
        items_with_streams
    );
    Ok(())
}

fn env_usize(name: &str, default: usize) -> usize {
    env::var(name)
        .ok()
        .and_then(|value| value.parse().ok())
        .unwrap_or(default)
}
