# Fleet launch and halt — public summary

> The full document describes part of the maintainers' private development
> setup and is not published. Other documents in `docs/goal/` that reference
> `fleet-launch-and-halt.md` resolve here.

## What a contributor needs to know

Nothing in this repository requires any of it. A plain `cargo build` of the
crate you change is the supported path, and nothing in the build depends on
any service of the maintainers' being present. How the maintainers' own
machines start a working session, read how it ended and keep their working
copies is private infrastructure and out of scope for this repository; how
those machines are handed their next piece of work is summarized in
[`fleet-scheduler.md`](fleet-scheduler.md), and the control center both
belong to in [`fleet-control.md`](fleet-control.md).
