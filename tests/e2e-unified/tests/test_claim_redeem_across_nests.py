"""Tier_3 — a claim code minted on another nest, redeemed from the creator's
profile.

``docs/goal/behavior/monetization.md`` § Pillar 3 → *Buyer↔actor binding (Q4)*
→ *Which nest an in-app redeem binds on* → *The doors* is the owner; the page
half is ``docs/goal/ui/profile.md`` § Layout & flow → *Another's profile*. A
creator's code binds on the creator's own nest, so the app's redeem must name
that nest: the OTHER profile's Tiers tab carries the
``subscription-claim-redeem-input`` + ``subscription-claim-redeem-button`` pair
of ``subscription-settings`` and calls the one shared method,
``PaymentsClient::claims_redeem_across_nests``, with the page's actor-home slot
exactly as ``profile-follow-button`` passes it.

**Topology:** two real nest binaries with handle domains and floor TLS (the
``test_federated_follow_cross_nest.py`` shape). The creator is a handled actor
on ``home`` (``cross_nest_foreign``) in their own app seat; the buyer is
``away``'s (``cross_nest_foreign_ephemeral``) pre-provisioned handled actor in
a second seat.

**The journey, every mutation a gesture (convention 8):**

1. The creator makes a paid tier and mints a code for it from the Tiers tab's
   §5 manual-mint section.
2. The buyer finds the creator by ``handle@authority`` and opens their Profile
   through the Find User result, which carries the creator's home.
3. On the Tiers tab the buyer redeems a code that does not exist: the refusal
   from the creator's nest surfaces through ``error-message``.
4. The buyer redeems the real code: the input clears, and the creator's nest
   holds the code as redeemed by the buyer.
5. The creator's app approves the payment-entitled request with no gesture
   (its pump's ``drain_auto_approvals``), and the approval crosses back: the
   buyer's own nest reads the tier as held (``status.get`` over
   ``remote_follows`` — the read the tier's status badge derives from).

**Why step 5 reads the buyer's nest and not a badge.** The offers browse
(``fauna.subscriptions.offers.list``) reads the buyer's own nest, which holds
no tier rows for a creator homed elsewhere, so no offer row — and no badge —
paints for a remote creator yet; the redeem row is outside the offer rows and
paints regardless. ``status.get`` on the buyer's nest is the projected state
the badge would read.

**What a regression looks like.** A redeem that drops the home sends the code
to the buyer's own nest, which answers ``claim_not_found`` for a code it never
minted: step 4 fails on the error text, and the creator's code stays
unredeemed.

Latency-independent by construction (convention 14): every wait is a deadline
poll over projected state.
"""

from __future__ import annotations

import secrets

import pytest

from conftest import provision_foreign_handled_actor
from helpers.federated_follow import (
    CROSSING_BUDGET_S,
    call,
    follow_status,
    open_profile_across_nests,
)
from helpers.room_seats import (  # noqa: F401 — room_app is a pytest fixture adopted by import
    MESSAGE_BUDGET_S,
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
    pytest.mark.real_conversations,
    pytest.mark.tui,
    pytest.mark.web,
    pytest.mark.linux,
    pytest.mark.android,
    pytest.mark.windows,
    pytest.mark.macos,
    pytest.mark.ios,
]


@pytest.fixture()
def redeem_seats(cross_nest_foreign, cross_nest_foreign_ephemeral, room_app, request):
    """``(creator, buyer)`` on two nests: the creator a fresh handled actor on
    ``cross_nest_foreign`` (their home), the buyer the handled actor
    ``cross_nest_foreign_ephemeral`` provisioned."""
    home = cross_nest_foreign
    away = cross_nest_foreign_ephemeral
    app_path = seat_app_path(request, room_app)
    seats: list[Seat] = []
    try:
        creator = launch_seat(
            home, app_path, room_app, request, actor=provision_foreign_handled_actor(home)
        )
        seats.append(creator)
        buyer = launch_seat(away, app_path, room_app, request, actor=away["actor"])
        seats.append(buyer)
        yield creator, buyer
    finally:
        teardown_seats(seats)


