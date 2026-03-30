use async_stream::try_stream;
use axum::{
    body::{Body, Bytes},
    http::{HeaderMap, Method, StatusCode, Uri},
    response::{IntoResponse, Response},
    routing::{get, options},
    Router,
};
use serde_json;
use std::collections::HashMap;
use std::env;
use std::io;
use std::net::SocketAddr;
use std::sync::{
    atomic::{AtomicU64, Ordering},
    Arc,
};
use tokio::sync::RwLock;
use tower_http::cors::{Any, CorsLayer};
use tracing::{error, info, warn};
use tracing_subscriber::EnvFilter;

mod config;
mod cors;
mod errors;
mod kv_client;
mod local_cache;
mod s3_client;

use config::Config;
use errors::ProxyError;
use kv_client::KvClient;
use local_cache::{CacheStatus, LocalCache, PendingCacheWrite};
use s3_client::S3Client;

struct AppCache {
    whitelist: HashMap<String, String>, // 虚拟名 -> 真实 bucket
    cors_config: HashMap<String, Option<cors::CorsConfig>>,
    spa: HashMap<String, bool>,
}

#[derive(Default)]
struct AppMetrics {
    requests_total: AtomicU64,
    cache_hit_total: AtomicU64,
    cache_miss_total: AtomicU64,
    cache_bypass_total: AtomicU64,
    cache_disabled_total: AtomicU64,
    bytes_served_total: AtomicU64,
    bytes_from_cache_total: AtomicU64,
    bytes_from_origin_total: AtomicU64,
}

#[derive(Debug, PartialEq)]
struct MetricsSnapshot {
    requests_total: u64,
    cache_hit_total: u64,
    cache_miss_total: u64,
    cache_bypass_total: u64,
    cache_disabled_total: u64,
    bytes_served_total: u64,
    bytes_from_cache_total: u64,
    bytes_from_origin_total: u64,
}

#[derive(Clone)]
struct RequestContext {
    host: String,
    virtual_bucket: String,
    real_bucket: String,
    object_key: String,
}

impl AppMetrics {
    fn record(&self, status: CacheStatus, bytes_served: u64) {
        self.requests_total.fetch_add(1, Ordering::Relaxed);
        self.bytes_served_total
            .fetch_add(bytes_served, Ordering::Relaxed);

        match status {
            CacheStatus::Hit => {
                self.cache_hit_total.fetch_add(1, Ordering::Relaxed);
                self.bytes_from_cache_total
                    .fetch_add(bytes_served, Ordering::Relaxed);
            }
            CacheStatus::Miss => {
                self.cache_miss_total.fetch_add(1, Ordering::Relaxed);
                self.bytes_from_origin_total
                    .fetch_add(bytes_served, Ordering::Relaxed);
            }
            CacheStatus::Bypass => {
                self.cache_bypass_total.fetch_add(1, Ordering::Relaxed);
                self.bytes_from_origin_total
                    .fetch_add(bytes_served, Ordering::Relaxed);
            }
            CacheStatus::Disabled => {
                self.cache_disabled_total.fetch_add(1, Ordering::Relaxed);
                self.bytes_from_origin_total
                    .fetch_add(bytes_served, Ordering::Relaxed);
            }
        }
    }

