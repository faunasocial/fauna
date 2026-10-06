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

The conversation arm — a takedown that writes NO obligation row at all, where a
gate reading ``obligation_action_records`` alone would refuse a triple the doc
guarantees (§ Legal takedown → *Conversations*) — is pinned nest-side, where
seeding a sealed MLS record is cheap:
``bins/fauna-nest/src/db/moderation.rs::a_taken_down_conversation_record_is_appealable_with_no_obligation_row``.

Latency-independent (convention 14): every assert here is on an RPC reply, so
there is nothing to wait for.
"""

import time

import pytest

from clients.ws_rpc_admin_client import RpcCallError
from common import create_actor_and_register
from tests.api import ws_api
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
