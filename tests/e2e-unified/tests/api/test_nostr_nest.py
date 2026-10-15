"""tier_3 nest-side witnesses for the Nostr page (`docs/features/nostr.md`).

The thirteen `[nest]` outcomes of the page — what a Nostr client, a Nostr relay
or another nest observes of a nest, the half no app journey can see. Each test
speaks only the wire, from outside, and drives no app:

* the nest's **own relay** (`/nostr`), as an anonymous NIP-01 client — the
  posts you share (5, 6) and the direct-message inbox (8)
  (`docs/goal/ui/nostr.md` § The relay event store);
* its **NIP-05 document** (`/.well-known/nostr.json`) — your address (7);
* **relays the nest dials** — a :class:`~helpers.nostr_relay.FakeRelay` as your
  publish relay, recording what the nest sends it (12, 14, 15, 16, 21), and as a
  followed author's relay, serving that author's notes and deletion (10)
  (§ Replying to and quoting a nostr note → *When the nest publishes*,
  § Implementation status today);
* the **WS-RPC link door** with a key held outside the nest — a NIP-07 signer
  stood in for by the in-tree `fauna_bridge_nostr` signer (`fauna_ffi`), and a
  second nest's bunker as the remote signer (18, 19, 22) (§ Errors & edge
  cases → *One pubkey, one account*, *Proof of possession*; § The nest as the
  user's NIP-46 signer (bunker)).

**Two nests under test, split by whether the nest must dial out.** The outcomes
observed at the nest's own doors (5, 6, 7, 8, 18's NIP-07 arm, 19) run on
``nostr_home_nest``, a nest of the run's mode — so a docker run witnesses the
image. The outcomes observed at a relay the nest DIALS (10, 12, 14, 15, 16, 18's
remote-signer arm, 22, 21) run on ``nostr_dialing_nest``, a locally built nest
with ``FAUNA_TEST_NOSTR_ALLOW_LOOPBACK`` set: every relay this harness can host
is on loopback, a production nest refuses a loopback relay both when the user
stores it and when it dials (`nest/network-exposure.md` § Rulings F7), and the
`test-hooks` build's env escape admits loopback alone, at both doors
(`relays.rs::relay_dial_policy`) — the shipping nest with exactly one address
class widened, which the image cannot be. The signer nest needs nothing: it is
only ever dialed.

**What the absence assertions anchor on (convention 14).** A post the nest must
NOT publish (15, 21) is asserted absent only after a later control post from
the same account has arrived at the same relay: the create-side arm feeds one
ordered outbound channel and one connection per relay, so an event that was
going to be sent would have arrived first. Never a wall-clock wait.

**The one cadence no test can poke.** A followed author's notes reach the feed
through the sync worker's one-minute inbound tick (`nostr/sync_worker.rs`), with
no wake for a new follow; the module's followed-author fixture pays it once and
the deletion test once more, under ``NOSTR_INBOUND_TICK_S``.
"""

from __future__ import annotations

import json
import os
import secrets
import ssl
import time
import urllib.request

import pytest
from clients.ws_rpc_admin_client import RpcCallError, WsRpcAdminClient
from common import create_actor_and_register

import fauna_ffi
from helpers import nostr_relay
from helpers.budgets import NOSTR_INBOUND_TICK_S, RPC_ROUNDTRIP_S
from helpers.nostr_relay import FakeRelay, await_received, tag_values
from helpers.waiting import wait_until
from tests.api import ws_api
from tests.api.bare import post_reference, sign_and_encode_post, sign_and_encode_tombstone

pytestmark = pytest.mark.tier_3

#: The home nest's identity domain — the `you@<domain>` of its NIP-05 names and
#: the host of the `wss://<domain>/nostr` relay it advertises.
NOSTR_DOMAIN = "fauna.test"


# ── fixtures ─────────────────────────────────────────────────────────────────


def _resident(nest) -> dict:
    """A user holding a generated key — the box's nsec deposit, which is the
    bridging gate every relay serve and every non-custodial link waits on
    (`nostr.md` § The bridging gate)."""
    user = _user(nest)
    _link_generated(nest, user)
    return user


