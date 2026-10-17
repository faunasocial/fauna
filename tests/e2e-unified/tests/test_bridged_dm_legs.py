"""tier_3 app journeys: the **Bluesky and ActivityPub DM legs** of the
bridged-conversation family, end to end on the lead app
(``docs/goal/ui/conversations.md`` § Where logic lives → *The ``Bridged``
adapter*, rulings 2 (e) and 3; the tier ``docs/goal/architecture/testing.md``
§ The four-tier taxonomy).

``test_bridged_conversation.py`` proves the family with a third-party bridge
principal and the in-process Nostr leg; the nest tests pin each of these two
legs' deposits, gates and far-call shapes without a far end. This file is the
far end: a real nest and a real app between a fake far network that speaks the
leg's own protocol, so a DM only becomes a room — and a reply only reaches the
peer — if every hop between them is real.

**Bluesky**, one sentence: a far Bluesky user writes the linked account in
``chat.bsky`` (:class:`helpers.atproto_fakes.FakeAtprotoFarEnd`, reached
through the ``test-hooks`` ``FAUNA_TEST_ATPROTO_FAR_END`` seam) → the DM
worker's pass, poked through ``POST /api/v1/test/bluesky/dm/poll-now`` because
its cadence is thirty seconds (convention 14), lists the conversation, reads
its head page and deposits the message sealed to the account's recipient key
(``bluesky/dm_leg.rs::ingest_convo``) → the app's receive loop opens it on the
bridged rail and paints the room with the leg's declared identity (🦋
"Bluesky") → the user replies in the compose bar → the family's ``send``
nudges the leg's drain, which opens the item under the leg's key and sends it
through ``getConvoForMembers`` + ``sendMessage`` under the account's session →
the fake's chat service records it in the conversation with that one peer.

**ActivityPub**, one sentence: a fediverse peer
(:class:`helpers.fake_follower.FakeFollower`) POSTs a signed ``Create{Note}``
addressed to the user's actor alone → the inbox's audience gate hands the
non-public Note to the DM leg (``activitypub/dm_leg.rs::
ingest_non_public_note``), which deposits it in the room keyed on the signer
→ the app paints it (🌐 "Fediverse") → the user replies → the leg's drain
builds a ``Create{Note}`` addressed to the peer alone and queues it for signed
delivery → the peer's inbox receives it, signed with the key the user's actor
publishes.

**The picker**, one sentence per leg: the user types the peer's human spelling
into the Conversations recipient picker → the bridged rail's probe asks the
nest's ``conversation.rooms.open``, which matches the leg's grammar and
resolves the spelling to the stable id the room keys on — a Bluesky handle,
``@`` or not, through ``app.bsky.actor.getProfile`` under the account's session
(``bluesky/dm_leg.rs::far_room_id``); a Fediverse ``@user@host`` by WebFinger
then the actor document (``activitypub/dm_leg.rs::far_room_id``) → the user
sends and the far end receives it addressed to that resolved peer alone. The
Fediverse peer's authority is a loopback ``127.0.0.1:<port>``, which the leg's
grammar, handle parse and WebFinger scheme admit only under the same loopback
hook (``ACTIVITYPUB_LEG_LOOPBACK_TEST_GRAMMAR``).

The account's recipient key is mail's, so each journey has the user enable
mail in the app first — the precondition under which any leg can seal to them.
Linking (Bluesky) and enabling federation (ActivityPub) are preconditions too,
taken through the nest's own doors as ``test_bluesky_feed_ingest.py`` and
``test_feed_fediverse_reply.py`` take them; the subject — the room and the
reply — is the app's (convention 8).

Each leg runs on its own dedicated nest, built with that leg's provider and
``test-hooks``: the AP reply dials a loopback peer, which only the
``FAUNA_TEST_AP_ALLOW_LOOPBACK`` hook a harness-spawned nest carries permits —
hence ``standalone_only``.
"""

from __future__ import annotations

import json
import urllib.request
import uuid

import pytest
import requests

