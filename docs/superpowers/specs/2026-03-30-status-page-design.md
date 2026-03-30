---
title: 2026-03-30-status-page-design
type: note
permalink: work/r2-proxy/docs/superpowers/specs/2026-03-30-status-page-design
---

# Status Page Design

**Goal:** Add a Redis-backed status service that starts alongside the proxy on a separate port, serves a protected operational dashboard, and exposes aggregate and recent-window metrics for requests, traffic, cache effectiveness, local cache usage, and proxy errors.

**Architecture:** Run a second Axum server in the same Rust process. The existing proxy request path emits metrics events into a Redis-backed stats store. The new status server serves an embedded single-page frontend plus authenticated JSON APIs. Long-lived totals are stored cumulatively, while recent analytics are stored in rolling `5m`, `1h`, and `1d` buckets and retained for 7 days. Dashboard filtering is supported for `global` and per-virtual-`bucket` scopes.

**Tech Stack:** Rust, Axum, Tokio, Redis, Serde, embedded static frontend assets, browser `localStorage`

## Product Requirements

- Start a second HTTP listener when the proxy boots.
- Status listener defaults to `127.0.0.1:<STATUS_PORT>` and can be rebound with environment variables.
- Serve a single-page dashboard from the status listener.
- Require an API key for all dashboard data APIs.
- Prompt for the API key in the browser on first visit and persist it in `localStorage`.
- Show all-time totals for:
  - total requests
  - total served bytes
  - total cache hit rate
  - total errors and error rate
  - local cache used bytes and usage rate
- Show recent analytics for `1h`, `24h`, and `7d` windows:
  - request QPS
  - traffic throughput
  - cache hit rate
  - error rate
  - window-specific summary totals
- Show top lists for the last 7 days:
  - top 10 hottest cached files by cache hit count
  - top 10 miss-heavy request URLs by cache miss count
  - top 10 error-heavy request URLs by error count
- Support dashboard filtering by virtual bucket, with a default global view.
- Update `README.md` with configuration, deployment, and usage guidance.

## Scope Decisions

### Included

- Global metrics and per-bucket filtered metrics
- Redis-backed counters, sorted sets, and rolling bucket storage
- Embedded frontend served by the Rust status service
- API-key-protected JSON endpoints
- Separate status listener with configurable bind host and port

### Excluded

- Host-based filtering
- Cookie or session authentication
- Historical retention beyond the recent 7-day analytics window
- Error-type drilldowns in the first version beyond recording an internal classification for logs and future expansion

## Runtime Architecture

### Process Layout

The binary starts both services from `main`:

1. Proxy server on `PORT`
2. Status server on `STATUS_PORT`

Shared application state is expanded to include:

- existing proxy dependencies
- a `StatsRecorder` used by the proxy path
- a `StatsReader` or shared `StatsStore` used by the status APIs

Both services share the same Redis connection factory so operational data and local cache metadata live in Redis without duplicating configuration surfaces.

### New Modules

- `src/stats.rs`
  - metrics domain types
  - Redis key generation
  - event recording
  - aggregated reads for overview, timeseries, and top lists
- `src/status_server.rs`
  - status router construction
  - API-key middleware
  - JSON handlers
  - static asset responses
- `src/status_assets.rs` or embedded asset module
  - generated or checked-in frontend asset strings/bytes

Existing modules updated:

- `src/config.rs`
  - new status-service configuration
- `src/main.rs`
  - initialize stats store
  - spawn second listener
  - record request outcomes
- `src/local_cache.rs`
  - expose current cache usage helpers needed by overview APIs

## Configuration

### New Environment Variables

```env
STATUS_PORT=3001
STATUS_HOST=127.0.0.1
STATUS_API_KEY=change-me
```

### Behavior

- `STATUS_PORT` defaults to `3001`
- `STATUS_HOST` defaults to `127.0.0.1`
- `STATUS_API_KEY` is required because the status server always starts when config parsing succeeds
- Local cache usage metrics degrade gracefully:
  - if local cache is disabled, usage values return `0` used, `0` rate, and a `disabled` flag

## Metrics Model

