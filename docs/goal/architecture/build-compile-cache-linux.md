# Cross-checkout compile cache on Linux — public summary

> The full document describes how the maintainers' private Linux development
> machine shares compiled work between checkouts through a compile cache, and
> is not published. Documents in `docs/goal/` that reference
> `build-compile-cache-linux.md` resolve here.

**This repository needs no compile cache**, on Linux or anywhere else. A
plain `cargo build` or `just <recipe>` in a fresh checkout is the supported
path. What generalizes from the private setup is summarized in
[`build-compile-cache.md`](build-compile-cache.md); the Linux-specific
configuration, store limits and measurements are private infrastructure and
out of scope for this repository.
