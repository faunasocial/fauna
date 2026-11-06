# The fleet's disk — public summary

> The full document describes part of the maintainers' private development
> setup and is not published. Other documents in `docs/goal/` that reference
> `fleet-disk.md` resolve here.

## What a contributor needs to know

Nothing in this repository requires any of it. A plain `cargo build` of the
crate you change is the supported path, and it uses whatever disk your machine
has. How the maintainers' own machines account for the disk their development
sessions take, and reclaim it, is private infrastructure and out of scope for
this repository; the per-machine budget it generalizes is summarized in
[`mac-disk-budget.md`](mac-disk-budget.md).
