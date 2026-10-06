"""Tier 3: what trains a user's spam filter, and what the scorer does with it —
witnessed from a standard IMAP mail app's side over the real MDA bridge and a
real nest (`docs/goal/behavior/mail-spam.md` § Training signal sources,
§ Scoring placement, § Cold start; `docs/goal/behavior/imap-server.md`
§ `\\Junk` flag-change ↔ spam-training contract).

Every recipient here is fresh (`_provision_msek_recipient`), so its
`spam_training_history` and `spam_models` rows start empty and the session-shared
fixtures' mailboxes are never perturbed. The training signal fires inline in the
STORE / COPY / MOVE handler before its tagged completion (`store.go`
`fireJunkTrainSignals`), and the SELECT-time scoring pass runs before SELECT's
snapshot (`spam_score.go`), so each assertion reads state already settled by a
tagged response — convention 14, no waits.
"""

import secrets
import sqlite3
import time

import pytest

from conftest import _provision_msek_recipient, _seed_spam_model
from clients.ws_rpc_admin_client import WsRpcAdminClient
from helpers.mail_wire import (
    _imap_append,
    _imap_auth_plain,
    _imap_cmd,
    _imap_uid_fetch_body,
    _imap_uid_search,
    _imaps_connect,
)

pytestmark = pytest.mark.tier_3


def _fresh(mda, nest_instance, seal_helper_binary, tag):
    local = f"sig{tag}{secrets.token_hex(3)}"
    return _provision_msek_recipient(
        nest_instance=nest_instance, seal_helper_binary=seal_helper_binary,
        domain=mda.domain, local_part=local, password=f"{local}-password-1")


def _history(db_path, actor_id):
    """The actor's lessons as (label, source), oldest first. The row's mailbox
    is a display detail the goal doc does not fix, so it is not compared."""
    conn = sqlite3.connect(f"file:{db_path}?mode=ro", uri=True)
    try:
        return conn.execute(
            "SELECT label, source FROM spam_training_history "
            "WHERE actor_id = ? ORDER BY created_at ASC, history_id ASC", (actor_id,),
        ).fetchall()
    finally:
        conn.close()


def _stored_model(db_path, actor_id):
    """The actor's ``spam_models.model_json`` at nest-disk rest (None = no row)."""
    conn = sqlite3.connect(f"file:{db_path}?mode=ro", uri=True)
    try:
        row = conn.execute(
            "SELECT model_json FROM spam_models WHERE actor_id = ?", (actor_id,),
        ).fetchone()
        return None if row is None else bytes(row[0])
    finally:
        conn.close()


def _msg(username, tag, body):
    return (
        b"From: sender@external.test\r\n"
        b"To: " + username.encode() + b"\r\n"
        b"Subject: signal " + tag.encode() + b"\r\n"
        b"Date: Wed, 15 May 2026 12:00:00 +0000\r\n"
        b"Message-ID: <signal-" + tag.encode() + b"-" + secrets.token_hex(4).encode()
        + b"@mda.fauna.test>\r\n"
        b"\r\n" + body.encode() + b"\r\n"
    )


class _Session:
    """One signed-in IMAP connection with a running tag counter."""

    def __init__(self, mda, recipient, deadline):
        self.deadline = deadline
        self.recipient = recipient
        self.sock, self.buf = _imaps_connect(mda, deadline)
        self.n = 0
        assert _imap_auth_plain(self.sock, self.buf, self._tag(), recipient.username,
                                recipient.password, deadline) == "OK", "AUTH PLAIN must succeed"

    def _tag(self):
        self.n += 1
        return f"s{self.n}"

    def ok(self, cmd):
        status, text = _imap_cmd(self.sock, self.buf, self._tag(), cmd, self.deadline)
        assert status == "OK", f"{cmd!r} must succeed; got {status}: {text}"
        return text

    def append(self, mailbox, tag, body):
        status, uid = _imap_append(self.sock, self.buf, self._tag(), mailbox,
                                   _msg(self.recipient.username, tag, body), self.deadline)
        assert status == "OK" and uid is not None, f"APPEND to {mailbox} must succeed; got {status}"
        return uid

    def search(self, criteria):
        status, hits = _imap_uid_search(self.sock, self.buf, self._tag(), criteria, self.deadline)
        assert status == "OK", f"UID SEARCH {criteria} must succeed; got {status}"
        return hits

    def close(self):
        try:
            self.sock.sendall(b"zz LOGOUT\r\n")
        finally:
            self.sock.close()


