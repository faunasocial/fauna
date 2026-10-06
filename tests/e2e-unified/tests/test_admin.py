import time

import pytest

pytestmark = [pytest.mark.tier2, pytest.mark.tier_3]


@pytest.mark.feature("admin-tiers")
def test_settings_tier_definitions(admin_app):
    """admin-settings shows the tier *definitions* (admin.md § 3 Settings).

    The nest seeds free/personal/community tiers; the settings page renders them
    as indexed `admin-settings-tier-item` rows under the
    `admin-settings-tiers-section` anchor (driven by `fauna.admin.tiers.list`).
    """
    admin_app.admin.navigate_settings()
    assert admin_app.admin.tiers_section_visible(), (
        f"tiers section missing. error: {admin_app.error_text()!r}"
    )
    # The tier list is fetched async on the admin-status check; poll for rows.
    deadline = time.monotonic() + 15.0
    while time.monotonic() < deadline and admin_app.admin.tier_definition_count() < 1:
        time.sleep(0.3)
    assert admin_app.admin.tier_definition_count() >= 1, (
        f"no tier definitions rendered. error: {admin_app.error_text()!r}"
    )


@pytest.mark.feature("admin-tiers")
def test_settings_tier_edit(admin_app):
    """admin can edit a tier definition's caps in place (admin.md § 3 Settings).

    Each indexed `admin-settings-tier-item` row carries editable raw-i64 cap
    inputs (`admin-settings-tier-cap-*`) + an `admin-settings-tier-save-button`.
    Saving fires `fauna.admin.tiers.update`; the page then refetches `tiers.list`,
    so reading the input back after save proves the new value persisted in the
    nest (not merely echoed in the widget).
    """
    admin_app.admin.navigate_settings()
    # Tier definitions load async on the admin-status check; poll for rows.
    deadline = time.monotonic() + 15.0
    while time.monotonic() < deadline and admin_app.admin.tier_definition_count() < 1:
        time.sleep(0.3)
    assert admin_app.admin.tier_definition_count() >= 1, (
        f"no tier definitions rendered. error: {admin_app.error_text()!r}"
    )

    # Pick a new inbox-bytes cap distinct from whatever the row currently holds.
    current = admin_app.admin.tier_cap_value("inbox", index=0)
    new_value = "424242" if current.strip() != "424242" else "131313"
    admin_app.admin.edit_tier_cap("inbox", new_value, index=0)
    admin_app.admin.save_tier(index=0)

    # The save refetches tiers.list and re-renders the row from persisted state.
    deadline = time.monotonic() + 15.0
    while time.monotonic() < deadline:
        if admin_app.admin.tier_cap_value("inbox", index=0).strip() == new_value:
            break
        time.sleep(0.3)
    assert admin_app.admin.tier_cap_value("inbox", index=0).strip() == new_value, (
        "tier inbox cap did not persist after save+refetch "
        f"(got {admin_app.admin.tier_cap_value('inbox', index=0)!r}). "
        f"error: {admin_app.error_text()!r}"
    )


def _wait_for_tier_rows(admin_app, at_least: int = 1) -> None:
    """Tier definitions load async on the admin-status check: poll for rows."""
    deadline = time.monotonic() + 15.0
    while time.monotonic() < deadline and admin_app.admin.tier_definition_count() < at_least:
        time.sleep(0.3)
    assert admin_app.admin.tier_definition_count() >= at_least, (
        f"tier definitions did not load. error: {admin_app.error_text()!r}"
    )


