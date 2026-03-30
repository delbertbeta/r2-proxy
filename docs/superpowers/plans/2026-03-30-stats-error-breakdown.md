---
title: 2026-03-30-stats-error-breakdown
type: note
permalink: work/r2-proxy/docs/superpowers/plans/2026-03-30-stats-error-breakdown-1
---

# Stats Error Breakdown Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Update the stats pipeline and status dashboard so 404 object-miss traffic and 5xx failures are tracked and displayed separately while preserving total error metrics.

**Architecture:** Extend the stats event model to classify errors as `NotFound` or `ServerError`, persist aggregate and per-class counters in Redis, then expose the new data through the status API and render it in the dashboard. Keep total error fields intact so existing behavior remains understandable while adding 404/5xx breakdowns.

**Tech Stack:** Rust, Axum, Redis, serde, vanilla JavaScript, existing status dashboard assets

---

## File Map

- Modify: `src/errors.rs`
  - Add a stats-facing error classification helper and tests for 404 vs 5xx categorization.
- Modify: `src/main.rs`
  - Record classified error events instead of a single generic error result.
- Modify: `src/stats.rs`
  - Extend totals, result types, Redis counters, top-error key handling, and unit tests.
- Modify: `src/status_server.rs`
  - Return 404/5xx counts and rates in overview/timeseries/top responses.
- Modify: `status/app.js`
  - Render overview, summary, charts, and top lists with separate 404 and 5xx data.
- Modify: `status/index.html`
  - Split the top error section into two panels.
- Test: `cargo test`

### Task 1: Classify Errors in the Backend Domain Model

**Files:**
- Modify: `src/errors.rs`
- Modify: `src/stats.rs`
- Test: `src/errors.rs`
- Test: `src/stats.rs`

- [ ] **Step 1: Write the failing tests for error classification**

Add tests that lock the new stats categories before changing implementation.

```rust
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

#[test]
fn stats_results_report_404_and_5xx_buckets() {
    assert!(StatsResult::NotFound.is_error());
    assert!(StatsResult::NotFound.is_not_found());
    assert!(!StatsResult::NotFound.is_server_error());

    assert!(StatsResult::ServerError.is_error());
    assert!(!StatsResult::ServerError.is_not_found());
    assert!(StatsResult::ServerError.is_server_error());

    assert!(!StatsResult::Success.is_error());
}
```

- [ ] **Step 2: Run the targeted tests to verify they fail**

Run: `cargo test classifies_proxy_errors_for_stats_breakdown stats_results_report_404_and_5xx_buckets`

Expected: compile or assertion failures because `StatsResult::NotFound`, `StatsResult::ServerError`, and `ProxyError::stats_result()` do not exist yet.

- [ ] **Step 3: Write the minimal implementation for classified stats results**

Update `src/stats.rs` so `StatsResult` carries separate 404 and 5xx states plus helper methods:

```rust
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum StatsResult {
    Success,
    NotFound,
    ServerError,
}

impl StatsResult {
    pub fn is_error(self) -> bool {
        !matches!(self, Self::Success)
    }

    pub fn is_not_found(self) -> bool {
        matches!(self, Self::NotFound)
    }

    pub fn is_server_error(self) -> bool {
        matches!(self, Self::ServerError)
    }
}
```

Update `src/errors.rs` to map proxy errors into stats results:

```rust
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
```

Keep `stats_error_kind()` unchanged unless a later task no longer needs it.

- [ ] **Step 4: Run the targeted tests to verify they pass**

Run: `cargo test classifies_proxy_errors_for_stats_breakdown stats_results_report_404_and_5xx_buckets`

Expected: both tests pass.

- [ ] **Step 5: Commit the domain-model change**

```bash
git add src/errors.rs src/stats.rs
git commit -m "Classify stats errors as 404 or 5xx"
```

### Task 2: Persist 404 and 5xx Counters in Redis Stats