@pytest.fixture(scope="module")
def nostr_home_nest(request, nest_mode, tmp_path_factory):
    """A nest of the run's mode claimed onto ``NOSTR_DOMAIN``, with one resident
    depositor so its relay serves — for the outcomes observed at the nest's own
    doors, which every mode can host."""
    from conftest import _start_dedicated_nest

    nest, cleanup = _start_dedicated_nest(
        request, nest_mode, tmp_path_factory, "nostr-home", claim_domain=NOSTR_DOMAIN,
    )
    nest["resident"] = _resident(nest)
    yield nest
    cleanup()


@pytest.fixture(scope="module")
def nostr_dialing_nest(nest_binary, tmp_path_factory):
    """A locally built nest allowed to dial a loopback relay, with one resident
    depositor — for the outcomes observed at a relay the NEST dials (the
    module docstring says why that needs this build)."""
    from conftest import _make_nest

    nest, cleanup = _make_nest(
        nest_binary, tmp_path_factory, "nostr-dialing",
        extra_env=dict(nostr_relay.ALLOW_LOOPBACK_ENV),
    )
    nest["resident"] = _resident(nest)
    yield nest
    cleanup()


@pytest.fixture(scope="module")
def nostr_signer_nest(request, nest_mode, tmp_path_factory):
    """A second nest whose bunker serves as the remote signer a dialing-nest user
    links. Its resident keeps its relay serving after a test unlinks its own
    signer account."""
    from conftest import _start_dedicated_nest

    nest, cleanup = _start_dedicated_nest(request, nest_mode, tmp_path_factory, "nostr-signer")
    nest["resident"] = _resident(nest)
    yield nest
    cleanup()


@pytest.fixture(scope="module")
def publish_relay():
    """The relay home-nest users publish to: it records what the nest sends."""
    relay = FakeRelay()
    yield relay
    relay.close()


@pytest.fixture(scope="module")
def followed(nostr_dialing_nest, publish_relay):
    """A home-nest user following a far Nostr author whose relay holds two of
    their notes, both already swept into the user's feed.

    ``note_to_delete`` is the deletion test's; ``note_to_answer`` the reply and
    quote test's — one fixture so the sync worker's inbound tick is paid once.
    """
    nest = nostr_dialing_nest
    author_secret = os.urandom(32)
    author = fauna_ffi.nostr_keys(author_secret)
    author_relay = FakeRelay()

    user = _user(nest)
    _link_generated(nest, user)
    _settings(nest, user, relay_list=[publish_relay.url])
    feed_id = ws_api.create_feed(nest["port"], user, "Everything", rules=[])

    now = int(time.time())
    notes = {}
    for label, age in (("note_to_delete", 120), ("note_to_answer", 60)):
        notes[label] = fauna_ffi.nostr_sign_event(author_secret, {
            "kind": 1, "created_at": now - age, "tags": [],
            "content": f"{label} from a far author {secrets.token_hex(4)}",
        })
        author_relay.seed(notes[label])

    ws_api.bridge_add_follow(
        nest["port"], user, "nostr", author["npub"],
        extra={"relay_hints": [author_relay.url]},
    )

    def _swept():
        posts = {p["body"]: p for p in ws_api.feed_posts(nest["port"], user, feed_id)}
        found = {k: posts.get(n["content"]) for k, n in notes.items()}
        return found if all(found.values()) else None

    swept = wait_until(
        _swept,
        NOSTR_INBOUND_TICK_S,
        diagnose=lambda: (
            f"the followed author's notes never reached the feed; the author's relay "
            f"saw REQs {author_relay.requests!r}; feed: "
            f"{[p['body'][:40] for p in ws_api.feed_posts(nest['port'], user, feed_id)]!r}"
        ),
    )
    yield {
        "nest": nest, "user": user, "feed_id": feed_id,
        "author_secret": author_secret, "author": author, "author_relay": author_relay,
        "notes": notes, "posts": swept,
    }
    author_relay.close()


