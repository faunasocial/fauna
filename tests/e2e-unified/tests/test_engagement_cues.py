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

Selectivity note: the off-topic posts are seeded as VIDEO posts that nobody
plays. A playable-media item's ``watch-complete`` gate is playback
(``media_played_pm ≥ 850‰``), and no app's inline player ever autoplays — so
an off-topic card can NEVER dwell-complete no matter how co-visible it is
while the on-topic card is dwelled (a completion there would weak-train the
factor on off-topic text and erase the rank differential). Its long exposure
derives no verdict at all (dwell ≥ CUE_SKIP_MS rules out ``skip``); only the
non-media on-topic post can complete via the dwell gate. This is the same
media/dwell asymmetry the goal doc specifies, used as the test's selectivity
mechanism — no viewport-geometry assumptions needed. A still IMAGE would not
do: an image post is a non-media post (``PostSummary::plays_media``), so a
long dwell completes it exactly as it does a text post.

linux and tui run the full journey below (tui since 2026-09-19: its capture
shell reads the frame the terminal actually painted, and its targeted scroll
moves the focus ring the viewport follows — `apps/fauna-tui/src/feed/cues.rs`);
windows, apple and web run narrower slices further down (the media playback
gate needs a video-capable client). tui has no inline playback at all (a
declared platform absence), so a video decoy can never complete there either.

A separate, capture-free test below proves the toggle + clear-button wiring on
every app.
"""
import sqlite3
import time
import uuid
from pathlib import Path

import pytest

from helpers import budgets
from helpers.budgets import APP_EXIT_S
from helpers.waiting import wait_until
from tests.api import ws_api

pytestmark = pytest.mark.tier_3

# Distinctive on-topic vocabulary — deliberately SHORT (one line): the dwelled
# card must sit fully visible (≥75%) while centered.
CAT_TEXT = "tabby kitten purring whiskers softly by the warm window"

# fauna_core::scoring::cues — the shared thresholds the engine derives by.
CUE_DWELL_LONG_S = 8.0  # CUE_DWELL_LONG_MS
CUE_LONG_DWELL_VISIBLE = 0.75  # CUE_LONG_DWELL_VISIBLE_PM / 1000
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


# The decoy attachment: a video nobody plays (the module doc's selectivity
# note) — the same fixture `test_feed_video_thumbnail.py` posts on all 7 apps.
DECOY_VIDEO = Path(__file__).parent.parent.parent / "fixtures" / "tiny-video.mp4"


def _post_decoy(app, text: str) -> None:
    """Post an off-topic decoy that can never ``watch-complete``: a video post
    the test never plays. Its cue gate is playback alone, and no inline player
    autoplays."""
    assert DECOY_VIDEO.exists(), f"fixture video missing: {DECOY_VIDEO}"
    app.feed.create_post_with_video(text=text, video_path=str(DECOY_VIDEO))


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
    row), windows' window close with Close-to-tray switched off first (it
    defaults ON — `AppSettingsStore.CloseToTray` — and a close then only hides
    the window, never reaching `TrayIconService.QuitApplication()`'s bounded
    flush). Either way the process then exits on its own."""
    if app.driver.is_tui():
        app.driver.click("exit-tab")
        return
    if app.driver.is_windows():
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


# Once a dwell has trained the topic, the harness ends later exposures of the
# on-topic post by its own speed — a restart drains the capture shell right
# after the top was read, and every reopen shows the feed page's default list
# before `open_feed` switches away from it. Neither is a scroll, so neither
# derives a ``skip`` (`engagement-cues.md` § Cue vocabulary & derivation, the
# 2026-10-10 ruling: the tracker stamps how an exposure ended, and a skip
# requires `ScrolledPast`); the journeys therefore hold nothing past a gate,
# and their staying green is the end-to-end proof the ruling is live.


def _restart_leg_diagnosis(app, port: int, actor: dict, factor: str, before: dict) -> str:
    """What a restart leg's red needs to name its side (convention 6): whether
    the persisted model moved across the relaunch (a put landing in the
    teardown), which feed the app had selected, and the order it rendered."""
    after = ws_api.personalization_model_fetch(port, actor, factor)
    try:
        diag = app.driver.get_state("feed.diag")
    except Exception as exc:  # a platform without the diag block
        diag = f"<unreadable: {exc}>"
    try:
        items = app.driver.get_texts("feed-item")
    except Exception as exc:
        items = f"<unreadable: {exc}>"
    order = [(app.feed.post_text(i) or "")[:40] for i in range(min(app.feed.post_count(), 6))]
    return (
        f"model before/after restart: sample_count "
        f"{before.get('sample_count')}→{after.get('sample_count')}, updated_at "
        f"{before.get('updated_at')}→{after.get('updated_at')}, blob "
        f"{'unchanged' if before.get('sealed_blob') == after.get('sealed_blob') else 'CHANGED'}; "
        f"feed diag={diag!r}; feed-items={items!r}; rendered order={order!r}"
    )


def _open_feed(app, feed_name: str) -> None:
    """Back to the feed page, then switch to ``feed_name``."""
    app.driver.set_state({"nav": {"stack": [{"view": "feed"}]}})
    app.driver.wait_for("feed-view")
    app.feed.open_feed(feed_name)


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
    # sits mid-list (provably not leading). The off-topic posts are unplayed
    # VIDEO posts: they cannot dwell-complete (selectivity note in the module
    # doc), so only the on-topic text post can derive the verdict ───────────
    app.driver.set_state({"nav": {"stack": [{"view": "feed"}]}})
    _post_decoy(app, f"quarterly budget spreadsheet {_unique('rows')}")
    time.sleep(1.2)  # strictly-older created_at (epoch-seconds resolution)
    app.feed.create_post(text=CAT_TEXT)
    time.sleep(1.2)
    _post_decoy(app, f"garage door spring {_unique('note')}")
    time.sleep(1.2)
    _post_decoy(app, f"commuter rail timetable {_unique('stop')}")

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

    # Back on the composed feed. Let the media-load layout churn settle
    # BEFORE reading the pre-dwell order: media cards grow as their blobs
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
    time.sleep(3.0)  # churn settle — media resolved, layout stable
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
    # hold past the long-dwell gate, then LEAVE the feed page — the page leave
    # drains every tracked exposure, the shared engine derives the non-media
    # watch-complete (end-agnostic), and the Layer-A hook weak-trains. Not a
    # scroll back to post-card[0]: four short cards (an unplayed video's card
    # is a play glyph, not a picture) fit the viewport together, so that
    # scroll never takes the on-topic card out of view and its exposure never
    # ends. (A pre-dwell flash-skip on the on-topic card is harmless: the
    # Skip→WatchComplete transition trains inverse-of-old + forward-of-new,
    # netting a pure watch-complete.) ──────────────────────────────────────
    app.feed.dwell_on_post(target_index, CUE_DWELL_LONG_S + 1.0)
    app.personalization.navigate()
    app.feed.return_to_feed_page()

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
    _open_feed(app, feed_name)
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


def _dwell_on_the_newest_post_and_leave(app) -> None:
    """Hold post-card[0] past the long-dwell gate, then leave the list.

    post-card[0] is visible from the moment the list paints, so no scroll is
    needed to expose it; leaving the page is tui's page-leave edge, which
    drains every tracked card into the engine (`apps/fauna-tui/src/feed/
    cues.rs` § Scheduling, lifecycle, leave model) — so the verdict is derived
    by the time the next page is up, not on a timer."""
    app.feed.dwell_on_post(0, CUE_DWELL_LONG_S + 1.0)
    app.personalization.navigate()


def _quit_and_wait_for_the_rollup(app, port, actor) -> dict:
    """Quit through the app's own door and return the flushed ``cues:v1`` row.

    The process EXITING is the barrier: once it is gone nothing more can be
    put, so what the nest holds afterwards is final. The rollup being there is
    the positive control that the dwell really was captured and derived.

    A browser tab has no quit door a test can drive, and needs none: leaving
    the feed already stopped web's capture (`apps/fauna-web/src/lib/
    feed-cues.ts`, `CueCapture.stop`), which awaits every `recordObservation`
    — the Layer-A train and its put included — BEFORE `flushCues` puts the
    rollup. Every caller cleared the rollup before its dwell, so the rollup
    arriving is the same barrier the exit is elsewhere.

    Apple takes the same barrier: leaving the feed cancels its capture
    `.task`, whose teardown drain (`apps/fauna-apple/FaunaKit/Sources/
    FaunaKit/Feed/CueViewportObserver.swift`, `run` → `emit`) awaits every
    `recordObservation` in turn before `flushCues`, the order web keeps — and
    iOS has no quit door a test can drive at all.

    Android takes it too: leaving the feed cancels the `CueViewportObserver`
    `LaunchedEffect` (`apps/fauna-android/.../feed/CueViewport.kt`), which
    drains every tracked card before the rollup flush, and the harness has no
    quit door to drive."""
    if (
        app.driver.is_web()
        or app.driver.is_macos()
        or app.driver.is_ios()
        or app.driver.is_android()
    ):
        rollup = _wait_model_blob(port, actor, CUES_ROLLUP_FACTOR)
    else:
        _quit_through_the_apps_own_leave_door(app)
        assert app.driver.wait_app_exit(APP_EXIT_S), (
            f"app did not exit; still alive={app.driver.is_app_alive()}"
        )
        rollup = ws_api.personalization_model_fetch(port, actor, CUES_ROLLUP_FACTOR)
    assert rollup.get("sealed_blob"), (
        f"the dwell was never captured — no cues:v1 rollup after quit: {rollup!r}"
    )
    return rollup


