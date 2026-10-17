"""tier_3 e2e: android multi-account account switcher.

The android twin of ``test_account_switcher_linux.py`` / ``_apple.py`` /
``_windows.py`` — same journeys, same element ids, same seeding contract
(`docs/goal/architecture/long-term-store.md` § Multi-account evolution;
switch-first, design Decision 1), over `multiple-accounts` outcomes 1, 2, 3, 5,
6, 8, 9 and 10. Outcomes 4 and 7 (the Stage-2 re-auth pair) already run on
android in ``test_account_switcher_apple.py`` (``STAGE2_APPS``) and are not
repeated here; outcome 11 waits on android's push call-site, which does not
exist yet (`apps/common.md` § Push Notifications, *Implementation status
today*); `second-identity-in-its-own-window` is a declared android absence.

**Where android's state is read from.** Everything the journeys assert about
the device is on-device, in the app's private data directory, which no host
path reaches on any venue (`testing.md` § Default app and nest mode →
*Android's run venue*):

- the credential file (``fauna/index`` + the per-actor slots) over the
  bridge's ``GET /credentials`` (``AndroidBridgeDriver.credential_map``);
- the account-scoped directories (``<filesDir>/<actor>``,
  ``<databases>/<actor>``, `AccountStores.kt`) through
  ``common.scope_store.AndroidScopeStore``, which asks the device from OUTSIDE
  the app (``run-as``) — always through ``store.exists(...)``, never a host
  ``Path.exists()``, which would be False for every device path.

**A relaunch is a force-stop and a fresh start.** ``teardown()`` force-stops
the app and ``launch()`` starts it again; the app's ``filesDir`` survives both,
and a launch config with no ``seed_credentials`` key leaves the credential file
as the previous process left it (``AndroidBridgeDriver._session_body``). That is
the relaunch the abandon journey uses. ``preserve_state_across_relaunch()`` is
not called: it answers False until the driver's module-level ``recover()``
exists, and it guards a different relaunch than this one.

These tests own their driver (``create_driver`` + ``launch``) rather than taking
the shared ``app`` fixture, whose reset path would wipe the seed — the reason
every per-app switcher suite owns its own launch.
"""
from __future__ import annotations

import os
import time
from pathlib import Path
from types import SimpleNamespace

import pytest

from actions import ActionLayer
from common import build_registry_seed, create_actor_and_register
from common.scope_store import attach_scope_store
from conftest import _seeded_environment
from drivers import create_driver
from drivers.machine_test_setter import set_handle_check_snapshot
from helpers.budgets import APP_RELAUNCH_S, RPC_ROUNDTRIP_S, UI_SETTLE_S
from helpers.waiting import (
    assert_no_relaunch,
    await_session_actor,
    wait_registry_index,
    wait_until,
)
from i18n.strings import S

pytestmark = [pytest.mark.tier_3, pytest.mark.android]

# tests/e2e-unified/ui.yaml § settings (switcher) + navigation (admin-tab).
SWITCHER_LIST = "account-switcher-list"
SWITCHER_ITEM = "account-switcher-item"
ITEM_HANDLE = "account-item-handle"
ACTIVE_INDICATOR = "account-item-active-indicator"
ADD_BUTTON = "account-add-button"
REMOVE_BUTTON = "account-remove-button"
ADMIN_TAB = "admin-tab"

# tests/e2e-unified/ui.yaml § onboarding — the append-mode "Add account" wizard
# and the fresh-install create path.
CREATE_IDENTITY = "create-identity-button"
IMPORT_IDENTITY_BUTTON = "import-identity-button"
PASTE_SECRET_FIELD = "paste-secret-field"
IMPORT_SUBMIT_BUTTON = "import-submit-button"
HANDLE_INPUT = "handle-input"
HANDLE_CONTINUE_BUTTON = "handle-entry-continue-button"
IDENTITY_CONTINUE_BUTTON = "identity-continue-button"
IDENTITY_CREATED_BACK_BUTTON = "identity-created-back-button"
RECOVERY_KIT_SKIP_BUTTON = "recovery-kit-skip-button"
HANDLE_ENTRY_BACK_BUTTON = "handle-entry-back-button"

# tests/e2e-unified/ui.yaml § folders (photo-backup-controls) + feed (composer).
PHOTO_BACKUP_TOGGLE = "photo-backup-enable-toggle"
COMPOSE_FIELD = "compose-text-field"

ACCOUNT_PAGE_NAV = {
    "nav": {"stack": [{"view": "settings"}, {"view": "settings", "id": "account"}]}
}
# The Settings ROOT, where android renders `admin-tab` (`SettingsScreen.kt`,
# gated on the shared am-i-admin answer) — the iOS shape, not a sidebar.
SETTINGS_PAGE_NAV = {"nav": {"stack": [{"view": "settings"}]}}


