"""Minimal cross-nest API test — no WinUI, no IPC, no pywinauto.

Tests whether a user on nest A can authenticate to nest B, and whether nest A
can fetch key packages from / deliver a Welcome to nest B over the federation
channel.

Transport (post WS-RPC conversations migration + the WS-RPC-everywhere rip-out):

* Same-nest channel send/fetch + key-package publish ride
  ``fauna.conversations.{channel,keypackage}.*`` (via ``tests.api.conv_api``);
  their HTTP twins were deleted.
* The **cross-nest** key-package fetch + Welcome delivery legs — once the HTTP
  twins ``GET /api/v1/keypackage/{id}`` / ``POST /api/v1/welcome/{id}`` — now ride
  the nest↔nest federation channel as ``fauna.federation.{keypackage.fetch,
  welcome.deliver}`` (via ``clients.ws_rpc_federation_client``). Federation byte
  fields are CBOR byte strings, like every raw-byte field.
* No surviving control-plane HTTP: the bearer-bootstrap rides
  ``fauna.auth.handshake`` (``common.auth.mint_token_via_handshake``) — its last
  HTTP twin ``POST /api/v1/auth/token`` was deleted at the rip-out endgame — and
  the inbox **read** rides ``fauna.inbox.fetch`` (``conv_api.inbox``).

Nests started via ``start_nest`` require registration, so every actor that
authenticates must first be registered on that nest via the admin API.

Run: pytest tests/e2e/test_cross_nest_api.py -v -s
"""

import os

import pytest
from nacl.signing import SigningKey

from clients.ws_rpc_federation_client import FederationChannelClient
from common import create_actor_and_register, register_user
from common.auth import mint_token_via_handshake

from tests.api import conv_api


# ---------------------------------------------------------------------------
# Auth helpers
# ---------------------------------------------------------------------------

pytestmark = pytest.mark.tier_3

def get_auth_token(nest_url: str, sk: SigningKey) -> str:
    """Mint a bearer over the pre-identity WS-RPC ``fauna.auth.handshake`` kind.

    Delegates to :func:`common.auth.mint_token_via_handshake` — the replacement
    for the deleted HTTP twin ``POST /api/v1/auth/token`` (same direct-auth
    contract; only the transport moves to the anonymous WS).
    """
    return mint_token_via_handshake(nest_url, sk)


# ---------------------------------------------------------------------------
# Fixtures
# ---------------------------------------------------------------------------

@pytest.fixture()
def two_nests(request, nest_mode, tmp_path_factory):
    """Two fresh nests of this run's mode.

    Actors must still be registered per nest before they can authenticate —
    the tests do that via the admin token.

    **Host-driven, so this collects in every mode.** The federation-channel
    client below opens ``/api/v1/federation/ws`` on the listener *from the
    pytest process*, signing the hello as the initiator; no nest ever dials
    another, so ``validate_peer_url``'s globally-routable requirement — and
    with it exclusion class (8) — never comes near these tests
    (``testing.md`` § Default app and nest mode, ruling (2)).

    Until 2026-09-02 this built its own binary through a module-local
    ``node_binary`` fixture and pinned ports 14011/14012. Both are gone: the
    provider owns the start, and it allocates.
    """
    from conftest import _start_dedicated_nest

    nest_a, cleanup_a = _start_dedicated_nest(
        request, nest_mode, tmp_path_factory, "cross-nest-a")
    try:
        nest_b, cleanup_b = _start_dedicated_nest(
            request, nest_mode, tmp_path_factory, "cross-nest-b")
        try:
            yield nest_a, nest_b
        finally:
            cleanup_b()
    finally:
        cleanup_a()


# ---------------------------------------------------------------------------
# Tests
# ---------------------------------------------------------------------------

