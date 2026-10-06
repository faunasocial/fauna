"""tier_3 e2e: Windows multi-account account switcher (Stage 1 Slice 2).

The windows twin of ``test_account_switcher_linux.py``. Multi-account clients let
one install (one OS-user context) hold several Fauna identities and switch between
them (``docs/goal/architecture/long-term-store.md`` § Multi-account evolution;
switch-first design, Decision 1). Windows is the LAST of seven apps to grow this
surface — linux/web landed 2026-07-02, apple 07-15, android 07-18, tui 07-19 — so
every shape here is an adoption of a proven one, not a new design.

1. Seed TWO registered identities into the file-backed credential store as a full
   ``AccountRegistry`` state (a claimed admin + a regular user), active on the
   regular user. ``build_registry_seed`` writes the exact logical-key layout the
   C# ``LogicalSecretStore`` File backend reads verbatim — no app code.
2. Launch ``FaunaApp.exe`` → it authenticates as the active (regular) account; the
   admin shell (``admin-tab``) is absent.
3. Account settings lists BOTH accounts with the active one marked.
4. Tapping the admin account switches to it with a live in-session reconnect
   (no relaunch) → the admin shell appears.

Windows-specific notes for whoever reads this next:

- The credential seed lands at ``{cred_dir}/{keyring_app}.json`` (default
  ``fauna-windows.json``), NOT linux's ``{tmp}/creds/fauna-e2e-agent-{port}.json``
  — see ``drivers/windows.py`` ``launch``. ``_read_registry_index`` below encodes
  that difference; it is the only structural divergence from the linux module.
- The seed is the registry alone: windows reads no pre-registry single slot
  (``long-term-store.md`` § Downgrade mirror + abandoned-append recovery), so
  every assert below reads the per-actor rows and the index.
- Stage 2 (re-auth-on-activate) IS here now, in the two tests at the bottom.
  Windows maps to the native Hello prompt, which carries no test ID, so it
  follows apple's ``{FAUNA_E2E_CREDENTIAL_DIR}/reauth-result`` file seam rather
  than linux's in-app prompt (``ui.yaml`` — apple/windows/android "never render"
  ``account-activate-reauth-prompt``). The two tests are the windows twins of
  ``test_account_switcher_apple.py``'s decline-then-approve + admin-auto-default.

tier_3: needs a real ``fauna-nest`` binary (``nest_instance``); windows driver only.
"""
from __future__ import annotations

import json
import os
import shutil
import tempfile
import time
from types import SimpleNamespace

import pytest
from nacl.signing import SigningKey

from common import build_registry_seed, create_actor_and_register
from common.scope_store import attach_scope_store
from conftest import _seeded_environment
from drivers import create_driver
from drivers.machine_test_setter import set_handle_check_snapshot
from helpers.budgets import APP_RELAUNCH_S, RPC_ROUNDTRIP_S, UI_SETTLE_S
from helpers.registry_store import read_registry_index, read_store_map
from helpers.waiting import (
    activation_gesture_completed,
    assert_no_relaunch,
    await_session_actor,
    session_generation,
    wait_registry_index,
    wait_until,
)
from i18n.strings import S

pytestmark = [pytest.mark.tier_3, pytest.mark.windows]

# tests/e2e-unified/ui.yaml § settings (switcher) + navigation (admin-tab).
SWITCHER_LIST = "account-switcher-list"
SWITCHER_ITEM = "account-switcher-item"
ITEM_HANDLE = "account-item-handle"
ACTIVE_INDICATOR = "account-item-active-indicator"
ADD_BUTTON = "account-add-button"
REMOVE_BUTTON = "account-remove-button"
REQUIRE_CONFIRM_TOGGLE = "account-require-confirm-toggle"
OPEN_NEW_INSTANCE_BUTTON = "account-open-new-instance-button"
ADMIN_TAB = "admin-tab"

# tests/e2e-unified/ui.yaml § onboarding — the append-mode "Add account" wizard.
IMPORT_IDENTITY_BUTTON = "import-identity-button"
PASTE_SECRET_FIELD = "paste-secret-field"
IMPORT_SUBMIT_BUTTON = "import-submit-button"
CANCEL_BUTTON = "onboarding-cancel-button"
HANDLE_INPUT = "handle-input"
HANDLE_CONTINUE_BUTTON = "handle-entry-continue-button"

# tests/e2e-unified/ui.yaml § onboarding — invite_request (the pending-invite surface).
INVITE_SUBMIT_BUTTON = "invite-request-submit-button"
INVITE_RECHECK_BUTTON = "invite-request-recheck-button"

# tests/e2e-unified/ui.yaml § onboarding — the first-run "create identity" wizard
# (ghost-row regression, below).
CREATE_IDENTITY_BUTTON = "create-identity-button"
IDENTITY_CONTINUE_BUTTON = "identity-continue-button"
HANDLE_ENTRY_BACK_BUTTON = "handle-entry-back-button"
IDENTITY_IMPORT_BACK_BUTTON = "identity-import-back-button"
RECOVERY_KIT_SKIP_BUTTON = "recovery-kit-skip-button"

# Two-element nav to the Account sub-page of the desktop Settings shell. Windows
# routes the deepest entry's `id` to a sub-page within SettingsShellPage
# (App.xaml.cs `nav` handler → SettingsNavigation.Account → SettingsAccountPage),
# so the identical stack every other app uses works verbatim here.
ACCOUNT_PAGE_NAV = {
    "nav": {"stack": [{"view": "settings"}, {"view": "settings", "id": "account"}]}
}


def _seed_two_accounts(nest_instance, active="user"):
    """A claimed-admin account + a freshly-registered regular-user account, seeded
    into the registry active on the regular user (``active="admin"`` flips it).
    Returns (seed_map, user_actor_id, admin_actor_id). The admin is
    ``nest_instance``'s own claimed identity (so ``am-i-admin`` is true for it);
    the user is registered via the admin key."""
    admin_sk = nest_instance["admin"]["signing_key"]
    admin_actor = bytes(admin_sk.verify_key).hex()
    admin_secret = bytes(admin_sk).hex()

    user = create_actor_and_register(
        nest_instance["port"], admin_signing_key=admin_sk
    )
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
    url = nest_instance["url"]
    seed = build_registry_seed(
        [{"actor_id": user_actor, "secret_hex": user_secret, "nest_url": url,
          "device_id": "switcher-user", "handle": "user"}],
        active=user_actor,
    )
    return seed, user_actor


def _wait_switcher_count(driver, n, timeout=30):
    """Poll the registry-backed switcher UI until it lists exactly ``n`` accounts
    with one active, RE-NAVIGATING each cycle. The switch tears down and rebuilds
    the main frame (dropping the Account sub-page), and a single fire-and-wait nav
    can be dropped by a just-rebuilt frame, so re-navigate rather than wait once."""
    deadline = time.monotonic() + timeout
    last = -1
    # Which STEP failed, and why. The linux twin swallows this with a bare
    # `except Exception: pass`, which is survivable there only because the helper
    # has always passed; on a first-ever windows run it turns three distinct
    # failures (nav never landed / list never realized / rows never counted) into
    # one indistinguishable `count=-1`. testing.md point 6: a failure must
    # diagnose itself, and the diagnosis must name the step.
    last_error = "none (loop never executed — timeout <= 0?)"
    while time.monotonic() < deadline:
        try:
            driver.set_state(ACCOUNT_PAGE_NAV)
        except Exception as e:
            last_error = f"set_state(nav→settings/account) raised {type(e).__name__}: {e}"
            time.sleep(0.5)
            continue
        try:
            driver.wait_for(SWITCHER_LIST, timeout=5)
        except Exception as e:
            last_error = (
                f"nav OK, but '{SWITCHER_LIST}' never appeared: "
                f"{type(e).__name__}: {e}"
            )
            time.sleep(0.5)
            continue
        try:
            last = driver.count(SWITCHER_ITEM)
            if last == n and driver.count(ACTIVE_INDICATOR) == 1:
                return
            last_error = (
                f"'{SWITCHER_LIST}' realized, but {SWITCHER_ITEM} count={last} "
                f"(want {n}) and {ACTIVE_INDICATOR} count="
                f"{driver.count(ACTIVE_INDICATOR)} (want 1)"
            )
        except Exception as e:
            last_error = f"list realized, but counting raised {type(e).__name__}: {e}"
        time.sleep(0.5)

    # Read the app's own error surface + a tree dump before asserting — an
    # exception thrown out of SettingsAccountPage.OnNavigatedTo (which now builds
    # the credential registry + switcher VM) aborts the frame navigation, so the
    # page never renders and every element read fails. That is invisible unless
    # the error element is read here.
    app_error = ""
    try:
        app_error = driver.get_text("error-message")
    except Exception as e:
        app_error = f"(error-message unreadable: {type(e).__name__}: {e})"
    try:
        tree = driver.tree()
    except Exception as e:  # pragma: no cover - diagnostic only
        tree = f"(tree dump failed: {e})"
    raise AssertionError(
        f"switcher never reached {n} accounts (one active) within {timeout}s.\n"
        f"  last step failure: {last_error}\n"
        f"  app error-message: {app_error!r}\n"
        f"--- accessibility tree ---\n{tree}"
    )


