"""Engagement cues — Layer A end-to-end (engagement-cues.md §§ Cue vocabulary /
At rest / Layer A; topic-factors.md § Training signals).

Create a trained topic → flip its "Learn from my activity" toggle ON → compose
a feed dominated by it → seed real posts → **dwell on the on-topic card**: an
HONEST viewport exposure, held ≥ CUE_DWELL_LONG_MS at long-visibility by
scrolling the client's REAL scroll position (nothing injected, no clock
shortcut) → the shared engine derives a non-media ``watch-complete`` when the
card leaves the viewport → Layer A weak-trains the factor → the composed
ordering shifts → the debounced ``cues:v1`` rollup flushes on the REAL window
close → survives the restart and is fetchable the way a second device would →
"Clear activity data" deletes it.

tier_3 throughout: a real ``fauna-nest`` binary, real posts, a real
``order=score`` fetch, and a real close-request. ``inject_posts`` bypasses the
sealed-compose seam and must never stand in for the re-rank leg.

Selectivity note: the off-topic posts are seeded as IMAGE posts. A media
item's ``watch-complete`` gate is playback (``media_played_pm ≥ 850‰``), and
linux renders feed media as stills with no playback surface — so an off-topic
card can NEVER dwell-complete no matter how co-visible it is while the
on-topic card is dwelled (a completion there would weak-train the factor on
off-topic text and erase the rank differential). Its long exposure derives no
verdict at all (dwell ≥ CUE_SKIP_MS rules out ``skip``); only the non-media
on-topic post can complete via the dwell gate. This is the same media/dwell
asymmetry the goal doc specifies, used as the test's selectivity mechanism —
no viewport-geometry assumptions needed.

linux and tui run the full journey below (tui since 2026-09-19: its capture
shell reads the frame the terminal actually painted, and its targeted scroll
moves the focus ring the viewport follows — `apps/fauna-tui/src/feed/cues.rs`);
windows, apple and web run narrower slices further down (the media playback
gate needs a video-capable client). tui renders feed media as half-block stills
with no playback surface, so the image-post selectivity note holds there
unchanged.

A separate, capture-free test below proves the toggle + clear-button wiring on
every app.
"""
import sqlite3
import time
import uuid
from pathlib import Path

import pytest

from helpers.budgets import APP_EXIT_S
from tests.api import ws_api

pytestmark = pytest.mark.tier_3

# Distinctive on-topic vocabulary — deliberately SHORT (one line): the dwelled
# card must sit fully visible (≥75%) while centered.
CAT_TEXT = "tabby kitten purring whiskers softly by the warm window"

# fauna_core::scoring::cues — the shared thresholds the engine derives by.
CUE_DWELL_LONG_S = 8.0  # CUE_DWELL_LONG_MS
CUES_ROLLUP_FACTOR = "cues:v1"  # CUES_ROLLUP_FACTOR_V1


def _unique(prefix: str) -> str:
    return f"{prefix}-{uuid.uuid4().hex[:8]}"


def _click_with_retry(driver, element_id: str, *, attempts: int = 5, delay: float = 1.0) -> None:
    """Retry a click through the FlaUI bridge's transient physical-input
    "Access is denied" — Windows 11 sandboxes SendInput from a non-foreground
    process, so the bridge's own `ForegroundApp`/`AttachThreadInput`
    workaround usually clears it but can still lose the race under
    machine-wide UI contention (the same standing fact
    `actions/conversations.py::accept_recipient_chip` documents and works
    around a different way). Retries only the bridge's own transient 500 —
    never a substitute for the element actually being findable/enabled."""
    last_exc: Exception | None = None
    for _ in range(attempts):
        try:
            driver.click(element_id)
            return
        except Exception as exc:  # noqa: BLE001 - retrying a transient bridge 500
            last_exc = exc
            time.sleep(delay)  # sleep-ok: retry backoff after a transient bridge 500, not a settle-wait
    assert last_exc is not None
    raise last_exc


FIXTURE_DIR = Path(__file__).parent.parent / "fixtures"
TEST_IMAGE = FIXTURE_DIR / "test-image.png"


def _wait_model_blob(port: int, actor: dict, factor: str, timeout: float = 20.0,
                     want_present: bool = True) -> dict:
    """Poll ``fauna.personalization.model.fetch`` until a sealed blob is
    present (or absent, for the delete leg) — the deterministic nest-side gate
    that a client seal-and-put (or delete) landed."""
    deadline = time.monotonic() + timeout
    reply = ws_api.personalization_model_fetch(port, actor, factor)
    while bool(reply.get("sealed_blob")) != want_present and time.monotonic() < deadline:
        time.sleep(0.3)
        reply = ws_api.personalization_model_fetch(port, actor, factor)
    return reply


def _quit_through_the_apps_own_leave_door(app) -> None:
    """Quit the way a user of this app does, so the app's own quit-time flushes
    run: linux's window close (the real close-request handler), tui's sidebar
    `exit-tab` (a terminal has no window to close — `apps/tui.md` § Sidebar quit
    row). Either way the process then exits on its own."""
    if app.driver.is_tui():
        app.driver.click("exit-tab")
    else:
        app.driver.window_close()


def _wait_top_post_contains(app, needle: str, timeout: float = 15.0) -> str:
    """Poll until the top rendered post's text contains ``needle`` (the
    manager re-ranks asynchronously: verdict → train → re-seal → put →
    re-rank → notify → row rebuild)."""
    deadline = time.monotonic() + timeout
    top = app.feed.first_post_text()
    while needle not in top and time.monotonic() < deadline:
        time.sleep(0.3)
        top = app.feed.first_post_text()
    return top


