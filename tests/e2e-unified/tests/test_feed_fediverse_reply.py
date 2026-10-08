"""Cross-app Feed — replying to and quoting a fediverse post, through the app.

``docs/goal/ui/feed.md`` § Interaction bar → *Reply and quote on a bridged
post*: tapping ``feed-reply-button`` / ``feed-quote-button`` on an
ActivityPub-sourced post composes the user's own signed reply or quote, and
the nest derives it into the fediverse form and delivers it to the post's
author (``docs/goal/behavior/activitypub.md`` § Reply and quote). When the
user's federation is switched off, the tap refuses inline with the reason
and composes nothing.

The nest half of the same chain is witnessed without an app by
``tests/api/test_activitypub_federation.py::TestActivityPubFollowerDelivery::
test_a_reply_or_quote_of_an_ingested_note_reaches_its_author``; this file is
the app-surface half — the user's gesture on the rendered card is what
composes, and the in-test follower's inbox (``helpers/fake_follower.py``) is
the oracle for what reached the fediverse.

The nest is a dedicated AP build (``activitypub,test-hooks``) started by
``helpers.ap_nest.start_ap_nest``: delivery to the loopback follower needs the
``FAUNA_TEST_AP_ALLOW_LOOPBACK`` test hook, which only a nest this harness
spawns can carry — hence ``standalone_only``.

Setup (enabling federation, the follower's Follow and its note) goes through
the nest's own doors, as a remote server and the Bridges page would; the
journey under test — reply, quote, the refusal — is driven only through the
app UI (convention 8).
"""

from __future__ import annotations

import json
import time
import urllib.request
import uuid

import pytest

from drivers.port_util import find_free_port
from helpers.ap_nest import (
    AP_BRIDGE_ID,
    ap_post_map_row,
    delivery_jobs_for,
    enable_ap,
    start_ap_nest,
)
from helpers.fake_follower import FakeFollower, assert_valid_http_signature
from tests.api import ws_api

pytestmark = [pytest.mark.tier_3, pytest.mark.standalone_only]


def _unique(prefix: str) -> str:
    return f"{prefix}-{uuid.uuid4().hex[:10]}"


@pytest.fixture(scope="module")
def ap_binary():
    """fauna-nest with ``activitypub`` + ``test-hooks`` — the federation
    suite's build, replayed from the collection-time prebuild through
    conftest's memoized ensure layer rather than paid inside a test."""
    from conftest import _ensure_ap_nest_built
    return _ensure_ap_nest_built()


@pytest.fixture(scope="module")
def fediverse_nest(ap_binary, tmp_path_factory):
    """One AP-enabled nest for the module, its domain its own loopback
    authority (the federation suite's shape). Each test registers its own
    user and follower on it, so no test reads another's state."""
    port = find_free_port()
    nest = start_ap_nest(
        ap_binary,
        tmp_path_factory.mktemp("fediverse-reply-nest"),
        port,
        f"127.0.0.1:{port}",
    )
    try:
        yield nest
    finally:
        nest["proc"].kill()
        nest["proc"].wait()


@pytest.fixture
def fediverse_spa_url(static_dir, fediverse_nest):
    """Web-only SPA proxy onto ``fediverse_nest`` — the session ``spa_url``
    proxies only the shared nest (mirrors ``bridges_spa_url``)."""
    from conftest import _serve_spa_proxy
    url, server = _serve_spa_proxy(static_dir, fediverse_nest["url"])
    yield url
    server.shutdown()


@pytest.fixture
def follower():
    """A remote fediverse server on a free loopback port, playing the author
    of the note the user replies to."""
    with FakeFollower(find_free_port()) as peer:
        yield peer


def _account_public_key(nest, username: str) -> str:
    """The RSA public key the nest publishes for the user's AP account."""
    req = urllib.request.Request(
        f"{nest['url']}/ap/users/{username}",
        headers={"Accept": "application/activity+json"},
    )
    with urllib.request.urlopen(req) as resp:
        return json.loads(resp.read())["publicKey"]["publicKeyPem"]


