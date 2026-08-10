# Multi-stage build for falcon. Produces a small runtime image running as a
# non-root user. Note: the `v8` crate downloads a prebuilt V8 static library at
# build time (needs network during `cargo build`).
FROM rust:1-bookworm AS build
WORKDIR /src
# Cache dependencies first.
COPY Cargo.toml Cargo.lock ./
RUN mkdir -p src && echo 'fn main(){}' > src/main.rs && \
    cargo build --release --bin falcon 2>/dev/null || true
# Real sources.
COPY src ./src
RUN touch src/main.rs && cargo build --release --bin falcon

FROM debian:bookworm-slim AS runtime
RUN apt-get update && apt-get install -y --no-install-recommends ca-certificates \
    && rm -rf /var/lib/apt/lists/* \
    && useradd --system --uid 10001 --create-home falcon
COPY --from=build /src/target/release/falcon /usr/local/bin/falcon
# ICU common data: without it any Intl-backed page call is a V8 fatal that
# kills the process (see engine.rs::init_icu). The compiled-in default path
# points at the build tree, so the runtime image carries the file + env.
COPY third_party/icu/icudt74l.dat /usr/local/share/falcon/icudt74l.dat
ENV FALCON_ICU_DATA=/usr/local/share/falcon/icudt74l.dat
USER falcon
EXPOSE 8200
ENTRYPOINT ["/usr/local/bin/falcon", "--bind", "0.0.0.0:8200"]