from clients.ws_rpc_admin_client import WsRpcAdminClient
from drivers.port_util import find_free_port
from helpers.ap_nest import deliver_direct_note, enable_ap, start_ap_nest
from helpers.app_surface import skip_unbuilt
from helpers.budgets import RECEIVE_CYCLE_S, RPC_ROUNDTRIP_S
from helpers.fake_follower import FakeFollower, assert_valid_http_signature
from helpers.waiting import wait_until
from tests.test_bluesky_feed_ingest import _link_through_callback
from tests.test_bridged_conversation import _await_text, _page_error, _threads

pytestmark = [pytest.mark.tier_3, pytest.mark.standalone_only, pytest.mark.real_conversations]

#: The domain the Bluesky nest is claimed onto — Bluesky OAuth needs a public
#: identity domain to derive its client (`bluesky::oauth_public_url`).
BSKY_DOMAIN = "bsky-dm.test"

#: The far Bluesky user who writes the linked account.
PEER_DID = "did:plc:erinfaketestdmpeer234567"
PEER_HANDLE = "erin.test"


def _unique(prefix: str) -> str:
    return f"{prefix}-{uuid.uuid4().hex[:10]}"


def _require_bridged(app) -> None:
    """Skip an app the bridged rail has not reached — the Nostr journey's
    gate (``test_bridged_conversation._bridged_built``), narrowed to the two
    apps these journeys are marked for."""
    if not (app.driver.is_tui() or app.driver.is_web()):
        skip_unbuilt(
            app.driver,
            surface="bridged rooms on the unified Conversations page",
            detail="no BridgedBackend is registered on this app yet",
            tracked="docs/goal/ui/conversations.md § Implementation status today",
        )


def _user_client(nest) -> WsRpcAdminClient:
    user = nest["user"]
    return WsRpcAdminClient(
        nest["url"],
        actor_id=bytes.fromhex(user["actor_id_hex"]),
        signing_key=bytes(user["signing_key"]),
    )


def _rooms(nest) -> list[dict]:
    with _user_client(nest) as ws:
        return ws.call("fauna.bridges.conversation.rooms.list", {})["rooms"]


def _leg_room(nest, bridge_id: str) -> dict:
    """The nest's own answer first (convention 5): the leg's one room, so a
    missing row in the app below is the app's defect, not the leg's."""
    rooms = [r for r in _rooms(nest) if r["bridge_id"] == bridge_id]
    assert len(rooms) == 1, f"the {bridge_id} leg holds exactly one room: {_rooms(nest)!r}"
    return rooms[0]


def _signed_in_with_mail(app, request, nest, spa_url_fixture: str) -> None:
    """Log ``app`` in as ``nest["user"]`` and have the user enable mail in the
    app — which provisions the recipient key every leg seals to."""
    from conftest import _login_app_as

    _login_app_as(
        app, request, nest, nest["user"],
        spa_url_fixture=spa_url_fixture, verify_live_actor=True,
    )
    app.mail_settings.navigate()
    app.mail_settings.ensure_mail_enabled()


def _leg_row_on_the_page(app, bridge_id: str, emoji: str, label: str):
    """Navigate to Conversations and return the thread of the leg's room,
    once it paints, after asserting it paints the leg's declared identity."""
    conv = app.conversations
    conv.navigate()

    def _row():
        owners = app.driver.get_attrs("protocol-icon", "bridge")
        threads = conv.list_threads()
        for i, owner in enumerate(owners):
            if owner == bridge_id and i < len(threads):
                return i, threads[i]
        return None

    index, thread = wait_until(
        _row,
        RECEIVE_CYCLE_S,
        diagnose=lambda: (
            f"no {bridge_id}-bridged row; threads={_threads(conv)!r}, "
            f"bridges={app.driver.get_attrs('protocol-icon', 'bridge')!r}, "
            f"error: {_page_error(app)!r}"
        ),
    )
    assert thread.rail == "Bridged", thread.rail
    icon = app.driver.get_text("protocol-icon", index=index)
    assert icon.startswith(emoji) and icon.endswith(label), (
        f"a {bridge_id} room paints the leg's declared glyph {emoji!r} and "
        f"label {label!r}: {icon!r}"
    )
    return thread


