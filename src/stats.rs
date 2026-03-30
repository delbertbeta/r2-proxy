use chrono::{TimeZone, Utc};
use redis::AsyncCommands;
use std::collections::HashMap;
use std::time::{SystemTime, UNIX_EPOCH};
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
    pub not_found_errors: u64,
    pub server_errors: u64,
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

    pub fn not_found_error_rate(self) -> f64 {
        if self.requests == 0 {
            0.0
        } else {
            self.not_found_errors as f64 / self.requests as f64
        }
    }

    pub fn server_error_rate(self) -> f64 {
        if self.requests == 0 {
            0.0
        } else {
            self.server_errors as f64 / self.requests as f64
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
    NotFound,
    ServerError,
}

impl StatsResult {
    pub fn is_error(self) -> bool {
        !matches!(self, Self::Success)
    }

    pub fn is_not_found(self) -> bool {
        matches!(self, Self::NotFound)
    }

    pub fn is_server_error(self) -> bool {
        matches!(self, Self::ServerError)
    }
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

    pub async fn read_totals(&self, scope: &StatsScope) -> Result<BucketTotals, redis::RedisError> {
        let mut connection = self.redis_client.get_multiplexed_async_connection().await?;
        let values: HashMap<String, u64> = connection.hgetall(self.totals_key(scope)).await?;
        Ok(bucket_totals_from_hash(values))
    }

    pub async fn read_series(
        &self,
        scope: &StatsScope,
        resolution: Resolution,
        points: usize,
        end_timestamp: u64,
    ) -> Result<Vec<(u64, BucketTotals)>, redis::RedisError> {
        let mut connection = self.redis_client.get_multiplexed_async_connection().await?;
        let step = resolution.duration_seconds();
        let end_bucket = bucket_start(end_timestamp, resolution);
        let start_bucket =
            end_bucket.saturating_sub(step.saturating_mul(points.saturating_sub(1) as u64));
        let mut series = Vec::with_capacity(points);

        for index in 0..points {
            let ts = start_bucket + (index as u64 * step);
            let values: HashMap<String, u64> = connection
                .hgetall(self.bucket_key(scope, resolution, ts))
                .await?;
            series.push((ts, bucket_totals_from_hash(values)));
        }

        Ok(series)
    }

    pub async fn read_top_hits(
        &self,
        scope: &StatsScope,
        end_timestamp: u64,
        limit: usize,
    ) -> Result<Vec<(String, u64)>, redis::RedisError> {
        self.read_recent_top_entries("hits", scope, end_timestamp, limit)
            .await
    }

    pub async fn read_top_misses(
        &self,
        scope: &StatsScope,
        end_timestamp: u64,
        limit: usize,
    ) -> Result<Vec<(String, u64)>, redis::RedisError> {
        self.read_recent_top_entries("misses", scope, end_timestamp, limit)
            .await
    }

    pub async fn read_top_not_found_errors(
        &self,
        scope: &StatsScope,
        end_timestamp: u64,
        limit: usize,
    ) -> Result<Vec<(String, u64)>, redis::RedisError> {
        self.read_recent_top_entries("errors_404", scope, end_timestamp, limit)
            .await
    }

    pub async fn read_top_server_errors(
        &self,
        scope: &StatsScope,
        end_timestamp: u64,
        limit: usize,
    ) -> Result<Vec<(String, u64)>, redis::RedisError> {
        self.read_recent_top_entries("errors_5xx", scope, end_timestamp, limit)
            .await
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
                .arg(if event.cache_status.counts_as_hit() {
                    1
                } else {
                    0
                })
                .cmd("HINCRBY")
                .arg(&totals_key)
                .arg("cache_misses")
                .arg(if event.cache_status.counts_as_miss() {
                    1
                } else {
                    0
                })
                .cmd("HINCRBY")
                .arg(&totals_key)
                .arg("errors")
                .arg(if event.result.is_error() { 1 } else { 0 })
                .cmd("HINCRBY")
                .arg(&totals_key)
                .arg("errors_404")
                .arg(if event.result.is_not_found() { 1 } else { 0 })
                .cmd("HINCRBY")
                .arg(&totals_key)
                .arg("errors_5xx")
                .arg(if event.result.is_server_error() { 1 } else { 0 })
                .query_async(&mut connection)
                .await?;

            for resolution in [
                Resolution::FiveMinutes,
                Resolution::OneHour,
                Resolution::OneDay,
            ] {
                let series_key = self.bucket_key(
                    &scope,
                    resolution,
                    bucket_start(event.timestamp, resolution),
                );
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
                    .arg(if event.cache_status.counts_as_hit() {
                        1
                    } else {
                        0
                    })
                    .cmd("HINCRBY")
                    .arg(&series_key)
                    .arg("cache_misses")
                    .arg(if event.cache_status.counts_as_miss() {
                        1
                    } else {
                        0
                    })
                    .cmd("HINCRBY")
                    .arg(&series_key)
                    .arg("errors")
                    .arg(if event.result.is_error() { 1 } else { 0 })
                    .cmd("HINCRBY")
                    .arg(&series_key)
                    .arg("errors_404")
                    .arg(if event.result.is_not_found() { 1 } else { 0 })
                    .cmd("HINCRBY")
                    .arg(&series_key)
                    .arg("errors_5xx")
                    .arg(if event.result.is_server_error() { 1 } else { 0 })
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
                        let _: bool = connection
                            .expire(&key, Resolution::OneDay.ttl_seconds())
                            .await?;
                    }
                }
                StatsResult::Success if event.cache_status.counts_as_miss() => {
                    let key = self.daily_top_misses_key(&scope, event.timestamp);
                    let member = format!("{}|{}", event.bucket, event.path_and_query);
                    let _: f64 = connection.zincr(&key, member, 1).await?;
                    let _: bool = connection
                        .expire(&key, Resolution::OneDay.ttl_seconds())
                        .await?;
                }
                result if result.is_not_found() => {
                    let key = self.daily_top_not_found_errors_key(&scope, event.timestamp);
                    let member = format!("{}|{}", event.bucket, event.path_and_query);
                    let _: f64 = connection.zincr(&key, member, 1).await?;
                    let _: bool = connection
                        .expire(&key, Resolution::OneDay.ttl_seconds())
                        .await?;
                }
                result if result.is_server_error() => {
                    let key = self.daily_top_server_errors_key(&scope, event.timestamp);
                    let member = format!("{}|{}", event.bucket, event.path_and_query);
                    let _: f64 = connection.zincr(&key, member, 1).await?;
                    let _: bool = connection
                        .expire(&key, Resolution::OneDay.ttl_seconds())
                        .await?;
                }
                _ => {}
            }
        }

        Ok(())
    }

    async fn read_recent_top_entries(
        &self,
        metric: &str,
        scope: &StatsScope,
        end_timestamp: u64,
        limit: usize,
    ) -> Result<Vec<(String, u64)>, redis::RedisError> {
        let mut connection = self.redis_client.get_multiplexed_async_connection().await?;
        let daily_keys = recent_daily_top_keys(&self.key_prefix, metric, scope, end_timestamp);
        let temporary_key = format!(
            "{}:stats:temp:{}:{}:{}",
            self.key_prefix,
            metric,
            scope.redis_key(),
            unique_suffix()
        );

        let mut union_command = redis::cmd("ZUNIONSTORE");
        union_command.arg(&temporary_key).arg(daily_keys.len());
        for key in &daily_keys {
            union_command.arg(key);
        }
        let _: i64 = union_command.query_async(&mut connection).await?;
        let _: bool = connection.expire(&temporary_key, 30).await?;
        let ranked: Vec<(String, u64)> = redis::cmd("ZREVRANGE")
            .arg(&temporary_key)
            .arg(0)
            .arg(limit.saturating_sub(1) as isize)
            .arg("WITHSCORES")
            .query_async(&mut connection)
            .await?;
        let _: () = connection.del(&temporary_key).await?;
        Ok(ranked)
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

    pub fn daily_top_not_found_errors_key(&self, scope: &StatsScope, timestamp: u64) -> String {
        format!(
            "{}:stats:top:errors_404:{}:{}",
            self.key_prefix,
            scope.redis_key(),
            day_stamp(timestamp)
        )
    }

    pub fn daily_top_server_errors_key(&self, scope: &StatsScope, timestamp: u64) -> String {
        format!(
            "{}:stats:top:errors_5xx:{}:{}",
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

fn bucket_totals_from_hash(values: HashMap<String, u64>) -> BucketTotals {
    BucketTotals {
        requests: values.get("requests").copied().unwrap_or(0),
        bytes: values.get("bytes").copied().unwrap_or(0),
        cache_hits: values.get("cache_hits").copied().unwrap_or(0),
        cache_misses: values.get("cache_misses").copied().unwrap_or(0),
        errors: values.get("errors").copied().unwrap_or(0),
        not_found_errors: values.get("errors_404").copied().unwrap_or(0),
        server_errors: values.get("errors_5xx").copied().unwrap_or(0),
    }
}

fn recent_daily_top_keys(
    prefix: &str,
    metric: &str,
    scope: &StatsScope,
    end_timestamp: u64,
) -> Vec<String> {
    let end_day = Utc
        .timestamp_opt(end_timestamp as i64, 0)
        .single()
        .unwrap_or_else(|| Utc.timestamp_opt(0, 0).single().expect("unix epoch"))
        .date_naive();

    (0..7)
        .map(|offset| {
            let day = end_day - chrono::Days::new(offset);
            format!(
                "{prefix}:stats:top:{metric}:{}:{}",
                scope.redis_key(),
                day.format("%Y_%m_%d")
            )
        })
        .collect()
}

fn unique_suffix() -> u128 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_nanos()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn rounds_bucket_starts_for_each_resolution() {
        assert_eq!(
            bucket_start(1_711_753_499, Resolution::FiveMinutes),
            1_711_753_200
        );
        assert_eq!(
            bucket_start(1_711_753_499, Resolution::OneHour),
            1_711_753_200
        );
        assert_eq!(
            bucket_start(1_711_753_499, Resolution::OneDay),
            1_711_670_400
        );
    }

    #[test]
    fn builds_scope_names_for_global_and_bucket_views() {
        assert_eq!(StatsScope::Global.redis_key(), "global");
        assert_eq!(
            StatsScope::Bucket("foo".to_string()).redis_key(),
            "bucket:foo"
        );
    }

    #[test]
    fn computes_rates_with_safe_zero_denominators() {
        let totals = BucketTotals::default();

        assert_eq!(totals.cache_hit_rate(), 0.0);
        assert_eq!(totals.error_rate(), 0.0);
        assert_eq!(totals.not_found_error_rate(), 0.0);
        assert_eq!(totals.server_error_rate(), 0.0);
        assert_eq!(totals.qps(300), 0.0);
    }

    #[test]
    fn stats_results_report_404_and_5xx_buckets() {
        assert!(StatsResult::NotFound.is_error());
        assert!(StatsResult::NotFound.is_not_found());
        assert!(!StatsResult::NotFound.is_server_error());

        assert!(StatsResult::ServerError.is_error());
        assert!(!StatsResult::ServerError.is_not_found());
        assert!(StatsResult::ServerError.is_server_error());

        assert!(!StatsResult::Success.is_error());
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
        assert_eq!(
            store.daily_top_not_found_errors_key(&scope, 1_711_753_499),
            "r2proxy:stats:top:errors_404:bucket:foo:2024_03_29"
        );
        assert_eq!(
            store.daily_top_server_errors_key(&scope, 1_711_753_499),
            "r2proxy:stats:top:errors_5xx:bucket:foo:2024_03_29"
        );
    }

    #[test]
    fn builds_recent_daily_top_key_window() {
        let keys = recent_daily_top_keys(
            "r2proxy",
            "errors",
            &StatsScope::Bucket("foo".to_string()),
            1_711_753_499,
        );

        assert_eq!(keys.len(), 7);
        assert_eq!(keys[0], "r2proxy:stats:top:errors:bucket:foo:2024_03_29");
        assert_eq!(keys[6], "r2proxy:stats:top:errors:bucket:foo:2024_03_23");
    }

    #[test]
    fn bucket_totals_defaults_new_error_counters_to_zero() {
        let totals = bucket_totals_from_hash(HashMap::new());

        assert_eq!(totals.errors, 0);
        assert_eq!(totals.not_found_errors, 0);
        assert_eq!(totals.server_errors, 0);
    }

    #[test]
    fn builds_daily_top_error_keys_for_404_and_5xx() {
        let redis = RedisConfig {
            redis_url: "redis://127.0.0.1:6379".to_string(),
            redis_key_prefix: "r2proxy".to_string(),
        };
        let store = StatsStore::new(&redis).expect("stats store");
        let scope = StatsScope::Bucket("foo".to_string());

        assert_eq!(
            store.daily_top_not_found_errors_key(&scope, 1_711_753_499),
            "r2proxy:stats:top:errors_404:bucket:foo:2024_03_29"
        );
        assert_eq!(
            store.daily_top_server_errors_key(&scope, 1_711_753_499),
            "r2proxy:stats:top:errors_5xx:bucket:foo:2024_03_29"
        );
    }
}
