use std::path::{Path, PathBuf};
use std::time::{SystemTime, UNIX_EPOCH};

use redis::AsyncCommands;
use serde::{Deserialize, Serialize};
use tokio::fs::{self, File};
use tokio::io::AsyncWriteExt;
use tracing::{info, warn};

use crate::config::{LocalCacheConfig, RedisConfig};
use crate::errors::ProxyError;

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum CacheStatus {
    Hit,
    Miss,
    Bypass,
    Disabled,
}

impl CacheStatus {
    pub fn header_value(self) -> &'static str {
        match self {
            Self::Hit => "HIT",
            Self::Miss => "MISS",
            Self::Bypass => "BYPASS",
            Self::Disabled => "DISABLED",
        }
    }
}

#[derive(Clone, Debug)]
pub struct CachedResponse {
    pub body: Vec<u8>,
    pub headers: CachedHeaders,
}

pub struct PendingCacheWrite {
    inner: Option<PendingCacheWriteInner>,
}

#[derive(Clone, Debug, Default, Serialize, Deserialize)]
pub struct CachedHeaders {
    pub content_type: Option<String>,
    pub content_length: Option<u64>,
    pub etag: Option<String>,
    pub last_modified: Option<String>,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
struct CacheMetadata {
    file_path: String,
    body_size: u64,
    headers: CachedHeaders,
    created_at: u64,
    last_accessed_at: u64,
}

#[derive(Clone)]
pub struct LocalCache {
    inner: Option<LocalCacheInner>,
}

#[derive(Clone)]
struct LocalCacheInner {
    redis_client: redis::Client,
    directory: PathBuf,
    max_size_bytes: u64,
    key_prefix: String,
}

struct PendingCacheWriteInner {
    redis_client: redis::Client,
    cache_key: String,
    metadata_key: String,
    total_size_key: String,
    lfu_key: String,
    accessed_key: String,
    temp_path: PathBuf,
    final_path: PathBuf,
    file: Option<File>,
    body_size: u64,
    written_size: u64,
    headers: CachedHeaders,
}

impl LocalCache {
    pub async fn new(config: Option<LocalCacheConfig>, redis: &RedisConfig) -> Self {
        let Some(config) = config else {
            return Self { inner: None };
        };

        if !config.enabled {
            return Self { inner: None };
        }

        let redis_client = match redis::Client::open(redis.redis_url.clone()) {
            Ok(client) => client,
            Err(error) => {
                warn!(error = %error, "failed to create redis client, local cache disabled");
                return Self { inner: None };
            }
        };

        if let Err(error) = fs::create_dir_all(&config.directory).await {
            warn!(error = %error, path = %config.directory, "failed to create local cache directory, local cache disabled");
            return Self { inner: None };
        }

        match redis_client.get_multiplexed_async_connection().await {
            Ok(mut connection) => {
                let ping: redis::RedisResult<String> =
                    redis::cmd("PING").query_async(&mut connection).await;
                if let Err(error) = ping {
                    warn!(error = %error, "failed to ping redis, local cache disabled");
                    return Self { inner: None };
                }
            }
            Err(error) => {
                warn!(error = %error, "failed to connect to redis, local cache disabled");
                return Self { inner: None };
            }
        }

        info!(path = %config.directory, max_size_bytes = config.max_size_bytes, "local cache enabled");

        Self {
            inner: Some(LocalCacheInner {
                redis_client,
                directory: PathBuf::from(config.directory),
                max_size_bytes: config.max_size_bytes,
                key_prefix: redis.redis_key_prefix.clone(),
            }),
        }
    }

