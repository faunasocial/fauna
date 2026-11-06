# The pass schedule — public summary

> The full document records when the maintainers' private asynchronous checking
> tier runs wide and when it runs lean on their development machines, and is not
> published. This abridged summary covers what a contributor needs; other
> documents in `docs/goal/` that reference `gate-check-schedule.md` resolve here.

## What a contributor needs to know

Nothing in this repository requires any of it. The maintainers run a heavy
post-merge check on their own machines; this document is the schedule that
check runs on.

Two facts from it describe the shape of the work rather than the private setup:

- The check has four states — a lean day pass, one heavy night pass that may
  take most of the shared CPU while nobody is working, a hurry raised by day when
  enough rows wait on the check's answer, and a free host used whole — and the
  state is one word published by the CPU broker, never decided by the check
  itself.
- The heavy gates are split by a measured yield test: a gate that runs for
  minutes and catches a red rarely runs only in the night pass, while every gate
  that catches reds often, or costs under five minutes, runs on every pass. The
  split is re-read from the check's own log, never set by hand.

Everything else — the machines, the window, the widths, the thresholds, the
per-gate table and the readings that decided them — is private.
