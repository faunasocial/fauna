"""tier_3 API-helper: the two `User`-class WS-RPC surfaces the Fauna-app
on-device mail spam scorer depends on, exercised over the real wire from Python
(no browser) — the nest-side half of `test_mail_client_spam_receive.py`, so a
failure there is triaged UI-side vs nest-side (E2E § rule 5).

Both are new this track (`mail-spam.md` § Wire
shapes):

  * `fauna.bridges.get_spam_scoring_policy` (User) — the admin-effective
    `spam_folder` threshold + Bayesian confidence-ramp/weight knobs the on-device
    scorer must compare against (the "byte-identical scores at every position"
    rule). A `User` may read this non-secret deployment policy.
  * `fauna.email.apply_spam_disposition` (User) — the least-privilege re-file the
    client issues: watermark the scored UIDs with `$FaunaSpamScored`, then move
    the spam subset INBOX→Junk (watermark-before-move). The `BridgeMda`-only
    `fauna.bridges.{store_flags,move}` the MDA uses are unreachable to a client.

A fresh mail-enabled recipient receives exactly ONE real MTA-sealed message, so
its single INBOX UID is unambiguous: fetch it (no watermark yet), apply the
disposition marking it scored+junk, and assert directly against the nest DB that
the row moved INBOX→Junk carrying the watermark and that INBOX is now empty. Every
binary is real; the MTA seal + inbox-fetch + STORE-flags + apply_move all run for
real. (The *scoring* itself — model → verdict — is covered by the Rust unit tests
+ the UI e2e; this helper isolates the nest-side re-file wire.)

Taxonomy: tier_3 — real `fauna-nest` binary, real MTA seal, real WS-RPC User wire.
"""

import sqlite3
import time

import pytest

from helpers.mail_wire import _connect_smtp_starttls

pytestmark = pytest.mark.tier_3

SENDER_DOMAIN = "external.test"
SPAM_SCORED_KEYWORD = "$FaunaSpamScored"


def _deliver_one(mx_port: int, server_name: str, recipient_addr: str,
                 raw_message: bytes, deadline: float) -> None:
    with _connect_smtp_starttls(mx_port, server_name, deadline) as conn:
        conn.cmd(f"MAIL FROM:<sender@{SENDER_DOMAIN}>", "250", deadline)
        conn.cmd(f"RCPT TO:<{recipient_addr}>", "250", deadline)
        conn.cmd("DATA", "354", deadline)
        conn.send_raw(raw_message)
        conn.cmd(".", "250", deadline)
        conn.cmd("QUIT", "221", deadline)


def _rows(db_path: str, actor_id: bytes):
    conn = sqlite3.connect(db_path, timeout=10.0)
    try:
        return conn.execute(
            "SELECT mailbox, uid, flags FROM bridge_imap_messages WHERE actor_id = ?1",
            (actor_id,),
        ).fetchall()
    finally:
        conn.close()


def test_get_spam_scoring_policy_returns_effective_defaults(nest_instance):
    """A `User` may read the admin-effective spam-scoring policy; with no admin
    override it is the catalog default (`spam_folder` 5 → 5000 milli + the default
    Bayesian knobs), the exact values the conformance test pins nest-side and the
    web scorer scores against."""
    from clients.ws_rpc_admin_client import WsRpcAdminClient
    from common.auth import create_actor_and_register

    user = create_actor_and_register(
        nest_instance["port"], admin_signing_key=nest_instance["admin"]["signing_key"]
    )
    client = WsRpcAdminClient(
        nest_instance["url"],
        actor_id=user["actor_id_bytes"],
        signing_key=bytes(user["signing_key"]),
    )
    with client:
        policy = client.call("fauna.bridges.get_spam_scoring_policy", {})
    assert policy["spam_folder_threshold"] == 5, policy
    assert policy["bayesian_weight_milli"] == 700, policy
    assert policy["bayesian_min_samples"] == 50, policy
    assert policy["bayesian_full_confidence_samples"] == 200, policy


