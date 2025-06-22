use crate::errors::ProxyError;
use aws_config::BehaviorVersion;
use aws_sdk_s3::{Client, Config};
use axum::body::Body;
use tracing::{debug, error, warn};

pub struct S3Response {
    pub body: Body,
    pub content_type: Option<axum::http::HeaderValue>,
    pub content_length: Option<u64>,
    pub etag: Option<String>,
    pub last_modified: Option<String>,
}

#[derive(Clone)]
pub struct S3Client {
    client: Client,
}

impl S3Client {
    pub async fn new(
        endpoint: &str,
        access_key_id: &str,
        secret_access_key: &str,
    ) -> Result<Self, ProxyError> {
        let config = aws_config::defaults(BehaviorVersion::latest())
            .region("auto")
            .credentials_provider(aws_sdk_s3::config::Credentials::new(
                access_key_id,
                secret_access_key,
                None,
                None,
                "static",
            ))
            .endpoint_url(endpoint)
            .load()
            .await;

        let s3_config = Config::new(&config);
        let client = Client::from_conf(s3_config);

        Ok(Self { client })
    }

    pub async fn get_object(&self, bucket: &str, key: &str) -> Result<S3Response, ProxyError> {
        debug!("Get object from S3: bucket={}, key={}", bucket, key);

        let output = self
            .client
            .get_object()
            .bucket(bucket)
            .key(key)
            .send()
            .await
            .map_err(|e| {
                warn!("S3 error: {}", e);
                ProxyError::S3Error(e.into())
            })?;

        // 获取元数据
        let content_type = output.content_type().map(|ct| {
            axum::http::HeaderValue::from_str(ct).unwrap_or_else(|_| {
                axum::http::HeaderValue::from_static("application/octet-stream")
            })
        });

        let content_length = output.content_length().map(|len| len as u64);
        let etag = output.e_tag().map(|s| s.to_string());
        let last_modified = output.last_modified().map(|dt| dt.to_string());

        // 创建响应体 - 直接读取所有数据
        let s3_body = output.body;
        let body = {
            // Read all data into memory
            let bytes = s3_body.collect().await.map_err(|e| {
                error!("S3 stream read error: {}", e);
                ProxyError::InternalError(format!("S3 stream read error: {}", e))
            })?;
            Body::from(bytes.into_bytes())
        };

        Ok(S3Response {
            body,
            content_type,
            content_length,
            etag,
            last_modified,
        })
    }
}
