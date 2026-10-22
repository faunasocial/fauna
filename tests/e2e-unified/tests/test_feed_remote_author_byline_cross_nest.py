"""Tier_3 — a followed author on another nest is named on their feed cards by their own signed profile.

``feed.md`` § The read model → *The remote author's byline* (ruled
2026-10-19): a card or detail dialog of a post homed on another nest names its
author from the author's own signed profile, read by the reader's client
through its own nest's relay (``fauna.profile.get_remote``), and only when the
author chose to show it beyond their nest — the signed ``Profile.audience``
the edit form's ``profile-edit-audience-toggle`` sets (``profile.md`` § Where
logic lives → *The remote read*). Every other outcome is the id fallback,
never ``error-message``.

**Topology — two real nest binaries with handle domains and floor TLS**, the
``test_federated_follow_cross_nest.py`` pair: two authors homed on ``home``
(``cross_nest_foreign``), the follower ``away``'s
(``cross_nest_foreign_ephemeral``) pre-provisioned handled actor. Two authors,
not one author before and after the toggle, because the reader's face is held
for the feed window's life (§ Architectural rules, rule 1): one window
answers each author once.

**What the journey proves, in order:** the follower follows both authors
across nests; one author names themself and turns the toggle on, the other
names themself and leaves it off, all through their own edit forms; each
posts through the composer; on the follower's feed the opted-in author's card
and detail dialog (``post-author``, ``feed-post-detail-author``) read their
display name, while the other author's read their id, with
``error-message`` empty.

**Latency-independent by construction** (convention 14): every wait is a
deadline poll over projected state. The negative leg needs a completion
barrier — "the card still shows the id" holds before the relay answers as
well as after — and the post's ``author_display`` state key supplies it:
``null`` until the face resolve has answered, an empty face after.
"""

from __future__ import annotations

import secrets

import pytest

from conftest import provision_foreign_handled_actor
from helpers.app_surface import skip_unbuilt
from helpers.federated_follow import CROSSING_BUDGET_S, feed_row, follow_across_nests
from helpers.room_seats import (  # noqa: F401 — room_app is a pytest fixture adopted by import
    Seat,
    launch_seat,
    room_app,
    seat_app_path,
    seat_log,
    seat_story,
    teardown_seats,
    wait_for,
)

pytestmark = [
    pytest.mark.tier_3,
    # tui LEADS (`feed.md` § Implementation status today); the others
    # join through the batched trickle-down, each with its own marker.
    pytest.mark.tui,
    pytest.mark.android,
    pytest.mark.linux,
]

BUILT_APPS = frozenset({"tui", "android", "linux"})
"""The apps that build the remote author's byline; each joins with its marker above."""

READ_BUDGET_S = 30.0
"""For the face resolve to answer — one nest → nest round trip under the
30 s peer deadline the relay itself enforces."""


def _short_id(hex_id: str) -> str:
    """The card's id fallback — ``fauna_core::format::short_id``
    (``value-formatting.md`` § Short id): 12 hex characters and an ellipsis."""
    return hex_id if len(hex_id) <= 12 else hex_id[:12] + "…"


@pytest.fixture()
def byline_seats(cross_nest_foreign, cross_nest_foreign_ephemeral, room_app, request):
    """``(opted, closed, follower)``: two fresh handled authors on
    ``cross_nest_foreign`` (their home), and the follower that
    ``cross_nest_foreign_ephemeral`` provisioned."""
    if room_app not in BUILT_APPS:
        skip_unbuilt(
            room_app,
            surface="the remote author's byline on a delivered post",
            detail="tui leads the face resolve; the others follow in their trickle-down",
            tracked="ui/feed.md § Implementation status today",
        )
    home = cross_nest_foreign
    away = cross_nest_foreign_ephemeral
    app_path = seat_app_path(request, room_app)
    seats: list[Seat] = []
    try:
        for _ in range(2):
            seats.append(
                launch_seat(
                    home, app_path, room_app, request, actor=provision_foreign_handled_actor(home)
                )
            )
        seats.append(launch_seat(away, app_path, room_app, request, actor=away["actor"]))
        yield tuple(seats)
    finally:
        teardown_seats(seats)


