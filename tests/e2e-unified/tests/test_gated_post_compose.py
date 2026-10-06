"""tier_3 e2e for gate-to-tier post authoring + subscriber unlock —
`monetization.md` § Pillars 2+3 (client UX) + `ui/feed.md` § Encryption at
rest; the compose-side leg the web-paywall track named as its prerequisite
("Gated-post AUTHORING is not built on any client" — feed.md § Implementation
status today, closed by this track).

The creator authors a tier-gated post entirely through the composer UI
(`compose-gate-tier-select` + `compose-gate-preview-field`, IDs user-approved
2026-07-12): the client reads the tier's period key from Pillar-1 custody
(the `fauna.state.subscriptions` plane), rides the shared
`fauna_client_core::post::build_gated_post` seal helper, uploads the sealed
full-body blob (`PeriodRestrictedPost` sidecar), and creates the post over
`fauna.posts.create`. The full body NEVER travels or rests in plaintext — the
list card shows the public teaser + the `gated-post-badge`; opening the detail
unseals transparently for an entitled reader (author via custody; subscriber
via KeyBlob entry → `derive_post_key(period_key, seal_id)` →
`decrypt_content`).

Like `test_subscriptions.py`, this runs on its own dedicated nest (every
fresh tier is client-minted — the storage-mode axis is retired) and needs no
mail bridge. The subscriber's subscribe is headless precondition setup
(`ApiActor`); every mutation under test — tier create, gated compose,
approve — is driven through the UI (e2e rule 8). The subscriber's READ leg
re-logs the same app in as the subscriber (set_state login), then opens the
post detail and sees the full body: proof the whole chain ran (custody →
key_blob.get → seal → upload → create → badge projection → KeyBlob-entry
decrypt).

Linux led; web joined via the wasm `FeedManager` gated surface + the Svelte
composer/badge/detail lift (same ui.yaml IDs, priority #1). The remaining four
(windows/macos/ios/android) lift the same IDs after and join the gate below.
"""

import time
import urllib.request
import uuid
from pathlib import Path

import pytest
from nacl.signing import SigningKey

from actions.api_actor import ApiActor
from common.auth import create_actor_and_register
from helpers.budgets import RPC_ROUNDTRIP_S
from helpers.e2e_session import login_as

pytestmark = [pytest.mark.tier_3]

PREVIEW = "Premium post — public teaser paragraph."
FULL_MARKER = "the-full-premium-body-marker"
FULL_BODY = f"Premium post full body: {FULL_MARKER} for subscribers only."

# The same shared fixture the public image-post journeys use — one picture, so
# a difference between the public and gated paths is never the input's fault.
TEST_IMAGE = Path(__file__).parent.parent / "fixtures" / "test-image.png"
PNG_MAGIC = b"\x89PNG\r\n\x1a\n"


def _unique(prefix: str) -> str:
    return f"{prefix}-{uuid.uuid4().hex[:8]}"


def _as_signing_key(raw) -> SigningKey:
    return raw if isinstance(raw, SigningKey) else SigningKey(bytes.fromhex(raw))


@pytest.fixture
def gated_nest(request, nest_mode, tmp_path_factory):
    """A dedicated fresh nest for this suite (same shape as
    test_subscriptions.py's `subs_nest` — own nest, own actors, no shared
    session state, so test order stays non-load-bearing; every fresh tier is
    client-minted, no mode variant exists)."""
    from conftest import _start_dedicated_nest

    nest, cleanup = _start_dedicated_nest(
        request, nest_mode, tmp_path_factory, "gated-compose-nest")
    yield nest
    cleanup()


