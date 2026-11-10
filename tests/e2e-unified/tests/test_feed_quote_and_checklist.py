"""Feed-read: a quote post carries the post it quotes as a card inside it
(render-model.md § D6 — Feed body adopts the same `RenderDocument`: the feed
body is the shared document, and a `Reference::Quote` folds the quoted post in
as a `QuotedPost` embed the page paints as its `quoted-post` element).

Quote WITH commentary has no UI path on any app yet (the quote button fires a
commentary-less quote — feed.md § Interaction bar; see the same note in
`test_feed_restricted_reference.py`), so the quoting post is the ordinary signed
post another actor's app would create, sent over the wire. That is setup, not
the journey under test: the subject is what the READER's app paints.

The checklist outcome (feed-read 15) is read through
`FeedActions.post_painted_lines`, not `feed-post-text`: that element's text is the
document's one-line plaintext, which drops a task item's box state and a list's
nesting, while the painted read keeps both (`helpers/painted_lines.py`: tui's
paint lines, web's rendered DOM), so that test runs where an app publishes it.

tier_3: a real `fauna-nest` binary + real signed posts.
"""
import time
import uuid

import pytest

from common.auth import create_actor_and_register
from helpers.waiting import wait_until
from tests.api import ws_api
from tests.api.bare import post_reference, sign_and_encode_post

pytestmark = pytest.mark.tier_3

# How long the page may take to follow the state it paints — a ceiling for a
# broken surface, never a subject (convention 14): every poll returns on the
# first read that shows the expected state.
REPAINT_BUDGET_S = 30.0


def _unique(prefix: str) -> str:
    return f"{prefix}-{uuid.uuid4().hex[:8]}"


@pytest.mark.macos
@pytest.mark.ios
@pytest.mark.web
@pytest.mark.linux
@pytest.mark.tui
@pytest.mark.windows
@pytest.mark.android
@pytest.mark.feature("feed-read")
def test_a_quote_post_shows_the_quoted_post_as_a_card_inside_it(
    logged_in_app, nest_instance
):
    """render-model.md § D6: the feed body is the shared `RenderDocument`, and a
    quote post's document carries the quoted post as a `QuotedPost` embed, which
    every app paints as the `quoted-post` card inside the quoting post's card.

    I post something; someone else quotes it with words of their own. On their
    quote's card the commentary is the post's own text, and the `quoted-post`
    card inside it carries MY words — the quoted original, not a second copy of
    the commentary. Each half is told apart from its wrong answer: the embed
    must hold the original's text and not the commentary, and the card must be
    the quote's own (found by its id), not my original's card.
    """
    app = logged_in_app
    feed = app.feed

    original = _unique("quoted-original")
    feed.create_post(text=original)
    row = feed.wait_for_post_state_by_text(original)
    assert row is not None and row.get("post_id"), (
        f"own post {original!r} should be readable from feed state; "
        f"error={app.error_text()!r}"
    )
    original_id = row["post_id"]

    other = create_actor_and_register(
        nest_instance["port"],
        base_url=nest_instance["url"],
        admin_signing_key=nest_instance["admin"]["signing_key"],
    )
    commentary = _unique("quote-commentary")
    quote_id = ws_api.create_post(
        nest_instance["port"], other,
        sign_and_encode_post(
            other["signing_key"], int(time.time() * 1_000_000), commentary,
            references=[post_reference("Quote", original_id)],
        ),
    )

    # No app live-reloads on another actor's out-of-band post; re-select the
    # feed the way a user would (the `test_feed_post_delete.py` pattern).
    feed.open_feed("General")

    def embed_of_quote() -> str | None:
        at = feed.post_index_by_id(quote_id)
        return None if at < 0 else feed.quoted_post_text(at)

    def text_of_quote() -> str | None:
        at = feed.post_index_by_id(quote_id)
        return None if at < 0 else feed.post_text(at)

    # The embed folds in asynchronously once the quote row has landed (the
    # manager resolves the original, then re-emits), so poll (convention 14).
    wait_until(
        lambda: original in (embed_of_quote() or ""),
        REPAINT_BUDGET_S,
        diagnose=lambda: (
            f"the quote card must show the quoted post inside it: quote in "
            f"window={feed.post_state_by_id(quote_id) is not None} "
            f"quoted-post text={embed_of_quote()!r} "
            f"card text={text_of_quote()!r} error={app.error_text()!r}"
        ),
    )
    assert commentary not in (embed_of_quote() or ""), (
        f"the quoted-post card must carry the ORIGINAL, not the quote's own "
        f"commentary; quoted-post text={embed_of_quote()!r}"
    )
    wait_until(
        lambda: commentary in (text_of_quote() or ""),
        REPAINT_BUDGET_S,
        diagnose=lambda: (
            f"the quote card must still show its own commentary {commentary!r} "
            f"beside the embed; card text={text_of_quote()!r} "
            f"error={app.error_text()!r}"
        ),
    )