def _launch_config(request) -> dict:
    """The android launch keys: the APK plus the run's bridge APK and device
    (`helpers/android_device.run_launch_facts`, the one home conftest's own
    android launch reads too)."""
    from helpers import android_device

    return {
        "app_path": request.getfixturevalue("android_app_path"),
        **android_device.run_launch_facts(Path(__file__).resolve().parents[3]),
    }


def _launch(request, nest_instance, seed):
    """Launch android over ``seed``. ``{}`` is a real instruction — "this
    install holds no identity" — that overwrites whatever an earlier test left
    in the app's ``filesDir`` (``AndroidBridgeDriver._session_body``)."""
    driver = create_driver("android")
    driver.launch({
        "url": nest_instance["url"],
        "seed_credentials": seed,
        "environment": _seeded_environment(request, nest_instance),
        **_launch_config(request),
    })
    return driver


def _relaunch(driver):
    """Force-stop and start the app again over the store it left behind: the
    same launch config with its seed withheld (see the module docstring)."""
    config = dict(driver._launch_config)
    config.pop("seed_credentials", None)
    driver.teardown()
    driver.launch(config)


def _seed_two_accounts(nest_instance, second="admin"):
    """A freshly-registered regular user (row 0, active) and the nest's own
    claimed admin (row 1). Returns (seed, user_actor, admin_actor).

    ``second="user"`` seeds a second fresh regular user in the admin's row
    instead, for a journey that needs two identities but not an admin — one
    that removes the second identity after signing it in and reads its nest
    roster, which must be exact; the admin is shared by the whole run, and a
    removal whose nest half were deferred would leave a grant no teardown could
    find to retire (`e2e-launch-isolation.md`, *A discarded driver retires its
    machine*)."""
    admin_sk = nest_instance["admin"]["signing_key"]
    if second == "user":
        other = create_actor_and_register(nest_instance["port"], admin_signing_key=admin_sk)
        admin_actor, admin_secret = other["actor_id_hex"], bytes(other["signing_key"]).hex()
    else:
        admin_actor, admin_secret = bytes(admin_sk.verify_key).hex(), bytes(admin_sk).hex()
    user = create_actor_and_register(nest_instance["port"], admin_signing_key=admin_sk)
    user_actor = user["actor_id_hex"]
    url = nest_instance["url"]
    seed = build_registry_seed(
        [
            {"actor_id": user_actor, "secret_hex": bytes(user["signing_key"]).hex(),
             "nest_url": url, "device_id": "switcher-user", "handle": "user"},
            {"actor_id": admin_actor, "secret_hex": admin_secret,
             "nest_url": url, "device_id": "switcher-admin", "handle": "admin"},
        ],
        active=user_actor,
    )
    return seed, user_actor, admin_actor


def _seed_one_account(nest_instance, *, admin=False):
    """One registered account, seeded active — the pre-append single-identity
    state. ``admin=True`` seeds the nest's own claimed admin, so `admin-tab`
    tells the routed identity apart. Returns (seed, actor)."""
    admin_sk = nest_instance["admin"]["signing_key"]
    if admin:
        actor, secret = bytes(admin_sk.verify_key).hex(), bytes(admin_sk).hex()
    else:
        user = create_actor_and_register(nest_instance["port"], admin_signing_key=admin_sk)
        actor, secret = user["actor_id_hex"], bytes(user["signing_key"]).hex()
    seed = build_registry_seed(
        [{"actor_id": actor, "secret_hex": secret, "nest_url": nest_instance["url"],
          "device_id": "switcher-one", "handle": "admin" if admin else "user"}],
        active=actor,
    )
    return seed, actor


def _read_registry_index(driver):
    """The persisted ``AccountIndex`` out of the on-device credential file."""
    import json

    idx = driver.credential_map().get("fauna/index")
    if idx is None:
        return None
    return json.loads(idx) if isinstance(idx, str) else idx


def _read_store_slot(driver, key):
    """One raw logical slot out of the on-device credential file, or None."""
    return driver.credential_map().get(key)


def _wait_registry_index(driver, n, timeout=30, *, active=None):
    return wait_registry_index(
        lambda: _read_registry_index(driver), n, active=active, budget_s=timeout
    )


def _wait_switcher_count(driver, n, timeout=30):
    """Poll the switcher until it lists exactly ``n`` accounts with one active,
    RE-NAVIGATING each cycle: a switch re-enters the launch routing and remounts
    the app shell, dropping the Account sub-page."""
    deadline = time.monotonic() + timeout
    last = -1
    while time.monotonic() < deadline:
        try:
            driver.set_state(ACCOUNT_PAGE_NAV)
            driver.wait_for(SWITCHER_LIST, timeout=5)
            last = driver.count(SWITCHER_ITEM)
            if last == n and driver.count(ACTIVE_INDICATOR) == 1:
                return
        except Exception:
            pass
        time.sleep(0.5)
    raise AssertionError(
        f"switcher never reached {n} accounts (one active) within {timeout}s; "
        f"last {SWITCHER_ITEM} count={last}; error-message: {_error_line(driver)!r}"
    )