@pytest.mark.feature("spam")
def test_leaving_junk_in_a_mail_app_trains_not_spam(mail_bridge_mda, nest_instance,
                                                     seal_helper_binary):
    """Moving or copying a message out of Junk, or clearing its `\\Junk` flag, in
    a mail app teaches the filter the message is not spam — one `ham` lesson per
    gesture, attributed to the gesture that made it."""
    r = _fresh(mail_bridge_mda, nest_instance, seal_helper_binary, "ham")
    db = nest_instance["db_path"]
    s = _Session(mail_bridge_mda, r, time.monotonic() + 90.0)
    try:
        # APPEND never trains: the message arrives in Junk with no lesson.
        moved = s.append("Junk", "move", "moved out of junk")
        copied = s.append("Junk", "copy", "copied out of junk")
        assert _history(db, r.actor_id) == [], "APPEND alone must train nothing"

        s.ok("SELECT Junk")
        s.ok(f"UID MOVE {moved} INBOX")
        assert _history(db, r.actor_id) == [("ham", "imap_junk_move")], (
            "MOVE out of Junk must train exactly one ham lesson")
        s.ok(f"UID COPY {copied} Archive")
        assert _history(db, r.actor_id)[1:] == [("ham", "imap_junk_move")], (
            "COPY out of Junk must train exactly one more ham lesson")

        flagged = s.append("INBOX", "flag", "junk flag cleared")
        s.ok("SELECT INBOX")
        s.ok(f"UID STORE {flagged} +FLAGS (\\Junk)")
        s.ok(f"UID STORE {flagged} -FLAGS (\\Junk)")
        assert _history(db, r.actor_id)[2:] == [
            ("spam", "imap_junk_flag"), ("ham", "imap_junk_flag"),
        ], "clearing the \\Junk mark must train ham after the mark trained spam"
    finally:
        s.close()


@pytest.mark.feature("spam")
def test_marking_junk_and_moving_it_to_junk_is_one_lesson(mail_bridge_mda, nest_instance,
                                                          seal_helper_binary):
    """A mail app that marks a message as junk and then moves it to Junk teaches
    one lesson, not two: one history row and one model change, on both arms of
    the mail bridge's agent-side train — a user with no model yet (it trains one
    from empty) and a user whose model already rests sealed (it opens, mutates and
    re-seals it). Moving it back out is a new lesson, and marking it junk
    again after that a third — the rule keys on the newest lesson on record, not
    on a clock, so nothing here depends on how fast the two commands follow each
    other (convention 14)."""
    db = nest_instance["db_path"]
    untrained = _fresh(mail_bridge_mda, nest_instance, seal_helper_binary, "one")
    sealed = _fresh(mail_bridge_mda, nest_instance, seal_helper_binary, "onesealed")
    _seed_spam_model(db_path=db, actor_id=sealed.actor_id,
                     seal_helper_binary=seal_helper_binary)
    for who in (untrained, sealed):
        s = _Session(mail_bridge_mda, who, time.monotonic() + 90.0)
        try:
            uid = s.append("INBOX", "one", "buy cheap pills now, limited offer")
            s.ok("SELECT INBOX")
            before = _stored_model(db, who.actor_id)
            s.ok(f"UID STORE {uid} +FLAGS (\\Junk)")
            assert _history(db, who.actor_id) == [("spam", "imap_junk_flag")], (
                "the junk mark must train exactly one lesson")
            after_mark = _stored_model(db, who.actor_id)
            assert after_mark is not None and after_mark != before, (
                "the junk mark must change the stored model")

            s.ok(f"UID MOVE {uid} Junk")
            assert _history(db, who.actor_id) == [("spam", "imap_junk_flag")], (
                "moving the marked message to Junk is the same lesson — no second row")
            assert _stored_model(db, who.actor_id) == after_mark, (
                "…and the stored model is byte-identical: nothing was written")

            # A MOVE mints a new UID in the destination; the fresh recipient's
            # mailboxes hold only this message, so ALL finds it.
            s.ok("SELECT Junk")
            [in_junk] = s.search("ALL")
            s.ok(f"UID MOVE {in_junk} INBOX")
            assert _history(db, who.actor_id) == [
                ("spam", "imap_junk_flag"), ("ham", "imap_junk_move"),
            ], "moving it back out is a new (ham) lesson"
            s.ok("SELECT INBOX")
            [in_inbox] = s.search("ALL")
            s.ok(f"UID MOVE {in_inbox} Junk")
            assert _history(db, who.actor_id) == [
                ("spam", "imap_junk_flag"), ("ham", "imap_junk_move"), ("spam", "imap_junk_move"),
            ], "marking it junk again after that is a third lesson, not a duplicate"
        finally:
            s.close()


