"""The compose bar's reply preview and the mail header's informational chips.

Goal doc: ``docs/goal/ui/conversations.md`` § Layout & flow (``dm-compose-bar``: "optional
reply preview (``dm-reply-preview`` + ``dm-reply-cancel``)") and § Participants vs. reply
recipients ("Header chips are non-removable; clicking a chip reveals the full address /
contact (never removes)"; the add-participant button is hidden on mail).

tui first (``docs/goal/architecture/testing.md`` § Default app and nest mode); web and
linux joined 2026-09-24 through the cross-app lift row, macos, ios and windows the same
day. The reply preview is the shared ``ConversationsManager::reply_preview`` on all six. An app
whose chip reveals the address on choosing (apple's popover, rather than showing it in
place as tui's, linux's, web's and windows' do) publishes the revealed address as the chip's
``revealed`` attribute, which the chip witness reads.

Seeded through the ``inject_inbound_for_test`` seam; every thread is keyed by a per-run
token so the session-scoped thread list cannot hand back somebody else's thread.
"""

import secrets

import pytest

from actions.conversations import SEAM_SELF_ADDRESS
from helpers.budgets import UI_SETTLE_S
from helpers.inert_refusal import is_inert_refusal
from helpers.waiting import wait_until

pytestmark = [
    pytest.mark.tier_3,
    pytest.mark.tui,
    pytest.mark.web,
    pytest.mark.linux,
    pytest.mark.macos,
    pytest.mark.ios,
    pytest.mark.windows,
    pytest.mark.android,
]


@pytest.mark.feature("replies-and-threads")
def test_reply_preview_shows_the_message_answered_and_cancel_clears_it(logged_in_app):
    """Choosing reply on a message shows THAT message in the compose bar's preview, and
    cancelling takes the preview away. Two messages share the thread so the preview
    has to name the right one — a preview that showed the latest message, or any fixed
    text, fails on the first reply."""
    conv = logged_in_app.conversations
    driver = logged_in_app.driver
    tag = secrets.token_hex(3)
    sender = f"rosa-{tag}@self-nest.test"
    first, second = f"first question {tag}", f"second question {tag}"
    thread_id = conv.inject_and_resolve_thread(rail="FaunaMls", sender=sender, body=first)
    assert conv.inject_and_resolve_thread(rail="FaunaMls", sender=sender, body=second) == thread_id, (
        "both messages must land in one thread for the preview to have a choice to get right"
    )
    conv.open_thread_by_id(thread_id)
    wait_until(
        lambda: driver.count("dm-reply-button") == 2,
        UI_SETTLE_S,
        diagnose=lambda: f"both bubbles should offer reply: {driver.diagnose('dm-reply-button')}",
    )
    assert driver.count("dm-reply-preview") == 0, (
        f"no reply is in progress yet: {driver.diagnose('dm-reply-preview')}"
    )

    for index, answered, other in ((0, first, second), (1, second, first)):
        driver.click("dm-reply-button", index=index)
        wait_until(
            lambda: driver.count("dm-reply-preview") == 1
            and answered in driver.get_text("dm-reply-preview"),
            UI_SETTLE_S,
            diagnose=lambda: f"replying to message {index} should preview {answered!r}: "
            f"{driver.diagnose('dm-reply-preview')}",
        )
        assert other not in driver.get_text("dm-reply-preview"), (
            f"the preview must show only the message answered: "
            f"{driver.get_text('dm-reply-preview')!r}"
        )
        driver.click("dm-reply-cancel")
        wait_until(
            lambda: driver.count("dm-reply-preview") == 0 and driver.count("dm-reply-cancel") == 0,
            UI_SETTLE_S,
            diagnose=lambda: f"cancel should take the reply preview away: "
            f"{driver.diagnose('dm-reply-preview')}",
        )


@pytest.mark.feature("replies-and-threads")
def test_mail_header_chips_are_informational(logged_in_app):
    """On a mail thread the header chips name every participant by full address, and
    nobody can be added or removed from them: you cannot un-send who a mail went to.

    tui shows each chip's full address in place, so there is nothing to reveal and its
    chips are inert labels. The witness chooses every chip anyway and asserts nobody
    left — the invariant every app shares, whatever its chip does when chosen — and
    that the add-participant button is not there at all."""
    conv = logged_in_app.conversations
    driver = logged_in_app.driver
    tag = secrets.token_hex(3)
    alice, carol = f"alice-{tag}@host.test", f"carol-{tag}@example.org"
    thread_id = conv.inject_and_open_thread(
        rail="Smtp",
        sender=alice,
        subject=f"Minutes {tag}",
        body=f"the minutes {tag}",
        recipients=[SEAM_SELF_ADDRESS, carol],
    )
    # The mail went from alice to you and carol. "You" is the app's own mail address
    # (the seam resolves its placeholder to the rail's real self), so the expected
    # set is the thread's published participants — asserted to be exactly alice,
    # carol and one address of your own.
    row = next(t for t in conv.list_threads() if t.thread_id == thread_id)
    expected = sorted(row.participant_displays)
    assert len(expected) == 3 and alice in expected and carol in expected, (
        f"the thread's participants are the mail's whole From/To/Cc set: {expected}"
    )

    def chips():
        # Stripped: web's chip carries the whitespace its template leaves beside the
        # optional review mark; the address is what the witness compares.
        return sorted(
            (driver.get_text("thread-member-chip", index=i) or "").strip()
            for i in range(driver.count("thread-member-chip"))
        )

    wait_until(
        lambda: chips() == expected,
        UI_SETTLE_S,
        diagnose=lambda: f"the header should chip every participant by full address "
        f"{expected}, shows {chips()}",
    )
    assert all("@" in chip for chip in chips()), f"every chip is a full address: {chips()}"
    assert driver.count("thread-add-participant-button") == 0, (
        "nobody can be added to a mail thread: "
        f"{driver.diagnose('thread-add-participant-button')}"
    )
    # Choose every chip: whatever choosing does, nobody leaves. An inert chip — tui's
    # and linux's, whose text already IS the full address — is refused by the agent
    # as having nothing to actuate, the one refusal allowed here (tui says "not
    # actuable", linux "not activatable"); any other failure is a real one.
    #
    # An app whose chip reveals the address on choosing (apple's popover) publishes it
    # as the chip's ``revealed`` attribute; one that shows it in place publishes none.
    # Either way, after choosing, the chip shows a full address or reveals one, and a
    # revealed address is one of the thread's participants — never somebody else.
    for i in range(len(expected)):
        try:
            driver.click("thread-member-chip", index=i)
        except (LookupError, RuntimeError) as refused:
            assert is_inert_refusal(refused), (
                f"choosing chip {i} failed for another reason than being inert: {refused}"
            )
        revealed = (driver.get_attr("thread-member-chip", "revealed", i) or "").strip()
        shown = (driver.get_text("thread-member-chip", index=i) or "").strip()
        assert "@" in shown or "@" in revealed, (
            f"choosing chip {i} must show or reveal a full address: text {shown!r}, "
            f"revealed {revealed!r}"
        )
        assert not revealed or revealed in expected, (
            f"chip {i} revealed {revealed!r}, who is not on this thread ({expected})"
        )
    assert chips() == expected, (
        f"choosing a chip must never remove anybody: {chips()} (was {expected})"
    )
    row = next(t for t in conv.list_threads() if t.thread_id == thread_id)
    assert sorted(row.participant_displays) == expected, (
        f"choosing a chip must never change who the thread is with: {row.participant_displays}"
    )
