use chrono::{TimeZone, Utc};
use redis::AsyncCommands;
use tracing::warn;

use crate::config::RedisConfig;

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum Resolution {
    FiveMinutes,
    OneHour,
    OneDay,
}

impl Resolution {
    pub fn duration_seconds(self) -> u64 {
        match self {
            Self::FiveMinutes => 300,
            Self::OneHour => 3600,
            Self::OneDay => 86400,
        }
    }

    pub fn redis_key(self) -> &'static str {
        match self {
            Self::FiveMinutes => "5m",
            Self::OneHour => "1h",
            Self::OneDay => "1d",
        }
    }

    pub fn ttl_seconds(self) -> i64 {
        match self {
            Self::FiveMinutes | Self::OneHour => 8 * 24 * 60 * 60,
            Self::OneDay => 10 * 24 * 60 * 60,
        }
    }
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub enum StatsScope {
    Global,
    Bucket(String),
}

impl StatsScope {
    pub fn redis_key(&self) -> String {
        match self {
            Self::Global => "global".to_string(),
            Self::Bucket(bucket) => format!("bucket:{bucket}"),
        }
    }
}

#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub struct BucketTotals {
    pub requests: u64,
    pub bytes: u64,
    pub cache_hits: u64,
    pub cache_misses: u64,
    pub errors: u64,
}

impl BucketTotals {
    pub fn cache_hit_rate(self) -> f64 {
        let denominator = self.cache_hits + self.cache_misses;
        if denominator == 0 {
            0.0
        } else {
            self.cache_hits as f64 / denominator as f64
        }
    }

    pub fn error_rate(self) -> f64 {
        if self.requests == 0 {
            0.0
        } else {
            self.errors as f64 / self.requests as f64
        }
    }