def _deliver_note(nest, peer, username: str, actor_url: str, text: str) -> str:
    """The peer follows the user (auto-accepted) and sends it a signed
    ``Create{Note}`` on the user's own inbox — the relationship gate's
    *addressed* arm ingests it into the user's feed. Returns the note's id."""
    code = peer.post_signed(
        f"{nest['url']}/ap/users/{username}/inbox",
        {
            "@context": "https://www.w3.org/ns/activitystreams",
            "type": "Follow",
            "id": f"{peer.actor_uri}/activities/follow-1",
            "actor": peer.actor_uri,
            "object": actor_url,
        },
    )
    assert code == 202, f"inbox did not accept the Follow: HTTP {code}"
    peer.wait_for("Accept")

    note_id = f"{peer.actor_uri}/statuses/{_unique('note')}"
    code = peer.post_signed(
        f"{nest['url']}/ap/users/{username}/inbox",
        {
            "@context": "https://www.w3.org/ns/activitystreams",
            "type": "Create",
            "id": f"{note_id}/activity",
            "actor": peer.actor_uri,
            "to": ["https://www.w3.org/ns/activitystreams#Public"],
            "object": {
                "type": "Note",
                "id": note_id,
                "attributedTo": peer.actor_uri,
                "content": f"<p>{text}</p>",
                # Now, never a fixed date: the inbox drops a note published
                # past its future bound, and a fixed date ages the feed order.
                "published": time.strftime("%Y-%m-%dT%H:%M:%SZ", time.gmtime()),
                "to": ["https://www.w3.org/ns/activitystreams#Public"],
            },
        },
    )
    assert code == 202, f"inbox did not accept the Create: HTTP {code}"
    assert ap_post_map_row(nest, note_id) is not None, (
        f"the peer's note {note_id!r} was accepted (202) but never ingested — "
        "a rejected activity answers the peer, not us; read the nest log"
    )
    return note_id


def _signed_in_on_the_note(app, request, nest, user, note_text: str) -> int:
    """Log ``app`` in as ``user`` on the AP nest and return the index of the
    card carrying the ingested note."""
    from conftest import _login_app_as
    _login_app_as(
        app, request, nest, user,
        spa_url_fixture="fediverse_spa_url", verify_live_actor=True,
    )
    assert app.feed.wait_for_post_text(note_text, timeout_s=30.0), (
        f"the ingested fediverse note {note_text!r} never rendered in the feed; "
        f"error={app.error_text()!r}"
    )
    index = app.feed.post_index_by_text(note_text)
    assert index >= 0, f"no post-card carries {note_text!r}"
    return index


def _federating_user(nest):
    from conftest import _make_user
    user = _make_user(nest)
    username, actor_url = enable_ap(nest, user)
    return user, username, actor_url


@pytest.mark.tui
@pytest.mark.feature("fediverse")
def test_a_reply_to_a_fediverse_post_reaches_its_author(
    request, app, fediverse_nest, follower
):
    """``feed-reply-button`` on a fediverse post, text typed and submitted →
    the author's inbox receives the user's signed ``Create`` threaded under
    the note (``inReplyTo`` = the note, the author in ``to``), the reply's
    body lands in the user's own feed, and the note's reply count moves."""
    nest = fediverse_nest
    user, username, actor_url = _federating_user(nest)
    note_text = _unique("fedi-note-reply")
    note_id = _deliver_note(nest, follower, username, actor_url, note_text)
    index = _signed_in_on_the_note(app, request, nest, user, note_text)

    reply_text = _unique("fedi-reply-body")
    app.feed.reply_post(reply_text, index)

    entry = follower.wait_for(
        "Create", where=lambda a: a.get("object", {}).get("inReplyTo") == note_id
    )
    assert_valid_http_signature(
        entry,
        _account_public_key(nest, username),
        f"/users/{follower.username}/inbox",
    )
    note = entry["activity"]["object"]
    assert reply_text in note["content"], (
        f"the delivered reply does not carry the typed text {reply_text!r}: "
        f"{note['content']!r}"
    )
    assert follower.actor_uri in note["to"], note["to"]

    after = app.feed.wait_for_interaction_count(note_text, "reply", 1)
    assert after == 1, (
        f"replying to the fediverse note must move its reply count 0 → 1; "
        f"got {after!r}. error={app.error_text()!r}"
    )
    # Composing does not reload the window; a throwaway post does (the
    # native reply test's pattern).
    app.feed.create_post(text=_unique("fedi-reply-reload-marker"))
    assert app.feed.wait_for_post_text(reply_text), (
        f"the reply's body {reply_text!r} never appeared in the feed after a "
        f"reload. error={app.error_text()!r}"
    )


