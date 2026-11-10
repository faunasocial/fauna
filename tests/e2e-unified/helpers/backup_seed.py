"""Seeds a backup journey needs before its gesture — precondition-only writes
(e2e convention 8 exempts fixture setup from the drive-it-through-the-UI rule).

Shared by the app journeys in ``tests/test_backups.py`` and the app-less api
tests that drive the backup legs through the harness library."""

from __future__ import annotations

import secrets

import requests


def import_one_mail(nest: dict, owner: dict, mailbox: str = "INBOX") -> None:
    """Import one unique message for ``owner`` into ``mailbox`` (INBOX by
    default) through the import surface — three User-class WS-RPC calls
    reaching the same write path the MDA does (``insert_appended_mail`` → the
    mail segment store). A mailbox the box does not hold yet is created by the
    placement, and the placement is journaled, as a migrated mailbox arrives.
    The owner's mailbox must already be enabled: the import seals fail-closed
    to the owner's own published key."""
    from clients.ws_rpc_admin_client import WsRpcAdminClient
    from helpers.mail_envelope_key import envelope_key

    nonce = secrets.token_hex(6)
    message_id = f"backup-upload-proof-{nonce}@example.org"
    body = (
        f"Message-ID: <{message_id}>\r\n"
        "From: alice@example.org\r\n"
        "To: bob@example.net\r\n"
        "Subject: segment for the backup upload proof\r\n"
        "Date: Thu, 24 Jul 2026 12:00:00 +0000\r\n"
        "\r\n"
        "one message is all the sweep needs to have something to upload\r\n"
    ).encode()

    client = WsRpcAdminClient(
        nest["url"],
        actor_id=owner["actor_id_bytes"],
        signing_key=bytes(owner["signing_key"]),
    )
    with client:
        session_id = client.call(
            "fauna.bridges.start_import_session",
            {"source_descriptor": f"imap://imap.example.org/backup-proof-{nonce}",
             "total_count": 1},
        )["session_id"]
        reply = client.call(
            "fauna.bridges.import_message",
            {
                "session_id": session_id,
                "message": {
                    "mailbox": mailbox,
                    "flags": ["\\Seen"],
                    "body": body,
                    "timestamp": 1_783_000_000,
                    "body_size": len(body),
                    "sender_domain": "example.org",
                    "source_uid": 1,
                    "source_uid_validity": 42,
                    "dedup_key": f"msgid:v1:<{message_id}>",
                    "envelope_key": envelope_key(body),
                },
                "skip_dedup": False,
            },
        )
        # Release the source lock so a second parametrized client can seed too.
        client.call("fauna.bridges.finalize_import_session", {"session_id": session_id})

    outcome = reply["outcome"]["outcome"]
    assert outcome == "imported", (
        f"seeding a mail segment failed: outcome={reply['outcome']!r}. Without a segment "
        f"the owner's backlog stays empty and nothing can be uploaded."
    )


def push_backup(nest: dict) -> dict:
    """One synchronous nest-side backup sweep (the test-hooks run-now barrier):
    when it returns, the pass has happened or definitively failed."""
    resp = requests.post(f"{nest['url']}/api/v1/test/backup/run-now", json={}, timeout=120)
    assert resp.status_code == 200, f"backup run-now returned {resp.status_code}: {resp.text}"
    payload = resp.json()
    assert payload.get("ok") is True and payload.get("owners_run", 0) >= 1, (
        f"the sweep ran no owner ({payload!r}): the enroll did not leave the owner "
        f"with a granted key and a registered destination"
    )
    return payload


def inbox_count(nest: dict, owner: dict) -> int:
    """How many INBOX messages ``owner`` has on ``nest``, read over the owner's
    own authenticated WS-RPC (convention 5: the nest-side half of a restore
    verdict, independent of what any page renders)."""
    from clients.ws_rpc_admin_client import WsRpcAdminClient

    client = WsRpcAdminClient(
        nest["url"],
        actor_id=owner["actor_id_bytes"],
        signing_key=bytes(owner["signing_key"]),
    )
    with client:
        reply = client.call("fauna.email.inbox.fetch", {"after_uid": 0, "limit": 50})
    return len(reply.get("messages", []))


def _owner_client(nest: dict, owner: dict):
    from clients.ws_rpc_admin_client import WsRpcAdminClient

    return WsRpcAdminClient(
        nest["url"],
        actor_id=owner["actor_id_bytes"],
        signing_key=bytes(owner["signing_key"]),
    )


def mailbox_counts(nest: dict, owner: dict) -> dict[str, int]:
    """``mailbox name → message count`` for ``owner`` on ``nest``, read over the
    owner's own ``fauna.bridges.list_own_mailboxes`` (convention 5: where each
    message sits, independent of what any page renders)."""
    with _owner_client(nest, owner) as client:
        reply = client.call("fauna.bridges.list_own_mailboxes", {})
    return {m["name"]: int(m.get("exists") or 0) for m in reply.get("mailboxes", [])}


# ── The calendar, address-book and post doors ────────────────────────────────
#
# Each kind is seeded through the production door its data arrives by — the
# ``fauna.bridges.*`` calendar and address-book doors a calendar or contacts
# app reaches, and ``fauna.posts.create`` — and read back through the door
# CalDAV, CardDAV and the feed serve from. ``seal`` is the ``run_seal_helper``
# fixture: the calendar and address-book doors accept only a genuine sealed
# record envelope of the matching kind.