    fn snapshot(&self) -> MetricsSnapshot {
        MetricsSnapshot {
            requests_total: self.requests_total.load(Ordering::Relaxed),
            cache_hit_total: self.cache_hit_total.load(Ordering::Relaxed),
            cache_miss_total: self.cache_miss_total.load(Ordering::Relaxed),
            cache_bypass_total: self.cache_bypass_total.load(Ordering::Relaxed),
            cache_disabled_total: self.cache_disabled_total.load(Ordering::Relaxed),
            bytes_served_total: self.bytes_served_total.load(Ordering::Relaxed),
            bytes_from_cache_total: self.bytes_from_cache_total.load(Ordering::Relaxed),
            bytes_from_origin_total: self.bytes_from_origin_total.load(Ordering::Relaxed),
        }
    }
}

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    init_logging();

    // Load config
    dotenv::dotenv().ok();
    let config = Config::from_env()?;

    info!(port = config.port, "starting r2 proxy server");

    // Initialize client
    let kv_client = KvClient::new(&config.cloudflare_account_id, &config.cloudflare_api_token)?;
    let s3_client = S3Client::new(
        &config.r2_endpoint,
        &config.r2_access_key_id,
        &config.r2_secret_access_key,
    )
    .await?;
    let local_cache = LocalCache::new(config.local_cache.clone()).await;
    info!(
        cloudflare_account_id = %config.cloudflare_account_id,
        cloudflare_kv_namespace_id = %kv_client.namespace_id(),
        "clients initialized"
    );

    // Initialize cache
    let cache = Arc::new(RwLock::new(AppCache {
        whitelist: HashMap::new(),
        cors_config: HashMap::new(),
        spa: HashMap::new(),
    }));
    let metrics = Arc::new(AppMetrics::default());

    // 初始化缓存（同步拉取一次）
    refresh_cache(&kv_client, &cache).await?;
    info!("initial cache refresh completed");

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
            local_cache,
            cache: cache.clone(),
            metrics,
        });

    // Start refresh task
    let kv_client_clone = kv_client.clone();
    let cache_clone = cache.clone();
    tokio::spawn(async move {
        loop {
            tokio::time::sleep(std::time::Duration::from_secs(600)).await;
            if let Err(e) = refresh_cache(&kv_client_clone, &cache_clone).await {
                error!(error = %e, "cache refresh failed");
            }
        }
    });

    // Start server
    let addr = SocketAddr::from(([0, 0, 0, 0], config.port));
    info!(address = %addr, "server listening");

    let listener = tokio::net::TcpListener::bind(addr).await?;
    axum::serve(listener, app).await?;

    Ok(())
}

#[derive(Clone)]
struct AppState {
    s3_client: S3Client,
    local_cache: LocalCache,
    cache: Arc<RwLock<AppCache>>,
    metrics: Arc<AppMetrics>,
}

async fn proxy_handler(
    uri: Uri,
    axum::extract::State(state): axum::extract::State<AppState>,
    axum::extract::Host(host): axum::extract::Host,
) -> Result<Response, ProxyError> {
    let bucket = resolve_virtual_bucket(&host);
    info!(host = %host, path = %uri.path(), virtual_bucket = bucket, "incoming request");
    if bucket.is_empty() {
        return Err(ProxyError::InvalidPath(
            "Bucket not found in host".to_string(),
        ));
    }
    let (real_bucket, cors_config, spa_enabled) = {
        let cache = state.cache.read().await;
        (
            cache.whitelist.get(bucket).cloned(),
            cache.cors_config.get(bucket).cloned().unwrap_or(None),
            cache.spa.get(bucket).copied().unwrap_or(false),
        )
    };
    let object_key = resolve_object_key(uri.path(), spa_enabled);
    info!(
        host = %host,
        path = %uri.path(),
        virtual_bucket = bucket,
        object_key = %object_key,
        spa_enabled,
        "resolved request routing"
    );

    // 检查 bucket 是否在白名单中
    let real_bucket = match real_bucket {
        Some(b) if !b.is_empty() => {
            info!(
                host = %host,
                virtual_bucket = bucket,
                real_bucket = %b,
                "bucket authorized"
            );
            b
        }
        _ => {
            let known_buckets = {
                let cache = state.cache.read().await;
                cache
                    .whitelist
                    .keys()
                    .cloned()
                    .collect::<Vec<_>>()
                    .join(",")
            };
            warn!(
                host = %host,
                path = %uri.path(),
                virtual_bucket = bucket,
                object_key = %object_key,
                known_virtual_buckets = %known_buckets,
                "access to bucket denied because whitelist lookup failed"
            );
            return Err(ProxyError::UnauthorizedBucket(bucket.to_string()));
        }
    };
    let request_context = RequestContext {
        host: host.clone(),
        virtual_bucket: bucket.to_string(),
        real_bucket: real_bucket.clone(),
        object_key: object_key.clone(),
    };

    let (cache_status, body, cached_headers) =
        match state.local_cache.get(&real_bucket, &object_key).await? {
            (CacheStatus::Hit, Some(cached_response)) => {
                let bytes_served = cached_response.body.len() as u64;
                state.metrics.record(CacheStatus::Hit, bytes_served);
                info!(
                    host = %request_context.host,
                    virtual_bucket = %request_context.virtual_bucket,
                    real_bucket = %request_context.real_bucket,
                    object_key = %request_context.object_key,
                    cache_status = %CacheStatus::Hit.header_value(),
                    bytes_served,
                    status = %StatusCode::OK,
                    "request served successfully"
                );

                (
                    CacheStatus::Hit,
                    Body::from(cached_response.body),
                    cached_response.headers,
                )
            }
            (lookup_status, _) => {
                let s3_response = state
                    .s3_client
                    .get_object(&real_bucket, &object_key)
                    .await?;
                let pending_write =
                    if matches!(lookup_status, CacheStatus::Disabled | CacheStatus::Bypass) {
                        None
                    } else {
                        state
                            .local_cache
                            .prepare_stream_store(
                                &real_bucket,
                                &object_key,
                                s3_response.headers.content_length,
                                s3_response.headers.clone(),
                            )
                            .await?
                            .1
                    };
                let response_status = response_cache_status(lookup_status);

                (
                    response_status,
                    Body::from_stream(stream_origin_body(
                        s3_response.body,
                        pending_write,
                        state.metrics.clone(),
                        request_context.clone(),
                        response_status,
                    )),
                    s3_response.headers,
                )
            }
        };

    // Build response
    let mut response = Response::new(body);

    // Set status code
    *response.status_mut() = StatusCode::OK;

    // Set content type
    if let Some(content_type) = cached_headers.content_type {
        let content_type = axum::http::HeaderValue::from_str(&content_type)
            .unwrap_or_else(|_| axum::http::HeaderValue::from_static("application/octet-stream"));
        response.headers_mut().insert("content-type", content_type);
    }
    // Set content length
    if let Some(content_length) = cached_headers.content_length {
        response.headers_mut().insert(
            axum::http::header::CONTENT_LENGTH,
            axum::http::HeaderValue::from_str(&content_length.to_string()).unwrap(),
        );
    }
    // Set ETag
    if let Some(etag) = cached_headers.etag {
        response.headers_mut().insert(
            axum::http::header::ETAG,
            axum::http::HeaderValue::from_str(&etag).unwrap(),
        );
    }
    // Set Last-Modified
    if let Some(last_modified) = cached_headers.last_modified {
        response.headers_mut().insert(
            axum::http::header::LAST_MODIFIED,
            axum::http::HeaderValue::from_str(&last_modified).unwrap(),
        );
    }
    apply_cache_control(response.headers_mut(), &object_key);
    apply_cache_status_header(response.headers_mut(), cache_status);

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
    info!("handled preflight options request");
    StatusCode::OK
}

