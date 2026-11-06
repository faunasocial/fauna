# The fleet janitor — public summary

> The full document describes part of the maintainers' private development
> setup and is not published. Other documents in `docs/goal/` that reference
> `fleet-janitor.md` resolve here.

## What a contributor needs to know

Nothing in this repository requires any of it. The maintainers run their
development sessions as a fleet across several machines, and a small service
keeps that fleet's work queue honest: a build break on the integration branch
is handed to a session to fix, a session that has stopped with nothing to wait
for is nudged or released, and the prerequisites written on queued work are
re-examined so they do not hold work longer than they should. How that service
reads the fleet and what it may do is private infrastructure and out of scope
for this repository; the scheduling it sits beside is summarized in
[`fleet-scheduler.md`](fleet-scheduler.md).
