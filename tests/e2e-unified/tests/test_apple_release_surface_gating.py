"""Convention 15, apple's release surface: no shipped apple artifact carries the
File Provider test CLI, names an e2e environment variable, or links the test FFI
flavor.

`docs/goal/architecture/e2e-automation-surface-gating.md` § The convention (the
Swift bullet, and the flavor-staleness caveat that follows the list). Two leaks
the seam-call scan in `test_apple_seam_gating.py` cannot see, because neither is
a `*_for_test` seam:

1. **The File Provider test CLI.** `FileProviderTestCLI` is the headless driver
   behind the signing-gated tier_3 read-path proof. Its verbs — `provision`
   (writes a nest URL, bearer and backup key from argv into the File Provider
   credential store), `revoke`, `register`/`remove`/`list`, `signal` — were gated
   only by `canImport(FileProvider)` and ran from the shared `@main` entry
   (`PlatformMainEntry`), so any process running as the user could drive the
   signed RELEASE binary to repoint or revoke that user's File Provider. It is
   automation surface like the `TestAgent`, so it gets the same gate: `#if DEBUG`.

2. **The iOS archive links whichever FFI flavor was staged last.** `apple-ffi`
   and `apple-ffi-test` write ONE staging slot (`FaunaFFI.xcframework`), so the
   `apps/fauna-apple/.ffi-flavor` marker is the whole gate. The iOS archive
   fixture asked for a production `just apple-ffi` in prose and checked only that
   the device slice existed, so an archive made after an e2e run linked the test
   flavor's seams.

3. **The `FAUNA_E2E_*` environment surface.** `FaunaE2E.isActive` sat outside
   every `#if DEBUG` and was read straight from `ProcessInfo`, and so were the
   redirects behind it: `FAUNA_E2E_DOWNLOAD_DIR` (relocates the user's exported
   snapshot and account data, from ungated production views on both targets),
   `FAUNA_E2E_CREDENTIAL_DIR` (relocates the credential store — and, in
   `AccountReauth`, stands in for `LAContext.deviceOwnerAuthentication` outright).
   Every read now goes through the one gated door `E2eEnv`, apple's twin of
   windows' `FaunaApp.Core/Services/E2eEnv.cs`, and the payload-bearing branches
   carry their own `#if DEBUG` on top. The pins below are the source half; the
   artifact half is `_apple-assert-no-e2e-env` in the justfile.

**Why source pins AND an artifact witness.** Convention 15 verifies by reading the
built artifact, and the two `*-store-safe-check` recipes already build a Release
macOS binary and a Release iOS archive and `strings` them — so the recipes carry
the artifact witness (`_apple-assert-no-fp-test-cli`). It is a minutes-long
release build, though, so this file pins the cheap half — the source shape that
makes the artifact clean — in milliseconds on every machine, plus the wiring that
keeps the artifact witness derived from the CLI rather than maintained by hand.

Pure text analysis of Swift + the justfile + the fixture — no build, no toolchain.
"""

import re
from pathlib import Path

import pytest

from helpers.swift_gating import debug_protected_lines, hand_written_swift, strip_comments

pytestmark = pytest.mark.tier_1

_REPO = Path(__file__).resolve().parents[3]
_APPLE = _REPO / "apps" / "fauna-apple"
_CLI = _APPLE / "FaunaKit" / "Sources" / "FaunaKit" / "FileProvider" / "FileProviderTestCLI.swift"
#: The one gated door for `FAUNA_E2E_*`. It shares a file with the `FaunaE2E`
#: façade on purpose: `check-apple-ios-typecheck` typechecks each file ALONE for
#: the iOS simulator, and `AutomationRegistry.swift` is one of the few that pass
#: standalone — a `Core/E2eEnv.swift` of its own made this file reference another
#: FaunaKit type and the gate refused the merge, which is that ratchet's
#: coverage-loss direction working exactly as designed.
_E2E_ENV_DOOR = _APPLE / "FaunaKit" / "Sources" / "FaunaKit" / "Testing" / "AutomationRegistry.swift"
_JUSTFILE = _REPO / "justfile"
_IOS_ARCHIVE_TEST = (
    _REPO / "tests" / "e2e-unified" / "tests" / "artifact" / "test_ios_archive.py"
)