### Request Outcome Definitions

Every proxy request is categorized along two axes:

1. Request result
  - `success`
  - `error`

2. Cache participation
  - `hit`
  - `miss`
  - `bypass`
  - `disabled`

Definitions:

- `hit`: object served from local cache
- `miss`: local cache was consulted, object absent, origin served successfully
- `bypass`: request intentionally skipped local caching rules
- `disabled`: local cache unavailable or disabled
- `error`: request failed before completing a successful response, including authorization failures, origin failures, internal failures, and stream failures

### Metric Inclusion Rules

- `total requests`: incremented for every request reaching the proxy handler, including requests that later fail
- `total served bytes`: incremented only for successful responses, using actual output bytes
- `cache hit rate`:
  - numerator: `hit`
  - denominator: `hit + miss`
  - `bypass` and `disabled` are excluded
- `error count`: incremented for every failed request
- `error rate`:
  - numerator: errors
  - denominator: total requests

### Time Windows And Bucket Sizes

- `1h` view uses native `5m` buckets
- `24h` view uses native `1h` buckets
- `7d` view uses native `1d` buckets

The system records each request into all three resolutions at write time. No graph fabricates finer-grained points from coarser buckets.

### Retention

- All-time totals are cumulative and do not expire
- Rolling bucket metrics retain 7 days of data
- Top lists retain 7 days of data

## Redis Data Design

Assume `REDIS_KEY_PREFIX` remains the namespace root. Stats keys add a `stats:` subtree to avoid collisions with cache metadata.

### Scope Naming

- global scope: `global`
- bucket scope: `bucket:<virtual_bucket>`

All metric keys are duplicated for:

- `global`
- the resolved virtual bucket for the request

### All-Time Totals

Redis hashes:

- `{prefix}:stats:totals:{scope}`

Fields:

- `requests`
- `bytes`
- `cache_hits`
- `cache_misses`
- `errors`

### Rolling Time Buckets

One hash per scope, resolution, and bucket timestamp:

- `{prefix}:stats:ts:{resolution}:{scope}:{bucket_start}`

Resolutions:

- `5m`
- `1h`
- `1d`

Fields:

- `requests`
- `bytes`
- `cache_hits`
- `cache_misses`
- `errors`

TTL:

- Each bucket key expires after slightly more than 7 days so reads still succeed around boundary conditions.
- Suggested TTL:
  - `5m`: 8 days
  - `1h`: 8 days
  - `1d`: 10 days

### Top Lists

Member format:

- cached hits: `<bucket>|<object_key>`
- misses/errors: `<bucket>|<url_path_and_query>`

Retention strategy:

- Maintain a companion per-day sorted set so 7-day reads can union recent days without carrying infinite history.
- Daily keys:
  - `{prefix}:stats:top:hits:{scope}:{yyyy_mm_dd}`
  - `{prefix}:stats:top:misses:{scope}:{yyyy_mm_dd}`
  - `{prefix}:stats:top:errors:{scope}:{yyyy_mm_dd}`
- Read path unions the latest 7 daily sets into a temporary sorted set, reads top 10, then deletes the temporary key.

This keeps the top lists aligned with the "recent 7 days only" requirement instead of drifting into all-time rankings.

### Bucket Filter List

Bucket options for the dashboard can be read from the existing in-memory whitelist cache. No extra Redis structure is needed.

## Recording Flow

### Success Path

For a successful request, record:

- `requests += 1`
- `bytes += actual_served_bytes`
- `cache_hits += 1` if status is `hit`
- `cache_misses += 1` if status is `miss`
- top hits entry if status is `hit`
- top misses entry if status is `miss`
- all three rolling bucket resolutions
- both `global` and `bucket:<virtual_bucket>` scopes

### Error Path

For a failed request, record:

- `requests += 1`
- `errors += 1`
- no bytes increment
- top errors entry for the request URL
- all three rolling bucket resolutions
- both `global` and `bucket:<virtual_bucket>` scopes

### Streamed Responses

Miss responses are often streamed from origin. The implementation must track actual emitted bytes, not only upstream `content-length`.

Design:

- Wrap the response body stream with a lightweight byte-counting layer
- On normal stream completion:
  - emit a success metrics event with the final byte count
- On stream failure after headers were prepared:
  - emit an error metrics event
  - do not increment successful bytes

This ensures bytes and errors reflect what the proxy actually delivered.

### Error Classification

The recorder will accept an internal error kind enum:

- `unauthorized_bucket`
- `origin`
- `internal`
- `stream`

The first version stores this only in logs and structured traces, not in dashboard aggregations. The dashboard tracks total errors and top error URLs.

## Status API Design

All `/api/*` routes require:

- header: `X-Status-API-Key: <STATUS_API_KEY>`

### POST `/api/login`

Purpose:

- Validate the API key before the frontend saves it locally

Request body:

```json
{ "apiKey": "secret" }
```

Responses:

- `204 No Content` on success
- `401 Unauthorized` on invalid key

### GET `/api/filters`

Response:

```json
{
  "defaultBucket": null,
  "buckets": ["@", "foo", "bar"]
}
```

### GET `/api/overview`

Query parameter:

- `bucket=<virtual_bucket>` when a bucket filter is selected

Response:

```json
{
  "scope": "global",
  "totals": {
    "requests": 123456,
    "bytes": 987654321,
    "cacheHitRate": 0.82,
    "errors": 42,
    "errorRate": 0.00034
  },
  "localCache": {
    "enabled": true,
    "usedBytes": 536870912,
    "capacityBytes": 1073741824,
    "usageRate": 0.5
  }
}
```

Rules:

- Local cache usage is process-wide, not bucket-scoped
- If `bucket` is absent, read the `global` scope
- If `bucket` is unknown, return `404`

### GET `/api/timeseries`

Query parameters:

- `range=1h|24h|7d`
- `bucket=<virtual_bucket>` when a bucket filter is selected

Response:

```json
{
  "scope": "bucket:foo",
  "range": "24h",
  "granularity": "1h",
  "summary": {
    "requests": 2400,
    "bytes": 123456789,
    "cacheHitRate": 0.77,
    "errors": 12,
    "errorRate": 0.005
  },
  "series": {
    "qps": [{ "ts": 1711753200, "value": 2.4 }],
    "throughputBytesPerSec": [{ "ts": 1711753200, "value": 120394.2 }],
    "cacheHitRate": [{ "ts": 1711753200, "value": 0.74 }],
    "errorRate": [{ "ts": 1711753200, "value": 0.01 }]
  }
}
```

Rules:

- `qps = requests / bucket_duration_seconds`
- `throughput = bytes / bucket_duration_seconds`
- hit rate and error rate return `0` when their denominator is `0`
- missing buckets are returned as zero-value points so charts remain continuous

### GET `/api/top`

Query parameter:

- `bucket=<virtual_bucket>` when a bucket filter is selected

Response:

```json
{
  "scope": "global",
  "window": "7d",
  "hotCacheFiles": [
    { "bucket": "foo", "objectKey": "assets/app.js", "hits": 1200 }
  ],
  "missUrls": [
    { "bucket": "foo", "url": "/assets/app.js?v=3", "misses": 87 }
  ],
  "errorUrls": [
    { "bucket": "foo", "url": "/broken/path", "errors": 41 }
  ]
}
```

## Frontend Design

### Interaction Model

- Initial render shows a branded login shell if no API key is stored
- User enters API key once
- Frontend calls `POST /api/login`
- On success:
  - store key in `localStorage`
  - fetch filters, overview, timeseries, and top lists
- On later visits:
  - reuse the stored key automatically
- If any API responds `401`:
  - clear the stored key
  - return to the login shell

### Visual Direction

The dashboard should feel like an operations board rather than a generic SaaS admin template:

- light industrial palette
- paper-white background with graphite and soft grid texture
- accent colors:
  - cache health green
  - warning amber
  - error rust
- expressive numeric typography for counters
- restrained but intentional motion during load and chart transitions

### Layout

1. Header bar
   - service name
   - environment/status label
   - bucket filter selector
   - refresh timestamp

