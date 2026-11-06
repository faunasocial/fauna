# Cross-checkout compile cache on macOS — public summary

> The full document describes how the maintainers' private macOS development
> machine shares compiled work between checkouts through a compile cache, and
> is not published. Documents in `docs/goal/` that reference
> `build-compile-cache-mac.md` resolve here.

**This repository needs no compile cache**, on macOS or anywhere else. A
plain `cargo build` or `just <recipe>` in a fresh checkout is the supported
path. What generalizes from the private setup is summarized in
[`build-compile-cache.md`](build-compile-cache.md); the macOS-specific
configuration, store limits and measurements are private infrastructure and
out of scope for this repository.
