"""tier_3 e2e for **re-pricing a sold post** — the *Change price…* verb on a
sold post's ⋯ menu and its dialog (`monetization.md` § The asking price →
*Re-pricing a sold post*; the seven `feed-post-sale-price-*` ids declared in
ui.yaml's `feed-post-actions-menu`, gated under `gated_features: payments`).

The author sells a post through the composer, then re-prices it through the
dialog: change both prices, clear the asking price, and — on a post sold with
no asking price — set one. After every save the selling tier is read back from
the nest (`fauna.subscriptions.tiers.list`, the author's bearer) — the save is
the shared `plan_sale_price_edit` → `apply_sale_price_edit` (update before any
clear; an untouched dialog sends nothing), so the tier the nest holds is the
outcome under test, never the dialog's own echo.

**The advertised-post leg** is the third test: an advertised post's dialog says
what saving does to the Nostr advertisement
(`feed-post-sale-price-advertised-note`, the catalog's
`sale_price.advertised_note`), a new price replaces the advertisement (a kind 5
naming the old note, then a kind 1 carrying the new price) and clearing the
asking price withdraws it. The publish relay is a `FakeRelay` on loopback, the
nest one this harness spawns, hence `standalone_only`. The nest half of that
lifecycle without an app is witnessed by
`tests/e2e-unified/tests/api/test_nostr_advertise_price_lifecycle.py`.
"""

import json
import uuid

import pytest
from nacl.signing import SigningKey

import fauna_ffi
from actions.api_actor import ApiActor
from common.auth import create_actor_and_register
from helpers import nostr_relay
from helpers.budgets import RPC_ROUNDTRIP_S
from helpers.e2e_session import login_as
from helpers.nostr_relay import FakeRelay, await_received, tag_values
from helpers.waiting import wait_until
from i18n.strings import S
from tests.api import ws_api

pytestmark = [pytest.mark.tier_3]

PREVIEW = "A teaser anyone can read."
FULL_BODY = "The paid part of the piece, for buyers only."
UNLOCK_TIER_PREFIX = "post-unlock-"
MSAT_PER_SAT = 1000


def _unique(prefix: str) -> str:
    return f"{prefix}-{uuid.uuid4().hex[:8]}"


def _as_signing_key(raw) -> SigningKey:
    return raw if isinstance(raw, SigningKey) else SigningKey(bytes.fromhex(raw))


@pytest.fixture
def reprice_nest(request, nest_mode, tmp_path_factory):
    """A dedicated fresh nest (`test_sell_post.py::sell_nest`'s shape), so
    test order stays non-load-bearing."""
    from conftest import _start_dedicated_nest

    # Loopback relays allowed, so the advertised-post leg's FakeRelay is
    # reachable; the two tests that never advertise are unaffected.
    nest, cleanup = _start_dedicated_nest(
        request, nest_mode, tmp_path_factory, "sale-price-nest",
        extra_env=dict(nostr_relay.ALLOW_LOOPBACK_ENV))
    yield nest
    cleanup()


@pytest.fixture
def publish_relay():
    relay = FakeRelay()
    yield relay
    relay.close()


@pytest.fixture
def reprice_spa_url(static_dir, reprice_nest):
    """Function-scoped SPA proxy → `reprice_nest` (the `sell_spa_url` shape),
    requested lazily so the native lead never triggers the web build."""
    from conftest import _serve_spa_proxy
    url, server = _serve_spa_proxy(static_dir, reprice_nest["url"])
    yield url
    server.shutdown()


def _seller(app, request, nest, before_login=None):
    """Register an author, sign the app in as them, and hand back their
    headless API twin for the read-backs. ``before_login`` (optional) is
    called with the registered author before the app signs in — the harness
    arranging account state the journey under test builds on (e2e rule 8b)."""
    admin_sk = _as_signing_key(nest["admin"]["signing_key"])
    author = create_actor_and_register(
        nest["port"], base_url=nest["url"], admin_signing_key=admin_sk,
    )
    if before_login is not None:
        before_login(author)
    login_as(
        app, nest, author, handle=_unique("e2e-repricer"),
        device_id="test-device-reprice", request=request,
        spa_url_fixture="reprice_spa_url", wait_for_id="compose-text-field",
    )
    api = ApiActor(
        nest["url"], author["token"], author["actor_id_hex"],
        bytes(author["signing_key"]),
    )
    return api


