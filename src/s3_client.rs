use crate::errors::ProxyError;
use crate::local_cache::CachedHeaders;
use aws_config::BehaviorVersion;
use aws_sdk_s3::operation::get_object::GetObjectError;
use aws_sdk_s3::primitives::ByteStream;
use aws_sdk_s3::{Client, Config};
use aws_smithy_runtime_api::client::result::SdkError;
use tracing::{info, warn};

pub struct S3Response {
    pub body: ByteStream,
    pub headers: CachedHeaders,
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
        info!(bucket = bucket, key = key, "fetching object from s3");

        let output = self
            .client
            .get_object()
            .bucket(bucket)
            .key(key)
            .send()
            .await
            .map_err(|e| {
                warn!("S3 error: {}", e);
                map_get_object_error(key, e)
            })?;

        // 获取元数据
        let content_type = output.content_type().map(|ct| {
            if ct.is_empty() {
                "application/octet-stream".to_string()
            } else {
                ct.to_string()
            }
        });

        let content_length = output.content_length().map(|len| len as u64);
        let etag = output.e_tag().map(|s| s.to_string());
        let last_modified = output.last_modified().map(|dt| dt.to_string());
        info!(
            bucket = bucket,
            key = key,
            content_length = ?content_length,
            content_type = ?content_type,
            etag = ?etag,
            "s3 object fetched successfully"
        );

        Ok(S3Response {
            body: output.body,
            headers: CachedHeaders {
                content_type,
                content_length,
                etag,
                last_modified,
            },
        })
    }
}

fn map_get_object_error<R>(
    key: &str,
    error: SdkError<GetObjectError, R>,
) -> ProxyError
where
    R: Send + Sync + std::fmt::Debug + 'static,
{
    if error
        .as_service_error()
        .is_some_and(GetObjectError::is_no_such_key)
    {
        return ProxyError::ObjectNotFound(key.to_string());
    }

    ProxyError::S3Error(error.into())
}

#[cfg(test)]
mod tests {
    use super::map_get_object_error;
    use crate::errors::ProxyError;
    use aws_sdk_s3::operation::get_object::GetObjectError;
    use aws_sdk_s3::types::error::NoSuchKey;
    use aws_smithy_runtime_api::client::result::SdkError;
    use aws_smithy_runtime_api::http::{Response, StatusCode};
    use aws_smithy_types::body::SdkBody;

    #[test]
    fn maps_missing_s3_objects_to_not_found_proxy_error() {
        let sdk_error = SdkError::service_error(
            GetObjectError::NoSuchKey(NoSuchKey::builder().message("missing").build()),
            Response::new(StatusCode::try_from(404).expect("valid status"), SdkBody::empty()),
        );

        let proxy_error = map_get_object_error("missing.txt", sdk_error);

        assert!(matches!(
            proxy_error,
            ProxyError::ObjectNotFound(key) if key == "missing.txt"
        ));
    }
}