def _wait_live_switcher_count(driver, n, timeout=15):
    """Poll the CURRENT switcher page with NO re-navigation — a live in-place
    refresh is the only way a removed row can vanish from it."""
    deadline = time.monotonic() + timeout
    last = -1
    while time.monotonic() < deadline:
        try:
            last = driver.count(SWITCHER_ITEM)
            if last == n:
                return
        except Exception:
            pass
        time.sleep(0.3)
    raise AssertionError(
        f"switcher never live-refreshed to {n} items within {timeout}s "
        f"(no re-navigation); last count={last}"
    )


def _admin_tab_shown(driver) -> bool:
    """Whether the Settings root renders `admin-tab` now (navigating there
    first: the tab lives inside Settings on android)."""
    driver.set_state(SETTINGS_PAGE_NAV)
    driver.wait_for("page-heading", timeout=15)
    return driver.is_visible(ADMIN_TAB)


def _error_line(driver):
    try:
        return driver.get_text("error-message") if driver.is_visible("error-message") else ""
    except Exception as e:  # noqa: BLE001 - diagnostic only
        return f"(unreadable: {type(e).__name__}: {e})"


def _wait_on_device(store, path, what):
    wait_until(
        lambda: store.exists(path),
        APP_RELAUNCH_S,
        diagnose=lambda: f"{what}: {path} never appeared on the device",
    )


@pytest.mark.feature("multiple-accounts")
def test_android_account_switcher_lists_switches_and_reveals_admin(nest_instance, request):
    """Outcome 1: the switcher lists both identities with the active one
    marked, and tapping the admin's row switches the live session to it (no
    relaunch: `AccountSettingsVM.switchAccount` re-enters the launch routing
    in-process), which reveals the admin entry in Settings."""
    seed, user_actor, admin_actor = _seed_two_accounts(nest_instance)
    driver = _launch(request, nest_instance, seed)
    try:
        await_session_actor(driver, user_actor, budget_s=APP_RELAUNCH_S,
                            what="the launch as the user")
        assert not _admin_tab_shown(driver), (
            "a regular (non-admin) active identity must have no admin entry"
        )

        _wait_switcher_count(driver, 2, timeout=30)
        driver.click(SWITCHER_ITEM, scope=f"{SWITCHER_ITEM}[1]")
        await_session_actor(driver, admin_actor, budget_s=APP_RELAUNCH_S,
                            what="the switch to the admin")
        wait_until(
            lambda: _admin_tab_shown(driver),
            RPC_ROUNDTRIP_S,
            diagnose=lambda: "switching to the admin identity never revealed "
            f"{ADMIN_TAB}; error-message: {_error_line(driver)!r}",
        )
    finally:
        driver.teardown()


@pytest.mark.feature("multiple-accounts")
def test_android_add_account_appends_second_identity_to_registry(nest_instance, request):
    """Outcome 2, the append half: "Add account" imports a second identity,
    grows the registry 1→2, switches to it, and that identity reaches a WORKING
    session — not just a registry row (`long-term-store.md` § Multi-account
    evolution, design Decision 1; tui's twin is the shape copied).

    NOT a `serve_tls=True` nest: the wizard derives the appended account's
    `nest_url` from the typed handle's domain (uniform https by design), so the
    post-append launch-from-store is redirected through the sanctioned
    `set_provider_base_urls` seam, mirrored into the process-global store-read
    dial (`fauna_launch_machine::dial`). The store must still hold the derived
    literal: the seam redirects the socket, never the truth."""
    admin_sk = nest_instance["admin"]["signing_key"]
    second = create_actor_and_register(nest_instance["port"], admin_signing_key=admin_sk)
    second_actor = second["actor_id_hex"]
    second_secret = bytes(second["signing_key"]).hex()
    seed, first_actor = _seed_one_account(nest_instance)

    driver = _launch(request, nest_instance, seed)
    try:
        await_session_actor(driver, first_actor, budget_s=APP_RELAUNCH_S,
                            what="the launch as the first identity")
        _wait_switcher_count(driver, 1, timeout=30)

        # "Add account" → the append-mode wizard over the shared onboarding
        # machine (`AccountSettingsVM.beginAddAccount`), then the paste-secret
        # import of the pre-registered second identity.
        driver.click(ADD_BUTTON)
        driver.wait_for(IMPORT_IDENTITY_BUTTON, timeout=30)
        driver.click(IMPORT_IDENTITY_BUTTON)
        driver.wait_for(PASTE_SECRET_FIELD, timeout=15)
        driver.clear_and_type(PASTE_SECRET_FIELD, second_secret)
        driver.click(IMPORT_SUBMIT_BUTTON)
        driver.wait_for(HANDLE_INPUT, timeout=15)

        # The dial override, BEFORE the append completes (see the docstring).
        driver.set_provider_base_urls({"nest": nest_instance["url"]})

        # The AlreadyOnNest outcome, so Continue lands `WizardOutcome::LoggedIn`
        # — the append terminal (`AccountSettingsVM.completeAddAccount`). The
        # real silent-challenge path is the onboarding suites'.
        handle = f"user2@localhost:{nest_instance['port']}"
        set_handle_check_snapshot(SimpleNamespace(driver=driver), {
            "phase": "Complete",
            "outcome": {"AlreadyOnNest": {"handle_matches": True, "current_handle": handle}},
            "message": {
                "key": "onboarding.handle_check.outcome.already_on_nest_handle_matches",
                "args": {"handle": handle},
            },
            "continue_enabled": True,
            "control_checkbox_visible": False,
            "control_checkbox_checked": False,
        }, handle=handle)
        driver.click(HANDLE_CONTINUE_BUTTON)

        index = _wait_registry_index(driver, 2, timeout=30, active=second_actor)
        actor_ids = [a["actor_id"] for a in index["accounts"]]
        assert first_actor in actor_ids, (
            "the original account must survive the append (no-user-data-loss)"
        )
        assert second_actor in actor_ids, "the imported account must be appended"

        await_session_actor(driver, second_actor, budget_s=APP_RELAUNCH_S, what="the append")

        stored_url = _read_store_slot(driver, f"fauna/{second_actor}/nest_url")
        assert stored_url == f"https://localhost:{nest_instance['port']}", (
            "the appended account must persist the URL the wizard derived from "
            f"the typed handle domain, NOT the harness dial override; got {stored_url!r}"
        )
    finally:
        driver.teardown()