def _wait_live_switcher_count(driver, n, timeout=15):
    """Poll the CURRENT (already-navigated) switcher page — with NO re-navigation —
    until it lists exactly ``n`` items. This is what asserts a *live* refresh:
    ``_wait_switcher_count`` re-navigates each cycle, which rebuilds the list from
    the (now-shrunken) registry and would pass even with no live update. Removing a
    non-active account does not switch (no teardown/rebuild), so the only way the
    row can vanish without navigating is an in-place refresh."""
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


def _read_registry_index(driver):
    """The persisted ``AccountIndex`` (``{active, accounts:[{actor_id,...}]}``) the
    file-backed ``LogicalSecretStore`` writes verbatim.

    THE ONE STRUCTURAL DIVERGENCE FROM THE LINUX MODULE: windows seeds and reads
    ``{cred_dir}/{keyring_app}.json`` (``drivers/windows.py`` ``launch`` →
    ``FAUNA_E2E_CREDENTIAL_DIR`` + ``FAUNA_KEYRING_APP``), whereas linux uses
    ``{tmp}/creds/fauna-e2e-agent-{agent_port}.json``.

    Reading the store file directly asserts the append's effect independent of the
    post-switch reconnect: the append-derived nest_url is https (Pillar C
    uniform-https) which the plain-http tier_3 nest can't serve, so the switch's
    live sign-in can't complete — but the registry WRITE is client-side and does.
    The read itself is the shared ``helpers/registry_store.py`` (it resolves the
    windows ``_cred_dir``/``_keyring_app`` pair as well as tui's pinned pair)."""
    return read_registry_index(driver)


def _read_store_slot(driver, key):
    """One raw logical slot out of the flat ``{cred_dir}/{keyring_app}.json``
    store — the sibling of :func:`_read_registry_index`, which reads only the
    ``fauna/index`` blob: the per-actor three-slot contract
    (``fauna/{actor}/{secret,nest_url,device_id}`` — ``long-term-store.md``
    § The three slots) lives in slots of its own, so the index alone cannot
    answer what URL an account will dial on its next launch. Windows analog
    of tui's / apple's `_read_store_slot` (same file windows' own
    `_read_registry_index` above reads). Returns the raw string, or None."""
    return _read_store_map(driver).get(key)


def _read_store_map(driver) -> dict:
    """The whole flat ``{cred_dir}/{keyring_app}.json`` store (every logical key,
    verbatim), or ``{}`` when it is absent or unreadable
    (``helpers/registry_store.py``)."""
    return read_store_map(driver)


def _wait_registry_index(driver, n, timeout=30, *, active=None):
    """Poll this app's registry file until it lists exactly ``n`` accounts (and,
    given ``active``, names it active); return it. Shared loop:
    ``helpers.waiting.wait_registry_index``."""
    return wait_registry_index(
        lambda: _read_registry_index(driver), n, active=active, budget_s=timeout
    )


def _wait_path(path, timeout=60):
    """Poll until ``path`` exists on disk (a file or dir), returning it. The app
    resolves + creates account-scoped stores off the UI thread after login/switch,
    so the filesystem effect lags the ``authenticated`` state by a moment."""
    deadline = time.monotonic() + timeout
    while time.monotonic() < deadline:
        if os.path.exists(path):
            return path
        time.sleep(0.3)
    raise AssertionError(f"path never appeared within {timeout}s: {path}")


def _write_reauth_verdict(driver, verdict):
    """Stage-2 e2e seam (``Services/AccountReauth.cs``): when
    ``FAUNA_E2E_CREDENTIAL_DIR`` is set the app reads ``{cred_dir}/reauth-result``
    instead of showing the real Windows Hello prompt. The file is read PER PROMPT,
    so one app session covers both verdicts. ``None`` removes the file — ABSENT
    reads as decline (fail-closed), the strictest arm of the seam. Byte-identical to
    apple's ``_write_reauth_verdict`` (same file convention, priority #1)."""
    path = os.path.join(driver._cred_dir, "reauth-result")
    if verdict is None:
        try:
            os.remove(path)
        except FileNotFoundError:
            pass
    else:
        with open(path, "w") as f:
            f.write(verdict)


def _wait_flag(driver, actor, want, key="require_confirm_to_activate", timeout=30):
    """Poll the persisted registry index until ``actor``'s ``key`` reads ``want``.
    Returns the ``{actor_id: flag}`` map. ``key`` defaults to the activation flag;
    pass ``require_confirm_user_set`` to read the OFF-sticks marker."""
    deadline = time.monotonic() + timeout
    flags = {}
    while time.monotonic() < deadline:
        index = _read_registry_index(driver)
        if index:
            flags = {a["actor_id"]: a.get(key, False) for a in index.get("accounts", [])}
            if flags.get(actor) == want:
                return flags
        time.sleep(0.5)
    raise AssertionError(
        f"account {actor} never reached {key}={want} within {timeout}s; last flags={flags!r}"
    )


@pytest.mark.feature("multiple-accounts")
def test_windows_account_switcher_lists_switches_and_reveals_admin(
    nest_instance, windows_app_path, request
):
    seed, _user_actor, admin_actor = _seed_two_accounts(nest_instance)

    driver = create_driver("windows")
    driver.launch({
        "app_path": windows_app_path,
        "url": nest_instance["url"],
        "seed_credentials": seed,
        "environment": _seeded_environment(request, nest_instance),
    })
    try:
        # (2) Launched active on the regular user → authenticated, NO admin shell.
        state = driver.wait_for_state(
            lambda s: bool(s.get("session", {}).get("authenticated")), timeout=60
        )
        assert state["session"]["authenticated"] is True, (
            f"active (regular) account must launch authenticated, got {state.get('session')!r}"
        )
        assert driver.is_absent(ADMIN_TAB), (
            "a regular (non-admin) active identity must have no admin shell"
        )

        # (3) Account settings lists BOTH accounts, the active one marked.
        _wait_switcher_count(driver, 2, timeout=45)

        # (4) Tap the admin row → live in-session reconnect → admin shell appears.
        driver.click(SWITCHER_ITEM, index=1)

        # Assert the reconnect through SESSION STATE as well as the admin shell.
        # The state assertion is the stronger one: `admin-tab` visibility can be
        # satisfied by a stale shell, whereas actor_id can only change if the
        # teardown/rebuild actually re-authenticated as the target identity.
        switched = driver.wait_for_state(
            lambda s: s.get("session", {}).get("actor_id") == admin_actor,
            timeout=60,
        )
        assert switched["session"]["actor_id"] == admin_actor, (
            "the live in-session reconnect must re-authenticate as the target "
            f"identity; session={switched.get('session')!r}"
        )
        driver.wait_for(ADMIN_TAB, timeout=45)
        assert driver.is_visible(ADMIN_TAB), (
            "switching to the admin identity must reveal the admin shell "
            "(live reconnect, no relaunch)"
        )
    finally:
        driver.teardown()


