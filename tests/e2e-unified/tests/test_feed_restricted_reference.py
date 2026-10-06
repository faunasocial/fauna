"""tier_3: words under a restricted post you cannot write for are refused, and
nothing is posted; a wordless repost or quote of it still works.

``docs/goal/ui/feed.md`` § Encryption at rest → *A reply, quote or repost of a
restricted post*, rulings 3 and 6. A reply is an ordinary post of the person
who makes it, fanned out to THEIR followers, so words typed under someone
else's paid post would cross from that post's buyers to everyone. Until ruling
5's public-by-confirmation arm is built, such words are refused with the reason
(`REFERENCE_REFUSED_RESTRICTED`, painted as `feed.reference_restricted`) before
anything is created. A repost and a commentary-less quote carry no words, so
they stay public and still work (ruling 3).

The restricted post is a SOLD post (`sell_post`, the test_sell_post.py
pattern): selling auto-mints its own tier, so the lead app can author one with
no author-side subscriptions surface, and a second account on the same nest
sees the teaser. That reader holds no seat under the post's arm (it is neither
the tier's owner nor a room member), so the shared manager answers its reply
"public only by confirmation", which today is the refusal.

**Quote with words has no UI path on any app yet**: the quote button fires a
commentary-less quote (feed.md § Interaction bar; the commentary composer is a
deferred fleet-wide follow-on). Its refusal is pinned in shared Rust
(`fauna-feed`'s `words_under_a_restricted_post_are_refused_and_nothing_is_created`,
reply and quote); this journey drives the words a user CAN type today, the
reply, and the wordless quote and repost.
"""

import uuid

import pytest
from nacl.signing import SigningKey

from common.auth import create_actor_and_register
from helpers.e2e_session import login_as
from helpers.waiting import wait_until
from i18n.strings import S

pytestmark = [pytest.mark.tier_3]

PREVIEW = "A teaser anyone can read."
FULL_BODY = "The paid part, for buyers only."
PRICE = "$2"

# How long the refusal may take to reach the page. A ceiling for a broken
# surface, never a subject (convention 14): the poll returns on the first read
# that carries it.
REFUSAL_SURFACE_BUDGET_S = 30.0


def _unique(prefix: str) -> str:
    return f"{prefix}-{uuid.uuid4().hex[:8]}"


def _as_signing_key(raw) -> SigningKey:
    return raw if isinstance(raw, SigningKey) else SigningKey(bytes.fromhex(raw))


@pytest.fixture
def restricted_nest(request, nest_mode, tmp_path_factory):
    """A dedicated fresh nest, so the reader's feed holds only the sold post and
    test order stays non-load-bearing (the `test_sell_post.py::sell_nest`
    shape)."""
    from conftest import _start_dedicated_nest

    nest, cleanup = _start_dedicated_nest(
        request, nest_mode, tmp_path_factory, "restricted-reference-nest")
    yield nest
    cleanup()


@pytest.fixture
def restricted_spa_url(static_dir, restricted_nest):
    """Function-scoped SPA proxy → `restricted_nest` for a web leg; requested
    lazily, so a native run never triggers the web build."""
    from conftest import _serve_spa_proxy
    url, server = _serve_spa_proxy(static_dir, restricted_nest["url"])
    yield url
    server.shutdown()


def _login_as(app, request, nest, user, *, handle: str) -> None:
    login_as(
        app, nest, user, handle=handle, device_id="test-device-restricted-ref",
        request=request, spa_url_fixture="restricted_spa_url",
        wait_for_id="compose-text-field",
    )


@pytest.mark.macos
@pytest.mark.ios
@pytest.mark.web
@pytest.mark.tui
@pytest.mark.windows
@pytest.mark.linux
@pytest.mark.feature("feed-interactions")
def test_words_under_a_restricted_post_are_refused_and_a_bare_repost_or_quote_still_works(
    app, request, restricted_nest
):
    app.subscriptions.require_sell_post_authoring_supported()

    admin_sk = _as_signing_key(restricted_nest["admin"]["signing_key"])
    seller = create_actor_and_register(
        restricted_nest["port"], base_url=restricted_nest["url"],
        admin_signing_key=admin_sk,
    )
    reader = create_actor_and_register(
        restricted_nest["port"], base_url=restricted_nest["url"],
        admin_signing_key=admin_sk,
    )
    feed = app.feed

    # ── The seller sells a post: a restricted post the reader cannot write for.
    _login_as(app, request, restricted_nest, seller, handle=_unique("e2e-seller"))
    feed.sell_post(FULL_BODY, PREVIEW, PRICE)
    assert feed.wait_for_gated_badge_text() is not None, (
        f"the sold post's card should carry gated-post-badge; "
        f"error={app.error_text()!r}"
    )

    # ── The reader finds its teaser.
    _login_as(app, request, restricted_nest, reader, handle=_unique("e2e-reader"))
    row = feed.wait_for_post_state_by_text(PREVIEW)
    assert row is not None and row.get("post_id"), (
        f"the reader's feed should list the sold post's teaser; "
        f"error={app.error_text()!r}"
    )
    target_id = row["post_id"]
    assert app.error_text() == "", (
        f"the reader's feed page must start with no error; got {app.error_text()!r}"
    )

    # ── Words under it: refused, with the reason.
    words = _unique("words-that-must-not-publish")
    feed.reply_post(words, index=feed.post_index_by_id(target_id))

    def the_refusal():
        shown = app.error_text()
        return shown if S.feed.reference_restricted in shown else None

    wait_until(
        the_refusal,
        REFUSAL_SURFACE_BUDGET_S,
        diagnose=lambda: f"error_text={app.error_text()!r}",
    )

    # ── And nothing was posted: a fresh re-query of the feed on screen holds
    # the sold post with its reply count unmoved, and the words nowhere. Asserted
    # on this test's own posts, never the whole feed: on a populated box (live
    # mode) the reader's feed carries everyone else's posts too.
    feed.return_to_feed_page()
    posts = feed._feed_posts_from_state()
    assert not any(words in (p.get("body") or "") for p in posts), (
        f"a refused reply must create no post; the reader's feed holds "
        f"{[(p.get('post_id'), p.get('body')) for p in posts]!r}"
    )
    target = next((p for p in posts if p.get("post_id") == target_id), None)
    assert target is not None, (
        f"the sold post must still be on the reader's feed; it holds "
        f"{[p.get('post_id') for p in posts]!r}"
    )
    assert target.get("reply_count") == 0, (
        f"a refused reply must not move the target's reply count; row={target!r}"
    )

    # ── A bare repost still works (ruling 3: no words, so it stays public).
    feed.repost_post(feed.post_index_by_id(target_id))
    assert feed.wait_for_interaction_count_by_id(target_id, "repost", 1) == 1, (
        f"a wordless repost of a restricted post must still land; "
        f"error={app.error_text()!r}"
    )

    # ── So does a bare quote (today's quote button carries no commentary).
    feed.driver.click(
        "feed-quote-button", scope=f"post-card[{feed.post_index_by_id(target_id)}]"
    )
    assert feed.wait_for_interaction_count_by_id(target_id, "quote", 1) == 1, (
        f"a commentary-less quote of a restricted post must still land; "
        f"error={app.error_text()!r}"
    )
