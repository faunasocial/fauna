"""tier_1: the windows FFI recipes must guard the flavor switch they now have.

Pure text/regex analysis of the justfile + .gitignore. No cargo, no MSBuild, no
driver — this proves the wiring WITHOUT a real FFI build (each one costs 8+ min).

Guards a drift class that is SILENT by construction. The shared impl
`_windows-ffi-flavor` is parameterized on TWO axes — the cargo profile
(`dev` | `release` | `dist`) and the feature set (production vs `test-helpers`,
added 2026-08-01 by convention 15's recipe split) — but every combination stages
into ONE fixed pair of consumer paths:

    apps/fauna-windows/FaunaApp/FaunaApp/runtimes/win-arm64/native/fauna_ffi.dll
    apps/fauna-windows/FaunaApp/FaunaApp.Core/Generated/uniffi/

and `build-if-stale` decides whether to restage by comparing *mtimes*. So without a
flavor marker, alternating profiles serves the WRONG core:

  1. `just windows-debug`  -> stages the dev .dll  (mtime T2)
  2. `just windows-release` -> cargo no-ops off a warm cache, so the release .dll
     keeps its ORIGINAL mtime T1 < T2; the gate compares T1 against the staged
     T2, reads "fresh", and skips the copy.
  3. The release app now links the DEV core, and nothing anywhere says so.

There is no error, no warning, and no failing build — the app just runs the other
flavor. That is why this is a test and not a comment. Apple hit the identical
hazard first and solved it the identical way (`apps/fauna-apple/.ffi-flavor`,
build-system.md § Host-only debug FFI (mac dev loop)); this pins the Windows twin.

The invariant, in one sentence: **if the windows FFI recipes can build more than
one flavor into a shared staging path, they must drop the staged artifact whenever
the recorded flavor differs from the requested one, and record the flavor just
staged.** After the feature split the marker's `$PROFILE:$FEATURES` shape is the
security-relevant half: a profile-only marker reads "fresh" across a
production<->test switch at the same profile and leaves seams staged for a
`windows-release` (testing.md convention 15; the recipe-shape half of that split
is pinned in `test_ffi_flavor_split.py`).
"""

import os
import re

import pytest

pytestmark = pytest.mark.tier_1

_HERE = os.path.dirname(__file__)
_REPO = os.path.normpath(os.path.join(_HERE, "..", "..", ".."))
_JUSTFILE = os.path.join(_REPO, "justfile")
_GITIGNORE = os.path.join(_REPO, ".gitignore")

_MARKER = "apps/fauna-windows/.ffi-flavor"
_STAGED_DLL = (
    "apps/fauna-windows/FaunaApp/FaunaApp/runtimes/win-arm64/native/fauna_ffi.dll"
)


def _read(path):
    with open(path, encoding="utf-8") as fh:
        return fh.read()


def _recipe_body(text, name):
    """Return the lines of a just recipe body (up to the next top-level recipe).

    just recipe bodies are indented; a new recipe starts at column 0. Good enough
    for the shape assertions below, and it keeps this test free of a just parser.
    """
    lines = text.splitlines()
    start = None
    for i, line in enumerate(lines):
        if re.match(rf"^{re.escape(name)}(\s+[^:]*)?:", line):
            start = i
            break
    assert start is not None, f"recipe {name!r} not found in justfile"
    body = []
    for line in lines[start + 1 :]:
        if line and not line[0].isspace() and not line.startswith("#"):
            break
        body.append(line)
    return "\n".join(body)


def test_windows_debug_takes_the_dev_flavor():
    """The dev loop must ask for the TEST flavor at the `dev` profile, not inherit
    `windows-ffi`'s production `release` default.

    A bare `windows-debug: windows-ffi` silently reverts the dev loop to the
    release core — the exact state this track migrated away from — and would still
    build and pass every test, so only an explicit assertion catches it. Since
    2026-08-01 the recipe name also carries the feature axis: `windows-ffi-test`
    is the only flavor whose bindings expose the seams the `#if DEBUG` TestAgent
    calls, so a plain `windows-ffi` here would not compile Debug at all.
    """
    text = _read(_JUSTFILE)
    match = re.search(r"^windows-debug:(.*)$", text, re.M)
    assert match, "windows-debug recipe not found"
    deps = match.group(1)
    assert re.search(r"\(\s*windows-ffi-test\s+[\"']dev[\"']\s*\)", deps), (
        "windows-debug must depend on (windows-ffi-test \"dev\") so the dev loop "
        f"links the debug core WITH the e2e seams; got dependencies: {deps.strip()!r}"
    )


def test_windows_release_stays_on_the_release_flavor():
    """The release app keeps a release core — the flip is dev-loop-only."""
    text = _read(_JUSTFILE)
    match = re.search(r"^windows-release:(.*)$", text, re.M)
    assert match, "windows-release recipe not found"
    deps = match.group(1)
    assert "windows-ffi" in deps, "windows-release must still build the FFI"
    assert not re.search(r"windows-ffi\s+[\"']dev[\"']", deps), (
        "windows-release must NOT take the dev flavor — a shipped release app "
        f"links a release core; got: {deps.strip()!r}"
    )
    assert "windows-ffi-test" not in deps, (
        "windows-release must take the PRODUCTION flavor — windows-ffi-test's "
        f"bindings export the e2e seams (convention 15); got: {deps.strip()!r}"
    )