    pub async fn get(
        &self,
        bucket: &str,
        object_key: &str,
    ) -> Result<(CacheStatus, Option<CachedResponse>), ProxyError> {
        let Some(inner) = &self.inner else {
            return Ok((CacheStatus::Disabled, None));
        };

        if should_bypass_cache(object_key) {
            return Ok((CacheStatus::Bypass, None));
        }

        let cache_key = build_cache_key(bucket, object_key);
        let metadata_key = inner.redis_metadata_key(&cache_key);
        let lfu_key = inner.redis_lfu_key();
        let accessed_key = inner.redis_accessed_key();

        let mut connection = match inner.redis_client.get_multiplexed_async_connection().await {
            Ok(connection) => connection,
            Err(error) => {
                warn!(error = %error, "redis unavailable, local cache disabled for read");
                return Ok((CacheStatus::Disabled, None));
            }
        };

        let metadata_json: Option<String> =
            connection.get(&metadata_key).await.map_err(|error| {
                ProxyError::InternalError(format!(
                    "failed to load cache metadata from redis: {error}"
                ))
            })?;

        let Some(metadata_json) = metadata_json else {
            return Ok((CacheStatus::Miss, None));
        };

        let metadata: CacheMetadata = serde_json::from_str(&metadata_json).map_err(|error| {
            ProxyError::InternalError(format!("failed to parse cache metadata: {error}"))
        })?;

        let body = match fs::read(&metadata.file_path).await {
            Ok(body) => body,
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
                let _: () = connection.del(&metadata_key).await.map_err(|redis_error| {
                    ProxyError::InternalError(format!(
                        "failed to delete stale cache metadata: {redis_error}"
                    ))
                })?;
                let _: () = connection
                    .zrem(&lfu_key, &cache_key)
                    .await
                    .map_err(|redis_error| {
                        ProxyError::InternalError(format!(
                            "failed to delete stale cache lfu entry: {redis_error}"
                        ))
                    })?;
                let _: () =
                    connection
                        .zrem(&accessed_key, &cache_key)
                        .await
                        .map_err(|redis_error| {
                            ProxyError::InternalError(format!(
                                "failed to delete stale cache access entry: {redis_error}"
                            ))
                        })?;
                return Ok((CacheStatus::Miss, None));
            }
            Err(error) => {
                return Err(ProxyError::InternalError(format!(
                    "failed to read cached body: {error}"
                )));
            }
        };

        let now = unix_timestamp();
        let _: f64 = connection
            .zincr(&lfu_key, &cache_key, 1)
            .await
            .map_err(|error| {
                ProxyError::InternalError(format!("failed to update cache lfu score: {error}"))
            })?;
        let _: () = connection
            .zadd(&accessed_key, &cache_key, now)
            .await
            .map_err(|error| {
                ProxyError::InternalError(format!("failed to update cache access score: {error}"))
            })?;
        let metadata = CacheMetadata {
            last_accessed_at: now,
            ..metadata
        };
        let metadata_json = serde_json::to_string(&metadata).map_err(|error| {
            ProxyError::InternalError(format!(
                "failed to serialize updated cache metadata: {error}"
            ))
        })?;
        let _: () = connection
            .set(&metadata_key, metadata_json)
            .await
            .map_err(|error| {
                ProxyError::InternalError(format!(
                    "failed to persist updated cache metadata: {error}"
                ))
            })?;

        Ok((
            CacheStatus::Hit,
            Some(CachedResponse {
                body,
                headers: metadata.headers,
            }),
        ))
    }

    pub async fn prepare_stream_store(
        &self,
        bucket: &str,
        object_key: &str,
        content_length: Option<u64>,
        headers: CachedHeaders,
    ) -> Result<(CacheStatus, Option<PendingCacheWrite>), ProxyError> {
        let Some(inner) = &self.inner else {
            return Ok((CacheStatus::Disabled, None));
        };

        if should_bypass_cache(object_key) {
            return Ok((CacheStatus::Bypass, None));
        }

        if !can_stream_store(content_length, inner.max_size_bytes) {
            return Ok((CacheStatus::Bypass, None));
        }

        let body_size = content_length.expect("checked above");
        let cache_key = build_cache_key(bucket, object_key);
        let final_path = inner.directory.join(&cache_key);
        let temp_path = inner.temp_path_for(&cache_key);
        let metadata_key = inner.redis_metadata_key(&cache_key);
        let total_size_key = inner.redis_total_size_key();
        let lfu_key = inner.redis_lfu_key();
        let accessed_key = inner.redis_accessed_key();

        let mut connection = match inner.redis_client.get_multiplexed_async_connection().await {
            Ok(connection) => connection,
            Err(error) => {
                warn!(error = %error, "redis unavailable, local cache disabled for stream write");
                return Ok((CacheStatus::Disabled, None));
            }
        };

        inner.ensure_capacity(&mut connection, body_size).await?;

        let file = File::create(&temp_path).await.map_err(|error| {
            ProxyError::InternalError(format!("failed to create temp cache file: {error}"))
        })?;

        Ok((
            CacheStatus::Miss,
            Some(PendingCacheWrite {
                inner: Some(PendingCacheWriteInner {
                    redis_client: inner.redis_client.clone(),
                    cache_key,
                    metadata_key,
                    total_size_key,
                    lfu_key,
                    accessed_key,
                    temp_path,
                    final_path,
                    file: Some(file),
                    body_size,
                    written_size: 0,
                    headers,
                }),
            }),
        ))
    }
}