@pytest.fixture
def gated_spa_url(static_dir, gated_nest):
    """Function-scoped SPA proxy → the dedicated `gated_nest`, so a WEB browser can
    reach it without CORS (the raw nest never sends `Access-Control-Allow-Origin`,
    which blocks the cross-origin blob-upload POST; WS-RPC survives, HTTP does not).
    The session `spa_url` only proxies `nest_instance`; a dedicated-nest web test
    needs its own proxy — the web twin of pointing a native app straight at the
    nest URL. Mirrors `dedicated_mail_spa_url`. Requested lazily (web only), so the
    native lead (linux) never triggers the `static_dir` web build."""
    from conftest import _serve_spa_proxy
    url, server = _serve_spa_proxy(static_dir, gated_nest["url"])
    yield url
    server.shutdown()


def _login_as(app, request, nest, user, *, handle: str) -> None:
    """set_state login landing on the feed view. For web the `node_url` must
    be the SPA proxy (`gated_spa_url`), not the raw nest URL — the browser
    reaches the nest only through a CORS-adding proxy (conftest
    `_serve_spa_proxy` note); native apps use the raw URL. Thin wrapper
    over the shared `helpers.e2e_session.login_as` — see that docstring for
    the barrier rationale (2026-08-10)."""
    login_as(
        app, nest, user, handle=handle, device_id="test-device-gated",
        request=request, spa_url_fixture="gated_spa_url",
    )


