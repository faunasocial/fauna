"""E2E tests for MLS channel infrastructure: channel messages, key packages, and welcome delivery.

All same-nest MLS-plane operations now ride ``fauna.conversations.*`` WS-RPC
kinds — the HTTP twins (``POST|GET /api/v1/channel/{id}``, the keypackage
publish/count/fetch routes, ``POST /api/v1/welcome/{id}``) were deleted in the
WS-RPC migration + the WS-RPC-everywhere rip-out. Driven here via
``tests.api.conv_api``:

* Channel send/fetch — ``fauna.conversations.channel.{send,fetch}``.
* Key-package publish/count/**fetch** — ``fauna.conversations.keypackage.{upload,count,fetch}``
  (the deleted ``GET /api/v1/keypackage/{id}`` consume route → ``keypackage.fetch``).
* Welcome delivery — ``fauna.conversations.welcome.deliver`` (the deleted
  ``POST /api/v1/welcome/{id}`` same-nest path).

The only surviving HTTP route here is the inbox **read**
(``GET /api/v1/inbox/{actor_id}``), used to observe Welcome delivery
(``conv_api.inbox``). The cross-nest KP-fetch / Welcome-deliver legs ride the
federation channel and are covered in ``test_cross_nest_api.py``.
"""
import os

import pytest

from common import create_actor_and_register, port_base_url

from tests.api import conv_api

pytestmark = pytest.mark.tier_3


CHANNEL_ID = "aa" * 32  # 64 hex chars = 32 bytes


# ---------------------------------------------------------------------------
# Test 1: Channel message roundtrip
# ---------------------------------------------------------------------------

def test_channel_message_roundtrip(two_nodes):
    """Alice posts a message to a channel, Bob reads it back.

    Drives ``fauna.conversations.channel.{send,fetch}`` — the HTTP twins
    (``POST|GET /api/v1/channel/{id}``) were deleted.
    """
    port_a = two_nodes["port_a"]

    alice = create_actor_and_register(port_a, admin_signing_key=two_nodes["admin_sk_a"])
    bob = create_actor_and_register(port_a, admin_signing_key=two_nodes["admin_sk_a"])

    # Alice posts a REAL `ChannelEnvelope::Application` to the channel and gets
    # back the assigned seq. `SealedStorage` (the only mode since
    # 2026-07-12) strict-decodes the body (`classify_envelope_shape`), so a fake
    # byte blob now 400s `invalid_params`/`ingest_failed`; the channel storage
    # layer itself is content-opaque about *which* group the envelope came from
    # (roster membership auto-registers the sender per-channel — see
    # `may_auto_register`/`conversations_handlers.rs`), so a throwaway one-off
    # group is sufficient to mint a structurally-valid envelope.
    throwaway_recipient_kp = conv_api.mint_key_packages(os.urandom(32), 1)[0]
    _, _, envelope = conv_api.mint_group_welcome_with_message(
        bytes(alice["signing_key"]), throwaway_recipient_kp, "hello bob"
    )
    seq = conv_api.channel_send(port_a, alice, CHANNEL_ID, envelope)
    assert seq >= 1, f"Expected seq >= 1, got {seq}"

    # Bob reads the channel from seq 0 (after=0 → messages with seq > 0).
    messages = conv_api.channel_fetch(port_a, bob, CHANNEL_ID, after=0)
    assert len(messages) >= 1, f"Expected at least 1 message, got {len(messages)}"

    # Find the message we posted. The WS reply carries the envelope as raw
    # bytes (CBOR bstr), not the hex string the old HTTP twin returned.
    found = [m for m in messages if m["seq"] == seq]
    assert len(found) == 1, f"Expected to find seq={seq} in messages"
    assert found[0]["envelope"] == envelope


# ---------------------------------------------------------------------------
# Test 2: Key package lifecycle (publish, count, fetch/consume, count)
# ---------------------------------------------------------------------------

@pytest.mark.feature("conversations", "encryption-settings")
def test_key_package_lifecycle(two_nodes):
    """Publish 3 key packages, count, fetch (consume) one, count again.

    Publish + count + fetch all run over
    ``fauna.conversations.keypackage.{upload,count,fetch}`` (the HTTP twins were
    deleted). ``keypackage.fetch`` is destructive — any User-class actor may
    consume one of the target's key packages (same-nest).
    """
    port_a = two_nodes["port_a"]

    admin_sk = two_nodes["admin_sk_a"]
    alice = create_actor_and_register(port_a, admin_signing_key=admin_sk)
    actor_id = alice["actor_id_hex"]

    # Publish 3 REAL key packages — the retired `PlaintextStorage` mode used to
    # accept arbitrary bytes; `SealedStorage` (the only mode since
    # 2026-07-12) structurally parses uploads via
    # `fauna_mls::engine::verify_uploaded_key_package`, so a fake blob now 400s
    # `invalid_params`. Mint real ones (same helper the cross-nest MLS tests use).
    packages = conv_api.mint_key_packages(bytes(alice["signing_key"]), 3)
    stored = conv_api.keypackage_upload(port_a, alice, packages)
    assert stored == 3, f"Expected stored=3, got {stored}"

    # Count — expect 3.
    count = conv_api.keypackage_count(port_a, alice, actor_id)
    assert count == 3, f"Expected count=3, got {count}"

    # Fetch (consume) one key package — any other authenticated user can do this
    # over the same-nest keypackage.fetch kind. The consumed package rides back
    # as raw bytes (CBOR bstr) and should be one of the three we uploaded.
    bob = create_actor_and_register(port_a, admin_signing_key=admin_sk)
    consumed = conv_api.keypackage_fetch(port_a, bob, actor_id)
    assert consumed in set(packages), f"Unexpected key package data: {consumed!r}"

    # Count again — expect 2.
    count = conv_api.keypackage_count(port_a, alice, actor_id)
    assert count == 2, f"Expected count=2, got {count}"


