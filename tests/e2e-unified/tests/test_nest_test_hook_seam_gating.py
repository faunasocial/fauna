"""Convention 15, the nest's own family: every `*_test_hook(s)` module in
`bins/fauna-nest/src/` mounts unauthenticated `/api/v1/test/*` routes and
must be compiled out of release artifacts.

`docs/goal/architecture/e2e-automation-surface-gating.md` § The convention —
convention 15, the shared-Rust clause, which names the nest's own
`test-hooks` feature as the precedent every Rust-app `e2e-agent` gate
mirrors. The nest already has an artifact-level witness for this family,
`just nest-automation-surface-check` (wired into the merge-gate check's
heavy tier): it builds `fauna-nest` twice and `strings -a`-scans the binary
for the `/api/v1/test/` route prefix, proving the DEFAULT flavor carries none
and the `--features test-hooks` flavor carries some. That witness has two
blind spots this file closes. First, it scans only the `fauna-nest` binary
itself — a shipping dep line in a *consumer* binary (`fauna-nest-daemon`,
`apps/fauna-windows/fauna-nest-service`) that named `fauna-nest/test-hooks`
directly would turn the seams on in THAT artifact without ever showing up in
a `fauna-nest`-only scan. Second, an artifact scan can never name which of
the 21 modules is the offender when it fails, or notice a 22nd module
authored with no gate at all before its first build. This file adds the
source-level, per-module complement: which modules are gated, and which
manifests are forbidden from turning the feature on.

**Two independent halves, because either alone false-greens.**

  1. *Every hook module is gated.* Derived from the file list, not a
     maintained roster, so a 22nd `*_test_hook.rs` added tomorrow is covered
     with no edit here.
  2. *No shipping manifest names `fauna-nest/test-hooks` on a
     `[dependencies]` line.* `fauna-nest-daemon` and
     `apps/fauna-windows/fauna-nest-service` both need the feature and both
     forward it correctly — from their OWN opt-in `[features]` table entry,
     never a dep line — and this half is what keeps that the only path in.

**The trap: two independently-sufficient idioms.** 18 of the 21 modules gate
the `pub mod` line in `lib.rs` with an outer `#[cfg(feature = "test-hooks")]`;
the other 3 (`rpc_hold_test_hook`, `custody_hosting_test_hook`,
`segment_backup_test_hook`) carry no outer attribute at all — they gate
themselves with an inner `#![cfg(feature = "test-hooks")]` at the top of the
file instead. A witness reading only one idiom reports the modules using the
other as false-positive offenders. This file checks for either.

Pure text analysis of `lib.rs`, the 21 hook source files, and every workspace
manifest — no build, no driver.

**Dep-line half's `test-helpers` twin.**
`test_support.rs` — the module-level test-support surface widened into
`test_shared_crate_seam_gating.py` (this file's sibling, module docstring
"Module-level complement") — is gated on `test-helpers`, not `test-hooks`;
that half's own dep-line pin (rule (b)) covered `test-hooks` only, so a
shipping manifest naming `fauna-nest/test-helpers` directly on a
`[dependencies]` line was unpinned. `test_no_shipping_dep_line_turns_the_nest_test_helpers_feature_on`
closes it, delegating to the same shared scan
(`helpers.manifest_seams.shipping_feature_offenders`) as the `test-hooks`
check above.
"""

import re
from pathlib import Path

import pytest

from helpers.manifest_seams import shipping_feature_offenders

pytestmark = pytest.mark.tier_1

_REPO = Path(__file__).resolve().parents[3]
_SRC_DIR = _REPO / "bins" / "fauna-nest" / "src"
_LIB_RS = _SRC_DIR / "lib.rs"

_OUTER_GATE = '#[cfg(feature = "test-hooks")]'
_INNER_GATE = '#![cfg(feature = "test-hooks")]'


def _test_hook_modules() -> list[str]:
    """Every `*_test_hook(s)` source file's module name (its stem), derived
    from the file list — 20 end `_test_hook`, one (`push_test_hooks.rs`) ends
    `_test_hooks` — so a new module added tomorrow needs no edit here."""
    return sorted(p.stem for p in _SRC_DIR.glob("*test_hook*.rs"))


def _has_outer_gate(lib_rs_text: str, module: str) -> bool:
    """True iff `lib_rs_text` declares `pub mod {module};` with
    `_OUTER_GATE` as the line immediately above it."""
    return (
        re.search(
            re.escape(_OUTER_GATE) + r"\s*\npub mod " + re.escape(module) + r"\s*;",
            lib_rs_text,
        )
        is not None
    )


def _has_inner_gate(module: str) -> bool:
    """True iff the module's own source file carries `_INNER_GATE`."""
    text = (_SRC_DIR / f"{module}.rs").read_text(encoding="utf-8")
    return _INNER_GATE in text


def _ungated_test_hook_modules() -> list[str]:
    lib_rs_text = _LIB_RS.read_text(encoding="utf-8")
    offenders = []
    for module in _test_hook_modules():
        if _has_outer_gate(lib_rs_text, module) or _has_inner_gate(module):
            continue
        offenders.append(module)
    return offenders