@pytest.mark.feature("spam")
def test_only_an_explicit_mark_trains(mail_bridge_mda, nest_instance, seal_helper_binary):
    """Reading, flagging, answering, archiving and deleting a message — in a mail
    app, and reading it in the app — train nothing; only the explicit junk mark
    at the end does, so the empty history before it is not a dead pipeline."""
    r = _fresh(mail_bridge_mda, nest_instance, seal_helper_binary, "none")
    db = nest_instance["db_path"]
    s = _Session(mail_bridge_mda, r, time.monotonic() + 90.0)
    try:
        read = s.append("INBOX", "read", "just read it")
        gone = s.append("INBOX", "del", "deleted without a mark")
        s.ok("SELECT INBOX")
        status, body = _imap_uid_fetch_body(s.sock, s.buf, s._tag(), read, s.deadline)
        assert status == "OK" and body, "a mail-app open (BODY[], which sets \\Seen) must succeed"
        s.ok(f"UID STORE {read} +FLAGS (\\Answered \\Flagged)")
        s.ok(f"UID MOVE {read} Archive")
        s.ok(f"UID STORE {gone} +FLAGS (\\Deleted)")
        s.ok(f"UID EXPUNGE {gone}")
        app_read = s.append("INBOX", "app", "read in the app")
        with WsRpcAdminClient(nest_instance["url"], actor_id=r.actor_id,
                              signing_key=bytes(r.recipient["signing_key"])) as ws:
            ws.call("fauna.email.inbox.mark_seen", {"uids": [app_read]})
        assert _history(db, r.actor_id) == [], (
            f"no implicit gesture may train the filter; got {_history(db, r.actor_id)}")

        s.ok(f"UID STORE {app_read} +FLAGS (\\Junk)")
        assert _history(db, r.actor_id) == [("spam", "imap_junk_flag")], (
            "the explicit mark must train exactly one lesson")
    finally:
        s.close()


@pytest.mark.feature("spam")
def test_a_message_moved_back_out_of_junk_stays_out(mail_bridge_mda, nest_instance,
                                                     seal_helper_binary):
    """Mail the user's filter sorted into Junk and the user moved back to the
    inbox is not sorted into Junk again by a later scoring pass, although the
    model still scores it as spam."""
    r = _fresh(mail_bridge_mda, nest_instance, seal_helper_binary, "back")
    token = f"qzback{secrets.token_hex(3)}wx"
    _seed_spam_model(db_path=nest_instance["db_path"], actor_id=r.actor_id,
                     seal_helper_binary=seal_helper_binary,
                     ngrams={token: (110, 0)}, spam_messages=110, ham_messages=110)
    s = _Session(mail_bridge_mda, r, time.monotonic() + 90.0)
    try:
        s.append("INBOX", "back", f"the {token} offer")
        s.ok("SELECT INBOX")
        assert not s.search(f'BODY "{token}"'), "precondition: the scorer files it to Junk"
        s.ok("SELECT Junk")
        [uid] = s.search(f'BODY "{token}"')
        s.ok(f"UID MOVE {uid} INBOX")
        for _ in range(2):  # every SELECT re-runs the scoring pass
            s.ok("SELECT INBOX")
            assert s.search(f'BODY "{token}"'), (
                "a message the user moved out of Junk must stay in the inbox")
        s.ok("SELECT Junk")
        assert not s.search(f'BODY "{token}"'), "…and must not be back in Junk"
    finally:
        s.close()


@pytest.mark.feature("spam")
def test_own_filter_does_not_sort_until_it_has_learned_enough(mail_bridge_mda, nest_instance,
                                                              seal_helper_binary):
    """A filter trained on no more than `bayesian_min_samples` (50) messages does
    not move mail, however strongly its few lessons point; the same message to a
    user whose filter has learned from enough marks is sorted into Junk."""
    token = f"qzcold{secrets.token_hex(3)}wx"
    db = nest_instance["db_path"]
    novice = _fresh(mail_bridge_mda, nest_instance, seal_helper_binary, "cold")
    _seed_spam_model(db_path=db, actor_id=novice.actor_id,
                     seal_helper_binary=seal_helper_binary,
                     ngrams={token: (25, 0)}, spam_messages=25, ham_messages=25)
    trained = _fresh(mail_bridge_mda, nest_instance, seal_helper_binary, "warm")
    _seed_spam_model(db_path=db, actor_id=trained.actor_id,
                     seal_helper_binary=seal_helper_binary,
                     ngrams={token: (110, 0)}, spam_messages=110, ham_messages=110)

    for who, expect_inbox in ((novice, True), (trained, False)):
        s = _Session(mail_bridge_mda, who, time.monotonic() + 90.0)
        try:
            s.append("INBOX", "cold", f"the {token} offer")
            s.ok("SELECT INBOX")
            in_inbox = bool(s.search(f'BODY "{token}"'))
        finally:
            s.close()
        assert in_inbox is expect_inbox, (
            "a filter at 50 samples must not sort mail" if expect_inbox else
            "control: a filter at full confidence must sort the same message to Junk")