# ── helpers ──────────────────────────────────────────────────────────────────


def _user(nest) -> dict:
    return create_actor_and_register(
        nest["port"], base_url=nest["url"], admin_signing_key=nest["admin"]["signing_key"],
    )


def _ws(nest, user) -> WsRpcAdminClient:
    return WsRpcAdminClient(
        nest["url"], actor_id=user["actor_id_bytes"], signing_key=bytes(user["signing_key"]),
    )


def _link_generated(nest, user) -> str:
    """Link ``user`` with a nest-generated key; returns its hex pubkey."""
    reply = ws_api.bridge_link(nest["port"], user, "nostr", "generate")
    assert reply["linked"], reply
    user["nostr_npub"] = reply["identity"]["value"]
    user["nostr_pubkey"] = fauna_ffi.nostr_npub_to_hex(user["nostr_npub"])
    return user["nostr_pubkey"]


def _settings(nest, user, *, relay_list=None, **flags) -> None:
    """The Nostr page's settings, as its toggles and relay list send them."""
    settings = dict(flags)
    if relay_list is not None:
        settings["relay_list"] = json.dumps(relay_list)
    ws_api.bridge_set_settings(nest["port"], user, "nostr", settings)


def _post(nest, user, text, references=None) -> str:
    return ws_api.create_post(
        nest["port"], user,
        sign_and_encode_post(
            user["signing_key"], int(time.time() * 1_000_000), text, references=references,
        ),
    )


def _text(label: str) -> str:
    return f"{label} {secrets.token_hex(4)}"


def _own_relay_notes(nest, pubkey) -> list[dict]:
    """The kind-1 notes by ``pubkey`` an anonymous client reads off the nest's
    own relay."""
    return nostr_relay.query(
        nostr_relay.nest_relay_url(nest), {"authors": [pubkey], "kinds": [1]},
    )


def _published(relay, user, text) -> dict:
    return await_received(
        relay,
        lambda e: e["pubkey"] == user["nostr_pubkey"] and e["kind"] == 1 and text in e["content"],
        RPC_ROUNDTRIP_S,
        f"the note {text!r}",
    )


def _refused(code, call):
    with pytest.raises(RpcCallError) as refused:
        call()
    assert refused.value.code == code, f"{refused.value.code}: {refused.value}"


def _bunker_invite(signer_nest, signer_user, secret=None) -> str:
    """A connect string from ``signer_user``'s bunker, pointed at the signer
    nest's relay as this harness reaches it (the nest names its relay by its own
    domain, which the harness does not resolve). ``secret`` overrides the
    invite's one-time secret."""
    with _ws(signer_nest, signer_user) as ws:
        invite = ws.call("fauna.nostr.bunker.create_invite", {})
    connect = invite["connect_string"]
    issued_secret = connect.rsplit("secret=", 1)[1]
    return (
        f"bunker://{invite['signer_pubkey']}?relay={nostr_relay.nest_relay_url(signer_nest)}"
        f"&secret={secret or issued_secret}"
    )


def _bunker_apps(signer_nest, signer_user) -> list[dict]:
    with _ws(signer_nest, signer_user) as ws:
        return ws.call("fauna.nostr.bunker.list", {})["apps"]


# ── the nest's own relay ─────────────────────────────────────────────────────


@pytest.mark.feature("nostr")
def test_another_nostr_client_reads_your_shared_posts_from_your_nest(nostr_home_nest):
    """Outcome 5 — with sharing on, a Nostr client reads your post straight off
    your nest's relay, signed by your key (`nostr.md` § The relay event store)."""
    nest = nostr_home_nest
    user = _user(nest)
    pubkey = _link_generated(nest, user)
    text = _text("shared from my nest")
    _post(nest, user, text)

    assert not [e for e in _own_relay_notes(nest, pubkey) if e["content"] == text], (
        "sharing is off by default: the post must not be on the relay yet"
    )

    _settings(nest, user, expose_content=True)
    served = [e for e in _own_relay_notes(nest, pubkey) if e["content"] == text]
    assert len(served) == 1, f"the relay serves the shared post once: {served!r}"
    assert served[0]["pubkey"] == pubkey
    assert fauna_ffi.nostr_verify_event(served[0]), (
        f"a Nostr client must be able to verify what the relay serves: {served[0]!r}"
    )


