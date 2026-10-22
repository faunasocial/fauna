"""tier_3 app journey: a **bridged room on the unified Conversations page**
(``docs/goal/ui/conversations.md`` § Where logic lives → *The ``Bridged``
adapter*; the kinds ``docs/goal/architecture/apps/bridges.md`` § Bridge-kind
catalogue → Phase G; the tier ``docs/goal/architecture/testing.md`` § The
four-tier taxonomy).

``tests/api/test_bridged_conversation_kinds.py`` proves the family's wire with
opaque bytes — the nest opens nothing, so nothing there is really sealed. This
file is the other half: a real app on the user's side and a fake **bridge
principal** on the far side that seals and opens for real
(``fauna_ffi.bridged_seal_for_user`` / ``bridged_open_as_bridge``), so the two
ends only meet if the app's glue seals to the bridge's key, opens under the
account's recipient key, and paints what the bridge declared.

Production data flow, one sentence: a consented third-party principal whose
signed manifest declares a ``bridge`` block reads the account's recipient key
(``fauna.bridges.fetch_recipient_mls_pubkey``) → HPKE-seals a message to it and
deposits ciphertext (``conversation.deposit``) → the nest stores it and nudges
(``fauna.bridges.push.conversation_changed``) → the app's receive loop reads
``conversation.rooms.list`` + ``conversation.inbox.fetch``, opens the row under
the account's mail-derived recipient key and ingests it on the bridged rail →
the room renders on the Conversations page with the bridge's declared label,
the transport-only class and the opened text → the user replies in the compose
bar → the app seals the reply to the bridge principal's X25519 key and to the
account's own key (``conversation.send``) → the principal drains it
(``conversation.outbox.fetch``), opens it under its holder key, and acks.

The account's recipient key is mail's (one MSEK-derived key for every sealed
inbound rail), so the fixture has the user enable mail in the app first — the
precondition under which a bridge can seal to them at all.

Two more journeys ride the same page:

- **The supervised ward** (``docs/goal/behavior/family-safety.md`` § The
  bridge-DM gate → *App affordance*): a ward whose guardian set
  ``unknown_peer_dm: hold`` gets the same deposit from a peer no verdict row
  knows → the nest stores it sealed as always and computes ``held`` at read →
  the row and the open thread carry ``conversation-guardian-state`` with
  ``state`` = ``held`` → the message text is on screen all the same (reading is
  never gated).
- **Nostr** (``docs/goal/ui/nostr.md`` § Implementation status today → DMs): a
  far Nostr user gift-wraps a DM (NIP-17, ``fauna_ffi.nostr_gift_wrap_dm`` — the
  shared ``nip17::wrap_dm``) to the account's custodial key and posts it to the
  nest's own ``/nostr`` relay → the nest unwraps it, seals it to the account's
  recipient key and deposits it through the in-process Nostr leg of the same
  family → the app reads it like any bridged room and paints it with the leg's
  declared identity (the bolt glyph, "Nostr").
"""

import os

import pytest
from cryptography.hazmat.primitives import serialization
from cryptography.hazmat.primitives.asymmetric import ed25519, x25519

import fauna_ffi
from helpers import nostr_relay
from helpers.app_surface import skip_unbuilt
from clients.ws_rpc_admin_client import WsRpcAdminClient
from helpers.atproto_consent import (  # noqa: F401 — consent_nest/consent_bridge/consent_spa_url are pytest fixtures adopted by import
    HANDLE_DOMAIN,
    USER_ROTATION_PUB,
    consent_bridge,
    consent_nest,
    consent_spa_url,
)
from helpers.budgets import RECEIVE_CYCLE_S, RPC_ROUNDTRIP_S
from helpers.family_wire import DEFAULT_WARD_POLICY, admit_adult_directly, admit_ward
from helpers.client_metadata_server import (
    DOCUMENT_PATH,
    ClientMetadataServer,
    client_document,
    manifest_payload,
    sign_manifest,
)
from helpers.oauth_client import OAuthClient
from helpers.waiting import wait_until
from tests.api.test_bridged_conversation_kinds import _approve, _ok
from helpers.principal_session import alice_client, PrincipalSession, upgrade
from i18n.strings import S

