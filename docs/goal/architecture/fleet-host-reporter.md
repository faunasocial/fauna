# Fleet host reporter — public summary

> The full document describes part of the maintainers' private development
> setup and is not published. Other documents in `docs/goal/` that reference
> `fleet-host-reporter.md` resolve here.

## What a contributor needs to know

Nothing in this repository requires any of it. A plain `cargo build` of the
crate you change is the supported path, and nothing in the build depends on
any service of the maintainers' being present. How the machine hosting their
development VMs reports its own view of them is private infrastructure and
out of scope for this repository; the shared CPU budget it feeds is
summarized in [`fleet-core-broker.md`](fleet-core-broker.md).
