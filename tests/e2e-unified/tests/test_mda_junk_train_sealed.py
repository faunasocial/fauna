"""Tier_3: the MDA's **agent-side** ``\\Junk``-train writes the spam model SEALED.

Every per-user spam model and training-history row rests sealed to the actor's
own recipient key (``docs/goal/behavior/mail-spam.md`` § Encrypted-mode
interaction; ``docs/goal/architecture/content-moderation-and-ranking.md``
§ Sealing tier-1), and the nest can neither read nor train one. So an IMAP
``+\\Junk`` STORE trains **agent-side**: the AUTH'd MDA session fetches the
model (``fauna.bridges.fetch_spam_model``), opens a stored one under its session
capability — or starts from an EMPTY model when the actor has none
(``stored_sealed=false``; never from the cold-start seed) — applies the training
delta via the shared ``apply_spam_training`` FFI, seals the result to the actor's
own recipient key, and writes it back (``fauna.bridges.put_spam_model`` with an
atomic sealed history row). There is no server relay to fall back to. Only this
full-stack test proves the real ``fetch_spam_model`` (``stored_sealed``
dispatch) → ``OpenMailRecord``-unseal → shared FFI mutate → seal →
``put_spam_model`` chain across the deployed binaries — the Go unit tests stub
the opener/scorer, and the Go ``TestSealSpamModelRoundTrip`` only pins the
seed's open-ability.

Ground truth is read at **nest-disk rest**: after the STORE, ``spam_models``
holds an opaque sealed blob (never plaintext ``serde_json``), and a new
``spam_training_history`` row carries a non-empty **sealed** subject and a
sealed delta — for a recipient whose model was already stored (the open →
mutate → re-seal arm) and for an untrained one (the train-from-empty arm) alike.

Server-only IMAP harness (``mail_bridge_mda`` + the ``mda_junk_train_sealed``
recipients); no client driver — the seed model is pre-sealed by the test-only
seal-helper standing in for the user's primary client (E2E rule 8 carve-out
(b): fixture setup arranging preconditions ≠ the action under test, which is the
real IMAP ``+\\Junk`` STORE driven over the wire).
"""

from __future__ import annotations

import json
import sqlite3
import time

import pytest

from helpers.budgets import MAIL_AGENT_WRITEBACK_S
from helpers.mail_wire import (
    _imap_append,
    _imap_auth_plain,
    _imap_cmd,
    _imaps_connect,
)
from helpers.waiting import wait_until

pytestmark = pytest.mark.tier_3


def _stored_model(db_path: str, actor_id: bytes) -> bytes | None:
    """The actor's ``spam_models.model_json`` at nest-disk rest (None = no row)."""
    conn = sqlite3.connect(f"file:{db_path}?mode=ro", uri=True)
    try:
        row = conn.execute(
            "SELECT model_json FROM spam_models WHERE actor_id = ?",
            (actor_id,),
        ).fetchone()
        return None if row is None else bytes(row[0])
    finally:
        conn.close()


def _is_plaintext_model(blob: bytes) -> bool:
    """True iff the stored blob decodes as plaintext serde_json — a shape no
    writer may store any more (the nest's ``is_sealed_model_blob`` inverse)."""
    try:
        json.loads(blob)
        return True
    except (ValueError, UnicodeDecodeError):
        return False


def _history_rows(db_path: str, actor_id: bytes) -> list[dict]:
    """The actor's ``spam_training_history`` rows, oldest first."""
    conn = sqlite3.connect(f"file:{db_path}?mode=ro", uri=True)
    try:
        rows = conn.execute(
            "SELECT mailbox, label, source, model_delta_applied, sealed_subject "
            "FROM spam_training_history WHERE actor_id = ? ORDER BY created_at ASC, history_id ASC",
            (actor_id,),
        ).fetchall()
    finally:
        conn.close()
    return [
        {
            "mailbox": r[0],
            "label": r[1],
            "source": r[2],
            "model_delta_applied": None if r[3] is None else bytes(r[3]),
            "sealed_subject": None if r[4] is None else bytes(r[4]),
        }
        for r in rows
    ]