# `real_conversations`: windows (like macOS/iOS/android) registers the real receive
# loop only behind the launch-time FAUNA_E2E_REAL_CONVERSATIONS gate this marker
# sets; without it the app runs the mock manager and a bridged deposit never lands.
pytestmark = [pytest.mark.tier_3, pytest.mark.real_conversations]

PUBLISHER = "example.com"
SCOPE = "fauna:conversations:bridge"
REDIRECT_URI = "http://127.0.0.1:17781/callback"
FAR_ROOM = "!journey:example.org"
PEER = "@bob:example.org"
SELF = "@alice:example.org"
INBOUND_BODY = "hello from the far network"
REPLY_BODY = "and hello back across the bridge"
COLD_PEER = "@stranger:example.org"
HELD_BODY = "you do not know me yet"
NOSTR_BODY = "a gift-wrapped hello over nostr"

#: The manifest's block. `globe` is deliberately not the generic `bridge`
#: glyph, so a row painting the declared glyph is distinguishable from one
#: painting the rail's fallback.
BRIDGE = {
    "id": "matrix",
    "glyph": "globe",
    "address_grammar": "^@[^:]+:.+$",
    "capabilities": {
        "supports_attachments": False,
        "supports_markdown": False,
        "supports_reactions": False,
        "supports_message_delete": False,
        "supports_per_message_reply": False,
        "supports_membership_change": False,
        "supports_recipient_selection": False,
        "supports_rename": False,
        "supports_subject": False,
        "delivery_mode": "Async",
    },
}


@pytest.fixture(scope="module")
def metadata_server(tmp_path_factory):
    server = ClientMetadataServer(PUBLISHER, tmp_path_factory.mktemp("client-metadata"))
    publisher_key = ed25519.Ed25519PrivateKey.generate()
    payload = manifest_payload(PUBLISHER, publisher_key, [])
    payload["bridge"] = BRIDGE
    server.documents[DOCUMENT_PATH] = client_document(
        server.client_id(), REDIRECT_URI, SCOPE, manifest_jws=sign_manifest(publisher_key, payload)
    )
    yield server
    server.close()


@pytest.fixture(scope="module")
def consent_nest_env(metadata_server):
    """The shared consent nest's env: its metadata fetcher pointed at
    ``metadata_server``."""
    return metadata_server.nest_env()


def _bridged_built(driver) -> bool:
    """Whether this app registers the bridged rail and paints it — the apps
    the lift (``conversations.md`` § Implementation status today) has reached."""
    return driver.is_tui() or driver.is_web() or driver.is_linux() or driver.is_windows() or driver.is_macos() or driver.is_ios()


@pytest.fixture
def bridged_app(request, app, consent_nest, consent_bridge):
    """`app`, logged in as alice on the consent nest, with mail enabled through
    the app's own UI — which is what gives the account the recipient key a
    bridge seals to and the app opens under."""
    if not _bridged_built(app.driver):
        skip_unbuilt(
            app.driver,
            surface="bridged rooms on the unified Conversations page",
            detail="no BridgedBackend is registered on this app yet",
            tracked="docs/goal/ui/conversations.md § Implementation status today",
        )
    from conftest import _login_app_as

    _login_app_as(
        app, request, consent_nest, consent_nest["user"], spa_url_fixture="consent_spa_url"
    )
    app.mail_settings.navigate()
    app.mail_settings.ensure_mail_enabled()
    return app


def _raw(public_key) -> bytes:
    return public_key.public_bytes(serialization.Encoding.Raw, serialization.PublicFormat.Raw)


def _page_error(app) -> str:
    """The page's `error-message`, for a failure message (convention 6)."""
    try:
        return app.driver.get_text("error-message") or ""
    except Exception as e:  # the element is absent while nothing is wrong
        return f"<no error-message: {e}>"


def _rooms(nest) -> list[dict]:
    with alice_client(nest) as ws:
        return ws.call("fauna.bridges.conversation.rooms.list", {})["rooms"]


