FROM rust:bookworm AS builder

WORKDIR /app
COPY Cargo.toml Cargo.lock ./
COPY src ./src
COPY web ./web
RUN cargo build --release

FROM debian:bookworm-slim

RUN useradd --system --uid 10001 --create-home bones \
    && apt-get update \
    && apt-get install -y --no-install-recommends ca-certificates \
    && rm -rf /var/lib/apt/lists/*

WORKDIR /app
COPY --from=builder /app/target/release/bones /app/bones
COPY --from=builder /app/web /app/web

USER 10001
ENV BONES_ADDR=0.0.0.0:8080
ENV BONES_WEB_DIR=/app/web
ENV RUST_LOG=bones=info
EXPOSE 8080

CMD ["/app/bones"]
