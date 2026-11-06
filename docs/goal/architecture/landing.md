# Landing work on the main branch — public summary

> The full document describes the one tool the maintainers' development
> sessions use to land their work on the main branch, and is not published.
> This abridged summary covers what a contributor needs; other documents in
> `docs/goal/` that reference `landing.md` resolve here.

## What a contributor needs to know

Contributions to this repository arrive as ordinary pull requests, reviewed
and merged through GitHub. Nothing here asks you to run a landing tool.

Two observations from the private setup generalize:

- **A landing is one command with one verdict.** When several steps — commit,
  rebase, checks, push — are run by hand every time, each step is re-derived
  from documentation and each can fail in its own way. Putting the sequence
  behind one command with a single last line (`LANDED`, or `FAILED at <step>`
  with that step's evidence) makes the outcome readable at a glance and the
  steps impossible to run out of order.
- **A conflict the format can settle should be settled by a tool.** Where a
  file has a grammar (a keyed record, an append-only list), a three-way merge
  over that grammar resolves the routine conflicts a textual merge reports,
  and leaves only genuine contention for a person.

Everything else — the maintainers' own merge scripts, the private work-queue
files, the measurements behind the design and the per-machine details — is
private infrastructure and out of scope for this repository.

See also [`merge-gates.md`](merge-gates.md) for the checks every change passes
before it lands, and [`detached-run.md`](detached-run.md) for how a long
verification outlives the tool that started it.
