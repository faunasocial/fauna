"""tier_3 e2e: screen-capture suppression on the minted-credential reveals, read
back off the running app.

Goal doc: ``docs/goal/architecture/security.md`` § On-screen secret exposure
(screen capture). Rule 2 — suppress capture while, and only while, a minted
credential is revealed: on while revealed, off on hide or navigate-away. Rule 1 —
never on a root-secret surface (the recovery kit here). Rule 3 — linux, web and
tui have no per-window primitive and are declared absences, so no marker for them
here.

What this adds over the tier_1 source pin (``test_screen_capture_posture.py``):
that pin proves each reveal CALLS its app's suppression; this proves the call
takes effect while the secret shows and — the failure users actually feel —
lets go afterwards. The read is the ``capture_posture`` state observable
(``helpers/capture_posture.py``): on macOS the live hold count AND the window's
real ``sharingType``; on windows the guard's live hold count AND the main
window's real display affinity (``GetWindowDisplayAffinity``); on iOS the count
of reveals with the blanking armed. iOS
has no platform bit to read back and its control is partial by construction —
it blanks the value while a recording is live or the app leaves the foreground
and cannot stop a still screenshot — so on iOS these tests witness that the
partial control is armed and released, never that screenshots are blocked.

Why the apple apps lead: the lead app tui is a declared absence here, and so is
web, the lead for a journey tui cannot carry (``testing.md`` § Default app and
nest mode); the android and windows legs followed, each publishing the same
observable. windows renders the recovery kit (Settings → Account, and the
onboarding kit step), so rule 1's test runs there too.
"""
from __future__ import annotations

import uuid

import pytest

from actions.connected_apps import ConnectedAppsActions
from helpers.budgets import RPC_ROUNDTRIP_S
from helpers.capture_posture import (
    require_capture_posture,
    skip_if_unavailable,
    wait_for_posture,
)

pytestmark = [pytest.mark.tier_3, pytest.mark.macos, pytest.mark.ios, pytest.mark.windows]

_BUDGET_S = RPC_ROUNDTRIP_S


def _assert_baseline_released(app, where: str) -> None:
    """No hold before a reveal — a hold leaked by an earlier test would make
    every later "it is on" assertion pass vacuously."""
    posture = require_capture_posture(app.driver)
    assert posture.released, f"{where}: capture is already suppressed before any reveal — {posture!r}"


def _await_suppressed(app, what: str):
    # A system the app cannot suppress on (the posture's generic `unavailable`
    # arm — no app publishes it today) witnesses the no-harm half here and skips,
    # never passes.
    skip_if_unavailable(app.driver, require_capture_posture(app.driver))
    return wait_for_posture(app.driver, lambda p: p.suppressed, budget_s=_BUDGET_S, what=what)


def _await_released(app, what: str):
    return wait_for_posture(app.driver, lambda p: p.released, budget_s=_BUDGET_S, what=what)


@pytest.mark.android
@pytest.mark.feature("turn-on-mail")
def test_a_revealed_mail_app_password_suppresses_capture_until_hidden_or_left(logged_in_app):
    """turn-on-mail 28: revealing a mail app password on its Connected apps row
    takes a suppression hold; Hide drops it; revealing again and navigating away
    drops it too."""
    app = logged_in_app
    mail = app.mail_settings
    ca: ConnectedAppsActions = app.connected_apps
    mail.navigate()
    mail.ensure_mail_enabled()
    name = f"Capture {uuid.uuid4().hex[:4]}"
    chosen = "CapturePosture01Aa"
    mail.add_credential_plain(name, chosen)

    row = ca.wait_for_item_containing(name)
    assert row is not None, f"the new password must be a roster row. error: {ca.current_error_text()!r}"
    n = ca.mail_row_items().index(row)
    _assert_baseline_released(app, "Connected apps, secret hidden")

    assert ca.reveal_mail_secret(n) == chosen, (
        f"reveal must show the password typed. error: {ca.current_error_text()!r}"
    )
    _await_suppressed(app, "mail app password revealed")

    # Hide: the same control toggles the reveal off.
    app.driver.click("connected-apps-item-reveal-secret", n)
    _await_released(app, "mail app password hidden")

    assert ca.reveal_mail_secret(n) == chosen
    _await_suppressed(app, "mail app password revealed again")
    app.settings.navigate()
    _await_released(app, "navigated away from the revealed mail app password")