impl LocalCacheInner {
    async fn ensure_capacity(
        &self,
        connection: &mut redis::aio::MultiplexedConnection,
        required_size: u64,
    ) -> Result<(), ProxyError> {
        let total_size_key = self.redis_total_size_key();
        let lfu_key = self.redis_lfu_key();
        let accessed_key = self.redis_accessed_key();

        loop {
            let current_size: Option<u64> =
                connection.get(&total_size_key).await.map_err(|error| {
                    ProxyError::InternalError(format!("failed to read cache total size: {error}"))
                })?;
            let current_size = current_size.unwrap_or(0);

            if current_size + required_size <= self.max_size_bytes {
                return Ok(());
            }

            let lowest_frequency = redis::cmd("ZRANGE")
                .arg(&lfu_key)
                .arg(0)
                .arg(0)
                .arg("WITHSCORES")
                .query_async::<Vec<(String, u64)>>(connection)
                .await
                .map_err(|error| {
                    ProxyError::InternalError(format!("failed to read lfu scores: {error}"))
                })?;

            let Some((_, score)) = lowest_frequency.into_iter().next() else {
                return Ok(());
            };

            let candidate_keys: Vec<String> = redis::cmd("ZRANGEBYSCORE")
                .arg(&lfu_key)
                .arg(score)
                .arg(score)
                .query_async(connection)
                .await
                .map_err(|error| {
                    ProxyError::InternalError(format!("failed to read lfu score bucket: {error}"))
                })?;

            let mut victim: Option<(String, u64)> = None;
            for cache_key in candidate_keys {
                let last_accessed: Option<u64> = connection
                    .zscore(&accessed_key, &cache_key)
                    .await
                    .map_err(|error| {
                        ProxyError::InternalError(format!(
                            "failed to read cache access score: {error}"
                        ))
                    })?;
                let last_accessed = last_accessed.unwrap_or(0);
                if victim
                    .as_ref()
                    .map(|(_, current)| last_accessed < *current)
                    .unwrap_or(true)
                {
                    victim = Some((cache_key, last_accessed));
                }
            }

            let Some((cache_key, _)) = victim else {
                return Ok(());
            };

            let metadata_key = self.redis_metadata_key(&cache_key);
            let metadata_json: Option<String> =
                connection.get(&metadata_key).await.map_err(|error| {
                    ProxyError::InternalError(format!("failed to load eviction metadata: {error}"))
                })?;

            let Some(metadata_json) = metadata_json else {
                let _: () = connection
                    .zrem(&lfu_key, &cache_key)
                    .await
                    .map_err(|error| {
                        ProxyError::InternalError(format!("failed to prune lfu entry: {error}"))
                    })?;
                let _: () = connection
                    .zrem(&accessed_key, &cache_key)
                    .await
                    .map_err(|error| {
                        ProxyError::InternalError(format!("failed to prune access entry: {error}"))
                    })?;
                continue;
            };

            let metadata: CacheMetadata =
                serde_json::from_str(&metadata_json).map_err(|error| {
                    ProxyError::InternalError(format!("failed to parse eviction metadata: {error}"))
                })?;

            let _ = fs::remove_file(Path::new(&metadata.file_path)).await;
            let _: () = connection.del(&metadata_key).await.map_err(|error| {
                ProxyError::InternalError(format!("failed to delete eviction metadata: {error}"))
            })?;
            let _: () = connection
                .zrem(&lfu_key, &cache_key)
                .await
                .map_err(|error| {
                    ProxyError::InternalError(format!(
                        "failed to delete eviction lfu entry: {error}"
                    ))
                })?;
            let _: () = connection
                .zrem(&accessed_key, &cache_key)
                .await
                .map_err(|error| {
                    ProxyError::InternalError(format!(
                        "failed to delete eviction access entry: {error}"
                    ))
                })?;
            let _: i64 = connection
                .decr(&total_size_key, metadata.body_size as i64)
                .await
                .map_err(|error| {
                    ProxyError::InternalError(format!(
                        "failed to update eviction total size: {error}"
                    ))
                })?;
        }
    }

