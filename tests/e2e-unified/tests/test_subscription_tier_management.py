"""tier_3 e2e for the author's own tier management past create + approve —
`monetization.md` § Pillar 1 — UX ("the author's own profile creates, edits and
deletes tiers"; "the selected tier's roster has a per-row remove") and § The
unifying model — tier as entitlement (a rank-R subscriber is enrolled in every
tier at or below R).

`test_subscriptions.py` pins create, approve and the roster; this module pins
the rest of the author's hand on their tiers: edit, delete, reject, remove,
and the rank cascade a higher tier's subscriber reads lower posts through.

Seeding follows the canonical author-client shape (`test_subscriptions.py`'s
module docstring): the AUTHOR'S APP creates every tier — a raw WS-RPC
`tiers.create` mints an orphan tier whose period key lands in no custody — and a
second actor's subscribe is headless precondition setup (`ApiActor.subscribe`),
because the behavior under test is what the AUTHOR does about it. Verification
reads the nest over WS-RPC where the UI shows nothing (a tier's description,
payment link and auto-approve flag are not columns of the §1 row; a subscriber's
access is the subscriber-gated `key_blob.get`), per convention 8: mutations
through the UI, ground truth read back.

All seven apps render the §1/§2/§3 surface these journeys drive; tui is the
lead and the column these witnesses were first recorded on. The tier editor's
price and payment-link fields are inside the `payments` plane
(`dynamic-features.md` § Platform-family surface excision → *The price-and-route
class*); the e2e harness builds every app's default flavor, which carries it, so
no flavor gate is needed here — a store-safe build is witnessed by its own
excision checks, never by this suite.
"""

import time
import uuid

import pytest
from nacl.signing import SigningKey

from actions.api_actor import ApiActor
from clients._ws_rpc_core import RpcCallError
from common.auth import create_actor_and_register
from helpers.e2e_session import login_as

pytestmark = [pytest.mark.tier_3]

# Convention-14 named budget for an author-side grant becoming visible to the
# subscriber (`test_subscriptions.py`'s constant, same reasoning).
AUTHOR_GRANT_BUDGET_S = 90

FOLLOWERS_TIER = "followers"


def _unique(prefix: str) -> str:
    return f"{prefix}-{uuid.uuid4().hex[:8]}"


def _as_signing_key(raw) -> SigningKey:
    return raw if isinstance(raw, SigningKey) else SigningKey(bytes.fromhex(raw))


@pytest.fixture
def tiers_nest(request, nest_mode, tmp_path_factory):
    """A dedicated fresh nest — a tier cannot be un-minted on the shared nest,
    so each journey owns its own (test order stays non-load-bearing)."""
    from conftest import _start_dedicated_nest

    nest, cleanup = _start_dedicated_nest(
        request, nest_mode, tmp_path_factory, "tier-mgmt-nest")
    yield nest
    cleanup()


@pytest.fixture
def tiers_spa_url(static_dir, tiers_nest):
    """The web twin of pointing a native app at the nest (`gated_spa_url`'s
    shape); requested lazily, so the native lead never builds the SPA."""
    from conftest import _serve_spa_proxy
    url, server = _serve_spa_proxy(static_dir, tiers_nest["url"])
    yield url
    server.shutdown()


def _register(nest) -> dict:
    return create_actor_and_register(
        nest["port"], base_url=nest["url"],
        admin_signing_key=_as_signing_key(nest["admin"]["signing_key"]),
    )


def _api(nest, user) -> ApiActor:
    return ApiActor(nest["url"], user["token"], user["actor_id_hex"], bytes(user["signing_key"]))


def _login_as(app, request, nest, user, *, handle: str) -> None:
    login_as(
        app, nest, user, handle=handle, device_id="test-device-tier-mgmt",
        request=request, spa_url_fixture="tiers_spa_url",
    )


def _create_tier(app, name: str, rank: int, **kw) -> None:
    subs = app.subscriptions
    subs.navigate()
    subs.open_tiers_tab()
    subs.create_tier(name, rank=rank, **kw)
    assert subs.wait_for_tier(name), (
        f"created tier {name!r} should appear in §1 My tiers; error={subs.error_text()!r}"
    )


def _tier_row(api: ApiActor, name: str) -> dict | None:
    return next((t for t in api.subscription_tiers_list() if t.get("name") == name), None)


def _wait(pred, timeout: float) -> bool:
    deadline = time.monotonic() + timeout
    while time.monotonic() < deadline:
        if pred():
            return True
        time.sleep(1)  # sleep-ok: poll interval of a deadline loop
    return bool(pred())