def _consented_bridge(nest, metadata_server):
    """The bridge principal, consented by ``nest["user"]`` and holding that
    account's recipient key: ``(session, holder_secret, key)``. ``nest`` names
    whose account it rides — the consent nest as is, or :func:`_as_user`'s view
    of it for another account."""
    holder = x25519.X25519PrivateKey.generate()
    holder_secret = holder.private_bytes(
        serialization.Encoding.Raw,
        serialization.PrivateFormat.Raw,
        serialization.NoEncryption(),
    )
    client = OAuthClient(
        base=nest["url"],
        htu_origin=f"https://{HANDLE_DOMAIN}",
        redirect_uri=REDIRECT_URI,
        scope=SCOPE,
        holder_x25519=_raw(holder.public_key()),
        client_id_url=metadata_server.client_id(),
    )
    tokens = _approve(nest, client)
    session = PrincipalSession(upgrade(nest, client, tokens["access_token"]))
    key = _ok(
        session,
        "fauna.bridges.fetch_recipient_mls_pubkey",
        {"actor_id": nest["user"]["actor_id_bytes"]},
    )
    assert key.get("key"), (
        "enabling mail in the app must have provisioned the account's recipient "
        f"key — a bridge has nothing to seal to otherwise: {key!r}"
    )
    return session, holder_secret, key


def _deposit(session, key, *, far_room_id, far_message_id, sender, body):
    """The bridge's inbound leg: the room, then one message sealed to the
    account's recipient key."""
    mlkem_ek = bytes(key["key"]["mlkem_ek"])
    _ok(
        session,
        "fauna.bridges.conversation.room.upsert",
        {"far_room_id": far_room_id, "participants": [sender], "self_address": SELF},
    )
    _ok(
        session,
        "fauna.bridges.conversation.deposit",
        {
            "far_room_id": far_room_id,
            "far_message_id": far_message_id,
            "sender": sender,
            "sealed_content": fauna_ffi.bridged_seal_for_user(
                bytes(key["key"]["mls_pubkey"]), mlkem_ek, body.encode()
            ),
            "created_at": 1,
        },
    )


def _as_user(nest, user) -> dict:
    """``nest`` as the helpers that act for ``nest["user"]`` should see it when
    the account under test is ``user`` instead."""
    return {**nest, "user": user}


def _threads(conv):
    return [(t.label, t.rail) for t in conv.list_threads()]


def _await_text(app, body):
    wait_until(
        lambda: any(body in (t or "") for t in app.driver.get_texts("dm-message-text")),
        RPC_ROUNDTRIP_S,
        diagnose=lambda: f"bubbles: {app.driver.get_texts('dm-message-text')!r}",
    )


