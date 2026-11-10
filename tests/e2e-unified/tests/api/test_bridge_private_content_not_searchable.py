"""A private message that reaches a user over a linked network never enters the
nest's search; only what that network published openly can be found there.
``content-index.md`` § Bridge content in the Search corpus owns the policy: the
split is by content class, never by bridge — a bridge's public content may enter
the nest-side Search corpus (``content_fts``, served by ``fauna.search.query``),
its private content (bridge DMs, Nostr gift wraps, mail bodies) never does. On a
real ``fauna-nest`` binary.

Nostr is the bridge walked, because the nest's own relay lets a test deposit
both classes the way the network does, from outside, with no app:

* **public** — the user's own Nostr app writes a kind-1 note to the nest's relay
  over NIP-42. The relay store is a transit point that indexes it under
  ``bridge.nostr`` (``nostr::store::index_into_search_corpus``), so its word is
  the positive control.
* **private, read by the nest** — a far stranger sends the user a NIP-17 gift
  wrap at the nest's unauthenticated inbox. The user's key is on the nest, so
  the nest unwraps it, reads the plaintext, seals it to the user's recipient key
  and deposits it in the user's bridged-conversation inbox
  (``sync_worker::process_gift_wrap_inbound`` → ``bridge_legs::deposit_gated``).
  That the DM shows up in the inbox proves the nest held its words; that search
  then cannot find them is the outcome.
* **private, carried in clear** — the user's Nostr app writes a legacy NIP-04
  kind-4 DM whose content is plain text (the relay does not check encryption).
  The relay stores it; ``store::is_search_corpus_kind`` keeps kind 4 out of the
  corpus.

Two signed-in callers search: the recipient (whose search also covers their own
bridge-delivered content) and someone else on the nest. ``fauna.search.query``
is User-class only, so there is no anonymous search.

Convention 14: the absence of the private words is asserted only after the same
caller's search has found the public note, and after the DM has reached the
inbox. Never a wall-clock wait.

Witness of ``bridges`` outcome 15.
"""

import os
import secrets
import time

import pytest

import fauna_ffi
from clients.ws_rpc_admin_client import WsRpcAdminClient
from common import create_actor_and_register
from helpers import nostr_relay
from helpers.budgets import RPC_ROUNDTRIP_S
from helpers.recipient_seal_key import provision_recipient_seal_key
from helpers.waiting import wait_until
from tests.api import ws_api
from tests.api.test_nostr_nest import NOSTR_DOMAIN, nostr_home_nest  # noqa: F401 — fixture

pytestmark = pytest.mark.tier_3

#: The nest's first-party Nostr DM leg (`db/bridged_conversations.rs`).
NOSTR_LEG = "nostr"


def _user(nest: dict) -> dict:
    return create_actor_and_register(
        nest["port"], base_url=nest["url"], admin_signing_key=nest["admin"]["signing_key"],
    )


def _ws(nest: dict, user: dict) -> WsRpcAdminClient:
    return WsRpcAdminClient(
        nest["url"], actor_id=user["actor_id_bytes"], signing_key=bytes(user["signing_key"]),
    )


def _naming(hits: list[dict], word: str) -> list[dict]:
    """The hits whose snippet quotes ``word`` (a hit's ``content_id`` is the
    opaque corpus key, so the snippet is what names the item)."""
    return [h for h in hits if word in (h.get("snippet") or "")]


def _inbound_dms_from(nest: dict, user: dict, peer_pubkey: str) -> list[dict]:
    """The inbound messages in ``user``'s Nostr room with ``peer_pubkey``."""
    with _ws(nest, user) as ws:
        rooms = [
            r for r in ws.call("fauna.bridges.conversation.rooms.list", {})["rooms"]
            if r["bridge_id"] == NOSTR_LEG and peer_pubkey in r["participants"]
        ]
        messages = []
        for room in rooms:
            messages += ws.call(
                "fauna.bridges.conversation.inbox.fetch", {"room_id": bytes(room["room_id"])}
            )["messages"]
    return [m for m in messages if not m["outbound"]]


