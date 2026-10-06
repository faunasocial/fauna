"""Shell-out helpers for the macOS client-artifact suite (`tests/artifact/`).

Lives in `helpers/` rather than beside the tests because a `conftest.py` is not
importable by name — `tests/e2e-unified` is on `sys.path`, so `import conftest`
resolves to the ROOT conftest, silently, and the artifact directory's own
helpers would never be found.

Everything here is a thin wrapper over an Apple command-line tool. The one rule
they all follow is convention 6's: a failure quotes the exact command and its
output, so an `hdiutil`/`xcodebuild` red diagnoses itself instead of needing a
re-run to see what happened.
"""

from __future__ import annotations

import contextlib
import subprocess
from pathlib import Path
from typing import Iterator

import pytest

# The extended attribute LaunchServices stamps on anything that arrived from a
# browser, a mail client, or a mounted disk image. Its presence is what puts a
# copied-out app through Gatekeeper assessment and App Translocation, so a DMG
# test that does not set it is testing a plain directory copy, not an install.
#: Where `just mac-app debug` assembles the bundle (the recipe's `build/<Config>`
#: convention), relative to the repo root. ONE definition, imported by both the
#: root conftest (which points the driver at it) and the artifact suite (which
#: builds and asserts on it) — two copies of a path like this drift silently, and
#: the failure would be a suite testing a stale bundle.
#:
#: DEBUG, and it must be: convention 15 compiles the automation surface out of
#: release artifacts, so `build/Release/Fauna.app` — the bundle `mac-dmg` and the
#: installer .pkg actually ship — carries no in-process agent and cannot be driven.
#: The substitution is honest because `mac-app` branches on `{{config}}` only for
#: the FFI flavour and `swift build -c`; every packaging step after that is
#: config-independent, which `test_release_and_debug_bundles_are_packaged_by_the
#: _same_steps` pins by reading the recipe.
ARTIFACT_APP_RELPATH = "build/Debug/Fauna.app"

QUARANTINE_XATTR = "com.apple.quarantine"
# The value shape a real download carries: flags;timestamp;agent;UUID. `0081`
# is "quarantined, never opened" — the state a freshly-downloaded app is in.
QUARANTINE_DOWNLOAD_VALUE = "0081;00000000;Safari;"


#: The apple FFI staging slot's flavor marker, relative to the repo root.
#: `_apple-ffi-flavor` writes `FLAVOR:FEATURES` into it after every staging, where
#: FEATURES is the production flavor's EMPTY string, `test-helpers` (e2e) or
#: `store-safe`. `apple-ffi` and `apple-ffi-test` write ONE xcframework slot, so
#: this marker is the only record of which one is on disk.
FFI_FLAVOR_MARKER_RELPATH = "apps/fauna-apple/.ffi-flavor"


def ffi_flavor_refusal(marker: str | None) -> str | None:
    """Why the staged apple FFI must NOT be archived for a shipping iOS build, or
    None when it may be.

    Convention 15 (`e2e-automation-surface-gating.md`, the flavor-staleness
    caveat): a build output shared by two flavors must be guarded against serving
    the wrong one, and a release archive is the one consumer that must never get
    the test flavor. The slice check alone cannot tell — the test flavor carries
    the same `ios-arm64` slice — so an archive made after an e2e run silently
    linked the `test-helpers` seams into the shipping binary.

    Fails CLOSED on a missing or unparseable marker: it cannot prove the staged
    xcframework is a production one, and an archive is the artifact that ships.
    The `store-safe` flavor is accepted — it is the App Store escape hatch, a
    shipping flavor with no seams.
    """
    text = (marker or "").strip()
    _flavor, sep, features = text.partition(":")
    if not text or not sep:
        state = "absent" if marker is None else f"unparseable ({marker!r})"
        return (
            f"{FFI_FLAVOR_MARKER_RELPATH} is {state}, so the staged FaunaFFI.xcframework's "
            "flavor is unknown. An iOS archive is the artifact that ships and may link only "
            "a PRODUCTION xcframework.\n"
            "Fix: run `just apple-ffi` (the production 5-slice build) and retry."
        )
    if "test-helpers" in features:
        return (
            f"the staged FaunaFFI.xcframework is the TEST flavor ({FFI_FLAVOR_MARKER_RELPATH} = "
            f"{text!r}). `just apple-ffi` and `just apple-ffi-test` share ONE staging slot, so "
            "an iOS archive made now would link the `test-helpers` seams into the shipping "
            "binary (convention 15).\n"
            "Fix: run `just apple-ffi` (production) LAST, after any e2e run, and retry."
        )
    return None


def run(*argv: str, cwd: Path | None = None, check: bool = True,
        timeout: float = 900) -> subprocess.CompletedProcess:
    """Run a tool; on `check`, fail the test with the command and its output."""
    try:
        proc = subprocess.run(
            argv, cwd=cwd, capture_output=True, text=True, timeout=timeout,
        )
    except subprocess.TimeoutExpired as exc:
        pytest.fail(
            f"command timed out after {timeout}s: {' '.join(argv)}\n"
            f"output so far:\n{(exc.output or b'')!r}",
            pytrace=False,
        )
    if check and proc.returncode != 0:
        pytest.fail(
            f"command failed (rc={proc.returncode}): {' '.join(argv)}\n"
            f"stdout:\n{proc.stdout[-2000:]}\nstderr:\n{proc.stderr[-2000:]}",
            pytrace=False,
        )
    return proc


