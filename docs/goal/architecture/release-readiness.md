# Release readiness — public summary

> The full document records how the maintainers' private development machines
> keep a proven main-branch commit ready for release, and is not published. This
> abridged summary covers what a contributor needs; other documents in
> `docs/goal/` that reference `release-readiness.md` resolve here.

## What a contributor needs to know

Nothing in this repository requires any of it. A release of Fauna is cut from a
main-branch commit that the maintainers' own machines have already proven: this
document is the mechanism that keeps such a commit named at all times.

Two facts from it describe the shape of the work rather than the private setup:

- A lane replays this repository's own CI against the curated tree of the newest
  main-branch commit, continuously and apart from every other check, and the
  newest commit it proved is the one a release tips to. A commit it has not
  proved is never released by default.
- Every release build is rehearsed on the machine that owns it, at that proven
  commit, once a night, re-running only the builds a change reached; a release
  picks the newest proven commit every rehearsal covers.

Everything else — the machines, the tree and dataset, the lock, the widths, the
record, the routing of a red and the readings that decided them — is private.