@pytest.mark.feature("conversations")
@pytest.mark.tui
@pytest.mark.web
@pytest.mark.linux
@pytest.mark.windows
@pytest.mark.macos
@pytest.mark.ios
def test_a_bridged_room_renders_and_a_reply_reaches_only_the_bridge(
    bridged_app, consent_nest, metadata_server
):
    app = bridged_app
    nest = consent_nest
    conv = app.conversations

    # ── Precondition: the bridge principal is consented, and reads the key. ──
    session, holder_secret, key = _consented_bridge(nest, metadata_server)

    # ── The bridge deposits a message sealed to the user. ──
    _deposit(
        session,
        key,
        far_room_id=FAR_ROOM,
        far_message_id="$journey-1",
        sender=PEER,
        body=INBOUND_BODY,
    )
    room = next(r for r in _rooms(nest) if r["far_room_id"] == FAR_ROOM)
    label = room["bridge_label"]
    assert label and room["glyph"] == "globe", room

    # ── It appears on the conversations page as that bridge's room. ──
    conv.navigate()

    def _row():
        for i, t in enumerate(conv.list_threads()):
            if PEER in (t.label or ""):
                return i, t
        return None

    index, thread = wait_until(
        _row,
        RECEIVE_CYCLE_S,
        diagnose=lambda: (
            f"no thread for {PEER}; threads={_threads(conv)!r}, error: {_page_error(app)!r}"
        ),
    )
    assert thread.rail == "Bridged", thread.rail
    icon = app.driver.get_text("protocol-icon", index=index)
    assert icon.endswith(label), (
        f"the row must carry the bridge's DECLARED label {label!r} beside its glyph: {icon!r}"
    )
    assert app.driver.get_attr("protocol-icon", "bridge", index=index) == "matrix"
    assert app.driver.count("conversation-guardian-state") == 0, (
        "an unsupervised account's room carries no guardian marker"
    )

    # ── The detail: transport-only by class, the sealed text opened. ──
    conv.open_thread_by_id(thread.thread_id)
    assert conv.room_class() == "transport-only", conv.room_class()
    _await_text(app, INBOUND_BODY)

    # ── The user replies; only the bridge's key opens what was queued. ──
    conv.send_in_open_thread(REPLY_BODY)

    def _queued():
        return _ok(session, "fauna.bridges.conversation.outbox.fetch", {})["items"]

    items = wait_until(
        _queued,
        RECEIVE_CYCLE_S,
        diagnose=lambda: f"the outbox stayed empty; error: {_page_error(app)!r}",
    )
    assert len(items) == 1 and items[0]["far_room_id"] == FAR_ROOM, items
    ciphertext = bytes(items[0]["ciphertext"])
    assert REPLY_BODY.encode() not in ciphertext, "the queued item must be ciphertext"
    assert fauna_ffi.bridged_open_as_bridge(holder_secret, ciphertext) == REPLY_BODY.encode()
    stranger = x25519.X25519PrivateKey.generate().private_bytes(
        serialization.Encoding.Raw,
        serialization.PrivateFormat.Raw,
        serialization.NoEncryption(),
    )
    with pytest.raises(RuntimeError):
        fauna_ffi.bridged_open_as_bridge(stranger, ciphertext)

    # ── Ack drains it; the user's own copy stays in the thread. ──
    assert (
        _ok(session, "fauna.bridges.conversation.outbox.ack", {"ids": [items[0]["id"]]})["acked"]
        == 1
    )
    assert _queued() == []
    session.ws.close()
    texts = app.driver.get_texts("dm-message-text")
    assert any(REPLY_BODY in (t or "") for t in texts), texts
    assert len([t for t in conv.list_threads() if PEER in (t.label or "")]) == 1, (
        "the deposit and the user's own reply are ONE thread"
    )


# ── The bridge stops serving ─────────────────────────────────────────────


