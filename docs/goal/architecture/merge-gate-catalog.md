# The heavy check catalog — public summary

> The full document is the maintainers' per-check record for their private
> post-merge checking infrastructure — what each check covers, the wall-clock
> cost measured on their own hardware, and the honest list of what is not
> covered — and is not published. This abridged summary covers what a
> contributor needs; other documents in `docs/goal/` that reference
> `merge-gate-catalog.md` resolve here.

## What the compile-scale checks cover

The checks that run after a change lands are the compile-scale ones: workspace
clippy, compiling the workspace's test code as well as its libraries, the
per-platform builds, binding freshness, and — for a few areas where compiling is
not enough — actually running a suite. Each is a `just` recipe, so `just --list`
names them and a green run locally means the same thing as a green run there.
The split between "runs on the way in" and "runs just after" is decided by cost,
and the rule is in [`merge-gates.md`](merge-gates.md).

Two properties are worth knowing because they shape how the recipes are written:

- **Every recipe passes `--locked`**, so running a check can never rewrite
  `Cargo.lock` underneath the tree it is checking.
- **Scope, not everything, every time.** Most checks declare which paths can
  affect them and are skipped when a change touches none of those paths; a few
  cost too much for that and run on a fixed cadence instead. Either way, a check
  that is skipped says so rather than reporting green.

## Coverage is deliberately incomplete, and that is written down

The full document keeps a numbered list of what no check covers, each item with
either the reasoning for leaving it uncovered or a record of the check that
later closed it. The general shape: a gap is stated rather than assumed away, and
closing one is a deliberate act with its own record — including the measurement
that showed the new check was worth its cost.

The public buildability contract for this repository is
`.github/workflows/ci.yml`, which is self-contained and runs against the tree as
published. See also [`merge-gates.md`](merge-gates.md) and
[`merge-gate-check.md`](merge-gate-check.md).
