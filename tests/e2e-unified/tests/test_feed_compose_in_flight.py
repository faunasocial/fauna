"""The composer while a post is sending, and when it fails to send —
`docs/goal/ui/feed.md` § User actions (`post-submit-button`) and § Errors &
edge cases.

Three promises, each a state the user stands in rather than an instant:

* **Text typed while a post is still sending survives the submit.** The
  composer is read once, at the press; on success only the fields still holding
  what was sent clear (shared `FeedComposeState::clear_sent`).
* **The audience picked for a post stays picked while you keep typing.** A
  tier, room or sale answer clears only when the composer is otherwise
  untouched, so the next post is never silently widened to Public (owner
  ruling on the same clear).
* **A post that fails to send says so in the composer and keeps what you
  wrote.** `FeedComposeState.error` renders into `compose-error`; nothing is
  cleared, so the same press sends it once the nest takes it.

The sending window is made a state, not a race (convention 14): the nest's
`rpc-hold` test hook parks `fauna.posts.create` until the test releases it, and
its `refuse` mode answers it with an error (`helpers/rpc_hold.py`,
`bins/fauna-nest/src/rpc_hold_test_hook.rs`). The submit is started with
`FeedActions.start_submit`, which on tui is the keyboard's Enter — the path a
user's keypress takes, which does not hold the app's other input hostage the
way the agent's awaited click does.

Standalone-nest only, like every `rpc-hold` journey: the hook is compiled into
the `test-hooks` e2e build and into no release artifact (convention 15).
"""
from __future__ import annotations

import uuid

import pytest

from helpers import budgets
from helpers.rpc_hold import (
    arm_rpc_hold,
    refuse_rpc,
    release_rpc_hold,
    wait_for_held_rpc,
    wait_for_refused_rpc,
)
from helpers.waiting import wait_until
from i18n.strings import S

pytestmark = [pytest.mark.tier_3]

#: The kind every submit arm ends in — the plain create, and the gated and sold
#: arms after their upload.
POSTS_CREATE = "fauna.posts.create"


def _unique(prefix: str) -> str:
    return f"{prefix}-{uuid.uuid4().hex[:8]}"


def _state(app) -> str:
    """The composer as the app holds it, for failure messages (convention 6)."""
    feed = app.feed
    try:
        body = feed.compose_body_text()
    except Exception as e:  # noqa: BLE001 — diagnostic only
        body = f"<{type(e).__name__}: {e}>"
    return (
        f"compose-text-field={body!r} compose-error={feed.compose_error_text()!r} "
        f"error-message={app.error_text()!r}"
    )


def _empty_composer(app) -> None:
    """Leave the composer empty and Public for the next test — its state is
    manager-owned and persisted as a draft, so what a test leaves in it is
    what the next test's first keystroke appends to."""
    feed = app.feed
    feed.open_composer()
    d = app.driver
    if S.feed.post.gate_public not in feed.compose_audience():
        if d.is_visible("compose-sell-price"):
            d.clear_and_type("compose-sell-price", "")
        if d.is_visible("compose-gate-preview-field"):
            d.clear_and_type("compose-gate-preview-field", "")
        d.select("compose-gate-tier-select", S.feed.post.gate_public)
    d.clear_and_type("compose-text-field", "")


@pytest.mark.feature("feed-compose")
def test_text_typed_while_a_post_sends_survives_the_submit(logged_in_app, nest_instance):
    """Press Post, and while the post is still sending start the next one: when
    the send lands, the first post is in the feed and the second one's text is
    still in the composer. A composer that cleared itself on success — rather
    than clearing only what it sent — leaves it empty."""
    app = logged_in_app
    feed = app.feed
    d = app.driver
    port = nest_instance["port"]
    sent = _unique("sent-first")
    typed_meanwhile = _unique("typed-while-sending")

    feed.open_composer()
    d.clear_and_type("compose-text-field", sent)
    arm_rpc_hold(port, POSTS_CREATE)
    try:
        feed.start_submit()
        wait_for_held_rpc(port, POSTS_CREATE, diagnose=lambda: _state(app))
        # The post is provably in flight: the nest holds its create.
        d.clear_and_type("compose-text-field", typed_meanwhile)
    finally:
        release_rpc_hold(port, POSTS_CREATE)
    try:
        # The success arm clears before it reloads the list, so the sent post
        # showing up means the clear has run.
        assert feed.wait_for_post_text(sent, timeout_s=budgets.RPC_ROUNDTRIP_S), (
            f"the held post never landed after release; {_state(app)}"
        )
        assert feed.compose_body_text() == typed_meanwhile, (
            "text typed while the post was sending must survive its submit — "
            f"only what was sent clears; {_state(app)}"
        )
    finally:
        _empty_composer(app)