def _painted_line_with(lines: list[str] | None, word: str) -> str | None:
    """The one painted line carrying ``word``, or ``None``."""
    found = [line for line in (lines or []) if word in line]
    return found[0] if len(found) == 1 else None


@pytest.mark.tui
@pytest.mark.web
@pytest.mark.linux
@pytest.mark.android
@pytest.mark.macos
@pytest.mark.ios
@pytest.mark.feature("feed-read")
def test_a_checklist_post_paints_ticked_and_unticked_boxes_and_a_nested_item_indents(
    logged_in_app, nest_instance
):
    """render-model.md § Implementation status today (D7 task lists and nested
    lists): a post written with checklist items reads as a document whose task
    items paint a static box — ticked or unticked — and whose nested items
    indent under their parent.

    Someone posts a short checklist: one ticked item, one unticked item, and an
    item nested under the unticked one. On my feed their card paints the ticked
    item with a ticked box and the unticked item with an empty one (each told
    apart from the other: the words alone are the same in the plaintext either
    way), and the nested item sits further right than its parent, its own box
    intact.

    The painted read (`post_painted_lines`) is tui's paint lines and web's
    rendered DOM; another app joins when its agent publishes the same read
    (`surfaces.toml` `document-painted-read`).
    """
    app = logged_in_app
    feed = app.feed

    ticked = _unique("ticked")
    open_item = _unique("unticked")
    nested = _unique("nested")
    body = f"- [x] {ticked}\n- [ ] {open_item}\n  - [x] {nested}"

    author = create_actor_and_register(
        nest_instance["port"],
        base_url=nest_instance["url"],
        admin_signing_key=nest_instance["admin"]["signing_key"],
    )
    post_id = ws_api.create_post(
        nest_instance["port"], author,
        sign_and_encode_post(
            author["signing_key"], int(time.time() * 1_000_000), body,
        ),
    )

    # No app live-reloads on another actor's out-of-band post; re-select the
    # feed the way a user would (the `test_feed_post_delete.py` pattern).
    feed.open_feed("General")

    def painted() -> list[str] | None:
        at = feed.post_index_by_id(post_id)
        return None if at < 0 else feed.post_painted_lines(at)

    def all_three_painted() -> bool:
        lines = painted()
        return all(
            _painted_line_with(lines, word) is not None
            for word in (ticked, open_item, nested)
        )

    wait_until(
        all_three_painted,
        REPAINT_BUDGET_S,
        diagnose=lambda: (
            f"the checklist post must paint each item on a line of its own: "
            f"in window={feed.post_state_by_id(post_id) is not None} "
            f"painted={painted()!r} error={app.error_text()!r}"
        ),
    )
    lines = painted()
    ticked_line = _painted_line_with(lines, ticked)
    open_line = _painted_line_with(lines, open_item)
    nested_line = _painted_line_with(lines, nested)

    assert ticked_line.lstrip().startswith("☑"), (
        f"a ticked item paints a ticked box; line={ticked_line!r} painted={lines!r}"
    )
    assert open_line.lstrip().startswith("☐"), (
        f"an unticked item paints an empty box; line={open_line!r} painted={lines!r}"
    )

    def lead(line: str) -> int:
        return len(line) - len(line.lstrip())

    assert lead(nested_line) > lead(open_line), (
        f"a nested item indents beyond its parent; parent={open_line!r} "
        f"nested={nested_line!r} painted={lines!r}"
    )
    assert nested_line.lstrip().startswith("☑"), (
        f"the nested item keeps its own ticked box; line={nested_line!r}"
    )