2. Core metrics strip
   - total requests
   - total bytes served
   - cache hit rate
   - total errors / error rate
   - local cache usage

3. Window analytics section
   - range switcher: `1h`, `24h`, `7d`
   - summary row for selected range
   - four charts:
     - QPS
     - throughput
     - cache hit rate
     - error rate

4. Top lists section
   - hottest cached files
   - cache miss URLs
   - error URLs

### Responsiveness

- Desktop-first two-dimensional layout
- Tablet stacks charts into two columns
- Mobile collapses to one column while preserving legibility of metric cards and tables

## Local Cache Usage Read Path

The dashboard needs current local cache usage and usage rate. The source of truth should be the local cache subsystem rather than stats counters.

Design:

- expose a method from `LocalCache` that returns:
  - `enabled`
  - `used_bytes`
  - `capacity_bytes`
- when local cache is enabled:
  - prefer Redis `total_size_key` for the fast current usage number
  - clamp negatives or missing values to `0`
- when disabled:
  - return disabled state with zeroed values

This avoids walking the filesystem on every status-page request.

## Security Model

- The status listener bind host defaults to loopback to reduce accidental exposure
- API key validation is constant-time string comparison where practical
- Static frontend shell is public, but no sensitive data is returned without the API key header
- API responses include `Cache-Control: no-store`
- The frontend stores the API key in `localStorage` because this behavior was explicitly requested

Risk acknowledged:

- Storing a long-lived API key in `localStorage` is less secure than cookie-based or server-issued sessions. The design keeps this because it matches the requested UX.

## Failure Handling

- If Redis is unavailable at startup:
  - local cache may already degrade to disabled behavior
  - stats store should also degrade gracefully enough for the proxy to keep serving traffic
- Recommended behavior:
  - proxy remains functional
  - status APIs return partial data or an explicit degraded-state response rather than crashing the process

First-version read/write degradation strategy:

- metrics writes:
  - best effort
  - log warnings on Redis failure
  - never fail the proxied request because stats recording failed
- metrics reads:
  - return `503` with a structured error if Redis is unavailable and data cannot be assembled
- local cache usage:
  - if unavailable, return `enabled: true` with `usageUnavailable: true`

## Testing Strategy

### Unit Tests

- config parsing for status host, port, and API key
- bucket timestamp rounding for `5m`, `1h`, and `1d`
- rate calculations for chart series
- hit-rate and error-rate denominator behavior
- Redis key naming stability
- auth middleware key validation behavior

### Integration Tests

- proxy hit path records request, bytes, and top cached file metrics
- proxy miss path records request, bytes, miss count, and miss URL metrics
- proxy error path records request, error count, and error URL metrics
- status API rejects missing or invalid API key
- status API returns zero-filled series for sparse windows

### Manual Verification

- boot both listeners locally
- submit a valid and invalid API key from the browser
- issue a mix of hit, miss, bypass, disabled, and error requests
- confirm overview totals update
- confirm chart windows match expected granularity
- confirm bucket filter changes datasets
- confirm top 10 lists update within the recent 7-day window

## Deployment And README Updates

`README.md` must be updated to cover:

- new environment variables:
  - `STATUS_PORT`
  - `STATUS_HOST`
  - `STATUS_API_KEY`
- how the second listener works
- default loopback-only binding and how to expose it intentionally
- the API-key login flow
- Redis requirement for status metrics
- example `docker run` publishing both ports when desired

## Open Questions Resolved

- Scope filter: global plus per-bucket
- Host filter: excluded
- Status listener binding: configurable, default loopback
- Login persistence: `localStorage`
- Frontend delivery: embedded single-page app from the Rust status service
- Recent-window metrics: native multi-resolution buckets, not derived downsampling
- Error metrics: included in totals, time series, and top lists

## Implementation Notes

- Keep the stats recorder isolated so future metrics additions do not tangle `main.rs`
- Avoid blocking Redis reads or writes on the hot path more than necessary; pipelined commands or batched futures are preferred
- Emit metrics after the request outcome is known, not merely when routing begins
- Reuse existing whitelist cache for bucket filter options to avoid duplicate config stores