@pytest.mark.feature("multiple-accounts")
def test_windows_add_account_appends_second_identity_to_registry(
    nest_instance, windows_app_path, request
):
    # (0) A pre-registered SECOND identity to import in the append wizard.
    admin_sk = nest_instance["admin"]["signing_key"]
    second = create_actor_and_register(nest_instance["port"], admin_signing_key=admin_sk)
    second_actor = second["actor_id_hex"]
    second_secret = bytes(second["signing_key"]).hex()

    seed, first_actor = _seed_one_account(nest_instance)

    driver = create_driver("windows")
    driver.launch({
        "app_path": windows_app_path,
        "url": nest_instance["url"],
        "seed_credentials": seed,
        "environment": _seeded_environment(request, nest_instance),
    })
    try:
        # (1) Launched active on the single seeded account, listing exactly ONE.
        driver.wait_for_state(
            lambda s: bool(s.get("session", {}).get("authenticated")), timeout=60
        )
        assert driver.is_absent(ADMIN_TAB), (
            "a regular (non-admin) active identity must have no admin shell"
        )
        _wait_switcher_count(driver, 1, timeout=45)

        # (2) "Add account" → append-mode onboarding over the live session.
        # Unlike linux (a separate toplevel needing `set_active_onboarding_machine`),
        # windows resolves the agent's target through the static
        # `OnboardingViewModel.Current`, which self-registers in the VM constructor —
        # so a freshly-constructed append wizard becomes the target automatically.
        driver.click(ADD_BUTTON)
        driver.wait_for(IMPORT_IDENTITY_BUTTON, timeout=45)

        # (3) Import the pre-registered second identity (the paste-secret path).
        driver.click(IMPORT_IDENTITY_BUTTON)
        driver.wait_for(PASTE_SECRET_FIELD, timeout=20)
        driver.clear_and_type(PASTE_SECRET_FIELD, second_secret)
        driver.click(IMPORT_SUBMIT_BUTTON)
        driver.wait_for(HANDLE_INPUT, timeout=20)

        # (4) Install the dial override BEFORE the append completes: the switch
        # it triggers is a launch FROM THE STORE, and the URL sitting there is
        # the wizard's uniform-https derivation (`fauna_provisioning::probe::
        # resolve_handle_domain_with_local_port`), which this plain-HTTP
        # tier_3 nest cannot serve. One gesture covers both halves — the
        # machine's HTTP providers and, mirrored into
        # `fauna_launch_machine::dial`, the process-global store-read dial the
        # post-append switch resolves through. Without this the switch lands
        # `Offline(transient)` and step (7) below never reaches a session.
        driver.set_provider_base_urls({"nest": nest_instance["url"]})

        # (5) Inject the AlreadyOnNest handle-check outcome so Continue lands
        # `WizardOutcome::LoggedIn` — the append divergence's trigger
        # (`registry.AddAccount` + the same switch path).
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

        # (6) The append grew the file-backed registry 1→2 and switched to the new
        # account, with the original preserved (no-user-data-loss).
        index = _wait_registry_index(driver, 2, timeout=45, active=second_actor)
        actor_ids = [a["actor_id"] for a in index["accounts"]]
        assert first_actor in actor_ids, (
            "the original account must survive the append (no-user-data-loss)"
        )
        assert second_actor in actor_ids, "the imported account must be appended"
        assert index["active"] == second_actor, (
            "the append switches to the newly-added account (design Decision 1)"
        )

        # (7) …and that account reaches a WORKING session, not just a registry
        # row. This was the unasserted half of "Add account" on windows, and
        # building this assertion found a real bug: App.SwitchAccountHandler's
        # "already active — nothing to tear down" guard read the REGISTRY's
        # `active` pointer, which AddAccount had already moved to the new
        # actor moments earlier — so the guard treated the brand-new identity
        # as already live and skipped the entire teardown+rebuild, silently
        # stranding the app authenticated as NEITHER identity. Fixed by
        # checking the LIVE `_cryptoService` identity instead of the store
        # (`long-term-store.md` § Implementation status today records the
        # finding; `App.xaml.cs`'s `SwitchAccountHandler` carries the fix).
        await_session_actor(
            driver, second_actor, budget_s=APP_RELAUNCH_S, what="the append"
        )

        # (8) The seam redirects the SOCKET, never the truth: what the append
        # persisted for the new account is still the literal derived from the
        # typed handle domain — an https URL this plain-HTTP nest never served.
        # Asserting the store is what keeps (7) honest: an override that leaked
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


@pytest.mark.feature("multiple-accounts")
def test_windows_add_account_pending_invite_submit_switches_the_live_session(
    nest_instance, windows_app_path, request
):
    """A pending-invite submit during "Add account" moves the registry's active
    pointer AND the running session together — never the registry alone.

    The windows twin of
    ``test_account_switcher_apple.py::test_apple_add_account_pending_invite_submit_switches_the_live_session``
    (same journey, same assertions — priority #1). ``onboarding.md`` § Multi-account:
    the append wizard's pending-invite state is not an exit, so "the append glue
    adopts on the submit return — register the append identity in the account
    registry, write its per-actor pending-invite slot, switch to it". Before this
    fix, ``OnboardingViewModel.PersistPendingInviteSlot`` ran the shared writer
    (``add_account`` + ``set_active`` + the per-actor slot) and stopped: the
    registry named the still-pending joiner while the live session kept serving
    the outgoing identity, with the "Add account" wizard still up over it.

    The joiner is a fresh identity the nest has never seen, so a real invite
    request is the only way onto this claimed nest. Driven through the app's own
    UI (convention 8) up to the one hop a test nest forces —
    ``navigate_to_invite_request_for_known_nest``, because a test nest is not
    DNS-discoverable and the wizard cannot find it by handle (the same hop
    ``test_pending_invite_journey.py`` makes).

    **Pending is not connected.** The new account has no ``nest_url`` yet, so
    unlike the ``LoggedIn`` append test above there is no working session to
    reach and ``await_session_actor`` cannot be the witness. "The session agrees
    with the registry" reads instead as: the outgoing identity's session is GONE
    (one counted teardown, no authenticated actor) and the new account's own
    launch surface — the invite page, hydrated from its per-actor slot — is what
    renders. Nothing here waits on a clock: every wait is a deadline poll on
    state the app actually reaches (convention 14)."""
    seed, first_actor = _seed_one_account(nest_instance)

    joiner_key = SigningKey.generate()
    joiner_secret = bytes(joiner_key).hex()
    joiner_actor = bytes(joiner_key.verify_key).hex()
    handle = f"addpending-{int(time.time() * 1000)}"

    driver = create_driver("windows")
    driver.launch({
        "app_path": windows_app_path,
        "url": nest_instance["url"],
        "seed_credentials": seed,
        "environment": _seeded_environment(request, nest_instance),
    })
    try:
        # (1) One seeded account, authenticated. The generation read here is the
        # baseline the adoption's teardown is counted against.
        driver.wait_for_state(
            lambda s: bool(s.get("session", {}).get("authenticated")), timeout=60
        )
        _wait_switcher_count(driver, 1, timeout=45)
        generation_before = session_generation(driver)
        assert generation_before is not None, (
            "the app must publish `session_generation` (convention 14) — without it "
            "this test cannot tell a switched session from an untouched one"
        )

        # (2) "Add account" → append-mode onboarding → import the joiner's secret
        # (the paste-secret path) → the handle page. In append mode that import's
        # shared confirm-identity moment writes nothing; it registers nothing.
        driver.click(ADD_BUTTON)
        driver.wait_for(IMPORT_IDENTITY_BUTTON, timeout=45)
        driver.click(IMPORT_IDENTITY_BUTTON)
        driver.wait_for(PASTE_SECRET_FIELD, timeout=20)
        driver.clear_and_type(PASTE_SECRET_FIELD, joiner_secret)
        driver.click(IMPORT_SUBMIT_BUTTON)
        driver.wait_for(HANDLE_INPUT, timeout=20)

        # (3) Hop to `invite_request` for this claimed nest, then the REAL submit —
        # an `invite_request.submit` over the anonymous WS to `nest_instance`.
        # windows resolves the agent's target through the static
        # `OnboardingViewModel.Current`, which self-registers in the VM
        # constructor — the same seam `test_windows_add_account_appends_second_
        # identity_to_registry` above relies on.
        driver.call_machine_method(
            "navigate_to_invite_request_for_known_nest",
            json.dumps([nest_instance["url"], handle]),
        )
        driver.wait_for(INVITE_SUBMIT_BUTTON, timeout=30)
        driver.click(INVITE_SUBMIT_BUTTON)

        # (4) The registry half — the shared writer's own contract, already true
        # before the switch existed: the joiner is registered and active, the
        # original survives (no-user-data-loss), and the per-actor pending-invite
        # slot is written.
        index = _wait_registry_index(driver, 2, timeout=45, active=joiner_actor)
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
        # `SwitchAccountHandler`, which counts itself at its top
        # (`E2eSessionCounters.RecordSessionTeardown`), so a generation that
        # never moves means the registry changed under a session that never
        # noticed.
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
        # recheck affordance exists only in that state, against the joiner's own
        # (still-pending, `nest_url: None`) nest — not the default/current one.
        driver.wait_for(INVITE_RECHECK_BUTTON, timeout=45)
    finally:
        driver.teardown()


