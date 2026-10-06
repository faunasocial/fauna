"""E2E for the per-user IMAP/CalDAV-serving toggle on the mail-settings page.

Target state: docs/goal/ui/mail-settings.md § Local IMAP/CalDAV-serving toggle +
docs/goal/architecture/nest/deployment-home-with-public-relay.md § MUA reach.

The toggle is user-set (default on) and backed by the per-actor nest flag
(`fauna.bridges.set_mail_serving_enabled` / `get_mail_serving_enabled`) through
the shared `MailSettingsMachine`. The dispatch is **non-optimistic**: the
machine flips `serving_enabled` (and the rendered `state` attr) only after the
nest confirms the write — so "the toggle reads off after the gesture" is itself
the proof that the WS-RPC write against the real nest succeeded.

tier_3: a real client driver drives the real `fauna-nest` binary end-to-end
(glue → machine → WS-RPC → nest DB → render). Lead app = linux; web + android
+ windows have lifted it; the remaining clients (macos/ios) lift the same
`mail-settings-serve-here-toggle` ID over the same shared machine (route-3
hand-offs). The hydrate *read* path (`get_mail_serving_enabled`) is covered by
the shared crate's `state_machine.rs::hydrate_reads_serving_flag_from_nest`.
"""

import pytest

# NOT marked `ios`: this test's only path runs `ensure_mail_enabled`, which on iOS
# can't complete. ⚠ Refined N+38: apple's inline-reveal fix DID fix the
# iOS add-credential form registration (probe: name-input count=1), but submitting the
# enable form on iOS doesn't complete the enable (no credential row, no status flip,
# form doesn't dismiss, ≥40s, no error) — likely the iOS view not observing the
# snapshot the submit mutates. macOS runs the identical shared flow green.
# Same root as the 6 `test_mail_credentials.py` enable-dependent tests — re-add `ios`
# when apple completes the iOS enable path. macOS green.
#
# `tui` (M8 slice 5): the serve-here toggle rides the same shared machine's
# SetServingEnabled action + serving_enabled snapshot field; tui reads `state`
# via the uniform get_attr idiom, so the cross-app body runs unchanged.
# android: `MailSettingsScreen.kt` renders `mail-settings-serve-here-toggle`
# (Compose-content-tested: `serveHereToggleVisibleAndCheckedWhenEnabled` /
# `serveHereToggleHiddenWhenMailDisabled` / `serveHereToggleFlipDispatches`);
# its tier_3 run stays host-emulator-gated like every other android e2e test.
pytestmark = [pytest.mark.tier_3, pytest.mark.linux, pytest.mark.web, pytest.mark.windows, pytest.mark.macos, pytest.mark.ios, pytest.mark.tui, pytest.mark.android]


@pytest.mark.feature("turn-on-mail")
def test_serve_here_toggle_defaults_on_and_writes_through_to_nest(logged_in_app):
    app = logged_in_app
    app.mail_settings.navigate()
    # Idempotent: the tier_3 nest + actor are session-scoped, so a prior mail
    # test may already have enabled mail for this actor.
    app.mail_settings.ensure_mail_enabled()

    # The toggle is enabled-gated (nothing to serve until the user has a mailbox).
    assert app.mail_settings.serve_here_visible(), (
        "serve-here toggle should be visible once mail is enabled; "
        f"error: {app.mail_settings.page_error_text(timeout=2.0)!r}"
    )
    # Default on (deployment-home-with-public-relay.md § MUA reach: absent ⇒ on).
    assert app.mail_settings.wait_for_serve_here_state("on"), (
        f"serve-here should default on; got {app.mail_settings.serve_here_state()!r}; "
        f"error: {app.mail_settings.page_error_text(timeout=2.0)!r}"
    )

    # Turn serving OFF → SetServingEnabled{false} → set_mail_serving_enabled WS-RPC
    # → nest persists the per-actor flag. The `state` attr flips to "off" only
    # after that write returns ok (non-optimistic), so this asserts the real
    # write path, not a local UI flip.
    app.mail_settings.set_serve_here(False)
    assert app.mail_settings.wait_for_serve_here_state("off"), (
        "serve-here should read off after the gesture (nest write confirmed); "
        f"got {app.mail_settings.serve_here_state()!r}; "
        f"error: {app.mail_settings.page_error_text(timeout=2.0)!r}"
    )

    # Turn it back ON — proves the write-back path and leaves the shared
    # session-scoped actor as found (default on).
    app.mail_settings.set_serve_here(True)
    assert app.mail_settings.wait_for_serve_here_state("on"), (
        "serve-here should toggle back on; "
        f"got {app.mail_settings.serve_here_state()!r}; "
        f"error: {app.mail_settings.page_error_text(timeout=2.0)!r}"
    )
