ARG HTTP_PROXY
ARG HTTPS_PROXY
ARG NO_PROXY

FROM rust:1.87-slim-bookworm AS builder
WORKDIR /app
ARG HTTP_PROXY
ARG HTTPS_PROXY
ARG NO_PROXY
ENV HTTP_PROXY=${HTTP_PROXY} \
    HTTPS_PROXY=${HTTPS_PROXY} \
    NO_PROXY=${NO_PROXY}
RUN apt-get update && \
    apt-get install -y --no-install-recommends pkg-config libssl-dev ca-certificates && \
    rm -rf /var/lib/apt/lists/*

# Prime dependency compilation to improve incremental Docker rebuilds.
COPY Cargo.toml Cargo.lock ./
RUN mkdir src && \
    printf 'fn main() {}\n' > src/main.rs && \
    cargo build --release && \
    rm -rf src target/release/deps/r2_proxy*

COPY src ./src
RUN cargo build --release && strip target/release/r2-proxy

FROM debian:bookworm-slim AS runtime
ARG HTTP_PROXY
ARG HTTPS_PROXY
ARG NO_PROXY
ENV HTTP_PROXY=${HTTP_PROXY} \
    HTTPS_PROXY=${HTTPS_PROXY} \
    NO_PROXY=${NO_PROXY}
RUN apt-get update && \
    apt-get install -y --no-install-recommends ca-certificates && \
    rm -rf /var/lib/apt/lists/*

COPY --from=builder /app/target/release/r2-proxy /usr/local/bin/r2-proxy

EXPOSE 3000
ENTRYPOINT ["/usr/local/bin/r2-proxy"]
