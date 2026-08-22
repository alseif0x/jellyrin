use std::env;

use anyhow::Context;
use jellyrin_db::{DatabaseConfig, DatabaseDriver, DatabaseManager, RemoteMediaCatalogStage};

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    let database_url = env::var("DATABASE_URL").context("DATABASE_URL is required")?;
    let stage_id =
        env::var("JELLYRIN_REMOTE_STAGE_ID").context("JELLYRIN_REMOTE_STAGE_ID is required")?;
    let config = DatabaseConfig::new(DatabaseDriver::PostgreSql, database_url)?;
    let database = DatabaseManager::new(config)?.connect().await?;
    let stage = RemoteMediaCatalogStage::try_from_id(stage_id)?;
    let folders = database.publish_remote_media_catalog_stage(&stage).await?;
    println!("published_folders={}", folders.len());
    Ok(())
}
