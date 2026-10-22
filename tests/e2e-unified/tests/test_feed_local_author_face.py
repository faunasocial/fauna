"""Tier_3 — a neighbour's post is named by their published face on the viewer's feed.

``bridges.md`` § Unified feed ingestion → *Bridged authors* → *The local native
author — the nest-side projection* (ruled 2026-10-19): the nest decorates a
post whose author is a user of this nest with that user's own face — their
signed ``Profile.display_name`` and their ``users`` handle — in
``PostSummary.author_display``, and every card hands it to
``peer_display_label`` (``value-formatting.md`` § Peer display label: nickname →
display name → handle → short id). No app code changes with the projection;
what this file witnesses is the journey on the apps.

**Two seats on one nest** (``room_seat_pair``): the author and a neighbour who
reads the nest's local feed. Both cases run the author's own gestures — the
profile edit form, the composer — and read what the neighbour's card
(``post-author``) and detail dialog (``feed-post-detail-author``) paint:

* the author published a display name → both read that name, never the id;
* the author is a bare handled account (no profile) → both read the handle,
  never the 12-hex short id the card painted before the projection.

**Latency-independent by construction** (convention 14): every wait is a
deadline poll over projected state; the ``author_display`` state key is the
barrier that the nest's projection has reached the neighbour's feed.
"""

from __future__ import annotations

import secrets

import pytest

from helpers.room_seats import (  # noqa: F401 — `room_app`/`room_seat_pair` are fixtures adopted by import
    Seat,
    room_app,
    room_seat_pair,
    seat_story,
    wait_for,
)

pytestmark = [pytest.mark.tier_3]

READ_BUDGET_S = 30.0
"""For the author's post to list on the neighbour's feed with its face."""


def _short_id(hex_id: str) -> str:
    """The id fallback — ``fauna_core::format::short_id`` (``value-formatting.md``
    § Short id): 12 hex characters and an ellipsis."""
    return hex_id if len(hex_id) <= 12 else hex_id[:12] + "…"


def _publish_name(author: Seat, name: str) -> None:
    """The author names themself through their own edit form and saves."""
    mine = author.app.profile
    mine.navigate()
    mine.open_edit_form()
    mine.set_display_name(name)
    mine.save()
    assert mine.wait_for_save(), f"the author's save failed: {mine.error_text()!r}"
    assert mine.wait_for_handle(name), f"SELF header={mine.handle_text()!r}"


def _post_and_read(author: Seat, viewer: Seat, word: str) -> tuple[dict, str, str]:
    """The author posts ``word`` through the composer; returns the neighbour's
    feed row once it lists with its face resolved, and what the card and the
    detail dialog paint for the author."""
    author.app.feed.navigate()
    author.app.feed.create_post(f"local face {word}")
    assert not author.app.has_error(), author.app.error_text()

    def row():
        viewer.app.feed.reenter()
        return viewer.app.feed.post_state_by_text(word)

    found = wait_for(
        f"the post holding {word!r} reaching the neighbour's feed with its face",
        row,
        lambda r: r is not None and r.get("author_display") is not None,
        READ_BUDGET_S,
        diagnose=lambda: f"error-message={viewer.app.error_text()!r}\n{seat_story(viewer)}",
    )
    assert found.get("author") == author.actor_hex, found

    feed = viewer.app.feed
    index = feed.post_index_by_text(word)
    assert index >= 0, f"no card holds {word!r}"
    card = feed.post_author(index)
    feed.open_post_detail(index)
    detail = feed.post_detail_author()
    if viewer.app.driver.is_tui() or viewer.app.driver.is_linux():
        feed.close_post_detail()
    return found, card, detail


@pytest.mark.feature("feed-read")
def test_a_neighbours_published_display_name_labels_their_card_and_detail(room_seat_pair):
    """The author's signed display name names their post, card and detail."""
    author, viewer = room_seat_pair
    name = f"Neighbour {secrets.token_hex(3)}"
    word = f"named{secrets.token_hex(4)}"

    _publish_name(author, name)
    found, card, detail = _post_and_read(author, viewer, word)

    assert found["author_display"].get("display_name") == name, found["author_display"]
    short = _short_id(author.actor_hex)
    for where, text in (("post-author", card), ("feed-post-detail-author", detail)):
        assert name in text, f"{where} must read the published display name: {text!r}"
        assert short not in text, f"{where} must not fall back to the id: {text!r}"
    assert viewer.app.error_text() == "", viewer.app.error_text()


@pytest.mark.feature("feed-read")
def test_a_handle_only_neighbour_is_labelled_by_their_handle_not_the_short_id(room_seat_pair):
    """An author with a handle and no profile reads as that handle."""
    author, viewer = room_seat_pair
    word = f"bare{secrets.token_hex(4)}"

    found, card, detail = _post_and_read(author, viewer, word)

    assert found["author_display"].get("handle") == author.handle, found["author_display"]
    short = _short_id(author.actor_hex)
    for where, text in (("post-author", card), ("feed-post-detail-author", detail)):
        assert author.handle in text, f"{where} must read the handle: {text!r}"
        assert short not in text, f"{where} must not read the short id: {text!r}"
    assert viewer.app.error_text() == "", viewer.app.error_text()