def test_windows_add_account_cancel_returns_to_live_session(
    nest_instance, windows_app_path, request
):
    """Append-mode ("Add account") escape hatch: windows onboarding is a
    frame-hosted Page with no window to close, unlike linux/apple's native
    window/sheet dismissal, so reconsidering right after opening the wizard
    stranded the user (the UX hole Slice 2 introduced). ``onboarding-cancel-button``
    renders on ``identity_choice`` ONLY while append mode is active
    (``OnboardingViewModel.IsAppendMode``, threaded from ``App.IsAppendingAccount``)
    and invokes the already-wired ``App.AbandonAddAccountHandler``.

    The registry is never mutated by an abandoned append (``add_account`` only
    happens on the wizard's ``LoggedIn`` outcome), so this is a pure re-render:
    the active account and switcher row count must be exactly as before."""
    seed, first_actor = _seed_one_account(nest_instance)

    driver = create_driver("windows")
    driver.launch({
        "app_path": windows_app_path,
        "url": nest_instance["url"],
        "seed_credentials": seed,
        "environment": _seeded_environment(request, nest_instance),
    })
    try:
        # (1) Launched active on the single seeded account, listing exactly ONE.
        driver.wait_for_state(
            lambda s: bool(s.get("session", {}).get("authenticated")), timeout=60
        )
        _wait_switcher_count(driver, 1, timeout=45)

        # (2) "Add account" → append-mode onboarding, cold identity_choice start.
        driver.click(ADD_BUTTON)
        driver.wait_for(IMPORT_IDENTITY_BUTTON, timeout=45)

        # (3) The escape hatch is visible ONLY here, in append mode — reconsider
        # immediately, before touching create/import.
        driver.wait_for(CANCEL_BUTTON, timeout=10)
        assert driver.is_visible(CANCEL_BUTTON), (
            "onboarding-cancel-button must render on identity_choice while an "
            "append-mode wizard is active"
        )
        driver.click(CANCEL_BUTTON)

        # (4) Abandoning restores the running session's UI — same active account,
        # still authenticated, registry untouched (no-user-data-loss).
        restored = driver.wait_for_state(
            lambda s: s.get("session", {}).get("actor_id") == first_actor
            and bool(s.get("session", {}).get("authenticated")),
            timeout=60,
        )
        assert restored["session"]["actor_id"] == first_actor, (
            "cancelling an append must leave the ORIGINAL account active; "
            f"session={restored.get('session')!r}"
        )

        _wait_switcher_count(driver, 1, timeout=45)
        index = _read_registry_index(driver)
        assert index is not None and len(index.get("accounts", [])) == 1, (
            f"an abandoned append must never mutate the registry; index={index!r}"
        )
        assert index["active"] == first_actor
    finally:
        driver.teardown()


@pytest.mark.feature("multiple-accounts")
def test_windows_add_account_abandon_after_import_recovers_prior_identity(
    nest_instance, windows_app_path, request
):
    """Abandoning an append-mode "Add account" wizard AFTER identity-confirm
    (not just before it, like `test_windows_add_account_cancel_returns_to_live_session`
    above) must still leave the ORIGINAL account active and the registry
    untouched — the windows twin of web's
    `test_web_add_account_abandon_recovers_prior_identity_on_current_load`.

    windows shipped the identical bug web did (`long-term-store.md` § Multi-
    account evolution): `ConfirmImportedIdentity`'s moment-1 commit routed
    UNCONDITIONALLY through the shared `persist_confirmed_identity_mirrored`,
    which `add_account`s AND `set_active`s — so importing a second identity
    inside "Add account" registered a half-account and moved `active` to it
    *before* the user ever reached Continue. Abandoning past that point (Back
    to identity_choice, then Cancel) would restore the running session's UI
    from a registry that had ALREADY been mutated: `AbandonAddAccountHandler`
    re-dispatches over whatever the registry currently calls active, which was
    by then the abandoned import, not the original account — a live-session
    hijack indistinguishable, from the user's chair, from having been silently
    switched to someone else's half-registered identity. Today append mode goes
    through the shared `ConfirmIdentity(secret, append: true)`, which writes
    nothing at all — step (3) pins that no row anywhere in the store carries the
    imported secret.

    The two-hop Back chain (`handle_entry` → `identity_import` →
    `identity_choice`) is the real navigation surface: `IsAppendMode`'s cancel
    affordance renders ONLY on `identity_choice` (every later stage's existing
    Back chain already retreats there), so reaching Cancel after import
    necessarily walks it. Reached via the SAME real paste-secret import path
    `test_windows_add_account_appends_second_identity_to_registry` uses, so the
    two tests differ only in what happens after `HANDLE_INPUT`.
    """
    admin_sk = nest_instance["admin"]["signing_key"]
    second = create_actor_and_register(nest_instance["port"], admin_signing_key=admin_sk)
    second_secret = bytes(second["signing_key"]).hex()

    seed, first_actor = _seed_one_account(nest_instance)

    driver = create_driver("windows")
    driver.launch({
        "app_path": windows_app_path,
        "url": nest_instance["url"],
        "seed_credentials": seed,
        "environment": _seeded_environment(request, nest_instance),
    })
    try:
        # (1) Launched active on the single seeded account.
        driver.wait_for_state(
            lambda s: bool(s.get("session", {}).get("authenticated")), timeout=60
        )
        _wait_switcher_count(driver, 1, timeout=45)

        # (2) "Add account" → import the second identity for REAL — reaching
        # HANDLE_INPUT proves the import committed (moment 1 already ran).
        driver.click(ADD_BUTTON)
        driver.wait_for(IMPORT_IDENTITY_BUTTON, timeout=45)
        driver.click(IMPORT_IDENTITY_BUTTON)
        driver.wait_for(PASTE_SECRET_FIELD, timeout=20)
        driver.clear_and_type(PASTE_SECRET_FIELD, second_secret)
        driver.click(IMPORT_SUBMIT_BUTTON)
        driver.wait_for(HANDLE_INPUT, timeout=20)

        # (3) THE DIRECT PROOF: moment 1 must NOT have grown the registry or
        # moved `active` — this is exactly what the unconditional
        # `persist_confirmed_identity_mirrored` call broke (it would already
        # read 2 accounts, active on the abandoned import, at this point).
        index = _read_registry_index(driver)
        assert index is not None and len(index.get("accounts", [])) == 1, (
            "importing inside append mode must NOT register the identity "
            f"before Continue; index={index!r}"
        )
        assert index["active"] == first_actor, (
            "importing inside append mode must NOT move active off the "
            f"original account before Continue; index={index!r}"
        )
        # …and it wrote NOTHING: no per-actor slot and no single-slot row holds
        # the imported secret (the shared moment is write-free in append mode).
        holders = sorted(k for k, v in _read_store_map(driver).items() if v == second_secret)
        assert not holders, (
            "an append-mode import must write nothing before its own terminal; "
            f"rows holding the imported secret: {holders!r}"
        )

        # (4) Abandon via the real Back chain, not a shortcut: handle_entry →
        # identity_import → identity_choice, then Cancel.
        driver.wait_for(HANDLE_ENTRY_BACK_BUTTON, timeout=15)
        driver.click(HANDLE_ENTRY_BACK_BUTTON)
        driver.wait_for(IDENTITY_IMPORT_BACK_BUTTON, timeout=15)
        driver.click(IDENTITY_IMPORT_BACK_BUTTON)
        driver.wait_for(CANCEL_BUTTON, timeout=15)
        driver.click(CANCEL_BUTTON)

        # (5) The running session must still be the ORIGINAL account — not the
        # abandoned import — and the registry must still show exactly one row.
        restored = driver.wait_for_state(
            lambda s: s.get("session", {}).get("actor_id") == first_actor
            and bool(s.get("session", {}).get("authenticated")),
            timeout=60,
        )
        assert restored["session"]["actor_id"] == first_actor, (
            "abandoning after import must leave the ORIGINAL account active; "
            f"session={restored.get('session')!r}"
        )
        _wait_switcher_count(driver, 1, timeout=45)
        index2 = _read_registry_index(driver)
        assert index2 is not None and len(index2.get("accounts", [])) == 1, (
            f"an abandoned append must never leave the registry grown; index={index2!r}"
        )
        assert index2["active"] == first_actor
    finally:
        driver.teardown()


@pytest.mark.feature("multiple-accounts")
def test_windows_remove_account_shrinks_switcher_live(
    nest_instance, windows_app_path, request
):
    seed, user_actor, admin_actor = _seed_two_accounts(nest_instance)

    driver = create_driver("windows")
    driver.launch({
        "app_path": windows_app_path,
        "url": nest_instance["url"],
        "seed_credentials": seed,
        "environment": _seeded_environment(request, nest_instance),
    })
    try:
        driver.wait_for_state(
            lambda s: bool(s.get("session", {}).get("authenticated")), timeout=60
        )
        _wait_switcher_count(driver, 2, timeout=45)

        # Remove the NON-ACTIVE (admin) row. `account-remove-button` renders on
        # non-active rows only, so scoping to row 1 targets the admin.
        driver.click(REMOVE_BUTTON, scope=f"{SWITCHER_ITEM}[1]")

        # The removal does not switch (no teardown/rebuild), so the row can only
        # vanish via an in-place live refresh — assert WITHOUT re-navigating.
        _wait_live_switcher_count(driver, 1, timeout=20)
        assert driver.count(ACTIVE_INDICATOR) == 1, (
            "exactly one account must remain marked active after the removal"
        )

        index = _wait_registry_index(driver, 1, timeout=30, active=user_actor)
        actor_ids = [a["actor_id"] for a in index["accounts"]]
        assert user_actor in actor_ids, (
            "removing the non-active account must never drop the ACTIVE one"
        )
        assert admin_actor not in actor_ids, "the removed account must be gone"
        assert index["active"] == user_actor
    finally:
        driver.teardown()


