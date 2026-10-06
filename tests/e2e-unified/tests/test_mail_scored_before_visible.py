"""tier_3 — S5 scored-before-visible serve gate (Phase-3 design D5;
`content-scoring.md` § Timing: scored-before-visible, ratified 2026-07-07).

**Arm 2 — the serve-time gate (the all-boxes GUARANTEE).** A per-user scorer
whose output gates presentation (here: spam disposition) MUST complete before
the user first sees the message. The AUTH'd MDA session is the gate.

A recon fact shapes both tests here (verified 2026-07-08): in fauna, **inbound
SMTP delivery does NOT wake an IMAP IDLE session** — the ingest path emits only
`PushEvent::MailReceived` (the native-client arrival push), never the IMAP
`BridgeMailboxState` append push. The IMAP mailbox-state append push fires only
on IMAP-side mutations (APPEND/COPY into INBOX). A client discovers newly
*delivered* mail via its ≤30 s re-SELECT poll (the documented correctness
backstop, `bridge_routing_handlers.rs` inbound path). So the two announce paths
by which a NEW INBOX message reaches a client are:

  1. **re-SELECT** (how inbound mail is discovered) — gated by `scoreSelectedInbox`
     at `select.go` before the `select_mailbox` snapshot. THE inbound guarantee.
  2. **the IDLE append push** (an APPEND/COPY into INBOX by another session) —
     now gated by the same pass in the `idle.go` `MailboxStateEventAppend`
     branch before the EXISTS emit (S5 Arm 2 this session).

(`Session.Poll` is a no-op, and per IMAP EXISTS-monotonicity a FETCH can't reveal
a message beyond the announced count — so 1 + 2 are the whole announce surface.)

Test 1 proves path 1 (the real inbound scored-before-visible guarantee end-to-end
over real inbound SMTP + seal). Test 2 proves path 2 (the new IDLE append gate).

The model is seeded straight into `spam_models`, sealed to the recipient's own
key as every model rests (`_seed_spam_model`, idempotent) — a fixture precondition per the E2E rules' carve-out (b), not the mutation under
test (the real inbound delivery / IMAP APPEND + the gate's scoring). nest +
bridge + crypto are all real; only the benign perimeter AV daemons are fakes.
Fresh session-shared MSEK recipient (`mail_bridge_inbound_to_imap`), so a unique
per-message body nonce disambiguates each message and Junk-watermark deltas
isolate each test's contribution.

(Arm 1 — the grant-box ingest-drain fast path — is proven separately once the
ingest-notify trigger + labeler-obligation seeding land; tracked internally,
§ S5.)
"""

import sqlite3
import time

import pytest

from helpers.mail_wire import (
    _connect_smtp_starttls,
    _imap_append,
    _imap_auth_plain,
    _imap_cmd,
    _imap_read_tagged,
    _imap_uid_search,
    _imaps_connect,
    _recv_line,
)

pytestmark = pytest.mark.tier_3

SPAM_TOKEN = "sbvspamtokenqz"
HAM_TOKEN = "sbvhamtokenqz"


def _junk_watermarked_count(db_path: str, actor_id: bytes) -> int:
    """Rows this actor carries in Junk bearing the `$FaunaSpamScored` watermark —
    the durable nest-rest signal that the per-user scoring pass re-filed them."""
    conn = sqlite3.connect(f"file:{db_path}?mode=ro", uri=True)
    try:
        return conn.execute(
            "SELECT COUNT(*) FROM bridge_imap_messages "
            "WHERE actor_id = ?1 AND mailbox = 'Junk' AND flags LIKE '%$FaunaSpamScored%'",
            (actor_id,),
        ).fetchone()[0]
    finally:
        conn.close()


def _deliver_inbound(handle, raw_message: bytes, deadline: float) -> None:
    """Drive one real inbound SMTP MAIL/RCPT/DATA transaction through the MTA's
    port-25 STARTTLS listener to the fixture recipient (250 on `.` follows the
    synchronous `ingest_inbound_mail`)."""
    with _connect_smtp_starttls(handle.mx_port, handle.domain, deadline) as conn:
        conn.cmd("MAIL FROM:<sender@external.test>", "250", deadline)
        conn.cmd(f"RCPT TO:<{handle.recipient_username}>", "250", deadline)
        conn.cmd("DATA", "354", deadline)
        conn.send_raw(raw_message)
        conn.cmd(".", "250", deadline)
        conn.cmd("QUIT", "221", deadline)


