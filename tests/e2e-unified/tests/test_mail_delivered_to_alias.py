"""tier_3: a received message says which of the user's aliases it arrived on.

A witness for ``docs/features/mail-aliases.md`` outcome 16 and
``docs/goal/behavior/mail-aliases.md`` § Label: the bubble of a mail message
delivered on a labelled alias carries a ``dm-message-delivered-to-alias`` tag,
"Delivered to: <address> [<label>]", and nothing else on the thread carries
one.

**How the tag is made.** The nest's recipient resolver stamps the envelope
address it delivered to (``X-Fauna-Delivered-To``) on the mailbox copy; the app
keeps the stamp when it opens the message and resolves it against the
account's own alias rows (``fauna.bridges.list_account_aliases``), read when
the thread opens — the shared ``delivered_alias`` + ``delivered_to_text``.

**The stamp is the nest's, not the sender's.** The message's own ``To:`` names
a different address (as a Bcc'd or list delivery would), so a tag that read
``To:`` would name the wrong alias; and the message carries a forged
``X-Fauna-Delivered-To`` naming another address, which every filing door strips
before stamping, so a tag that trusted it would name that one.

**Convention 14.** The tag is waited for as UI state once the thread is open;
nothing is timed.

tier_3: every binary real, the real SMTP wire and recipient resolver.
"""

from __future__ import annotations

import time
import uuid

import pytest

from clients.ws_rpc_admin_client import WsRpcAdminClient
from helpers.budgets import MLS_HANDSHAKE_S
from helpers.mail_client_ui import deliver_inbound, wait_for_thread_with_nonce
from helpers.waiting import wait_until
from i18n.strings import S

pytestmark = [
    pytest.mark.tier_3,
    pytest.mark.tui,
    pytest.mark.linux,
    pytest.mark.web,
    pytest.mark.real_conversations,
]

SENDER = "sender@external.test"
LABEL = "Online shops"


def _message(written_to: str, forged: str, nonce: str) -> bytes:
    return (
        "\r\n".join(
            [
                f"X-Fauna-Delivered-To: {forged}",
                f"From: External Sender <{SENDER}>",
                f"To: {written_to}",
                f"Subject: Tagged {nonce}",
                f"Message-ID: <{nonce}@external.test>",
                "Date: Mon, 21 Sep 2026 12:00:00 +0000",
                "MIME-Version: 1.0",
                "Content-Type: text/plain; charset=utf-8",
                "",
                f"The {nonce} body.",
            ]
        )
        + "\r\n"
    ).encode()


@pytest.mark.feature("mail-aliases")
def test_a_received_message_shows_the_alias_it_arrived_on(
    app, request, nest_instance, mail_bridge_mta
):
    from conftest import _login_app_as, _make_user

    user = _make_user(nest_instance)
    _login_app_as(app, request, nest_instance, user, verify_live_actor=True)
    app.mail_settings.navigate()
    app.mail_settings.ensure_mail_enabled()
    local = f"e2eshop{uuid.uuid4().hex[:8]}"
    recipient = f"{local}@{mail_bridge_mta.domain}"
    with WsRpcAdminClient(
        nest_instance["url"],
        actor_id=user["actor_id_bytes"],
        signing_key=bytes(user["signing_key"]),
    ) as member:
        member.call(
            "fauna.bridges.create_account_alias",
            {
                "kind": "exact",
                "local_domain": mail_bridge_mta.domain,
                "pattern": local,
                "controls": {"label": LABEL},
            },
        )

    nonce = f"tag{uuid.uuid4().hex[:10]}qx"
    deliver_inbound(
        mail_bridge_mta.mx_port, mail_bridge_mta.domain, SENDER, recipient,
        _message(
            f"someone-else@{mail_bridge_mta.domain}",
            f"forged@{mail_bridge_mta.domain}",
            nonce,
        ),
        time.monotonic() + 40.0,
    )

    # ── What the user sees: open the thread, the tag shows on the bubble.
    app.conversations.navigate()
    thread = wait_for_thread_with_nonce(
        app, nonce, what="the tagged message", budget_s=MLS_HANDSHAKE_S,
        bridge_log=mail_bridge_mta.log_file,
    )
    app.conversations.open_thread_by_id(thread.thread_id)
    tags = wait_until(
        lambda: app.conversations.delivered_to_alias_tags() or None,
        MLS_HANDSHAKE_S,
        diagnose=lambda: (
            f"the received message tagged {nonce!r} never showed the alias it "
            f"arrived on (dm-message-delivered-to-alias) once its thread opened.\n"
            f"  conversations error: {app.error_text()!r}\n"
            f"  bridge log: {mail_bridge_mta.log_file}"
        ),
    )
    expected = S.mail_aliases.delivered_to(address=recipient, label=LABEL)
    assert tags == [expected], (
        f"one received message, one tag naming the alias the nest delivered it "
        f"on and its label — never the sender's To: or a forged stamp; got {tags!r}"
    )
