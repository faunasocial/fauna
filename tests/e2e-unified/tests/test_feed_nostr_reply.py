"""Cross-app Feed — replying to and quoting a Nostr post, through the app.

``docs/goal/ui/nostr.md`` § Replying to and quoting a nostr note: tapping
``feed-reply-button`` / ``feed-quote-button`` on a swept Nostr note composes
the user's own signed reply or quote, and the nest derives it into a kind-1
event on the user's relays — a reply threaded under the note (NIP-10 marked
``e`` root, the author in a ``p`` tag), a quote carrying a ``q`` tag and a
``nostr:`` link. ``docs/goal/ui/feed.md`` § Interaction bar → *Reply and quote
on a bridged post* routes both through the same eligibility door; § Errors &
edge cases of ``nostr.md`` is the refusal: a reply or quote the user's Nostr
settings cannot publish is refused inline, naming what to change on the Nostr
page, and nothing is composed.

The nest half of the same chain is witnessed without an app by
``tests/api/test_nostr_nest.py::test_your_reply_threads_under_a_nostr_post_and_your_quote_links_it``;
this file is the app-surface half — the user's gesture on the rendered card is
what composes — and the fediverse twin is ``test_feed_fediverse_reply.py``.

**The far end.** Two :class:`~helpers.nostr_relay.FakeRelay` on loopback: the
author's relay, seeded with the far author's signed note and named as the
follow's relay hint; and the user's publish relay, the oracle for what reached
Nostr. ``FAUNA_TEST_NOSTR_ALLOW_LOOPBACK`` lets the nest dial both — a nest
this harness spawns, hence ``standalone_only``.

**What the harness arranges, and why that is not a shortcut (e2e rule 8b).**
Setup — the generated key, the relay list, the follow — goes through the
nest's own doors, as the Nostr page sends them (its journey is witnessed by
``test_nostr_inbound_feed.py``); the far author is not a Fauna user. The sweep
is poked through ``POST /api/v1/test/nostr/inbound/poll-now`` (the worker's
cadence has no wake for a new follow; convention 14). The journey under test —
reply, quote, the refusal — is driven only through the app UI (convention 8).
"""

from __future__ import annotations

import json
import os
import secrets
import time

import pytest
import requests

import fauna_ffi
from helpers import nostr_relay
from helpers.budgets import RPC_ROUNDTRIP_S
from helpers.nostr_relay import FakeRelay, await_received, tag_values
from helpers.waiting import wait_until
from tests.api import ws_api

pytestmark = [pytest.mark.tier_3, pytest.mark.standalone_only]

#: One poked sweep: the nudge, the worker's REQ to the author's relay, its
#: answer, the drain and the store — re-poked every pass.
INBOUND_POKED_S = 60.0

#: The Nostr page, as every one of the door's refusals names it
#: (``i18n/strings/en.yaml`` § ``nostr``).
NOSTR_PAGE = "nostr page"


@pytest.fixture(scope="module")
def nostr_reply_nest(nest_binary, tmp_path_factory):
    """A locally built nest allowed to dial loopback relays. Each test
    registers its own user, author and relay, so no test reads another's
    state."""
    from conftest import _make_nest

    nest, cleanup = _make_nest(
        nest_binary, tmp_path_factory, "nostr-reply",
        extra_env=dict(nostr_relay.ALLOW_LOOPBACK_ENV),
    )
    yield nest
    cleanup()


@pytest.fixture
def nostr_reply_spa_url(static_dir, nostr_reply_nest):
    """Web-only SPA proxy onto ``nostr_reply_nest`` (the fediverse twin's
    ``fediverse_spa_url``)."""
    from conftest import _serve_spa_proxy
    url, server = _serve_spa_proxy(static_dir, nostr_reply_nest["url"])
    yield url
    server.shutdown()


@pytest.fixture
def publish_relay():
    """The user's own relay: it records what the nest publishes for them."""
    relay = FakeRelay()
    yield relay
    relay.close()


@pytest.fixture
def author_relay():
    """The far author's relay, holding the note the user answers."""
    relay = FakeRelay()
    yield relay
    relay.close()


def _text(label: str) -> str:
    return f"{label} {secrets.token_hex(4)}"