@pytest.mark.feature("paid-posts-and-tips")
def test_gate_to_tier_compose_and_subscriber_unlock(app, request, gated_nest):
    admin_sk = _as_signing_key(gated_nest["admin"]["signing_key"])
    author = create_actor_and_register(
        gated_nest["port"], base_url=gated_nest["url"], admin_signing_key=admin_sk,
    )
    subscriber = create_actor_and_register(
        gated_nest["port"], base_url=gated_nest["url"], admin_signing_key=admin_sk,
    )

    _login_as(app, request, gated_nest, author, handle="e2e-gated-author")

    subs = app.subscriptions
    feed = app.feed
    tier = _unique("gold")

    # ── Author: create the paid tier through the Tiers tab (custody records ──
    # the period key client-side — the compose leg reads it from there).
    subs.navigate()
    subs.open_tiers_tab()
    subs.create_tier(tier, rank=1, price_hint="$5/mo")
    assert subs.wait_for_tier(tier), (
        f"created tier {tier!r} should appear in §1 My tiers; error={subs.error_text()!r}"
    )

    # ── Author: compose the gated post through the composer UI. ─────────────
    app.driver.set_state({"nav": {"stack": [{"view": "feed"}]}})
    time.sleep(1.0)
    feed.create_gated_post(FULL_BODY, PREVIEW, tier)

    # The list card carries the tier badge and shows ONLY the teaser.
    # `wait_for_gated_badge_text`/`wait_for_first_post_text`, not bare reads —
    # both race the feed's re-query after the compose call (their own doc
    # comments).
    badge = feed.wait_for_gated_badge_text()
    assert badge is not None and tier in badge, (
        f"gated-post-badge should show the tier name {tier!r}, got {badge!r}"
    )
    assert feed.wait_for_first_post_text(PREVIEW.split()[0]), (
        "list card should show the teaser"
    )
    top = feed.first_post_text()
    assert FULL_MARKER not in top, f"full body leaked into the list card: {top!r}"

    # ── Author opens the detail: custody period key unseals transparently. ──
    feed.open_post_detail(0)
    # The detail dialog opens asynchronously on some clients (windows'
    # PostCard_Click → ContentDialog.ShowAsync); wait for the body element before
    # reading it, so a client whose get_text raises (rather than returns "") on a
    # not-yet-rendered element doesn't crash the unseal poll below. No-op where the
    # dialog is already up (mirrors test_post_detail_opens' post_detail_visible gate).
    app.driver.wait_for("feed-post-detail-body")
    deadline = time.monotonic() + 10
    body = ""
    while time.monotonic() < deadline:
        body = feed.post_detail_body()
        if FULL_MARKER in body:
            break
        time.sleep(0.5)
    # Read `error-message` BEFORE asserting (e2e conventions rule 6 — a failure must
    # diagnose itself): the detail-unseal chain (gated_blob_hash → GET blob →
    # unlock_gated_post → repaint) surfaces every break there, so an unseal that never
    # lands names its own failing link instead of only reporting the stale teaser.
    assert FULL_MARKER in body, (
        f"the author's own gated post should unseal on detail open (custody "
        f"period key), got {body!r}; error-message={app.error_text()!r}"
    )

    # NOTE: the windows skip that used to sit here is GONE. The subscriber leg needs
    # a SECOND `set_state` login in the same app process, which tripped a cross-cutting
    # windows TestAgent bug — the `nav` block of a set_state overwrote the `session`
    # block's queued MainPage hand-off, so MainPage kept the clients `DisposeNestClients()`
    # had just disposed. Fixed by composing the post-actions instead of clobbering
    # (`PostActionChain.Then`); pinned by `test_second_login_live_clients.py`. The old note
    # here claimed it broke "EVERY second windows login" — it did not: `driver.reset()`
    # navigates back to OnboardingPage, so only a second login with NO intervening reset
    # (this test) was ever exposed.

    # ── Subscriber joins the tier: headless subscribe (precondition), the ──
    # author approves through the UI (the mutation under test — the approve
    # mints + uploads the KeyBlob the unlock below reads).
    sub_actor = ApiActor(
        gated_nest["url"], subscriber["token"], subscriber["actor_id_hex"],
        bytes(subscriber["signing_key"]),
    )
    author_id = author["actor_id_bytes"]
    reply = sub_actor.subscribe(author_id, tier)
    assert reply.get("outcome") == "queued", (
        f"client-minted-tier subscribe should queue, got {reply!r}"
    )

    subs.navigate()
    subs.open_tiers_tab()
    subs.refresh()
    assert subs.wait_for_pending_request(1), (
        f"the queued subscribe should surface in §2; error={subs.error_text()!r}"
    )
    subs.approve_first_request()
    assert subs.wait_for_subscriber(1), (
        f"after approve the subscriber should join the roster; error={subs.error_text()!r}"
    )

    # ── Subscriber's client: the gated card renders teaser + badge, and the ──
    # detail unseals via the minted KeyBlob entry.
    _login_as(app, request, gated_nest, subscriber, handle="e2e-gated-subscriber")
    # This assertion's failure mode is "0 posts and NO error", which is the least
    # diagnosable shape there is — it looks identical to "this actor's feed is
    # legitimately empty" (e2e rule 6: a failure must diagnose itself). Where the
    # driver captures the browser console ring, attach it: it is what finally
    # distinguished the causes on web, showing the post-switch session ending on
    # `fauna_rpc_wasm::client: ws client closed; reconnect loop exiting` rather
    # than on any thrown error. Diagnostic only — never an assertion — so a driver
    # without the ring simply contributes nothing.
    console_tail = ""
    if hasattr(app.driver, "console_log"):
        console_tail = "\nbrowser console (tail):\n" + "\n".join(app.driver.console_log()[-60:])
    assert feed.wait_post_count(1) >= 1, (
        f"subscriber's feed should list the gated post; "
        f"error-message={app.error_text()!r}{console_tail}"
    )
    assert feed.wait_for_gated_badge_text() is not None, (
        "the gated post's card should carry gated-post-badge for the subscriber"
    )
    sub_top = feed.first_post_text()
    assert FULL_MARKER not in sub_top, (
        f"full body leaked into the subscriber's list card: {sub_top!r}"
    )

    feed.open_post_detail(0)
    app.driver.wait_for("feed-post-detail-body")  # see the author open above (async dialog)
    deadline = time.monotonic() + 15
    sub_body = ""
    while time.monotonic() < deadline:
        sub_body = feed.post_detail_body()
        if FULL_MARKER in sub_body:
            break
        time.sleep(0.5)
    assert FULL_MARKER in sub_body, (
        f"the subscriber should unseal the gated post on detail open (KeyBlob "
        f"entry → derive_post_key → decrypt_content), got {sub_body!r}"
    )


