"""Launch fauna-tui on its HEADLESS credential store — the sealed, passphrase-
unlocked file arm (``docs/goal/architecture/apps/tui.md`` § Credential storage)
— the one launcher the two sealed-store journeys share
(``test_tui_headless_credential_store.py``, the create/relaunch/unlock arc, and
``test_tui_credential_store_rekey.py``, the change-passphrase arc).

The driver's ``headless_store`` launch mode empties ``FAUNA_E2E_CREDENTIAL_DIR``
(so the e2e file backend is OFF — these journeys run the REAL sealed store) and
sets the test-only ``FAUNA_E2E_FORCE_HEADLESS_STORE`` so a desktop dev box's live
Secret Service cannot shadow the arm under test. The XDG base is the caller's,
because the sealed file — ``$XDG_CONFIG_HOME/fauna-tui/<app>.sealed`` — must
survive a teardown + relaunch for the unlock stages to mean anything.

A driver factory in ``scripts/features_scan.py``'s sense: a test module that
imports ``headless_launch`` drives an app, so it is registered in
``DRIVER_FACTORY_IMPORTS``. Until 2026-09-26 the re-key module obtained its
driver through the headless-store TEST module's private helper — a door the
lint's static closure (conftest + the module under scan) cannot see, which is
why rule 4 read the re-key journey as launching no app and no page could cite
it.
"""
from __future__ import annotations

import contextlib
import json

from actions.onboarding import OnboardingActions
from common.nest import CLAIM_CODE
from drivers import create_driver

# Element IDs (tests/e2e-unified/ui.yaml § tui-unlock + onboarding).
PASSPHRASE_INPUT = "tui-unlock-passphrase-input"
CONFIRM_INPUT = "tui-unlock-confirm-input"
SUBMIT = "tui-unlock-submit-button"
CREATE_IDENTITY = "create-identity-button"


@contextlib.contextmanager
def headless_launch(app_path: str, nest: dict, xdg_base, request):
    """One isolated headless-store launch: sealed backend forced, XDG base
    pinned by the caller (so the sealed file — which lives under
    ``$XDG_CONFIG_HOME/fauna-tui`` — survives a teardown + relaunch).

    Yields ``(driver, config)``; ``config`` is the launch dict the driver was
    given, which a relaunch may hand back to ``driver.launch`` as is."""
    from conftest import _seeded_environment

    driver = create_driver("tui")
    config = {
        "app_path": app_path,
        "url": nest["url"],
        "headless_store": True,
        "xdg_base": str(xdg_base),
        # A stable namespace: the sealed file is {config_dir}/{app}.sealed, so
        # a port-derived default would dodge the relaunch-unlock stage.
        "keyring_app": "fauna-tui-headless-e2e",
        "environment": _seeded_environment(request, nest),
    }
    driver.launch(config)
    try:
        yield driver, config
    finally:
        with contextlib.suppress(Exception):
            driver.teardown()


def submit_passphrase(driver, passphrase: str, confirm: str | None = None):
    """Fill the unlock surface — CREATE mode when ``confirm`` is given — and
    submit it."""
    driver.clear_and_type(PASSPHRASE_INPUT, passphrase)
    if confirm is not None:
        driver.clear_and_type(CONFIRM_INPUT, confirm)
    driver.click(SUBMIT)


def claim_admin_through_wizard(driver, nest_url: str, admin_secret_hex: str):
    """The real UI claim of a fresh nest (the crash-journeys module's claim
    front-half): import identity → claim-code page for the known nest → submit
    the fixture nest's claim code → nat-mode seed → authenticated shell. Both
    sealed-store journeys need a store that HOLDS a signed-in identity, and
    every persisted slot along this path goes through the sealed backend."""
    driver.click("import-identity-button")
    driver.wait_for("paste-secret-field", timeout=15)
    driver.clear_and_type("paste-secret-field", admin_secret_hex)
    driver.click("import-submit-button")
    driver.wait_for("handle-input", timeout=15)
    driver.call_machine_method(
        "navigate_to_claim_code_for_known_nest",
        json.dumps([nest_url, "admin@localhost"]),
    )
    driver.wait_for("claim-code-input", timeout=20)
    driver.clear_and_type("claim-code-input", CLAIM_CODE)
    driver.click("claim-code-submit-button")
    # A successful admin claim routes to nat_mode_choice
    # (onboarding.md § 3b-bis); accept the pre-selected seed.
    driver.wait_for("nat-mode-confirm-button", timeout=20)
    OnboardingActions(driver).finish_nat_mode()
    state = driver.wait_for_state(
        lambda s: bool(s.get("session", {}).get("authenticated")), timeout=40
    )
    assert state["session"]["authenticated"] is True, (
        f"the UI claim must land the authenticated shell, got "
        f"session={state.get('session')!r} error={driver.get_text('error-message')!r}"
    )
