"""tier_3 e2e for **"sell this post"** — per-post pay-to-unlock authoring
(`monetization.md` § Per-post pay-to-unlock; the app-UI half of gap (2b), whose
shared-Rust orchestration landed 2026-07-29).

The author sells a post entirely through the composer UI: picking "Sell this
post…" on `compose-gate-tier-select` (the select's third answer, IDs
user-approved 2026-07-29), filling `compose-sell-price` and the public teaser,
and submitting. Underneath, the shared `FeedManager::prepare_sell_post` runs the
whole forced five-step ordering in one call — mint + persist the period key,
build the birth `KeyBlob`, build the gated body naming that tier, take
`post_id = blake3(body)`, then `commit_tier` with `unlocks_post = post_id` —
and the app finishes through the **unchanged** `submit_gated_post` pair.

**Why this journey is provable on tui, and `test_gated_post_compose.py` is not.**
That suite's author leg needs the §1 Tiers tab to create the tier first, a
surface tui has not built (`ui-actual-tui.yaml:164`). Selling needs **no
pre-existing tier** — the tier is auto-minted as part of the submit — so the
lead app can drive the entire authoring journey today. That is a property of the
ratified shape, not a workaround.

**What this asserts, and what it deliberately does not.** The author-side chain
is proven end to end, including that the nest really did record the designation:
the badge names a `post-unlock-…` tier, which only exists if the mint + gate +
create sequence all landed. The **buyer** leg (claim-code redemption → §2
approve → unseal) is proven separately below, on apps that have a §5 claim
surface — see the module-level residual note at the bottom of this file for
which apps still lack it.
"""

import time
import urllib.request
import uuid
from pathlib import Path

import pytest

from common.auth import create_actor_and_register
from actions.api_actor import ApiActor
from helpers.budgets import RPC_ROUNDTRIP_S
from helpers.e2e_session import login_as
from nacl.signing import SigningKey

pytestmark = [pytest.mark.tier_3]

PREVIEW = "The first two paragraphs, free to read."
FULL_MARKER = "the-sold-body-marker"
FULL_BODY = f"The rest of the piece: {FULL_MARKER}, for buyers only."
PRICE = "$3"
TEST_IMAGE = Path(__file__).parent.parent / "fixtures" / "test-image.png"
PNG_MAGIC = b"\x89PNG\r\n\x1a\n"

# `monetization.md` § Per-post pay-to-unlock — names are
# `post-unlock-<16 hex>` from the shared `mint_unlock_tier_name()`,
# deliberately not derived from the sealed text.
UNLOCK_TIER_PREFIX = "post-unlock-"


def _unique(prefix: str) -> str:
    return f"{prefix}-{uuid.uuid4().hex[:8]}"


def _as_signing_key(raw) -> SigningKey:
    return raw if isinstance(raw, SigningKey) else SigningKey(bytes.fromhex(raw))


@pytest.fixture
def sell_nest(request, nest_mode, tmp_path_factory):
    """A dedicated fresh nest (the `test_gated_post_compose.py::gated_nest`
    shape) — own nest, own actor, so test order stays non-load-bearing."""
    from conftest import _start_dedicated_nest

    nest, cleanup = _start_dedicated_nest(
        request, nest_mode, tmp_path_factory, "sell-post-nest")
    yield nest
    cleanup()


@pytest.fixture
def sell_spa_url(static_dir, sell_nest):
    """Function-scoped SPA proxy → `sell_nest`, so a WEB browser reaches it
    without CORS (the raw nest sends no `Access-Control-Allow-Origin`, which
    blocks the blob-upload POST). Requested lazily, so the native lead never
    triggers the web build. Mirrors `gated_spa_url`."""
    from conftest import _serve_spa_proxy
    url, server = _serve_spa_proxy(static_dir, sell_nest["url"])
    yield url
    server.shutdown()


def _login_as(app, request, nest, user, *, handle: str) -> None:
    """set_state login landing on the feed view, then wait for the composer
    field (the only journey among this module's siblings that immediately
    drives compose after login). Thin wrapper over the shared
    `helpers.e2e_session.login_as` — see that docstring for the barrier
    rationale (2026-08-10; this suite is where it was measured)."""
    login_as(
        app, nest, user, handle=handle, device_id="test-device-sell",
        request=request, spa_url_fixture="sell_spa_url",
        wait_for_id="compose-text-field",
    )


