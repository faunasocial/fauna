# Fleet scheduler — public summary

> The full document describes part of the maintainers' private development
> setup and is not published. Other documents in `docs/goal/` that reference
> `fleet-scheduler.md` resolve here.

## What a contributor needs to know

Nothing in this repository requires any of it. A plain `cargo build` of the
crate you change is the supported path, and nothing in the build depends on
any service of the maintainers' being present. How the maintainers' own
machines are handed their next piece of work from one place — which queue
entry goes to which machine, in what order, and who records it — is private
infrastructure and out of scope for this repository; the control center it
belongs to is summarized in [`fleet-control.md`](fleet-control.md), and the
shared CPU budget it schedules under in
[`fleet-core-broker.md`](fleet-core-broker.md).
