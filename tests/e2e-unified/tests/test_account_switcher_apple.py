"""tier_3 e2e: apple (macOS + iOS) multi-account account switcher (Stage 1, leg B).

The apple twin of ``test_account_switcher_linux.py`` — same journeys, same
element ids, same seeding contract (`docs/goal/architecture/long-term-store.md`
§ Multi-account evolution; switch-first, design Decision 1):

1. Seed TWO registered identities into the app's credential store as a full
   ``AccountRegistry`` state (a claimed admin + a regular user), active on the
   regular user. ``build_registry_seed`` emits the exact flat logical-key layout
   apple's ``KeychainSecretStore`` reads — ``fauna/index`` and
   ``fauna/{actor}/…`` verbatim, and nothing else: apple retired its
   pre-registry single slot (`long-term-store.md` § Downgrade mirror +
   abandoned-append recovery), so no app code participates in the seeding.
2. Launch the app → it authenticates as the active (regular) account; the
   admin shell (``admin-tab``) is absent (checked from the Settings page on
   iOS, the primary view on macOS — see the platform note below).
3. Account settings lists BOTH accounts with the active one marked.
4. Tapping the admin row switches to it with a **live in-session reconnect** (no
   relaunch) → the admin shell appears. The nav gate is keyed on the client
   INSTANCE, `SessionKey` (macOS `ContentView.task(id: SessionKey(client))`;
   iOS `MainTabView.task(id: SessionKey(client))` — both in `App/ContentView.swift`,
   not `SettingsView`, which only *renders* `appState.isAdmin`), and the
   switch's teardown nils the client before the rebuild sets it — so the
   `am-i-admin` probe re-runs and reveals `admin-tab` without a process
   restart. **Fixed row 384, was a real bug on both platforms, not test
   infra:** the previous `client != nil` key only flips across the switch's
   momentary nil phase, so a switch whose incoming client lands in the same
   view-update pass as the outgoing one's teardown never re-fired this task —
   measured live via the `[admin-gate]` log, which showed the SECOND
   `FaunaClient` build after a switch with no matching `am_i_admin=` line ever
   following it. iOS is reachable because `tearDownSessionForSwitch` leaves
   the tab view mounted across a switch (no remount to force a fresh task);
   macOS was not OBSERVED hitting it only because its own teardown happens to
   unmount+remount the whole window (`ContentView.swift`'s own comment), which
   is an accident of the current teardown shape, not a guarantee — fixed
   there too for uniformity (priority #1).

Both apple apps share the switcher (`FaunaKit/Views/AccountSwitcherSection.swift` +
`AccountSwitcherVM`) and the app-root switch/append glue (`FaunaMacApp`/`FaunaApp`
`switchAccount`/`beginAddAccount`/`completeAppendedAccount`), so one parametrized suite
covers both — priority #1. The one iOS-specific wrinkle for the POSITIVE
(post-switch) `admin-tab` read: it lives in the Settings ROOT page
(`SettingsView.swift`), not a persistent sidebar — but
`tearDownSessionForSwitch()` resets `appState.selectedSettingsPage = nil` as part of the
switch, which pops the Settings `NavigationStack` back to its root *before* the rebuild
completes, so the same `wait_for(ADMIN_TAB)` the macOS journey uses finds it there too,
with no test-level platform branch for that read. (True since row 384: the
Settings root used to be a lazy `List`, so `admin-tab` — sitting behind ~25 other rows —
never registered off-screen on iOS whatever the gate's real answer was,
apple-e2e-automation.md rule 6; fixed by converting the page to an eager
`ScrollView { VStack }`, mirroring `AdminDashboardView`'s own page-switcher conversion.)

The NEGATIVE (pre-switch) `admin-tab` read is a genuinely different shape per
platform, and DOES carry a platform branch (`SETTINGS_PAGE_NAV`, below): iOS
must navigate to Settings first (`admin-tab` sits inside a tab `TabView`
doesn't construct until visited), while macOS must NOT (`admin-tab` sits in
`SidebarView.mainList`, which `selection == .settings` REPLACES with
`SettingsNavRail` — navigating there makes admin-tab genuinely, correctly
absent regardless of the gate, which would make the check vacuous in the
OTHER direction). One more genuinely unrelated `is_mobile()` branch remains in
this file — the CFFIXED_USER_HOME launch-config wrinkle below — for a
store-location reason, not either of these.

These tests own their driver (``create_driver`` + ``launch``) rather than taking
the shared ``app`` fixture: that fixture's reset path calls
``KeychainStore.deleteAll()``, which would wipe the seed we just wrote. Same
reason the linux suite owns its own launch.

tier_3: needs a real ``fauna-nest`` binary (``nest_instance``); macOS/iOS drivers,
plus android for the two Stage-2 re-auth journeys (``STAGE2_APPS``).

**android on the Stage-2 pair.** The two re-auth journeys (decline-then-approve,
admin auto-default) are the cross-app Stage-2 contract, and android shares the
native-prompt shape they were written for: an OS dialog with no test id
(``BiometricPrompt``), replaced under e2e by a ``reauth-result`` verdict file read
per prompt. What differs is only WHERE the files live — android's credential
file and verdict sit in the app's own filesDir, which no host path reaches, so
the two store helpers below route through the bridge (``credential_map`` /
``write_reauth_verdict``) when the driver is android, and through the host file
otherwise. The rest of this module stays apple-only.
"""
from __future__ import annotations

import json
import os
import sqlite3
import tempfile
import time
import uuid
from pathlib import Path
from types import SimpleNamespace

import pytest
from nacl.signing import SigningKey

from actions import ActionLayer
from common import build_registry_seed, create_actor_and_register
from conftest import _seeded_environment
from drivers import create_driver
from drivers.machine_test_setter import set_handle_check_snapshot
from helpers.apple_admin_gate import admin_gate_log
from common.scope_store import attach_scope_store
from helpers.budgets import APP_RELAUNCH_S, RPC_ROUNDTRIP_S, UI_SETTLE_S
from helpers.instance_guard import (
    expect_launch_refused,
    is_app_alive,
    wait_alive_until,
)
from helpers.succession_ceremony import (
    SUCCESSION_AND_RELAUNCH_S,
    wait_for_successor_actor,
    kit_on_screen,
    succeed_identity_from,
)
from helpers.succession_retry import (
    assert_the_retry_affordance_matches_the_sweep,
    sweep_owes_work,
)
from helpers.waiting import (
    activation_gesture_completed,
    assert_no_relaunch,
    await_session_actor,
    session_generation,
    wait_registry_index,
    wait_until,
)
from i18n.strings import S

# apple-only: this suite regression-gates the apple-specific `AccountSwitcherVM` +
# app-root switch/append glue (Keychain-seeded, live in-session reconnect). tui's own
# account-switch journey has its own dedicated coverage (`test_account_switcher_tui.py`)
# over its own driver contract — not a silent omission. android joins for the
# Stage-2 re-auth pair only (`STAGE2_APPS`; the module docstring says why).
pytestmark = [pytest.mark.tier_3, pytest.mark.macos, pytest.mark.ios, pytest.mark.android]

APPS = ["macos", "ios"]

# The Stage-2 re-auth journeys' app set: every app whose re-auth prompt is a
# native OS dialog behind the `reauth-result` seam and whose driver this module
# can launch. (windows shares the shape and runs the same pair in
# `test_account_switcher_windows.py`.)
STAGE2_APPS = [*APPS, "android"]


@pytest.fixture
def app_name(request):
    """The app this item drives — an INDIRECT parametrization on purpose. A
    direct ``parametrize("app_name", …)`` is a pytest pseudo-fixture, which
    ``conftest._parametrized_clients`` deliberately ignores, so the item fell
    back to this module's ``[macos, ios]`` marks: ``--app macos`` kept every
    ``[ios]`` item (whose app it never prebuilt) and ``--app ios`` kept every
    macOS-only one. A real fixture makes the parametrization the deselection
    key, like every ``app``-fixture test."""
    return request.param

# tests/e2e-unified/ui.yaml § settings (switcher) + navigation (admin-tab).
SWITCHER_LIST = "account-switcher-list"
SWITCHER_ITEM = "account-switcher-item"
ITEM_HANDLE = "account-item-handle"
ACTIVE_INDICATOR = "account-item-active-indicator"
ADD_BUTTON = "account-add-button"
REMOVE_BUTTON = "account-remove-button"
REQUIRE_CONFIRM_TOGGLE = "account-require-confirm-toggle"
ADMIN_TAB = "admin-tab"
CREATE_IDENTITY = "create-identity-button"

# tests/e2e-unified/ui.yaml § onboarding — the append-mode "Add account" wizard.
IMPORT_IDENTITY_BUTTON = "import-identity-button"
PASTE_SECRET_FIELD = "paste-secret-field"
IMPORT_SUBMIT_BUTTON = "import-submit-button"
HANDLE_INPUT = "handle-input"
HANDLE_CONTINUE_BUTTON = "handle-entry-continue-button"
INVITE_SUBMIT_BUTTON = "invite-request-submit-button"
INVITE_RECHECK_BUTTON = "invite-request-recheck-button"

# Two-element nav to the Account rail sub-page of the desktop Settings shell —
# the same shape linux and web use; iOS's Settings tab consumes the identical
# {"view":"settings"},{"view":"settings","id":"account"} stack (SettingsView.swift's
# `settingsDestination(.account)`).
ACCOUNT_PAGE_NAV = {
    "nav": {"stack": [{"view": "settings"}, {"view": "settings", "id": "account"}]}
}

# The bare Settings ROOT — `ACCOUNT_PAGE_NAV` minus the Account sub-page push.
# iOS-ONLY navigation target for the admin-tab pre-checks below (genuine
# platform divergence, not a workaround — see the `driver.is_mobile()` guard at
# each call site). `admin-tab` sits INSIDE iOS's Settings page
# (SettingsView.swift), which `MainTabView` reaches via More, not a top-level
# tab, and `TabView` does not even CONSTRUCT an unvisited tab's content — so a
# gate-bypass mutant on `admin-tab`'s condition left the iOS pre-check green
# 4/4 runs when it fired right after launch, before anything had ever put the
# row in the tree to observe (confirmed live, row 384).
#
# macOS is the OPPOSITE shape: `admin-tab` sits in `SidebarView.mainList`,
# which `selection == .settings` REPLACES with `SettingsNavRail` (the sidebar-
# swap, `SidebarView.swift`'s own doc comment: "Admin and Settings are distinct
# shells... the normal nav is swapped in place for that shell's page rail").
# So navigating to Settings on macOS makes `admin-tab` genuinely absent
# whatever the gate says — measured live: the SAME mutant went from reddening
# most runs with no navigation to 0/6 once this nav call was (wrongly) applied
# to macOS too. macOS's pre-check stays exactly as it always was: checked
# on the primary view, no navigation.
SETTINGS_PAGE_NAV = {"nav": {"stack": [{"view": "settings"}]}}


def _app_launch_config(app_name, request):
    """App-specific keys `create_driver(app_name).launch()` needs — macOS wants
    only `app_path` (the bare `FaunaMacOS` executable); iOS additionally needs `udid`
    (the simctl target); android needs the APK plus the run's bridge APK and
    device (`helpers/android_device.run_launch_facts`, the one home conftest's
    own android launch reads too). Fetched via `request.getfixturevalue` (not a
    plain fixture parameter) so a `--client macos` run never triggers the iOS
    build, and vice versa."""
    if app_name == "ios":
        setup = request.getfixturevalue("ios_setup")
        return {"app_path": setup["app_path"], "udid": setup["udid"]}
    if app_name == "android":
        from helpers import android_device

        return {
            "app_path": request.getfixturevalue("android_app_path"),
            **android_device.run_launch_facts(Path(__file__).resolve().parents[3]),
        }
    return {"app_path": request.getfixturevalue("macos_app_path")}


def _seed_two_accounts(nest_instance, active="user"):
    """A claimed-admin account + a freshly-registered regular-user account, seeded
    into the registry active on the regular user (or the admin, ``active="admin"``).
    Returns (seed_map, user_actor, admin_actor). The admin is ``nest_instance``'s
    own claimed identity (so ``am-i-admin`` is true for it); the user is registered
    via the admin key."""
    admin_sk = nest_instance["admin"]["signing_key"]
    admin_actor = bytes(admin_sk.verify_key).hex()
    admin_secret = bytes(admin_sk).hex()

    user = create_actor_and_register(nest_instance["port"], admin_signing_key=admin_sk)
    user_actor = user["actor_id_hex"]
    user_secret = bytes(user["signing_key"]).hex()

    url = nest_instance["url"]
    seed = build_registry_seed(
        [
            # add order == display order → user is row 0, admin is row 1.
            {"actor_id": user_actor, "secret_hex": user_secret, "nest_url": url,
             "device_id": "switcher-user", "handle": "user"},
            {"actor_id": admin_actor, "secret_hex": admin_secret, "nest_url": url,
             "device_id": "switcher-admin", "handle": "admin"},
        ],
        active=admin_actor if active == "admin" else user_actor,
    )
    return seed, user_actor, admin_actor


def _seed_one_account(nest_instance):
    """A SINGLE registered regular-user account, seeded active — the pre-append
    single-identity state a first-run install lands in."""
    admin_sk = nest_instance["admin"]["signing_key"]
    user = create_actor_and_register(nest_instance["port"], admin_signing_key=admin_sk)
    user_actor = user["actor_id_hex"]
    user_secret = bytes(user["signing_key"]).hex()
    seed = build_registry_seed(
        [{"actor_id": user_actor, "secret_hex": user_secret,
          "nest_url": nest_instance["url"], "device_id": "switcher-user",
          "handle": "user"}],
        active=user_actor,
    )
    return seed, user_actor


