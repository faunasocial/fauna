"""Tier_3 — another nest's actor's profile, read through the relay: two seats, two nests.

``profile.md`` § Where logic lives → *The remote read* (ruled 2026-10-19):
a remote actor's profile is read from the home the open target carried,
through the viewer's own nest, and only when the actor chose to show it beyond
their nest — the signed ``Profile.audience`` the edit form's
``profile-edit-audience-toggle`` sets. The header read for an OTHER actor is
this nest first, and on its ``not_found`` with a home carried, the relay
(``fauna.profile.get_remote``); a home that does not serve the profile is the
header's actor-id fallback, never ``error-message`` (§ Errors & edge cases).

**Topology — two real nest binaries with handle domains and floor TLS**, the
``test_conversation_room_community_cross_nest.py`` pair, because the reader's
nest has to dial the author's: the author is ``cross_nest_foreign_ephemeral``'s
pre-provisioned handled actor (so the reader can find them as
``handle@authority``), the reader a fresh account on ``cross_nest_foreign``.
The reader reaches the author's profile by door (1), the Find User result,
which carries the nest the lookup found them on as the open target's home.

**What the journey proves, in order:** an author who never opened their
profile shows as their id on the other nest, with no error; the author turns
the toggle on through their own edit form and saves (``fauna.profile.set``),
and the form reopens on the stored value; the reader's next open shows the
author's display name, read from the author's home through the reader's nest
— and paints the author's avatar, fetched by the reader's app straight from
the author's home over the plane the relay reply's ``home_nest_actor_id``
stamp pins (``transport-trust.md`` § Mechanism for the federation-granted row
→ *The post arm* → *A relayed profile's pictures*, ruled 2026-10-20): both
nests serve the self-signed floor, so a painted avatar is the pin at work —
on the WebPKI floor alone the fetch fails at TLS and the element stays empty.

**Latency-independent by construction** (convention 14): every wait is a
deadline poll over projected state. The negative leg needs a completion
barrier — "the header still shows the id" holds before the read answers as
well as after — and the profile open supplies one: the open-time block-state
read is issued after the header read returns, on the same task, and
``profile-block-button`` stays disabled until it lands.
"""

from __future__ import annotations

import secrets
from pathlib import Path

import pytest

from helpers.room_seats import (  # noqa: F401 — room_app is a pytest fixture adopted by import
    Seat,
    launch_seat,
    room_app,
    seat_app_path,
    seat_element,
    teardown_seats,
    wait_for,
)

pytestmark = [
    pytest.mark.tier_3,
    # tui LEADS (`profile.md` § Implementation status today); the others
    # join through the batched trickle-down, each with its own marker.
    pytest.mark.tui,
    pytest.mark.android,
    pytest.mark.linux,
]

READ_BUDGET_S = 30.0
"""For a profile open's reads to answer — the relay leg is one nest → nest
round trip under the 30 s peer deadline the relay itself enforces; the
avatar's fetch from the home (one pin graduation, one blob GET) rides the
same budget."""

FIXTURE_DIR = Path(__file__).parent.parent / "fixtures"
TEST_IMAGE = FIXTURE_DIR / "test-image.png"


def _short_id(hex_id: str) -> str:
    """The OTHER header's id fallback — ``fauna_core::format::short_id``
    (``value-formatting.md`` § Short id): 12 hex characters and an ellipsis."""
    return hex_id if len(hex_id) <= 12 else hex_id[:12] + "…"


@pytest.fixture()
def remote_profile_seats(cross_nest_foreign, cross_nest_foreign_ephemeral, room_app, request):
    """``(author, reader)`` on two nests: the author the handled actor
    ``cross_nest_foreign_ephemeral`` provisioned, on their own home; the reader
    a fresh account on ``cross_nest_foreign``."""
    away = cross_nest_foreign_ephemeral
    app_path = seat_app_path(request, room_app)
    seats: list[Seat] = []
    try:
        author = launch_seat(away, app_path, room_app, request, actor=away["actor"])
        seats.append(author)
        reader = launch_seat(cross_nest_foreign, app_path, room_app, request)
        seats.append(reader)
        yield author, reader
    finally:
        teardown_seats(seats)


