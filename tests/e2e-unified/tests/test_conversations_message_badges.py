"""A message bubble paints each crypto badge only when its own shared flag says so.

`encrypted-badge`, `signed-badge` and `verified-badge` (ui.yaml elements) render
off `MessageSnapshot.badges` — the shared `MessageBadges` projection
(`libs/fauna-conversations/src/message.rs`) — one badge per true flag and no
text beside them, as `apps/fauna-linux/src/views/conversations/message_bubble.rs`
paints them. A plain mail message is neither encrypted nor signed, so its bubble
paints none of the three.

Driven through the conversations inject seam (`inject_and_open_thread`), so the
message's badges are deterministic and nothing waits on a live rail.
"""

import pytest

from helpers import budgets
from helpers.waiting import wait_until

pytestmark = [pytest.mark.tier1, pytest.mark.tier_3]

SENDER = "badges-plain@self-nest.test"


def test_a_plain_mail_message_paints_no_crypto_badge(logged_in_app):
    """An unsigned, unencrypted message shows no encrypted, signed or verified
    badge — an absent flag paints nothing, never a badge for its negation."""
    conv = logged_in_app.conversations
    d = logged_in_app.driver

    conv.inject_and_open_thread(
        rail="Smtp",
        sender=SENDER,
        subject="Badge check",
        body="A plain message: no signature, no encryption.",
    )
    wait_until(
        lambda: d.count("dm-message-text") >= 1,
        budgets.UI_SETTLE_S,
        diagnose=lambda: d.diagnose("dm-message-text"),
    )

    for badge in ("encrypted-badge", "signed-badge", "verified-badge"):
        assert d.count(badge) == 0, (
            f"a plain mail message painted {badge}: {d.diagnose(badge)}"
        )