@pytest.mark.feature("paid-posts-and-tips")
def test_sell_post_mints_an_unlock_tier_and_gates_the_post(app, request, sell_nest):
    app.subscriptions.require_sell_post_authoring_supported()

    admin_sk = _as_signing_key(sell_nest["admin"]["signing_key"])
    author = create_actor_and_register(
        sell_nest["port"], base_url=sell_nest["url"], admin_signing_key=admin_sk,
    )
    _login_as(app, request, sell_nest, author, handle=_unique("e2e-seller"))

    feed = app.feed

    # ── Sell the post through the composer UI (the mutation under test). ─────
    # No tier is created first: `prepare_sell_post` mints one as part of this.
    feed.sell_post(FULL_BODY, PREVIEW, PRICE)

    # ── The card is a gated card: teaser only, never the sold body. ──────────
    # `wait_for_first_post_text`, not a bare `first_post_text()` read: the new
    # post reaching the top of the re-queried feed races `sell_post()`'s
    # compose call (its own doc comment).
    assert feed.wait_for_first_post_text(PREVIEW.split()[0]), (
        f"the list card should show the teaser; "
        f"error-message={app.error_text()!r}"
    )
    top = feed.first_post_text()
    assert FULL_MARKER not in top, (
        f"the sold body leaked into the list card in PLAINTEXT: {top!r}"
    )

    # ── The badge names an auto-minted unlock tier. ──────────────────────────
    # This is the end-to-end proof that the whole forced ordering landed: the
    # badge renders off `GatedInfo.tier` as the NEST projected it back, so a
    # `post-unlock-` name here means mint → birth KeyBlob → gated body →
    # post_id → commit_tier(unlocks_post) all really happened. Read
    # `error-message` into the failure text first (conventions rule 6) — every
    # link in that chain surfaces there. `wait_for_gated_badge_text`, not a
    # bare count-then-text read: the two are not the same registry moment
    # (its own doc comment).
    badge = feed.wait_for_gated_badge_text()
    assert badge is not None and badge.startswith(UNLOCK_TIER_PREFIX), (
        f"the badge should name an auto-minted {UNLOCK_TIER_PREFIX}* tier, got "
        f"{badge!r}; error-message={app.error_text()!r}"
    )

    # ── The author still reads their own post: custody holds the period key. ─
    feed.open_post_detail(0)
    app.driver.wait_for("feed-post-detail-body")
    deadline = time.monotonic() + 10
    body = ""
    while time.monotonic() < deadline:
        body = feed.post_detail_body()
        if FULL_MARKER in body:
            break
        time.sleep(0.5)
    assert FULL_MARKER in body, (
        f"the author's own sold post should unseal on detail open (custody "
        f"period key), got {body!r}; error-message={app.error_text()!r}"
    )

    # ── §1 My-tiers exclusion (leg (2), monetization.md § Per-post ───────────
    # pay-to-unlock): a designated unlock tier never appears in the author's own
    # tier-management list. Unconditional — every app that passes this module's
    # authoring gate renders a §1 surface, tui included since the tui Tiers author track
    # (2026-08-01); the old platform branch here predated that and silently
    # stopped covering the lead app.
    subs = app.subscriptions
    subs.navigate()
    subs.open_tiers_tab()
    names = subs.tier_names()
    assert badge not in names, (
        f"the auto-minted unlock tier {badge!r} must not appear in §1 My "
        f"tiers (monetization.md § Per-post pay-to-unlock), got {names!r}"
    )
    # ⚠ The INVERSE half — that §3/§4/§5's pickers still *offer* the designated
    # tier — is deliberately not asserted here: no driver exposes a select's
    # option list, and adding that command is a 7-app contract change
    # (conventions rule 11). It is pinned per-app instead (tui:
    # `profile::tests::the_three_tier_pickers_still_offer_a_designated_unlock_tier`)
    # and end-to-end by the buyer leg below, which mints a §5 claim against this
    # very tier and then reads its buyer out of §3.


