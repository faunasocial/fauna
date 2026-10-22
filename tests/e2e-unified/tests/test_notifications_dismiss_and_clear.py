"""The user's two deletes on the Notifications page: dismiss one row, clear all.

`docs/goal/behavior/notifications.md` § Retention rule 2: a notification row
leaves the nest by the user's hand on exactly two grains —
``fauna.notifications.dismiss { id }`` deletes one row, and
``fauna.notifications.clear { up_to? }`` deletes every row created at or before
``up_to`` (a page passes the newest ``created_at`` it has shown, so a row that
arrived after it looked survives). The page offers them as the per-row
``notification-dismiss`` and the page-level ``notification-clear`` (ui.yaml
``notifications``; ids user-approved 2026-10-16). One carve-out: a
``security.notice`` row younger than 14 days is deletable by neither grain —
the nest refuses the dismiss and the clear skips it — so after a clear-all
exactly the retained notices may stay listed.

Rule 3 rides along because it is cheap here: dismissing a knock on the Contacts
page deletes its ``knock`` doorbell row with it, so the Notifications page must
not keep it. The knock is a REAL one — a fresh second actor's
``fauna.inbox.send`` held at the door under the default ``allow_knock`` inbox
mode, which is the production path that writes both the knock and its doorbell
(``routes.rs::store_knock``); the nest's ``push/knock`` hook writes no doorbell.

The seeding is fixture setup arranging a precondition (e2e rule 8b): the rows
come through ``POST /api/v1/test/push/notify`` (as in
``test_notifications_localized_body.py``); every delete is a UI gesture. Each
effect is read twice — the painted list, and ``fauna.notifications.list`` on
the wire — so a failure says whether the click never reached the nest or the
page never repainted (convention 6).

tier_2: a real app driver renders the page, the rows are seeded through the
nest's test hook rather than a live producer.
"""

from __future__ import annotations

import time
import uuid

import pytest
import requests

from clients.ws_rpc_admin_client import WsRpcAdminClient
from common import build_email_inbox_payload, create_actor_and_register
from helpers.app_surface import app_name
from helpers.budgets import PUSH_REFRESH_S
from helpers.waiting import wait_until

pytestmark = [pytest.mark.tier_2]

# `fauna_protocol::notifications::SECURITY_NOTICE_RETENTION_SECS`, in micros.
_RETENTION_US = 14 * 24 * 3600 * 1_000_000


def _wire_rows(nest_instance, test_user) -> list[dict]:
    with WsRpcAdminClient(
        nest_instance["url"],
        actor_id=bytes.fromhex(test_user["actor_id_hex"]),
        signing_key=bytes(test_user["signing_key"]),
    ) as client:
        return client.call("fauna.notifications.list", {"limit": 100})["notifications"]


def _retained(row: dict) -> bool:
    """`is_security_notice_retained` — the one class a clear-all leaves listed."""
    now_us = int(time.time() * 1_000_000)
    return row.get("notif_type") == "security.notice" and (
        now_us - int(row["created_at"]) < _RETENTION_US
    )


