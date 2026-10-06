"""Identity export — the Settings/Account QR a second device scans to import.

Spec: `docs/goal/ui/settings.md` § Identity export. The counterpart of onboarding's
`identity_import` step (`docs/goal/behavior/onboarding.md` § 1 Identity), whose error path
is covered by `test_onboarding_errors.py::test_invalid_secret_shows_i18n_error`.

The load-bearing contract is the **reveal gate**: the description and the toggle are always
visible, but the warning and the QR render *only after the user presses show*. Hiding is not
a security control — it exists so the identity secret is never on screen by accident when a
user opens Settings (e.g. while sharing a screen) — but a regression that renders the QR
unconditionally would silently put the full Ed25519 secret in front of every screen-share,
so it is worth a test on every app.

Per-app status:
- linux  ✅ GTK DrawingArea (the reference impl)
- web    ✅ inline SVG built from the shared boolean grid
- android ✅ Compose Canvas (the marker is declaratory: the android driver is only
  available where an emulator runs, so this suite exercises the section there only
  under `--client android`)
- windows ✅ XAML Canvas + Rectangle per dark module (no Win2D/NuGet QR dependency)
- macos / ios ✅ SwiftUI Canvas (one shared FaunaKit `QrCodeView`, both apps) — landed
  2026-07-12, replacing CoreImage's `CIFilter.qrCodeGenerator` (the last platform QR
  encoder in the tree) and its NSImage/UIImage fork.
- tui    ✅ landed its own copy independently 2026-07-15, predating the 2026-07-19
  fleet-wide parity flip

All seven apps draw the SAME `fauna_core::qr_matrix` boolean grid: no client links a
platform QR library (priorities #1/#2).
"""

import pytest

from i18n.strings import S

# tier2 = extended-journey breadth (a Settings feature, like test_settings.py), tier_3 =
# full-stack depth (`logged_in_app` spins a real nest binary and a real client).
pytestmark = [
    pytest.mark.tier2,
    pytest.mark.tier_3,
    pytest.mark.linux,
    pytest.mark.web,
    pytest.mark.android,
    pytest.mark.windows,
    pytest.mark.macos,
    pytest.mark.ios,
    pytest.mark.tui,
]


@pytest.mark.feature("identity-on-another-device")
def test_identity_qr_is_hidden_until_the_user_asks_for_it(logged_in_app):
    """The reveal gate: description + toggle always visible; warning + QR only after show."""
    app = logged_in_app
    app.settings.open_identity_export()

    # Always visible — the section explains itself whether or not the QR is shown.
    assert app.is_visible("identity-export-description"), (
        "the identity-export description should always render: "
        f"{app.driver.diagnose('identity-export-description')}"
    )

    # The secret must NOT be on screen just because the user opened Settings.
    assert not app.settings.identity_qr_shown(), (
        "the identity QR must stay hidden until the user presses show: "
        f"{app.driver.diagnose('identity-export-qr')}"
    )
    assert app.is_absent("identity-export-warning"), (
        "the warning renders only alongside the QR, never before it: "
        f"{app.driver.diagnose('identity-export-warning')}"
    )
    assert app.settings.identity_export_toggle_label() == S.settings.identity_export.show_qr, (
        "collapsed, the toggle should offer to SHOW the QR; got "
        f"{app.settings.identity_export_toggle_label()!r}"
    )


@pytest.mark.feature("identity-on-another-device")
def test_identity_qr_and_its_warning_appear_together_on_show(logged_in_app):
    """Pressing show reveals the QR *and* the warning, and flips the toggle's label.

    The warning is not optional: whoever scans the code gains the identity, so it must
    render adjacent to the QR — never the QR alone (settings.md § Identity export, "Risk").
    """
    app = logged_in_app
    app.settings.open_identity_export()
    app.settings.toggle_identity_qr()
    app.driver.wait_for("identity-export-qr", timeout=10.0)

    assert app.settings.identity_qr_shown(), (
        "pressing show should render the QR: "
        f"{app.driver.diagnose('identity-export-qr')}"
    )
    assert app.is_visible("identity-export-warning"), (
        "the QR must never render without its warning — whoever scans it gains the "
        f"identity: {app.driver.diagnose('identity-export-warning')}"
    )
    assert app.settings.identity_export_toggle_label() == S.settings.identity_export.hide_qr, (
        "revealed, the SAME toggle should now offer to HIDE the QR (one button, label "
        f"flips); got {app.settings.identity_export_toggle_label()!r}"
    )


@pytest.mark.feature("identity-on-another-device")
def test_identity_qr_hides_again_on_second_press(logged_in_app):
    """The toggle is symmetric — pressing it again puts the secret back off screen."""
    app = logged_in_app
    app.settings.open_identity_export()
    app.settings.toggle_identity_qr()
    app.driver.wait_for("identity-export-qr", timeout=10.0)
    assert app.settings.identity_qr_shown(), "precondition: the QR should be shown"

    app.settings.toggle_identity_qr()

    assert not app.settings.identity_qr_shown(), (
        "pressing hide should take the QR back off screen: "
        f"{app.driver.diagnose('identity-export-qr')}"
    )
    assert app.is_absent("identity-export-warning"), (
        "hiding the QR should retract its warning too: "
        f"{app.driver.diagnose('identity-export-warning')}"
    )
    assert app.settings.identity_export_toggle_label() == S.settings.identity_export.show_qr, (
        "collapsed again, the toggle should offer to SHOW; got "
        f"{app.settings.identity_export_toggle_label()!r}"
    )
