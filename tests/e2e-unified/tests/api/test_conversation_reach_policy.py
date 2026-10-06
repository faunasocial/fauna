"""The inbox mode at conversation initiation — full-stack (tier_3) API E2E.

Witnesses two `privacy-settings` outcomes, both `docs/goal/behavior/direct-messages.md`
§ Reach policy:

* **5** — *"A stranger cannot start a conversation with you unless your inbox
  mode allows it, while people you have accepted always get through."*
* **6** — *"With your inbox closed, someone on another nest cannot reach you
  either — nothing of theirs lands on your side."*

**Why these had no witness, and why that was the dangerous kind of gap.** The
rule is enforced (`fauna_core::data::dm_initiation_mode_verdict`, applied at
`conversations_handlers.rs::conversation_initiation_reach_gate`) and pinned by
Rust conformance tests. But the e2e suites *arrange around* it: the shared
helper sets the recipient's mode open and accepts the contact precisely so a
Welcome delivers (`tests/api/conv_api.py:298,324-329`). So every e2e in the
tree depended on the rule being permissive and not one asserted the refusal —
a regression that stopped enforcing it would have turned the whole suite
greener, not redder.

**Both outcomes are asserted as two-way discriminators**, because a refusal
test alone cannot tell "the mode was enforced" from "this call never works":

* outcome 5 — the same stranger, same call, refused under `closed` and
  delivered once the recipient accepts them as a contact. The accepted arm is
  the outcome's own second clause, and it is also what proves the refusal was
  the *mode* rather than some unrelated gate.
* outcome 6 — the same peer nest, same relayed Welcome, refused under `closed`
  and delivered under `open`, with the recipient's inbox asserted empty after
  the refusal ("nothing of theirs lands").

**Scope note, from the goal doc.** Cross-nest, only the `closed` arm is
enforced (since 2026-08-23); `allow_knock`/`contacts_only` across nests are
DECLARED gaps — their verdicts need a contact edge and the federation wire
carries no signed sender (`direct-messages.md:401-423`). So outcome 6 is about
`closed` alone, which is exactly what its sentence says.

Process safety: no ``pkill``/``killall``; both nests are session-scoped
fixtures torn down by their own fixtures.
"""

from __future__ import annotations

import secrets

import pytest

from clients.ws_rpc_admin_client import RpcCallError, WsRpcAdminClient
from clients.ws_rpc_federation_client import FederationChannelClient
from common.auth import create_actor_and_register
from tests.api import conv_api

pytestmark = pytest.mark.tier_3

#: `rpc_errors::forbidden_ns("conversations", …)` — the same-nest refusal.
CONVERSATIONS_FORBIDDEN = "fauna.conversations.forbidden"
#: `rpc_errors::forbidden_ns("federation", …)` — the cross-nest refusal.
FEDERATION_FORBIDDEN = "fauna.federation.forbidden"
#: The refusal text both rails share, deliberately identical and opaque so a
#: policy refusal and a supervised-ward refusal are indistinguishable to the
#: sender (`conversations_handlers.rs:2999-3004`).
REFUSAL_TEXT = "recipient is not accepting new conversations"


def _ws(nest, actor) -> WsRpcAdminClient:
    return WsRpcAdminClient(
        nest["url"],
        actor_id=actor["actor_id_bytes"],
        signing_key=bytes(actor["signing_key"]),
    )


def _set_inbox_mode(nest, actor, mode: str) -> None:
    with _ws(nest, actor) as ws:
        ws.call("fauna.inbox.mode.set", {"mode": mode})


def _inbox_items(nest, actor) -> list:
    with _ws(nest, actor) as ws:
        return ws.call("fauna.inbox.fetch", {"limit": 0}).get("items", [])


