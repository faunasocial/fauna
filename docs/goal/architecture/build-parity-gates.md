# Build, parity and compile gates — public summary

> The full document catalogues the maintainers' private build and packaging
> checks, including per-machine costs and the private tiering that decides when
> each one runs, and is not published. This abridged summary covers what a
> contributor needs; other documents in `docs/goal/` that reference
> `build-parity-gates.md` resolve here.

## The problem these checks exist for

Several artifacts in this repository have **two independent producers**. The
container image builds the workspace one way and the local `just` recipes build
it another; the web bundle is named in a build script and again in a Dockerfile;
a Go toolchain version is pinned in one file and asserted in another. Nothing in
a compiler notices when two such producers drift apart — each side is
internally consistent, and the break shows up only when the artifact is
assembled somewhere neither author was looking.

So each of these checks names both sides and asserts they still agree, at parse
time, on the way in. They are cheap by construction: they read files and compare
declarations rather than building anything.

## The compile-class checks

A second family does compile: the workspace under each non-default feature
combination, the **test** code as well as the shipped code, and clippy across
every target. These cost minutes rather than seconds, so they run just after a
change lands rather than before it — the tiering rationale is in
[`merge-gate-check.md`](merge-gate-check.md).

Two properties are worth knowing because they are easy to assume and false. A
default `cargo clippy --workspace` does not lint code behind a non-default
feature flag, and `cargo build` does not type-check test code at all. Both gaps
have cost real breakage here, which is why each has its own check.

## Running them yourself

Every check is a `just` recipe — `just --list` shows them. Before opening a pull
request the most useful are `cargo fmt --all --check`, `cargo clippy
--workspace --all-targets`, and `just check-generated` (generated files are
never edited by hand — see [`build-system.md`](build-system.md)). The public
buildability contract for this repository is `.github/workflows/ci.yml`, which
is self-contained and runs against the tree as published.
