use std::sync::Arc;
use std::time::{SystemTime, UNIX_EPOCH};

use axum::{
    body::Body,
    extract::{Query, State},
    http::{header, HeaderMap, HeaderValue, Response, StatusCode},
    response::IntoResponse,
    routing::{get, post},
    Json, Router,
};
use serde::{Deserialize, Serialize};
use tokio::sync::RwLock;

use crate::{
    local_cache::{LocalCache, LocalCacheUsage},
    stats::{BucketTotals, Resolution, StatsScope, StatsStore},
    status_assets, AppCache,
};

#[derive(Clone)]
pub struct StatusState {
    pub api_key: Arc<String>,
    pub stats_store: StatsStore,
    pub local_cache: LocalCache,
    pub cache: Arc<RwLock<AppCache>>,
}

impl StatusState {
    #[cfg(test)]
    fn for_test(api_key: &str) -> Self {
        let redis = crate::config::RedisConfig {
            redis_url: "redis://127.0.0.1:6379".to_string(),
            redis_key_prefix: "r2proxy-test".to_string(),
        };

        Self {
            api_key: Arc::new(api_key.to_string()),
            stats_store: StatsStore::new(&redis).expect("test stats store"),
            local_cache: LocalCache::disabled(),
            cache: Arc::new(RwLock::new(AppCache::default())),
        }
    }
}

pub fn build_status_router(state: StatusState) -> Router {
    Router::new()
        .route("/", get(serve_index))
        .route("/assets/app.css", get(serve_asset))
        .route("/assets/app.js", get(serve_asset))
        .route("/api/login", post(login))
        .route("/api/filters", get(filters))
        .route("/api/overview", get(overview))
        .route("/api/timeseries", get(timeseries))
        .route("/api/top", get(top))
        .with_state(state)
}

#[derive(Deserialize)]
struct LoginRequest {
    #[serde(rename = "apiKey")]
    api_key: String,
}

#[derive(Serialize)]
struct FiltersResponse {
    #[serde(rename = "defaultBucket")]
    default_bucket: Option<String>,
    buckets: Vec<String>,
}

#[derive(Serialize)]
struct TotalsResponse {
    requests: u64,
    bytes: u64,
    #[serde(rename = "cacheHitRate")]
    cache_hit_rate: f64,
    errors: u64,
    #[serde(rename = "errorRate")]
    error_rate: f64,
}

#[derive(Serialize)]
struct LocalCacheResponse {
    enabled: bool,
    #[serde(rename = "usedBytes")]
    used_bytes: u64,
    #[serde(rename = "capacityBytes")]
    capacity_bytes: u64,
    #[serde(rename = "usageRate")]
    usage_rate: f64,
}

#[derive(Serialize)]
struct OverviewResponse {
    scope: String,
    totals: TotalsResponse,
    #[serde(rename = "localCache")]
    local_cache: LocalCacheResponse,
}

#[derive(Deserialize)]
struct ScopeQuery {
    bucket: Option<String>,
}

#[derive(Deserialize)]
struct TimeSeriesQuery {
    range: String,
    bucket: Option<String>,
}

#[derive(Serialize)]
struct SummaryResponse {
    requests: u64,
    bytes: u64,
    #[serde(rename = "cacheHitRate")]
    cache_hit_rate: f64,
    errors: u64,
    #[serde(rename = "errorRate")]
    error_rate: f64,
}

#[derive(Serialize)]
struct DataPoint {
    ts: u64,
    value: f64,
}

#[derive(Serialize)]
struct SeriesResponse {
    qps: Vec<DataPoint>,
    #[serde(rename = "throughputBytesPerSec")]
    throughput_bytes_per_sec: Vec<DataPoint>,
    #[serde(rename = "cacheHitRate")]
    cache_hit_rate: Vec<DataPoint>,
    #[serde(rename = "errorRate")]
    error_rate: Vec<DataPoint>,
}

#[derive(Serialize)]
struct TimeSeriesResponse {
    scope: String,
    range: String,
    granularity: String,
    summary: SummaryResponse,
    series: SeriesResponse,
}

#[derive(Serialize)]
struct CacheFileEntry {
    bucket: String,
    #[serde(rename = "objectKey")]
    object_key: String,
    hits: u64,
}

#[derive(Serialize)]
struct UrlMetricEntry {
    bucket: String,
    url: String,
    misses: Option<u64>,
    errors: Option<u64>,
}

#[derive(Serialize)]
struct TopResponse {
    scope: String,
    window: String,
    #[serde(rename = "hotCacheFiles")]
    hot_cache_files: Vec<CacheFileEntry>,
    #[serde(rename = "missUrls")]
    miss_urls: Vec<UrlMetricEntry>,
    #[serde(rename = "errorUrls")]
    error_urls: Vec<UrlMetricEntry>,
}

async fn serve_index() -> impl IntoResponse {
    asset_response("/")
}

async fn serve_asset(uri: axum::http::Uri) -> impl IntoResponse {
    asset_response(uri.path())
}