@pytest.mark.feature("privacy-settings")
def test_a_stranger_is_refused_until_the_recipient_accepts_them(nest_instance):
    """Outcome 5: the mode refuses a stranger; an accepted contact gets through.

    `closed` is the mode used because its verdict for a stranger is
    unambiguous `Suppress`. The default `allow_knock` also refuses the Welcome,
    but routes the sender to the contact-request path instead — a different
    outcome with a different sentence, and folding the two would make this
    assertion say less than it looks like it says.

    The accepted arm is not a nicety: `dm_initiation_mode_verdict`
    (`libs/fauna-core/src/data.rs:1307-1322`) short-circuits on an
    `Accepted`/`Confirmed` contact edge BEFORE the mode is consulted at all, so
    a regression that inverted the two would still refuse strangers and would
    silently start refusing the people you know.
    """
    admin_sk = nest_instance["admin"]["signing_key"]
    recipient = create_actor_and_register(nest_instance["port"], admin_signing_key=admin_sk)
    stranger = create_actor_and_register(nest_instance["port"], admin_signing_key=admin_sk)

    _set_inbox_mode(nest_instance, recipient, "closed")

    channel_hex = secrets.token_bytes(32).hex()
    welcome = secrets.token_bytes(96)

    with pytest.raises(RpcCallError) as refusal:
        conv_api.welcome_deliver(
            nest_instance["port"],
            stranger,
            recipient["actor_id_hex"],
            channel_hex,
            welcome,
        )
    assert CONVERSATIONS_FORBIDDEN in str(refusal.value), (
        f"a stranger's Welcome to a closed inbox must be refused with "
        f"{CONVERSATIONS_FORBIDDEN}, got {refusal.value!r}"
    )
    assert REFUSAL_TEXT in str(refusal.value), refusal.value

    assert _inbox_items(nest_instance, recipient) == [], (
        "a refused Welcome must leave nothing in the recipient's inbox — a "
        "refusal that still charged a row against them would be the quota and "
        "push-event cost the gate exists to prevent"
    )

    # The recipient accepts the stranger. The contact edge is what
    # `dm_initiation_mode_verdict` short-circuits on, so the identical call
    # must now succeed WITHOUT the mode being touched.
    with _ws(nest_instance, recipient) as recipient_ws:
        recipient_ws.call(
            "fauna.knocks.accept", {"peer_id": stranger["actor_id_hex"]}
        )

    inbox_id = conv_api.welcome_deliver(
        nest_instance["port"],
        stranger,
        recipient["actor_id_hex"],
        channel_hex,
        welcome,
    )
    assert inbox_id >= 1, (
        f"an accepted contact must get through under EVERY mode — the inbox "
        f"stayed `closed` for this call — got inbox_id={inbox_id!r}"
    )


@pytest.mark.feature("privacy-settings")
def test_a_closed_inbox_holds_against_a_sender_on_another_nest(
    nest_instance, second_nest
):
    """Outcome 6: the `closed` arm is enforced at federation ingest.

    The mode is the *recipient's own nest's* fact, so enforcement is the
    RECEIVING nest's, at `federation_handlers.rs::welcome_deliver_handler` —
    the relay leg on the sender's nest does not consult it and never will
    (`direct-messages.md:401-406`). This drives that ingest directly over the
    nest↔nest federation channel, which is what a peer nest relaying a
    stranger's Welcome really does.

    `welcome.deliver` is **open-federation**: any handshake-verified peer
    reaches it and its wire carries no signed sender. That is exactly why the
    `closed` arm can be enforced here while the other three modes cannot — a
    closed inbox is a fact about the recipient alone and needs no sender
    identity — and why this test is about `closed` only.
    """
    recipient = create_actor_and_register(
        nest_instance["port"], admin_signing_key=nest_instance["admin"]["signing_key"]
    )
    channel_hex = secrets.token_bytes(32).hex()
    welcome = secrets.token_bytes(96)

    def relay():
        """One relayed Welcome from the peer nest to this one."""
        with FederationChannelClient(initiator=second_nest, target=nest_instance) as fed:
            return fed.call(
                "fauna.federation.welcome.deliver",
                {
                    "recipient_actor_id": recipient["actor_id_hex"],
                    "channel_id": channel_hex,
                    "welcome_bytes": welcome,
                    "channel_type": None,
                    "group_id": None,
                    "origin_nest_url": second_nest["url"],
                },
            )

    _set_inbox_mode(nest_instance, recipient, "closed")
    with pytest.raises(RpcCallError) as refusal:
        relay()
    assert FEDERATION_FORBIDDEN in str(refusal.value), (
        f"a closed inbox must refuse a cross-nest Welcome with "
        f"{FEDERATION_FORBIDDEN}, got {refusal.value!r}"
    )
    assert REFUSAL_TEXT in str(refusal.value), refusal.value

    # "nothing of theirs lands on your side" — the outcome's own second half.
    # The Welcome door lands strictly more than the sibling inbox door does: an
    # inbox row charged against the recipient's quota, a `PushEvent::Welcome` on
    # their live socket, a device push, and a `register_actor_channel` seating
    # them on the channel. None of it may have happened.
    assert _inbox_items(nest_instance, recipient) == [], (
        "a refused cross-nest Welcome must leave the recipient's inbox empty"
    )

    # The same peer, the same relayed Welcome, under `open` — without this the
    # refusal above is indistinguishable from a federation channel that cannot
    # deliver to this recipient at all.
    _set_inbox_mode(nest_instance, recipient, "open")
    reply = relay()
    assert reply["inbox_id"] >= 1, (
        f"the identical relayed Welcome must deliver once the inbox is open, "
        f"got {reply!r}"
    )
