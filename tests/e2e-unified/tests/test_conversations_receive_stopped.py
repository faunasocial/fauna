"""tier_3: when new messages stop arriving because the app's receive loop died,
the Conversations page says so on ``error-message`` until a working receive loop
replaces it — conversations outcome 18 (``docs/goal/ui/conversations.md`` §
Errors & edge cases, *A fourth truth … the receive rail stopped*).

The shared mechanism is ``ConversationsManager::receive_stopped`` — a loop
generation counter the supervisor marks on a panic and a newer
``begin_receive_loop`` supersedes — and every app's page ranks the resolved
``conversations.errors.receive_stopped`` notice directly under the
served-elsewhere refusal. The unit pins sit on the manager and on each app's
page read; this is the end-to-end witness that the running app paints it.

Why seams and not a real failure: no product path kills a receive loop on
demand, so ``conversations_mark_receive_stopped`` drives the shared
``mark_receive_stopped_for_test`` (claim a generation, mark it stopped — what a
supervised loop's first panic leaves). And on tui a new receive loop only
starts with a new session, which builds a fresh manager, so the retirement half
— a newer loop over the SAME manager — goes through
``conversations_begin_receive_loop``, the one call a starting loop makes. Both
commands are compiled out of release artifacts (convention 15).

tui leads; the other apps' agents add the two command names and widen the marks
in trickle-down rows.
"""

import time

import pytest

from i18n.strings import S

pytestmark = pytest.mark.tier_3

# Generous, named and latency-independent (convention 14): the assertion is on
# state — the label eventually carries the notice — never on how fast a loaded
# box got there. The ceiling only bounds a genuinely broken surface.
ERROR_SURFACE_BUDGET_S = 60.0


def _flat(text: str) -> str:
    """Collapse whitespace, so a surface that wraps the sentence still compares."""
    return " ".join(text.split())


def _wait_visible(driver, want: bool) -> bool:
    deadline = time.time() + ERROR_SURFACE_BUDGET_S
    while time.time() < deadline:
        if driver.is_visible("error-message") == want:
            return True
        time.sleep(0.1)
    return driver.is_visible("error-message") == want


@pytest.mark.linux
@pytest.mark.web
@pytest.mark.tui
@pytest.mark.android
@pytest.mark.windows
@pytest.mark.macos
@pytest.mark.ios
@pytest.mark.feature("conversations")
# The seam marks a death only once a real receive loop has claimed its
# generation. linux, web and tui run the real session at every e2e login; the
# native apps launch it only behind the real_conversations gate (conftest's
# `_apply_real_conversations_env`), so without the marker windows' seam answers
# "pending" until the driver gives up ("the app's receive loop never began").
@pytest.mark.real_conversations
def test_a_stopped_receive_loop_says_so_until_a_newer_loop_replaces_it(logged_in_app):
    app = logged_in_app

    app.conversations.navigate()
    assert not app.driver.is_visible("error-message"), (
        "the conversations page must start with no error surfaced; "
        f"error_text={app.error_text()!r}"
    )

    app.conversations.mark_receive_stopped_for_test()
    try:
        assert _wait_visible(app.driver, True), (
            "a receive loop that stopped must surface on the page error-message "
            "— otherwise new messages silently stop arriving. "
            f"error_text={app.error_text()!r}"
        )
        shown = app.driver.get_text("error-message")
        assert _flat(shown) == _flat(S.conversations.errors.receive_stopped), (
            "error-message must carry the resolved receive-stopped notice, "
            f"word for word (got {shown!r})"
        )
    finally:
        # The retirement half, and the cleanup a shared app needs either way:
        # a newer loop over the same manager supersedes the dead one.
        app.conversations.begin_receive_loop_for_test()

    assert _wait_visible(app.driver, False), (
        "a newer working receive loop must retire the notice; it stayed: "
        f"{app.driver.get_text('error-message')!r}"
    )
