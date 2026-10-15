# The nest image under test plus an e2e build of the Fauna updater — the tier_4
# witness of `docs/goal/architecture/nest/update-channel.md` § Tests
# (`test_update_channel_updater.py`). NEVER shipped: built only by that test, on
# top of the image the tier_4 suite already built, and tagged
# `fauna-updater-e2e:local`.
#
# Why a second build at all: the shipped updater follows exactly one repository,
# `ghcr.io/faunasocial/nest`, and nothing it reads can name another
# (§ The updater → *What it refuses*). The witness serves the channel heads from
# a harness registry on loopback, so it needs the `test-hooks` build, which reads
# that registry's origin from `FAUNA_E2E_UPDATE_REGISTRY_ORIGIN` (e2e conventions
# point 15: the seam is compiled out of the shipped artifact, whose Dockerfile
# builds the updater without the feature). Everything else — the nest, its
# image, the engine — is the shipped artifact.
#
# Only the updater's own crate graph compiles here (a few minutes cold), on the
# same pinned toolchain image and linker setup as the root `Dockerfile`'s
# `rust-base`; nothing new is pulled.

ARG BASE_IMAGE=fauna-nest-test:local

FROM rust:bookworm@sha256:82150a52ec202c1b14d7817e14516c392bb7f5cfebd88f1ed531cb37ebd39922 AS build
RUN apt-get update && apt-get install -y --no-install-recommends clang mold \
    && rm -rf /var/lib/apt/lists/*
WORKDIR /src
COPY rust-toolchain.toml ./
RUN rustup show
ENV CARGO_RESOLVER_FEATURE_UNIFICATION=selected
COPY . .
ARG TARGETARCH
RUN --mount=type=cache,id=cargo-registry-native,target=/usr/local/cargo/registry \
    --mount=type=cache,id=cargo-git-native,target=/usr/local/cargo/git \
    --mount=type=cache,id=rust-target-updater-e2e-${TARGETARCH},target=/src/target \
    cargo build --locked --release -p fauna-updater --features test-hooks \
    && mkdir -p /out \
    && cp /src/target/release/fauna-updater /out/fauna-updater-e2e

FROM ${BASE_IMAGE}
COPY --from=build /out/fauna-updater-e2e /usr/local/bin/fauna-updater-e2e