async fn login(
    State(state): State<StatusState>,
    Json(payload): Json<LoginRequest>,
) -> impl IntoResponse {
    if payload.api_key == state.api_key.as_str() {
        StatusCode::NO_CONTENT
    } else {
        StatusCode::UNAUTHORIZED
    }
}

async fn filters(State(state): State<StatusState>, headers: HeaderMap) -> impl IntoResponse {
    if !is_valid_api_key(&headers, &state.api_key) {
        return StatusCode::UNAUTHORIZED.into_response();
    }

    let cache = state.cache.read().await;
    let mut buckets = cache.whitelist.keys().cloned().collect::<Vec<_>>();
    buckets.sort();

    no_store(Json(FiltersResponse {
        default_bucket: None,
        buckets,
    }))
}

async fn overview(
    State(state): State<StatusState>,
    headers: HeaderMap,
    Query(query): Query<ScopeQuery>,
) -> impl IntoResponse {
    if !is_valid_api_key(&headers, &state.api_key) {
        return StatusCode::UNAUTHORIZED.into_response();
    }

    let scope = match resolve_scope(&state.cache, query.bucket).await {
        Ok(scope) => scope,
        Err(status) => return status.into_response(),
    };

    match (
        state.stats_store.read_totals(&scope).await,
        state.local_cache.usage().await,
    ) {
        (Ok(totals), usage) => no_store(Json(OverviewResponse {
            scope: scope.redis_key(),
            totals: totals_response(totals),
            local_cache: local_cache_response(usage),
        })),
        (Err(_), _) => StatusCode::SERVICE_UNAVAILABLE.into_response(),
    }
}

async fn timeseries(
    State(state): State<StatusState>,
    headers: HeaderMap,
    Query(query): Query<TimeSeriesQuery>,
) -> impl IntoResponse {
    if !is_valid_api_key(&headers, &state.api_key) {
        return StatusCode::UNAUTHORIZED.into_response();
    }

    let scope = match resolve_scope(&state.cache, query.bucket).await {
        Ok(scope) => scope,
        Err(status) => return status.into_response(),
    };
    let (resolution, points) = match query.range.as_str() {
        "1h" => (Resolution::FiveMinutes, 12),
        "24h" => (Resolution::OneHour, 24),
        "7d" => (Resolution::OneDay, 7),
        _ => return StatusCode::BAD_REQUEST.into_response(),
    };

    match state
        .stats_store
        .read_series(&scope, resolution, points, unix_timestamp())
        .await
    {
        Ok(series) => {
            let summary = series
                .iter()
                .fold(BucketTotals::default(), |mut acc, (_, totals)| {
                    acc.requests += totals.requests;
                    acc.bytes += totals.bytes;
                    acc.cache_hits += totals.cache_hits;
                    acc.cache_misses += totals.cache_misses;
                    acc.errors += totals.errors;
                    acc
                });

            let bucket_seconds = resolution.duration_seconds();
            let response = TimeSeriesResponse {
                scope: scope.redis_key(),
                range: query.range,
                granularity: resolution.redis_key().to_string(),
                summary: SummaryResponse {
                    requests: summary.requests,
                    bytes: summary.bytes,
                    cache_hit_rate: summary.cache_hit_rate(),
                    errors: summary.errors,
                    error_rate: summary.error_rate(),
                },
                series: SeriesResponse {
                    qps: series
                        .iter()
                        .map(|(ts, totals)| DataPoint {
                            ts: *ts,
                            value: totals.qps(bucket_seconds),
                        })
                        .collect(),
                    throughput_bytes_per_sec: series
                        .iter()
                        .map(|(ts, totals)| DataPoint {
                            ts: *ts,
                            value: totals.bytes as f64 / bucket_seconds as f64,
                        })
                        .collect(),
                    cache_hit_rate: series
                        .iter()
                        .map(|(ts, totals)| DataPoint {
                            ts: *ts,
                            value: totals.cache_hit_rate(),
                        })
                        .collect(),
                    error_rate: series
                        .iter()
                        .map(|(ts, totals)| DataPoint {
                            ts: *ts,
                            value: totals.error_rate(),
                        })
                        .collect(),
                },
            };

            no_store(Json(response))
        }
        Err(_) => StatusCode::SERVICE_UNAVAILABLE.into_response(),
    }
}