@pytest.mark.feature("paid-posts-and-tips")
def test_sold_post_attachment_is_sealed_and_never_uploaded_in_plaintext(
    app, request, sell_nest,
):
    """A photo attached to a SOLD post is sealed under the tier the sale itself
    mints — the hardest case of `ui/media.md` § Encryption at rest's ordering
    rule, and the reason the mint is split in two.

    The rule is that a blob's audience decides its seal, so the seal must be
    resolved before the bytes are POSTed; blob `GET` is unauthenticated by
    design and the nest exposes no blob DELETE, so "no plaintext copy is
    fetchable" is achievable only by never uploading one. For a *gated* post the
    audience is a tier the author already has. For a *sold* post there is no
    tier at all until `prepare_sell_post` mints one — which is why
    `FeedManager::stage_sell_tier` exists: it mints and persists the period key
    without creating anything server-side, so the photo has something to seal
    under, and `prepare_sell_post` then finishes on that same stage rather than
    minting a second tier (`behavior/monetization.md` § Per-post pay-to-unlock).

    Two properties, and the second is the one this test exists for:

    1. The photo survives the sale — the sold body carries it as a `MediaItem`,
       which it can only do if the seal ran and `prepare_sell_post` folded a
       `TextWithMedia` body under the *same* `seal_id`.
    2. **No plaintext copy of it is ever POSTed** — asserted on the bytes the
       store actually holds, fetched with no credentials at all, exactly as any
       stranger holding the hash would.
    """
    app.subscriptions.require_sell_post_authoring_supported()
    if not TEST_IMAGE.exists():
        pytest.skip("Test image fixture not found")

    admin_sk = _as_signing_key(sell_nest["admin"]["signing_key"])
    author = create_actor_and_register(
        sell_nest["port"], base_url=sell_nest["url"], admin_signing_key=admin_sk,
    )
    _login_as(app, request, sell_nest, author, handle=_unique("e2e-sell-photo"))

    feed = app.feed
    # The sale is picked first and the file attached after — the order the app
    # itself must follow, since phase one of the mint has to run before the
    # seal can reach a period key (see `sell_post`'s own doc comment).
    feed.sell_post(FULL_BODY, PREVIEW, PRICE, image_path=str(TEST_IMAGE))

    assert feed.wait_for_first_post_text(PREVIEW.split()[0]), (
        f"the list card should show the teaser; error-message={app.error_text()!r}"
    )

    # The author unseals from custody; the opened body is what carries the item,
    # so the image is only nameable on this side of the unlock.
    feed.open_post_detail(0)
    app.driver.wait_for("feed-post-detail-body")
    deadline = time.monotonic() + 15
    body = ""
    while time.monotonic() < deadline:
        body = feed.post_detail_body()
        if FULL_MARKER in body:
            break
        time.sleep(0.5)  # sleep-ok: poll interval inside a deadline loop, not a settle wait
    assert FULL_MARKER in body, (
        f"the author's own sold post should unseal on detail open, got {body!r}; "
        f"error-message={app.error_text()!r}"
    )

    # Keyed on FULL_BODY, not the teaser: the unlock replaced the card's teaser
    # with the opened body, which is also what sets `has_media`/`media_hash` —
    # the nest's own projection cannot, since the item is named only inside a
    # body it can never read.
    blob_hash = feed.post_image_blob_hash_by_text(FULL_BODY)
    assert len(blob_hash) == 64, (
        f"the unsealed sold post should carry its attachment as a MediaItem — a "
        f"64-hex blob hash, got {blob_hash!r}. An empty hash means the sell arm "
        f"dropped the attachment (PostBody::Text instead of TextWithMedia); "
        f"posts={app.driver.get_state('data.feed.posts')!r} "
        f"error-message={app.error_text()!r}"
    )

    with urllib.request.urlopen(
        urllib.request.Request(
            f"{sell_nest['url']}/api/v1/blob/{blob_hash}", method="GET",
        ),
        timeout=RPC_ROUNDTRIP_S,
    ) as resp:
        stored = resp.read()
    assert stored[: len(PNG_MAGIC)] != PNG_MAGIC, (
        "the blob a SOLD post names is readable PNG plaintext — the composer "
        "uploaded before the sale's tier was staged, so a copy of the very "
        "picture the author is charging for is fetchable by anyone with the "
        f"hash, and no blob DELETE exists to remove it. first bytes={stored[:16]!r}"
    )
    assert len(stored) >= 28, (
        f"a ChaCha20-Poly1305 sealed blob is at least 28 bytes (nonce + tag); got "
        f"{len(stored)}B — that is not a seal"
    )