@pytest.mark.android
@pytest.mark.web
@pytest.mark.windows
@pytest.mark.macos
@pytest.mark.ios
@pytest.mark.linux
@pytest.mark.tui
@pytest.mark.feature("paid-posts-and-tips")
def test_gated_post_attachment_is_sealed_and_never_uploaded_in_plaintext(
    app, request, gated_nest,
):
    """A photo attached to a tier-gated post is SEALED on the way out — the
    compose leg of `ui/media.md` § Encryption at rest (*one per-post key seals
    the post body and all attachments together*).

    Three properties; the second is the one this test was written for, and the
    third is its read-side twin:

    1. The photo survives the gate. Until 2026-09-07 the composer silently
       dropped it (`prepare_gated_blob` never read `compose.attached_file`, and
       `build_gated_post` hardcoded `PostBody::Text`), so the author published a
       caption with no picture and no error.
    2. **No plaintext copy of it is ever POSTed.** Every app used to upload a
       `PublicPost` (plaintext) blob first and branch on the gate afterwards, so
       a restricted post's photo sat on the nest readable by anyone with the
       hash. Blob `GET` is unauthenticated by design and the nest exposes no
       blob DELETE, so the only achievable form of "no plaintext copy is
       fetchable" is *never upload one* — which is why the assertion below is on
       the bytes the store actually holds, fetched with no credentials at all.
    3. **The opened photo PAINTS.** Sealing is only half the round trip: a
       reader that hands the sealed item to its decoder unopened shows the
       placeholder with no error, which is how a gated photo post rendered its
       caption over a blank card until 2026-09-07. A public photo paints the same
       down either path, so only this journey can see the difference, and only
       through a headless paint read (`FeedActions.post_detail_image_painted`).

    The seal lives in shared Rust (`FeedManager::seal_compose_attachment`)
    because a tier's period key must never cross the FFI/wasm boundary, so this
    journey covers every app that adopts the helper. Web led rather than tui —
    the usual lead app — because this suite's author leg drives the §1 Tiers tab,
    which tui lacked when it was written. **tui joined 2026-09-21**: its Tiers
    tab had landed on 2026-08-01, and the detail paint read below had never
    matched on tui, whose detail registered its picture outside
    `feed-post-detail-dialog` until the same day. **windows joined 2026-09-07**, the first of
    the five apps that used to POST a `PublicPost` blob before the audience
    branch; it is author-only, so the second-login disposed-client blocker that
    guards this suite's *subscriber* leg on windows does not reach it. **macOS
    and iOS joined 2026-09-08**: they were the two apps that uploaded at *pick*
    time rather than at submit, so the deferral had to land before the seal was
    even expressible — the composer's audience is unknown when the picker
    returns.
    """
    if not TEST_IMAGE.exists():
        pytest.skip("Test image fixture not found")

    admin_sk = _as_signing_key(gated_nest["admin"]["signing_key"])
    author = create_actor_and_register(
        gated_nest["port"], base_url=gated_nest["url"], admin_signing_key=admin_sk,
    )
    _login_as(app, request, gated_nest, author, handle="e2e-gated-photo")

    subs = app.subscriptions
    feed = app.feed
    tier = _unique("gold")

    subs.navigate()
    subs.open_tiers_tab()
    subs.create_tier(tier, rank=1, price_hint="$5/mo")
    assert subs.wait_for_tier(tier), (
        f"created tier {tier!r} should appear in §1 My tiers; error={subs.error_text()!r}"
    )

    # Tier first, then the file — the order the app itself must follow, since
    # the composer's audience is what decides the seal (see create_gated_post).
    app.driver.set_state({"nav": {"stack": [{"view": "feed"}]}})
    # A causal anchor, not a settle sleep: the nav patch is asynchronous, so
    # wait on the state the composer needs rather than on a duration (the
    # sibling journey above still sleeps 1s here — convention 14 debt, not a
    # pattern to copy).
    app.driver.wait_for("feed-view")
    feed.create_gated_post(FULL_BODY, PREVIEW, tier, image_path=str(TEST_IMAGE))

    # The author unseals their own post from custody; the opened body is what
    # carries the item, so the image appears only on this side of the unlock.
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
        f"the author's own gated post should unseal on detail open, got {body!r}; "
        f"error-message={app.error_text()!r}"
    )

    # Keyed on FULL_BODY, not the teaser: `post_image_blob_hash_by_text` matches
    # `body` exactly, and the unlock replaced the card's teaser with the opened
    # body (`fold_unlocked_body`, which is also what sets `has_media` /
    # `media_hash` — the nest's own projection cannot, since the item is named
    # only inside a body it can never read).
    blob_hash = feed.post_image_blob_hash_by_text(FULL_BODY)
    assert len(blob_hash) == 64, (
        f"the unsealed gated post should carry its attachment as a MediaItem — a "
        f"64-hex blob hash, got {blob_hash!r}. An empty hash means the compose leg "
        f"dropped the attachment again (PostBody::Text instead of TextWithMedia); "
        f"posts={app.driver.get_state('data.feed.posts')!r} "
        f"error-message={app.error_text()!r}"
    )

    # The property. Fetched with NO credentials, exactly as any stranger holding
    # the hash would: the stored bytes must be ciphertext, not the PNG.
    with urllib.request.urlopen(
        urllib.request.Request(
            f"{gated_nest['url']}/api/v1/blob/{blob_hash}", method="GET",
        ),
        timeout=RPC_ROUNDTRIP_S,
    ) as resp:
        stored = resp.read()
    assert stored[: len(PNG_MAGIC)] != PNG_MAGIC, (
        "the blob a tier-restricted post names is readable PNG plaintext — the "
        "composer uploaded before resolving the audience, so a copy of the picture "
        "is fetchable by anyone with the hash and no blob DELETE exists to remove "
        f"it. first bytes={stored[:16]!r}"
    )
    assert len(stored) >= 28, (
        f"a ChaCha20-Poly1305 sealed blob is at least 28 bytes (nonce + tag); got "
        f"{len(stored)}B — that is not a seal"
    )

    # The READ side, which nothing above reaches: the opened bytes must become a
    # PAINTED picture. An app that hands the sealed item to its decoder unopened
    # fails the decode and shows the placeholder with no error anywhere — the
    # blank card this post's photo showed before the reader existed. A public
    # photo paints identically down either path, so only a gated one can tell
    # them apart. Read on the detail the author just unlocked: it is where the
    # unlock lands, and the list card is not on screen beneath it on every app
    # (linux swaps pages), so a card read here would be asking about nothing.
    assert feed.post_detail_image_painted(), (
        "the unlocked gated post's photo should PAINT on its post detail, not stay "
        "the placeholder — the item did not open before decoding, or its opened "
        f"bytes never reached the picture; paint={feed.post_image_paint_report()} "
        f"error-message={app.error_text()!r}"
    )