async fn top(
    State(state): State<StatusState>,
    headers: HeaderMap,
    Query(query): Query<ScopeQuery>,
) -> impl IntoResponse {
    if !is_valid_api_key(&headers, &state.api_key) {
        return StatusCode::UNAUTHORIZED.into_response();
    }

    let scope = match resolve_scope(&state.cache, query.bucket).await {
        Ok(scope) => scope,
        Err(status) => return status.into_response(),
    };
    let now = unix_timestamp();

    match (
        state.stats_store.read_top_hits(&scope, now, 10).await,
        state.stats_store.read_top_misses(&scope, now, 10).await,
        state.stats_store.read_top_errors(&scope, now, 10).await,
    ) {
        (Ok(hits), Ok(misses), Ok(errors)) => no_store(Json(TopResponse {
            scope: scope.redis_key(),
            window: "7d".to_string(),
            hot_cache_files: hits
                .into_iter()
                .filter_map(|(member, count)| {
                    split_member(&member).map(|(bucket, object_key)| CacheFileEntry {
                        bucket,
                        object_key,
                        hits: count,
                    })
                })
                .collect(),
            miss_urls: misses
                .into_iter()
                .filter_map(|(member, count)| {
                    split_member(&member).map(|(bucket, url)| UrlMetricEntry {
                        bucket,
                        url,
                        misses: Some(count),
                        errors: None,
                    })
                })
                .collect(),
            error_urls: errors
                .into_iter()
                .filter_map(|(member, count)| {
                    split_member(&member).map(|(bucket, url)| UrlMetricEntry {
                        bucket,
                        url,
                        misses: None,
                        errors: Some(count),
                    })
                })
                .collect(),
        })),
        _ => StatusCode::SERVICE_UNAVAILABLE.into_response(),
    }
}

fn resolve_scope_key(bucket: Option<String>) -> StatsScope {
    bucket.map(StatsScope::Bucket).unwrap_or(StatsScope::Global)
}

async fn resolve_scope(
    cache: &Arc<RwLock<AppCache>>,
    bucket: Option<String>,
) -> Result<StatsScope, StatusCode> {
    let scope = resolve_scope_key(bucket);
    if let StatsScope::Bucket(bucket_name) = &scope {
        let cache = cache.read().await;
        if !cache.whitelist.contains_key(bucket_name) {
            return Err(StatusCode::NOT_FOUND);
        }
    }
    Ok(scope)
}

fn is_valid_api_key(headers: &HeaderMap, expected: &str) -> bool {
    headers
        .get("x-status-api-key")
        .and_then(|value| value.to_str().ok())
        .map(|value| value == expected)
        .unwrap_or(false)
}

fn totals_response(totals: BucketTotals) -> TotalsResponse {
    TotalsResponse {
        requests: totals.requests,
        bytes: totals.bytes,
        cache_hit_rate: totals.cache_hit_rate(),
        errors: totals.errors,
        error_rate: totals.error_rate(),
    }
}

fn local_cache_response(usage: LocalCacheUsage) -> LocalCacheResponse {
    LocalCacheResponse {
        enabled: usage.enabled,
        used_bytes: usage.used_bytes,
        capacity_bytes: usage.capacity_bytes,
        usage_rate: usage.usage_rate(),
    }
}

fn split_member(member: &str) -> Option<(String, String)> {
    let (bucket, value) = member.split_once('|')?;
    Some((bucket.to_string(), value.to_string()))
}

fn asset_response(path: &str) -> Response<Body> {
    match status_assets::asset(path) {
        Some((content_type, body)) => {
            let mut response = Response::new(Body::from(body.to_vec()));
            *response.status_mut() = StatusCode::OK;
            response
                .headers_mut()
                .insert(header::CONTENT_TYPE, HeaderValue::from_static(content_type));
            response
        }
        None => {
            let mut response = Response::new(Body::empty());
            *response.status_mut() = StatusCode::NOT_FOUND;
            response
        }
    }
}

fn no_store<T: IntoResponse>(response: T) -> Response<Body> {
    let mut response = response.into_response();
    response
        .headers_mut()
        .insert(header::CACHE_CONTROL, HeaderValue::from_static("no-store"));
    response
}

fn unix_timestamp() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_secs()
}

#[cfg(test)]
mod tests {
    use axum::http::Request;
    use tower::Service;

    use super::*;

    #[tokio::test]
    async fn rejects_status_api_requests_without_api_key() {
        let mut app = build_status_router(StatusState::for_test("secret")).into_service();

        let response = app
            .call(
                Request::builder()
                    .uri("/api/overview")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();

        assert_eq!(response.status(), StatusCode::UNAUTHORIZED);
    }

    #[tokio::test]
    async fn accepts_login_when_api_key_matches() {
        let mut app = build_status_router(StatusState::for_test("secret")).into_service();

        let response = app
            .call(
                Request::builder()
                    .method("POST")
                    .uri("/api/login")
                    .header("content-type", "application/json")
                    .body(Body::from(r#"{"apiKey":"secret"}"#))
                    .unwrap(),
            )
            .await
            .unwrap();

        assert_eq!(response.status(), StatusCode::NO_CONTENT);
    }

    #[tokio::test]
    async fn serves_dashboard_shell() {
        let mut app = build_status_router(StatusState::for_test("secret")).into_service();

        let response = app
            .call(Request::builder().uri("/").body(Body::empty()).unwrap())
            .await
            .unwrap();

        assert_eq!(response.status(), StatusCode::OK);
        assert_eq!(
            response.headers().get(header::CONTENT_TYPE).unwrap(),
            "text/html; charset=utf-8"
        );
    }
}