def test_cross_nest_auth(two_nests):
    """Alice (nest A) can get an auth token from nest B."""
    nest_a, nest_b = two_nests

    alice_sk = SigningKey.generate()
    alice_id = bytes(alice_sk.verify_key).hex()

    # Alice must be registered on each nest she authenticates to.
    register_user(nest_a["port"], alice_id, admin_signing_key=nest_a["admin"]["signing_key"])
    register_user(nest_b["port"], alice_id, admin_signing_key=nest_b["admin"]["signing_key"])

    # Alice gets a token from her own nest
    token_a = get_auth_token(nest_a["url"], alice_sk)
    assert len(token_a) > 0, "Should get token from nest A"
    print(f"Alice token on nest A: {token_a[:20]}...")

    # Alice gets a token from Bob's nest (cross-nest auth)
    token_b = get_auth_token(nest_b["url"], alice_sk)
    assert len(token_b) > 0, "Should get token from nest B"
    print(f"Alice token on nest B: {token_b[:20]}...")


def test_cross_nest_keypackage_fetch(two_nests):
    """Nest A fetches Bob's key package from nest B over the federation channel.

    Bob publishes on nest B via ``fauna.conversations.keypackage.upload``; nest A
    dials the federation channel to nest B and consumes one of Bob's key packages
    via ``fauna.federation.keypackage.fetch`` (the cross-nest leg that replaced
    the deleted ``GET /api/v1/keypackage/{id}`` twin). The peer is verified at
    handshake, so the fetch carries no per-request signature.
    """
    nest_a, nest_b = two_nests

    # Bob registered on nest B (to publish a key package there).
    bob = create_actor_and_register(
        nest_b["port"], admin_signing_key=nest_b["admin"]["signing_key"]
    )
    bob_id = bob["actor_id_hex"]

    # Bob publishes a REAL key package on nest B (self-upload over WS-RPC). The
    # retired `PlaintextStorage` mode used to accept arbitrary bytes;
    # `SealedStorage` (the only mode since 2026-07-12) structurally
    # parses uploads via `fauna_mls::engine::verify_uploaded_key_package`, so a
    # random blob now 400s `invalid_params`.
    fake_kp = conv_api.mint_key_packages(bytes(bob["signing_key"]), 1)[0]
    stored = conv_api.keypackage_upload(nest_b["port"], bob, [fake_kp])
    assert stored == 1, f"Expected stored=1, got {stored}"

    count = conv_api.keypackage_count(nest_b["port"], bob, bob_id)
    assert count >= 1

    # Nest A (initiator) fetches Bob's key package from nest B (listener) over
    # the federation channel. `key_package` is a byte string.
    with FederationChannelClient(initiator=nest_a, target=nest_b) as fed:
        reply = fed.call("fauna.federation.keypackage.fetch", {
            "target_actor_id": bob_id,
        })
    assert reply["key_package"] == bytes(fake_kp)

    # The fetch is destructive — the count drops back to 0.
    count = conv_api.keypackage_count(nest_b["port"], bob, bob_id)
    assert count == 0, f"Expected 0 after consume, got {count}"


@pytest.mark.feature("conversations")
def test_cross_nest_welcome_delivery(two_nests):
    """Nest A delivers a Welcome to Bob's inbox on nest B over the federation channel.

    The cross-nest Welcome leg (once ``POST /api/v1/welcome/{id}?nest_url=``) now
    rides ``fauna.federation.welcome.deliver``: nest A (initiator) dials nest B
    (listener) and delivers a Welcome to Bob, carrying ``origin_nest_url`` so the
    recipient client can address its reply hop. Bob (registered on nest B) reads
    the Welcome from his inbox over the surviving HTTP read route.
    """
    nest_a, nest_b = two_nests

    bob = create_actor_and_register(
        nest_b["port"], admin_signing_key=nest_b["admin"]["signing_key"]
    )
    bob_id = bob["actor_id_hex"]

    fake_welcome = os.urandom(256)
    fake_channel_id = os.urandom(32).hex()

    with FederationChannelClient(initiator=nest_a, target=nest_b) as fed:
        reply = fed.call("fauna.federation.welcome.deliver", {
            "recipient_actor_id": bob_id,
            "channel_id": fake_channel_id,
            "welcome_bytes": fake_welcome,
            "channel_type": None,
            "group_id": None,
            "origin_nest_url": nest_a["url"],
        })
    assert reply["inbox_id"] >= 1, f"Expected an inbox row id, got {reply}"

    # Bob reads his inbox on nest B (surviving HTTP read route).
    inbox = conv_api.inbox(nest_b["url"], bob)
    assert len(inbox) >= 1, "Bob should have at least 1 inbox item (the Welcome)"