@pytest.mark.feature("multiple-accounts")
def test_android_add_account_abandon_recovers_prior_identity_on_relaunch(
    nest_instance, request
):
    """Outcome 2, the abandon half: importing an identity inside "Add account"
    and then leaving before Continue — the app force-stopped mid-wizard and
    started again — comes back to the identity the user had, with the registry
    unchanged (`long-term-store.md` § Multi-account evolution, append mode is
    outside moment 1). linux's shape (`test_linux_add_account_abandon_recovers_
    prior_identity_on_relaunch`): android's append wizard has no in-app cancel
    (`onboarding-cancel-button` is windows-only), so leaving the app is the
    abandon.

    The recovery is structural: an append-mode confirm writes NOTHING
    (`IdentityImportScreen`'s `confirmIdentity(validated, host.appendMode)`), so
    the imported identity lives only in the wizard machine. Dropping that
    `appendMode` argument reds step (2) here — the confirm would register and
    activate the half-onboarded identity. Account #1 is the nest's own admin, so
    `admin-tab` tells the routed identity apart from the regular #2."""
    admin_sk = nest_instance["admin"]["signing_key"]
    second = create_actor_and_register(nest_instance["port"], admin_signing_key=admin_sk)
    second_actor = second["actor_id_hex"]
    second_secret = bytes(second["signing_key"]).hex()
    seed, admin_actor = _seed_one_account(nest_instance, admin=True)

    driver = _launch(request, nest_instance, seed)
    try:
        # (1) Launch #1 as the admin.
        await_session_actor(driver, admin_actor, budget_s=APP_RELAUNCH_S,
                            what="the launch as the admin")
        _wait_switcher_count(driver, 1, timeout=30)

        # (2) Import #2 inside "Add account" — reaching HANDLE_INPUT proves the
        # import was confirmed — and check it wrote nothing anywhere.
        driver.click(ADD_BUTTON)
        driver.wait_for(IMPORT_IDENTITY_BUTTON, timeout=30)
        driver.click(IMPORT_IDENTITY_BUTTON)
        driver.wait_for(PASTE_SECRET_FIELD, timeout=15)
        driver.clear_and_type(PASTE_SECRET_FIELD, second_secret)
        driver.click(IMPORT_SUBMIT_BUTTON)
        driver.wait_for(HANDLE_INPUT, timeout=15)

        index = _read_registry_index(driver)
        assert index is not None and len(index.get("accounts", [])) == 1, (
            f"an append-mode import must not register the identity before Continue; index={index!r}"
        )
        assert index["active"] == admin_actor, (
            f"an append-mode import must not move active off the original account; index={index!r}"
        )
        holders = sorted(k for k, v in driver.credential_map().items() if v == second_secret)
        assert not holders, (
            "an append-mode import must write nothing before its own terminal; "
            f"rows holding the imported secret: {holders!r}"
        )

        # (3) Abandon: leave the app mid-wizard, start it again.
        _relaunch(driver)

        # (4) The relaunch routes on the registry's active account — the admin.
        await_session_actor(driver, admin_actor, budget_s=APP_RELAUNCH_S,
                            what="the relaunch after the abandoned append")
        wait_until(
            lambda: _admin_tab_shown(driver),
            RPC_ROUNDTRIP_S,
            diagnose=lambda: "the relaunch must route to the active admin (admin-tab), "
            f"not the abandoned #2; error-message: {_error_line(driver)!r}",
        )
        index2 = _read_registry_index(driver)
        assert [a["actor_id"] for a in index2["accounts"]] == [admin_actor], (
            f"an abandoned append leaves the registry unchanged; index={index2!r}"
        )
        assert _read_store_slot(driver, f"fauna/{second_actor}/secret") is None, (
            "the abandoned #2 must not have been materialized by the relaunch"
        )
    finally:
        driver.teardown()


