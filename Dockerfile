# Incremental build strategy (BuildKit cache mounts — requires DOCKER_BUILDKIT):
#
#   rust-base            shared toolchain
#   rust-native          real source → native binaries
#   rust-wasm            real source → WASM module
#   bridges-builder      Go binaries (cgo against libfauna_ffi.so)
#   web-builder          Svelte SPA (depends only on rust-wasm, NOT rust-native)
#   final                assemble runtime image
#
# The cargo registry/git caches and the cargo target/ dir (and the Go module /
# build caches) are persisted via `--mount=type=cache`. Those caches survive
# across builds AND across a killed-and-retried build — a `type=cache` directory
# retains its writes even when the RUN is interrupted — so a rebuild recompiles
# only the crates whose source actually changed (cargo's own incremental
# fingerprinting is the staleness signal). This replaces an older stub-dependency
# stage that had to be hand-edited (a per-crate Cargo.toml COPY manifest) for
# every new workspace crate.
#
# Gotcha: a `type=cache` target/ is NOT baked into the image layer. Each Rust
# stage therefore `cp`-s its built binaries to /out *within the same RUN* (before
# the mount detaches), and later stages `COPY --from=<stage> /out/...`.

# ── Build-time CPU cap (shared by all compile stages) ───────────────
# BUILD_JOBS bounds the parallelism of the CPU-heavy compile stages so a deploy
# build (`just docker-push-dev`) does NOT saturate every core of the shared host
# — the build host that runs the Linux/Windows/macOS build VMs — and starve the other
# VMs. It caps cargo's parallel-rustc count (CARGO_BUILD_JOBS; cargo otherwise
# defaults to ~one rustc per core ≈ nproc, the saturation source) in both Rust
# stages, and Go's GOMAXPROCS in the mail-bridge stage. The `--builder default`
# (docker-driver) build runs daemon-side, so a per-build `--cpus` cap is NOT
# available — this parallelism cap is the lever. Default 16 (leaves headroom on
# a 28-core build VM); applies to every image build (docker-build + docker-push-
# dev). Override per build with `FAUNA_DOCKER_BUILD_JOBS=N just docker-push-dev`.
ARG BUILD_JOBS=16

