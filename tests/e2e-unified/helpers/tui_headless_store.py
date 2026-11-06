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
import time
from pathlib import Path

from actions.onboarding import OnboardingActions
from drivers import create_driver

# Element IDs (tests/e2e-unified/ui.yaml § unlock + onboarding).
PASSPHRASE_INPUT = "unlock-passphrase-input"
CONFIRM_INPUT = "unlock-confirm-input"
SUBMIT = "unlock-submit-button"
CREATE_IDENTITY = "create-identity-button"


@contextlib.contextmanager
def headless_launch(app_path: str, nest: dict, xdg_base, request, extra_env: dict | None = None):
    """One isolated headless-store launch: sealed backend forced, XDG base
    pinned by the caller (so the sealed file — which lives under
    ``$XDG_CONFIG_HOME/fauna-tui`` — survives a teardown + relaunch).
    ``extra_env`` adds to the launch's environment (a second copy's
    ``FAUNA_BOUND_ACCOUNT``, say).

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
        "environment": {**_seeded_environment(request, nest), **(extra_env or {})},
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


def claim_admin_through_wizard(driver, nest: dict, admin_secret_hex: str):
    """The real UI claim of a fresh nest (the crash-journeys module's claim
    front-half): import identity → claim-code page for the known nest → submit
    the fixture nest's claim code → nat-mode seed → authenticated shell. Both
    sealed-store journeys need a store that HOLDS a signed-in identity, and
    every persisted slot along this path goes through the sealed backend.

    The claim code comes off the handle (`nest["claim_code"]`), never the
    `common.nest.CLAIM_CODE` constant: that constant is standalone's, while
    docker's provider mints a random code per container."""
    driver.click("import-identity-button")
    driver.wait_for("paste-secret-field", timeout=15)
    driver.clear_and_type("paste-secret-field", admin_secret_hex)
    driver.click("import-submit-button")
    driver.wait_for("handle-input", timeout=15)
    driver.call_machine_method(
        "navigate_to_claim_code_for_known_nest",
        json.dumps([nest["url"], "admin@localhost"]),
    )
    driver.wait_for("claim-code-input", timeout=20)
    driver.clear_and_type("claim-code-input", nest["claim_code"])
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


# ── The sealed file itself (``libs/fauna-credential-store/src/sealed.rs``) ──
# The header the integrity journeys read without the passphrase: magic,
# version byte, KDF triple (12), salt (16), nonce (12), then ciphertext + the
# 16-byte AEAD tag. Read here so a witness can tell a whole file from a cut-
# short one, and write a "newer build" version byte, without the key.
SEALED_MAGIC = b"FAUNACREDSTORE\0"
SEALED_VERSION_OFFSET = len(SEALED_MAGIC)
SEALED_HEADER_LEN = len(SEALED_MAGIC) + 1 + 12 + 16 + 12
AEAD_TAG_LEN = 16


def sealed_path(config: dict) -> Path:
    """Where this launch's sealed file rests:
    ``$XDG_CONFIG_HOME/fauna-tui/<namespace>.sealed`` (``tui.md`` § Credential
    storage), the XDG base the launch pinned."""
    return Path(config["xdg_base"]) / "config" / "fauna-tui" / f"{config['keyring_app']}.sealed"


def assert_sealed_whole(path: Path, context: str) -> bytes:
    """The file is there and whole — its magic, a complete header and at least
    a tag's worth of sealed bytes after it — and its bytes are returned. (Whole
    is not the same as opening: the unlock that follows is what proves it
    reads.)"""
    assert path.is_file(), f"{context}: the sealed file is gone ({path})"
    data = path.read_bytes()
    assert data.startswith(SEALED_MAGIC), (
        f"{context}: the sealed file lost its header: {data[:24]!r}"
    )
    assert len(data) >= SEALED_HEADER_LEN + AEAD_TAG_LEN, (
        f"{context}: the sealed file was cut short to {len(data)} bytes"
    )
    return data


def relaunch(driver) -> None:
    """A clean quit, then a fresh process over the same XDG base (the launch
    dict the driver was last given, escrow-trust seed included)."""
    driver.teardown()
    driver.launch(driver._launch_config)


def crash_and_relaunch(driver) -> None:
    """SIGKILL — no handler, no flush, the genuine crash — then relaunch."""
    driver.kill_uncleanly()
    relaunch(driver)


def wait_unlock_surface(driver, *, mode: str = "unlock") -> None:
    """The unlock page is up, in ``mode`` (``unlock``: one input — a file
    exists; ``create``: the confirm input too)."""
    driver.wait_for(PASSPHRASE_INPUT, timeout=20)
    if mode == "unlock":
        assert driver.is_absent(CONFIRM_INPUT), (
            "an existing sealed file must open the UNLOCK surface, never create "
            "a fresh store over it"
        )
    else:
        assert driver.is_visible(CONFIRM_INPUT), (
            f"expected the CREATE surface: {driver.diagnose(CONFIRM_INPUT)}"
        )


def unlock_refused(driver, passphrase: str, context: str) -> str:
    """Submit ``passphrase`` and require the refusal: the honest error paints
    and the surface stays locked. Returns the error text."""
    submit_passphrase(driver, passphrase)
    err = driver.get_text("error-message")
    assert err, f"{context}: the unlock must be refused with error-message"
    assert driver.is_visible(PASSPHRASE_INPUT), (
        f"{context}: a refused unlock must stay on the unlock surface"
    )
    return err


def unlock_opens(driver, passphrase: str, timeout: float = 20) -> bool:
    """Submit ``passphrase``; True when the unlock surface lets go (the sealed
    file opened under it), False when it stays with the refusal painted."""
    submit_passphrase(driver, passphrase)
    deadline = time.monotonic() + timeout
    while time.monotonic() < deadline:
        if driver.is_absent(PASSPHRASE_INPUT):
            return True
        if driver.get_text("error-message"):
            return False
        time.sleep(0.2)  # sleep-ok: polling two observables of one synchronous submit
    return driver.is_absent(PASSPHRASE_INPUT)


def wait_authenticated(driver, context: str, timeout: float = 60) -> None:
    try:
        state = driver.wait_for_state(
            lambda s: bool(s.get("session", {}).get("authenticated")), timeout=timeout
        )
    except TimeoutError:
        state = driver.get_state() or {}
        error = stderr = None
        with contextlib.suppress(Exception):
            error = driver.get_text("error-message")
        with contextlib.suppress(Exception):
            stderr = driver.app_stderr_text()[-1500:]
        raise AssertionError(
            f"{context}: never signed in within {timeout}s — "
            f"session={state.get('session')!r} nav={state.get('nav')!r} "
            f"error={error!r} stderr tail={stderr!r}"
        ) from None
    assert state["session"]["authenticated"] is True, (
        f"{context}: the sealed identity must sign straight back in, got "
        f"session={state.get('session')!r} error={driver.get_text('error-message')!r}"
    )