#: Swift stores a string literal of at most this many UTF-8 bytes INLINE (a "small
#: string": an immediate in the instruction stream), so `strings` can never see
#: it in any binary — Debug included. A marker that short would make the artifact
#: witness vacuous for a reason no red run could show.
_SWIFT_SMALL_STRING_MAX_BYTES = 15


def test_the_fp_test_cli_is_compiled_only_under_if_debug():
    """The declaration is wholly inside `#if DEBUG && canImport(FileProvider)`, with
    NO `#else`: the Release build gets nothing — not the verbs, and not a no-op
    twin that would leave the type (and its dispatch table) behind.

    `canImport(FileProvider)` stays in the condition: watchOS has no FileProvider
    framework, and `#if DEBUG` alone would try to `import` it there.
    """
    raw = _CLI.read_text(encoding="utf-8")
    code = strip_comments(raw)
    protected = debug_protected_lines(raw)

    assert re.search(r"^#if DEBUG && canImport\(FileProvider\)\s*$", code, re.MULTILINE), (
        f"{_CLI.name} must open with `#if DEBUG && canImport(FileProvider)` — convention 15's "
        "Swift gate, plus the platform gate the file already needed."
    )
    unprotected = [
        f"{_CLI.name}:{n}: {line.strip()}"
        for n, line in enumerate(code.splitlines(), start=1)
        if re.search(r"\b(enum|struct|class|extension|func)\b", line) and n not in protected
    ]
    assert not unprotected, (
        "the File Provider test CLI declares code a Release build compiles. It writes a "
        "bearer + backup key from argv and repoints or revokes the user's File Provider, so "
        "it is automation surface (convention 15) and must be `#if DEBUG` only:\n  "
        + "\n  ".join(unprotected)
    )
    assert not re.search(r"^\s*#else\b", code, re.MULTILINE), (
        f"{_CLI.name} has an `#else` branch. The Release build must receive NOTHING from this "
        "file: gate the (single) call site in `PlatformMainEntry` instead of shipping a "
        "no-op twin."
    )


def test_every_reference_to_the_fp_test_cli_outside_its_file_sits_under_if_debug():
    """The declaration being gated is half the boundary: an ungated CALL is a Release
    compile error at best, and at worst a reason someone re-adds a Release stub.

    Guard the derivation first — a scan that stopped matching would pass vacuously.
    """
    assert re.search(r"\benum FileProviderTestCLI\b", strip_comments(_CLI.read_text(encoding="utf-8"))), (
        f"{_CLI.name} no longer declares `FileProviderTestCLI` — this scan is looking for a "
        "name that does not exist, so every assertion below would pass vacuously."
    )
    offenders: list[str] = []
    for f in hand_written_swift(_APPLE):
        if f == _CLI:
            continue
        raw = f.read_text(encoding="utf-8", errors="ignore")
        protected = debug_protected_lines(raw)
        for n, line in enumerate(strip_comments(raw).splitlines(), start=1):
            if "FileProviderTestCLI" in line and n not in protected:
                offenders.append(f"{f.relative_to(_REPO)}:{n}: {line.strip()[:100]}")
    assert not offenders, (
        "hand-written apple Swift references `FileProviderTestCLI` from code that compiles "
        "into a Release build (convention 15). Put the call behind `#if DEBUG && "
        "canImport(FileProvider)`:\n  " + "\n  ".join(offenders)
    )


def _markers() -> list[str]:
    """The strings the Release-artifact witness greps for, from the justfile's one
    definition — both `*-store-safe-check` recipes use that variable."""
    m = re.search(r'^fp_test_cli_markers\s*:=\s*"([^"]+)"', _JUSTFILE.read_text(encoding="utf-8"), re.MULTILINE)
    assert m, (
        "the justfile defines no `fp_test_cli_markers` — the Release-artifact witness for "
        "the File Provider test CLI has nothing to grep for"
    )
    return m.group(1).split("|")


