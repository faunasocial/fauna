# Build target-directory layout, Linux — public summary

> The full document describes how the maintainers' private Linux development
> machine lays out and rations its build artifacts, and is not published. This
> abridged summary covers what a contributor needs; other documents in
> `docs/goal/` that reference `build-target-layout-linux.md` resolve here.

## What a contributor needs to know

**This repository needs no special target-directory setup on Linux.** A plain
`cargo build` or `just <recipe>` in a fresh checkout is the supported path;
artifacts go wherever cargo puts them, and nothing in the build depends on the
private fleet's arrangement.

What the private setup adds — a separate filesystem dataset per checkout, a size
quota on each, and garbage-collection tiers that reclaim caches from checkouts
that have gone idle — exists to keep many simultaneous checkouts of this
repository on one machine from exhausting a shared disk. Two of its findings
generalize, and are the reason the arrangement is worth describing at all:

- **A workspace build's target directory is large and grows quickly.** Tens of
  gigabytes per checkout is normal once tests and multiple feature
  configurations are involved, so keeping several checkouts of this tree side by
  side is a disk-planning decision, not a free one.
- **Reusing a warm target directory dominates.** Building into a cold, empty
  target costs a full compile; the private tooling therefore seeds a new
  checkout's target from an existing one and prefers the warmest available
  target for the heaviest job. `cargo clean` is correspondingly expensive to
  reach for.

Everything else — the dataset layout and its ownership contract, the quota
values, the startup hook, the reclamation tiers and their thresholds — is
private infrastructure and out of scope for this repository.

See also [`build-system.md`](build-system.md) for the build system a contributor
builds with.
