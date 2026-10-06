# Convention gates — public summary

> The full document catalogues the maintainers' private pre-merge convention
> checks, including the grammar of their internal work-queue files, and is not
> published. This abridged summary covers what a contributor needs; other
> documents in `docs/goal/` that reference `convention-gates.md` resolve here.

## What a convention gate is

Most checks in this repository assert something about a build product: the tree
compiles, a generated file is fresh, two producers of one artifact agree. A
smaller family asserts something about a *convention* instead — that a rule the
codebase decided to follow is still followed everywhere, and that it has exactly
one home.

They exist because a convention with no check decays silently. Nothing breaks
the day someone hand-rolls a date format, opens a second door to the wall clock,
or lets a generated identifier drift out of the file that owns it; the cost
arrives months later, in a bug that reads as unreproducible. So each of these
checks was added the day a convention was found already broken, and each is
scoped to the files that convention lives in.

## The ones you can run

They are all `just` recipes, all parse-only — no compiler, no network, seconds
at most — so running them locally means the same thing as running them anywhere
else. `just --list` shows the full set. The ones a contributor is most likely to
meet are `cargo fmt --all --check` (formatting), and the lints over
platform-specific UI markup, which report the file and line and the rule.

A check that fails prints what it found, where, and what to do about it. If the
remedy is not obvious from the message, that is a bug in the message.

For how checks are split between the fast set that runs as a change lands and
the slower set that runs afterwards, see [`merge-gates.md`](merge-gates.md) and
[`merge-gate-check.md`](merge-gate-check.md). The public buildability contract
for this repository is `.github/workflows/ci.yml`, which is self-contained and
runs against the tree as published.
