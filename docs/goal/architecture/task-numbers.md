# Task numbers — public summary

> The full document describes part of the maintainers' private development
> setup and is not published. Other documents in `docs/goal/` that reference
> `task-numbers.md` resolve here.

## What a contributor needs to know

Nothing in this repository requires any of it. A plain `cargo build` of the
crate you change is the supported path, and nothing in the build depends on
any service of the maintainers' being present. How the entries of the
maintainers' private work queues are numbered — one counter for every queue on
every machine, each number issued once and never reused — is private
infrastructure and out of scope for this repository; the control center it
belongs to is summarized in [`fleet-control.md`](fleet-control.md).
