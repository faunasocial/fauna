"""Live, read-only: ``Fauna-iOS/Resources/Info.plist`` agrees with the iOS app's
export-compliance record in App Store Connect.

**Both directions.** With ``ITSAppUsesNonExemptEncryption`` ``false`` (since
2026-11-06: the questionnaire needed no documents, so App Store Connect issued no
code), the plist pins no code and App Store Connect holds no standing declaration
with one. With the key ``true``, the pin equals the newest standing declaration's
code — the reading below.

App Store Connect's upload validation refuses a bundle that declares non-exempt
encryption unless its ``ITSEncryptionExportComplianceCode`` matches the app's
export-compliance record (``docs/goal/architecture/installers/ios.md`` § Upload
mechanics → *Export compliance*, measured 2026-11-01). A missing or stale pin is
otherwise found only at upload time, after the train's public commit exists.
This witness reads the record from the authority — ``just ios-export-compliance``,
App Store Connect's API through the App Store Connect API key — and holds the pin
to it before any upload.

**What it reads.** The newest declaration that still stands (not rejected,
invalid or expired) and carries a ``codeValue``. Its absence is the environment,
never a red: the questionnaire is the organisation representative's own
attestation (``docs/goal/architecture/export-compliance.md``), so until they have
answered it there is nothing for the pin to match.

**What skips.** No App Store Connect key on this box (exit 3), or App Store
Connect unreachable — both environment. A refusal by App Store Connect itself
(HTTP 401/403, a key revoked or a role narrowed) FAILS, carrying the reply.

tier_4: the subject is the live store record a shipped build is validated
against (``testing.md`` § The four-tier taxonomy). macOS only — the key lives on
the macOS development VM, and the reader is a Swift script.
"""

from __future__ import annotations

import json
import plistlib
import subprocess
from pathlib import Path

import pytest

from common import get_repo_root
from helpers.app_surface import skip_environment

pytestmark = [pytest.mark.tier_4, pytest.mark.host_platform("macos"), pytest.mark.timeout(600)]

INFO_PLIST = Path("apps/fauna-apple/Fauna-iOS/Resources/Info.plist")
PIN_KEY = "ITSEncryptionExportComplianceCode"
NON_EXEMPT_KEY = "ITSAppUsesNonExemptEncryption"
#: Declaration states that no longer stand for the app.
DEAD_STATES = {"REJECTED", "INVALID", "EXPIRED"}
#: The reader's exit codes.
EXIT_REFUSED, EXIT_NO_CREDENTIAL, EXIT_NO_APP = 2, 3, 4
RECIPE = "ios-export-compliance"


def _read_declarations(root: Path) -> dict:
    shown = subprocess.run(["just", "--show", RECIPE], cwd=root, capture_output=True, text=True, timeout=60)
    if shown.returncode != 0:
        skip_environment(f"this tree has no `just {RECIPE}` recipe (the App Store Connect reader is maintainer tooling)")
    proc = subprocess.run(
        ["just", RECIPE],
        cwd=root,
        capture_output=True,
        text=True,
        timeout=540,
    )
    if proc.returncode == EXIT_NO_CREDENTIAL:
        skip_environment(f"no App Store Connect API key on this box — {proc.stderr.strip()}")
    if proc.returncode == EXIT_REFUSED and "request failed:" in proc.stderr:
        skip_environment(f"App Store Connect unreachable — {proc.stderr.strip()}")
    if proc.returncode != 0:
        pytest.fail(
            f"`just ios-export-compliance` exited {proc.returncode}:\n{proc.stderr.strip()}",
            pytrace=False,
        )
    return json.loads(proc.stdout)


def _standing_code(declarations: list[dict]) -> dict | None:
    """The newest standing declaration with a code (the reader sorts newest first)."""
    for row in declarations:
        if row.get("platform") not in (None, "IOS"):
            continue
        if row.get("appEncryptionDeclarationState") in DEAD_STATES:
            continue
        if row.get("codeValue"):
            return row
    return None


def test_info_plist_pins_the_app_store_connect_code():
    root = Path(get_repo_root())
    with (root / INFO_PLIST).open("rb") as fh:
        plist = plistlib.load(fh)
    reading = _read_declarations(root)
    declarations = reading.get("declarations", [])
    current = _standing_code(declarations)
    pinned = plist.get(PIN_KEY)
    if plist.get(NON_EXEMPT_KEY) is False:
        # The key states that App Store Connect needs no documents and so issues
        # no code (export-compliance.md § What each store form answers): hold
        # both halves of that statement against the record.
        assert pinned is None, (
            f"{INFO_PLIST} declares {NON_EXEMPT_KEY} = false but still pins {PIN_KEY} = {pinned!r}; "
            f"a code belongs only with a `true` key"
        )
        assert current is None, (
            f"{INFO_PLIST} declares {NON_EXEMPT_KEY} = false, but App Store Connect holds a standing "
            f"declaration {current.get('id')} (state {current.get('appEncryptionDeclarationState')}, "
            f"created {current.get('createdDate')}) with code {current.get('codeValue')!r}: the record "
            f"changed — settle with the representative whether the key becomes `true` with that code"
        )
        return
    if current is None:
        skip_environment(
            f"App Store Connect holds no standing export-compliance declaration with a code for "
            f"{reading['app']['bundleId']} ({len(declarations)} declaration(s) in all): the "
            f"representative has not answered the encryption questionnaire yet"
        )
    code = current["codeValue"]
    assert pinned == code, (
        f"{INFO_PLIST} pins {PIN_KEY} = {pinned!r}, but App Store Connect's standing declaration "
        f"{current.get('id')} (state {current.get('appEncryptionDeclarationState')}, created "
        f"{current.get('createdDate')}) carries code {code!r}; upload validation refuses the bundle "
        f"until the pin is {code!r}"
    )