@pytest.mark.feature("feed-compose")
def test_the_audience_picked_for_a_post_stays_picked_while_you_keep_typing(
    logged_in_app, nest_instance,
):
    """Sell a post, and while it is still sending start typing the next one:
    when the sale lands, the composer's audience is still the sale — never
    silently back to Public. The sold arm reaches `fauna.posts.create` after
    its upload, so the hold parks the whole submit behind the edit."""
    app = logged_in_app
    feed = app.feed
    d = app.driver
    port = nest_instance["port"]
    preview = _unique("teaser")
    full_body = _unique("sold-body")
    typed_meanwhile = _unique("next-post")

    feed.open_composer()
    d.select("compose-gate-tier-select", S.feed.post.gate_sell)
    d.wait_for("compose-sell-price")
    d.clear_and_type("compose-sell-price", "$3")
    d.clear_and_type("compose-gate-preview-field", preview)
    d.clear_and_type("compose-text-field", full_body)
    picked = feed.compose_audience()
    assert S.feed.post.gate_sell in picked, (
        f"the sale should read as picked before the send; audience={picked!r} {_state(app)}"
    )

    arm_rpc_hold(port, POSTS_CREATE)
    try:
        feed.start_submit()
        wait_for_held_rpc(port, POSTS_CREATE, diagnose=lambda: _state(app))
        d.clear_and_type("compose-text-field", typed_meanwhile)
    finally:
        release_rpc_hold(port, POSTS_CREATE)
    try:
        assert feed.wait_for_post_text(preview, timeout_s=budgets.RPC_ROUNDTRIP_S), (
            f"the held sale never landed after release; {_state(app)}"
        )
        assert feed.compose_body_text() == typed_meanwhile, (
            f"the next post's text must survive the sale's submit; {_state(app)}"
        )
        audience = feed.compose_audience()
        assert audience == picked, (
            "the audience picked for the post in flight must stay picked while "
            f"the next post is typed — got {audience!r}, picked {picked!r}; "
            f"{_state(app)}"
        )
    finally:
        _empty_composer(app)


@pytest.mark.feature("feed-compose")
def test_a_post_that_fails_to_send_says_so_and_keeps_what_you_wrote(
    logged_in_app, nest_instance,
):
    """The nest turns the create away: the composer shows its error and still
    holds the text. Once the nest takes posts again, the same press sends it and
    the composer clears."""
    app = logged_in_app
    feed = app.feed
    d = app.driver
    port = nest_instance["port"]
    text = _unique("will-fail-then-send")

    feed.open_composer()
    d.clear_and_type("compose-text-field", text)
    refuse_rpc(port, POSTS_CREATE)
    try:
        feed.start_submit()
        wait_for_refused_rpc(port, POSTS_CREATE)
        error = wait_until(
            feed.compose_error_text,
            budgets.UI_SETTLE_S,
            diagnose=lambda: f"no compose-error after a refused send; {_state(app)}",
        )
        assert feed.compose_body_text() == text, (
            f"a failed send must keep what was written; error={error!r} {_state(app)}"
        )
    finally:
        release_rpc_hold(port, POSTS_CREATE)
    try:
        # "…so you can try again": the kept text sends on the next press.
        feed.start_submit()
        assert feed.wait_for_post_text(text, timeout_s=budgets.RPC_ROUNDTRIP_S), (
            f"the retried post never landed; {_state(app)}"
        )
        assert wait_until(
            lambda: feed.compose_body_text() == "",
            budgets.UI_SETTLE_S,
            diagnose=lambda: f"the sent post's text did not clear; {_state(app)}",
        )
        assert feed.compose_error_text() == "", (
            f"a successful retry must clear the old error; {_state(app)}"
        )
    finally:
        _empty_composer(app)
