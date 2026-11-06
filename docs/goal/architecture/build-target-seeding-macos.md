# Warm-target seeding, macOS — public summary

> The full document describes how the maintainers' private macOS development
> machine warms a fresh checkout's build directory, and is not published.
> Other documents in `docs/goal/` that reference `build-target-seeding-macos.md`
> resolve here.

## What a contributor needs to know

Nothing in this repository requires it. A plain `cargo build` or
`just <recipe>` in a fresh checkout is the supported path, and a first build
there is simply a cold build.

The one observation that generalizes is already in
[`build-target-layout-macos.md`](build-target-layout-macos.md): on APFS, cloning
an existing target directory makes a fresh checkout's build state nearly free
to create, though not to diverge. How the private fleet chooses its donors and
measures that divergence is private infrastructure and out of scope for this
repository.
