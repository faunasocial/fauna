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
    """A genuine recipient envelope — the only body shape the PUT accepts (the
    at-rest seal is proven at the wire edge), from the same shared-Rust seal the
    apps and the MDA use. The recipient key is throwaway: these tests read sizes,
    never bodies."""
    recipient = base64.b64encode(PrivateKey.generate().public_key.encode()).decode()

    def _sealed(plaintext: bytes) -> bytes:
        return run_seal_helper("seal-mail-record", {
            "plaintext_b64": base64.b64encode(plaintext).decode(),
            "recipient_x25519_pubkey_b64": recipient,
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
                "encrypted_metadata": b"{}",
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
                "encrypted_metadata": b"{}",
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