# ---------------------------------------------------------------------------
# Test 3: Welcome delivery with channel_id
# ---------------------------------------------------------------------------

def test_welcome_delivery_with_channel(two_nodes):
    """Deliver a Welcome with a channel_id, verify it appears in Bob's inbox.

    Welcome delivery rides ``fauna.conversations.welcome.deliver`` (same-nest
    plane; the ``POST /api/v1/welcome/{id}`` twin was deleted). The inbox read
    stays on the surviving HTTP route ``GET /api/v1/inbox/{id}``.
    """
    port_a = two_nodes["port_a"]
    base = port_base_url(port_a)

    admin_sk = two_nodes["admin_sk_a"]
    alice = create_actor_and_register(port_a, admin_signing_key=admin_sk)
    bob = create_actor_and_register(port_a, admin_signing_key=admin_sk)
    bob_id = bob["actor_id_hex"]

    channel_hex = "bb" * 32

    # Alice delivers a Welcome to Bob with a channel_id (same-nest path). The
    # inbox row payload is the canonical inbox envelope (layer 1) — a Welcome
    # envelope whose WelcomeInbox embeds the raw welcome bytes as a contiguous
    # CBOR byte string, so they appear verbatim inside the payload hex.
    welcome_data = bytes.fromhex("cafe" * 16)  # 32 bytes of fake welcome
    # Mode gate (direct-messages.md § Reach policy): alice is a stranger to
    # bob, so open bob's inbox — this test's subject is delivery mechanics.
    conv_api.set_inbox_mode(port_a, bob, "open")
    inbox_id = conv_api.welcome_deliver(port_a, alice, bob_id, channel_hex, welcome_data)
    assert inbox_id >= 1, f"Expected an assigned inbox id, got {inbox_id}"

    # Bob fetches his inbox. The response is a list of {id, payload} (payload hex).
    items = conv_api.inbox(base, bob)
    assert isinstance(items, list), f"Expected list, got {type(items)}"
    assert len(items) >= 1, f"Expected at least 1 inbox item, got {len(items)}"

    # The welcome bytes are embedded in one inbox row's envelope payload.
    payloads = [item["payload"] for item in items]
    assert any(welcome_data.hex() in p for p in payloads), (
        f"Welcome data not found in inbox envelope payloads: {payloads}"
    )


# ---------------------------------------------------------------------------
# Test 4: Full DM roundtrip — key package → welcome → channel message → inbox
# ---------------------------------------------------------------------------

