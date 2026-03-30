---
title: 2026-03-30-status-page-dashboard
type: note
permalink: work/r2-proxy/docs/superpowers/plans/2026-03-30-status-page-dashboard
---

# Status Page Dashboard Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Add a Redis-backed status dashboard that starts on a second port, records proxy traffic and error metrics, serves an embedded frontend, and documents how to deploy and review the feature.

**Architecture:** Extend the existing single-process Rust service with a shared stats store and a second Axum router. The proxy hot path records best-effort success and failure metrics into Redis for all-time totals, multi-resolution time buckets, and 7-day top lists. A protected status server serves embedded HTML/CSS/JS and authenticated JSON APIs for overview, timeseries, and ranking data.

**Tech Stack:** Rust, Axum, Tokio, Redis, Serde, embedded static assets, browser `localStorage`, GitHub PR workflow

---

## File Structure

**Create:**

- `src/stats.rs`
  - Stats domain types, Redis key building, write/read operations, and tests
- `src/status_server.rs`
  - Status router, auth middleware, API handlers, and asset responses
- `src/status_assets.rs`
  - Embedded frontend asset constants and content-type helpers
- `docs/superpowers/plans/2026-03-30-status-page-dashboard.md`
  - This implementation plan

**Modify:**

- `src/config.rs`
  - Parse status host/port/api-key config and tests
- `src/local_cache.rs`
  - Expose current cache usage for overview APIs
- `src/errors.rs`
  - Add error-kind helpers used by metrics recording
- `src/main.rs`
  - Initialize stats store, start second listener, and emit request metrics
- `Cargo.toml`
  - Add any dependencies required for embedded assets or time helpers
- `README.md`
  - Document configuration, deployment, status login, and PR/review usage

## Task 1: Extend Configuration For The Status Service

**Files:**
- Modify: `src/config.rs`
- Test: `src/config.rs`

- [ ] **Step 1: Write the failing tests**

```rust
#[test]
fn config_reads_status_server_settings() {
    set_base_env();
    unsafe {
        env::set_var("STATUS_PORT", "3001");
        env::set_var("STATUS_HOST", "127.0.0.1");
        env::set_var("STATUS_API_KEY", "secret-status-key");
    }

    let config = Config::from_env().unwrap();

    assert_eq!(config.status.port, 3001);
    assert_eq!(config.status.host, "127.0.0.1");
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
```

- [ ] **Step 2: Run test to verify it fails**

Run: `cargo test config:: -- --nocapture`
Expected: FAIL because `Config` has no `status` field and the new env vars are not parsed.

- [ ] **Step 3: Write minimal implementation**

```rust
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
    pub cloudflare_account_id: String,
    pub cloudflare_api_token: String,
    pub r2_endpoint: String,
    pub r2_access_key_id: String,
    pub r2_secret_access_key: String,
    pub local_cache: Option<LocalCacheConfig>,
}

status: StatusConfig {
    port: env::var("STATUS_PORT")
        .unwrap_or_else(|_| "3001".to_string())
        .parse::<u16>()
        .map_err(|e| ConfigError::InvalidPort(e.to_string()))?,
    host: env::var("STATUS_HOST").unwrap_or_else(|_| "127.0.0.1".to_string()),
    api_key: env::var("STATUS_API_KEY")
        .map_err(|_| ConfigError::MissingEnvVar("STATUS_API_KEY".to_string()))?,
},
```

- [ ] **Step 4: Run test to verify it passes**

Run: `cargo test config:: -- --nocapture`
Expected: PASS

- [ ] **Step 5: Commit**

```bash
git add src/config.rs
git commit -m "feat: add status service configuration"
```

## Task 2: Build The Redis Stats Store

**Files:**
- Create: `src/stats.rs`
- Modify: `Cargo.toml`
- Test: `src/stats.rs`

- [ ] **Step 1: Write the failing tests**

