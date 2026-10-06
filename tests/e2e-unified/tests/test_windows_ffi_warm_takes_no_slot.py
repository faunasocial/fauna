"""The warm-takes-no-slot fix for windows' UniFFI cdylib build — the Windows leg of
the cross-app "warm tree must not queue for a build slot" track. `test_apple_ffi_host_warm_takes_no_slot.py` is the macOS twin this
file mirrors, and `test_e2e_binary_prebuild.py` pins the same discipline for the
nest/mail-bridge legs; the windows leg is unreachable from those boxes because
the recipe shells out to `scripts\\cargo-win.cmd`, which only runs on Windows.

Before this: `windows-debug`'s own MSBuild step had been freshness-gated outside
the slot since 2026-07-30 — but `windows-debug` takes `(windows-ffi-test "dev")`
as a PREREQUISITE, and that recipe's shared implementation
(`_windows-ffi-flavor`) ran its `cargo-win.cmd rustc` line **unconditionally**
inside `{{slot_build}}`. So every `windows-debug` — i.e. every windows e2e run's
build phase, `conftest.py::_ensure_app_built("windows")` — still acquired a
machine-wide `build` slot on a fully warm tree, and queued behind sibling builds
before cargo ever got the chance to say "nothing to do". Reading only
`windows-debug`'s own body (whose comment says the step is gated) wrongly
suggests windows was already fixed; it was not, for exactly that reason.

Text/structural analysis only — no real win build is needed to prove the wiring
exists. Real-build verification for the change that introduced this file is
recorded with its numbers in § UniFFI cdylib rebuild cost (win).
"""

import re
from pathlib import Path

import pytest

pytestmark = pytest.mark.tier_1

_REPO = Path(__file__).resolve().parents[3]
_JUSTFILE = _REPO / "justfile"

_CARGO_NEEDLE = "cargo-win.cmd rustc --locked -p fauna-ffi"


def _recipe_body(text: str, name: str) -> str:
    """The indented body of one justfile recipe — `name:` or a parametrized
    `name *ARGS:` declaration alike. Duplicated from
    `test_apple_ffi_host_warm_takes_no_slot.py` per this suite's
    no-cross-import-between-test-files convention (small helpers get
    duplicated, not imported)."""
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


def _flavor_body() -> str:
    body = _recipe_body(_JUSTFILE.read_text(encoding="utf-8"), "_windows-ffi-flavor")
    assert body, "_windows-ffi-flavor recipe not found"
    return body


def test_windows_ffi_flavor_cargo_step_is_freshness_gated_outside_the_slot():
    """`_windows-ffi-flavor` (shared by `windows-ffi` and `windows-ffi-test`,
    the latter being `windows-debug`'s prerequisite) must decide "is there
    work?" BEFORE queueing: build-if-stale outermost, `{{slot_build}}` inside
    it, the cargo-win.cmd rustc call innermost — the same shape
    `mail-bridge-ffi-lib`, `_apple-ffi-host-flavor` and `windows-debug`'s own
    MSBuild step already use.

    `--stamp`, not the .dll, must carry freshness: a cargo no-op leaves the
    .dll's mtime untouched, so an artifact-keyed gate would go permanently
    stale after any mtime churn that doesn't relink (a rebase does this to the
    whole tree) and would queue on every warm run anyway."""
    joined = " ".join(_flavor_body().split())
    assert _CARGO_NEEDLE in joined, "_windows-ffi-flavor must build fauna-ffi's cdylib"
    assert "build-if-stale.py" in joined, (
        "the cargo step must be gated by build-if-stale (a warm tree must take "
        "no build slot)"
    )
    assert "--stamp" in joined, (
        "the gate must be --stamp-keyed. NOTE: a `build-if-stale.py` WITHOUT "
        "--stamp already existed here before this fix — it gates the bindgen + "
        "dll-copy TAIL on the freshly-built .dll and sits AFTER the cargo line, "
        "so it does nothing for the slot wait ahead of it"
    )
    gate_pos = joined.index("build-if-stale.py")
    stamp_pos = joined.index("--stamp", gate_pos)
    slot_pos = joined.index("{{slot_build}}", gate_pos)
    cargo_pos = joined.index(_CARGO_NEEDLE, gate_pos)
    assert gate_pos < stamp_pos < slot_pos < cargo_pos, (
        "order must be build-if-stale (freshness) OUTSIDE {{slot_build}} "
        "OUTSIDE cargo — a slot acquired before the freshness verdict queues a "
        "warm tree, which is exactly the defect this pins"
    )