@pytest.mark.feature("nostr")
def test_turning_sharing_off_takes_posts_off_your_relay_and_on_brings_them_back(
    nostr_home_nest,
):
    """Outcome 6 — the off toggle deletes the derived events, the on toggle
    re-derives them (`nostr.md` § The relay event store)."""
    nest = nostr_home_nest
    user = _user(nest)
    pubkey = _link_generated(nest, user)
    text = _text("shared, then not")
    _post(nest, user, text)

    def _served():
        return [e for e in _own_relay_notes(nest, pubkey) if e["content"] == text]

    _settings(nest, user, expose_content=True)
    assert len(_served()) == 1, "sharing on: the post is on the relay"

    _settings(nest, user, expose_content=False)
    assert _served() == [], "sharing off: the post is gone from the relay"

    _settings(nest, user, expose_content=True)
    back = _served()
    assert len(back) == 1 and fauna_ffi.nostr_verify_event(back[0]), (
        f"sharing back on: the post is served again, validly signed: {back!r}"
    )


@pytest.mark.feature("nostr")
def test_your_nostr_address_resolves_to_your_key(nostr_home_nest):
    """Outcome 7 — `you@<nest-domain>` resolves to your key over NIP-05, with
    your nest's own relay as the hint (`nostr.md` § Goal)."""
    nest = nostr_home_nest
    user = _user(nest)
    pubkey = _link_generated(nest, user)
    handle = user["handle"]

    # A docker nest serves its self-signed floor cert; what is under test is
    # the document, not the trust path to it.
    context = ssl._create_unverified_context() if nest["url"].startswith("https://") else None
    with urllib.request.urlopen(
        f"{nest['url']}/.well-known/nostr.json?name={handle}",
        timeout=RPC_ROUNDTRIP_S, context=context,
    ) as resp:
        assert resp.headers.get("Access-Control-Allow-Origin") == "*", (
            "a web Nostr client fetches NIP-05 cross-origin"
        )
        document = json.loads(resp.read())
    assert document["names"].get(handle) == pubkey, document
    assert document["relays"].get(pubkey) == [f"wss://{NOSTR_DOMAIN}/nostr"], document


@pytest.mark.feature("nostr")
def test_a_direct_message_is_accepted_by_your_own_nest(nostr_home_nest):
    """Outcome 8 — a far user's NIP-17 gift wrap posted straight to your nest's
    relay is accepted and kept there for you: served to a Nostr app holding your
    key once it authenticates, and to nobody else; a wrap addressed to a key no
    account here holds is refused (`nostr.md` § The relay event store →
    *Inbox role*, *Read/write policy*). That it then shows as a conversation is
    the app-side outcome 24's."""
    nest = nostr_home_nest
    user = _user(nest)
    secret = os.urandom(32)
    keys = fauna_ffi.nostr_keys(secret)
    ws_api.bridge_link(nest["port"], user, "nostr", "import", {"nsec": keys["nsec"]})
    relay = nostr_relay.nest_relay_url(nest)

    stranger = fauna_ffi.nostr_keys(os.urandom(32))["npub"]
    accepted, message = nostr_relay.publish(
        relay, fauna_ffi.nostr_gift_wrap_dm(os.urandom(32), stranger, _text("to nobody here")),
    )
    assert not accepted, f"a wrap for a key no account holds is refused: {message!r}"

    wrap = fauna_ffi.nostr_gift_wrap_dm(os.urandom(32), keys["npub"], _text("hello"))
    accepted, message = nostr_relay.publish(relay, wrap)
    assert accepted, f"your own nest is your inbox relay: {message!r}"

    inbox = {"kinds": [1059], "#p": [keys["pubkey"]]}
    served, closed = nostr_relay.subscribe(relay, inbox)
    assert not served and closed and closed.startswith("auth-required"), (
        f"an anonymous client is told to authenticate, and gets nothing: {closed!r} {served!r}"
    )
    served, closed = nostr_relay.subscribe(
        relay, inbox,
        auth=(f"wss://{NOSTR_DOMAIN}/nostr", lambda t: fauna_ffi.nostr_sign_event(secret, t)),
    )
    assert closed is None and [e["id"] for e in served] == [wrap["id"]], (
        f"the recipient's own Nostr app reads the DM from the nest: {closed!r} {served!r}"
    )


