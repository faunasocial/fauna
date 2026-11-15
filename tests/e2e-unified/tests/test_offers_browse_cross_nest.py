"""Tier_3 — browsing the tiers of an author whose account lives on ANOTHER nest.

``docs/goal/behavior/subscriptions.md`` § Pillar 1 → *The offers browse across
nests* is the owner. The OTHER Tiers tab of a creator homed on another nest
lists that creator's offered tiers, read from their home through the viewer's
own nest: the app's offers load passes the page's actor-home slot to the shared
``SubscriptionsClient::offers_list_across_nests``, and the viewer's status
stays the viewer's own nest's ``status.get``.

**Topology:** the two-nest shape ``test_federated_follow_cross_nest.py``
drives. The author is a handled actor on ``home`` (``cross_nest_foreign``);
the reader is ``away``'s (``cross_nest_foreign_ephemeral``) pre-provisioned
handled actor.

**What the journey proves, in order:**

1. The author creates a paid tier through their own Tiers tab.
2. The reader finds the author by ``handle@authority``, opens their profile
   through the Find User door (which carries the home) and follows at the
   home.
3. The reader's Tiers tab lists the author's paid tier — read from ``home``
   through ``away``, which holds no tier of an author it does not host — with
   the shared status badge reading "Not subscribed" from ``away``'s own
   ``status.get``, while the follow button (the free followers tier's
   affordance, not a per-row offer) reads "Following".

Which tiers the browse withholds (a hidden tier, a post's pay-to-unlock tier)
is the nest's one ``offered_tiers`` filter, answered the same through the
relay; its witnesses are nest-side, not this journey's.

**Latency-independent by construction** (convention 14): every wait is a
deadline poll over projected state.
"""

from __future__ import annotations

import secrets

import pytest

from conftest import provision_foreign_handled_actor
from helpers.federated_follow import CROSSING_BUDGET_S, follow_across_nests
from helpers.room_seats import (  # noqa: F401 — room_app is a pytest fixture adopted by import
    Seat,
    launch_seat,
    room_app,
    seat_app_path,
    seat_log,
    seat_story,
    teardown_seats,
)

pytestmark = [
    pytest.mark.tier_3,
    # windows, macos, ios and android open the real conversations rail at
    # launch only under this marker (`launch_seat` → `enable_real_faunamls`).
    pytest.mark.real_conversations,
    # tui LEADS (`testing.md` § Default app and nest mode); the other six join
    # with the batched trickle-down, each adding its marker here.
    pytest.mark.tui,
    pytest.mark.android,
    pytest.mark.ios,
    pytest.mark.linux,
    pytest.mark.macos,
    pytest.mark.web,
]


@pytest.fixture()
def browse_seats(cross_nest_foreign, cross_nest_foreign_ephemeral, room_app, request):
    """``(author, reader)`` on two nests: the author a fresh handled actor on
    ``cross_nest_foreign``, the reader ``cross_nest_foreign_ephemeral``'s
    handled actor."""
    home = cross_nest_foreign
    away = cross_nest_foreign_ephemeral
    app_path = seat_app_path(request, room_app)
    seats: list[Seat] = []
    try:
        author = launch_seat(
            home, app_path, room_app, request, actor=provision_foreign_handled_actor(home)
        )
        seats.append(author)
        reader = launch_seat(away, app_path, room_app, request, actor=away["actor"])
        seats.append(reader)
        yield author, reader
    finally:
        teardown_seats(seats)


@pytest.mark.feature("subscriptions-and-tiers")
def test_a_reader_on_another_nest_sees_the_authors_offered_tiers(
    browse_seats, cross_nest_foreign, cross_nest_foreign_ephemeral
):
    """The author offers a paid tier on their home; a reader on another nest
    opens the author's profile through Find User, follows, and the Tiers tab
    lists the paid tier with the viewer's status badge."""
    author, reader = browse_seats
    home = cross_nest_foreign
    away = cross_nest_foreign_ephemeral
    paid = f"gold{secrets.token_hex(3)}"

    # ── 1. The author offers a paid tier on their home ──────────────────
    subs = author.app.subscriptions
    subs.navigate()
    subs.open_tiers_tab()
    subs.create_tier(paid, rank=1, price_hint="$5/mo")
    assert subs.wait_for_tier(paid), (
        f"the author's tier {paid!r} should list in their own Tiers tab; "
        f"error={subs.error_text()!r}"
    )

    # ── 2. The door and the follow at the home ──────────────────────────
    follow_across_nests(reader, author, home, away)

    # ── 3. The reader's Tiers tab reads the offers from the home ────────
    profile = reader.app.profile
    profile.open_tiers_tab()
    assert profile.wait_for_offer(paid, timeout=CROSSING_BUDGET_S), (
        f"the reader's Tiers tab should list the author's paid tier {paid!r}, read "
        f"from their home through the reader's own nest; rows={profile.offer_count()} "
        f"error={profile.error_text()!r}\n{seat_story(reader)}\n"
        f"{seat_log(reader, 'offers', 'list_remote', 'relay')}"
    )
    assert profile.offer_count() == 1, (
        "the followers tier is the follow button's, never a per-row offer: "
        f"rows={profile.offer_count()}"
    )
    assert profile.wait_for_offer_status("Not subscribed"), (
        f"the badge reads the reader's own nest's status: {profile.offer_status()!r}"
    )
    assert profile.follow_label() == "Following", profile.follow_label()
    assert not reader.app.has_error(), reader.app.error_text()