@pytest.mark.feature("multiple-accounts")
def test_windows_require_confirm_gates_switch_decline_then_approve(
    nest_instance, windows_app_path, request
):
    """Stage 2: a require_confirm-flagged account demands a native Windows Hello
    re-auth BEFORE the switch. Declining is a PURE no-op (registry untouched,
    session stays put, no error banner); approving completes the same live
    reconnect the unflagged path takes. The flag is written through the UI toggle
    and asserted against the persisted registry index. Windows twin of
    ``test_account_switcher_apple.py::test_apple_require_confirm_gates_switch_decline_then_approve``.

    Active is the regular user (NON-admin), so the admin auto-default never fires —
    the only flag in play is the one this test sets on the admin row."""
    seed, user_actor, admin_actor = _seed_two_accounts(nest_instance)  # active = user

    driver = create_driver("windows")
    driver.launch({
        "app_path": windows_app_path,
        "url": nest_instance["url"],
        "seed_credentials": seed,
        "environment": _seeded_environment(request, nest_instance),
    })
    try:
        driver.wait_for_state(
            lambda s: bool(s.get("session", {}).get("authenticated")), timeout=60
        )
        _wait_switcher_count(driver, 2, timeout=45)

        # (1) Flag the admin (row 1) through the UI toggle → assert it reached the
        # persisted index. The toggle renders false (no auto-default on a non-admin
        # active account), so toggling flips it ON.
        driver.click(REQUIRE_CONFIRM_TOGGLE, scope=f"{SWITCHER_ITEM}[1]")
        _wait_flag(driver, admin_actor, True, timeout=15)

        # (2) DECLINE — the absent-file arm (fail-closed, the strictest arm of the
        # seam). Tapping the flagged admin row must NOT switch. The VM reads the flag
        # fresh from the registry (never the rendered row), so the gate fires
        # regardless of render timing; the absent verdict declines it.
        _write_reauth_verdict(driver, None)
        # Convention 14: the absence is anchored to causal order, not to a window.
        # Windows has no in-app prompt to watch close — the Hello dialog carries no
        # test ID (ui.yaml: "never render"), and under the e2e seam the verdict is a
        # file read — and a declined activation is a pure no-op by product design,
        # so NOTHING on screen changes to mark the handler done. The gesture's own
        # completion counter is that mark; the barrier inside the helper then drains
        # the dispatcher queue, and the switch handler counts a teardown
        # synchronously before its first await, precisely so this check would see
        # one if it happened.
        assert_no_relaunch(
            driver,
            lambda: driver.click(SWITCHER_ITEM, index=1),
            activation_gesture_completed(driver),
            what="declining the re-auth prompt",
        )
        state = driver.get_state()
        assert state["session"]["actor_id"] == user_actor, (
            "declining re-auth must leave the ORIGINAL account active; "
            f"session={state.get('session')!r}"
        )
        assert state["session"]["authenticated"] is True, (
            "declining re-auth must not tear the session down"
        )
        assert driver.count(SWITCHER_ITEM) == 2 and driver.count(ACTIVE_INDICATOR) == 1, (
            "the account page must survive un-rebuilt after a declined switch"
        )
        assert _read_registry_index(driver)["active"] == user_actor, (
            "a declined switch must never mutate the persisted registry"
        )

        # (3) APPROVE — the same tap now completes the live reconnect and reveals the
        # admin shell.
        _write_reauth_verdict(driver, "approve")
        driver.click(SWITCHER_ITEM, index=1)
        switched = driver.wait_for_state(
            lambda s: s.get("session", {}).get("actor_id") == admin_actor, timeout=60
        )
        assert switched["session"]["authenticated"] is True
        driver.wait_for(ADMIN_TAB, timeout=45)
        assert driver.is_visible(ADMIN_TAB), (
            "approving re-auth must complete the switch and reveal the admin shell "
            "(live reconnect, no relaunch)"
        )
    finally:
        driver.teardown()


@pytest.mark.feature("multiple-accounts")
def test_windows_admin_auto_default_flags_admin_and_explicit_off_sticks(
    nest_instance, windows_app_path, request
):
    """Stage 2: launching authenticated as the ADMIN auto-enables its require-confirm
    flag at the am-i-admin observation with NO user tap; the regular account stays
    unflagged; an explicit OFF sticks across a fresh observation because
    ``require_confirm_user_set`` pins it. Windows twin of
    ``test_account_switcher_apple.py::test_apple_admin_auto_default_flags_admin_and_explicit_off_sticks``.

    The am-i-admin probe fires from ``MainPage.CheckAdminStatusAsync`` (an async nest
    RPC), which can land AFTER the first navigation to the account page — so the test
    re-navigates once the store shows the auto-default before toggling OFF, otherwise
    a stale-false-rendered toggle would flip the wrong way. This is the exact
    build-once-render hazard the goal doc names; windows re-reads on page-visible."""
    seed, user_actor, admin_actor = _seed_two_accounts(nest_instance, active="admin")

    driver = create_driver("windows")
    driver.launch({
        "app_path": windows_app_path,
        "url": nest_instance["url"],
        "seed_credentials": seed,
        "environment": _seeded_environment(request, nest_instance),
    })
    try:
        driver.wait_for_state(
            lambda s: s.get("session", {}).get("actor_id") == admin_actor, timeout=60
        )
        _wait_switcher_count(driver, 2, timeout=45)

        # (1) Auto-enabled on the ADMIN with NO user tap; the regular account stays
        # untouched (long-term-store.md — the auto-default flags only the admin).
        flags = _wait_flag(driver, admin_actor, True, timeout=30)
        assert flags.get(user_actor) is False, (
            "the auto-default must flag ONLY the admin identity"
        )

        # (2) Explicit user OFF on the admin (the ACTIVE row 1 — its toggle renders
        # on the active row too). Re-navigate first so the toggle reflects the
        # now-true store flag, then toggling flips it OFF.
        _wait_switcher_count(driver, 2, timeout=30)
        driver.click(REQUIRE_CONFIRM_TOGGLE, scope=f"{SWITCHER_ITEM}[1]")
        _wait_flag(driver, admin_actor, False, timeout=10)
        index = _read_registry_index(driver)
        admin_entry = next(a for a in index["accounts"] if a["actor_id"] == admin_actor)
        assert admin_entry.get("require_confirm_user_set") is True, (
            "an explicit toggle must set require_confirm_user_set (the OFF-sticks pin)"
        )

        # (3) Re-trigger the am-i-admin observation (switch away and back). Both
        # accounts are unflagged now, so no re-auth is involved and no verdict file
        # is needed.
        driver.click(SWITCHER_ITEM, index=0)
        driver.wait_for_state(
            lambda s: s.get("session", {}).get("actor_id") == user_actor, timeout=60
        )
        _wait_switcher_count(driver, 2, timeout=30)
        driver.click(SWITCHER_ITEM, index=1)
        driver.wait_for_state(
            lambda s: s.get("session", {}).get("actor_id") == admin_actor, timeout=60
        )
        time.sleep(4.0)  # bounded window for the refused (idempotent) auto-default
        index = _read_registry_index(driver)
        admin_entry = next(a for a in index["accounts"] if a["actor_id"] == admin_actor)
        assert admin_entry.get("require_confirm_to_activate") is False, (
            "an explicit OFF must stick against the admin auto-default "
            "(require_confirm_user_set pins it)"
        )
    finally:
        driver.teardown()