def _wait_switcher_count(driver, n, timeout=30):
    """Poll the registry-backed switcher UI until it lists exactly ``n`` accounts with
    one active, RE-NAVIGATING each cycle. A switch tears the session down and rebuilds
    it (dropping the Account sub-page), and a single fire-and-wait nav can be dropped by
    a just-rebuilt window, so re-navigate until the switcher renders."""
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
        f"last {SWITCHER_ITEM} count={last}"
    )


def _wait_live_switcher_count(driver, n, timeout=15):
    """Poll the CURRENT (already-navigated) switcher page — with NO re-navigation —
    until it lists exactly ``n`` items. This is what asserts a *live* in-place refresh:
    `_wait_switcher_count` re-navigates each cycle, which would rebuild the section from
    the (already-shrunken) registry and pass even with no live update. Removing a
    non-active account never switches, so the row can only vanish via a live refresh."""
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


def _read_credential_map(driver):
    """The app's whole flat credential store — ``{logical_key: value}``, or None
    when unreadable. apple: the host-side ``{cred_dir}/keychain.json`` its
    drivers seed. android: the on-device ``FileSecretBackend`` file, which only
    the bridge reaches (``AndroidBridgeDriver.credential_map``, ``GET
    /credentials``); both key ``fauna/index`` and the per-actor slots verbatim."""
    if driver.is_android():
        return driver.credential_map()
    try:
        with open(os.path.join(driver._cred_dir, "keychain.json")) as f:
            store = json.load(f)
    except (OSError, ValueError, TypeError):
        return None
    return store if isinstance(store, dict) else None


def _read_registry_index(driver):
    """The persisted ``AccountIndex`` (``{active, accounts:[{actor_id,…}]}``) that
    apple's E2E credential store flushes to disk — the apple analog of web's
    ``localStorage['fauna/index']`` and linux's store-file read.

    Reading the store file directly asserts the append's *registry* effect independent
    of the post-switch reconnect: the append-derived nest_url is https (Pillar C
    uniform-https) which the plain-http tier_3 nest can't serve, so the switch's live
    sign-in can't complete — but the registry WRITE is client-side and does. Same file
    both apple drivers seed (`drivers/macos.py` / `drivers/ios.py` →
    ``{cred_dir}/keychain.json``); android's is on-device, read over the bridge
    (:func:`_read_credential_map`)."""
    store = _read_credential_map(driver)
    if store is None:
        return None
    idx = store.get("fauna/index")
    if idx is None:
        return None
    return json.loads(idx) if isinstance(idx, str) else idx


def _read_store_slot(driver, key):
    """One raw logical slot out of the flat ``{cred_dir}/keychain.json`` store —
    the sibling of :func:`_read_registry_index`, which reads only the
    ``fauna/index`` blob: the per-actor three-slot contract
    (``fauna/{actor}/{secret,nest_url,device_id}`` — ``long-term-store.md``
    § The three slots) lives in slots of its own, so the index alone cannot
    answer what URL an account will dial on its next launch. Apple analog of
    tui's `_read_store_slot`. Returns the raw string, or None."""
    store = _read_credential_map(driver)
    return None if store is None else store.get(key)


def _wait_registry_index(driver, n, timeout=30, *, active=None):
    """Poll this app's registry file until it lists exactly ``n`` accounts (and,
    given ``active``, names it active); return it. Shared loop:
    ``helpers.waiting.wait_registry_index``."""
    return wait_registry_index(
        lambda: _read_registry_index(driver), n, active=active, budget_s=timeout
    )


@pytest.mark.parametrize("app_name", APPS, indirect=True)
@pytest.mark.feature("multiple-accounts")
def test_apple_account_switcher_lists_switches_and_reveals_admin(
    nest_instance, app_name, request
):
    seed, _user_actor, _admin_actor = _seed_two_accounts(nest_instance)

    driver = create_driver(app_name)
    driver.launch({
        "url": nest_instance["url"],
        "seed_credentials": seed,
        "environment": _seeded_environment(request, nest_instance),
        **_app_launch_config(app_name, request),
    })
    try:
        # (2) Launched active on the regular user → authenticated, NO admin shell.
        state = driver.wait_for_state(
            lambda s: bool(s.get("session", {}).get("authenticated")), timeout=30
        )
        assert state["session"]["authenticated"] is True, (
            f"the active (regular) account must launch authenticated — the seeded "
            f"registry is the only identity source; got {state.get('session')!r}"
        )
        if driver.is_mobile():
            # See `SETTINGS_PAGE_NAV` — `admin-tab` is unreachable-but-unmounted
            # on iOS until Settings is visited at least once.
            driver.set_state(SETTINGS_PAGE_NAV)
            driver.wait_for("page-heading", timeout=15)
        assert driver.is_absent(ADMIN_TAB), (
            "a regular (non-admin) active identity must have no admin shell"
        )

        # (3) Account settings lists BOTH accounts, the active one marked.
        _wait_switcher_count(driver, 2, timeout=30)

        # (4) Tap the admin row (row 1) → live in-session reconnect → admin shell.
        # No relaunch: the switch nils the client and rebuilds it in-process, which
        # re-fires the `am-i-admin` nav gate (both apple app roots'
        # `ContentView.task(id: SessionKey(client))` — row 384).
        #
        # SCOPED, not `index=1`. On apple the flat `index=` is the *registration* order of
        # the in-process automation registry — the order rows fired `.onAppear` — and
        # SwiftUI does not promise that equals document order: these rows come from an
        # async `.task` reload, and they register bottom-up, so `index=1` resolves to the
        # ACTIVE row. That failure is silent — a real row is actuated, just not the one
        # named, and the active row's activate self-guards, so the switch becomes a no-op
        # with no error anywhere. A scoped query instead resolves by real subtree
        # containment against the index the row *declares* via `.automationScope`, so it
        # addresses the intended row. Scoped queries are the convention for any indexed
        # component, and the same `scope=` DSL works on every app.
        driver.click(SWITCHER_ITEM, scope=f"{SWITCHER_ITEM}[1]")

        # `admin-tab` now registers on iOS too (Settings' root moved from
        # a lazy `List` to an eager `ScrollView { VStack }`, SettingsView.swift,
        # apple-e2e-automation.md rule 6), so the same read the macOS journey uses
        # applies uniformly; no mobile substitute needed.
        driver.wait_for(ADMIN_TAB, timeout=45)
        assert driver.is_visible(ADMIN_TAB), (
            "switching to the admin identity must reveal the admin shell "
            "(live reconnect, no relaunch)"
        )
    finally:
        driver.teardown()


@pytest.mark.parametrize("app_name", APPS, indirect=True)
@pytest.mark.feature("multiple-accounts")
def test_apple_add_account_appends_second_identity_to_registry(
    nest_instance, app_name, request
):
    """Both halves of the append journey are asserted — the persisted
    AccountIndex (client-side, reconnect-independent) AND that the account it
    switched to reaches a *working session*. The second half was unasserted
    until 2026-08-14 (tui first, `test_account_switcher_tui.py`'s twin of this
    test, whose shape this copies): "Add account → you land in a working
    session as the new identity" was unproven, per `long-term-store.md`
    § Multi-account evolution's design Decision 1 and its own
    § Implementation status bullet naming this apple leg as owed.

    NOT a `serve_tls=True` nest — the wizard derives the incoming account's
    `nest_url` from the typed handle's domain (uniform https by ratified
    design), so the post-switch launch-from-store must be redirected via the
    sanctioned `driver.set_provider_base_urls` override seam (mirrored into
    the process-global store-read dial), never by standing up TLS here (which
    would also fight a `localhost`-vs-`127.0.0.1` authority mismatch)."""
    # (0) A pre-registered SECOND identity to import in the append wizard — the
    # "import an identity I already have" paste-secret path.
    admin_sk = nest_instance["admin"]["signing_key"]
    second = create_actor_and_register(nest_instance["port"], admin_signing_key=admin_sk)
    second_actor = second["actor_id_hex"]
    second_secret = bytes(second["signing_key"]).hex()

    seed, first_actor = _seed_one_account(nest_instance)

    driver = create_driver(app_name)
    driver.launch({
        "url": nest_instance["url"],
        "seed_credentials": seed,
        "environment": _seeded_environment(request, nest_instance),
        **_app_launch_config(app_name, request),
    })
    try:
        # (1) One seeded account, authenticated, no admin shell.
        driver.wait_for_state(
            lambda s: bool(s.get("session", {}).get("authenticated")), timeout=30
        )
        if driver.is_mobile():
            # See `SETTINGS_PAGE_NAV` — `admin-tab` is unreachable-but-unmounted
            # on iOS until Settings is visited at least once.
            driver.set_state(SETTINGS_PAGE_NAV)
            driver.wait_for("page-heading", timeout=15)
        assert driver.is_absent(ADMIN_TAB), (
            "a regular (non-admin) active identity must have no admin shell"
        )
        _wait_switcher_count(driver, 1, timeout=30)

        # (2) "Add account" → append-mode onboarding, presented as a SHEET over the
        # live session (the apple analogue of linux's separate wizard window; the
        # running client is untouched underneath). It reuses the app's one
        # `onboardingVM`, so the test agent's machine wiring already points at it —
        # no re-pointing needed.
        driver.click(ADD_BUTTON)
        driver.wait_for(IMPORT_IDENTITY_BUTTON, timeout=30)

        # (3) Import the pre-registered second identity (paste-secret path).
        driver.click(IMPORT_IDENTITY_BUTTON)
        driver.wait_for(PASTE_SECRET_FIELD, timeout=15)
        driver.clear_and_type(PASTE_SECRET_FIELD, second_secret)
        driver.click(IMPORT_SUBMIT_BUTTON)
        driver.wait_for(HANDLE_INPUT, timeout=15)

        # (3.5) Install the dial override BEFORE the append completes: the switch
        # it triggers is a launch **from the store**, and the URL the wizard is
        # about to persist is its uniform-https derivation, which this plain-HTTP
        # nest cannot serve. One gesture covers both halves — the machine's HTTP
        # providers and, mirrored, the process-global store-read dial
        # (`fauna_launch_machine::dial`) the post-append switch resolves through
        # — the seam exists precisely because a store-read launch has no
        # onboarding machine in scope, which is apple's case exactly. Do NOT
        # reach for a `serve_tls=True` nest instead (see docstring).
        driver.set_provider_base_urls({"nest": nest_instance["url"]})

        # (4) Inject the AlreadyOnNest (welcome-back) handle-check outcome so Continue
        # lands `WizardOutcome::LoggedIn` — the trigger for the append divergence
        # (`addAccount` + switch). The real silent-challenge → AlreadyOnNest path is
        # covered by the onboarding suites; here we drive the NOVEL append branch.
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

        # (5) The append grew the registry 1→2 and switched to the new account, with
        # the original preserved (no-user-data-loss). The append's moment 1 wrote
        # nothing, so this is the append terminal registering the identity it read
        # off the wizard machine (`completeAppendedAccount`) — a terminal that read
        # the store instead would find no new identity to register.
        index = _wait_registry_index(driver, 2, timeout=30, active=second_actor)
        actor_ids = [a["actor_id"] for a in index["accounts"]]
        assert first_actor in actor_ids, (
            "the original account must survive the append (no-user-data-loss)"
        )
        assert second_actor in actor_ids, "the imported account must be appended"
        assert index["active"] == second_actor, (
            "the append switches to the newly-added account (design Decision 1)"
        )

        # (6) …and that account reaches a WORKING session, not just a registry
        # row. This is the half the append test never asserted; it is the whole
        # point of "Add account".
        await_session_actor(
            driver, second_actor, budget_s=APP_RELAUNCH_S, what="the append"
        )

        # (7) The seam redirects the SOCKET, never the truth: what the append
        # persisted for the new account is still the literal derived from the
        # typed handle domain — an https URL this plain-HTTP nest never served.
        # Asserting the store is what keeps (6) honest: an override that leaked
        # into persistence would make every later launch, in a release build
        # with no override installed, dial a torn-down fixture.
        stored_url = _read_store_slot(driver, f"fauna/{second_actor}/nest_url")
        assert stored_url == f"https://localhost:{nest_instance['port']}", (
            "the appended account must persist the URL the wizard derived from "
            "the typed handle domain, NOT the harness dial override; got "
            f"{stored_url!r}"
        )
    finally:
        driver.teardown()