def _sell(app, price: str, asking_price_sats: int | None) -> str:
    """Sell a post through the composer and return its selling tier's name."""
    feed = app.feed
    feed.sell_post(FULL_BODY, PREVIEW, price, asking_price_sats=asking_price_sats)
    tier = feed.wait_for_gated_badge_text()
    assert tier is not None and tier.startswith(UNLOCK_TIER_PREFIX), (
        f"the sold post's badge should name a {UNLOCK_TIER_PREFIX}* tier, got "
        f"{tier!r}; error-message={app.error_text()!r}"
    )
    return tier


def _tier(api: ApiActor, name: str) -> dict:
    for t in api.subscription_tiers_list():
        if t.get("name") == name:
            return t
    raise AssertionError(f"the nest lists no tier {name!r}")


def _asking_sats(tier: dict) -> int | None:
    """The tier's asking price in sats, or None when it carries none."""
    asking = tier.get("asking_price")
    if not asking:
        return None
    assert asking.get("unit") == "msat", f"unexpected asking-price unit: {asking!r}"
    return asking["value"] // MSAT_PER_SAT


def _await_tier(api: ApiActor, name: str, hint: str, asking: int | None) -> None:
    """Poll the nest's tier until it holds ``hint`` and ``asking`` (the dialog
    closes on the saved answer, so this normally passes on the first read;
    polled anyway, convention 14)."""
    def held():
        t = _tier(api, name)
        return t.get("price_hint") == hint and _asking_sats(t) == asking
    wait_until(
        held, RPC_ROUNDTRIP_S,
        diagnose=lambda: f"want hint={hint!r} asking={asking!r}, nest holds {_tier(api, name)!r}",
    )


@pytest.mark.feature("paid-posts-and-tips")
def test_change_price_sets_changes_and_clears_the_asking_price(app, request, reprice_nest):
    api = _seller(app, request, reprice_nest)
    tier = _sell(app, "$3", asking_price_sats=2100)
    _await_tier(api, tier, "$3", 2100)

    feed = app.feed

    # ── Change both prices; the dialog opened pre-filled from the tier. ──────
    prefill = feed.change_sale_price(0, "$5", "5000")
    assert prefill == ("$3", "2100"), (
        f"the dialog should open on the tier's stored prices, got {prefill!r}"
    )
    _await_tier(api, tier, "$5", 5000)

    # ── Clear the asking price; the hint stays. ──────────────────────────────
    prefill = feed.change_sale_price(0, "$5", "")
    assert prefill == ("$5", "5000"), (
        f"the dialog should re-open on the saved prices, got {prefill!r}"
    )
    _await_tier(api, tier, "$5", None)

    # ── Re-opened once more: it reads the cleared state back. ────────────────
    prefill = feed.open_sale_price_dialog(0)
    assert prefill == ("$5", ""), (
        f"a cleared asking price should pre-fill empty, got {prefill!r}"
    )
    app.driver.click("feed-post-sale-price-cancel-button")
    wait_until(
        lambda: app.driver.is_absent("feed-post-sale-price-dialog"),
        RPC_ROUNDTRIP_S, diagnose=lambda: "cancel left the dialog open",
    )
    _await_tier(api, tier, "$5", None)


@pytest.mark.feature("paid-posts-and-tips")
def test_change_price_sets_an_asking_price_on_a_post_sold_without_one(
    app, request, reprice_nest,
):
    api = _seller(app, request, reprice_nest)
    tier = _sell(app, "$2", asking_price_sats=None)
    _await_tier(api, tier, "$2", None)

    feed = app.feed

    # ── A price that is not a whole number of sats is refused in the dialog:
    # the composer's own rule (`parse_asking_price_sats`), on error-message,
    # the dialog left open and the tier untouched.
    feed.open_sale_price_dialog(0)
    app.driver.clear_and_type("feed-post-sale-price-asking", "12.5")
    app.driver.click("feed-post-sale-price-save-button")
    wait_until(
        lambda: app.error_text(), RPC_ROUNDTRIP_S,
        diagnose=lambda: "a fractional asking price raised no error-message",
    )
    assert app.driver.is_visible("feed-post-sale-price-dialog"), (
        "a refused save must leave the dialog open with what the author typed"
    )
    app.driver.click("feed-post-sale-price-cancel-button")
    _await_tier(api, tier, "$2", None)

    # ── Set one. ─────────────────────────────────────────────────────────────
    prefill = feed.change_sale_price(0, "$2", "1500")
    assert prefill == ("$2", ""), (
        f"a post sold without an asking price should pre-fill it empty, got {prefill!r}"
    )
    _await_tier(api, tier, "$2", 1500)


