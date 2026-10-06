"""Test that the iOS app launches to the welcome screen after clean install.

This verifies the Appium driver correctly resets app state (including keychain)
so the onboarding flow starts fresh each time.
"""
import pytest

from helpers.app_surface import declared_absence


pytestmark = [pytest.mark.ios, pytest.mark.tier_2]


def test_app_launches_to_welcome(app):
    """After clean install, the app should show the identity choice screen."""
    if not app.driver.is_ios():
        declared_absence(
            app.driver,
            capability="the iOS-specific app-launch smoke probe",
            doc="testing.md § Cross-app e2e conventions, point 7 (marker-"
            "scoped file — pytestmark already deselects non-iOS apps; this "
            "in-body guard only matters for a no---app run)",
        )

    # The identity choice screen should be visible with create/import buttons
    assert app.driver.is_visible("create-identity-button") or app.driver.is_visible("import-identity-button"), (
        "create-identity-button/import-identity-button not visible — app may have "
        "persisted keychain credentials from a previous run. The driver must "
        "uninstall the app (simctl uninstall) before reinstalling to clear keychain."
    )