@pytest.mark.linux
@pytest.mark.tui
@pytest.mark.feature("learn-from-my-activity")
def test_engagement_dwell_derives_watch_complete_reranks_and_rollup_round_trips(
    logged_in_app, nest_instance, test_user
):
    app = logged_in_app
    port = nest_instance["port"]

    # ── 1. Trained topic + the Layer-A opt-in ────────────────────────────
    app.personalization.navigate()
    topic_name = _unique("Cats")
    app.personalization.create_sole_topic(topic_name)
    factor_key = app.personalization.topic_factor_key()

    # Off per v1 until the user opts in — and it deliberately STAYS off
    # through all of setup: capture is live from login (the engine folds
    # verdicts for whatever the user scrolls past while seeding), and any
    # setup-phase verdict would either weak-train the factor early or eat
    # the dwell's verdict TRANSITION (old == new trains nothing). The toggle
    # flips on, and the engine resets, right before the dwell (step 4).
    assert not app.personalization.engagement_toggle_on(0)

    # ── 2. Seed real posts. Oldest → newest: off-A, CAT, off-B, off-C, so
    # the pre-train created_at DESC order is C, B, CAT, A — the on-topic card
    # sits mid-list (provably not leading). The off-topic posts are IMAGE
    # posts: media items cannot dwell-complete on linux (selectivity note in
    # the module doc), so only the on-topic text post can derive the verdict ─
    assert TEST_IMAGE.exists(), f"fixture image missing: {TEST_IMAGE}"
    app.driver.set_state({"nav": {"stack": [{"view": "feed"}]}})
    app.feed.create_post_with_image(
        text=f"quarterly budget spreadsheet {_unique('rows')}",
        image_path=str(TEST_IMAGE),
    )
    time.sleep(1.2)  # strictly-older created_at (epoch-seconds resolution)
    app.feed.create_post(text=CAT_TEXT)
    time.sleep(1.2)
    app.feed.create_post_with_image(
        text=f"garage door spring {_unique('note')}", image_path=str(TEST_IMAGE)
    )
    time.sleep(1.2)
    app.feed.create_post_with_image(
        text=f"commuter rail timetable {_unique('stop')}", image_path=str(TEST_IMAGE)
    )

    # ── 3. Compose a feed dominated by the trained factor ────────────────
    feed_name = _unique("cats-feed")
    app.feed.create_feed_with_factor(
        name=feed_name, factor_label=factor_key, weight="5.0",
    )
    app.feed.open_feed(feed_name)
    deadline = time.monotonic() + 15
    while app.feed.post_count() < 4 and time.monotonic() < deadline:
        time.sleep(0.3)
    count = app.feed.post_count()
    assert count >= 4, f"composed feed rendered {count} posts; error={app.error_text()!r}"
    target_index = next(
        (i for i in range(count) if CAT_TEXT in (app.feed.post_text(i) or "")), None
    )
    assert target_index is not None, "on-topic post missing from the composed feed"
    assert target_index > 0, (
        "pre-train the on-topic post must not already lead (it is mid-age)"
    )

    # The factor was never trained — explicitly or via engagement (the
    # toggle is still off): no model row yet. Layer A mints it below.
    fetch = ws_api.personalization_model_fetch(port, test_user, factor_key)
    assert not fetch.get("sealed_blob"), "factor must start with no model row"

    # ── 4. Arm Layer A deterministically: opt the factor in, then CLEAR the
    # activity data — the clear resets the live engine, erasing whatever
    # verdicts the setup scrolling incidentally derived (e.g. the on-topic
    # card sitting fully visible while later posts were composed), so the
    # dwell below produces a fresh None → watch-complete TRANSITION ────────
    app.personalization.navigate()
    assert app.personalization.wait_for_topic_count(1) == 1
    app.personalization.set_engagement_toggle(0, True)
    app.personalization.clear_engagement_data()

    # Back on the composed feed. Let the image-load layout churn settle
    # BEFORE reading the pre-dwell order: image cards grow as their blobs
    # resolve, briefly flashing cards through the viewport — with the factor
    # now armed, a surviving flash-skip re-ranks, so an index read mid-churn
    # goes stale (and a stale index dwells the wrong card). Kept well under
    # CUE_DWELL_LONG_MS so the on-topic card cannot pre-complete.
    #
    # `feed_name` is still the selected feed (step 3 opened it), so this is a
    # RE-ENTRY, not a selection: re-clicking the still-selected row starts no
    # reload on linux, and `open_feed`'s barrier waited out its budget here in
    # every sweep. The nav edge's own re-query is what applies the armed factor.
    app.feed.return_to_feed_page()
    deadline = time.monotonic() + 15
    while app.feed.post_count() < 4 and time.monotonic() < deadline:
        time.sleep(0.3)
    time.sleep(3.0)  # churn settle — images resolved, layout stable
    target_index = next(
        (i for i in range(app.feed.post_count())
         if CAT_TEXT in (app.feed.post_text(i) or "")), None
    )
    assert target_index is not None, "on-topic post missing after reopening"
    assert target_index > 0, (
        f"on-topic post must still not lead before the dwell; "
        f"error={app.error_text()!r}"
    )

    # ── 5. The honest dwell: center the on-topic card in the REAL viewport,
    # hold past the long-dwell gate, then scroll far away so it LEAVES the
    # viewport — the observer emits the exposure, the shared engine derives
    # the non-media watch-complete, and the Layer-A hook weak-trains. (A
    # pre-dwell flash-skip on the on-topic card is harmless: the
    # Skip→WatchComplete transition trains inverse-of-old + forward-of-new,
    # netting a pure watch-complete.) ──────────────────────────────────────
    app.feed.dwell_on_post(target_index, CUE_DWELL_LONG_S + 1.0)
    app.feed.scroll_post_into_view(0)

    # Deterministic nest-side gate: the weak train re-sealed + put the model.
    reply = _wait_model_blob(port, test_user, factor_key)
    assert reply.get("sealed_blob"), (
        f"Layer-A weak train never landed a sealed model: {reply!r}; "
        f"error={app.error_text()!r}"
    )

    # The loaded window re-ranks: the on-topic post rises to the top.
    top = _wait_top_post_contains(app, CAT_TEXT)
    rendered = [
        (app.feed.post_text(i) or "")[:36] for i in range(app.feed.post_count())
    ]
    state_posts = [
        (p.get("body") or "")[:36] for p in app.feed._feed_posts_from_state()
    ]
    assert CAT_TEXT in top, (
        f"composed feed did not re-rank after the dwell-derived watch-complete; "
        f"rendered order = {rendered!r}, manager-snapshot order = {state_posts!r}, "
        f"error={app.error_text()!r}"
    )

    # ── 6. Flush-on-close + restart: the debounced cues:v1 rollup is put by
    # the REAL quit path (it blocks on a bounded flush), then survives as a
    # sealed nest-side row any device can fetch ─────────────────────────────
    _quit_through_the_apps_own_leave_door(app)
    assert app.driver.wait_app_exit(APP_EXIT_S), (
        f"app did not exit on window close; still alive={app.driver.is_app_alive()}"
    )
    rollup = _wait_model_blob(port, test_user, CUES_ROLLUP_FACTOR, timeout=10.0)
    assert rollup.get("sealed_blob"), (
        f"cues:v1 rollup was not flushed on close: {rollup!r}"
    )

    app.driver.hard_reload()  # relaunch + session replay (same actor)
    app.driver.set_state({"nav": {"stack": [{"view": "feed"}]}})
    app.feed.open_feed(feed_name)
    top = _wait_top_post_contains(app, CAT_TEXT, timeout=25)
    assert CAT_TEXT in top, (
        f"trained ranking did not survive the restart; top post = {top!r}, "
        f"error={app.error_text()!r}"
    )

    # The opt-in itself round-trips the sealed registry across the restart,
    # and the rollup is still fetchable (the second-device read).
    app.personalization.navigate()
    assert app.personalization.wait_for_topic_count(1) == 1
    assert app.personalization.engagement_toggle_on(0), (
        "learn_from_engagement must survive the restart (sealed registry)"
    )
    rollup = ws_api.personalization_model_fetch(port, test_user, CUES_ROLLUP_FACTOR)
    assert rollup.get("sealed_blob"), "cues:v1 rollup must survive the restart"

    # ── 7. User-revocable: "Clear activity data" deletes the sealed rollup
    # from the user's own nest — this time with a real nest-side row to
    # delete (step 4's clear ran before any put had landed) ────────────────
    app.personalization.clear_engagement_data()
    gone = _wait_model_blob(port, test_user, CUES_ROLLUP_FACTOR, want_present=False)
    assert not gone.get("sealed_blob"), (
        f"clear-activity-data must delete the cues:v1 rollup: {gone!r}"
    )

    # Cleanup — the session-scoped test_user is shared. A test that dies before
    # this line still leaves its topic behind; `create_sole_topic` is what keeps
    # that from failing the next suite, so this is hygiene, not the guard.
    app.personalization.delete_topic(0)
    assert app.personalization.wait_for_topic_count(0) == 0


