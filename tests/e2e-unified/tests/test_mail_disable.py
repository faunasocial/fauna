"""E2E for Disable mail on the mail-settings page (mail-settings.md § Disable mail).

Flipping `mail-settings-enabled-toggle` off opens the destructive
`mail-settings-disable-confirm` dialog; confirming with
`mail-settings-disable-confirm-button` dispatches MailSettingsAction::DisableMail.
**Turning mail off stops mail, and only mail** (`mail-credentials.md` § Trigger
taxonomy, the Disable-mail row): the nest's per-actor mail flag goes off (IMAP
refuses), every submission token is revoked (no sending), and the credentials
are KEPT — the calendar, contacts and file apps signing in with them keep
working, and the page keeps showing them (§ Credential-management
reachability). Turning mail back on restores sending and reading under the same
credentials, with no new set-up. It deliberately does NOT touch the Admin-scoped
deployment-wide `set_mail_enabled` flag (one user disabling their own mailbox
must not tear down the box's mail subsystem).

tier_3: a real client driver drives the real `fauna-nest` binary end-to-end
(glue → shared MailSettingsMachine → WS-RPC → nest DB → render). The shared
crate's enable→disable→re-enable round-trip (the kept credentials, the nest flag
and the "no set_mail_enabled(false)" assertion) is unit-tested in
`libs/fauna-client-mail-settings/tests/state_machine.rs`; the nest's IMAP refusal
and the CalDAV handler serving on are nest unit tests; that the calendar app
stays signed in is `test_mail_settings_controls.py`'s outcome over a dedicated
mail nest.

A DEDICATED actor (``dedicated_actor_app``), never the shared session
``test_user``: while this test's mail is off the actor's mail stops being
served, which no later module on ``test_user`` expects.
"""

import pytest

pytestmark = [pytest.mark.tier_3, pytest.mark.linux, pytest.mark.web, pytest.mark.windows, pytest.mark.macos, pytest.mark.ios, pytest.mark.tui, pytest.mark.android]
# android: built with exact ID matches (`MailSettingsScreen.kt`'s disable-confirm
# dialog) and Compose-content-tested (`MailSettingsContentTest.kt::disableMail
# RequiresConfirm` / `disableMailCancelLeavesMailEnabled`); its tier_3 run stays
# host-emulator-gated like every other android e2e test.
# macos re-added 2026-06-14: the disable-confirm
# overlay reveal was an accessibility-clobber bug — the bare container id
# `mail-settings-disable-confirm` on the overlay VStack overwrote the child
# `mail-settings-disable-confirm-button` id, so `disable_mail()` timed out waiting
# on the (clobbered) button. Fixed by adding `.accessibilityElement(children:
# .contain)` to the VStack (MailSettingsView.swift). Re-verified on the next
# single-runner apple e2e pass. ios marker still pending (needs an iOS-sim run).


@pytest.mark.feature("turn-on-mail")
def test_disable_mail_keeps_every_credential_via_confirm_dialog(dedicated_actor_app):
    app, _actor = dedicated_actor_app
    app.mail_settings.navigate()
    app.mail_settings.ensure_mail_enabled()

    # A second credential, so the test proves EVERY row is kept, not just one.
    app.mail_settings.add_credential("Phone")
    assert app.mail_settings.wait_for_credential_count_at_least(2, timeout=app.mail_settings.ENABLE_SETTLE_S), (
        "expected ≥2 credentials before disabling; "
        f"error: {app.mail_settings.page_error_text(timeout=2.0)!r}"
    )
    before = app.mail_settings.credential_count()

    # Flip the enabled-toggle off → confirmation dialog → Disable; the page
    # re-renders disabled once the round-trip settles.
    app.mail_settings.disable_mail()
    assert app.mail_settings.wait_for_enabled_toggle_state("off"), (
        "mail must read off after the Disable; "
        f"error: {app.mail_settings.page_error_text(timeout=2.0)!r}"
    )

    assert app.mail_settings.credential_count() == before, (
        "turning mail off keeps every credential; "
        f"error: {app.mail_settings.page_error_text(timeout=2.0)!r}"
    )
    # The kept credentials stay manageable: the credential section — and the
    # serve-here toggle it carries — still renders.
    assert app.mail_settings.serve_here_visible(), (
        "the credential section stays while a kept credential exists"
    )

    # Turning mail back on reuses the kept credentials — no new one is minted.
    app.mail_settings.enable_mail("Default")
    assert app.mail_settings.wait_for_enabled_toggle_state("on", timeout=app.mail_settings.ENABLE_SETTLE_S), (
        "mail must read on after the re-enable; "
        f"error: {app.mail_settings.page_error_text(timeout=2.0)!r}"
    )
    assert app.mail_settings.credential_count() == before, (
        "the re-enable reuses the kept credentials; "
        f"error: {app.mail_settings.page_error_text(timeout=2.0)!r}"
    )