@pytest.mark.feature("conversations")
@pytest.mark.tui
@pytest.mark.web
@pytest.mark.linux
def test_a_revoked_bridges_room_says_so_and_its_undelivered_reply_too(
    bridged_app, consent_nest, metadata_server
):
    """``docs/goal/architecture/apps/bridges.md`` § Bridge-kind catalogue →
    Phase G → *When the bridge stops serving* (consequences 2 and 3), painted
    as ``docs/goal/ui/conversations.md`` § Element IDs rules it.

    Production data flow, one sentence: the user replies in a bridged room →
    the app queues it sealed to the bridge (``conversation.send``) and the
    bridge has not drained it → the user revokes the bridge on Connected apps
    (``fauna.principals.revoke``) → the nest deletes the undrained item and
    stamps the Sent row ``undelivered`` in the same transaction, the room now
    ``disconnected`` on ``rooms.list``, and nudges ``conversation_changed`` →
    the app's receive loop re-reads the rooms, and its glue re-reads the
    disconnected room (the row was read before the stamp, so the cursor-paged
    inbox would never serve it again) → the header's ``thread-room-notice``
    states ``bridge-disconnected`` and ``dm-message-undelivered`` sits under
    the user's own bubble, whose text stays on screen → the room's gate
    withholds the send (``ThreadCapabilities.can_send``), so ``dm-text-field``
    and ``dm-send-button`` stay in view, disabled: readable, never writable."""
    app = bridged_app
    nest = consent_nest
    conv = app.conversations
    tail = os.urandom(3).hex()
    far_room = f"!revoked-{tail}:example.org"
    peer = f"@carol-{tail}:example.org"
    reply = f"this one never left {tail}"

    session, _holder_secret, key = _consented_bridge(nest, metadata_server)
    _deposit(
        session,
        key,
        far_room_id=far_room,
        far_message_id=f"$revoked-{tail}",
        sender=peer,
        body=INBOUND_BODY,
    )

    conv.navigate()

    def _thread():
        return next((t for t in conv.list_threads() if peer in (t.label or "")), None)

    thread = wait_until(
        _thread,
        RECEIVE_CYCLE_S,
        diagnose=lambda: f"no thread for {peer}; threads={_threads(conv)!r}",
    )
    conv.open_thread_by_id(thread.thread_id)
    _await_text(app, INBOUND_BODY)
    assert app.driver.count("thread-room-notice") == 0, "a served room carries no notice"

    # ── The reply is queued, and the bridge never drains it. ──
    conv.send_in_open_thread(reply)
    wait_until(
        lambda: _ok(session, "fauna.bridges.conversation.outbox.fetch", {})["items"],
        RECEIVE_CYCLE_S,
        diagnose=lambda: f"the reply never queued; error: {_page_error(app)!r}",
    )
    _await_text(app, reply)
    session.ws.close()

    # ── The user revokes the bridge from the app. ──
    ca = app.connected_apps
    assert ca.visit(), f"Connected apps did not load; error: {ca.current_error_text()!r}"
    row = ca.wait_for_item_containing(metadata_server.client_id())
    assert row is not None, "the consented bridge must be on the roster"
    ca.revoke_item(row)
    assert ca.wait_for_no_item_containing(metadata_server.client_id()), (
        f"the revoked bridge must leave the roster; error: {ca.current_error_text()!r}"
    )

    # The nest's own answer first (convention 5): a missing mark below is then
    # the app's, not the nest's.
    room = next(r for r in _rooms(nest) if r["far_room_id"] == far_room)
    assert room.get("disconnected") is True, room
    with alice_client(nest) as ws:
        rows = ws.call(
            "fauna.bridges.conversation.inbox.fetch", {"room_id": bytes(room["room_id"])}
        )["messages"]
    assert [m.get("undelivered", False) for m in rows if m["outbound"]] == [True], rows

    # ── The room says so, and so does the reply that never left. ──
    conv.navigate()
    conv.open_thread_by_id(thread.thread_id)

    def _painted():
        try:
            state = app.driver.get_attr("thread-room-notice", "state")
        except Exception:
            return None
        if state != "bridge-disconnected" or app.driver.count("dm-message-undelivered") == 0:
            return None
        return state

    wait_until(
        _painted,
        RECEIVE_CYCLE_S,
        diagnose=lambda: (
            f"notice={app.driver.get_texts('thread-room-notice')!r}, "
            f"undelivered={app.driver.count('dm-message-undelivered')}, "
            f"error: {_page_error(app)!r}"
        ),
    )
    assert app.driver.get_text("thread-room-notice") == (
        S.conversations.unified.room_notice_bridge_disconnected
    )
    assert app.driver.get_texts("dm-message-undelivered") == [
        S.conversations.unified.dm_message_undelivered
    ], "only the user's own, undrained reply is marked"
    texts = app.driver.get_texts("dm-message-text")
    assert any(reply in (t or "") for t in texts), (
        f"the undelivered reply's text stays on screen, to send again: {texts!r}"
    )
    assert any(INBOUND_BODY in (t or "") for t in texts), texts

    # ── Readable, never writable: the reply bar is withheld. ──
    if not app.driver.is_tui():
        skip_unbuilt(
            app.driver,
            surface="a read-only dm-compose-bar on a disconnected bridged room",
            detail="dm-text-field / dm-send-button do not yet gate on can_send",
            tracked="docs/goal/behavior/bridged-adapter.md § Implementation status today",
        )
    for control in ("dm-text-field", "dm-send-button"):
        assert app.driver.get_attr(control, "disabled") == "true", (
            f"{control} must be withheld on a room no bridge serves"
        )


# ── The supervised ward ──────────────────────────────────────────────────


