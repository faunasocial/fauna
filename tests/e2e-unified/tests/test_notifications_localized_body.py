"""A notification row says what its localized body says — and a body this app
cannot read says the summary instead.

`docs/goal/behavior/notifications.md` § Localized body: the nest sends every
row twice — ``body``, a catalog key plus data args the app says in the
reader's language, and ``summary``, the same sentence in English. Which one a
row paints is the shared decision ``notification_text`` (the compat table):
a key in the app's catalog paints the localized body; a key the app's catalog
lacks (a newer nest's) paints ``summary`` — never the raw key.

**Why the two texts differ here.** Every real producer's ``summary`` is the
catalog's own English rendering of its body, so on an English build the two
read identically and a test seeding a real row could not tell which one an app
painted. The nest's ``POST /api/v1/test/push/notify`` hook takes ``body_key``
+ ``body_args`` beside ``summary`` precisely so a suite can seed a row whose
two texts DISAGREE: the known-key row must show the catalog sentence and not
its summary; the unknown-key row must show its summary and not its key. An app
painting ``summary`` verbatim — the pre-body shape — fails the first; an app
resolving the body without asking whether the key exists fails the second.

The seeding is fixture setup arranging a precondition (e2e rule 8b): the user's
part in a notification is, by design, nothing. The wire leg runs first so a
failure diagnoses itself (convention 6) — if ``fauna.notifications.list``
already lacks the body, the defect is nest-side.

tier_2: a real app driver renders the page, but the rows are seeded through the
nest's test hook rather than a live producer.
"""

from __future__ import annotations

import uuid

import pytest
import requests

from clients.ws_rpc_admin_client import WsRpcAdminClient
from helpers.budgets import PUSH_REFRESH_S
from helpers.waiting import wait_until
from i18n.strings import S

pytestmark = [pytest.mark.tier_2]


@pytest.mark.tui
@pytest.mark.web
@pytest.mark.linux
@pytest.mark.windows
@pytest.mark.macos
@pytest.mark.ios
# android renders through the same shared decision (`notification_text_for`)
# and is compile-proved; no android e2e venue exists yet to run this leg
# (`testing.md` § Default app and nest mode → *Android's run venue*).
@pytest.mark.android
@pytest.mark.feature("notifications")
def test_a_row_paints_its_localized_body_and_an_unknown_key_paints_the_summary(
    logged_in_app, nest_instance, test_user
):
    actor_id_hex = test_user["actor_id_hex"]
    tag = uuid.uuid4().hex[:10]

    # A catalog key, with a summary that deliberately says something else.
    sender = f"loc{tag}"
    known_summary = f"english fallback {tag} (must not be painted)"
    known_sentence = S.notifications.row_like(sender=sender)
    # A key no catalog carries — the shape of a row a still-newer nest minted.
    unknown_key = f"notifications.row_not_minted_{tag}"
    unknown_summary = f"english fallback {tag} for a key this app lacks"

    ids = {}
    for label, body_key, body_args, summary in (
        ("known", "notifications.row_like", {"sender": sender}, known_summary),
        ("unknown", unknown_key, {"sender": sender}, unknown_summary),
    ):
        resp = requests.post(
            f"{nest_instance['url']}/api/v1/test/push/notify",
            json={
                "actor_id": actor_id_hex,
                "summary": summary,
                "notif_type": "like",
                "body_key": body_key,
                "body_args": body_args,
                # Distinct rows for the session-scoped actor across runs.
                "content_id": uuid.uuid4().hex,
            },
            timeout=10,
        )
        assert resp.status_code == 200, (
            f"test-hooks push endpoint returned {resp.status_code}: {resp.text}"
        )
        assert resp.json().get("ok") is True, resp.json()
        ids[label] = resp.json()["notification_id"]

    # ── Wire leg: both rows carry their body beside the summary.
    with WsRpcAdminClient(
        nest_instance["url"],
        actor_id=bytes.fromhex(actor_id_hex),
        signing_key=bytes(test_user["signing_key"]),
    ) as client:
        reply = client.call("fauna.notifications.list", {"limit": 100})
    rows = {row["id"]: row for row in reply["notifications"]}
    for label, body_key in (("known", "notifications.row_like"), ("unknown", unknown_key)):
        row = rows.get(ids[label])
        assert row is not None, (
            f"fauna.notifications.list did not return the {label}-key row "
            f"{ids[label]}; got ids {sorted(rows)}"
        )
        assert (row.get("body") or {}).get("key") == body_key, (
            f"the {label}-key row must carry its body on the wire "
            f"(notifications.md § Localized body); got {row!r}"
        )

    # ── Render leg.
    app = logged_in_app
    app.notifications.navigate()

    def painted() -> list[str]:
        return [
            app.driver.get_text("notification-item", index=i) or ""
            for i in range(app.driver.count("notification-item"))
        ]

    # Both seeded rows carry the tag whichever text they paint (the sender arg,
    # the summaries, the unknown key), so arrival is judged before the verdict.
    def both_rows_arrived() -> list[str] | None:
        texts = painted()
        return texts if sum(tag in text for text in texts) >= 2 else None

    texts = wait_until(
        both_rows_arrived,
        PUSH_REFRESH_S,
        diagnose=lambda: (
            f"the two seeded rows (tag {tag!r}) never both showed in the list; "
            f"rows: {painted()!r}; page error: {app.error_text()!r}; "
            f"{app.driver.diagnose('notification-item')}"
        ),
    )

    # The known key: the catalog sentence, not the summary.
    assert any(known_sentence in text for text in texts), (
        "a row whose body key is in the app's catalog must paint the localized "
        f"sentence {known_sentence!r}; rows: {texts!r}"
    )
    assert not any(known_summary in text for text in texts), (
        "a row with a known body key must NOT paint its English summary "
        f"{known_summary!r} — the app is still painting `summary` verbatim; "
        f"rows: {texts!r}"
    )

    # The unknown key: the summary, never the raw key.
    assert any(unknown_summary in text for text in texts), (
        "a row whose body key the app's catalog lacks must fall back to its "
        f"summary {unknown_summary!r}; rows: {texts!r}"
    )
    assert not any(unknown_key in text for text in texts), (
        f"an unknown body key must never be painted at the user ({unknown_key!r}); "
        f"rows: {texts!r}"
    )

    assert not app.has_error(), (
        f"notifications page surfaced an error after load: {app.error_text()!r}"
    )