def _approve_until(app, done, timeout: float = AUTHOR_GRANT_BUDGET_S) -> bool:
    """Drive the author's §2 until ``done()`` — approving every row that is
    pending when observed. The connect-time drain may win some rows first
    (`test_sell_post`'s buyer-leg note); a manual approve is the fallback, not
    the mechanism under test."""
    subs = app.subscriptions
    deadline = time.monotonic() + timeout
    while time.monotonic() < deadline:
        if done():
            return True
        subs.refresh()
        if subs.pending_request_count() >= 1:
            subs.approve_first_request()
        time.sleep(1)  # sleep-ok: poll interval of a deadline loop
    return bool(done())


def _can_read_tier(api: ApiActor, author_id: bytes, tier: str) -> bool:
    """Whether ``api``'s actor holds a delivered wrap for ``tier`` — the
    subscriber-gated `key_blob.get` answers only a current subscriber of a
    tier with a minted blob."""
    try:
        return api.subscription_key_blob(author_id, tier)["version"] >= 1
    except RpcCallError:
        return False


@pytest.mark.feature("subscriptions-and-tiers")
def test_editing_a_tier_changes_what_it_offers(app, request, tiers_nest):
    """Outcome: you change a tier you already offer — its rank, description,
    price, payment link and whether it grants by itself. Every field is
    REPLACED with a new value; none is cleared, because `tiers.update` has no
    clear verb (`monetization.md` § Implementation status today) and the edit
    form promises none."""
    author = _register(tiers_nest)
    _login_as(app, request, tiers_nest, author, handle=_unique("e2e-editor"))
    tier = _unique("gold")
    _create_tier(
        app, tier, 1, price_hint="$5/mo", description="the first offer",
        payment_url="https://pay.example.com/old",
    )

    subs = app.subscriptions
    subs.edit_tier(
        tier, rank=3, description="the revised offer", price_hint="$8/mo",
        payment_url="https://pay.example.com/new", flip_auto_approve=True,
    )
    deadline = time.monotonic() + 10
    while time.monotonic() < deadline and subs.tier_price(tier) != "$8/mo":
        time.sleep(0.3)  # sleep-ok: poll interval of a deadline loop
    assert subs.tier_price(tier) == "$8/mo", (
        f"the §1 row should re-read the edited price, got {subs.tier_price(tier)!r}; "
        f"error={subs.error_text()!r}"
    )

    # The fields the §1 row does not show, read back off the nest.
    row = _tier_row(_api(tiers_nest, author), tier)
    assert row is not None, f"the edited tier {tier!r} must still exist nest-side"
    assert row.get("description") == "the revised offer", f"description not persisted: {row!r}"
    assert row.get("price_hint") == "$8/mo", f"price not persisted: {row!r}"
    assert row.get("payment_url") == "https://pay.example.com/new", (
        f"payment link not persisted: {row!r}"
    )
    assert row.get("auto_approve") is True, f"auto-approve not persisted: {row!r}"
    assert subs.error_text() == "", f"the edit raised an error: {subs.error_text()!r}"

    # Rank last, so a red here names exactly the one field that did not move.
    # Measured 2026-10-09 on tui: every field above lands and rank stays 1 —
    # the nest's `tiers.update` accepts `rank` on the wire and never applies
    # it (`tiers_update_handler`'s own doc comment). The app sends it; the
    # product gap is nest-side, recorded in `monetization.md` § Implementation
    # status today, and this assertion is the guard that turns green when the
    # rank edit is built.
    assert row.get("rank") == 3 and subs.wait_for_tier_rank(tier, 3), (
        f"the edited rank 3 should persist and re-read in §1, nest row={row!r}, "
        f"§1 rank={subs.tier_rank(tier)!r} — tiers.update does not apply rank "
        f"today (monetization.md § Implementation status today)"
    )


@pytest.mark.feature("subscriptions-and-tiers")
def test_deleting_a_tier_removes_it(app, request, tiers_nest):
    """Outcome: you delete a tier you no longer offer. A sibling tier is kept
    so the read-back proves the delete took the one row it was aimed at."""
    author = _register(tiers_nest)
    _login_as(app, request, tiers_nest, author, handle=_unique("e2e-deleter"))
    keep, drop = _unique("silver"), _unique("bronze")
    _create_tier(app, keep, 2)
    _create_tier(app, drop, 1)

    subs = app.subscriptions
    subs.delete_tier(drop)
    assert subs.wait_for_tier_gone(drop), (
        f"the deleted tier {drop!r} should leave §1, rows={subs.tier_names()!r}; "
        f"error={subs.error_text()!r}"
    )
    assert keep in subs.tier_names(), (
        f"only the aimed-at tier may go; rows={subs.tier_names()!r}"
    )
    api = _api(tiers_nest, author)
    assert _tier_row(api, drop) is None, (
        f"the nest still lists the deleted tier: {api.subscription_tiers_list()!r}"
    )
    assert _tier_row(api, keep) is not None, "the kept tier must survive nest-side"