def _assert_sealed_row(row: dict) -> None:
    """A training-history row as every writer now stores it: a non-empty sealed
    subject and a sealed forward delta, neither of which decodes as plaintext
    (the plaintext subject column itself is gone)."""
    assert row["sealed_subject"], (
        f"the row must carry a non-empty SEALED subject (opaque ciphertext); got {row!r}"
    )
    assert row["model_delta_applied"], f"the row must carry the sealed forward delta; got {row!r}"
    assert not _is_plaintext_model(row["model_delta_applied"]), (
        "the stored delta must be sealed, never a plaintext JSON n-gram set"
    )


def _spam_message(recipient_username: str, tag: str, body: str) -> bytes:
    return (
        b"From: promo@external.test\r\n"
        b"To: " + recipient_username.encode() + b"\r\n"
        b"Subject: agent junk train " + tag.encode() + b"\r\n"
        b"Date: Wed, 15 May 2026 12:00:00 +0000\r\n"
        b"Message-ID: <agent-junk-" + tag.encode() + b"@mda.fauna.test>\r\n"
        b"\r\n" + body.encode() + b"\r\n"
    )


def _append_and_junk(handle, recipient, tag: str, body: str, deadline: float) -> None:
    """AUTH → APPEND a spam message to INBOX → SELECT INBOX → ``UID STORE +\\Junk``
    (the real user action that fires the training signal). STORE needs a SELECTed
    mailbox; the ``+\\Junk`` add is what ``store.go`` turns into a train."""
    sock, buf = _imaps_connect(handle.mda, deadline)
    with sock:
        assert (
            _imap_auth_plain(sock, buf, tag + "a", recipient.username, recipient.password, deadline)
            == "OK"
        ), "AUTH PLAIN must succeed for the recipient"
        status, uid = _imap_append(
            sock, buf, tag + "b", "INBOX", _spam_message(recipient.username, tag, body), deadline
        )
        assert status == "OK" and uid is not None, f"APPEND must return an [APPENDUID]; got {status}"
        st, _ = _imap_cmd(sock, buf, tag + "c", "SELECT INBOX", deadline)
        assert st == "OK", f"SELECT INBOX must succeed; got {st}"
        st, _ = _imap_cmd(sock, buf, tag + "d", f"UID STORE {uid} +FLAGS (\\Junk)", deadline)
        assert st == "OK", f"UID STORE +FLAGS (\\Junk) must succeed; got {st}"
        sock.sendall((tag + "z LOGOUT\r\n").encode())