def _open_and_reply(app, thread, inbound: str, reply: str) -> None:
    conv = app.conversations
    conv.open_thread_by_id(thread.thread_id)
    assert conv.room_class() == "transport-only", conv.room_class()
    _await_text(app, inbound)
    conv.send_in_open_thread(reply)


# ── Bluesky ──────────────────────────────────────────────────────────────


@pytest.fixture
def atproto_far_end():
    from helpers.atproto_fakes import FakeAtprotoFarEnd

    far = FakeAtprotoFarEnd()
    yield far
    far.close()


@pytest.fixture
def bluesky_dm_nest(bluesky_nest_binary, tmp_path_factory, atproto_far_end):
    """A `bluesky` + `test-hooks` nest claimed onto a public domain whose
    consume-side OAuth client talks to :func:`atproto_far_end`, with one
    registered user. Per-test: the test links the user's account, which a
    second app's run against a shared nest would be refused."""
    from conftest import _make_nest, _make_user

    nest, cleanup = _make_nest(
        bluesky_nest_binary, tmp_path_factory, "bluesky-dm-leg",
        claim_domain=BSKY_DOMAIN,
        extra_env={"FAUNA_TEST_ATPROTO_FAR_END": atproto_far_end.url},
    )
    nest["user"] = _make_user(nest)
    yield nest
    cleanup()


@pytest.fixture
def bluesky_dm_spa_url(spa_proxy_for, bluesky_dm_nest):
    return spa_proxy_for(bluesky_dm_nest["url"])


def _dm_poll_now(nest) -> dict:
    resp = requests.post(
        f"{nest['url']}/api/v1/test/bluesky/dm/poll-now", timeout=60, verify=False,
    )
    assert resp.status_code == 200, f"dm poll-now: {resp.status_code} {resp.text}"
    return resp.json()


@pytest.mark.feature("atproto")
@pytest.mark.tui
@pytest.mark.web
def test_a_bluesky_dm_arrives_as_a_bluesky_room_and_the_reply_reaches_that_peer_alone(
    request, app, bluesky_dm_nest, atproto_far_end
):
    _require_bridged(app)
    nest, far = bluesky_dm_nest, atproto_far_end
    _link_through_callback(nest, far)
    _signed_in_with_mail(app, request, nest, "bluesky_dm_spa_url")

    # ── A far Bluesky user writes the linked account; one worker pass. ──
    inbound = _unique("a bluesky dm")
    convo_id = far.send_dm(sender_did=PEER_DID, sender_handle=PEER_HANDLE, text=inbound)
    report = _dm_poll_now(nest)
    assert report["errors"] == [] and report["stored"] == 1, (
        f"the pass should store the one DM: {report!r}; far end saw {far.requests!r}"
    )
    room = _leg_room(nest, "bluesky")
    assert room["far_room_id"] == PEER_DID, room
    assert room["bridge_label"] == "Bluesky" and room["glyph"] == "butterfly", room

    # ── It is a room on the conversations page, painted as the leg declares. ──
    thread = _leg_row_on_the_page(app, "bluesky", "\U0001f98b", "Bluesky")

    # ── The user replies; the leg sends it to that conversation alone. ──
    reply = _unique("and back over bluesky")
    _open_and_reply(app, thread, inbound, reply)
    sent = wait_until(
        far.sent_snapshot,
        RECEIVE_CYCLE_S,
        diagnose=lambda: (
            f"the chat service received no sendMessage; far end saw "
            f"{far.requests[-10:]!r}; error: {_page_error(app)!r}"
        ),
    )
    assert sent == [{
        "convo_id": convo_id,
        "members": sorted([far.did, PEER_DID]),
        "text": reply,
    }], sent
    assert far.convo_lookups == [[PEER_DID]], (
        f"the leg must ask for the conversation with the room's peer alone: "
        f"{far.convo_lookups!r}"
    )
    texts = app.driver.get_texts("dm-message-text")
    assert any(reply in (t or "") for t in texts), texts


