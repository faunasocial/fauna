# The task plan — public summary

> The full document records how the maintainers' private development tooling
> plans each item of their work queue before work on it starts and tracks the
> item against that plan, and is not published. This abridged summary covers
> what a contributor needs; other documents in `docs/goal/` that reference
> `task-plan.md` resolve here.

## What a contributor needs to know

Nothing in this repository requires any of it. It concerns only how the
maintainers measure their own development work.

Two facts from it describe the shape of the work rather than the private setup:

- Before work on an item starts, a tool writes a plan for it: the operations
  the item is expected to take until it is closed, how often each is expected
  to run and how long each run takes, calculated from the records of earlier
  items. Small items get only that calculated plan; large items get their
  counts reviewed; a few kinds of item, a release among them, have a planner
  of their own.
- Every recorded action of an item is held against its plan, and an action
  that took longer than planned, ran more often than planned or was not
  planned at all is listed as a deviation with its cause, so the time left can
  be calculated at any moment and the time lost can be read per cause.

Everything else — the record's fields, the formulas, the thresholds and the
maintainers' console — is private.
