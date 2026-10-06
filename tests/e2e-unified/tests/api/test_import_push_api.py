"""tier_3 e2e for the mailbox-migration import Push events.

The in-module handler tests in ``bins/fauna-nest/src/bridge_import_handlers.rs``
assert the DB rows and the Reply, but they cannot assert that a
``BridgeImport*`` Push actually *reaches a connected client*: ``notify_push``
fans out over ``WsState``, which only has subscribers behind a real WebSocket.
This file closes that gap (tracked internally, item B4) over a real connection.

Flow under test, per ``docs/goal/behavior/mailbox-migration.md`` § Progress
lives nest-side::

    fauna.bridges.import_message
      → bridge_import_handlers.rs::notify_import_progress
      → ws.rs::notify_push  →  Frame::Push{2: kind, 4: payload, 8: seq}
      → this client's buffered Push queue

The three kinds are ``fauna.bridges.push.import_{progress,complete,error}``
(``libs/fauna-protocol/src/push_events.rs::PushEvent::kind``).

Import kinds are User-class and caller-scoped, so a plain registered actor is
a valid caller — no bridge keypair, no admin role.
"""

from __future__ import annotations

import pytest

from common import create_actor_and_register, port_base_url
from clients.ws_rpc_admin_client import WsRpcAdminClient
from helpers.mail_envelope_key import envelope_key
from helpers.recipient_seal_key import provision_recipient_seal_key

pytestmark = pytest.mark.tier_3

PROGRESS = "fauna.bridges.push.import_progress"
COMPLETE = "fauna.bridges.push.import_complete"
ERROR = "fauna.bridges.push.import_error"


def _user_client(port: int, actor: dict) -> WsRpcAdminClient:
    """A User-class WS-RPC client for ``actor`` against the loopback nest.

    ("Admin" in the class name is historical — a User keypair makes it a
    User-class caller; see ``test_push_api.py``.)
    """
    return WsRpcAdminClient(
        port_base_url(port),
        actor_id=actor["actor_id_bytes"],
        signing_key=bytes(actor["signing_key"]),
    )


def _rfc5322(message_id: str, subject: str = "hello") -> bytes:
    return (
        f"Message-ID: <{message_id}>\r\n"
        f"From: alice@example.org\r\n"
        f"To: bob@example.net\r\n"
        f"Subject: {subject}\r\n"
        f"Date: Thu, 09 Jul 2026 12:00:00 +0000\r\n"
        f"\r\n"
        f"body of {subject}\r\n"
    ).encode()


def _item(message_id: str, *, source_uid: int = 1, mailbox: str = "INBOX") -> dict:
    """One ``ImportMessageItem`` (``libs/fauna-protocol/src/bridge_routing.rs``).

    ``dedup_key`` is client-computed (`fauna_mail::mail_dedup_keys`); the nest
    only validates that it is non-empty, so a stable literal is enough here.
    ``body_size`` must equal ``len(body)`` or the item is per-message
    ``errored`` rather than imported. ``body`` is always the raw RFC 5322
    bytes — the nest seals body + nest-derived index hint at ingest
    (``encryption-at-rest.md`` S1; the pre-Phase-3 ``body_mode``/``index_hint``
    wire fields are retired).
    """
    body = _rfc5322(message_id)
    return {
        "mailbox": mailbox,
        "flags": ["\\Seen"],
        "body": body,
        "timestamp": 1_783_000_000,
        "body_size": len(body),
        "sender_domain": "example.org",
        "source_uid": source_uid,
        "source_uid_validity": 42,
        "dedup_key": f"msgid:v1:<{message_id}>",
        "envelope_key": envelope_key(body),
    }


def _start_session(c: WsRpcAdminClient, total: int = 1) -> str:
    # The import path seals at ingest and fails closed without a registered
    # recipient seal key, so every importer registers one first (the nest
    # only ever seals to it — a throwaway key is fine; nothing here opens).
    provision_recipient_seal_key(c, c._actor_id)
    reply = c.call(
        "fauna.bridges.start_import_session",
        {"source_descriptor": "imap://imap.example.org/alice", "total_count": total},
    )
    return reply["session_id"]


@pytest.mark.feature("mailbox-import")
def test_import_message_emits_progress_push(two_nodes):
    """A single `import_message` delivers a `BridgeImportProgress` Push.

    The Push is emitted inside the handler, *before* its Reply is written, so
    `call()` buffers it while awaiting the Reply and `wait_for_push` reads it
    straight back out of the buffer.
    """
    port = two_nodes["port_a"]
    actor = create_actor_and_register(port, admin_signing_key=two_nodes["admin_sk_a"])
    with _user_client(port, actor) as c:
        session_id = _start_session(c)

        reply = c.call(
            "fauna.bridges.import_message",
            {
                "session_id": session_id,
                "message": _item("a1@example.org"),
                "skip_dedup": False,
            },
        )
        assert reply["outcome"]["outcome"] == "imported", (
            f"expected the message to import, got outcome={reply['outcome']!r}"
        )
        assert reply["imported_count"] == 1

        push = c.wait_for_push(PROGRESS)
        assert push.payload["session_id"] == session_id
        assert push.payload["imported_count"] == 1
        assert push.payload["skipped_count"] == 0
        assert push.payload["errored_count"] == 0