@pytest.mark.feature("atproto")
@pytest.mark.tui
@pytest.mark.web
def test_a_bluesky_room_started_from_the_picker_by_handle_reaches_the_resolved_peer_alone(
    request, app, bluesky_dm_nest, atproto_far_end
):
    _require_bridged(app)
    nest, far = bluesky_dm_nest, atproto_far_end
    _link_through_callback(nest, far)
    far.register_profile(handle=PEER_HANDLE, did=PEER_DID)
    _signed_in_with_mail(app, request, nest, "bluesky_dm_spa_url")
    conv = app.conversations

    # ── A handle the service names no one by opens nothing. ──
    state = conv.resolve_recipient("@nobody.test")
    assert state == "not-found", (
        f"a handle getProfile refuses is no bridged address: {state!r}; "
        f"error: {_page_error(app)!r}"
    )
    conv.cancel_new_conversation()
    assert [r for r in _rooms(nest) if r["bridge_id"] == "bluesky"] == [], _rooms(nest)

    # ── The user writes a handle, with its `@`, from the picker. ──
    body = _unique("a bluesky dm from the picker")
    conv.start_new_conversation(recipient=f"@{PEER_HANDLE}", body=body)
    sent = wait_until(
        far.sent_snapshot,
        RECEIVE_CYCLE_S,
        diagnose=lambda: (
            f"the chat service received no sendMessage; far end saw "
            f"{far.requests[-10:]!r}; rooms: {_rooms(nest)!r}; error: {_page_error(app)!r}"
        ),
    )
    assert [(s["members"], s["text"]) for s in sent] == [
        (sorted([far.did, PEER_DID]), body)
    ], sent
    assert far.convo_lookups == [[PEER_DID]], (
        f"the leg must ask for the conversation with the resolved peer alone: "
        f"{far.convo_lookups!r}"
    )
    room = _leg_room(nest, "bluesky")
    assert room["far_room_id"] == PEER_DID, (
        f"the room keys on the DID the handle resolved to, never the handle: {room!r}"
    )
    texts = app.driver.get_texts("dm-message-text")
    assert any(body in (t or "") for t in texts), texts


# ── ActivityPub ──────────────────────────────────────────────────────────


@pytest.fixture(scope="module")
def ap_binary():
    """fauna-nest with ``activitypub`` + ``test-hooks`` — the federation
    suite's build, replayed from the collection-time prebuild."""
    from conftest import _ensure_ap_nest_built

    return _ensure_ap_nest_built()


@pytest.fixture
def fediverse_dm_nest(ap_binary, tmp_path_factory):
    """An AP-enabled nest, its domain its own loopback authority, with one
    registered user whose federation is enabled. Per-test, as the Bluesky
    nest is: each app's run reads a nest no other run wrote."""
    from conftest import _make_user

    port = find_free_port()
    nest = start_ap_nest(
        ap_binary,
        tmp_path_factory.mktemp("fediverse-dm-nest"),
        port,
        f"127.0.0.1:{port}",
    )
    try:
        nest["user"] = _make_user(nest)
        nest["ap_username"], nest["ap_actor_url"] = enable_ap(nest, nest["user"])
        yield nest
    finally:
        nest["proc"].kill()
        nest["proc"].wait()


@pytest.fixture
def fediverse_dm_spa_url(static_dir, fediverse_dm_nest):
    from conftest import _serve_spa_proxy

    url, server = _serve_spa_proxy(static_dir, fediverse_dm_nest["url"])
    yield url
    server.shutdown()


@pytest.fixture
def fediverse_peer():
    with FakeFollower(find_free_port(), "frank") as peer:
        yield peer


