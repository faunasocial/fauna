"""tier_3 API: ``fauna.moderation.appeal`` records against a REAL decision.

``moderation.md`` § Legal takedown — every takedown owes a transparency triple:
a visible tombstone, an appeal by ``content_id``, and a permanent audit row.
The appeal leg was live but **unguarded**: the handler audit-logged any
``{content_id, reason}`` a caller sent, with no check that the content had ever
been actioned (§ Errors & edge cases recorded this as verified-against-code).
That is not an appeal trail — it is an unauthenticated write surface onto one.

This module is the wire-level companion to the app journey
(``tests/test_moderation_appeal.py``, the ``[app]``-tagged witness for
``moderation-queue`` outcome 3) — e2e convention 5: an appeal that fails could
be the UI's fault or the nest's, so the nest half is asserted directly.

The arms:

1. An appeal against a post nobody has actioned is **refused**
   (``fauna.moderation.not_found``) — the gate.
2. After an admin takes that post down through ``fauna.moderation.legal_takedown``,
   the same appeal is **recorded** — the author's queue row is the handle.
3. A reasonless appeal is refused (``fauna.moderation.invalid_params``) — the
   guard the shared ``appeal_form_view`` renders client-side.
4. After the admin **overturns** the takedown, the appeal still records: the
   obligation row survives as additive history (§ Persistence), so an overturn
   never retroactively revokes the handle.

A second test pins the residue the gate left (``moderation.md`` § Errors &
edge cases): a post appeal is the **author's** alone, the reason is bounded by
the shared ``MAX_APPEAL_REASON_BYTES``, and a repeat while the caller's appeal
is pending collapses instead of appending another permanent audit row.

A third test carries the same triple to a **conversation** message
(§ Legal takedown → *Conversations*): the nest cannot read the sealed record,
but it attested its sender on ``channel.send``, so the takedown writes that
author's queue row exactly as a post's does — the author appeals it, a member
who merely holds the record id is refused, and the overturn keeps the row. The
unattested arm — a record no attestation names, whose takedown writes NO
obligation row and whose appeal stays any holder's — is pinned nest-side,
where seeding a record with no authenticated sender is cheap:
``bins/fauna-nest/src/db/moderation.rs::a_taken_down_conversation_record_is_appealable_with_no_obligation_row``.

Latency-independent (convention 14): every assert here is on an RPC reply, so
there is nothing to wait for.
"""

import os
import time

import blake3
import cbor2
import pytest

from clients.ws_rpc_admin_client import RpcCallError
from common import create_actor_and_register
from tests.api import conv_api, ws_api
from tests.api.bare import sign_and_encode_post

pytestmark = pytest.mark.tier_3

REFERENCE = "Court order 42/2026"
REASON = "I hold the licence for this recording."
# `fauna_protocol::moderation::MAX_APPEAL_REASON_BYTES` — the wire bound, in
# UTF-8 bytes. Mirrored here (a Python test cannot import the Rust constant);
# the Rust conformance test pins the constant itself.
MAX_APPEAL_REASON_BYTES = 4096


def _admin_actor(admin_sk) -> dict:
    """The claimed admin as a ``ws_api`` actor dict (``actor_id_bytes`` +
    ``signing_key``) — the shape ``_client_for`` takes."""
    return {
        "actor_id_bytes": bytes(admin_sk.verify_key),
        "signing_key": admin_sk,
    }


@pytest.mark.feature("moderation-queue")
def test_an_appeal_needs_a_real_enforcement_record(two_nodes):
    """refuse (un-actioned) → take down → record → refuse (no reason) →
    restore → still record."""
    port = two_nodes["port_a"]
    admin_sk = two_nodes["admin_sk_a"]
    admin = _admin_actor(admin_sk)

    alice = create_actor_and_register(port, admin_signing_key=admin_sk)
    now_us = int(time.time() * 1_000_000)
    post = sign_and_encode_post(
        alice["signing_key"], now_us, "a post that will be compelled away", tags=[]
    )
    post_id = ws_api.create_post(port, alice, post)

    # --- 1. The gate: nothing has been actioned, so there is nothing to appeal.
    with pytest.raises(RpcCallError) as refused:
        ws_api.moderation_appeal(port, alice, post_id, REASON)
    assert refused.value.code == "fauna.moderation.not_found", (
        "an appeal against content no enforcement ever touched must be "
        f"refused, got {refused.value.code}"
    )

    # --- 2. The admin takes it down; the obligation row IS the appeal handle.
    reply = ws_api.moderation_legal_takedown(
        port, admin, post_id, legal_reference=REFERENCE
    )
    assert reply.get("status") == "taken_down", reply

    actions = ws_api.moderation_actions(port, alice)
    assert len(actions) == 1, (
        f"the author's queue must carry exactly one takedown row, got {actions!r}"
    )
    assert actions[0]["content_id"] == post_id

    recorded = ws_api.moderation_appeal(port, alice, post_id, REASON)
    assert recorded.get("status") == "appeal_recorded", recorded
    assert recorded.get("content_id") == post_id

    # --- 3. The reason is structural, not decoration.
    with pytest.raises(RpcCallError) as no_reason:
        ws_api.moderation_appeal(port, alice, post_id, "")
    assert no_reason.value.code == "fauna.moderation.invalid_params", (
        f"a reasonless appeal must be refused, got {no_reason.value.code}"
    )

    # --- 4. An overturn does not revoke the handle: the takedown row stays as
    # additive history, so the decision that was made is still appealable.
    restored = ws_api.moderation_legal_takedown(
        port, admin, post_id, legal_reference="", restore=True
    )
    assert restored.get("status") == "restored", restored

    still = ws_api.moderation_appeal(port, alice, post_id, REASON)
    assert still.get("status") == "appeal_recorded", (
        "an overturned takedown is still a decision that was made — the appeal "
        f"handle survives it, got {still!r}"
    )


