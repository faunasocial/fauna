"""tier_3: opening the new-thread composer (+) must never make the existing
conversations unreachable.

Regression guard for the user-reported bug "clicking + makes old conversations
inaccessible". Goal doc: ``docs/goal/ui/conversations.md`` § Layout (line 110 —
"``new-conversation-button`` swaps the **detail pane** for a ``recipient-picker``
component", i.e. the composer replaces the detail pane only, never the list pane)
and § Persistence (line 1049 — only an explicit cancel/discard or a successful send
clears the new-thread draft, so switching away from the composer must show the
picked surface without discarding anything).

Two distinct failure modes are asserted, because the report conflated them:

1. **The list is emptied / hidden while composing.** The thread list lives in its
   own pane (windows: grid column 0, a fixed 320px) and the composer only ever
   replaces the *detail* pane, so ``conversation-item`` must still be there.

2. **The previously-open thread can't be re-opened.** This is the one that actually
   bit, and it is windows-only: a WinUI ``ListView`` suppresses ``SelectionChanged``
   when the clicked row is *already* ``SelectedItem``. Opening thread A, then ``+``
   (the composer shows *over* still-selected A), then clicking A again fired no
   event — so the app never left new-thread mode and A's messages stayed hidden.
   Clicking a *different* thread worked, which is why the bug read as "old
   conversations are inaccessible" rather than "the list is broken". Fixed by clearing the list's visual selection while the composer is
   active, so the next click on *any* row — the previously-open one included — is a
   fresh selection that fires ``SelectionChanged``.

The re-click in step 3 is therefore the **discriminating** step, and it only
discriminates because it re-clicks *the same row that was open before ``+``*: the
FlaUI bridge drives a ``ListViewItem`` through ``SelectionItemPattern.Select()``,
which is a no-op (no ``SelectionChanged``) on an already-selected row — exactly the
production gesture. Do not "simplify" it to clicking a second, different thread; that
variant passed even with the bug present.

Windows owns the regression and is the only marker verified so far (red/green proven
against the fix: reverting the fix fails step 3 exactly, and only step 3 — the
list itself was never emptied, so failure mode 1 above is a guard, not the bug that
bit). The asserted behavior is universal, so the other apps extend this test by
adding their marker once run locally; linux/web list rows use a per-click handler
rather than a selection-change signal, so they were never exposed. The web leg is
unverified here because ``just web-test`` on win-arm64 currently can't build the
``fauna_wasm_labeler_catalog`` chunk (a known win-arm64 build gap);
add ``pytest.mark.web`` from a machine where that build is green.
"""

import time

import pytest

pytestmark = [pytest.mark.tier_3, pytest.mark.windows]

# This test's OWN peer — the thread is keyed by it, and threads accumulate across the
# session-scoped run, so sharing a peer with another test would merge their messages.
# The seeded thread is addressed by the id `seed_own_message` returns, never by needling
# this value out of a label.
_PEER = "bob-newthread@self-nest.test"


def _wait_visible(app, element_id: str, timeout: float = 8.0) -> bool:
    """Poll until ``element_id`` is visible. The selection round-trip is async
    (UniFFI observer -> DispatcherQueue.TryEnqueue -> Refresh), so a click returns
    before the pane swaps (condition-based waiting, e2e action-layer convention)."""
    deadline = time.time() + timeout
    while time.time() < deadline:
        try:
            if app.driver.is_visible(element_id):
                return True
        except Exception:
            pass
        time.sleep(0.1)
    return False


def _wait_not_visible(app, element_id: str, timeout: float = 8.0) -> bool:
    deadline = time.time() + timeout
    while time.time() < deadline:
        try:
            if not app.driver.is_visible(element_id):
                return True
        except Exception:
            pass
        time.sleep(0.1)
    return False


def _wait_count_at_least(app, element_id: str, minimum: int, timeout: float = 8.0) -> int:
    """Poll ``count(element_id)`` until it reaches ``minimum``. Returns the last count."""
    deadline = time.time() + timeout
    last = -1
    while time.time() < deadline:
        try:
            last = app.driver.count(element_id)
            if last >= minimum:
                return last
        except Exception:
            pass
        time.sleep(0.1)
    return last


def _seeded_row_index(threads: list, thread_id: str) -> int:
    """Index of the seeded row, BY IDENTITY. Needling the peer out of the label /
    snippet (what this used to do) picks whichever accumulated thread matches first —
    and the whole point of the test is to re-click *the row that was open*, so a
    wrong row silently invalidates it."""
    for i, t in enumerate(threads):
        if t.thread_id == thread_id:
            return i
    raise AssertionError(
        f"seeded thread {thread_id!r} not in the list; "
        f"rows={[(t.thread_id, t.label) for t in threads]!r}"
    )


@pytest.mark.feature("conversations")
def test_new_conversation_keeps_existing_threads_reachable(logged_in_app):
    app = logged_in_app
    body = "seeded message that must survive the composer 4412"

    # 1. Seed an existing conversation and leave it OPEN (and, on windows, the
    #    list's SelectedItem). Per-platform seeding lives in the action layer.
    app.conversations.navigate()
    thread_id = app.conversations.seed_own_message(body, recipient=_PEER)
    assert _wait_visible(app, "thread-header"), (
        "precondition: the seeded thread must be open before clicking +; "
        f"error={app.error_text()!r}"
    )
    threads = app.conversations.list_threads()
    row = _seeded_row_index(threads, thread_id)

    # 2. Open the new-thread composer. It replaces only the DETAIL pane — the
    #    thread list is a separate, always-visible pane.
    app.driver.click("new-conversation-button")
    assert _wait_visible(app, "new-conversation-cancel", timeout=10.0), (
        "the new-thread composer did not open; " f"error={app.error_text()!r}"
    )
    listed = _wait_count_at_least(app, "conversation-item", 1)
    assert listed >= 1, (
        "the conversation list was emptied/hidden while the new-thread composer "
        f"was open (count={listed}) — the list pane must stay populated; "
        f"{app.driver.diagnose('conversation-item')}"
    )

    # 3. DISCRIMINATING STEP — re-click the SAME row that was open before +.
    #    Pre-fc25e83a9 this row was still the ListView's SelectedItem, so the
    #    bridge's SelectionItemPattern.Select() raised no SelectionChanged, the
    #    page never left new-thread mode, and the thread's messages stayed hidden.
    app.driver.click("conversation-item", index=row)
    assert _wait_visible(app, "thread-header", timeout=10.0), (
        "re-clicking the conversation that was open before + did not re-open it — "
        "the new-thread composer stayed up and the thread is unreachable "
        f"(a regression); error={app.error_text()!r}"
    )
    assert _wait_not_visible(app, "new-conversation-cancel"), (
        "the new-thread composer is still shown after re-opening the thread; "
        f"error={app.error_text()!r}"
    )

    # 4. ...and its messages are rendered again (the symptom the user actually
    #    reported: "the old messages are not shown when I click a conversation").
    bubbles = _wait_count_at_least(app, "dm-message-text", 1)
    assert bubbles >= 1, (
        "the re-opened thread rendered zero message bubbles; "
        f"{app.driver.diagnose('dm-message-text')}; error={app.error_text()!r}"
    )