@pytest.mark.android
@pytest.mark.feature("atproto")
def test_a_revealed_bluesky_app_password_suppresses_capture_until_left(atproto_hosted_logged_in_app):
    """atproto 52: a minted Bluesky app password is revealed inline and holds a
    suppression; its reveal has no hide, so navigating away is the release."""
    app = atproto_hosted_logged_in_app
    bs = app.atproto_settings
    bs.ensure_hosted_full()
    assert bs.wait_for_fullpds_visible(), (
        f"the full-PDS panel must be present to mint. error: {bs.page_error_text()!r}"
    )
    _assert_baseline_released(app, "Bluesky page, nothing revealed")
    before = bs.credential_count()

    bs.mint_credential()
    assert bs.wait_for_credential_count(before + 1), (
        f"minting should add one credential row. error: {bs.page_error_text()!r}"
    )
    secret = bs.reveal_credential_secret(before)
    assert secret and secret != "Reveal", f"mint must reveal the secret, got {secret!r}"
    _await_suppressed(app, "Bluesky app password revealed")

    app.settings.navigate()
    _await_released(app, "navigated away from the revealed Bluesky app password")


@pytest.mark.android
@pytest.mark.feature("nostr")
def test_a_revealed_signer_connect_string_suppresses_capture_until_left(logged_in_app):
    """nostr 28: the one-time bunker connect string holds a suppression while it
    shows, and navigating away releases it."""
    app = logged_in_app
    nostr = app.nostr
    nostr.navigate()
    assert nostr.is_page_visible(), f"nostr page not reachable. error: {app.error_text()!r}"
    assert nostr.ensure_linked(), f"could not link. error: {nostr.page_error_text()!r}"
    assert nostr.wait_for_connect_section(), "the signer-invite section must render"
    _assert_baseline_released(app, "Nostr page, no invite minted")

    nostr.connect_app()
    assert nostr.wait_for_connect_string(), (
        f"minting an invite should reveal the connect string. error: {nostr.page_error_text()!r}"
    )
    assert nostr.connect_string().startswith("bunker://")
    _await_suppressed(app, "signer connect string revealed")

    app.settings.navigate()
    _await_released(app, "navigated away from the revealed connect string")


# No feature marker: rule 1 is a goal-doc property, not a catalog outcome, and
# a marked test must be cited by a page (features-lint rule 3).
@pytest.mark.android
def test_the_recovery_kit_secret_never_suppresses_capture(ungranted_app):
    """Rule 1: the recovery kit is the copy that saves the account, so a user may
    screenshot it — the app takes no hold and the window stays capturable while
    it shows. ``ungranted_app`` because the ceremony writes the account's
    registration chain (``test_recovery_kit_settings.py`` says why)."""
    app = ungranted_app
    app.settings.navigate()
    app.settings.open_recovery_kit()
    _assert_baseline_released(app, "Recovery kit section, before create")

    app.settings.create_recovery_kit()
    app.wait_for("recovery-kit-secret-display", timeout=30.0)
    assert app.settings.recovery_kit_secret(), (
        f"the kit secret must be on screen for this to mean anything. error: {app.error_text()!r}"
    )
    posture = require_capture_posture(app.driver)
    assert posture.released, (
        "the recovery kit secret is a root secret (security.md rule 1) and must stay "
        f"capturable, but the app suppresses capture while it shows — {posture!r}"
    )
