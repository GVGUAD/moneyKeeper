# ─── Build Stage ─────────────────────────────────────────────────────────────
FROM rust:1-alpine AS builder

RUN apk add --no-cache musl-dev pkgconf
# sqlite-dev and sqlite-static removed — no longer needed

WORKDIR /app

# Cache dependencies layer
COPY Cargo.toml Cargo.lock ./
RUN mkdir -p src && \
    echo "fn main() {}" > src/main.rs && \
    touch src/lib.rs && \
    cargo build --release 2>/dev/null || true && \
    rm -rf src

# Build the real binary
COPY static ./static
COPY src ./src
RUN touch src/main.rs src/lib.rs && cargo build --release

# ─── Runtime Stage ───────────────────────────────────────────────────────────
FROM alpine:3.21
RUN apk add --no-cache ca-certificates tzdata
WORKDIR /app
COPY --from=builder /app/target/release/moneykeeper .

ENV RUST_LOG=info
ENV BIND_ADDR=0.0.0.0:8080
# DATABASE_URL and SUPABASE_JWT_SECRET are injected via fly secrets — not set here
# Removed: DATABASE_URL default, JWT_SECRET, VOLUME /data

EXPOSE 8080
ENTRYPOINT ["./moneykeeper"]
