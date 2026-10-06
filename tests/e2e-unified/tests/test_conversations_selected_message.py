"""tier_3: apple (macOS + iOS) real-simulator proof of the selected-message
paint + scroll (`docs/goal/ui/conversations.md` § The selected message).

Row 114 landed the shared FaunaKit paint
(`DmMessageBubble`'s `selected` attribute + amber stroke, `ThreadDetailView`'s
`ScrollViewReader` scroll-into-view) on both apple targets, e2e-proven on macOS
through `test_search_local_index.py`'s real mail-arrives -> local-index-build ->
Search -> select journey. iOS could not get the same proof the same way: its
local content index is **query-only** — `CLIENT_BUILDS_INDEX` is false there
(`libs/fauna-ffi/src/index_launch.rs`), because a phone's background-execution
budget makes it an unreliable *builder*, only a *querier* of segments another of
the user's devices built and synced. A single-seat test never has a second
device to sync from, so it can never build a segment to search, so
`SearchNav::Mail` can never surface on iOS in this harness — regardless of
whether the paint is correct (`test_search_local_index.py`'s own note, and
`ui/search.md` § Implementation status today).

So this test isolates and proves the piece that a real Search-result tap and
this seam both ultimately drive — `ConversationsManager::select_thread_and_message`
painting the right bubble and scrolling it into view — through the
`conversations_select_message` test-only command
(`FaunaApp.swift`/`FaunaMacApp.swift` `handleConversationsSelectMessage`, both
calling the same `ConversationsVM.selectThreadAndMessage` a real
`SearchResultsView` row tap calls). Run under `--app ios` this drives a REAL
booted iOS Simulator through the in-process automation server, not a
compile-only check — closing the residual
without needing a two-device sync run to exercise the real Search path.

macOS is included too: it is the app that already has the full real-Search
proof, so running the same isolated assertion there is a free cross-check that
the new seam calls the identical production path (no new code diverges between
the two apple targets — priority #1).

linux joined 2026-09-19 through the same seam (its test agent calls the same
`ConversationsManager::select_thread_and_message` a Search `Mail` row does,
`views/search.rs`). Two things changed with it, both app-neutral: the target
sits in the MIDDLE of the thread, because linux opens a thread at its newest
message where apple opens at its oldest; and the scroll proof is an explicit
`message_in_view` read (`driver.in_viewport`), because GTK realizes every row
of the thread and so its registry cannot say "on screen" by omission the way
apple's does.

tui joined 2026-09-19 on the same terms as linux. Its test agent dispatches the
very `SearchNav::Mail` gesture a Search result row dispatches
(`crate::search::open_result`), whose scroll is a focus move — tui's viewport
follows its focus ring. tui's registry, like GTK's, lists every row, so the
in-view read is the agent's `in-viewport` attribute, measured on the frame the
terminal actually painted.
"""

import uuid

import pytest

from helpers.waiting import wait_until

pytestmark = [
    pytest.mark.tier_3,
    pytest.mark.macos,
    pytest.mark.ios,
    pytest.mark.linux,
    pytest.mark.tui,
    pytest.mark.windows,
]

# Enough history on EITHER side of the target that it is out of view when the
# thread first opens, whichever end it opens at: apple's `ThreadDetailView`
# opens a fresh ScrollView at its top (the oldest message) and never scrolls to
# the newest on open, while linux scrolls to the newest on open (`detail.rs`).
# A target at either end would be in view from the start on one of them, and
# "is it in view after selecting" would then prove nothing — the precondition
# below fails loudly if this ever stops holding.
_FILLERS_EACH_SIDE = 30