def test_windows_ffi_cargo_stamp_is_scoped_by_features_not_just_profile_dir():
    """Both flavors run ONE cargo invocation shape into ONE cargo output path
    (`target/$PROFILE_DIR/fauna_ffi.dll` since 2026-08-23's implicit-host move,
    `target/aarch64-pc-windows-msvc/$PROFILE_DIR/fauna_ffi.dll` before it —
    neither carries a feature axis), so a stamp keyed only on `$PROFILE_DIR` plus
    source mtimes would read "fresh" the moment a caller switches BACK to a flavor
    built earlier — even though the .dll cargo just left on disk holds the OTHER
    flavor's build. That is the convention-15 production/test seam separation, so
    the stamp path must carry the features axis too.

    Still load-bearing after the flavor-private copy landed
    (`test_win_host_tree_is_shared.py`): the private slot removes the *staging*
    ambiguity, but the shared cargo output path is what the build itself writes,
    so the stamp is still the only thing that can tell cargo to re-run."""
    body = _flavor_body()
    stamp_lines = [ln for ln in body.splitlines() if "CARGO_STAMP=" in ln]
    assert stamp_lines, "_windows-ffi-flavor must define a CARGO_STAMP path"
    assert "FEATURES" in stamp_lines[0], (
        "CARGO_STAMP must be scoped by $FEATURES (not just $PROFILE_DIR), or a "
        f"flavor switch silently skips a needed rebuild: {stamp_lines[0]!r}"
    )


def test_windows_ffi_flavor_guard_also_invalidates_the_cargo_stamp():
    """The pre-existing flavor guard drops the STAGED .dll on a
    `$PROFILE:$FEATURES` mismatch. It must ALSO remove `$CARGO_STAMP` in the
    same branch: a features-scoped stamp alone still reads "fresh" when
    switching back to a flavor whose stamp survives from an earlier run, while
    the shared cargo output path holds the other flavor's dll. The marker
    mismatch is itself the proof a rebuild is owed, independent of any stamp."""
    body = _flavor_body()
    guard = re.search(r"FLAVOR_MARKER=.*?\n\s*\}", body, re.DOTALL)
    assert guard, (
        "flavor-mismatch guard block not found — it must be a braced block so "
        "it can drop both the staged .dll and the cargo stamp"
    )
    guard_block = guard.group(0)
    assert "fauna_ffi.dll" in guard_block, (
        "sanity: this must be the flavor-mismatch guard block"
    )
    assert "CARGO_STAMP" in guard_block, (
        "the flavor guard's mismatch branch must also remove $CARGO_STAMP — "
        "leaving a stale stamp means a flavor switch can silently skip the "
        "cargo rebuild it requires, staging the other flavor's core"
    )


def test_windows_debug_msbuild_step_stays_freshness_gated_outside_the_slot():
    """Regression pin for the 2026-07-30 half of this fix, so a future edit
    cannot quietly undo it while the FFI half above stays green: MSBuild is the
    second of `windows-debug`'s two build legs, and both must be gated for the
    e2e build phase to take no slot warm.

    The needle is `{{msbuild_exe}}`, not a literal `MSBuild.exe` substring:
    row 124 (build-system.md § Windows toolchain location) collapsed the four
    absolute MSBuild.exe spellings the justfile used to carry into one
    discovered variable, so the RECIPE TEXT this test reads (pre-`{{}}`-
    interpolation) now names the variable, not the binary — `msbuild_exe`'s
    own definition is what still ends in `/MSBuild/Current/Bin/MSBuild.exe`."""
    joined = " ".join(_recipe_body(_JUSTFILE.read_text(encoding="utf-8"), "windows-debug").split())
    assert "{{msbuild_exe}}" in joined, "windows-debug must invoke MSBuild via {{msbuild_exe}}"
    assert "build-if-stale.py" in joined and "--stamp" in joined, (
        "windows-debug's MSBuild step must stay build-if-stale --stamp gated"
    )
    gate_pos = joined.index("build-if-stale.py")
    slot_pos = joined.index("{{slot_build}}", gate_pos)
    msbuild_pos = joined.index("{{msbuild_exe}}", gate_pos)
    assert gate_pos < slot_pos < msbuild_pos, (
        "order must be build-if-stale OUTSIDE {{slot_build}} OUTSIDE MSBuild"
    )