def _claim(home: dict, creator: Seat, code: str) -> dict:
    """The creator's own claim row for ``code``, read on their nest."""
    rows = call(home, creator.actor, "fauna.payments.claims.list", {}).get("claims", [])
    return next((c for c in rows if c.get("code") == code), {})


@pytest.mark.feature("subscriptions-and-tiers")
def test_a_code_minted_on_another_nest_redeems_from_the_creators_profile(
    redeem_seats, cross_nest_foreign, cross_nest_foreign_ephemeral
):
    """The buyer redeems, on the creator's profile, a code the creator minted
    on their own nest: a bogus code is refused from there, the real one binds
    to the buyer there, and the creator's app grants it."""
    creator, buyer = redeem_seats
    home = cross_nest_foreign
    away = cross_nest_foreign_ephemeral
    tier = f"gold{secrets.token_hex(3)}"

    # ── 1. The creator: a paid tier and a code for it, through the Tiers tab
    tiers = creator.app.subscriptions
    tiers.navigate()
    tiers.open_tiers_tab()
    tiers.create_tier(tier, rank=1, price_hint="$5/mo")
    assert tiers.wait_for_tier(tier, timeout=MESSAGE_BUDGET_S), (
        f"the tier should list: error={creator.app.error_text()!r}"
    )
    tiers.mint_claim(tier)
    assert tiers.wait_for_claim_count(1, timeout=MESSAGE_BUDGET_S), (
        f"the minted code should list: error={creator.app.error_text()!r}"
    )
    code = tiers.claim_codes()[0]
    assert code, "the §5 row names the code"

    # ── 2. The buyer opens the creator's profile across nests ─────────────
    profile = open_profile_across_nests(buyer, creator, home)
    profile.open_tiers_tab()
    buyer.app.driver.wait_for("subscription-claim-redeem-input", timeout=MESSAGE_BUDGET_S)

    # ── 3. A code that does not exist is refused, from the creator's nest ─
    profile.redeem_claim(f"NOSUCH{secrets.token_hex(3).upper()}")
    refused = wait_for(
        "the bogus code's refusal surfacing on the profile",
        buyer.app.error_text,
        bool,
        CROSSING_BUDGET_S,
        diagnose=lambda: f"{seat_story(buyer)}\n{seat_log(buyer, 'redeem', 'claim')}",
    )
    assert refused, "a bogus code must surface fauna.payments.claim_not_found"

    # ── 4. The real code binds to the buyer on the creator's nest ─────────
    profile.redeem_claim(code)
    wait_for(
        "the creator's nest binding the code to the buyer",
        lambda: _claim(home, creator, code),
        lambda c: bytes(c.get("redeemed_by") or b"").hex() == buyer.actor_hex,
        CROSSING_BUDGET_S,
        diagnose=lambda: (
            f"error-message={buyer.app.error_text()!r}\n{seat_story(buyer)}\n"
            f"{seat_log(buyer, 'redeem', 'claim', 'federation')}"
        ),
    )
    wait_for(
        "the taken code leaving the redeem input",
        profile.redeem_input_text,
        lambda text: text == "",
        MESSAGE_BUDGET_S,
        diagnose=lambda: f"error-message={buyer.app.error_text()!r}",
    )
    assert not buyer.app.has_error(), (
        f"a taken code leaves no error behind: {buyer.app.error_text()!r}"
    )

    # ── 5. The creator's app grants it; the approval crosses back ─────────
    status = wait_for(
        "the paid request approved at the creator's nest and the state reaching the buyer's",
        lambda: follow_status(away, buyer, creator),
        lambda s: s.get("tier") == tier and not s.get("follow_pending"),
        CROSSING_BUDGET_S,
        diagnose=lambda: (
            f"{seat_story(creator)}\n{seat_log(creator, 'approv', 'drain', 'subscri', 'payment')}"
        ),
    )
    assert status.get("tier") == tier, status