@pytest.mark.feature("subscriptions-and-tiers")
def test_turning_a_subscribe_request_down(app, request, tiers_nest):
    """Outcome: you can turn a request to subscribe down. The request leaves
    §2, nobody joins §3, and the requester holds no access to the tier."""
    author = _register(tiers_nest)
    requester = _register(tiers_nest)
    _login_as(app, request, tiers_nest, author, handle=_unique("e2e-rejecter"))
    tier = _unique("gold")
    _create_tier(app, tier, 1)

    req_api = _api(tiers_nest, requester)
    reply = req_api.subscribe(author["actor_id_bytes"], tier)
    assert reply.get("outcome") == "queued", f"a fresh tier's subscribe queues, got {reply!r}"

    subs = app.subscriptions
    subs.refresh()
    assert subs.wait_for_pending_request(1), (
        f"the request should surface in §2; error={subs.error_text()!r}"
    )
    assert requester["actor_id_hex"].lower() in subs.pending_request_ids(), (
        f"§2 should name the requester, got {subs.pending_request_ids()!r}"
    )

    subs.reject_request(0)
    assert subs.wait_for_pending_request_count(0), (
        f"the rejected request should leave §2, still {subs.pending_request_ids()!r}; "
        f"error={subs.error_text()!r}"
    )
    assert subs.subscriber_count() == 0, (
        f"a rejected requester must not join §3, got {subs.subscriber_ids()!r}"
    )
    assert _api(tiers_nest, author).subscription_requests_list() == [], (
        "the nest should hold no pending request after the reject"
    )
    status = req_api.subscription_status(author["actor_id_bytes"])
    assert status.get("tier") != tier, f"the rejected requester holds the tier: {status!r}"
    assert not _can_read_tier(req_api, author["actor_id_bytes"], tier), (
        "the rejected requester must not read the tier's KeyBlob"
    )


@pytest.mark.feature("subscriptions-and-tiers")
def test_removing_a_subscriber_cuts_them_off_from_later_posts(app, request, tiers_nest):
    """Outcome: you remove a subscriber from a tier, and what you post to that
    tier afterwards no longer opens for them.

    Removal is a roster rotation: the author's client mints a reduced-roster
    KeyBlob under a fresh period key, and every post sealed to the tier after
    it is sealed under that key. So "no longer opens" is witnessed at its gate:
    after the remove, the subscriber-gated `key_blob.get` refuses the removed
    actor — the only door to the key a later post is sealed under — while
    before it the same read succeeded. The later post is composed through the
    author's UI so the gate is shown guarding a real post, not an empty tier."""
    author = _register(tiers_nest)
    subscriber = _register(tiers_nest)
    _login_as(app, request, tiers_nest, author, handle=_unique("e2e-remover"))
    tier = _unique("gold")
    _create_tier(app, tier, 1)

    sub_api = _api(tiers_nest, subscriber)
    author_id = author["actor_id_bytes"]
    assert sub_api.subscribe(author_id, tier).get("outcome") == "queued"
    subs = app.subscriptions
    subs.refresh()
    assert subs.wait_for_pending_request(1), f"§2 should show the request; error={subs.error_text()!r}"
    subs.approve_first_request()
    assert subs.wait_for_subscriber(1), f"approve should roster them; error={subs.error_text()!r}"
    assert _wait(lambda: _can_read_tier(sub_api, author_id, tier), AUTHOR_GRANT_BUDGET_S), (
        "precondition: the approved subscriber reads the tier's KeyBlob"
    )

    # ── The mutation under test: remove them from the §3 roster. ─────────────
    assert subscriber["actor_id_hex"].lower() in subs.subscriber_ids()
    subs.remove_subscriber(subs.subscriber_ids().index(subscriber["actor_id_hex"].lower()))
    assert subs.wait_for_subscriber_count(0), (
        f"the removed subscriber should leave §3, still {subs.subscriber_ids()!r}; "
        f"error={subs.error_text()!r}"
    )

    # ── A post to the tier after the removal. ────────────────────────────────
    preview = f"after the removal {_unique('teaser')}"
    app.feed.navigate()
    app.feed.create_gated_post("sealed for the remaining roster only", preview, tier)
    assert app.feed.wait_for_post_state_by_text(preview) is not None, (
        f"the post-removal gated post should land; error={app.error_text()!r}"
    )

    status = sub_api.subscription_status(author_id)
    assert status.get("tier") != tier, f"the removed subscriber still holds the tier: {status!r}"
    assert not _can_read_tier(sub_api, author_id, tier), (
        "the removed subscriber can still read the tier's KeyBlob — the key the "
        "post above is sealed under is reachable by someone the author removed"
    )