def seal_fixture(seal, kind: str, plaintext: bytes) -> bytes:
    """A genuine sealed record envelope of ``kind`` (``calendar``, ``card``,
    ``mail``) sealed to a throwaway recipient. Nothing opens it: every read
    back compares the bytes a box serves with the bytes it was given."""
    import base64

    from nacl.public import PrivateKey

    return seal("seal-mail-record", {
        "plaintext_b64": base64.b64encode(plaintext).decode(),
        "recipient_x25519_pubkey_b64": base64.b64encode(
            PrivateKey.generate().public_key.encode()).decode(),
        "kind": kind,
    })


# Per DAV kind: (record kind, provision kind, collection field, put kind,
# query kind, reply items key, after-id field, collection metadata).
_DAV = {
    "calendar": ("calendar", "fauna.bridges.provision_calendar", "calendar_id",
                 "fauna.bridges.put_event_ciphertext", "fauna.bridges.query_events",
                 "events", "after_event_id", b"Personal"),
    "card": ("card", "fauna.bridges.provision_addressbook", "addressbook_id",
             "fauna.bridges.put_card_ciphertext", "fauna.bridges.query_cards",
             "cards", "after_card_id", b"Contacts"),
}


def provision_dav_collection(nest: dict, owner: dict, seal, dav: str) -> bytes:
    """Create a fresh calendar (``dav="calendar"``) or address book
    (``dav="card"``) for ``owner`` on ``nest``; return its 32-byte id."""
    record_kind, provision, field, _put, _query, _key, _after, metadata = _DAV[dav]
    collection = secrets.token_bytes(32)
    with _owner_client(nest, owner) as client:
        made = client.call(provision, {
            "actor_id": owner["actor_id_bytes"], field: collection,
            "encrypted_metadata": seal_fixture(seal, record_kind, metadata),
            "update_metadata": False,
        })
    assert made["outcome"] == "created", f"provision the {dav} collection: {made!r}"
    return collection


def put_dav_item(
    nest: dict, owner: dict, seal, dav: str, collection: bytes, uid_hash: bytes, text: str
) -> bytes:
    """Put (or replace) the event or card ``uid_hash`` in ``collection``, its
    sealed body carrying ``text``; return the body bytes, which is what the
    query door serves back."""
    import time

    record_kind, _prov, field, put, _query, _key, _after, _meta = _DAV[dav]
    if dav == "calendar":
        plain = f"BEGIN:VEVENT\r\nSUMMARY:{text}\r\nEND:VEVENT\r\n".encode()
    else:
        plain = f"BEGIN:VCARD\r\nFN:{text}\r\nEND:VCARD\r\n".encode()
    body = seal_fixture(seal, record_kind, plain)
    with _owner_client(nest, owner) as client:
        reply = client.call(put, {
            "actor_id": owner["actor_id_bytes"], field: collection, "uid_hash": uid_hash,
            "encrypted_body": body,
            "encrypted_index_hint": seal_fixture(seal, "mail", f"hint-{text}".encode()),
            "timestamp": int(time.time()), "ciphertext_size": len(body),
            "if_match": None,
        })
    assert reply["outcome"] in ("created", "updated"), f"put the {dav} item {text!r}: {reply!r}"
    return body


def dav_items_served(nest: dict, owner: dict, dav: str, collection: bytes) -> list[tuple]:
    """Every ``(uid_hash, body)`` the query door serves in ``collection`` — a
    list, not a map, so a resource served twice shows."""
    _kind, _prov, field, _put, query, key, after, _meta = _DAV[dav]
    with _owner_client(nest, owner) as client:
        reply = client.call(query, {
            "actor_id": owner["actor_id_bytes"], field: collection,
            "since_modseq": None, after: None, "limit": 200,
        })
    return [(bytes(i["uid_hash"]), bytes(i["encrypted_body"])) for i in reply.get(key, [])]


def create_post(nest: dict, owner: dict, text: str) -> tuple[str, bytes]:
    """Publish one signed text post as ``owner``; return ``(post_id, body)``."""
    import time

    from tests.api.bare import sign_and_encode_post

    body = bytes(sign_and_encode_post(
        owner["signing_key"], int(time.time() * 1_000_000), text, tags=[]))
    with _owner_client(nest, owner) as client:
        post_id = client.call("fauna.posts.create", {"body": body})["post_id"]
    return post_id, body


def post_body(nest: dict, owner: dict, post_id: str) -> bytes:
    """The bytes the by-id post door serves for ``post_id``; a post the box
    does not hold raises with the door's own code."""
    with _owner_client(nest, owner) as client:
        return bytes(client.call("fauna.posts.get", {"post_id": post_id})["body"])


def author_feed_ids(nest: dict, owner: dict) -> list[str]:
    """The post ids the owner's author feed lists, duplicates kept."""
    with _owner_client(nest, owner) as client:
        posts = client.call(
            "fauna.feed.author.posts", {"author": owner["actor_id_bytes"].hex()})["posts"]
    return [p["post_id"] for p in posts]
