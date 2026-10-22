"""tier_3: a received message shows its own malware and spam verdict.

A witness for ``docs/features/email-in-conversations.md`` outcome 17 and
``docs/goal/behavior/mail-content-scanning.md`` § User sees their own message's
row: the bubble of a mail message the user received carries a
``dm-message-scan-result`` row — the ClamAV verdict, the rspamd score with the
rules that fired, and what the nest did with the message — and nothing else on
the thread carries one.

**How the verdict is made.** The session MTA runs the fake clamd and rspamd
(conftest's ``mail_bridge_mta``): a benign body scans clean, and the message's
``X-Test-Rspamd-Score`` header names the score the fake rspamd answers, beside
its fixed ``BAYES_HAM`` / ``MIME_GOOD`` rules. The nest writes the scan row in
the same ingest the ``250`` to ``DATA`` waits on.

**Never anyone else's.** The app reads the verdict through
``fauna.bridges.get_message_scan_result``, which answers only the caller's own
message; the test asks the same read as a second account for the message the
first one received and expects the nest's one ``not_found`` — the read the
surface is built on cannot reach another user's row (rule 5's API half; the
full two-user wire is ``tests/api/test_mail_scan_result_user_read.py``).

**Convention 14.** The row is waited for as UI state once the thread is open;
nothing is timed.

tier_3: every binary real, the real SMTP wire and scan pipeline.
"""

from __future__ import annotations

import time
import uuid

import pytest

from clients.ws_rpc_admin_client import RpcCallError, WsRpcAdminClient
from helpers.budgets import MLS_HANDSHAKE_S
from helpers.mail_aliases import add_exact_alias
from helpers.mail_client_ui import deliver_inbound, wait_for_thread_with_nonce
from helpers.waiting import wait_until
from i18n.strings import S

pytestmark = [
    pytest.mark.tier_3,
    pytest.mark.tui,
    pytest.mark.web,
    pytest.mark.linux,
    pytest.mark.android,
    pytest.mark.macos,
    pytest.mark.ios,
    pytest.mark.real_conversations,
]

SENDER = "sender@external.test"
RSPAMD_SCORE = "4.2"
NOT_FOUND = "fauna.bridges.not_found"


def _message(recipient: str, nonce: str) -> bytes:
    return (
        "\r\n".join(
            [
                f"From: External Sender <{SENDER}>",
                f"To: {recipient}",
                f"Subject: Scanned {nonce}",
                f"Message-ID: <{nonce}@external.test>",
                "Date: Mon, 21 Sep 2026 12:00:00 +0000",
                f"X-Test-Rspamd-Score: {RSPAMD_SCORE}",
                "MIME-Version: 1.0",
                "Content-Type: text/plain; charset=utf-8",
                "",
                f"The {nonce} body.",
            ]
        )
        + "\r\n"
    ).encode()


@pytest.mark.feature("email-in-conversations")
def test_a_received_message_shows_its_own_scan_verdict(
    app, request, nest_instance, mail_bridge_mta
):
    from conftest import _login_app_as, _make_user

    user = _make_user(nest_instance)
    _login_app_as(app, request, nest_instance, user, verify_live_actor=True)
    app.mail_settings.navigate()
    app.mail_settings.ensure_mail_enabled()
    recipient = add_exact_alias(
        nest_instance["url"], user["signing_key"], mail_bridge_mta.domain,
        f"e2escan{uuid.uuid4().hex[:8]}",
    )

    nonce = f"scan{uuid.uuid4().hex[:10]}qx"
    deliver_inbound(
        mail_bridge_mta.mx_port, mail_bridge_mta.domain, SENDER, recipient,
        _message(recipient, nonce), time.monotonic() + 40.0,
    )

    # ── What the user sees: open the thread, the verdict shows on the bubble.
    app.conversations.navigate()
    thread = wait_for_thread_with_nonce(
        app, nonce, what="the scanned message", budget_s=MLS_HANDSHAKE_S,
        bridge_log=mail_bridge_mta.log_file,
    )
    app.conversations.open_thread_by_id(thread.thread_id)
    rows = wait_until(
        lambda: app.conversations.scan_result_rows() or None,
        MLS_HANDSHAKE_S,
        diagnose=lambda: (
            f"the received message tagged {nonce!r} never showed its scan verdict "
            f"(dm-message-scan-result) once its thread opened.\n"
            f"  conversations error: {app.error_text()!r}\n"
            f"  bridge log: {mail_bridge_mta.log_file}"
        ),
    )
    assert len(rows) == 1, f"one received message, one verdict row; got {rows!r}"
    expected = [
        S.conversations.scan_result.clamav_clean,
        S.conversations.scan_result.rspamd_score_rules(
            score=RSPAMD_SCORE, rules="BAYES_HAM, MIME_GOOD"
        ),
        S.conversations.scan_result.action_delivered,
    ]
    for line in expected:
        assert line in rows[0], f"the verdict row must say {line!r}; it says {rows[0]!r}"

    # ── Never anyone else's: the read under the row refuses a second account.
    with WsRpcAdminClient(
        nest_instance["url"],
        actor_id=user["actor_id_bytes"],
        signing_key=bytes(user["signing_key"]),
    ) as owner:
        page = owner.call("fauna.email.inbox.fetch", {"after_uid": 0, "limit": 50})
    message_ids = [bytes(m["message_id"]) for m in page.get("messages", [])]
    assert len(message_ids) == 1, f"the one delivery is the account's INBOX: {page!r}"

    other = _make_user(nest_instance)
    with WsRpcAdminClient(
        nest_instance["url"],
        actor_id=other["actor_id_bytes"],
        signing_key=bytes(other["signing_key"]),
    ) as stranger:
        with pytest.raises(RpcCallError) as refused:
            stranger.call(
                "fauna.bridges.get_message_scan_result", {"message_id": message_ids[0]}
            )
    assert refused.value.code == NOT_FOUND, (
        f"another account must never read this message's verdict: {refused.value}"
    )