@pytest.mark.feature("subscriptions-and-tiers")
def test_a_higher_tier_opens_every_lower_tiers_posts(app, request, tiers_nest):
    """Outcome: subscribing to a higher tier also opens the posts of every lower
    tier, the author's follower posts included (`monetization.md` § The unifying
    model — the at-or-below-rank cascade, delivered by the grant-time fan-out
    enqueue the author's client then mints).

    The followers tier is provisioned by a THIRD actor's follow, never the
    subscriber's own: a subscriber who followed would read the follower post as
    a follower, and the cascade would go unwitnessed."""
    author = _register(tiers_nest)
    follower = _register(tiers_nest)
    subscriber = _register(tiers_nest)
    author_id = author["actor_id_bytes"]
    author_handle = _unique("e2e-cascade")
    _login_as(app, request, tiers_nest, author, handle=author_handle)

    silver, gold = _unique("silver"), _unique("gold")
    _create_tier(app, silver, 1)
    _create_tier(app, gold, 2)

    # ── The followers tier exists once someone follows; the author's client ──
    # drains the follow (auto_approve) and holds the tier's period key.
    follow_api = _api(tiers_nest, follower)
    follow_api.subscribe(author_id, FOLLOWERS_TIER)
    assert _approve_until(app, lambda: _can_read_tier(follow_api, author_id, FOLLOWERS_TIER)), (
        "precondition: a third actor's follow should be granted so the followers "
        f"tier exists with a key; error={app.subscriptions.error_text()!r}"
    )

    feed = app.feed
    feed.navigate()
    assert feed.wait_for_audience_option(FOLLOWERS_TIER, timeout_s=30.0), (
        f"the composer should offer the followers audience once the tier exists; "
        f"error={app.error_text()!r}"
    )
    marker_silver, marker_followers = _unique("silver-body"), _unique("followers-body")
    preview_silver, preview_followers = _unique("silver-teaser"), _unique("followers-teaser")
    feed.create_gated_post(f"for silver: {marker_silver}", preview_silver, silver)
    feed.create_gated_post(f"for followers: {marker_followers}", preview_followers, FOLLOWERS_TIER)

    # ── The subscriber subscribes to GOLD only, and the author approves it. ──
    sub_api = _api(tiers_nest, subscriber)
    assert sub_api.subscribe(author_id, gold).get("outcome") == "queued"
    assert _approve_until(
        app,
        lambda: all(_can_read_tier(sub_api, author_id, t) for t in (gold, silver, FOLLOWERS_TIER)),
    ), (
        "a gold grant should cascade into silver and followers (the fan-out rows "
        f"the author's client mints); reads: gold={_can_read_tier(sub_api, author_id, gold)}, "
        f"silver={_can_read_tier(sub_api, author_id, silver)}, "
        f"followers={_can_read_tier(sub_api, author_id, FOLLOWERS_TIER)}; "
        f"error={app.subscriptions.error_text()!r}"
    )

    # ── The gold subscriber's own app opens both lower posts. ────────────────
    # ONE post is opened through the UI — the follower post, the less obvious
    # half of the outcome. ui.yaml has no post-detail close, and on tui an open
    # detail survives both a nav reset and a same-actor re-login, hiding every
    # card; the silver post's cascade is the delivered silver wrap asserted
    # above, the same key its detail would unseal with.
    _login_as(app, request, tiers_nest, subscriber, handle=_unique("e2e-gold-sub"))
    for preview, marker in ((preview_followers, marker_followers),):
        row = feed.wait_for_post_state_by_text(preview, timeout=30.0)
        assert row is not None, (
            f"the gold subscriber's feed should list the post teased {preview!r}; "
            f"error={app.error_text()!r}"
        )
        # The state dump can lead the painted cards by a frame after the nav
        # reset: wait for the card itself before aiming the click at it.
        deadline = time.monotonic() + 15
        card = feed.post_index_by_id(row["post_id"])
        while time.monotonic() < deadline and not (0 <= card < app.driver.count("post-card")):
            time.sleep(0.3)  # sleep-ok: poll interval of a deadline loop
            card = feed.post_index_by_id(row["post_id"])
        assert 0 <= card < app.driver.count("post-card"), (
            f"the post teased {preview!r} is in state but has no painted card "
            f"(index {card}, cards={app.driver.count('post-card')})"
        )
        feed.open_post_detail(card)
        app.driver.wait_for("feed-post-detail-body")
        deadline = time.monotonic() + 15
        body = ""
        while time.monotonic() < deadline:
            body = feed.post_detail_body()
            if marker in body:
                break
            time.sleep(0.5)  # sleep-ok: poll interval of a deadline loop
        assert marker in body, (
            f"a gold subscriber should open the lower post teased {preview!r}, got "
            f"{body!r}; error={app.error_text()!r}"
        )
