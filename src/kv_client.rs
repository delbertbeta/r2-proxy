use crate::errors::ProxyError;
use reqwest::Client;
use tracing::{info, warn};

#[derive(Clone)]
pub struct KvClient {
    client: Client,
    account_id: String,
    api_token: String,
    namespace_id: String,
}

impl KvClient {
    pub fn new(account_id: &str, api_token: &str) -> Result<Self, ProxyError> {
        // Get namespace_id from env
        let namespace_id = std::env::var("CLOUDFLARE_KV_NAMESPACE_ID").map_err(|_| {
            ProxyError::ConfigError(crate::config::ConfigError::MissingEnvVar(
                "CLOUDFLARE_KV_NAMESPACE_ID".to_string(),
            ))
        })?;

        Ok(Self {
            client: Client::new(),
            account_id: account_id.to_string(),
            api_token: api_token.to_string(),
            namespace_id,
        })
    }

    pub async fn get_kv_value(&self, key: &str) -> Result<Option<String>, ProxyError> {
        let url = format!(
            "https://api.cloudflare.com/client/v4/accounts/{}/storage/kv/namespaces/{}/values/{}",
            self.account_id, self.namespace_id, key
        );
        info!(kv_key = key, "fetching kv value");

        let response = self
            .client
            .get(&url)
            .header("Authorization", format!("Bearer {}", self.api_token))
            .send()
            .await
            .map_err(|e| ProxyError::KvError(format!("KV request failed: {}", e)))?;

        if response.status().is_success() {
            info!(kv_key = key, status = %response.status(), "kv value fetched");
            let text = response
                .text()
                .await
                .map_err(|e| ProxyError::KvError(format!("Read response failed: {}", e)))?;
            Ok(Some(text))
        } else if response.status() == reqwest::StatusCode::NOT_FOUND {
            warn!(kv_key = key, "kv value not found");
            Ok(None)
        } else {
            let status = response.status();
            let text = response.text().await.unwrap_or_default();
            Err(ProxyError::KvError(format!(
                "KV API error: {} - {}",
                status, text
            )))
        }
    }

    pub fn namespace_id(&self) -> &str {
        &self.namespace_id
    }
}