def _open_author_by_find(reader: Seat, address: str, author_hex: str) -> None:
    """Door (1): Find User by ``handle@authority``, then the result's open
    button — and wait for the open's reads to land (the barrier above)."""
    contacts = reader.app.contacts
    contacts.navigate()
    contacts.find_by_handle(address)
    found = wait_for(
        f"the Find User lookup of {address} resolving across nests",
        contacts.actor_id_result_text,
        lambda text: text == author_hex,
        READ_BUDGET_S,
        diagnose=lambda: f"find-error={contacts.find_error_text()!r}",
    )
    assert found == author_hex
    contacts.open_found_profile()
    viewed = reader.app.profile.wait_for_viewed_actor(author_hex)
    assert viewed == author_hex, f"profile.actor_id={viewed!r}"
    wait_for(
        "the profile open's reads answering (profile-block-button enabled)",
        lambda: reader.app.driver.is_enabled("profile-block-button"),
        bool,
        READ_BUDGET_S,
        diagnose=lambda: seat_element(reader, "profile-block-button"),
    )


@pytest.mark.feature("profile")
def test_a_remote_actors_profile_shows_their_name_only_once_they_open_it(
    remote_profile_seats, cross_nest_foreign_ephemeral
):
    """Closed by default → the reader sees the id; the author turns the
    toggle on → the reader's next open shows their display name, relayed
    from the author's home, and paints their avatar, fetched from that home
    over the pinned plane."""
    author, reader = remote_profile_seats
    author.app.profile.require_remote_profile_read_supported()
    author.app.profile.require_avatar_banner_upload_supported()
    reader.app.contacts.require_find_result_opens_profile()
    reader.app.profile.require_header_pictures_supported()
    if not TEST_IMAGE.exists():
        pytest.skip(f"test fixture missing: {TEST_IMAGE}")
    away = cross_nest_foreign_ephemeral
    address = f"{author.handle}@{away['authority']}"
    name = f"Remote {secrets.token_hex(3)}"

    # ── the author publishes a name and an avatar, the toggle untouched ──
    mine = author.app.profile
    mine.navigate()
    mine.open_edit_form()
    mine.set_display_name(name)
    mine.set_avatar(str(TEST_IMAGE))
    mine.save()
    assert mine.wait_for_save(), f"the author's save failed: {mine.error_text()!r}"
    assert mine.wait_for_handle(name), f"SELF header={mine.handle_text()!r}"
    state = mine.wait_for_picture_state("profile-avatar", "painted")
    assert state == "painted", f"the author's own header paints the avatar: {state!r}"

    # ── closed: the reader sees the id fallback, and no error ────────────
    _open_author_by_find(reader, address, author.actor_hex)
    theirs = reader.app.profile
    assert theirs.handle_text() == _short_id(author.actor_hex), (
        "a profile its owner did not open to other nests is the id fallback: "
        f"header={theirs.handle_text()!r}"
    )
    assert theirs.error_text() == "", (
        f"the fallback is never an error: error-message={theirs.error_text()!r}"
    )
    assert theirs.picture_state("profile-avatar") == "absent", (
        "no profile reached the reader, so no picture is named: "
        f"{theirs.picture_state('profile-avatar')!r}"
    )

    # ── the author opens it: the toggle, saved and read back ─────────────
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
    assert mine.wait_for_audience_toggle(True), (
        "the saved audience round-trips through fauna.profile.set: "
        f"{seat_element(author, 'profile-edit-audience-toggle')}"
    )
    mine.cancel()

    # ── open: the reader's next open shows the author's name and avatar ──
    _open_author_by_find(reader, address, author.actor_hex)
    assert theirs.handle_text() == name, (
        "an opened profile is read from its home through the reader's nest: "
        f"header={theirs.handle_text()!r}, error={theirs.error_text()!r}"
    )
    assert theirs.error_text() == "", f"error-message={theirs.error_text()!r}"
    # The avatar's bytes come from the author's home, which serves the
    # self-signed floor: they paint only because the relay reply stamped the
    # home's identity and the plane graduated its pin against it.
    state = theirs.wait_for_picture_state("profile-avatar", "painted", READ_BUDGET_S)
    assert state == "painted", (
        "a relayed profile's avatar paints from a self-signed home over the "
        f"pinned plane, got {state!r}; error-message={theirs.error_text()!r}"
    )
    assert theirs.error_text() == "", f"error-message={theirs.error_text()!r}"