@pytest.mark.feature("multiple-accounts")
def test_windows_abandoned_create_identity_does_not_ghost_the_switcher(
    nest_instance, windows_app_path, request
):
    """Ghost-row regression (long-term-store.md § Eager vs. lazy migration at native
    boot): quitting mid-create and later onboarding a DIFFERENT identity must not
    leave the abandoned one behind as a handle-less, nest-less, activatable switcher
    row.

    ⚠ This test has caught the ghost through TWO different producers, and its
    docstring described only the first for two weeks. Originally the culprit was
    eager boot migration: the wizard persisted a CREATED identity's secret to the
    legacy slot at Continue, before it was ever registered, and the old
    ``FfiAccountRegistry.EnsureMigrated()`` boot call minted a real index entry for
    it on the VERY NEXT LAUNCH. Dropping that call made this test belt-and-braces
    — until moment 1 (2026-08-15) made ``CommitConfirmedIdentity`` ->
    ``persist_confirmed_identity_mirrored`` register AND activate the identity at
    Continue on all seven apps, reintroducing the ghost through the front door
    while the whole test was still timing out at step (2) on an unrelated driver
    bug and so had never once reached step (5). It is NOT belt-and-braces:
    it failed for real the first time it ran.

    The fix is shared Rust, not windows glue — moment 1 now retires the superseded
    provisional row as it commits
    (``AccountRegistry::retire_superseded_provisionals``), so every app is covered;
    tui pins the same sequence in-process
    (``abandoning_a_created_identity_leaves_no_second_switcher_row``).

    Sequence: fresh install -> create identity A -> Continue (persists A's secret,
    lands at handle_entry) -> quit + relaunch (``driver.recover()`` under a
    ``preserve_state_across_relaunch()`` pin, so the credential store survives it
    — boot reads A back and re-seeds handle_entry, but writes NO index entry)
    -> Back (``identity_origin`` is ``None`` for a boot-seeded identity, so Back
    routes to identity_choice, per ``OnboardingMachine::back``'s ``None =>
    IdentityChoice`` arm — NOT ``identity_created``, which is only reachable from a
    same-session create) -> import a SECOND, pre-registered identity B and complete
    its onboarding for real -> the switcher must show exactly ONE row (B), never
    two.
    """
    # A second, pre-registered identity to complete onboarding with for real.
    admin_sk = nest_instance["admin"]["signing_key"]
    second = create_actor_and_register(nest_instance["port"], admin_signing_key=admin_sk)
    second_secret = bytes(second["signing_key"]).hex()

    driver = create_driver("windows")
    driver.launch({
        "app_path": windows_app_path,
        "url": nest_instance["url"],
        "environment": _seeded_environment(request, nest_instance),
    })
    try:
        # (1) Fresh install -> identity_choice -> create identity A -> Continue.
        # ConfirmGeneratedIdentity commits A HERE through the shared moment 1
        # (registered + active, a provisional row with no nest yet) — the row the
        # later commit of B must retire rather than leave behind as a ghost.
        driver.wait_for(CREATE_IDENTITY_BUTTON, timeout=45)
        driver.click(CREATE_IDENTITY_BUTTON)
        driver.wait_for(IDENTITY_CONTINUE_BUTTON, timeout=15)
        driver.click(IDENTITY_CONTINUE_BUTTON)
        # Sign-up now offers the recovery kit before the handle (windows renders it, as linux does); skipping it is the user's own choice.
        driver.wait_for(RECOVERY_KIT_SKIP_BUTTON, timeout=20)
        driver.click(RECOVERY_KIT_SKIP_BUTTON)
        driver.wait_for(HANDLE_INPUT, timeout=20)

        # (2) Quit + relaunch, SAME credential dir. Boot reads A's persisted secret
        # back through the registry (App.xaml.cs's active-account session
        # material) and the LaunchMachine re-seeds the wizard at handle_entry.
        # recover() itself now strips a frozen --nest-url
        # before reposting (drivers/windows.py) — without that fix this
        # relaunch replayed the CLI override baked into this driver's one-and-
        # only launch() call, which wins over the vault and made the machine
        # verify A against a KNOWN nest (logged `fauna.auth.not_registered`)
        # instead of taking the "(identity, no node_url)" branch this step needs.
        # The "SAME credential dir" this step needs is now the PINNED relaunch, not
        # the default one: a default relaunch empties the store between the kill and
        # the new process (so the relaunched app comes back signed out, the contract
        # every driver shares), which would leave boot with no persisted secret to
        # read and land the wizard on identity_choice instead of handle_entry.
        assert driver.preserve_state_across_relaunch(), (
            "this step reads A's persisted secret back after the relaunch — the "
            "store has to be pinned for that to be what boot finds"
        )
        assert driver.recover(), "force-quit + relaunch failed"
        driver.wait_for(HANDLE_INPUT, timeout=45)

        # (3) Abandon A: Back from a BOOT-SEEDED handle_entry (identity_origin ==
        # None) routes to identity_choice, not identity_created.
        driver.wait_for(HANDLE_ENTRY_BACK_BUTTON, timeout=15)
        driver.click(HANDLE_ENTRY_BACK_BUTTON)
        driver.wait_for(CREATE_IDENTITY_BUTTON, timeout=15)
        assert driver.is_visible(CREATE_IDENTITY_BUTTON) and driver.is_visible(
            IMPORT_IDENTITY_BUTTON
        ), "Back from a boot-seeded handle_entry must land on identity_choice"

        # (4) Onboard the SECOND identity for real: import + the AlreadyOnNest
        # handle-check happy path (same injection technique as the append-mode test
        # above), reaching Done -> LoggedIn -> the FIRST real index write.
        driver.click(IMPORT_IDENTITY_BUTTON)
        driver.wait_for(PASTE_SECRET_FIELD, timeout=20)
        driver.clear_and_type(PASTE_SECRET_FIELD, second_secret)
        driver.click(IMPORT_SUBMIT_BUTTON)
        driver.wait_for(HANDLE_INPUT, timeout=20)

        handle = f"user-ghost-check@localhost:{nest_instance['port']}"
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

        # ⚠ There is deliberately NO `wait_for_state(session.authenticated)` barrier
        # here, and re-adding one will red this test. It carried one until
        # 2026-08-09, and that wait was VACUOUS: windows published
        # `session.authenticated` as "an identity key is loaded", while step (2)'s
        # relaunch loads the ABANDONED identity A's persisted secret — so the
        # predicate had been true since step (2), for the wrong identity, and
        # returned instantly. Once the agent was fixed to mean "the authenticated
        # main app is mounted" (e2e-conventions.md § convention 11), the same wait
        # timed out at 60s, and a self-diagnosing probe classified it in one run:
        # `visible={feed: False, handle_entry: False, identity_choice: False}` with
        # `handle: None` — i.e. this arranged completion leaves the app on none of
        # those surfaces and never re-dispatches into Online.
        #
        # ANSWERED 2026-08-10 (CLOSED), and the answer is neither of the two
        # candidates that row proposed. It is NOT a product routing gap — a real
        # completion reaches the main app on windows, pinned by
        # `test_onboarding_launch_routing_smoke.py::test_smoke_k_real_onboarding_
        # completion_reaches_the_main_app`. And it is NOT "the injection seam can't
        # reach LoggedIn" — `submit_handle_check_continue`'s `AlreadyOnNest` arm is
        # explicitly independent of whether the probe phases ran, so it completes
        # here too. What actually happens: that arm derives the session nest URL
        # from the TYPED handle via `resolve_handle_domain`, which is uniform
        # **https** for any explicit-port host (`libs/fauna-provisioning/src/
        # probe.rs`) — while `nest_instance` serves plain HTTP (`conftest.py`
        # defaults `serve_tls=False`). So the wizard completes correctly and hands
        # the app `https://localhost:<port>`, which it cannot dial; the app is on
        # the LAUNCH-RETRY surface, the one surface that probe never checked for.
        # Re-adding an arrival barrier therefore still reds this test — the fix
        # would be a `handle_domain` + `serve_tls=True` nest (see case K's
        # `handled_nest`), which is a fixture change this test does not need: its
        # subject is the GHOST ROW, and step (5) below is its real barrier —
        # `_wait_switcher_count` re-navigates each cycle (which mounts the main
        # frame itself) and diagnoses its own failure.

        # (5) The whole point: exactly ONE row, never the abandoned A as a ghost.
        _wait_switcher_count(driver, 1, timeout=45)
        index = _read_registry_index(driver)
        assert index is not None and len(index.get("accounts", [])) == 1, (
            f"the abandoned CREATE-identity flow left a ghost row; index={index!r}"
        )
        assert index["active"] == second["actor_id_hex"], (
            "the switcher's single row must be the identity that actually "
            f"completed onboarding, not a ghost; index={index!r}"
        )
    finally:
        driver.teardown()


@pytest.mark.feature("multiple-accounts")
def test_windows_account_switch_isolates_scoped_state_on_disk(
    nest_instance, windows_app_path, request
):
    """Account-scoping isolation leg (account-scoping.md § Serialized switching): a
    switch leaves NO cross-account readable state on disk, and nothing
    account-scoped is ever written directly into the base.

    Each account's content stores live under ``<data_dir>/<actor-id-hex>/`` — the app
    resolves that scope at login (the conversations session's MLS store,
    ``AccountStateDir.MlsDbPath``) and again on the switch. A flat store left at the
    base is never adopted by either account: the pre-scoping first-adopter hand-off
    was removed by the compat-remnant sweep (version-compatibility.md § Dimension 2,
    the fourth ratified exception), and this is its end-to-end refusal pin.

    ``data_dir`` is supplied (not the driver's per-instance mkdtemp), so this test
    owns pre-seeding the base AND cleaning it up (the driver leaves a
    caller-supplied data_dir alone — ``drivers/windows.py`` ``_owns_data_dir``)."""
    data_dir = tempfile.mkdtemp(prefix="fauna-e2e-acctscope-")
    # A flat store at the base — the shape the removed adoption used to hand to the
    # first account to resolve its scope.
    flat_mls = os.path.join(data_dir, "mls.db")
    with open(flat_mls, "wb") as f:
        f.write(b"not an account's store")

    seed, user_actor, admin_actor = _seed_two_accounts(nest_instance)
    driver = create_driver("windows")
    driver.launch({
        "app_path": windows_app_path,
        "url": nest_instance["url"],
        "seed_credentials": seed,
        "data_dir": data_dir,
        "environment": _seeded_environment(request, nest_instance),
    })
    try:
        # (1) Launched active on A (regular user).
        driver.wait_for_state(
            lambda s: bool(s.get("session", {}).get("authenticated")), timeout=60
        )

        # (2) A's scope was resolved at login — and the flat store was NOT adopted.
        a_scope = os.path.join(data_dir, user_actor)
        _wait_path(a_scope, timeout=60)
        a_mls = os.path.join(a_scope, "mls.db")
        if os.path.exists(a_mls):
            with open(a_mls, "rb") as f:
                assert f.read() != b"not an account's store", (
                    "the flat mls.db must never be adopted into the active account's scope"
                )
        assert not os.path.exists(os.path.join(data_dir, "state-owner")), (
            "no first-adopter marker is ever written"
        )

        # (3) Switch to B (admin) → live in-session reconnect (no relaunch).
        _wait_switcher_count(driver, 2, timeout=45)
        driver.click(SWITCHER_ITEM, index=1)
        driver.wait_for_state(
            lambda s: s.get("session", {}).get("actor_id") == admin_actor, timeout=60
        )

        # (4) B's scope is DISTINCT — the isolation contract: account B cannot read
        #     account A's local state, and A's scope survives the switch.
        b_scope = os.path.join(data_dir, admin_actor)
        _wait_path(b_scope, timeout=60)
        assert a_scope != b_scope
        assert os.path.isdir(a_scope), "A's scope must survive the switch, isolated from B"
        # Nothing account-scoped landed at the base, and no unresolved scope was minted.
        assert not os.path.exists(os.path.join(data_dir, "-unresolved-")), (
            "both sessions named their account — nothing may resolve unscoped"
        )
    finally:
        driver.teardown()
        shutil.rmtree(data_dir, ignore_errors=True)


