"""The warm-takes-no-slot fix for apple's host-only FFI + mac-debug builds —
the macOS/iOS leg of the cross-app "warm tree must not queue for a build slot"
track (build-system.md § Build/e2e slot locks; `test_e2e_binary_prebuild.py`
pins the same discipline for the nest/mail-bridge legs, which this file
mirrors for the macOS-only legs those tests cannot reach — no Xcode/Swift
toolchain on the other dev machines).

Before this: `_apple-ffi-host-flavor` (the shared implementation behind
`apple-ffi-host` and `apple-ffi-host-test`, which `mac-debug`/`swift-test`
take) ran its `cargo build -p fauna-ffi` unconditionally on every invocation,
and `mac-debug`'s own `swift build` line had no gate at all — every single
`mac-debug`/`swift-test` run queued for a machine-wide `build` slot even on a
fully warm tree. Text/structural analysis only, no real Xcode build needed to
prove the wiring exists (real-build verification: `just mac-debug` twice in a
row on a macOS box, confirmed both `build-if-stale` lines read `up-to-date` and
`build-slot.py --status` showed no slot touched).
"""

import re
from pathlib import Path

import pytest

pytestmark = pytest.mark.tier_1

_REPO = Path(__file__).resolve().parents[3]
_JUSTFILE = _REPO / "justfile"


def _recipe_body(text: str, name: str) -> str:
    """The indented body of one justfile recipe — `name:` or a parametrized
    `name *ARGS:` declaration alike. Duplicated from
    `test_e2e_binary_prebuild.py` per this suite's no-cross-import-between-
    test-files convention (small helpers get duplicated, not imported)."""
    lines = text.splitlines()
    header = re.compile(rf"^{re.escape(name)}(\s+\S+)*:")
    for i, ln in enumerate(lines):
        if header.match(ln):
            body = []
            for bl in lines[i + 1:]:
                if bl and not bl[:1].isspace():
                    break
                body.append(bl)
            return "\n".join(body)
    return ""


def test_apple_ffi_host_flavor_cargo_step_is_freshness_gated_outside_the_slot():
    """`_apple-ffi-host-flavor` (shared by `apple-ffi-host` and
    `apple-ffi-host-test`) must decide "is there work?" BEFORE queueing:
    build-if-stale outermost, {{slot_build}} inside it, cargo innermost —
    same shape as `mail-bridge-ffi`'s gate. --stamp, not the .a, must carry
    freshness: a cargo no-op leaves the .a untouched, so an artifact-keyed
    gate would go permanently stale on rebase mtime churn and queue on every
    warm run anyway."""
    body = _recipe_body(_JUSTFILE.read_text(encoding="utf-8"), "_apple-ffi-host-flavor")
    assert body, "_apple-ffi-host-flavor recipe not found"
    joined = " ".join(body.split())
    cargo_needle = "cargo build --locked -p fauna-ffi"
    assert cargo_needle in joined, "_apple-ffi-host-flavor must build fauna-ffi"
    assert "build-if-stale.py" in joined and "--stamp" in joined, (
        "the cargo step must be gated by build-if-stale --stamp (a warm tree "
        "must take no build slot)"
    )
    gate_pos = joined.index("build-if-stale.py")
    slot_pos = joined.index("{{slot_build}}", gate_pos)
    cargo_pos = joined.index(cargo_needle, gate_pos)
    assert gate_pos < slot_pos < cargo_pos, (
        "order must be build-if-stale (freshness) OUTSIDE {{slot_build}} "
        "OUTSIDE cargo — a slot acquired before the freshness verdict queues "
        "a warm tree"
    )


def _expand_recipe_vars(body: str, expr: str) -> str:
    """`expr` with `$VAR`/`${VAR}` replaced by the recipe's own assignments,
    repeatedly (assignments reference each other). Used so a path assertion
    reads the SHAPE of the path rather than the spelling of one line: since
    2026-08-22 the stamp is feature-scoped through `$ARTIFACTS` rather than by a
    suffix in its own filename, and a literal substring check called that a
    regression when it is strictly stronger scoping."""
    assigns = dict(re.findall(r'^\s*(\w+)="([^"]*)"', body, re.M))
    for _ in range(5):
        before = expr
        expr = re.sub(r'\$\{?(\w+)\}?', lambda m: assigns.get(m.group(1), m.group(0)), expr)
        if expr == before:
            break
    return expr