@pytest.mark.feature("spam")
def test_apply_spam_disposition_watermarks_and_moves_to_junk(
    nest_instance, mail_bridge_mta, seal_helper_binary
):
    """The `User`-class re-file: a delivered INBOX message the client scores as
    spam is watermarked then moved INBOX→Junk, and INBOX is left empty."""
    from clients.ws_rpc_admin_client import WsRpcAdminClient
    from conftest import _provision_msek_recipient

    recip = _provision_msek_recipient(
        nest_instance=nest_instance,
        seal_helper_binary=seal_helper_binary,
        domain=mail_bridge_mta.domain,
        local_part="apply-disp",
        password="apply-disp-password-1",
    )
    actor_id = recip.actor_id
    db_path = nest_instance["db_path"]

    # ── Deliver exactly one real MTA-sealed message to the fresh recipient's INBOX.
    nonce = f"applydisp{int(time.time() * 1000)}"
    message = (
        f"From: External Sender <sender@{SENDER_DOMAIN}>\r\n"
        f"To: {recip.username}\r\n"
        f"Subject: disposition target {nonce}\r\n"
        f"Message-ID: <{nonce}@{SENDER_DOMAIN}>\r\n"
        "Date: Mon, 25 May 2026 12:00:00 +0000\r\n"
        "\r\n"
        f"The {nonce} body is the re-file target.\r\n"
    ).encode()
    _deliver_one(mail_bridge_mta.mx_port, mail_bridge_mta.domain, recip.username,
                 message, time.monotonic() + 40.0)

    client = WsRpcAdminClient(
        nest_instance["url"],
        actor_id=actor_id,
        signing_key=bytes(recip.recipient["signing_key"]),
    )
    with client:
        # ── Fetch the single INBOX message; it is not yet watermarked.
        deadline = time.monotonic() + 30.0
        messages = []
        while time.monotonic() < deadline and not messages:
            reply = client.call("fauna.email.inbox.fetch", {"after_uid": 0, "limit": 50})
            messages = reply.get("messages", [])
            if not messages:
                time.sleep(0.5)
        assert len(messages) == 1, (
            f"expected exactly one delivered INBOX message, got {len(messages)}: "
            f"{[(m.get('uid'), m.get('flags')) for m in messages]}"
        )
        uid = messages[0]["uid"]
        assert SPAM_SCORED_KEYWORD not in (messages[0].get("flags") or []), (
            "a freshly delivered message must not yet carry the scoring watermark; "
            f"flags={messages[0].get('flags')!r}"
        )

        # ── Apply the on-device scorer's outcome: mark this UID scored + junk.
        disp = client.call(
            "fauna.email.apply_spam_disposition",
            {"scored_uids": [uid], "junk_uids": [uid]},
        )
        assert disp["watermarked"] == 1, disp
        assert disp["moved_to_junk"] == 1, disp

        # ── INBOX is now empty over the wire (the message left it).
        reply = client.call("fauna.email.inbox.fetch", {"after_uid": 0, "limit": 50})
        assert reply.get("messages", []) == [], (
            f"the moved message must be gone from INBOX; still returned "
            f"{[(m.get('uid'), m.get('flags')) for m in reply.get('messages', [])]}"
        )

    # ── Nest-DB ground truth: the actor's single row is in Junk with the watermark.
    rows = _rows(db_path, actor_id)
    assert len(rows) == 1, f"expected one placement row for the actor, got {rows}"
    mailbox, _uid, flags = rows[0]
    assert mailbox == "Junk", f"the scored-spam row must be in Junk, not {mailbox!r}"
    assert SPAM_SCORED_KEYWORD in flags, (
        f"the moved row must carry the {SPAM_SCORED_KEYWORD} watermark; flags={flags!r}"
    )
