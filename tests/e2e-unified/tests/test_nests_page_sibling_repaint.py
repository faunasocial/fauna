"""The Nests page repaints what another of your devices did there — an OPEN page,
no re-visit.

Goal doc: ``docs/goal/architecture/account-runtime.md`` § Multi-instance
concurrency → *A runtime's own pump is a source of the notice too*: an open
store-backed surface shows what a fresh visit would show, whichever process
applied the change; tui's notice re-drives the open Nests sub-page through
``settings::store_resync_op`` (``Op::HydrateNests { reread: true }``), which
re-reads the linked rows and the trust grants through the shared
``LinkedNestsMachine``. The catalog outcome is ``docs/features/nests-and-trust.md``
outcome 29: *a nest linked or a grant minted on another of your devices shows on
an open Nests page without leaving it*.

The journey: two seats of one account on this machine (convention 16) — the app
under test (seat A) and the fleet's tui sibling (seat B,
``helpers.fleet.SiblingSeats``). Both sign in as a dedicated nest's admin, the
one actor that can create the tier a paywalled-posts grant derives from and
mint it to the nest's own holder (``nests.md`` § Implementation status today:
holder discovery is admin-gated in v1). Seat A opens Settings → Nests ONCE and
never leaves it; seat B mints a grant (one test) or links a nest (the other)
through its own Nests page (convention 8), and it must appear on seat A's open
page.

**The link half's source is indirect.** The link itself is a nest-side write
(``fauna.pair.add``, then the machine's own refresh) that moves nothing in the
account store, so seat A's repaint arrives through the store notice after seat
B's next pass — not through the link's own reply. The link test therefore
settles both stores first, so a fresh sign-in's own rows (enrollment, device
endpoints) cannot be what re-drives the page. Measured 2026-11-07 on tui: green
alone, green settled, green right after a grant on the same open page; one red
in a 25-minute run on a loaded box (the repaint not inside 240 s, a re-visit
listing the pairing), not reproduced since.

tier_3: a real nest, two real seats, the real account runtimes. Every wait is a
deadline poll on painted state with both seats' pumps poked (convention 14);
the failure message names each seat's pump role and what a re-visit would show,
so a missing repaint reads apart from a change that never landed.
"""
from __future__ import annotations

import uuid

import pytest

from helpers.fleet import SiblingSeats, poked_pass
from helpers.waiting import account_pump_role, wait_until

pytestmark = [
    pytest.mark.tier_3, pytest.mark.tui, pytest.mark.web, pytest.mark.linux,
    pytest.mark.android, pytest.mark.macos, pytest.mark.ios,
]

#: A seat's own gesture painting on its own page: a WS-RPC round trip and a fold.
_UI_S = 30.0

#: Seat B's publish, the nest's relay, seat A's walk, then the notice and the
#: open page's re-hydrate — pump passes, poked on both seats; the floor's own
#: cadence (10 s) where a co-located process holds a seat's pump.
_CONVERGE_S = 240.0

#: Poked passes per seat before the link (see the link test).
_SETTLE_PASSES = 3

#: A syntactically valid nest identity (`test_linked_nests.py`'s shape): the
#: nest's `fauna.pair.add` stores it without resolving a live nest.
_LINKED_NEST_ID = "ab" * 32


@pytest.fixture
def repaint_nest(request, nest_mode, tmp_path_factory):
    """A dedicated nest — own admin, own grant table, own pairing list, so every
    count below is this test's own."""
    from conftest import _start_dedicated_nest

    nest, cleanup = _start_dedicated_nest(
        request, nest_mode, tmp_path_factory, "nests-sibling-repaint")
    yield nest
    cleanup()


@pytest.fixture
def sibling_seats(request, repaint_nest):
    """Second enrolled seats on the dedicated nest (`helpers.fleet.SiblingSeats`)."""
    seats = SiblingSeats(request, repaint_nest)
    try:
        yield seats
    finally:
        seats.teardown()


def _admin_user(nest: dict) -> dict:
    """The nest's admin as a `_login_app_as` user dict, under its real handle."""
    return {**nest["admin"], "handle": "admin"}


def _create_tier(seat) -> str:
    """A tier on seat B's Tiers tab — what makes a paywalled-posts mint option
    derive (`test_nest_trust_grants.py`'s setup step)."""
    tier = f"gold-{uuid.uuid4().hex[:6]}"
    subs = seat.subscriptions
    subs.navigate()
    subs.open_tiers_tab()
    subs.create_tier(tier, rank=1, price_hint="$5/mo")
    assert subs.wait_for_tier(tier), (
        f"seat B's tier {tier!r} never appeared; error={subs.error_text()!r}"
    )
    return tier


