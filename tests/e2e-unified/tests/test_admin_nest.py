"""admin-nest page (admin.md § N Nest).

The Nest page is the home for nest-wide admin settings that aren't a feature
page — introduced by the per-page-services redesign (2026-06-04, admin.md
§ Admin IA redesign), which removed the standalone admin-services page. It
carries the operator pairing toggle (the one live service flag that moved off
Services — gates `fauna.pair.add`) and the Factory Reset danger zone (moved
off Settings). The bridge/dns/algorithm toggles were dropped (bridge
vestigial; algorithm left the wire with its sidecar; dns is the
admin-dns master switch; mail on/off is `admin-mail-enabled-toggle` on
admin-mail). The read-only storage-mode indicator that used to live here
(`nest-mode-indicator`) is RETIRED (no-modes retirement, ratified
2026-07-12) — every nest is sealed at rest unconditionally now, so there is
no deployment-wide storage mode left to display.

linux leads; web/windows/macos/ios/android lift admin-nest via the route-3
hand-offs (NEXT-<client>). Until a client lifts it, admin-nest doesn't exist
there — its pairing controls still live on admin-settings / admin-services
(covered cross-app by test_admin.py / test_linked_nests.py via the
transition-tolerant navigate_to_* helpers). So this test, which asserts the
*unified* page, skips on a client that hasn't lifted it yet (the skip names the
owed hand-off — it is not a platform-difference skip, it is a transitional
migration gap that each app's route-3 NEXT closes).

This replaces the retired test_admin_services.py (the admin-services page it
tested no longer exists).
"""
import time

import pytest

pytestmark = [pytest.mark.tier2, pytest.mark.tier_3]


def _require_nest_page(admin_app) -> None:
    """Navigate to admin-nest; skip if this client hasn't lifted it yet."""
    admin_app.admin.navigate_nest()
    deadline = time.monotonic() + 15.0
    while time.monotonic() < deadline and admin_app.driver.count("admin-nest-heading") == 0:
        time.sleep(0.3)
    if admin_app.driver.count("admin-nest-heading") == 0:
        pytest.skip(
            "admin-nest not yet lifted on this client (per-page-services redesign "
            "route-3 hand-off — NEXT-{web,windows,macos,ios,android}); its controls "
            "still render on admin-settings / admin-services here"
        )


@pytest.mark.feature("admin-nest")
def test_nest_page_renders(admin_app):
    """The Nest page renders its heading + the two nest-wide controls: the
    operator pairing toggle and the Factory Reset button."""
    _require_nest_page(admin_app)

    # Operator pairing toggle (moved off the removed Services page).
    assert admin_app.admin.service_toggle_present("pairing"), (
        f"admin-service-pairing-toggle missing on admin-nest. error: {admin_app.error_text()!r}"
    )

    # Factory Reset danger zone (moved off Settings).
    assert admin_app.driver.count("admin-factory-reset-button") > 0, (
        f"admin-factory-reset-button missing on admin-nest. error: {admin_app.error_text()!r}"
    )


@pytest.mark.feature("admin-nest")
def test_nest_pairing_toggle_flips(admin_app):
    """Flipping the operator pairing toggle writes via fauna.admin.services.update
    (name="pairing") and the status badge reflects the new state after the
    refetch. Restores the prior (default-on) state so the shared session nest is
    left as found."""
    _require_nest_page(admin_app)

    deadline = time.monotonic() + 15.0
    while time.monotonic() < deadline and not admin_app.admin.service_status_text("pairing").strip():
        time.sleep(0.3)
    before = admin_app.admin.service_status_text("pairing").strip()
    assert before, f"pairing status empty. error: {admin_app.error_text()!r}"

    admin_app.admin.toggle_service("pairing")
    try:
        deadline = time.monotonic() + 15.0
        while (
            time.monotonic() < deadline
            and admin_app.admin.service_status_text("pairing").strip() == before
        ):
            time.sleep(0.3)
        after = admin_app.admin.service_status_text("pairing").strip()
        assert after != before, (
            f"pairing status did not change after toggle (still {before!r}). "
            f"error: {admin_app.error_text()!r}"
        )
    finally:
        # Restore the prior state for the shared session nest.
        if admin_app.admin.service_status_text("pairing").strip() != before:
            admin_app.admin.toggle_service("pairing")


