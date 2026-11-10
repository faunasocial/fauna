"""tier_3 e2e: the post's mark-as-spam verb, end to end through the app UI.

Target state: ``docs/goal/behavior/report-sharing.md`` § Report capture → *The
post's mark-as-spam verb* (ruled 2026-11-07): ``feed-post-mark-as-spam-button``
on ``feed-post-actions-menu``, gated ``!is_own``, a toggle whose one press has
three effects — the nest half (``fauna.moderation.train {verdict:"spam"}``, the
opt-in-gated ``report:spam`` capture), the marker's own view (the post id joins
the sealed ``spam_marked`` list and the shared ``FeedManager`` folds ``spam``@1000
into the post's labels, so the render verdict collapses it at the marker's own
slider), and undo by the verb itself. ``ui/feed.md`` § User actions renders the
item and its ``state``.

Two journeys:

1. **The shared count reached by a UI gesture** — the first witness of k = 3
   driven by an app: two other actors opt in and flag the post over the wire
   (their own acts — the seed), the app's user (opted in) marks it through the
   ⋯ verb → the post collapses for them at once, and the nest's feed read now
   carries ``spam``@200 for it (``test_report_sharing.py``'s API assertions are
   the model). After a reveal the verb reads ``state=on``; the un-mark drops the
   aggregate below k (``report_share.status.published`` loses the entry), the
   served label goes, and the app's own fold goes with it.
2. **The plain case** — sharing off: the mark still collapses the marker's own
   view, and the nest captures no report row.

Mutations are UI-only (convention 8) save the OTHER actors' posts, opt-ins and
flags, which are their own acts through the nest API, and the app user's
report-sharing opt-in, which is arrangement (restored at teardown). Waits are
named budgets over deadline polls (convention 14).

App arms: **tui** (the lead app); web, linux and android (the first trickle-down;
android runs on the emulator host); macos and ios (one shared FaunaKit paint);
windows follows.
"""

import sqlite3
import uuid

import pytest

from actions.api_actor import ApiActor
from common.auth import create_actor_and_register
from helpers.waiting import wait_until
from tests.api import ws_api

pytestmark = [
    pytest.mark.tier_3, pytest.mark.tui, pytest.mark.web, pytest.mark.linux, pytest.mark.android,
    pytest.mark.macos, pytest.mark.ios,
]

_FEED_BUDGET_S = 30.0     # a re-selected feed reflecting a new post
_SEND_BUDGET_S = 60.0     # one gesture's plane write + WS-RPC round trip + repaint
_K = 3                    # report-sharing.md § The k-anonymity floor


def _actor(nest_instance):
    other = create_actor_and_register(
        nest_instance["port"],
        base_url=nest_instance["url"],
        admin_signing_key=nest_instance["admin"]["signing_key"],
    )
    return other, ApiActor(
        nest_instance["url"], other["token"], other["actor_id_hex"],
        bytes(other["signing_key"]),
    )


def _seed_post(app, poster, text) -> tuple[str, int]:
    """The OTHER actor posts; the app re-selects its feed and finds the card."""
    post_id = poster.post_to_feed("", text)
    assert post_id, "the other actor's post was not created"
    app.feed.navigate()
    app.feed.open_feed("General")
    assert app.feed.wait_for_post_text(text, timeout_s=_FEED_BUDGET_S), (
        f"the other actor's post should appear in the feed; error={app.error_text()!r}"
    )
    return post_id, app.feed.post_index_by_text(text)


def _served_labels(port, viewer, post_id):
    posts = {p["post_id"]: p for p in ws_api.local_feed_posts(port, viewer, limit=50)}
    assert post_id in posts, f"post {post_id} not in the local feed"
    return posts[post_id].get("labels", [])


def _published_count(port, actor, post_id):
    for e in ws_api.report_share_status(port, actor)["published"]:
        if e.get("content_hash") == post_id and e.get("factor") == "report:spam":
            return e.get("count")
    return None


def _report_rows(db_path, post_id) -> int:
    conn = sqlite3.connect(f"file:{db_path}?mode=ro", uri=True)
    try:
        return conn.execute(
            "SELECT COUNT(*) FROM content_reports WHERE content_hash = ?",
            (bytes.fromhex(post_id),),
        ).fetchone()[0]
    finally:
        conn.close()


def _app_labels(app, post_id):
    row = app.feed.post_state_by_id(post_id) or {}
    return row.get("labels", [])