@pytest.mark.feature("second-identity-in-its-own-window")
def test_windows_open_as_new_instance_spawns_a_bound_sibling(
    nest_instance, windows_app_path, request
):
    """The switcher row's ``account-open-new-instance-button`` (account-scoping.md
    § Concurrent instances → "the running instance's surface"): clicking it on a
    NON-active row starts a second app process bound to that row's account, which
    authenticates as that account while this window stays on its own.

    The load-bearing assertion is the CHILD's ``session.actor_id``, not merely that
    a process appeared. A child that read the ACTIVE account's material would route
    its launch machine on the bound account and then build its session from the
    active one — a second window that looks right and IS the wrong account, silent
    by construction. Only comparing the two live instances' resolved
    actors catches it (apple proved that the hard way; the same assertion is the
    third case of ``test_account_instance_lock_windows.py``).

    **Why the child gets its own bridge.** Windows' test agent is an outbound poller
    against a bridge URL, not a server on a free port, so it has no analogue of
    linux's/apple's per-child ``FAUNA_E2E_AGENT_PORT``: a child that inherited its
    parent's bridge would steal the parent's commands and clobber its state pushes.
    So ``InstanceSpawner`` never passes its own bridge down, and a test that wants to
    observe the child stands up a second, app-less bridge
    (``driver.start_bridge_only()``) and names it in ``FAUNA_E2E_CHILD_BRIDGE``. The
    observer drives nothing — it only reads ``/app/state``.
    """
    seed, user_actor, admin_actor = _seed_two_accounts(nest_instance)
    # ONE shared install world: the child inherits the parent's environment, so it
    # reads the same registry and contends on the same per-account lock files. A
    # supplied data_dir is the caller's to clean up (`_owns_data_dir`).
    world_base = tempfile.mkdtemp(prefix="fauna-e2e-win-spawn-world-")
    world = {
        "credential_dir": os.path.join(world_base, "credentials"),
        "keyring_app": f"fauna-e2e-spawn-world-{os.path.basename(world_base)}",
        "data_dir": os.path.join(world_base, "data"),
    }

    observer = create_driver("windows")
    child_bridge = observer.start_bridge_only()

    driver = create_driver("windows")
    driver.launch({
        "app_path": windows_app_path,
        "url": nest_instance["url"],
        "seed_credentials": seed,
        "environment": {
            **_seeded_environment(request, nest_instance),
            "FAUNA_E2E_CHILD_BRIDGE": child_bridge,
        },
        **world,
    })
    try:
        driver.wait_for_state(
            lambda s: s.get("session", {}).get("actor_id") == user_actor
            and bool(s.get("session", {}).get("authenticated")),
            timeout=60,
        )
        # Row 0 = user (active), row 1 = admin. Since windows' W5.6
        # (account-data-plane.md § Workstreams) retirement (
        # 2026-08-24) the button renders on EVERY row, active included
        # (account-scoping.md § Concurrent instances — the restriction that
        # reserved it for non-active rows only has lapsed; mirrors linux's own
        # test_linux_open_as_new_instance_spawns_a_bound_sibling), so
        # `index=1` targets the admin row explicitly rather than relying on
        # the bare ID being the only match.
        _wait_switcher_count(driver, 2, timeout=45)
        driver.click(OPEN_NEW_INSTANCE_BUTTON, index=1)

        # The spawn record appears in the parent's state — the cross-app
        # `spawned_instances` shape (apple's stateRecords, linux's spawned_instances).
        state = driver.wait_for_state(
            lambda s: any(
                r.get("actor_id") == admin_actor and r.get("pid")
                for r in s.get("spawned_instances", [])
            ),
            timeout=30,
        )
        assert any(
            r["actor_id"] == admin_actor for r in state["spawned_instances"]
        ), f"the spawn must be recorded for the picked account; got {state.get('spawned_instances')!r}"

        # The child comes up BOUND: authenticated as the admin, over its own bridge.
        child = observer.wait_for_state(
            lambda s: s.get("session", {}).get("actor_id") == admin_actor
            and bool(s.get("session", {}).get("authenticated")),
            timeout=90,
        )
        assert child["session"]["actor_id"] == admin_actor, (
            "the spawned sibling must authenticate as its BOUND account, not the "
            "store-active one — reading the active account here is what makes a bound "
            f"launch silently inert; got {child.get('session')!r}"
        )

        # ...and the parent is untouched: same account, still authenticated.
        parent = driver.get_state()
        assert parent["session"]["actor_id"] == user_actor, (
            "the parent must stay on its own account after spawning a sibling"
        )
        assert bool(parent["session"]["authenticated"]), (
            "a spawned sibling must not disturb the instance that spawned it"
        )
    finally:
        # The observer's teardown DELETEs its (never-created) session, which the
        # bridge's ProcessManager turns into a terminate of whatever child reported
        # to it — the spawned instance. Torn down before the parent so the child
        # never outlives the run.
        observer.teardown()
        driver.teardown()
        shutil.rmtree(world_base, ignore_errors=True)


# ---------------------------------------------------------------------------
# The tui-first witnesses, lifted to windows: remove erases only that
# identity's data (6), an unlaunchable switch is refused and said (9), device
# settings stay while a draft follows its identity (10). tui twins:
# `test_account_switcher_tui.py`; linux ports: `test_account_switcher_linux.py`.
# ---------------------------------------------------------------------------

AUTOSTART_TOGGLE = "settings-autostart-toggle"
GENERAL_PAGE_NAV = {
    "nav": {"stack": [{"view": "settings"}, {"view": "settings", "id": "general"}]}
}
COMPOSE_FIELD = "compose-text-field"


def _await_working_session(driver, actor_id, what):
    """Authenticated as ``actor_id`` — the switch's rebuild has completed as the
    incoming account. linux's and tui's helper of the same name also waits on
    ``data.conv_real_backend_active``; on windows that flag is set only under the
    session-wide ``real_conversations`` marker (the real MLS manager registers at
    login only then), which would put every other test in this module on the real
    backend too, so these journeys anchor on the session and, where they need an
    account's on-disk state, on its scope directory instead."""

    def _served() -> bool:
        s = driver.get_state() or {}
        session = s.get("session", {})
        return session.get("actor_id") == actor_id and bool(session.get("authenticated"))

    wait_until(
        _served,
        APP_RELAUNCH_S,
        diagnose=lambda: (
            f"after {what}: session={(driver.get_state() or {}).get('session')!r}; "
            f"app error: {_app_error(driver)!r}"
        ),
    )


def _wait_for_path(path, what):
    wait_until(
        lambda: os.path.exists(path),
        UI_SETTLE_S,
        diagnose=lambda: f"{what}: {path} never appeared",
    )


def _app_error(driver):
    """The app's own error text over the state protocol (``messages.error`` =
    ``App.CurrentErrorMessage``), for a failure message."""
    try:
        return (driver.get_state() or {}).get("messages", {}).get("error")
    except Exception as e:  # noqa: BLE001 - diagnostic only
        return f"(unreadable: {type(e).__name__}: {e})"