def _spam_message(recipient_username: str, nonce: str) -> bytes:
    """A trained-spam message: SPAM_TOKEN drives the per-user score past the
    `spam_folder` threshold; the nonce uniquely identifies it in a shared INBOX."""
    return (
        "\r\n".join(
            [
                "From: External Spammer <sender@external.test>",
                f"To: {recipient_username}",
                f"Subject: scored-before-visible spam {nonce}",
                f"Message-ID: <{nonce}@external.test>",
                "Date: Mon, 06 Jul 2026 12:00:00 +0000",
                "MIME-Version: 1.0",
                "Content-Type: text/plain; charset=utf-8",
                "",
                f"Act now: the {SPAM_TOKEN} offer expires today ({nonce}).",
            ]
        )
        + "\r\n"
    ).encode()


def _seed_recipient_model(nest_instance, recipient, seal_helper_binary) -> None:
    from conftest import _seed_spam_model

    _seed_spam_model(
        db_path=nest_instance["db_path"],
        actor_id=recipient.actor_id,
        seal_helper_binary=seal_helper_binary,
        ngrams={SPAM_TOKEN: (110, 0), HAM_TOKEN: (0, 110)},
        spam_messages=110,
        ham_messages=110,
    )


@pytest.mark.feature("spam")
def test_inbound_spam_scored_before_visible_via_select_poll(
    mail_bridge_inbound_to_imap, nest_instance, seal_helper_binary,
):
    """Path 1 (THE inbound guarantee): a spam message delivered by real inbound
    SMTP is re-filed INBOX→Junk by the SELECT-time gate on the client's next
    (re-)SELECT — its FIRST view of INBOX already excludes it. The client polls
    to discover inbound mail (fauna's inbound path does not wake IDLE), and that
    poll IS a SELECT, so the spam is never visible in INBOX un-scored.
    """
    handle = mail_bridge_inbound_to_imap
    recipient = handle.recipient
    assert recipient is not None, "inbound fixture must expose the MSEK recipient"
    deadline = time.monotonic() + 120.0
    handle.assert_mta_running()
    _seed_recipient_model(nest_instance, recipient, seal_helper_binary)

    nonce = f"sbvselect{int(time.time() * 1000)}qx"
    _deliver_inbound(handle, _spam_message(handle.recipient_username, nonce), deadline)

    sock, buf = _imaps_connect(handle.mda, deadline)
    with sock:
        assert _imap_auth_plain(
            sock, buf, "p1", handle.recipient_username, handle.recipient_password, deadline
        ) == "OK", "AUTH PLAIN must succeed for the recipient"

        # The client's poll: a read-write SELECT INBOX runs scoreSelectedInbox
        # BEFORE the select_mailbox snapshot, so the spam is already re-filed.
        # (Give inbound placement a moment to propagate, then poll the SELECT —
        # re-SELECT is idempotent under the $FaunaSpamScored watermark.)
        junk_hits = []
        inbox_hits = ["<pending>"]
        poll_deadline = time.monotonic() + 40.0
        while time.monotonic() < poll_deadline and not junk_hits:
            st, _ = _imap_cmd(sock, buf, "p2", "SELECT INBOX", deadline)
            assert st == "OK", f"SELECT INBOX must succeed; got {st}"
            _, inbox_hits = _imap_uid_search(sock, buf, "p3", f'BODY "{nonce}"', deadline)
            st, _ = _imap_cmd(sock, buf, "p4", "SELECT Junk", deadline)
            assert st == "OK", f"SELECT Junk must succeed; got {st}"
            _, junk_hits = _imap_uid_search(sock, buf, "p5", f'BODY "{nonce}"', deadline)
            if not junk_hits:
                time.sleep(1.0)

        assert junk_hits, (
            f"the inbound spam ({nonce}) must be re-filed to Junk by the SELECT-time "
            f"gate — the client's poll never surfaces it un-scored in INBOX "
            f"(MDA log {handle.mda.log_file})"
        )
        assert not inbox_hits, (
            f"the inbound spam ({nonce}) must be GONE from INBOX after the SELECT-time "
            f"pass re-filed it (scored before visible); still matched {sorted(inbox_hits)}"
        )
        sock.sendall(b"p9 LOGOUT\r\n")


