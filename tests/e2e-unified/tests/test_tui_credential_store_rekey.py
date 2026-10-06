"""tier_3 e2e: the sealed store's change-passphrase affordance (the
credential-store settings section).

Authoritative behavior: ``docs/goal/ui/settings.md`` § Credential store (the
render surface) + ``docs/goal/architecture/apps/tui.md`` § Credential storage
(the re-key mechanics; design ratified 2026-08-06). The section renders only
while the sealed arm is active, arms a modal of three MASKED inputs plus the
mandated back-up-your-seed nudge, and the submit re-seals the whole namespace
map under the new passphrase — fresh salt, current interactive Argon2id
params, fresh nonce, tmp+rename.

Same chassis as ``test_tui_headless_credential_store`` — the shared
``helpers/tui_headless_store.py`` launcher: the driver's ``headless_store``
launch mode empties ``FAUNA_E2E_CREDENTIAL_DIR`` (the journey runs the REAL
sealed store) and forces the headless arm so a desktop dev box's live Secret
Service cannot shadow it. Real nest, real UI claim, real Argon2id derives —
tier_3.

The journey (one test on purpose — every stage depends on the artifacts of
the previous one, and the point IS the arc):

1.  Fresh headless launch → CREATE the store under the OLD passphrase → a
    real UI claim lands the authenticated shell (the sealed file now holds a
    signed-in identity worth re-keying).
2.  Settings → Account: the credential-store section renders (status line
    non-empty), modal absent until armed.
3.  Arm the modal: the nudge + three inputs + submit/cancel render, and a
    typed passphrase reads back as a same-length bullet mask (the raw value
    must never enter the automation registry).
4.  Wrong current passphrase → the honest wrong-or-corrupt error paints in
    ``error-message``; the modal stays.
5.  Mismatched new/confirm → the mismatch error; the modal stays.
6.  Correct entries → the modal disarms and the success line paints; the
    session stays authenticated (the running store keeps serving under the
    swapped key).
7.  Force-quit + relaunch (same XDG base) → the UNLOCK surface; the OLD
    passphrase is refused with the honest error.
8.  The NEW passphrase unlocks, launch routing reads the sealed identity,
    and the session authenticates — the re-key held end to end.
"""

from __future__ import annotations

import pytest
from nacl.signing import SigningKey

from conftest import get_available_apps
from helpers import tui_headless_store as hs

pytestmark = [pytest.mark.tier_3, pytest.mark.tui]

OLD_PASSPHRASE = "correct-horse-battery"
NEW_PASSPHRASE = "staple-gun-tuesday"

# Element IDs (tests/e2e-unified/ui.yaml § settings — the credential-store
# family, user-approved 2026-08-06 — and § tui-unlock).
SECTION = "credential-store-section"
STATUS = "credential-store-status"
REKEY_BUTTON = "credential-store-rekey-button"
MODAL = "credential-store-rekey-modal"
SEED_NUDGE = "credential-store-rekey-seed-nudge"
CURRENT_INPUT = "credential-store-rekey-current-input"
NEW_INPUT = "credential-store-rekey-new-input"
CONFIRM_INPUT = "credential-store-rekey-confirm-input"
SUBMIT = "credential-store-rekey-submit-button"
SUCCESS = "credential-store-rekey-success"

# The Account settings sub-page, navigated the way the switcher journey does
# (test_account_switcher_tui.ACCOUNT_PAGE_NAV): navigation is stage-setting
# here, not the behavior under test — the re-key itself is driven click by
# click through the UI (e2e convention 8).
ACCOUNT_PAGE_NAV = {
    "nav": {"stack": [{"view": "settings"}, {"view": "settings", "id": "account"}]}
}


@pytest.fixture(params=["tui"])
def launch_client(request):
    """tui-only, via the parametrized-fixture shape the headless-store module
    uses: the client id lands in the test name for conftest's ``--client``
    filter."""
    if request.param not in get_available_apps():
        pytest.skip("fauna-tui is not available on this machine")
    return request.param


@pytest.fixture
def rekey_unclaimed_nest(request, nest_mode, tmp_path_factory):
    """A dedicated fresh, NEVER-claimed nest — stage 1 drives the real UI
    claim itself (the headless-store module's fixture, same reasoning).

    ``unclaimed`` is honoured in every mode (the docker provider declares it),
    so stage 1's claim is the same ceremony against a container.
    """
    from conftest import _start_dedicated_nest

    nest, cleanup = _start_dedicated_nest(
        request, nest_mode, tmp_path_factory, "tui-rekey-nest", unclaimed=True)
    yield nest
    cleanup()


def _rekey_submit(driver, current: str, new: str, confirm: str):
    driver.clear_and_type(CURRENT_INPUT, current)
    driver.clear_and_type(NEW_INPUT, new)
    driver.clear_and_type(CONFIRM_INPUT, confirm)
    driver.click(SUBMIT)


