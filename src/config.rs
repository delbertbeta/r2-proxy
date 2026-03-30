use std::env;
use thiserror::Error;

#[derive(Error, Debug)]
pub enum ConfigError {
    #[error("Missing env var: {0}")]
    MissingEnvVar(String),
    #[error("Port parse failed: {0}")]
    InvalidPort(String),
    #[error("Invalid cache size: {0}")]
    InvalidCacheSize(String),
}

#[derive(Clone, Debug)]
pub struct LocalCacheConfig {
    pub enabled: bool,
    pub max_size_bytes: u64,
    pub directory: String,
}

#[derive(Clone, Debug)]
pub struct RedisConfig {
    pub redis_url: String,
    pub redis_key_prefix: String,
}

#[derive(Clone, Debug)]
pub struct StatusConfig {
    pub host: String,
    pub port: u16,
    pub api_key: String,
}

#[derive(Clone, Debug)]
pub struct Config {
    pub port: u16,
    pub status: StatusConfig,
    pub redis: RedisConfig,
    pub cloudflare_account_id: String,
    pub cloudflare_api_token: String,
    pub r2_endpoint: String,
    pub r2_access_key_id: String,
    pub r2_secret_access_key: String,
    pub local_cache: Option<LocalCacheConfig>,
}

impl Config {
    pub fn from_env() -> Result<Self, ConfigError> {
        let local_cache_enabled = env::var("LOCAL_CACHE_ENABLED")
            .ok()
            .map(|value| {
                matches!(
                    value.trim().to_ascii_lowercase().as_str(),
                    "1" | "true" | "yes" | "on"
                )
            })
            .unwrap_or(false);
        let redis = RedisConfig {
            redis_url: env::var("REDIS_URL")
                .map_err(|_| ConfigError::MissingEnvVar("REDIS_URL".to_string()))?,
            redis_key_prefix: env::var("REDIS_KEY_PREFIX")
                .unwrap_or_else(|_| "r2proxy".to_string()),
        };

        Ok(Self {
            port: env::var("PORT")
                .unwrap_or_else(|_| "3000".to_string())
                .parse::<u16>()
                .map_err(|e| ConfigError::InvalidPort(e.to_string()))?,
            status: StatusConfig {
                host: env::var("STATUS_HOST").unwrap_or_else(|_| "127.0.0.1".to_string()),
                port: env::var("STATUS_PORT")
                    .unwrap_or_else(|_| "3001".to_string())
                    .parse::<u16>()
                    .map_err(|e| ConfigError::InvalidPort(e.to_string()))?,
                api_key: env::var("STATUS_API_KEY")
                    .map_err(|_| ConfigError::MissingEnvVar("STATUS_API_KEY".to_string()))?,
            },
            redis,
            cloudflare_account_id: env::var("CLOUDFLARE_ACCOUNT_ID")
                .map_err(|_| ConfigError::MissingEnvVar("CLOUDFLARE_ACCOUNT_ID".to_string()))?,
            cloudflare_api_token: env::var("CLOUDFLARE_API_TOKEN")
                .map_err(|_| ConfigError::MissingEnvVar("CLOUDFLARE_API_TOKEN".to_string()))?,
            r2_endpoint: env::var("R2_ENDPOINT")
                .map_err(|_| ConfigError::MissingEnvVar("R2_ENDPOINT".to_string()))?,
            r2_access_key_id: env::var("R2_ACCESS_KEY_ID")
                .map_err(|_| ConfigError::MissingEnvVar("R2_ACCESS_KEY_ID".to_string()))?,
            r2_secret_access_key: env::var("R2_SECRET_ACCESS_KEY")
                .map_err(|_| ConfigError::MissingEnvVar("R2_SECRET_ACCESS_KEY".to_string()))?,
            local_cache: if local_cache_enabled {
                Some(LocalCacheConfig {
                    enabled: true,
                    max_size_bytes: parse_size(&env::var("LOCAL_CACHE_MAX_SIZE").map_err(
                        |_| ConfigError::MissingEnvVar("LOCAL_CACHE_MAX_SIZE".to_string()),
                    )?)?,
                    directory: env::var("LOCAL_CACHE_DIR")
                        .map_err(|_| ConfigError::MissingEnvVar("LOCAL_CACHE_DIR".to_string()))?,
                })
            } else {
                None
            },
        })
    }
}

