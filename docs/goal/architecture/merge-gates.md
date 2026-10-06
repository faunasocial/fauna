# Merge gates — public summary

> The full merge-gate design document describes the maintainers' private
> pre-merge and post-merge checking infrastructure and is not published. This
> abridged summary covers what a contributor needs; other documents in
> `docs/goal/` that reference `merge-gates.md` resolve here.

## What checks this tree

The public buildability contract is `.github/workflows/ci.yml`, which is
self-contained and runs against this repository as published.

Internally the same checks are split across two tiers — a small set of cheap,
parse-only checks that run synchronously when a change lands, and the
compile-scale checks (workspace clippy, test-code compile, binding freshness,
per-platform builds) that run asynchronously just afterwards and are fixed
forward when they go red. The tiering exists so that landing a change stays
fast while compile-scale coverage is not lost; the mechanics of the private
tier, including which machines run it, are out of scope here.

Every check has a `just` recipe you can run yourself — `just --list` shows
them. The ones a contributor is most likely to want before opening a pull
request are `cargo fmt --all --check`, `just check-generated` (generated files
are never edited by hand — see
[`build-system.md`](build-system.md)), and `cargo clippy --workspace`.