# ── a followed author's relay ────────────────────────────────────────────────


@pytest.mark.feature("nostr")
def test_a_followed_authors_delete_removes_the_post_from_your_feed(followed):
    """Outcome 10 — the author's NIP-09 deletion, arriving from their relay,
    withdraws the swept post; their other post stays (`nostr.md`
    § Implementation status today)."""
    nest, user, feed_id = followed["nest"], followed["user"], followed["feed_id"]
    doomed = followed["posts"]["note_to_delete"]["post_id"]
    kept = followed["posts"]["note_to_answer"]["post_id"]

    followed["author_relay"].seed(fauna_ffi.nostr_sign_event(followed["author_secret"], {
        "kind": 5, "created_at": int(time.time()),
        "tags": [["e", followed["notes"]["note_to_delete"]["id"]]], "content": "",
    }))

    def _ids():
        return {p["post_id"] for p in ws_api.feed_posts(nest["port"], user, feed_id)}

    ids = wait_until(
        lambda: (lambda ids: ids if doomed not in ids else None)(_ids()),
        NOSTR_INBOUND_TICK_S,
        diagnose=lambda: f"the deleted note is still in the feed: {sorted(_ids())!r}",
    )
    assert kept in ids, "a deletion naming one note leaves the author's other note alone"


@pytest.mark.feature("nostr")
def test_your_reply_threads_under_a_nostr_post_and_your_quote_links_it(
    followed, publish_relay,
):
    """Outcome 12 — a reply to a swept note reaches your relays as a NIP-10
    threaded reply; a quote carries a `q` tag and a `nostr:` link (`nostr.md`
    § Replying to and quoting a nostr note). Both go out with automatic
    publishing off: the reference is the intent."""
    nest, user = followed["nest"], followed["user"]
    note = followed["notes"]["note_to_answer"]
    target = followed["posts"]["note_to_answer"]["post_id"]
    author = followed["author"]["pubkey"]

    reply_text = _text("my reply")
    _post(nest, user, reply_text, references=[post_reference("Reply", target)])
    reply = _published(publish_relay, user, reply_text)
    assert ["e", note["id"], "", "root"] in reply["tags"], (
        f"the reply names the note as its thread root (NIP-10 marked): {reply['tags']!r}"
    )
    assert [author] in tag_values(reply, "p"), f"the reply tags the author: {reply['tags']!r}"

    quote_text = _text("my quote")
    _post(nest, user, quote_text, references=[post_reference("Quote", target)])
    quote = _published(publish_relay, user, quote_text)
    assert any(v and v[0] == note["id"] for v in tag_values(quote, "q")), (
        f"the quote names the note in a q tag: {quote['tags']!r}"
    )
    assert "nostr:nevent1" in quote["content"], (
        f"the quote links the note for every client to embed: {quote['content']!r}"
    )
    assert fauna_ffi.nostr_verify_event(reply) and fauna_ffi.nostr_verify_event(quote)


# ── your publish relays ──────────────────────────────────────────────────────


