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

**What this deliberately does not drive.** The advertised-post leg: an
advertised post's dialog must say what saving does to the Nostr advertisement
(`feed-post-sale-price-advertised-note`), a public text that reaches the
catalog only through its public-texts round, and until it does tui does not
offer the verb on an advertised post at all. The nest half of that lifecycle
(a new price re-advertises, a cleared one withdraws) is witnessed by
`tests/api/test_nostr_advertise_price_lifecycle.py`; the UI leg follows the
note.
"""

import uuid

import pytest
from nacl.signing import SigningKey

from actions.api_actor import ApiActor
from common.auth import create_actor_and_register
from helpers.budgets import RPC_ROUNDTRIP_S
from helpers.e2e_session import login_as
from helpers.waiting import wait_until

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

    nest, cleanup = _start_dedicated_nest(
        request, nest_mode, tmp_path_factory, "sale-price-nest")
    yield nest
    cleanup()


@pytest.fixture
def reprice_spa_url(static_dir, reprice_nest):
    """Function-scoped SPA proxy → `reprice_nest` (the `sell_spa_url` shape),
    requested lazily so the native lead never triggers the web build."""
    from conftest import _serve_spa_proxy
    url, server = _serve_spa_proxy(static_dir, reprice_nest["url"])
    yield url
    server.shutdown()


def _seller(app, request, nest):
    """Register an author, sign the app in as them, and hand back their
    headless API twin for the read-backs."""
    admin_sk = _as_signing_key(nest["admin"]["signing_key"])
    author = create_actor_and_register(
        nest["port"], base_url=nest["url"], admin_signing_key=admin_sk,
    )
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
