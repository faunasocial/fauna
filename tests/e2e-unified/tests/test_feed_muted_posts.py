"""Muted keywords, feed side — a matching post collapses behind a reveal.

``docs/goal/behavior/moderation.md`` § Muted keywords;
``docs/goal/behavior/topic-factors.md`` § Scoring (a mute **collapses
everywhere** — chronological feeds included, not just score-ordered ones);
``tests/e2e-unified/ui.yaml`` ``feed-post-muted`` / ``feed-post-muted-reveal-button``
(IDs user-approved 2026-07-12).

The feed twin of ``test_muted_words.py``'s conversation leg: there a decrypted
DM collapses behind ``dm-message-muted``; here a feed post collapses behind
``feed-post-muted``. Same shared list (``fauna.state.moderation`` ``muted_keywords``),
same session-local one-tap reveal, same hide-not-flag semantics (the mute never
feeds the moderation queue).

tier_3, not tier_2: the collapse predicate is ``FeedManager::is_muted``, which
reads the ``MutedKeywords`` entry that ``load_sealed_scorers`` installs during a
real ``fetch_page``. ``inject_posts`` replaces the snapshot without ever taking
that path, so an injected post can never collapse — the leg needs real posts and
a real fetch (the same sealed-seam constraint ``test_trained_topics.py`` calls
out).

Rollout: linux first, then web (S8, 2026-07-12), windows (S8, 2026-07-13),
macos/ios (S8, 2026-07-15), tui (2026-07-29 — the 7th client, over the same
shared ``FeedManager::is_muted`` predicate in direct Rust); android markers land
with its own S8 lift.
"""
import time
import uuid

import pytest

pytestmark = pytest.mark.tier_3

# Distinctive enough that no other module's post can collide with it — the
# actor is session-scoped, so a term left muted would follow the suite around
# (the test removes it again in `finally`, but the term is inert regardless).
MUTED_TERM = "zzspoiler"
MUTED_BODY = f"the finale twist {MUTED_TERM} everyone dies at the end"
CLEAN_BODY = "sourdough starter finally doubled overnight"


def _unique(prefix: str) -> str:
    return f"{prefix}-{uuid.uuid4().hex[:8]}"


def _wait_muted_count(app, expected: int, timeout: float = 20.0) -> int:
    """Poll until the collapsed-post count settles. The mute lands in the
    sealed scorers on the feed's next real fetch, which is asynchronous."""
    deadline = time.monotonic() + timeout
    count = app.driver.count("feed-post-muted")
    while count != expected and time.monotonic() < deadline:
        time.sleep(0.3)
        count = app.driver.count("feed-post-muted")
    return count


@pytest.mark.linux
@pytest.mark.web
@pytest.mark.windows
@pytest.mark.macos
@pytest.mark.ios
@pytest.mark.tui
# iOS enabled 2026-07-17. The old skip blamed the scroll-to-element gap,
# but that was wrong: only 2 posts seed here, both on-screen, so nothing is
# off-screen. The real, previously-untested bug was that iOS's feed never
# reloaded its sealed scorers on a nav-back from Settings, so the mute never
# applied (`is_muted` stayed false). Fixed in two places: FeedListView now
# re-pulls on the `selectedTab` change (a TabView keeps tabs mounted, so
# `.onAppear` didn't re-fire), and FeedVM.rehydrate re-pulls the local/trending
# feed too, not only a custom feed.
@pytest.mark.feature("muted-words")
def test_muted_post_collapses_behind_reveal(logged_in_app):
    """A feed post whose body matches a muted term collapses behind
    ``feed-post-muted``; a non-matching post in the same feed stays visible;
    the reveal button un-collapses it for the session (the mute itself stays)."""
    app = logged_in_app
    d = app.driver

    try:
        # ── 1. Seed two real posts (one matching, one clean) BEFORE the mute,
        # so the collapse is provably a render-time decision over the sealed
        # list — not a compose-time filter ────────────────────────────────
        d.set_state({"nav": {"stack": [{"view": "feed"}]}})
        clean_body = f"{CLEAN_BODY} {_unique('loaf')}"
        app.feed.create_post(text=clean_body)
        app.feed.create_post(text=MUTED_BODY)
        assert app.feed.wait_post_count(2) >= 2, (
            f"both seeded posts should render pre-mute; error={app.error_text()!r}"
        )
        assert d.count("feed-post-muted") == 0, (
            "nothing is muted yet — no post may collapse"
        )

        # ── 2. Mute the term through the real Settings UI (the mutation under
        # test goes through the client — e2e point 8) ──────────────────────
        app.muted_words.navigate()
        app.muted_words.add(MUTED_TERM)
        assert app.muted_words.wait_for_word(MUTED_TERM), (
            f"muted term did not persist; words={app.muted_words.words()!r}"
        )

        # ── 3. Back to the feed: the next fetch reloads the sealed scorers, so
        # the matching post collapses and the clean one does not ────────────
        d.set_state({"nav": {"stack": [{"view": "feed"}]}})
        muted = _wait_muted_count(app, 1)
        assert muted == 1, (
            "exactly the matching post should collapse behind the muted "
            f"placeholder; muted={muted} error={app.error_text()!r}"
        )
        assert d.is_visible("feed-post-muted-reveal-button"), (
            "a collapsed post is missing its reveal button"
        )
        # The clean post is untouched — a mute hides the matching post only.
        texts = [app.feed.post_text(i) for i in range(app.feed.post_count())]
        assert any(clean_body in (t or "") for t in texts), (
            f"the non-matching post must stay visible; texts={texts!r}"
        )
        assert not any(MUTED_TERM in (t or "") for t in texts), (
            f"the collapsed post must not leak its body; texts={texts!r}"
        )

        # ── 4. Reveal is session-local: the body appears, the placeholder goes,
        # and the term stays muted (this un-collapses this one instance) ────
        d.click("feed-post-muted-reveal-button")
        assert _wait_muted_count(app, 0) == 0, (
            "the placeholder should be gone after reveal"
        )
        revealed = [app.feed.post_text(i) for i in range(app.feed.post_count())]
        assert any(MUTED_TERM in (t or "") for t in revealed), (
            f"reveal should show the muted body; texts={revealed!r}"
        )
        app.muted_words.navigate()
        assert app.muted_words.wait_for_row_count(1), (
            "revealing one post must not un-mute the term"
        )
    finally:
        # The actor is session-scoped — never leave the term muted for the
        # modules that run after this one.
        try:
            app.muted_words.navigate()
            while app.muted_words.row_count() > 0:
                app.muted_words.remove(0)
        except Exception:  # noqa: BLE001 — best-effort cleanup
            pass


