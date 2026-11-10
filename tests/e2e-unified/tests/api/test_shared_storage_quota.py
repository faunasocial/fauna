"""Your storage number is ONE figure covering mail and calendar together — on a
real ``fauna-nest`` binary, read through the surface a mail MUA's quota comes
from.

Owner doc: ``docs/goal/behavior/caldav-server.md`` § QUOTA — shared with IMAP —
*"Per-actor quota covers ciphertext bytes across ``bridge_imap_messages`` +
``bridge_caldav_events`` (one quota root per actor named ``user/<handle>`` …).
A user's storage budget is one number across mail + calendar."*

The number is the one the MDA reads over ``fauna.bridges.get_quota`` (the
source of every IMAP ``GETQUOTAROOT`` — ``imap-server.md`` § QUOTA /
GETQUOTAROOT) and the one every write enforces against — mail's
APPEND/COPY/MOVE/inbound delivery and the calendar's and address book's own PUTs.
Before the calendar half was built that figure summed mail placements only, so
an event's bytes moved nothing. Here the owner PUTs a sealed event through their
own door (``fauna.bridges.put_event_ciphertext``, the path every app's Events
page takes), the MDA reads the quota, and the figure must move by at least the
event's sealed bytes — then fall back when the event is deleted, which is what
makes the movement the event's and not anything else's. MESSAGE stays the mail
count: an event is not a mailbox message.

The MDA identity is seeded the way ``test_dr_restore.py`` seeds it (a direct
``bridge_service_users`` row — the approval flow is its own suite,
``test_mail_bridge_approval.py``), on a nest of this test's own.
"""

from __future__ import annotations

import base64
import secrets

import pytest
from nacl.public import PrivateKey

from common.auth import create_actor_and_register

from clients.ws_rpc_admin_client import RpcCallError

from tests.api.test_dr_restore import _seed_approved_mda, _ws_client

pytestmark = pytest.mark.tier_3


@pytest.fixture()
def quota_nest(request, nest_mode, tmp_path_factory):
    from conftest import _start_dedicated_nest

    nest, cleanup = _start_dedicated_nest(request, nest_mode, tmp_path_factory, "shared-quota")
    yield nest
    cleanup()


def _sealer(run_seal_helper):
    """A genuine ``calendar-record`` envelope — the only body and metadata shape
    the calendar RPCs accept (the at-rest seal and its kind are proven at the
    wire edge, ``owner-key-material.md`` § Path B-sibling-5), from the same
    shared-Rust seal the apps and the MDA use. The recipient key is throwaway:
    these tests read sizes, never bodies."""
    recipient = base64.b64encode(PrivateKey.generate().public_key.encode()).decode()

    def _sealed(plaintext: bytes) -> bytes:
        return run_seal_helper("seal-mail-record", {
            "plaintext_b64": base64.b64encode(plaintext).decode(),
            "recipient_x25519_pubkey_b64": recipient,
            "kind": "calendar",
        })

    return _sealed


def _owner_and_quota(nest: dict):
    """The calendar owner, and a reader of their storage number through the
    approved MDA's ``fauna.bridges.get_quota`` — the figure GETQUOTAROOT shows."""
    admin_sk = nest["admin"]["signing_key"]
    owner = create_actor_and_register(nest["port"], admin_signing_key=admin_sk)
    mda = create_actor_and_register(nest["port"], admin_signing_key=admin_sk)
    _seed_approved_mda(
        nest["db_path"],
        mda["actor_id_bytes"],
        bridge_id=f"e2e-shared-quota-{secrets.token_hex(6)}",
    )

    def quota() -> dict:
        with _ws_client(nest, mda) as mda_ws:
            return mda_ws.call("fauna.bridges.get_quota", {"actor_id": owner["actor_id_bytes"]})

    return owner, quota