@pytest.mark.feature("multiple-accounts")
def test_android_remove_account_shrinks_switcher_live(nest_instance, request):
    """Outcome 3: removing a non-active account drops its row from the SAME
    page with no re-navigation (a remove never switches), and from the
    persisted registry."""
    seed, _user_actor, admin_actor = _seed_two_accounts(nest_instance)
    driver = _launch(request, nest_instance, seed)
    try:
        driver.wait_for_state(
            lambda s: bool(s.get("session", {}).get("authenticated")), timeout=30
        )
        _wait_switcher_count(driver, 2, timeout=30)

        # The admin (row 1) is the non-active row, the only one with a remove button.
        driver.click(REMOVE_BUTTON, scope=f"{SWITCHER_ITEM}[1]")

        _wait_live_switcher_count(driver, 1, timeout=15)
        assert driver.count(ACTIVE_INDICATOR) == 1, (
            "the surviving (regular) account stays active after the removal"
        )
        index = _wait_registry_index(driver, 1, timeout=15)
        assert admin_actor not in [a["actor_id"] for a in index["accounts"]], (
            "remove must drop the account from the persisted registry index"
        )
    finally:
        driver.teardown()


@pytest.mark.feature("multiple-accounts")
def test_android_account_switch_isolates_scoped_state_on_disk(nest_instance, request):
    """Outcome 5: each identity's on-device state lives in a scope of its own
    (`account-scoping-dispositions.md` § Serialized switching). The user's
    session resolves `<filesDir>/<user>`; the switch to the admin resolves a
    DISTINCT `<filesDir>/<admin>` and leaves the user's scope where it was; no
    session ever resolves the unscoped fallback (`-unresolved-`,
    `AccountStores.UNRESOLVED_ACTOR_COMPONENT`). windows' twin is the shape."""
    seed, user_actor, admin_actor = _seed_two_accounts(nest_instance)
    driver = _launch(request, nest_instance, seed)
    try:
        await_session_actor(driver, user_actor, budget_s=APP_RELAUNCH_S,
                            what="the launch as the user")
        store = attach_scope_store("android", driver)
        user_scope = store.scope_dirs(user_actor)[0]
        _wait_on_device(store, user_scope, "the user's own account scope")

        _wait_switcher_count(driver, 2, timeout=30)
        driver.click(SWITCHER_ITEM, scope=f"{SWITCHER_ITEM}[1]")
        await_session_actor(driver, admin_actor, budget_s=APP_RELAUNCH_S,
                            what="the switch to the admin")
        admin_scope = store.scope_dirs(admin_actor)[0]
        _wait_on_device(store, admin_scope, "the admin's own account scope")

        assert user_scope != admin_scope
        assert store.exists(user_scope), (
            f"the user's scope {user_scope} must survive the switch, isolated from the admin's"
        )
        unresolved = [root / "-unresolved-" for root in store.scope_roots()]
        assert not any(store.exists(p) for p in unresolved), (
            "both sessions named their account — nothing may resolve unscoped; found "
            f"{[str(p) for p in unresolved if store.exists(p)]}"
        )
    finally:
        driver.teardown()


