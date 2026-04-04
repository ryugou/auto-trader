FROM rust:1.85-bookworm AS builder
WORKDIR /app
COPY Cargo.toml Cargo.lock rust-toolchain.toml ./
COPY crates/ crates/
COPY migrations/ migrations/
RUN cargo build --release --bin auto-trader

FROM debian:bookworm-slim
RUN apt-get update && apt-get install -y ca-certificates && rm -rf /var/lib/apt/lists/*
COPY --from=builder /app/target/release/auto-trader /usr/local/bin/auto-trader
COPY config/ /app/config/
COPY migrations/ /app/migrations/
WORKDIR /app
ENV CONFIG_PATH=/app/config/default.toml
CMD ["auto-trader"]
