"""tier_3 — the nest's own promises about a mail import, witnessed from outside.

`docs/features/mailbox-import.md` carries `[nest]` outcomes the apps cannot
show: what the nest decides to keep or skip, whichever app drives the import.
This file drives the production kinds (``fauna.bridges.import_message`` and its
siblings) with no app in the loop, as the shared import machine does, and
reads each decision off the per-message outcome the nest returns.

The dedup key and the envelope key are the client's (``mailbox-migration.md``
§ Key format): the nest only compares them, so a witness here sends the keys
the production client would compute for the bytes it sends
(``helpers/mail_envelope_key.py`` mirrors the shared Rust function).

* **38** — dedup is decided against the person's own mail only (§ Dedup scope).
* **40** — a changed copy is imported as the separate message it now is (§ Dedup scope).
* **43** — a message filed under several labels lands only once (§ Don't do these).
* **45** — imported mail rests sealed, and an import the nest cannot seal is refused and stores
  nothing (§ Per-message flow).
"""

from __future__ import annotations

from pathlib import Path

import pytest

from clients._ws_rpc_core import RpcCallError
from common import create_actor_and_register
from helpers.mail_envelope_key import envelope_key
from helpers.recipient_seal_key import provision_recipient_seal_key
from tests.api.test_import_push_api import _user_client

pytestmark = pytest.mark.tier_3

IMPORTED = "imported"
DEDUP = "skipped:dedup"


def _raw(message_id: str, *, subject: str = "hello", footer: str = "") -> bytes:
    return (
        f"Message-ID: <{message_id}>\r\n"
        f"From: alice@example.org\r\n"
        f"To: bob@example.net\r\n"
        f"Subject: {subject}\r\n"
        f"Date: Thu, 09 Jul 2026 12:00:00 +0000\r\n"
        f"\r\n"
        f"body of {subject}\r\n{footer}"
    ).encode()


def _item(raw: bytes, message_id: str, *, uid: int = 1, mailbox: str = "INBOX") -> dict:
    """One ``ImportMessageItem`` for ``raw`` — keys as the production client computes them."""
    return {
        "mailbox": mailbox,
        "flags": [],
        "body": raw,
        "timestamp": 1_783_000_000,
        "body_size": len(raw),
        "sender_domain": "example.org",
        "source_uid": uid,
        "source_uid_validity": 42,
        # `normalize(Message-ID)`: brackets stripped, lowercased (`dedup_key.rs`), so a
        # message the nest delivered itself is found by the key a client computes.
        "dedup_key": f"msgid:v1:{message_id.lower()}",
        "envelope_key": envelope_key(raw),
    }


def _importer(two_nodes, *, with_seal_key: bool = True) -> tuple[int, dict]:
    port = two_nodes["port_a"]
    actor = create_actor_and_register(port, admin_signing_key=two_nodes["admin_sk_a"])
    if with_seal_key:
        with _user_client(port, actor) as c:
            provision_recipient_seal_key(c, c._actor_id)
    return port, actor


def _session(c) -> str:
    return c.call(
        "fauna.bridges.start_import_session",
        {"source_descriptor": "imap://imap.example.org/alice", "total_count": 4},
    )["session_id"]


def _import(c, session_id: str, item: dict, *, skip_dedup: bool = False) -> str:
    """The nest's decision for ``item``: ``imported``, or ``skipped:<reason>``."""
    outcome = c.call(
        "fauna.bridges.import_message",
        {"session_id": session_id, "message": item, "skip_dedup": skip_dedup},
    )["outcome"]
    if outcome["outcome"] == "skipped":
        return f"skipped:{outcome['reason']}"
    return outcome["outcome"]


# ── 38: dedup is decided against your own mail only ──────────────────────────


@pytest.mark.feature("mailbox-import")
def test_a_message_someone_else_holds_is_never_skipped_for_you_and_never_shows_they_hold_it(
    two_nodes,
):
    raw = _raw("shared-newsletter@example.org")
    item = _item(raw, "shared-newsletter@example.org")

    port, first = _importer(two_nodes)
    with _user_client(port, first) as c:
        sid = _session(c)
        assert _import(c, sid, item) == IMPORTED
        assert _import(c, sid, item) == DEDUP, (
            "the control: the person who holds the message is told they already have it"
        )

    _, second = _importer(two_nodes)
    with _user_client(port, second) as c:
        answer = _import(c, _session(c), item)
    assert answer == IMPORTED, (
        "another person's copy of the very same message must not make this person's "
        f"import skip it, nor answer any differently than for a message nobody holds: {answer!r}"
    )