@pytest.mark.web
@pytest.mark.linux
@pytest.mark.tui
@pytest.mark.windows
@pytest.mark.android
@pytest.mark.feature("muted-words")
def test_a_word_inside_a_longer_word_collapses_and_removing_it_brings_the_post_back(
    logged_in_app,
):
    """Two promises of the muted-word match, driven end to end on one post
    (``content-moderation-and-ranking.md`` § Resolved design decisions — the
    match is a case-insensitive SUBSTRING; § Composition — the mute is the
    user's lens, so taking the word out takes the lens off):

    - a post whose body contains the muted word only inside a longer word
      collapses behind ``feed-post-muted``;
    - removing the word in Settings — the mutation under test, not cleanup —
      brings the post back with its body on the next feed visit."""
    app = logged_in_app
    d = app.driver
    term = _unique("zzsub").replace("-", "")
    # The term never stands alone: it is the middle of one longer word.
    embedded_body = f"spoilers ahead: pre{term}post was the twist"

    try:
        d.set_state({"nav": {"stack": [{"view": "feed"}]}})
        app.feed.create_post(text=embedded_body)
        assert app.feed.wait_post_count(1) >= 1, (
            f"the seeded post should render; error={app.error_text()!r}"
        )

        app.muted_words.navigate()
        app.muted_words.add(term)
        assert app.muted_words.wait_for_word(term), (
            f"muted term did not persist; words={app.muted_words.words()!r}"
        )

        d.set_state({"nav": {"stack": [{"view": "feed"}]}})
        assert _wait_muted_count(app, 1) == 1, (
            f"a word muted inside a longer word must still collapse the post; "
            f"error={app.error_text()!r}"
        )
        texts = [app.feed.post_text(i) for i in range(app.feed.post_count())]
        assert not any(term in (t or "") for t in texts), (
            f"the collapsed post must not leak its body; texts={texts!r}"
        )

        # ── Remove the word: the post comes back.
        app.muted_words.navigate()
        app.muted_words.remove(app.muted_words.words().index(term))
        deadline = time.monotonic() + 12.0
        while term in app.muted_words.words() and time.monotonic() < deadline:
            time.sleep(0.3)
        assert term not in app.muted_words.words(), (
            f"the remove did not land; words={app.muted_words.words()!r}"
        )
        d.set_state({"nav": {"stack": [{"view": "feed"}]}})
        assert _wait_muted_count(app, 0) == 0, (
            f"removing the word must bring the post back; error={app.error_text()!r}"
        )
        deadline = time.monotonic() + 20.0
        texts = []
        while time.monotonic() < deadline:
            texts = [app.feed.post_text(i) for i in range(app.feed.post_count())]
            if any(embedded_body in (t or "") for t in texts):
                break
            time.sleep(0.3)
        assert any(embedded_body in (t or "") for t in texts), (
            f"the post's body must render again; texts={texts!r}"
        )
    finally:
        try:
            app.muted_words.navigate()
            while app.muted_words.row_count() > 0:
                app.muted_words.remove(0)
        except Exception:  # noqa: BLE001 — best-effort cleanup
            pass