@pytest.mark.feature("multiple-accounts")
def test_android_removing_an_identity_deletes_its_data_and_leaves_the_others(
    nest_instance, request
):
    """Outcome 6: removing an identity deletes THAT identity's data on this
    device and leaves every other identity's data untouched (`account-scoping.md`
    § The scoping taxonomy; android's door is `AccountStores.removeAccount`, the
    shared whole-gesture `removeAccountGesture`, run by
    `AccountSettingsVM.removeAccount`, which erases over both roots
    `AndroidScopeStore` names).

    Both identities first run a real session each (the second identity's through
    a switch), so each owns a scope; the removal must then leave no scope of the
    second identity under either root while the user's scope and secret are
    still there.

    The removal also retires this phone from the second identity, as a sign-out
    does (§ Erasure follows scope → *Removing an account retires this machine
    from it, as a sign-out does*): its named row stays on the nest with its
    grant cleared. The roster read sits behind causal barriers (convention 14).

    The second identity is a fresh regular user, never the run's shared admin:
    the roster must be exact (``helpers.enrollment``), and a removal whose nest
    half were deferred would leave a grant no teardown could find to retire
    (`e2e-launch-isolation.md`, *A discarded driver retires its machine*)."""
    from nacl.signing import SigningKey

    from helpers import enrollment

    seed, user_actor, other_actor = _seed_two_accounts(nest_instance, second="user")
    nest_url = nest_instance["url"]
    other = {
        "actor_id_hex": other_actor,
        "signing_key": SigningKey(bytes.fromhex(seed[f"fauna/{other_actor}/secret"])),
    }
    driver = _launch(request, nest_instance, seed)
    try:
        await_session_actor(driver, user_actor, budget_s=APP_RELAUNCH_S,
                            what="the launch as the user")
        store = attach_scope_store("android", driver)
        user_scope = store.scope_dirs(user_actor)[0]
        _wait_on_device(store, user_scope, "the user's own account scope")

        _wait_switcher_count(driver, 2, timeout=30)
        driver.click(SWITCHER_ITEM, scope=f"{SWITCHER_ITEM}[1]")
        await_session_actor(driver, other_actor, budget_s=APP_RELAUNCH_S,
                            what="the switch to the second identity")
        other_dirs = store.scope_dirs(other_actor)
        _wait_on_device(store, other_dirs[0], "the second identity's own account scope")
        # This phone is enrolled on it now: its named row carries a grant.
        _writer, other_row, _ = enrollment.await_enrollment(
            SimpleNamespace(driver=driver), nest_url, other
        )
        assert enrollment.principals(nest_url, other).get(other_row), (
            f"the precondition: the latched row {other_row} carries this phone's grant"
        )

        _wait_switcher_count(driver, 2, timeout=30)
        driver.click(SWITCHER_ITEM, scope=f"{SWITCHER_ITEM}[0]")
        await_session_actor(driver, user_actor, budget_s=APP_RELAUNCH_S,
                            what="the switch back to the user")

        _wait_switcher_count(driver, 2, timeout=30)
        driver.click(REMOVE_BUTTON, scope=f"{SWITCHER_ITEM}[1]")
        index = _wait_registry_index(driver, 1, timeout=30)
        assert [a["actor_id"] for a in index["accounts"]] == [user_actor], (
            f"only the removed identity may leave the registry; got {index!r}"
        )

        wait_until(
            lambda: not any(store.exists(d) for d in other_dirs),
            RPC_ROUNDTRIP_S,
            diagnose=lambda: (
                "the removed identity's scope survived under: "
                f"{[str(d) for d in other_dirs if store.exists(d)]}; "
                f"error-message: {_error_line(driver)!r}"
            ),
        )
        assert store.exists(user_scope), (
            f"removing the second identity must leave the remaining identity's data untouched; {user_scope} is gone"
        )
        assert _read_store_slot(driver, f"fauna/{other_actor}/secret") is None, (
            "the removed identity's secret must be gone from this device"
        )
        assert _read_store_slot(driver, f"fauna/{user_actor}/secret"), (
            "the remaining identity's secret must survive the removal"
        )
        # The nest half: the named row stays, its grant cleared — as after a
        # sign-out (the barrier's diagnosis names every other reading).
        enrollment.await_grant_cleared(
            nest_url, other, other_row, "after removing the identity through the switcher"
        )
    finally:
        driver.teardown()


@pytest.mark.feature("multiple-accounts")
def test_android_abandoned_created_identity_never_shows_among_your_identities(
    nest_instance, request
):
    """Outcome 8: a fresh install creates identity A, walks back out of it, and
    onboards a DIFFERENT identity B instead: the switcher lists exactly one
    identity — B (`long-term-store.md` § Multi-account evolution; the retirement
    is shared Rust, `AccountRegistry::retire_superseded_provisionals`, run by B's
    commit). Moment 1 registers AND activates A at `identity-continue-button`,
    and nothing on the Back path retracts that row — so this is the case that
    can ghost. B arrives by import, never a second create."""
    admin_sk = nest_instance["admin"]["signing_key"]
    second = create_actor_and_register(nest_instance["port"], admin_signing_key=admin_sk)
    second_actor = second["actor_id_hex"]
    second_secret = bytes(second["signing_key"]).hex()

    driver = _launch(request, nest_instance, {})
    try:
        # (1) Fresh install → create A → Continue: moment 1 writes A's row.
        driver.wait_for(CREATE_IDENTITY, timeout=45)
        driver.click(CREATE_IDENTITY)
        driver.wait_for(IDENTITY_CONTINUE_BUTTON, timeout=15)
        driver.click(IDENTITY_CONTINUE_BUTTON)
        driver.wait_for(RECOVERY_KIT_SKIP_BUTTON, timeout=20)
        provisional = _wait_registry_index(driver, 1, timeout=20)
        abandoned = provisional["active"]
        assert abandoned and abandoned != second_actor, (
            "precondition: moment 1 registered and activated the created identity, "
            f"so there is a row that could ghost; index={provisional!r}"
        )

        # (2) Walk all the way back out of A to identity_choice.
        driver.click(RECOVERY_KIT_SKIP_BUTTON)
        driver.wait_for(HANDLE_ENTRY_BACK_BUTTON, timeout=20)
        driver.click(HANDLE_ENTRY_BACK_BUTTON)
        driver.wait_for(IDENTITY_CREATED_BACK_BUTTON, timeout=15)
        driver.click(IDENTITY_CREATED_BACK_BUTTON)
        driver.wait_for(IMPORT_IDENTITY_BUTTON, timeout=15)

        # (3) Onboard B for real: import, then the AlreadyOnNest outcome so
        # Continue lands LoggedIn — B's commit, the write that must retire A.
        driver.click(IMPORT_IDENTITY_BUTTON)
        driver.wait_for(PASTE_SECRET_FIELD, timeout=15)
        driver.clear_and_type(PASTE_SECRET_FIELD, second_secret)
        driver.click(IMPORT_SUBMIT_BUTTON)
        driver.wait_for(HANDLE_INPUT, timeout=15)
        driver.set_provider_base_urls({"nest": nest_instance["url"]})
        handle = f"ghost-check@localhost:{nest_instance['port']}"
        set_handle_check_snapshot(SimpleNamespace(driver=driver), {
            "phase": "Complete",
            "outcome": {"AlreadyOnNest": {"handle_matches": True, "current_handle": handle}},
            "message": {
                "key": "onboarding.handle_check.outcome.already_on_nest_handle_matches",
                "args": {"handle": handle},
            },
            "continue_enabled": True,
            "control_checkbox_visible": False,
            "control_checkbox_checked": False,
        }, handle=handle)
        driver.click(HANDLE_CONTINUE_BUTTON)
        await_session_actor(driver, second_actor, budget_s=APP_RELAUNCH_S, what="onboarding B")

        # (4) What the user sees: one identity, B, in the switcher.
        _wait_switcher_count(driver, 1, timeout=30)
        index = _read_registry_index(driver)
        actor_ids = [a["actor_id"] for a in (index or {}).get("accounts", [])]
        assert actor_ids == [second_actor], (
            f"the abandoned identity {abandoned} still shows among your identities; index={index!r}"
        )
        assert index["active"] == second_actor
    finally:
        driver.teardown()