def test_the_artifact_markers_are_the_clis_own_visible_literals():
    """Each marker must be a string literal in the CLI source AND long enough to reach
    the binary's `__cstring` — the two ways a Release-absence grep goes vacuous.

    A marker that is not in the source rots silently when the CLI's messages are
    reworded (the grep would then match nothing in Debug OR Release). One of at most
    15 UTF-8 bytes is a Swift small string, stored inline, so no `strings` run finds
    it in any build.
    """
    code = strip_comments(_CLI.read_text(encoding="utf-8"))
    string_literal_lines = [line for line in code.splitlines() if '"' in line]
    markers = _markers()
    assert len(markers) >= 3, f"too few markers to cover the CLI's verbs: {markers!r}"
    for marker in markers:
        assert len(marker.encode("utf-8")) > _SWIFT_SMALL_STRING_MAX_BYTES, (
            f"marker {marker!r} is {len(marker.encode('utf-8'))} bytes — Swift stores literals "
            f"of <= {_SWIFT_SMALL_STRING_MAX_BYTES} bytes inline, so `strings` can never see it "
            "and the witness is vacuous."
        )
        assert any(marker in line for line in string_literal_lines), (
            f"marker {marker!r} is not a string literal in {_CLI.name} — the CLI's messages "
            "were reworded and the artifact witness now greps for nothing."
        )


def _recipe_body(name: str) -> str:
    text = _JUSTFILE.read_text(encoding="utf-8")
    m = re.search(rf"^{re.escape(name)}(?:\s+[^:\n]*)?:.*\n((?:[ \t]+.*\n|\n)*)", text, re.MULTILINE)
    assert m, f"no recipe named {name!r} in the justfile"
    return m.group(1)


def test_the_release_artifact_recipes_assert_the_cli_is_absent():
    """Every Release artifact the two `*-store-safe-check` recipes build is scanned:
    macOS builds two binaries (store-safe, default) and scans each; iOS routes both
    archives through one `archive()` function, which scans whatever it archives. The
    helper recipe carries the one grep, over the ONE marker list."""
    helper = _recipe_body("_apple-assert-no-fp-test-cli")
    assert "fp_test_cli_markers" in helper and "strings -a" in helper, (
        "`_apple-assert-no-fp-test-cli` must `strings -a` the artifact and grep it for "
        "`{{fp_test_cli_markers}}`"
    )

    macos = _recipe_body("_apple-store-safe-check-impl")
    calls = re.findall(r"just\s+_apple-assert-no-fp-test-cli\b", macos)
    assert len(calls) >= 2, (
        "`_apple-store-safe-check-impl` builds two Release binaries (store-safe + default) "
        f"and must run `just _apple-assert-no-fp-test-cli` on each; found {len(calls)} call(s)"
    )

    ios = _recipe_body("apple-ios-store-safe-check")
    archive_fn = re.search(r"archive\(\)\s*\{(.*?)^    \}", ios, re.DOTALL | re.MULTILINE)
    assert archive_fn, "`apple-ios-store-safe-check` no longer defines its `archive()` function"
    assert re.search(r"just\s+_apple-assert-no-fp-test-cli\b", archive_fn.group(1)), (
        "`archive()` must run `just _apple-assert-no-fp-test-cli` on the archive it just built "
        "— it is the one door every iOS archive in this recipe goes through"
    )
    archived = re.findall(r"^\s+archive\s+(store-safe|default)\b", ios, re.MULTILINE)
    assert sorted(archived) == ["default", "store-safe"], (
        f"expected `archive` to run for both columns (store-safe, default); saw {archived}"
    )


def _refusal(marker):
    from helpers.macos_artifact import ffi_flavor_refusal

    return ffi_flavor_refusal(marker)


@pytest.mark.parametrize(
    "marker",
    ["full-3slice:test-helpers", "full-5slice:test-helpers\n", "full-3slice-dist:test-helpers", "host:test-helpers"],
)
def test_a_staged_test_flavor_is_refused_for_an_ios_archive(marker):
    """`apple-ffi` and `apple-ffi-test` share one staging slot, so a test flavor left
    behind by an e2e run would link into the archive — and the slice check alone
    cannot tell (the test flavor carries the same `ios-arm64` slice)."""
    why = _refusal(marker)
    assert why and "test-helpers" in why and "just apple-ffi" in why, why


