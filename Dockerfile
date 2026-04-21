# Fully qualified names so Podman can resolve pulls without unqualified-search-registries (Docker defaults to docker.io).
FROM docker.io/library/rust:1.85-slim-bookworm AS builder
WORKDIR /app
COPY Cargo.toml Cargo.lock ./
RUN mkdir src && echo "fn main() {}" > src/main.rs && cargo build --release && rm -rf src
COPY src ./src
RUN touch src/main.rs && cargo build --release

FROM docker.io/library/debian:bookworm-slim
RUN apt-get update && apt-get install -y ca-certificates git && rm -rf /var/lib/apt/lists/*
WORKDIR /app
COPY --from=builder /app/target/release/mervyn .
COPY config ./config
VOLUME ["/app/data"]
EXPOSE 3000
CMD ["./mervyn"]
