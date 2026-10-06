# The build/e2e slot pool — public summary

> The full document describes how the maintainers' private development machines
> bound the number of heavy tasks running at once, and is not published. This
> abridged summary covers what a contributor needs; other documents in
> `docs/goal/` that reference `build-slot-pool.md` resolve here.

## What a contributor needs to know

Nothing in this repository's build requires the slot pool. A plain
`cargo build`, `just <recipe>`, or a single end-to-end run in a fresh checkout
is the supported path, and it does not consult any slot. The justfile's `slot_*`
variables are empty when the maintainers' slot script is absent, which it is
here, so every recipe line runs directly.

Two facts are worth knowing anyway, because they describe the shape of the work
rather than the private setup:

- **A full workspace build and an end-to-end suite are both resource-hungry.**
  Each is a multi-core, multi-gigabyte job. Running several at once on one
  machine oversubscribes CPU, memory and disk together. If you run the suites
  in parallel, expect to need some discipline of your own.
- **Free disk is a precondition, not a detail.** A build that runs out of space
  midway can leave truncated artifacts behind rather than failing cleanly.
  Checking free space before a long build is a reasonable habit anywhere.

Everything else — how the maintainers queue, size and bound their own builds —
is private infrastructure and out of scope for this repository.

See also [`build-machine-resources.md`](build-machine-resources.md) for the
build-cost facts a contributor does see, and [`build-system.md`](build-system.md)
for what the builds themselves do.