@pytest.mark.windows
@pytest.mark.feature("learn-from-my-activity")
def test_engagement_dwell_derives_watch_complete_via_navigate_away_flush(
    logged_in_app, nest_instance, test_user
):
    """The windows-scoped slice of the dwell → watch-complete chain above
    (task-6 of the personalization-port plan):
    the windows capture shell (`FaunaApp.Feed.CueViewportObserver`, a
    geometry-poll port of linux's `apps/fauna-linux/src/feed/viewport.rs`)
    samples a REAL, honest viewport dwell — no injected observation, no clock
    shortcut — and the shared `CueEngine` derives the identical non-media
    `watch-complete` verdict from it, weak-training the composed topic. This
    proves the SAMPLING MECHANISM end-to-end (real geometry → real credit
    accumulation → real `RecordObservation` call), not a re-derivation of the
    shared engine (proven once, in Rust, and already end-to-end on linux
    above).

    windows' shell mirrors linux's geometry-poll HOLD-on-unmeasured model,
    not android's virtualization-driven absence-is-leave rule: windows'
    `PostsList` is a deliberately non-virtualizing `StackPanel`
    (`FeedPage.xaml:139-151`), so every post-card container stays realized
    regardless of scroll position.

    A composed feed (`create_feed_with_factor`) is REQUIRED, not cosmetic:
    `FeedManager::train_engagement_for_transition` (`libs/fauna-feed/src/
    manager.rs`) only weak-trains factors in `engagement_factors`, which is
    resolved once per feed reload from the factors the CURRENTLY OPEN feed
    actually composes (`topic()` returns `None`, and the factor is silently
    skipped, for a factor the open feed doesn't compose — confirmed by
    reading the Rust source after this test's first cut, dwelling in the
    plain default Local feed, silently trained nothing for an entire session
    despite a real, correctly-measured 9-second full-visibility dwell).

    **Deliberately narrower than the linux test above, and structured
    differently, for a concrete test-infrastructure reason discovered while
    writing this test**: the FlaUI bridge's `POST /element/scroll-into-view`
    (`Actions.ScrollIntoView`) stops as soon as UIA reports the element
    `!IsOffscreen` — which on windows already flips true at a small, partial
    visibility fraction (empirically as low as ~25% in this feed's post-card
    layout, well under the shared `skipVisiblePm`/`longDwellVisiblePm` gates),
    not "centered" or "substantially visible". A mid-list post can therefore
    stay under the skip-visibility threshold for an entire dwell even after
    `dwell_on_post`'s scroll — the exact case this test hit when first written
    against linux's post-arrangement (3 posts, target at index 1: it settled
    at a ~25%-visible resting position and never crossed the 50% skip gate in
    9 real seconds, so no verdict was ever derived — confirmed via temporary
    `ShellLog` tracing of the real geometry during a live run, not guessed).
    Building a more precise scroll-to-N% primitive is FlaUI-bridge work,
    outside this task's declared scope (`apps/fauna-windows/FaunaApp/...` +
    this one test file) — tracked as a follow-on rather than gating this
    task's landing (task-6 report's concerns section).

    So this test avoids scroll precision entirely: the composed feed has only
    two posts (one off-topic decoy + the on-topic post, created LAST so it is
    the newest and renders as post-card[0], ~100%-visible with no scroll
    needed), and it ends the exposure by navigating AWAY from Feed rather than
    scrolling — which drives `FeedPage.OnNavigatedFrom` →
    `CueViewportObserver.Unwire()` → `FlushAll()`, the exact same "unmap ⇒
    flush everything" code path linux's `viewport.rs` exercises on page
    teardown. The scroll-to-N%-visible primitive itself remains FlaUI-bridge
    follow-on work, outside this task's declared scope
    (`apps/fauna-windows/FaunaApp/...` + this one test file) — tracked
    separately.

    It DOES now cover the flush-on-CLOSE + restart leg (linux's step 6+):
    windows gained the `window_close()` / `wait_app_exit()` driver hooks
 — `Actions.WindowClose` posts a real WM_CLOSE
    to the app's main window, running the real
    `AppWindow.Closing` → `TrayIconService.QuitApplication()` path (never
    `SessionManager.Quit()`'s force-kill, which skips both close-to-tray and
    every quit-time flush) — so the round trip through
    `FfiFeedManager.FlushCues()` (wired into `QuitApplication()` best-effort,
    same shape as the `ConvDrafts`/`Drafts` flushes beside it) is drivable
    from this harness exactly as it is on linux.
    """
    app = logged_in_app
    port = nest_instance["port"]

    # ── 1. Trained topic + the Layer-A opt-in ────────────────────────────
    app.personalization.navigate()
    topic_name = _unique("Cats")
    app.personalization.create_sole_topic(topic_name)
    factor_key = app.personalization.topic_factor_key()

    # Off per v1 until the user opts in (see the linux test's step 1 note on
    # why capture-while-off must never accidentally pre-train the factor).
    assert not app.personalization.engagement_toggle_on(0)

    # ── 2. Seed real posts: one off-topic IMAGE decoy (can't dwell-complete —
    # windows has no video-playback UI either, task-6 brief item 8), then the
    # on-topic TEXT post created LAST — the newest post, so it renders as
    # post-card[0] in ANY feed built from these two posts (untrained ranking
    # falls back to recency), ~100%-visible without any scroll (sidestepping
    # the scroll-precision gap documented above; this test's proof is the
    # CAPTURE mechanism, not re-ranking, so it does not need the on-topic post
    # to start mid-list) ──────────────────────────────────────────────────
    assert TEST_IMAGE.exists(), f"fixture image missing: {TEST_IMAGE}"
    app.driver.set_state({"nav": {"stack": [{"view": "feed"}]}})
    app.feed.create_post_with_image(
        text=f"quarterly budget spreadsheet {_unique('rows')}",
        image_path=str(TEST_IMAGE),
    )
    time.sleep(1.2)  # strictly-older created_at (epoch-seconds resolution)
    app.feed.create_post(text=CAT_TEXT)

    # ── 3. Compose a feed over the trained factor — REQUIRED for Layer A to
    # train it at all (see the docstring above); weight doesn't matter here
    # since re-ranking isn't asserted, only that the factor is COMPOSED ────
    feed_name = _unique("cats-feed")
    app.feed.create_feed_with_factor(
        name=feed_name, factor_label=factor_key, weight="5.0",
    )
    app.feed.open_feed(feed_name)
    assert app.feed.wait_for_post_text(CAT_TEXT, timeout_s=15), (
        f"on-topic post missing from the composed feed; error={app.error_text()!r}"
    )

    # The factor was never trained: no model row yet. Layer A mints it below.
    fetch = ws_api.personalization_model_fetch(port, test_user, factor_key)
    assert not fetch.get("sealed_blob"), "factor must start with no model row"

    # ── 4. Arm Layer A deterministically: opt in, then CLEAR activity data —
    # navigating to Personalization first flushes whatever the setup-phase
    # viewing of the on-topic post (while ~100% visible, above) incidentally
    # accumulated on the OUTGOING FeedPage's CueViewportObserver instance
    # (OnNavigatedFrom → Unwire → FlushAll — the same real code path this
    # test's own dwell will exercise below); clearing then erases whatever
    # verdict that incidental flush derived, so the real dwell below produces
    # a fresh None → watch-complete TRANSITION regardless of what the setup
    # phase incidentally recorded ─────────────────────────────────────────
    app.personalization.navigate()
    assert app.personalization.wait_for_topic_count(1) == 1
    app.personalization.set_engagement_toggle(0, True)
    app.personalization.clear_engagement_data()

    # ── 5. Re-enter the SAME composed feed — Page_Loaded builds a BRAND NEW
    # FfiFeedManager + CueViewportObserver (FeedPage isn't cached), so this
    # dwell starts with a completely fresh tracker; hydrate_cues() finds the
    # just-deleted rollup absent (a fresh capture, not an error). Re-selecting
    # the composed feed re-populates `engagement_factors` for THIS reload. ──
    app.driver.set_state({"nav": {"stack": [{"view": "feed"}]}})
    app.driver.wait_for("feed-view")
    app.feed.open_feed(feed_name)
    assert app.feed.wait_for_post_text(CAT_TEXT, timeout_s=15), (
        f"on-topic post missing after reopening the composed feed; error={app.error_text()!r}"
    )
    target_index = next(
        (i for i in range(app.feed.post_count())
         if CAT_TEXT in (app.feed.post_text(i) or "")), None
    )
    assert target_index is not None, "on-topic post missing after reopening"
    assert target_index == 0, (
        "the on-topic post is the newest — it must render as post-card[0], "
        "already ~100% visible with no scroll needed"
    )

    # ── 6. The honest dwell: hold real wall-clock time past the long-dwell
    # gate on the already-fully-visible post-card[0] (no scroll call at all —
    # `dwell_on_post` would invoke the imprecise ScrollIntoView for nothing,
    # since the element already satisfies `!IsOffscreen`), then navigate AWAY
    # from Feed to end the exposure — OnNavigatedFrom's Unwire()/FlushAll()
    # emits the accumulated exposure deterministically, no scroll needed ────
    time.sleep(CUE_DWELL_LONG_S + 1.0)
    app.personalization.navigate()

    reply = _wait_model_blob(port, test_user, factor_key)
    assert reply.get("sealed_blob"), (
        f"Layer-A weak train never landed a sealed model: {reply!r}; "
        f"error={app.error_text()!r}"
    )

    # ── 7. Flush-on-close + restart: mirrors the linux test's step 6 above.
    # windows' AppWindow.Closing only reaches TrayIconService.QuitApplication()
    # when Close-to-tray is OFF (it defaults ON — AppSettingsStore.CloseToTray —
    # so a plain window_close() would just HIDE the window, never quitting and
    # never flushing; leaving it on is what made the FIRST cut of this leg fail
    # with "app did not exit on window close"). Flip it off first so close
    # really quits. The debounced cues:v1 rollup is then put by the REAL
    # close-request handler (QuitApplication()'s bounded flush — the quit path
    # blocks on it), and survives as a sealed nest-side row any device
    # could fetch ──────────────────────────────────────────────────────
    app.driver.set_state(
        {"nav": {"stack": [{"view": "settings"}, {"view": "settings", "id": "general"}]}}
    )
    app.driver.wait_for("close-to-tray-toggle", timeout=10)
    if app.driver.get_attr("close-to-tray-toggle", "state") == "true":
        _click_with_retry(app.driver, "close-to-tray-toggle")
    assert app.driver.get_attr("close-to-tray-toggle", "state") != "true", (
        "close-to-tray must be OFF for window_close() to reach the real quit "
        f"path; error={app.error_text()!r}"
    )

    app.driver.window_close()
    assert app.driver.wait_app_exit(APP_EXIT_S), (
        f"app did not exit on window close; still alive={app.driver.is_app_alive()}"
    )
    rollup = _wait_model_blob(port, test_user, CUES_ROLLUP_FACTOR, timeout=10.0)
    assert rollup.get("sealed_blob"), (
        f"cues:v1 rollup was not flushed on close: {rollup!r}"
    )

    app.driver.hard_reload()  # relaunch + session replay (same actor)
    app.personalization.navigate()

    # Cleanup — the session-scoped test_user is shared across this module's
    # tests (test-order load-bearing, per the linux test above). Also proves
    # the trained topic survived the restart (sealed registry).
    assert app.personalization.wait_for_topic_count(1) == 1
    app.personalization.clear_engagement_data()
    app.personalization.delete_topic(0)
    assert app.personalization.wait_for_topic_count(0) == 0