@pytest.fixture
def held_ward(consent_nest):
    """A supervised account on the consent nest whose guardian holds DMs from
    unknown peers — arranged over the wire, as ``test_family.py``'s fixtures
    arrange theirs (fixture setup, convention 8's exemption: the admission and
    the knob are ``test_family.py``'s own subjects). One anonymous invite-request
    submit per use — two tests use it; this module spends no other. Carries the
    guardian's identity as ``ward["guardian"]`` for the test that Approves.

    The ward is taken to ``hosted_full`` exactly as ``consent_nest`` takes its
    own user, for the same reason: the consent ceremony the bridge rides
    completes as an identity."""
    nest = consent_nest
    tail = os.urandom(3).hex()
    guardian = admit_adult_directly(nest, f"guardian-{tail}")
    handle = f"ward-{tail}"
    ward = admit_ward(nest, guardian, handle)
    ward["actor_id_bytes"] = bytes.fromhex(ward["actor_id_hex"])
    ward["handle"] = handle
    with WsRpcAdminClient(
        nest["url"], bytes.fromhex(guardian["actor_id_hex"]), bytes(guardian["signing_key"])
    ) as guardian_ws:
        guardian_ws.call(
            "fauna.family.policy.update",
            {
                "supervised_actor_id": ward["actor_id_bytes"],
                "policy": {**DEFAULT_WARD_POLICY, "unknown_peer_dm": "hold"},
            },
        )
    with alice_client(_as_user(nest, ward)) as ward_ws:
        ward_ws.call(
            "fauna.bridges.atproto.set_integration_level",
            {
                "target_level": "hosted_full",
                "did_method": "plc",
                "user_rotation_pub_did_key": USER_ROTATION_PUB,
                "history_backfill": False,
            },
        )
    ward["guardian"] = guardian
    return ward


@pytest.mark.feature("family-safety")
@pytest.mark.tui
@pytest.mark.web
@pytest.mark.linux
@pytest.mark.windows
@pytest.mark.macos
@pytest.mark.ios
def test_a_cold_peers_bridged_room_is_marked_held_for_a_ward_and_stays_readable(
    request, app, consent_nest, consent_bridge, metadata_server, held_ward
):
    if not _bridged_built(app.driver):
        skip_unbuilt(
            app.driver,
            surface="bridged rooms on the unified Conversations page",
            detail="no BridgedBackend is registered on this app yet",
            tracked="docs/goal/ui/conversations.md § Implementation status today",
        )
    from conftest import _login_app_as

    nest = _as_user(consent_nest, held_ward)
    _login_app_as(
        app, request, nest, held_ward, spa_url_fixture="consent_spa_url", verify_live_actor=True
    )
    app.mail_settings.navigate()
    app.mail_settings.ensure_mail_enabled()
    conv = app.conversations

    # ── A peer no verdict row knows writes through the bridge. ──
    session, _holder_secret, key = _consented_bridge(nest, metadata_server)
    far_room = f"!cold-{os.urandom(3).hex()}:example.org"
    _deposit(
        session,
        key,
        far_room_id=far_room,
        far_message_id="$cold-1",
        sender=COLD_PEER,
        body=HELD_BODY,
    )
    session.ws.close()
    # The nest's own answer first (convention 5): a missing marker below is then
    # the app's, not the gate's.
    room = next(r for r in _rooms(nest) if r["far_room_id"] == far_room)
    assert room.get("guardian_state") == "held", room

    # ── The row carries the marker… ──
    conv.navigate()

    def _row():
        for t in conv.list_threads():
            if COLD_PEER in (t.label or ""):
                return t
        return None

    thread = wait_until(
        _row,
        RECEIVE_CYCLE_S,
        diagnose=lambda: (
            f"no thread for {COLD_PEER}; threads={_threads(conv)!r}, "
            f"error: {_page_error(app)!r}"
        ),
    )
    assert thread.rail == "Bridged", thread.rail

    def _markers():
        return app.driver.get_attrs("conversation-guardian-state", "state")

    states = wait_until(
        lambda: _markers() or None,
        RPC_ROUNDTRIP_S,
        diagnose=lambda: (
            f"the held room carries no conversation-guardian-state; "
            f"threads={_threads(conv)!r}, error: {_page_error(app)!r}"
        ),
    )
    assert set(states) == {"held"}, states

    # ── …and so does the open thread, whose text is on screen all the same. ──
    conv.open_thread_by_id(thread.thread_id)
    _await_text(app, HELD_BODY)
    states = _markers()
    assert states and set(states) == {"held"}, (
        f"the open held thread must still say so: {states!r}"
    )


