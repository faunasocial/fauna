# fauna-cors-proxy — stateless CORS forwarder for provisioning provider APIs.
#
# The crate is a workspace member, so the build context is the REPO ROOT:
#
#   docker build -f services/fauna-cors-proxy/Dockerfile -t fauna-cors-proxy .
#   docker run -p 8080:8080 fauna-cors-proxy
#
# PORT (default 8080) is the only runtime input — artifact-set wiring, not
# user configuration. TLS terminates in front of this container (the service
# is plain HTTP); see README.md § Deployment.

FROM rust:bookworm AS builder
WORKDIR /build
COPY . .
RUN cargo build --release -p fauna-cors-proxy

FROM debian:bookworm-slim
RUN apt-get update \
    && apt-get install -y --no-install-recommends ca-certificates curl \
    && rm -rf /var/lib/apt/lists/*
COPY --from=builder /build/target/release/fauna-cors-proxy /usr/local/bin/fauna-cors-proxy
USER nobody
EXPOSE 8080
HEALTHCHECK --interval=30s --timeout=5s CMD curl -fsS http://127.0.0.1:8080/healthz || exit 1
ENTRYPOINT ["fauna-cors-proxy"]