def _cue_rollup_row_count(db_path: str, actor_id: bytes, factor: str = CUES_ROLLUP_FACTOR) -> int:
    """The nest-side row count for this actor's `cues:v1` model — the
    ground-truth "was it actually put/deleted" read, mirroring
    `_signal_contributions`'s direct SQLite access below. Read-only against the
    running nest's SQLite."""
    conn = sqlite3.connect(f"file:{db_path}?mode=ro", uri=True)
    try:
        return conn.execute(
            "SELECT COUNT(*) FROM personalization_models WHERE actor_id = ? AND factor = ?",
            (actor_id, factor),
        ).fetchone()[0]
    finally:
        conn.close()


@pytest.mark.web
@pytest.mark.windows
@pytest.mark.tui
@pytest.mark.linux
# apple joined 2026-09-21. It was never a missing control — `PersonalizationView`
# has carried the whole Layer-A surface on both targets for as long as windows
# has. The only thing missing was the capture-free seed seam: the
# `feed_seed_cue_rollup_for_test` TestAgent command, without which "Clear
# activity data" has no `cues:v1` row to delete and its assertion degenerates to
# the "didn't error" check this test's 2026-07-29 mutation pass exists to
# forbid. That handler is now shared FaunaKit (`FeedSeedCueRollupTestCommand`),
# over the same `FeedManager::set_cue_rollup_for_test` seam tui/linux drive.
@pytest.mark.macos
@pytest.mark.ios
@pytest.mark.feature("learn-from-my-activity")
def test_engagement_toggle_and_clear_activity_data(logged_in_app, nest_instance, test_user):
    """The capture-free slice of the personalization chain's engagement
    legs: the trained-factor
    row's "Learn from my activity" toggle (`personalization-trained-factor-
    engagement-toggle`, engagement-cues.md § Layer A) flips, round-trips the
    sealed registry across a reload, and "Clear activity data"
    (`personalization-clear-engagement-data-button`, engagement-cues.md § At
    rest) actually **deletes the nest-side `cues:v1` row** — over the same
    `setTrainedTopicEngagement` / `WasmFeedManager.deleteCueRollup`
    wasm faces (web) / `TrainedTopicsSetLearnFromEngagementAsync` /
    `FfiFeedManager.DeleteCueRollup` UniFFI faces (windows) the linux dwell
    test above exercises. Deliberately does NOT dwell/capture (the dwell tests
    above and below own that): the row this leg deletes is seeded via the `set_cue_rollup_for_test` shared-Rust seam
    (`feed_seed_cue_rollup_for_test`), not a real dwell.

    Mutation-tested (2026-07-29): before the seed+nest-read discipline below,
    this test's entire check on the clear button was `assert not
    app.has_error()` — neutering `Action::ClearEngagementData` to a no-op left
    the suite fully green. The row-count assertions here are what makes that
    mutation fail.
    """
    app = logged_in_app
    db_path = nest_instance["db_path"]
    actor_id = bytes.fromhex(test_user["actor_id_hex"])
    app.personalization.navigate()
    topic_name = _unique("Toggle")
    app.personalization.create_sole_topic(topic_name)

    # Off by default (topic-factors.md § Training signals v2).
    assert not app.personalization.engagement_toggle_on(0), (
        "learn_from_engagement must default off"
    )

    app.personalization.set_engagement_toggle(0, True)
    assert app.personalization.engagement_toggle_on(0), (
        f"toggle did not flip on; error={app.error_text()!r}"
    )

    # The opt-in is sealed-registry state, not local UI state: a reload
    # (web's relaunch twin — localStorage/origin survive, so the SPA
    # rehydrates as the same actor) must still show it on.
    app.driver.hard_reload()
    app.personalization.navigate()
    assert app.personalization.wait_for_topic_count(1) == 1
    assert app.personalization.engagement_toggle_on(0), (
        "learn_from_engagement must survive a reload (sealed registry)"
    )

    # Seed a real cues:v1 nest row — this leg never dwells, so without this
    # there is nothing for "Clear activity data" to actually delete, and the
    # button's check degenerates to "didn't error" (the mutation this test
    # exists to catch).
    app.feed.seed_cue_rollup_for_test([uuid.uuid4().hex])
    assert _cue_rollup_row_count(db_path, actor_id) == 1, (
        "the seeded cues:v1 row never reached the nest — "
        "set_cue_rollup_for_test's PUT did not land"
    )

    # "Clear activity data" must both complete without error AND actually
    # remove the row — the discriminator a bare has_error() check cannot make.
    app.personalization.clear_engagement_data()
    assert not app.has_error(), (
        f"clear-activity-data should not error: {app.error_text()!r}"
    )
    assert _cue_rollup_row_count(db_path, actor_id) == 0, (
        "Clear activity data clicked without error, but the cues:v1 row is "
        "still on the nest — the button did nothing"
    )

    # Cleanup — the session-scoped test_user is shared across this module's
    # tests (test-order load-bearing, per the dwell test above).
    app.personalization.delete_topic(0)
    assert app.personalization.wait_for_topic_count(0) == 0