# ── Rust toolchain (shared by all Rust stages) ──────────────────────
FROM rust:bookworm@sha256:82150a52ec202c1b14d7817e14516c392bb7f5cfebd88f1ed531cb37ebd39922 AS rust-base
# mold: the linker used by the rust-native stage (set via RUSTFLAGS there, NOT in
# rust-wasm — wasm32 links with wasm-ld and -fuse-ld=mold would break it).
# util-linux: provides `taskset`, used by the rust-wasm stage to confine the
# binaryen wasm-opt pass to BUILD_JOBS cores (see that stage). It is
# Priority:required in Debian so already present in rust:bookworm; named here
# explicitly so the wasm CPU cap can't silently break under a future slimmer base.
RUN apt-get update && apt-get install -y --no-install-recommends clang mold util-linux \
    && rm -rf /var/lib/apt/lists/*
WORKDIR /src
# Build with the SAME pinned nightly as every dev machine + CI (rust-toolchain.toml)
# instead of the rust:bookworm stable default. Two reasons: (1) repo-wide
# consistency — the deployed binary is now built on the same toolchain everything
# else uses, closing a latent stable-vs-nightly divergence; (2) the rust-native
# stage's `-Z threads` parallel front-end is nightly-only. rustup auto-installs
# the pinned toolchain (and its wasm32 target) on the first cargo invocation; the
# explicit `rustup show` forces that install into this layer so it is cached.
COPY rust-toolchain.toml ./
RUN rustup show && rustup target add wasm32-unknown-unknown
# Pinned to the version the Linux development web builds run, so the image's
# wasm chunks come from the same wasm-pack — and the wasm-opt it fetches — as a
# development build of one commit; `--locked` builds it from its own lockfile.
RUN cargo install wasm-pack --version 0.15.0 --locked
# Shipped-artifact builds resolve features per-invocation ("selected"), never
# workspace-unified — the repo .cargo/config.toml turns unification on for the
# dev inner loop only, and a unified production closure would pull ffi-/app-only
# stacks (uniffi, wireguard) into the nest image build (
# build-machine-resources.md § Track C-5). Inherited by both cargo stages.
ENV CARGO_RESOLVER_FEATURE_UNIFICATION=selected
# Content-hash freshness instead of mtime (inherited by both cargo stages,
# rust-native + rust-wasm; the Go/Deno stages are unaffected — different bases).
# WHY: cargo's default freshness compares each source file's mtime against the
# mtime stored in its cached fingerprint. That is unreliable here because the
# `rust-target-*-${TARGETARCH}` cache mount is shared MACHINE-WIDE across every
# checkout (the ${TARGETARCH} suffix isolates arch, not checkout), so another
# checkout's recent build — or a `git` operation that leaves an
# edited file's mtime EARLIER than that cached fingerprint — makes cargo wrongly
# decide the crate is "fresh" and `cp` a STALE (or another checkout's) binary,
# even though the source content changed. Reproduced + fixed 2026-06-05: a
# content edit with a backdated mtime served the prior binary under the default
# (mtime) path and the correct one under checksum-freshness. Keying freshness on
# the file CONTENT closes this regardless of mtime, while still keeping the
# ~1039 unchanged dep crates cached (only content-changed crates recompile).
# Requires nightly — which the whole image already pins (rust-toolchain.toml,
# also needed for `-Z threads` below). One-time full recompile when the cache
# first switches from mtime to checksum fingerprints (same one-time cost as any
# RUSTFLAGS/toolchain change), then incremental again.
ENV CARGO_UNSTABLE_CHECKSUM_FRESHNESS=true

# ── Rust native binaries ────────────────────────────────────────────
# Native targets:
#   fauna-nest, fauna-sandbox and fauna-sni-router are built in ONE cargo
#   invocation with `--features bluesky,nostr,activitypub`
#   (bluesky + nostr + activitypub on — the shipping
#   bridge surface, matching the production nest binary; AP mounts its routes
#   with the feature but federates nothing until a user enables it per-actor).
#   ⚠ The BARE feature names, never `fauna-nest/<feat>`: fauna-nest lists itself
#   as a dev-dependency, so cargo binds `fauna-nest/<feat>` to that dev-dep edge,
#   which a build never activates — the features were silently dropped from every
#   image 2026-09-02 → 2026-10-04 (pinned by
#   test_cli_features_never_name_a_self_dev_dep.py + the build workflow's smoke
#   step, which curls one route per feature on the built image). A bare feature
#   applies to every selected package defining it; sandbox and sni-router define
#   none of the three.
#   A member that depends on fauna-nest MUST join this invocation rather than
#   take its own `-p`: a separate one resolves fauna-nest WITHOUT those
#   features, a different feature fingerprint, and recompiles the whole (very
#   expensive) fauna-nest crate a second time on every build.
#   fauna-ffi stays a SEPARATE invocation:
#   fauna-ffi is built --no-default-features --features labeler — these flags
#   MUST match `just mail-bridge-ffi` exactly: the resulting libfauna_ffi.so ABI
#   is what the checked-in libs/fauna-mail-go/ bindings were generated against,
#   and the bridges-builder stage links against it. A flag drift here is a
#   LINKER failure at image build (run 28898957135: the labeler feature landed
#   in the recipe but not here → the bindings referenced
#   run_wasm_labeler_score/mail_to_labeler_input_bare the .so didn't export).
#   Merging fauna-ffi into the nest invocation would feature-unify
#   --no-default-features away and change that ABI, so it stays separate.
#   fauna-iroh-relay (the self-hosted P2P relay sidecar) is also a SEPARATE
#   invocation: it does NOT
#   depend on fauna-nest (it dials the sidecar channel via shared crates only —
#   fauna-protocol / fauna-ws-substrate / fauna-mls), so building it separately
#   reuses those already-compiled crates and only adds iroh-relay's tree under its
#   `relay` feature. Every released image builds it, here and only here: it is
#   never folded into the fauna-nest group, so iroh-relay's tree is compiled
#   into the relay binary and into nothing else.
FROM rust-base AS rust-native
ARG FAUNA_BUILD_COMMIT=dev
ENV FAUNA_BUILD_COMMIT=${FAUNA_BUILD_COMMIT}
# Cap parallel rustc invocations (see the global BUILD_JOBS comment). cargo
# otherwise runs ~one rustc per core (≈ nproc, e.g. 28 on the build VM) → saturates the shared
# host. With the cap, the wide middle of the build (many small crates) tops out at
# BUILD_JOBS cores; the `-Z threads` front-end below only loads cores on the few
# big-crate tails (≤1-2 in flight), so aggregate stays near BUILD_JOBS throughout.
ARG BUILD_JOBS
ENV CARGO_BUILD_JOBS=${BUILD_JOBS}
# RUSTFLAGS for the native binaries (NOT rust-wasm, which inherits rust-base but
# never sets this):
#   -Z threads=8        parallelize the rustc front-end (nightly-only; the whole
#                       Docker build is on the pinned nightly per rust-base). The
#                       big fauna-nest crate codegen dominates the incremental
#                       rebuild, so the parallel front-end is the larger lever.
#   -C link-arg=-fuse-ld=mold  link with mold (much faster relinks) instead of
#                       GNU ld. Native-only: wasm32 links with wasm-ld, where the
#                       flag is invalid.
# Setting RUSTFLAGS changes cargo's fingerprint, so the first build after a change
# here is a one-time full recompile.
ENV RUSTFLAGS="-Z threads=8 -C link-arg=-fuse-ld=mold"
COPY Cargo.toml Cargo.lock ./
COPY libs/ libs/
COPY bins/ bins/
# Keep EVERY workspace member present so the committed Cargo.lock stays canonical
# for `--locked` (S6, release-integrity.md § "--locked in every shipped build").
# We do NOT strip apps/services/tools members from [workspace].members: stripping
# prunes app-only crates from the resolution, which collapses cross-boundary
# DUPLICATE versions (quick-xml, textwrap, unicode-width — pulled at one version by
# a kept runtime crate via c2pa/clap3 and at another by a stripped app via
# clap2/ksni) down to a single UNqualified ref in a *kept* package's lock entry. The
# committed full-workspace lock (which qualifies those refs because both versions
# exist) is then no longer canonical for the stripped subset, so `cargo build
# --locked` fails "cannot update the lock file" (the regression: --locked
# was added 2026-07-05 but its Docker-build validation was deferred, so this only
# surfaced on the next production build). Instead we COPY the member trees so cargo
# resolves the FULL workspace. These members are LEAVES — nothing in the nest build
# graph (fauna-nest / -sandbox / -sni-router / -ffi / -iroh-relay)
# depends on them — so their source is resolved but never compiled and never
# reaches the runtime image. Copy just the member trees (fauna-linux + the six
# apps/fauna-windows service/shell crates + cors-proxy + the additive-evolution lint),
# NOT all of apps/ (which holds the multi-GB web/android/apple client sources; those
# and ios/apple are not cargo members — see .dockerignore). Add a COPY here (and in
# rust-wasm below) when a new apps/services/tools cargo member lands.
COPY apps/fauna-tui apps/fauna-tui
COPY apps/fauna-linux apps/fauna-linux
COPY apps/fauna-windows apps/fauna-windows
COPY services/fauna-cors-proxy services/fauna-cors-proxy
COPY services/fauna-front-door services/fauna-front-door
COPY tools/check-additive-evolution tools/check-additive-evolution
# The target/ cache mount carries compiled deps + crates across builds; cargo's
# incremental fingerprinting recompiles only what changed. Binaries are cp-ed to
# /out in the same RUN because a cache-mounted target/ is not kept in the layer.
#
# The target/ cache id is suffixed with ${TARGETARCH}: target/ holds host-arch
# compiled artifacts, so an amd64 and an arm64 build sharing one id clobber each
# other (each finds the other arch's release/ and recompiles from scratch). A
# multi-arch build (both arches on one runner, the CI case) MUST keep them
# separate or the cache is useless. The registry/git mounts hold arch-independent
# crate sources, so they stay shared across arches (saves re-downloading).
ARG TARGETARCH
RUN --mount=type=cache,id=cargo-registry-native,target=/usr/local/cargo/registry \
    --mount=type=cache,id=cargo-git-native,target=/usr/local/cargo/git \
    --mount=type=cache,id=rust-target-native-${TARGETARCH},target=/src/target \
    cargo build --locked --release -p fauna-nest -p fauna-sandbox -p fauna-sni-router --features bluesky,nostr,activitypub \
    && cargo build --locked --release -p fauna-ffi --no-default-features --features labeler \
    && cargo build --locked --release -p fauna-iroh-relay --features fauna-iroh-relay/relay \
    && strip /src/target/release/fauna-nest \
    && strip /src/target/release/fauna-sandbox \
    && strip /src/target/release/fauna-sni-router \
    && strip /src/target/release/fauna-iroh-relay \
    && mkdir -p /out \
    && cp /src/target/release/fauna-nest \
          /src/target/release/fauna-sandbox \
          /src/target/release/fauna-sni-router \
          /src/target/release/fauna-iroh-relay \
          /src/target/release/libfauna_ffi.so /out/
# NB: libfauna_ffi.so is deliberately NOT stripped — the bridges-builder
# stage links the Go bridge against it via cgo, and the final image loads it at
# runtime (rpath /usr/local/lib). strip would drop nothing cgo needs but buys
# nothing either; leave it intact.

# ── WASM module (for web SPA) ───────────────────────────────────────
# wasm-pack writes its output to libs/<crate>/pkg/ (real paths kept in the
# layer); only the compile intermediates live in the cache-mounted target/, so
# no cp-out is needed here.
#
# ⚠ This stage's cargo cache ids (registry/git/target) are all suffixed `-wasm`,
# distinct from rust-native's `-native` ids. rust-native and rust-wasm build in
# PARALLEL; a shared registry cache mount lets two cargo processes unpack the
# same crate into one dir at once, racing on `.cargo-ok` ("File exists (os error
# 17)"). Separate ids give each stage its own cache — full parallelism, no race.
# Do not consolidate them back to shared ids.
FROM rust-base AS rust-wasm
# Same parallel-rustc cap as rust-native — wasm-pack drives cargo, which honors
# CARGO_BUILD_JOBS (see the global BUILD_JOBS comment). But CARGO_BUILD_JOBS caps
# ONLY the cargo/rustc compile; the binaryen `wasm-opt` size pass wasm-pack runs
# afterwards spawns one thread per core with NO job/thread flag, so it escapes
# this cap and would saturate the host. The wasm-pack RUN below pins the whole
# tree to BUILD_JOBS cores with `taskset` to confine it — the same affinity-mask
# approach the justfile uses for local wasm builds (build-system.md § WASM build
# CPU cap explains why affinity, not a job flag, is the only cap that holds).
ARG BUILD_JOBS
ENV CARGO_BUILD_JOBS=${BUILD_JOBS}
# Remap this stage's checkout (/src), CARGO_HOME (/usr/local/cargo) and cargo
# target (/src/target) roots to the SAME placeholders the justfile's
# CARGO_TARGET_WASM32_UNKNOWN_UNKNOWN_RUSTFLAGS export uses, so the image's wasm
# chunks carry no build-environment path and match a `just web` rebuild of the
# same commit (release-integrity.md § Release signing → Web-app verifiability;
# the justfile comment owns the rationale). wasm32-only: build scripts and
# proc-macros compile unchanged.
ENV CARGO_TARGET_WASM32_UNKNOWN_UNKNOWN_RUSTFLAGS="--remap-path-prefix=/src=/fauna --remap-path-prefix=/usr/local/cargo=/cargo --remap-path-prefix=/src/target=/fauna/target"
COPY Cargo.toml Cargo.lock ./
COPY libs/ libs/
COPY bins/ bins/
# Keep every workspace member present for `--locked` — same rationale as the
# rust-native stage (the strip-vs-`--locked` lock-canonicality incompatibility);
# wasm-pack drives `cargo build --locked`, which validates the WHOLE-workspace
# resolution regardless of which crate is built, so the stripped subset would fail
# here too. Copy the member trees instead of stripping them.
COPY apps/fauna-tui apps/fauna-tui
COPY apps/fauna-linux apps/fauna-linux
COPY apps/fauna-windows apps/fauna-windows
COPY services/fauna-cors-proxy services/fauna-cors-proxy
COPY services/fauna-front-door services/fauna-front-door
COPY tools/check-additive-evolution tools/check-additive-evolution
# target/ id suffixed with ${TARGETARCH} — see the rust-native stage: the wasm32
# OUTPUT is host-arch-independent, but the build-script/proc-macro intermediates
# in target/ are host-arch artifacts, so amd64 and arm64 builds must not share
# this mount.
ARG TARGETARCH
# The wasm-pack tool cache (/root/.cache/.wasm-pack) holds the wasm-bindgen-cli
# install plus the downloaded binaryen (wasm-opt) tarball. Persist it via a cache
# mount so retries stop re-downloading binaryen from GitHub releases on the
# critical path — same rationale as the Go module/build caches below. A flaky
# GitHub 504 on the binaryen asset was killing this stage at ~96s; once the
# tarball lands in the mount it is reused by every subsequent build.
# `taskset -c 0-$((BUILD_JOBS-1))` pins each wasm-pack — and the cargo/rustc/
# wasm-opt children that inherit the affinity mask — to BUILD_JOBS cores. This is
# what actually caps wasm-opt, which has no thread flag (see the stage comment
# above); it mirrors the justfile's per-invocation taskset prefix. Assumes
# BUILD_JOBS ≤ the build host's core count (the BUILD_JOBS=16-on-28-cores
# contract) — taskset errors on a range wider than the online CPUs.
# The trailing `-- --locked` passes through to the underlying `cargo build`
# (wasm-pack's EXTRA_OPTIONS) — these WASM artifacts ship into the nest image
# via web-builder below, so release-integrity.md's "--locked in every shipped
# build" applies here too, same as the rust-native stage.
RUN --mount=type=cache,id=cargo-registry-wasm,target=/usr/local/cargo/registry \
    --mount=type=cache,id=cargo-git-wasm,target=/usr/local/cargo/git \
    --mount=type=cache,id=rust-target-wasm-${TARGETARCH},target=/src/target \
    --mount=type=cache,id=wasm-pack-tool-cache,target=/root/.cache/.wasm-pack \
    CPUSET="0-$((BUILD_JOBS - 1))" \
    && taskset -c "$CPUSET" wasm-pack build libs/fauna-wasm --target web -- --locked \
    && taskset -c "$CPUSET" wasm-pack build libs/fauna-wasm-onboarding --target web -- --locked \
    && taskset -c "$CPUSET" wasm-pack build libs/fauna-wasm-launch --target web -- --locked \
    && taskset -c "$CPUSET" wasm-pack build libs/fauna-wasm-folders --target web -- --locked \
    && taskset -c "$CPUSET" wasm-pack build libs/fauna-wasm-backups --target web -- --locked \
    && taskset -c "$CPUSET" wasm-pack build libs/fauna-wasm-media --target web -- --locked \
    && taskset -c "$CPUSET" wasm-pack build libs/fauna-wasm-labeler-catalog --target web -- --locked \
    && taskset -c "$CPUSET" wasm-pack build libs/fauna-wasm-share --target web -- --locked \
    && taskset -c "$CPUSET" wasm-pack build libs/fauna-wasm-connected-apps --target web -- --locked \
    && taskset -c "$CPUSET" wasm-pack build libs/fauna-wasm-atproto-settings --target web -- --locked

# ── Go mail-bridge (cgo against libfauna_ffi.so) + supervisor sidekick ──
# The Go mail-bridge links the UniFFI shared library libfauna_ffi.so via cgo.
# This mirrors `just mail-bridge-build` /
# `mail-bridge-ffi`: the checked-in libs/fauna-mail-go/ bindings #include
# namespace-local headers and resolve symbols against the .so at link + runtime.
# Layout is preserved under /src so the go.mod `replace ... => ../../libs/
# fauna-mail-go` directive resolves.
#
# The Go module cache (/go/pkg/mod) and build cache (/root/.cache/go-build) are
# persisted via --mount=type=cache so the stage stops fetching modules from the
# network on the critical path of every retry — a hung `go mod download` (no read
# timeout) was the original ~90-min stall this whole change exists to prevent.
# Build the Go mail bridge on the BUILD platform and CROSS-COMPILE to the target
# arch — it must NEVER run under emulation. The amd64 `go` toolchain under
# Rosetta DEADLOCKS during heavy cold builds: go's runtime drops queued
# SIGCHLD, so its wait() for dead `compile`/`asm` children never returns — all
# runtime threads sleep, zombies pile up, the build hangs to the job timeout.
# (Reproduced on a self-hosted CI runner, 2026-06-13; diagnosed via 27 sleeping
# threads + SigQ:9 + unreaped zombie children.) Rust under Rosetta is fine, but the
# Go subprocess fan-out trips it. `--platform=$BUILDPLATFORM` keeps go native;
# GOARCH=$TARGETARCH cross-compiles (CGO via the matching cross C toolchain). This
# also removes Rosetta's interpretation overhead from the Go stage entirely.
# Pinned to an EXACT patch version (never the floating golang:1.26-bookworm tag) so
# the shipped Go toolchain is reproducible and the govulncheck gate scans exactly
# what we release. Kept equal to bins/fauna-bridges/go.mod's `toolchain`
# directive (the single source of truth) by a dedicated merge-gate toolchain-sync
# gate. Bump both together — § Go toolchain pin in build-system.md.
FROM --platform=$BUILDPLATFORM golang:1.26.8-bookworm@sha256:a688600ca24f8a4d3ca77f95b0dd40704a9fc787c826660eb7ba0b641b8b175d AS bridges-builder
WORKDIR /src
# Cap Go's build parallelism to match the Rust stages (see the global BUILD_JOBS
# comment) so the mail-bridge cross-compile doesn't spike the shared host.
ARG BUILD_JOBS
ENV GOMAXPROCS=${BUILD_JOBS}
# Cross C toolchains for CGO (fauna-mail-bridge links libfauna_ffi). The CC is
# selected by $TARGETARCH below; for a native target the matching triple is just
# the host compiler. The libc6-dev-*-cross packages carry the target-arch libc
# DEV headers (bits/libc-header-start.h, bits/wordsize.h, …) into
# /usr/<triple>/include — the cross gcc Recommends but does NOT Depend on them,
# so --no-install-recommends drops them and the cross compile dies with
# "fatal error: bits/libc-header-start.h: No such file or directory". Install
# both arches' dev libc so the build cross-compiles regardless of $BUILDPLATFORM.
# Kept in its own layer so source changes don't re-apt.
RUN apt-get update \
    && apt-get install -y --no-install-recommends \
       gcc-x86-64-linux-gnu gcc-aarch64-linux-gnu \
       libc6-dev-amd64-cross libc6-dev-arm64-cross \
    && rm -rf /var/lib/apt/lists/*
COPY --from=rust-native /out/libfauna_ffi.so /usr/local/lib/libfauna_ffi.so
COPY libs/fauna-mail-go/ libs/fauna-mail-go/
COPY bins/fauna-bridges/ bins/fauna-bridges/
# go-build holds target-arch compiled objects → suffix with ${TARGETARCH} so the
# amd64 and arm64 builds keep separate caches (see rust-native). go-mod is
# arch-independent module source, so it stays shared.
ARG TARGETARCH
RUN --mount=type=cache,id=go-mod,target=/go/pkg/mod \
    --mount=type=cache,id=go-build-${TARGETARCH},target=/root/.cache/go-build \
    set -e; \
    case "$TARGETARCH" in \
      amd64) CC=x86_64-linux-gnu-gcc ;; \
      arm64) CC=aarch64-linux-gnu-gcc ;; \
      *) echo "unsupported TARGETARCH=$TARGETARCH" >&2; exit 1 ;; \
    esac; \
    export CC GOOS=linux GOARCH="$TARGETARCH"; \
    INCLUDES=""; for d in /src/libs/fauna-mail-go/*/; do INCLUDES="$INCLUDES -I$d"; done; \
    export CGO_CFLAGS="$INCLUDES"; \
    export CGO_LDFLAGS="-L/usr/local/lib -lfauna_ffi -Wl,-rpath,/usr/local/lib"; \
    export LD_LIBRARY_PATH="/usr/local/lib"; \
    CGO_ENABLED=1 go -C bins/fauna-bridges build -trimpath -ldflags '-s -w' -o /fauna-mail-bridge ./cmd/fauna-mail-bridge; \
    CGO_ENABLED=0 go -C bins/fauna-bridges build -trimpath -ldflags '-s -w' -o /fauna-supervisor ./cmd/fauna-supervisor; \
    CGO_ENABLED=1 go -C bins/fauna-bridges build -trimpath -ldflags '-s -w' -o /fauna-atproto-bridge ./cmd/fauna-atproto-bridge