    pub fn qps(self, seconds: u64) -> f64 {
        if seconds == 0 {
            0.0
        } else {
            self.requests as f64 / seconds as f64
        }
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum StatsCacheStatus {
    Hit,
    Miss,
    Bypass,
    Disabled,
}

impl StatsCacheStatus {
    pub fn is_non_cacheable(self) -> bool {
        matches!(self, Self::Bypass | Self::Disabled)
    }

    pub fn counts_as_hit(self) -> bool {
        matches!(self, Self::Hit)
    }

    pub fn counts_as_miss(self) -> bool {
        matches!(self, Self::Miss)
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum StatsResult {
    Success,
    Error,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct StatsEvent {
    pub timestamp: u64,
    pub bucket: String,
    pub path_and_query: String,
    pub object_key: Option<String>,
    pub bytes: u64,
    pub cache_status: StatsCacheStatus,
    pub result: StatsResult,
}

#[derive(Clone)]
pub struct StatsStore {
    redis_client: redis::Client,
    key_prefix: String,
}

impl StatsStore {
    pub fn new(redis: &RedisConfig) -> Result<Self, redis::RedisError> {
        let redis_client = redis::Client::open(redis.redis_url.clone())?;
        Ok(Self {
            redis_client,
            key_prefix: redis.redis_key_prefix.clone(),
        })
    }

    pub fn key_prefix(&self) -> &str {
        &self.key_prefix
    }

    pub async fn record(&self, event: StatsEvent) {
        if let Err(error) = self.record_inner(&event).await {
            warn!(
                error = %error,
                bucket = %event.bucket,
                path = %event.path_and_query,
                "failed to record stats event"
            );
        }
    }

    async fn record_inner(&self, event: &StatsEvent) -> Result<(), redis::RedisError> {
        let mut connection = self.redis_client.get_multiplexed_async_connection().await?;

        for scope in [StatsScope::Global, StatsScope::Bucket(event.bucket.clone())] {
            let totals_key = self.totals_key(&scope);
            let _: () = redis::pipe()
                .cmd("HINCRBY")
                .arg(&totals_key)
                .arg("requests")
                .arg(1)
                .cmd("HINCRBY")
                .arg(&totals_key)
                .arg("bytes")
                .arg(event.bytes as i64)
                .cmd("HINCRBY")
                .arg(&totals_key)
                .arg("cache_hits")
                .arg(if event.cache_status.counts_as_hit() { 1 } else { 0 })
                .cmd("HINCRBY")
                .arg(&totals_key)
                .arg("cache_misses")
                .arg(if event.cache_status.counts_as_miss() { 1 } else { 0 })
                .cmd("HINCRBY")
                .arg(&totals_key)
                .arg("errors")
                .arg(if matches!(event.result, StatsResult::Error) { 1 } else { 0 })
                .query_async(&mut connection)
                .await?;

            for resolution in [Resolution::FiveMinutes, Resolution::OneHour, Resolution::OneDay] {
                let series_key =
                    self.bucket_key(&scope, resolution, bucket_start(event.timestamp, resolution));
                let _: () = redis::pipe()
                    .cmd("HINCRBY")
                    .arg(&series_key)
                    .arg("requests")
                    .arg(1)
                    .cmd("HINCRBY")
                    .arg(&series_key)
                    .arg("bytes")
                    .arg(event.bytes as i64)
                    .cmd("HINCRBY")
                    .arg(&series_key)
                    .arg("cache_hits")
                    .arg(if event.cache_status.counts_as_hit() { 1 } else { 0 })
                    .cmd("HINCRBY")
                    .arg(&series_key)
                    .arg("cache_misses")
                    .arg(if event.cache_status.counts_as_miss() { 1 } else { 0 })
                    .cmd("HINCRBY")
                    .arg(&series_key)
                    .arg("errors")
                    .arg(if matches!(event.result, StatsResult::Error) { 1 } else { 0 })
                    .cmd("EXPIRE")
                    .arg(&series_key)
                    .arg(resolution.ttl_seconds())
                    .query_async(&mut connection)
                    .await?;
            }

            match event.result {
                StatsResult::Success if event.cache_status.counts_as_hit() => {
                    if let Some(object_key) = &event.object_key {
                        let key = self.daily_top_hits_key(&scope, event.timestamp);
                        let member = format!("{}|{}", event.bucket, object_key);
                        let _: f64 = connection.zincr(&key, member, 1).await?;
                        let _: bool = connection.expire(&key, Resolution::OneDay.ttl_seconds()).await?;
                    }
                }
                StatsResult::Success if event.cache_status.counts_as_miss() => {
                    let key = self.daily_top_misses_key(&scope, event.timestamp);
                    let member = format!("{}|{}", event.bucket, event.path_and_query);
                    let _: f64 = connection.zincr(&key, member, 1).await?;
                    let _: bool = connection.expire(&key, Resolution::OneDay.ttl_seconds()).await?;
                }
                StatsResult::Error => {
                    let key = self.daily_top_errors_key(&scope, event.timestamp);
                    let member = format!("{}|{}", event.bucket, event.path_and_query);
                    let _: f64 = connection.zincr(&key, member, 1).await?;
                    let _: bool = connection.expire(&key, Resolution::OneDay.ttl_seconds()).await?;
                }
                _ => {}
            }
        }

        Ok(())
    }

    pub fn totals_key(&self, scope: &StatsScope) -> String {
        format!("{}:stats:totals:{}", self.key_prefix, scope.redis_key())
    }

    pub fn bucket_key(
        &self,
        scope: &StatsScope,
        resolution: Resolution,
        bucket_start: u64,
    ) -> String {
        format!(
            "{}:stats:ts:{}:{}:{}",
            self.key_prefix,
            resolution.redis_key(),
            scope.redis_key(),
            bucket_start
        )
    }

    pub fn daily_top_hits_key(&self, scope: &StatsScope, timestamp: u64) -> String {
        format!(
            "{}:stats:top:hits:{}:{}",
            self.key_prefix,
            scope.redis_key(),
            day_stamp(timestamp)
        )
    }

    pub fn daily_top_misses_key(&self, scope: &StatsScope, timestamp: u64) -> String {
        format!(
            "{}:stats:top:misses:{}:{}",
            self.key_prefix,
            scope.redis_key(),
            day_stamp(timestamp)
        )
    }

    pub fn daily_top_errors_key(&self, scope: &StatsScope, timestamp: u64) -> String {
        format!(
            "{}:stats:top:errors:{}:{}",
            self.key_prefix,
            scope.redis_key(),
            day_stamp(timestamp)
        )
    }
}

pub fn bucket_start(timestamp: u64, resolution: Resolution) -> u64 {
    let duration = resolution.duration_seconds();
    timestamp - (timestamp % duration)
}

fn day_stamp(timestamp: u64) -> String {
    Utc.timestamp_opt(timestamp as i64, 0)
        .single()
        .unwrap_or_else(|| Utc.timestamp_opt(0, 0).single().expect("unix epoch"))
        .format("%Y_%m_%d")
        .to_string()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn rounds_bucket_starts_for_each_resolution() {
        assert_eq!(bucket_start(1_711_753_499, Resolution::FiveMinutes), 1_711_753_200);
        assert_eq!(bucket_start(1_711_753_499, Resolution::OneHour), 1_711_753_200);
        assert_eq!(bucket_start(1_711_753_499, Resolution::OneDay), 1_711_670_400);
    }

    #[test]
    fn builds_scope_names_for_global_and_bucket_views() {
        assert_eq!(StatsScope::Global.redis_key(), "global");
        assert_eq!(StatsScope::Bucket("foo".to_string()).redis_key(), "bucket:foo");
    }

    #[test]
    fn computes_rates_with_safe_zero_denominators() {
        let totals = BucketTotals::default();

        assert_eq!(totals.cache_hit_rate(), 0.0);
        assert_eq!(totals.error_rate(), 0.0);
        assert_eq!(totals.qps(300), 0.0);
    }

    #[test]
    fn creates_stats_store_from_global_redis_config() {
        let redis = RedisConfig {
            redis_url: "redis://127.0.0.1:6379".to_string(),
            redis_key_prefix: "r2proxy".to_string(),
        };

        let store = StatsStore::new(&redis).unwrap();

        assert_eq!(store.key_prefix(), "r2proxy");
    }

    #[test]
    fn builds_expected_redis_keys() {
        let redis = RedisConfig {
            redis_url: "redis://127.0.0.1:6379".to_string(),
            redis_key_prefix: "r2proxy".to_string(),
        };
        let store = StatsStore::new(&redis).unwrap();
        let scope = StatsScope::Bucket("foo".to_string());

        assert_eq!(store.totals_key(&scope), "r2proxy:stats:totals:bucket:foo");
        assert_eq!(
            store.bucket_key(&scope, Resolution::FiveMinutes, 1_711_753_200),
            "r2proxy:stats:ts:5m:bucket:foo:1711753200"
        );
        assert_eq!(
            store.daily_top_errors_key(&scope, 1_711_753_499),
            "r2proxy:stats:top:errors:bucket:foo:2024_03_29"
        );
    }
}