**Files:**
- Modify: `src/stats.rs`
- Test: `src/stats.rs`

- [ ] **Step 1: Write the failing tests for totals and Redis key layout**

Add tests covering the new totals fields and daily top-key naming:

```rust
#[test]
fn bucket_totals_defaults_new_error_counters_to_zero() {
    let totals = bucket_totals_from_hash(HashMap::new());

    assert_eq!(totals.errors, 0);
    assert_eq!(totals.not_found_errors, 0);
    assert_eq!(totals.server_errors, 0);
}

#[test]
fn builds_daily_top_error_keys_for_404_and_5xx() {
    let redis = RedisConfig {
        redis_url: "redis://127.0.0.1:6379".to_string(),
        redis_key_prefix: "r2proxy".to_string(),
    };
    let store = StatsStore::new(&redis).expect("stats store");
    let scope = StatsScope::Bucket("foo".to_string());

    assert_eq!(
        store.daily_top_not_found_errors_key(&scope, 1_711_753_499),
        "r2proxy:stats:top:errors_404:bucket:foo:2024_03_29"
    );
    assert_eq!(
        store.daily_top_server_errors_key(&scope, 1_711_753_499),
        "r2proxy:stats:top:errors_5xx:bucket:foo:2024_03_29"
    );
}
```

- [ ] **Step 2: Run the targeted tests to verify they fail**

Run: `cargo test bucket_totals_defaults_new_error_counters_to_zero builds_daily_top_error_keys_for_404_and_5xx`

Expected: failures because `not_found_errors`, `server_errors`, and the new key helpers do not exist yet.

- [ ] **Step 3: Extend totals, rate helpers, and top-key helpers**

Update `BucketTotals` in `src/stats.rs`:

```rust
#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub struct BucketTotals {
    pub requests: u64,
    pub bytes: u64,
    pub cache_hits: u64,
    pub cache_misses: u64,
    pub errors: u64,
    pub not_found_errors: u64,
    pub server_errors: u64,
}

impl BucketTotals {
    pub fn error_rate(self) -> f64 { /* keep current total calculation */ }

    pub fn not_found_error_rate(self) -> f64 {
        if self.requests == 0 { 0.0 } else { self.not_found_errors as f64 / self.requests as f64 }
    }

    pub fn server_error_rate(self) -> f64 {
        if self.requests == 0 { 0.0 } else { self.server_errors as f64 / self.requests as f64 }
    }
}
```

Update hash decoding:

```rust
fn bucket_totals_from_hash(values: HashMap<String, u64>) -> BucketTotals {
    BucketTotals {
        requests: values.get("requests").copied().unwrap_or(0),
        bytes: values.get("bytes").copied().unwrap_or(0),
        cache_hits: values.get("cache_hits").copied().unwrap_or(0),
        cache_misses: values.get("cache_misses").copied().unwrap_or(0),
        errors: values.get("errors").copied().unwrap_or(0),
        not_found_errors: values.get("errors_404").copied().unwrap_or(0),
        server_errors: values.get("errors_5xx").copied().unwrap_or(0),
    }
}
```

Add separate key helpers and readers:

```rust
pub async fn read_top_not_found_errors(
    &self,
    scope: &StatsScope,
    end_timestamp: u64,
    limit: usize,
) -> Result<Vec<(String, u64)>, redis::RedisError> {
    self.read_recent_top_entries("errors_404", scope, end_timestamp, limit).await
}

pub async fn read_top_server_errors(
    &self,
    scope: &StatsScope,
    end_timestamp: u64,
    limit: usize,
) -> Result<Vec<(String, u64)>, redis::RedisError> {
    self.read_recent_top_entries("errors_5xx", scope, end_timestamp, limit).await
}
```

- [ ] **Step 4: Update stats recording to write the new counters**

In both totals and time-bucket Redis pipelines, increment the new fields based on helper methods:

```rust
.cmd("HINCRBY")
.arg(&totals_key)
.arg("errors")
.arg(if event.result.is_error() { 1 } else { 0 })
.cmd("HINCRBY")
.arg(&totals_key)
.arg("errors_404")
.arg(if event.result.is_not_found() { 1 } else { 0 })
.cmd("HINCRBY")
.arg(&totals_key)
.arg("errors_5xx")
.arg(if event.result.is_server_error() { 1 } else { 0 })
```

Split top-error writes:

```rust
if event.result.is_not_found() {
    let key = self.daily_top_not_found_errors_key(&scope, event.timestamp);
    // zincrby with member
}

if event.result.is_server_error() {
    let key = self.daily_top_server_errors_key(&scope, event.timestamp);
    // zincrby with member
}
```

- [ ] **Step 5: Run the focused stats tests**

Run: `cargo test bucket_totals_defaults_new_error_counters_to_zero builds_daily_top_error_keys_for_404_and_5xx`

Expected: both tests pass.

- [ ] **Step 6: Run the full stats test subset**

Run: `cargo test stats::tests`

Expected: all stats tests pass with updated assertions for new counters and keys.

- [ ] **Step 7: Commit the Redis stats changes**

```bash
git add src/stats.rs
git commit -m "Store 404 and 5xx stats separately"
```

### Task 3: Record Classified Errors From Request Handling

**Files:**
- Modify: `src/main.rs`
- Test: `src/errors.rs`
- Test: `src/stats.rs`

- [ ] **Step 1: Write the failing test for request-side classification usage**

Add a small test in `src/errors.rs` or `src/stats.rs` that verifies the request layer can use `ProxyError::stats_result()` without collapsing 404s into generic errors:

```rust
#[test]
fn object_not_found_errors_record_not_found_stats_result() {
    let result = ProxyError::ObjectNotFound("foo".to_string()).stats_result();
    assert_eq!(result, StatsResult::NotFound);
}
```

If an equivalent test already exists from Task 1, reuse it and skip creating a duplicate.

- [ ] **Step 2: Run the targeted test to verify red/green coverage**

Run: `cargo test object_not_found_errors_record_not_found_stats_result`

Expected: pass if already covered by Task 1, otherwise fail until the helper is wired in.

- [ ] **Step 3: Update request error recording in `src/main.rs`**

Change `record_error_event` to accept the concrete error result instead of hardcoding `StatsResult::Error`:

```rust
async fn record_error_event(
    state: &AppState,
    bucket: &str,
    path_and_query: String,
    result: StatsResult,
) {
    state
        .stats_store
        .record(StatsEvent {
            timestamp: current_unix_timestamp(),
            bucket: bucket.to_string(),
            path_and_query,
            object_key: None,
            bytes: 0,
            cache_status: StatsCacheStatus::Disabled,
            result,
        })
        .await;
}
```

Update call sites to pass `error.stats_result()`:

```rust
let error = ProxyError::UnauthorizedBucket(bucket.to_string());
record_error_event(&state, bucket, request_path_and_query.clone(), error.stats_result()).await;
return Err(error);
```

Repeat for all branches that currently call `record_error_event`.

- [ ] **Step 4: Run the main test suite**

Run: `cargo test`

Expected: request-layer compilation succeeds and existing tests remain green.

- [ ] **Step 5: Commit the request recording change**

```bash
git add src/main.rs src/errors.rs src/stats.rs
git commit -m "Record 404 and 5xx request errors separately"
```

### Task 4: Expose 404 and 5xx Breakdowns in the Status API

**Files:**
- Modify: `src/status_server.rs`
- Test: `src/status_server.rs`

- [ ] **Step 1: Write the failing API-shape tests**

Add tests around the JSON helpers instead of full end-to-end Redis-backed responses:

```rust
#[test]
fn totals_response_includes_404_and_5xx_fields() {
    let totals = BucketTotals {
        requests: 100,
        bytes: 2048,
        cache_hits: 40,
        cache_misses: 10,
        errors: 7,
        not_found_errors: 5,
        server_errors: 2,
    };

    let body = serde_json::to_value(totals_response(totals)).expect("json");

    assert_eq!(body["errors"], 7);
    assert_eq!(body["notFoundErrors"], 5);
    assert_eq!(body["serverErrors"], 2);
    assert_eq!(body["notFoundErrorRate"], 0.05);
    assert_eq!(body["serverErrorRate"], 0.02);
}
```

Also add a top-response serialization test for `notFoundUrls` and `serverErrorUrls`.

- [ ] **Step 2: Run the targeted tests to verify they fail**

Run: `cargo test totals_response_includes_404_and_5xx_fields`

Expected: failures because the response structs do not yet have the new fields.

- [ ] **Step 3: Extend the status API response structs and helpers**

Update `src/status_server.rs` response structs:

```rust
struct TotalsResponse {
    requests: u64,
    bytes: u64,
    #[serde(rename = "cacheHitRate")]
    cache_hit_rate: f64,
    errors: u64,
    #[serde(rename = "errorRate")]
    error_rate: f64,
    #[serde(rename = "notFoundErrors")]
    not_found_errors: u64,
    #[serde(rename = "notFoundErrorRate")]
    not_found_error_rate: f64,
    #[serde(rename = "serverErrors")]
    server_errors: u64,
    #[serde(rename = "serverErrorRate")]
    server_error_rate: f64,
}
```

Mirror those fields in `SummaryResponse`, and add these series vectors:

```rust
#[serde(rename = "notFoundErrorRate")]
not_found_error_rate: Vec<DataPoint>,
#[serde(rename = "serverErrorRate")]
server_error_rate: Vec<DataPoint>,
```

Update `totals_response()` and timeseries summary/series construction to populate them from `BucketTotals`.

- [ ] **Step 4: Split top error data into two response lists**

Replace the single `error_urls` response field with:

```rust
#[serde(rename = "notFoundUrls")]
not_found_urls: Vec<UrlMetricEntry>,
#[serde(rename = "serverErrorUrls")]
server_error_urls: Vec<UrlMetricEntry>,
```

Read the two new Redis leaderboards:

```rust
state.stats_store.read_top_not_found_errors(&scope, now, 10).await
state.stats_store.read_top_server_errors(&scope, now, 10).await
```

Populate entries like:

```rust
UrlMetricEntry {
    bucket,
    url,
    misses: None,
    errors: Some(count),
}
```

Reuse the existing `errors` field on `UrlMetricEntry` to minimize front-end shape churn.

- [ ] **Step 5: Run the targeted status-server tests**

Run: `cargo test totals_response_includes_404_and_5xx_fields`

Expected: the new serialization test passes.

- [ ] **Step 6: Run the full status API test subset**

Run: `cargo test status_server::tests`

Expected: all status server tests pass after updating any JSON assertions affected by the new fields.

- [ ] **Step 7: Commit the API changes**

```bash
git add src/status_server.rs
git commit -m "Expose 404 and 5xx metrics in status API"
```

### Task 5: Render the 404 and 5xx Breakdown in the Dashboard

**Files:**
- Modify: `status/index.html`
- Modify: `status/app.js`

- [ ] **Step 1: Write down the expected frontend payload shape in code comments or constants**

Add a small constant block near the top of `status/app.js` to keep property names consistent:

```javascript
const errorMetricKeys = {
  totalRate: "errorRate",
  notFoundRate: "notFoundErrorRate",
  serverRate: "serverErrorRate",
};
```

This is not a placeholder task; it prevents inconsistent field naming while updating multiple render paths.

- [ ] **Step 2: Update the HTML layout for split error lists**

In `status/index.html`, replace the single error URLs container with two containers:

