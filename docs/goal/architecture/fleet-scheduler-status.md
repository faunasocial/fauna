# Fleet scheduler status — public summary

> The full document describes part of the maintainers' private development
> setup and is not published. Other documents in `docs/goal/` that reference
> `fleet-scheduler-status.md` resolve here.

## What a contributor needs to know

Nothing in this repository requires any of it. A plain `cargo build` of the
crate you change is the supported path, and nothing in the build depends on
any service of the maintainers' being present. The record of how far the
maintainers have built the scheduler that hands their own machines their next
piece of work is private infrastructure and out of scope for this repository;
the scheduler itself is summarized in [`fleet-scheduler.md`](fleet-scheduler.md),
and the control center it belongs to in [`fleet-control.md`](fleet-control.md).