@pytest.mark.parametrize(
    "marker",
    ["full-3slice:", "full-5slice:\n", "full-3slice-dist:", "full-3slice:store-safe"],
)
def test_a_production_or_store_safe_flavor_is_accepted_for_an_ios_archive(marker):
    """The production flavor writes an EMPTY features half; store-safe is the App
    Store escape hatch and is a shipping flavor too."""
    assert _refusal(marker) is None


@pytest.mark.parametrize("marker", [None, "", "\n", "no-colon-at-all"])
def test_an_unknowable_flavor_is_refused_for_an_ios_archive(marker):
    """Fail closed: a missing or unparseable marker cannot prove the staged xcframework
    is a production one, and an archive is the artifact that ships."""
    why = _refusal(marker)
    assert why and ".ffi-flavor" in why and "just apple-ffi" in why, why


def test_the_ios_archive_fixture_consults_the_flavor_refusal():
    """The refusal is only worth having if the archive fixture calls it, on the marker
    the recipes actually write, BEFORE it builds anything."""
    src = _IOS_ARCHIVE_TEST.read_text(encoding="utf-8")
    fixture = src[src.index("def ios_device_ffi"):src.index("def ios_archive")]
    assert "ffi_flavor_refusal(" in fixture, (
        "`ios_device_ffi` must call `ffi_flavor_refusal` on `apps/fauna-apple/.ffi-flavor` — "
        "the slice check alone lets a staged test flavor through"
    )
    assert ".ffi-flavor" in fixture, "the fixture must read the `.ffi-flavor` marker"


# --------------------------------------------------------------------------
# 3. The `FAUNA_E2E_*` environment surface — one gated door, no name in Release
# --------------------------------------------------------------------------

#: Regex for an e2e environment variable named as a Swift string literal.
_E2E_ENV_LITERAL = re.compile(r'"(FAUNA_E2E_[A-Z0-9_]*)"')

#: Members of the door that deliberately have no production twin, with the reason.
#: Each is read from a `#if DEBUG` call site only, so no Release build can reach
#: it, and a twin would put the literal it carries back into the artifact.
_DOOR_DEBUG_ONLY_MEMBERS = {
    "agentPortName": "the spawn side composes an environment rather than reading one",
}


def _door_arms() -> tuple[str, str]:
    """The `E2eEnv` declaration split into its (`#if DEBUG`, `#else`) arms.

    Bounded by brace depth rather than "to end of file": the door shares
    `AutomationRegistry.swift` with 1700 further lines, and an unbounded scan
    would read `FaunaE2E`'s and the registry's members as the door's own.
    """
    raw = _E2E_ENV_DOOR.read_text(encoding="utf-8")
    code = strip_comments(raw)
    protected = debug_protected_lines(raw)
    debug_arm, prod_arm = [], []
    depth, started = 0, False
    for n, line in enumerate(code.splitlines(), start=1):
        if not started:
            if not re.search(r"\benum E2eEnv\b", line):
                continue
            started = True
        (debug_arm if n in protected else prod_arm).append(line)
        depth += line.count("{") - line.count("}")
        if started and depth <= 0 and "{" in "".join(debug_arm + prod_arm):
            break
    assert started, f"no `enum E2eEnv` declaration in {_E2E_ENV_DOOR.name}"
    return "\n".join(debug_arm), "\n".join(prod_arm)


def _members(arm: str) -> set[str]:
    return set(re.findall(r"\bstatic\s+(?:var|let)\s+([A-Za-z_][A-Za-z0-9_]*)", arm))