fn init_logging() {
    let rust_log = env::var("RUST_LOG").ok();
    let filter = default_log_filter(rust_log.as_deref());

    tracing_subscriber::fmt()
        .with_env_filter(EnvFilter::new(filter))
        .with_writer(std::io::stdout)
        .with_ansi(false)
        .init();
}

fn default_log_filter(value: Option<&str>) -> &str {
    match value {
        Some(v) if !v.trim().is_empty() => v,
        _ => "info",
    }
}

fn resolve_virtual_bucket(host: &str) -> &str {
    if let Some((prefix, rest)) = host.split_once('.') {
        if rest == "delbertbeta.life" {
            return prefix;
        }
    } else if host == "delbertbeta.life" {
        return "@";
    }

    "@"
}

fn preview_value(value: &str, max_chars: usize) -> String {
    let preview: String = value.chars().take(max_chars).collect();
    if value.chars().count() > max_chars {
        format!("{preview}...")
    } else {
        preview
    }
}

fn resolve_object_key(path: &str, spa_enabled: bool) -> String {
    let trimmed_path = path.trim_start_matches('/');

    if trimmed_path.is_empty() {
        return "index.html".to_string();
    }

    if spa_enabled && !has_file_extension(trimmed_path) {
        return "index.html".to_string();
    }

    let mut key = trimmed_path.to_string();
    if key.ends_with('/') {
        key.push_str("index.html");
    }
    key
}

fn has_file_extension(path: &str) -> bool {
    path.rsplit('/')
        .next()
        .map(|segment| segment.contains('.'))
        .unwrap_or(false)
}

fn apply_cache_control(headers: &mut HeaderMap, object_key: &str) {
    if object_key == "index.html" || object_key.ends_with("/index.html") {
        return;
    }

    headers.insert(
        axum::http::header::CACHE_CONTROL,
        axum::http::HeaderValue::from_static("public, max-age=31536000, immutable"),
    );
}

fn apply_cache_status_header(headers: &mut HeaderMap, status: CacheStatus) {
    headers.insert(
        "X-R2-Proxy-Cached",
        axum::http::HeaderValue::from_static(status.header_value()),
    );
}