fn parse_size(input: &str) -> Result<u64, ConfigError> {
    let trimmed = input.trim();
    if trimmed.is_empty() {
        return Err(ConfigError::InvalidCacheSize(input.to_string()));
    }

    let split_index = trimmed
        .find(|ch: char| !ch.is_ascii_digit())
        .unwrap_or(trimmed.len());
    let (digits, suffix) = trimmed.split_at(split_index);

    let base = digits
        .parse::<u64>()
        .map_err(|_| ConfigError::InvalidCacheSize(input.to_string()))?;

    let multiplier = match suffix.trim().to_ascii_uppercase().as_str() {
        "" | "B" => 1,
        "K" | "KB" => 1024,
        "M" | "MB" => 1024 * 1024,
        "G" | "GB" => 1024 * 1024 * 1024,
        _ => return Err(ConfigError::InvalidCacheSize(input.to_string())),
    };

    base.checked_mul(multiplier)
        .ok_or_else(|| ConfigError::InvalidCacheSize(input.to_string()))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn set_base_env() {
        unsafe {
            env::set_var("PORT", "3000");
            env::set_var("STATUS_PORT", "3001");
            env::set_var("STATUS_HOST", "127.0.0.1");
            env::set_var("STATUS_API_KEY", "status-key");
            env::set_var("REDIS_URL", "redis://127.0.0.1:6379");
            env::set_var("REDIS_KEY_PREFIX", "r2proxy");
            env::set_var("CLOUDFLARE_ACCOUNT_ID", "account");
            env::set_var("CLOUDFLARE_API_TOKEN", "token");
            env::set_var("R2_ENDPOINT", "https://example.r2.cloudflarestorage.com");
            env::set_var("R2_ACCESS_KEY_ID", "key");
            env::set_var("R2_SECRET_ACCESS_KEY", "secret");
        }
    }

    #[test]
    fn config_reads_status_server_settings() {
        set_base_env();
        unsafe {
            env::set_var("STATUS_PORT", "3009");
            env::set_var("STATUS_HOST", "0.0.0.0");
            env::set_var("STATUS_API_KEY", "secret-status-key");
        }

        let config = Config::from_env().unwrap();

        assert_eq!(config.status.port, 3009);
        assert_eq!(config.status.host, "0.0.0.0");
        assert_eq!(config.status.api_key, "secret-status-key");
    }

    #[test]
    fn config_uses_safe_status_defaults() {
        set_base_env();
        unsafe {
            env::remove_var("STATUS_PORT");
            env::remove_var("STATUS_HOST");
            env::set_var("STATUS_API_KEY", "secret-status-key");
        }

        let config = Config::from_env().unwrap();

        assert_eq!(config.status.port, 3001);
        assert_eq!(config.status.host, "127.0.0.1");
    }

    #[test]
    fn config_reads_global_redis_settings_without_local_cache() {
        set_base_env();
        unsafe {
            env::set_var("REDIS_URL", "redis://cache.internal:6379");
            env::set_var("REDIS_KEY_PREFIX", "status");
            env::remove_var("LOCAL_CACHE_ENABLED");
        }

        let config = Config::from_env().unwrap();

        assert_eq!(config.redis.redis_url, "redis://cache.internal:6379");
        assert_eq!(config.redis.redis_key_prefix, "status");
        assert!(config.local_cache.is_none());
    }

    #[test]
    fn parses_human_readable_local_cache_size() {
        assert_eq!(parse_size("512M").unwrap(), 512 * 1024 * 1024);
        assert_eq!(parse_size("1G").unwrap(), 1024 * 1024 * 1024);
        assert_eq!(parse_size("1024K").unwrap(), 1024 * 1024);
    }

    #[test]
    fn config_reads_optional_local_cache_and_redis_settings() {
        set_base_env();
        unsafe {
            env::set_var("LOCAL_CACHE_ENABLED", "true");
            env::set_var("LOCAL_CACHE_MAX_SIZE", "512M");
            env::set_var("LOCAL_CACHE_DIR", "/tmp/r2-proxy");
            env::set_var("REDIS_URL", "redis://127.0.0.1:6379");
            env::set_var("REDIS_KEY_PREFIX", "custom");
        }

        let config = Config::from_env().unwrap();
        let local_cache = config.local_cache.expect("local cache config");

        assert!(local_cache.enabled);
        assert_eq!(local_cache.max_size_bytes, 512 * 1024 * 1024);
        assert_eq!(local_cache.directory, "/tmp/r2-proxy");
        assert_eq!(config.redis.redis_url, "redis://127.0.0.1:6379");
        assert_eq!(config.redis.redis_key_prefix, "custom");
    }
}
