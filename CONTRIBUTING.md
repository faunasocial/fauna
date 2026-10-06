# Contributing to Fauna

Thanks for your interest in Fauna. A few things to know up front.

## Project status

Fauna is in early development. This repository is a **curated, published mirror**:
the maintainers develop in a separate working repository and publish reviewed
snapshots here. As a result:

- History on `main` is a sequence of reviewed snapshots, not individual upstream commits.
- External pull requests are welcome, but are integrated by **re-applying the change
  upstream** and republishing — so a merged change may land as part of a later snapshot
  rather than as your exact commit. Your authorship is preserved in the changelog/credits.
- For anything non-trivial, **open an issue first** to discuss direction before writing code.

## Licensing — no CLA

Fauna is dual-licensed under **Apache-2.0 OR MIT** (see `LICENSE-APACHE` and
`LICENSE-MIT`). There is no Contributor License Agreement. By submitting a
contribution, you agree that it is licensed under the same dual license, and you
retain your copyright.

The licenses cover the code, not the name. The Fauna names and artwork are held
by the association under the [trademark policy](TRADEMARK.md): forks meant for
contributing back may keep the name while the work is in progress; a modified
version distributed to others takes its own name.

## Building

Fauna is a Rust workspace with native clients per platform. The build is
orchestrated by [`just`](https://github.com/casey/just):

```sh
# Rust workspace (server, libraries, Linux and terminal clients)
cargo build --workspace

# Web client (Svelte SPA + WASM) — needs Deno + wasm-pack
just web

# Go mail bridge — needs Go; links the Rust FFI cdylib via cgo
just mail-bridge-test
```

The pinned Rust toolchain is declared in `rust-toolchain.toml` and installs
automatically on first `cargo` run.

Each app and server component has its own README with build, test, and
layout details:

- [`apps/fauna-web`](apps/fauna-web/README.md) — web SPA (Deno + WASM)
- [`apps/fauna-linux`](apps/fauna-linux/README.md) — Linux desktop (GTK4)
- [`apps/fauna-tui`](apps/fauna-tui/README.md) — terminal UI (Rust; builds with the workspace)
- [`apps/fauna-apple`](apps/fauna-apple/README.md) — macOS + iOS (SwiftUI; needs a Mac)
- [`apps/fauna-android`](apps/fauna-android/README.md) — Android (Compose; SDK/NDK setup in its [BUILD-SETUP.md](apps/fauna-android/BUILD-SETUP.md))
- [`apps/fauna-windows`](apps/fauna-windows/README.md) — Windows (WinUI 3; build the app with MSBuild, not `dotnet build`)
- [`bins/fauna-nest`](bins/fauna-nest/README.md) — the server
- [`bins/fauna-bridges`](bins/fauna-bridges/README.md) — the Go mail/calendar bridge

Cross-client e2e tests live in [`tests/e2e-unified`](tests/e2e-unified/README.md).
See `docs/goal/` for the architecture and feature specifications, and
[`docs/README.md`](docs/README.md) for the documentation front door.

## Code of conduct

This project follows the [Code of Conduct](CODE_OF_CONDUCT.md). By participating,
you are expected to uphold it.

## Security

Please do **not** open public issues for security vulnerabilities. See
[SECURITY.md](SECURITY.md) for responsible-disclosure instructions.