@pytest.mark.feature("paid-posts-and-tips")
def test_sell_post_buyer_redeems_claim_and_unseals(app, request, sell_nest):
    """The buyer leg (gap (2c)): a self-serve "buy this post" button has no
    price to read from yet (both generic tier reads filter designated tiers
    out, and `GatedInfo` carries only the tier name), so today's working
    purchase path is claim-code redemption (§5) — proven end to end here on
    linux, which has a §5 claim surface (`test_manual_claim_mint_and_list`'s
    UI mint + `test_claim_redeem_via_ui`'s UI redeem are each already proven
    individually; this composes them with the sell-post orchestration and the
    §2 UI-approve leg, `test_gate_to_tier_compose_and_subscriber_unlock`'s
    re-login pattern, to prove the whole chain).

    The grant lands via `SubscriptionsAuthor::drain_auto_approvals`, which
    treats a `payment_entitled` request exactly like an `auto_approve` tier's
    — it grants "without creator judgment" on the author's next connect
    (`monetization.md` § Pillar 3), and a real re-login (the seller's third
    login below) always re-triggers that connect-time drain
    (`subscriptions_author::start`). On a fast box the drain reliably wins
    before any UI click could, so this test polls for the grant landing
    rather than requiring the row to still be visible in §2 — clicking
    Approve is a fallback exercised only if the row is still pending when
    observed, not the mechanism under test."""
    app.subscriptions.require_sell_post_buyer_leg_supported()

    admin_sk = _as_signing_key(sell_nest["admin"]["signing_key"])
    seller = create_actor_and_register(
        sell_nest["port"], base_url=sell_nest["url"], admin_signing_key=admin_sk,
    )
    buyer = create_actor_and_register(
        sell_nest["port"], base_url=sell_nest["url"], admin_signing_key=admin_sk,
    )
    seller_handle = _unique("e2e-seller")
    buyer_handle = _unique("e2e-buyer")

    # ── Seller: sell the post through the composer UI, capture the ──────────
    # auto-minted tier name off the badge.
    _login_as(app, request, sell_nest, seller, handle=seller_handle)
    feed = app.feed
    feed.sell_post(FULL_BODY, PREVIEW, PRICE)
    tier = feed.wait_for_gated_badge_text()
    assert tier is not None and tier.startswith(UNLOCK_TIER_PREFIX), (
        f"a sold post's card should carry gated-post-badge naming an auto-minted "
        f"{UNLOCK_TIER_PREFIX}* tier, got {tier!r}; error-message={app.error_text()!r}"
    )

    # ── Seller: mint a manual claim code for the unlock tier through §5. ─────
    subs = app.subscriptions
    subs.navigate()
    subs.open_tiers_tab()
    subs.mint_claim(tier)
    assert subs.wait_for_claim_count(1), (
        f"a minted claim should appear in §5; error={subs.error_text()!r}"
    )
    code = subs.claim_codes()[0]

    # ── Buyer: redeem the code — a payment_entitled request queues (the ──────
    # same UI leg `test_claim_redeem_via_ui` already proves).
    _login_as(app, request, sell_nest, buyer, handle=buyer_handle)
    subs.navigate_settings()
    subs.redeem_claim(code)
    assert subs.wait_for_mine_subscription(tier), (
        f"the redeemed entitlement should appear in My Subscriptions; "
        f"error={subs.error_text()!r}"
    )

    # ── Seller: grant the buyer's now-queued payment-entitled request. This ──
    # re-login always re-triggers the connect-time `drain_auto_approvals`
    # pass, which grants a `payment_entitled` request "without creator
    # judgment" (monetization.md § Pillar 3) — on a fast box it reliably wins
    # before the UI could see the row pending, which is why asserting
    # "pending" first is the wrong expectation, not a flaky one. The
    # Tiers-tab is also observer-free (manual re-read only — this module's
    # own doc comment): a mutation the drain makes headlessly never repaints
    # this page on its own, so each poll re-triggers a fresh read via
    # `refresh()`. Click Approve only if the row is still visible when
    # observed — the manual §2 path stays exercised, it just isn't required
    # to win the race.
    _login_as(app, request, sell_nest, seller, handle=seller_handle)
    subs.navigate()
    subs.open_tiers_tab()
    deadline = time.monotonic() + 20
    approved_manually = False
    while time.monotonic() < deadline and subs.subscriber_count() < 1:
        subs.refresh()
        if not approved_manually and subs.pending_request_count() >= 1:
            subs.approve_first_request()
            approved_manually = True
        time.sleep(0.5)
    # Report which of the two grant routes was even REACHABLE (e2e convention 6):
    # a bare `0 >= 1` cannot tell "the redemption never queued a request on the
    # seller's side" from "the request queued but neither the drain nor the
    # manual approve granted it" — and those have different owners. `pending`
    # is re-read here rather than reused from the loop so it reflects the
    # terminal state.
    assert subs.subscriber_count() >= 1, (
        f"the buyer's redeemed claim should grant — via the seller's own §2 "
        f"approve or the connect-time auto-drain; "
        f"pending_requests={subs.pending_request_count()}, "
        f"clicked_approve={approved_manually}; error={subs.error_text()!r}"
    )

    # ── Buyer: the sold post now unseals on detail open (KeyBlob entry → ─────
    # derive_post_key → decrypt_content) — the working end-to-end purchase.
    _login_as(app, request, sell_nest, buyer, handle=buyer_handle)
    assert feed.wait_post_count(1) >= 1, (
        f"buyer's feed should list the sold post; error-message={app.error_text()!r}"
    )
    feed.open_post_detail(0)
    app.driver.wait_for("feed-post-detail-body")
    deadline = time.monotonic() + 15
    body = ""
    while time.monotonic() < deadline:
        body = feed.post_detail_body()
        if FULL_MARKER in body:
            break
        time.sleep(0.5)
    assert FULL_MARKER in body, (
        f"the buyer should unseal the sold post on detail open, got {body!r}; "
        f"error-message={app.error_text()!r}"
    )


