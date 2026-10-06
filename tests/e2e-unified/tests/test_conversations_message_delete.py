"""tier_3 e2e: conversation message delete (conversations.md § Reactions & message delete).

Phase C windows leg — verifies the WinUI render (C2):
the per-bubble ⋯ ``dm-message-actions-button`` → ``dm-message-delete-button``
(MenuFlyoutItem) → ``dm-message-delete-confirm-button`` (confirm sub-Flyout) path,
and the resulting ``dm-message-deleted`` tombstone placeholder.

Two assertions:

1. **delete own message**: create a FaunaMls group, SEND a message via the compose
   bar (which sets ``is_own=True`` in the manager state), then open the ⋯ flyout
   on that bubble and assert ``dm-message-delete-button`` is present and invokable.
   Clicking confirm → assert ``dm-message-deleted`` tombstone placeholder appears and
   the body (``dm-message-text``) is gone.  The optimistic delete (``self.deleted.insert``
   + ``self.notify()``) fires synchronously in the manager so the tombstone appears
   without a nest round-trip — this path is testable with the mock backend.

2. **peer message — delete absent**: on a PEER bubble (``is_own=False``), the
   ``dm-message-actions-button`` is present (supports_reactions=True) but
   ``dm-message-delete-button`` must be absent (count = 0) after opening the ⋯ flyout.

Each non-windows app adds its marker when it lifts the bubble's delete render
off its own ``DmMessageBubble`` equivalent.

⚠ FlaUI ``MenuFlyoutItem`` realization: if ``dm-message-delete-button`` count = 0
after the flyout opens on an own message, the action helper raises with BLOCKED
context — do NOT weaken the assertion.
"""

import time

import pytest

pytestmark = [pytest.mark.tier_3]


@pytest.mark.windows
@pytest.mark.linux
@pytest.mark.tui
@pytest.mark.web
@pytest.mark.macos
@pytest.mark.ios
@pytest.mark.android
@pytest.mark.feature("reactions-and-message-delete")
def test_delete_own_message_shows_tombstone(logged_in_app):
    """Send a message (is_own=True), open ⋯, click delete, confirm, assert tombstone.

    Uses ``create_mls_group`` + UI send so the message has ``is_own=True`` in the
    manager state.  The optimistic delete in ``ConversationsManager.delete_message``
    fires synchronously (``self.deleted.insert`` + ``self.notify()``), so the
    tombstone appears without a nest round-trip — testable with the mock backend.
    """
    conv = logged_in_app.conversations
    d = logged_in_app.driver

    # Seed an own FaunaMls bubble (is_own=True) in an open thread, uniformly
    # across every app (seed_own_message's per-app branch collapsed).
    # Own peer, own thread: the default peer is shared with other files that assert
    # bubble counts in this session-scoped run.
    conv.seed_own_message("message to delete", recipient="bob-delete@self-nest.test")
    assert d.count("dm-message-text") >= 1, "own bubble did not appear after seeding"

    body_count_before = d.count("dm-message-text")

    # The ⋯ button must be present (supports_reactions=True for FaunaMls, and
    # supports_message_delete=True && is_own=True → delete option also shown).
    deadline = time.time() + 5.0
    while time.time() < deadline:
        if d.count("dm-message-actions-button") >= 1:
            break
        time.sleep(0.1)
    assert d.count("dm-message-actions-button") >= 1, (
        "dm-message-actions-button must appear on the sent bubble "
        "(is_own=True, FaunaMls thread)"
    )

    # Find the sent message's bubble index — it's the last bubble in the list.
    # We need to click the ⋯ on the SENT message (not a peer message).
    # The sent bubble is is_own=True and index = count-1.
    bubble_idx = d.count("dm-message-actions-button") - 1

    # Drive the two-hop delete: ⋯ → dm-message-delete-button → confirm.
    conv.delete_message(message_index=bubble_idx, timeout_s=12.0)

    # Poll for the tombstone placeholder to appear (optimistic, synchronous).
    conv.wait_for_message_deleted(timeout_s=8.0)

    assert d.count("dm-message-deleted") >= 1, (
        "dm-message-deleted placeholder must appear after a successful delete"
    )
    # The body text for the deleted message should be gone (bubble collapses it).
    assert d.count("dm-message-text") < body_count_before, (
        "dm-message-text should be Collapsed / gone after delete; "
        f"count={d.count('dm-message-text')} (was {body_count_before})"
    )


@pytest.mark.windows
@pytest.mark.linux
@pytest.mark.tui
@pytest.mark.web
@pytest.mark.macos
@pytest.mark.ios
@pytest.mark.android
@pytest.mark.feature("reactions-and-message-delete")
def test_peer_message_delete_button_absent(logged_in_app):
    """On a PEER bubble (is_own=False), ``dm-message-delete-button`` must be absent
    (Collapsed → no UIA peer → count = 0) after opening the ⋯ flyout.

    Uses inject (which sets is_own=False) for a FaunaMls thread where supports_reactions
    is True (so the ⋯ button IS present, but delete must not be).
    """
    conv = logged_in_app.conversations
    d = logged_in_app.driver

    # Inject a peer FaunaMls message (is_own=False) and OPEN the thread it keys to.
    # inject_inbound keys a thread by SENDER, not the currently-open thread, so we
    # open the injected thread itself rather than creating a separate group first
    # (which would leave the injected message in a different, unopened thread).
    conv.inject_and_open_thread(
        rail="FaunaMls",
        sender="alice-delete@self-nest.test",
        body="peer message for delete-absent test",
    )
    deadline = time.time() + 5.0
    while time.time() < deadline:
        if d.count("dm-message-text") >= 1:
            break
        time.sleep(0.1)
    assert d.count("dm-message-text") >= 1, "no bubble after peer inject"

    # The ⋯ button should be present (supports_reactions=True for FaunaMls).
    deadline = time.time() + 5.0
    while time.time() < deadline:
        if d.count("dm-message-actions-button") >= 1:
            break
        time.sleep(0.1)
    assert d.count("dm-message-actions-button") >= 1, (
        "dm-message-actions-button must appear for FaunaMls peer message "
        "(supports_reactions=True); got count=0"
    )

    # Open the ⋯ flyout on the peer message bubble.
    conv.open_message_actions(message_index=0)

    # Poll briefly for the flyout items to appear.
    deadline = time.time() + 4.0
    while time.time() < deadline:
        try:
            if d.count("dm-reaction-option") > 0:
                break  # flyout is open and showing reaction items
        except Exception:
            pass
        time.sleep(0.1)

    # The delete button must be absent for a peer message (is_own=False).
    delete_button_count = d.count("dm-message-delete-button")
    assert delete_button_count == 0, (
        f"dm-message-delete-button must be absent (Collapsed) on a peer message "
        f"(is_own=False); got count={delete_button_count}. The delete capability "
        "gate (SupportsMessageDelete && msg.IsOwn) is not correctly applied."
    )