def _poke(nest) -> None:
    resp = requests.post(
        f"{nest['url']}/api/v1/test/nostr/inbound/poll-now", timeout=30, verify=False,
    )
    assert resp.status_code == 200, f"nostr poll-now: {resp.status_code} {resp.text}"


def _following_a_far_author(nest, publish_relay, author_relay) -> tuple[dict, dict, dict]:
    """A user with a generated Nostr key publishing to ``publish_relay``,
    following a far author whose note sits on ``author_relay`` and has been
    swept into the user's feed. Returns ``(user, author, note)``."""
    from conftest import _make_user

    user = _make_user(nest)
    reply = ws_api.bridge_link(nest["port"], user, "nostr", "generate")
    assert reply["linked"], reply
    user["nostr_pubkey"] = fauna_ffi.nostr_npub_to_hex(reply["identity"]["value"])
    ws_api.bridge_set_settings(
        nest["port"], user, "nostr", {"relay_list": json.dumps([publish_relay.url])},
    )

    author_secret = os.urandom(32)
    author = fauna_ffi.nostr_keys(author_secret)
    note = fauna_ffi.nostr_sign_event(author_secret, {
        "kind": 1, "created_at": int(time.time()) - 60, "tags": [],
        "content": _text("a far author's note to answer"),
    })
    author_relay.seed(note)
    ws_api.bridge_add_follow(
        nest["port"], user, "nostr", author["npub"],
        extra={"relay_hints": [author_relay.url]},
    )

    def swept():
        _poke(nest)
        return [
            p for p in ws_api.local_feed_posts(nest["port"], user, limit=50)
            if p.get("source") == "nostr" and p.get("body") == note["content"]
        ]

    wait_until(
        swept, INBOUND_POKED_S, interval=1.0,
        diagnose=lambda: (
            f"the followed author's note never reached the user's feed; the "
            f"author's relay saw REQs {author_relay.requests!r}"
        ),
    )
    return user, author, note


def _signed_in_on_the_note(app, request, nest, user, note_text: str) -> int:
    """Log ``app`` in as ``user`` and return the index of the card carrying
    the swept note."""
    from conftest import _login_app_as
    _login_app_as(
        app, request, nest, user,
        spa_url_fixture="nostr_reply_spa_url", verify_live_actor=True,
    )
    assert app.feed.wait_for_post_text(note_text, timeout_s=30.0), (
        f"the swept nostr note {note_text!r} never rendered in the feed; "
        f"error={app.error_text()!r}"
    )
    index = app.feed.post_index_by_text(note_text)
    assert index >= 0, f"no post-card carries {note_text!r}"
    return index


def _published(relay, user, predicate, what) -> dict:
    return await_received(
        relay,
        lambda e: e["pubkey"] == user["nostr_pubkey"] and e["kind"] == 1 and predicate(e),
        RPC_ROUNDTRIP_S,
        what,
    )


@pytest.mark.tui
@pytest.mark.web
@pytest.mark.linux
@pytest.mark.android
@pytest.mark.feature("nostr")
def test_a_reply_to_a_nostr_post_reaches_nostr_threaded_under_it(
    request, app, nostr_reply_nest, publish_relay, author_relay,
):
    """``feed-reply-button`` on a swept Nostr note, text typed and submitted →
    the user's relay receives a signed kind-1 carrying the typed text, its
    NIP-10 root the note and its ``p`` tag the author, and the note's reply
    count moves."""
    nest = nostr_reply_nest
    user, author, note = _following_a_far_author(nest, publish_relay, author_relay)
    index = _signed_in_on_the_note(app, request, nest, user, note["content"])

    reply_text = _text("my reply from the feed")
    app.feed.reply_post(reply_text, index)

    reply = _published(
        publish_relay, user, lambda e: reply_text in e["content"], f"the reply {reply_text!r}",
    )
    assert fauna_ffi.nostr_verify_event(reply), reply
    assert ["e", note["id"], "", "root"] in reply["tags"], (
        f"the reply names the note as its thread root (NIP-10 marked): {reply['tags']!r}"
    )
    assert [author["pubkey"]] in tag_values(reply, "p"), (
        f"the reply tags the note's author: {reply['tags']!r}"
    )

    after = app.feed.wait_for_interaction_count(note["content"], "reply", 1)
    assert after == 1, (
        f"replying to the nostr note must move its reply count 0 → 1; "
        f"got {after!r}. error={app.error_text()!r}"
    )


