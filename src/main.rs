use axum::{
    http::{Method, StatusCode, Uri},
    response::{IntoResponse, Response},
    routing::{get, options},
    Router,
};
use std::net::SocketAddr;
use tower_http::cors::{Any, CorsLayer};
use tracing::{info, warn};
use std::collections::HashMap;
use std::sync::Arc;
use tokio::sync::RwLock;
use serde_json;

mod config;
mod cors;
mod errors;
mod kv_client;
mod s3_client;

use config::Config;
use errors::ProxyError;
use kv_client::KvClient;
use s3_client::S3Client;

struct AppCache {
    whitelist: HashMap<String, String>, // 虚拟名 -> 真实 bucket
    cors_config: HashMap<String, Option<cors::CorsConfig>>,
}

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    // Initialize logger
    tracing_subscriber::fmt::init();
    
    // Load config
    dotenv::dotenv().ok();
    let config = Config::from_env()?;
    
    info!("Start R2 proxy server, listen port: {}", config.port);
    
    // Initialize client
    let kv_client = KvClient::new(&config.cloudflare_account_id, &config.cloudflare_api_token)?;
    let s3_client = S3Client::new(&config.r2_endpoint, &config.r2_access_key_id, &config.r2_secret_access_key).await?;
    
    // Initialize cache
    let cache = Arc::new(RwLock::new(AppCache {
        whitelist: HashMap::new(),
        cors_config: HashMap::new(),
    }));
    
    // 初始化缓存（同步拉取一次）
    refresh_cache(&kv_client, &cache).await?;

    // Create CORS middleware
    let cors = CorsLayer::new()
        .allow_methods([Method::GET, Method::OPTIONS])
        .allow_origin(Any)
        .allow_headers(Any);
    
    // Create routes
    let app = Router::new()
        .route("/", get(proxy_handler))
        .route("/", options(handle_options))
        .route("/*path", get(proxy_handler))
        .route("/*path", options(handle_options))
        .layer(cors)
        .with_state(AppState {
            s3_client,
            cache: cache.clone(),
        });
    
    // Start refresh task
    let kv_client_clone = kv_client.clone();
    let cache_clone = cache.clone();
    tokio::spawn(async move {
        loop {
            tokio::time::sleep(std::time::Duration::from_secs(600)).await;
            if let Err(e) = refresh_cache(&kv_client_clone, &cache_clone).await {
                tracing::error!("Refresh cache failed: {}", e);
            }
        }
    });

    // Start server
    let addr = SocketAddr::from(([0, 0, 0, 0], config.port));
    info!("Server started at http://{}", addr);

    let listener = tokio::net::TcpListener::bind(addr).await?;
    axum::serve(listener, app).await?;
    
    Ok(())
}

#[derive(Clone)]
struct AppState {
    s3_client: S3Client,
    cache: Arc<RwLock<AppCache>>,
}

async fn proxy_handler(
    uri: Uri,
    axum::extract::State(state): axum::extract::State<AppState>,
    axum::extract::Host(host): axum::extract::Host,
) -> Result<Response, ProxyError> {
    // 解析 bucket 从 host
    // 例如 bucket.delbertbeta.life 或 delbertbeta.life
    let bucket = if let Some((prefix, rest)) = host.split_once('.') {
        if rest == "delbertbeta.life" {
            prefix
        } else {
            "@"
        }
    } else if host == "delbertbeta.life" {
        "@"
    } else {
        "@"
    };
    if bucket.is_empty() {
        return Err(ProxyError::InvalidPath("Bucket not found in host".to_string()));
    }
    let path = uri.path();
    let object_key = {
        let mut key = path.trim_start_matches('/').to_string();
        if key.is_empty() || key.ends_with('/') {
            key.push_str("index.html");
        }
        key
    };
    info!("Proxy request: bucket={}, key={}", bucket, object_key);
    
    // 检查 bucket 是否在白名单中
    let real_bucket = {
        let cache = state.cache.read().await;
        cache.whitelist.get(bucket).cloned()
    };
    let real_bucket = match real_bucket {
        Some(b) if !b.is_empty() => b,
        _ => {
            warn!("Access to unauthorized bucket denied: {}", bucket);
            return Err(ProxyError::UnauthorizedBucket(bucket.to_string()));
        }
    };
    
    // Get CORS configuration
    let cors_config = {
        let cache = state.cache.read().await;
        cache.cors_config.get(bucket).cloned().unwrap_or(None)
    };
    
    // Get object from S3
    let s3_response = state.s3_client.get_object(&real_bucket, &object_key).await?;
    
    // Build response
    let mut response = Response::new(s3_response.body);
    
    // Set status code
    *response.status_mut() = StatusCode::OK;
    
    // Set content type
    if let Some(content_type) = s3_response.content_type {
        response.headers_mut().insert("content-type", content_type);
    }
    // Set content length
    if let Some(content_length) = s3_response.content_length {
        response.headers_mut().insert(
            axum::http::header::CONTENT_LENGTH,
            axum::http::HeaderValue::from_str(&content_length.to_string()).unwrap(),
        );
    }
    // Set ETag
    if let Some(etag) = s3_response.etag {
        response.headers_mut().insert(
            axum::http::header::ETAG,
            axum::http::HeaderValue::from_str(&etag).unwrap(),
        );
    }
    // Set Last-Modified
    if let Some(last_modified) = s3_response.last_modified {
        response.headers_mut().insert(
            axum::http::header::LAST_MODIFIED,
            axum::http::HeaderValue::from_str(&last_modified).unwrap(),
        );
    }
    // Set Cache-Control: public, max-age=7200 (120 min)
    response.headers_mut().insert(
        axum::http::header::CACHE_CONTROL,
        axum::http::HeaderValue::from_static("public, max-age=7200"),
    );
    
    // Set custom server header
    response.headers_mut().insert(
        "X-Served-By",
        axum::http::HeaderValue::from_static("r2-proxy"),
    );
    
    // Set CORS headers
    if let Some(cors) = cors_config {
        cors.apply_headers(response.headers_mut());
    }
    
    Ok(response)
}

async fn handle_options() -> impl IntoResponse {
    StatusCode::OK
}

async fn refresh_cache(kv_client: &KvClient, cache: &Arc<RwLock<AppCache>>) -> Result<(), anyhow::Error> {
    // 读取 whitelist
    let whitelist_value = kv_client.get_kv_value("whitelist").await.ok().flatten();
    let mut whitelist = HashMap::new();
    if let Some(val) = whitelist_value {
        if let Ok(list) = serde_json::from_str::<Vec<(String, String)>>(&val) {
            for (prefix, bucket) in list {
                if !prefix.trim().is_empty() && !bucket.trim().is_empty() {
                    whitelist.insert(prefix.trim().to_string(), bucket.trim().to_string());
                }
            }
        }
    }
    // 读取 cors
    let cors_value = kv_client.get_kv_value("cors").await.ok().flatten();
    let mut cors_config = HashMap::new();
    if let Some(val) = cors_value {
        if let Ok(map) = serde_json::from_str::<HashMap<String, cors::CorsConfig>>(&val) {
            for (bucket, config) in map {
                cors_config.insert(bucket, Some(config));
            }
        }
    }
    let mut cache_guard = cache.write().await;
    cache_guard.whitelist = whitelist;
    cache_guard.cors_config = cors_config;
    Ok(())
}
