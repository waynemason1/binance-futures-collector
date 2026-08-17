# ---- build ----
FROM rust:1-slim-bookworm AS build
# tikv-jemalloc-sys compiles jemalloc from C source, which needs make + a C
# toolchain. The rust:*-slim images ship the linker but not make, so install it
# before building or the Linux build aborts at jemalloc-sys.
RUN apt-get update \
    && apt-get install -y --no-install-recommends make \
    && rm -rf /var/lib/apt/lists/*
WORKDIR /src
COPY Cargo.toml Cargo.lock ./
# .cargo/config.toml carries JEMALLOC_SYS_WITH_MALLOC_CONF (background purge +
# decay). Without it the container builds jemalloc with default conf and loses
# the RSS-bounding this project advertises — so it must be present at build time.
COPY .cargo ./.cargo
COPY src ./src
RUN cargo build --release --locked

# ---- runtime ----
FROM debian:bookworm-slim
RUN apt-get update \
    && apt-get install -y --no-install-recommends ca-certificates bash \
    && rm -rf /var/lib/apt/lists/* \
    && useradd -mu 10001 collector
WORKDIR /app
COPY --from=build /src/target/release/binance-futures-collector /app/target/release/binance-futures-collector
COPY scripts/supervise.sh /app/scripts/supervise.sh
# The collector loads exclusions.toml from its working directory; without it the
# container captures the stablecoins and top-caps the shipped defaults exclude.
COPY exclusions.toml /app/exclusions.toml
RUN mkdir -p /app/data /app/logs && chown -R collector /app
USER collector
# Runs under the supervisor so the dashboard's "Restart to apply" (which drops
# data/control/restart.request in the shared volume) triggers a clean in-container
# restart. Mount config.toml at /app/config.toml; data + logs go to /app/data|logs.
ENTRYPOINT ["bash", "/app/scripts/supervise.sh"]
