# Build system — public summary

> The full build-system design document describes the maintainers' private
> build, release, and deployment infrastructure and is not published. This
> abridged summary covers what a contributor building from this repository
> needs; other documents in `docs/goal/` that reference `build-system.md`
> resolve here.

## What you need to build

- **Orchestration is `just`** (see the root `justfile`). Every generator and
  builder recipe is gated by `scripts/build-if-stale.py`, so repeating a
  `just` recipe is cheap (~50 ms when nothing changed) and recipes that
  consume generated files declare the generating recipes as dependencies.
- **Rust toolchain is pinned** in `rust-toolchain.toml` and installs
  automatically on first `cargo` run; CI reads the same file. The pin
  deliberately trails the latest nightly (newer nightlies have broken
  dependencies of the MLS stack). `rustfmt.toml` excludes generated and
  vendored code.
- **Generated files are never edited by hand.** i18n string constants are
  generated from `i18n/strings/en.yaml` (`just i18n-generate`); UniFFI
  bindings for the native clients and the Go mail bridge are generated from
  `libs/fauna-ffi`. `just check-generated` (also a CI job) fails if any
  generated file is stale relative to its source.
- **Per-platform entry points:** `cargo build --workspace` covers the server,
  shared libraries, and Linux client; `just web` builds the WASM chunks and
  the Svelte SPA (Deno, no Node.js); `just mail-bridge-test` builds the Rust
  FFI cdylib and runs the Go mail-bridge suite. The Apple, Android, and
  Windows clients use their platforms' native toolchains under `apps/`.

## What is deliberately not here

Release/deployment specifics — image registries and tag channels, deploy
verification, self-hosted runner layout, and per-machine development caches —
are part of the private infrastructure and out of scope for this repository.
The public CI workflow (`.github/workflows/ci.yml`) is self-contained and is
the buildability contract for this tree.
