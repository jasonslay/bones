FROM rust:alpine AS builder

WORKDIR /app
COPY rust-toolchain.toml ./
RUN rustup show
COPY Cargo.toml Cargo.lock ./
COPY src ./src
RUN cargo build --release \
    && cargo rustc --release --bin bones -- -C target-feature=+crt-static
COPY web ./web

FROM scratch

COPY --from=builder /app/target/release/bones /app/bones
COPY --from=builder /app/web /app/web

USER 10001
ENV BONES_ADDR=0.0.0.0:8080
ENV BONES_WEB_DIR=/app/web
ENV RUST_LOG=bones=info
EXPOSE 8080

CMD ["/app/bones"]