# ── Web SPA ─────────────────────────────────────────────────────────
FROM denoland/deno:latest@sha256:2014dc167ece617ef7e7ba40631ac2234c59e75ce693e7cc2dc2602b3c87859d AS web-builder
WORKDIR /src
COPY apps/fauna-web/ apps/fauna-web/
# The launcher every SPA deno task runs vite through (build-system.md § The Deno
# build sandbox); this stage has no git, so VITE_GIT_SHA below carries the commit.
COPY scripts/deno-sandbox.ts scripts/deno-sandbox-preload.mjs scripts/
# Copy WASM artifacts into the SPA static dir — all four files wasm-pack emits
# per chunk, exactly the set `just wasm` mirrors into static/
# (scripts/sync-wasm-static.py), so this tree and a local `just web` serve the
# same files and one asset-manifest.json covers both (release-integrity.md §
# Release signing → Web-app verifiability). The two `.d.ts` are type
# declarations nothing loads at runtime; a merge gate pins the set.
COPY --from=rust-wasm /src/libs/fauna-wasm/pkg/fauna_wasm_bg.wasm apps/fauna-web/static/
COPY --from=rust-wasm /src/libs/fauna-wasm/pkg/fauna_wasm.js apps/fauna-web/static/
COPY --from=rust-wasm /src/libs/fauna-wasm/pkg/fauna_wasm.d.ts apps/fauna-web/static/
COPY --from=rust-wasm /src/libs/fauna-wasm/pkg/fauna_wasm_bg.wasm.d.ts apps/fauna-web/static/
COPY --from=rust-wasm /src/libs/fauna-wasm-onboarding/pkg/fauna_wasm_onboarding_bg.wasm apps/fauna-web/static/
COPY --from=rust-wasm /src/libs/fauna-wasm-onboarding/pkg/fauna_wasm_onboarding.js apps/fauna-web/static/
COPY --from=rust-wasm /src/libs/fauna-wasm-onboarding/pkg/fauna_wasm_onboarding.d.ts apps/fauna-web/static/
COPY --from=rust-wasm /src/libs/fauna-wasm-onboarding/pkg/fauna_wasm_onboarding_bg.wasm.d.ts apps/fauna-web/static/
COPY --from=rust-wasm /src/libs/fauna-wasm-launch/pkg/fauna_wasm_launch_bg.wasm apps/fauna-web/static/
COPY --from=rust-wasm /src/libs/fauna-wasm-launch/pkg/fauna_wasm_launch.js apps/fauna-web/static/
COPY --from=rust-wasm /src/libs/fauna-wasm-launch/pkg/fauna_wasm_launch.d.ts apps/fauna-web/static/
COPY --from=rust-wasm /src/libs/fauna-wasm-launch/pkg/fauna_wasm_launch_bg.wasm.d.ts apps/fauna-web/static/
COPY --from=rust-wasm /src/libs/fauna-wasm-folders/pkg/fauna_wasm_folders_bg.wasm apps/fauna-web/static/
COPY --from=rust-wasm /src/libs/fauna-wasm-folders/pkg/fauna_wasm_folders.js apps/fauna-web/static/
COPY --from=rust-wasm /src/libs/fauna-wasm-folders/pkg/fauna_wasm_folders.d.ts apps/fauna-web/static/
COPY --from=rust-wasm /src/libs/fauna-wasm-folders/pkg/fauna_wasm_folders_bg.wasm.d.ts apps/fauna-web/static/
COPY --from=rust-wasm /src/libs/fauna-wasm-backups/pkg/fauna_wasm_backups_bg.wasm apps/fauna-web/static/
COPY --from=rust-wasm /src/libs/fauna-wasm-backups/pkg/fauna_wasm_backups.js apps/fauna-web/static/
COPY --from=rust-wasm /src/libs/fauna-wasm-backups/pkg/fauna_wasm_backups.d.ts apps/fauna-web/static/
COPY --from=rust-wasm /src/libs/fauna-wasm-backups/pkg/fauna_wasm_backups_bg.wasm.d.ts apps/fauna-web/static/
COPY --from=rust-wasm /src/libs/fauna-wasm-media/pkg/fauna_wasm_media_bg.wasm apps/fauna-web/static/
COPY --from=rust-wasm /src/libs/fauna-wasm-media/pkg/fauna_wasm_media.js apps/fauna-web/static/
COPY --from=rust-wasm /src/libs/fauna-wasm-media/pkg/fauna_wasm_media.d.ts apps/fauna-web/static/
COPY --from=rust-wasm /src/libs/fauna-wasm-media/pkg/fauna_wasm_media_bg.wasm.d.ts apps/fauna-web/static/
COPY --from=rust-wasm /src/libs/fauna-wasm-labeler-catalog/pkg/fauna_wasm_labeler_catalog_bg.wasm apps/fauna-web/static/
COPY --from=rust-wasm /src/libs/fauna-wasm-labeler-catalog/pkg/fauna_wasm_labeler_catalog.js apps/fauna-web/static/
COPY --from=rust-wasm /src/libs/fauna-wasm-labeler-catalog/pkg/fauna_wasm_labeler_catalog.d.ts apps/fauna-web/static/
COPY --from=rust-wasm /src/libs/fauna-wasm-labeler-catalog/pkg/fauna_wasm_labeler_catalog_bg.wasm.d.ts apps/fauna-web/static/
COPY --from=rust-wasm /src/libs/fauna-wasm-share/pkg/fauna_wasm_share_bg.wasm apps/fauna-web/static/
COPY --from=rust-wasm /src/libs/fauna-wasm-share/pkg/fauna_wasm_share.js apps/fauna-web/static/
COPY --from=rust-wasm /src/libs/fauna-wasm-share/pkg/fauna_wasm_share.d.ts apps/fauna-web/static/
COPY --from=rust-wasm /src/libs/fauna-wasm-share/pkg/fauna_wasm_share_bg.wasm.d.ts apps/fauna-web/static/
COPY --from=rust-wasm /src/libs/fauna-wasm-connected-apps/pkg/fauna_wasm_connected_apps_bg.wasm apps/fauna-web/static/
COPY --from=rust-wasm /src/libs/fauna-wasm-connected-apps/pkg/fauna_wasm_connected_apps.js apps/fauna-web/static/
COPY --from=rust-wasm /src/libs/fauna-wasm-connected-apps/pkg/fauna_wasm_connected_apps.d.ts apps/fauna-web/static/
COPY --from=rust-wasm /src/libs/fauna-wasm-connected-apps/pkg/fauna_wasm_connected_apps_bg.wasm.d.ts apps/fauna-web/static/
COPY --from=rust-wasm /src/libs/fauna-wasm-atproto-settings/pkg/fauna_wasm_atproto_settings_bg.wasm apps/fauna-web/static/
COPY --from=rust-wasm /src/libs/fauna-wasm-atproto-settings/pkg/fauna_wasm_atproto_settings.js apps/fauna-web/static/
COPY --from=rust-wasm /src/libs/fauna-wasm-atproto-settings/pkg/fauna_wasm_atproto_settings.d.ts apps/fauna-web/static/
COPY --from=rust-wasm /src/libs/fauna-wasm-atproto-settings/pkg/fauna_wasm_atproto_settings_bg.wasm.d.ts apps/fauna-web/static/
WORKDIR /src/apps/fauna-web
# Persist Deno's dependency cache across builds. Any apps/fauna-web/** change
# invalidates the COPY layer above and forces `deno install` to re-resolve; the
# DENO_DIR cache mount means it reuses already-downloaded deps instead of
# re-fetching them from the network on every web-source change.
ENV DENO_DIR=/deno-dir
# The build runs without bubblewrap in this stage, by the launcher's one
# artifact-set opt-out: user namespaces are off inside Docker, so bubblewrap
# cannot run, and the stage holds no credential (build-system.md § The Deno
# build sandbox, gap (2)).
ENV FAUNA_DENO_SANDBOX_NO_BWRAP=1
RUN --mount=type=cache,id=deno-dir,target=/deno-dir deno install
# The SPA's build identity is the commit (apps/fauna-web/build-id.js), so two
# builds of one ref yield a byte-identical tree — release-integrity.md
# § Release signing → Web-app verifiability, piece 1. This stage has no git,
# so the commit arrives as the same FAUNA_BUILD_COMMIT build argument the
# rust-native stage compiles in; declared AFTER `deno install` so a new commit
# invalidates only the build layer, never the dependency one. The local
# default `dev` is treated by build-id.js as "no commit known" and keeps
# SvelteKit's timestamp version.
ARG FAUNA_BUILD_COMMIT=dev
ENV VITE_GIT_SHA=${FAUNA_BUILD_COMMIT}
RUN --mount=type=cache,id=deno-dir,target=/deno-dir deno task build