```rust
#[test]
fn rounds_bucket_starts_for_each_resolution() {
    assert_eq!(bucket_start(1711753499, Resolution::FiveMinutes), 1711753200);
    assert_eq!(bucket_start(1711753499, Resolution::OneHour), 1711753200);
    assert_eq!(bucket_start(1711753499, Resolution::OneDay), 1711670400);
}

#[test]
fn builds_scope_names_for_global_and_bucket_views() {
    assert_eq!(StatsScope::Global.redis_key(), "global");
    assert_eq!(StatsScope::Bucket("foo".to_string()).redis_key(), "bucket:foo");
}

#[test]
fn computes_rates_with_safe_zero_denominators() {
    let totals = BucketTotals {
        requests: 0,
        bytes: 0,
        cache_hits: 0,
        cache_misses: 0,
        errors: 0,
    };

    assert_eq!(totals.cache_hit_rate(), 0.0);
    assert_eq!(totals.error_rate(), 0.0);
    assert_eq!(totals.qps(300), 0.0);
}
```

- [ ] **Step 2: Run test to verify it fails**

Run: `cargo test stats:: -- --nocapture`
Expected: FAIL because `src/stats.rs` and the stats types do not exist.

- [ ] **Step 3: Write minimal implementation**

```rust
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

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum Resolution {
    FiveMinutes,
    OneHour,
    OneDay,
}

pub fn bucket_start(timestamp: u64, resolution: Resolution) -> u64 {
    let window = match resolution {
        Resolution::FiveMinutes => 300,
        Resolution::OneHour => 3600,
        Resolution::OneDay => 86400,
    };
    timestamp - (timestamp % window)
}

#[derive(Clone, Copy, Debug, Default)]
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
```

- [ ] **Step 4: Add the Redis-backed store surface**

```rust
#[derive(Clone)]
pub struct StatsStore {
    redis_client: redis::Client,
    key_prefix: String,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum StatsCacheStatus {
    Hit,
    Miss,
    Bypass,
    Disabled,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum StatsResult {
    Success,
    Error,
}

#[derive(Clone, Debug)]
pub struct StatsEvent {
    pub timestamp: u64,
    pub bucket: String,
    pub path_and_query: String,
    pub object_key: Option<String>,
    pub bytes: u64,
    pub cache_status: StatsCacheStatus,
    pub result: StatsResult,
}
```

- [ ] **Step 5: Run test to verify it passes**

Run: `cargo test stats:: -- --nocapture`
Expected: PASS

- [ ] **Step 6: Commit**

```bash
git add Cargo.toml src/stats.rs
git commit -m "feat: add stats domain and redis store"
```

## Task 3: Record Proxy Success, Miss, And Error Metrics

**Files:**
- Modify: `src/errors.rs`
- Modify: `src/main.rs`
- Modify: `src/s3_client.rs`
- Modify: `src/stats.rs`
- Test: `src/main.rs`
- Test: `src/errors.rs`

- [ ] **Step 1: Write the failing tests**

```rust
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
fn counts_hit_bytes_from_cached_headers() {
    let headers = CachedHeaders {
        content_type: None,
        content_length: Some(1024),
        etag: None,
        last_modified: None,
    };

    assert_eq!(successful_response_bytes(CacheStatus::Hit, &headers, 0), 1024);
}

#[test]
fn excludes_bypass_and_disabled_from_cache_rate_denominator() {
    assert!(stats_cache_status(CacheStatus::Bypass).is_non_cacheable());
    assert!(stats_cache_status(CacheStatus::Disabled).is_non_cacheable());
}
```

- [ ] **Step 2: Run test to verify it fails**

Run: `cargo test main:: -- --nocapture`
Expected: FAIL because metrics helpers and error classification do not exist.

- [ ] **Step 3: Write minimal implementation for error classification**

```rust
impl ProxyError {
    pub fn stats_error_kind(&self) -> &'static str {
        match self {
            ProxyError::UnauthorizedBucket(_) => "unauthorized_bucket",
            ProxyError::S3Error(_) => "origin",
            ProxyError::InternalError(_) | ProxyError::ConfigError(_) | ProxyError::KvError(_) => {
                "internal"
            }
            ProxyError::HttpError(_) | ProxyError::InvalidPath(_) => "internal",
        }
    }
}
```

- [ ] **Step 4: Write minimal implementation for success-path metrics**