@pytest.mark.feature("bridges")
def test_a_private_bridge_message_never_enters_search_and_a_public_one_does(
    nostr_home_nest,  # noqa: F811
):
    nest = nostr_home_nest
    port = nest["port"]
    relay = nostr_relay.nest_relay_url(nest)
    public_word = f"publicword{secrets.token_hex(6)}"
    dm_word = f"giftwrapword{secrets.token_hex(6)}"
    clear_dm_word = f"cleardmword{secrets.token_hex(6)}"

    # ── The recipient: a Nostr key on the nest (so it can unwrap their DMs) and
    # a recipient seal key on file (so an unwrapped DM is stored, sealed). ──
    recipient = _user(nest)
    secret = os.urandom(32)
    keys = fauna_ffi.nostr_keys(secret)
    linked = ws_api.bridge_link(port, recipient, "nostr", "import", {"nsec": keys["nsec"]})
    assert linked["linked"], linked
    with _ws(nest, recipient) as ws:
        provision_recipient_seal_key(ws, recipient["actor_id_bytes"])
    bystander = _user(nest)
    callers = {"the recipient": recipient, "another user": bystander}

    def _app_write(template: dict) -> dict:
        event = fauna_ffi.nostr_sign_event(secret, {
            "created_at": int(time.time()), "tags": [], **template,
        })
        accepted, message = nostr_relay.publish(
            relay, event,
            auth=(f"wss://{NOSTR_DOMAIN}/nostr", lambda t: fauna_ffi.nostr_sign_event(secret, t)),
        )
        assert accepted, f"the user's Nostr app writes kind {event['kind']}: {message!r}"
        return event

    # ── Public: a note the user's Nostr app publishes openly. ──
    _app_write({"kind": 1, "content": f"an open note that says {public_word} once"})

    # ── Private, read by the nest: a far stranger's gift-wrapped DM. ──
    stranger_secret = os.urandom(32)
    stranger = fauna_ffi.nostr_keys(stranger_secret)
    wrap = fauna_ffi.nostr_gift_wrap_dm(
        stranger_secret, keys["npub"], f"a private message that says {dm_word} once",
    )
    accepted, message = nostr_relay.publish(relay, wrap)
    assert accepted, f"the nest is the recipient's inbox relay: {message!r}"

    # ── Private, carried in clear: a legacy kind-4 DM the user's app writes. ──
    _app_write({
        "kind": 4, "tags": [["p", stranger["pubkey"]]],
        "content": f"a legacy direct message that says {clear_dm_word} once",
    })

    # ── Positive controls. The DM reached the recipient: the nest held its
    # words. ──
    wait_until(
        lambda: _inbound_dms_from(nest, recipient, stranger["pubkey"]),
        RPC_ROUNDTRIP_S,
        diagnose=lambda: (
            f"the gift-wrapped DM never reached the recipient's inbox; rooms: "
            f"{_ws_rooms(nest, recipient)!r}"
        ),
    )
    # The open note is found, by each caller.
    for who, user in callers.items():
        found = wait_until(
            lambda user=user: _naming(ws_api.search(port, user, public_word), public_word),
            RPC_ROUNDTRIP_S,
            diagnose=lambda user=user: (
                f"search by {who} never found the open note {public_word!r}; hits: "
                f"{ws_api.search(port, user, public_word)!r}"
            ),
        )
        assert [h.get("content_type") for h in found] == ["bridge.nostr"], (
            f"the open note is found once, as the network's public content, by {who}: {found!r}"
        )

    # ── The outcome: neither private message is in search, for anyone. ──
    leaked = {
        (who, word): hits
        for who, user in callers.items()
        for word in (dm_word, clear_dm_word)
        if (hits := ws_api.search(port, user, word))
    }
    assert not leaked, f"a private bridge message's words were found by search: {leaked!r}"


def _ws_rooms(nest: dict, user: dict) -> list[dict]:
    with _ws(nest, user) as ws:
        return ws.call("fauna.bridges.conversation.rooms.list", {})["rooms"]
