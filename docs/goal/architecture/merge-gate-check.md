# The asynchronous check tier — public summary

> The full document describes the maintainers' private post-merge checking
> infrastructure — which machines run it, the trees and build caches they keep
> warm, and the internal protocol for recording a failure — and is not
> published. This abridged summary covers what a contributor needs; other
> documents in `docs/goal/` that reference `merge-gate-check.md` resolve here.

## Why there are two tiers

Checks are split by cost. Parse-only checks (formatting, generated-file
freshness, manifest and toolchain parity) are cheap enough to run on the way in,
so a change never lands without them. Compile-scale checks — workspace clippy,
compiling the test code, binding freshness, the per-platform builds — cost
minutes rather than seconds, and running them on the way in would make every
landing wait on them.

So the compile-scale set runs **just after** a change lands rather than before,
and anything it finds is **fixed forward**: the failure is recorded, the next
contributor to pick it up lands a fix on top, and the check clears itself on its
next green pass. Reverting someone else's landed work is not the remedy.

The tradeoff this accepts, deliberately: the mainline can be transiently red for
the minutes between a landing and the check that covers it. That is cheaper than
charging every landing the full compile-scale wait, and it is why the cheap tier
exists — a check that protects the tree from a *broken landing* belongs on the
way in, while a check that merely takes a long time belongs here.

## Running the same checks yourself

Every check is a `just` recipe — `just --list` shows them, and they are the same
recipes the private tier invokes, so a green run locally and a green run there
mean the same thing. Before opening a pull request the most useful are
`cargo fmt --all --check`, `cargo clippy --workspace`, and `just check-generated`
(generated files are never edited by hand — see
[`build-system.md`](build-system.md)).

The public buildability contract for this repository is
`.github/workflows/ci.yml`, which is self-contained and runs against the tree as
published. See also [`merge-gates.md`](merge-gates.md).