def _converge(app, seat_b, painted, *, what: str, revisit) -> None:
    """Poke both seats' pumps until ``painted()`` holds on seat A's OPEN page."""

    def a_shows_it() -> bool:
        poked_pass(seat_b.driver, what="seat B's publish")
        poked_pass(app.driver, what="seat A's walk")
        try:
            return painted()
        except LookupError:
            return False  # a row the list rebuilt away between count and read

    wait_until(
        a_shows_it, _CONVERGE_S, interval=1.0,
        diagnose=lambda: (
            f"seat A's OPEN Nests page never painted {what} without a re-visit; "
            f"seat A's pump role (runtime, holder) = {account_pump_role(app.driver)}, "
            f"seat B's = {account_pump_role(seat_b.driver)}; a re-visit now reads "
            f"{revisit()} (if it shows the change, the change landed and the notice "
            f"never re-drove the open page); error-message {app.error_text()!r}"
        ),
    )


def _revisit_counts(app) -> str:
    """Leave and re-enter the Nests page; what a fresh visit shows. Diagnosis
    only — called once the no-re-visit wait has already failed."""
    try:
        # Through the feed, so the visit is a real nav edge (`MutedWords.revisit`).
        app.driver.set_state({"nav": {"stack": [{"view": "feed"}]}})
        app.linked_nests.navigate()
        return (f"grants {app.nest_trust.grant_count()}, "
                f"pairings {app.linked_nests.pairing_count()}")
    except Exception as e:  # noqa: BLE001 — diagnostics must not mask the assert
        return f"(re-visit unreadable: {type(e).__name__}: {e})"


def _two_seats_on_an_open_nests_page(app, request, nest, sibling_seats):
    """Both seats signed in as the nest's admin and enrolled; seat A on its
    Nests page, entered ONCE, with nothing listed yet. Returns seat B."""
    from helpers import enrollment

    admin = _admin_user(nest)
    enrollment.sign_in(app, request, nest, admin)
    enrollment.await_enrollment(app, nest["url"], admin)
    seat_b = sibling_seats.launch(user=admin)
    enrollment.await_enrollment(seat_b, nest["url"], admin)

    app.linked_nests.navigate()
    assert app.linked_nests.is_page_visible(), (
        f"seat A's Nests page not reachable; error {app.error_text()!r}"
    )
    grants_before = app.nest_trust.grant_count()
    pairings_before = app.linked_nests.pairing_count()
    assert grants_before == 0 and pairings_before == 0, (
        f"a dedicated nest's admin starts with no grants and no pairings "
        f"(grants {grants_before}, pairings {pairings_before})"
    )
    return seat_b


@pytest.mark.feature("nests-and-trust")
def test_a_grant_minted_on_a_sibling_paints_on_an_open_nests_page(
    app, request, repaint_nest, sibling_seats
):
    seat_b = _two_seats_on_an_open_nests_page(app, request, repaint_nest, sibling_seats)
    tier = _create_tier(seat_b)  # on seat B's Tiers tab; seat A stays on Nests

    # ── Seat B mints a grant through its own Nests page.
    seat_b.linked_nests.navigate()
    assert seat_b.nest_trust.mint_button_present(), (
        f"seat B's mint button missing despite a holder and a held tier; "
        f"error {seat_b.error_text()!r}"
    )
    seat_b.nest_trust.open_mint()
    seat_b.nest_trust.select_scope(seat_b.nest_trust.paywalled_label(tier))
    seat_b.nest_trust.pick_standard_duration_where_offered()
    seat_b.nest_trust.confirm_mint()
    assert seat_b.nest_trust.wait_for_grant_count(1, timeout=_UI_S), (
        f"seat B's minted grant never rendered on its own page; "
        f"error {seat_b.error_text()!r}"
    )

    _converge(
        app, seat_b, lambda: app.nest_trust.grant_count() >= 1,
        what="the grant seat B minted", revisit=lambda: _revisit_counts(app),
    )


@pytest.mark.feature("nests-and-trust")
def test_a_nest_linked_on_a_sibling_paints_on_an_open_nests_page(
    app, request, repaint_nest, sibling_seats
):
    seat_b = _two_seats_on_an_open_nests_page(app, request, repaint_nest, sibling_seats)
    # Settle both stores first: a fresh sign-in's own rows (enrollment, device
    # endpoints) re-drive the open page for a while, and the link must be the
    # only change the page could be hearing.
    for i in range(_SETTLE_PASSES):
        poked_pass(seat_b.driver, what=f"seat B's settling pass {i + 1}")
        poked_pass(app.driver, what=f"seat A's settling pass {i + 1}")

    # ── Seat B links a nest through its own Nests page.
    seat_b.linked_nests.navigate()
    seat_b.linked_nests.link(_LINKED_NEST_ID)
    assert seat_b.linked_nests.wait_for_pairing_count(1, timeout=_UI_S), (
        f"seat B's link never listed on its own page; "
        f"error {seat_b.linked_nests.page_error_text()!r}"
    )

    _converge(
        app, seat_b, lambda: app.linked_nests.pairing_count() >= 1,
        what="the nest seat B linked", revisit=lambda: _revisit_counts(app),
    )
