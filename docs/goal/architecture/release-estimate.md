# The release estimate — public summary

> The full document records how the maintainers' private release tooling plans
> a release's cost before it starts and tracks the release against that plan,
> and is not published. This abridged summary covers what a contributor needs;
> other documents in `docs/goal/` that reference `release-estimate.md` resolve
> here.

## What a contributor needs to know

Nothing in this repository requires any of it. It concerns only how the
maintainers measure their own release work.

Two facts from it describe the shape of the work rather than the private setup:

- Before a release starts, a tool writes a plan: every operation the release is
  expected to take, how often each is expected to run, and how long each run
  takes, calculated from the records of earlier releases. The plan's total is
  calculated from those lines and is not changed once the release has started.
- Every action of the release is recorded against that plan, and an action that
  took longer than planned, ran more often than planned or was not planned at
  all is listed as a deviation, so the time left can be calculated at any
  moment and each release's review compares the plan with what happened.

Everything else — the record's fields, the formulas, the thresholds and the
maintainers' console — is private.