def test_apple_ffi_host_flavor_cargo_stamp_is_scoped_by_features_not_just_config():
    """A cargo freshness stamp keyed only on libs/Cargo.lock/rust-toolchain.toml
    would read "fresh" the moment a caller switches BACK to a flavor last built a
    while ago, even though the artifact on disk right now holds the OTHER
    flavor's build. The stamp path must therefore carry the features axis, not
    just $PROFILE_DIR — however it gets there: a suffix in the filename, or (as
    now) the per-flavor `--artifact-dir` the stamp lives in."""
    body = _recipe_body(_JUSTFILE.read_text(encoding="utf-8"), "_apple-ffi-host-flavor")
    assert body, "_apple-ffi-host-flavor recipe not found"
    stamp_lines = [ln for ln in body.splitlines() if "CARGO_STAMP=" in ln]
    assert stamp_lines, "_apple-ffi-host-flavor must define a CARGO_STAMP path"
    # Expansion bottoms out at the recipe's own `features` parameter, whichever
    # route the path took to it (`$FEATURES` directly, or via `$ARTIFACTS`).
    resolved = _expand_recipe_vars(body, stamp_lines[0])
    assert "{{features}}" in resolved, (
        f"CARGO_STAMP must be scoped by the features axis (not just "
        f"$PROFILE_DIR/$CONFIG), or a flavor switch silently skips a needed "
        f"rebuild: {stamp_lines[0]!r} resolves to {resolved!r}"
    )


def test_apple_ffi_host_flavor_guard_also_invalidates_the_cargo_stamp():
    """The pre-existing flavor guard already wipes the xcframework on a
    flavor mismatch (`.ffi-flavor` != this invocation's `$FLAVOR:$FEATURES`).
    It must ALSO remove `$CARGO_STAMP` in the same branch — otherwise a stale
    same-config, different-features stamp from an EARLIER run of THIS flavor
    survives the mismatch and the gate below reads "fresh" against unchanged
    libs/ sources, silently skipping the cargo rebuild the flavor switch
    actually requires (the .a would keep the OTHER flavor's code — exactly
    the production/test FFI seam separation convention 15 exists to protect)."""
    body = _recipe_body(_JUSTFILE.read_text(encoding="utf-8"), "_apple-ffi-host-flavor")
    assert body, "_apple-ffi-host-flavor recipe not found"
    guard_re = re.search(
        r"\.ffi-flavor.*?\}", body, re.DOTALL
    )
    assert guard_re, "flavor-mismatch guard block not found"
    guard_block = guard_re.group(0)
    assert "FaunaFFI.xcframework" in guard_block, (
        "sanity: this must be the flavor-mismatch guard block"
    )
    assert "CARGO_STAMP" in guard_block, (
        "the flavor guard's mismatch branch must also remove $CARGO_STAMP — "
        "leaving a stale same-flavor stamp from an earlier run means a "
        "flavor switch can silently skip a needed cargo rebuild"
    )


def test_mac_debug_swift_build_is_freshness_gated_outside_the_slot():
    """`mac-debug`'s `swift build --product FaunaMacOS` line had no gate at
    all — same defect class as android-debug's gradlew gap. Must be gated
    build-if-stale (outermost) -> {{slot_build}} -> swift build (innermost)."""
    body = _recipe_body(_JUSTFILE.read_text(encoding="utf-8"), "mac-debug")
    assert body, "mac-debug recipe not found"
    joined = " ".join(body.split())
    swift_needle = "swift build --package-path apps/fauna-apple --product FaunaMacOS"
    assert swift_needle in joined, "mac-debug must build the FaunaMacOS product"
    assert "build-if-stale.py" in joined and "--stamp" in joined, (
        "mac-debug's swift build step must be gated by build-if-stale --stamp "
        "(a warm tree must take no build slot)"
    )
    gate_pos = joined.index("build-if-stale.py")
    slot_pos = joined.index("{{slot_build}}", gate_pos)
    swift_pos = joined.index(swift_needle, gate_pos)
    assert gate_pos < slot_pos < swift_pos, (
        "order must be build-if-stale (freshness) OUTSIDE {{slot_build}} "
        "OUTSIDE swift build — a slot acquired before the freshness verdict "
        "queues a warm tree"
    )


def test_mac_debug_source_watch_excludes_its_own_build_output_and_flavor_marker():
    """Two self-referential-staleness traps a naive `--source apps/fauna-apple`
    would fall into, both caught live while building this gate:
    (1) swiftpm's own `.build/` output dir — watching your own build output
    means the gate looks stale right after building.
    (2) `.ffi-flavor` — the `apple-ffi-host-test` prerequisite rewrites this
    file's mtime on EVERY invocation regardless of whether it did any real
    work (it is the one apps/fauna-apple file with no byte-comparing write,
    unlike the generated i18n/providers files beside it), so without this
    exclude a truly warm re-run still reads "stale" every time."""
    body = _recipe_body(_JUSTFILE.read_text(encoding="utf-8"), "mac-debug")
    assert body, "mac-debug recipe not found"
    joined = " ".join(body.split())
    assert "--source apps/fauna-apple" in joined
    assert "--exclude '*/.build/*'" in joined, (
        "must exclude swiftpm's own .build/ output — a source watching its "
        "own build output always looks stale right after building"
    )
    assert "--exclude '*/.ffi-flavor'" in joined, (
        "must exclude .ffi-flavor — it is rewritten unconditionally by the "
        "apple-ffi-host-test prerequisite on every invocation, so without "
        "this exclude the gate never reads up-to-date"
    )
