"""tier_3 — the shared report aggregate applied to mail at the MDA's scoring
position, end to end over real binaries (`docs/goal/behavior/report-sharing.md`
§ Applying the aggregate — the user's own threshold; `mail-spam.md`
§ Combined-score formula, § Re-file timing).

ONE journey proves all of spam outcome 26, over the real MTA, the real MDA and a
real nest, four people on one domain:

1. One message is delivered by real inbound SMTP to all four in one transaction,
   so every copy carries the same canonical report-hash (an IMAP APPEND stores
   none and could not aggregate).
2. The fourth person has set their own account threshold to 3 before delivery
   (the stamp folds at RCPT), and their first SELECT INBOX scores their copy
   with a model that calls it ham: watermarked `$FaunaSpamScored`, kept in INBOX.
3. The other three opt in to report sharing and move their copies to Junk in
   their mail app — three spam lessons, three reports. The third one reaches
   k = 3: the aggregate lands on every copy's bus row (200 per mille) and the
   nest RE-OPENS the fourth person's copy (the watermark is removed).
4. The fourth person's next SELECT INBOX scores the copy again with the report
   term: 3 points at their own line of 3 — filed to Junk.
5. Reporter 1 moves theirs back out of Junk (a ham lesson): their report is
   withdrawn (2 < k), the bus row and the published entry go, and their copy is
   stamped `$NotJunk` and stays in INBOX over two more SELECTs.

Fixture preconditions (carve-out (b) of the E2E rules): the fourth person's
sealed model is seeded (`_seed_spam_model`) so their first pass watermarks the
copy; every gesture under test — delivery, SELECT, MOVE, the opt-in — is real.
Every step is read off state a tagged response already settled (the training
signal, the report capture and the re-open run inside the MOVE's handler, the
scoring pass before SELECT's snapshot) — convention 14, no waits.
"""

import secrets
import sqlite3
import time

import pytest

from conftest import _provision_msek_recipient, _seed_spam_model
from clients.ws_rpc_admin_client import WsRpcAdminClient
from helpers.mail_wire import ImapSession, _connect_smtp_starttls
from tests.api import ws_api

pytestmark = pytest.mark.tier_3

HAM_TOKEN = "rptaggrhamtokenqz"
SPAM_TOKEN = "rptaggrspamtokenqz"
WATERMARK = "$FaunaSpamScored"
NOT_JUNK = "$NotJunk"


def _fresh(mda, nest_instance, seal_helper_binary, tag):
    local = f"rpt{tag}{secrets.token_hex(3)}"
    return _provision_msek_recipient(
        nest_instance=nest_instance, seal_helper_binary=seal_helper_binary,
        domain=mda.domain, local_part=local, password=f"{local}-password-1")


def _set_account_threshold(nest_instance, recipient, value):
    with WsRpcAdminClient(
        nest_instance["url"], actor_id=recipient.actor_id,
        signing_key=bytes(recipient.recipient["signing_key"]),
    ) as client:
        client.call("fauna.bridges.set_spam_threshold_override",
                    {"spam_threshold_override": value})


def _ro(db_path):
    return sqlite3.connect(f"file:{db_path}?mode=ro", uri=True)


def _copy(db_path, actor_id, nonce_uid, mailbox):
    """(message_id, flag set) of one placement, read at nest-disk rest."""
    conn = _ro(db_path)
    try:
        row = conn.execute(
            "SELECT message_id, flags FROM bridge_imap_messages "
            "WHERE actor_id = ? AND mailbox = ? AND uid = ?",
            (actor_id, mailbox, nonce_uid),
        ).fetchone()
    finally:
        conn.close()
    assert row is not None, f"no {mailbox} placement uid={nonce_uid}"
    return bytes(row[0]), set(row[1].split())


def _report_hash(db_path, message_id):
    conn = _ro(db_path)
    try:
        row = conn.execute(
            "SELECT report_hash FROM segment_records "
            "WHERE kind = 'mail' AND substr(record_cid, 5) = ?", (message_id,),
        ).fetchone()
    finally:
        conn.close()
    return None if row is None or row[0] is None else bytes(row[0])


def _bus_report_score(db_path, message_id):
    """The copy's `report:spam` bus-row score (per mille), or None — the same
    read as `tests/api/test_report_sharing.py::_bus_report_score`."""
    conn = _ro(db_path)
    try:
        row = conn.execute(
            "SELECT score FROM content_scores "
            "WHERE content_id = ? AND factor = 'report:spam'", (message_id,),
        ).fetchone()
    finally:
        conn.close()
    return None if row is None else row[0]


def _published(nest_instance, actor, hash_hex):
    status = ws_api.report_share_status(nest_instance["port"], actor)
    return next((e for e in status.get("published", []) if e["content_hash"] == hash_hex), None)


def _message(nonce):
    return (
        "\r\n".join([
            "From: Campaign Sender <sender@external.test>",
            "To: undisclosed-recipients:;",
            f"Subject: your parcel is held {nonce}",
            f"Message-ID: <{nonce}@external.test>",
            "Date: Mon, 06 Jul 2026 12:00:00 +0000",
            "MIME-Version: 1.0",
            "Content-Type: text/plain; charset=utf-8",
            "",
            f"Hello, {HAM_TOKEN}: your parcel is held, pay the fee ({nonce}).",
        ]) + "\r\n"
    ).encode()