@pytest.mark.feature("paid-posts-and-tips")
def test_sell_post_teaser_price_and_self_serve_buy(app, request, sell_nest):
    """The self-serve teaser purchase (gap (2c), `monetization.md` § Per-post
    pay-to-unlock → the buyer's price read is post-addressed): a buyer who can
    see a sold post's teaser sees its price and can start the purchase
    WITHOUT a claim code — `gated-post-price` + `gated-post-buy-button`,
    resolved lazily off `fauna.subscriptions.post_unlock.get`.

    Proven in two tiers: the price-resolve + buy-click leg runs on every app
    that has the teaser affordance (tui included — the lead app); the
    §2-approve + detail-open-unseal completion (the same chain the claim-code
    buyer arm above proves) additionally needs an author-side subscriptions
    management surface, which tui does not have at all (`monetization.md`:
    "tui, which has no author-side subscriptions surface at all") — the exact
    reason the claim-code buyer test above skips on tui too."""
    app.subscriptions.require_teaser_buy_supported()

    admin_sk = _as_signing_key(sell_nest["admin"]["signing_key"])
    seller = create_actor_and_register(
        sell_nest["port"], base_url=sell_nest["url"], admin_signing_key=admin_sk,
    )
    buyer = create_actor_and_register(
        sell_nest["port"], base_url=sell_nest["url"], admin_signing_key=admin_sk,
    )
    seller_handle = _unique("e2e-seller")
    buyer_handle = _unique("e2e-buyer")
    # This run's own teaser text. On a live box the buyer's feed can also hold
    # sold posts other runs left behind at the same price, so every buyer
    # step below aims at the card carrying this preview, never at "the first
    # buy button on screen" (the shared-box rule: assert on the test's own
    # data).
    preview = f"{PREVIEW} {_unique('sold')}"

    # ── Seller: sell the post through the composer UI. ───────────────────────
    _login_as(app, request, sell_nest, seller, handle=seller_handle)
    feed = app.feed
    feed.sell_post(FULL_BODY, preview, PRICE)
    assert feed.wait_for_gated_badge_text() is not None, (
        f"a sold post's card should carry gated-post-badge; "
        f"error-message={app.error_text()!r}"
    )
    # (The author must never be OFFERED a purchase of their own post — the
    # defect that let this test's completion leg subscribe the wrong actor. That
    # invariant is pinned deterministically in shared Rust
    # (`fauna-feed`'s `resolve_post_unlock_offer_ignores_the_authors_own_post` +
    # `buy_unlock_offer_refuses_the_authors_own_post`) rather than as an e2e
    # absence assert, which could only be a settle-window against an async
    # `post_unlock.get` round trip — convention 14.)

    # ── Buyer: the teaser's price resolves (async post_unlock.get) and ───────
    # matches what the seller typed — the post-addressed read working end to
    # end, not just a static echo of compose state.
    _login_as(app, request, sell_nest, buyer, handle=buyer_handle)
    assert feed.wait_post_count(1) >= 1, (
        f"buyer's feed should list the sold post; error-message={app.error_text()!r}"
    )
    row = feed.wait_for_post_state_by_text(preview)
    assert row is not None, (
        f"buyer's feed should hold this run's sold post; "
        f"error-message={app.error_text()!r}"
    )
    card = feed.post_index_by_id(row["post_id"])
    assert card >= 0, f"the sold post {row['post_id']} has no rendered card"
    other_buy_buttons = app.driver.count("gated-post-buy-button") - 1
    assert feed.wait_for_unlock_offer_price(card=card), (
        f"gated-post-price should resolve off fauna.subscriptions.post_unlock.get; "
        f"error-message={app.error_text()!r}"
    )
    assert feed.unlock_offer_price_text(card=card) == PRICE, (
        f"the teaser price should match what the seller typed at sell time"
    )

    # ── Buyer: click the self-serve buy button — no claim code involved. ─────
    feed.buy_unlock_offer(card=card)
    # Barrier on the PURCHASE ITSELF, not on the click returning. The click is
    # fire-and-forget (the handler's subscribe round trip is still in flight
    # when it returns), and the very next step switches actors — which tears
    # this session down under the in-flight call. The buy affordance
    # un-rendering is the completion signal `FeedManager::buy_unlock_offer`
    # clears `unlock_offer` to produce (testing.md convention 14: poll a real
    # state change to a deadline, never sleep a guessed interval).
    assert feed.wait_for_unlock_offer_cleared(card=card), (
        f"the buy affordance should un-render once the purchase lands — "
        f"buy_unlock_offer clears the post's unlock_offer on success; "
        f"other sold posts on screen={other_buy_buttons}; "
        f"error-message={app.error_text()!r}"
    )
    print(f"[sell-post] other sold posts on the buyer's screen: {other_buy_buttons}")

    # ── The completion (§2-approve + detail-open-unseal) needs an author-side
    # subscriptions management surface — tui has none at all (the same reason
    # the claim-code buyer test above skips there), so this leg is linux/web/
    # windows (+ any future lift that gains §1/§2), never tui.
    #
    # macOS/iOS have the §1/§2 surface (the claim-code buyer arm proves it,
    # `test_sell_post_buyer_redeems_claim_and_unseals`) but are DELIBERATELY
    # excluded here too — measured 2026-08-25: §2-approve genuinely
    # works on apple (wait_for_pending_request + wait_for_subscriber both
    # green), but the FINAL detail-open unseal fails with "sealed model
    # failed to decrypt: backup chunk decryption failed (wrong key or
    # tampered data): aead::Error" — the SAME error signature already under
    # active investigation elsewhere on apple for a different ceremony
    # (multi-actor-switch key material, the succession-aftermath leg-1
    # track) after repeated actor switches in one process (this test
    # does four: seller, buyer, seller, buyer). A pre-existing, apple-wide
    # gap this row's teaser-render scope did not create and should not carry
    # — tracked separately rather than silently included in gap (2c)'s green.
    if not (
        app.driver.is_linux()
        or app.driver.is_web()
        or app.driver.is_windows()
        or app.driver.is_macos()
        or app.driver.is_ios()
    ):
        return

    # ── Seller: the buy button's subscribe call queued a real request — the ──
    # same §2 surface the claim-code path drains, proving this is the genuine
    # subscribe flow and not an inert click.
    _login_as(app, request, sell_nest, seller, handle=seller_handle)
    subs = app.subscriptions
    subs.navigate()
    subs.open_tiers_tab()
    subs.refresh()
    assert subs.wait_for_pending_request(1), (
        f"the buy button's subscribe should surface in §2; "
        f"error={subs.error_text()!r}"
    )
    # The request must name the BUYER before it is approved — this separates a
    # purchase made by the wrong (outgoing) session from a correct purchase the
    # approve then rosters wrongly.
    assert buyer["actor_id_hex"].lower() in subs.pending_request_ids(), (
        f"the queued request should name the BUYER "
        f"{buyer['actor_id_hex'][:12]}…, got {subs.pending_request_ids()}; "
        f"error={subs.error_text()!r}"
    )
    subs.approve_first_request()
    assert subs.wait_for_subscriber(1), (
        f"after approve the buyer should join the unlock tier's roster; "
        f"error={subs.error_text()!r}"
    )
    # WHO joined, not just how many. A bare count is satisfied by ANY actor —
    # including the outgoing one — which is how this leg read green while the
    # roster held the seller (2026-08-25). The buyer's own id is the
    # latency-independent state the purchase is supposed to reach.
    assert buyer["actor_id_hex"].lower() in subs.subscriber_ids(), (
        f"the unlock tier's roster should name the BUYER "
        f"{buyer['actor_id_hex'][:12]}…, got {subs.subscriber_ids()}; "
        f"error={subs.error_text()!r}"
    )

    # ── Buyer: the sold post now unseals on detail open — the working ────────
    # end-to-end self-serve purchase.
    _login_as(app, request, sell_nest, buyer, handle=buyer_handle)
    assert feed.wait_post_count(1) >= 1, (
        f"buyer's feed should list the sold post; error-message={app.error_text()!r}"
    )
    row = feed.wait_for_post_state_by_text(preview)
    assert row is not None, "buyer's feed should still hold this run's sold post"
    feed.open_post_detail(feed.post_index_by_id(row["post_id"]))
    app.driver.wait_for("feed-post-detail-body")
    deadline = time.monotonic() + 15
    body = ""
    while time.monotonic() < deadline:
        body = feed.post_detail_body()
        if FULL_MARKER in body:
            break
        time.sleep(0.5)
    assert FULL_MARKER in body, (
        f"the buyer should unseal the sold post on detail open via the "
        f"self-serve purchase, got {body!r}; error-message={app.error_text()!r}"
    )