@pytest.mark.feature("nostr")
def test_automatic_publishing_sends_your_new_posts_to_your_relays(
    nostr_dialing_nest, publish_relay,
):
    """Outcome 14 — with automatic publishing on, a new post goes out to your
    relays, signed by your key; before it was on, one did not
    (`nostr.md` § Replying to and quoting a nostr note → *When the nest publishes*)."""
    nest = nostr_dialing_nest
    user = _user(nest)
    _link_generated(nest, user)
    _settings(nest, user, relay_list=[publish_relay.url])

    unpublished = _text("before automatic publishing")
    _post(nest, user, unpublished)
    _settings(nest, user, auto_publish=True)
    published_text = _text("with automatic publishing")
    _post(nest, user, published_text)

    event = _published(publish_relay, user, published_text)
    assert fauna_ffi.nostr_verify_event(event), event
    assert not [e for e in publish_relay.received_by(user["nostr_pubkey"], 1)
                if unpublished in e["content"]], (
        "a plain post written while automatic publishing was off stays home"
    )


@pytest.mark.feature("nostr")
def test_with_publishing_replies_off_your_replies_stay_off_your_relays(
    nostr_dialing_nest, publish_relay,
):
    """Outcome 15 — publishing replies off keeps a reply home while plain posts
    still go; switched back on, the next reply goes (`nostr.md` § Replying to
    and quoting a nostr note → *When the nest publishes*)."""
    nest = nostr_dialing_nest
    user = _user(nest)
    _link_generated(nest, user)
    _settings(nest, user, relay_list=[publish_relay.url], auto_publish=True, publish_replies=False)

    parent_text = _text("a post to answer")
    parent = _post(nest, user, parent_text)
    _published(publish_relay, user, parent_text)

    held_reply = _text("a reply kept home")
    _post(nest, user, held_reply, references=[post_reference("Reply", parent)])
    control = _text("a plain post after the reply")
    _post(nest, user, control)
    _published(publish_relay, user, control)
    assert not [e for e in publish_relay.received_by(user["nostr_pubkey"])
                if held_reply in e["content"]], (
        "a reply written with publishing replies off never reaches the relay"
    )

    _settings(nest, user, publish_replies=True)
    sent_reply = _text("a reply sent out")
    _post(nest, user, sent_reply, references=[post_reference("Reply", parent)])
    _published(publish_relay, user, sent_reply)


@pytest.mark.feature("room-restricted-posts")
def test_a_room_post_is_never_published_to_nostr(nostr_dialing_nest, publish_relay):
    """`room-restricted-posts` outcome 14 — a room post is never published to
    another network (`restricted-posts.md` § Encryption at rest; the off-box
    predicate `db/public_servability.rs`, which the Nostr publish and the
    relay materializer both hold to).

    Its author publishes everything else: automatic publishing on, sharing on
    the nest's own relay on. A community room is founded over the wire
    (``helpers/community_room.py``) and the author posts to it, then posts a
    public control. The control reaching the publish relay settles the one
    ordered outbound channel the room post would have gone down first, and the
    own relay's answer is read once the control is on it (convention 14); the
    room post's teaser is on neither."""
    from helpers.community_room import found_room, room_post

    nest = nostr_dialing_nest
    user = _user(nest)
    pubkey = _link_generated(nest, user)
    _settings(nest, user, relay_list=[publish_relay.url], auto_publish=True, expose_content=True)

    room = found_room(nest, owner=user)
    teaser = _text("a room post's teaser")
    room_post(room, teaser, f"{teaser}\n\nFor the square only.")
    control = _text("a public post after the room post")
    _post(nest, user, control)

    _published(publish_relay, user, control)
    assert not [e for e in publish_relay.received_by(pubkey) if teaser in e["content"]], (
        "a room post never reaches the author's relays"
    )
    own = _own_relay_notes(nest, pubkey)
    assert [e for e in own if e["content"] == control], (
        f"the nest's own relay serves the public control: {own!r}"
    )
    assert not [e for e in own if teaser in e["content"]], (
        "the nest's own relay never serves a room post"
    )


