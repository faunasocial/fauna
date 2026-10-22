# The Windows-target checks — public summary

> The full document describes the maintainers' private post-merge checks for the
> Windows target — the machine that runs them, the build caches it keeps warm,
> and what a run costs there — and is not published. This abridged summary covers
> what a contributor needs; other documents in `docs/goal/` that reference
> `merge-gate-check-win.md` resolve here.

## What runs for the Windows target

Most compile-scale checks are platform-independent and run once, on Linux. The
Windows target gets checks of its own only where a Linux build cannot see a
break: compiling the Rust crates for the Windows target, building the Windows
app itself in both debug and release, compiling its test projects, and the few
test suites whose failures only ever showed up on Windows (path separators, line
endings, process spawning). Each is a `just` recipe, so a contributor on Windows
can run the same check locally and get the same answer.

The two-tier design these checks belong to is in
[`merge-gate-check.md`](merge-gate-check.md), and the rule for which tier a check
belongs in is in [`merge-gates.md`](merge-gates.md).