```html
<article class="panel">
  <div class="panel-heading">
    <div>
      <p class="eyebrow">404s</p>
      <h3>Top 404 URLs</h3>
    </div>
  </div>
  <div id="not-found-urls"></div>
</article>
<article class="panel">
  <div class="panel-heading">
    <div>
      <p class="eyebrow">5xx</p>
      <h3>Top 5xx URLs</h3>
    </div>
  </div>
  <div id="server-error-urls"></div>
</article>
```

- [ ] **Step 3: Update overview and summary rendering**

In `status/app.js`, update `renderMetrics()` and `renderSummary()`:

```javascript
metricCard(
  "Errors",
  formatCompact(overview.totals.errors),
  `404 ${formatCompact(overview.totals.notFoundErrors)} · 5xx ${formatCompact(overview.totals.serverErrors)}`
)
```

```javascript
["Error Rate", formatRate(summary.errorRate)],
["404 Rate", formatRate(summary.notFoundErrorRate)],
["5xx Rate", formatRate(summary.serverErrorRate)],
```

- [ ] **Step 4: Replace the single error chart with 404 and 5xx charts**

Update `renderCharts()` to produce two chart cards and Apex series:

```javascript
chartCardMarkup("chart-404-rate", "404 Rate", series.notFoundErrorRate, formatRate),
chartCardMarkup("chart-5xx-rate", "5xx Rate", series.serverErrorRate, formatRate),
```

Create matching chart configs:

```javascript
{
  id: "chart-404-rate",
  color: "#c0841a",
  points: series.notFoundErrorRate,
  formatter: formatRate,
},
{
  id: "chart-5xx-rate",
  color: "#b4432f",
  points: series.serverErrorRate,
  formatter: formatRate,
},
```

Remove the old single `chart-error-rate` references.

- [ ] **Step 5: Render separate 404 and 5xx top lists**

Update DOM lookups and refresh rendering:

```javascript
const notFoundUrls = document.getElementById("not-found-urls");
const serverErrorUrls = document.getElementById("server-error-urls");
```

```javascript
renderTable(notFoundUrls, top.notFoundUrls, "errors");
renderTable(serverErrorUrls, top.serverErrorUrls, "errors");
```

Delete the old single `errorUrls` usage.

- [ ] **Step 6: Manually sanity-check the dashboard payload flow**

Run: `cargo test`

Expected: backend still passes after the frontend-oriented changes because static assets are compiled into tests.

Then inspect the updated files for naming consistency:

Run: `rg -n "errorUrls|notFoundUrls|serverErrorUrls|notFoundErrorRate|serverErrorRate" status src/status_server.rs`

Expected: only the new names remain where intended.

- [ ] **Step 7: Commit the dashboard changes**

```bash
git add status/index.html status/app.js src/status_server.rs
git commit -m "Show separate 404 and 5xx metrics on dashboard"
```

### Task 6: Final Verification and Cleanup

**Files:**
- Modify: any files touched above if verification reveals mismatches

- [ ] **Step 1: Run the complete test suite**

Run: `cargo test`

Expected: all tests pass with zero failures.

- [ ] **Step 2: Review the final diff for scope drift**

Run: `git diff --stat master...HEAD`

Expected: only the stats, status API, status frontend, and related tests/spec/plan files are included.

- [ ] **Step 3: Review changed symbols for consistency**

Run: `rg -n "StatsResult::Error|error_urls|chart-error-rate|read_top_errors|errors_404|errors_5xx" src status`

Expected:
- no remaining `StatsResult::Error`
- no stale single-error UI identifiers unless intentionally preserved
- new Redis counter names appear in the stats layer

- [ ] **Step 4: Commit any final fixups**

```bash
git add src/errors.rs src/main.rs src/stats.rs src/status_server.rs status/index.html status/app.js
git commit -m "Polish stats error breakdown implementation"
```

Only do this step if verification required actual code changes. If there are no changes, skip this commit.