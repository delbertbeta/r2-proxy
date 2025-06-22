use std::env;
use thiserror::Error;

#[derive(Error, Debug)]
pub enum ConfigError {
    #[error("Missing env var: {0}")]
    MissingEnvVar(String),
    #[error("Port parse failed: {0}")]
    InvalidPort(String),
}

#[derive(Clone, Debug)]
pub struct Config {
    pub port: u16,
    pub cloudflare_account_id: String,
    pub cloudflare_api_token: String,
    pub r2_endpoint: String,
    pub r2_access_key_id: String,
    pub r2_secret_access_key: String,
}

impl Config {
    pub fn from_env() -> Result<Self, ConfigError> {
        Ok(Self {
            port: env::var("PORT")
                .unwrap_or_else(|_| "3000".to_string())
                .parse::<u16>()
                .map_err(|e| ConfigError::InvalidPort(e.to_string()))?,
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
        })
    }
} 