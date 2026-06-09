# ---- Build stage ---------------------------------------------------------
FROM rust:1-bookworm AS builder

WORKDIR /app

# Cache dependencies first: copy only the manifests, build a dummy binary so
# the dependency layer is reused across source-only changes.
COPY Cargo.toml Cargo.lock* ./
RUN mkdir src \
    && echo "fn main() {}" > src/main.rs \
    && cargo build --release \
    && rm -rf src

# Now build the real source.
COPY src ./src
RUN touch src/main.rs && cargo build --release

# ---- Runtime stage -------------------------------------------------------
FROM debian:bookworm-slim

RUN apt-get update \
    && apt-get install -y --no-install-recommends ca-certificates \
    && rm -rf /var/lib/apt/lists/*

# Run as a non-root user.
RUN useradd --create-home --uid 10001 appuser
USER appuser

COPY --from=builder /app/target/release/ping-pong /usr/local/bin/ping-pong

ENV PORT=8080
EXPOSE 8080

CMD ["ping-pong"]
