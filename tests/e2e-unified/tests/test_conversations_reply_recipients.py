"""Reply-recipient editable "To" line (#2b).

Goal doc: ``conversations.md`` § Participants vs. reply recipients (ratified
2026-06-07). Reply recipients are a per-reply *draft*, distinct from the
historical thread participants: ``dm-reply-button`` seeds the sender only,
``dm-reply-all-button`` seeds every participant-but-self, and the
always-visible "To" line (``dm-reply-recipient-chip`` + ``-remove`` + ``-add``)
lets the user edit the set before sending. Removing a chip drops a recipient
from *this reply only* — thread history is untouched.

The To line + reply-all button are gated on the shared
``ThreadCapabilities.supports_recipient_selection`` (mail = true; FaunaMls =
false — recipients ARE the group), so they appear on a mail thread and are
hidden on a FaunaMls thread. The seeding/minus-self/dedup *logic* is covered by
shared-Rust unit tests (``fauna-conversations`` manager + smtp backend); this
e2e proves the linux app wires the buttons → manager → re-render correctly.

Driven through the same ``inject_inbound_for_test`` seam the other conversation
e2es use — no mail-enabled login required.
"""

import secrets

import pytest

from actions.conversations import SEAM_SELF_ADDRESS
from helpers.budgets import UI_SETTLE_S
from helpers.waiting import wait_until

pytestmark = [pytest.mark.tier_3, pytest.mark.linux, pytest.mark.web, pytest.mark.windows, pytest.mark.macos, pytest.mark.ios, pytest.mark.tui]


def _open_mail_thread(app, sender="alice@host.test", body="hi", recipients=None):
    """Inject one inbound mail and open it BY IDENTITY. A subject-keyed mail thread is
    labelled by its subject, not the sender, so neither the sender nor the subject is a
    safe needle — and `conversation-item` index 0 (what this used to click) is the first
    row of a list that every earlier test in this session-scoped run has been adding to,
    so it opened somebody else's thread as soon as this stopped being the only one.

    ``recipients`` is the mail's whole To/Cc set (default: the seam's self address
    alone) — what a reply-all has to choose from."""
    conv = app.conversations
    driver = app.driver
    conv.inject_and_open_thread(
        rail="Smtp", sender=sender, subject="Lunch", body=body, recipients=recipients
    )
    wait_until(
        lambda: driver.is_visible("thread-header"),
        UI_SETTLE_S,
        diagnose=lambda: f"thread detail never opened: {driver.diagnose('thread-header')}",
    )
    return conv


@pytest.mark.feature("replies-and-threads")
def test_reply_seeds_to_line_with_sender_and_chip_removes(logged_in_app):
    """``dm-reply-button`` seeds the To line with the message's sender; the
    chip's × drops it (and never touches thread membership)."""
    driver = logged_in_app.driver
    _open_mail_thread(logged_in_app)

    # The To line is present (mail = supports_recipient_selection) but empty
    # until a reply is seeded.
    assert driver.count("dm-reply-recipient-add") >= 1, (
        "To-line add input shows on mail: "
        f"{driver.diagnose('dm-reply-recipient-add')}"
    )
    assert driver.count("dm-reply-recipient-chip") == 0, (
        "To line starts empty before a reply is seeded: "
        f"{driver.diagnose('dm-reply-recipient-chip')}"
    )

    # Reply (sender-only) → exactly the sender appears as a chip.
    driver.click("dm-reply-button", index=0)
    driver.wait_for("dm-reply-recipient-chip", timeout=5.0)
    assert driver.count("dm-reply-recipient-chip") == 1, (
        "reply seeds exactly the sender as one chip: "
        f"{driver.diagnose('dm-reply-recipient-chip')}"
    )
    assert "alice@host.test" in driver.get_text("dm-reply-recipient-chip", index=0), (
        "reply chip should name the sender: "
        f"{driver.get_text('dm-reply-recipient-chip', index=0)!r} "
        f"{driver.diagnose('dm-reply-recipient-chip')}"
    )

    # × removes that recipient from this reply only.
    driver.click("dm-reply-recipient-remove", index=0)
    deadline_count = driver.count("dm-reply-recipient-chip")
    assert deadline_count == 0, f"chip should be gone, saw {deadline_count}"


@pytest.mark.feature("replies-and-threads")
def test_mail_thread_offers_reply_all(logged_in_app):
    """``dm-reply-all-button`` on a mail thread seeds the editable To line with
    every participant but yourself (``conversations.md`` § Participants vs. reply
    recipients: "reply-all = every participant but self").

    The mail goes from alice to you and carol, so the thread has three
    participants and reply-all has exactly two to seed. Counting the button
    alone (what this witness did until 2026-09-21) passed with reply-all seeding
    nobody, or seeding you as your own recipient — which it did under the e2e
    mock until the mock mail rail learned the seam's self address."""
    driver = logged_in_app.driver
    tag = secrets.token_hex(3)
    alice, carol = f"alice-{tag}@host.test", f"carol-{tag}@host.test"
    _open_mail_thread(
        logged_in_app,
        sender=alice,
        body=f"lunch for three {tag}",
        recipients=[SEAM_SELF_ADDRESS, carol],
    )
    assert driver.count("dm-reply-all-button") >= 1, (
        "reply-all offered on mail: "
        f"{driver.diagnose('dm-reply-all-button')}"
    )
    assert driver.count("dm-reply-recipient-chip") == 0, (
        "the To line starts empty before a reply is seeded: "
        f"{driver.diagnose('dm-reply-recipient-chip')}"
    )

    driver.click("dm-reply-all-button", index=0)

    def _seeded():
        n = driver.count("dm-reply-recipient-chip")
        return sorted(driver.get_text("dm-reply-recipient-chip", index=i) for i in range(n))

    wait_until(
        lambda: len(_seeded()) == 2,
        UI_SETTLE_S,
        diagnose=lambda: f"reply-all never seeded two recipients: {_seeded()!r} "
        f"{driver.diagnose('dm-reply-recipient-chip')}",
    )
    seeded = _seeded()
    assert any(alice in chip for chip in seeded) and any(carol in chip for chip in seeded), (
        f"reply-all must seed the sender and the other recipient, saw {seeded!r}"
    )
    assert not any(SEAM_SELF_ADDRESS in chip for chip in seeded), (
        f"reply-all must never address you to yourself, saw {seeded!r}"
    )


@pytest.mark.feature("replies-and-threads")
def test_fauna_thread_hides_to_line_and_reply_all(logged_in_app):
    """On a FaunaMls thread the recipients ARE the group membership, so the
    editable To line and reply-all button are hidden
    (``supports_recipient_selection`` == false)."""
    driver = logged_in_app.driver
    conv = logged_in_app.conversations
    conv.inject_and_open_thread(
        rail="FaunaMls", sender="bob-replyrecip@self-nest.test", body="hey"
    )

    # A reply button still exists (per-message reply), but the recipient-set
    # affordances do not (hidden widgets are pruned from the a11y tree → count 0).
    assert driver.count("dm-reply-recipient-add") == 0, (
        "no To line off mail (FaunaMls hides recipient selection): "
        f"{driver.diagnose('dm-reply-recipient-add')}"
    )
    assert driver.count("dm-reply-all-button") == 0, (
        "no reply-all off mail (FaunaMls hides recipient selection): "
        f"{driver.diagnose('dm-reply-all-button')}"
    )