    fn redis_metadata_key(&self, cache_key: &str) -> String {
        format!("{}:cache:meta:{cache_key}", self.key_prefix)
    }

    fn redis_lfu_key(&self) -> String {
        format!("{}:cache:lfu", self.key_prefix)
    }

    fn redis_accessed_key(&self) -> String {
        format!("{}:cache:last_accessed", self.key_prefix)
    }

    fn redis_total_size_key(&self) -> String {
        format!("{}:cache:total_size", self.key_prefix)
    }

    fn temp_path_for(&self, cache_key: &str) -> PathBuf {
        let suffix = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap_or_default()
            .as_nanos();
        self.directory.join(format!("{cache_key}.{suffix}.tmp"))
    }
}

pub fn should_bypass_cache(object_key: &str) -> bool {
    object_key.ends_with("index.html")
}

pub fn can_stream_store(content_length: Option<u64>, max_size_bytes: u64) -> bool {
    matches!(content_length, Some(length) if length <= max_size_bytes)
}

fn should_commit_after_write(written_size: u64, body_size: u64) -> bool {
    written_size >= body_size
}

fn build_cache_key(bucket: &str, object_key: &str) -> String {
    let digest = md5::compute(format!("{bucket}:{object_key}"));
    format!("{digest:x}.bin")
}

fn unix_timestamp() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_secs()
}

impl PendingCacheWrite {
    pub async fn write_chunk(&mut self, chunk: &[u8]) -> Result<(), ProxyError> {
        let should_commit = {
            let Some(inner) = self.inner.as_mut() else {
                return Ok(());
            };

            let file = inner.file.as_mut().expect("pending cache file must exist");
            file.write_all(chunk).await.map_err(|error| {
                ProxyError::InternalError(format!("failed to write cache chunk: {error}"))
            })?;
            inner.written_size = inner.written_size.saturating_add(chunk.len() as u64);
            should_commit_after_write(inner.written_size, inner.body_size)
        };

        if should_commit {
            if let Some(inner) = self.inner.take() {
                Self::commit_inner(inner).await?;
            }
        }

        Ok(())
    }

    pub async fn commit(mut self) -> Result<(), ProxyError> {
        let Some(inner) = self.inner.take() else {
            return Ok(());
        };

        Self::commit_inner(inner).await
    }

    pub async fn abort(mut self) {
        if let Some(mut inner) = self.inner.take() {
            inner.file.take();
            let _ = fs::remove_file(&inner.temp_path).await;
        }
    }