@pytest.mark.feature("moderation-queue")
def test_a_post_appeal_is_the_authors_bounded_and_once_per_decision(two_nodes):
    """non-author refused → over-long refused → author records → repeat
    collapses."""
    port = two_nodes["port_a"]
    admin_sk = two_nodes["admin_sk_a"]
    admin = _admin_actor(admin_sk)

    alice = create_actor_and_register(port, admin_signing_key=admin_sk)
    bob = create_actor_and_register(port, admin_signing_key=admin_sk)
    now_us = int(time.time() * 1_000_000)
    post = sign_and_encode_post(
        alice["signing_key"], now_us, "a post a court will order away", tags=[]
    )
    post_id = ws_api.create_post(port, alice, post)
    reply = ws_api.moderation_legal_takedown(
        port, admin, post_id, legal_reference=REFERENCE
    )
    assert reply.get("status") == "taken_down", reply

    # --- A reader of the post saw its id, but the appeal is not theirs.
    with pytest.raises(RpcCallError) as not_author:
        ws_api.moderation_appeal(port, bob, post_id, REASON)
    assert not_author.value.code == "fauna.moderation.permission_denied", (
        "a post appeal from anyone but its author must be refused, got "
        f"{not_author.value.code}"
    )

    # --- The reason is bounded, even for the author.
    with pytest.raises(RpcCallError) as too_long:
        ws_api.moderation_appeal(
            port, alice, post_id, "x" * (MAX_APPEAL_REASON_BYTES + 1)
        )
    assert too_long.value.code == "fauna.moderation.invalid_params", (
        f"an over-long reason must be refused, got {too_long.value.code}"
    )

    # --- The author's appeal records; a repeat before any decision collapses.
    first = ws_api.moderation_appeal(port, alice, post_id, REASON)
    assert first.get("status") == "appeal_recorded", first
    again = ws_api.moderation_appeal(port, alice, post_id, REASON + " Again.")
    assert again.get("status") == "appeal_already_recorded", (
        "a repeat while the appeal is pending must collapse onto it, not "
        f"append another permanent audit row, got {again!r}"
    )
    assert again.get("content_id") == post_id


def _conv_record_id(envelope: bytes) -> str:
    """The conversation message's ``content_id``: the BLAKE3 digest of the
    dag-cbor ``ConvRecordEnvelope`` the nest files the sealed payload under —
    the same derivation ``fauna_mls::segments::derive_record_cid`` makes for a
    reporting client. A wrong derivation fails loudly: the takedown answers
    ``not_found``."""
    record = cbor2.dumps(
        {"format_version": 1, "sealed_payload": bytes(envelope)}, canonical=True
    )
    return blake3.blake3(record).hexdigest()


@pytest.mark.feature("moderation-queue")
def test_a_conversation_appeal_is_the_attested_authors(two_nodes):
    """send → take down → author's queue row → author records, member refused →
    restore keeps the row."""
    port = two_nodes["port_a"]
    admin_sk = two_nodes["admin_sk_a"]
    admin = _admin_actor(admin_sk)

    alice = create_actor_and_register(port, admin_signing_key=admin_sk)
    bob = create_actor_and_register(port, admin_signing_key=admin_sk)
    channel = os.urandom(32).hex()
    throwaway_recipient_kp = conv_api.mint_key_packages(os.urandom(32), 1)[0]
    _, _, envelope = conv_api.mint_group_welcome_with_message(
        bytes(alice["signing_key"]), throwaway_recipient_kp, "a message a court will order away"
    )
    seq = conv_api.channel_send(port, alice, channel, envelope)
    content_id = _conv_record_id(envelope)

    # Bob reads the channel, so he holds the record — and the nest names its
    # attested author on the entry, the premise the queue row rests on.
    entries = [m for m in conv_api.channel_fetch(port, bob, channel) if m["seq"] == seq]
    assert len(entries) == 1, entries
    assert entries[0].get("author") == bytes(alice["signing_key"].verify_key).hex(), (
        f"the nest must attest the sender of an authenticated send: {entries[0]!r}"
    )

    reply = ws_api.moderation_legal_takedown(
        port, admin, content_id, content_type="conversation", legal_reference=REFERENCE
    )
    assert reply.get("status") == "taken_down", reply

    rows = [a for a in ws_api.moderation_actions(port, alice) if a["content_id"] == content_id]
    assert len(rows) == 1, (
        f"the attested author's queue must carry the conversation takedown, got {rows!r}"
    )
    assert rows[0]["content_type"] == "conversation", rows[0]
    assert ws_api.moderation_actions(port, bob) == [], "only the author's queue carries it"

    recorded = ws_api.moderation_appeal(port, alice, content_id, REASON)
    assert recorded.get("status") == "appeal_recorded", recorded

    with pytest.raises(RpcCallError) as not_author:
        ws_api.moderation_appeal(port, bob, content_id, REASON)
    assert not_author.value.code == "fauna.moderation.permission_denied", (
        "once a queue row names the author, a member holding the record id must "
        f"be refused, got {not_author.value.code}"
    )

    restored = ws_api.moderation_legal_takedown(
        port, admin, content_id, content_type="conversation", legal_reference="", restore=True
    )
    assert restored.get("status") == "restored", restored
    kept = [a for a in ws_api.moderation_actions(port, alice) if a["content_id"] == content_id]
    assert len(kept) == 1, f"an overturn keeps the queue row as history, got {kept!r}"