RELEASED_PEER = "@newfriend:example.org"
FIRST_BODY = "hi, we met at the climbing wall"
LATER_BODY = "see you there on saturday"


@pytest.mark.feature("family-safety")
@pytest.mark.tui
@pytest.mark.web
@pytest.mark.linux
@pytest.mark.windows
@pytest.mark.macos
@pytest.mark.ios
def test_a_guardian_lets_a_held_bridged_conversation_through_and_later_messages_arrive(
    request, app, consent_nest, consent_bridge, metadata_server, held_ward
):
    """`family-safety.md` § The bridge-DM gate → *Queue*: "**Approve** = verdict
    `allow`: the conversation releases and future DMs deliver."

    A cold peer's bridged conversation lands held for the ward (the sibling
    test above proves the marker), the guardian Approves its `dm_hold` row on
    their own Family page — the real queue gesture — and the peer writes again:
    the ward's thread now carries no guardian marker, and both the first and
    the later message are on screen.

    The nest's own answer is read at each step (convention 5) — held before,
    released after the Approve — so a marker the app still paints names the
    app, not the gate.
    """
    if not _bridged_built(app.driver):
        skip_unbuilt(
            app.driver,
            surface="bridged rooms on the unified Conversations page",
            detail="no BridgedBackend is registered on this app yet",
            tracked="docs/goal/ui/conversations.md § Implementation status today",
        )
    from conftest import _login_app_as

    ward_nest = _as_user(consent_nest, held_ward)
    guardian = held_ward["guardian"]
    conv = app.conversations

    def _sign_in(nest, identity) -> None:
        app.driver.reset()
        _login_app_as(
            app, request, nest, identity, spa_url_fixture="consent_spa_url", verify_live_actor=True
        )

    def _room_state(far_room: str):
        room = next((r for r in _rooms(ward_nest) if r["far_room_id"] == far_room), None)
        assert room is not None, f"the nest lists no room {far_room!r} for the ward"
        return room.get("guardian_state")

    # ── The ward's app holds the recipient key; a peer no verdict row knows writes ──
    _sign_in(ward_nest, held_ward)
    app.mail_settings.navigate()
    app.mail_settings.ensure_mail_enabled()
    session, _holder_secret, key = _consented_bridge(ward_nest, metadata_server)
    far_room = f"!release-{os.urandom(3).hex()}:example.org"
    _deposit(
        session,
        key,
        far_room_id=far_room,
        far_message_id="$release-1",
        sender=RELEASED_PEER,
        body=FIRST_BODY,
    )
    assert _room_state(far_room) == "held", "precondition: the cold peer's room must be held"

    # ── The guardian lets the conversation through, on their Family page ──
    _sign_in(_as_user(consent_nest, guardian), guardian)
    app.family.navigate()

    def _queue_row() -> list[int] | None:
        for i in range(app.family.approval_count()):
            if RELEASED_PEER in (app.family.approval_text(i) or ""):
                return [i]
        app.family.reload()
        return None

    (row,) = wait_until(
        _queue_row,
        RPC_ROUNDTRIP_S,
        interval=1.0,
        diagnose=lambda: (
            f"no dm_hold row naming {RELEASED_PEER} in the guardian's queue; rows: "
            f"{[app.family.approval_text(i) for i in range(app.family.approval_count())]!r}, "
            f"error: {_page_error(app)!r}"
        ),
    )
    app.family.approve(row)
    wait_until(
        lambda: _room_state(far_room) is None,
        RPC_ROUNDTRIP_S,
        diagnose=lambda: (
            f"after the guardian's Approve the nest still reports the room as "
            f"{_room_state(far_room)!r}; error: {_page_error(app)!r}"
        ),
    )

    # ── The peer writes again ──
    _deposit(
        session,
        key,
        far_room_id=far_room,
        far_message_id="$release-2",
        sender=RELEASED_PEER,
        body=LATER_BODY,
    )
    session.ws.close()
    assert _room_state(far_room) is None, "a later message must not re-hold an approved peer"

    # ── The ward's conversation is open: no marker, both messages there ──
    _sign_in(ward_nest, held_ward)
    conv.navigate()

    def _row():
        for t in conv.list_threads():
            if RELEASED_PEER in (t.label or ""):
                return t
        return None

    thread = wait_until(
        _row,
        RECEIVE_CYCLE_S,
        diagnose=lambda: (
            f"no thread for {RELEASED_PEER}; threads={_threads(conv)!r}, "
            f"error: {_page_error(app)!r}"
        ),
    )
    conv.open_thread_by_id(thread.thread_id)
    _await_text(app, LATER_BODY)
    _await_text(app, FIRST_BODY)
    states = app.driver.get_attrs("conversation-guardian-state", "state")
    assert not states, (
        f"a conversation the guardian let through must carry no guardian marker: {states!r}"
    )