@pytest.mark.feature("multiple-accounts")
def test_windows_removing_an_identity_deletes_its_data_and_leaves_the_others(
    nest_instance, windows_app_path, request
):
    """Removing an identity deletes THAT identity's data on this device and
    leaves every other identity's data untouched (`account-scoping.md` § The
    scoping taxonomy; windows' erase door is the switcher VM's
    `AccountStateDir.Erase` after a successful registry remove).

    `test_windows_remove_account_shrinks_switcher_live` proves the row and the
    registry entry go; neither says anything about the account's on-device
    state — the half a user cannot see and cannot get back. Both identities
    first run a real session each (the admin's through a switch), so each owns
    a populated scope; the removal must then leave no admin scope under ANY
    base the erase sweeps (`common.scope_store.WindowsScopeStore`: the flat
    base and the unified store root) while the user's scope and secret are
    still there. Every wait is on state."""
    seed, user_actor, admin_actor = _seed_two_accounts(nest_instance)

    driver = create_driver("windows")
    driver.launch({
        "app_path": windows_app_path,
        "url": nest_instance["url"],
        "seed_credentials": seed,
        "environment": _seeded_environment(request, nest_instance),
    })
    try:
        _await_working_session(driver, user_actor, "the launch as the user")
        store = attach_scope_store("windows", driver)
        user_scope = store.scope_dirs(user_actor)[0]
        _wait_for_path(user_scope, "the user's own account scope")

        # Give the admin a real session of its own, so it has data to lose.
        _wait_switcher_count(driver, 2, timeout=45)
        driver.click(SWITCHER_ITEM, index=1)
        _await_working_session(driver, admin_actor, "the switch to the admin")
        admin_dirs = store.scope_dirs(admin_actor)
        _wait_for_path(admin_dirs[0], "the admin's own account scope")

        # Back to the user: the admin row is now the non-active one, the only row
        # the switcher offers a remove button on.
        _wait_switcher_count(driver, 2, timeout=45)
        driver.click(SWITCHER_ITEM, index=0)
        _await_working_session(driver, user_actor, "the switch back to the user")

        _wait_switcher_count(driver, 2, timeout=45)
        driver.click(REMOVE_BUTTON, scope=f"{SWITCHER_ITEM}[1]")
        index = _wait_registry_index(driver, 1, timeout=30)
        assert [a["actor_id"] for a in index["accounts"]] == [user_actor], (
            f"only the removed admin may leave the registry; got {index!r}"
        )

        wait_until(
            lambda: not any(d.exists() for d in admin_dirs),
            RPC_ROUNDTRIP_S,
            diagnose=lambda: (
                "the removed identity's scope survived under: "
                f"{[str(d) for d in admin_dirs if d.exists()]}; app error: "
                f"{_app_error(driver)!r}"
            ),
        )
        assert user_scope.is_dir(), (
            "removing the admin must leave the remaining identity's data "
            f"untouched; {user_scope} is gone"
        )
        assert _read_store_slot(driver, f"fauna/{admin_actor}/secret") is None, (
            "the removed identity's secret must be gone from this device"
        )
        assert _read_store_slot(driver, f"fauna/{user_actor}/secret"), (
            "the remaining identity's secret must survive the removal"
        )
    finally:
        driver.teardown()


@pytest.mark.feature("multiple-accounts")
def test_windows_switching_to_an_identity_the_device_cannot_sign_in_as_is_refused(
    nest_instance, windows_app_path, request
):
    """Switching to an identity this device can no longer sign in as is
    refused, says so, and leaves the user on the identity they were using
    (`long-term-store.md` § Multi-account evolution, "Activating refuses an
    account it cannot launch as").

    The seed leaves the admin listed with no `fauna/<actor>/secret` slot — the
    shape a keystore write that silently never landed produces. The shared
    `set_active` refuses before any teardown with `FfiException.General`, whose
    message IS the shared `switch_refused_copy` line; the switch handler hands it
    back to the Account page's switcher, which paints it on `error-message`
    through `Strings.Error`. Both reads are asserted: the state protocol's
    `messages.error` must be the bare line (not the exception's aggregated
    `@msg=…` text), and the page's own `error-message` element must show it
    (its accessible text carries the InfoBar's severity-icon prefix, so
    `endswith`). No relaunch happens, and the live session and the persisted
    active pointer are both still the user's."""
    seed, user_actor, admin_actor = _seed_two_accounts(nest_instance)
    seed.pop(f"fauna/{admin_actor}/secret")

    driver = create_driver("windows")
    driver.launch({
        "app_path": windows_app_path,
        "url": nest_instance["url"],
        "seed_credentials": seed,
        "environment": _seeded_environment(request, nest_instance),
    })
    try:
        _await_working_session(driver, user_actor, "the launch as the user")
        _wait_switcher_count(driver, 2, timeout=45)

        label = driver.get_text(ITEM_HANDLE, index=1)
        expected = S.settings.switch_refused_no_secret(account=label)
        assert_no_relaunch(
            driver,
            lambda: driver.click(SWITCHER_ITEM, index=1),
            lambda: _app_error(driver) == expected,
            what="switching to an identity with no secret on this device",
        )
        assert driver.is_visible_scrolled("error-message"), (
            "the refusal must be shown on the Account page, not only published; "
            f"state error={_app_error(driver)!r}"
        )
        shown = driver.get_text("error-message")
        assert shown.endswith(expected), (
            f"error-message must show the shared refusal line; shows {shown!r}, "
            f"want (after the icon prefix) {expected!r}"
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
def test_windows_switching_identity_keeps_device_settings_and_moves_drafts(
    nest_instance, windows_app_path, request
):
    """Settings that describe this device stay as they are across a switch,
    while a draft — a choice tied to an identity — follows its identity
    (`account-scoping.md` § Serialized switching and § The scoping taxonomy,
    class 2 vs class 1).

    windows' device setting is start-at-login (`settings-autostart-toggle`),
    persisted install-wide by `AppSettingsStore.AutoStartChoice` and named by
    no actor scope. The identity-tied half is the feed composer's draft, which
    rests in that identity's own sealed `__drafts` plane on the nest. The arc:
    as the user, turn start-at-login off and leave a draft (confirmed on the
    nest before switching, so the switch tests scoping, not the debounce);
    switch to the admin → still off, and the composer does NOT hold the user's
    draft; switch back → the user's draft is there again and start-at-login
    still off. The drafts read is the sanctioned side-channel verification
    (convention 8's carve-out); the mutations are all UI. linux's twin keeps
    the same device setting; tui's keeps its external-media choice."""
    from actions.feed import FeedActions
    from clients.ws_rpc_admin_client import WsRpcAdminClient

    admin_sk = nest_instance["admin"]["signing_key"]
    admin_actor = bytes(admin_sk.verify_key).hex()
    user = create_actor_and_register(nest_instance["port"], admin_signing_key=admin_sk)
    user_actor = user["actor_id_hex"]
    seed = build_registry_seed(
        [
            {"actor_id": user_actor, "secret_hex": bytes(user["signing_key"]).hex(),
             "nest_url": nest_instance["url"], "device_id": "scope-user", "handle": "user"},
            {"actor_id": admin_actor, "secret_hex": bytes(admin_sk).hex(),
             "nest_url": nest_instance["url"], "device_id": "scope-admin", "handle": "admin"},
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

    driver = create_driver("windows")
    driver.launch({
        "app_path": windows_app_path,
        "url": nest_instance["url"],
        "seed_credentials": seed,
        "environment": _seeded_environment(request, nest_instance),
    })
    feed = FeedActions(driver)

    def start_at_login():
        driver.set_state(GENERAL_PAGE_NAV)
        driver.wait_for(AUTOSTART_TOGGLE, timeout=15)
        return driver.get_attr(AUTOSTART_TOGGLE, "checked")

    def composer_body():
        feed.navigate()
        feed.open_composer()
        return feed.compose_body_text()

    try:
        _await_working_session(driver, user_actor, "the launch as the user")

        # (1) As the user: a device choice and a draft.
        assert start_at_login() == "true", "precondition: the fresh install's default"
        driver.click(AUTOSTART_TOGGLE)
        assert driver.get_attr(AUTOSTART_TOGGLE, "checked") == "false"
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
            diagnose=lambda: "the user's draft never reached its __drafts plane",
        )

        # (2) Switch to the admin.
        _wait_switcher_count(driver, 2, timeout=45)
        driver.click(SWITCHER_ITEM, index=1)
        _await_working_session(driver, admin_actor, "the switch to the admin")
        assert start_at_login() == "false", (
            "a setting that describes this device must survive the switch; "
            f"start-at-login reads {driver.get_attr(AUTOSTART_TOGGLE, 'checked')!r}"
        )
        assert composer_body() != draft, (
            "the user's draft must not follow the device to another identity"
        )

        # (3) Switch back: the user's draft is theirs again, the device choice is
        # still the one made.
        _wait_switcher_count(driver, 2, timeout=45)
        driver.click(SWITCHER_ITEM, index=0)
        _await_working_session(driver, user_actor, "the switch back to the user")
        feed.navigate()
        feed.open_composer()
        wait_until(
            lambda: feed.compose_body_text() == draft, RPC_ROUNDTRIP_S,
            diagnose=lambda: f"the user's composer reads {feed.compose_body_text()!r}",
        )
        assert start_at_login() == "false", (
            "the device setting must still hold after switching back; reads "
            f"{driver.get_attr(AUTOSTART_TOGGLE, 'checked')!r}"
        )
    finally:
        driver.teardown()