def _signal_contributions(db_path: str, reporter_id: bytes, factor: str) -> list[str]:
    """The app user's own ``content_reports`` rows under ``factor`` — the raw
    per-contributor facts the producer writes (below k they exist here even
    though nothing is published anywhere). Returns each row's hex content_hash
    (the contributed post's id). Read-only against the running nest's SQLite,
    the deterministic gate the nest-side ``test_signal_sharing.py`` uses."""
    conn = sqlite3.connect(f"file:{db_path}?mode=ro", uri=True)
    try:
        rows = conn.execute(
            "SELECT content_hash FROM content_reports WHERE reporter = ? AND factor = ?",
            (reporter_id, factor),
        ).fetchall()
    finally:
        conn.close()
    return [r[0].hex() for r in rows]


def _wait_signal_contribution(db_path, reporter_id, factor, timeout=15.0):
    """Poll until the producer's contribution under ``factor`` lands, then
    return its post-id hex. Asserts EXACTLY one (signal-sharing was off in every
    prior test, so the only signal:* row is this dwell's)."""
    deadline = time.monotonic() + timeout
    hashes = _signal_contributions(db_path, reporter_id, factor)
    while not hashes and time.monotonic() < deadline:
        time.sleep(0.3)
        hashes = _signal_contributions(db_path, reporter_id, factor)
    if not hashes:
        return None
    assert len(hashes) == 1, f"expected exactly one {factor} contribution, got {hashes!r}"
    return hashes[0]


def _wait_pane_row(app, post_id_hex, timeout=20.0):
    """Poll the transparency pane for a row whose hash matches ``post_id_hex``,
    bouncing feed↔personalization each round to re-fire the page-visible
    hydrate (the pane re-reads signal_share.status on connect_map). Returns the
    row index, or None."""
    deadline = time.monotonic() + timeout
    while time.monotonic() < deadline:
        app.personalization.navigate()
        inner = time.monotonic() + 3.0
        while time.monotonic() < inner:
            for i in range(app.personalization.signal_published_count()):
                if app.personalization.signal_published_hash(i) == post_id_hex:
                    return i
            time.sleep(0.3)
        # Bounce away so the next navigate re-fires the personalization
        # home_page's connect_map hydrate.
        app.driver.set_state({"nav": {"stack": [{"view": "feed"}]}})
        time.sleep(0.3)
    return None


@pytest.mark.linux
@pytest.mark.web
@pytest.mark.feature("learn-from-my-activity")
def test_signal_sharing_optin_producer_and_transparency_pane(
    logged_in_app, nest_instance, test_user, request
):
    """Layer-B end-to-end (engagement-cues.md § Layer B): the opt-in toggle
    round-trips, an opted-in user's REAL dwell watch-complete on a PUBLIC post
    fires the client PRODUCER (fauna.moderation.signal_contribute), and — once
    two more contributors cross k=3 — the transparency pane renders the ≥k
    aggregate this nest exports.

    The unique behaviour under test is the PRODUCER: no nest test can prove the
    client contributes from a derived cue — it needs the real UI + real
    CueEngine + a real dwell. (The wire's k-gate / transparency / verdict-flip /
    opt-out are proven over ws_api in tests/api/test_signal_sharing.py.) The two
    helper contributions ride ws_api as E2E-rule-8 fixture setup — they arrange
    the ≥k precondition the pane renders, never the action under test; the app
    user's own contribution is the UI-driven one, and the toggle is UI-driven.

    linux and web: web's capture shell (`apps/fauna-web/src/lib/feed-cues.ts`)
    samples the same honest scroll `dwell_on_post` drives, and its
    Personalization page carries the Layer-B toggle + pane over the live
    `WasmFeedManager`.
    """
    app = logged_in_app
    port = nest_instance["port"]
    db_path = nest_instance["db_path"]
    admin_sk = nest_instance["admin"]["signing_key"]
    reporter_id = bytes.fromhex(test_user["actor_id_hex"])
    factor = "signal:watch-complete"

    from common.auth import create_actor_and_register

    # ── 1. Opt in via the toggle — round-trips the nest-confirmed state ───
    app.personalization.navigate()
    assert not app.personalization.signal_sharing_on(), "signal-sharing defaults off"
    app.personalization.set_signal_sharing(True)
    # Hygiene for the session-scoped test_user, not the action under test: a
    # failure below must not leave the opt-in on, or the next test's
    # "defaults off" read fails for this test's reason. Step 7 opts out through
    # the UI; this wire-side opt-out only covers a failure before it.
    request.addfinalizer(lambda: ws_api.signal_share_set(port, test_user, False))
    assert app.personalization.signal_sharing_on(), (
        f"opt-in did not round-trip on; error={app.error_text()!r}"
    )

    # ── 2. Seed a PUBLIC text post to dwell on + IMAGE anchors to scroll to.
    # Image posts can't dwell-complete on linux (the media playback gate is
    # absent here — the dwell test's selectivity note), so only the text target
    # derives a watch-complete: exactly one signal:watch-complete contribution
    # lands, and its content_hash IS the dwelled post's id. ────────────────
    assert TEST_IMAGE.exists(), f"fixture image missing: {TEST_IMAGE}"
    app.driver.set_state({"nav": {"stack": [{"view": "feed"}]}})
    target_text = f"{CAT_TEXT} {_unique('sig')}"
    app.feed.create_post(text=target_text)
    time.sleep(1.2)  # strictly-older created_at (epoch-seconds resolution)
    app.feed.create_post_with_image(
        text=f"anchor {_unique('a')}", image_path=str(TEST_IMAGE)
    )
    time.sleep(1.2)
    app.feed.create_post_with_image(
        text=f"anchor {_unique('b')}", image_path=str(TEST_IMAGE)
    )

    # ── 3. Real dwell on the target: center it past the long-dwell gate, then
    # scroll far away so it LEAVES the viewport — the observer emits the
    # exposure, the shared CueEngine derives the non-media watch-complete, and
    # the PRODUCER contributes it (opted in + public). Nothing injected. ───
    app.driver.set_state({"nav": {"stack": [{"view": "feed"}]}})
    deadline = time.monotonic() + 15
    while app.feed.post_count() < 3 and time.monotonic() < deadline:
        time.sleep(0.3)
    time.sleep(2.0)  # image-load layout churn settle before reading indices
    target_index = next(
        (i for i in range(app.feed.post_count())
         if target_text in (app.feed.post_text(i) or "")), None
    )
    assert target_index is not None, (
        f"target post missing from the feed; error={app.error_text()!r}"
    )
    app.feed.dwell_on_post(target_index, CUE_DWELL_LONG_S + 1.0)
    app.feed.scroll_post_into_view(0)
    # Then leave the feed: every shell drains everything it tracks when its
    # list goes away (linux's unmap, web's route unmount), so the exposure ends
    # whether or not the scroll above took the target below skip-visibility —
    # on a tall layout, or with earlier tests' posts in the list, it may not.
    app.personalization.navigate()

    # ── 4. PRODUCER PROOF: exactly one signal:watch-complete row lands for the
    # app user (content_reports — the raw per-contributor fact; below k it lives
    # only here). The row's content_hash is the dwelled post's id. ─────────
    post_id = _wait_signal_contribution(db_path, reporter_id, factor)
    assert post_id is not None, (
        f"the producer never contributed a {factor} for the dwelled post; "
        f"error={app.error_text()!r}"
    )

    # ── 5. Two more opted-in contributors cross k=3 on the SAME post (ws_api
    # fixture setup — the ≥k precondition the pane renders). ────────────────
    for _ in range(2):
        c = create_actor_and_register(port, admin_signing_key=admin_sk)
        assert ws_api.signal_share_set(port, c, True)["share"] is True
        ws_api.signal_contribute(port, c, post_id, "watch-complete")

    # ── 6. PANE PROOF: the transparency pane renders the ≥k aggregate exactly
    # as a peer nest sees it — (post-hash, signal:watch-complete, 3). ───────
    row = _wait_pane_row(app, post_id)
    assert row is not None, (
        f"signal-share-published-list never rendered the ≥k aggregate for "
        f"{post_id!r}; count={app.personalization.signal_published_count()} "
        f"error={app.error_text()!r}"
    )
    assert app.personalization.signal_published_factor(row) == factor
    assert app.personalization.signal_published_contributor_count(row) == "3", (
        "the published count is the ≥k local contributor count"
    )

    # ── 7. Opt out — the toggle round-trips back off, and (leaving the shared
    # session nest clean) withdraws the app user's signal rows. ─────────────
    app.personalization.set_signal_sharing(False)
    assert not app.personalization.signal_sharing_on(), "opt-out did not round-trip off"
    assert not _signal_contributions(db_path, reporter_id, factor), (
        "opt-out must withdraw the app user's signal contributions"
    )