def _publish_name(author: Seat, name: str, *, show_beyond_nest: bool) -> None:
    """The author names themself through their own edit form and saves; with
    ``show_beyond_nest`` they then turn ``profile-edit-audience-toggle`` on,
    save, and the form reopens on the stored value."""
    mine = author.app.profile
    mine.navigate()
    mine.open_edit_form()
    mine.set_display_name(name)
    mine.save()
    assert mine.wait_for_save(), f"the author's save failed: {mine.error_text()!r}"
    assert mine.wait_for_handle(name), f"SELF header={mine.handle_text()!r}"
    if not show_beyond_nest:
        return
    mine.open_edit_form()
    # The form is shown before its base lands; the display name it paints is
    # the barrier that the stored profile (and its audience) has arrived.
    wait_for(
        "the edit form's base landing (the display-name field)",
        lambda: author.app.driver.get_text("profile-edit-display-name"),
        lambda text: text == name,
        READ_BUDGET_S,
    )
    assert not mine.audience_toggle_checked(), "closed is the default"
    mine.toggle_audience()
    mine.save()
    assert mine.wait_for_save(), f"the toggle's save failed: {mine.error_text()!r}"
    mine.open_edit_form()
    assert mine.wait_for_audience_toggle(True), "the saved audience round-trips"
    mine.cancel()


def _resolved_face(follower: Seat, word: str, author: Seat) -> dict:
    """The follower's card for ``word`` once its delivery has listed and its
    face resolve has answered — the ``author_display`` state key, ``null``
    until then."""
    row = wait_for(
        f"the post holding {word!r} reaching the follower's feed and its face resolving",
        lambda: feed_row(follower, word),
        lambda r: r is not None and r.get("author_display") is not None,
        CROSSING_BUDGET_S + READ_BUDGET_S,
        diagnose=lambda: (
            f"error-message={follower.app.error_text()!r}\n{seat_story(follower)}\n"
            f"{seat_log(author, 'outbox', 'deliver')}\n{seat_log(follower, 'face', 'profile')}"
        ),
    )
    assert row.get("author") == author.actor_hex, row
    return row["author_display"]


def _card_and_detail_author(follower: Seat, word: str) -> tuple[str, str]:
    """What the card for ``word`` paints in ``post-author``, and what its
    detail dialog paints in ``feed-post-detail-author``."""
    feed = follower.app.feed
    index = feed.post_index_by_text(word)
    assert index >= 0, f"no card holds {word!r}"
    card = feed.post_author(index)
    feed.open_post_detail(index)
    detail = feed.post_detail_author()
    feed.close_post_detail()
    return card, detail


@pytest.mark.feature("feed-read")
def test_a_followed_remote_author_is_named_by_their_own_profile_only_once_they_show_it(
    byline_seats, cross_nest_foreign, cross_nest_foreign_ephemeral
):
    """The opted-in author's card and detail read their display name; the
    other author's read their id, and no error shows."""
    opted, closed, follower = byline_seats
    opted.app.profile.require_remote_profile_read_supported()
    home = cross_nest_foreign
    away = cross_nest_foreign_ephemeral
    opted_name = f"Shown {secrets.token_hex(3)}"
    closed_name = f"Kept {secrets.token_hex(3)}"
    opted_word = f"shown{secrets.token_hex(4)}"
    closed_word = f"kept{secrets.token_hex(4)}"

    # ── the follower follows both authors on their home ──────────────────
    follow_across_nests(follower, opted, home, away)
    follow_across_nests(follower, closed, home, away)

    # ── both authors name themselves; only one shows it beyond their nest ─
    _publish_name(opted, opted_name, show_beyond_nest=True)
    _publish_name(closed, closed_name, show_beyond_nest=False)

    # ── each posts through the composer ──────────────────────────────────
    for author, word in ((opted, opted_word), (closed, closed_word)):
        author.app.feed.navigate()
        author.app.feed.create_post(f"from home {word}")
        assert not author.app.has_error(), author.app.error_text()

    # ── the opted-in author: their own signed name, card and detail ──────
    face = _resolved_face(follower, opted_word, opted)
    assert face.get("display_name") == opted_name, (
        f"the face is the author's signed display name: {face!r}"
    )
    card, detail = wait_for(
        "the opted-in author's name painting on the card and in the detail",
        lambda: _card_and_detail_author(follower, opted_word),
        lambda pair: all(opted_name in text for text in pair),
        READ_BUDGET_S,
    )
    assert opted_name in card and opted_name in detail, (card, detail)

    # ── the other author: the id fallback, and no error ──────────────────
    face = _resolved_face(follower, closed_word, closed)
    assert not face.get("display_name"), (
        f"a profile its owner did not open to other nests is no face: {face!r}"
    )
    card, detail = _card_and_detail_author(follower, closed_word)
    for where, text in (("post-author", card), ("feed-post-detail-author", detail)):
        assert closed_name not in text, f"{where} must not name a closed profile: {text!r}"
        assert _short_id(closed.actor_hex) in text, f"{where} reads the id: {text!r}"
    assert follower.app.error_text() == "", (
        f"the fallback is never an error: error-message={follower.app.error_text()!r}"
    )
