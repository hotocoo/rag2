# syntax=docker/dockerfile:1
# Multi-stage build: compile with the pinned toolchain, ship a minimal non-root runtime.
# Build:  docker build -f deploy/Dockerfile -t rag2:latest .
# Run:    docker run -p 8080:8080 -v rag2-data:/var/lib/rag2 --env-file rag2.env rag2:latest

ARG RUST_VERSION=1.99
FROM rust:${RUST_VERSION}-bookworm AS build
RUN apt-get update && apt-get install -y --no-install-recommends protobuf-compiler && rm -rf /var/lib/apt/lists/*
WORKDIR /src
COPY . .
# --locked: build exactly the audited Cargo.lock.
RUN --mount=type=cache,target=/usr/local/cargo/registry \
    --mount=type=cache,target=/src/target \
    cargo build --release --locked -p rag2-cli && cp target/release/rag2 /rag2

FROM gcr.io/distroless/cc-debian12:nonroot
COPY --from=build /rag2 /usr/local/bin/rag2
COPY deploy/profile.container.yaml /etc/rag2/rag2.yaml
ENV RAG2_CONFIG_DIR=/var/lib/rag2/config \
    RAG2_LOG_FORMAT=json
VOLUME ["/var/lib/rag2"]
EXPOSE 8080
USER nonroot:nonroot
ENTRYPOINT ["/usr/local/bin/rag2"]
CMD ["serve", "--file", "/etc/rag2/rag2.yaml", "--watch"]