@pytest.mark.standalone_only
@pytest.mark.feature("paid-posts-and-tips")
def test_change_price_on_an_advertised_post_shows_the_note_and_re_advertises(
    app, request, reprice_nest, publish_relay,
):
    """Advertised post → *Change price…* shows the advertised note → a new
    asking price replaces the advertisement (kind 5 on the old note, kind 1 at
    the new price) → clearing it withdraws it and the menu offers *Advertise on
    Nostr* again (`monetization.md` § Re-pricing a sold post; § The Nostr
    advertisement of a sold post → Lifecycle)."""
    pubkey_box: dict = {}

    def arrange_nostr(author):
        # The generated key and relay list go through the nest's own doors, as
        # the Nostr page sends them (rule 8b) — as test_advertise_post_nostr.
        reply = ws_api.bridge_link(reprice_nest["port"], author, "nostr", "generate")
        assert reply["linked"], reply
        pubkey_box["hex"] = fauna_ffi.nostr_npub_to_hex(reply["identity"]["value"])
        ws_api.bridge_set_settings(
            reprice_nest["port"], author, "nostr",
            {"relay_list": json.dumps([publish_relay.url])},
        )

    api = _seller(app, request, reprice_nest, before_login=arrange_nostr)
    pubkey = pubkey_box["hex"]
    tier = _sell(app, "$3", asking_price_sats=2100)
    _await_tier(api, tier, "$3", 2100)

    feed = app.feed
    feed.advertise_post_on_nostr(0)
    old_note = await_received(
        publish_relay,
        lambda e: e["pubkey"] == pubkey and e["kind"] == 1 and PREVIEW in e["content"],
        RPC_ROUNDTRIP_S, "the advertised teaser as a kind-1 note",
    )
    assert ["price", "2100000", "msat"] in old_note["tags"], old_note["tags"]

    # ── The dialog says what saving does before the author saves. ────────────
    # The relay's receipt proves the nest published, not that the card has
    # repainted from `nostr_advertised_at`: the dialog paints its note from that
    # field, so wait for the card's own advertised arm first (convention 14).
    feed.open_post_actions(0)
    app.driver.wait_for("feed-post-stop-advertising-nostr-button", timeout=RPC_ROUNDTRIP_S)
    feed.open_sale_price_dialog(0)
    app.driver.wait_for("feed-post-sale-price-advertised-note", timeout=RPC_ROUNDTRIP_S)
    assert app.driver.get_text("feed-post-sale-price-advertised-note") == (
        S.sale_price.advertised_note
    ), "the dialog's note should be the catalog's text"
    app.driver.click("feed-post-sale-price-cancel-button")
    wait_until(
        lambda: app.driver.is_absent("feed-post-sale-price-dialog"),
        RPC_ROUNDTRIP_S, diagnose=lambda: "cancel left the dialog open",
    )

    # ── A new asking price replaces the advertisement. ───────────────────────
    feed.change_sale_price(0, "$3", "5000")
    _await_tier(api, tier, "$3", 5000)
    await_received(
        publish_relay,
        lambda e: e["pubkey"] == pubkey and e["kind"] == 5
        and any(v and v[0] == old_note["id"] for v in tag_values(e, "e")),
        RPC_ROUNDTRIP_S, "a kind 5 retracting the old note",
    )
    new_note = await_received(
        publish_relay,
        lambda e: e["pubkey"] == pubkey and e["kind"] == 1
        and e["id"] != old_note["id"]
        and ["price", "5000000", "msat"] in e["tags"],
        RPC_ROUNDTRIP_S, "a kind-1 note carrying the new price",
    )

    # ── Clearing the asking price withdraws it. ──────────────────────────────
    feed.change_sale_price(0, "$3", "")
    _await_tier(api, tier, "$3", None)
    await_received(
        publish_relay,
        lambda e: e["pubkey"] == pubkey and e["kind"] == 5
        and any(v and v[0] == new_note["id"] for v in tag_values(e, "e")),
        RPC_ROUNDTRIP_S, "a kind 5 retracting the re-advertised note",
    )
    feed.open_post_actions(0)
    app.driver.wait_for("feed-post-advertise-nostr-button", timeout=RPC_ROUNDTRIP_S)
