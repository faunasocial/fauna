# Fleet activity view — public summary

> The full document describes part of the maintainers' private development
> setup and is not published. Other documents in `docs/goal/` that reference
> `fleet-activity.md` resolve here.

## What a contributor needs to know

Nothing in this repository requires any of it. A plain `cargo build` of the
crate you change is the supported path, and nothing in the build depends on
any service of the maintainers' being present. How the maintainers read
whether their own machines' cores, memory and disk are being used for the
work they want done is private infrastructure and out of scope for this
repository; the control center it is one view of is summarized in
[`fleet-control.md`](fleet-control.md), and the shared CPU budget it measures
against in [`fleet-core-broker.md`](fleet-core-broker.md).