def test_cross_nest_channel_post_and_read(two_nests):
    """Alice posts to a channel on nest A; Bob reads it from nest A.

    Channel send/fetch run over ``fauna.conversations.channel.{send,fetch}``
    (the ``POST|GET /api/v1/channel/{id}`` HTTP twins were deleted). Both
    actors are registered on nest A.
    """
    nest_a, nest_b = two_nests

    alice = create_actor_and_register(
        nest_a["port"], admin_signing_key=nest_a["admin"]["signing_key"]
    )
    bob = create_actor_and_register(
        nest_a["port"], admin_signing_key=nest_a["admin"]["signing_key"]
    )

    channel_id = os.urandom(32).hex()
    # A REAL `ChannelEnvelope::Application` — `SealedStorage` (the only mode
    # since 2026-07-12) strict-decodes the body
    # (`classify_envelope_shape`), so a random blob now 400s `invalid_params`.
    # The channel storage layer is content-opaque about which group an
    # envelope came from (roster auto-registers the sender per-channel), so a
    # throwaway one-off group is sufficient to mint a structurally-valid one.
    throwaway_recipient_kp = conv_api.mint_key_packages(os.urandom(32), 1)[0]
    _, _, envelope = conv_api.mint_group_welcome_with_message(
        bytes(alice["signing_key"]), throwaway_recipient_kp, "hello bob"
    )

    # Alice posts to the channel on nest A.
    seq = conv_api.channel_send(nest_a["port"], alice, channel_id, envelope)
    print(f"Alice posted to channel on nest A: seq={seq}")
    assert seq >= 1

    # Bob reads the channel from nest A.
    messages = conv_api.channel_fetch(nest_a["port"], bob, channel_id, after=0)
    print(f"Bob read channel from nest A: {len(messages)} message(s)")
    assert len(messages) >= 1
    found = [m for m in messages if m["seq"] == seq]
    assert len(found) == 1
    assert found[0]["envelope"] == envelope


@pytest.mark.feature("group-conversations")
def test_cross_nest_group_channel(two_nodes):
    """Cross-nest group channel: Alice posts to channel on nest A, Bob reads from nest A.

    Channel send/fetch run over ``fauna.conversations.channel.{send,fetch}``
    (the ``POST|GET /api/v1/channel/{id}`` HTTP twins were deleted). Bob is
    registered on nest A so he can authenticate there (cross-nest read).
    """
    port_a = two_nodes["port_a"]
    port_b = two_nodes["port_b"]

    alice = create_actor_and_register(
        port_a, admin_signing_key=two_nodes["admin_sk_a"]
    )
    bob = create_actor_and_register(
        port_b, admin_signing_key=two_nodes["admin_sk_b"]
    )

    channel_id = "ab" * 32  # 64-char hex channel ID

    # A REAL `ChannelEnvelope::Application` — `SealedStorage` (the only mode
    # since 2026-07-12) strict-decodes the body
    # (`classify_envelope_shape`), so a plain byte string now 400s
    # `invalid_params`. See ``test_cross_nest_channel_post_and_read`` for why a
    # throwaway one-off group suffices.
    throwaway_recipient_kp = conv_api.mint_key_packages(os.urandom(32), 1)[0]
    _, _, msg_body = conv_api.mint_group_welcome_with_message(
        bytes(alice["signing_key"]), throwaway_recipient_kp, "hello group encrypted message"
    )

    # Alice posts a message to the channel on nest A.
    seq = conv_api.channel_send(port_a, alice, channel_id, msg_body)
    assert seq == 1
    print(f"Alice posted to channel: seq={seq}")

    # Register Bob on nest A so cross-nest auth is permitted.
    register_user(port_a, bob["actor_id_hex"], admin_signing_key=two_nodes["admin_sk_a"])
    print(f"Bob registered on nest A for cross-nest access")

    # Bob reads the channel from nest A (cross-nest).
    messages = conv_api.channel_fetch(port_a, bob, channel_id, after=0)
    assert len(messages) == 1
    assert messages[0]["seq"] == 1
    assert messages[0]["envelope"] == msg_body
    print(f"Bob read group channel from nest A: {len(messages)} message(s)")