@pytest.mark.feature("paid-posts-and-tips")
def test_sell_post_existing_subscriber_reads_it_free(app, request, sell_nest):
    """The rank knob's DELIVERY (ruled 2026-07-29 after
    the review proved the cascade unreachable for client-minted
    tiers): an EXISTING paid subscriber reads a post sold with the default
    "subscribers get it free" toggle — the exact population the toggle names
    and the one the pre-fix code silently locked out.

    The chain under test: §1 create tier → offers subscribe → §2 approve →
    sell with the toggle ON (creation-time fan-out reconcile enqueues the
    subscriber's mint, payment-marked) → §2 approve the fan-out row (the same
    call `drain_auto_approvals`' background poll would make, driven
    synchronously like the buyer leg above) → the subscriber's detail-open
    UNSEALS. Readability through the UI, never a rank integer."""
    app.subscriptions.require_sell_post_seller_fanout_leg_supported()

    admin_sk = _as_signing_key(sell_nest["admin"]["signing_key"])
    seller = create_actor_and_register(
        sell_nest["port"], base_url=sell_nest["url"], admin_signing_key=admin_sk,
    )
    sub = create_actor_and_register(
        sell_nest["port"], base_url=sell_nest["url"], admin_signing_key=admin_sk,
    )
    seller_handle = _unique("e2e-seller")
    sub_handle = _unique("e2e-sub")

    # ── Seller: §1 create the ordinary paid tier the subscriber will hold. ───
    _login_as(app, request, sell_nest, seller, handle=seller_handle)
    subs = app.subscriptions
    subs.navigate()
    subs.open_tiers_tab()
    subs.create_tier("gold", 2, price_hint="$5/mo")
    assert subs.wait_for_tier("gold"), (
        f"gold should appear in §1; error={subs.error_text()!r}"
    )

    # ── Subscriber: request gold (headless — convention 8's proven-UI-path ───
    # carve-out: the identical subscribe_offer UI mutation is proven by
    # test_profile.py's OTHER-profile offers test; it is unreachable here
    # because linux's nav-patch data fetches keep the FIRST login's client
    # after a set_state account switch, so a second-login actor's contacts/
    # offers surfaces read as the wrong actor — see the 2026-07-29 harness
    # note). ApiActor.subscribe exists for exactly
    # this multi-user shape: a headless second actor subscribes while one
    # actor drives the author UI.
    reply = ApiActor(
        sell_nest["url"], sub["token"], sub["actor_id_hex"],
        bytes(sub["signing_key"]),
    ).subscribe(bytes.fromhex(seller["actor_id_hex"]), "gold")
    assert reply.get("outcome") == "queued", (
        f"a client-minted tier subscribe must queue, got {reply!r}"
    )

    # ── Seller: approve the subscription through §2. ─────────────────────────
    _login_as(app, request, sell_nest, seller, handle=seller_handle)
    subs.navigate()
    subs.open_tiers_tab()
    subs.refresh()
    assert subs.wait_for_pending_request(1), (
        f"the subscribe request should surface in §2; error={subs.error_text()!r}"
    )
    subs.approve_first_request()
    assert subs.wait_for_subscriber(1), (
        f"after approve the subscriber should join gold's roster; "
        f"error={subs.error_text()!r}"
    )

    # ── Seller: NOW sell a post, toggle at its default (subscribers free). ───
    # Back to the feed first — the §2 approve left the app on the profile view
    # and the composer is a feed-view element.
    app.driver.set_state({"nav": {"stack": [{"view": "feed"}]}})
    app.driver.wait_for("compose-text-field")
    feed = app.feed
    feed.sell_post(FULL_BODY, PREVIEW, PRICE)
    assert feed.wait_for_gated_badge_text() is not None, (
        f"a sold post's card should carry gated-post-badge; "
        f"error-message={app.error_text()!r}"
    )

    # ── The creation-time reconcile enqueued the existing subscriber's mint —
    # payment-marked so the drain pump needs no creator judgment. Drive the
    # same approve synchronously through §2 rather than waiting on the poll
    # cadence (the buyer-leg pattern above).
    subs.navigate()
    subs.open_tiers_tab()
    subs.refresh()
    assert subs.wait_for_pending_request(1), (
        f"selling with the toggle ON should enqueue the existing subscriber's "
        f"unlock-tier mint; error={subs.error_text()!r}"
    )
    assert subs.request_paid(0), (
        "the fan-out row must be payment-marked (the drain lane)"
    )
    subs.approve_first_request()

    # ── Subscriber: the sold post unseals on detail open — readability. ──────
    _login_as(app, request, sell_nest, sub, handle=sub_handle)
    assert feed.wait_post_count(1) >= 1, (
        f"subscriber's feed should list the sold post; "
        f"error-message={app.error_text()!r}"
    )
    feed.open_post_detail(0)
    app.driver.wait_for("feed-post-detail-body")
    deadline = time.monotonic() + 15
    body = ""
    while time.monotonic() < deadline:
        body = feed.post_detail_body()
        if FULL_MARKER in body:
            break
        time.sleep(0.5)
    assert FULL_MARKER in body, (
        f"an existing rank-2 subscriber should read a subscribers-free sold "
        f"post without paying again, got {body!r}; "
        f"error-message={app.error_text()!r}"
    )


# ── Residual gap — named so a green run above is not mistaken for full ───────
# coverage on every app: the buyer leg is proven above on linux + android + web
# (the web e2e test-agent's actor-switch bug that used to block it — was fully fixed 2026-07-30), on macos + ios (2026-08-02),
# and on tui, which gained the §5 mint surface with the tui Tiers author track
# (2026-08-01) and the consumer redeem input with `subscription-settings`
# (2026-08-02). **windows is the one app still owed**; widen the buyer test's guard when its lift lands.