@pytest.mark.tui
@pytest.mark.web
@pytest.mark.linux
@pytest.mark.android
@pytest.mark.feature("nostr")
def test_a_quote_of_a_nostr_post_reaches_nostr_linking_it(
    request, app, nostr_reply_nest, publish_relay, author_relay,
):
    """``feed-quote-button`` on a swept Nostr note → the user's relay receives
    a signed kind-1 naming the note in a ``q`` tag and linking it with a
    ``nostr:nevent1`` reference, threaded under nothing, and the note's quote
    count moves."""
    nest = nostr_reply_nest
    user, _author, note = _following_a_far_author(nest, publish_relay, author_relay)
    index = _signed_in_on_the_note(app, request, nest, user, note["content"])

    app.feed.quote_post(index)

    quote = _published(
        publish_relay, user,
        lambda e: any(v and v[0] == note["id"] for v in tag_values(e, "q")),
        f"a quote naming {note['id'][:16]}",
    )
    assert fauna_ffi.nostr_verify_event(quote), quote
    assert "nostr:nevent1" in quote["content"], (
        f"the quote links the note for every client to embed: {quote['content']!r}"
    )
    assert not [t for t in quote["tags"] if t[:1] == ["e"] and "root" in t], (
        f"a quote is not a reply — no NIP-10 root: {quote['tags']!r}"
    )

    after = app.feed.wait_for_interaction_count(note["content"], "quote", 1)
    assert after == 1, (
        f"quoting the nostr note must move its quote count 0 → 1; "
        f"got {after!r}. error={app.error_text()!r}"
    )


@pytest.mark.tui
@pytest.mark.web
@pytest.mark.linux
@pytest.mark.android
@pytest.mark.feature("nostr")
@pytest.mark.parametrize("kind", ["reply", "quote"])
def test_replying_or_quoting_with_no_relays_is_refused_naming_the_nostr_page(
    request, app, nostr_reply_nest, publish_relay, author_relay, kind,
):
    """With every relay removed from the user's list after the note arrived,
    the reply / quote gesture renders the refusal inline — naming the Nostr
    page, where a relay is added back — and composes nothing: no post on the
    nest, nothing on any relay, the note's count unmoved (``nostr.md``
    § Errors & edge cases; the door's ``fauna.nostr.no_relays_configured``).
    An emptied list is the one settings-reachable remedy refusing both arms —
    *Publish replies* off refuses a reply only."""
    nest = nostr_reply_nest
    user, _author, note = _following_a_far_author(nest, publish_relay, author_relay)
    ws_api.bridge_set_settings(nest["port"], user, "nostr", {"relay_list": json.dumps([])})

    index = _signed_in_on_the_note(app, request, nest, user, note["content"])
    if kind == "reply":
        app.feed.reply_post(_text("a refused reply"), index)
    else:
        app.feed.quote_post(index)

    error = wait_until(
        lambda: app.error_text(), 30.0, interval=0.2,
        diagnose=lambda: f"the refused {kind} never rendered an inline error",
    )
    assert NOSTR_PAGE in error.lower(), (
        f"the {kind} of a nostr post with no relays must refuse inline, naming "
        f"the Nostr page; error-message read {error!r}"
    )

    # The refusal precedes composing (`FeedManager` sends the eligibility ack
    # first and stops on its error), so once it renders the outcome is
    # settled: nothing was created and nothing was published.
    own = [
        p for p in ws_api.local_feed_posts(nest["port"], user, limit=100)
        if p.get("author") == user["actor_id_hex"]
    ]
    assert own == [], f"a refused {kind} still created posts: {own}"
    # Kind 1 only: setting the relay list itself publishes the user's NIP-65
    # relay list (kind 10002) to it, before the list was emptied.
    assert publish_relay.received_by(user["nostr_pubkey"], 1) == [], (
        f"a refused {kind} still reached a relay"
    )
    counts = app.feed.post_interaction_counts_by_text(note["content"]) or {}
    assert counts.get(kind) == 0, (
        f"a refused {kind} moved the note's {kind} count: {counts}"
    )
