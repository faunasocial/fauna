"""tier_3 e2e: the tui HEADLESS credential store (M7) — create, mismatch,
relaunch-unlock, wrong-passphrase, and the authenticated sign-in that is this
store's whole reason to exist.

Authoritative behavior: ``docs/goal/architecture/apps/tui.md`` § Credential
storage (design ratified 2026-07-14): when no usable OS secret store is
reachable, the identity rests in a passphrase-encrypted sealed file
(``libs/fauna-credential-store::sealed`` — Argon2id + ChaCha20-Poly1305 over
the whole namespace map) and the ``unlock`` page gates launch routing.

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

import contextlib

import pytest
from nacl.signing import SigningKey

from conftest import get_available_apps
from helpers import tui_headless_store as hs
from helpers.tui_headless_store import (
    CONFIRM_INPUT,
    CREATE_IDENTITY,
    PASSPHRASE_INPUT,
    claim_admin_through_wizard,
    headless_launch,
    submit_passphrase,
)

from tests.test_onboarding_launch_routing_smoke import smoke_handled_nest  # noqa: E402,F401 (fixture)

pytestmark = [pytest.mark.tier_3, pytest.mark.tui]

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


@pytest.mark.feature("identity-protected-on-this-device", "identity-sealed-under-a-passphrase")
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
        assert driver.is_absent("unlock-keyring-locked-notice"), (
            "the forced headless arm (no keyring probed) must not claim a locked "
            "keyring — that notice is the KeyringLocked cause's alone"
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
        claim_admin_through_wizard(driver, headless_unclaimed_nest, admin_secret_hex)

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
        assert driver.is_absent("unlock-keyring-locked-notice"), (
            "unlock mode never shows the locked-keyring notice"
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


# ── The sealed identity's integrity promises ─────────────────────────────────
#
# `identity-sealed-under-a-passphrase` outcomes 12–16: however the app stops,
# whatever it finds it cannot read, however many copies are open, the sealed
# identity is never cut short, emptied, abandoned or lost; and a forgotten
# passphrase has no reset. Each journey signs a REGISTERED actor in through the
# real wizard (the `smoke_handled_nest` shape — a real handle check, no claim),
# so the sealed file holds an identity worth losing, then attacks the file or
# the process from outside, the way a crash, a disk fault or a second SSH
# session would. Mechanism owner: `tui.md` § Credential storage (tmp+rename,
# the split read contract, the namespace lock, data-present precedence).

ACCOUNT_PAGE_NAV = {
    "nav": {"stack": [{"view": "settings"}, {"view": "settings", "id": "account"}]}
}
REKEY_BUTTON = "credential-store-rekey-button"
REKEY_MODAL = "credential-store-rekey-modal"
REKEY_SUCCESS = "credential-store-rekey-success"


def _handled_actor(nest, stem: str) -> dict:
    from helpers.admin_wire import admit_adult

    suffix = SigningKey.generate().verify_key.encode().hex()[:6]
    return admit_adult(nest, f"{stem}{suffix}", domain=nest["authority"], token=True)


def _create_and_sign_in(driver, actor, nest, passphrase: str = PASSPHRASE) -> None:
    from common.launch_harness import sign_in_through_the_wizard

    hs.wait_unlock_surface(driver, mode="create")
    submit_passphrase(driver, passphrase, confirm=passphrase)
    driver.wait_for(CREATE_IDENTITY, timeout=15)
    sign_in_through_the_wizard(driver, actor, nest)


def _rekey(driver, current: str, new: str) -> None:
    """Settings → Account → change passphrase, driven through the modal."""
    driver.set_state(ACCOUNT_PAGE_NAV)
    driver.wait_for(REKEY_BUTTON, timeout=15)
    if not driver.is_visible(REKEY_MODAL):
        driver.click(REKEY_BUTTON)
        driver.wait_for(REKEY_MODAL, timeout=10)
    driver.clear_and_type("credential-store-rekey-current-input", current)
    driver.clear_and_type("credential-store-rekey-new-input", new)
    driver.clear_and_type("credential-store-rekey-confirm-input", new)
    driver.click("credential-store-rekey-submit-button")


@pytest.mark.feature("identity-sealed-under-a-passphrase")
def test_an_unreadable_sealed_identity_is_left_as_it_is(
    launch_client, tui_app_path, smoke_handled_nest, tmp_path, request  # noqa: F811
):
    """Outcome 14 — a sealed identity the app cannot read (damaged, or written
    by a newer build) is left as it is, never replaced with an empty one:

    1. Signed in on the sealed store; quit.
    2. A damaged file (one flipped tag byte) → relaunch: the UNLOCK surface,
       never create; the right passphrase is refused; the file's bytes are
       exactly what was there.
    3. A newer build's file (version byte bumped) → the same, bytes unchanged.
    4. Damage arriving while the app runs unlocked → a write (the re-key) is
       refused and the damaged bytes stay — never a valid store resealed over
       them (the split read contract).
    5. The original bytes put back → the right passphrase signs straight back
       in: the identity was never lost.
    """
    nest = smoke_handled_nest
    actor = _handled_actor(nest, "unread")
    with hs.headless_launch(tui_app_path, nest, tmp_path / "xdg", request) as (driver, config):
        _create_and_sign_in(driver, actor, nest)
        path = hs.sealed_path(config)
        driver.teardown()
        original = hs.assert_sealed_whole(path, "after sign-in")

        # 2. Damaged.
        damaged = original[:-1] + bytes([original[-1] ^ 0x01])
        path.write_bytes(damaged)
        driver.launch(driver._launch_config)
        hs.wait_unlock_surface(driver)
        hs.unlock_refused(driver, PASSPHRASE, "a damaged sealed file")
        driver.teardown()
        assert path.read_bytes() == damaged, (
            "a damaged sealed file must be left exactly as found"
        )

        # 3. Written by a newer build.
        newer = bytearray(original)
        newer[hs.SEALED_VERSION_OFFSET] = original[hs.SEALED_VERSION_OFFSET] + 1
        path.write_bytes(bytes(newer))
        driver.launch(driver._launch_config)
        hs.wait_unlock_surface(driver)
        hs.unlock_refused(driver, PASSPHRASE, "a newer build's sealed file")
        driver.teardown()
        assert path.read_bytes() == bytes(newer), (
            "a sealed file from a newer build must never be reinterpreted or overwritten"
        )

        # 4. Damaged under a running, unlocked app; a write must not reseal.
        path.write_bytes(original)
        driver.launch(driver._launch_config)
        hs.wait_unlock_surface(driver)
        assert hs.unlock_opens(driver, PASSPHRASE), driver.get_text("error-message")
        hs.wait_authenticated(driver, "the original file")
        path.write_bytes(damaged)
        _rekey(driver, PASSPHRASE, "a-new-passphrase-here")
        assert driver.get_text("error-message"), (
            "a re-key over a file the app can no longer read must be refused"
        )
        assert driver.is_absent(REKEY_SUCCESS)
        assert path.read_bytes() == damaged, (
            "a write must never replace a sealed file the app could not read"
        )
        driver.teardown()
        assert path.read_bytes() == damaged, (
            "quitting must not reseal over a sealed file the app could not read"
        )

        # 5. The identity was never lost.
        path.write_bytes(original)
        driver.launch(driver._launch_config)
        hs.wait_unlock_surface(driver)
        assert hs.unlock_opens(driver, PASSPHRASE), driver.get_text("error-message")
        hs.wait_authenticated(driver, "the restored file")


@pytest.mark.feature("identity-sealed-under-a-passphrase")
def test_a_forgotten_passphrase_cannot_be_reset(
    launch_client, tui_app_path, smoke_handled_nest, tmp_path, request  # noqa: F811
):
    """Outcome 12 — a forgotten passphrase cannot be reset; the identity comes
    back only from a copy of the key kept elsewhere or the recovery kit:

    1. Signed in on the sealed store; quit.
    2. Relaunch: the unlock surface offers no reset, hint or bypass (the
       start-over pair is web's, where there is no file to remove —
       ``web.md`` § Identity store at rest), and guess after guess is refused
       with the file untouched.
    3. The terminal's start-over is removing the sealed file. The next launch
       asks for a NEW passphrase and lands on a wizard holding no identity:
       nothing brought the old one back. Its ways back are a kept key and the
       recovery kit.
    4. Importing the kept key signs the same account straight back in.
    """
    from common.launch_harness import sign_in_through_the_wizard

    nest = smoke_handled_nest
    actor = _handled_actor(nest, "forgot")
    with hs.headless_launch(tui_app_path, nest, tmp_path / "xdg", request) as (driver, config):
        _create_and_sign_in(driver, actor, nest)
        path = hs.sealed_path(config)
        driver.teardown()
        sealed = hs.assert_sealed_whole(path, "after sign-in")

        # 2. No reset; every guess refused; nothing changed.
        driver.launch(driver._launch_config)
        hs.wait_unlock_surface(driver)
        for affordance in ("unlock-reset-button", "unlock-reset-confirm-button"):
            assert driver.is_absent(affordance), (
                f"the terminal's unlock surface must offer no passphrase reset ({affordance})"
            )
        for guess in ("correct-horse", "battery-staple", "i-forgot-it"):
            hs.unlock_refused(driver, guess, f"the guess {guess!r}")
        driver.teardown()
        assert path.read_bytes() == sealed, "refused guesses must not touch the sealed file"

        # 3. Start over: the identity does not come back by itself.
        path.unlink()
        driver.launch(driver._launch_config)
        hs.wait_unlock_surface(driver, mode="create")
        submit_passphrase(driver, "a-brand-new-passphrase", confirm="a-brand-new-passphrase")
        driver.wait_for(CREATE_IDENTITY, timeout=15)
        assert not driver.get_state("session.authenticated"), (
            "a fresh store must hold no identity until one is brought back"
        )
        for way_back in ("import-identity-button", "restore-from-recovery-kit-button"):
            assert driver.is_visible(way_back), (
                f"the wizard must offer {way_back} as a way back: {driver.diagnose(way_back)}"
            )

        # 4. The kept key brings the same account back.
        sign_in_through_the_wizard(driver, actor, nest)
        hs.wait_authenticated(driver, "the kept key")


@pytest.mark.feature("identity-sealed-under-a-passphrase")
def test_after_a_reset_the_next_setup_chooses_a_passphrase_again(
    launch_client, tui_app_path, smoke_handled_nest, tmp_path, request  # noqa: F811
):
    """Outcome 17 on tui — ``tui.md`` § Credential storage: a reset deletes the
    sealed file **and relocks**, so the next onboarding re-chooses its passphrase.
    tui's reset is the Account page's sign-out, ``App::reset`` (the unlock surface
    offers none — the forgotten-passphrase journey above pins that).

    1. Signed in on the sealed store, the user signs out through the page.
    2. The CREATE surface comes straight back: the file is gone, and the wizard
       is not reachable until a passphrase is chosen — the old key was dropped,
       never kept to re-seal silently.
    3. A new passphrase and the kept key sign the account back in.
    4. Relaunch: the store opens under the new passphrase and never the old.
    """
    from actions import ActionLayer
    from common.launch_harness import sign_in_through_the_wizard

    nest = smoke_handled_nest
    actor = _handled_actor(nest, "reset")
    new_passphrase = "a-brand-new-passphrase"
    with hs.headless_launch(tui_app_path, nest, tmp_path / "xdg", request) as (driver, config):
        _create_and_sign_in(driver, actor, nest)
        path = hs.sealed_path(config)
        hs.assert_sealed_whole(path, "after sign-in")

        ActionLayer(driver).settings.press_sign_out()
        hs.wait_unlock_surface(driver, mode="create")
        assert not path.exists(), f"a reset must delete the sealed file: {path}"
        assert driver.is_absent(CREATE_IDENTITY), (
            "the wizard must wait for the new passphrase: nothing may persist before it"
        )

        submit_passphrase(driver, new_passphrase, confirm=new_passphrase)
        driver.wait_for(CREATE_IDENTITY, timeout=15)
        sign_in_through_the_wizard(driver, actor, nest)
        hs.wait_authenticated(driver, "the new passphrase")
        hs.assert_sealed_whole(path, "after choosing the new passphrase")

        driver.teardown()
        driver.launch(driver._launch_config)
        hs.wait_unlock_surface(driver)
        hs.unlock_refused(driver, PASSPHRASE, "the passphrase from before the reset")
        assert hs.unlock_opens(driver, new_passphrase), (
            f"the new passphrase must open the store: {driver.get_text('error-message')!r}"
        )
        hs.wait_authenticated(driver, "the new passphrase after a relaunch")


@pytest.mark.feature("identity-sealed-under-a-passphrase")
def test_a_crash_while_saving_never_cuts_the_sealed_file_short(
    launch_client, tui_app_path, smoke_handled_nest, tmp_path, request  # noqa: F811
):
    """Outcome 13 — if the app stops while saving the sealed identity, what it
    saved is never left cut short or unreadable:

    1. Create the store, then kill the app as the imported key's save is
       submitted — a timer fires the kill, so it races the save rather than
       following it — at several offsets.
    2. After each kill: the file is whole; a cut-short write file left beside
       it (what a crash before the rename leaves) changes nothing; the
       passphrase opens it. Whichever offsets happen to land inside the
       write, every one must hold — the assertions are on state, not timing.
    3. Sign in to the end, kill straight after, and the passphrase signs the
       identity straight back in.
    """
    import threading

    from actions import ActionLayer
    from common.launch_harness import finish_sign_in_from_handle_entry

    nest = smoke_handled_nest
    actor = _handled_actor(nest, "crash")
    secret_hex = bytes(actor["signing_key"]).hex()
    with hs.headless_launch(tui_app_path, nest, tmp_path / "xdg", request) as (driver, config):
        path = hs.sealed_path(config)
        hs.wait_unlock_surface(driver, mode="create")
        submit_passphrase(driver, PASSPHRASE, confirm=PASSPHRASE)
        driver.wait_for(CREATE_IDENTITY, timeout=15)

        for offset in (0.0, 0.02, 0.1):
            if driver.is_visible("import-identity-button"):
                driver.click("import-identity-button")
                driver.wait_for("paste-secret-field", timeout=15)
                driver.clear_and_type("paste-secret-field", secret_hex)
                killer = threading.Timer(offset, driver.kill_uncleanly)
                killer.start()
                with contextlib.suppress(Exception):
                    driver.click("import-submit-button")
                killer.join()
            else:
                driver.kill_uncleanly()
            before = hs.assert_sealed_whole(path, f"a kill {offset}s into a save")
            residue = path.with_name(f".{path.name}.tmp-424242")
            residue.write_bytes(before[: hs.SEALED_HEADER_LEN // 2])
            hs.relaunch(driver)
            hs.wait_unlock_surface(driver)
            assert hs.unlock_opens(driver, PASSPHRASE), (
                f"after a kill {offset}s into a save the passphrase must still open the "
                f"sealed file: error={driver.get_text('error-message')!r}"
            )
            residue.unlink(missing_ok=True)

        ActionLayer(driver).onboarding.resume_or_import_identity(secret_hex)
        finish_sign_in_from_handle_entry(driver, actor, nest)

        hs.crash_and_relaunch(driver)
        hs.assert_sealed_whole(path, "a kill straight after the sign-in")
        hs.wait_unlock_surface(driver)
        assert hs.unlock_opens(driver, PASSPHRASE), driver.get_text("error-message")
        hs.wait_authenticated(driver, "a kill straight after the sign-in")


@pytest.mark.feature("identity-sealed-under-a-passphrase")
def test_two_copies_on_one_sealed_identity_never_lose_it(
    launch_client, tui_app_path, smoke_handled_nest, tmp_path, request  # noqa: F811
):
    """Outcome 15 — two copies of the app open at once on the same sealed
    identity (two SSH sessions on one box) never lose it between them:

    1. Copy A signs in on the sealed store. Copy B starts over the same files
       as an instance bound to the same account (a plain second launch of the
       one account gets the instance chooser instead —
       ``test_account_instance_lock_tui.py``), unlocks with the same
       passphrase and is signed in beside it.
    2. B changes the passphrase. A, still holding the old key, then tries the
       same change from the old passphrase: refused against the file itself,
       never resealing B's file under its stale key.
    3. Both quit; one relaunch: the old passphrase is refused and the new one
       signs the identity straight back in.
    """
    nest = smoke_handled_nest
    actor = _handled_actor(nest, "twocopy")
    xdg = tmp_path / "xdg"
    new_passphrase = "second-copy-passphrase"
    with hs.headless_launch(tui_app_path, nest, xdg, request) as (a, config):
        _create_and_sign_in(a, actor, nest)
        path = hs.sealed_path(config)
        bound = {"FAUNA_BOUND_ACCOUNT": actor["actor_id_hex"]}
        with hs.headless_launch(tui_app_path, nest, xdg, request, extra_env=bound) as (b, _):
            hs.wait_unlock_surface(b)
            assert hs.unlock_opens(b, PASSPHRASE), b.get_text("error-message")
            hs.wait_authenticated(b, "the second copy")
            assert a.get_state("session.authenticated"), (
                "the first copy must stay signed in beside the second"
            )

            _rekey(b, PASSPHRASE, new_passphrase)
            b.wait_for(REKEY_SUCCESS, timeout=15)
            after_b = hs.assert_sealed_whole(path, "the second copy's re-key")

            _rekey(a, PASSPHRASE, "the-stale-copys-try")
            assert a.get_text("error-message"), (
                "a copy whose passphrase was changed elsewhere must be refused"
            )
            assert a.is_absent(REKEY_SUCCESS)
            assert path.read_bytes() == after_b, (
                "the stale copy must not reseal the file the other copy wrote"
            )
        a.teardown()

        hs.assert_sealed_whole(path, "both copies quit")
        a.launch(a._launch_config)
        hs.wait_unlock_surface(a)
        hs.unlock_refused(a, PASSPHRASE, "the passphrase the second copy replaced")
        assert hs.unlock_opens(a, new_passphrase), a.get_text("error-message")
        hs.wait_authenticated(a, "after two copies")


@pytest.mark.feature("identity-sealed-under-a-passphrase")
# A Secret Service on the session bus is a linux box's (convention 7, the fourth skip
# kind); the environment skip below stays for a linux box with no daemon.
@pytest.mark.host_platform("linux")
def test_a_sealed_identity_is_never_left_behind_for_another_store(
    launch_client, tui_app_path, smoke_handled_nest, tmp_path, request  # noqa: F811
):
    """Outcome 16 — once the identity is sealed under a passphrase, the app
    keeps finding it there and never switches by itself to another store:

    1. Signed in on the sealed store (no protected store reachable — the
       headless arm).
    2. Relaunch with a working, unlocked Secret Service on the session bus —
       the production resolution, unforced (a box that has since gained a
       desktop keyring). The existing sealed file wins: the UNLOCK surface,
       never the wizard over an empty keyring.
    3. The passphrase signs straight back in, and the keyring holds nothing
       for the app — the identity stayed where it was sealed.
    """
    from common.keyring import namespace_accounts, secret_service_available
    from drivers.secret_service import PrivateSecretService
    from helpers.app_surface import skip_environment

    if not secret_service_available():
        skip_environment("gnome-keyring-daemon + dbus-daemon are not installed")
    nest = smoke_handled_nest
    actor = _handled_actor(nest, "nostray")
    svc = PrivateSecretService().start()
    try:
        with hs.headless_launch(tui_app_path, nest, tmp_path / "xdg", request) as (driver, config):
            _create_and_sign_in(driver, actor, nest)
            path = hs.sealed_path(config)
            driver.teardown()
            hs.assert_sealed_whole(path, "after sign-in")

            relaunch_config = dict(driver._launch_config)
            relaunch_config.pop("headless_store", None)
            relaunch_config["probed_store"] = True
            relaunch_config["environment"] = {
                **relaunch_config.get("environment", {}),
                "DBUS_SESSION_BUS_ADDRESS": svc.address,
            }
            driver.launch(relaunch_config)
            hs.wait_unlock_surface(driver)
            assert hs.unlock_opens(driver, PASSPHRASE), driver.get_text("error-message")
            hs.wait_authenticated(driver, "a box that gained a keyring")
            assert not namespace_accounts(svc.address, config["keyring_app"]), (
                "the identity must stay in its sealed file, never be moved to "
                "(or recreated empty in) the keyring the app now finds"
            )
            hs.assert_sealed_whole(path, "after unlocking beside a keyring")
    finally:
        svc.stop()