# ── 40: a changed copy is a separate message ─────────────────────────────────


@pytest.mark.feature("mailbox-import")
def test_a_copy_changed_on_the_way_is_imported_as_its_own_message_not_skipped(two_nodes):
    mid = "changed-on-the-way@example.org"
    original = _raw(mid, subject="quarterly report")
    with_footer = _raw(mid, subject="quarterly report", footer="-- sent by a list server\r\n")
    tagged = _raw(mid, subject="[list] quarterly report")

    port, actor = _importer(two_nodes)
    with _user_client(port, actor) as c:
        sid = _session(c)
        assert _import(c, sid, _item(original, mid, uid=1)) == IMPORTED
        assert _import(c, sid, _item(original, mid, uid=2)) == DEDUP, (
            "the control: the same bytes again are the same message"
        )
        assert _import(c, sid, _item(with_footer, mid, uid=3)) == IMPORTED, (
            "a footer added on the way makes it a different message"
        )
        assert _import(c, sid, _item(tagged, mid, uid=4)) == IMPORTED, (
            "a subject tagged on the way makes it a different message"
        )


# ── 43: several labels, one message ──────────────────────────────────────────


@pytest.mark.feature("mailbox-import")
def test_a_message_filed_under_several_labels_lands_only_once(two_nodes):
    mid = "three-labels@example.org"
    raw = _raw(mid)

    port, actor = _importer(two_nodes)
    with _user_client(port, actor) as c:
        sid = _session(c)
        outcomes = [
            _import(c, sid, _item(raw, mid, uid=uid, mailbox=mailbox))
            for uid, mailbox in enumerate(["INBOX", "Receipts", "Travel"], start=1)
        ]
        counts = c.call("fauna.bridges.list_import_sessions", {})["sessions"]
    assert outcomes == [
        IMPORTED,
        DEDUP,
        DEDUP,
    ], outcomes
    mine = next(s for s in counts if s["session_id"] == sid)
    assert (mine["imported_count"], mine["skipped_count"]) == (1, 2), mine


# ── 45 (refusal half): an import the nest cannot seal is refused ─────────────


@pytest.mark.feature("mailbox-import")
def test_an_import_the_nest_cannot_seal_is_refused_and_stores_nothing(two_nodes):
    mid = "no-key-yet@example.org"
    item = _item(_raw(mid), mid)

    port, actor = _importer(two_nodes, with_seal_key=False)
    with _user_client(port, actor) as c:
        sid = _session(c)
        with pytest.raises(RpcCallError) as refused:
            _import(c, sid, item)
        assert "key" in str(refused.value.details).lower(), (
            f"the refusal must say why: {refused.value.code!r} {refused.value.details!r}"
        )

        provision_recipient_seal_key(c, c._actor_id)
        assert _import(c, sid, item) == IMPORTED, (
            "once a key is on file the same message imports as new — so the refusal "
            "stored nothing, readable or otherwise, that would make it a duplicate"
        )


# ── 45 (rest half): imported mail rests sealed ───────────────────────────────


@pytest.mark.feature("mailbox-import")
def test_imported_mail_rests_sealed_on_the_nest(nest_instance):
    """The message really arrived, and no byte of its plaintext is on the nest's disk.

    A distinctive phrase rides in both the subject and the body: the nest keeps
    a searchable index hint beside the sealed body, and both are sealed, so the
    phrase must be in no file the nest holds — the database and its write-ahead
    log included.
    """
    phrase = "zanzibar-quokka-7f3a91c2e8"
    # Not in the Message-ID: the client's dedup key is derived from it and rests
    # beside the message by design (`mailbox-migration.md` § Key format).
    mid = "rests-sealed-probe@example.org"
    raw = _raw(mid, subject=f"plans for {phrase}")
    user = create_actor_and_register(
        nest_instance["port"], admin_signing_key=nest_instance["admin"]["signing_key"]
    )
    with _user_client(nest_instance["port"], user) as c:
        provision_recipient_seal_key(c, c._actor_id)
        assert _import(c, _session(c), _item(raw, mid)) == IMPORTED, (
            "the message must really have arrived, or its absence from disk proves nothing"
        )

    held = []
    for path in Path(nest_instance["tmp_dir"]).rglob("*"):
        if not path.is_file():
            continue
        try:
            if phrase.encode() in path.read_bytes():
                held.append(str(path))
        except FileNotFoundError:
            continue  # a `-shm`/`-wal` side file SQLite removed mid-scan
    assert not held, f"imported mail's plaintext is readable on the nest's disk: {held}"
