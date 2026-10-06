"""A knock names its sender by the shared short id, on every app.

``knock-sender`` is the "Sender ID display on a knock request" (ui.yaml
``elements.knock-sender``), and ``fauna.knocks.list`` carries no handle — the
sender's actor id is the only identity a knock row has. Every app renders it
through the one shared formatter, ``fauna_core::format::short_id`` (the first 12
hex characters and a ``…``; ``value-formatting.md`` § Short id), with the knock's
summary on a line of its own (``contacts.md`` § Where logic lives → Knock sender
display).

Nothing read this element before, and its text had drifted: the short id on web,
android and apple, an empty string on windows, the full hex on tui, and the
knock's summary on linux — so linux's "sender" label showed whatever the knock
said about itself.

The knock is stored by the nest's ``POST /api/v1/test/push/knock`` (the
``test-hooks`` feature — the production ``db.push_knock`` path, as in
``test_knock_live_refresh.py``) with a fresh sender and a summary that shares no
text with any id, so a row showing the full hex or the summary cannot pass.
"""
from __future__ import annotations

import re
import uuid

import pytest
import requests

from helpers import budgets
from helpers.connection import wait_until_online
from helpers.waiting import wait_until

pytestmark = pytest.mark.tier_2

# `fauna_core::format::short_id` over a 64-hex actor id: 12 hex characters, then U+2026.
_SHORT_ID = re.compile(r"[0-9a-f]{12}…")


def _short_id(hex_id: str) -> str:
    """The oracle — ``fauna_core::format::short_id``."""
    return hex_id if len(hex_id) <= 12 else hex_id[:12] + "…"


@pytest.mark.feature("contacts")
def test_knock_sender_shows_the_shared_short_id(logged_in_app, nest_instance, test_user):
    """A stored knock → the contacts page's ``knock-sender`` reads ``short_id(sender)``."""
    app = logged_in_app
    wait_until_online(app.driver)

    # A FRESH sender per call: the actor is session-scoped, so this test firing once
    # per app must not collide with its own earlier rows (or test_knock_live_refresh's).
    sender_id = uuid.uuid4().hex + uuid.uuid4().hex  # 64 hex chars = 32 bytes
    summary = f"knock sender display probe {uuid.uuid4().hex[:8]}"
    resp = requests.post(
        f"{nest_instance['url']}/api/v1/test/push/knock",
        json={
            "actor_id": test_user["actor_id_hex"],
            "sender_id": sender_id,
            "summary": summary,
        },
        timeout=10,
    )
    assert resp.status_code == 200, (
        f"test-hooks knock endpoint returned {resp.status_code}: {resp.text}"
    )
    assert resp.json().get("ok") is True, resp.text

    # Either path converges on the rendered list — arriving on the page re-reads it,
    # and the knock push the hook fires re-reads it on an already-mounted page — so
    # the wait is on the rendered state, never on which of the two ran.
    app.contacts.navigate()

    expected = _short_id(sender_id)

    def sender_texts():
        texts = []
        for i in range(app.driver.count("knock-sender")):
            try:
                texts.append(app.contacts.knock_sender(i))
            except Exception:  # noqa: BLE001 — the list re-rendered under the read.
                # Not swallowed: the next poll reads the whole list again, and a
                # read that never succeeds times out into _diagnose below.
                return None
        return texts

    def _diagnose():
        texts = sender_texts() or []
        if sender_id in texts:
            shape = "the FULL sender hex, not the shared short id"
        elif summary in texts:
            shape = "the knock's SUMMARY in the sender element"
        elif any(t.startswith(sender_id[:12]) for t in texts):
            shape = "a truncation of the sender that is not the shared short_id"
        elif not texts:
            shape = "no knock-sender at all — the page never listed the stored knock"
        else:
            shape = "knock rows, none of them naming this sender"
        return (
            f"knock-sender never read {expected!r} for the stored knock from "
            f"{sender_id}: {shape}. knock-sender texts={texts!r}; "
            f"knock-card count={app.contacts.knock_count()}; "
            f"{app.driver.diagnose('knock-sender')}"
        )

    texts = wait_until(
        lambda: (lambda t: t if t and expected in t else None)(sender_texts()),
        budgets.PUSH_REFRESH_S,
        diagnose=_diagnose,
    )

    # The general invariant, over every knock on the page (convention 17): each
    # sender element holds a short id, so no row shows a full hex or a summary.
    off_shape = [t for t in texts if not _SHORT_ID.fullmatch(t)]
    assert not off_shape, (
        f"every knock-sender must read fauna_core::format::short_id of its sender; "
        f"these do not: {off_shape!r} (all: {texts!r})"
    )

    # And every sender sits in a knock-card — the indexed row container ui.yaml
    # declares, and the id `test_knock_live_refresh.py` counts. An app that paints
    # a row's children but not the row's own id lists the knock to a user and
    # answers `count("knock-card") == 0` to every test; linux's and windows' rows
    # did, and this test — reading only the children — passed on both throughout.
    def cards_match_senders():
        cards, senders = app.contacts.knock_count(), app.driver.count("knock-sender")
        return (cards, senders) if cards == senders else None

    wait_until(
        cards_match_senders,
        budgets.PUSH_REFRESH_S,
        diagnose=lambda: (
            f"every knock-sender must sit in a knock-card, but knock-card "
            f"count={app.contacts.knock_count()} against knock-sender "
            f"count={app.driver.count('knock-sender')}: the rows render without "
            f"their container id. {app.driver.diagnose('knock-card')}"
        ),
    )