@pytest.mark.feature("nostr")
def test_deleting_a_published_post_takes_it_back_on_nostr(nostr_dialing_nest, publish_relay):
    """Outcome 16 — deleting a post that went out sends your relays a NIP-09
    deletion naming the event (`nostr.md` § Replying to and quoting a nostr
    note → *Mirror, thread, delete*)."""
    nest = nostr_dialing_nest
    user = _user(nest)
    pubkey = _link_generated(nest, user)
    _settings(nest, user, relay_list=[publish_relay.url], auto_publish=True)

    text = _text("published, then deleted")
    post_id = _post(nest, user, text)
    event = _published(publish_relay, user, text)

    ws_api.delete_post(
        nest["port"], user,
        sign_and_encode_tombstone(user["signing_key"], post_id, int(time.time() * 1_000_000)),
    )
    deletion = await_received(
        publish_relay,
        lambda e: e["pubkey"] == pubkey and e["kind"] == 5 and [event["id"]] in [
            v[:1] for v in tag_values(e, "e")
        ],
        RPC_ROUNDTRIP_S,
        f"a deletion of {event['id'][:16]}",
    )
    assert fauna_ffi.nostr_verify_event(deletion), deletion


@pytest.mark.feature("nostr")
def test_an_empty_relay_list_publishes_nowhere(nostr_dialing_nest, publish_relay):
    """Outcome 21 — with every relay removed, a post that would go out goes
    nowhere — not to the relay you removed, not to built-in defaults — and the
    next post, once a relay is back, does (`nostr.md` § Errors & edge cases)."""
    nest = nostr_dialing_nest
    user = _user(nest)
    _link_generated(nest, user)
    _settings(nest, user, relay_list=[publish_relay.url], auto_publish=True)
    _settings(nest, user, relay_list=[])

    held = _text("written with no relays")
    _post(nest, user, held)
    _settings(nest, user, relay_list=[publish_relay.url])
    control = _text("written with a relay back")
    _post(nest, user, control)

    _published(publish_relay, user, control)
    assert not [e for e in publish_relay.received_by(user["nostr_pubkey"])
                if held in e["content"]], (
        "a post written while the relay list was empty is never sent, even once a relay is back"
    )


# ── keys held outside the nest ───────────────────────────────────────────────


@pytest.mark.feature("nostr")
def test_a_browser_extension_key_links_only_once_proven(nostr_home_nest):
    """Outcome 18, the NIP-07 arm — a key the nest does not hold links only with
    a signature over this nest's challenge, by that key (`nostr.md` § Errors &
    edge cases → *Proof of possession*)."""
    nest = nostr_home_nest
    user = _user(nest)
    port = nest["port"]
    secret = os.urandom(32)
    keys = fauna_ffi.nostr_keys(secret)

    _refused("fauna.bridges.proof_required", lambda: ws_api.bridge_link(
        port, user, "nostr", "nip07", {"pubkey": keys["pubkey"]},
    ))

    challenge = ws_api.bridge_link_challenge(port, user, "nostr", "nip07")
    forged = fauna_ffi.nostr_sign_event(os.urandom(32), challenge["payload"])
    _refused("fauna.bridges.proof_required", lambda: ws_api.bridge_link(
        port, user, "nostr", "nip07",
        {"pubkey": keys["pubkey"], "proof_json": json.dumps(forged)},
    ))
    assert not ws_api.bridge_status(port, user, "nostr")["linked"], "nothing unproven links"

    challenge = ws_api.bridge_link_challenge(port, user, "nostr", "nip07")
    proof = fauna_ffi.nostr_sign_event(secret, challenge["payload"])
    reply = ws_api.bridge_link(
        port, user, "nostr", "nip07",
        {"pubkey": keys["pubkey"], "proof_json": json.dumps(proof)},
    )
    assert reply["linked"] and reply["identity"]["value"] == keys["npub"], reply


