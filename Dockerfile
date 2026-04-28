# ── Stage 1: Build the frontend ─────────────────────────────────────────────
FROM node:22-alpine AS frontend-builder

WORKDIR /app/web
COPY web/package.json web/package-lock.json* ./
RUN npm ci
COPY web/ ./
RUN npm run build

# ── Stage 2: Build the Rust binary ──────────────────────────────────────────
FROM rust:1.95.0-alpine3.22 AS backend-builder

RUN apk add --no-cache musl-dev pkgconf openssl-dev perl make

WORKDIR /app

# Cache dependencies by copying manifests first
COPY Cargo.toml Cargo.lock ./
# Build a dummy main so Cargo fetches and compiles deps
RUN mkdir src && echo 'fn main() {}' > src/main.rs && \
    cargo build --release && \
    rm -rf src

# Build the real binary
COPY src/ src/
COPY migrations/ migrations/
RUN touch src/main.rs && cargo build --release

# ── Stage 3: Minimal runtime image ──────────────────────────────────────────
FROM alpine:3.22

RUN apk add --no-cache ca-certificates libssl3 tini

WORKDIR /app

COPY --from=backend-builder /app/target/release/dabeacon_indexer /app/dabeacon_indexer
COPY --from=backend-builder /app/migrations /app/migrations
COPY --from=frontend-builder /app/web/build /app/web/build

EXPOSE 3000

ENTRYPOINT ["/sbin/tini", "--", "/app/dabeacon_indexer"]
