use axum::{
    http::StatusCode,
    response::{IntoResponse, Response},
};
use thiserror::Error;

#[derive(Error, Debug)]
pub enum ProxyError {
    #[error("Invalid path: {0}")]
    InvalidPath(String),

    #[error("Unauthorized bucket: {0}")]
    UnauthorizedBucket(String),

    #[error("Object not found: {0}")]
    ObjectNotFound(String),

    #[error("S3 error: {0}")]
    S3Error(#[from] aws_sdk_s3::Error),

    #[error("HTTP error: {0}")]
    HttpError(#[from] reqwest::Error),

    #[error("Cloudflare KV error: {0}")]
    KvError(String),

    #[error("Config error: {0}")]
    ConfigError(#[from] crate::config::ConfigError),

    #[error("Internal server error: {0}")]
    InternalError(String),
}

impl ProxyError {
    pub fn stats_result(&self) -> crate::stats::StatsResult {
        match self {
            Self::ObjectNotFound(_) => crate::stats::StatsResult::NotFound,
            Self::UnauthorizedBucket(_)
            | Self::S3Error(_)
            | Self::InvalidPath(_)
            | Self::HttpError(_)
            | Self::KvError(_)
            | Self::ConfigError(_)
            | Self::InternalError(_) => crate::stats::StatsResult::ServerError,
        }
    }

    pub fn stats_error_kind(&self) -> &'static str {
        match self {
            Self::UnauthorizedBucket(_) => "unauthorized_bucket",
            Self::ObjectNotFound(_) | Self::S3Error(_) => "origin",
            Self::InvalidPath(_)
            | Self::HttpError(_)
            | Self::KvError(_)
            | Self::ConfigError(_)
            | Self::InternalError(_) => "internal",
        }
    }
}

impl IntoResponse for ProxyError {
    fn into_response(self) -> Response {
        let (status, message) = match self {
            ProxyError::InvalidPath(msg) => (StatusCode::BAD_REQUEST, msg),
            ProxyError::UnauthorizedBucket(bucket) => (
                StatusCode::FORBIDDEN,
                format!("Access to bucket denied: {}", bucket),
            ),
            ProxyError::ObjectNotFound(key) => {
                (StatusCode::NOT_FOUND, format!("Object not found: {}", key))
            }
            ProxyError::S3Error(e) => {
                tracing::error!("S3 error: {}", e);
                (
                    StatusCode::INTERNAL_SERVER_ERROR,
                    "S3 service error".to_string(),
                )
            }
            ProxyError::HttpError(e) => {
                tracing::error!("HTTP error: {}", e);
                (
                    StatusCode::INTERNAL_SERVER_ERROR,
                    "HTTP request error".to_string(),
                )
            }
            ProxyError::KvError(msg) => {
                tracing::error!("KV error: {}", msg);
                (
                    StatusCode::INTERNAL_SERVER_ERROR,
                    "KV storage error".to_string(),
                )
            }
            ProxyError::ConfigError(e) => {
                tracing::error!("Config error: {}", e);
                (
                    StatusCode::INTERNAL_SERVER_ERROR,
                    "Config error".to_string(),
                )
            }
            ProxyError::InternalError(msg) => {
                tracing::error!("Internal error: {}", msg);
                (StatusCode::INTERNAL_SERVER_ERROR, msg)
            }
        };

        (status, message).into_response()
    }
}

#[cfg(test)]
mod tests {
    use super::ProxyError;
    use crate::stats::StatsResult;
    use axum::{body, http::StatusCode, response::IntoResponse};

    #[test]
    fn classifies_proxy_errors_for_metrics() {
        assert_eq!(
            ProxyError::UnauthorizedBucket("foo".to_string()).stats_error_kind(),
            "unauthorized_bucket"
        );
        assert_eq!(
            ProxyError::InternalError("boom".to_string()).stats_error_kind(),
            "internal"
        );
    }

    #[test]
    fn classifies_proxy_errors_for_stats_breakdown() {
        assert_eq!(
            ProxyError::ObjectNotFound("missing.txt".to_string()).stats_result(),
            StatsResult::NotFound
        );
        assert_eq!(
            ProxyError::InternalError("boom".to_string()).stats_result(),
            StatsResult::ServerError
        );
    }

    #[tokio::test]
    async fn maps_missing_origin_objects_to_not_found() {
        let response = ProxyError::ObjectNotFound("missing.txt".to_string()).into_response();

        assert_eq!(response.status(), StatusCode::NOT_FOUND);

        let body = body::to_bytes(response.into_body(), usize::MAX)
            .await
            .expect("body should be readable");
        assert_eq!(&body[..], b"Object not found: missing.txt");
    }
}
