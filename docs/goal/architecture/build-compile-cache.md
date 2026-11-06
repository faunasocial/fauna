# Cross-checkout compile cache — public summary

> The full document describes how the maintainers' private development
> machines share compiled work between checkouts through a compile cache, and
> is not published. This abridged summary covers what a contributor needs;
> other documents in `docs/goal/` that reference `build-compile-cache.md`
> resolve here.

## What a contributor needs to know

**This repository needs no compile cache.** A plain `cargo build` or
`just <recipe>` in a fresh checkout is the supported path, and nothing in the
build depends on a cache being present.

Two observations from the private setup generalize:

- **A cache keyed by content only helps when the inputs really are equal.** A
  crate whose build script stamps the checkout's current commit into its output
  changes at every commit and misses the cache between checkouts. The project
  therefore reads that stamp only in the crate that is the shipped artifact
  (see [`../ui/status.md`](../ui/status.md)), so the libraries beneath it stay
  cacheable.
- **A linked library is costlier to cache than an rlib.** Keeping the FFI's
  compiled code in an rlib, with a thin library that only links it, lets a
  cache restore the expensive part and leaves only the link to redo.

Everything else — which machines run a cache, its configuration, its store
limits and the measurements behind each ruling — is private infrastructure and
out of scope for this repository.

See also [`build-system.md`](build-system.md) for the build system a contributor
builds with.
