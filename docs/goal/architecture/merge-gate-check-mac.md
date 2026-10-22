# The Apple-platform checks — public summary

> The full document describes the maintainers' private post-merge checks for the
> Apple platforms — the machine that runs them, the build caches it keeps warm,
> and what a run costs there — and is not published. This abridged summary covers
> what a contributor needs; other documents in `docs/goal/` that reference
> `merge-gate-check-mac.md` resolve here.

## What runs for the Apple platforms

Only a Mac can build the Apple apps, so the checks here cover what no Linux build
can see: building the macOS and iOS app from its Swift sources together with the
Rust library it links, in debug and release, type-checking the Swift files that
only compile for iOS, and running the Rust tests whose bodies only compile on
macOS. Each is a `just` recipe, so a contributor on a Mac can run the same check
locally and get the same answer. What the checks deliberately leave uncovered —
for example, tests that would need a phone or a simulator — is written down in
the full document rather than assumed away.

The two-tier design these checks belong to is in
[`merge-gate-check.md`](merge-gate-check.md), and the rule for which tier a check
belongs in is in [`merge-gates.md`](merge-gates.md).
