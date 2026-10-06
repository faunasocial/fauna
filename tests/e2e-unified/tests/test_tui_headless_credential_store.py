"""tier_3 e2e: the tui HEADLESS credential store (M7) — create, mismatch,
relaunch-unlock, wrong-passphrase, and the authenticated sign-in that is this
store's whole reason to exist.

Authoritative behavior: ``docs/goal/architecture/apps/tui.md`` § Credential
storage (design ratified 2026-07-14): when no usable OS secret store is
reachable, the identity rests in a passphrase-encrypted sealed file
(``libs/fauna-credential-store::sealed`` — Argon2id + ChaCha20-Poly1305 over
the whole namespace map) and the ``tui-unlock`` page gates launch routing.

The driver's ``headless_store`` launch mode empties
``FAUNA_E2E_CREDENTIAL_DIR`` (so the e2e file backend is OFF — this journey
runs the REAL sealed store) and sets the test-only
``FAUNA_E2E_FORCE_HEADLESS_STORE`` so a desktop dev box's live Secret Service
cannot shadow the arm under test. Real nest binary, real claim through the
real wizard UI, real Argon2id derive: nothing in the loop is mocked, hence
tier_3. tui-only by structure — no other app has (or should grow) this
surface; the desktop apps keep their OS stores.

The journey (one test on purpose — every stage depends on the artifacts of
the previous one, and the point IS the arc):

1. Fresh headless launch → the CREATE surface (passphrase + confirm inputs).
2. Mismatched entries → the mismatch error paints in ``error-message``; still
   on the surface (nothing created).
3. Matching entries → the store is created; launch routing runs; the wizard
   appears (fresh install ⇒ identity_choice).
4. A real UI claim of the fresh nest (import identity → claim-code submit)
   lands the authenticated shell — every wizard write went into the SEALED
   store.
5. Force-quit + relaunch (same XDG base) → the UNLOCK surface (no confirm
   input this time — the file exists).
6. Wrong passphrase → the honest wrong-or-corrupt error; still locked.
7. Right passphrase → launch routing reads the sealed identity, the silent
   challenge succeeds, and the session is authenticated with NO OS secret
   store reachable — ``tui.md`` § Goal's headless sign-in, end to end.
"""

from __future__ import annotations

import pytest
from nacl.signing import SigningKey

from conftest import get_available_apps
from helpers.tui_headless_store import (
    CONFIRM_INPUT,
    CREATE_IDENTITY,
    PASSPHRASE_INPUT,
    claim_admin_through_wizard,
    headless_launch,
    submit_passphrase,
)

pytestmark = [
    pytest.mark.tier_3,
    pytest.mark.tui,
    pytest.mark.feature("identity-protected-on-this-device"),
]

PASSPHRASE = "correct-horse-battery"


@pytest.fixture(params=["tui"])
def launch_client(request):
    """tui-only, via the same parametrized-fixture shape as the launch-routing
    smoke module: the client id lands in the test name, which is what
    conftest's ``--client`` filter reads (so ``--client linux`` deselects it
    instead of failing on a missing surface)."""
    if request.param not in get_available_apps():
        pytest.skip("fauna-tui is not available on this machine")
    return request.param


@pytest.fixture
def headless_unclaimed_nest(request, nest_mode, tmp_path_factory):
    """A dedicated fresh, NEVER-claimed nest (claim code = common.nest
    CLAIM_CODE) — stage 4 drives the real UI claim itself, so the shared
    (already-claimed) ``nest_instance`` can't serve here.

    ``unclaimed`` is honoured in every mode (the docker provider declares it),
    so stage 4's claim is the same ceremony against a container.
    """
    from conftest import _start_dedicated_nest

    nest, cleanup = _start_dedicated_nest(
        request, nest_mode, tmp_path_factory, "tui-headless-nest", unclaimed=True)
    yield nest
    cleanup()


def test_headless_store_create_claim_relaunch_unlock(
    launch_client, tui_app_path, headless_unclaimed_nest, tmp_path, request
):
    """The full M7 journey (module docs) against a real nest + the real sealed
    store."""
    xdg_base = tmp_path / "xdg"
    admin_secret_hex = bytes(SigningKey.generate()).hex()

    with headless_launch(tui_app_path, headless_unclaimed_nest, xdg_base, request) as (
        driver,
        config,
    ):
        # 1. Fresh headless launch → the CREATE surface, both inputs.
        driver.wait_for(PASSPHRASE_INPUT, timeout=15)
        assert driver.is_visible(CONFIRM_INPUT), (
            "first headless run must be CREATE mode (confirm input present): "
            f"{driver.diagnose(CONFIRM_INPUT)}"
        )

        # 2. Mismatch → error, still on the surface, nothing created.
        submit_passphrase(driver, PASSPHRASE, confirm="something-else")
        err = driver.get_text("error-message")
        assert err, "a mismatched confirm must paint error-message"
        assert driver.is_visible(CONFIRM_INPUT), (
            "a mismatch must stay on the create surface"
        )

        # 3. Matching entries → store created → launch routing → the wizard's
        # fresh-install entry (identity_choice).
        submit_passphrase(driver, PASSPHRASE, confirm=PASSPHRASE)
        driver.wait_for(CREATE_IDENTITY, timeout=15)

        # 4. A real UI claim — every persisted slot along the way went into
        # the sealed store (shared front-half; see the helper's docstring).
        claim_admin_through_wizard(driver, headless_unclaimed_nest["url"], admin_secret_hex)

        # 5-7 run against a relaunch of the SAME store. Relaunches
        # `driver._launch_config` (the same dict `config` names) rather than
        # `config` itself, so it carries the escrow-trust seed the first launch
        # applied without a fresh `_seeded_environment` call here.
        driver.teardown()
        driver.launch(driver._launch_config)

        # 5. The file exists now → UNLOCK mode: no confirm input.
        driver.wait_for(PASSPHRASE_INPUT, timeout=15)
        assert driver.is_absent(CONFIRM_INPUT), (
            "an existing sealed store must relaunch into UNLOCK mode "
            "(no confirm input)"
        )

        # 6. Wrong passphrase → the honest error; still locked on the surface.
        submit_passphrase(driver, "wrong-horse")
        err = driver.get_text("error-message")
        assert err, "a wrong passphrase must paint error-message"
        assert driver.is_visible(PASSPHRASE_INPUT), (
            "a wrong passphrase must stay on the unlock surface"
        )

        # 7. Right passphrase → the sealed identity is read back, the silent
        # challenge runs, and the session authenticates: a headless sign-in
        # with no OS secret store anywhere in the loop.
        submit_passphrase(driver, PASSPHRASE)
        state = driver.wait_for_state(
            lambda s: bool(s.get("session", {}).get("authenticated")), timeout=40
        )
        assert state["session"]["authenticated"] is True, (
            "unlocking the sealed store must sign the saved identity straight "
            f"in (LaunchPhase::Online), got session={state.get('session')!r} "
            f"error={driver.get_text('error-message')!r}"
        )