def test_every_test_hook_module_is_gated_and_at_least_one_exists():
    """Every `*_test_hook(s)` module in `bins/fauna-nest/src/` carries the
    `test-hooks` feature gate, on the `pub mod` line in `lib.rs` or inline in
    the module itself — and the derivation actually finds modules, so this
    cannot pass by scanning nothing (the absence assertion below is
    vacuously true over an empty set)."""
    modules = _test_hook_modules()
    assert modules, (
        "bins/fauna-nest/src/*test_hook*.rs matched no files — the naming "
        "convention this scanner derives from is gone; update the glob or "
        "delete this test with the family it was written to cover"
    )
    offenders = _ungated_test_hook_modules()
    assert not offenders, (
        f"module(s) {offenders} in bins/fauna-nest/src/ match the "
        "`*_test_hook*` naming convention but carry neither an outer "
        f'{_OUTER_GATE!r} on their `pub mod` line in lib.rs nor an inner '
        f"{_INNER_GATE!r} in the module itself, so they compile into every "
        "release nest artifact regardless of the `test-hooks` feature "
        "(e2e-automation-surface-gating.md § The convention, convention 15)"
    )


def test_the_scan_actually_finds_both_gating_idioms():
    """Self-check against vacuity in the OTHER direction: both idioms are
    live in this codebase today, so a regression that stops recognizing
    either one (e.g. `_has_inner_gate` reading the wrong file, or the outer
    regex losing its adjacency requirement) must fail loudly here rather
    than merely widening the offenders list silently."""
    lib_rs_text = _LIB_RS.read_text(encoding="utf-8")
    modules = _test_hook_modules()
    outer = [m for m in modules if _has_outer_gate(lib_rs_text, m)]
    inner = [m for m in modules if _has_inner_gate(m)]
    assert outer, "no test_hook module resolved via the outer #[cfg] idiom — the lib.rs regex likely broke"
    assert inner, "no test_hook module resolved via the inner #![cfg] idiom — the per-file scan likely broke"


def test_no_shipping_dep_line_turns_the_nest_test_hooks_feature_on():
    """No crate in the workspace enables `fauna-nest`'s `test-hooks` feature
    on a `[dependencies]` line, where cargo turns it on unconditionally for
    every artifact that crate builds.

    A consumer that needs the hooks forwards them from its OWN opt-in
    feature instead (`bins/fauna-nest-daemon/Cargo.toml` and
    `apps/fauna-windows/fauna-nest-service/Cargo.toml`'s own `test-hooks =
    ["fauna-nest/test-hooks"]` entries are the reference shape), so the
    build recipe decides, not the manifest. `just nest-automation-surface-check`
    proves this holds for the `fauna-nest` binary itself; this half is what
    would catch a THIRD consumer making the opposite, dep-line mistake.

    Delegates the scan itself to `helpers.manifest_seams.shipping_feature_offenders`
    (multi-line-aware; see that module's docstring), narrowed to `fauna-nest`.
    """
    offenders = shipping_feature_offenders("test-hooks", _REPO, crates={"fauna-nest"})
    assert not offenders, (
        "a shipping dependency line enables fauna-nest's test-hooks feature "
        "directly, so the 21 *_test_hook modules' unauthenticated "
        "/api/v1/test/* routes are compiled into that artifact regardless "
        "of its own build recipe (e2e-automation-surface-gating.md § The "
        "convention, convention 15 rule (b)). Forward test-hooks from the "
        "consuming crate's own opt-in feature instead. Offending line(s): "
        + repr(offenders)
    )


def test_no_shipping_dep_line_turns_the_nest_test_helpers_feature_on():
    """The `test-helpers` twin of the check above: no
    crate in the workspace enables `fauna-nest`'s `test-helpers` feature — the
    gate that lends `test_support.rs`'s in-process real-nest builder to other
    crates — on a `[dependencies]` line either.

    `fauna-sync-agent` is the only outside consumer, and it already forwards
    correctly from its own opt-in `tier3-nest` feature
    (`bins/fauna-sync-agent/Cargo.toml`: `tier3-nest = ["dep:fauna-nest",
    "fauna-nest/test-helpers", "dep:axum"]` — a `[features]` table entry, not
    a dependency line, so `shipping_feature_offenders` already excludes it).

    Delegates the scan itself to `helpers.manifest_seams.shipping_feature_offenders`
    (multi-line-aware; see that module's docstring), narrowed to `fauna-nest`;
    the general, all-crate form of this same scan lives in
    `test_test_helpers_dep_line_gating.py`.
    """
    offenders = shipping_feature_offenders("test-helpers", _REPO, crates={"fauna-nest"})
    assert not offenders, (
        "a shipping dependency line enables fauna-nest's test-helpers "
        "feature directly, so test_support.rs's in-process real-nest "
        "builder is compiled into that artifact regardless of its own "
        "build recipe (e2e-automation-surface-gating.md § The convention, "
        "convention 15 rule (b)). Forward test-helpers from the consuming "
        "crate's own opt-in feature instead. Offending line(s): "
        + repr(offenders)
    )