@pytest.mark.feature("private-search-index")
def test_select_thread_and_message_marks_and_scrolls_the_named_message(logged_in_app):
    app = logged_in_app
    conv = app.conversations
    # The conversation-list observer only starts publishing
    # `data.conversation_threads` once the page has appeared at least once
    # (`_ensure_on_conversations_page`'s own note) — navigate up front rather
    # than depend on some earlier test in the run having warmed it.
    conv.navigate()

    # Unique per run: nest_instance/test_user are session-scoped and threads
    # accumulate across the whole pytest run, so a static sender risks
    # colliding with another test's thread (conversations.md's own
    # inject_and_resolve_thread docstring covers why identity, not a needle,
    # is the only safe resolver here).
    sender = f"search-select-{uuid.uuid4().hex[:10]}@self-nest.test"
    target_body = "the target message select_thread_and_message must find"

    before = {t.thread_id for t in conv.list_threads()}
    conv.inject_inbound_for_test(rail="FaunaMls", sender=sender, body="filler message 0")
    thread_id = wait_until(
        lambda: next((t.thread_id for t in conv.list_threads() if t.thread_id not in before), None),
        60.0,
        diagnose=lambda: f"the first filler message never landed in a new thread; conversations error: {app.error_text()!r}",
    )
    for i in range(1, _FILLERS_EACH_SIDE):
        conv.inject_inbound_for_test(rail="FaunaMls", sender=sender, body=f"filler message {i}")
    # The middle of the thread — never opened yet, so nothing has rendered or
    # realized any of this thread's rows.
    target_message_id = conv.inject_inbound_for_test(rail="FaunaMls", sender=sender, body=target_body)
    for i in range(_FILLERS_EACH_SIDE, 2 * _FILLERS_EACH_SIDE):
        conv.inject_inbound_for_test(rail="FaunaMls", sender=sender, body=f"filler message {i}")

    conv.open_thread_by_id(thread_id)
    assert wait_until(
        lambda: conv.driver.count("dm-message-text") >= 1,
        10.0,
        diagnose=lambda: f"the thread opened but rendered no messages; {conv.driver.diagnose('dm-message-text')}",
    )

    assert conv.selected_message_index() is None, (
        "no message should be marked selected before select_thread_and_message "
        "is ever called"
    )
    # The precondition that makes the scroll assertion below mean something.
    assert not conv.message_in_view(target_body), (
        "the target was already in view when the thread opened, so the "
        "post-select in-view check cannot prove the scroll did anything; add "
        f"more filler messages (currently {_FILLERS_EACH_SIDE} each side)"
    )

    conv.select_message(thread_id, target_message_id)

    def _selected_once_marked():
        """The selected index wrapped in a 1-tuple, or None while nothing is
        marked yet.

        `wait_until` polls on truthiness and RETURNS the value that released
        it. A bare index cannot use that: index 0 is falsy, so the poll would
        keep spinning on a correctly-selected first bubble. A 1-tuple is truthy
        for 0 too — which lets the test KEEP the value instead of re-reading
        the index once settled, and that re-read was a whole extra scan of the
        thread. On a 61-bubble thread a scan is a full pass over the list, and
        this test ran three (pre-select, this poll, the re-read).
        """
        i = conv.selected_message_index()
        return None if i is None else (i,)

    (selected,) = wait_until(
        _selected_once_marked,
        10.0,
        diagnose=lambda: (
            "conversations_select_message was sent but no dm-message-timestamp "
            f"ever reads selected=true; conversations error: {app.error_text()!r}"
        ),
    )

    # The load-bearing assertion is not "something is marked" but "the marked
    # message is the one we named" — mirrors test_search_local_index.py's own
    # check, since a marker stuck on the wrong bubble would otherwise pass.
    marked_text = conv.driver.get_text("dm-message-text", index=selected)
    assert target_body in marked_text, (
        f"the marked message must be the one select_thread_and_message named: "
        f"expected the target bubble, got bubble {selected} reading {marked_text!r}"
    )

    # The scroll half: the target was out of view before the select (the
    # precondition above) and must be in view now. On apple a marked bubble
    # being listed at all already says so (its registry lists only realized,
    # on-screen slots); linux answers through the agent's viewport geometry.
    wait_until(
        lambda: conv.message_in_view(target_body),
        10.0,
        diagnose=lambda: (
            "the target is marked but was never brought into view; "
            f"conversations error: {app.error_text()!r}"
        ),
    )