    async fn commit_inner(mut inner: PendingCacheWriteInner) -> Result<(), ProxyError> {
        let file = inner.file.as_mut().expect("pending cache file must exist");
        file.flush().await.map_err(|error| {
            ProxyError::InternalError(format!("failed to flush cache file: {error}"))
        })?;
        inner.file.take();

        fs::rename(&inner.temp_path, &inner.final_path)
            .await
            .map_err(|error| {
                ProxyError::InternalError(format!("failed to finalize cache file: {error}"))
            })?;

        let mut connection = inner
            .redis_client
            .get_multiplexed_async_connection()
            .await
            .map_err(|error| {
                ProxyError::InternalError(format!(
                    "failed to connect to redis for cache commit: {error}"
                ))
            })?;

        let now = unix_timestamp();
        let metadata = CacheMetadata {
            file_path: inner.final_path.to_string_lossy().to_string(),
            body_size: inner.body_size,
            headers: inner.headers.clone(),
            created_at: now,
            last_accessed_at: now,
        };
        let metadata_json = serde_json::to_string(&metadata).map_err(|error| {
            ProxyError::InternalError(format!("failed to serialize cache metadata: {error}"))
        })?;

        let _: () = connection
            .set(&inner.metadata_key, metadata_json)
            .await
            .map_err(|error| {
                ProxyError::InternalError(format!("failed to persist cache metadata: {error}"))
            })?;
        let _: f64 = connection
            .zadd(&inner.lfu_key, &inner.cache_key, 1)
            .await
            .map_err(|error| {
                ProxyError::InternalError(format!("failed to persist cache lfu score: {error}"))
            })?;
        let _: f64 = connection
            .zadd(&inner.accessed_key, &inner.cache_key, now)
            .await
            .map_err(|error| {
                ProxyError::InternalError(format!("failed to persist cache access score: {error}"))
            })?;
        let _: i64 = connection
            .incr(&inner.total_size_key, inner.body_size as i64)
            .await
            .map_err(|error| {
                ProxyError::InternalError(format!("failed to update cache total size: {error}"))
            })?;

        Ok(())
    }
}

impl Drop for PendingCacheWriteInner {
    fn drop(&mut self) {
        if self.temp_path.exists() {
            match std::fs::remove_file(&self.temp_path) {
                Ok(()) => {
                    warn!(path = %self.temp_path.display(), "removed abandoned cache temp file");
                }
                Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
                Err(error) => {
                    warn!(
                        path = %self.temp_path.display(),
                        error = %error,
                        "failed to remove abandoned cache temp file"
                    );
                }
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::atomic::{AtomicU64, Ordering};
    use tokio::time::{sleep, Duration};

    #[test]
    fn skips_cache_for_any_index_html_suffix() {
        assert!(should_bypass_cache("index.html"));
        assert!(should_bypass_cache("docs/index.html"));
        assert!(!should_bypass_cache("assets/app.js"));
    }

    #[test]
    fn cache_status_header_values_are_stable() {
        assert_eq!(CacheStatus::Hit.header_value(), "HIT");
        assert_eq!(CacheStatus::Miss.header_value(), "MISS");
        assert_eq!(CacheStatus::Bypass.header_value(), "BYPASS");
        assert_eq!(CacheStatus::Disabled.header_value(), "DISABLED");
    }

    #[test]
    fn stream_store_requires_known_length_within_limit() {
        assert!(can_stream_store(Some(1024), 2048));
        assert!(!can_stream_store(None, 2048));
        assert!(!can_stream_store(Some(4096), 2048));
    }

    #[test]
    fn commit_boundary_is_last_written_byte() {
        assert!(!should_commit_after_write(1023, 1024));
        assert!(should_commit_after_write(1024, 1024));
        assert!(should_commit_after_write(2048, 1024));
    }

    #[tokio::test]
    async fn dropping_pending_cache_write_removes_temp_file() {
        static COUNTER: AtomicU64 = AtomicU64::new(0);

        let suffix = COUNTER.fetch_add(1, Ordering::Relaxed);
        let temp_path = std::env::temp_dir().join(format!("r2-proxy-cache-drop-test-{suffix}.tmp"));
        let final_path =
            std::env::temp_dir().join(format!("r2-proxy-cache-drop-test-{suffix}.bin"));

        let file = File::create(&temp_path).await.unwrap();
        let pending_write = PendingCacheWrite {
            inner: Some(PendingCacheWriteInner {
                redis_client: redis::Client::open("redis://127.0.0.1:6379").unwrap(),
                cache_key: "cache-key".to_string(),
                metadata_key: "meta".to_string(),
                total_size_key: "total".to_string(),
                lfu_key: "lfu".to_string(),
                accessed_key: "accessed".to_string(),
                temp_path: temp_path.clone(),
                final_path: final_path.clone(),
                file: Some(file),
                body_size: 1,
                written_size: 0,
                headers: CachedHeaders::default(),
            }),
        };

        drop(pending_write);
        sleep(Duration::from_millis(25)).await;

        assert!(!temp_path.exists());
        assert!(!final_path.exists());
    }
}