@pytest.mark.parametrize("app_name", APPS, indirect=True)
@pytest.mark.feature("multiple-accounts")
def test_apple_add_account_pending_invite_submit_switches_the_live_session(
    nest_instance, app_name, request
):
    """A pending-invite submit during "Add account" moves the registry's active
    pointer AND the running session together — never the registry alone.

    `onboarding.md` § Multi-account: the append wizard's pending-invite state is
    not an exit, so "the append glue adopts on the submit return — register the
    append identity in the account registry, write its per-actor pending-invite
    slot, switch to it". Apple ran the shared writer
    (`persistPendingInvite`: `add_account` + `set_active` + the slot) and
    stopped there: the registry named the still-pending identity while the live
    session kept serving the old one, with the "Add account" sheet still up over
    it — registry and session disagreeing about who the user is.

    The joiner is a fresh identity the nest has never seen, so a real invite
    request is the only way onto this claimed nest. It is driven through the
    app's own UI (convention 8) up to the one hop a test nest forces —
    `navigate_to_invite_request_for_known_nest`, because a test nest is not
    DNS-discoverable and the wizard cannot find it by handle (the same hop
    `test_pending_invite_journey.py` makes).

    **Pending is not connected.** The new account has no `nest_url` yet, so
    unlike the `LoggedIn` append above there is no working session to reach and
    `await_session_actor` cannot be the witness. "The session agrees with the
    registry" reads instead as: the outgoing identity's session is GONE (one
    counted teardown, no authenticated actor) and the new account's own launch
    surface — the invite page, hydrated from its per-actor slot — is what
    renders. Nothing here waits on a clock: every wait is a deadline poll on
    state the app actually reaches (convention 14)."""
    seed, first_actor = _seed_one_account(nest_instance)

    joiner_key = SigningKey.generate()
    joiner_secret = bytes(joiner_key).hex()
    joiner_actor = bytes(joiner_key.verify_key).hex()
    handle = f"addpending-{int(time.time() * 1000)}"

    driver = create_driver(app_name)
    driver.launch({
        "url": nest_instance["url"],
        "seed_credentials": seed,
        "environment": _seeded_environment(request, nest_instance),
        **_app_launch_config(app_name, request),
    })
    try:
        # (1) One seeded account, authenticated. The generation read here is the
        # baseline the adoption's teardown is counted against.
        driver.wait_for_state(
            lambda s: bool(s.get("session", {}).get("authenticated")), timeout=30
        )
        _wait_switcher_count(driver, 1, timeout=30)
        generation_before = session_generation(driver)
        assert generation_before is not None, (
            "the app must publish `session_generation` (convention 14) — without it "
            "this test cannot tell a switched session from an untouched one"
        )

        # (2) "Add account" → append-mode onboarding → import the joiner's secret
        # (the paste-secret path) → the handle page. In append mode that import
        # writes nothing (`confirmIdentity(append: true)`); it registers nothing.
        driver.click(ADD_BUTTON)
        driver.wait_for(IMPORT_IDENTITY_BUTTON, timeout=30)
        driver.click(IMPORT_IDENTITY_BUTTON)
        driver.wait_for(PASTE_SECRET_FIELD, timeout=15)
        driver.clear_and_type(PASTE_SECRET_FIELD, joiner_secret)
        driver.click(IMPORT_SUBMIT_BUTTON)
        driver.wait_for(HANDLE_INPUT, timeout=15)

        # (3) Hop to `invite_request` for this claimed nest, then the REAL submit —
        # an `invite_request.submit` over the anonymous WS to `nest_instance`.
        driver.call_machine_method(
            "navigate_to_invite_request_for_known_nest",
            json.dumps([nest_instance["url"], handle]),
        )
        driver.wait_for(INVITE_SUBMIT_BUTTON, timeout=20)
        driver.click(INVITE_SUBMIT_BUTTON)

        # (4) The registry half — the shared writer's own contract, already true
        # before the switch existed: the joiner is registered and active, the
        # original survives (no-user-data-loss), and the per-actor pending-invite
        # slot is written.
        index = _wait_registry_index(driver, 2, timeout=30, active=joiner_actor)
        actor_ids = [a["actor_id"] for a in index["accounts"]]
        assert first_actor in actor_ids, (
            "the original account must survive the append (no-user-data-loss)"
        )
        assert joiner_actor in actor_ids, "the joiner must be registered on the submit return"
        assert index["active"] == joiner_actor, (
            "the pending-invite submit activates the joiner (onboarding.md § Multi-account)"
        )
        slot = _read_store_slot(driver, f"fauna/{joiner_actor}/pending_invite")
        assert slot, (
            "the joiner's per-actor pending-invite slot must be written on the "
            f"submit return; registry index={index!r}"
        )

        # (5) THE assertion this test exists for: the live session followed the
        # registry. The adoption's switch tears the old session down through
        # `tearDownSessionForSwitch`, which counts itself at its top, so a
        # generation that never moves means the registry changed under a session
        # that never noticed.
        def _diagnose():
            state = driver.get_state() or {}
            return (
                f"session={state.get('session')!r}, "
                f"generation {session_generation(driver)!r} (was {generation_before}), "
                f"registry active={(_read_registry_index(driver) or {}).get('active')!r} "
                f"(joiner={joiner_actor!r}, outgoing={first_actor!r})"
            )

        wait_until(
            lambda: (session_generation(driver) or 0) > generation_before,
            APP_RELAUNCH_S,
            diagnose=_diagnose,
        )
        state = driver.wait_for_state(
            lambda s: not s.get("session", {}).get("authenticated"),
            timeout=30,
        )
        assert state.get("session", {}).get("actor_id") != first_actor, (
            "the running session still names the outgoing identity while the "
            f"registry's active account is the joiner: {_diagnose()}"
        )

        # (6) …and what renders is the joiner's OWN launch surface: the invite
        # page in `PendingReview`, hydrated from the slot written in (4) — the
        # recheck affordance exists only in that state.
        driver.wait_for(INVITE_RECHECK_BUTTON, timeout=30)
    finally:
        driver.teardown()


@pytest.mark.parametrize("app_name", APPS, indirect=True)
def test_apple_add_account_import_does_not_touch_registry_before_completion(
    nest_instance, app_name, request
):
    """Moment 1 of the append wizard must NOT register or activate anything.

    `long-term-store.md` § Multi-account evolution, doc on
    `persist_confirmed_identity`: *"Append mode does not come here... routing it
    through the registry would register a half-account and switch `active` to
    it mid-session, before the user has finished adding it."* web shipped
    exactly that omission (fixed,
    `test_web_add_account_abandon_recovers_prior_identity_on_current_load`) —
    this is apple's twin, checking the SAME moment web got wrong: right after
    the import step (before Continue → `LoggedIn`), the registry must be
    UNCHANGED. A wizard that registered here would already show 2 accounts and
    a switched `active` at this checkpoint, well before the user could even
    abandon it.
    """
    admin_sk = nest_instance["admin"]["signing_key"]
    second = create_actor_and_register(nest_instance["port"], admin_signing_key=admin_sk)
    second_actor = second["actor_id_hex"]
    second_secret = bytes(second["signing_key"]).hex()

    seed, first_actor = _seed_one_account(nest_instance)

    driver = create_driver(app_name)
    driver.launch({
        "url": nest_instance["url"],
        "seed_credentials": seed,
        "environment": _seeded_environment(request, nest_instance),
        **_app_launch_config(app_name, request),
    })
    try:
        driver.wait_for_state(
            lambda s: bool(s.get("session", {}).get("authenticated")), timeout=30
        )
        _wait_switcher_count(driver, 1, timeout=30)

        # Add account → import the second identity — moment 1 of the append
        # wizard. Stop here, BEFORE handle-entry's Continue → LoggedIn, which
        # is the only step allowed to register+switch (`completeAppendedAccount`).
        driver.click(ADD_BUTTON)
        driver.wait_for(IMPORT_IDENTITY_BUTTON, timeout=30)
        driver.click(IMPORT_IDENTITY_BUTTON)
        driver.wait_for(PASTE_SECRET_FIELD, timeout=15)
        driver.clear_and_type(PASTE_SECRET_FIELD, second_secret)
        driver.click(IMPORT_SUBMIT_BUTTON)
        driver.wait_for(HANDLE_INPUT, timeout=15)

        # The import committed synchronously before this page could render, so
        # a premature registry write would already be visible here — no poll.
        index = _read_registry_index(driver)
        assert index and len(index.get("accounts", [])) == 1, (
            f"the append must not grow the registry before Continue: {index!r}"
        )
        assert index["active"] == first_actor, (
            f"the append must not switch `active` before Continue: {index!r}"
        )
        assert second_actor not in [a["actor_id"] for a in index["accounts"]], (
            "the imported (not-yet-completed) identity must not be registered yet"
        )
    finally:
        driver.teardown()


def _seed_flat_mls_store(path, marker):
    """Write a flat MLS store at the base — a real SQLite file carrying one
    recognizable row.

    ``marker`` is what makes the assertions strong: "no account inherited it"
    means this row is absent from every scoped store — not merely that some file
    of the right name exists or doesn't.
    """
    path.parent.mkdir(parents=True, exist_ok=True)
    conn = sqlite3.connect(path)
    try:
        conn.execute("CREATE TABLE IF NOT EXISTS pre_upgrade_state (marker TEXT)")
        conn.execute("INSERT INTO pre_upgrade_state (marker) VALUES (?)", (marker,))
        conn.commit()
    finally:
        conn.close()


def _mls_markers(path):
    """The markers in an MLS store at ``path``: ``None`` when the file does not
    exist at all, else the list of rows (``[]`` for a store that exists but never
    inherited the seeded state — a freshly-created one)."""
    if not path.exists():
        return None
    conn = sqlite3.connect(f"file:{path}?mode=ro", uri=True)
    try:
        return [r[0] for r in conn.execute("SELECT marker FROM pre_upgrade_state")]
    except sqlite3.DatabaseError:
        return []
    finally:
        conn.close()


@pytest.mark.parametrize("app_name", APPS, indirect=True)
@pytest.mark.feature("multiple-accounts")
def test_apple_mls_store_is_account_scoped_and_never_adopts_a_flat_store(
    nest_instance, app_name, request, tmp_path
):
    """The isolation contract for apple's MLS store (`account-scoping.md`
    § Serialized switching — completing the isolation contract; gap-ledger row
    "apple").

    Before scoping, every account of a switching install opened ONE
    ``Application Support/Fauna/conv-mls.db``, so account B rendered and mutated
    account A's conversation state. The remediation is per-account paths
    (``Fauna/<actor-id-hex>/mls.db``); the first-adopter hand-off of the flat
    layout was removed by the compat-remnant sweep (`version-compatibility.md`
    § Dimension 2), so this is also that removal's end-to-end refusal pin:

    1. **Never adopted** — the active account's scoped store does not carry the
       flat store's row.
    2. **Untouched** — the flat file is left exactly as it was, and no
       first-adopter marker is written beside it.
    3. **No leak** — switching to the second account gives it a store that does
       NOT carry the flat state either.

    Both apple apps resolve the store through the same shared-Rust derivation
    (`account_state_dir` via FaunaKit's `AccountStateDir`), and both drivers
    expose the same
    ``app_support_dir()`` + ``seed_app_support`` seam, so there is no platform
    branch here — priority #1.
    """
    seed, user_actor, admin_actor = _seed_two_accounts(nest_instance)
    marker = f"pre-upgrade-{uuid.uuid4().hex[:8]}"
    flat_seed = tmp_path / "conv-mls.db"
    _seed_flat_mls_store(flat_seed, marker)

    driver = create_driver(app_name)
    launch_config = {
        "url": nest_instance["url"],
        "seed_credentials": seed,
        "seed_app_support": {"Fauna/conv-mls.db": str(flat_seed)},
        "environment": _seeded_environment(request, nest_instance),
        **_app_launch_config(app_name, request),
    }
    if not driver.is_mobile():
        # macOS resolves `.applicationSupportDirectory` through CFFIXED_USER_HOME,
        # which the driver pins only for a relocated launch — iOS needs no
        # equivalent (its store lives in the per-launch simulator container).
        launch_config["home"] = str(tmp_path / "home")
    driver.launch(launch_config)
    try:
        app_support = driver.app_support_dir()
        assert app_support, (
            "this test asserts on-disk account-scoped state, so the launch must have "
            "relocated Application Support away from the real profile"
        )
        fauna_dir = Path(app_support) / "Fauna"
        flat = fauna_dir / "conv-mls.db"

        driver.wait_for_state(
            lambda s: s.get("session", {}).get("actor_id") == user_actor, timeout=30
        )

        # (1) The ACTIVE account opened its own scoped store (at login), and it
        # carries none of the flat store's state.
        user_db = fauna_dir / user_actor / "mls.db"
        deadline = time.monotonic() + 30
        while time.monotonic() < deadline and not user_db.exists():
            time.sleep(0.5)
        assert user_db.exists(), (
            f"the active account must open its scoped MLS store at {user_db} — "
            f"without it the never-adopted assertion below would be vacuous"
        )
        assert _mls_markers(user_db) != [marker], (
            f"the flat store must never be adopted into {user_db}"
        )

        # (2) The flat file is not the app's to touch, and no marker is stamped.
        assert _mls_markers(flat) == [marker], "the flat file is left untouched"
        assert not (fauna_dir / "state-owner").exists(), (
            "no first-adopter marker is ever written"
        )

        # The cross-process mutation lock is ADOPTED, not merely available
        # (`long-term-store.md` § Multi-account evolution → Cross-process mutation
        # lock). Every registry mutator is a read-modify-write of one `fauna/index`
        # blob, so two apple processes writing at once can lose an account; the
        # lock is what serializes them, and a client that forgot to construct with
        # it would pass every other assertion in this file. The lock file is
        # created lazily on the first acquire — login's `save_authenticated`
        # already ran one — and it must sit at the INSTALL-scoped base beside the
        # per-account dirs, never inside one: it guards the index the accounts
        # share.
        lock_file = fauna_dir / "account-registry.lock"
        deadline = time.monotonic() + 20
        while time.monotonic() < deadline and not lock_file.exists():
            time.sleep(0.5)
        assert lock_file.is_file(), (
            f"apple must construct its registry with the cross-process mutation "
            f"lock, which stamps {lock_file} on its first mutation; its absence "
            f"means the registry was built unlocked"
        )
        assert not (fauna_dir / user_actor / "account-registry.lock").exists(), (
            "the mutation lock is install-scoped — an account-scoped lock file "
            "would let two instances mutate the shared index side by side"
        )

        # The in-app segment-backup upload driver is RETIRED (slice-5 flip,
        # 2026-08-15 — `docs/goal/behavior/backup-restore.md` § Background
        # Tasks → *Flip status (slice 5)*): the source nest has been the
        # segment-backup writer since 2026-07-24, so a fresh account never
        # opens a segment-backup state dir at either the legacy flat path or
        # an account-scoped one. This is the retirement witness's e2e half
        # (Swift peer: `FaunaKitTests/RetiredSegmentBackupDriverTests.swift`)
        # — an app that resurrected the driver could only double-write or
        # diverge from the nest.
        assert not (fauna_dir / "segment-backup").exists(), (
            "nothing may still write the retired actor-blind flat segment-backup dir"
        )
        assert not (fauna_dir / user_actor / "segment-backup").exists(), (
            "nothing may still write the retired account-scoped segment-backup dir"
        )

        # (3) Switch to the second account — it must NOT inherit account A's state.
        _wait_switcher_count(driver, 2, timeout=30)
        driver.click(SWITCHER_ITEM, scope=f"{SWITCHER_ITEM}[1]")
        driver.wait_for_state(
            lambda s: s.get("session", {}).get("actor_id") == admin_actor, timeout=45
        )
        admin_dir = fauna_dir / admin_actor
        deadline = time.monotonic() + 20
        while time.monotonic() < deadline and not admin_dir.is_dir():
            time.sleep(0.5)
        # Non-vacuity first: the switched-in account really did resolve a scoped
        # home of its own. Without this, "B has no store carrying A's marker" would
        # also pass if B resolved no store at all.
        assert admin_dir.is_dir(), (
            f"the switched-in account must resolve its OWN scoped state dir at "
            f"{admin_dir} — a switch that resolves none would make the leak "
            f"assertion below vacuous"
        )
        admin_db = admin_dir / "mls.db"
        assert _mls_markers(admin_db) != [marker], (
            f"the second account must NEVER read the first's MLS state — a store at "
            f"{admin_db} carrying {marker!r} means the accounts share a path again"
        )
        assert user_db.exists(), (
            "and the switched-away account's own store is kept by the switch"
        )
    finally:
        driver.teardown()


