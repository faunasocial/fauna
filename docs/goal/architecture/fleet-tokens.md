# The fleet's token ledger — public summary

> The full document describes part of the maintainers' private development
> setup and is not published. Other documents in `docs/goal/` that reference
> `fleet-tokens.md` resolve here.

## What a contributor needs to know

Nothing in this repository requires any of it. The maintainers run their
development sessions as a fleet across several machines on a few model
subscriptions, and keep a private ledger of which subscription each machine
runs on and what each session spent in tokens, read off the sessions' own
local logs and shown on one page of their console. How that ledger is read,
kept and shown is private infrastructure and out of scope for this
repository; the console it sits in is summarized in
[`fleet-surfaces.md`](fleet-surfaces.md).
