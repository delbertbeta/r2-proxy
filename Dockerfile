# ---- Build Stage ----
FROM rust:1.87 as builder
WORKDIR /app
COPY . .
RUN cargo build --release

# ---- Runtime Stage ----
FROM debian:bookworm-slim
RUN apt update && apt install -y openssl ca-certificates
WORKDIR /app
COPY --from=builder /app/target/release/r2-proxy /usr/local/bin/r2-proxy
EXPOSE 3000
CMD ["r2-proxy"]