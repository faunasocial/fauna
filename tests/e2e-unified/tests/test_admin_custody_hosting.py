"""Admin Held-Custody registry (`admin-custody-hosting`) — smoke coverage,
distinct from the full ceremony.

docs/goal/architecture/account-data-plane.md § Two-sided bounds, finding
piece 3.

`test_custody_ceremony_journey.py`'s `test_custody_ceremony_nest_anchored`
covers the full read + remove flow against a REAL planted hosting row (it
needs a two-account MLS ceremony to plant one — heavy, and today driven
through the tui UI only). This file covers the cheap, always-available half
every lift app owes the moment its page lands: the page is reachable, the
read distinguishes "not yet answered" from "answered, empty" (the honesty
rule tui/linux/web all pin with unit tests — see e.g.
`apps/fauna-tui/src/admin/custody_hosting.rs`'s
`an_unhydrated_page_is_not_an_empty_one`), and a fresh nest genuinely has
nothing to show.

tier_3 (full stack): a real `fauna-nest` binary + a real admin client over
WS-RPC — no planted data needed, since a fresh nest's registry is empty by
construction.
"""

from __future__ import annotations

import pytest

from helpers.waiting import wait_until

pytestmark = [pytest.mark.tier2, pytest.mark.tier_3]


@pytest.mark.feature("admin-held-custody")
def test_admin_custody_hosting_page_reachable_and_honestly_empty(admin_app):
    """A fresh nest's registry is empty, and the page says so ONLY once the
    read has answered — never before. Pre-hydrate must show neither a count
    nor the empty state; only an ANSWERED empty list says "nobody asked".

    All 7 apps now own this leg (windows landed last) — the per-app skip_unbuilt gate this test carried is retired."""
    app = admin_app

    app.admin.navigate_custody_hosting()
    wait_until(
        lambda: app.admin.hosting_registry_answered(),
        15.0,
        diagnose=lambda: f"error: {app.error_text()!r}",
    )

    assert app.admin.hosting_row_count() == 0, (
        "a fresh nest has no hosting rows — genuine planted-row coverage "
        "lives in test_custody_ceremony_journey.py's nest-anchored ceremony"
    )
    assert not app.error_text(), f"unexpected error: {app.error_text()!r}"