def test_no_e2e_env_variable_is_named_outside_the_one_gated_door():
    """A `"FAUNA_E2E_*"` literal anywhere in hand-written apple Swift EXCEPT the door
    is the defect this section exists for.

    Keyed on the literal rather than on a read, so it catches the three shapes that
    slipped past review: a bare `ProcessInfo` read (`AccountReauth`), a read behind a
    runtime-only predicate (`SnapshotFileSaver`, `KeychainStore`), and a name
    *composed* into a child's environment (`InstanceSpawner`).
    """
    offenders: list[str] = []
    for f in hand_written_swift(_APPLE):
        if f == _E2E_ENV_DOOR:
            continue
        for n, line in enumerate(strip_comments(f.read_text(encoding="utf-8", errors="ignore")).splitlines(), start=1):
            if _E2E_ENV_LITERAL.search(line):
                offenders.append(f"{f.relative_to(_REPO)}:{n}: {line.strip()[:100]}")
    assert not offenders, (
        "hand-written apple Swift names an e2e environment variable outside "
        f"{_E2E_ENV_DOOR.name}. Every read goes through that one gated door so a Release "
        "artifact carries no harness variable at all (convention 15):\n  " + "\n  ".join(offenders)
    )


def test_the_door_names_every_variable_only_under_if_debug():
    """The door's whole point: the names live in the `#if DEBUG` arm, so the `#else`
    arm a Release build compiles contains no literal for `strings` to find.

    Guard the derivation first — a door that stopped naming any variable would make
    every assertion here pass vacuously.
    """
    raw = _E2E_ENV_DOOR.read_text(encoding="utf-8")
    code = strip_comments(raw)
    protected = debug_protected_lines(raw)
    named = [
        (n, m.group(1))
        for n, line in enumerate(code.splitlines(), start=1)
        for m in [_E2E_ENV_LITERAL.search(line)]
        if m
    ]
    assert len(named) >= 6, (
        f"{_E2E_ENV_DOOR.name} names only {len(named)} e2e variable(s) — either the door was "
        "emptied (and the readers went back to reading ProcessInfo directly) or this scan no "
        "longer matches, in which case every assertion here is vacuous."
    )
    ungated = [f"{_E2E_ENV_DOOR.name}:{n}: {name}" for n, name in named if n not in protected]
    assert not ungated, (
        "the door names an e2e variable from its production arm, so a Release artifact "
        "carries the literal and the artifact witness (`_apple-assert-no-e2e-env`) will go "
        "red:\n  " + "\n  ".join(ungated)
    )


def test_every_release_reachable_door_member_has_a_production_twin():
    """A `#if DEBUG`-only member turns a future production-path read into a
    RELEASE-ONLY compile error — invisible to every debug build and every e2e run,
    which is how `just windows-release` sat unbuildable on `origin/main` for a week
    (§ Implementation status today, the C# leg). The twin is what keeps every call
    site's production arm compiling by construction.
    """
    debug_arm, prod_arm = _door_arms()
    debug_members, prod_members = _members(debug_arm), _members(prod_arm)
    assert debug_members, "no members found in the door's `#if DEBUG` arm — the scan is broken"
    missing = debug_members - prod_members - set(_DOOR_DEBUG_ONLY_MEMBERS) - {"env"}
    assert not missing, (
        f"{_E2E_ENV_DOOR.name}'s production arm declares no twin for: {sorted(missing)}. Add a "
        "constant-answering twin, or record the member in `_DOOR_DEBUG_ONLY_MEMBERS` with the "
        "reason its only call site can never be Release-compiled."
    )
    orphans = prod_members - debug_members
    assert not orphans, (
        f"{_E2E_ENV_DOOR.name}'s production arm declares {sorted(orphans)}, which the DEBUG arm "
        "does not — the two arms have drifted and the debug build takes a different shape."
    )