```rust
fn successful_response_bytes(
    cache_status: CacheStatus,
    headers: &CachedHeaders,
    streamed_bytes: u64,
) -> u64 {
    match cache_status {
        CacheStatus::Hit => headers.content_length.unwrap_or(streamed_bytes),
        CacheStatus::Miss => streamed_bytes,
        CacheStatus::Bypass | CacheStatus::Disabled => streamed_bytes,
    }
}

fn stats_cache_status(status: CacheStatus) -> StatsCacheStatus {
    match status {
        CacheStatus::Hit => StatsCacheStatus::Hit,
        CacheStatus::Miss => StatsCacheStatus::Miss,
        CacheStatus::Bypass => StatsCacheStatus::Bypass,
        CacheStatus::Disabled => StatsCacheStatus::Disabled,
    }
}
```

- [ ] **Step 5: Integrate the recorder into `main`**

```rust
let stats_store = StatsStore::new(config.local_cache.clone().map(|c| c.redis_url), config.local_cache.clone().map(|c| c.redis_key_prefix))?;

let app = Router::new()
    .route("/", get(proxy_handler))
    .route("/", options(handle_options))
    .route("/*path", get(proxy_handler))
    .route("/*path", options(handle_options))
    .with_state(AppState {
        s3_client,
        local_cache,
        stats_store: stats_store.clone(),
        cache: cache.clone(),
    });
```

And in `proxy_handler`:

```rust
state.stats_store
    .record(StatsEvent {
        timestamp: unix_timestamp(),
        bucket: bucket.to_string(),
        path_and_query: uri.path_and_query().map(|v| v.as_str()).unwrap_or(uri.path()).to_string(),
        object_key: Some(object_key.clone()),
        bytes,
        cache_status: stats_cache_status(cache_status),
        result: StatsResult::Success,
    })
    .await;
```

Error path wrapper:

```rust
match proxy_handler_inner(uri, state.clone(), host.clone()).await {
    Ok(response) => Ok(response),
    Err(error) => {
        state.stats_store
            .record(StatsEvent {
                timestamp: unix_timestamp(),
                bucket: bucket_for_error,
                path_and_query,
                object_key: None,
                bytes: 0,
                cache_status: StatsCacheStatus::Disabled,
                result: StatsResult::Error,
            })
            .await;
        Err(error)
    }
}
```

- [ ] **Step 6: Run test to verify it passes**

Run: `cargo test main:: -- --nocapture`
Expected: PASS

- [ ] **Step 7: Commit**

```bash
git add src/errors.rs src/main.rs src/s3_client.rs src/stats.rs
git commit -m "feat: record proxy stats into redis"
```

## Task 4: Expose Local Cache Usage And Status JSON APIs

**Files:**
- Modify: `src/local_cache.rs`
- Create: `src/status_server.rs`
- Modify: `src/stats.rs`
- Test: `src/local_cache.rs`
- Test: `src/status_server.rs`

- [ ] **Step 1: Write the failing tests**

```rust
#[test]
fn cache_usage_is_disabled_when_local_cache_is_off() {
    let cache = LocalCache::new(None);
    let usage = tokio_test::block_on(async { cache.await.usage().await });

    assert!(!usage.enabled);
    assert_eq!(usage.used_bytes, 0);
    assert_eq!(usage.capacity_bytes, 0);
}

#[tokio::test]
async fn rejects_status_api_requests_without_api_key() {
    let app = build_status_router(StatusState::for_test("secret"));

    let response = app
        .oneshot(
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
    let app = build_status_router(StatusState::for_test("secret"));

    let response = app
        .oneshot(
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
```

- [ ] **Step 2: Run test to verify it fails**

Run: `cargo test status_server:: -- --nocapture`
Expected: FAIL because the status server module, auth middleware, and cache usage API do not exist.

- [ ] **Step 3: Write minimal local cache usage support**

```rust
#[derive(Clone, Copy, Debug, Default)]
pub struct LocalCacheUsage {
    pub enabled: bool,
    pub used_bytes: u64,
    pub capacity_bytes: u64,
}

impl LocalCache {
    pub async fn usage(&self) -> LocalCacheUsage {
        let Some(inner) = &self.inner else {
            return LocalCacheUsage::default();
        };

        let mut connection = match inner.redis_client.get_multiplexed_async_connection().await {
            Ok(connection) => connection,
            Err(_) => {
                return LocalCacheUsage {
                    enabled: true,
                    used_bytes: 0,
                    capacity_bytes: inner.max_size_bytes,
                };
            }
        };

        let used_bytes: Option<u64> = connection.get(inner.redis_total_size_key()).await.ok();

        LocalCacheUsage {
            enabled: true,
            used_bytes: used_bytes.unwrap_or(0),
            capacity_bytes: inner.max_size_bytes,
        }
    }
}
```

