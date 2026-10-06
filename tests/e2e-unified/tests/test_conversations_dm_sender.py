"""Every conversation message bubble shows a per-message sender label
(`dm-sender`, ui.yaml `conversations` page — "Sender display on a
conversation item"), rendered from `MessageSnapshot.sender_display`
(`apps/fauna-linux/src/views/conversations/message_bubble.rs`:242 and the
sibling implementations on web/windows/android/macOS/iOS/tui — the ID is
now wired on all 7 apps).

Driven through the same conversations inject seam as
`test_conversations_bubble_timestamp.py` (`inject_and_open_thread`) — the
injected message is deterministic, so the bubble's sender label can be
asserted without any live-rail resolution timing.
"""

import pytest

pytestmark = [pytest.mark.tier1, pytest.mark.tier_3]

SENDER = "carol-dmsender@self-nest.test"


@pytest.mark.feature("conversations")
def test_bubble_shows_sender_label(logged_in_app):
    """An open thread's message bubble shows a `dm-sender` element whose text
    identifies the injected sender."""
    conv = logged_in_app.conversations
    d = logged_in_app.driver

    conv.inject_and_open_thread(
        rail="FaunaMls",
        sender=SENDER,
        subject=None,
        body="hello from carol",
    )

    assert d.count("dm-message-text") >= 1, "no message bubble on select"
    assert d.count("dm-sender") >= 1, (
        f"bubble has no dm-sender element: {d.diagnose('dm-sender')}"
    )

    label = d.get_text("dm-sender", index=0).strip()
    assert label, "dm-sender should not render an empty label"
    assert SENDER in label or SENDER.split("@", 1)[0] in label, (
        f"dm-sender label {label!r} should identify the injected sender {SENDER!r}"
    )