def test_only_the_door_reads_the_door_from_release_compiled_code():
    """The predicate façade (`FaunaE2E`) is twin-backed and reads as a constant in
    Release, so its 21 production call sites are fine — and it shares the door's file,
    so it is covered by the same exclusion. Every OTHER reader is standing in front of
    a payload — a relocated credential store, a redirected export, a re-auth verdict
    read off disk — and severity is the payload's, so those branches carry their own
    `#if DEBUG` rather than trusting an optimizer to fold a branch behind a constant
    `nil` (§ Implementation status today, the tui `download_dir()` bullet).
    """
    offenders: list[str] = []
    for f in hand_written_swift(_APPLE):
        if f == _E2E_ENV_DOOR:
            continue
        raw = f.read_text(encoding="utf-8", errors="ignore")
        protected = debug_protected_lines(raw)
        for n, line in enumerate(strip_comments(raw).splitlines(), start=1):
            if re.search(r"\bE2eEnv\.", line) and n not in protected:
                offenders.append(f"{f.relative_to(_REPO)}:{n}: {line.strip()[:100]}")
    assert not offenders, (
        "apple Swift reads `E2eEnv` from a line a Release build compiles, outside the door "
        f"({_E2E_ENV_DOOR.name}) and its `FaunaE2E` façade. Put the whole e2e branch under "
        "`#if DEBUG`:\n  " + "\n  ".join(offenders)
    )


def test_the_e2e_env_names_are_long_enough_for_strings_to_see():
    """The artifact witness greps a `FAUNA_E2E_` prefix, which is only 10 bytes — but
    what a build would actually carry is a full NAME. Every one must clear the Swift
    small-string cutoff, or the absence grep is vacuous for a reason no red run could
    show (the same guard the File Provider markers carry above)."""
    names = set(_E2E_ENV_LITERAL.findall(strip_comments(_E2E_ENV_DOOR.read_text(encoding="utf-8"))))
    assert names, f"{_E2E_ENV_DOOR.name} names no e2e variable — nothing to measure"
    short = {n for n in names if len(n.encode("utf-8")) <= _SWIFT_SMALL_STRING_MAX_BYTES}
    assert not short, (
        f"{sorted(short)} are at most {_SWIFT_SMALL_STRING_MAX_BYTES} UTF-8 bytes, so Swift "
        "stores them inline and `strings` can never see them — the artifact witness cannot "
        "witness their absence. Rename, or give the witness a different marker."
    )


def test_the_release_artifact_recipes_assert_no_e2e_env_variable_ships():
    """The source pins above make the artifact clean; this is the wiring that checks
    the artifact itself, on every Release binary the two `*-store-safe-check` recipes
    build — the macOS pair and the one iOS `archive()` door."""
    text = _JUSTFILE.read_text(encoding="utf-8")
    marker = re.search(r'^apple_e2e_env_marker\s*:=\s*"([^"]+)"', text, re.MULTILINE)
    assert marker, (
        "the justfile defines no `apple_e2e_env_marker` — the Release-artifact witness for "
        "apple's e2e environment surface has nothing to grep for"
    )
    assert marker.group(1) == "FAUNA_E2E_", (
        f"the marker is {marker.group(1)!r}; the source pins above derive from the "
        "`FAUNA_E2E_` prefix, so the two halves would be witnessing different things"
    )

    helper = _recipe_body("_apple-assert-no-e2e-env")
    assert "apple_e2e_env_marker" in helper and "strings -a" in helper, (
        "`_apple-assert-no-e2e-env` must `strings -a` the artifact and grep it for "
        "`{{apple_e2e_env_marker}}`"
    )

    macos = _recipe_body("_apple-store-safe-check-impl")
    calls = re.findall(r"just\s+_apple-assert-no-e2e-env\b", macos)
    assert len(calls) >= 2, (
        "`_apple-store-safe-check-impl` builds two Release binaries (store-safe + default) "
        f"and must run `just _apple-assert-no-e2e-env` on each; found {len(calls)} call(s)"
    )

    ios = _recipe_body("apple-ios-store-safe-check")
    archive_fn = re.search(r"archive\(\)\s*\{(.*?)^    \}", ios, re.DOTALL | re.MULTILINE)
    assert archive_fn, "`apple-ios-store-safe-check` no longer defines its `archive()` function"
    assert re.search(r"just\s+_apple-assert-no-e2e-env\b", archive_fn.group(1)), (
        "`archive()` must run `just _apple-assert-no-e2e-env` on the archive it just built — "
        "it is the one door every iOS archive in this recipe goes through"
    )