@pytest.mark.feature("spam")
def test_idle_gate_scores_appended_spam_before_announce(
    mail_bridge_inbound_to_imap, nest_instance, seal_helper_binary,
):
    """Path 2 (the new IDLE append gate): a spam message an IMAP APPEND lands in
    INBOX while a client IDLEs is scored + re-filed INBOX→Junk BEFORE the IDLE'd
    session is announced the new message. session-1 IDLEs and NEVER re-SELECTs;
    session-2 APPENDs the spam (an APPEND — unlike inbound SMTP — DOES emit the
    mailbox-state append push that wakes session-1). The IDLE append handler runs
    scoreSelectedInbox before the EXISTS emit, so the +1 Junk-watermark delta is
    attributable solely to the IDLE gate.
    """
    handle = mail_bridge_inbound_to_imap
    recipient = handle.recipient
    db_path = nest_instance["db_path"]
    deadline = time.monotonic() + 120.0
    _seed_recipient_model(nest_instance, recipient, seal_helper_binary)

    idle_sock, idle_buf = _imaps_connect(handle.mda, deadline)
    with idle_sock:
        assert _imap_auth_plain(
            idle_sock, idle_buf, "i1", handle.recipient_username, handle.recipient_password, deadline
        ) == "OK", "AUTH PLAIN must succeed (session 1)"
        idle_sock.sendall(b"i2 SELECT INBOX\r\n")
        sel_status, _ = _imap_read_tagged(idle_sock, idle_buf, "i2", deadline)
        assert sel_status == "OK", f"SELECT INBOX must succeed; got {sel_status}"

        # Baseline AFTER session-1's initial SELECT; from here session-1 ONLY
        # IDLEs (never a second SELECT INBOX), so any Junk move is the IDLE gate.
        before = _junk_watermarked_count(db_path, recipient.actor_id)

        idle_sock.sendall(b"i3 IDLE\r\n")
        cont = _recv_line(idle_sock, idle_buf, deadline)
        assert cont.startswith("+"), f"expected IDLE continuation, got {cont!r}"

        # Session 2 APPENDs a trained-spam message to the same INBOX — the append
        # push wakes session-1's IDLE gate.
        nonce = f"sbvidle{int(time.time() * 1000)}qx"
        push_sock, push_buf = _imaps_connect(handle.mda, deadline)
        with push_sock:
            assert _imap_auth_plain(
                push_sock, push_buf, "s1", handle.recipient_username, handle.recipient_password, deadline
            ) == "OK", "AUTH PLAIN must succeed (session 2)"
            status, _ = _imap_append(
                push_sock, push_buf, "s2", "INBOX", _spam_message(handle.recipient_username, nonce), deadline
            )
            assert status == "OK", f"second-session APPEND must succeed; got {status}"
            push_sock.sendall(b"s3 LOGOUT\r\n")

        # The IDLE append gate scores the APPEND'd spam and re-files it to Junk +
        # watermark — with NO re-SELECT on session-1. Poll the nest-rest delta.
        db_deadline = time.monotonic() + 40.0
        while (
            time.monotonic() < db_deadline
            and _junk_watermarked_count(db_path, recipient.actor_id) <= before
        ):
            time.sleep(0.5)
        after = _junk_watermarked_count(db_path, recipient.actor_id)
        assert after == before + 1, (
            "the IDLE append serve-gate must score the APPEND'd spam and re-file it "
            "INBOX→Junk (stamping $FaunaSpamScored) BEFORE announcing it. session-1 "
            "issued NO SELECT INBOX after entering IDLE, so a Junk-watermark delta of "
            f"+1 is attributable only to the IDLE gate (before={before}, after={after}; "
            f"MDA log {handle.mda.log_file})."
        )

        idle_sock.sendall(b"DONE\r\n")
        done_status, _ = _imap_read_tagged(idle_sock, idle_buf, "i3", deadline)
        assert done_status == "OK", f"IDLE DONE must complete OK; got {done_status}"
        idle_sock.sendall(b"i4 LOGOUT\r\n")
