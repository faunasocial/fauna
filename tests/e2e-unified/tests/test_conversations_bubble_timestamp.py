"""Every conversation message bubble shows a per-message timestamp, and every
app formats it through the **one** shared bucketer
``fauna_core::format::conversation_timestamp_display`` (value-formatting.md
§ Conversation timestamp) — today → a local 24h ``HH:MM`` clock, Yesterday /
weekday → an i18n key, ``≥ 7 d`` → a native locale date.

This closes the last per-app divergence on the conversation timestamp: the
conversation-LIST row already adopted the shared formatter on all 6 apps
(2026-06-20/21); the per-BUBBLE timestamp was rendered only by linux (a buggy
naive-UTC ``HH:MM``) and apple (already on the shared formatter), and not at all
by web / windows / android. Now all 6 read ``MessageSnapshot.timestamp_ms``
through the shared ``conversation_timestamp_display`` and tag the bubble's time
``dm-message-timestamp`` (render-model.md § D5-adjacent drift; priorities #1/#4).

Driven through the conversations inject seam (the same path
``test_conversations_remote_image`` / ``test_subject_divider`` use): the injected
message is stamped at "now", so it always lands in the **today** bucket and the
bubble renders the local 24h clock — a deterministic ``HH:MM`` assertion
independent of the host timezone.
"""

import re

import pytest

pytestmark = [pytest.mark.tier1, pytest.mark.tier_3]

# Today-bucket clock: two digits, colon, two digits (24h local wall clock).
_CLOCK = re.compile(r"^\d{2}:\d{2}$")


@pytest.mark.feature("conversations")
def test_bubble_shows_shared_timestamp(logged_in_app):
    """An open thread's message bubble shows a ``dm-message-timestamp`` whose text
    is the shared today-bucket 24h clock (``HH:MM``) for a just-injected message."""
    conv = logged_in_app.conversations
    d = logged_in_app.driver

    conv.inject_and_open_thread(
        rail="FaunaMls",
        sender="carol-timestamp@self-nest.test",
        subject=None,
        body="hello there",
    )

    assert d.count("dm-message-text") >= 1, "no message bubble on select"
    # The per-message timestamp is present on every bubble.
    assert (
        d.count("dm-message-timestamp") >= 1
    ), "bubble has no dm-message-timestamp element"

    # Just-injected → today bucket → the shared 24h local clock "HH:MM".
    ts = d.get_text("dm-message-timestamp", index=0).strip()
    assert _CLOCK.match(ts), (
        f"bubble timestamp {ts!r} is not the shared today-bucket HH:MM clock "
        "(client must format MessageSnapshot.timestamp_ms through the shared "
        "conversation_timestamp_display, not a hand-rolled formatter)"
    )