@pytest.mark.macos
@pytest.mark.ios
@pytest.mark.feature("learn-from-my-activity")
def test_signal_sharing_persisted_optin_honoured_by_producer_after_relaunch(
    logged_in_app, nest_instance, test_user, request
):
    """A PERSISTED Layer-B opt-in reaches the producer without the user ever
    opening Personalization (engagement-cues.md § Layer B): the shared producer
    (`FeedManager::record_observation`) contributes only when the manager's
    cached opt-in is true, and that cache is filled by `hydrate_signal_optin`,
    which every capture shell calls at feed init. apple's shell once did not —
    a relaunch (or a second device) left the cache empty, so the dwell below
    derived a watch-complete that was never contributed until the pane's own
    read happened to fill it. Red before `CueViewportObserver.hydrateIfNeeded`
    gained the call.

    Drives by element id only. Opt in through the UI, relaunch, and — WITHOUT
    visiting Personalization, whose own status read would fill the cache and
    mask the bug — dwell on a public post and leave the feed through the
    profile page (the leave door that also flushes the capture task).
    """
    app = logged_in_app
    port = nest_instance["port"]
    db_path = nest_instance["db_path"]
    reporter_id = bytes.fromhex(test_user["actor_id_hex"])
    factor = "signal:watch-complete"

    # ── 1. Opt in via the toggle, nest-confirmed ──────────────────────────
    _wait_feed_manager_ready(app)
    app.personalization.navigate()
    assert not app.personalization.signal_sharing_on(), "signal-sharing defaults off"
    app.personalization.set_signal_sharing(True)
    # Hygiene for the session-scoped test_user (this module's other Layer-B
    # tests assert "defaults off"); the wire-side opt-out also withdraws rows.
    request.addfinalizer(lambda: ws_api.signal_share_set(port, test_user, False))
    assert app.personalization.signal_sharing_on(), (
        f"opt-in did not round-trip on; error={app.error_text()!r}"
    )

    # ── 2. A public text post to dwell on (newest, so it renders first) ───
    app.driver.set_state({"nav": {"stack": [{"view": "feed"}]}})
    target_text = f"{CAT_TEXT} {_unique('optin')}"
    app.feed.create_post(text=target_text)

    # ── 3. Relaunch: the manager cache is process memory, so it starts empty
    # and only feed init can refill it from the persisted opt-in. ──────────
    app.driver.hard_reload()
    _wait_feed_manager_ready(app)
    assert app.feed.wait_for_post_text(target_text, timeout_s=15), (
        f"target post missing after relaunch; error={app.error_text()!r}"
    )
    target_index = next(
        (i for i in range(app.feed.post_count())
         if target_text in (app.feed.post_text(i) or "")), None
    )
    assert target_index is not None, "target post missing from the feed after relaunch"

    # ── 4. Honest dwell, then leave the feed — NOT via Personalization ────
    app.feed.dwell_on_post(target_index, CUE_DWELL_LONG_S + 1.0)
    app.driver.set_state({"nav": {"stack": [{"view": "profile"}]}})

    # ── 5. The producer contributed under the persisted opt-in ────────────
    post_id = _wait_signal_contribution(db_path, reporter_id, factor)
    assert post_id is not None, (
        f"the producer never contributed a {factor} under a persisted opt-in "
        f"(feed init did not hydrate it); error={app.error_text()!r}"
    )


def _wait_feed_manager_ready(app) -> None:
    """Give the Feed page's async ``FfiFeedManager`` build a moment to land
    before touching the signal-share toggle, which reaches that SAME live
    instance (never a second manager — mirrors ``DeleteCueRollupAsync``'s
    accessor). A real user's own multi-click journey through the UI takes far
    longer than this harness's direct deep-link, so this guards only against
    a harness-induced race, never a product one.

    apple has the same dependency under a different name: ``PersonalizationView``
    reaches the shared ``@Environment(FeedVM.self)`` instance, and BOTH
    ``hydrateSignalShare`` and ``setSignalShare`` open with
    ``guard let manager = feedVM.manager else { return }`` — so an unwarmed
    ``feedVM.manager`` makes the toggle SILENTLY no-op (no error element, no
    exception; the poll in ``set_signal_sharing`` just times out). Warming it is
    the same act on every app: land on the feed once before the first navigate
    into Settings."""
    app.driver.set_state({"nav": {"stack": [{"view": "feed"}]}})
    app.driver.wait_for("feed-view")
    time.sleep(2.0)