# ── Nostr ────────────────────────────────────────────────────────────────


def _post_to_relay(nest, event: dict) -> None:
    """Publish ``event`` to the nest's own ``/nostr`` relay as an anonymous
    client does (NIP-01 ``EVENT`` → ``OK``)."""
    accepted, message = nostr_relay.publish(nostr_relay.nest_relay_url(nest), event)
    assert accepted, f"the relay refused the gift wrap: {message!r}"


@pytest.mark.feature("nostr")
@pytest.mark.tui
@pytest.mark.web
@pytest.mark.linux
@pytest.mark.windows
@pytest.mark.macos
@pytest.mark.ios
def test_a_nostr_gift_wrap_arrives_as_a_nostr_bridged_room(bridged_app, consent_nest):
    app = bridged_app
    nest = consent_nest
    conv = app.conversations

    # ── Precondition: the account has a custodial Nostr key (the app's own
    #    link form), which is what the in-process leg serves. ──
    app.nostr.navigate()
    assert app.nostr.ensure_linked(), (
        f"could not link a generated Nostr key: {app.nostr.page_error_text()!r}"
    )
    with alice_client(nest) as ws:
        bridges = ws.call("fauna.bridges.list", {})["bridges"]
    nostr = next(b for b in bridges if b["id"] == "nostr")
    assert nostr["linked"] and nostr.get("identity"), nostr
    npub = nostr["identity"]["value"]

    # ── A far Nostr user gift-wraps a DM to that key and posts it to the
    #    nest's relay. ──
    _post_to_relay(nest, fauna_ffi.nostr_gift_wrap_dm(os.urandom(32), npub, NOSTR_BODY))
    room = wait_until(
        lambda: next((r for r in _rooms(nest) if r["bridge_id"] == "nostr"), None),
        RPC_ROUNDTRIP_S,
        diagnose=lambda: f"the nest listed no nostr room: {_rooms(nest)!r}",
    )
    assert room["bridge_label"] == "Nostr" and room["glyph"] == "bolt", room

    # ── It is a room on the conversations page, painted as the Nostr leg
    #    declares itself. ──
    conv.navigate()

    def _row():
        owners = app.driver.get_attrs("protocol-icon", "bridge")
        threads = conv.list_threads()
        for i, owner in enumerate(owners):
            if owner == "nostr" and i < len(threads):
                return i, threads[i]
        return None

    index, thread = wait_until(
        _row,
        RECEIVE_CYCLE_S,
        diagnose=lambda: (
            f"no nostr-bridged row; threads={_threads(conv)!r}, "
            f"bridges={app.driver.get_attrs('protocol-icon', 'bridge')!r}, "
            f"error: {_page_error(app)!r}"
        ),
    )
    assert thread.rail == "Bridged", thread.rail
    icon = app.driver.get_text("protocol-icon", index=index)
    assert icon.startswith("\u26a1") and icon.endswith("Nostr"), (
        f"a Nostr room paints the bolt and the leg's label: {icon!r}"
    )

    conv.open_thread_by_id(thread.thread_id)
    assert conv.room_class() == "transport-only", conv.room_class()
    _await_text(app, NOSTR_BODY)
