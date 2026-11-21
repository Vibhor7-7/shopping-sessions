# Shopping Sessions: a single static binary, built from source.
# No FROM line pins a platform: the scorer builds this natively on linux/amd64,
# and `make build PLATFORM=linux/arm64` builds it natively on an Apple-silicon Mac.

FROM rust:1.100-alpine3.22 AS build
WORKDIR /src
COPY Cargo.toml Cargo.lock ./
COPY src ./src
# The crate has no dependencies, so the build needs nothing from the network.
# The scorers are Cascade Lake or newer: x86-64-v3 (AVX2, BMI2) is always there.
RUN set -eu; \
    case "$(uname -m)" in x86_64) export RUSTFLAGS="-C target-cpu=x86-64-v3" ;; esac; \
    cargo build --release --locked --offline --bin pipeline; \
    cp target/release/pipeline /pipeline

# At run time the root filesystem is read-only, the user is 1000:1000 and there
# is no network. The binary is static and writes only to $OUTPUT_DIR.
# The base is the starters' Debian, not scratch or Alpine, on purpose: the
# organizers' scripts/docker_run.sh runs `chmod -R` from inside the image to
# clean up, and BusyBox's chmod gives up where GNU's carries on.
FROM debian:bookworm-slim
COPY --from=build /pipeline /app/pipeline
CMD ["/app/pipeline"]
