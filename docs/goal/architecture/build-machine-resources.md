# Development build resources — public summary

> The full document describes how the maintainers' private development machines
> lay out and ration their build resources, and is not published. This abridged
> summary covers what a contributor needs; other documents in `docs/goal/` that
> reference `build-machine-resources.md` resolve here.

## What a contributor needs to know

- **Build artifacts go where cargo puts them.** This repository needs no
  special target-directory setup: a plain `cargo build` / `just <recipe>` in a
  fresh checkout is the supported path.
- **Builds are wide.** A full workspace build plus the WASM chunks is a
  multi-core, multi-gigabyte job; the debug profile is configured to keep
  debuginfo small (see the workspace `Cargo.toml`'s `[profile.dev]`), which is
  the only part of the maintainers' resource tuning that is committed here.
- **Running several heavy jobs at once is expensive.** The end-to-end suites
  and a workspace build each want most of a machine; if you run them in
  parallel, expect them to be resource-hungry.

Everything else in the full document — how the maintainers' own machines lay
out, ration and measure their builds — is private infrastructure and out of
scope for this repository.