@pytest.mark.linux
@pytest.mark.web
@pytest.mark.tui
@pytest.mark.windows
@pytest.mark.macos
@pytest.mark.ios
@pytest.mark.android
@pytest.mark.feature("learn-from-my-activity")
def test_a_switched_off_topic_learns_nothing_and_switching_off_keeps_what_it_learned(
    logged_in_app, nest_instance, test_user
):
    """Outcomes 8 and 9 (`engagement-cues.md` § Layer A — the private training
    feed: only factors with `learn_from_engagement` on are trained;
    `topic-factors.md` § Authoring surface & picker: flipping it off stops
    future weak training without rewriting what engagement already taught).

    Three identical honest dwells on the same post, each one a fresh verdict
    transition (Clear activity data resets the engine before each): switched
    off → no model; switched on → a model; switched off again → the same model,
    byte for byte. Every model read — the "learned nothing" reads and the
    learned baseline alike — is taken after the app has EXITED, so no put can
    still be on its way, and beside a flushed rollup proving the dwell was
    captured.
    """
    app = logged_in_app
    port = nest_instance["port"]

    app.personalization.navigate()
    app.personalization.create_sole_topic(_unique("Cats"))
    factor_key = app.personalization.topic_factor_key()
    assert not app.personalization.engagement_toggle_on(0)

    # Video decoys first, the on-topic text post LAST: the newest post renders
    # as post-card[0], on screen with no scroll, and the unplayed video posts
    # cannot dwell-complete (the module doc's selectivity note).
    app.driver.set_state({"nav": {"stack": [{"view": "feed"}]}})
    app.feed.open_feed("General")
    _post_decoy(app, f"quarterly budget spreadsheet {_unique('rows')}")
    time.sleep(1.2)  # sleep-ok: strictly-older created_at (epoch-seconds resolution)
    app.feed.create_post(text=CAT_TEXT)
    feed_name = _unique("cats-feed")
    app.feed.create_feed_with_factor(name=feed_name, factor_label=factor_key, weight="5.0")

    def open_the_cat_feed(reenter: bool = False):
        # Re-clicking the still-selected row is not a gesture every app
        # answers with a reload, so a same-launch return re-enters the page
        # (`FeedActions.return_to_feed_page`); a fresh launch selects it.
        if reenter:
            app.feed.return_to_feed_page()
        else:
            app.driver.set_state({"nav": {"stack": [{"view": "feed"}]}})
            app.feed.open_feed(feed_name)
        assert app.feed.wait_for_post_text(CAT_TEXT, timeout_s=15), (
            f"on-topic post missing from the composed feed; error={app.error_text()!r}"
        )
        assert CAT_TEXT in app.feed.first_post_text(), (
            "the on-topic post is the newest — it must lead the composed feed"
        )

    def reset_capture():
        app.personalization.navigate()
        assert app.personalization.wait_for_topic_count(1) == 1
        app.personalization.clear_engagement_data()
        _wait_model_blob(port, test_user, CUES_ROLLUP_FACTOR, want_present=False)

    # ── Outcome 8: switched off, a dwell teaches nothing ─────────────────
    open_the_cat_feed()
    reset_capture()
    open_the_cat_feed(reenter=True)
    _dwell_on_the_newest_post_and_leave(app)
    _quit_and_wait_for_the_rollup(app, port, test_user)
    off =ws_api.personalization_model_fetch(port, test_user, factor_key)
    assert not off.get("sealed_blob"), (
        f"a topic whose switch is off learned from a dwell: {off!r}"
    )

    # ── The positive control: switched on, the same dwell teaches it ─────
    app.driver.hard_reload()
    app.personalization.navigate()
    assert app.personalization.wait_for_topic_count(1) == 1
    app.personalization.set_engagement_toggle(0, True)
    reset_capture()
    open_the_cat_feed()
    _dwell_on_the_newest_post_and_leave(app)
    # The baseline is what the dwell taught once its training has SETTLED, not
    # the first blob seen: a leave hands every exposure it drains to its own
    # `record_observation`, each of which may train and put, so a blob polled
    # mid-drain can be replaced a moment later (1 module run in 4 did). The
    # same exit barrier the "learned nothing" reads stand on settles it.
    _quit_and_wait_for_the_rollup(app, port, test_user)
    learned = ws_api.personalization_model_fetch(port, test_user, factor_key)
    assert learned.get("sealed_blob"), (
        f"switched on, the same dwell never trained the topic: {learned!r}"
    )

    # ── Outcome 9: switching off keeps what it learned, and learns no more ──
    app.driver.hard_reload()
    app.personalization.navigate()
    assert app.personalization.wait_for_topic_count(1) == 1
    app.personalization.set_engagement_toggle(0, False)
    kept = ws_api.personalization_model_fetch(port, test_user, factor_key)
    assert kept.get("sealed_blob") == learned["sealed_blob"], (
        "switching the topic off must leave what it learned untouched"
    )
    reset_capture()
    open_the_cat_feed()
    _dwell_on_the_newest_post_and_leave(app)
    _quit_and_wait_for_the_rollup(app, port, test_user)
    after =ws_api.personalization_model_fetch(port, test_user, factor_key)
    assert after.get("sealed_blob") == learned["sealed_blob"], (
        "a switched-off topic changed after another dwell — it must learn no more"
    )

    app.driver.hard_reload()
    app.personalization.navigate()
    assert app.personalization.wait_for_topic_count(1) == 1
    assert not app.personalization.engagement_toggle_on(0), (
        "the switch must stay off across a restart"
    )
    assert ws_api.personalization_model_fetch(port, test_user, factor_key).get(
        "sealed_blob"
    ) == learned["sealed_blob"], "what the topic learned must survive the restart"

    app.personalization.clear_engagement_data()
    app.personalization.delete_topic(0)
    assert app.personalization.wait_for_topic_count(0) == 0