- [ ] **Step 4: Write minimal status API implementation**

```rust
#[derive(Clone)]
pub struct StatusState {
    pub api_key: Arc<String>,
    pub stats_store: StatsStore,
    pub local_cache: LocalCache,
    pub buckets: Arc<RwLock<AppCache>>,
}

pub fn build_status_router(state: StatusState) -> Router {
    Router::new()
        .route("/", get(index_html))
        .route("/api/login", post(login))
        .route("/api/filters", get(filters))
        .route("/api/overview", get(overview))
        .route("/api/timeseries", get(timeseries))
        .route("/api/top", get(top))
        .with_state(state)
}
```

Auth check:

```rust
fn is_valid_api_key(headers: &HeaderMap, expected: &str) -> bool {
    headers
        .get("x-status-api-key")
        .and_then(|value| value.to_str().ok())
        .map(|value| value == expected)
        .unwrap_or(false)
}
```

- [ ] **Step 5: Run test to verify it passes**

Run: `cargo test status_server:: -- --nocapture`
Expected: PASS

- [ ] **Step 6: Commit**

```bash
git add src/local_cache.rs src/status_server.rs src/stats.rs
git commit -m "feat: add protected status json apis"
```

## Task 5: Serve The Embedded Dashboard Frontend

**Files:**
- Create: `src/status_assets.rs`
- Modify: `src/status_server.rs`
- Test: `src/status_server.rs`

- [ ] **Step 1: Write the failing tests**

```rust
#[tokio::test]
async fn serves_dashboard_shell() {
    let app = build_status_router(StatusState::for_test("secret"));

    let response = app
        .oneshot(
            Request::builder()
                .uri("/")
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();

    assert_eq!(response.status(), StatusCode::OK);
    assert_eq!(
        response.headers().get(header::CONTENT_TYPE).unwrap(),
        "text/html; charset=utf-8"
    );
}
```

- [ ] **Step 2: Run test to verify it fails**

Run: `cargo test status_server::serves_dashboard_shell -- --nocapture`
Expected: FAIL because no embedded dashboard assets exist.

- [ ] **Step 3: Create the embedded assets**

```rust
pub const INDEX_HTML: &str = include_str!("../status/index.html");
pub const APP_CSS: &str = include_str!("../status/app.css");
pub const APP_JS: &str = include_str!("../status/app.js");

pub fn asset(path: &str) -> Option<(&'static str, &'static [u8])> {
    match path {
        "/" | "/index.html" => Some(("text/html; charset=utf-8", INDEX_HTML.as_bytes())),
        "/assets/app.css" => Some(("text/css; charset=utf-8", APP_CSS.as_bytes())),
        "/assets/app.js" => Some(("application/javascript; charset=utf-8", APP_JS.as_bytes())),
        _ => None,
    }
}
```

- [ ] **Step 4: Build the frontend shell and dashboard logic**

```html
<body>
  <div id="app" class="shell">
    <section id="login-panel"></section>
    <section id="dashboard" hidden></section>
  </div>
  <script src="/assets/app.js" defer></script>
</body>
```

```js
const storageKey = "r2_proxy_status_api_key";

async function login(apiKey) {
  const response = await fetch("/api/login", {
    method: "POST",
    headers: { "content-type": "application/json" },
    body: JSON.stringify({ apiKey }),
  });
  if (!response.ok) throw new Error("invalid api key");
  localStorage.setItem(storageKey, apiKey);
}
```

- [ ] **Step 5: Run test to verify it passes**

Run: `cargo test status_server::serves_dashboard_shell -- --nocapture`
Expected: PASS

- [ ] **Step 6: Commit**

```bash
git add src/status_assets.rs src/status_server.rs status/index.html status/app.css status/app.js
git commit -m "feat: add embedded status dashboard frontend"
```

## Task 6: Start The Second Listener And Wire The End-To-End Status Flow

