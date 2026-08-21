use std::{
    collections::HashMap,
    sync::{
        Arc,
        atomic::{AtomicU64, Ordering},
    },
    time::{Duration, SystemTime, UNIX_EPOCH},
};

use jellyrin_db::{MediaCatalogStore, MediaItemFacetKind};
use redis::aio::ConnectionManager;
use sha2::{Digest, Sha256};
use tokio::sync::Mutex;
use uuid::Uuid;

use crate::Database;

const CACHE_NAMESPACE: &str = "jellyrin:catalog:v1";
const MAX_CACHE_VALUE_BYTES: usize = 64 * 1024;
const MAX_SINGLE_FLIGHT_KEYS: usize = 128;
const STARTUP_CONNECT_TIMEOUT: Duration = Duration::from_secs(2);
const FAILURE_BYPASS: Duration = Duration::from_secs(5);

static SHARED_CATALOG_CACHE: std::sync::OnceLock<SharedCatalogCache> = std::sync::OnceLock::new();

struct SharedCatalogCache {
    connection: ConnectionManager,
    ttl: Duration,
    command_timeout: Duration,
    fill_locks: Mutex<HashMap<String, Arc<Mutex<()>>>>,
    overflow_fill_lock: Arc<Mutex<()>>,
    bypass_until_epoch_millis: AtomicU64,
}

enum CacheLookup {
    Hit(Vec<String>),
    Miss,
    Unavailable,
}

impl SharedCatalogCache {
    async fn connect(redis_url: &str, ttl: Duration, command_timeout: Duration) -> Option<Self> {
        let client = redis::Client::open(redis_url).ok()?;
        let connection =
            tokio::time::timeout(STARTUP_CONNECT_TIMEOUT, ConnectionManager::new(client))
                .await
                .ok()?
                .ok()?;
        Some(Self {
            connection,
            ttl,
            command_timeout,
            fill_locks: Mutex::new(HashMap::new()),
            overflow_fill_lock: Arc::new(Mutex::new(())),
            bypass_until_epoch_millis: AtomicU64::new(0),
        })
    }

    async fn get(&self, key: &str) -> CacheLookup {
        if self.bypass_until_epoch_millis.load(Ordering::Relaxed) > epoch_millis() {
            return CacheLookup::Unavailable;
        }
        let mut connection = self.connection.clone();
        let result = tokio::time::timeout(
            self.command_timeout,
            redis::cmd("GET")
                .arg(key)
                .query_async::<Option<Vec<u8>>>(&mut connection),
        )
        .await;
        let payload = match result {
            Ok(Ok(Some(payload))) => payload,
            Ok(Ok(None)) => return CacheLookup::Miss,
            Ok(Err(_)) | Err(_) => {
                self.trip_bypass();
                return CacheLookup::Unavailable;
            }
        };
        if payload.len() > MAX_CACHE_VALUE_BYTES {
            return CacheLookup::Miss;
        }
        serde_json::from_slice(&payload)
            .map(CacheLookup::Hit)
            .unwrap_or(CacheLookup::Miss)
    }

    async fn put(&self, key: &str, values: &[String]) {
        let Ok(payload) = serde_json::to_vec(values) else {
            return;
        };
        if payload.len() > MAX_CACHE_VALUE_BYTES {
            return;
        }
        let mut connection = self.connection.clone();
        let ttl_seconds = self.ttl.as_secs().max(1);
        let result = tokio::time::timeout(
            self.command_timeout,
            redis::cmd("SET")
                .arg(key)
                .arg(payload)
                .arg("EX")
                .arg(ttl_seconds)
                .query_async::<()>(&mut connection),
        )
        .await;
        if !matches!(result, Ok(Ok(()))) {
            self.trip_bypass();
        }
    }

    async fn fill_lock(&self, key: &str) -> Arc<Mutex<()>> {
        let mut locks = self.fill_locks.lock().await;
        locks.retain(|_, lock| Arc::strong_count(lock) > 1);
        if let Some(lock) = locks.get(key) {
            return Arc::clone(lock);
        }
        if locks.len() >= MAX_SINGLE_FLIGHT_KEYS {
            return Arc::clone(&self.overflow_fill_lock);
        }
        let lock = Arc::new(Mutex::new(()));
        locks.insert(key.to_string(), Arc::clone(&lock));
        lock
    }

    fn trip_bypass(&self) {
        self.bypass_until_epoch_millis.store(
            epoch_millis().saturating_add(FAILURE_BYPASS.as_millis() as u64),
            Ordering::Relaxed,
        );
    }
}