@pytest.mark.tui
@pytest.mark.web
@pytest.mark.linux
@pytest.mark.feature("notifications")
def test_dismiss_deletes_one_row_and_clear_all_deletes_the_rest(
    logged_in_app, nest_instance, test_user
):
    app = logged_in_app
    tag = uuid.uuid4().hex[:10]
    summaries = [f"dismiss probe {tag} #{n}" for n in range(3)]
    ids = []
    for summary in summaries:
        resp = requests.post(
            f"{nest_instance['url']}/api/v1/test/push/notify",
            json={
                "actor_id": test_user["actor_id_hex"],
                "summary": summary,
                "notif_type": "like",
                "content_id": uuid.uuid4().hex,
            },
            timeout=10,
        )
        assert resp.status_code == 200, (
            f"test-hooks push endpoint returned {resp.status_code}: {resp.text}"
        )
        ids.append(resp.json()["notification_id"])

    app.notifications.navigate()

    def painted() -> list[str]:
        return [
            app.driver.get_text("notification-item", index=i) or ""
            for i in range(app.driver.count("notification-item"))
        ]

    def diagnose(what: str):
        return lambda: (
            f"{what}; rows: {painted()!r}; page error: {app.error_text()!r}; "
            f"wire ids: {[r['id'] for r in _wire_rows(nest_instance, test_user)]!r}; "
            f"{app.driver.diagnose('notification-dismiss')}"
        )

    wait_until(
        lambda: all(any(s in t for t in painted()) for s in summaries) or None,
        PUSH_REFRESH_S,
        diagnose=diagnose(f"the three seeded rows (tag {tag!r}) never all showed"),
    )

    # Every row carries its own dismiss: the flat indexed id pairs with
    # `notification-item[i]` one-for-one (convention 17 — the general invariant).
    assert app.driver.count("notification-dismiss") == app.driver.count(
        "notification-item"
    ), (
        "every notification row must paint its own notification-dismiss; "
        f"{app.driver.diagnose('notification-dismiss')}"
    )

    # ── Dismiss the middle seeded row.
    texts = painted()
    middle = next(i for i, t in enumerate(texts) if summaries[1] in t)
    app.driver.click("notification-dismiss", index=middle)

    wait_until(
        lambda: (
            not any(summaries[1] in t for t in painted())
            and all(any(s in t for t in painted()) for s in (summaries[0], summaries[2]))
        )
        or None,
        PUSH_REFRESH_S,
        diagnose=diagnose(
            f"dismissing row {middle} ({summaries[1]!r}) must remove exactly that row"
        ),
    )
    wire_ids = {r["id"] for r in _wire_rows(nest_instance, test_user)}
    assert ids[1] not in wire_ids, (
        f"the dismissed row {ids[1]} is still on the nest — the page hid it "
        f"without calling fauna.notifications.dismiss; wire ids {sorted(wire_ids)}"
    )
    assert {ids[0], ids[2]} <= wire_ids, (
        f"a dismiss must delete one row only; wire ids {sorted(wire_ids)}"
    )
    assert not app.has_error(), f"dismiss surfaced an error: {app.error_text()!r}"

    # ── Clear all: everything goes but a security notice inside its window.
    app.driver.click("notification-clear")

    # Wrapped in a 1-tuple: an emptied list is the commonest success, and
    # `wait_until` reads a falsy return as "not yet".
    def only_retained_left():
        rows = _wire_rows(nest_instance, test_user)
        if any(not _retained(r) for r in rows):
            return None
        return (rows,) if app.driver.count("notification-item") == len(rows) else None

    (rows,) = wait_until(
        only_retained_left,
        PUSH_REFRESH_S,
        diagnose=diagnose(
            "clear-all must delete every row but a retained security notice, "
            "and the page must list exactly what the nest kept"
        ),
    )
    unread_left = sum(1 for r in rows if not r.get("is_read"))
    state = app.driver.get_state(
        "data.notifications",
        wait_for=lambda s: (s or {}).get("unread_count") == unread_left,
    )
    # Web publishes no `data.notifications` section (a pre-existing state-provider
    # gap, not this slice's): the check binds an app that publishes it.
    if state is not None or app_name(app.driver) != "web":
        assert (state or {}).get("unread_count") == unread_left, (
            f"after clear-all data.notifications.unread_count must count only the "
            f"{unread_left} unread retained notice(s) left; state {state!r}"
        )
    assert not app.has_error(), f"clear-all surfaced an error: {app.error_text()!r}"


@pytest.mark.tui
@pytest.mark.feature("notifications")
def test_dismissing_a_knock_takes_its_doorbell_row_with_it(
    logged_in_app, nest_instance, test_user
):
    """§ Retention rule 3 — the knock and its doorbell are one thing turned away."""
    app = logged_in_app
    knocker = create_actor_and_register(
        nest_instance["port"], admin_signing_key=nest_instance["admin"]["signing_key"]
    )
    payload, _ = build_email_inbox_payload(
        knocker["signing_key"],
        test_user["actor_id_hex"],
        f"knock {uuid.uuid4().hex[:8]}",
        "may I come in",
        node_url=nest_instance["url"],
    )
    with WsRpcAdminClient(
        nest_instance["url"],
        actor_id=knocker["actor_id_bytes"],
        signing_key=bytes(knocker["signing_key"]),
    ) as client:
        reply = client.call(
            "fauna.inbox.send",
            {
                "recipient_actor_id": test_user["actor_id_hex"],
                "recipient_nest_url": None,
                "payload_bytes": payload,
            },
        )
    assert reply.get("inbox_id") is None, (
        f"a stranger's email under allow_knock must be held as a knock, got {reply!r}"
    )

    def doorbells() -> list[dict]:
        return [
            r
            for r in _wire_rows(nest_instance, test_user)
            if r.get("notif_type") == "knock"
            and r.get("sender_id") == knocker["actor_id_hex"]
        ]

    assert doorbells(), "store_knock must ring a doorbell row for the knock"

    app.contacts.navigate()
    # A 1-tuple again: index 0 is falsy.
    (index,) = wait_until(
        lambda: (
            lambda i: None if i is None else (i,)
        )(app.contacts.knock_index_for(knocker["actor_id_hex"])),
        PUSH_REFRESH_S,
        diagnose=lambda: (
            f"the knock from {knocker['actor_id_hex']} never listed; "
            f"senders {app.contacts.knock_senders()!r}"
        ),
    )
    app.contacts.dismiss_knock(index)
    wait_until(
        lambda: not doorbells() or None,
        PUSH_REFRESH_S,
        diagnose=lambda: f"the knock's doorbell outlived its dismissal: {doorbells()!r}",
    )

    app.notifications.navigate()
    # The doorbell's sentence names the sender by its first 8 hex
    # (`routes.rs::knock_body`).
    short = knocker["actor_id_hex"][:8]
    painted = [
        app.driver.get_text("notification-item", index=i) or ""
        for i in range(app.driver.count("notification-item"))
    ]
    assert not any(short in t for t in painted), (
        f"the Notifications page still paints the dismissed knock's doorbell: {painted!r}"
    )
