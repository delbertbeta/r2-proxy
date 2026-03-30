---
title: 2026-03-30-stats-error-breakdown-design
type: note
permalink: work/r2-proxy/docs/superpowers/specs/2026-03-30-stats-error-breakdown-design
---

# Stats Error Breakdown Design

## Goal

Update the status dashboard so error reporting distinguishes between object-not-found traffic and server-side failures instead of treating every error as a single bucket.

This change applies consistently across:

- overview totals
- time-series summaries and charts
- top error URL rankings

## Current Problem

The current stats pipeline only records whether a request ended in `Success` or `Error`.

That means:

- `404 Not Found` responses from missing R2 objects are counted together with true server failures
- the dashboard cannot show whether error pressure is caused by invalid URLs or backend instability
- the top error URL list mixes expected missing-object traffic with genuine 5xx failures

## Proposed Approach

Extend the stats model so error events carry an error class instead of a single undifferentiated error state.

Use three result categories:

- `Success`
- `NotFound`
- `ServerError`

From those categories, the stats store will maintain:

- `errors` as total errors
- `errors_404` as 404-class errors
- `errors_5xx` as 5xx-class errors

The top URL leaderboards will also be split into two sorted sets:

- `top:errors_404`
- `top:errors_5xx`

## Classification Rules

The proxy will classify request outcomes as follows:

- successful responses remain `Success`
- `ProxyError::ObjectNotFound` records as `NotFound`
- all other request failures that currently count as errors record as `ServerError`

This keeps the dashboard behavior aligned with actual HTTP semantics:

- missing objects are tracked as 404 traffic
- origin, cache, KV, config, and internal failures are tracked as 5xx traffic

## Backend Changes

### Error and Stats Types

Update the request outcome type in the stats layer so it can represent `NotFound` and `ServerError`.

Add helper methods where useful so the write path can ask:

- whether the event is an error
- whether it counts as 404
- whether it counts as 5xx

`ProxyError` will expose a stats classification helper that maps each proxy error to the correct stats error class.

### Stats Storage

Update totals hashes and time-bucket hashes to increment:

- `requests`
- `bytes`
- `cache_hits`
- `cache_misses`
- `errors`
- `errors_404`
- `errors_5xx`

Keep `errors` as the aggregate count so existing mental models remain valid while adding the two detailed counters.

Update top-error recording so 404 and 5xx URLs are written to different daily keys.

### Status API

Update API responses to expose the new fields:

- overview totals: total errors, 404 errors, 5xx errors, and their rates
- time-series summary: total, 404, and 5xx rates
- time-series series payload: `notFoundErrorRate` and `serverErrorRate`
- top payload: `notFoundUrls` and `serverErrorUrls`

The existing total error fields should remain available unless a rename is required for internal consistency.

## Frontend Changes

### Overview

Keep the main error card but show richer breakdown text:

- total error count remains the primary number
- subtext includes total rate plus 404 / 5xx counts or rates

### Summary Strip

Add separate summary values for:

- 404 rate
- 5xx rate

### Charts

Replace the single error-rate chart with two charts:

- 404 Error Rate
- 5xx Error Rate

This makes the cause of elevated error traffic visible without requiring users to infer it from totals.

### Top Lists

Split the current error URL list into:

- Top 404 URLs
- Top 5xx URLs

This keeps broken links and backend failures visually separate.

## Data Compatibility

Existing Redis keys without the new fields will naturally read as zero because the current code already defaults missing hash fields to zero.

New top-level sorted-set names for 404 and 5xx rankings will begin accumulating from deployment forward. Historical aggregated `top:errors` data does not need migration.

## Testing

Add or update tests for:

- proxy error to stats error-class mapping
- totals/hash decoding with new 404 and 5xx counters
- stats write behavior for success, 404, and 5xx events
- status API response serialization for new fields
- frontend rendering paths for separated 404 and 5xx sections where practical

## Scope Guardrails

This change does not introduce:

- additional error categories beyond 404 and 5xx
- historical backfill or Redis migration jobs
- changes to authentication, caching, or bucket filtering behavior