**Files:**
- Modify: `src/main.rs`
- Modify: `src/status_server.rs`
- Modify: `src/stats.rs`
- Test: `src/main.rs`

- [ ] **Step 1: Write the failing tests**

```rust
#[test]
fn parses_status_socket_addr() {
    let addr = status_socket_addr("127.0.0.1", 3001).unwrap();
    assert_eq!(addr.to_string(), "127.0.0.1:3001");
}
```

- [ ] **Step 2: Run test to verify it fails**

Run: `cargo test main::parses_status_socket_addr -- --nocapture`
Expected: FAIL because the status listener helper does not exist.

- [ ] **Step 3: Write minimal implementation**

```rust
fn status_socket_addr(host: &str, port: u16) -> anyhow::Result<SocketAddr> {
    format!("{host}:{port}")
        .parse::<SocketAddr>()
        .map_err(|error| anyhow::anyhow!("invalid status bind address: {error}"))
}
```

Spawn both listeners:

```rust
let proxy_listener = tokio::net::TcpListener::bind(proxy_addr).await?;
let status_listener = tokio::net::TcpListener::bind(status_addr).await?;

let proxy_server = axum::serve(proxy_listener, app);
let status_server = axum::serve(status_listener, status_app);

tokio::try_join!(proxy_server, status_server)?;
```

- [ ] **Step 4: Run test to verify it passes**

Run: `cargo test main::parses_status_socket_addr -- --nocapture`
Expected: PASS

- [ ] **Step 5: Run the full test suite**

Run: `cargo test`
Expected: PASS

- [ ] **Step 6: Commit**

```bash
git add src/main.rs src/status_server.rs src/stats.rs
git commit -m "feat: launch status server alongside proxy"
```

## Task 7: Document Deployment, Usage, And Open A Reviewable PR

**Files:**
- Modify: `README.md`

- [ ] **Step 1: Update configuration docs**

Add:

```env
STATUS_PORT=3001
STATUS_HOST=127.0.0.1
STATUS_API_KEY=change-me
```

And explain:

```md
- The binary starts the proxy port and a separate status port together
- The status server binds to `127.0.0.1` by default
- The dashboard stores the API key in `localStorage` after a successful login
- Redis is required for both local cache metadata and status metrics
```

- [ ] **Step 2: Update deployment examples**

Add examples like:

```bash
cargo run
```

```bash
curl -X POST http://127.0.0.1:3001/api/login \
  -H 'content-type: application/json' \
  -d '{"apiKey":"change-me"}'
```

```bash
docker run -d \
  -p 3000:3000 \
  -p 3001:3001 \
  -e STATUS_API_KEY=change-me \
  --name r2-proxy \
  delbertbeta/r2-proxy:latest
```

- [ ] **Step 3: Verify formatting and behavior**

Run: `cargo test`
Expected: PASS

Run: `cargo run`
Expected: logs include both listeners, such as `server listening` and `status server listening`

- [ ] **Step 4: Commit**

```bash
git add README.md
git commit -m "docs: describe status dashboard deployment"
```

- [ ] **Step 5: Push branch and open PR**

Run:

```bash
git push -u origin feat/status-page-dashboard
gh pr create --fill
```

Expected:

- branch is pushed
- a PR URL is returned for review

## Self-Review

### Spec coverage

- Second listener with configurable host/port: Task 1 and Task 6
- Redis-backed totals, windows, and top lists: Task 2 and Task 3
- Error totals, error series, and top error URLs: Task 2 and Task 3
- API-key-protected status APIs: Task 4
- Embedded frontend with `localStorage` login: Task 5
- Local cache usage reporting: Task 4
- README deployment and usage updates: Task 7
- GitHub PR creation: Task 7

No spec gaps remain.

### Placeholder scan

- No `TODO`, `TBD`, or "implement later" placeholders remain.
- Every task includes concrete files, commands, and code examples.

### Type consistency

- `StatsScope`, `StatsStore`, `StatsEvent`, `StatsCacheStatus`, and `StatsResult` are introduced in Task 2 and reused consistently later.
- `StatusState` and `build_status_router` are introduced in Task 4 before Task 5 and Task 6 depend on them.
- `successful_response_bytes` and `status_socket_addr` are defined before later integration steps refer to them.
