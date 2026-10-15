"""A tip's payee, built through the nest's production doors: a user who has
linked a Nostr key and shared a post, which now sits on the nest's own relay.

A zap receipt names the payee's Nostr key (``p``) and the zapped event (``e``),
and the nest believes it only for an event its relay holds
(`docs/goal/behavior/monetization.md` § Zap receipts — the trust model), so
every nest-side tip witness starts here. The drive is the one an app makes: a
``nostr`` bridge link that generates the key, a post, and the share toggle that
signs the account's posts onto the relay.

Lifted out of `tests/api/test_tip_notification.py` when the trust-gate witness
(`tests/api/test_zap_trust_gate.py`) needed the same payee — one home, so the
two cannot drift.
"""

from __future__ import annotations

import secrets
import time

from clients.ws_rpc_admin_client import WsRpcAdminClient
from common import create_actor_and_register

import fauna_ffi
from helpers import nostr_relay
from helpers.budgets import RPC_ROUNDTRIP_S
from helpers.waiting import wait_until
from tests.api import ws_api
from tests.api.bare import sign_and_encode_post


def ws(nest, user) -> WsRpcAdminClient:
    """A WS-RPC session as ``user`` on ``nest``."""
    return WsRpcAdminClient(
        nest["url"], actor_id=user["actor_id_bytes"], signing_key=bytes(user["signing_key"]),
    )


def shared_post(nest) -> tuple[dict, str, dict]:
    """A user whose post sits on the nest's relay: ``(user, post_id, note)``."""
    user = create_actor_and_register(
        nest["port"], base_url=nest["url"], admin_signing_key=nest["admin"]["signing_key"],
    )
    reply = ws_api.bridge_link(nest["port"], user, "nostr", "generate")
    assert reply["linked"], reply
    pubkey = fauna_ffi.nostr_npub_to_hex(reply["identity"]["value"])

    text = f"a post worth a tip {secrets.token_hex(4)}"
    post_id = ws_api.create_post(
        nest["port"], user,
        sign_and_encode_post(user["signing_key"], int(time.time() * 1_000_000), text),
    )
    # Turning sharing on signs the account's posts onto the relay there and
    # then; a post written while it is already on waits for the next sweep.
    ws_api.bridge_set_settings(nest["port"], user, "nostr", {"expose_content": True})
    relay = nostr_relay.nest_relay_url(nest)
    note = wait_until(
        lambda: next(
            (e for e in nostr_relay.query(relay, {"authors": [pubkey], "kinds": [1]})
             if e["content"] == text),
            None,
        ),
        RPC_ROUNDTRIP_S,
        diagnose=lambda: f"the shared post {text!r} never reached the nest's relay",
    )
    return user, post_id, note
