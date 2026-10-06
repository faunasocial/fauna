"""tier_3: mail the nest files as junk never shows in Conversations.

A witness for ``docs/features/email-in-conversations.md`` outcome 13, and the
scope rule ``docs/goal/behavior/mail-app-surface.md`` § Inbound client receive
states: the conversations feed is ``INBOX`` only — *"`Junk` is excluded (spam
doesn't belong in the conversations view)"* — and a push for a Junk-routed
arrival self-filters on the client.

**Why a second witness.** The same outcome is witnessed by
``test_mail_client_spam_receive.py::test_client_scores_inbound_spam_to_junk``,
where the *app's own* scorer opens a message from INBOX, judges it spam, hides
it from the view and moves it to Junk (``suppress_from_view``). Mail that is
already in Junk before the app ever sees it — filed at delivery by the nest —
is kept out by a different mechanism entirely: the app polls only the INBOX and
Sent feeds, so such a message is never fetched at all. A regression that
widened what the app polls would pass the scorer's witness and break this one.

**How the mail is filed (e2e rule 8b).** The account holds a delivery-time
filter rule that files one sender's mail into Junk — the precondition, written
over the account's own ``fauna.email.filters.create``, the kind an app's filter
editor issues. What the journey observes is only what the user sees: the
junk-filed message delivered first, an ordinary one after it, and the ordinary
one showing while the junk one never does. The nest's placement is read back
from its message store so the test cannot pass because nothing was filed.

**Convention 14.** The ordinary message is delivered after the junk one and
waited for as list state; the junk message's absence is read once that later
message shows, so it is ordered by the mail, never by a clock.

tier_3: every binary real, the real SMTP wire and delivery-time filter.
"""

from __future__ import annotations

import sqlite3
import time
import uuid

import pytest

from clients.ws_rpc_admin_client import WsRpcAdminClient
from helpers.budgets import MLS_HANDSHAKE_S
from helpers.mail_client_ui import (
    deliver_inbound,
    plain_message,
    thread_with_nonce,
    wait_for_thread_with_nonce,
)
from helpers.mail_aliases import add_exact_alias

pytestmark = [
    pytest.mark.tier_3,
    pytest.mark.web,
    pytest.mark.linux,
    pytest.mark.windows,
    pytest.mark.macos,
    pytest.mark.ios,
    pytest.mark.android,
    pytest.mark.tui,
    pytest.mark.real_conversations,
]

JUNK_SENDER_DOMAIN = "junkmail.test"
JUNK_SENDER = f"offers@{JUNK_SENDER_DOMAIN}"
ORDINARY_SENDER = "sender@external.test"


@pytest.mark.feature("email-in-conversations")
def test_mail_filed_as_junk_on_arrival_never_shows_in_conversations(
    app, request, nest_instance, mail_bridge_mta
):
    from conftest import _login_app_as, _make_user

    user = _make_user(nest_instance)
    _login_app_as(app, request, nest_instance, user, verify_live_actor=True)

    app.mail_settings.navigate()
    app.mail_settings.ensure_mail_enabled()
    recipient = add_exact_alias(
        nest_instance["url"], user["signing_key"], mail_bridge_mta.domain,
        f"e2ejunk{uuid.uuid4().hex[:8]}",
    )

    # ── The precondition: this account files one sender's mail as junk.
    with WsRpcAdminClient(
        nest_instance["url"],
        actor_id=user["actor_id_bytes"],
        signing_key=bytes(user["signing_key"]),
    ) as account:
        account.call(
            "fauna.email.filters.create",
            {
                "name": "file offers as junk",
                "rules": [{"SenderDomain": {"domain": JUNK_SENDER_DOMAIN}}],
                "combination": "all",
                "action": {"FileInto": {"mailbox": "Junk"}},
                "priority": 0,
            },
        )

    # ── The junk-filed message first, an ordinary one after it.
    junk_nonce = f"junk{uuid.uuid4().hex[:10]}qx"
    ordinary_nonce = f"ordinary{uuid.uuid4().hex[:10]}qx"
    for sender, nonce in ((JUNK_SENDER, junk_nonce), (ORDINARY_SENDER, ordinary_nonce)):
        deliver_inbound(
            mail_bridge_mta.mx_port, mail_bridge_mta.domain, sender, recipient,
            plain_message(sender, recipient, nonce), time.monotonic() + 40.0,
        )

    placed = _placements(nest_instance, user["actor_id_bytes"])
    assert placed.get(JUNK_SENDER_DOMAIN) == "Junk", (
        f"the filter must have filed the {JUNK_SENDER_DOMAIN} message into Junk, or "
        f"this run witnesses nothing; placements={placed}"
    )
    assert placed.get("external.test") == "INBOX", f"placements={placed}"

    # ── What the user sees.
    app.conversations.navigate()
    wait_for_thread_with_nonce(
        app, ordinary_nonce, what="the ordinary message delivered after the junk one",
        budget_s=MLS_HANDSHAKE_S, bridge_log=mail_bridge_mta.log_file,
    )
    assert thread_with_nonce(app, junk_nonce) is None, (
        "mail filed as junk on arrival must never show in Conversations; it shows "
        f"as {thread_with_nonce(app, junk_nonce)!r}"
    )


def _placements(nest_instance, actor_id: bytes) -> dict[str, str]:
    """``{sender domain: mailbox}`` for every message the nest placed for the
    account — the store IMAP SELECT reads."""
    conn = sqlite3.connect(nest_instance["db_path"], timeout=10.0)
    try:
        rows = conn.execute(
            "SELECT from_norm, mailbox FROM bridge_imap_messages WHERE actor_id = ?1",
            (actor_id,),
        ).fetchall()
    finally:
        conn.close()
    return dict(rows)