def test_windows_ffi_maps_dev_profile_to_the_debug_output_dir():
    """cargo writes `--profile dev` into `debug/`, not `dev/`.

    Using the profile name directly as a path component silently looks for a
    `target/<triple>/dev/fauna_ffi.dll` that cargo never writes, so the bindgen
    gate would read a stale or missing artifact.
    """
    body = _recipe_body(_read(_JUSTFILE), "_windows-ffi-flavor")
    assert re.search(r"dev\s*\)\s*PROFILE_DIR=debug", body), (
        "_windows-ffi-flavor must map the `dev` profile to the `debug` output dir "
        "(cargo's naming rule), e.g. `dev) PROFILE_DIR=debug ;;`"
    )
    # The staging paths must go through the mapped dir, never the raw profile name.
    assert "aarch64-pc-windows-msvc/{{profile}}" not in body, (
        "_windows-ffi-flavor must not use the raw {{profile}} as a path component — "
        "`dev` would resolve to a directory cargo never writes"
    )


def test_windows_ffi_drops_the_staged_dll_on_a_flavor_switch():
    """The guard itself: a recorded-flavor mismatch must invalidate the staging.

    Without this, build-if-stale's mtime comparison serves the previously-staged
    flavor (see the module docstring's 3-step walkthrough).
    """
    body = _recipe_body(_read(_JUSTFILE), "_windows-ffi-flavor")
    assert _MARKER in body, (
        f"_windows-ffi-flavor must consult the {_MARKER} flavor marker"
    )
    # RID-parameterized since 2026-08-24 (the x64 cross leg) — `$RID` resolves to
    # `win-arm64` (default host) or `win-x64`, not the literal `win-arm64` alone.
    assert re.search(r'rm\s+-f\s+"\S*runtimes/\$RID/native/fauna_ffi\.dll"', body), (
        "_windows-ffi-flavor must `rm -f` the staged runtimes/$RID/ dll when the "
        "recorded flavor differs from the requested one, so build-if-stale restages it"
    )


def test_windows_ffi_records_the_flavor_it_staged():
    """A guard that never writes the marker can never detect a switch."""
    body = _recipe_body(_read(_JUSTFILE), "_windows-ffi-flavor")
    # RID joined the marker 2026-08-24 alongside PROFILE:FEATURES (the x64 cross
    # leg) — same collision class the FEATURES axis already guarded, one axis
    # further.
    assert re.search(r"echo\s+\"\$RID:\$PROFILE:\$FEATURES\"\s*>\s*\"\$FLAVOR_MARKER\"", body), (
        "_windows-ffi-flavor must record the staged RID, profile AND feature set "
        "after a successful build, else the next invocation cannot tell the flavor "
        "changed"
    )


def test_gnullvm_mail_bridge_maps_dev_profile_too():
    """The sibling windows FFI recipe carries the same profile-dir invariant.

    `windows-mail-bridge-build` builds its own gnullvm `fauna-ffi` and derives an
    output dir from the same profile argument. Latent today (callers pass
    `release`/`dist`), but identical in kind — pinned so the two windows FFI paths
    can't drift apart on the rule.

    A refactor extracted this into the shared `_windows-go-cgo-build` helper
    (now used by the mail bridge, the atproto bridge, and the seal-helper), and
    its cargo step + cgo env later moved one level down into `_windows-go-cgo-env`
    (so the go-test sibling `mail-bridge-test-win-cgo` shares it too) — the
    mapping lives in THAT helper's body; `windows-mail-bridge-build` is a
    one-line delegating call.
    """
    body = _recipe_body(_read(_JUSTFILE), "windows-mail-bridge-build")
    assert "_windows-go-cgo-build" in body, (
        "windows-mail-bridge-build should delegate to the shared gnullvm/cgo build "
        "helper — if this changed, the profile-dir mapping check below must move too"
    )
    build_body = _recipe_body(_read(_JUSTFILE), "_windows-go-cgo-build")
    assert "_windows-go-cgo-env" in build_body, (
        "_windows-go-cgo-build should take its cargo step from _windows-go-cgo-env "
        "— if this changed, the profile-dir mapping check below must move too"
    )
    helper_body = _recipe_body(_read(_JUSTFILE), "_windows-go-cgo-env")
    assert re.search(r"dev\s*\)\s*PROFILE_DIR=debug", helper_body), (
        "_windows-go-cgo-env must map the `dev` profile to `debug` when "
        "deriving its output dir, same as _windows-ffi-flavor"
    )


def test_windows_flavor_marker_is_gitignored():
    """The marker is local build state, like its apple twin."""
    ignored = _read(_GITIGNORE)
    assert _MARKER in ignored, (
        f"{_MARKER} is per-checkout build state and must be gitignored "
        "(mirrors apps/fauna-apple/.ffi-flavor)"
    )
