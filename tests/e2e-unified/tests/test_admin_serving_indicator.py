"""E2E for the admin's read-only IMAP/CalDAV-serving audit indicator on the
consolidated `admin-users` hub.

Target state: docs/goal/behavior/admin.md § Users (the read-only per-user
indicator — never an admin control) + docs/goal/ui/mail-settings.md § Local
IMAP/CalDAV-serving toggle ("admin-visible read-only") + docs/goal/architecture/
nest/deployment-home-with-public-relay.md § MUA reach ("the admin sees it
read-only for audit") / § Done definition checkbox 3 (the admin-panel half).

The flag is **user-set, admin-read-only**: a per-actor nest flag the user flips
from their own mail-settings serve-here toggle, surfaced into the
`fauna.admin.users.list` projection as `AdminUser.mail_serving_enabled` (default
on) and rendered on each `user-row` as `admin-users-mail-serving-status`. This
test drives the full loop end-to-end through the real linux UI + real nest: the
admin (who is also a registered user) sees their own row default to "Serving
here", disables serving from their own mail-settings, and then sees the admin
hub reflect "Not serving" — read-only, no admin write. That on→off flip is the
proof the row renders the *projected field*, not a hardcoded label.

The wire-level on/off projection is independently proven by the nest crate test
`conformance_admin.rs::list_carries_mail_serving_audit_flag`.

tier_3: a real client driver drives the real `fauna-nest` binary end-to-end.
Lead app = linux; web (2026-06-03) and windows (2026-06-04) have lifted it;
the remaining clients (macos/ios/android) lift the same
`admin-users-mail-serving-status` ID over the same shared `AdminUser`
projection (route-3 hand-offs).
"""

import pytest

from helpers import budgets
from helpers.waiting import wait_until
from i18n.strings import S

# NOT marked `ios`: this test's `ensure_mail_enabled` rides the same iOS gap. ⚠ N+38:
# apple's inline-reveal fix fixed the iOS form registration but the
# enable-submit still doesn't complete on iOS. macOS green.
# `tui` added 2026-07-29 — reached independently by a marker sweep and by the tui
# admin build, which converged on the same line. Both halves were already built:
# the admin-side read (`admin/users.rs`, `admin-users-mail-serving-status`) and
# the user-side write (`settings/mail.rs`, `mail-settings-serve-here-toggle`);
# only this marker list predated the M8 tui admin build. Verified 1/1 PASSED
# `--app tui`.
pytestmark = [pytest.mark.tier_3, pytest.mark.linux, pytest.mark.web, pytest.mark.windows, pytest.mark.macos, pytest.mark.ios, pytest.mark.tui, pytest.mark.android]
# android: both halves are built with exact ID matches
# (`AdminUsersScreen.kt`'s `admin-users-mail-serving-status`,
# `MailSettingsScreen.kt`'s `mail-settings-serve-here-toggle`) and
# Compose-content-tested (`AdminUsersContentTest.kt::userRowRendersReadOnly
# ServingStatus`); the tier_3 run stays host-emulator-gated like every other
# android e2e test.
# macos GREEN 2026-06-14: the Settings sidebar-swap
# shell gives mail-settings its own on-screen pane, so the serve-here
# toggle the user-side half drives is now hittable, and the serve-here Switch maps
# its value to the "on"/"off" `state` contract — the two gaps the prior OFF note
# described are both resolved. Admin-side read-only status was already green.
# NOT marked `ios` (reverted N+37 — was a latent red): the test seeds a user with
# mail enabled via `ensure_mail_enabled`, which on iOS can't complete (the
# add-credential `.sheet` doesn't register in-process; same root as the
# `test_mail_credentials.py` 6). Re-add `ios` on apple's MSEK inline-reveal fix.


def _admin_row_index(admin_app, admin_hex: str) -> int:
    """Thin alias for the shared `admin.admin_row_index` (actions/admin.py), which
    owns the by-actor-id row lookup and the reason position-based identity is
    unsound (second-granular `created_at` ties)."""
    return admin_app.admin.admin_row_index(admin_hex)


def _wait_serving_status(app, admin_hex: str, want: str, why: str) -> None:
    """Wait for the admin's own row to read `want` (convention 14).

    The users list is re-fetched when the admin shell is entered and the rows from
    the last fetch stay on screen until the reply lands, so one read right after
    navigating can see the previous state. A row lookup that raises while the list
    re-renders is an observation, asked again."""
    seen: list[str] = []

    def status():
        try:
            got = app.admin.user_mail_serving_status(_admin_row_index(app, admin_hex))
        except (AssertionError, RuntimeError, TimeoutError) as e:
            seen.append(f"raised {type(e).__name__}: {str(e)[:120]}")
            return False
        seen.append(repr(got))
        return got == want

    wait_until(status, budgets.RPC_ROUNDTRIP_S, diagnose=lambda: (
        f"{why}: the admin's row never read {want!r}; last reads {seen[-4:]}; "
        f"rows {app.admin.user_count()}; error: {app.error_text()!r}"))


@pytest.mark.feature("admin-users")
def test_admin_sees_user_serving_status_read_only(admin_app, nest_instance):
    app = admin_app
    admin_hex = bytes(nest_instance["admin"]["signing_key"].verify_key).hex()

    # Default on: the admin's own row reads "Serving here" (no flag set).
    app.admin.navigate_users()
    wait_until(lambda: app.admin.user_count() >= 1, budgets.RPC_ROUNDTRIP_S,
               diagnose=lambda: f"the users page lists no row; error: {app.error_text()!r}")
    _wait_serving_status(app, admin_hex, S.admin.users_page.serving_here,
                         "serving indicator should default to 'Serving here'")

    # Drive the *user* half: the admin disables their own serving from their own
    # mail-settings serve-here toggle (the only client-driven write path — there
    # is no admin write). Mail must be enabled for the serve-here toggle to show.
    app.mail_settings.navigate()
    app.mail_settings.ensure_mail_enabled()
    assert app.mail_settings.serve_here_visible(), (
        "serve-here toggle should be visible once mail is enabled; "
        f"error: {app.mail_settings.page_error_text(timeout=2.0)!r}"
    )
    try:
        app.mail_settings.set_serve_here(False)
        assert app.mail_settings.wait_for_serve_here_state("off"), (
            f"serve-here should read off after the gesture; error: {app.error_text()!r}"
        )

        # The admin hub now reflects the user's choice read-only: "Not serving"
        # (proves the row renders the projected field, not a constant).
        app.admin.navigate_users()
        _wait_serving_status(app, admin_hex, S.admin.users_page.serving_disabled,
                             "serving indicator should reflect the disabled flag")
    finally:
        # Restore even on a red: the actor is session-scoped, and a run left at
        # "Not serving" would fail every later run's default read on this nest.
        app.mail_settings.navigate()
        app.mail_settings.set_serve_here(True)
        assert app.mail_settings.wait_for_serve_here_state("on")
