"""A muted word collapses the notification rows whose relayed text carries it —
and never a security notice.

``docs/goal/behavior/notifications.md`` § Muted words (ratified 2026-10-09):
the match runs over a row's ``body.args`` — the text other people wrote — never
over the catalog's sentence around them (rule 1); only a word muted at Hide
collapses (rule 2); a collapsed row paints ``notification-muted`` and
``notification-muted-reveal-button`` in place of the sentence, is no tap target,
and the reveal is per-row and session-local (rule 3); the unread badge is the
nest's count, collapsed rows included (rule 4); a ``security.notice`` row is
never collapsed (rule 5). The decision is the shared
``fauna_client_notifications::notification_muted``; the witness for
``docs/features/muted-words.md`` outcome 8.

**The seeding.** Three rows through the nest's ``POST /api/v1/test/push/notify``
hook, each with a localized body whose arg carries (or does not carry) the term:
a knock whose ``{message}`` carries it, a like whose ``{sender}`` does not, and
a security notice whose ``{ip}`` carries it. A user's part in a notification is
nothing, so seeding is fixture setup (e2e rule 8b); the mutations under test —
muting the word and revealing the row — go through the app UI.

**The term is per run.** The actor is session-scoped and keeps its rows, so a
fixed term would collapse an earlier run's rows too and skew the counts; the
muted list is emptied in ``finally`` for the tests that assert it empty.

tier_2: a real app driver and a real nest, the rows seeded through a test hook
rather than a live producer.
"""

from __future__ import annotations

import re
import uuid

import pytest
import requests

from clients.ws_rpc_admin_client import WsRpcAdminClient
from helpers.budgets import PUSH_REFRESH_S
from helpers.waiting import wait_until
from tests.test_muted_words import _clear_muted_words

pytestmark = [pytest.mark.tier_2]


def _seed_notification_row(nest_instance, actor_id_hex: str, notif_type: str, key: str, args: dict) -> int:
    resp = requests.post(
        f"{nest_instance['url']}/api/v1/test/push/notify",
        json={
            "actor_id": actor_id_hex,
            "summary": f"{notif_type} row",
            "notif_type": notif_type,
            "body_key": key,
            "body_args": args,
            "content_id": uuid.uuid4().hex,
        },
        timeout=10,
    )
    assert resp.status_code == 200, (
        f"test-hooks push endpoint returned {resp.status_code}: {resp.text}"
    )
    return resp.json()["notification_id"]


@pytest.mark.tui
@pytest.mark.web
@pytest.mark.linux
@pytest.mark.android
@pytest.mark.macos
@pytest.mark.ios
@pytest.mark.feature("muted-words")
def test_a_muted_word_collapses_the_notification_rows_that_carry_it(
    logged_in_app, nest_instance, test_user
):
    app = logged_in_app
    d = app.driver
    tag = uuid.uuid4().hex[:10]
    term = f"mutedterm{tag}"
    clean_sender = f"clean{tag}"
    # This run's own knock text: the actor is session-scoped and shared across
    # the apps a run names, so an earlier app's knock (its term no longer muted)
    # still paints in full on this page — only this run's text may be asserted on.
    knock_text = f"hello, about {term.upper()} again"

    app.muted_words.navigate()
    app.muted_words.add(term)
    try:
        assert app.muted_words.wait_for_word(term), (
            f"muted term did not persist; words={app.muted_words.words()!r}"
        )

        actor = test_user["actor_id_hex"]
        _seed_notification_row(
            nest_instance,
            actor,
            "knock",
            "notifications.row_knock",
            {"sender": f"k{tag[:7]}", "message": knock_text},
        )
        _seed_notification_row(nest_instance, actor, "like", "notifications.row_like", {"sender": clean_sender})
        _seed_notification_row(
            nest_instance,
            actor,
            "security.notice",
            "notifications.row_security_archive_exported",
            {"ip": f"203.0.113.7 {term}"},
        )

        app.notifications.navigate()

        def painted() -> list[str]:
            return [
                d.get_text("notification-item", index=i) or ""
                for i in range(d.count("notification-item"))
            ]

        # The clean row and the security notice paint in full; the knock does not.
        def settled() -> list[str] | None:
            texts = painted()
            ok = any(clean_sender in t for t in texts) and any(term in t for t in texts)
            return texts if ok and d.count("notification-muted") >= 1 else None

        texts = wait_until(
            settled,
            PUSH_REFRESH_S,
            diagnose=lambda: (
                f"the seeded rows (tag {tag!r}) never settled into one collapsed "
                f"knock + a clean like + a full security notice; rows: {painted()!r}; "
                f"muted={d.count('notification-muted')}; page error: {app.error_text()!r}; "
                f"{d.diagnose('notification-muted')}"
            ),
        )
        assert d.count("notification-muted") == 1, (
            "exactly the knock carrying the term should collapse; "
            f"muted={d.count('notification-muted')} rows={texts!r}"
        )
        assert not any(knock_text in t for t in texts), (
            f"the collapsed knock's text must not paint as a row; rows={texts!r}"
        )
        assert any(term in t for t in texts), (
            "a security notice is never collapsed, whatever its args carry "
            f"(§ Muted words rule 5); rows={texts!r}"
        )
        assert d.count("notification-muted-reveal-button") == 1, (
            "the collapsed row offers its one gesture, the reveal"
        )

        # The badge is the nest's count — the collapsed row still counts (rule 4).
        with WsRpcAdminClient(
            nest_instance["url"],
            actor_id=bytes.fromhex(actor),
            signing_key=bytes(test_user["signing_key"]),
        ) as client:
            wire_count = client.call("fauna.notifications.count", {})["count"]
        badge = app.notifications.unread_badge_text()
        assert re.search(rf"\b{wire_count}\b", badge), (
            f"the unread badge must show the nest's count {wire_count}, collapsed "
            f"rows included; badge={badge!r}"
        )

        # The reveal paints the row in full, for this session; the mute stays.
        rows_before = d.count("notification-item")
        d.click("notification-muted-reveal-button", index=0)
        wait_until(
            lambda: d.count("notification-muted") == 0
            and any(knock_text in t for t in painted()),
            PUSH_REFRESH_S,
            diagnose=lambda: (
                f"the reveal did not paint the knock; rows={painted()!r} "
                f"muted={d.count('notification-muted')}"
            ),
        )
        assert d.count("notification-item") == rows_before + 1, (
            f"the revealed knock should paint as one more row; rows={painted()!r}"
        )
    finally:
        _clear_muted_words(app.muted_words)
