"""The feed's reaction pills (feed.md § Layout & flow → *Reaction posts count
and paint*, rulings 2 and 4; `post-reaction-pill`, user-approved 2026-11-01):
a wordless ``React { emoji }`` post by someone else paints a pill under my
post's interaction bar — the emoji and its count, not mine — and a tap on it is
``FeedManager::react(post_id, emoji)``, the repost toggle's semantics: my own
reaction of that emoji joins the tally (highlighted), a second tap takes it
back. A repost row paints no pill; the post's ``post_detail`` paints the same
pill unscoped.

tier_3: a real ``fauna-nest`` binary indexes the other person's signed React
post into the actor-keyed ``react`` link the tally is read from, and the tap
composes and deletes my own React post over the wire — no local-only fold.
The other person's two posts (the reaction and a repost) are the setup, sent as
the ordinary signed posts any app would create, not the journey under test.
"""
import re
import time
import uuid

import pytest

from common.auth import create_actor_and_register
from helpers.waiting import wait_until
from tests.api import ws_api
from tests.api.bare import post_reference, sign_and_encode_post

pytestmark = pytest.mark.tier_3

# One code point, so the pill's text has no variation selector to compare.
EMOJI = "\U0001f525"

# How long the page may take to follow the state it paints — a ceiling for a
# broken surface, never a subject (convention 14).
REPAINT_BUDGET_S = 30.0


def _unique(prefix: str) -> str:
    return f"{prefix}-{uuid.uuid4().hex[:8]}"


def _count(pill: dict) -> int | None:
    """The count a pill's text carries after its emoji (``"🔥 2"`` → 2; a
    trailing own-reaction mark, where the app paints one, is ignored)."""
    text = pill["text"]
    if EMOJI not in text:
        return None
    m = re.search(r"(\d+)", text.split(EMOJI, 1)[1])
    return int(m.group(1)) if m else None


@pytest.mark.tui
@pytest.mark.linux
@pytest.mark.web
@pytest.mark.macos
@pytest.mark.ios
@pytest.mark.android
@pytest.mark.windows
@pytest.mark.feature("feed-interactions")
def test_reaction_pill_paints_counts_and_toggles_my_own_reaction(
    logged_in_app, nest_instance
):
    app = logged_in_app
    feed = app.feed

    mine = _unique("reacted-post")
    feed.create_post(text=mine)
    row = feed.wait_for_post_state_by_text(mine)
    assert row is not None and row.get("post_id"), (
        f"own post {mine!r} should be readable from feed state; "
        f"error={app.error_text()!r}"
    )
    target_id = row["post_id"]

    other = create_actor_and_register(
        nest_instance["port"],
        base_url=nest_instance["url"],
        admin_signing_key=nest_instance["admin"]["signing_key"],
    )
    now_us = int(time.time() * 1_000_000)
    ws_api.create_post(
        nest_instance["port"], other,
        sign_and_encode_post(
            other["signing_key"], now_us, "",
            references=[post_reference("React", target_id, emoji=EMOJI)],
        ),
    )
    ws_api.create_post(
        nest_instance["port"], other,
        sign_and_encode_post(
            other["signing_key"], now_us + 1, "",
            references=[post_reference("Repost", target_id)],
        ),
    )

    # No app live-reloads on another actor's out-of-band post; re-select the
    # feed the way a user would (the `test_feed_post_delete.py` pattern).
    feed.open_feed("General")

    def pills():
        at = feed.post_index_by_id(target_id)
        return [] if at < 0 else feed.reaction_pills(at)

    def diagnose():
        return (
            f"pills={pills()!r} state row={feed.post_state_by_id(target_id)!r} "
            f"error={app.error_text()!r}"
        )

    def reads(count: int, state: str):
        def check():
            got = pills()
            return len(got) == 1 and _count(got[0]) == count and got[0]["state"] == state
        return check

    # Someone else's reaction: one pill, the emoji and 1, not mine.
    wait_until(reads(1, "off"), REPAINT_BUDGET_S, diagnose=diagnose)

    # My tap joins the tally, highlighted.
    feed.toggle_reaction_pill(feed.post_index_by_id(target_id))
    wait_until(reads(2, "on"), REPAINT_BUDGET_S, diagnose=diagnose)

    # A second tap takes my reaction back; theirs stands.
    feed.toggle_reaction_pill(feed.post_index_by_id(target_id))
    wait_until(reads(1, "off"), REPAINT_BUDGET_S, diagnose=diagnose)

    # The repost row of the same post paints no pill.
    repost_row = feed.repost_row_state_by_target(target_id)
    assert repost_row is not None and repost_row.get("post_id"), (
        f"the other actor's repost of {mine!r} should be in the window; "
        f"error={app.error_text()!r}"
    )
    repost_at = feed.post_index_by_id(repost_row["post_id"])
    assert repost_at >= 0 and feed.reaction_pills(repost_at) == [], (
        f"a repost row paints no reaction pill; got "
        f"{feed.reaction_pills(repost_at) if repost_at >= 0 else 'no card'!r}"
    )

    # The post's detail paints the same pill, unscoped.
    feed.open_post_detail(feed.post_index_by_id(target_id))
    app.driver.wait_for("feed-post-detail-dialog")
    detail = wait_until(
        lambda: feed.reaction_pills(None) or None,
        REPAINT_BUDGET_S,
        diagnose=lambda: f"detail pills={feed.reaction_pills(None)!r} error={app.error_text()!r}",
    )
    assert len(detail) == 1 and _count(detail[0]) == 1 and detail[0]["state"] == "off", (
        f"post_detail should paint one {EMOJI} pill reading 1, not mine; got {detail!r}"
    )
    # No dismiss step: nothing after the detail check needs the list back, and the
    # per-test teardown closes the app. A trailing Esc would add a physical-input
    # dependency (windows SendInput can be denied by the box's foreground state)
    # that proves nothing about the pills.
