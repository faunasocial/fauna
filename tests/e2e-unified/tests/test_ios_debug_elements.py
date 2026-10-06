"""Debug test: dump visible elements after import-identity-button click."""
import time
import pytest

from helpers.app_surface import declared_absence

# iOS-only diagnostic (deselected under `--client {linux,web,…}` by the conftest
# marker-platform filter). The in-body `is_ios()` skip still guards a no-`--client`
# run where the `app` fixture parametrizes over every available client.
pytestmark = [pytest.mark.ios, pytest.mark.tier_2]


def test_dump_after_signin_click(app):
    if not app.driver.is_ios():
        declared_absence(
            app.driver,
            capability="the iOS-specific post-signin element dump diagnostic",
            doc="testing.md § Cross-app e2e conventions, point 7 (marker-"
            "scoped file — pytestmark already deselects non-iOS apps; this "
            "in-body guard only matters for a no---app run)",
        )

    # Wait for identity choice screen
    app.driver.wait_for("import-identity-button", timeout=20)
    app.driver.click("import-identity-button")

    # Wait for sheet animation
    time.sleep(2)

    # Check specific elements via bridge API (new onboarding flow)
    for eid in ["paste-secret-field", "import-submit-button", "qr-camera-view"]:
        visible = app.driver.is_visible(eid)
        print(f"  {eid}: {'VISIBLE' if visible else 'NOT FOUND'}")