@pytest.mark.feature("status-quotas-and-limits")
def test_an_events_bytes_move_the_one_storage_number_mail_reads(quota_nest, run_seal_helper):
    _sealed = _sealer(run_seal_helper)
    owner, quota = _owner_and_quota(quota_nest)

    calendar_id = secrets.token_bytes(32)
    uid_hash = secrets.token_bytes(32)
    body = _sealed(b"BEGIN:VCALENDAR\r\n" + b"x" * 4000 + b"\r\nEND:VCALENDAR\r\n")

    with _ws_client(quota_nest, owner) as owner_ws:
        owner_ws.call(
            "fauna.bridges.provision_calendar",
            {
                "actor_id": owner["actor_id_bytes"],
                "calendar_id": calendar_id,
                "encrypted_metadata": _sealed(b"{}"),
                "update_metadata": False,
            },
        )
        before = quota()

        owner_ws.call(
            "fauna.bridges.put_event_ciphertext",
            {
                "actor_id": owner["actor_id_bytes"],
                "calendar_id": calendar_id,
                "uid_hash": uid_hash,
                "encrypted_body": body,
                "encrypted_index_hint": _sealed(b"hint"),
                "timestamp": 1_700_000_000,
                "ciphertext_size": len(body),
            },
        )
        with_event = quota()

        owner_ws.call(
            "fauna.bridges.delete_event",
            {"actor_id": owner["actor_id_bytes"], "calendar_id": calendar_id, "uid_hash": uid_hash},
        )
        after_delete = quota()

    grew = with_event["storage_bytes_used"] - before["storage_bytes_used"]
    assert grew >= len(body), (
        f"an event's {len(body)} sealed bytes must count toward the storage number mail "
        f"reads: {before['storage_bytes_used']} → {with_event['storage_bytes_used']}"
    )
    assert with_event["message_count_used"] == before["message_count_used"], (
        "an event is not a mailbox message — MESSAGE must not move"
    )
    assert after_delete["storage_bytes_used"] == before["storage_bytes_used"], (
        "deleting the event must give its bytes back: "
        f"{with_event['storage_bytes_used']} → {after_delete['storage_bytes_used']}, "
        f"expected {before['storage_bytes_used']}"
    )


@pytest.mark.feature("status-quotas-and-limits")
def test_an_event_past_the_storage_ceiling_is_refused_and_moves_nothing(
    quota_nest, run_seal_helper
):
    """``caldav-server.md`` § QUOTA → § Enforcement points: once the account's
    one storage number is at its ceiling, a new event is refused with the typed
    ``fauna.bridges.over_quota`` (the code every app renders as "storage is
    full" and the MDA answers ``507``) and the number does not move — while a
    shrinking update of an event already there still passes, so the user can
    always make room. The ceiling is the admin's deployment quota
    (``fauna.bridges.put_imap_policy``), lowered to just above current usage."""
    _sealed = _sealer(run_seal_helper)
    owner, quota = _owner_and_quota(quota_nest)
    admin = quota_nest["admin"]
    calendar_id = secrets.token_bytes(32)
    kept_uid = secrets.token_bytes(32)

    def put(ws, uid_hash: bytes, body: bytes) -> dict:
        return ws.call(
            "fauna.bridges.put_event_ciphertext",
            {
                "actor_id": owner["actor_id_bytes"],
                "calendar_id": calendar_id,
                "uid_hash": uid_hash,
                "encrypted_body": body,
                "encrypted_index_hint": _sealed(b"hint"),
                "timestamp": 1_700_000_000,
                "ciphertext_size": len(body),
            },
        )

    with _ws_client(quota_nest, owner) as owner_ws:
        owner_ws.call(
            "fauna.bridges.provision_calendar",
            {
                "actor_id": owner["actor_id_bytes"],
                "calendar_id": calendar_id,
                "encrypted_metadata": _sealed(b"{}"),
                "update_metadata": False,
            },
        )
        put(owner_ws, kept_uid, _sealed(b"BEGIN:VCALENDAR\r\n" + b"k" * 2000 + b"\r\nEND:VCALENDAR\r\n"))
        used = quota()["storage_bytes_used"]

        admin_ws = _ws_client(quota_nest, {
            "actor_id_bytes": bytes(admin["signing_key"].verify_key),
            "signing_key": admin["signing_key"],
        })
        with admin_ws:
            admin_ws.call("fauna.bridges.put_imap_policy", {"storage_bytes_default": used + 64})

        with pytest.raises(RpcCallError) as refused:
            put(owner_ws, secrets.token_bytes(32),
                _sealed(b"BEGIN:VCALENDAR\r\n" + b"n" * 4000 + b"\r\nEND:VCALENDAR\r\n"))
        assert refused.value.code == "fauna.bridges.over_quota"
        assert quota()["storage_bytes_used"] == used, (
            "a refused event must write nothing: the storage number moved"
        )

        put(owner_ws, kept_uid, _sealed(b"BEGIN:VCALENDAR\r\nsmaller\r\nEND:VCALENDAR\r\n"))
        assert quota()["storage_bytes_used"] < used, (
            "a shrinking update must pass at the ceiling and give bytes back"
        )


# The two witnesses below lower the admin's deployment policy on this test's
# own nest (`quota_nest` is function-scoped), so nothing needs putting back.


def _card_sealer(run_seal_helper):
    """``_sealer``'s twin for the address book: a genuine ``card`` record
    envelope, the only body shape ``put_card_ciphertext`` accepts."""
    recipient = base64.b64encode(PrivateKey.generate().public_key.encode()).decode()

    def _sealed(plaintext: bytes) -> bytes:
        return run_seal_helper("seal-mail-record", {
            "plaintext_b64": base64.b64encode(plaintext).decode(),
            "recipient_x25519_pubkey_b64": recipient,
            "kind": "card",
        })

    return _sealed