@pytest.mark.tui
@pytest.mark.feature("fediverse")
def test_a_quote_of_a_fediverse_post_reaches_its_author(
    request, app, fediverse_nest, follower
):
    """``feed-quote-button`` on a fediverse post → the author's inbox receives
    the user's signed ``Create`` carrying the quote in every spelling the
    fediverse reads (the three quote keys, the FEP-e232 ``Link`` tag, the
    ``RE:`` anchor in the content), and the note's quote count moves."""
    nest = fediverse_nest
    user, username, actor_url = _federating_user(nest)
    note_text = _unique("fedi-note-quote")
    note_id = _deliver_note(nest, follower, username, actor_url, note_text)
    index = _signed_in_on_the_note(app, request, nest, user, note_text)

    app.feed.quote_post(index)

    entry = follower.wait_for(
        "Create", where=lambda a: a.get("object", {}).get("quote") == note_id
    )
    assert_valid_http_signature(
        entry,
        _account_public_key(nest, username),
        f"/users/{follower.username}/inbox",
    )
    note = entry["activity"]["object"]
    for key in ("quote", "quoteUri", "_misskey_quote"):
        assert note.get(key) == note_id, (key, note)
    links = [t for t in note.get("tag", []) if t.get("type") == "Link"]
    assert [link["href"] for link in links] == [note_id], note
    assert f'RE: <a href="{note_id}">' in note["content"], note["content"]
    assert "inReplyTo" not in note, note

    after = app.feed.wait_for_interaction_count(note_text, "quote", 1)
    assert after == 1, (
        f"quoting the fediverse note must move its quote count 0 → 1; "
        f"got {after!r}. error={app.error_text()!r}"
    )


@pytest.mark.tui
@pytest.mark.feature("fediverse")
@pytest.mark.parametrize("kind", ["reply", "quote"])
def test_replying_or_quoting_with_federation_off_is_refused_inline(
    request, app, fediverse_nest, follower, kind
):
    """With the user's federation switched off after the note arrived, the
    reply / quote gesture renders the refusal inline — naming the Bridges
    page — and composes nothing: no post on the nest, no delivery to the
    author, the note's count unmoved (``feed.md`` § Interaction bar → the
    affordance's inline failure; the nest's ``400`` from the eligibility
    door)."""
    nest = fediverse_nest
    user, username, actor_url = _federating_user(nest)
    note_text = _unique(f"fedi-note-off-{kind}")
    _deliver_note(nest, follower, username, actor_url, note_text)
    ws_api.bridge_set_settings(nest["port"], user, AP_BRIDGE_ID, {"enabled": False})
    jobs_before = delivery_jobs_for(nest, follower.inbox_url)

    index = _signed_in_on_the_note(app, request, nest, user, note_text)
    reply_text = _unique("fedi-refused-body")
    if kind == "reply":
        app.feed.reply_post(reply_text, index)
    else:
        app.feed.quote_post(index)

    # A deadline poll on the refusal landing, never a settle-sleep (14).
    error = ""
    deadline = time.monotonic() + 30.0
    while time.monotonic() < deadline:
        error = app.error_text()
        if error:
            break
        time.sleep(0.2)
    assert "bridges" in error.lower(), (
        f"the {kind} of a fediverse post with federation off must refuse "
        f"inline, naming the Bridges page; error-message read {error!r}"
    )

    # The refusal precedes composing (`FeedManager` sends the eligibility
    # ack first and stops on its error), so once it renders the outcome is
    # settled: nothing was created and nothing was queued for the author.
    own = [
        p for p in ws_api.local_feed_posts(nest["port"], user, limit=100)
        if p.get("author") == user["actor_id_hex"]
    ]
    assert own == [], f"a refused {kind} still created posts: {own}"
    assert delivery_jobs_for(nest, follower.inbox_url) == jobs_before, (
        f"a refused {kind} still queued a delivery to the author"
    )
    counts = app.feed.post_interaction_counts_by_text(note_text) or {}
    assert counts.get(kind) == 0, (
        f"a refused {kind} moved the note's {kind} count: {counts}"
    )