@pytest.mark.tui
@pytest.mark.linux
@pytest.mark.web
@pytest.mark.macos
@pytest.mark.ios
@pytest.mark.windows
@pytest.mark.feature("learn-from-my-activity")
def test_what_one_device_collected_informs_the_feeds_on_your_other_device(
    request, logged_in_app, nest_instance, test_user
):
    """Outcome 16 (`engagement-cues.md` § At rest — the sealed cue rollup: it
    lives on the nest for cross-device continuity, the phone's watches inform
    the desktop's feeds; `topic-factors.md` § Placement: fetch-on-demand).

    Seat A switches a topic's learning on and lingers on the on-topic post
    until the topic learns, then quits through the app's own door so the
    ``cues:v1`` rollup flushes. Only then is a second seat of the same identity
    launched (e2e convention 16; sync is fetch-on-demand, so an open seat is
    not expected to change). Seat B's cue engine, once hydrated, already holds
    seat A's verdict for that post — the rollup's own cross-device job, read
    off the live engine since the sealed model cannot show it. Newer posts
    arrive on seat B, and the feed composed of the topic still ranks the post
    seat A lingered on first: what seat A's app collected reached the other
    device's feed.
    """
    app = logged_in_app
    port = nest_instance["port"]

    app.personalization.navigate()
    topic_name = _unique("Cats")
    app.personalization.create_sole_topic(topic_name)
    factor_key = app.personalization.topic_factor_key()
    assert not app.personalization.engagement_toggle_on(0)

    # A video decoy, then the on-topic text post LAST, so it renders as
    # post-card[0] with no scroll (the module doc's selectivity note).
    app.driver.set_state({"nav": {"stack": [{"view": "feed"}]}})
    app.feed.open_feed("General")
    _post_decoy(app, f"quarterly budget spreadsheet {_unique('rows')}")
    time.sleep(1.2)  # sleep-ok: strictly-older created_at (epoch-seconds resolution)
    app.feed.create_post(text=CAT_TEXT)
    feed_name = _unique("cats-feed")
    app.feed.create_feed_with_factor(name=feed_name, factor_label=factor_key, weight="5.0")
    app.feed.open_feed(feed_name)
    assert app.feed.wait_for_post_text(CAT_TEXT, timeout_s=15), (
        f"on-topic post missing from the composed feed; error={app.error_text()!r}"
    )

    # Arm Layer A, then clear the activity data so the dwell below is a fresh
    # verdict transition (the setup scrolling may have derived one).
    app.personalization.navigate()
    assert app.personalization.wait_for_topic_count(1) == 1
    app.personalization.set_engagement_toggle(0, True)
    app.personalization.clear_engagement_data()
    _wait_model_blob(port, test_user, CUES_ROLLUP_FACTOR, want_present=False)
    assert not ws_api.personalization_model_fetch(port, test_user, factor_key).get(
        "sealed_blob"
    ), "the topic must start with nothing learned"

    # ── Seat A: linger on the on-topic post, then quit ───────────────────
    app.feed.return_to_feed_page()
    assert app.feed.wait_for_post_text(CAT_TEXT, timeout_s=15)
    assert CAT_TEXT in app.feed.first_post_text(), (
        "the on-topic post is the newest — it must lead the composed feed"
    )
    cat_id = app.feed._post_id_of_card(0)
    _dwell_on_the_newest_post_and_leave(app)
    assert _wait_model_blob(port, test_user, factor_key).get("sealed_blob"), (
        f"the dwell on seat A never taught the topic; error={app.error_text()!r}"
    )
    assert app.feed.cue_verdict_for_test(cat_id)["verdict"] == "watch-complete", (
        f"seat A's own engine must hold the dwell's verdict: "
        f"{app.feed.cue_verdict_for_test(cat_id)!r}"
    )
    _quit_and_wait_for_the_rollup(app, port, test_user)

    # ── Seat B: launched only now, holding nothing it did not fetch ──────
    # web's second device is a twin page in its own BrowserContext (fresh
    # storage, same identity); the native ones a fresh-state-root launch.
    seat_b, _driver_b = request.getfixturevalue(
        "alice_second_web_device" if app.driver.is_web() else "alice_second_device"
    )
    row = seat_b.personalization.wait_for_topic_reopening(
        topic_name,
        # The switch is a later write of the same registry record than the
        # create: wait until this seat has walked that one too.
        predicate=lambda i: seat_b.personalization.engagement_toggle_on(i),
    )
    assert seat_b.personalization.topic_factor_key(row) == factor_key

    # Seat B's engine starts from seat A's rollup: once hydrated it already
    # holds seat A's `watch-complete` for a post seat B has not yet shown in
    # the composed feed — so a re-watch here is last-wins, never taught twice.
    # The sealed topic model cannot show this (`sample_count` excludes
    # engagement), so read the live engine itself.
    hydrated = wait_until(
        lambda: (lambda r: r if r["hydrated"] else None)(
            seat_b.feed.cue_verdict_for_test(cat_id)
        ),
        budgets.RPC_ROUNDTRIP_S,
        diagnose=lambda: f"seat B's cue engine never hydrated; error={seat_b.error_text()!r}",
    )
    assert hydrated["verdict"] == "watch-complete", (
        f"seat B hydrated without seat A's verdict for the on-topic post — a "
        f"hydration gap: {hydrated!r}; error={seat_b.error_text()!r}"
    )

    # Newer posts arrive (unplayed video posts: they cannot dwell-complete).
    seat_b.driver.set_state({"nav": {"stack": [{"view": "feed"}]}})
    seat_b.feed.open_feed("General")
    newer = [f"garage door spring {_unique('note')}", f"commuter rail {_unique('stop')}"]
    for text in newer:
        _post_decoy(seat_b, text)
    seat_b.feed.open_feed(feed_name)
    for text in [CAT_TEXT, *newer]:
        assert seat_b.feed.wait_for_post_text(text, timeout_s=15), (
            f"{text!r} missing from the other device's composed feed; "
            f"error={seat_b.error_text()!r}"
        )
    # The sealed scorer loads on the feed's reload, so poll for the settled top.
    top = _wait_top_post_contains(seat_b, CAT_TEXT, timeout=25)
    assert CAT_TEXT in top, (
        f"what seat A's activity taught must rank the on-topic post above newer "
        f"posts on the other device; it shows {top!r} first; "
        f"error={seat_b.error_text()!r}"
    )

    # Cleanup — the session-scoped test_user is shared.
    app.driver.hard_reload()
    app.personalization.navigate()
    assert app.personalization.wait_for_topic_count(1) == 1
    app.personalization.clear_engagement_data()
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
    samples a REAL, honest viewport dwell — no injected observation; the dwell
    is advanced on the frozen cue clock (convention 14), the geometry the
    app's own — and the shared `CueEngine` derives the identical non-media
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

    **The on-topic post starts mid-list, below a newer decoy, as in its
    web/apple twin below — so the dwell moving it to the top is a re-rank
    recency cannot fake (`learn-from-my-activity.md` outcome 14).** The
    FlaUI bridge's plain `POST /element/scroll-into-view`
    (`Actions.ScrollIntoView`) stops as soon as UIA reports the element
    `!IsOffscreen`, which on windows flips at a small, partial visibility
    (empirically as low as ~25% of a post-card, under both the skip and the
    long-dwell gates), so `dwell_on_post`'s plain scroll can leave a mid-list
    card credited nothing. The dwell therefore scrolls with the bridge's
    MEASURED sibling, `scroll_to_visible_fraction` (`Actions.
    ScrollIntoViewFraction`: a sweep until the card's own rect overlaps the
    viewport by the asked fraction), to the long-dwell visibility, and
    asserts the fraction it reached. It ends the exposure by navigating AWAY
    from Feed rather than scrolling — `FeedPage.OnNavigatedFrom` →
    `CueViewportObserver.Unwire()` → `FlushAll()`, the same "unmap ⇒ flush
    everything" path linux's `viewport.rs` exercises on page teardown.

    It covers the flush-on-CLOSE + restart leg too (linux's step 6+):
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

    # ── 2. Seed real posts: the on-topic TEXT post FIRST, then one newer
    # off-topic unplayed VIDEO decoy (it can NEVER dwell-complete — its gate
    # is playback, and nothing plays it). Recency therefore puts the decoy
    # above the on-topic post, so the dwell below moving it to the top is a
    # re-rank recency cannot fake (outcome 14) ───────────────────────────
    app.driver.set_state({"nav": {"stack": [{"view": "feed"}]}})
    app.feed.create_post(text=CAT_TEXT)
    time.sleep(1.2)  # sleep-ok: strictly-newer created_at (epoch-seconds resolution)
    _post_decoy(app, f"quarterly budget spreadsheet {_unique('rows')}")

    # ── 3. Compose a feed dominated by the trained factor — REQUIRED for
    # Layer A to train it at all (see the docstring above), and the weight is
    # what lets the trained factor out-rank recency in step 7 ─────────────
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

    # ── 5. Re-enter the SAME composed feed — Page_Loaded wires a new
    # CueViewportObserver (FeedPage isn't cached) over the session's manager
    # and the auth's one capture tracker, which the navigate-away above
    # drained, so this dwell starts with nothing tracked. Re-selecting the
    # composed feed re-populates `engagement_factors` for THIS reload. ──
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
    assert target_index > 0, (
        f"before the dwell the on-topic post must not lead (the decoy is "
        f"newer); error={app.error_text()!r}"
    )

    # ── 6. The honest dwell: bring the mid-list on-topic card to long-dwell
    # visibility with the MEASURED scroll (the docstring's reason the plain
    # one is not enough; the capture samples that same scroll position, so
    # the exposure is honest), advance the frozen clock past the long-dwell gate, then
    # navigate AWAY from Feed to end the exposure — OnNavigatedFrom's
    # Unwire()/FlushAll() emits the accumulated exposure deterministically ──
    visible = app.driver.scroll_to_visible_fraction(
        "post-card", CUE_LONG_DWELL_VISIBLE, index=target_index
    )
    assert visible >= CUE_LONG_DWELL_VISIBLE, (
        f"the measured scroll left post-card[{target_index}] only "
        f"{visible:.0%} visible — under the long-dwell gate, so no dwell can "
        f"be credited; error={app.error_text()!r}"
    )
    # The hold is advanced on the frozen cue clock, never slept (convention 14).
    app.feed.accrue_dwell(target_index, CUE_DWELL_LONG_S + 1.0)
    app.personalization.navigate()

    reply = _wait_model_blob(port, test_user, factor_key)
    assert reply.get("sealed_blob"), (
        f"Layer-A weak train never landed a sealed model: {reply!r}; "
        f"error={app.error_text()!r}"
    )

    # Outcome 14: what the topic learned from the dwell moves the on-topic
    # post to the top of the composed feed.
    _open_feed(app, feed_name)
    top = _wait_top_post_contains(app, CAT_TEXT)
    assert CAT_TEXT in top, (
        f"the composed feed did not re-rank after the dwell-derived "
        f"watch-complete; top post = {top!r}, error={app.error_text()!r}"
    )
    app.personalization.navigate()

    # ── 7. Flush-on-close + restart: mirrors the linux test's step 6 above.
    # windows' AppWindow.Closing only reaches TrayIconService.QuitApplication()
    # when Close-to-tray is OFF (it defaults ON — AppSettingsStore.CloseToTray —
    # so a plain window_close() would just HIDE the window, never quitting and
    # never flushing; leaving it on is what made the FIRST cut of this leg fail
    # with "app did not exit on window close"), so the leave door switches it
    # off before the close. The debounced cues:v1 rollup is then put by the REAL
    # close-request handler (QuitApplication()'s bounded flush — the quit path
    # blocks on it), and survives as a sealed nest-side row any device
    # could fetch ──────────────────────────────────────────────────────
    _quit_through_the_apps_own_leave_door(app)
    assert app.driver.wait_app_exit(APP_EXIT_S), (
        f"app did not exit on window close; still alive={app.driver.is_app_alive()}"
    )
    rollup = _wait_model_blob(port, test_user, CUES_ROLLUP_FACTOR, timeout=10.0)
    assert rollup.get("sealed_blob"), (
        f"cues:v1 rollup was not flushed on close: {rollup!r}"
    )

    before = ws_api.personalization_model_fetch(port, test_user, factor_key)
    app.driver.hard_reload()  # relaunch + session replay (same actor)
    _open_feed(app, feed_name)
    top = _wait_top_post_contains(app, CAT_TEXT, timeout=25)
    assert CAT_TEXT in top, (
        f"the dwell-trained order did not survive the restart; top post = "
        f"{top!r}, error={app.error_text()!r}; "
        f"{_restart_leg_diagnosis(app, port, test_user, factor_key, before)}"
    )
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
@pytest.mark.android
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


def _wait_signal_contribution(db_path, reporter_id, factor, post_id=None, timeout=15.0):
    """Poll until the producer's contribution under ``factor`` lands — for
    ``post_id`` when given, else for any post — and return every contributed
    post-id hex at that moment (empty on timeout).

    Never asserts EXACTLY one: verdicts are per item (engagement-cues.md §
    Layer A), so any other non-media post really held on screen through the
    dwell — a neighbour an earlier test left in the session-scoped feed —
    honestly derives its own watch-complete and is contributed too."""
    def landed(hashes):
        return post_id in hashes if post_id is not None else bool(hashes)

    deadline = time.monotonic() + timeout
    hashes = _signal_contributions(db_path, reporter_id, factor)
    while not landed(hashes) and time.monotonic() < deadline:
        time.sleep(0.3)
        hashes = _signal_contributions(db_path, reporter_id, factor)
    return hashes if landed(hashes) else []


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

    # ── 2. Seed a PUBLIC text post to dwell on + VIDEO anchors to scroll to.
    # Unplayed video posts can't dwell-complete (a playable-media item
    # completes only through playback, and nobody plays these — the dwell
    # test's selectivity note), so no anchor may be contributed; the text target must
    # be, its content_hash being the dwelled post's id. ────────────────────
    app.driver.set_state({"nav": {"stack": [{"view": "feed"}]}})
    target_text = f"{CAT_TEXT} {_unique('sig')}"
    anchor_texts = [f"anchor {_unique('a')}", f"anchor {_unique('b')}"]
    app.feed.create_post(text=target_text)
    for anchor_text in anchor_texts:
        time.sleep(1.2)  # strictly-older created_at (epoch-seconds resolution)
        _post_decoy(app, anchor_text)

    # ── 3. Real dwell on the target: center it past the long-dwell gate, then
    # scroll far away so it LEAVES the viewport — the observer emits the
    # exposure, the shared CueEngine derives the non-media watch-complete, and
    # the PRODUCER contributes it (opted in + public). Nothing injected. ───
    app.driver.set_state({"nav": {"stack": [{"view": "feed"}]}})
    deadline = time.monotonic() + 15
    while app.feed.post_count() < 3 and time.monotonic() < deadline:
        time.sleep(0.3)
    time.sleep(2.0)  # media-load layout churn settle before reading indices
    target_index = next(
        (i for i in range(app.feed.post_count())
         if target_text in (app.feed.post_text(i) or "")), None
    )
    assert target_index is not None, (
        f"target post missing from the feed; error={app.error_text()!r}"
    )
    # The ids the producer's rows are keyed by (content_hash = post id).
    def _post_id_of(text):
        row = app.feed.post_state_by_text(text)
        return row.get("post_id") if row else None

    target_id = _post_id_of(target_text)
    anchor_ids = [_post_id_of(t) for t in anchor_texts]
    assert target_id and all(anchor_ids), (
        f"a seeded post has no id in the feed state: target={target_id!r} "
        f"anchors={anchor_ids!r}; error={app.error_text()!r}"
    )
    app.feed.dwell_on_post(target_index, CUE_DWELL_LONG_S + 1.0)
    app.feed.scroll_post_into_view(0)
    # Then leave the feed: every shell drains everything it tracks when its
    # list goes away (linux's unmap, web's route unmount), so the exposure ends
    # whether or not the scroll above took the target below skip-visibility —
    # on a tall layout, or with earlier tests' posts in the list, it may not.
    app.personalization.navigate()

    # ── 4. PRODUCER PROOF: a signal:watch-complete row lands for the app user
    # on the dwelled post (content_reports — the raw per-contributor fact; below
    # k it lives only here), and none on a video anchor. A text post an earlier
    # test left beside the target may land too: it was on screen for the whole
    # dwell, so its verdict is as honest as the target's. ─────────────────
    contributed = _wait_signal_contribution(db_path, reporter_id, factor, target_id)
    assert target_id in contributed, (
        f"the producer never contributed a {factor} for the dwelled post "
        f"{target_id!r}; contributed={contributed!r} error={app.error_text()!r}"
    )
    assert not set(anchor_ids) & set(contributed), (
        f"a video anchor was contributed as {factor} without playback: "
        f"anchors={anchor_ids!r} contributed={contributed!r}"
    )
    post_id = target_id

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
@pytest.mark.android
@pytest.mark.windows
@pytest.mark.feature("learn-from-my-activity")
def test_signal_sharing_persisted_optin_honoured_by_producer_after_relaunch(
    logged_in_app, nest_instance, test_user, request
):
    """A PERSISTED Layer-B opt-in reaches the producer without the user ever
    opening Personalization (engagement-cues.md § Layer B): the shared producer
    (`FeedManager::record_observation`) contributes only when the manager's
    cached opt-in is true, and that cache is filled by `hydrate_signal_optin`,
    which every capture shell calls at feed init. apple's and windows' shells
    once did not — a relaunch (or a second device) left the cache empty, so the dwell below
    derived a watch-complete that was never contributed until the pane's own
    read happened to fill it. Red before `CueViewportObserver.hydrateIfNeeded`
    (apple) and `FeedPage.Page_Loaded` (windows) gained the call.

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
    assert _wait_signal_contribution(db_path, reporter_id, factor), (
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

    Deliberately does NOT dwell/produce a real signal here: the producer side
    is proven by the dwell journeys and the persisted-opt-in relaunch test
    (engagement-cues.md § Implementation status today). Mirrors
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
@pytest.mark.android
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

    # ── 2. Seed real posts: the on-topic TEXT post FIRST, then one newer
    # off-topic unplayed VIDEO decoy (it can NEVER dwell-complete — a
    # playable-media item's watch-complete gate is playback; apple's shell
    # reports `mediaPlayedPm: nil` (no inline player yet), and web's player
    # never autoplays, so nothing plays it).
    # Recency therefore puts the decoy above the on-topic post, so the dwell
    # below moving it to the top is a re-rank recency cannot fake (outcome 14)
    app.driver.set_state({"nav": {"stack": [{"view": "feed"}]}})
    app.feed.create_post(text=CAT_TEXT)
    time.sleep(1.2)  # sleep-ok: strictly-newer created_at (epoch-seconds resolution)
    _post_decoy(app, f"quarterly budget spreadsheet {_unique('rows')}")

    # ── 3. Compose a feed dominated by the trained factor — REQUIRED for
    # Layer A to train it at all (see the docstring), and the weight is what
    # lets the trained factor out-rank recency in step 7 ─────────────────
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
    assert target_index > 0, (
        f"before the dwell the on-topic post must not lead (the decoy is "
        f"newer); error={app.error_text()!r}"
    )

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

    # ── 7. Outcome 14: what the topic learned from the dwell moves the
    # on-topic post up the composed feed, and the order survives a restart
    # (the linux/tui journey's step 6, through `hard_reload`'s relaunch +
    # session replay, which every app's driver offers) ───────────────────
    def reopen_and_read_top(timeout: float) -> str:
        _open_feed(app, feed_name)
        return _wait_top_post_contains(app, CAT_TEXT, timeout=timeout)

    top = reopen_and_read_top(15)
    assert CAT_TEXT in top, (
        f"the composed feed did not re-rank after the dwell-derived "
        f"watch-complete; top post = {top!r}, error={app.error_text()!r}"
    )
    before = ws_api.personalization_model_fetch(port, test_user, factor_key)
    app.driver.hard_reload()
    top = reopen_and_read_top(25)
    assert CAT_TEXT in top, (
        f"the dwell-trained order did not survive the restart; top post = "
        f"{top!r}, error={app.error_text()!r}; "
        f"{_restart_leg_diagnosis(app, port, test_user, factor_key, before)}"
    )
    app.personalization.navigate()

    # Cleanup — the session-scoped test_user is shared across this module's
    # tests (test-order load-bearing, per the linux test above).
    assert app.personalization.wait_for_topic_count(1) == 1
    app.personalization.clear_engagement_data()
    app.personalization.delete_topic(0)
    assert app.personalization.wait_for_topic_count(0) == 0


# The 5 s VP9 clip both linux's GStreamer and Playwright's Chromium play
# (`test_feed_video_playback.py`'s fixture map).
FIXTURE_DIR = Path(__file__).parent.parent.parent / "fixtures"


def _tiny_video(app) -> tuple[Path, float]:
    """The clip each app's native player decodes, and its length in seconds —
    the per-app choice ``test_feed_video_playback.py`` makes: web's browser and
    linux's GStreamer play the 5 s VP9 ``tiny-video.webm``; AVFoundation
    (macos, ios) and windows' ``MediaPlayerElement`` play no VP9, so they get
    the 1 s H.264 ``tiny-video.mp4``."""
    if app.driver.is_web() or app.driver.is_linux():
        return FIXTURE_DIR / "tiny-video.webm", 5.0
    return FIXTURE_DIR / "tiny-video.mp4", 1.0
CUE_COMPLETE_FRACTION_PM = 850  # fauna_core::scoring::cues::CUE_COMPLETE_FRACTION_PM


def _played_s(app, card_index: int) -> float | None:
    """The inline player's position in seconds on ``post-card[card_index]``
    (read where each app's native player keeps it, as
    ``test_feed_video_playback.py`` does); ``None`` while no player stands."""
    if app.driver.is_web():
        value = app.driver.eval_js(
            "(() => { const v = document.querySelectorAll('[data-testid=\"post-card\"]')"
            f"[{card_index}]?.querySelector('[data-testid=\"video-thumbnail\"] video');"
            " return v ? (v.ended ? v.duration : v.currentTime) : null; })()"
        )
        return float(value) if isinstance(value, (int, float)) else None
    try:
        return float(app.driver.get_attr(
            "video-thumbnail", "position", scope=f"post-card[{card_index}]"
        ))
    except (TypeError, ValueError):
        return None


#: fauna_core::scoring::cues::CUE_SKIP_MS — an exposure ended by a scroll after
#: less than this at skip-visibility derives `skip`, which trains the topic.
CUE_SKIP_MS = 1_200


def _await_on_screen_cards_past_skip(app) -> None:
    """Hold until every exposure in progress has dwelt at skip-visibility for
    at least ``CUE_SKIP_MS`` — so the scroll that follows ends none of them as
    a ``skip``. A skip on a decoy trains the composed topic, and that training
    re-ranks the loaded window in place (measured on web 2026-11-02: the
    played post jumped from post-card[7] to post-card[0] during the scroll to
    it, the decoys above it having been on screen under 1.2 s). Polled off
    the capture's own probe — the state the derivation reads — never a sleep
    (convention 14)."""
    def settled():
        exposures = app.feed.cue_probe().get("exposures") or []
        return all(
            e.get("dwell_ms_at_skip_visibility", 0) >= CUE_SKIP_MS for e in exposures
        ) or None

    wait_until(
        settled,
        CUE_SKIP_MS / 1000 + budgets.RPC_ROUNDTRIP_S,
        diagnose=lambda: f"the cards on screen never dwelt past the skip window: "
                         f"{app.feed.cue_probe()!r}",
    )


def _card_of(app, post_id: str, hint: int) -> int | None:
    """The index of the card rendering ``post_id`` now — ``hint`` first, the
    whole list when it no longer holds that post. The played post is the
    subject, not its slot: a training write mid-play re-ranks the loaded
    window in place (``SealedScorers::set_topic``), and the player's own card
    growing to its 320×180 frame pushes the cards below out of view, ending
    exposures that can derive exactly such a write. ``None`` when no card
    renders it."""
    def post_at(i: int) -> str | None:
        try:
            return app.feed._post_id_of_card(i)
        except Exception:  # noqa: BLE001 — a card mid-rebuild; read again next poll
            return None

    if post_at(hint) == post_id:
        return hint
    return next((i for i in range(app.feed.post_count()) if post_at(i) == post_id), None)


def _video_cards(app) -> str:
    """Every card's identity and inline-player reading, for a play wait that
    timed out (convention 6): which post each card renders, and its
    ``video-thumbnail``'s ``state`` / ``source`` / ``position`` — so a tap that
    reached another card, a player that never left ``loading``, and a stream
    that stands ``playing`` at 0 tell themselves apart."""
    def read(fn):
        try:
            return fn()
        except Exception as e:  # noqa: BLE001 — a diagnostic never masks the timeout
            return f"<{type(e).__name__}>"

    count = read(app.feed.post_count)
    rows = []
    for i in range(count if isinstance(count, int) else 0):
        scope = f"post-card[{i}]"
        post = read(lambda: app.feed._post_id_of_card(i)) or ""
        attrs = {
            name: read(lambda: app.driver.get_attr("video-thumbnail", name, scope=scope))
            for name in ("state", "source", "position")
        }
        rows.append(f"[{i}] {str(post)[:12]} {attrs}")
    return "; ".join(rows) or "no cards"


@pytest.mark.linux
@pytest.mark.web
@pytest.mark.macos
@pytest.mark.ios
@pytest.mark.windows
@pytest.mark.feature("learn-from-my-activity")
def test_a_video_played_to_its_end_derives_watch_complete_through_the_media_arm(
    logged_in_app, nest_instance, test_user
):
    """The media arm of ``watch-complete`` (``engagement-cues.md`` § Cue
    vocabulary & derivation): a reader taps a post's video, lets it play to
    the end, and moves on — and that alone teaches the composed topic, exactly
    as a long dwell on a text post does in the journeys above.

    The inline player (``video-thumbnail``, render-model.md § D6c → *Inline
    playback*) notes its progress into a per-post peak; the capture shell
    takes it into the post's cue row each sample (linux
    ``views/feed/video_thumbnail.rs`` → ``feed/viewport.rs``; web
    ``VideoThumbnail.svelte`` → ``feed-cues.ts``), the shared ``CueTracker``
    folds the peak into the exposure, and ``CueEngine`` derives the verdict.

    **Only the media arm can explain the verdict, by construction rather than
    by timing:** a post carrying media is derived by its played fraction
    alone — the dwell arm is never consulted for it (``CueEngine::derive``) —
    so however long the card sat on screen while the clip loaded and played,
    no dwell could have completed it. That is why this test asserts no
    wall-clock bound on the dwell (convention 14). On linux, web, macos and ios the capture's own
    probe additionally shows the exposure attended by the play while it plays
    (``playing`` — a play is attention, ``engagement-cues.md`` § Cue vocabulary
    & derivation) and carrying the played fraction before the leave.

    Same setup as the apple/web journey above: an on-topic post FIRST, then a
    newer off-topic VIDEO decoy that can never complete (the test never
    plays it), a composed feed (Layer A only trains factors the open feed
    composes), opt in, clear, re-enter.
    """
    app = logged_in_app
    port = nest_instance["port"]
    tiny_video, tiny_video_s = _tiny_video(app)
    assert tiny_video.exists(), f"fixture video missing: {tiny_video}"

    # ── 1. Trained topic, engagement learning still off ──────────────────
    app.personalization.navigate()
    app.personalization.create_sole_topic(_unique("Cats"))
    factor_key = app.personalization.topic_factor_key()
    assert not app.personalization.engagement_toggle_on(0)

    # ── 2. The on-topic VIDEO post first, then the newer video decoy ─────
    marker = _unique("clip")
    video_text = f"{CAT_TEXT} {marker}"
    app.driver.set_state({"nav": {"stack": [{"view": "feed"}]}})
    app.feed.create_post_with_video(text=video_text, video_path=str(tiny_video))
    time.sleep(1.2)  # sleep-ok: strictly-newer created_at (epoch-seconds resolution)
    _post_decoy(app, f"quarterly budget spreadsheet {_unique('rows')}")

    # ── 3. A feed the trained factor dominates ───────────────────────────
    feed_name = _unique("cats-feed")
    app.feed.create_feed_with_factor(name=feed_name, factor_label=factor_key, weight="5.0")
    app.feed.open_feed(feed_name)
    assert app.feed.wait_for_post_text(marker, timeout_s=15), (
        f"the video post is missing from the composed feed; error={app.error_text()!r}"
    )
    fetch = ws_api.personalization_model_fetch(port, test_user, factor_key)
    assert not fetch.get("sealed_blob"), "factor must start with no model row"

    # ── 4. Opt in, then clear what the setup's incidental viewing derived ─
    app.personalization.navigate()
    assert app.personalization.wait_for_topic_count(1) == 1
    app.personalization.set_engagement_toggle(0, True)
    app.personalization.clear_engagement_data()

    # ── 5. Re-enter the composed feed; the video post sits below the decoy ─
    app.driver.set_state({"nav": {"stack": [{"view": "feed"}]}})
    app.driver.wait_for("feed-view")
    app.feed.open_feed(feed_name)
    assert app.feed.wait_for_post_text(marker, timeout_s=15)
    target = next(
        (i for i in range(app.feed.post_count()) if marker in (app.feed.post_text(i) or "")),
        None,
    )
    assert target is not None, "the video post is missing after reopening"
    assert target > 0, (
        f"before the play the video post must not lead (the decoy is newer); "
        f"error={app.error_text()!r}"
    )

    # ── 6. Bring the card into view, tap its video, let it play out ──────
    # The scroll waits out the skip window first, so the decoy it moves past
    # derives no `skip` that would train the topic on its own
    # (`_await_on_screen_cards_past_skip`).
    _await_on_screen_cards_past_skip(app)
    app.feed.scroll_post_into_view(target)
    _await_on_screen_cards_past_skip(app)
    tapped_post = app.feed._post_id_of_card(target)
    app.driver.click("video-thumbnail", scope=f"post-card[{target}]")

    def played_s() -> float | None:
        index = _card_of(app, tapped_post, target)
        return None if index is None else _played_s(app, index)

    def tapped_exposure() -> dict | None:
        return next(
            (e for e in app.feed.cue_probe().get("exposures") or []
             if e["post_id"] == tapped_post),
            None,
        )

    # linux, web, macos, ios and windows relay their player's engagement to the
    # shared tracker; while the clip plays, the tapped post's exposure reads as
    # attended (`playing`).
    relays_playing = (
        app.driver.is_linux() or app.driver.is_web()
        or app.driver.is_macos() or app.driver.is_ios()
        or app.driver.is_windows()
    )
    attended: list[dict] = []

    def played_to_its_end() -> float | None:
        if relays_playing and not attended:
            exposure = tapped_exposure()
            if exposure and exposure.get("playing"):
                attended.append(exposure)
        s = played_s()
        if s is not None and s >= 0.95 * tiny_video_s:
            return s
        # Where the play leaves the card under the skip gate (windows), the
        # end-of-play release ends the exposure, its emit trains the topic and
        # the re-rank rebuilds the cards — often before a poll reads the 1 s
        # clip's end off the card. The post's own derived verdict is the same
        # fact, read latency-independently: watch-complete needs the media arm.
        verdict = app.feed.cue_verdict_for_test(tapped_post)
        return tiny_video_s if verdict.get("verdict") == "watch-complete" else None

    # The player reaching (nearly) its end is the causal anchor, polled for
    # (convention 14): 95 % is past the 85 % gate with a tick to spare. Read
    # on the tapped POST's card wherever it now stands (`_card_of`).
    played = wait_until(
        played_to_its_end,
        tiny_video_s + 25.0,
        diagnose=lambda: (
            f"the video never played to its end: position={played_s()!r}, "
            f"tapped post-card[{target}] = {tapped_post[:12]}; cards now: "
            f"{_video_cards(app)}"
        ),
    )
    assert played
    if relays_playing:
        assert attended, (
            f"the capture never showed {tapped_post[:12]}'s exposure attended by "
            f"the play while it played: {app.feed.cue_probe()!r}"
        )

    if app.feed.has_cue_clock():
        # linux: the capture itself shows the played fraction folded into the
        # exposure in progress — what the media arm reads at emit.
        post_id = tapped_post

        def folded():
            exposure = tapped_exposure()
            pm = (exposure or {}).get("media_played_pm")
            if pm is not None and pm >= CUE_COMPLETE_FRACTION_PM:
                return exposure
            # A card the play leaves under the skip gate (windows' card, grown
            # with its player in the 600 x 600 DIP e2e window) ends its exposure
            # at the end-of-play release, and the held peak is taken at that
            # emit (engagement-cues.md, *A play is attention*): the post's own
            # derived verdict is then the witness of the fold.
            verdict = app.feed.cue_verdict_for_test(post_id)
            return verdict if verdict.get("verdict") == "watch-complete" else None

        wait_until(
            folded,
            budgets.RPC_ROUNDTRIP_S,
            diagnose=lambda: f"the capture never folded the play into {post_id[:12]}'s "
                             f"exposure: {app.feed.cue_probe()!r}; verdict "
                             f"{app.feed.cue_verdict_for_test(post_id)!r}",
        )

    # ── 7. Move on: leaving the feed drains the exposure, and the derived
    # verdict weak-trains the topic. (The rollup flush is each app's own
    # leave door — linux's is the window close — and is the other journeys'
    # witness, not this one's.) ──────────────────────────────────────────
    app.personalization.navigate()
    reply = _wait_model_blob(port, test_user, factor_key)
    assert reply.get("sealed_blob"), (
        f"the played-to-the-end video never weak-trained the topic: {reply!r}; "
        f"error={app.error_text()!r}"
    )
    # The played post's own verdict, so no other card's cue can stand in for it.
    verdict = app.feed.cue_verdict_for_test(tapped_post)
    assert verdict.get("verdict") == "watch-complete", (
        f"the played-to-the-end video derived {verdict!r}, not watch-complete"
    )

    # What the topic learned moves an on-topic post above the newer decoy,
    # read after a relaunch (`hard_reload`, every app's driver): it lands on
    # the default feed, so switching to the composed one is a real reload on
    # every app — re-selecting a feed linux kept selected across the detour
    # would start none.
    app.driver.hard_reload()
    _open_feed(app, feed_name)
    top = _wait_top_post_contains(app, CAT_TEXT, timeout=25)
    assert CAT_TEXT in top, (
        f"the composed feed did not re-rank after the media watch-complete; "
        f"top post = {top!r}, error={app.error_text()!r}"
    )
    app.personalization.navigate()

    # Cleanup — the session-scoped test_user is shared across this module.
    assert app.personalization.wait_for_topic_count(1) == 1
    app.personalization.clear_engagement_data()
    app.personalization.delete_topic(0)
    assert app.personalization.wait_for_topic_count(0) == 0


#: fauna_core::scoring::cues::CUE_SKIP_VISIBLE_PM as a fraction — the skip
#: gate a card under which the visibility rule alone would end its exposure.
CUE_SKIP_VISIBILITY = 0.5

#: Where the bottom-edge journey leaves the played card: a quarter of its
#: height above the feed's bottom edge, well under the 50 % gate.
BOTTOM_EDGE_FRACTION = 0.25

#: Newer video decoys composed above the played post — enough height above it
#: for the feed to scroll it down to its bottom edge. Videos, like the media-arm
#: journey's decoy, so none of them can ever complete (the test never plays
#: them).
BOTTOM_EDGE_DECOYS = 7

@pytest.mark.linux
@pytest.mark.web
@pytest.mark.macos
@pytest.mark.ios
@pytest.mark.feature("learn-from-my-activity")
def test_a_video_played_near_the_bottom_edge_still_counts_as_watched(
    logged_in_app, nest_instance, test_user
):
    """*A play is attention* (``engagement-cues.md`` § Cue vocabulary &
    derivation, ruled 2026-10-30): a video card that sits under the 50 % skip
    gate while its player plays to the end still derives ``watch-complete`` on
    the leave — the gate answers "was the reader looking?" for a card that
    merely sits in the viewport, and a play the reader started answers it by
    itself.

    The reader taps a post's video and scrolls on until only a quarter of the
    card shows above the feed's bottom edge, while the clip plays out. The
    placement is the driver's measured bottom-edge scroll
    (``scroll_to_bottom_fraction``, ``e2e-conventions.md`` § Driver primitives
    → *Placing an element at a measured fraction*), and the fraction it
    measured is asserted under the gate. Before the ruling the tracker ended
    the exposure ``ScrolledPast`` at that instant and dropped every later
    reading of the player, so the play taught nothing; now the engaged row is
    attended and its held peak is taken at emit.

    **The witness is valid only if the card went under the gate before the
    play could complete** — otherwise the peak was folded while the card was
    still above the gate, and the old rule would pass too. So the player's
    position is read right after the placement and asserted short of the
    85 % completion line: a state assertion on what happened, not a timing
    assumption (convention 14).

    The setup is the media-arm journey's, with more newer video decoys so the
    feed has the height above the played post to scroll it to the bottom edge.
    """
    app = logged_in_app
    port = nest_instance["port"]
    tiny_video, tiny_video_s = _tiny_video(app)
    assert tiny_video.exists(), f"fixture video missing: {tiny_video}"

    # ── 1. Trained topic, engagement learning still off ──────────────────
    app.personalization.navigate()
    app.personalization.create_sole_topic(_unique("Cats"))
    factor_key = app.personalization.topic_factor_key()
    assert not app.personalization.engagement_toggle_on(0)

    # ── 2. The on-topic VIDEO post first, then the newer video decoys ────
    marker = _unique("edge")
    app.driver.set_state({"nav": {"stack": [{"view": "feed"}]}})
    app.feed.create_post_with_video(text=f"{CAT_TEXT} {marker}", video_path=str(tiny_video))
    time.sleep(1.2)  # sleep-ok: strictly-newer created_at (epoch-seconds resolution)
    for _ in range(BOTTOM_EDGE_DECOYS):
        _post_decoy(app, f"quarterly budget spreadsheet {_unique('rows')}")

    # ── 3. A feed the trained factor dominates ───────────────────────────
    feed_name = _unique("edge-feed")
    app.feed.create_feed_with_factor(name=feed_name, factor_label=factor_key, weight="5.0")
    app.feed.open_feed(feed_name)
    assert app.feed.wait_for_post_text(marker, timeout_s=15), (
        f"the video post is missing from the composed feed; error={app.error_text()!r}"
    )
    fetch = ws_api.personalization_model_fetch(port, test_user, factor_key)
    assert not fetch.get("sealed_blob"), "factor must start with no model row"

    # ── 4. Opt in, then clear what the setup's incidental viewing derived ─
    app.personalization.navigate()
    assert app.personalization.wait_for_topic_count(1) == 1
    app.personalization.set_engagement_toggle(0, True)
    app.personalization.clear_engagement_data()

    # ── 5. Re-enter the composed feed; the video post sits below the decoys
    app.driver.set_state({"nav": {"stack": [{"view": "feed"}]}})
    app.driver.wait_for("feed-view")
    app.feed.open_feed(feed_name)
    assert app.feed.wait_for_post_text(marker, timeout_s=15)
    target = next(
        (i for i in range(app.feed.post_count()) if marker in (app.feed.post_text(i) or "")),
        None,
    )
    assert target is not None, "the video post is missing after reopening"
    assert target >= BOTTOM_EDGE_DECOYS, (
        f"the video post must sit below its {BOTTOM_EDGE_DECOYS} newer decoys, "
        f"found at post-card[{target}]; error={app.error_text()!r}"
    )

    # ── 6. Tap its video, then scroll on until the card sits under the gate
    # Every scroll below waits out the skip window first, so no decoy it
    # moves past derives a `skip` (`_await_on_screen_cards_past_skip`).
    _await_on_screen_cards_past_skip(app)
    app.feed.scroll_post_into_view(target)
    _await_on_screen_cards_past_skip(app)
    tapped_post = app.feed._post_id_of_card(target)
    app.driver.click("video-thumbnail", scope=f"post-card[{target}]")

    def played_s() -> float | None:
        index = _card_of(app, tapped_post, target)
        return None if index is None else _played_s(app, index)

    placed_at = _card_of(app, tapped_post, target)
    assert placed_at is not None, f"the tapped post left the feed: {_video_cards(app)}"
    fraction = app.feed.place_post_at_bottom_edge(placed_at, BOTTOM_EDGE_FRACTION)
    position_at_placement = played_s()
    assert fraction < CUE_SKIP_VISIBILITY, (
        f"the bottom-edge scroll left {fraction:.2f} of post-card[{placed_at}] "
        f"visible, not under the {CUE_SKIP_VISIBILITY:.0%} skip gate — the feed "
        f"could not scroll it that far (too little height above it?); tapped "
        f"post {tapped_post[:12]} was post-card[{target}]; cards now: "
        f"{_video_cards(app)}; error={app.error_text()!r}"
    )
    assert position_at_placement is None or position_at_placement < 0.85 * tiny_video_s, (
        f"the clip was already at {position_at_placement:.2f}s of {tiny_video_s}s when "
        f"the card went under the gate — past the 85 % completion line, so the "
        f"peak was folded above the gate and this run witnesses nothing"
    )

    def tapped_exposure() -> dict | None:
        return next(
            (e for e in app.feed.cue_probe().get("exposures") or []
             if e["post_id"] == tapped_post),
            None,
        )

    # While the clip plays under the gate the capture shows the exposure open
    # and attended (`playing`) — the rule's part 2, read off the shared probe.
    attended: list[dict] = []

    def played_to_its_end() -> float | None:
        if not attended:
            exposure = tapped_exposure()
            if exposure and exposure.get("playing"):
                attended.append(exposure)
        s = played_s()
        return s if s is not None and s >= 0.95 * tiny_video_s else None

    played = wait_until(
        played_to_its_end,
        tiny_video_s + 25.0,
        diagnose=lambda: (
            f"the video never played to its end under the gate: position={played_s()!r}, "
            f"tapped post {tapped_post[:12]}; cards now: {_video_cards(app)}"
        ),
    )
    assert played
    assert attended, (
        f"the capture never showed {tapped_post[:12]}'s exposure attended by the "
        f"play while its card sat under the gate: {app.feed.cue_probe()!r}"
    )

    if app.driver.is_linux() or app.driver.is_web():
        # linux and web: the exposure still open under the gate carries the
        # played fraction — the held peak the media arm reads at emit. Apple
        # folds the peak at the NEXT sample and the leave drains it, so by the
        # time this poll reads, the exposure can already have ended and the
        # learned topic re-ranked the feed (the post is no longer tracked): the
        # state to read there is the verdict below, not this transient.
        def folded():
            exposure = tapped_exposure()
            pm = (exposure or {}).get("media_played_pm")
            return exposure if pm is not None and pm >= CUE_COMPLETE_FRACTION_PM else None

        wait_until(
            folded,
            budgets.RPC_ROUNDTRIP_S,
            diagnose=lambda: f"the capture never folded the play into {tapped_post[:12]}'s "
                             f"exposure under the gate: {app.feed.cue_probe()!r}",
        )

    # ── 7. Move on: the leave drains the exposure and the verdict
    # weak-trains the topic ──────────────────────────────────────────────
    app.personalization.navigate()
    reply = _wait_model_blob(port, test_user, factor_key)
    assert reply.get("sealed_blob"), (
        f"the video played to its end under the gate never weak-trained the topic: "
        f"{reply!r}; error={app.error_text()!r}"
    )
    # The played post's own verdict, so no other card's cue can stand in for it.
    verdict = app.feed.cue_verdict_for_test(tapped_post)
    assert verdict.get("verdict") == "watch-complete", (
        f"the video played to its end under the gate derived {verdict!r}, not "
        f"watch-complete"
    )

    # What the topic learned moves an on-topic post above the newer decoys,
    # read after a relaunch (as in the media-arm journey above).
    app.driver.hard_reload()
    _open_feed(app, feed_name)
    top = _wait_top_post_contains(app, CAT_TEXT, timeout=25)
    assert CAT_TEXT in top, (
        f"the composed feed did not re-rank after the bottom-edge watch-complete; "
        f"top post = {top!r}, error={app.error_text()!r}"
    )
    app.personalization.navigate()

    # Cleanup — the session-scoped test_user is shared across this module.
    assert app.personalization.wait_for_topic_count(1) == 1
    app.personalization.clear_engagement_data()
    app.personalization.delete_topic(0)
    assert app.personalization.wait_for_topic_count(0) == 0


# Bytes that are no sealed rollup: the AEAD open refuses them, exactly as it
# refuses a corrupted row or one sealed under a key this device does not hold.
UNOPENABLE_ROLLUP = b"not a sealed cue rollup -- the open must refuse it"


@pytest.mark.tui
@pytest.mark.linux
@pytest.mark.web
@pytest.mark.macos
@pytest.mark.ios
@pytest.mark.windows
@pytest.mark.feature("learn-from-my-activity")
def test_saved_activity_data_that_cannot_be_opened_is_reported_not_started_over(
    logged_in_app, nest_instance, test_user
):
    """Outcome 13 (`engagement-cues.md` § Implementation status today:
    `hydrate_cues()` treats an unopenable `cues:v1` rollup as a surfaced error,
    the `fetch_model` hard-fail rule).

    The saved activity data is made unopenable as an environment fault — the
    `cues:v1` row overwritten, through the test's own WS-RPC seat, with bytes
    that will not open (the nest stores the blob opaque) — not as anything a
    user does, so convention 8 has no gesture to route it through. The app
    reads the rollup once, at session start, so the leg is a relaunch: the
    feed page it opens on says the data would not open, and the row still
    holds the same bytes rather than a fresh rollup.

    The error is read straight after the relaunch, before any feed gesture:
    the first feed operation to succeed clears the page's error slot.
    """
    app = logged_in_app
    port = nest_instance["port"]

    before = ws_api.personalization_model_fetch(port, test_user, CUES_ROLLUP_FACTOR)
    ws_api.personalization_model_put(
        port, test_user, CUES_ROLLUP_FACTOR, UNOPENABLE_ROLLUP, 5
    )
    try:
        app.driver.hard_reload()  # relaunch + session replay: hydrate meets the fault
        app.driver.wait_for("feed-view")
        error = wait_until(
            lambda: (lambda e: e if "sealed model failed to decrypt" in e else "")(
                app.error_text() or ""
            ),
            budgets.RPC_ROUNDTRIP_S,
            diagnose=lambda: f"error line reads {app.error_text()!r}",
        )
        assert error
        row = ws_api.personalization_model_fetch(port, test_user, CUES_ROLLUP_FACTOR)
        assert bytes(row.get("sealed_blob") or b"") == UNOPENABLE_ROLLUP and row[
            "sample_count"
        ] == 5, f"the app replaced the unopenable activity data: {row!r}"
    finally:
        # Put the session actor's own row back, then relaunch so the app holds
        # a hydrated rollup again: the actor and the app outlive this test.
        if before.get("sealed_blob"):
            ws_api.personalization_model_put(
                port, test_user, CUES_ROLLUP_FACTOR,
                bytes(before["sealed_blob"]), before.get("sample_count", 0),
            )
        else:
            ws_api.personalization_model_delete(port, test_user, CUES_ROLLUP_FACTOR)
        app.driver.hard_reload()


# ── Skips and flick-throughs: the frozen cue clock ──────────────────────────
#
# A skip is an exposure SHORTER than CUE_SKIP_MS (1 200 ms), and a fast-scroll
# burst is two exposures ending LESS than CUE_BURST_MIN_GAP_MS (400 ms) apart —
# both bounded from above, so a witness that let real time pass between scroll
# steps would go red on a loaded box for no product reason (convention 14).
# These journeys freeze the capture clocks before any post is on screen (the
# shared tracker's, `fauna_feed::CueTracker` § The test clock, on every app
# that runs them) and advance them by exact amounts; the scroll is real (tui's
# focus ring, the other apps' pixel scroll position), the geometry what the
# app laid out, the derivation the shared engine. Frozen, a card accrues exactly what
# the test advanced while it was in view, so every setup exposure ends with no
# dwell and derives nothing.
#
# One scroll sequence serves outcomes 5 and 6, which differ only in pace: the
# list opens with a video decoy above the on-topic post; the scroll walks down
# until that decoy has left view while the on-topic post is still in it (the
# decoy's exposure ends at wall time W, with no dwell); the clock advances by
# PACE; the scroll walks on until the on-topic post leaves view — alone, in one
# step, so its exposure ends `ScrolledPast` at W + PACE after exactly PACE of
# dwell. PACE = 600 ms is a skip (briefly seen, at reading pace); PACE = 100 ms
# is a flick-through (the previous exposure ended 100 ms earlier).

SKIP_PACE_MS = 600    # < CUE_SKIP_MS, ≥ CUE_BURST_MIN_GAP_MS
FLICK_PACE_MS = 100   # > 0, < CUE_BURST_MIN_GAP_MS
CUE_DWELL_LONG_MS = 8_000


def _set_up_on_topic_post_below_a_decoy(app) -> tuple[str, str, str, str]:
    """Freeze the cue clocks, then build a composed feed whose list reads: a
    video decoy, the on-topic text post, then six older video decoys — more
    than any app's viewport holds, so the on-topic post can be scrolled out of
    view.

    The feed holds these eight posts and nothing else: its ``BodyContains``
    rule names a token only they carry. The session's test user keeps every
    post earlier tests made, and a skip sinks the on-topic post to the very
    bottom of the list, among the oldest of them. Those are text and image
    posts, so outcome 7's long linger would complete them too, train the topic
    on their words and erase the rank differential (the module doc's
    selectivity note) — this journey went red only after such a test, and
    passed alone.

    Returns ``(factor_key, cat_text, cat_post_id, below_text)`` —
    ``below_text`` is the decoy directly under the on-topic post. The on-topic
    text carries a token of its own too: the session's test user keeps the same
    vocabulary from earlier tests. The topic learns from activity
    from the start: with the clocks frozen nothing on screen accrues any
    dwell, so setup derives no verdict to train on. The decoys are unplayed
    video posts so the long linger of outcome 7 cannot complete them (the module doc's
    selectivity note)."""
    app.driver.set_state({"nav": {"stack": [{"view": "feed"}]}})
    app.driver.wait_for("feed-view")
    app.feed.freeze_cue_clock()

    app.personalization.navigate()
    app.personalization.create_sole_topic(_unique("Cats"))
    factor_key = app.personalization.topic_factor_key()
    app.personalization.set_engagement_toggle(0, True)
    app.personalization.clear_engagement_data()

    app.driver.set_state({"nav": {"stack": [{"view": "feed"}]}})
    app.feed.open_feed("General")
    # Oldest first: an untrained composed feed lists newest first.
    journey = _unique("journey")
    cat_text = f"{CAT_TEXT} {_unique('paw')} {journey}"
    below_text = f"garden hose nozzle {_unique('spray')} {journey}"
    for text in (f"harbour ferry schedule {_unique('dock')} {journey}",
                 f"office printer toner {_unique('page')} {journey}",
                 f"parking garage levels {_unique('ramp')} {journey}",
                 f"commuter rail timetable {_unique('stop')} {journey}",
                 f"quarterly budget spreadsheet {_unique('rows')} {journey}",
                 below_text):
        _post_decoy(app, text)
        time.sleep(1.2)  # sleep-ok: strictly-older created_at (epoch-seconds resolution)
    app.feed.create_post(text=cat_text)
    time.sleep(1.2)  # sleep-ok: strictly-older created_at (epoch-seconds resolution)
    _post_decoy(app, f"bicycle chain grease {_unique('gear')} {journey}")

    feed_name = _unique("cats-feed")
    app.feed.create_feed_with_factor(
        name=feed_name, factor_label=factor_key, weight="5.0",
        rule=("BodyContains", journey),
    )
    app.feed.open_feed(feed_name)
    order = wait_until(
        lambda: (lambda o: o if len(o) == 8 else None)(_rendered_order(app)),
        budgets.RPC_ROUNDTRIP_S,
        diagnose=lambda: f"composed feed reads {_rendered_order(app)!r}; "
                         f"error={app.error_text()!r}",
    )
    assert cat_text in order[1] and below_text in order[2], (
        f"the on-topic post must open second, above {below_text!r}: {order!r}"
    )
    cat = [p for p in app.feed._feed_posts_from_state() if cat_text in (p.get("body") or "")]
    assert len(cat) == 1, f"the on-topic post is not in the feed state once: {cat!r}"
    return factor_key, cat_text, cat[0]["post_id"], below_text


def _rendered_order(app) -> list[str]:
    return [(app.feed.post_text(i) or "") for i in range(app.feed.post_count())]


def _position(order: list[str], needle: str) -> int:
    hits = [i for i, text in enumerate(order) if needle in text]
    assert len(hits) == 1, f"{needle!r} is not in the feed exactly once: {order!r}"
    return hits[0]


def _pass_the_on_topic_post(app, cat_id: str, pace_ms: int) -> None:
    """The shared scroll sequence (the note above), at ``pace_ms``."""
    start = app.feed.scroll_feed_to_top()
    probe = app.feed.cue_probe()
    assert probe["frozen"], f"the cue clock thawed: {probe!r}"

    def above_cat_gone(p):
        card = next((c for c in p["cards"] if c["post_id"] == cat_id), None)
        if card is None or cat_id not in p["tracked"]:
            return False
        above = {c["post_id"] for c in p["cards"] if c["top"] < card["top"]}
        return bool(above) and not above & set(p["tracked"])

    _, _, resume = app.feed.scroll_feed_until(
        start, above_cat_gone,
        what="had the on-topic post in view with every card above it scrolled out",
    )
    app.feed.advance_cue_clock(pace_ms)
    before, after, _ = app.feed.scroll_feed_until(
        resume, lambda p: cat_id not in p["tracked"],
        what="scrolled the on-topic post out of view",
    )
    left = set(before["tracked"]) - set(after["tracked"])
    assert left == {cat_id}, (
        f"the step that scrolls the on-topic post out must move it out alone "
        f"(a second card ending in the same sample reads as a burst): "
        f"left={sorted(left)!r}, before={before!r}, after={after!r}"
    )


@pytest.mark.tui
@pytest.mark.linux
@pytest.mark.web
@pytest.mark.android
@pytest.mark.macos
@pytest.mark.ios
@pytest.mark.windows
@pytest.mark.feature("learn-from-my-activity")
def test_scrolling_past_a_post_teaches_less_like_this_and_lingering_later_takes_it_back(
    logged_in_app, nest_instance, test_user
):
    """Outcomes 5 and 7 (`engagement-cues.md` § Cue vocabulary & derivation:
    a skip is ≥ 50 % visible for under `CUE_SKIP_MS` before scrolling on, at
    reading pace, and verdicts are last-wins; § Layer A: a skip trains a weak
    negative, reversible through the verdict flip).

    The on-topic post is scrolled past after 600 ms in view: the skip trains
    the topic away from it, and it sinks below the decoy that was under it.
    Then it is brought back into view and held past the long-dwell gate: the
    skip flips to watch-complete, the topic takes the negative back and learns
    the positive, and the post rises to the top."""
    app = logged_in_app
    port = nest_instance["port"]
    try:
        factor_key, cat_text, cat_id, below_text = _set_up_on_topic_post_below_a_decoy(app)
        assert not ws_api.personalization_model_fetch(port, test_user, factor_key).get(
            "sealed_blob"
        ), "setup must teach the topic nothing"

        # ── Outcome 5: scrolled past at reading pace → less like this ────
        _pass_the_on_topic_post(app, cat_id, SKIP_PACE_MS)
        skipped = _wait_model_blob(port, test_user, factor_key)
        assert skipped.get("sealed_blob"), (
            f"the skip never trained the topic: {skipped!r}; error={app.error_text()!r}"
        )
        order = wait_until(
            lambda: (lambda o: o if _position(o, cat_text) > _position(o, below_text)
                     else None)(_rendered_order(app)),
            budgets.RPC_ROUNDTRIP_S,
            diagnose=lambda: f"feed reads {_rendered_order(app)!r}; "
                             f"error={app.error_text()!r}",
        )

        # ── Outcome 7: a later linger takes the skip back ───────────────
        app.feed.bring_post_into_dwell_view(_position(order, cat_text))
        probe = app.feed.cue_probe()
        assert cat_id in probe["tracked"], f"the on-topic post is not in view: {probe!r}"
        probe = app.feed.advance_cue_clock(CUE_DWELL_LONG_MS + 1_000)
        assert cat_id in probe["tracked"], f"the linger lost the post: {probe!r}"
        # Leaving the page ends the exposure; a watch-complete derives however
        # its exposure ended.
        app.personalization.navigate()
        relearned = wait_until(
            lambda: (lambda r: r if r.get("sealed_blob") != skipped["sealed_blob"] else None)(
                ws_api.personalization_model_fetch(port, test_user, factor_key)
            ),
            budgets.RPC_ROUNDTRIP_S,
            diagnose=lambda: f"the topic model never changed after the linger; "
                             f"error={app.error_text()!r}",
        )
        assert relearned.get("sealed_blob")
        app.feed.return_to_feed_page()
        top = _wait_top_post_contains(app, cat_text)
        assert cat_text in top, (
            f"the linger did not take the skip back: feed reads {_rendered_order(app)!r}; "
            f"error={app.error_text()!r}"
        )
    finally:
        app.feed.thaw_cue_clock()

    app.personalization.navigate()
    app.personalization.clear_engagement_data()
    app.personalization.delete_topic(0)
    assert app.personalization.wait_for_topic_count(0) == 0


@pytest.mark.tui
@pytest.mark.linux
@pytest.mark.web
@pytest.mark.android
@pytest.mark.macos
@pytest.mark.ios
@pytest.mark.windows
@pytest.mark.feature("learn-from-my-activity")
def test_flicking_fast_past_a_post_teaches_nothing(
    logged_in_app, nest_instance, test_user
):
    """Outcome 6 (`engagement-cues.md` § Cue vocabulary & derivation: a
    fast-scroll burst derives nothing — flick-throughs are not judgments).

    The same scroll sequence as the skip above with only the pace changed:
    the on-topic post leaves view 100 ms after the card above it did, after
    100 ms in view. Nothing is learned — the order stands, and once no put
    can still be on its way (the app has exited; on web, android, macos and
    ios, the scroll step has acked) the topic has no model."""
    app = logged_in_app
    port = nest_instance["port"]
    factor_key, cat_text, cat_id, below_text = _set_up_on_topic_post_below_a_decoy(app)

    _pass_the_on_topic_post(app, cat_id, FLICK_PACE_MS)
    order = _rendered_order(app)
    assert _position(order, cat_text) < _position(order, below_text), (
        f"a flick-through moved the on-topic post: {order!r}"
    )
    if not app.feed.cue_step_is_the_record_barrier():
        _quit_through_the_apps_own_leave_door(app)
        assert app.driver.wait_app_exit(APP_EXIT_S), (
            f"app did not exit; still alive={app.driver.is_app_alive()}"
        )
    # A browser tab, a phone app or a macOS app (which outlives its last
    # window) has no quit door a test can drive and wait out; there the barrier
    # is the scroll step itself — it acked only once every exposure it ended was
    # recorded, a train and its put included (web's `#lib/feed-cues-e2e`,
    # android's TestAgent `feedCueCommand`, apple's `FeedCueClockTestCommand`).
    learned = ws_api.personalization_model_fetch(port, test_user, factor_key)
    assert not learned.get("sealed_blob"), f"a flick-through taught the topic: {learned!r}"

    app.driver.hard_reload()
    app.personalization.navigate()
    assert app.personalization.wait_for_topic_count(1) == 1
    app.personalization.clear_engagement_data()
    app.personalization.delete_topic(0)
    assert app.personalization.wait_for_topic_count(0) == 0
