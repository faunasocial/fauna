"""tier_3: the new-thread composer's Cancel affordance discards the half-written
draft and dismisses the composer.

Since the shared-Rust new-thread draft now PERSISTS across switching
(``select_thread`` keeps it, ``start_new_conversation`` restores it), an explicit **cancel/discard** is — besides a
successful send — the only thing that clears it (``docs/goal/ui/conversations.md``
§ Persistence: "Only an explicit cancel/discard (``cancel_new_conversation``,
surfaced as the composer's Cancel affordance) or a successful send clears the
new-thread draft").

This test is the **discriminating** check for that affordance: typing forwards the
body to the shared manager (the bridge's ValuePattern SetValue raises the
composer's BodyChanged, which is NOT suppressed for non-seed input), so the manager
holds a non-empty draft. Cancel must DISCARD it: re-opening the composer then shows
a FRESH EMPTY field. Had Cancel merely hidden the view, ``start_new_conversation``
would RESTORE the body on re-open (the preserve-on-switch behavior) and step 3 would
catch it.

Windows owns the canonical ``new-conversation-cancel`` ui.yaml element + shipped the
first impl. **Web and Linux legs both landed (2026-06-21):** web — its new-thread
composer gained the Cancel button (``compose-actions`` row, gated ``mode==='new'``)
wired to ``manager.cancelNewConversation()``; linux — a header row in the new_thread
stack page (a ``conversations/compose/new_message`` title + a ``common/cancel``
Button) whose click calls ``cancel_new_conversation``; the snapshot observer then
collapses the composer (stack switches off ``new_thread`` once ``new_thread_compose``
is ``None``). The remaining native legs extend this test (add their marker) as they
land, per the goal-doc fast-follow note. ``--client`` deselects the unwired ones.
"""

import time

import pytest

pytestmark = [pytest.mark.tier_3, pytest.mark.windows, pytest.mark.linux, pytest.mark.web, pytest.mark.macos, pytest.mark.ios, pytest.mark.tui]


def _wait_compose_body(app, expected: str, timeout: float = 8.0) -> str:
    """Poll the composer body until it equals ``expected`` (or time out). The
    windows ``dm-text-field`` is a ``MarkdownRichEditBox`` whose ValuePattern read
    can transiently time out under post-edit UI-thread contention (the live
    BodyChanged -> manager -> snapshot Refresh churn), so a single read can raise;
    re-read rather than fail (condition-based waiting, e2e action-layer
    convention). Returns the last value read (or a read-error marker)."""
    deadline = time.time() + timeout
    last = "<unread>"
    while time.time() < deadline:
        try:
            last = app.conversations.compose_body_text()
            if last == expected:
                return last
        except Exception as e:  # transient bridge/UIA read timeout — retry
            last = f"<read error: {e!r}>"
        time.sleep(0.25)
    return last


def _read_compose_body(app, timeout: float = 5.0) -> str:
    """Robust single read of the composer body, retrying past transient UIA read
    timeouts. An EMPTY windows composer reads back as "": its peer reports
    ControlType Edit, and the bridge's GetText answers "" for an empty Edit
    control rather than scraping the localized placeholder ("Type a message...")
    as it did until 2026-09-21."""
    deadline = time.time() + timeout
    last = "<unread>"
    while time.time() < deadline:
        try:
            return app.conversations.compose_body_text()
        except Exception as e:  # transient bridge/UIA read timeout — retry
            last = f"<read error: {e!r}>"
            time.sleep(0.25)
    return last


def _wait_not_visible(app, element_id: str, timeout: float = 6.0) -> bool:
    """Poll until ``element_id`` is no longer visible (the composer collapses on a
    deferred UI-thread Refresh after the cancel mutator; the click return races
    it)."""
    deadline = time.time() + timeout
    while time.time() < deadline:
        try:
            if not app.driver.is_visible(element_id):
                return True
        except Exception:
            pass
        time.sleep(0.1)
    return False


@pytest.mark.feature("drafts-survive")
def test_new_thread_cancel_discards_draft_and_dismisses(logged_in_app):
    app = logged_in_app
    draft_body = "half-written draft to discard 7731"

    # 1. Open the new-thread composer and type a draft body. Each edit live-
    #    forwards to the shared manager (BodyChanged -> SetNewThreadBody), so the
    #    manager's new_thread_compose now holds a non-empty draft.
    app.conversations.navigate()
    app.driver.click("new-conversation-button")
    app.driver.wait_for("dm-text-field", timeout=10.0)
    app.driver.type_text("dm-text-field", draft_body)
    assert _wait_compose_body(app, draft_body) == draft_body, (
        "precondition: the draft body must be in the composer before Cancel; "
        f"error={app.error_text()!r}"
    )

    # 2. Click Cancel -> cancel_new_conversation() clears the new-thread draft.
    #    NewThreadView's visibility is a pure function of the manager's
    #    new_thread_compose (null -> collapsed), so the cleared draft collapses the
    #    composer and dm-text-field leaves the UIA tree (WinUI Collapsed prunes the
    #    visual/UIA subtree). There is no hide-without-discard path.
    app.conversations.cancel_new_conversation()
    assert _wait_not_visible(app, "dm-text-field"), (
        "the new-thread composer was not dismissed after Cancel; "
        f"error={app.error_text()!r}"
    )

    # 3. Re-open the composer. start_new_conversation seeds a FRESH empty composer
    #    because Cancel discarded the stashed draft, so the draft body must be GONE.
    #    Settle first so a (hypothetical) restore would have landed before we read —
    #    making the assertion sound (a mere-hide bug would show draft_body here).
    app.driver.click("new-conversation-button")
    app.driver.wait_for("dm-text-field", timeout=10.0)
    time.sleep(1.0)
    body = _read_compose_body(app)
    assert draft_body not in body, (
        "the new-thread draft was not discarded by Cancel — it reappeared on "
        f"re-open ({body!r} still contains the draft); error={app.error_text()!r}"
    )
