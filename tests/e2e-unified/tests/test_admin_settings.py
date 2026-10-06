"""Tests for the admin settings page."""
import time
import uuid

import pytest

from actions.api_actor import ApiActor

pytestmark = [pytest.mark.tier2, pytest.mark.tier_3]

# Budget for polling a membership-tier refetch after a WS-RPC round trip
# (seed / designate / clear). 15s (the previous width) went red under a 7-deep
# build-slot queue (linux app-gate drain) but the same fetch
# lands in well under a second once isolated — widened per convention 14
# rather than re-run-to-green.
_TIER_REFRESH_BUDGET_S = 30.0


@pytest.mark.feature("admin-tiers")
def test_admin_settings_loads(admin_app):
    """Admin settings page renders after navigation."""
    admin_app.admin.navigate_settings()
    admin_app.driver.wait_for("admin-settings-heading", timeout=15.0)
    assert admin_app.driver.is_visible("admin-settings-heading"), (
        "admin settings page should render its heading after navigate: "
        f"{admin_app.driver.diagnose('admin-settings-heading')} "
        f"error={admin_app.error_text()!r}"
    )


@pytest.mark.linux
@pytest.mark.web
@pytest.mark.tui
@pytest.mark.macos
@pytest.mark.ios
@pytest.mark.windows
@pytest.mark.android
@pytest.mark.feature("admin-tiers")
def test_admin_designates_a_membership_tier_re_points_it_and_clears_it(
    admin_app, nest_instance,
):
    """The membership designation section (monetization.md § Pillar 4): a link
    editor over the admin's own subscription tiers, driven through the client
    UI (e2e rule 8), never a raw RPC call.

    Precondition setup only (rule 8's fixture-setup carve-out): the admin's own
    subscription tier is created via the WS-RPC `subscription_create_tier` —
    the mechanism under test is the ADMIN-side designation UI, not subscription
    tier creation itself (a separate, already-proven surface, `test_subscriptions.py`).

    Full round-trip: the row renders for an undesignated owned tier (default
    lapse = the shared `free` constant, never hard-coded) → designate via the
    two quota-tier selects + Save (`fauna.admin.membership_tiers.set`, an
    upsert) → the persisted values survive a refetch → Clear
    (`fauna.admin.membership_tiers.clear`) reverts the row to undesignated
    (lapse back to `free`) while the row itself — and the underlying
    subscription tier — survives (designating/clearing mutates neither tier
    system, monetization.md § Pillar 4).

    android: built with exact ID matches (`AdminSettingsScreen.kt`'s
    `MembershipSection`/`MembershipRow`/`MembershipTierPicker`) and
    Compose-content-tested (`AdminSettingsContentTest.kt`); the tier_3 run
    stays host-emulator-gated like every other android e2e test.
    """
    admin = nest_instance["admin"]
    tier_name = f"paid-{uuid.uuid4().hex[:8]}"
    ApiActor(
        nest_instance["url"], admin["token"], admin["actor_id_hex"],
        bytes(admin["signing_key"]),
    ).subscription_create_tier(tier_name, rank=1, price_hint="$5/mo")

    admin_app.admin.navigate_settings()
    admin_app.driver.wait_for("admin-settings-membership-section", timeout=15.0)
    assert admin_app.admin.membership_section_visible(), (
        "the membership section must render on admin-settings after a "
        f"subscription tier exists. error={admin_app.error_text()!r}"
    )

    def _row_index() -> int:
        count = admin_app.admin.membership_row_count()
        for i in range(count):
            if admin_app.admin.membership_tier_name(index=i) == tier_name:
                return i
        raise AssertionError(
            f"no membership row for {tier_name!r} among {count} rows; "
            f"error={admin_app.error_text()!r}"
        )

    def _row_present() -> bool:
        count = admin_app.admin.membership_row_count()
        return any(
            admin_app.admin.membership_tier_name(index=i) == tier_name
            for i in range(count)
        )

    # The section container renders unconditionally at page-build time (its
    # ListBox shows a "Loading membership tiers..." placeholder while empty),
    # so `wait_for("admin-settings-membership-section")` above only proves the
    # static shell landed, not that the async subscription-tier fetch has —
    # poll for the seeded row itself before the first `_row_index()` call.
    deadline = time.monotonic() + _TIER_REFRESH_BUDGET_S
    while time.monotonic() < deadline:
        if _row_present():
            break
        time.sleep(0.5)

    idx = _row_index()
    assert admin_app.admin.membership_lapse_tier(index=idx) == "free", (
        "an undesignated row must default its lapse-tier select to the shared "
        f"DEFAULT_LAPSE_TIER ('free'), got "
        f"{admin_app.admin.membership_lapse_tier(index=idx)!r}"
    )

    # ── Designate: admit at "personal", lapse to "community" ──────────────
    admin_app.admin.set_membership_admin_tier("personal", index=idx)
    admin_app.admin.set_membership_lapse_tier("community", index=idx)
    admin_app.admin.save_membership_tier(index=idx)
    admin_app.driver.wait_for("admin-settings-membership-section", timeout=15.0)

    def _designated() -> bool:
        i = _row_index()
        return (
            admin_app.admin.membership_admin_tier(index=i) == "personal"
            and admin_app.admin.membership_lapse_tier(index=i) == "community"
        )

    deadline = time.monotonic() + _TIER_REFRESH_BUDGET_S
    while time.monotonic() < deadline:
        if _designated():
            break
        time.sleep(0.5)
    assert _designated(), (
        "the designation must persist through a refetch: expected admin_tier="
        f"'personal' lapse_tier='community', got admin_tier="
        f"{admin_app.admin.membership_admin_tier(index=_row_index())!r} lapse_tier="
        f"{admin_app.admin.membership_lapse_tier(index=_row_index())!r}. "
        f"error={admin_app.error_text()!r}"
    )

    # ── Clear: the designation drops, the row (and its subscription tier)
    # survives, undesignated ────────────────────────────────────────────────
    idx = _row_index()
    admin_app.admin.clear_membership_tier(index=idx)
    admin_app.driver.wait_for("admin-settings-membership-section", timeout=15.0)

    def _cleared() -> bool:
        i = _row_index()  # raises if the row itself vanished
        return admin_app.admin.membership_lapse_tier(index=i) == "free"

    deadline = time.monotonic() + _TIER_REFRESH_BUDGET_S
    while time.monotonic() < deadline:
        if _cleared():
            break
        time.sleep(0.5)
    assert _cleared(), (
        "clearing must revert the row to undesignated (lapse back to 'free') "
        f"without removing the row itself; error={admin_app.error_text()!r}"
    )