@pytest.mark.feature("nostr")
def test_a_remote_signer_key_links_only_once_its_bunker_proves_it(
    nostr_dialing_nest, nostr_signer_nest,
):
    """Outcome 18, the remote-signer arm — a bunker that will not complete the
    NIP-46 handshake links nothing; one that does links the key it proved, the
    signer's USER key and never its signer key (`nostr.md` § Errors & edge
    cases → *Proof of possession*)."""
    signer_user = _user(nostr_signer_nest)
    _link_generated(nostr_signer_nest, signer_user)
    user = _user(nostr_dialing_nest)
    port = nostr_dialing_nest["port"]

    wrong = _bunker_invite(nostr_signer_nest, signer_user, secret="not-the-invite-secret")
    _refused("fauna.bridges.proof_required", lambda: ws_api.bridge_link(
        port, user, "nostr", "remote", {"bunker_url": wrong},
    ))
    assert not ws_api.bridge_status(port, user, "nostr")["linked"], "nothing unproven links"

    reply = ws_api.bridge_link(
        port, user, "nostr", "remote",
        {"bunker_url": _bunker_invite(nostr_signer_nest, signer_user)},
    )
    assert reply["linked"] and reply["identity"]["value"] == signer_user["nostr_npub"], (
        f"the link holds the key the bunker signed as: {reply!r}"
    )


@pytest.mark.feature("nostr")
def test_a_key_another_account_holds_cannot_be_linked_to_yours(nostr_home_nest):
    """Outcome 19 — one pubkey, one account: pasting it, or proving it from an
    extension, is refused while another account holds it, and the holder keeps
    it (`nostr.md` § Errors & edge cases → *One pubkey, one account*)."""
    nest = nostr_home_nest
    port = nest["port"]
    secret = os.urandom(32)
    keys = fauna_ffi.nostr_keys(secret)
    holder, other = _user(nest), _user(nest)

    held = ws_api.bridge_link(port, holder, "nostr", "import", {"nsec": keys["nsec"]})
    assert held["identity"]["value"] == keys["npub"], held

    _refused("fauna.bridges.identity_in_use", lambda: ws_api.bridge_link(
        port, other, "nostr", "import", {"nsec": keys["nsec"]},
    ))
    challenge = ws_api.bridge_link_challenge(port, other, "nostr", "nip07")
    proof = fauna_ffi.nostr_sign_event(secret, challenge["payload"])
    _refused("fauna.bridges.identity_in_use", lambda: ws_api.bridge_link(
        port, other, "nostr", "nip07",
        {"pubkey": keys["pubkey"], "proof_json": json.dumps(proof)},
    ))

    assert not ws_api.bridge_status(port, other, "nostr")["linked"]
    kept = ws_api.bridge_status(port, holder, "nostr")
    assert kept["linked"] and kept["identity"]["value"] == keys["npub"], (
        f"the holder keeps the key: {kept!r}"
    )


@pytest.mark.feature("nostr")
def test_unlinking_disconnects_every_signer_app(nostr_dialing_nest, nostr_signer_nest):
    """Outcome 22 — unlinking revokes every app using your nest as its NIP-46
    signer, connected or still pending: the roster empties, and an app holding
    an invite gets no answer from the bunker (`nostr.md` § The nest as the
    user's NIP-46 signer (bunker) → *Connection model*)."""
    signer_user = _user(nostr_signer_nest)
    _link_generated(nostr_signer_nest, signer_user)
    connecting, pending = (
        _bunker_invite(nostr_signer_nest, signer_user),
        _bunker_invite(nostr_signer_nest, signer_user),
    )

    # An app connects through the first invite: here, a home-nest user's
    # remote-signer link, whose handshake IS a NIP-46 client's.
    app_user = _user(nostr_dialing_nest)
    ws_api.bridge_link(nostr_dialing_nest["port"], app_user, "nostr", "remote",
                       {"bunker_url": connecting})
    statuses = sorted(a["status"] for a in _bunker_apps(nostr_signer_nest, signer_user))
    assert statuses == ["active", "pending"], f"one connected app, one invite out: {statuses!r}"

    ws_api.bridge_unlink(nostr_signer_nest["port"], signer_user, "nostr")
    assert _bunker_apps(nostr_signer_nest, signer_user) == [], (
        "unlinking leaves no connected app and no open invite"
    )

    late_app = _user(nostr_dialing_nest)
    _refused("fauna.bridges.proof_required", lambda: ws_api.bridge_link(
        nostr_dialing_nest["port"], late_app, "nostr", "remote", {"bunker_url": pending},
    ))
