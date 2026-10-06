"""The macOS photo-library venue's ONE human step — the Photos grant — in under a minute.

Run by a person at the macOS VM's screen: `just mac-photos-e2e-grant`. Everything
that needs no human happens first (preflight, finding the build and the signing
identity); only then does it print the "input needed now" line, and the two dialogs
follow within seconds of each other:

1. the keychain asking whether `codesign` may use the Apple Development key — only
   when the staged bundle has to be (re)signed, i.e. after a rebuild; answer with
   the login password and **Always Allow** so later rebuilds sign unattended;
2. Photos access for "Fauna E2E Photos" — **Allow**. Given once: TCC keys it on the
   bundle's designated requirement, which survives rebuilds.

No nest, no pytest, no e2e slot queue: those are what made a pytest-driven grant
wait an unpredictable time before asking for anything. Already granted, it answers
in seconds with no dialog at all. Mechanism and rationale:
`tests/e2e-unified/drivers/macos.py` § the photo-library venue; owner
`docs/goal/architecture/e2e-conventions.md` convention 12 → *The macOS arm*.
"""
from __future__ import annotations

import subprocess
import sys
from pathlib import Path

E2E = Path(__file__).resolve().parents[1]           # tests/e2e-unified
REPO = E2E.parents[1]
sys.path.insert(0, str(E2E))

from drivers import macos  # noqa: E402

BINARY = REPO / "apps/fauna-apple/.build/arm64-apple-macosx/debug/FaunaMacOS"
#: How long the Photos dialog may stay unanswered once it is on screen.
ANSWER_WAIT_S = 300


def _refuse(msg: str) -> int:
    print(f"mac-photos-e2e-grant: {msg}", file=sys.stderr)
    return 1


def main() -> int:
    # --- Preflight: nothing here needs a human. ---
    if subprocess.run(["sysctl", "-n", "kern.hv_vmm_present"], capture_output=True,
                      text=True).stdout.strip() != "1":
        return _refuse("this Mac is not a VM; the venue is approved only for the dev "
                       "VM's own disposable photo library")
    if subprocess.run(["launchctl", "managername"], capture_output=True,
                      text=True).stdout.strip() != "Aqua":
        return _refuse("not inside the logged-in GUI session; run this from a Terminal "
                       "window on the Mac itself")
    if not BINARY.exists():
        return _refuse(f"no macOS debug build at {BINARY} — run `just mac-debug` first")
    try:
        macos.apple_development_identity()
    except RuntimeError as exc:
        return _refuse(str(exc))

    print("\n=== Ready. INPUT NEEDED NOW — up to two dialogs in the next ~30 seconds: ===")
    print("  1. Keychain (only after a rebuild): 'codesign wants to use key' — type the")
    print("     login password and click 'Always Allow'.")
    print("  2. Photos: '\"Fauna E2E Photos\" would like to access your photos' — 'Allow'.")
    print("  (A notifications prompt for the same app may also appear; either answer.)\n",
          flush=True)

    driver = macos.MacosInProcessDriver()
    try:
        driver.launch({"app_path": str(BINARY), "launch_mode": "photo-library",
                       "environment": {}})
        answer = driver.request_photos_access(timeout=ANSWER_WAIT_S)
    finally:
        driver.teardown()

    if answer in ("authorized", "limited"):
        print(f"Photos access for {macos.PHOTO_LIBRARY_BUNDLE_ID}: {answer}. Done — "
              "`just e2e-macos-photo-library-test` now runs unattended.")
        return 0
    return _refuse(
        f"Photos access is {answer!r}. If an earlier prompt was refused, re-enable "
        "'Fauna E2E Photos' under System Settings > Privacy & Security > Photos, "
        "then run this again.")


if __name__ == "__main__":
    sys.exit(main())
