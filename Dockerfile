# ---- build ---------------------------------------------------------------------------------
FROM rust:1.90-bookworm AS builder
WORKDIR /src
COPY . .
RUN cargo build --release -p ems-server --locked

# ---- runtime -------------------------------------------------------------------------------
FROM debian:bookworm-slim
RUN apt-get update \
 && apt-get install -y --no-install-recommends ca-certificates curl \
 && rm -rf /var/lib/apt/lists/* \
 && useradd --system --uid 10001 --home-dir /data --shell /usr/sbin/nologin ems \
 && mkdir -p /data /config && chown ems:ems /data /config
COPY --from=builder /src/target/release/evm-mcp-server /usr/local/bin/evm-mcp-server

USER ems
# /data: sqlite ems.db (usage counters, client keys, call log)
# /config: config.toml + secrets.toml (written by the dashboard) + admin_token (created on first start)
VOLUME ["/data", "/config"]
ENV EMS__SERVER__DATA_DIR=/data \
    EMS__SERVER__HTTP_BIND=0.0.0.0:8787 \
    RUST_LOG=info
EXPOSE 8787 8788
HEALTHCHECK --interval=30s --timeout=5s --start-period=10s --retries=3 \
  CMD curl -fsS http://127.0.0.1:8787/healthz || exit 1

ENTRYPOINT ["evm-mcp-server"]
CMD ["serve", "--config-dir", "/config"]