@pytest.mark.windows
@pytest.mark.tui
@pytest.mark.macos
@pytest.mark.ios
@pytest.mark.web
@pytest.mark.feature("learn-from-my-activity")
def test_signal_sharing_toggle_and_pane_render(logged_in_app):
    """The toggle-and-pane slice of engagement-cues.md § Layer B: the opt-in toggle
    (`personalization-share-signals-toggle`) round-trips through the
    nest-confirmed `fauna.moderation.signal_share.{set,status}` reply (via the
    shared `FfiFeedManager` instance the Feed page observes), and the
    transparency pane (`signal-share-published-list`) renders without error.

    Deliberately does NOT dwell/produce a real signal: the windows capture
    shell (the `media_played_pm` playback gate a video-capable client needs)
    is a separate, not-yet-built track (engagement-cues.md § Implementation
    status today: "Not built: capture shells on windows/apple"). Mirrors
    apple's own landing scope note verbatim — "this lift proves only the
    toggle round-trip + pane render",
    which is exactly this test's scope too. The full k=3 producer→pane proof
    (`test_signal_sharing_optin_producer_and_transparency_pane` above) runs on
    linux and web; web runs this slice too.

    **The apple legs (macos + ios, added 2026-09-21) take the same scope for
    the same reason, over ONE shared renderer.** `FaunaKit`'s
    `PersonalizationView` carries the whole Layer-B surface unconditionally for
    both targets — the `personalization-share-signals-toggle` (registered with
    the `"on"`/`"off"` value contract `signal_sharing_on` reads, NOT the bare
    Switch `"true"`/`"false"` the Layer-A row toggle above uses) and
    `signalSharePublishedSection`'s transparency pane — so there is no separate
    iOS leg to write. Apple is non-optimistic in exactly the way this test
    asserts: `setSignalShare` re-reads `signalShareStatus()` after the set, so
    `signalShare` only ever reflects a nest-confirmed value. Apple's own dwell
    capture shell DOES exist (`test_..._via_navigate_away_flush_apple` below
    drives it), but the k=3 producer leg additionally needs the playback gate,
    so the scope split is the windows one, not an apple capability gap.

    Reload-persistence leg (mirrors the Layer-A toggle test above): a
    prior attempt here used a one-shot read immediately after `hard_reload()`
    + re-navigate and flaked, reading `false` with no error even after 20+ s
    of polling — a longer poll alone cannot fix that, since polling harder
    doesn't help if the read genuinely never becomes true. The Layer-A
    sibling's own `hard_reload()` + Settings re-navigate already runs green
    on windows, so the extra dependency here is the seam: unlike Layer-A,
    this toggle's `LoadAsync()` reaches the SAME live `FfiFeedManager` the
    Feed page built (`ActiveFeedManagerHolder.Current`, read via
    `NestRpcClient.SharedFeedManager()`) — a static that resets with the
    relaunched process and is normally re-warmed by `_wait_feed_manager_ready()`
    before this test's FIRST navigate. `hard_reload()` itself lands on the
    feed tab (which rebuilds the holder) but does not wait for that async
    build, so a navigate straight into Settings right after can race an
    unwarmed holder. Re-run `_wait_feed_manager_ready()` after the reload,
    exactly as before the first navigate, then poll the toggle read with a
    bounded ceiling for `PersonalizationPage.Page_Loaded`'s own async chain
    (labeler-catalog refresh → trained-topics load → signal-share load) to
    settle. Nest-side persistence itself was already independently confirmed
    correct via a direct ws_api read while diagnosing the original flake.
    """
    app = logged_in_app
    _wait_feed_manager_ready(app)
    app.personalization.navigate()

    # Default off (user-controls-their-data) — and the transparency pane
    # renders (here: empty) without error even before any opt-in, a pure read
    # of fauna.moderation.signal_share.status over the live FfiFeedManager.
    assert not app.personalization.signal_sharing_on(), "signal-sharing defaults off"
    assert not app.has_error(), (
        f"signal-share pane failed to render: {app.error_text()!r}"
    )

    app.personalization.set_signal_sharing(True)
    assert app.personalization.signal_sharing_on(), (
        f"opt-in did not round-trip on; error={app.error_text()!r}"
    )

    # The opt-in is nest-confirmed persisted state
    # (fauna.moderation.signal_share.status), not local UI state: a hard
    # reload (relaunch to a fresh process + replay the session) must still
    # show it on. Re-warm ActiveFeedManagerHolder.Current exactly as before
    # this test's first navigate (see the docstring's reload-persistence-leg
    # note) before deep-linking back into Settings, then poll for
    # PersonalizationPage's own async load chain to settle.
    app.driver.hard_reload()
    _wait_feed_manager_ready(app)
    app.personalization.navigate()
    deadline = time.monotonic() + 15.0
    while not app.personalization.signal_sharing_on() and time.monotonic() < deadline:
        time.sleep(0.3)
    assert app.personalization.signal_sharing_on(), (
        "signal-sharing opt-in must survive a reload (sealed, nest-confirmed "
        f"state); error={app.error_text()!r}"
    )

    # Opt back out — round-trips off, leaving the shared session nest clean
    # (no contributions were ever produced, so nothing to withdraw-assert).
    app.personalization.set_signal_sharing(False)
    assert not app.personalization.signal_sharing_on(), "opt-out did not round-trip off"