@pytest.mark.feature("multiple-accounts")
def test_android_switching_to_an_identity_the_device_cannot_sign_in_as_is_refused(
    nest_instance, request
):
    """Outcome 9: switching to an identity this device can no longer sign in
    as is refused, says so, and leaves the user on the identity they were using
    (`long-term-store.md` § Multi-account evolution, "Activating refuses an
    account it cannot launch as").

    The seed leaves the admin listed with no `fauna/<actor>/secret` slot.
    `FfiAccountRegistry.setActive` refuses before any teardown with
    `FfiException.General`, whose `msg` IS the shared `switch_refused_copy`
    line; `AccountSettingsVM.switchAccount` puts that line — bare, not the
    exception's aggregated `msg=…` message — on `switchError`, which the
    Account page paints on `error-message`. No relaunch happens, and the live
    session and the persisted active pointer are both still the user's."""
    seed, user_actor, admin_actor = _seed_two_accounts(nest_instance)
    seed.pop(f"fauna/{admin_actor}/secret")
    driver = _launch(request, nest_instance, seed)
    try:
        await_session_actor(driver, user_actor, budget_s=APP_RELAUNCH_S,
                            what="the launch as the user")
        _wait_switcher_count(driver, 2, timeout=30)

        row = f"{SWITCHER_ITEM}[1]"
        label = driver.get_text(ITEM_HANDLE, scope=row)
        expected = S.settings.switch_refused_no_secret(account=label)
        assert_no_relaunch(
            driver,
            lambda: driver.click(SWITCHER_ITEM, scope=row),
            lambda: driver.is_visible_scrolled("error-message")
            and driver.get_text("error-message") == expected,
            what="switching to an identity with no secret on this device",
        )
        state = driver.get_state()
        assert state["session"]["actor_id"] == user_actor and state["session"][
            "authenticated"
        ], f"a refused switch must leave the user where they were; got {state.get('session')!r}"
        index = _read_registry_index(driver)
        assert index["active"] == user_actor, (
            f"a refused switch must not move the persisted active pointer; got {index!r}"
        )
        assert admin_actor in [a["actor_id"] for a in index["accounts"]], (
            "refusing the switch must not remove the unlaunchable identity"
        )
    finally:
        driver.teardown()


