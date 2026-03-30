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

    pub fn redis_client(&self) -> &redis::Client {
        &self.redis_client
    }
}

pub fn bucket_start(timestamp: u64, resolution: Resolution) -> u64 {
    let duration = resolution.duration_seconds();
    timestamp - (timestamp % duration)
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
        let _ = store.redis_client();
    }
}
use crate::config::RedisConfig;