def _seed_approved_mta(db_path: str, ed25519_pubkey: bytes, bridge_id: str) -> None:
    """Enroll ``ed25519_pubkey`` as an approved MTA bridge — the
    ``_seed_approved_mda`` shortcut with ``role='mta'``, the one class
    ``fauna.bridges.place_inbound_invite`` admits (``bridge_method_allowlist.rs``)."""
    import sqlite3
    import time

    now = int(time.time())
    conn = sqlite3.connect(db_path, timeout=10.0)
    try:
        conn.execute(
            "INSERT OR REPLACE INTO bridge_service_users"
            " (ed25519_pubkey, x25519_pubkey, role, bridge_id, status,"
            "  created_at, approved_at)"
            " VALUES (?, ?, 'mta', ?, 'approved', ?, ?)",
            (ed25519_pubkey, None, bridge_id, now, now),
        )
        conn.commit()
    finally:
        conn.close()


def _admin_ws(nest: dict):
    admin = nest["admin"]
    return _ws_client(nest, {
        "actor_id_bytes": bytes(admin["signing_key"].verify_key),
        "signing_key": admin["signing_key"],
    })


def _personal_calendar_id() -> bytes:
    """The deterministic id of the lazy ``Personal`` calendar an emailed
    invitation lands in (``fauna_protocol::dav_identity::personal_calendar_id``
    = blake3 of the slug ``personal``)."""
    import blake3

    return blake3.blake3(b"personal").digest()


def _provision(ws, owner: dict, calendar_id: bytes, book_id: bytes, sealed, card_sealed) -> None:
    ws.call("fauna.bridges.provision_calendar", {
        "actor_id": owner["actor_id_bytes"], "calendar_id": calendar_id,
        "encrypted_metadata": sealed(b"{}"), "update_metadata": False,
    })
    ws.call("fauna.bridges.provision_addressbook", {
        "actor_id": owner["actor_id_bytes"], "addressbook_id": book_id,
        "encrypted_metadata": card_sealed(b"Contacts"), "update_metadata": False,
    })


def _put_card(ws, owner: dict, book_id: bytes, body: bytes, sealed) -> dict:
    return ws.call("fauna.bridges.put_card_ciphertext", {
        "actor_id": owner["actor_id_bytes"], "addressbook_id": book_id,
        "uid_hash": secrets.token_bytes(32), "encrypted_body": body,
        "encrypted_index_hint": sealed(b"hint"),
        "timestamp": 1_700_000_000, "ciphertext_size": len(body), "if_match": None,
    })