@pytest.fixture()
def sharing_user(nest_instance, test_user):
    """The app user's report-sharing opt-in, restored to off at teardown (an
    opt-out also withdraws the rows the journey left)."""
    port = nest_instance["port"]
    yield lambda share: ws_api.report_share_set(port, test_user, share)
    ws_api.report_share_set(port, test_user, False)


def test_one_mark_reaches_k_on_the_nest_and_collapses_the_post_for_the_marker(
    logged_in_app, nest_instance, test_user, sharing_user
):
    app = logged_in_app
    port = nest_instance["port"]
    poster_dict, poster = _actor(nest_instance)
    text = f"Claim your prize now {uuid.uuid4().hex[:8]} at http://prize.example.test"

    flaggers = [_actor(nest_instance)[0] for _ in range(_K - 1)]
    assert sharing_user(True)["share"] is True
    post_id, idx = _seed_post(app, poster, text)
    for flagger in flaggers:
        assert ws_api.report_share_set(port, flagger, True)["share"] is True
        assert ws_api.moderation_train(port, flagger, post_id, "spam")["status"] == "trained"
    assert _published_count(port, poster_dict, post_id) is None, "k-1 flags publish nothing"

    # ── The gesture: one press through the ⋯ verb ──
    app.feed.mark_post_spam(idx)
    assert wait_until(lambda: app.feed.post_collapsed(idx), _SEND_BUDGET_S,
                      diagnose=lambda: (
                          f"the marked post should collapse for the marker; "
                          f"labels={_app_labels(app, post_id)!r} error={app.error_text()!r} "
                          f"{app.driver.diagnose('content-policy-blocked-notice')}")), \
        "collapse"
    assert not app.has_error(), f"the verb raised: {app.error_text()!r}"

    # The nest: the k-th distinct reporter came from the UI gesture.
    assert wait_until(lambda: _published_count(port, poster_dict, post_id), _SEND_BUDGET_S,
                      diagnose=lambda: "the mark never reached the nest's aggregate") == _K
    assert _served_labels(port, poster_dict, post_id) == [
        {"category": "spam", "confidence_per_mille": 200}
    ], "the k=3 aggregate must reach the post as spam@200"

    # ── Undo: reveal, the verb reads on, the second press un-marks ──
    app.feed.reveal_collapsed_post(idx)
    app.feed.open_post_actions(index=idx)
    assert app.feed.spam_verb_state() == "on", (
        f"a marked post's verb reads on; "
        f"{app.driver.diagnose('feed-post-mark-as-spam-button')}"
    )
    app.feed.unmark_post_spam(idx)
    assert wait_until(lambda: _published_count(port, poster_dict, post_id) is None,
                      _SEND_BUDGET_S,
                      diagnose=lambda: "the un-mark never withdrew the reporter's row"), \
        "withdrawn"
    assert _served_labels(port, poster_dict, post_id) == [], "below k the label goes"
    assert wait_until(
        lambda: not any(l.get("category") == "spam" for l in _app_labels(app, post_id)),
        _SEND_BUDGET_S,
        diagnose=lambda: f"the app's own fold should go; labels={_app_labels(app, post_id)!r}",
    ), "fold gone"
    assert app.driver.count("feed-post-text", scope=f"post-card[{idx}]") >= 1, (
        "the un-marked post paints its text"
    )
    app.feed.open_post_actions(index=idx)
    assert app.feed.spam_verb_state() == "off"
    assert not app.has_error(), f"the un-mark raised: {app.error_text()!r}"


def test_with_sharing_off_the_mark_collapses_the_post_and_the_nest_captures_nothing(
    logged_in_app, nest_instance, test_user, sharing_user
):
    app = logged_in_app
    assert sharing_user(False)["share"] is False
    _, poster = _actor(nest_instance)
    text = f"Cheap watches {uuid.uuid4().hex[:8]} for everyone"
    post_id, idx = _seed_post(app, poster, text)

    app.feed.mark_post_spam(idx)
    assert wait_until(lambda: app.feed.post_collapsed(idx), _SEND_BUDGET_S,
                      diagnose=lambda: (
                          f"the mark must collapse the marker's own view; "
                          f"labels={_app_labels(app, post_id)!r} error={app.error_text()!r}")), \
        "collapse"
    assert {"category": "spam", "confidence_per_mille": 1000} in _app_labels(app, post_id)
    # The nest half ran (no error) and captured nothing: the reporter is opted out.
    assert not app.has_error(), f"the verb raised: {app.error_text()!r}"
    assert _report_rows(nest_instance["db_path"], post_id) == 0, (
        "an opted-out marker's press must capture no report row"
    )
