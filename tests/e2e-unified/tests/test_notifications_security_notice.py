"""A security notice from the nest shows in the Notifications list with its full
detail.

The witness for ``docs/features/notifications.md`` outcome 6, and the ruling
``docs/goal/behavior/notifications.md`` § Security notices makes (2026-08-10):
a ``SecurityNotifier`` event **renders on this page** — the nest writes a
``security.notice`` row alongside its other channels — and *"the summary is the
full notice: the body carries the actionable detail (the IP address, a pending
action's id) … One-line rendering is app-side truncation."* The localized body
carries that same detail as args (§ Localized body → *Security notices*), so
the sentence the row shows is the catalog's, with the detail substituted in.

**The events.** Two, one test each, both through the same
``SecurityNotifier::notify`` every security event uses:

- The account's full archive is downloaded (``GET /api/v1/export``) —
  ``export_routes.rs`` fires ``SecurityEvent::ArchiveExported`` with the peer
  address on every download.
- A handle change is queued on the account (``fauna.profile.handle.change``) —
  a person-initiated pending action, which rings its creator with
  ``PendingActionCreated`` (§ Security notices → *Pending actions*).

**The new-sign-in notice is not witnessed here, deliberately.** It rings when a
mint arrives from an address other than the actor's last one, and this harness
reaches the nest from one loopback address only — a second source address
(``127.0.0.2``) exists on Linux loopback but not on macOS, where this tui test
also runs. Its pin lives in the nest instead, where the dispatcher is driven
with chosen peer addresses: ``routes::new_sign_in_notice_tests`` (both mints,
ring-once and never-for-the-same-address).

**Why the events are headless (e2e rule 8b).** The journey is *reading the
notice*. Each event is the thing being warned about, and in the case the notice
exists for it is made by someone other than the person holding the app — a
download or a queued change from a stolen key. Each rides the production kind
with a bearer minted by the ordinary handshake.

**A dedicated actor.** A security notice also goes out as mail and as an inbox
envelope, so each is raised on a fresh account rather than on the session's
shared user, whose mailbox other modules read (and whose handle they rely on).

**Convention 14.** The notice is written off the download's response path, so
the assertion waits on list state (the open page refetches on the
notification's push event), never on elapsed time.

tier_3: a real app driver, a real nest, the production export route and
notifier.
"""

from __future__ import annotations

import ipaddress

import pytest
import requests

from common.auth import _authed_call, mint_token_via_handshake
from helpers.budgets import PUSH_REFRESH_S
from helpers.waiting import wait_until
from i18n.strings import S

pytestmark = [pytest.mark.tier_3]

_IP_SLOT = "\x00ip\x00"


@pytest.mark.web
@pytest.mark.linux
@pytest.mark.macos
@pytest.mark.ios
@pytest.mark.android
@pytest.mark.windows
@pytest.mark.tui
@pytest.mark.feature("notifications")
def test_security_notice_shows_in_the_list_with_its_full_detail(app, request, nest_instance):
    from conftest import _login_app_as, _make_user

    user = _make_user(nest_instance)
    _login_app_as(app, request, nest_instance, user, verify_live_actor=True)

    # The page is open before the event, as a user's would be.
    app.notifications.navigate()

    # ── The event: the account's archive is downloaded.
    token = mint_token_via_handshake(nest_instance["url"], user["signing_key"])
    resp = requests.get(
        f"{nest_instance['url']}/api/v1/export",
        params={"include_blobs": "false"},
        headers={"Authorization": f"Bearer {token}"},
        timeout=120,
    )
    assert resp.status_code == 200, (
        f"GET /api/v1/export returned {resp.status_code}: {resp.text[:400]}"
    )

    # ── The notice, as the list shows it.
    before, after = S.notifications.row_security_archive_exported(ip=_IP_SLOT).split(_IP_SLOT)
    row = wait_until(
        lambda: _row_containing(app, before),
        PUSH_REFRESH_S,
        diagnose=lambda: (
            "the archive download's security notice never showed in the list; rows: "
            f"{_rows(app)!r}; page error: {app.error_text()!r}"
        ),
    )

    # Full detail: the whole sentence, not a truncated line, with the address
    # the download came from substituted in.
    assert after in row, (
        f"the notice must show in full — expected it to continue {after!r}: {row!r}"
    )
    detail = row[row.index(before) + len(before) : row.index(after)]
    try:
        ipaddress.ip_address(detail)
    except ValueError:
        pytest.fail(
            "the notice must carry the actionable detail — the address the "
            f"archive was downloaded from — but it reads {detail!r} in {row!r}"
        )


@pytest.mark.tui
@pytest.mark.feature("notifications")
def test_pending_action_notice_shows_in_the_list_with_its_full_detail(
    app, request, nest_instance
):
    from conftest import _login_app_as, _make_user

    user = _make_user(nest_instance)
    _login_app_as(app, request, nest_instance, user, verify_live_actor=True)
    app.notifications.navigate()

    # ── The event: a handle change is queued on the account.
    queued = _authed_call(
        nest_instance["url"],
        user["signing_key"],
        "fauna.profile.handle.change",
        {"handle": f"moved{user['actor_id_hex'][:10]}"},
    )

    # ── The notice, as the list shows it: the whole catalog sentence, with the
    # action's id and deadline — the detail a user needs to find and cancel it
    # under Settings → Pending actions.
    sentence = S.notifications.row_security_pending_action_queued(
        action_type="handle.change",
        action_id=str(queued["pending_action_id"]),
        execute_after=str(queued["execute_after"]),
    )
    wait_until(
        lambda: _row_containing(app, sentence),
        PUSH_REFRESH_S,
        diagnose=lambda: (
            "the queued handle change's security notice never showed in full; "
            f"expected {sentence!r}; rows: {_rows(app)!r}; "
            f"page error: {app.error_text()!r}"
        ),
    )


def _rows(app) -> list[str]:
    return [
        app.driver.get_text("notification-item", index=i)
        for i in range(app.notifications.notification_count())
    ]


def _row_containing(app, text: str) -> str | None:
    return next((row for row in _rows(app) if text in row), None)
