"""Cross-app Conversations — the content-label badge on a message.

``docs/goal/behavior/moderation.md`` § Per-row badge data path; ui.yaml
``content-label-badge`` (a child of ``dm-message-bubble``). The feed twin is
``test_feed_content_label_badge.py``; this is the conversation-message leg its
docstring names as untested.

A message carrying ``labels`` (``MessageSnapshot.labels`` — on a real receive,
the receiving app's own post-decrypt detection, ``observe_local_detection``, or
the room's home nest's served labels) paints a ``content-label-badge`` for its
highest-confidence category, through the one shared pick
(``fauna_core::content_category::primary_content_label``) the feed card uses.
An unlabeled message paints none.

tier_2: the labels are staged by the ``conversations_inject_inbound`` seam's
``labels`` field (the same staging the feed twin does through
``TestPostSpec.labels``), because the generic inject path does not classify.
That keeps the category deterministic: a real spam or phishing verdict at or
above the viewer's own threshold COLLAPSES the message (family-safety.md
§ Content policy) and paints no badge at all, which is a different outcome.

The badge is read unscoped: tui paints a bubble's children flat (no
``dm-message-bubble[i]`` scope), and one open thread holds exactly the two
messages this test injected, so a count of one is a count of the labeled one.
The order makes it causal: the badge count is read 0 with the unlabeled
message on screen, and 1 only once the labeled message joins it.
"""

import uuid

import pytest

from helpers.waiting import wait_until
from i18n.strings import S

pytestmark = [pytest.mark.tier_2]

# Poll ceilings for a broken surface, never subjects (convention 14).
MESSAGE_PAINT_BUDGET_S = 15.0


@pytest.mark.macos
@pytest.mark.ios
@pytest.mark.web
@pytest.mark.linux
@pytest.mark.tui
@pytest.mark.windows
@pytest.mark.feature("post-badges")
def test_a_labeled_message_shows_its_content_label(logged_in_app):
    app = logged_in_app
    conv = app.conversations
    d = app.driver
    sender = f"label-badge-{uuid.uuid4().hex[:8]}@self-nest.test"

    thread_id = conv.inject_and_resolve_thread(
        rail="FaunaMls",
        sender=sender,
        subject=None,
        body="an ordinary message, nothing to say about it",
    )
    conv.open_thread_by_id(thread_id)
    wait_until(
        lambda: d.count("dm-message-text") >= 1,
        MESSAGE_PAINT_BUDGET_S,
        diagnose=lambda: f"messages={d.count('dm-message-text')} error={app.error_text()!r}",
    )
    assert d.count("content-label-badge") == 0, (
        "an unlabeled message must paint no content-label-badge"
    )

    # The labeled message joins the same thread. Two verdicts, so the badge
    # must also make the highest-confidence pick (commercial 900 over spam 200,
    # a spam score well under any collapse threshold).
    same_thread = conv.inject_and_resolve_thread(
        rail="FaunaMls",
        sender=sender,
        subject=None,
        body="buy one, get one: a labeled message",
        labels=[
            {"category": "spam", "confidence_per_mille": 200},
            {"category": "commercial", "confidence_per_mille": 900},
        ],
    )
    assert same_thread == thread_id, (
        f"the second inject should land in the open thread {thread_id!r}, "
        f"not {same_thread!r}"
    )
    wait_until(
        lambda: d.count("dm-message-text") >= 2 and d.count("content-label-badge") == 1,
        MESSAGE_PAINT_BUDGET_S,
        diagnose=lambda: (
            f"messages={d.count('dm-message-text')} "
            f"badges={d.count('content-label-badge')} error={app.error_text()!r}"
        ),
    )
    badge = d.get_text("content-label-badge")
    assert S.moderation.category.commercial in badge, (
        f"the badge must name the highest-confidence category "
        f"({S.moderation.category.commercial!r}); got {badge!r}"
    )
