# The fleet's subscriptions — public summary

> The full document describes part of the maintainers' private development
> setup and is not published. Other documents in `docs/goal/` that reference
> `fleet-subscriptions.md` resolve here.

## What a contributor needs to know

Nothing in this repository requires any of it. The maintainers run their
development sessions as a fleet across several machines on a few model
subscriptions, and let the fleet move a machine to another subscription when
one reaches its usage limit, or when a maintainer asks. How that choice is
made, how a session is authenticated and how a switch is recorded is private
infrastructure and out of scope for this repository; the console it sits in
is summarized in [`fleet-surfaces.md`](fleet-surfaces.md).