def make_unsigned_dmg(app_bundle: Path, dmg_path: Path) -> Path:
    """Package `app_bundle` into a UDZO disk image — the `mac-dmg` shape, unsigned.

    Deliberately NOT `just mac-dmg`: that recipe hard-requires four
    `FAUNA_*` signing/notarization secrets and then talks to Apple's notary
    service over the network. Those are the credentialed half, and they stay a
    user-gated proof.

    What is left when you subtract them is still the whole packaging round trip —
    `hdiutil` compression, the mounted filesystem, the copy-out, and the quarantine
    an install carries — and that half is testable headlessly, right now, on every
    build. Splitting it this way is the point: the human is left only with the
    inch that genuinely needs Apple credentials, instead of the whole mile.
    """
    dmg_path.unlink(missing_ok=True)
    run("hdiutil", "create",
        "-volname", "Fauna",
        "-srcfolder", str(app_bundle),
        "-ov", "-format", "UDZO",
        "-quiet",
        str(dmg_path))
    return dmg_path


@contextlib.contextmanager
def mounted(dmg_path: Path) -> Iterator[Path]:
    """Attach `dmg_path` read-only and yield its mount point; always detach.

    `-nobrowse` keeps the volume out of Finder's sidebar (this runs on a real
    desktop session that a human may also be using), and the detach is in a
    `finally` because a leaked mount is machine-global state — exactly the class
    of ambient leakage convention 10 bans, and it would strand the next run's
    `hdiutil create -ov` on a busy volume.
    """
    out = run("hdiutil", "attach", str(dmg_path),
              "-nobrowse", "-readonly", "-mountrandom", "/tmp").stdout
    mount = None
    for line in out.splitlines():
        parts = line.split("\t")
        if len(parts) >= 3 and parts[-1].strip().startswith("/"):
            mount = Path(parts[-1].strip())
            break
    if mount is None:
        pytest.fail(f"could not parse a mount point out of `hdiutil attach`:\n{out}",
                    pytrace=False)
    try:
        yield mount
    finally:
        # `-force` because a lingering mds/Spotlight probe on a freshly mounted
        # volume routinely holds it busy for a beat; a leaked mount is worse than
        # a forced unmount of a read-only image.
        subprocess.run(["hdiutil", "detach", str(mount), "-force", "-quiet"],
                       capture_output=True, text=True)


def set_quarantine(path: Path) -> None:
    """Mark `path` as downloaded, the way a browser or a mounted DMG does."""
    run("xattr", "-w", QUARANTINE_XATTR, QUARANTINE_DOWNLOAD_VALUE, str(path))


def read_quarantine(path: Path) -> str | None:
    """The quarantine xattr on `path`, or None when it carries none."""
    proc = run("xattr", "-p", QUARANTINE_XATTR, str(path), check=False)
    return proc.stdout.strip() if proc.returncode == 0 else None


def approve_quarantined_copy(path: Path) -> None:
    """Clear the quarantine on `path` — the user's "Open Anyway", modelled.

    **Never call this on the copy a test asserts is quarantined.** It is for a
    private, throwaway *launch* copy, and it stands in for a step a real user
    of an un-notarized build has to take with their own hands.

    Why it has to exist at all, measured on macOS 2026-08-27 (Darwin 25.6.0): exec'ing a quarantined bundle's inner executable
    **does** invoke Gatekeeper — `com.apple.syspolicy.exec` logs
    `GK performScan` → `GK evaluateScanResult` (with a TLS round trip to look up
    a notarization ticket), and `CoreServicesUIAgent` then logs
    `present code-evaluation prompt`: the blocking "Apple could not verify …"
    dialog. The process stays alive and silent behind that modal forever, so a
    headless run does not fail — it *hangs* until its budget expires. Raising
    the budget cannot help: a modal nobody dismisses never resolves. The suite
    already contains the proof this is the verdict — its own
    `test_an_ad_hoc_signed_build_is_refused_by_gatekeeper` asserts
    `spctl --assess --type execute` REJECTS the same artifact.

    So the honest thing is to make the approval explicit rather than to widen a
    timeout or to quietly drop the stamp. Callers must first assert that the
    build really is in the un-notarized regime this stands in for (see
    `assert_gatekeeper_refuses`), which is what makes the substitution
    **self-retiring**: the day a notarized build lands, that assertion fails and
    this call has to go, leaving the test launching a quarantined download with
    no approval at all — which is the real outcome, and a user gate until then
    (`installers/macos.md` § Distribution Channels).
    """
    run("xattr", "-d", QUARANTINE_XATTR, "-r", str(path), check=False)


def assert_gatekeeper_refuses(path: Path) -> str:
    """Assert Gatekeeper rejects `path`, and return what it said.

    The regime check behind `approve_quarantined_copy`, and the assertion behind
    `test_an_ad_hoc_signed_build_is_refused_by_gatekeeper` — one helper so the
    two can never drift into disagreeing about what "un-notarized" means.
    """
    proc = run("spctl", "--assess", "--type", "execute", "--verbose=2",
               str(path), check=False)
    combined = proc.stdout + proc.stderr
    assert proc.returncode != 0, (
        f"`spctl --assess` ACCEPTED this build:\n{combined}\n"
        f"Either this box has Gatekeeper disabled (in which case every assessment "
        f"assertion in the DMG module is vacuous — fix the box, do not relax the "
        f"test), or the build is now signed with a real identity and notarized. If "
        f"it is the latter, this is the good failure: retire the "
        f"`approve_quarantined_copy` stand-in, let the launch test run a quarantined "
        f"download with NO approval, and flip this assertion to `accepted`."
    )
    assert "rejected" in combined or "denied" in combined, (
        f"`spctl --assess` failed for a reason other than rejection:\n{combined}"
    )
    return combined
