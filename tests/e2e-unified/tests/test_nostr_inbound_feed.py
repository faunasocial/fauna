"""Posts from someone you follow on Nostr arrive in your feed.

`docs/goal/ui/nostr.md` § Implementation status today (the sync worker's
inbound sweep subscribes, per account with ``nostr-inbound-to-feed`` on and a
deposited key, to each followed author's notes and stores them in the feed)
and § The bridging gate (the sweep runs only for a depositor). The nest-side
half — the sweep, authorization at ingest, deletion — is witnessed over the
wire in ``tests/api/test_nostr_nest.py``; this is the journey through the
lead app: link, turn the setting on, follow, and the note is in the feed.

**The far end.** A :class:`~helpers.nostr_relay.FakeRelay` on loopback stands
in for the public relay network. A follow added in the app carries no relay
hints, so the nest subscribes at its built-in default relays
(`relays.rs::resolve_relay_hints`); the ``test-hooks``-only
``FAUNA_TEST_NOSTR_DEFAULT_RELAYS`` seam names the fake in their place, and
``FAUNA_TEST_NOSTR_ALLOW_LOOPBACK`` lets the dial reach it — the shipping
nest's code path with only the network canned (the ``FAUNA_TEST_ATPROTO_FAR_END``
precedent).

**What the harness arranges, and why that is not a shortcut (e2e rule 8b).**
The far author's signed note is seeded on the fake — the author is not a
Fauna user, and no app owns their relay. The sweep is poked through
``POST /api/v1/test/nostr/inbound/poll-now`` because the worker's own cadence
is a minute with no wake for a new follow (convention 14); the test polls for
the post, re-poking as it goes. Every act of the user's — the link, the
setting, the follow — is the app's.

tier_3: a locally-built nest and the real app — tui, the lead app; the Nostr
network is a fake.
"""

from __future__ import annotations

import os
import secrets
import time

import pytest
import requests

import fauna_ffi
from clients.ws_rpc_admin_client import WsRpcAdminClient
from helpers import nostr_relay
from helpers.nostr_relay import FakeRelay
from helpers.waiting import wait_until

pytestmark = [pytest.mark.tier_3]

#: One poked sweep: the nudge, the worker's REQ to the fake, the fake's answer,
#: the drain and the store — re-poked every pass, so a note the relay answers
#: after one drain's short window is collected on the next.
INBOUND_POKED_S = 60.0

#: A painted-state re-read after a local gesture.
PAINT_S = 30.0

INBOUND_TOGGLE = "nostr-inbound-to-feed"


@pytest.fixture
def far_relay():
    relay = FakeRelay()
    yield relay
    relay.close()


@pytest.fixture
def nostr_inbound_nest(nest_binary, tmp_path_factory, far_relay):
    """A locally built nest whose default relays are :func:`far_relay` and
    which may dial it on loopback, with one registered user."""
    from conftest import _make_nest, _make_user

    nest, cleanup = _make_nest(
        nest_binary, tmp_path_factory, "nostr-inbound-feed",
        extra_env={
            **nostr_relay.ALLOW_LOOPBACK_ENV,
            "FAUNA_TEST_NOSTR_DEFAULT_RELAYS": far_relay.url,
        },
    )
    nest["user"] = _make_user(nest)
    yield nest
    cleanup()


def _poke(nest) -> None:
    resp = requests.post(
        f"{nest['url']}/api/v1/test/nostr/inbound/poll-now", timeout=30, verify=False,
    )
    assert resp.status_code == 200, f"nostr poll-now: {resp.status_code} {resp.text}"


def _local_nostr_posts(nest, text: str) -> list[dict]:
    user = nest["user"]
    with WsRpcAdminClient(
        nest["url"],
        actor_id=bytes.fromhex(user["actor_id_hex"]),
        signing_key=bytes(user["signing_key"]),
    ) as client:
        local = client.call("fauna.feed.local.posts", {"limit": 50})
    return [
        p for p in local.get("posts", [])
        if p.get("source") == "nostr" and p.get("body") == text
    ]


@pytest.mark.tui
@pytest.mark.standalone_only
@pytest.mark.feature("nostr")
def test_posts_from_someone_you_follow_on_nostr_arrive_in_your_feed(
    request, app, nostr_inbound_nest, far_relay,
):
    from conftest import _login_app_as

    nest, relay = nostr_inbound_nest, far_relay

    author_secret = os.urandom(32)
    author = fauna_ffi.nostr_keys(author_secret)
    text = f"a note from someone you follow on nostr {secrets.token_hex(4)}"
    relay.seed(fauna_ffi.nostr_sign_event(author_secret, {
        "kind": 1, "created_at": int(time.time()) - 60, "tags": [], "content": text,
    }))

    _login_app_as(app, request, nest, nest["user"], verify_live_actor=True)
    app.nostr.navigate()
    assert app.nostr.is_page_visible(), (
        f"the Nostr page should be reachable; error={app.error_text()!r}"
    )
    app.nostr.link_generate()
    assert app.nostr.wait_for_linked(), (
        f"generate-link should link the account; error={app.nostr.page_error_text()!r}"
    )
    app.nostr.set_toggle(INBOUND_TOGGLE, True)
    assert app.nostr.wait_for_toggle_state(INBOUND_TOGGLE, "on"), (
        f"{INBOUND_TOGGLE} should read on after the tap; it reads "
        f"{app.nostr.toggle_state(INBOUND_TOGGLE)!r}; error={app.nostr.page_error_text()!r}"
    )
    app.nostr.add_follow(author["npub"])
    assert app.nostr.wait_for_follow_count(1), (
        f"the follow row should appear; error={app.nostr.page_error_text()!r}"
    )

    def swept():
        _poke(nest)
        return _local_nostr_posts(nest, text)

    wire = wait_until(
        swept, INBOUND_POKED_S, interval=1.0,
        diagnose=lambda: (
            f"the followed author's note never reached fauna.feed.local.posts; the "
            f"fake relay saw REQs {relay.requests!r}"
        ),
    )
    assert len(wire) == 1, f"the note arrives exactly once; got {wire!r}"
    post_id = wire[0]["post_id"]

    app.feed.navigate()

    def card():
        i = app.feed.post_index_by_id(post_id)
        return i + 1 if i >= 0 else 0  # truthy for index 0 too

    index = wait_until(
        card, PAINT_S,
        diagnose=lambda: (
            f"the swept note {post_id} is on the wire but not in the app's feed; "
            f"app feed ids={[p.get('post_id') for p in app.feed._feed_posts_from_state()]!r}; "
            f"error={app.error_text()!r}"
        ),
    ) - 1
    card_text = app.feed.post_text(index).strip()
    assert card_text == text, (
        f"the followed author's note shows its text {text!r}; got {card_text!r}; "
        f"error={app.error_text()!r}"
    )