# The served SPA tree alone — exactly what the final stage copies to
# /usr/share/fauna-web/ — for `--target web-tree --output type=local`, so the
# reproducibility probe workflow (web-tree-reproducibility.yml) can
# extract and hash it without building rust-native or pushing an image
# (release-integrity.md § Release signing → Web-app verifiability). Not on the
# final image's path: nothing below copies from it.
FROM scratch AS web-tree
COPY --from=web-builder /src/apps/fauna-web/build/ /

# ── Final image ─────────────────────────────────────────────────────
FROM debian:bookworm-slim@sha256:88200866dfff7ea7f5cbcb6ec7c8a701889efe6fe859fe64d6990e4b07ea4171

ARG S6_OVERLAY_VERSION=3.2.0.2

# Install s6-overlay (install curl first since debian:bookworm-slim doesn't have it)
RUN apt-get update && apt-get install -y --no-install-recommends ca-certificates curl xz-utils \
    && rm -rf /var/lib/apt/lists/*
RUN S6_ARCH=$(case $(dpkg --print-architecture) in amd64) echo x86_64;; arm64) echo aarch64;; *) echo $(dpkg --print-architecture);; esac) \
    && curl -sL "https://github.com/just-containers/s6-overlay/releases/download/v${S6_OVERLAY_VERSION}/s6-overlay-noarch.tar.xz" -o /tmp/s6-overlay-noarch.tar.xz \
    && curl -sL "https://github.com/just-containers/s6-overlay/releases/download/v${S6_OVERLAY_VERSION}/s6-overlay-${S6_ARCH}.tar.xz" -o /tmp/s6-overlay-arch.tar.xz \
    && tar -C / -Jxpf /tmp/s6-overlay-noarch.tar.xz \
    && tar -C / -Jxpf /tmp/s6-overlay-arch.tar.xz \
    && rm /tmp/s6-overlay-*.tar.xz

# Runtime dependencies (ffmpeg for video transcoding; libcap2-bin for setcap —
# the mail bridge / SNI router bind privileged ports 25/465/587/993/443 as their
# non-root per-role UIDs (fauna-mta / fauna-mda / fauna-router) via
# CAP_NET_BIND_SERVICE, a file capability granted at execve regardless of UID)
RUN apt-get update \
    && apt-get install -y --no-install-recommends jq ffmpeg libcap2-bin \
    && rm -rf /var/lib/apt/lists/*

# Co-resident process trust boundary (security.md § UID isolation): each
# network-facing s6 service runs under its OWN UID so a compromised process
# cannot read another's key material or nest's sealed store. `fauna` (1000) is
# NEST — it owns the sealed store (/data/nest.db, /data/blobs, /data/acme) and
# the per-deployment secrets. The bridges (fauna-mta / fauna-mda) and the SNI
# router (fauna-router) get distinct non-root UIDs; they need NO write access to
# /data beyond their own UID-isolated key subdir (/data/keys/<role>/), and the
# router needs none at all. Privileged-port binding works for these non-root
# UIDs via the file capability set with `setcap` below (granted at execve
# regardless of UID), so none of them needs root. The router's distinct UID also
# lets nest distinguish the legitimate PROXY-header writer from the bridges via
# SO_PEERCRED. The supervisor stays
# root (it calls s6-svc).
RUN useradd -r -u 1000 -d /data -m -s /usr/sbin/nologin fauna \
    && useradd -r -u 1001 -d /nonexistent -M -s /usr/sbin/nologin fauna-mta \
    && useradd -r -u 1002 -d /nonexistent -M -s /usr/sbin/nologin fauna-mda \
    && useradd -r -u 1003 -d /nonexistent -M -s /usr/sbin/nologin fauna-router \
    && useradd -r -u 1004 -d /nonexistent -M -s /usr/sbin/nologin fauna-relay \
    && useradd -r -u 1005 -d /nonexistent -M -s /usr/sbin/nologin fauna-atproto

# Copy binaries from build stages
COPY --from=rust-native /out/fauna-nest /usr/local/bin/fauna-nest
# The self-hosted iroh P2P relay sidecar. No setcap: it binds unprivileged ports
# only — the relay protocol on loopback (127.0.0.1:8445 HTTPS, :8446 HTTP),
# SNI-fronted at :443 by fauna-sni-router, and its address-discovery server on
# 0.0.0.0:7842/udp (the one port it exposes itself; docker-compose.yml publishes
# it) — so it needs no privileged-port capability. Runs as the non-root fauna-relay UID
# (s6 run-script), reading its cert as a sealed blob over the sidecar channel —
# never /data/acme (security.md § UID isolation).
COPY --from=rust-native /out/fauna-iroh-relay /usr/local/bin/fauna-iroh-relay
COPY --from=rust-native /out/fauna-sandbox /usr/local/bin/fauna-sandbox
# fauna-sandbox wraps the mail bridges (the bridge / bridge-imap profiles) under
# Landlock + seccomp. Those profiles set no_new_privs, which would drop the
# bridge binary's OWN cap_net_bind_service file-cap at execve — so fauna-sandbox
# carries the cap itself and raises it into the ambient set (which survives
# no_new_privs) before exec'ing the bridge, letting the sandboxed MTA/MDA still
# bind 25/465/587 / 993/143 (main.rs raise_net_bind_ambient).
RUN setcap cap_net_bind_service=+ep /usr/local/bin/fauna-sandbox

# fauna-sni-router fronts container :443 as the non-root `fauna-router` user (the
# fauna-sni-router s6 service), L4 SNI-routing nest vs the MDA CalDAV listener.
# setcap lets it bind the privileged port, same as the mail bridge below.
COPY --from=rust-native /out/fauna-sni-router /usr/local/bin/fauna-sni-router
RUN setcap cap_net_bind_service=+ep /usr/local/bin/fauna-sni-router

# Mail bridge (cgo) + its FFI shared library + the supervisor sidekick.
# libfauna_ffi.so goes to /usr/local/lib (matched by the binary's rpath);
# ldconfig refreshes the dynamic-linker cache. setcap lets the bridge bind
# privileged ports under its non-root per-role UID (fauna-mta / fauna-mda;
# Docker's default capability bounding set includes CAP_NET_BIND_SERVICE).
COPY --from=rust-native /out/libfauna_ffi.so /usr/local/lib/libfauna_ffi.so
COPY --from=bridges-builder /fauna-mail-bridge /usr/local/bin/fauna-mail-bridge
COPY --from=bridges-builder /fauna-supervisor /usr/local/bin/fauna-supervisor
# The ATProto PDS bridge (role atproto.pds). CGO like the mail bridge (it links
# libfauna_ffi for the sealed-blob unseal — the shared .so copied above at
# /usr/local/lib covers it). Unlike the mail bridge it needs NO setcap: its F1
# XRPC listener binds an UNPRIVILEGED loopback high port (127.0.0.1:8447; the SNI
# router fronts public :443 and forwards here), and it terminates `pds.<domain>`
# TLS itself via the sealed cert blob. Runs as the non-root fauna-atproto UID (its
# s6 run-script). docs/goal/behavior/atproto-pds-full.md § Wire & process topology.
COPY --from=bridges-builder /fauna-atproto-bridge /usr/local/bin/fauna-atproto-bridge
RUN ldconfig \
    && setcap cap_net_bind_service=+ep /usr/local/bin/fauna-mail-bridge

# Copy web SPA build output
COPY --from=web-builder /src/apps/fauna-web/build/ /usr/share/fauna-web/

# Copy default config (base for first-run initialization)
COPY config/default.toml /etc/fauna/default.toml

# Copy entrypoint and s6 service definitions
COPY docker/entrypoint.sh /entrypoint.sh
COPY docker/rebless-bridge-key.sh /usr/local/bin/rebless-bridge-key.sh
# The entrypoint's nest.toml writes, split out so they are runnable — and so
# testable without docker — against the real config/default.toml
# (tests/docker/test_entrypoint_overlay.py, `just entrypoint-test`).
COPY docker/nest-toml-overlay.sh /usr/local/bin/nest-toml-overlay.sh
COPY docker/s6/ /etc/s6-overlay/s6-rc.d/
RUN chmod +x /entrypoint.sh /usr/local/bin/rebless-bridge-key.sh \
        /usr/local/bin/nest-toml-overlay.sh \
    && find /etc/s6-overlay/s6-rc.d -name run -exec chmod +x {} +

VOLUME /data
# 443 = SNI router (fronts nest + MDA CalDAV); 3000 = nest's internal listener
# (FAUNA_PORT; the router's loopback target); 8443 = the MDA's user-facing CalDAV
# port on a bare-IP box (no SNI to front it); 465 = implicit-TLS submission;
# 587 = STARTTLS submission; 993 = IMAPS.
EXPOSE 8080 3000 443 8443 25 465 587 993

# Probe the nest's own listener (FAUNA_PORT, default 3000 — the entrypoint's
# value and what the compose sets in production). Follows FAUNA_PORT so a port
# override stays healthy.
HEALTHCHECK --interval=30s --timeout=5s --start-period=15s --retries=3 \
    CMD curl -sf http://localhost:${FAUNA_PORT:-3000}/api/v1/health 2>/dev/null || curl -sfk https://localhost:${FAUNA_PORT:-3000}/api/v1/health || exit 1

# Per-build artifact identity, served by /api/v1/health beside `commit` so the
# release pipeline's promotion gate can bind to THIS ARTIFACT and not merely to
# a commit string — two builds of one commit are otherwise indistinguishable to
# it, and a container the pipeline never built could satisfy the gate while
# :latest moved to the one it did (see build-system.md § Image tags & channels).
#
# Deliberately placed LAST and read at RUNTIME (bins/fauna-nest/src/
# build_identity.rs owns the rationale), unlike FAUNA_BUILD_COMMIT which the
# rust-native stage bakes in at compile time:
#   - it changes on EVERY build, so stamping it before `cargo build` would
#     recompile fauna-nest on each re-dispatch of an unchanged commit, and
#     placing it here invalidates no earlier layer;
#   - the image config is readable off an already-built artifact
#     (`docker inspect --format '{{range .Config.Env}}...'`), which is what lets
#     restage-nest.yml verify an image it did not build.
# It reaches the nest process because docker/s6/fauna-nest/run is a
# `with-contenv` script and execs `env VAR=... fauna-nest` (adding to, never
# replacing, the inherited container environment).
ARG FAUNA_BUILD_ID=dev
ENV FAUNA_BUILD_ID=${FAUNA_BUILD_ID}
# FAUNA_BUILD_COMMIT is COMPILED into fauna-nest by the rust-native stage above
# (build_identity.rs); it is repeated here as image config so the artifact can be
# identified without booting it — the release-candidate gate reads it off
# `docker inspect`'s `.Config.Env` (feature-catalog.md § Release-candidate run,
# `helpers/feature_ledger.release_candidate_refusal_for_image`). Every image
# built before 2026-08-29 carried only FAUNA_BUILD_ID here, so that gate refused
# them all as "carries no FAUNA_BUILD_COMMIT". Same layer discipline as the ID:
# metadata only, last, invalidating nothing. The nest process ignores the
# runtime value (`option_env!` is compile-time).
ARG FAUNA_BUILD_COMMIT=dev
ENV FAUNA_BUILD_COMMIT=${FAUNA_BUILD_COMMIT}
# The standards-track twin of the `.Config.Env` identity above: the same
# `FAUNA_BUILD_COMMIT` value, also as an OCI label, so a tool that reads
# labels (not env) can answer "which ref was this built from" with one
# `docker inspect --format '{{index .Config.Labels
# "org.opencontainers.image.revision"}}'` instead of a behavioural inference
# (closes the other half of testing.md class (7)) — a `dev` value on a local build is expected and excluded from the
# 40-hex assertion the same way `FAUNA_BUILD_ID`'s test-hooks default is.
LABEL org.opencontainers.image.revision=${FAUNA_BUILD_COMMIT} \
      org.opencontainers.image.source="https://github.com/faunasocial/fauna"

ENTRYPOINT ["/entrypoint.sh"]
