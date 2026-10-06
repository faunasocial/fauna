"""E2E for Disable mail on the mail-settings page (mail-settings.md § Disable mail).

Flipping `mail-settings-enabled-toggle` off opens the destructive
`mail-settings-disable-confirm` dialog; confirming with
`mail-settings-disable-confirm-button` dispatches MailSettingsAction::DisableMail
= bulk soft-revoke **every** credential (one RevokeCredential per row) + clear
the `msek` in the `fauna.state.mail` plane over the real nest, returning the snapshot to
`enabled: false`. It deliberately does NOT touch the Admin-scoped deployment-wide
`set_mail_enabled` flag (one user disabling their own mailbox must not tear down
the box's mail subsystem).

tier_3: a real client driver drives the real `fauna-nest` binary end-to-end
(glue → shared MailSettingsMachine → WS-RPC → nest DB → render). Lead app =
linux; wired on **linux + web + windows** (the web overlay in
`apps/fauna-web/src/lib/components/MailSettingsSection.svelte`; the windows inline
overlay in `apps/fauna-windows/.../Controls/MailSettingsPanel.xaml{,.cs}`);
macos/ios/android lift the same dialog + DisableMail over the shared machine
(route-3 hand-offs). The shared crate's enable→disable→re-enable round-trip
(including the "no set_mail_enabled(false)" assertion) is unit-tested in
`libs/fauna-client-mail-settings/tests/state_machine.rs`.

⚠ A DEDICATED actor (``dedicated_actor_app``), never the shared session
``test_user``. The disable clears the MSEK and its grace generations by
design, so every record already in the actor's mailbox can never open again,
and the re-enable does not bring them back. On ``test_user`` that stranded
all the mail earlier modules had delivered, and every later module's relaunch
counted it on the conversations page's unopenable-mail floor for the rest of
the run.
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
def test_disable_mail_revokes_all_credentials_via_confirm_dialog(dedicated_actor_app):
    app, _actor = dedicated_actor_app
    app.mail_settings.navigate()
    app.mail_settings.ensure_mail_enabled()

    # Provision a second credential so disabling exercises the *bulk* revoke
    # (every row), not just a single-credential revoke.
    app.mail_settings.add_credential("Phone")
    assert app.mail_settings.wait_for_credential_count_at_least(2, timeout=12.0), (
        "expected ≥2 credentials before disabling; "
        f"error: {app.mail_settings.page_error_text(timeout=2.0)!r}"
    )
    # Serve-here is enabled-gated, so it's visible while mail is on.
    assert app.mail_settings.serve_here_visible(), (
        "serve-here toggle should be visible while mail is enabled"
    )

    # Flip the enabled-toggle off → confirmation dialog → Disable. DisableMail
    # revokes every credential and leaves the MSEK dormant on the account's mail
    # custody (present-wins — `mail-credentials.md` § MSEK lifecycle, *Disable
    # mail*); the page re-renders disabled once the round-trip settles.
    app.mail_settings.disable_mail()

    assert app.mail_settings.wait_for_credential_count(0, timeout=15.0), (
        "DisableMail should revoke every credential (0 rows remain); "
        f"error: {app.mail_settings.page_error_text(timeout=2.0)!r}"
    )
    # Disabled ⇒ the enabled-gated serve-here toggle disappears.
    assert not app.mail_settings.serve_here_visible(timeout=3.0), (
        "serve-here toggle should disappear once mail is disabled"
    )

    # Re-enable works — over the dormant MSEK with no live credential, EnableMail
    # mints a fresh credential under the SAME key, so mail sealed before the
    # disable stays openable.
    app.mail_settings.enable_mail("Default")
    assert app.mail_settings.wait_for_credential_count_at_least(1, timeout=15.0), (
        "re-enable should mint a fresh credential after disable; "
        f"error: {app.mail_settings.page_error_text(timeout=2.0)!r}"
    )
