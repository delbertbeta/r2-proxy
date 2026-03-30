# Repository Guidelines

## Project Structure & Module Organization
`src/` contains the Rust service: `main.rs` wires the Axum servers, `config.rs` reads environment variables, `kv_client.rs` and `s3_client.rs` handle Cloudflare KV and R2, and `status_server.rs` serves the monitoring UI. `status/` holds the embedded dashboard assets (`index.html`, `app.css`, `app.js`). `tests/` contains lightweight Node-based checks for frontend regressions. Design notes and implementation plans live in `docs/superpowers/`.

## Build, Test, and Development Commands
- `cargo build` builds the debug binary for local development.
- `cargo run` starts the proxy and status server using your current `.env`.
- `cargo build --release` produces the optimized binary at `target/release/r2-proxy`.
- `cargo test` runs Rust unit and integration tests.
- `node --test tests/*.test.mjs` runs the dashboard asset checks in `tests/`.
- `docker build -t r2-proxy .` builds the production image defined in `Dockerfile`.

## Coding Style & Naming Conventions
Follow standard Rust formatting with 4-space indentation and run `cargo fmt` before opening a PR. Use `cargo clippy --all-targets --all-features` to catch avoidable issues. Keep modules focused and named by responsibility (`stats.rs`, `local_cache.rs`). Prefer `snake_case` for functions, variables, and files; `CamelCase` for structs and enums; `SCREAMING_SNAKE_CASE` for constants. Frontend changes in `status/` should stay framework-free and easy to embed.

## Testing Guidelines
Add Rust tests close to the code they verify when possible, and add `.test.mjs` files under `tests/` for status-page CSS or JavaScript regressions. Name tests after the behavior they protect, for example `returns_404_for_missing_object` or `status-login-persists-api-key.test.mjs`. Run the relevant test commands before submitting changes.

## Commit & Pull Request Guidelines
Recent history favors short, imperative subjects, often with prefixes like `feat:`, `fix:`, and `ci:`. Keep commits scoped to one change. PRs should summarize the behavior change, note any config or env var updates, link related issues, and include screenshots when touching the status dashboard.

## Security & Configuration Tips
Do not commit real `.env` values, API keys, or Cloudflare credentials. Keep `STATUS_API_KEY` strong, and default `STATUS_HOST` to `127.0.0.1` unless the dashboard is intentionally exposed behind a trusted proxy.
