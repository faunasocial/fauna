"""A stranger's knock raises one notice in your language, and a flood replaces
it rather than piling up.

`docs/features/notifications.md` outcome 15. Authority:
`docs/goal/behavior/notifications.md` § Localized body, *The knock toast* —
every app raises an OS-level toast for an inbound knock, titled
`notifications.knock_title`, saying the shared
`fauna_client_notifications::knock_push_text` decision (a catalog-known body is
the knock row's own `notifications.row_knock` sentence), and "a knock toast
carries one notification id per app, never one per sender, so a flood of fresh
keys replaces one toast instead of stacking".

**What is asserted, and what deliberately is not.** The app appends every knock
toast it hands to the platform to the shared fired-notice log, published as
`fauna_e2e_agent::KNOCK_NOTICES_KEY`; this file reads that log. It never looks
at the OS notification centre — whether the toast *appears* is the last inch,
and no test can read it. The entry is written at the firing site after the
muted-word gate, so it means "the user was shown this". "Replaced, not stacked"
reads as every entry of the flood carrying the one shared replace id
(`KNOCK_TOAST_REPLACE_ID`), which the platform's notification service replaces
under.

**Latency independence (convention 14).** Every read is a wait for this run's
own entries to appear in an append-only log, under a named ceiling; no sleep.

The knocks are stored and pushed by the nest's `POST /api/v1/test/push/knock`
hook, the production `store_knock` path minus the stranger's `fauna.inbox.send`
— a precondition arranged for the user, whose part in receiving a knock is
nothing (e2e rule 8b). tier_2: the trigger is the nest's test hook.
"""

from __future__ import annotations

import uuid

import pytest
import requests

from helpers.app_surface import app_name, launched_as_a_bundle, skip_environment
from helpers.budgets import PUSH_REFRESH_S
from helpers.connection import wait_until_online
from helpers.waiting import wait_until
from i18n.strings import S

pytestmark = pytest.mark.tier_2

KNOCK_NOTICES_KEY = "knock_notices"

#: `fauna_client_notifications::KNOCK_TOAST_REPLACE_ID` — the one id every
#: knock toast is raised under, on every arm that carries a replace id
#: (freedesktop for tui and linux; android's notification id; web's
#: `Notification` tag and apple's `UNNotificationRequest` identifier, both
#: `fauna-knock-1003`, derived one-to-one from it).
KNOCK_TOAST_REPLACE_ID = 1003

#: Why the macOS bare-binary column is short: a launch shape, not an app gap.
_BARE_BINARY = (
    "the knock toast's firing leg IS built — FaunaKit's `startKnockObserver`, "
    "shared line for line with `--app ios` — but an ordinary `--app macos` run "
    "launches the BARE `FaunaMacOS` binary, and a non-bundle process has no "
    "notification host (`NotificationHost.isAvailable`), so the app hands no "
    "toast to the platform and honestly records none. Replay it with `pytest "
    "tests/e2e-unified/tests/test_notifications_knock_toast.py --app macos "
    "--macos-artifact`"
)


def _knock_notices(driver):
    """The app's fired-knock log, or a loud refusal (convention 11).

    An app with no knock-toast leg publishes nothing; reading that as an empty
    list would turn an unbuilt toast into a pass of any negative assertion.
    """
    value = driver.get_state(KNOCK_NOTICES_KEY)
    if not isinstance(value, dict) or not isinstance(value.get("fired"), list):
        raise AssertionError(
            f"{type(driver).__name__} does not publish '{KNOCK_NOTICES_KEY}' — the "
            "knock-toast log that witnesses `notifications` outcome 15 (see "
            "`fauna_e2e_agent::KNOCK_NOTICES_KEY`). Build the app's knock toast "
            f"and publish the shared log; got {value!r}."
        )
    return value


def _knock(nest_instance, actor_hex, sender_id, summary):
    resp = requests.post(
        f"{nest_instance['url']}/api/v1/test/push/knock",
        json={"actor_id": actor_hex, "sender_id": sender_id, "summary": summary},
        timeout=10,
    )
    assert resp.status_code == 200, (
        f"test-hooks knock endpoint returned {resp.status_code}: {resp.text}"
    )
    assert resp.json().get("ok") is True, resp.text


@pytest.mark.tui
@pytest.mark.linux
@pytest.mark.android
@pytest.mark.web
@pytest.mark.macos
@pytest.mark.ios
@pytest.mark.windows
@pytest.mark.feature("notifications")
def test_a_flood_of_knocks_raises_one_localized_notice(
    logged_in_app, nest_instance, test_user
):
    app = logged_in_app
    d = app.driver
    tag = uuid.uuid4().hex[:10]
    if app_name(d) == "macos" and not launched_as_a_bundle(d):
        skip_environment(_BARE_BINARY)

    # A push fired into a handshake still in flight is dropped by design, so the
    # flood starts only once the app's own transport verdict reads online.
    wait_until_online(d)
    _knock_notices(d)  # refuse up front if the leg is absent

    # Three FRESH senders: the flood the goal doc names is "fresh keys", and a
    # per-sender id would stack exactly here.
    knocks = []
    for i in range(3):
        sender = uuid.uuid4().hex + uuid.uuid4().hex  # 64 hex = 32 bytes
        message = f"knock-toast probe {tag} #{i}"
        knocks.append((sender, message))
        _knock(nest_instance, test_user["actor_id_hex"], sender, message)

    expected = [
        S.notifications.row_knock(sender=sender[:8], message=message)
        for sender, message in knocks
    ]

    def mine():
        return [
            e for e in _knock_notices(d)["fired"]
            if isinstance(e, dict) and tag in (e.get("text") or "")
        ]

    entries = wait_until(
        lambda: (lambda got: got if len(got) >= 3 else None)(mine()),
        PUSH_REFRESH_S,
        diagnose=lambda: (
            f"three knocks (tag {tag!r}) were stored and pushed, but the app logged "
            f"{len(mine())} knock toast(s) for them; log: {_knock_notices(d)!r}; "
            f"page error: {app.error_text() if app.has_error() else '(none)'}"
        ),
    )

    assert len(entries) == 3, (
        f"one toast per knock push, no duplicates; got {entries!r}"
    )
    assert sorted(e.get("text") for e in entries) == sorted(expected), (
        "each knock toast must say the knock row's own localized sentence "
        f"(`notifications.row_knock`), never the raw message; got {entries!r}, "
        f"expected {expected!r}"
    )
    assert all(e.get("key") == "notifications.row_knock" for e in entries), (
        f"the toast text must resolve from the catalog key; got {entries!r}"
    )
    ids = {e.get("replace_id") for e in entries}
    assert ids == {KNOCK_TOAST_REPLACE_ID}, (
        "every knock toast must carry the one shared replace id, so a flood of "
        "fresh keys replaces one toast instead of stacking "
        f"(§ Localized body, the knock toast); replace ids: {ids!r}"
    )