/// Enables the optional, fail-open Redis cache for shared catalogue projections.
///
/// The URL may contain authentication material and is deliberately never logged or retained in
/// application diagnostics. Failure leaves PostgreSQL as the only read path.
pub async fn configure_shared_catalog_cache(
    redis_url: Option<&str>,
    ttl: Duration,
    command_timeout: Duration,
) -> bool {
    if SHARED_CATALOG_CACHE.get().is_some() {
        return true;
    }
    let Some(redis_url) = redis_url.map(str::trim).filter(|url| !url.is_empty()) else {
        return false;
    };
    let Some(cache) = SharedCatalogCache::connect(redis_url, ttl, command_timeout).await else {
        return false;
    };
    SHARED_CATALOG_CACHE.set(cache).is_ok() || SHARED_CATALOG_CACHE.get().is_some()
}

pub(crate) async fn cached_media_item_facet_display_values(
    db: &Database,
    kind: MediaItemFacetKind,
    virtual_folder_ids: &[Uuid],
) -> anyhow::Result<Vec<String>> {
    let Some(cache) = SHARED_CATALOG_CACHE.get() else {
        return database_facet_display_values(db, kind, virtual_folder_ids).await;
    };
    let key = facet_cache_key(kind, virtual_folder_ids);
    match cache.get(&key).await {
        CacheLookup::Hit(values) => return Ok(values),
        CacheLookup::Unavailable => {
            return database_facet_display_values(db, kind, virtual_folder_ids).await;
        }
        CacheLookup::Miss => {}
    }

    // Recheck after entering the per-key lane so an expiry cannot fan out the same expensive
    // PostgreSQL projection across every concurrent Home/catalogue request.
    let fill_lock = cache.fill_lock(&key).await;
    let _fill_guard = fill_lock.lock().await;
    match cache.get(&key).await {
        CacheLookup::Hit(values) => return Ok(values),
        CacheLookup::Unavailable => {
            return database_facet_display_values(db, kind, virtual_folder_ids).await;
        }
        CacheLookup::Miss => {}
    }
    let values = database_facet_display_values(db, kind, virtual_folder_ids).await?;
    cache.put(&key, &values).await;
    Ok(values)
}

fn epoch_millis() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_millis()
        .try_into()
        .unwrap_or(u64::MAX)
}

async fn database_facet_display_values(
    db: &Database,
    kind: MediaItemFacetKind,
    virtual_folder_ids: &[Uuid],
) -> anyhow::Result<Vec<String>> {
    Ok(
        MediaCatalogStore::media_item_facet_values(db, kind, virtual_folder_ids)
            .await?
            .into_iter()
            .map(|facet| facet.display_value)
            .collect(),
    )
}

fn facet_cache_key(kind: MediaItemFacetKind, virtual_folder_ids: &[Uuid]) -> String {
    let mut folder_ids = virtual_folder_ids.to_vec();
    folder_ids.sort_unstable();
    folder_ids.dedup();
    let mut digest = Sha256::new();
    for folder_id in folder_ids {
        digest.update(folder_id.as_bytes());
    }
    format!(
        "{CACHE_NAMESPACE}:facet:{}:{:x}",
        kind.as_str(),
        digest.finalize()
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn facet_keys_are_order_independent_scoped_and_do_not_expose_folder_ids() {
        let first = Uuid::from_u128(1);
        let second = Uuid::from_u128(2);
        let genre = facet_cache_key(MediaItemFacetKind::Genre, &[first, second]);
        assert_eq!(
            genre,
            facet_cache_key(MediaItemFacetKind::Genre, &[second, first, first])
        );
        assert_ne!(
            genre,
            facet_cache_key(MediaItemFacetKind::Studio, &[first, second])
        );
        assert!(!genre.contains(&first.to_string()));
        assert!(!genre.contains(&second.to_string()));
    }

    #[tokio::test]
    async fn redis_cache_round_trip_is_optional_bounded_and_unicode_safe() {
        let Ok(redis_url) = std::env::var("JELLYRIN_TEST_REDIS_URL") else {
            return;
        };
        let cache = SharedCatalogCache::connect(
            &redis_url,
            Duration::from_secs(5),
            Duration::from_millis(250),
        )
        .await
        .expect("test Redis must be reachable");
        let key = format!("{CACHE_NAMESPACE}:test:{}", Uuid::new_v4());
        assert!(matches!(cache.get(&key).await, CacheLookup::Miss));
        let expected = vec!["Drama".to_string(), "Ciencia ficción".to_string()];
        cache.put(&key, &expected).await;
        let CacheLookup::Hit(actual) = cache.get(&key).await else {
            panic!("Redis cache did not return the populated value");
        };
        assert_eq!(actual, expected);
    }
}