def test_import_batch_emits_exactly_one_progress_push(two_nodes):
    """§ Batching: one Push per *call*, not one per message."""
    port = two_nodes["port_a"]
    actor = create_actor_and_register(port, admin_signing_key=two_nodes["admin_sk_a"])
    with _user_client(port, actor) as c:
        session_id = _start_session(c, total=2)

        reply = c.call(
            "fauna.bridges.import_message_batch",
            {
                "session_id": session_id,
                "messages": [
                    _item("b1@example.org", source_uid=1),
                    _item("b2@example.org", source_uid=2),
                ],
                "skip_dedup": False,
            },
        )
        assert [o["outcome"] for o in reply["outcomes"]] == ["imported", "imported"]
        assert reply["imported_count"] == 2

        push = c.wait_for_push(PROGRESS)
        assert push.payload["session_id"] == session_id
        assert push.payload["imported_count"] == 2

        with pytest.raises(TimeoutError):
            c.wait_for_push(PROGRESS, timeout=1.0)


@pytest.mark.feature("mailbox-import")
def test_import_dedup_skip_is_reported_in_the_progress_push(two_nodes):
    """A dedup hit lands as `skipped_count` in the very next progress Push.

    Nest-side dedup scope is *any* mailbox of the actor, so re-importing the
    same `dedup_key` is skipped even though the first copy went to INBOX.
    """
    port = two_nodes["port_a"]
    actor = create_actor_and_register(port, admin_signing_key=two_nodes["admin_sk_a"])
    with _user_client(port, actor) as c:
        session_id = _start_session(c, total=2)

        c.call(
            "fauna.bridges.import_message",
            {
                "session_id": session_id,
                "message": _item("dup@example.org", source_uid=1),
                "skip_dedup": False,
            },
        )
        c.drain_pushes(PROGRESS)

        reply = c.call(
            "fauna.bridges.import_message",
            {
                "session_id": session_id,
                "message": _item("dup@example.org", source_uid=2, mailbox="Archive"),
                "skip_dedup": False,
            },
        )
        assert reply["outcome"] == {"outcome": "skipped", "reason": "dedup"}

        push = c.wait_for_push(PROGRESS)
        assert push.payload["imported_count"] == 1
        assert push.payload["skipped_count"] == 1


def test_finalize_emits_complete_push(two_nodes):
    """`finalize_import_session` delivers `BridgeImportComplete` with the summary."""
    port = two_nodes["port_a"]
    actor = create_actor_and_register(port, admin_signing_key=two_nodes["admin_sk_a"])
    with _user_client(port, actor) as c:
        session_id = _start_session(c)
        c.call(
            "fauna.bridges.import_message",
            {
                "session_id": session_id,
                "message": _item("c1@example.org"),
                "skip_dedup": False,
            },
        )

        reply = c.call("fauna.bridges.finalize_import_session", {"session_id": session_id})
        assert reply["session"]["state"] == "completed"

        push = c.wait_for_push(COMPLETE)
        assert push.payload["session_id"] == session_id
        assert push.payload["imported_count"] == 1


def test_fail_session_emits_error_push(two_nodes):
    """`fail_import_session` delivers `BridgeImportError` carrying the reason."""
    port = two_nodes["port_a"]
    actor = create_actor_and_register(port, admin_signing_key=two_nodes["admin_sk_a"])
    with _user_client(port, actor) as c:
        session_id = _start_session(c)

        reply = c.call(
            "fauna.bridges.fail_import_session",
            {"session_id": session_id, "reason": "source auth failed"},
        )
        assert reply["session"]["state"] == "errored"

        push = c.wait_for_push(ERROR)
        assert push.payload["session_id"] == session_id
        assert push.payload["reason"] == "source auth failed"


@pytest.mark.feature("mailbox-import")
def test_pushes_are_scoped_to_the_importing_actor(two_nodes):
    """A second actor's connection sees none of the first actor's import Pushes.

    `notify_push` fans out per `actor_id`; this pins that the import family
    honours it (a leak here would expose one user's mailbox names + counters
    to another).
    """
    port = two_nodes["port_a"]
    importer = create_actor_and_register(port, admin_signing_key=two_nodes["admin_sk_a"])
    bystander = create_actor_and_register(port, admin_signing_key=two_nodes["admin_sk_a"])

    with _user_client(port, bystander) as other, _user_client(port, importer) as c:
        session_id = _start_session(c)
        c.call(
            "fauna.bridges.import_message",
            {
                "session_id": session_id,
                "message": _item("d1@example.org"),
                "skip_dedup": False,
            },
        )
        assert c.wait_for_push(PROGRESS).payload["session_id"] == session_id

        with pytest.raises(TimeoutError):
            other.wait_for_push(PROGRESS, timeout=1.0)
