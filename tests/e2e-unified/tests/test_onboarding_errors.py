"""Tests that onboarding error messages use i18n strings, not raw exceptions.

Each test triggers a specific validation or connection error during the
unified 2-step onboarding and asserts the exact user-facing message text
from the i18n resource files. These strings are shared across all apps.

Run with: pytest tests/test_onboarding_errors.py -v --client ios
"""

import time
import pytest

from i18n.strings import S

pytestmark = [pytest.mark.tier1, pytest.mark.tier_3]


def _ensure_secret_field(app) -> bool:
    """Navigate to the import identity screen (paste-secret-field visible).

    Resets the app first to ensure we start from the identity choice screen.
    All native apps use the same unified 2-step onboarding — and so does **web**
    since 2026-07-30: this helper used to bail out with `if is_web(): return False`
    ("Web uses state protocol, not UI onboarding"), which turned into a bare
    `pytest.skip` in the caller — a SECOND, undeclared skip sitting behind the
    caller's declared one, invisible in a summary line. Live-verified false: web
    builds `create-identity-button` / `import-identity-button` / `paste-secret-field`
    / `import-submit-button` and this walk reaches the field on web.
    """
    app.driver.reset()
    time.sleep(1)
    deadline = time.monotonic() + 15
    while time.monotonic() < deadline:
        if app.driver.is_visible("paste-secret-field"):
            return True
        if app.driver.is_visible("import-identity-button"):
            app.driver.click("import-identity-button")
            time.sleep(1)
            continue
        if app.driver.is_visible("create-identity-button"):
            # On identity choice screen — click import
            app.driver.click("import-identity-button") if app.driver.is_visible("import-identity-button") else None
            time.sleep(1)
            continue
        if app.driver.is_visible("compose-button"):
            return False  # Authenticated
        time.sleep(0.5)
    return False


def _read_error(app, timeout=15) -> str:
    """Wait for an error message to appear and return it.

    Reads from the state protocol (messages.error) first, falls back
    to the error-message UI element.
    """
    deadline = time.monotonic() + timeout
    while time.monotonic() < deadline:
        if app.has_error():
            return app.error_text()
        time.sleep(0.5)
    return ""


@pytest.mark.feature("create-identity")
def test_invalid_secret_shows_i18n_error(app, nest_instance):
    """Entering a secret key that isn't 64 hex chars should show validation error."""
    if not _ensure_secret_field(app):
        pytest.skip("Could not reach import identity screen")
    app.driver.clear_and_type("paste-secret-field", "too-short")
    app.driver.click("import-submit-button")

    error = _read_error(app)
    assert error, "Expected a validation error for invalid secret key"
    # All native apps route the import field through the shared
    # `parse_identity_import` parser (onboarding.md §1.identity_import), so a
    # non-hex secret surfaces the parse-time `invalid_secret` string — never the
    # machine's `errors.secret_key_invalid`. (linux calls
    # `fauna_core::identity_qr::parse_import_input` directly; the rest go through
    # the UniFFI/WASM face.)
    assert error == S.onboarding.identity_import.invalid_secret, (
        f"Expected '{S.onboarding.identity_import.invalid_secret}', got '{error}'"
    )