@pytest.mark.feature("status-quotas-and-limits")
def test_a_full_account_refuses_a_new_card_and_leaves_an_emailed_invitation_unplaced(
    quota_nest, run_seal_helper
):
    """``caldav-server.md`` § QUOTA → § Enforcement points: the one storage
    number binds every write that stores an event or a card. With the account
    at its ceiling, a new contact card is refused with the typed
    ``fauna.bridges.over_quota`` (the MDA's ``507``) and writes nothing, and an
    emailed invitation the mail door hands over (``place_inbound_invite``, the
    MTA's call once the mail copy is delivered) answers ``over_quota`` and is
    not placed — an outcome the MTA logs, never a failed delivery. Before the
    ceiling is lowered the same card and invitation shapes land, so each
    refusal is the quota's."""
    _sealed = _sealer(run_seal_helper)
    _card = _card_sealer(run_seal_helper)
    owner, quota = _owner_and_quota(quota_nest)
    mta = create_actor_and_register(
        quota_nest["port"], admin_signing_key=quota_nest["admin"]["signing_key"])
    _seed_approved_mta(
        quota_nest["db_path"], mta["actor_id_bytes"],
        bridge_id=f"e2e-quota-mta-{secrets.token_hex(6)}",
    )
    calendar_id = _personal_calendar_id()
    book_id = secrets.token_bytes(32)

    def place_invite(uid_hash: bytes) -> dict:
        body = _sealed(b"BEGIN:VCALENDAR\r\n" + b"i" * 3000 + b"\r\nEND:VCALENDAR\r\n")
        with _ws_client(quota_nest, mta) as mta_ws:
            return mta_ws.call("fauna.bridges.place_inbound_invite", {
                "actor_id": owner["actor_id_bytes"], "uid_hash": uid_hash,
                "encrypted_body": body, "encrypted_index_hint": _sealed(b"hint"),
                "timestamp": 1_700_000_000, "sender_address": "organizer@external.test",
            })

    def on_calendar(ws) -> set[bytes]:
        reply = ws.call("fauna.bridges.query_events", {
            "actor_id": owner["actor_id_bytes"], "calendar_id": calendar_id,
            "since_modseq": None, "after_event_id": None, "limit": 200,
        })
        return {bytes(e["uid_hash"]) for e in reply.get("events", [])}

    with _ws_client(quota_nest, owner) as owner_ws:
        _provision(owner_ws, owner, calendar_id, book_id, _sealed, _card)

        # With room, the card and the invitation both land.
        _put_card(owner_ws, owner, book_id,
                  _card(b"BEGIN:VCARD\r\nFN:Has Room\r\nEND:VCARD\r\n"), _sealed)
        placed_uid = secrets.token_bytes(32)
        placed = place_invite(placed_uid)
        assert placed.get("outcome") == "placed", (
            f"with room, an emailed invitation must be placed: {placed!r}"
        )
        assert placed_uid in on_calendar(owner_ws), "the placed invitation is not on the calendar"
        used = quota()["storage_bytes_used"]

        with _admin_ws(quota_nest) as admin_ws:
            admin_ws.call("fauna.bridges.put_imap_policy", {"storage_bytes_default": used + 64})

        with pytest.raises(RpcCallError) as refused:
            _put_card(owner_ws, owner, book_id, _card(
                b"BEGIN:VCARD\r\nFN:No Room\r\nNOTE:" + b"c" * 3000 + b"\r\nEND:VCARD\r\n"),
                _sealed)
        assert refused.value.code == "fauna.bridges.over_quota", (
            f"a new card past the ceiling must be refused over_quota; got {refused.value!r}"
        )

        unplaced_uid = secrets.token_bytes(32)
        unplaced = place_invite(unplaced_uid)
        assert unplaced.get("outcome") == "over_quota", (
            f"an emailed invitation for a full account must answer over_quota: {unplaced!r}"
        )
        assert unplaced_uid not in on_calendar(owner_ws), (
            "an invitation the account has no room for must not reach the calendar"
        )
        assert quota()["storage_bytes_used"] == used, (
            "the refused card and the unplaced invitation must write nothing"
        )


@pytest.mark.feature("status-quotas-and-limits")
def test_a_lower_message_limit_never_refuses_an_event_or_a_card(quota_nest, run_seal_helper):
    """``caldav-server.md`` § QUOTA: ``MESSAGE`` stays the mail placement count
    — an event is not a mailbox message — and only the STORAGE ceiling binds a
    DAV write (§ Enforcement points; ``enforce_dav_write_quota``). With the
    deployment's message limit lowered to zero, so that no further mail message
    would fit, a new event and a new card are still stored, and MESSAGE does
    not move."""
    _sealed = _sealer(run_seal_helper)
    _card = _card_sealer(run_seal_helper)
    owner, quota = _owner_and_quota(quota_nest)
    calendar_id = secrets.token_bytes(32)
    book_id = secrets.token_bytes(32)

    with _ws_client(quota_nest, owner) as owner_ws:
        _provision(owner_ws, owner, calendar_id, book_id, _sealed, _card)
        with _admin_ws(quota_nest) as admin_ws:
            admin_ws.call("fauna.bridges.put_imap_policy", {"message_count_default": 0})
        before = quota()
        assert before["message_count_limit"] == 0, f"sanity: the lowered limit must read back: {before!r}"

        event = _sealed(b"BEGIN:VCALENDAR\r\n" + b"e" * 2000 + b"\r\nEND:VCALENDAR\r\n")
        put_event = owner_ws.call("fauna.bridges.put_event_ciphertext", {
            "actor_id": owner["actor_id_bytes"], "calendar_id": calendar_id,
            "uid_hash": secrets.token_bytes(32), "encrypted_body": event,
            "encrypted_index_hint": _sealed(b"hint"),
            "timestamp": 1_700_000_000, "ciphertext_size": len(event),
        })
        assert put_event.get("outcome") == "created", (
            f"a message limit must never refuse an event: {put_event!r}"
        )
        put_card = _put_card(owner_ws, owner, book_id, _card(
            b"BEGIN:VCARD\r\nFN:Under A Message Limit\r\nEND:VCARD\r\n"), _sealed)
        assert put_card.get("outcome") == "created", (
            f"a message limit must never refuse a card: {put_card!r}"
        )
        after = quota()
        assert after["message_count_used"] == before["message_count_used"], (
            "an event or a card is not a mailbox message — MESSAGE must not move"
        )
        assert after["storage_bytes_used"] > before["storage_bytes_used"], (
            "the event and the card must still count toward the storage number"
        )
