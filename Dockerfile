FROM rust:1.100.0-slim-bookworm AS builder
WORKDIR /build
COPY Cargo.toml Cargo.lock ./
RUN mkdir -p src && echo 'fn main() {}' > src/main.rs && cargo build --release --locked && rm -rf src
COPY src ./src
RUN touch src/main.rs && cargo build --release --locked

FROM debian:bookworm-slim
COPY --from=builder /build/target/release/mech-royale-bot /usr/local/bin/mech-royale-bot
USER 65534:65534
ENTRYPOINT ["/usr/local/bin/mech-royale-bot"]