@pytest.mark.feature("feed-images-and-video")
def test_a_restricted_post_s_picture_opens_for_an_entitled_reader(app, request, gated_nest):
    """A picture on an audience-restricted post opens for a reader the post is
    for — a subscriber of its tier — and never paints as scrambled bytes
    (`docs/goal/ui/media.md` § Encryption at rest: one per-post key seals the
    body and every attachment, so whoever can open the body can open the
    picture).

    The attachment journey above asserts the paint on the AUTHOR's side, who
    opens the post through their own custody of the tier's period key. A reader
    opens it through a different chain — the KeyBlob entry the author's approval
    minted for them — so this seats one: the author creates the tier and
    approves the subscriber in the UI, posts the gated picture through the
    composer, and the same app then signs in as the subscriber, opens the post
    and must see the picture PAINT. A sealed item handed to the decoder unopened
    paints the placeholder, never a picture — which is the difference this reads.
    """
    admin_sk = _as_signing_key(gated_nest["admin"]["signing_key"])
    author = create_actor_and_register(
        gated_nest["port"], base_url=gated_nest["url"], admin_signing_key=admin_sk,
    )
    subscriber = create_actor_and_register(
        gated_nest["port"], base_url=gated_nest["url"], admin_signing_key=admin_sk,
    )
    _login_as(app, request, gated_nest, author, handle=_unique("e2e-picture-author"))

    subs = app.subscriptions
    feed = app.feed
    tier = _unique("gold")

    # ── Author: the tier, and the subscriber approved into it through the UI
    # (the approval mints the KeyBlob entry the reader opens the post with). ──
    subs.navigate()
    subs.open_tiers_tab()
    subs.create_tier(tier, rank=1, price_hint="$5/mo")
    assert subs.wait_for_tier(tier), (
        f"created tier {tier!r} should appear in §1 My tiers; error={subs.error_text()!r}"
    )
    sub_actor = ApiActor(
        gated_nest["url"], subscriber["token"], subscriber["actor_id_hex"],
        bytes(subscriber["signing_key"]),
    )
    reply = sub_actor.subscribe(author["actor_id_bytes"], tier)
    assert reply.get("outcome") == "queued", f"subscribe should queue, got {reply!r}"
    subs.refresh()
    assert subs.wait_for_pending_request(1), (
        f"the queued subscribe should surface in §2; error={subs.error_text()!r}"
    )
    subs.approve_first_request()
    assert subs.wait_for_subscriber(1), (
        f"after approve the subscriber should join the roster; error={subs.error_text()!r}"
    )

    # ── Author: the gated post, picture attached after the audience is picked. ──
    app.driver.set_state({"nav": {"stack": [{"view": "feed"}]}})
    app.driver.wait_for("compose-text-field")
    feed.create_gated_post(FULL_BODY, PREVIEW, tier, image_path=str(TEST_IMAGE))

    # ── Reader: the same app, signed in as the subscriber. ──
    _login_as(app, request, gated_nest, subscriber, handle=_unique("e2e-picture-reader"))
    assert feed.wait_post_count(1) >= 1, (
        f"the subscriber's feed should list the gated post; error={app.error_text()!r}"
    )
    feed.open_post_detail(0)
    app.driver.wait_for("feed-post-detail-body")
    opened = ""
    deadline = time.monotonic() + RPC_ROUNDTRIP_S
    while time.monotonic() < deadline:
        opened = feed.post_detail_body()
        if FULL_MARKER in opened:
            break
        time.sleep(0.3)
    assert FULL_MARKER in opened, (
        f"the subscriber should open the gated post's body first, got {opened!r}; "
        f"error-message={app.error_text()!r}"
    )
    assert feed.post_detail_image_painted(), (
        "the entitled reader's opened post must PAINT its picture, not show the "
        "placeholder a sealed item leaves — the reader's key opened the body but "
        "not the attachment, or its opened bytes never reached the picture; "
        f"paint={feed.post_image_paint_report()} error-message={app.error_text()!r}"
    )
    # What the reader painted rests SEALED on the nest — so the paint above is
    # an opened picture, not a public one the decoder took as it came.
    blob_hash = feed.post_image_blob_hash_by_text(FULL_BODY)
    assert len(blob_hash) == 64, f"expected a 64-hex blob hash, got {blob_hash!r}"
    with urllib.request.urlopen(
        f"{gated_nest['url']}/api/v1/blob/{blob_hash}", timeout=RPC_ROUNDTRIP_S
    ) as resp:
        assert resp.read()[:8] != PNG_MAGIC, (
            "the restricted post's picture must rest sealed on the nest, never as "
            "the plain PNG"
        )
