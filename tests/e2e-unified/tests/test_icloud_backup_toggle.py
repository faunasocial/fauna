"""iCloud Keychain backup toggle (apple-only) — Settings → Account.

Spec: `docs/goal/architecture/apps/ios.md` § Credential Storage (ratified 2026-07-10,
landed 2026-07-19). Default OFF = device-bound: the identity secret is stored
`kSecAttrAccessibleAfterFirstUnlockThisDeviceOnly` and never silently migrates via iCloud
Keychain or an encrypted-device restore. Opting in rewrites the keychain items
`synchronizable` + `AfterFirstUnlock`.

The real SecItem accessibility class is deliberately NOT observable here: apple e2e runs the
store in-memory (`apple-e2e-automation.md`), and an unsigned process can't even create a
`synchronizable` item — that attribute is verified manually on device, and the class choice
is pinned by the FaunaKit unit tests (`KeychainAccessibilityTests`). What this suite guards
is the e2e-only property the unit tests can't reach: that the toggle is actually
**registered and hittable** in the real UI (the recurring apple regression — a bare
`.accessibilityIdentifier` is invisible to the in-process driver) and that its state
round-trips through the view model.

Apple-only: no other app has this toggle (each platform's secure store has its own iCloud
posture), so the suite is scoped to macos + ios.
"""

import pytest

# tier2 = extended-journey breadth (a Settings feature, like test_identity_export.py);
# tier_3 = full-stack depth (`logged_in_app` spins a real nest binary and a real client).
#
# ⚠ NO `tui`/other-app markers, and NOT a silent omission: the
# module docstring above states it — iCloud Keychain is an apple-only secure-store
# posture, no other platform has this toggle. Genuinely apple-only; no tui leg exists
# to add.
pytestmark = [
    pytest.mark.tier2,
    pytest.mark.tier_3,
    pytest.mark.macos,
    pytest.mark.ios,
]


def test_icloud_backup_toggle_defaults_off_and_round_trips(logged_in_app):
    """The toggle renders, defaults to device-bound (OFF), and flips both ways."""
    app = logged_in_app
    app.settings.open_icloud_backup()

    # Default: device-bound. The identity secret must never be set up to leave the device
    # without the user explicitly asking.
    assert app.settings.icloud_backup_state() == "off", (
        "iCloud backup must default OFF (device-bound): "
        f"{app.driver.diagnose('settings-icloud-backup-toggle')}"
    )

    # Opt in → reads ON (the VM re-reads the persisted preference, so this proves the flip
    # actually landed, not just an optimistic UI guess).
    app.settings.set_icloud_backup(True)
    assert app.settings.icloud_backup_state() == "on", (
        "toggling should opt the identity into iCloud Keychain backup: "
        f"{app.driver.diagnose('settings-icloud-backup-toggle')}"
    )

    # Opt back out → device-bound again (the toggle is symmetric).
    app.settings.set_icloud_backup(False)
    assert app.settings.icloud_backup_state() == "off", (
        "toggling off should return the identity to device-bound: "
        f"{app.driver.diagnose('settings-icloud-backup-toggle')}"
    )