def _write_reauth_verdict(driver, verdict):
    """Stage-2 e2e seam (FaunaKit / android ``AccountReauth``): when launched on an
    e2e credential store the app reads ``reauth-result`` beside it instead of
    showing the real native prompt (LAContext / BiometricPrompt). The file is read
    per prompt, so one app session covers both verdicts. ``None`` removes the
    file — ABSENT reads as decline (fail-closed). apple's is the host-side
    ``{cred_dir}/reauth-result``; android's sits in the app's filesDir, so the
    bridge writes it (``AndroidBridgeDriver.write_reauth_verdict``)."""
    if driver.is_android():
        driver.write_reauth_verdict(verdict)
        return
    path = os.path.join(driver._cred_dir, "reauth-result")
    if verdict is None:
        try:
            os.remove(path)
        except FileNotFoundError:
            pass
    else:
        with open(path, "w") as f:
            f.write(verdict)


@pytest.mark.parametrize("app_name", STAGE2_APPS, indirect=True)
@pytest.mark.feature("multiple-accounts")
def test_apple_require_confirm_gates_switch_decline_then_approve(
    nest_instance, app_name, request
):
    """Stage 2 (`long-term-store.md` § Multi-account evolution, ratified 2026-07-16):
    flagging an account via its `account-require-confirm-toggle` makes activating it
    demand the native re-auth prompt (e2e seam file, `AccountReauth`). Declining is a
    PURE NO-OP — registry untouched, no teardown, the current account stays active —
    and approving completes the same switch journey the unflagged path takes. The
    flag write is driven through the UI (testing.md point 8) and asserted against
    the persisted registry index (headless observable)."""
    seed, user_actor, admin_actor = _seed_two_accounts(nest_instance)

    driver = create_driver(app_name)
    driver.launch({
        "url": nest_instance["url"],
        "seed_credentials": seed,
        "environment": _seeded_environment(request, nest_instance),
        **_app_launch_config(app_name, request),
    })
    try:
        driver.wait_for_state(
            lambda s: bool(s.get("session", {}).get("authenticated")), timeout=30
        )
        _wait_switcher_count(driver, 2, timeout=30)

        # (1) Flag the ADMIN row (row 1) through its own toggle — the UI write path.
        # Setting the flag itself never prompts; only activation does.
        driver.click(REQUIRE_CONFIRM_TOGGLE, scope=f"{SWITCHER_ITEM}[1]")

        # ... and the flag reaches the PERSISTED index blob, not just the view.
        deadline = time.monotonic() + 10
        flagged = None
        while time.monotonic() < deadline:
            index = _read_registry_index(driver) or {}
            flagged = {
                a["actor_id"]: a.get("require_confirm_to_activate", False)
                for a in index.get("accounts", [])
            }
            if flagged.get(admin_actor):
                break
            time.sleep(0.3)
        assert flagged and flagged.get(admin_actor) is True, (
            f"the row toggle must persist require_confirm_to_activate for the admin "
            f"account; got {flagged!r}"
        )

        # (2) DECLINE: no verdict file — absent reads as decline (fail-closed), the
        # strictest arm of the seam. The tap must be a PURE no-op: no registry
        # mutation, no teardown, the regular user stays authenticated and the
        # switcher page stays rendered (a teardown would blank it).
        #
        # Convention 14: the absence is anchored CAUSALLY, not to a settle window
        # (`assert_no_relaunch`). A teardown here would be counted synchronously
        # by `SessionGeneration.recordTeardown()` at the top of the app's
        # `tearDownSessionForSwitch`, so once the gate has finished evaluating
        # and the barrier has drained the main queue behind it, an unchanged
        # `session_generation` PROVES no relaunch was initiated — where
        # `sleep(3.0)` could only report that none had landed yet.
        #
        # `settled` is the activation-gesture counter because apple's re-auth
        # prompt is the OS sheet: nothing renders, so there is no prompt-close to
        # watch the way tui/linux/web have
        # (`fauna_e2e_agent::ACTIVATION_GESTURES_KEY`, whose windows leg landed
        # the same day — same wall, same answer, one name).
        #
        # ⚠ `activation_gesture_completed(driver)` baselines the counter WHERE IT
        # IS WRITTEN, in the argument list: Python evaluates it before entering
        # `assert_no_relaunch`, i.e. before the trigger runs. Its precondition —
        # no other activation gesture in flight — holds here because the
        # preceding step positively waited out the flag write.
        _write_reauth_verdict(driver, None)
        assert_no_relaunch(
            driver,
            lambda: driver.click(SWITCHER_ITEM, scope=f"{SWITCHER_ITEM}[1]"),
            activation_gesture_completed(driver),
            budget_s=15.0,
            what="tapping a flagged row with the re-auth DECLINED",
        )
        state = driver.get_state()
        assert state["session"]["actor_id"] == user_actor, (
            "a DECLINED re-auth must leave the original account active (pure no-op); "
            f"got {state.get('session')!r}"
        )
        assert state["session"]["authenticated"] is True, (
            "a declined re-auth must not tear the live session down"
        )
        assert driver.count(SWITCHER_ITEM) == 2 and driver.count(ACTIVE_INDICATOR) == 1, (
            "the switcher page must survive a declined re-auth un-rebuilt"
        )
        index = _read_registry_index(driver)
        assert index["active"] == user_actor, (
            "a declined re-auth must leave the persisted registry untouched"
        )

        # (3) APPROVE: the verdict file confirms → the very same tap completes the
        # switch (live in-session reconnect, exactly the unflagged journey).
        _write_reauth_verdict(driver, "approve")
        driver.click(SWITCHER_ITEM, scope=f"{SWITCHER_ITEM}[1]")
        state = driver.wait_for_state(
            lambda s: s.get("session", {}).get("actor_id") == admin_actor, timeout=45
        )
        assert state["session"]["authenticated"] is True, (
            "an APPROVED re-auth must complete the switch to the flagged account; "
            f"got {state.get('session')!r}"
        )
        # Same admin-shell reveal the unflagged journey asserts — uniform on both
        # platforms since row 384 (see journey 1's comment).
        driver.wait_for(ADMIN_TAB, timeout=45)
    finally:
        driver.teardown()


def _admin_gate_log(driver, limit=12):
    """The app's own admin-gate chain lines — see `helpers/apple_admin_gate`, which
    owns the reader (and is pinned headlessly by `test_apple_admin_gate_log.py`).
    Wrapped here only to resolve the driver's per-launch app-support dir."""
    try:
        support = driver.app_support_dir()
    except Exception as exc:
        return [f"(app_support_dir() unavailable: {exc!r})"]
    return admin_gate_log(support, limit=limit)


def _wait_flag(driver, actor, want, key="require_confirm_to_activate", timeout=20):
    """Poll the persisted index until ``actor``'s ``key`` reads ``want``; return the
    final per-actor map for the failure message.

    On failure this reports WHICH LINK of the auto-default chain broke, because each
    needs a completely different fix and the bare flag map cannot tell them apart
    (e2e rule 6 — a failure must diagnose itself). The chain is ``refreshAdminStatus``
    → ``api.amIAdmin()`` → *(only if true)* →
    ``FaunaAccounts.autoEnableRequireConfirmForActiveAdmin()`` → registry write.

    Two witnesses, in decreasing order of authority:

    1. **The app's own log lines** (`_admin_gate_log`) — the only one that covers the
       whole chain, and the only one that distinguishes "the probe never ran" from "it
       ran and returned false". Every link logs: `[admin-gate] no client → isAdmin=false`,
       `[admin-gate] am_i_admin=<bool>`, `[admin-auto-default] …auto-enabled…` /
       `…refused…` / `…no active account…`.
    2. **`session.is_admin` in the state snapshot** — the value the probe last assigned
       (`FaunaClient.refreshAdminStatus`'s return). Splits upstream from downstream in
       one bit, on BOTH apps.

    ``admin-tab`` visibility is deliberately NOT used, on either platform (both register
    it reliably since row 384 fixed iOS's lazily rendered Settings ROOT `List` —
    apple-e2e-automation.md rule 6). It answers only `isAdmin`, the UPSTREAM half of the
    chain — it cannot tell whether the SEPARATE downstream write
    (``autoEnableRequireConfirmForActiveAdmin``) also ran, which is exactly the
    distinction this helper exists to make. Reading an upstream/downstream verdict out of
    a single visibility bit is exactly the wrong inference this helper used to invite.
    """
    deadline = time.monotonic() + timeout
    flags = None
    while time.monotonic() < deadline:
        index = _read_registry_index(driver) or {}
        flags = {a["actor_id"]: a.get(key, False) for a in index.get("accounts", [])}
        if flags.get(actor) is want:
            return flags
        time.sleep(0.3)
    log_lines = _admin_gate_log(driver)
    try:
        is_admin = driver.get_state().get("session", {}).get("is_admin")
        if is_admin is True:
            gate = (
                "session.is_admin=True → the am-i-admin probe DID observe admin, so the "
                "break is DOWNSTREAM: autoEnableRequireConfirm / the registry write"
            )
        elif is_admin is False:
            gate = (
                "session.is_admin=False → the probe never observed admin, so the break is "
                "UPSTREAM of the auto-default (probe not fired, nil client, or amIAdmin() "
                "false/failing) — the log lines below say which"
            )
        else:
            gate = (
                f"session.is_admin absent from the state snapshot (got {is_admin!r}) — the "
                "app did not publish this field; fall back to the log lines below"
            )
    except Exception as exc:  # never let the diagnostic mask the real assert
        # A dead bridge is itself the finding, but "bridge dead" conflates two very
        # different faults, so resolve it here against the app's REAL host pid (the
        # apple drivers track it): process GONE ⇒ the app terminated/crashed and no
        # assert past this point ever had a chance; process ALIVE ⇒ the app is up but
        # its in-process agent stopped serving (hang/suspension/main-actor starvation).
        pid, liveness = getattr(driver, "_app_pid", None), "app pid unknown"
        if pid is not None:
            try:
                os.kill(pid, 0)
                liveness = f"app pid {pid} is ALIVE → agent stopped serving (hang/suspension), app did NOT crash"
            except OSError:
                liveness = f"app pid {pid} is GONE → the app process TERMINATED (crash/kill) mid-test"
        gate = f"state snapshot unreadable ({exc!r}); {liveness}"
    rendered = "\n".join(f"    {ln}" for ln in log_lines)
    raise AssertionError(
        f"{key} for {actor} never became {want} within {timeout}s; last {flags!r}\n"
        f"  diagnosis: {gate}\n"
        f"  app admin-gate log (oldest first):\n{rendered}"
    )


