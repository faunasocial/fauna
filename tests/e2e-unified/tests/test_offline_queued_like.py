"""A like given while the nest cannot be reached is queued, survives closing the
app, and reaches the nest once it is back.

Owners:

* ``docs/goal/architecture/account-offline-mutation.md`` § The offline-mutation
  contract, discriminator 1: an ``OfflineQueued`` mutation is not a store
  item — it awaits an effect the nest assigns, so with no nest it rests as a
  durable intent in the device's outbox and drains, keyed by its intent id, when
  the nest is back. ``fauna.posts.interact`` is ``OfflineQueued`` (the like
  counter is the nest's to assign), and the shared feed manager composes it
  through ``fauna_protocol::offline_intent::send_or_queue`` — the first
  production caller of the outbox.
* ``docs/features/offline-aware-controls.md`` outcome 6 — this is its queued
  form; ``test_offline_preference_edits.py`` witnesses its offline-safe form.

The discriminator for "queued, not failed" is the like toggle itself:
``FeedManager::like`` lights ``viewer_liked`` only when the interact call
succeeded, so a lit toggle with the nest down means the composer queued the act
instead of sending it into a dead connection (which would leave the toggle dark
and paint ``error-message``). The count stays the nest's: it moves only after the
nest is back and the device's pump has drained the intent.

tier_3: the real nest, taken down and brought back on the same data dir; a real
seat; every mutation a UI gesture (convention 8); every wait a deadline poll on
painted state (convention 14). tui first (``testing.md`` § Default app and nest
mode), then linux and web (their batch: the shared FFI door, the linux feed host,
the web feed manager); the remaining apps add their marker as theirs reaches them.
"""
from __future__ import annotations

import time

import pytest

from common.nest import start_nest_in_place
from helpers.fleet import poked_pass
from helpers.nest_outage import hold_nest_down
from helpers.waiting import wait_until

pytestmark = [
    pytest.mark.tier_3,
    pytest.mark.tui,
    pytest.mark.linux,
    pytest.mark.web,
    pytest.mark.macos,
    pytest.mark.ios,
]

#: A post composed online reaching the seat's feed state.
POST_S = 120.0

#: The offline tap lighting the toggle (a local outbox write).
QUEUE_S = 60.0

#: The returning nest, the restarted seat's drain, and the feed re-reading it.
CONVERGE_S = 300.0


def _stamp() -> int:
    return int(time.time() * 1000) % 1_000_000




@pytest.mark.feature("offline-aware-controls")
def test_a_like_given_offline_is_queued_kept_across_restart_and_reaches_the_nest(
    app, request, nest_instance
):
    from conftest import _make_user
    from helpers import enrollment

    user = _make_user(nest_instance)
    enrollment.sign_in(app, request, nest_instance, user)
    enrollment.await_enrollment(app, nest_instance["url"], user)

    text = f"offline-like {_stamp():06d}"
    app.feed.navigate()
    app.feed.create_post(text=text)
    row = app.feed.wait_for_post_state_by_text(text, timeout=POST_S)
    assert row is not None and row.get("post_id"), (
        f"the online post {text!r} never reached the feed state; "
        f"error-message {app.error_text()!r} — nothing below can be trusted"
    )
    post_id = row["post_id"]
    assert row.get("like_count") == 0 and not row.get("viewer_liked"), (
        f"a fresh post starts un-liked at 0; got {row!r}"
    )

    # The restart must be THIS device restarting: without the pin a relaunch
    # gets an empty store, and the queued intent would be gone for that reason.
    assert app.driver.preserve_state_across_relaunch(), (
        "tui could not pin its client store across the relaunch, so the restart "
        "half of this outcome would be vacuous"
    )

    hold_nest_down(app, nest_instance, act="the like below")
    try:
        app.feed.like_post(app.feed.card_index_by_id(post_id))
        seen: dict = {}

        def toggle_lit() -> bool:
            seen["row"] = app.feed.post_state_by_id(post_id)
            return bool(seen["row"] and seen["row"].get("viewer_liked"))

        wait_until(
            toggle_lit, QUEUE_S,
            diagnose=lambda: (
                f"the like tapped with the nest down never lit its toggle: row "
                f"{seen.get('row')!r}, error-message {app.error_text()!r} — the "
                "tap was sent into the dead connection and failed instead of "
                "being queued in the outbox"
            ),
        )
        assert seen["row"].get("like_count") == 0, (
            f"the count is the nest's to assign; with the nest down it must not "
            f"move, got {seen['row']!r}"
        )

        # Close the app and open it again, the nest still down: the like lives
        # only in this device's outbox now.
        app.driver.hard_reload()
    finally:
        start_nest_in_place(nest_instance)

    # The nest is back. The count moves only when the nest records the like, so
    # seeing it move proves the restarted seat drained the queued intent.
    counts: dict = {}

    def nest_counted_it() -> bool:
        poked_pass(app.driver, what="the restarted seat's outbox drain")
        app.feed.reenter()
        got = app.feed.wait_for_interaction_count_by_id(post_id, "like", 1, timeout=5.0)
        counts["like"] = got
        return got == 1

    wait_until(
        nest_counted_it, CONVERGE_S, interval=1.0,
        diagnose=lambda: (
            f"after the nest came back the post's like count reads "
            f"{counts.get('like')!r}, not 1 — the like queued offline and kept "
            f"across the restart never drained to the nest. Row "
            f"{app.feed.post_state_by_id(post_id)!r}, error-message "
            f"{app.error_text()!r}"
        ),
    )