@pytest.mark.feature("admin-nest")
def test_region_declaration_round_trips(admin_app):
    """The declared region — the region tier's one human control (admin.md § N
    Nest → Declared region; region-blocking.md § Region determination, ratified
    *declared, never detected*).

    Drives the whole loop through the app UI as an admin would: read the
    fresh-install state, declare a region, see it reflected with its authority
    line, then withdraw it and see the conditional members disappear. Restores
    the prior declaration so the shared session nest is left as found.

    The authority assertion is deliberately about PRESENCE, not wording: the
    curated registry enrols nobody today, so every deployment lands in the
    "no authority enrolled" arm — asserting the specific sentence would pin a
    state that changes the day an authority is enrolled, while the line's
    existence is the invariant (a declared region always owes one).
    """
    _require_nest_page(admin_app)

    before_status = admin_app.admin.region_status_text().strip()
    assert before_status, (
        f"region status empty — the undeclared state has words, it is not blank. "
        f"error: {admin_app.error_text()!r}"
    )
    had_declaration = admin_app.admin.withdraw_region_present()

    admin_app.admin.declare_region("NO")
    try:
        deadline = time.monotonic() + 15.0
        while (
            time.monotonic() < deadline
            and "NO" not in admin_app.admin.region_status_text()
        ):
            time.sleep(0.3)
        status = admin_app.admin.region_status_text()
        assert "NO" in status, (
            f"declared region not reflected after save (status {status!r}). "
            f"error: {admin_app.error_text()!r}"
        )
        # A declaration owes an authority line and a withdraw affordance.
        assert admin_app.admin.region_authority_text().strip(), (
            "a declared region must say whether an authority is enrolled — "
            "silence reads as a broken feature"
        )
        assert admin_app.admin.withdraw_region_present(), (
            "withdraw must be reachable once a region is declared"
        )

        # Withdrawing takes the conditional members away with it.
        admin_app.admin.withdraw_region()
        deadline = time.monotonic() + 15.0
        while time.monotonic() < deadline and admin_app.admin.withdraw_region_present():
            time.sleep(0.3)
        assert not admin_app.admin.withdraw_region_present(), (
            f"withdraw still present after withdrawing. error: {admin_app.error_text()!r}"
        )
        assert not admin_app.admin.region_authority_text().strip(), (
            "an undeclared deployment has no authority channel to describe"
        )
    finally:
        # Restore the prior declaration for the shared session nest, and wait for
        # it to actually land — a fire-and-forget restore can lose the race with
        # teardown and leave the nest declaring nothing.
        if had_declaration:
            restored = before_status.split()[-1]
            admin_app.admin.declare_region(restored)
            deadline = time.monotonic() + 15.0
            while (
                time.monotonic() < deadline
                and restored not in admin_app.admin.region_status_text()
            ):
                time.sleep(0.3)


@pytest.mark.feature("admin-nest")
def test_malformed_region_is_refused_client_side(admin_app):
    """A malformed code is refused onto the page's own error surface with no
    declaration — the serving-port shape. Lower case is malformed on purpose:
    RegionCode is never case-folded, because a helpfully-upcasing client would
    make two spellings of one region both storable and the nest's key ambiguous.
    """
    _require_nest_page(admin_app)

    before = admin_app.admin.region_status_text().strip()
    admin_app.admin.declare_region("no")

    # A generous deadline poll, not a settle sleep: the refusal is local (no
    # round trip), so a green run pays almost nothing, while the ceiling sits far
    # above any non-pathological repaint delay under load (convention 14).
    deadline = time.monotonic() + 15.0
    while time.monotonic() < deadline and not admin_app.has_error():
        time.sleep(0.3)

    assert admin_app.has_error(), (
        "a malformed region code must surface on error-message, not be sent"
    )
    # The refusal is client-side, so nothing was dispatched and the declaration
    # cannot have moved — asserted after the error is visible, which is the
    # causal barrier that makes this a real check rather than a race the wrong
    # way round (a too-early read would pass even if the write HAD gone out).
    assert admin_app.admin.region_status_text().strip() == before, (
        f"a refused code must not change the declaration (was {before!r}, "
        f"now {admin_app.admin.region_status_text().strip()!r})"
    )