@pytest.mark.feature("multiple-accounts")
@pytest.mark.parametrize("app_name", STAGE2_APPS, indirect=True)
def test_apple_admin_auto_default_flags_admin_and_explicit_off_sticks(
    nest_instance, app_name, request
):
    """The admin auto-default (`long-term-store.md` § Multi-account evolution —
    "Default off; a client turns it on for its admin identity"): launching
    authenticated as the ADMIN account auto-enables its require-confirm flag at the
    am-i-admin observation, with NO user tap (asserted against the persisted index;
    the regular account stays unflagged). And the user's explicit OFF sticks: turn
    the toggle off, re-trigger the observation by switching away and back (both
    switches unflagged → no confirm involved), and the auto-default must NOT
    re-flip it — `require_confirm_user_set` pins the user's choice."""
    seed, user_actor, admin_actor = _seed_two_accounts(nest_instance, active="admin")

    driver = create_driver(app_name)
    driver.launch({
        "url": nest_instance["url"],
        "seed_credentials": seed,
        "environment": _seeded_environment(request, nest_instance),
        **_app_launch_config(app_name, request),
    })
    try:
        driver.wait_for_state(
            lambda s: s.get("session", {}).get("actor_id") == admin_actor, timeout=30
        )
        # On iOS the am-i-admin probe lives in the Settings shell — navigating to
        # the Account page (which the journey needs anyway) mounts it on both.
        _wait_switcher_count(driver, 2, timeout=30)

        # (1) The observation auto-enabled the ADMIN row's flag — no tap anywhere.
        flags = _wait_flag(driver, admin_actor, True, timeout=30)
        assert flags.get(user_actor) is False, (
            f"the auto-default must only flag the admin identity; got {flags!r}"
        )

        # (2) Explicit user OFF (the admin is the ACTIVE row 1 — its toggle is
        # tappable even on the active row).
        driver.click(REQUIRE_CONFIRM_TOGGLE, scope=f"{SWITCHER_ITEM}[1]")
        _wait_flag(driver, admin_actor, False, timeout=10)
        index = _read_registry_index(driver)
        admin_entry = next(a for a in index["accounts"] if a["actor_id"] == admin_actor)
        assert admin_entry.get("require_confirm_user_set") is True, (
            "the toggle write must mark the flag user-set (the OFF-sticks pin); "
            f"got {admin_entry!r}"
        )

        # (3) Re-trigger the observation: switch to the user (admin is unflagged
        # now, the user always was — no confirm anywhere), then back to the admin.
        driver.click(SWITCHER_ITEM, scope=f"{SWITCHER_ITEM}[0]")
        driver.wait_for_state(
            lambda s: s.get("session", {}).get("actor_id") == user_actor, timeout=45
        )
        _wait_switcher_count(driver, 2, timeout=30)
        driver.click(SWITCHER_ITEM, scope=f"{SWITCHER_ITEM}[1]")
        driver.wait_for_state(
            lambda s: s.get("session", {}).get("actor_id") == admin_actor, timeout=45
        )
        # The rebuilt session re-probes am-i-admin (macOS ContentView.task /
        # iOS SettingsView.task) — give the refused auto-default a bounded window,
        # then pin the user's OFF.
        time.sleep(4.0)
        index = _read_registry_index(driver)
        admin_entry = next(a for a in index["accounts"] if a["actor_id"] == admin_actor)
        assert admin_entry.get("require_confirm_to_activate") is False, (
            "an explicit user OFF must stick against the admin auto-default across "
            f"a fresh am-i-admin observation; got {admin_entry!r}"
        )
    finally:
        driver.teardown()


@pytest.mark.parametrize("app_name", APPS, indirect=True)
@pytest.mark.feature("multiple-accounts")
def test_apple_remove_account_shrinks_switcher_live(nest_instance, app_name, request):
    """Removing a non-active account LIVE-refreshes the Account page: the row vanishes
    in place, with no re-navigation. A remove never switches (no teardown/rebuild), so
    an in-place refresh is the only way the row can go."""
    seed, _user_actor, admin_actor = _seed_two_accounts(nest_instance)

    driver = create_driver(app_name)
    driver.launch({
        "url": nest_instance["url"],
        "seed_credentials": seed,
        "environment": _seeded_environment(request, nest_instance),
        **_app_launch_config(app_name, request),
    })
    try:
        driver.wait_for_state(
            lambda s: bool(s.get("session", {}).get("authenticated")), timeout=30
        )
        # Both listed, active on the regular user (row 0). The admin (row 1) is the
        # non-active row, so it carries the only remove button. Leaves the Account page
        # navigated-to and rendered.
        _wait_switcher_count(driver, 2, timeout=30)

        driver.click(REMOVE_BUTTON)

        # LIVE: the removed row vanishes from the SAME page, and the survivor stays active.
        _wait_live_switcher_count(driver, 1, timeout=15)
        assert driver.count(ACTIVE_INDICATOR) == 1, (
            "the surviving (regular) account stays active after the removal"
        )

        # And it is gone from the persisted registry, not just the view.
        index = _wait_registry_index(driver, 1, timeout=15)
        assert admin_actor not in [a["actor_id"] for a in index["accounts"]], (
            "remove must drop the account from the persisted registry index"
        )
    finally:
        driver.teardown()


# Onboarding ids the abandoned-create journey walks (tests/e2e-unified/ui.yaml
# § onboarding).
IDENTITY_CONTINUE_BUTTON = "identity-continue-button"
IDENTITY_CREATED_BACK_BUTTON = "identity-created-back-button"
RECOVERY_KIT_SKIP_BUTTON = "recovery-kit-skip-button"
HANDLE_ENTRY_BACK_BUTTON = "handle-entry-back-button"


def _launch_seeded(app_name, request, nest_instance, seed):
    driver = create_driver(app_name)
    driver.launch({
        "url": nest_instance["url"],
        "seed_credentials": seed,
        "environment": _seeded_environment(request, nest_instance),
        **_app_launch_config(app_name, request),
    })
    return driver


def _error_line(driver):
    return driver.get_text("error-message") if driver.is_visible("error-message") else ""


@pytest.mark.parametrize("app_name", APPS, indirect=True)
@pytest.mark.feature("multiple-accounts")
def test_apple_removing_an_identity_deletes_its_data_and_leaves_the_others(
    nest_instance, app_name, request
):
    """Removing an identity deletes THAT identity's data on this device and
    leaves every other identity's data untouched (`account-scoping.md` § The
    scoping taxonomy; apple's erase door is `AccountStateDir.erase`, run by
    `AccountSwitcherVM.remove` after the registry drops the row).

    The apple twin of `test_linux_removing_an_identity_deletes_its_data_and_leaves_the_others`.
    Both identities first run a real session each (the admin's through a
    switch), so each owns a populated scope; the removal must then leave no
    admin scope under ANY base the erase sweeps (`AppleScopeStore`) while the
    user's `mls.db` and secret are still there. Every wait is on state."""
    seed, user_actor, admin_actor = _seed_two_accounts(nest_instance)
    driver = _launch_seeded(app_name, request, nest_instance, seed)
    try:
        await_session_actor(driver, user_actor, budget_s=APP_RELAUNCH_S,
                            what="the launch as the user")
        store = attach_scope_store(app_name, driver)
        user_db = store.scope_dirs(user_actor)[0] / "mls.db"
        wait_until(user_db.exists, APP_RELAUNCH_S,
                   diagnose=lambda: f"the user's own MLS state {user_db} never appeared")

        # Give the admin a real session of its own, so it has data to lose.
        _wait_switcher_count(driver, 2, timeout=30)
        driver.click(SWITCHER_ITEM, scope=f"{SWITCHER_ITEM}[1]")
        await_session_actor(driver, admin_actor, budget_s=APP_RELAUNCH_S,
                            what="the switch to the admin")
        admin_dirs = store.scope_dirs(admin_actor)
        admin_db = admin_dirs[0] / "mls.db"
        wait_until(admin_db.exists, APP_RELAUNCH_S,
                   diagnose=lambda: f"the admin's own MLS state {admin_db} never appeared")

        # Back to the user: the admin row is now the non-active one, the only
        # row the switcher offers a remove button on.
        _wait_switcher_count(driver, 2, timeout=30)
        driver.click(SWITCHER_ITEM, scope=f"{SWITCHER_ITEM}[0]")
        await_session_actor(driver, user_actor, budget_s=APP_RELAUNCH_S,
                            what="the switch back to the user")

        _wait_switcher_count(driver, 2, timeout=30)
        driver.click(REMOVE_BUTTON)
        index = _wait_registry_index(driver, 1, timeout=30)
        assert [a["actor_id"] for a in index["accounts"]] == [user_actor], (
            f"only the removed admin may leave the registry; got {index!r}"
        )

        wait_until(
            lambda: not any(d.exists() for d in admin_dirs),
            RPC_ROUNDTRIP_S,
            diagnose=lambda: (
                "the removed identity's scope survived under: "
                f"{[str(d) for d in admin_dirs if d.exists()]}; "
                f"error-message: {_error_line(driver)!r}"
            ),
        )
        assert user_db.exists(), (
            "removing the admin must leave the remaining identity's data "
            f"untouched; {user_db} is gone"
        )
        assert _read_store_slot(driver, f"fauna/{admin_actor}/secret") is None, (
            "the removed identity's secret must be gone from this device"
        )
        assert _read_store_slot(driver, f"fauna/{user_actor}/secret"), (
            "the remaining identity's secret must survive the removal"
        )
    finally:
        driver.teardown()


@pytest.mark.parametrize("app_name", APPS, indirect=True)
@pytest.mark.feature("multiple-accounts")
def test_apple_switching_to_an_identity_the_device_cannot_sign_in_as_is_refused(
    nest_instance, app_name, request
):
    """Switching to an identity this device can no longer sign in as is
    refused, says so, and leaves the user on the identity they were using
    (`long-term-store.md` § Multi-account evolution, "Activating refuses an
    account it cannot launch as").

    The apple twin of the linux/tui/web journeys. The seed leaves the admin
    listed with no `fauna/<actor>/secret` slot. `FfiAccountRegistry.setActive`
    refuses before any teardown with the SHARED line
    (`fauna_client_accounts::switch_refused_copy`) as its `FfiError.General`
    message; the app's `switchAccount` throws it back through the switch seam
    and `AccountSwitcherSection` paints it on the Account page's
    `error-message`. No relaunch happens (`assert_no_relaunch`, anchored on the
    painted line — the handler's own completion observable), and the live
    session and the persisted active pointer are both still the user's."""
    seed, user_actor, admin_actor = _seed_two_accounts(nest_instance)
    seed.pop(f"fauna/{admin_actor}/secret")
    driver = _launch_seeded(app_name, request, nest_instance, seed)
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


# tests/e2e-unified/ui.yaml § settings (General page), folders
# (photo-backup-controls) + feed (the composer).
AUTOSTART_TOGGLE = "settings-autostart-toggle"
PHOTO_BACKUP_TOGGLE = "photo-backup-enable-toggle"
COMPOSE_FIELD = "compose-text-field"
GENERAL_PAGE_NAV = {
    "nav": {"stack": [{"view": "settings"}, {"view": "settings", "id": "general"}]}
}