@pytest.mark.macos
@pytest.mark.ios
@pytest.mark.web
@pytest.mark.feature("learn-from-my-activity")
def test_engagement_dwell_derives_watch_complete_via_navigate_away_flush_apple(
    logged_in_app, nest_instance, test_user
):
    """The apple-scoped slice of the dwell → watch-complete chain: apple's
    capture shell (`FaunaKit/Feed/CueViewportObserver.swift` over the pure
    `CueTracker.swift`) samples a REAL, honest viewport dwell — no injected
    observation, no clock shortcut — and the shared `CueEngine` derives the
    identical non-media `watch-complete` verdict from it, weak-training the
    composed topic. This proves the SAMPLING MECHANISM end-to-end (real
    SwiftUI geometry → real credit accumulation → a real `recordObservation`
    call over UniFFI), not a re-derivation of the shared engine (proven once
    in Rust, and already end-to-end on linux above).

    apple's shell mirrors linux's/windows' geometry-poll HOLD-on-unmeasured
    model, not android's virtualization-driven absence-is-leave rule: BOTH
    apple feeds are deliberately non-virtualizing eager `ScrollView { VStack }`
    (iOS `FeedListView.swift:72`, macOS `MacFeedDetailView.swift:153` — each
    with its own comment explaining that a lazy `List` pools rows and produces
    delete-zombies), so every post-card stays realized regardless of scroll
    position and an unmeasured row is mid-layout, never disposed.

    A composed feed (`create_feed_with_factor`) is REQUIRED, not cosmetic, for
    the same reason the windows test above documents:
    `FeedManager::train_engagement_for_transition` only weak-trains factors the
    CURRENTLY OPEN feed actually composes.

    **Deliberately scoped like the windows test above, for the same two
    test-infrastructure reasons — both in the apple e2e bridge, neither in the
    product:**

    1. ~~`dwell_on_post`/`scroll_post_into_view` are unavailable~~ **— CLOSED
       2026-08-02.** `POST /element/scroll-into-view` is now REAL on the apple
       in-process agent (it centres the element in every enclosing scroll view
       by walking up from its registration sentinel) and both apple drivers set
       `_supports_scroll_into_view = True`, so this test drives the SAME honest
       `dwell_on_post` path as the linux test above rather than sidestepping
       scroll. The old dodge — create the on-topic post LAST so it lands on
       `post-card[0]`, already fully visible — is gone with it. Regression gate
       for the route itself, including the ≥75%-visible bar an edge-aligned
       scroll would miss: `tests/test_apple_scroll_into_view.py`.
    2. The apple drivers have no `window_close()`/`wait_app_exit()` hook
       (linux's do), so the flush-on-quit + restart-survival leg isn't drivable
       here either. `flushCues` IS wired on both apple apps (iOS
       `scenePhase → .background`; macOS `applicationShouldTerminate`, bounded
       via `.terminateLater`) — it just has no harness path yet. Navigating
       AWAY from the feed is the flush trigger this test drives instead, and on
       apple that is not a lesser path: it cancels the capture `.task`, which
       IS the teardown (the drain + `flushCues` live in that cancellation
       handler), so it exercises the same production code an app quit does.

    **web runs this same journey (added with its capture shell).** It drives
    by element id and the shared `dwell_on_post` alone, and web's leave door
    is the same: routing off the feed unmounts the page, whose `onDestroy`
    stops `CueCapture` (`apps/fauna-web/src/lib/feed-cues.ts`) — drain, report,
    `flushCues()`. The tab-leave flush (`visibilitychange` → hidden,
    `pagehide`) rides the same `leave()` but a browser grants no reliable async
    work after either event, so it is best-effort and not asserted here.

    Gap 2 is tracked as a follow-on rather than gating the shell's landing.
    **The iOS leg is no longer owed** — it was entrusted because a cold `--client ios` run needs the full
    multi-slice apple-ffi; that ran 2026-08-02 (N+90) off a warm test-flavor
    build and PASSED, alongside the macOS leg, both driving the real
    `dwell_on_post` scroll.
    """
    app = logged_in_app
    port = nest_instance["port"]

    # ── 1. Trained topic + the Layer-A opt-in ────────────────────────────
    app.personalization.navigate()
    topic_name = _unique("Cats")
    app.personalization.create_sole_topic(topic_name)
    factor_key = app.personalization.topic_factor_key()

    # Off per v1 until the user opts in (see the linux test's step 1 note on
    # why capture-while-off must never accidentally pre-train the factor).
    assert not app.personalization.engagement_toggle_on(0)

    # ── 2. Seed real posts: one off-topic IMAGE decoy (it can NEVER
    # dwell-complete — a media item's watch-complete gate is playback, and
    # apple renders feed media as `AsyncImage` stills with no AVPlayer, so the
    # shell reports `mediaPlayedPm: nil`), then the on-topic TEXT post created
    # LAST so it is the newest and renders as post-card[0], ~100% visible with
    # no scroll needed (sidestepping gap 1 in the docstring) ──────────────
    assert TEST_IMAGE.exists(), f"fixture image missing: {TEST_IMAGE}"
    app.driver.set_state({"nav": {"stack": [{"view": "feed"}]}})
    app.feed.create_post_with_image(
        text=f"quarterly budget spreadsheet {_unique('rows')}",
        image_path=str(TEST_IMAGE),
    )
    time.sleep(1.2)  # strictly-older created_at (epoch-seconds resolution)
    app.feed.create_post(text=CAT_TEXT)

    # ── 3. Compose a feed over the trained factor — REQUIRED for Layer A to
    # train it at all (see the docstring); weight is immaterial here since
    # re-ranking isn't asserted, only that the factor is COMPOSED ──────────
    feed_name = _unique("cats-feed")
    app.feed.create_feed_with_factor(
        name=feed_name, factor_label=factor_key, weight="5.0",
    )
    app.feed.open_feed(feed_name)
    assert app.feed.wait_for_post_text(CAT_TEXT, timeout_s=15), (
        f"on-topic post missing from the composed feed; error={app.error_text()!r}"
    )

    # The factor was never trained: no model row yet. Layer A mints it below.
    fetch = ws_api.personalization_model_fetch(port, test_user, factor_key)
    assert not fetch.get("sealed_blob"), "factor must start with no model row"

    # ── 4. Arm Layer A deterministically: opt in, then CLEAR activity data.
    # Navigating to Personalization first cancels the feed's capture `.task`,
    # which drains and flushes whatever the setup-phase viewing incidentally
    # accumulated (the same real code path the dwell below exercises);
    # clearing then erases whatever verdict that incidental flush derived, so
    # the real dwell produces a fresh None → watch-complete TRANSITION ─────
    app.personalization.navigate()
    assert app.personalization.wait_for_topic_count(1) == 1
    app.personalization.set_engagement_toggle(0, True)
    app.personalization.clear_engagement_data()

    # ── 5. Re-enter the SAME composed feed. Re-selecting it re-populates
    # `engagement_factors` for THIS reload. The capture `.task` restarts with a
    # brand-new `CueTracker`; `hydrateCues()` does NOT re-run (it is guarded to
    # once per manager generation — a second hydrate would replace the live
    # CueEngine and discard folded verdicts), and the just-cleared rollup is
    # absent anyway, which is a fresh capture rather than an error ─────────
    app.driver.set_state({"nav": {"stack": [{"view": "feed"}]}})
    app.driver.wait_for("feed-view")
    app.feed.open_feed(feed_name)
    assert app.feed.wait_for_post_text(CAT_TEXT, timeout_s=15), (
        f"on-topic post missing after reopening the composed feed; error={app.error_text()!r}"
    )
    target_index = next(
        (i for i in range(app.feed.post_count())
         if CAT_TEXT in (app.feed.post_text(i) or "")), None
    )
    assert target_index is not None, "on-topic post missing after reopening"

    # ── 6. The honest dwell: CENTRE the on-topic card via the real targeted
    # scroll (the client's cue observer samples that same scroll position, so
    # this is an honest exposure rather than an injected one), then hold real
    # wall-clock time past the long-dwell gate with the 250 ms tick accruing
    # credit throughout, then navigate AWAY from the feed to end the exposure —
    # the `.task` cancellation drains the tracker, emits the accumulated
    # exposure, and calls `flushCues()`.
    #
    # This is `dwell_on_post`, the same helper the linux test above drives; it
    # replaced a bare `time.sleep` on a deliberately-index-0 card back when the
    # apple scroll route was a no-op stub (docstring gap 1, now closed).
    app.feed.dwell_on_post(target_index, CUE_DWELL_LONG_S + 1.0)
    app.personalization.navigate()

    reply = _wait_model_blob(port, test_user, factor_key)
    assert reply.get("sealed_blob"), (
        f"Layer-A weak train never landed a sealed model: {reply!r}; "
        f"error={app.error_text()!r}"
    )

    # The capture shell's own at-rest artefact: the sealed `cues:v1` rollup,
    # put by the `flushCues()` in the same teardown.
    rollup = _wait_model_blob(port, test_user, CUES_ROLLUP_FACTOR)
    assert rollup.get("sealed_blob"), (
        f"flushCues never sealed the cues:v1 rollup: {rollup!r}"
    )

    # Cleanup — the session-scoped test_user is shared across this module's
    # tests (test-order load-bearing, per the linux test above).
    assert app.personalization.wait_for_topic_count(1) == 1
    app.personalization.clear_engagement_data()
    app.personalization.delete_topic(0)
    assert app.personalization.wait_for_topic_count(0) == 0
