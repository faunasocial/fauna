"""Cross-app Feed — advertising a sold post on Nostr, through the app.

``docs/goal/behavior/monetization.md`` § The Nostr advertisement of a sold post
→ *What the author sees and agrees to*: on an own **sold** post (a per-post
unlock tier with an asking price) and a nest that advertises the
``nostr-advertise`` capability, the feed's ⋯ overflow offers *Advertise on
Nostr*; its confirm step states the consequence, and confirming publishes the
post's teaser as a kind-1 note carrying the tier's price (``["price", value,
unit]``) to the account's publish relays. *Stop advertising on Nostr* then
retracts it with a kind 5 naming the note. The nest half (the act, its
refusals, the lifecycle) is pinned without an app by
``bins/fauna-nest/tests/conformance_nostr_advertise.rs``; this file is the
app-surface half — the author's gestures are what publish and withdraw.

**The far end.** A :class:`~helpers.nostr_relay.FakeRelay` on loopback is the
user's publish relay, the oracle for what reached Nostr.
``FAUNA_TEST_NOSTR_ALLOW_LOOPBACK`` lets the nest dial it — a nest this harness
spawns, hence ``standalone_only``.

**What the harness arranges (e2e rule 8b).** The generated Nostr key and the
relay list go through the nest's own doors, as the Nostr page sends them (that
journey is witnessed by ``test_nostr_inbound_feed.py``). The journey under test
— selling the post with its asking price, advertising it, stopping — is driven
only through the app UI (convention 8).

**The confirm step's sentence is a public text** (it states what the act
guarantees), so it reaches the string catalog only through its public-texts
round, and the apps offer no *Advertise* verb while the catalog lacks it. Until
it lands this test skips on that one static condition — not an app gate: the
moment ``nostr_advertise.confirm_body`` is in the catalog, it runs.
"""

from __future__ import annotations

import json
import uuid

import pytest

import fauna_ffi
from helpers import nostr_relay
from helpers.budgets import RPC_ROUNDTRIP_S
from helpers.nostr_relay import FakeRelay, await_received, tag_values
from i18n.strings import S
from tests.api import ws_api

pytestmark = [pytest.mark.tier_3, pytest.mark.standalone_only]

PREVIEW = "A teaser anyone on Nostr may read."
FULL_BODY = "The sold remainder, for buyers only."
PRICE = "21 sats"
ASKING_SATS = 21

#: The public-text gate (module docstring).
CONFIRM_SENTENCE_LANDED = hasattr(getattr(S, "nostr_advertise", None), "confirm_body")


@pytest.fixture(scope="module")
def advertise_nest(nest_binary, tmp_path_factory):
    """A locally built nest allowed to dial loopback relays; each test
    registers its own user and relay."""
    from conftest import _make_nest

    nest, cleanup = _make_nest(
        nest_binary, tmp_path_factory, "nostr-advertise",
        extra_env=dict(nostr_relay.ALLOW_LOOPBACK_ENV),
    )
    yield nest
    cleanup()


@pytest.fixture
def advertise_spa_url(static_dir, advertise_nest):
    """Web-only SPA proxy onto ``advertise_nest`` (the blob upload a sale
    makes needs it); requested lazily, so the native lead never builds web."""
    from conftest import _serve_spa_proxy
    url, server = _serve_spa_proxy(static_dir, advertise_nest["url"])
    yield url
    server.shutdown()


@pytest.fixture
def publish_relay():
    relay = FakeRelay()
    yield relay
    relay.close()


@pytest.mark.tui
@pytest.mark.web
@pytest.mark.linux
@pytest.mark.macos
@pytest.mark.ios
@pytest.mark.android
@pytest.mark.windows
@pytest.mark.feature("nostr")
@pytest.mark.requires_surface("feed-post-advertise-nostr")
@pytest.mark.skipif(
    not CONFIRM_SENTENCE_LANDED,
    reason="the confirm step's sentence (nostr_advertise.confirm_body) is a public "
    "text still in its public-texts round; no app offers the verb until it lands",
)
def test_an_advertised_sold_post_reaches_nostr_with_its_price_and_stopping_retracts_it(
    request, app, advertise_nest, publish_relay,
):
    """Sell a post with an asking price → *Advertise on Nostr* → confirm → the
    publish relay receives a kind-1 carrying the teaser and ``["price", "21000",
    "msat"]``; *Stop advertising* → the relay receives a kind 5 naming it."""
    from conftest import _login_app_as, _make_user

    nest = advertise_nest
    user = _make_user(nest)
    reply = ws_api.bridge_link(nest["port"], user, "nostr", "generate")
    assert reply["linked"], reply
    pubkey = fauna_ffi.nostr_npub_to_hex(reply["identity"]["value"])
    ws_api.bridge_set_settings(
        nest["port"], user, "nostr", {"relay_list": json.dumps([publish_relay.url])},
    )
    _login_app_as(
        app, request, nest, user,
        spa_url_fixture="advertise_spa_url", verify_live_actor=True,
    )

    feed = app.feed
    feed.sell_post(
        f"{FULL_BODY} {uuid.uuid4().hex[:8]}", PREVIEW, PRICE,
        asking_price_sats=ASKING_SATS,
    )
    assert feed.wait_for_first_post_text(PREVIEW.split()[0]), (
        f"the sold post never rendered; error-message={app.error_text()!r}"
    )

    feed.advertise_post_on_nostr(0)
    note = await_received(
        publish_relay,
        lambda e: e["pubkey"] == pubkey and e["kind"] == 1 and PREVIEW in e["content"],
        RPC_ROUNDTRIP_S,
        "the advertised teaser as a kind-1 note on the publish relay",
    )
    prices = [t for t in note["tags"] if t and t[0] == "price"]
    assert prices == [["price", str(ASKING_SATS * 1000), "msat"]], (
        f"the note must carry the tier's asking price, got tags {note['tags']!r}; "
        f"error-message={app.error_text()!r}"
    )

    # The verb pair flips to its withdrawal arm from the nest's own answer.
    feed.stop_advertising_post_on_nostr(0)
    await_received(
        publish_relay,
        lambda e: e["pubkey"] == pubkey and e["kind"] == 5
        and any(v and v[0] == note["id"] for v in tag_values(e, "e")),
        RPC_ROUNDTRIP_S,
        "a kind 5 retracting the advertised note",
    )
    feed.open_post_actions(0)
    app.driver.wait_for("feed-post-advertise-nostr-button", timeout=RPC_ROUNDTRIP_S)
