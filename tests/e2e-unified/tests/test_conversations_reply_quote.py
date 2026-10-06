"""A conversation message that **replies** to another message renders an
in-bubble reply-quote (``dm-message-quote``) above its body — the parent's
author plus a short snippet of the parent's text — on every app, projected
by the **one** shared manager seam (render-model.md § D2 ``QuotedMessage``).

The quote is a ``RenderBlock::QuotedMessage{author_display, snippet}`` the
conversations manager folds into the message's render document at read time
(``ConversationsManager::thread_detail``, alongside the D3 remote-image reveal
projection), resolving the parent from the same thread's message list. No client
reads the sibling ``reply_to`` field and re-projects the quote itself — they all
walk the document (priorities #1/#2/#4).

User-approved design (2026-06-23):

* **Content** — author display + up to two lines of the parent's body text.
* **Missing parent** — when the reply's parent is *not* loaded in the thread
  (a cross-MUA reply, or one not yet ingested: we hold only the bare id, no
  author/snippet), the quote is **hidden** and the bubble renders normally.
* **Tap-to-jump** — out of scope for this slice (deferred follow-on).

Driven through the conversations inject seam (the same path
``test_conversations_bubble_timestamp`` / ``test_conversations_remote_image``
use): ``inject_inbound_for_test`` returns the injected message id, and a second
inject references it via ``in_reply_to=`` so the reply threads onto the parent
(``ByMessageReference`` keying) and the manager can resolve the quote.
"""

import pytest

pytestmark = [pytest.mark.tier1, pytest.mark.tier_3]

# One peer per test = one thread per test. These tests assert EXACT bubble/quote counts,
# and `nest_instance`/`test_user` are session-scoped, so a peer shared with another test
# file (a bare "carol" was shared by five) merges its messages into this thread and
# breaks the counts. Threads are opened by identity, so a peer is a keying value here,
# never a lookup needle.
QUOTE_PEER = "carol-replyquote@self-nest.test"
NO_QUOTE_PEER = "dave-replyquote@self-nest.test"
ORPHAN_QUOTE_PEER = "erin-replyquote@self-nest.test"


@pytest.mark.feature("replies-and-threads")
def test_reply_shows_quote_of_parent(logged_in_app):
    """A reply bubble shows a ``dm-message-quote`` whose text contains a snippet
    of the parent message's body; the parent (non-reply) bubble shows none."""
    conv = logged_in_app.conversations
    d = logged_in_app.driver

    parent_id = conv.inject_inbound_for_test(
        rail="FaunaMls",
        sender=QUOTE_PEER,
        subject=None,
        body="Let us meet at noon by the fountain please",
    )
    conv.inject_and_open_thread(
        rail="FaunaMls",
        sender=QUOTE_PEER,
        subject=None,
        body="Sounds good, see you then",
        in_reply_to=parent_id,
    )

    assert d.count("dm-message-text") >= 2, "both messages should render"
    # Exactly the reply carries a quote; the parent (a non-reply) carries none.
    assert d.count("dm-message-quote") == 1, (
        "exactly one bubble (the reply) should show an in-bubble reply-quote; "
        f"got {d.count('dm-message-quote')} (client must walk the document's "
        "RenderBlock::QuotedMessage, not ignore it)"
    )
    quote = d.get_text("dm-message-quote", index=0)
    assert "fountain" in quote.lower(), (
        f"reply-quote {quote!r} does not contain the parent body snippet — the "
        "manager must project the parent's text into the QuotedMessage block"
    )


@pytest.mark.feature("replies-and-threads")
def test_non_reply_has_no_quote(logged_in_app):
    """A standalone (non-reply) message renders no ``dm-message-quote``."""
    conv = logged_in_app.conversations
    d = logged_in_app.driver

    conv.inject_and_open_thread(
        rail="FaunaMls",
        sender=NO_QUOTE_PEER,
        subject=None,
        body="hello there, no reply here",
    )

    assert d.count("dm-message-text") >= 1, "the message should render"
    assert d.count("dm-message-quote") == 0, (
        "a non-reply message must not show a reply-quote"
    )


@pytest.mark.feature("replies-and-threads")
def test_missing_parent_hides_quote(logged_in_app):
    """A reply whose parent was never ingested (bare ``in_reply_to`` id, no
    parent in the thread) renders no quote — the user-approved hide behaviour."""
    conv = logged_in_app.conversations
    d = logged_in_app.driver

    conv.inject_and_open_thread(
        rail="FaunaMls",
        sender=ORPHAN_QUOTE_PEER,
        subject=None,
        body="replying to a message you never saw",
        in_reply_to="msg-never-ingested-0001",
    )

    assert d.count("dm-message-text") >= 1, "the reply should still render"
    assert d.count("dm-message-quote") == 0, (
        "a reply whose parent is not loaded must hide the quote (we hold only "
        "the bare id, no author/snippet)"
    )