@pytest.mark.feature("conversations")
def test_dm_roundtrip(two_nodes):
    """Full DM flow: Alice publishes key packages, Bob fetches one, sends
    a Welcome + channel message, Alice reads both from inbox + channel.

    This exercises the exact API sequence the web app uses for same-nest DMs,
    post WS-RPC migration: publish/count + channel send/fetch + keypackage fetch
    + Welcome deliver over ``fauna.conversations.*``; inbox read over the
    surviving HTTP route.
    """
    port_a = two_nodes["port_a"]
    base = port_base_url(port_a)
    admin_sk = two_nodes["admin_sk_a"]

    alice = create_actor_and_register(port_a, admin_signing_key=admin_sk)
    bob = create_actor_and_register(port_a, admin_signing_key=admin_sk)
    alice_id = alice["actor_id_hex"]

    channel_hex = "cc" * 32

    # Step 1: Alice publishes REAL key packages so Bob can create a DM (a fake
    # blob 400s `invalid_params` under `SealedStorage` — see test 2's comment).
    alice_packages = conv_api.mint_key_packages(bytes(alice["signing_key"]), 3)
    stored = conv_api.keypackage_upload(port_a, alice, alice_packages)
    assert stored == 3, f"Expected stored=3, got {stored}"

    # Step 2: Bob fetches one of Alice's key packages (same-nest keypackage.fetch).
    consumed = conv_api.keypackage_fetch(port_a, bob, alice_id)
    assert consumed in set(alice_packages), f"Unexpected key package: {consumed!r}"

    # Step 3: Bob delivers a Welcome to Alice (simulating MLS group creation).
    # Mode gate (direct-messages.md § Reach policy): bob is a stranger to
    # alice, so open alice's inbox — this test's subject is the roundtrip.
    welcome_bytes = bytes.fromhex("d00d" * 16)
    conv_api.set_inbox_mode(port_a, alice, "open")
    conv_api.welcome_deliver(port_a, bob, alice_id, channel_hex, welcome_bytes)

    # Step 4: Alice checks her inbox — should have the Welcome.
    items = conv_api.inbox(base, alice)
    assert isinstance(items, list)
    assert len(items) >= 1, f"Expected Welcome in inbox, got {items}"
    # Payload is the canonical inbox envelope (layer 1); the welcome bytes are
    # embedded as a contiguous CBOR byte string inside it.
    payloads = [item["payload"] for item in items]
    assert any(welcome_bytes.hex() in p for p in payloads)

    # Step 5: Bob posts a REAL encrypted message to the channel (WS-RPC) — a
    # fake blob 400s `invalid_params` under `SealedStorage` (see test 1's
    # comment); a throwaway one-off group mints a structurally-valid envelope.
    throwaway_recipient_kp = conv_api.mint_key_packages(os.urandom(32), 1)[0]
    _, _, ciphertext = conv_api.mint_group_welcome_with_message(
        bytes(bob["signing_key"]), throwaway_recipient_kp, "hi alice"
    )
    msg_seq = conv_api.channel_send(port_a, bob, channel_hex, ciphertext)

    # Step 6: Alice reads the channel — should have the message (WS-RPC).
    messages = conv_api.channel_fetch(port_a, alice, channel_hex, after=0)
    assert len(messages) >= 1
    found = [m for m in messages if m["seq"] == msg_seq]
    assert len(found) == 1
    # Envelope rides back as raw bytes (CBOR bstr), not hex.
    assert found[0]["envelope"] == ciphertext

    # Step 7: Verify Alice's key package count decreased (3 published, 1
    # consumed by Bob in step 2).
    count = conv_api.keypackage_count(port_a, alice, alice_id)
    assert count == 2, "Expected 2 remaining (3 published, 1 consumed)"


# ---------------------------------------------------------------------------
# Test 5: a non-member's Commit upload auto-registers them onto the roster
# ---------------------------------------------------------------------------

def test_commit_upload_auto_registers_a_non_member_sender(two_nodes):
    """A non-member posting a ``ChannelEnvelope::Commit`` joins the roster.

    This is the load-bearing transport ruling under the per-group identity-
    succession sweep (`docs/goal/behavior/identity-succession.md` § Propagation
    → *MLS groups*; driver `fauna_client_recovery::group_sweep`). After a
    succession the old identity is refused at every authenticated surface, so it
    cannot upload its own add-successor commit — the **successor** uploads both
    halves instead, and the only reason that works is that ``channel.send`` is
    one of the three raw roster auto-register paths
    (``may_auto_register``/``conversations_handlers.rs``). MLS carries the real
    membership; the nest only ever sees a member-shaped upload.

    Pinned here because the driver's own unit tests
    (``libs/fauna-client-recovery/tests/group_sweep.rs``) necessarily fake the
    nest — if this ruling ever changed, the sweep would break with no Rust test
    noticing, and the remedy would be to add a nest kind, which the design
    explicitly rejects.
    """
    import cbor2

    port_a = two_nodes["port_a"]
    admin_sk = two_nodes["admin_sk_a"]
    channel_hex = "cd" * 32

    alice = create_actor_and_register(port_a, admin_signing_key=admin_sk)
    successor = create_actor_and_register(port_a, admin_signing_key=admin_sk)

    # Alice's first send registers her, giving us a member who may read the
    # roster (`channel.actors` is member-scoped and never auto-registers).
    throwaway_kp = conv_api.mint_key_packages(os.urandom(32), 1)[0]
    _, _, envelope = conv_api.mint_group_welcome_with_message(
        bytes(alice["signing_key"]), throwaway_kp, "hello"
    )
    conv_api.channel_send(port_a, alice, channel_hex, envelope)

    def roster() -> set[str]:
        client = conv_api._client_for(conv_api._base_url(port_a), alice)
        reply = client.call(
            "fauna.conversations.channel.actors", {"channel_id": channel_hex}
        )
        return {a.lower() for a in reply["actors"]}

    before = roster()
    assert alice["actor_id_hex"].lower() in before, f"alice should be on the roster: {before}"
    assert successor["actor_id_hex"].lower() not in before, (
        f"the successor must start as a non-member, else this proves nothing: {before}"
    )

    # The successor uploads a Commit. `SealedStorage` checks the envelope's
    # *shape* only (decodable, inner >= 28 bytes, no plaintext magic prefix) —
    # it is deliberately content-opaque about which group a commit came from,
    # which is what lets MLS rather than the nest own membership.
    commit = cbor2.dumps({"Commit": os.urandom(64)}, canonical=True)
    seq = conv_api.channel_send(port_a, successor, channel_hex, commit)
    assert seq >= 1

    after = roster()
    assert successor["actor_id_hex"].lower() in after, (
        "a non-member's Commit upload must auto-register them — the whole "
        f"succession sweep rests on it: {after}"
    )
