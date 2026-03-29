# Local Disk Cache Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Add optional local disk object caching backed by Redis metadata/LFU indexes to reduce R2 origin fetches.

**Architecture:** Keep response bodies on local disk and store cache metadata, headers, file paths, total size accounting, and LFU state in Redis. Route requests through a cache-aware fetch path that bypasses any key ending with `index.html`, degrades to origin-only behavior when Redis is unavailable, and emits `X-R2-Proxy-Cached` on every response.

**Tech Stack:** Rust, Axum, Tokio, AWS SDK for S3, Redis, Serde

---

### Task 1: Extend Configuration Parsing

**Files:**
- Modify: `src/config.rs`
- Modify: `Cargo.toml`
- Test: `src/config.rs`

- [ ] **Step 1: Write the failing tests**

```rust
#[test]
fn parses_human_readable_local_cache_size() {
    assert_eq!(parse_size("512M").unwrap(), 512 * 1024 * 1024);
    assert_eq!(parse_size("1G").unwrap(), 1024 * 1024 * 1024);
}

#[test]
fn config_reads_optional_local_cache_and_redis_settings() {
    // populate env and assert LOCAL_CACHE_ENABLED / LOCAL_CACHE_MAX_SIZE /
    // LOCAL_CACHE_DIR / REDIS_URL / REDIS_KEY_PREFIX are parsed
}
```

- [ ] **Step 2: Run test to verify it fails**

Run: `cargo test config:: -- --nocapture`
Expected: FAIL because size parser and new config fields do not exist.

- [ ] **Step 3: Write minimal implementation**

```rust
pub struct LocalCacheConfig { ... }

fn parse_size(input: &str) -> Result<u64, ConfigError> { ... }
```

- [ ] **Step 4: Run test to verify it passes**

Run: `cargo test config:: -- --nocapture`
Expected: PASS

- [ ] **Step 5: Commit**

```bash
git add Cargo.toml src/config.rs
git commit -m "feat: add local cache config parsing"
```

### Task 2: Build Cache Metadata and Redis Coordination

**Files:**
- Create: `src/local_cache.rs`
- Modify: `src/errors.rs`
- Test: `src/local_cache.rs`

- [ ] **Step 1: Write the failing tests**

```rust
#[tokio::test]
async fn skips_cache_for_any_index_html_suffix() {
    assert!(should_bypass_cache("docs/index.html"));
}

#[tokio::test]
async fn cache_status_header_values_are_stable() {
    assert_eq!(CacheStatus::Hit.header_value(), "HIT");
}
```

- [ ] **Step 2: Run test to verify it fails**

Run: `cargo test local_cache:: -- --nocapture`
Expected: FAIL because module and types do not exist.

- [ ] **Step 3: Write minimal implementation**

```rust
pub enum CacheStatus { Hit, Miss, Bypass, Disabled }
pub struct LocalCache { ... }
```

- [ ] **Step 4: Run test to verify it passes**

Run: `cargo test local_cache:: -- --nocapture`
Expected: PASS

- [ ] **Step 5: Commit**

```bash
git add src/local_cache.rs src/errors.rs
git commit -m "feat: add redis-backed local cache metadata layer"
```

### Task 3: Integrate Cache Into Request Flow

**Files:**
- Modify: `src/main.rs`
- Modify: `src/s3_client.rs`
- Test: `src/main.rs`

- [ ] **Step 1: Write the failing tests**

```rust
#[test]
fn applies_r2_proxy_cached_header() {
    let mut headers = HeaderMap::new();
    apply_cache_status_header(&mut headers, CacheStatus::Hit);
    assert_eq!(headers["x-r2-proxy-cached"], "HIT");
}
```

- [ ] **Step 2: Run test to verify it fails**

Run: `cargo test main:: -- --nocapture`
Expected: FAIL because cache-aware response header handling does not exist.

- [ ] **Step 3: Write minimal implementation**

```rust
let cached = state.local_cache.fetch_or_fill(...).await?;
apply_cache_status_header(response.headers_mut(), cached.status);
```

- [ ] **Step 4: Run test to verify it passes**

Run: `cargo test main:: -- --nocapture`
Expected: PASS

- [ ] **Step 5: Commit**

```bash
git add src/main.rs src/s3_client.rs
git commit -m "feat: serve objects from local disk cache"
```

### Task 4: Update Documentation

**Files:**
- Modify: `README.md`

- [ ] **Step 1: Update environment variable documentation**

```md
LOCAL_CACHE_ENABLED=true
LOCAL_CACHE_MAX_SIZE=1G
LOCAL_CACHE_DIR=/var/cache/r2-proxy
REDIS_URL=redis://127.0.0.1:6379
REDIS_KEY_PREFIX=r2proxy
```

- [ ] **Step 2: Document cache behavior**

```md
- any path ending with `index.html` is never cached
- `X-R2-Proxy-Cached` returns `HIT|MISS|BYPASS|DISABLED`
```

- [ ] **Step 3: Verify docs formatting**

Run: `cargo test`
Expected: PASS and README examples align with implementation.

- [ ] **Step 4: Commit**

```bash
git add README.md
git commit -m "docs: describe local disk cache settings"
```
