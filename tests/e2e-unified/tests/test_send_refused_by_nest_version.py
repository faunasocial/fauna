"""tier_3 E2E: a send the nest turns away — because it needs updating, or with a
refusal the app is too old to recognise — is shown as that refusal, offers no
retry, and is not retried behind the user's back.

`docs/goal/architecture/version-compatibility.md` § Dimension 4 — Honest
version / compatibility error surfacing: a version mismatch maps to a typed
code the app renders localized and routes to an update prompt, never a retry;
any code the app does not know classifies `Rejected` (show it, do not retry —
`RpcError::action`'s deliberate default, `libs/fauna-protocol/src/error.rs`).
The launch-path half (a degraded nest refusing the connect handshake) is
`test_version_mismatch_launch.py`; this file is the SEND half, which a degraded
nest cannot provoke — it refuses the handshake too, so a signed-in app never
reaches a send against it.

The refusal is made a state, not a race (convention 14): the nest's `rpc-hold`
test hook answers `fauna.posts.create` with an error instead of running its
handler — `as_="outdated"` with the nest's own `fauna.nest.outdated`, the
default with `fauna.test.refused`, a code no app recognises
(`helpers/rpc_hold.py`, `bins/fauna-nest/src/rpc_hold_test_hook.rs`). The send
is the feed composer's Post, the one send every app shares a driver for.

"Not retried" is read from the nest's own count of requests it turned away:
exactly one after the app has shown the refusal and taken more input. A retry
on a timer could in principle land later than any barrier a test can set; the
classification that rules one out is pinned at tier_1 in `fauna-protocol`
(`nest_outdated_is_actionable_not_transient`), and what this journey proves is
that the app's surface comes to rest on the refusal rather than on a sending
or retrying state.

Standalone-nest only, like every `rpc-hold` journey: the hook is compiled into
the `test-hooks` e2e build and into no release artifact (convention 15).
"""
from __future__ import annotations

import uuid

import pytest

from helpers import budgets
from helpers.rpc_hold import (
    refuse_rpc,
    release_rpc_hold,
    rpc_hold_status,
    wait_for_refused_rpc,
)
from helpers.waiting import wait_until
from i18n.strings import S

pytestmark = [pytest.mark.tier_3]

POSTS_CREATE = "fauna.posts.create"

#: The retry affordances ui.yaml names anywhere. None belongs on a page showing
#: a refusal a retry cannot fix; the launch one is the update surface's
#: defining absence (`onboarding.md` § App-launch routing).
RETRY_IDS = ("launch-retry-button", "provisioning-retry-button")


def _unique(prefix: str) -> str:
    return f"{prefix}-{uuid.uuid4().hex[:8]}"


def _state(app, port: int) -> str:
    """The composer and the nest's count, for failure messages (convention 6)."""
    feed = app.feed
    try:
        body = feed.compose_body_text()
    except Exception as e:  # noqa: BLE001 — diagnostic only
        body = f"<{type(e).__name__}: {e}>"
    try:
        hold = rpc_hold_status(port, POSTS_CREATE)
    except Exception as e:  # noqa: BLE001 — diagnostic only
        hold = f"<{type(e).__name__}: {e}>"
    return (
        f"compose-text-field={body!r} compose-error={feed.compose_error_text()!r} "
        f"error-message={app.error_text()!r} rpc-hold={hold!r}"
    )


def _send_refused(app, port: int, *, as_: str | None) -> tuple[str, str, int]:
    """Type a post, have the nest refuse its create, press Post, and return the
    composer's error, the text that was typed and the nest's refusal count once
    the refusal shows. The count is cumulative over the session nest's life, so
    it is read against the count before the press."""
    feed = app.feed
    d = app.driver
    text = _unique("refused-send")
    feed.open_composer()
    d.clear_and_type("compose-text-field", text)
    before = rpc_hold_status(port, POSTS_CREATE).get("refused", 0)
    refuse_rpc(port, POSTS_CREATE, as_=as_)
    feed.start_submit()
    wait_for_refused_rpc(port, POSTS_CREATE, count=before + 1)
    error = wait_until(
        feed.compose_error_text,
        budgets.UI_SETTLE_S,
        diagnose=lambda: f"no compose-error after a refused send; {_state(app, port)}",
    )
    return error, text, before + 1


def _assert_at_rest_on_the_refusal(app, port: int, text: str, refused_after: int) -> None:
    """The refused send stays refused: the text is kept, no retry affordance is
    on screen, and after the app has taken further input the nest has still
    turned away exactly the one create the user pressed."""
    feed = app.feed
    d = app.driver
    assert feed.compose_body_text() == text, (
        f"a refused send must keep what was written; {_state(app, port)}"
    )
    for retry in RETRY_IDS:
        assert d.is_absent(retry), (
            f"{retry} must not be offered for a refusal a retry cannot fix; "
            f"{_state(app, port)}"
        )
    # A further keystroke through the agent: the app has run its loop past the
    # refusal and is accepting input, not sitting in a send it will repeat.
    d.clear_and_type("compose-text-field", text + " ")
    refused = rpc_hold_status(port, POSTS_CREATE).get("refused")
    assert refused == refused_after, (
        f"the app sent the refused create again by itself (refused={refused}, "
        f"want {refused_after}); "
        f"{_state(app, port)}"
    )


def _empty_composer(app) -> None:
    app.feed.open_composer()
    app.driver.clear_and_type("compose-text-field", "")


@pytest.mark.feature("upgrades-never-lose-data")
def test_a_send_refused_because_the_nest_needs_updating_says_so_and_offers_no_retry(
    logged_in_app, nest_instance,
):
    """The nest answers the post with `fauna.nest.outdated`: the composer shows
    the localized "this nest must be updated" message, never the generic
    try-again string, and nothing offers or performs a retry."""
    app = logged_in_app
    port = nest_instance["port"]
    try:
        error, text, refused_after = _send_refused(app, port, as_="outdated")
        assert S.error.nest.outdated in error, (
            f"the refusal must name the nest needing an update; got {error!r}; "
            f"{_state(app, port)}"
        )
        assert S.error.unexpected not in error, (
            f"an update refusal must not read as a try-again failure; got {error!r}"
        )
        _assert_at_rest_on_the_refusal(app, port, text, refused_after)
    finally:
        release_rpc_hold(port, POSTS_CREATE)
        _empty_composer(app)


@pytest.mark.feature("upgrades-never-lose-data")
def test_a_refusal_the_app_does_not_recognise_is_shown_as_a_refusal_and_not_retried(
    logged_in_app, nest_instance,
):
    """The nest answers the post with a code this app has never heard of (a
    newer nest's refusal): the composer shows it as a failed send — the generic
    refusal string, since there is no specific copy to give — and the app does
    not keep trying."""
    app = logged_in_app
    port = nest_instance["port"]
    try:
        error, text, refused_after = _send_refused(app, port, as_=None)
        assert S.error.unexpected in error, (
            f"an unrecognised refusal must render as the generic refusal; got {error!r}; "
            f"{_state(app, port)}"
        )
        _assert_at_rest_on_the_refusal(app, port, text, refused_after)
    finally:
        release_rpc_hold(port, POSTS_CREATE)
        _empty_composer(app)