@pytest.mark.feature("identity-protected-on-this-device")
def test_change_passphrase_relaunch_old_refused_new_unlocks(
    launch_client, tui_app_path, rekey_unclaimed_nest, tmp_path, request
):
    """The full re-key journey (module docs) against a real nest + the real
    sealed store."""
    xdg_base = tmp_path / "xdg"
    admin_secret_hex = bytes(SigningKey.generate()).hex()

    with hs.headless_launch(tui_app_path, rekey_unclaimed_nest, xdg_base, request) as (
        driver,
        config,
    ):
        # 1. Create the store under OLD_PASSPHRASE, then claim through the
        # real wizard so the sealed file holds a signed-in identity.
        driver.wait_for(hs.PASSPHRASE_INPUT, timeout=15)
        hs.submit_passphrase(driver, OLD_PASSPHRASE, confirm=OLD_PASSPHRASE)
        driver.wait_for(hs.CREATE_IDENTITY, timeout=15)
        hs.claim_admin_through_wizard(
            driver, rekey_unclaimed_nest["url"], admin_secret_hex
        )

        # 2. Settings → Account: the section renders on the sealed arm; the
        # modal waits for the arming gesture.
        driver.set_state(ACCOUNT_PAGE_NAV)
        driver.wait_for(SECTION, timeout=15)
        assert driver.get_text(STATUS), (
            "the status line must say how credentials are held: "
            f"{driver.diagnose(STATUS)}"
        )
        assert driver.is_absent(MODAL), "the modal paints only while armed"

        # 3. Arm: the modal family renders; a typed value reads back masked.
        driver.click(REKEY_BUTTON)
        driver.wait_for(MODAL, timeout=10)
        assert driver.get_text(SEED_NUDGE), (
            "the mandated back-up-your-seed nudge must carry its copy: "
            f"{driver.diagnose(SEED_NUDGE)}"
        )
        driver.clear_and_type(CURRENT_INPUT, "hunter2")
        masked = driver.get_text(CURRENT_INPUT)
        assert masked == "•" * len("hunter2"), (
            "a passphrase input's registered text must be a same-length "
            f"bullet mask, got {masked!r}"
        )

        # 4. Wrong current passphrase → honest error, modal stays.
        _rekey_submit(driver, "wrong-horse", NEW_PASSPHRASE, NEW_PASSPHRASE)
        err = driver.get_text("error-message")
        assert err, "a wrong current passphrase must paint error-message"
        assert driver.is_visible(MODAL), "the modal stays on refusal"
        assert driver.is_absent(SUCCESS)

        # 5. Mismatched new/confirm → error, modal stays. (The store was NOT
        # touched: stage 7's old-refused assert would catch a partial write.)
        _rekey_submit(driver, OLD_PASSPHRASE, NEW_PASSPHRASE, "something-else")
        err = driver.get_text("error-message")
        assert err, "a mismatched confirm must paint error-message"
        assert driver.is_visible(MODAL), "the modal stays on a mismatch"

        # 6. Correct entries → success: modal disarms, success line paints,
        # session still authenticated (the swapped key keeps serving).
        _rekey_submit(driver, OLD_PASSPHRASE, NEW_PASSPHRASE, NEW_PASSPHRASE)
        driver.wait_for(SUCCESS, timeout=10)
        assert driver.is_absent(MODAL), "success disarms the modal"
        state = driver.get_state()
        assert state.get("session", {}).get("authenticated") is True, (
            "the running session must keep serving across the re-key, got "
            f"session={state.get('session')!r}"
        )

        # 7-8 run against a relaunch of the SAME store. Relaunches
        # `driver._launch_config` (the same dict `config` names) rather than
        # `config` itself, so it carries the escrow-trust seed the first launch
        # applied without a fresh `_seeded_environment` call here.
        driver.teardown()
        driver.launch(driver._launch_config)

        # 7. UNLOCK mode; the OLD passphrase is refused.
        driver.wait_for(hs.PASSPHRASE_INPUT, timeout=15)
        assert driver.is_absent(hs.CONFIRM_INPUT), (
            "an existing sealed store must relaunch into UNLOCK mode"
        )
        hs.submit_passphrase(driver, OLD_PASSPHRASE)
        err = driver.get_text("error-message")
        assert err, "the old passphrase must be refused after a re-key"
        assert driver.is_visible(hs.PASSPHRASE_INPUT), (
            "a refused passphrase must stay on the unlock surface"
        )

        # 8. The NEW passphrase unlocks straight into the session.
        hs.submit_passphrase(driver, NEW_PASSPHRASE)
        state = driver.wait_for_state(
            lambda s: bool(s.get("session", {}).get("authenticated")), timeout=40
        )
        assert state["session"]["authenticated"] is True, (
            "the new passphrase must sign the saved identity straight in, got "
            f"session={state.get('session')!r} "
            f"error={driver.get_text('error-message')!r}"
        )