@pytest.mark.parametrize("app_name", APPS, indirect=True)
@pytest.mark.feature("multiple-accounts")
def test_apple_switching_identity_keeps_device_settings_and_moves_drafts(
    nest_instance, app_name, request
):
    """Settings that describe this device stay as they are across a switch,
    while a draft — a choice tied to an identity — follows its identity
    (`account-scoping-dispositions.md` § Serialized switching, "Preferences
    split by meaning, not by file"; `account-scoping.md` § The scoping
    taxonomy, class 2 vs class 1).

    The apple twin of `test_linux_switching_identity_keeps_device_settings_and_moves_drafts`.
    Each target flips its own device setting — each app's own class-2 choice,
    as tui's twin keeps its external-media choice:

    - **macOS: start-at-login** (`settings-autostart-toggle`), persisted
      install-wide under `AutoStart.choiceKey` in `UserDefaults` and named by
      no actor scope (the OS registration itself is e2e-gated off, so the
      persisted choice is what the toggle shows).
    - **iOS: back up this device's photos** (`photo-backup-enable-toggle` on
      Settings → Folders), persisted install-wide under
      `PhotoBackupControlsView.enabledKey`. iOS has no desktop residency to
      start at login, and its other class-2 fact — the push subscribed bit —
      is outcome 11's own. Photos access is pre-granted at launch, because
      PhotoKit's prompt is a SpringBoard alert no in-process driver reaches.

    The identity-tied half is the feed composer's draft on the `posts` rail
    (`FeedVM.configure`'s restore, `reserved-folders.md` § Drafts Sync), which
    rests in that identity's own sealed `__drafts` plane. The arc: as the user,
    flip the device setting and leave a draft (confirmed on the nest before
    switching, so the switch tests scoping, not the debounce); switch to the
    admin → the setting still reads flipped, and the composer does NOT hold
    the user's draft; switch back → the user's draft is there again and the
    setting still flipped. The drafts read is the sanctioned side-channel
    verification (convention 8's carve-out); the mutations are all UI."""
    from clients.ws_rpc_admin_client import WsRpcAdminClient
    from helpers.waiting import photo_backup_funnel

    admin_sk = nest_instance["admin"]["signing_key"]
    admin_actor = bytes(admin_sk.verify_key).hex()
    user = create_actor_and_register(nest_instance["port"], admin_signing_key=admin_sk)
    user_actor = user["actor_id_hex"]
    seed = build_registry_seed(
        [
            {"actor_id": user_actor, "secret_hex": bytes(user["signing_key"]).hex(),
             "nest_url": nest_instance["url"], "device_id": "switcher-user",
             "handle": "user"},
            {"actor_id": admin_actor, "secret_hex": bytes(admin_sk).hex(),
             "nest_url": nest_instance["url"], "device_id": "switcher-admin",
             "handle": "admin"},
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

    driver = create_driver(app_name)
    if app_name == "ios":
        driver.grant_photos_access()
    driver.launch({
        "url": nest_instance["url"],
        "seed_credentials": seed,
        "environment": _seeded_environment(request, nest_instance),
        **_app_launch_config(app_name, request),
    })
    app = ActionLayer(driver)
    feed = app.feed
    toggle = PHOTO_BACKUP_TOGGLE if app_name == "ios" else AUTOSTART_TOGGLE

    def device_setting():
        if app_name == "ios":
            app.backups.navigate_folders()
        else:
            driver.set_state(GENERAL_PAGE_NAV)
        driver.wait_for(toggle, timeout=15)
        return driver.get_attr(toggle, "checked")

    def composer_body():
        feed.navigate()
        feed.open_composer()
        return feed.compose_body_text()

    def photo_backup_enabled():
        """The install's persisted photo-backup choice (`PHOTO_BACKUP_KEY`'s
        `enabled`), or None while the app publishes no funnel."""
        return (photo_backup_funnel(driver) or {}).get("enabled")

    try:
        await_session_actor(driver, user_actor, budget_s=APP_RELAUNCH_S,
                            what="the launch as the user")

        # (1) As the user: a device choice and a draft. The setting's starting
        # value is read, never assumed, so the flip is a real choice either way.
        before = device_setting()
        assert before in ("true", "false"), f"precondition: {toggle} reads {before!r}"
        chosen = "false" if before == "true" else "true"
        driver.click(toggle)
        wait_until(
            lambda: driver.get_attr(toggle, "checked") == chosen, UI_SETTLE_S,
            diagnose=lambda: f"{toggle} reads {driver.get_attr(toggle, 'checked')!r}; "
            f"error-message: {_error_line(driver)!r}",
        )
        # iOS persists an ON only in PhotoKit's authorization callback, after
        # the toggle's own view state has flipped: the published persisted bit
        # is the observable that the choice took.
        if app_name == "ios":
            wait_until(
                lambda: photo_backup_enabled() == (chosen == "true"), UI_SETTLE_S,
                diagnose=lambda: "the photo-backup choice never reached this install's "
                f"persisted default; funnel={photo_backup_funnel(driver)!r}",
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
            f"{toggle} reads {driver.get_attr(toggle, 'checked')!r}, chose {chosen!r}"
        )
        if app_name == "ios":
            # The toggle re-seeds from the persisted default when it remounts;
            # read the default itself too, so a view that merely kept its own
            # state across the switch cannot pass for a kept choice.
            assert photo_backup_enabled() == (chosen == "true"), (
                "the switch must not reset this install's photo-backup choice; "
                f"funnel={photo_backup_funnel(driver)!r}"
            )
        assert composer_body() != draft, (
            "the user's draft must not follow the device to another identity"
        )

        # (3) Switch back: the user's draft is theirs again, the device choice
        # is still the one made.
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
            f"{toggle} reads {driver.get_attr(toggle, 'checked')!r}, chose {chosen!r}"
        )
    finally:
        driver.teardown()


@pytest.mark.parametrize("app_name", APPS, indirect=True)
@pytest.mark.feature("multiple-accounts")
def test_apple_abandoned_created_identity_never_shows_among_your_identities(
    nest_instance, app_name, request
):
    """A fresh install creates identity A, walks back out of it, and onboards a
    DIFFERENT identity B instead: the switcher lists exactly one identity — B —
    and A never shows up among your identities (`long-term-store.md`
    § Multi-account evolution; the retirement is shared Rust,
    `AccountRegistry::retire_superseded_provisionals`, run by B's commit).

    The apple twin of
    `test_linux_abandoned_created_identity_never_shows_among_your_identities`.
    Moment 1 registers AND activates A at `identity-continue-button`, and
    nothing on the Back path retracts that row — so this is the case that can
    ghost. B arrives by import, never a second create."""
    admin_sk = nest_instance["admin"]["signing_key"]
    second = create_actor_and_register(nest_instance["port"], admin_signing_key=admin_sk)
    second_actor = second["actor_id_hex"]
    second_secret = bytes(second["signing_key"]).hex()

    driver = create_driver(app_name)
    driver.launch({
        "url": nest_instance["url"],
        "environment": _seeded_environment(request, nest_instance),
        **_app_launch_config(app_name, request),
    })
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

        # (3) Onboard B for real: import, then the AlreadyOnNest handle-check
        # outcome so Continue lands LoggedIn — B's commit, the write that must
        # retire A. The dial override lets B's session come up on this
        # plain-HTTP nest (see the append journey above).
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
        await_session_actor(
            driver, second_actor, budget_s=APP_RELAUNCH_S, what="onboarding B"
        )

        # (4) What the user sees: one identity, B, in the switcher.
        _wait_switcher_count(driver, 1, timeout=30)
        index = _read_registry_index(driver)
        actor_ids = [a["actor_id"] for a in (index or {}).get("accounts", [])]
        assert actor_ids == [second_actor], (
            f"the abandoned identity {abandoned} still shows among your identities; "
            f"index={index!r}"
        )
        assert index["active"] == second_actor
    finally:
        driver.teardown()


# ---------------------------------------------------------------------------
# Concurrent instances — the bound launch (macOS only by construction: iOS
# admits one instance per app; `account-scoping.md` § Concurrent instances).
# ---------------------------------------------------------------------------


def _read_seeded_store(driver):
    """The whole flushed credential store (`{cred_dir}/keychain.json`) — the
    logical `fauna/...` keys, exactly as seeded/rewritten."""
    with open(os.path.join(driver._cred_dir, "keychain.json")) as f:
        return json.load(f)


@pytest.mark.parametrize("app_name", ["macos"], indirect=True)
def test_apple_bound_launch_authenticates_as_the_bound_account(
    nest_instance, app_name, request
):
    """FAUNA_BOUND_ACCOUNT routes the WHOLE launch — machine *and* session.

    The regression this pins (found live 2026-07-22, and the reason the
    session-identity seam exists): the launch machine routed on the bound
    account, but `completeAuthenticatedLaunch` rebuilt the session from the
    (since retired) single slot — the *active* account's mirror — so the
    instance authenticated as the wrong account. Session identity must resolve
    through the session's account (`account-scoping.md` § Concurrent instances
    → *Session identity resolves through the session's account*).

    Also pins the never-happens: a bound launch does not move
    `fauna/index.active`, and apple writes no native single-slot row (both read
    back from the flushed store file, independent of any UI).
    """
    seed, user_actor, admin_actor = _seed_two_accounts(nest_instance)
    user_secret = seed[f"fauna/{user_actor}/secret"]

    driver = create_driver(app_name)
    driver.launch({
        "url": nest_instance["url"],
        "seed_credentials": seed,
        "environment": {**_seeded_environment(request, nest_instance), "FAUNA_BOUND_ACCOUNT": admin_actor},
        **_app_launch_config(app_name, request),
    })
    try:
        # The session comes up as the BOUND (admin) account, while `active`
        # names the regular user. Before the seam this authenticated as the
        # user (`actor_id == user_actor`, `am_i_admin == false`).
        state = driver.wait_for_state(
            lambda s: s.get("session", {}).get("actor_id") == admin_actor
            and bool(s.get("session", {}).get("authenticated")),
            timeout=45,
        )
        assert state["session"]["handle"] == "admin", (
            "the bound session must carry the bound account's material; got "
            f"{state.get('session')!r}"
        )

        # The bound instance served the admin — but the store still says the
        # regular user is active, and both accounts' per-actor secrets are intact.
        store = _read_seeded_store(driver)
        index = json.loads(store["fauna/index"])
        assert index["active"] == user_actor, (
            "a bound launch must not move fauna/index.active"
        )
        assert store[f"fauna/{user_actor}/secret"] == user_secret
        assert "secret_key" not in store, (
            "apple retired its native single-slot rows — nothing may write them"
        )
    finally:
        driver.teardown()


@pytest.mark.parametrize("app_name", ["macos"], indirect=True)
def test_apple_bound_launch_refuses_an_unknown_account(
    nest_instance, app_name, request
):
    """A binding the gate refuses is terminal — the instance exits instead of
    falling back to a plain launch on the active account (`account-scoping.md`
    § Concurrent instances: refusal must never degrade into a primary launch,
    which would put a second window on the active account and, for a flagged
    account, walk past its re-auth).

    The witness is process death: a refusing app renders no UI and exits.
    Usually that lands before the in-process agent reports healthy, so
    `launch` raises its early-exit RuntimeError — but the agent starts in
    `init()`, ahead of the launch task, so a fast agent can win that race;
    then the exit must land moments later. Either interleaving satisfies the
    contract ("the instance must NOT run"); a survivor fails.
    """
    seed, _user_actor, _admin_actor = _seed_two_accounts(nest_instance)
    unknown_actor = "ab" * 32  # well-formed 64-hex, in nobody's registry

    driver = create_driver(app_name)
    try:
        _expect_launch_refused(
            driver,
            {
                "url": nest_instance["url"],
                "seed_credentials": seed,
                "environment": {"FAUNA_BOUND_ACCOUNT": unknown_actor},
                **_app_launch_config(app_name, request),
            },
            "a bound launch for an unknown account must exit — refusal is "
            "terminal, never a fallback onto the active account",
        )
    finally:
        driver.teardown()


# ---------------------------------------------------------------------------
# The (OS login, account) single-instance guard — at most one instance per
# account, any number of accounts concurrently (`account-scoping.md`
# § Concurrent instances). macOS only: iOS admits one instance per app.
#
# Two instances only ever contend when they share one install world — the
# same Application Support base (where the per-account `instance-<actor>.lock`
# files live) and the same credential store. The driver's `home` +
# `credential_dir` pins provide exactly that world-sharing, so both launches
# below pass the same pinned pair (convention-10 isolation is preserved: the
# world is still a throwaway the test owns; it is shared between the two
# instances, not with the box).
# ---------------------------------------------------------------------------


def _shared_instance_world():
    """One throwaway install world (state home + credential store) for two
    instances to share — the same-OS-login premise of the guard."""
    base = tempfile.mkdtemp(prefix="fauna-e2e-macos-instance-world-")
    return {
        "home": os.path.join(base, "home"),
        "credential_dir": os.path.join(base, "credentials"),
    }


# The terminal-refusal assertion is shared across the per-app re-keying legs
# (linux/windows consume it too) — the docstring and the deliberate
# interleaving tolerance live there. Still used below by
# `test_apple_bound_launch_refuses_an_unknown_account`: an UNKNOWN-actor
# binding is refused by `bind_account`'s own gate (`UnknownActor`), which is
# mode-independent and unaffected by the W5.6 (account-data-plane.md § Workstreams) instance-lock retirement below.
_expect_launch_refused = expect_launch_refused

#: The muted-words term the coexistence case converges across the two
#: instances (mirrors `test_account_instance_lock_tui.py` /
#: `test_account_instance_lock_linux.py`'s constant of the same name).
CONVERGED_TERM = "coexist-probe"

#: Ceiling for cross-instance store convergence — see the tui/linux twins'
#: derivation (the writer's synchronous dual write + the reader's
#: `data_version` observer poll + a page re-read; a deadline poll exits the
#: moment the term appears, so a green run pays only the real latency).
CONVERGENCE_BUDGET_S = 90.0


@pytest.mark.parametrize("app_name", ["macos"], indirect=True)
def test_apple_second_instance_on_the_same_account_coexists(
    nest_instance, app_name, request
):
    """**The W5.6 success condition, plain-launch half** (`account-scoping.md`
    § Concurrent instances; `account-data-plane.md` § Multi-instance
    concurrency): two plain (primary) launches of one account in one install
    world now COEXIST — until 2026-08-16 this exact launch shape was
    terminally refused, overtaken by the ratified W5.6 retirement, not by a
    regression. apple has no launch-collision chooser (`ui.yaml`
    `launch_instance_chooser` excludes macOS by construction), so a plain
    second launch runs the identical `completeAuthenticatedLaunch` path a
    bound one does — see `test_apple_bound_launch_onto_the_served_account_
    coexists` for the fuller three-observable proof; this case only needs to
    pin that the PLAIN path coexists too, since it is a distinct code
    entry (`resolveLaunchBinding` → `.primary`, not `.bound`).

    Both launches are BARE-BINARY — precisely the channel LaunchServices'
    per-(OS login, bundle id) single-instance behaviour never sees — so what
    this pins is the app's own (OS login, account) lock, not the OS's bundle
    guard."""
    seed, user_actor, _admin_actor = _seed_two_accounts(nest_instance)
    world = _shared_instance_world()
    launch_config = _app_launch_config(app_name, request)

    first = create_driver(app_name)
    first.launch({
        "url": nest_instance["url"],
        "seed_credentials": seed,
        "environment": _seeded_environment(request, nest_instance),
        **world,
        **launch_config,
    })
    try:
        first.wait_for_state(
            lambda s: s.get("session", {}).get("actor_id") == user_actor
            and bool(s.get("session", {}).get("authenticated")),
            timeout=45,
        )

        second = create_driver(app_name)
        try:
            # No re-seed: the shared store already holds the registry, and a
            # re-seed would clobber it under the first instance.
            second.launch({
                "url": nest_instance["url"],
                "environment": _seeded_environment(request, nest_instance),
                **world,
                **launch_config,
            })
            second.wait_for_state(
                lambda s: s.get("session", {}).get("actor_id") == user_actor
                and bool(s.get("session", {}).get("authenticated")),
                timeout=45,
            )
        finally:
            second.teardown()

        # Coexistence is symmetric: the first instance is untouched by the
        # second's launch.
        state = first.get_state()
        assert state.get("session", {}).get("actor_id") == user_actor and bool(
            state.get("session", {}).get("authenticated")
        ), "the first instance must be untouched by the coexisting launch"
    finally:
        first.teardown()


@pytest.mark.feature("second-identity-in-its-own-window")
@pytest.mark.parametrize("app_name", ["macos"], indirect=True)
def test_apple_bound_launch_onto_the_served_account_coexists(
    nest_instance, app_name, request
):
    """**The W5.6 success condition** (`account-scoping.md` § Concurrent
    instances; `account-data-plane.md` § Multi-instance concurrency): a bound
    launch onto the account a live instance already serves passes the
    bound-or-refuse gate AND the (now shared) instance lock — two same-account
    macOS instances run concurrently against one store dir. Three observables,
    each a half the other two cannot fake (mirrors
    ``test_tui_bound_launch_onto_the_served_account_coexists`` /
    ``test_linux_bound_launch_onto_the_served_account_coexists`` exactly — tui
    is the proven pattern this leg mirrors):

    1. **Coexistence** — the second instance authenticates as the account and
       the first stays live and authenticated (until 2026-08-16 this exact
       launch was terminally refused).
    2. **Convergence** — a muted-words term added through the FIRST instance's
       UI becomes visible in the SECOND (the multi-process-safe store + the
       ``data_version`` observer poll; the mutation is a driver UI action per
       convention 8, the wait a deadline poll per convention 14).
    3. **The honest conversations refusal** — the first instance holds the
       conversations-engine role lock over the shared ``mls_state.db``, so the
       second's conversations page surfaces the ruled "served in another
       instance" on ``error-message`` (never a silent unwired page), while
       everything else in it — the muted-words leg above ran in that very
       process — works normally."""
    seed, user_actor, _admin_actor = _seed_two_accounts(nest_instance)
    world = _shared_instance_world()
    launch_config = _app_launch_config(app_name, request)

    first = create_driver(app_name)
    first.launch({
        "url": nest_instance["url"],
        "seed_credentials": seed,
        "environment": _seeded_environment(request, nest_instance),
        **world,
        **launch_config,
    })
    try:
        first.wait_for_state(
            lambda s: s.get("session", {}).get("actor_id") == user_actor
            and bool(s.get("session", {}).get("authenticated")),
            timeout=45,
        )

        second = create_driver(app_name)
        try:
            second.launch({
                "url": nest_instance["url"],
                "environment": {**_seeded_environment(request, nest_instance), "FAUNA_BOUND_ACCOUNT": user_actor},
                **world,
                **launch_config,
            })
            # Observable 1 — coexistence. Authenticating at all is the flip's
            # survival proof: the pre-W5.6 law exited this process before any
            # frame painted.
            state = second.wait_for_state(
                lambda s: s.get("session", {}).get("actor_id") == user_actor
                and bool(s.get("session", {}).get("authenticated")),
                timeout=45,
            )
            assert state.get("session", {}).get("actor_id") == user_actor, (
                "the bound instance must serve the bound account, "
                f"got {state.get('session')!r}"
            )
            fstate = first.get_state()
            assert fstate.get("session", {}).get("actor_id") == user_actor and bool(
                fstate.get("session", {}).get("authenticated")
            ), "the first instance must be untouched by the coexisting launch"

            first_app = ActionLayer(first)
            second_app = ActionLayer(second)

            # Observable 3 first — it needs no store round-trip, and reading
            # it before the muted-words leg proves the refusal is standing
            # from login, not an artifact of later activity. The SECOND
            # instance is the non-holder: the first opened its engine at
            # sign-in over the shared per-account ``mls_state.db``.
            #
            # Deadline-poll, not a bare read (convention 14): apple flips
            # `session.authenticated` synchronously in
            # `completeAuthenticatedLaunch`, then activates the conversations
            # session (and so `set_engine_served_elsewhere`) in a SEPARATE
            # async `Task` — a real race window `wait_for_state` above cannot
            # close, unlike linux/tui's synchronous engine-init-before-auth
            # shape. A one-shot read can win that race under load (observed:
            # green in isolation, red inside the full file at 113s).
            second_app.conversations.navigate()
            deadline = time.monotonic() + 20.0
            refusal = ""
            while time.monotonic() < deadline:
                refusal = second_app.error_text()
                if S.conversations.errors.served_elsewhere in (refusal or ""):
                    break
                time.sleep(0.3)
            assert S.conversations.errors.served_elsewhere in (refusal or ""), (
                "the non-role-holder's conversations page must refuse honestly "
                f"on error-message; got {refusal!r}"
            )

            # Observable 2 — convergence, first → second. The add is a UI
            # action in the first instance; the second polls its own re-read
            # of the same store.
            first_app.muted_words.navigate()
            first_app.muted_words.add(CONVERGED_TERM)
            assert first_app.muted_words.wait_for_row_count(1), (
                f"the add did not land in the writer; words={first_app.muted_words.words()!r}"
            )

            deadline = time.monotonic() + CONVERGENCE_BUDGET_S
            seen: list[str] = []
            while True:
                # Re-enter the page each attempt: navigation re-reads the
                # store, so the poll observes durable cross-process state, not
                # a hot in-memory list.
                second_app.muted_words.navigate()
                second_app.muted_words.wait_for_row_count(1, timeout=10.0)
                seen = second_app.muted_words.words()
                if CONVERGED_TERM in seen or time.monotonic() >= deadline:
                    break
            assert CONVERGED_TERM in seen, (
                "the first instance's write never became visible in the second "
                f"within {CONVERGENCE_BUDGET_S:.0f}s — the shared-store "
                f"notification floor is not converging; second saw {seen!r}"
            )

            # And the first's own conversations page carries no refusal — the
            # role holder serves normally.
            first_app.conversations.navigate()
            holder_error = first_app.error_text()
            assert S.conversations.errors.served_elsewhere not in (holder_error or ""), (
                f"the role holder must not report served-elsewhere; got {holder_error!r}"
            )
        finally:
            second.teardown()
    finally:
        first.teardown()


#: The app-log prefixes the bound seat's succession chain reports itself with —
#: the binding resolution, the switch, the relaunch, the ceremony.
_SUCCESSION_LOG_MARKERS = (
    "[bound-launch]", "[account-switch]", "[launch]", "succession", "superseded",
)


def _succession_log_lines(driver) -> str:
    """The seat's own succession-chain log lines, for a failure message
    (convention 6): which binding the relaunch resolved and which actor it
    authenticated as are observable only there — a stuck seat reads
    identically from the UI either way."""
    lines = [
        ln for ln in driver.app_stderr_text().splitlines()
        if any(m in ln for m in _SUCCESSION_LOG_MARKERS)
    ]
    return "\napp log (succession chain):\n" + "\n".join(lines[-60:])


@pytest.mark.parametrize("app_name", ["macos"], indirect=True)
@pytest.mark.feature("take-your-account-back")
def test_apple_a_bound_instance_survives_its_own_succession(
    nest_instance, app_name, request
):
    """**The binding follows the account** (`account-scoping.md` § Concurrent
    instances → *The binding follows the account*): the bound seat of the
    two-instance world runs "my identity was stolen" to completion. Mirrors
    ``test_tui_a_bound_instance_survives_its_own_succession`` exactly — tui is
    the proven pattern this leg follows. iOS is out of scope by construction
    (`account-scoping.md:371-373` — no bound-launch environment seam;
    "concurrent instances on apple means macOS").

    macOS never had tui's and windows's per-switch bound-or-refuse gate, so
    the gate half of their 2026-08-27 fix never applied here. It had its own
    defect instead (`account-scoping.md` § Implementation status today): the
    successor's freshly built ``FaunaClient`` authenticated from the legacy
    ``.secretKey`` slot, the ACTIVE account's mirror, which a bound instance
    never re-mirrors. Whenever that slot still held the retired actor, the
    client was re-pointed at it and every call was refused ``superseded``.
    The tier_1 pin is FaunaKit's
    ``startupAuthSignsAsTheClientsOwnIdentityNotTheLegacyMirror``; this
    journey is the end-to-end witness. It passed on some unfixed runs,
    because the mirror's content at that moment is a race.

    Four observables, in the order the ceremony produces them:

    1. **Survival, and the closing act.** The successor's fresh kit lands on
       screen in the SAME process (read first and with no navigation — the
       shown-once custody rule).
    2. **The switch.** The Status page settles on a different actor id and
       the session is authenticated as it.
    3. **The sweep view survived**, and it is the ``no_engine`` arm: this
       seat's conversations engine was refused at sign-in by the first
       instance's conversations-engine role lock, so the sweep had no engine
       to run over. The shared retry-gate helper then asserts the button
       renders on it (macOS landed `recovery-kit-sweep-retry-button`
       2026-08-27 — `docs/goal/ui/settings.md` § Recovery kit) and that a
       press answers.
    4. **The first instance is untouched** by the second's ceremony.

    ⚠ Do NOT "simplify" this into a lone bound launch: the first instance is
    what makes the seat *bound* in practice and what makes the sweep
    ``no_engine``.
    """
    seed, user_actor = _seed_one_account(nest_instance)
    world = _shared_instance_world()
    launch_config = _app_launch_config(app_name, request)

    first = create_driver(app_name)
    first.launch({
        "url": nest_instance["url"],
        "seed_credentials": seed,
        "environment": _seeded_environment(request, nest_instance),
        **world,
        **launch_config,
    })
    try:
        first.wait_for_state(
            lambda s: s.get("session", {}).get("actor_id") == user_actor
            and bool(s.get("session", {}).get("authenticated")),
            timeout=45,
        )

        second = create_driver(app_name)
        try:
            second.launch({
                "url": nest_instance["url"],
                "environment": {**_seeded_environment(request, nest_instance), "FAUNA_BOUND_ACCOUNT": user_actor},
                **world,
                **launch_config,
            })
            second.wait_for_state(
                lambda s: s.get("session", {}).get("actor_id") == user_actor
                and bool(s.get("session", {}).get("authenticated")),
                timeout=45,
            )
            app = ActionLayer(second)

            # ── run the theft ceremony from the bound seat ───────────────────
            old_actor, held = succeed_identity_from(app)
            assert old_actor == user_actor, (
                "the bound seat must render its own actor id before the "
                f"ceremony; got {old_actor!r}, error: {app.error_text()!r}"
            )

            # ── 1. survival, and the closing act ─────────────────────────────
            wait_alive_until(
                second,
                lambda: bool(kit_on_screen(app)),
                SUCCESSION_AND_RELAUNCH_S,
                "before the successor's kit was shown",
                diagnose=lambda: (
                    f"no kit on screen for the successor (reads {kit_on_screen(app)!r}), "
                    f"error={app.error_text()!r}{_succession_log_lines(second)}"
                ),
            )
            assert kit_on_screen(app) != held, (
                "the successor must mint a FRESH RecoveryKey — the old one "
                "retired with the old identity"
            )

            # ── 2. the switch ────────────────────────────────────────────────
            new_actor = wait_for_successor_actor(
                app, old_actor, guard_alive=True,
                diagnose=lambda: _succession_log_lines(second),
            )
            assert new_actor and new_actor != old_actor, (
                f"the successor id is a fresh actor id, got {new_actor!r}"
            )
            state = second.get_state()
            assert state.get("session", {}).get("actor_id") == new_actor and bool(
                state.get("session", {}).get("authenticated")
            ), (
                "the bound instance must come back up AUTHENTICATED as the "
                f"successor — the binding followed the account; got {state.get('session')!r}"
            )

            # ── 3. the sweep view survived — and it is the no_engine arm ─────
            sweep = second.get_state("data.succession_sweep")
            assert sweep is not None, (
                "the sweep view died with the switch — it is in-memory state of "
                "the ceremony's own session, and the retry's render gate reads it"
            )
            assert sweep.get("status") == "no_engine", (
                "the bound seat's engine is refused at sign-in by the first "
                "instance's conversations-engine role lock, so its ceremony "
                "sweeps with NO engine — the arm this world exists to produce; "
                f"got {sweep!r}"
            )
            assert sweep_owes_work(sweep), f"a no_engine sweep owes work; {sweep!r}"
            assert_the_retry_affordance_matches_the_sweep(app, sweep)

            # ── 4. the first instance is untouched ───────────────────────────
            assert is_app_alive(first) is not False, (
                "the first instance must survive the second's ceremony"
            )
            fstate = first.get_state()
            assert bool(fstate.get("session", {}).get("authenticated")), (
                f"the first instance must still be serving; got {fstate.get('session')!r}"
            )
        finally:
            second.teardown()
    finally:
        first.teardown()


@pytest.mark.parametrize("app_name", ["macos"], indirect=True)
@pytest.mark.feature("take-your-account-back")
def test_apple_a_launch_bound_to_a_retired_id_comes_up_as_the_successor(
    nest_instance, app_name, request
):
    """**A bound launch follows the chain** (`account-scoping.md` § Concurrent
    instances → *The binding follows the account*, rider 2): a spawn minted
    with the RETIRED id, after this install's registry has recorded the
    succession, comes up as the successor. Mirrors
    ``test_tui_a_launch_bound_to_a_retired_id_comes_up_as_the_successor``
    exactly — tui is the proven pattern this leg follows. iOS has no bound
    launch (no `--environment` seam), so this case is macOS-only.

    The world is ``test_apple_bound_launch_onto_the_served_account_coexists``'s:
    a second instance bound onto the account a live first instance already
    serves. That second (bound) seat then runs the theft ceremony
    (``test_identity_succession_ceremony.py``'s recipe, proven green on
    macOS), so the install's registry now holds the successor's row and the
    ``succeeded_by`` link, and ``active`` moved to the successor. A THIRD
    instance is then launched with ``FAUNA_BOUND_ACCOUNT=<old>`` — the shape
    of a spawn command minted before its spawner could observe the
    succession. The binding names an account, and the account is the
    successor now: ``resolve_launch_binding`` walks the chain and re-points
    the process binding before the launch machine resolves the session, so
    the bound-or-refuse gate meets the successor on both sides.

    The first (plain) seat still serves the retired id throughout; what it
    does about being superseded is the own-device-fleet leg, not this case's.
    """
    seed, user_actor = _seed_one_account(nest_instance)
    world = _shared_instance_world()
    launch_config = _app_launch_config(app_name, request)

    first = create_driver(app_name)
    first.launch({
        "url": nest_instance["url"],
        "seed_credentials": seed,
        "environment": _seeded_environment(request, nest_instance),
        **world,
        **launch_config,
    })
    try:
        first.wait_for_state(
            lambda s: s.get("session", {}).get("actor_id") == user_actor
            and bool(s.get("session", {}).get("authenticated")),
            timeout=45,
        )

        second = create_driver(app_name)
        try:
            second.launch({
                "url": nest_instance["url"],
                "environment": {**_seeded_environment(request, nest_instance), "FAUNA_BOUND_ACCOUNT": user_actor},
                **world,
                **launch_config,
            })
            second.wait_for_state(
                lambda s: s.get("session", {}).get("actor_id") == user_actor
                and bool(s.get("session", {}).get("authenticated")),
                timeout=45,
            )
            app = ActionLayer(second)

            # ── run the theft ceremony from the bound seat ───────────────────
            old_actor, held = succeed_identity_from(app)
            assert old_actor == user_actor, (
                "the bound seat must render its own actor id before the "
                f"ceremony; got {old_actor!r}, error: {app.error_text()!r}"
            )

            new_actor = wait_for_successor_actor(app, old_actor)
            assert new_actor and new_actor != old_actor, (
                f"the bound seat must settle on a NEW actor id; got {new_actor!r}"
            )

            # ── the spawn minted with the retired id ─────────────────────────
            third = create_driver(app_name)
            try:
                try:
                    third.launch({
                        "url": nest_instance["url"],
                        "environment": {**_seeded_environment(request, nest_instance), "FAUNA_BOUND_ACCOUNT": old_actor},
                        **world,
                        **launch_config,
                    })
                except RuntimeError as exited_early:
                    raise AssertionError(
                        "a launch bound to the RETIRED id exited instead of following "
                        "the chain to the successor (account-scoping.md § Concurrent "
                        f"instances → the binding follows the account, rider 2). "
                        f"Launch error: {exited_early}"
                    ) from exited_early
                await_session_actor(
                    third, new_actor, budget_s=45.0,
                    what="the seat bound to the retired id settling as the successor",
                )
                actor = (third.get_state() or {}).get("session", {}).get("actor_id")
                assert actor == new_actor, (
                    "the seat bound to the retired id must serve the SUCCESSOR — the "
                    f"binding names the account, and the account is {new_actor!r} now; "
                    f"got {actor!r}"
                )
            finally:
                third.teardown()

            fstate = first.get_state()
            assert fstate.get("session", {}).get("actor_id") == user_actor and bool(
                fstate.get("session", {}).get("authenticated")
            ), "the first (plain) seat must be untouched by either launch"
        finally:
            second.teardown()
    finally:
        first.teardown()


OPEN_NEW_INSTANCE_BUTTON = "account-open-new-instance-button"


@pytest.mark.parametrize("app_name", ["macos"], indirect=True)
@pytest.mark.feature("second-identity-in-its-own-window")
def test_apple_open_as_new_instance_spawns_a_bound_sibling(
    nest_instance, app_name, request
):
    """The switcher's "open in new window" affordance (ui.yaml
    `account-open-new-instance-button`; `account-scoping.md` § Concurrent
    instances, the running instance's surface): clicking it on the non-active
    (admin) row spawns a SECOND app instance bound to that account, while
    this window stays on its own.

    The spawned child is observed directly: under e2e the spawner allocates
    the child its own automation port and reports it in the parent's
    `spawned_instances` state, so the test polls the child's own agent for
    "authenticated as the admin". The child shares the parent's install
    world by inheritance (credential dir, relocated HOME) — which is exactly
    the production premise (one OS login)."""
    import urllib.request

    seed, user_actor, admin_actor = _seed_two_accounts(nest_instance)
    world = _shared_instance_world()
    launch_config = _app_launch_config(app_name, request)

    driver = create_driver(app_name)
    driver.launch({
        "url": nest_instance["url"],
        "seed_credentials": seed,
        "environment": _seeded_environment(request, nest_instance),
        **world,
        **launch_config,
    })
    try:
        driver.wait_for_state(
            lambda s: s.get("session", {}).get("actor_id") == user_actor
            and bool(s.get("session", {}).get("authenticated")),
            timeout=45,
        )
        # Row 0 = user (active), row 1 = admin. Since W5.6 (2026-08-16) the
        # button renders on EVERY row (macOS retired its same-account
        # refusal), so a bare click is now ambiguous — scope to row 1, the
        # same idiom `SWITCHER_ITEM` clicks elsewhere in this file use.
        _wait_switcher_count(driver, 2, timeout=30)
        driver.click(OPEN_NEW_INSTANCE_BUTTON, scope=f"{SWITCHER_ITEM}[1]")

        # The spawn record (with the child's own agent port) appears in the
        # parent's state.
        state = driver.wait_for_state(
            lambda s: any(
                r.get("actor_id") == admin_actor and r.get("agent_port")
                for r in s.get("spawned_instances", [])
            ),
            timeout=15,
        )
        child_port = next(
            r["agent_port"]
            for r in state["spawned_instances"]
            if r.get("actor_id") == admin_actor
        )

        # The child comes up bound: authenticated as the ADMIN while the
        # parent stays on the user. (The child dies with the parent's
        # process group at teardown — no orphan.)
        deadline = time.monotonic() + 60
        child_session = {}
        last_error = None
        while time.monotonic() < deadline:
            try:
                with urllib.request.urlopen(
                    f"http://127.0.0.1:{child_port}/app/state", timeout=10
                ) as resp:
                    body = json.load(resp)
                    # Same unwrap as the driver's _get_state_raw: the agent
                    # nests the payload under "state".
                    child_session = body.get("state", body).get("session", {})
                if (
                    child_session.get("actor_id") == admin_actor
                    and child_session.get("authenticated")
                ):
                    break
            except OSError as e:
                last_error = e
            time.sleep(0.5)
        assert (
            child_session.get("actor_id") == admin_actor
            and child_session.get("authenticated")
        ), (
            "the spawned instance must authenticate as the account it was "
            f"opened for; last child session: {child_session!r}; last poll "
            f"error: {last_error!r}"
        )

        parent = driver.get_state()
        assert parent.get("session", {}).get("actor_id") == user_actor and bool(
            parent.get("session", {}).get("authenticated")
        ), "the spawning window must stay on its own account"
    finally:
        driver.teardown()


# ---------------------------------------------------------------------------
# Registry-routed session-path deletes (`account-scoping.md` § Concurrent
# instances, the delete corollary). The bug these pin: logout once deleted only
# the single-slot rows (since retired) while the per-actor slots survived, so
# the next boot resurrected the identity and the logout silently undid itself.
# The delete routes through the registry. Relaunch on the same store is the
# proof: nothing is left to resurrect from.
# ---------------------------------------------------------------------------


@pytest.mark.parametrize("app_name", ["macos"], indirect=True)
def test_apple_logout_promotes_the_next_account_and_survives_relaunch(
    nest_instance, app_name, request
):
    """Logout on a two-account install removes the active account and lands on
    the promoted one — in-session, in the persisted registry, and across a
    relaunch."""
    seed, user_actor, admin_actor = _seed_two_accounts(nest_instance)
    admin_secret = seed[f"fauna/{admin_actor}/secret"]
    world = _shared_instance_world()
    launch_config = _app_launch_config(app_name, request)

    driver = create_driver(app_name)
    driver.launch({
        "url": nest_instance["url"],
        "seed_credentials": seed,
        "environment": _seeded_environment(request, nest_instance),
        **world,
        **launch_config,
    })
    try:
        driver.wait_for_state(
            lambda s: s.get("session", {}).get("actor_id") == user_actor
            and bool(s.get("session", {}).get("authenticated")),
            timeout=45,
        )
        driver.logout()
        # The removal promoted the admin and the logout relaunched into it.
        driver.wait_for_state(
            lambda s: s.get("session", {}).get("actor_id") == admin_actor
            and bool(s.get("session", {}).get("authenticated")),
            timeout=45,
        )
        store = _read_seeded_store(driver)
        index = json.loads(store["fauna/index"])
        assert [a["actor_id"] for a in index["accounts"]] == [admin_actor], (
            "logout must remove the logged-out account from the registry"
        )
        assert index["active"] == admin_actor
        assert store.get(f"fauna/{admin_actor}/secret") == admin_secret, (
            "the promoted account's per-actor secret must survive the logout"
        )
        assert f"fauna/{user_actor}/secret" not in store, (
            "the removed account's per-actor slots must be gone — they are what "
            "a relaunch would resurrect from"
        )
    finally:
        driver.teardown()

    # Relaunch over the SAME store: the promoted account boots; the logged-out
    # one stays gone (nothing resurrects it).
    relaunched = create_driver(app_name)
    relaunched.launch({
        "url": nest_instance["url"],
        "environment": _seeded_environment(request, nest_instance),
        **world,
        **launch_config,
    })
    try:
        state = relaunched.wait_for_state(
            lambda s: bool(s.get("session", {}).get("authenticated")), timeout=45
        )
        assert state["session"]["actor_id"] == admin_actor, (
            "the relaunch must come up as the promoted account, not the "
            "logged-out one"
        )
    finally:
        relaunched.teardown()


@pytest.mark.parametrize("app_name", ["macos"], indirect=True)
def test_apple_logout_of_the_last_account_survives_relaunch_signed_out(
    nest_instance, app_name, request
):
    """Logout of the ONLY account signs the install out durably: the relaunch
    lands on fresh onboarding, not a resurrected session (the single-account
    form of the resurrection bug — the seeded registry has materialized
    per-actor slots, which a single-slot delete once left behind)."""
    seed, user_actor = _seed_one_account(nest_instance)
    world = _shared_instance_world()
    launch_config = _app_launch_config(app_name, request)

    driver = create_driver(app_name)
    driver.launch({
        "url": nest_instance["url"],
        "seed_credentials": seed,
        "environment": _seeded_environment(request, nest_instance),
        **world,
        **launch_config,
    })
    try:
        driver.wait_for_state(
            lambda s: s.get("session", {}).get("actor_id") == user_actor
            and bool(s.get("session", {}).get("authenticated")),
            timeout=45,
        )
        driver.logout()
        driver.wait_for_state(
            lambda s: not s.get("session", {}).get("authenticated"), timeout=15
        )
        store = _read_seeded_store(driver)
        remaining = json.loads(store.get("fauna/index", '{"accounts": []}'))["accounts"]
        assert remaining == [], (
            f"the last account's logout must leave no account in the index; got {remaining!r}"
        )
        assert f"fauna/{user_actor}/secret" not in store, (
            "the per-actor slots must be gone with it"
        )
    finally:
        driver.teardown()

    relaunched = create_driver(app_name)
    relaunched.launch({
        "url": nest_instance["url"],
        "environment": _seeded_environment(request, nest_instance),
        **world,
        **launch_config,
    })
    try:
        relaunched.wait_for(CREATE_IDENTITY, timeout=45)
        state = relaunched.get_state()
        assert not state.get("session", {}).get("authenticated"), (
            "a relaunch after logging out the only account must not resurrect "
            "the session"
        )
    finally:
        relaunched.teardown()


@pytest.mark.parametrize("app_name", ["macos"], indirect=True)
@pytest.mark.feature("second-identity-in-its-own-window")
def test_apple_instances_on_different_accounts_run_concurrently(
    nest_instance, app_name, request
):
    """The guard's key is the account, not the install: a primary on the user
    and a bound secondary on the admin share one world and BOTH serve — the
    concurrent-instances goal state, and the proof the same-account refusals
    above come from the per-account lock, not some install-wide guard."""
    seed, user_actor, admin_actor = _seed_two_accounts(nest_instance)
    world = _shared_instance_world()
    launch_config = _app_launch_config(app_name, request)

    first = create_driver(app_name)
    first.launch({
        "url": nest_instance["url"],
        "seed_credentials": seed,
        "environment": _seeded_environment(request, nest_instance),
        **world,
        **launch_config,
    })
    second = None
    try:
        first.wait_for_state(
            lambda s: s.get("session", {}).get("actor_id") == user_actor
            and bool(s.get("session", {}).get("authenticated")),
            timeout=45,
        )

        second = create_driver(app_name)
        second.launch({
            "url": nest_instance["url"],
            "environment": {**_seeded_environment(request, nest_instance), "FAUNA_BOUND_ACCOUNT": admin_actor},
            **world,
            **launch_config,
        })
        second.wait_for_state(
            lambda s: s.get("session", {}).get("actor_id") == admin_actor
            and bool(s.get("session", {}).get("authenticated")),
            timeout=45,
        )

        # Both live at once, each as its own account.
        state = first.get_state()
        assert state.get("session", {}).get("actor_id") == user_actor and bool(
            state.get("session", {}).get("authenticated")
        ), "the primary must keep serving its account while the secondary runs"
    finally:
        if second is not None:
            second.teardown()
        first.teardown()
