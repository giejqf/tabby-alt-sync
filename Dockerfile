# syntax=docker/dockerfile:1

FROM rust:1-alpine AS builder
RUN apk add --no-cache build-base
WORKDIR /build
COPY Cargo.toml Cargo.lock ./
RUN mkdir -p src && printf 'fn main() {}\n' > src/main.rs \
    && cargo build --release --locked
COPY src ./src
RUN cargo build --release --locked

FROM alpine:3.21
RUN apk add --no-cache ca-certificates \
    && adduser -D -H -u 10001 tabby \
    && mkdir -p /data \
    && chown tabby:tabby /data
COPY --from=builder /build/target/release/tabby-alt-sync /usr/local/bin/tabby-alt-sync
ENV TABBY_ALT_SYNC_BIND=0.0.0.0:9600 \
    TABBY_ALT_SYNC_DB=/data/tabby-alt-sync.db
USER tabby
WORKDIR /data
VOLUME ["/data"]
EXPOSE 9600
ENTRYPOINT ["tabby-alt-sync"]