def _account_public_key(nest) -> str:
    req = urllib.request.Request(
        f"{nest['url']}/ap/users/{nest['ap_username']}",
        headers={"Accept": "application/activity+json"},
    )
    with urllib.request.urlopen(req) as resp:
        return json.loads(resp.read())["publicKey"]["publicKeyPem"]


@pytest.mark.feature("fediverse")
@pytest.mark.tui
@pytest.mark.web
def test_a_fediverse_dm_arrives_as_a_fediverse_room_and_the_reply_reaches_that_peer_alone(
    request, app, fediverse_dm_nest, fediverse_peer
):
    _require_bridged(app)
    nest, peer = fediverse_dm_nest, fediverse_peer
    _signed_in_with_mail(app, request, nest, "fediverse_dm_spa_url")

    # ── A fediverse peer sends the user a direct Note. ──
    inbound = _unique("a fediverse dm")
    deliver_direct_note(nest, peer, nest["ap_username"], nest["ap_actor_url"], inbound)
    room = wait_until(
        lambda: next((r for r in _rooms(nest) if r["bridge_id"] == "activitypub"), None),
        RPC_ROUNDTRIP_S,
        diagnose=lambda: f"the nest listed no activitypub room: {_rooms(nest)!r}",
    )
    assert room["far_room_id"] == peer.actor_uri, room
    assert room["bridge_label"] == "Fediverse" and room["glyph"] == "globe", room

    # ── It is a room on the conversations page, painted as the leg declares. ──
    thread = _leg_row_on_the_page(app, "activitypub", "\U0001f310", "Fediverse")

    # ── The user replies; the peer's inbox receives a Note to it alone. ──
    reply = _unique("and back over the fediverse")
    _open_and_reply(app, thread, inbound, reply)
    entry = peer.wait_for(
        "Create", where=lambda a: reply in (a.get("object") or {}).get("content", "")
    )
    assert_valid_http_signature(
        entry, _account_public_key(nest), f"/users/{peer.username}/inbox"
    )
    activity = entry["activity"]
    note = activity["object"]
    assert activity["actor"] == nest["ap_actor_url"], activity["actor"]
    for addressed in (activity, note):
        assert addressed["to"] == [peer.actor_uri], addressed["to"]
        assert not addressed.get("cc"), addressed.get("cc")
    texts = app.driver.get_texts("dm-message-text")
    assert any(reply in (t or "") for t in texts), texts


@pytest.mark.feature("fediverse")
@pytest.mark.tui
@pytest.mark.web
def test_a_fediverse_room_started_from_the_picker_by_account_reaches_the_resolved_peer_alone(
    request, app, fediverse_dm_nest, fediverse_peer
):
    _require_bridged(app)
    nest, peer = fediverse_dm_nest, fediverse_peer
    _signed_in_with_mail(app, request, nest, "fediverse_dm_spa_url")

    # ── The user writes the peer's `@user@host` from the picker. ──
    body = _unique("a fediverse dm from the picker")
    app.conversations.start_new_conversation(recipient=peer.handle, body=body)
    entry = peer.wait_for(
        "Create", where=lambda a: body in (a.get("object") or {}).get("content", "")
    )
    assert f"acct:{peer.handle.removeprefix('@')}" in peer.webfinger_queries, (
        f"the nest resolves the account by WebFinger: {peer.webfinger_queries!r}"
    )
    assert_valid_http_signature(
        entry, _account_public_key(nest), f"/users/{peer.username}/inbox"
    )
    activity = entry["activity"]
    note = activity["object"]
    assert activity["actor"] == nest["ap_actor_url"], activity["actor"]
    for addressed in (activity, note):
        assert addressed["to"] == [peer.actor_uri], addressed["to"]
        assert not addressed.get("cc"), addressed.get("cc")
    room = _leg_room(nest, "activitypub")
    assert room["far_room_id"] == peer.actor_uri, (
        f"the room keys on the actor WebFinger named, never the account: {room!r}"
    )
    texts = app.driver.get_texts("dm-message-text")
    assert any(body in (t or "") for t in texts), texts