def _deliver_to_all(handle, recipients, raw, deadline):
    with _connect_smtp_starttls(handle.mx_port, handle.domain, deadline) as conn:
        conn.cmd("MAIL FROM:<sender@external.test>", "250", deadline)
        for r in recipients:
            conn.cmd(f"RCPT TO:<{r.username}>", "250", deadline)
        conn.cmd("DATA", "354", deadline)
        conn.send_raw(raw)
        conn.cmd(".", "250", deadline)
        conn.cmd("QUIT", "221", deadline)


@pytest.mark.feature("spam")
def test_three_reports_file_the_message_at_your_own_line_and_not_spam_vetoes_it(
    mail_bridge_inbound_to_imap, nest_instance, seal_helper_binary,
):
    """Once three people who share reports have reported the same message, it
    scores as spam against the fourth person's own threshold too — the nest
    re-opens their already-scored copy and their own mail agent files it — and
    a not-spam mark withdraws the reporter's report and keeps their copy out of
    Junk for good."""
    handle = mail_bridge_inbound_to_imap
    handle.assert_mta_running()
    mda = handle.mda
    db = nest_instance["db_path"]
    deadline = time.monotonic() + 240.0

    reporters = [_fresh(mda, nest_instance, seal_helper_binary, f"r{i}") for i in range(3)]
    fourth = _fresh(mda, nest_instance, seal_helper_binary, "four")
    _seed_spam_model(
        db_path=db, actor_id=fourth.actor_id, seal_helper_binary=seal_helper_binary,
        ngrams={SPAM_TOKEN: (110, 0), HAM_TOKEN: (0, 110)},
        spam_messages=110, ham_messages=110,
    )
    _set_account_threshold(nest_instance, fourth, 3)

    nonce = f"rptaggr{int(time.time() * 1000)}qx"
    _deliver_to_all(handle, reporters + [fourth], _message(nonce), deadline)

    # (2) The fourth person's first pass: ham by their own model, watermarked,
    # still in INBOX.
    s4 = ImapSession(mda, fourth, deadline)
    try:
        [uid4] = s4.find("INBOX", nonce)
        mid4, flags4 = _copy(db, fourth.actor_id, uid4, "INBOX")
        assert WATERMARK in flags4, (
            f"the first SELECT must score + watermark the copy (flags {flags4}; "
            f"MDA log {mda.log_file})")
        report_hash = _report_hash(db, mid4)
        assert report_hash, "an MTA-delivered copy carries the canonical report-hash"
        hash_hex = report_hash.hex()

        # (3) Three opted-in reporters move their copies to Junk.
        reporter_uids = []
        for i, r in enumerate(reporters):
            assert ws_api.report_share_set(nest_instance["port"], r.recipient, True)["share"]
            s = ImapSession(mda, r, deadline)
            try:
                [uid] = s.find("INBOX", nonce)
                s.ok(f"UID MOVE {uid} Junk")
                [junk_uid] = s.find("Junk", nonce)
                reporter_uids.append(junk_uid)
            finally:
                s.close()
            expected = 200 if i == 2 else None
            assert _bus_report_score(db, mid4) == expected, (
                f"after {i + 1} report(s) the fourth copy's report:spam row must be "
                f"{expected} (k = 3)")

        # The k = 3 transition re-opened the fourth person's scored copy.
        _, flags4 = _copy(db, fourth.actor_id, uid4, "INBOX")
        assert WATERMARK not in flags4, (
            f"reaching k must remove the watermark from an INBOX copy without "
            f"$NotJunk (flags {flags4})")
        assert _published(nest_instance, fourth.recipient, hash_hex)["count"] == 3

        # (4) Their next SELECT files it: 3 points at their own line of 3.
        assert s4.find("INBOX", nonce) == [], (
            f"3 reports must file the message at the fourth person's threshold of 3 "
            f"(MDA log {mda.log_file})")
        assert len(s4.find("Junk", nonce)) == 1, "the re-filed copy is in their Junk"
    finally:
        s4.close()

    # (5) Reporter 1 moves theirs back out of Junk: the ham lesson.
    r1 = reporters[0]
    s1 = ImapSession(mda, r1, deadline)
    try:
        s1.ok("SELECT Junk")
        s1.ok(f"UID MOVE {reporter_uids[0]} INBOX")
        assert _bus_report_score(db, mid4) is None, (
            "a not-spam move withdraws the report: 2 < k, the bus row leaves")
        assert _published(nest_instance, r1.recipient, hash_hex) is None, (
            "the published list drops the entry below k")
        for _ in range(2):
            [uid1] = s1.find("INBOX", nonce)
            _, flags1 = _copy(db, r1.actor_id, uid1, "INBOX")
            assert NOT_JUNK in flags1, (
                f"a ham lesson stamps the copy $NotJunk (flags {flags1})")
        assert s1.find("Junk", nonce) == [], "a $NotJunk copy is never re-filed"
    finally:
        s1.close()