@pytest.mark.feature("spam")
def test_agent_side_junk_train_reseals_sealed_model_at_rest(mda_junk_train_sealed):
    """An IMAP ``+\\Junk`` on a recipient whose model is SEALED at rest trains it
    **agent-side**: the re-written model stays sealed and a sealed audit row
    lands — proving the
    whole ``fetch_spam_model``(``stored_sealed``) → open → mutate → re-seal →
    ``put_spam_model`` chain on the real binaries."""
    h = mda_junk_train_sealed
    r = h.sealed
    db = h.db_path

    # Baseline: the seeded model is sealed (opaque) at rest, no history yet.
    before = _stored_model(db, r.actor_id)
    assert before is not None, "the fixture must seed the recipient's sealed model"
    assert not _is_plaintext_model(before), (
        "the seeded model must be sealed (opaque) at rest before the \\Junk train"
    )
    assert _history_rows(db, r.actor_id) == [], "no training-history rows before the STORE"

    # ── The user action: a real IMAP +\Junk STORE.
    deadline = time.monotonic() + 120.0
    _append_and_junk(h, r, "s1", "BUY cheap pills now, limited offer, act now!!!", deadline)

    # The agent-side write-back re-seals the model. Poll until it changes.
    def _model_changed():
        value = _stored_model(db, r.actor_id)
        return value if (value is not None and value != before) else None

    after = wait_until(
        _model_changed,
        MAIL_AGENT_WRITEBACK_S,
        diagnose=lambda: (
            "the \\Junk train must REWRITE the model — the MDA opened the sealed model, "
            "applied the training delta, re-sealed it, and wrote it back "
            "(fauna.bridges.put_spam_model). An unchanged model means the agent-side "
            "path never fired."
        ),
    )
    assert not _is_plaintext_model(after), (
        "the re-written model must STILL be sealed at rest — the MDA only ever writes "
        "a model sealed to the actor's own key (mail-spam.md § Encrypted-mode interaction)"
    )

    # Exactly one agent-written history row: an OPAQUE sealed subject + a sealed
    # delta, source=imap_junk_flag, label=spam.
    rows = wait_until(
        lambda: _history_rows(db, r.actor_id) or None,
        MAIL_AGENT_WRITEBACK_S,
        diagnose=lambda: "no agent-side history row appeared after the \\Junk train",
    )
    assert len(rows) == 1, f"exactly one agent-side history row after one \\Junk; got {rows}"
    row = rows[0]
    assert row["source"] == "imap_junk_flag", f"source must be imap_junk_flag; got {row['source']!r}"
    assert row["label"] == "spam", f"+\\Junk trains spam; got label {row['label']!r}"
    assert row["mailbox"] == "INBOX", f"mailbox must be the SELECTed INBOX; got {row['mailbox']!r}"
    _assert_sealed_row(row)

    # ── Round-trip closure: a SECOND +\Junk. For the MDA to train again it must
    # fetch_spam_model and receive the freshly re-sealed model with
    # stored_sealed=true VERBATIM (no seal-on-read double-seal), then open + mutate
    # it — else it would train a fresh model from empty instead, or fail to open it
    # and write nothing. A third distinct sealed state + a second sealed row
    # proves the dispatch signal and open both survive a real persist+refetch.
    _append_and_junk(h, r, "s2", "FREE money waiting, click here to claim now!!!", deadline)

    def _model_changed_again():
        value = _stored_model(db, r.actor_id)
        return value if (value is not None and value != after) else None

    after2 = wait_until(
        _model_changed_again,
        MAIL_AGENT_WRITEBACK_S,
        diagnose=lambda: (
            "the second \\Junk must again rewrite the model — proving the nest returned the "
            "re-sealed model verbatim with stored_sealed=true and the MDA re-opened it"
        ),
    )
    assert not _is_plaintext_model(after2), "the twice-trained model must still be sealed at rest"

    def _rows_at_least_2():
        value = _history_rows(db, r.actor_id)
        return value if len(value) >= 2 else None

    rows2 = wait_until(
        _rows_at_least_2,
        MAIL_AGENT_WRITEBACK_S,
        diagnose=lambda: "a second agent-side history row never appeared after the second \\Junk train",
    )
    assert len(rows2) == 2, f"a second \\Junk must land a second agent-side row; got {rows2}"
    for x in rows2:
        _assert_sealed_row(x)


@pytest.mark.feature("spam")
def test_untrained_recipient_junk_train_writes_a_sealed_model_from_empty(mda_junk_train_sealed):
    """A recipient with NO stored model (``stored_sealed=false``) still trains
    agent-side: the MDA starts from an EMPTY model (never the cold-start seed —
    the baseline fold is read-time only, ``mail-spam.md`` § Cold start Path 2),
    seals the trained model to the recipient's own key and writes it with a
    sealed history row. There is no plaintext server relay to fall back to, so
    a plaintext model or a row without a sealed subject is a regression."""
    h = mda_junk_train_sealed
    r = h.untrained
    db = h.db_path

    assert _stored_model(db, r.actor_id) is None, "the untrained recipient must start with no model"
    assert _history_rows(db, r.actor_id) == [], "no history rows before the STORE"

    deadline = time.monotonic() + 120.0
    _append_and_junk(h, r, "u1", "BUY cheap pills now, limited offer, act now!!!", deadline)

    after = wait_until(
        lambda: _stored_model(db, r.actor_id),
        MAIL_AGENT_WRITEBACK_S,
        diagnose=lambda: (
            "the \\Junk train never wrote the untrained recipient's model — the MDA must "
            "train from an empty model, seal it and put_spam_model it "
            f"(MDA log {h.mda.log_file})"
        ),
    )
    assert not _is_plaintext_model(after), (
        "an untrained recipient's first model must be SEALED at rest — the nest refuses a "
        "plaintext model and no writer produces one (mail-spam.md § Encrypted-mode interaction)"
    )

    rows = wait_until(
        lambda: _history_rows(db, r.actor_id) or None,
        MAIL_AGENT_WRITEBACK_S,
        diagnose=lambda: "no agent-side history row appeared after the \\Junk train",
    )
    assert len(rows) == 1, f"exactly one agent-side history row; got {rows}"
    row = rows[0]
    assert row["source"] == "imap_junk_flag", f"source; got {row['source']!r}"
    assert row["label"] == "spam", f"label; got {row['label']!r}"
    _assert_sealed_row(row)
