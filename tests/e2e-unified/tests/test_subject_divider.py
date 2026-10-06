"""Subject changes within a thread render as inline subject-dividers."""

import pytest

pytestmark = pytest.mark.tier_3

# A thread is keyed by (rail, sender, subject), and `nest_instance`/`test_user` are
# SESSION-scoped — so two tests sharing a subject share one accumulating thread, and
# each sees the other's messages (and dividers). These tests assert on divider COUNTS,
# so they must each key their own thread: same sender, per-test subject. Opening is by
# identity (`inject_and_open_thread`), so the subject is a keying detail, not a needle.
SENDER = "alice@host.test"


@pytest.mark.feature("replies-and-threads")
def test_divider_appears_on_subject_change(logged_in_app):
    """Append message with new subject_line; divider rendered above its bubble."""
    conv = logged_in_app.conversations
    subject = "Q4 budget divider-change"
    conv.inject_and_open_thread(
        rail="Smtp", sender=SENDER, subject=subject, body="numbers"
    )
    # Now inject another message in the same thread but with a new subject
    conv.inject_inbound_for_test(
        rail="Smtp",
        sender=SENDER,
        subject=subject,
        body="lunch tomorrow?",
        force_subject_change="lunch tomorrow",  # sets MessageSnapshot.subject_line via in-reply-to threading
    )
    dividers = conv.read_subject_dividers()
    assert any("lunch tomorrow" in d.lower() for d in dividers)


def test_no_extra_divider_for_same_subject(logged_in_app):
    """Sequential messages with same subject_line produce no divider after the first."""
    conv = logged_in_app.conversations
    subject = "Q4 budget same-subject"
    thread_id = conv.inject_and_resolve_thread(
        rail="Smtp", sender=SENDER, subject=subject, body="m1"
    )
    for body in ["m2", "m3"]:
        conv.inject_inbound_for_test(
            rail="Smtp", sender=SENDER, subject=subject, body=body
        )
    conv.open_thread_by_id(thread_id)
    dividers = conv.read_subject_dividers()
    # First message starts the thread; no divider needed for subsequent same-subject messages
    assert len(dividers) <= 1