@pytest.mark.feature("multiple-accounts")
def test_android_switching_identity_keeps_device_settings_and_moves_drafts(
    nest_instance, request
):
    """Outcome 10: a setting that describes this device stays as it is across a
    switch, while a draft — a choice tied to an identity — follows its identity
    (`account-scoping-dispositions.md` § Serialized switching; `account-scoping.md`
    § The scoping taxonomy, class 2 vs class 1).

    android's device setting is iOS's: **back up this device's photos**
    (`photo-backup-enable-toggle` on Settings → Folders), persisted install-wide
    in `SecureStorage.autoPhotoBackup` and named by no actor scope. Photo access
    is pre-granted (`grant_photos_access`), because turning backup on asks
    through the system permission dialog, which no in-app driver reaches. The
    identity-tied half is the feed composer's draft, which rests in that
    identity's own sealed `__drafts` plane. The drafts read is the sanctioned
    side-channel verification (convention 8's carve-out); the mutations are all
    UI."""
    from clients.ws_rpc_admin_client import WsRpcAdminClient

    admin_sk = nest_instance["admin"]["signing_key"]
    admin_actor = bytes(admin_sk.verify_key).hex()
    user = create_actor_and_register(nest_instance["port"], admin_signing_key=admin_sk)
    user_actor = user["actor_id_hex"]
    seed = build_registry_seed(
        [
            {"actor_id": user_actor, "secret_hex": bytes(user["signing_key"]).hex(),
             "nest_url": nest_instance["url"], "device_id": "switcher-user", "handle": "user"},
            {"actor_id": admin_actor, "secret_hex": bytes(admin_sk).hex(),
             "nest_url": nest_instance["url"], "device_id": "switcher-admin", "handle": "admin"},
        ],
        active=user_actor,
    )
    draft = f"half-written as the user {os.getpid()}"

    def user_drafts_blob():
        with WsRpcAdminClient(
            nest_instance["url"], actor_id=user["actor_id_bytes"],
            signing_key=bytes(user["signing_key"]),
        ) as dev:
            return dev.call("fauna.drafts.get", {"path": "posts"}).get("blob")

    driver = create_driver("android")
    driver.grant_photos_access()
    driver.launch({
        "url": nest_instance["url"],
        "seed_credentials": seed,
        "environment": _seeded_environment(request, nest_instance),
        **_launch_config(request),
    })
    app = ActionLayer(driver)
    feed = app.feed

    def device_setting():
        app.backups.navigate_folders()
        driver.wait_for(PHOTO_BACKUP_TOGGLE, timeout=15)
        return driver.get_attr(PHOTO_BACKUP_TOGGLE, "checked")

    def composer_body():
        feed.navigate()
        feed.open_composer()
        return feed.compose_body_text()

    try:
        await_session_actor(driver, user_actor, budget_s=APP_RELAUNCH_S,
                            what="the launch as the user")

        # (1) As the user: a device choice and a draft. The starting value is
        # read, never assumed, so the flip is a real choice either way.
        before = device_setting()
        assert before in ("true", "false"), f"precondition: {PHOTO_BACKUP_TOGGLE} reads {before!r}"
        chosen = "false" if before == "true" else "true"
        driver.click(PHOTO_BACKUP_TOGGLE)
        wait_until(
            lambda: driver.get_attr(PHOTO_BACKUP_TOGGLE, "checked") == chosen, UI_SETTLE_S,
            diagnose=lambda: f"{PHOTO_BACKUP_TOGGLE} reads "
            f"{driver.get_attr(PHOTO_BACKUP_TOGGLE, 'checked')!r}; "
            f"error-message: {_error_line(driver)!r}",
        )
        baseline = user_drafts_blob()
        feed.navigate()
        feed.open_composer()
        driver.type_text(COMPOSE_FIELD, draft)
        wait_until(
            lambda: feed.compose_body_text() == draft, UI_SETTLE_S,
            diagnose=lambda: f"composer reads {feed.compose_body_text()!r}",
        )
        wait_until(
            lambda: user_drafts_blob() not in (None, baseline), RPC_ROUNDTRIP_S,
            diagnose=lambda: "the user's draft never reached its __drafts plane; "
            f"error-message: {_error_line(driver)!r}",
        )

        # (2) Switch to the admin.
        _wait_switcher_count(driver, 2, timeout=30)
        driver.click(SWITCHER_ITEM, scope=f"{SWITCHER_ITEM}[1]")
        await_session_actor(driver, admin_actor, budget_s=APP_RELAUNCH_S,
                            what="the switch to the admin")
        assert device_setting() == chosen, (
            "a setting that describes this device must survive the switch; "
            f"{PHOTO_BACKUP_TOGGLE} reads {driver.get_attr(PHOTO_BACKUP_TOGGLE, 'checked')!r}, "
            f"chose {chosen!r}"
        )
        assert composer_body() != draft, (
            "the user's draft must not follow the device to another identity"
        )

        # (3) Switch back: the draft is the user's again, the device choice holds.
        _wait_switcher_count(driver, 2, timeout=30)
        driver.click(SWITCHER_ITEM, scope=f"{SWITCHER_ITEM}[0]")
        await_session_actor(driver, user_actor, budget_s=APP_RELAUNCH_S,
                            what="the switch back to the user")
        feed.navigate()
        feed.open_composer()
        wait_until(
            lambda: feed.compose_body_text() == draft, RPC_ROUNDTRIP_S,
            diagnose=lambda: f"the user's composer reads {feed.compose_body_text()!r}; "
            f"error-message: {_error_line(driver)!r}",
        )
        assert device_setting() == chosen, (
            "the device setting must still hold after switching back; "
            f"{PHOTO_BACKUP_TOGGLE} reads {driver.get_attr(PHOTO_BACKUP_TOGGLE, 'checked')!r}, "
            f"chose {chosen!r}"
        )
    finally:
        driver.teardown()