@pytest.mark.feature("admin-tiers")
def test_settings_tier_define_new(admin_app):
    """An admin defines a NEW tier from the Tiers page (admin.md § 3 — *Defining
    a new tier*): name + the five raw-i64 caps in the add form, then
    `admin-settings-tier-add-button` fires `fauna.admin.tiers.create`.

    The page refetches `tiers.list`, so the new tier showing up as an ordinary
    row — name and every cap read back — proves it persisted in the nest, not
    merely that the form echoed the typed values.
    """
    admin_app.admin.navigate_settings()
    _wait_for_tier_rows(admin_app)
    assert admin_app.admin.tier_add_section_visible(), (
        f"add-a-tier form missing. error: {admin_app.error_text()!r}"
    )
    before = admin_app.admin.tier_definition_count()

    caps = {
        "inbox": "111000", "storage": "222000", "devices": "64",
        "blob-size": "333000", "feeds": "1000",
    }
    admin_app.admin.edit_new_tier_name("e2e-harness")
    for cap, value in caps.items():
        admin_app.admin.edit_new_tier_cap(cap, value)
    admin_app.admin.add_tier()

    deadline = time.monotonic() + 15.0
    while time.monotonic() < deadline and admin_app.admin.tier_definition_count() <= before:
        time.sleep(0.3)
    assert admin_app.admin.tier_definition_count() == before + 1, (
        f"the new tier did not appear after create+refetch. "
        f"rows: {admin_app.admin.tier_names()!r}. error: {admin_app.error_text()!r}"
    )
    index = admin_app.admin.tier_names().index("e2e-harness")
    for cap, value in caps.items():
        assert admin_app.admin.tier_cap_value(cap, index=index).strip() == value, (
            f"cap {cap!r} of the new tier did not persist"
        )


@pytest.mark.feature("admin-tiers")
def test_settings_tier_define_refuses_empty_and_duplicate_names(admin_app):
    """An empty name is refused locally and a taken name by the nest
    (`fauna.admin.conflict`); both surface on `error-message` and neither adds a
    row — a refusal the admin can read, never a silent no-op."""
    admin_app.admin.navigate_settings()
    _wait_for_tier_rows(admin_app)
    before = admin_app.admin.tier_definition_count()
    existing = admin_app.admin.tier_name(index=0)

    for cap, value in {"inbox": "1", "storage": "1", "devices": "1",
                       "blob-size": "1", "feeds": "1"}.items():
        admin_app.admin.edit_new_tier_cap(cap, value)

    admin_app.admin.edit_new_tier_name("")
    admin_app.admin.add_tier()
    empty_refusal = admin_app.error_text().strip()
    assert empty_refusal, "an empty tier name was refused silently"

    admin_app.admin.edit_new_tier_name(existing)
    admin_app.admin.add_tier()
    # The nest's refusal replaces the local one: wait for the text to change.
    deadline = time.monotonic() + 15.0
    while time.monotonic() < deadline and admin_app.error_text().strip() == empty_refusal:
        time.sleep(0.3)
    assert admin_app.error_text().strip() != empty_refusal, (
        "a taken tier name was refused silently (or with the empty-name text)"
    )
    assert admin_app.admin.tier_definition_count() == before


@pytest.mark.feature("admin-dashboard")
def test_admin_dashboard_loads(admin_app):
    """Admin dashboard shows stat cards after claiming admin."""
    admin_app.admin.navigate_dashboard()
    assert admin_app.admin.is_dashboard_visible()


@pytest.mark.feature("admin-dashboard")
def test_admin_dashboard_user_count(admin_app):
    """Dashboard shows at least 1 user (the admin)."""
    admin_app.admin.navigate_dashboard()
    value = admin_app.admin.dashboard_card_value("Users")
    assert int(value) >= 1


@pytest.mark.feature("admin-dashboard")
def test_admin_dashboard_version(admin_app):
    """Dashboard shows a non-empty version string."""
    admin_app.admin.navigate_dashboard()
    value = admin_app.admin.dashboard_card_value("Version")
    assert len(value) > 0


@pytest.mark.feature("admin-users")
def test_admin_user_list(admin_app, nest_instance):
    """Admin user list shows registered users."""
    admin_app.admin.navigate_users()
    assert admin_app.admin.user_count() >= 1


@pytest.mark.feature("admin-users")
def test_admin_invite_codes(admin_app):
    """Admin can mint invite codes from the consolidated admin-users Invite section."""
    admin_app.admin.navigate_invite_codes()
    initial = admin_app.admin.invite_code_count()
    code = admin_app.admin.create_invite_code()
    assert len(code) > 0
    assert admin_app.admin.invite_code_count() == initial + 1
    # The freshly minted token is surfaced copyable (admin.md § 2 — mint-on-empty
    # returns AdminInviteCodeCreateReply.code, shown via the copy button).
    assert admin_app.admin.copy_button_visible()
    admin_app.admin.copy_minted_code()  # tree-safe: must not raise