fn response_cache_status(lookup_status: CacheStatus) -> CacheStatus {
    match lookup_status {
        CacheStatus::Hit => CacheStatus::Hit,
        CacheStatus::Miss => CacheStatus::Miss,
        CacheStatus::Bypass => CacheStatus::Bypass,
        CacheStatus::Disabled => CacheStatus::Disabled,
    }
}

fn stream_origin_body(
    upstream_body: aws_sdk_s3::primitives::ByteStream,
    pending_write: Option<PendingCacheWrite>,
    metrics: Arc<AppMetrics>,
    request_context: RequestContext,
    cache_status: CacheStatus,
) -> impl futures_util::Stream<Item = Result<Bytes, io::Error>> {
    try_stream! {
        let mut upstream_body = upstream_body;
        let mut pending_write = pending_write;
        let mut bytes_served = 0_u64;

        loop {
            let next_chunk = match upstream_body.try_next().await {
                Ok(next_chunk) => next_chunk,
                Err(error) => {
                    if let Some(writer) = pending_write.take() {
                        writer.abort().await;
                    }
                    Err(io::Error::other(format!("failed to read s3 stream: {error}")))?;
                    unreachable!();
                }
            };

            let Some(chunk) = next_chunk else {
                break;
            };

            if let Some(writer) = pending_write.as_mut() {
                if let Err(error) = writer.write_chunk(chunk.as_ref()).await {
                    warn!(error = %error, "stream cache write failed, disabling cache fill for current request");
                    if let Some(writer) = pending_write.take() {
                        writer.abort().await;
                    }
                }
            }

            bytes_served = bytes_served.saturating_add(chunk.len() as u64);
            yield chunk;
        }

        if let Some(writer) = pending_write {
            if let Err(error) = writer.commit().await {
                warn!(error = %error, "stream cache commit failed");
            }
        }

        metrics.record(cache_status, bytes_served);
        info!(
            host = %request_context.host,
            virtual_bucket = %request_context.virtual_bucket,
            real_bucket = %request_context.real_bucket,
            object_key = %request_context.object_key,
            cache_status = %cache_status.header_value(),
            bytes_served,
            status = %StatusCode::OK,
            "request served successfully"
        );
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use axum::http::{header::CACHE_CONTROL, HeaderMap};
    use std::sync::Arc;

    #[test]
    fn default_log_filter_is_info() {
        assert_eq!(default_log_filter(None), "info");
    }

    #[test]
    fn custom_log_filter_is_preserved() {
        assert_eq!(
            default_log_filter(Some("debug,hyper=warn")),
            "debug,hyper=warn"
        );
    }

    #[test]
    fn resolves_virtual_bucket_from_subdomain_host() {
        assert_eq!(resolve_virtual_bucket("foo.delbertbeta.life"), "foo");
    }

    #[test]
    fn resolves_virtual_bucket_from_root_domain_host() {
        assert_eq!(resolve_virtual_bucket("delbertbeta.life"), "@");
    }

    #[test]
    fn resolves_virtual_bucket_to_default_for_unknown_host() {
        assert_eq!(resolve_virtual_bucket("example.com"), "@");
    }

    #[test]
    fn preview_value_truncates_long_text() {
        assert_eq!(preview_value("abcdef", 4), "abcd...");
    }

    #[test]
    fn preview_value_keeps_short_text() {
        assert_eq!(preview_value("abc", 4), "abc");
    }

    #[test]
    fn non_index_assets_get_cache_control() {
        let mut headers = HeaderMap::new();

        apply_cache_control(&mut headers, "assets/app.js");

        assert_eq!(
            headers.get(CACHE_CONTROL).unwrap(),
            "public, max-age=31536000, immutable"
        );
    }

    #[test]
    fn index_html_does_not_get_cache_control() {
        let mut headers = HeaderMap::new();

        apply_cache_control(&mut headers, "index.html");

        assert!(headers.get(CACHE_CONTROL).is_none());
    }

    #[test]
    fn nested_index_html_does_not_get_cache_control() {
        let mut headers = HeaderMap::new();

        apply_cache_control(&mut headers, "docs/index.html");

        assert!(headers.get(CACHE_CONTROL).is_none());
    }

    #[test]
    fn applies_r2_proxy_cached_header() {
        let mut headers = HeaderMap::new();

        apply_cache_status_header(&mut headers, CacheStatus::Hit);

        assert_eq!(headers.get("x-r2-proxy-cached").unwrap(), "HIT");
    }

    #[test]
    fn lookup_miss_stays_miss_even_without_cache_fill() {
        assert_eq!(response_cache_status(CacheStatus::Miss), CacheStatus::Miss);
        assert_eq!(
            response_cache_status(CacheStatus::Bypass),
            CacheStatus::Bypass
        );
        assert_eq!(
            response_cache_status(CacheStatus::Disabled),
            CacheStatus::Disabled
        );
    }

    #[test]
    fn metrics_snapshot_tracks_status_and_bytes() {
        let metrics = Arc::new(AppMetrics::default());

        metrics.record(CacheStatus::Hit, 10);
        metrics.record(CacheStatus::Miss, 20);
        metrics.record(CacheStatus::Bypass, 30);
        metrics.record(CacheStatus::Disabled, 40);

        let snapshot = metrics.snapshot();

        assert_eq!(snapshot.requests_total, 4);
        assert_eq!(snapshot.cache_hit_total, 1);
        assert_eq!(snapshot.cache_miss_total, 1);
        assert_eq!(snapshot.cache_bypass_total, 1);
        assert_eq!(snapshot.cache_disabled_total, 1);
        assert_eq!(snapshot.bytes_served_total, 100);
        assert_eq!(snapshot.bytes_from_cache_total, 10);
        assert_eq!(snapshot.bytes_from_origin_total, 90);
    }
}

async fn refresh_cache(
    kv_client: &KvClient,
    cache: &Arc<RwLock<AppCache>>,
) -> Result<(), anyhow::Error> {
    info!("refreshing cache from cloudflare kv");
    // 读取 whitelist
    let whitelist_value = kv_client.get_kv_value("whitelist").await?;
    let mut whitelist = HashMap::new();
    if let Some(val) = whitelist_value {
        info!(
            kv_key = "whitelist",
            value_length = val.len(),
            value_preview = %preview_value(&val, 200),
            "loaded raw kv value"
        );
        match serde_json::from_str::<Vec<(String, String)>>(&val) {
            Ok(list) => {
                for (prefix, bucket) in list {
                    if !prefix.trim().is_empty() && !bucket.trim().is_empty() {
                        whitelist.insert(prefix.trim().to_string(), bucket.trim().to_string());
                    }
                }
            }
            Err(error) => {
                warn!(
                    kv_key = "whitelist",
                    parse_error = %error,
                    value_preview = %preview_value(&val, 200),
                    "failed to parse kv value"
                );
            }
        }
    } else {
        warn!(kv_key = "whitelist", "kv value missing or empty");
    }
    // 读取 cors
    let cors_value = kv_client.get_kv_value("cors").await?;
    let mut cors_config = HashMap::new();
    if let Some(val) = cors_value {
        info!(
            kv_key = "cors",
            value_length = val.len(),
            value_preview = %preview_value(&val, 200),
            "loaded raw kv value"
        );
        match serde_json::from_str::<HashMap<String, cors::CorsConfig>>(&val) {
            Ok(map) => {
                for (bucket, config) in map {
                    cors_config.insert(bucket, Some(config));
                }
            }
            Err(error) => {
                warn!(
                    kv_key = "cors",
                    parse_error = %error,
                    value_preview = %preview_value(&val, 200),
                    "failed to parse kv value"
                );
            }
        }
    } else {
        warn!(kv_key = "cors", "kv value missing or empty");
    }
    // 读取 spa
    let spa_value = kv_client.get_kv_value("spa").await?;
    let mut spa = HashMap::new();
    if let Some(val) = spa_value {
        info!(
            kv_key = "spa",
            value_length = val.len(),
            value_preview = %preview_value(&val, 200),
            "loaded raw kv value"
        );
        match serde_json::from_str::<HashMap<String, bool>>(&val) {
            Ok(map) => {
                spa = map;
            }
            Err(error) => {
                warn!(
                    kv_key = "spa",
                    parse_error = %error,
                    value_preview = %preview_value(&val, 200),
                    "failed to parse kv value"
                );
            }
        }
    } else {
        warn!(kv_key = "spa", "kv value missing or empty");
    }
    let mut cache_guard = cache.write().await;
    cache_guard.whitelist = whitelist;
    cache_guard.cors_config = cors_config;
    cache_guard.spa = spa;
    info!(
        whitelist_count = cache_guard.whitelist.len(),
        cors_bucket_count = cache_guard.cors_config.len(),
        spa_bucket_count = cache_guard.spa.len(),
        "cache refresh completed"
    );
    Ok(())
}
