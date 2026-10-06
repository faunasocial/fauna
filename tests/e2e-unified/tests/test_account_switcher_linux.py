"""tier_3 e2e: linux multi-account account switcher (Stage 1 Slice 2).

Multi-account clients let one install (one OS-user context) hold several Fauna
identities and switch between them (`docs/goal/architecture/long-term-store.md`
§ Multi-account evolution; switch-first design, Decision 1 — tracked
internally). This drives the linux reference implementation:

1. Seed TWO registered identities into the file-backed credential store as a
   full ``AccountRegistry`` state (a claimed admin + a regular user), active on
   the regular user. ``build_registry_seed`` writes the exact logical-key layout
   the ``CredentialStore`` File backend reads verbatim — no app code.
2. Launch ``fauna-desktop`` → it authenticates as the active (regular) account;
   the admin shell (``admin-tab``) is absent.
3. Account settings lists BOTH accounts with the active one marked.
4. Tapping the admin account switches to it with a live in-session reconnect
   (no relaunch) → the admin shell appears.

Step 4 is also the multi-account home of the "set_state can't re-auth a running
client" gap (memory ``set-state-does-not-reauth-running-app``): the switch IS an
in-session re-auth.

tier_3: needs a real ``fauna-nest`` binary (``nest_instance``); linux driver only.
"""
from __future__ import annotations

import json
import os
import time
import urllib.request
from types import SimpleNamespace

import pytest
from nacl.signing import SigningKey

from actions.conversations import ConversationsActions
from common import build_registry_seed, create_actor_and_register
from common.cred_store import LibsecretCredStore
from common.keyring import ACCT_SECRET_KEY, secret_service_available, unique_namespace
from common.scope_store import XdgScopeStore
from conftest import _seeded_environment
from drivers import create_driver
from drivers.machine_test_setter import set_handle_check_snapshot
from helpers.app_surface import skip_environment
from helpers.budgets import APP_RELAUNCH_S, RPC_ROUNDTRIP_S, UI_SETTLE_S
from helpers.waiting import (
    assert_no_relaunch,
    await_session_actor,
    session_generation,
    wait_registry_index,
    wait_until,
)
from i18n.strings import S

pytestmark = [pytest.mark.tier_3, pytest.mark.linux]

# CredentialStore maps the multi-account index key `fauna/index` to a
# libsecret item whose `account` attribute is that logical key verbatim (every
# registry key is). Reading it back is the real-keyring analog of web's
# `localStorage['fauna/index']`.
INDEX_ACCOUNT = "fauna/index"

# tests/e2e-unified/ui.yaml § settings (switcher) + navigation (admin-tab).
SWITCHER_LIST = "account-switcher-list"
SWITCHER_ITEM = "account-switcher-item"
ITEM_HANDLE = "account-item-handle"
ACTIVE_INDICATOR = "account-item-active-indicator"
ADD_BUTTON = "account-add-button"
REMOVE_BUTTON = "account-remove-button"
ADMIN_TAB = "admin-tab"
OPEN_NEW_INSTANCE_BUTTON = "account-open-new-instance-button"

# Stage 2 (re-auth-on-activate). Linux has no native OS re-auth prompt, so it
# renders the in-app confirm surface — the shape this client ratified
# (long-term-store.md § Multi-account evolution → Per-account re-auth; web + tui
# adopt it). Unlike apple, there is deliberately NO `reauth-result` file seam:
# apple needs one because an OS sheet carries no test ID, whereas this prompt is
# in-app and drivable, so the journeys below click the real UI a user clicks
# (testing.md point 8) rather than a back door no user can reach.
REQUIRE_CONFIRM_TOGGLE = "account-require-confirm-toggle"
REAUTH_PROMPT = "account-activate-reauth-prompt"
REAUTH_CONFIRM_BUTTON = "account-activate-reauth-confirm-button"
REAUTH_CANCEL_BUTTON = "account-activate-reauth-cancel-button"

# tests/e2e-unified/ui.yaml § onboarding — the append-mode "Add account" wizard.
IMPORT_IDENTITY_BUTTON = "import-identity-button"
PASTE_SECRET_FIELD = "paste-secret-field"
IMPORT_SUBMIT_BUTTON = "import-submit-button"
HANDLE_INPUT = "handle-input"
HANDLE_CONTINUE_BUTTON = "handle-entry-continue-button"

# tests/e2e-unified/ui.yaml § onboarding — invite_request (the pending-invite surface).
INVITE_SUBMIT_BUTTON = "invite-request-submit-button"
INVITE_RECHECK_BUTTON = "invite-request-recheck-button"

# Two-element nav to the Account rail sub-page of the desktop Settings shell
# (settings_shell.rs child name "account"); mirrors the mail/admin actions.
ACCOUNT_PAGE_NAV = {
    "nav": {"stack": [{"view": "settings"}, {"view": "settings", "id": "account"}]}
}


def _seed_two_accounts(nest_instance, active="user"):
    """A claimed-admin account + a freshly-registered regular-user account, seeded
    into the registry active on the regular user (``active="admin"`` flips it, which
    the admin auto-default journey needs). Returns (seed_map, user_actor_id,
    admin_actor_id). The admin is `nest_instance`'s own claimed identity (so
    `am-i-admin` is true for it); the user is registered via the admin key.

    No-modes retirement (ratified 2026-07-12): a nest is content-ready from
    first boot now (no storage-mode-unresolved launch row to route around),
    so this module's launches take the Online row straight into the
    authenticated main app with no preliminary commit needed."""
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


@pytest.mark.feature("multiple-accounts")
def test_linux_account_switcher_lists_switches_and_reveals_admin(
    nest_instance, linux_app_path, request
):
    seed, _user_actor, _admin_actor = _seed_two_accounts(nest_instance)

    driver = create_driver("linux")
    driver.launch({
        "app_path": linux_app_path,
        "url": nest_instance["url"],
        "seed_credentials": seed,
        "environment": _seeded_environment(request, nest_instance),
    })
    try:
        # (2) Launched active on the regular user → authenticated, NO admin shell.
        state = driver.wait_for_state(
            lambda s: bool(s.get("session", {}).get("authenticated")), timeout=30
        )
        assert state["session"]["authenticated"] is True, (
            f"active (regular) account must launch authenticated, got {state.get('session')!r}"
        )
        assert driver.is_absent(ADMIN_TAB), (
            "a regular (non-admin) active identity must have no admin shell"
        )

        # (3) Account settings lists BOTH accounts, the active one marked. Use the
        # re-navigating poll (a single fire-and-wait nav can be dropped by the
        # just-built main window, and render can lag under a loaded machine) — this
        # asserts count==2 with exactly one active. See `_wait_switcher_count`.
        _wait_switcher_count(driver, 2, timeout=30)

        # (4) Tap the admin row → live in-session reconnect → admin shell appears.
        driver.click(SWITCHER_ITEM, index=1)
        driver.wait_for(ADMIN_TAB, timeout=45)
        assert driver.is_visible(ADMIN_TAB), (
            "switching to the admin identity must reveal the admin shell "
            "(live reconnect, no relaunch)"
        )
    finally:
        driver.teardown()


def test_linux_switch_clears_outgoing_identity_conversations(
    nest_instance, linux_app_path, request
):
    """The conversations manager (`crate::conversations::manager()`) is a
    process-wide, identity-scoped singleton that survives the window teardown
    a switch performs (`main.rs::register_switch_account_handler`) — the same
    contract linux already honors for `critical_alerts`/`screen_lock`. Before
    this test's fix (2026-08-02, closing the linux leg of an identity-change
    teardown gap first found on apple), the production switch handler never
    called `ConversationsManager::clear_for_identity_change()`, so the
    outgoing (regular-user) identity's threads rendered straight into the
    incoming (admin) identity's conversations list.

    Seeds a thread on the regular user via the `conversations_inject_inbound`
    test seam, switches to the admin through the real switcher UI (the
    production handler this test exists to cover, not the test-agent's
    `actor_switch` patch which already wiped the manager correctly), and
    asserts the admin's thread list comes up empty."""
    seed, _user_actor, _admin_actor = _seed_two_accounts(nest_instance)

    driver = create_driver("linux")
    driver.launch({
        "app_path": linux_app_path,
        "url": nest_instance["url"],
        "seed_credentials": seed,
        "environment": _seeded_environment(request, nest_instance),
    })
    try:
        driver.wait_for_state(
            lambda s: bool(s.get("session", {}).get("authenticated")), timeout=30
        )
        conv = ConversationsActions(driver)

        # Seed a thread while active on the outgoing (regular-user) identity.
        conv.inject_inbound_for_test(
            rail="Smtp",
            sender="leaker@host.test",
            subject="outgoing-user-thread",
            body="must not survive the account switch",
        )
        wait_until(
            lambda: len(conv.list_threads()) >= 1,
            15,
            diagnose=lambda: "the seeded thread never landed on the outgoing identity",
        )

        # Switch to the admin through the real switcher UI — the production
        # handler under test, not the test-agent's actor-switch shortcut.
        _wait_switcher_count(driver, 2, timeout=30)
        driver.click(SWITCHER_ITEM, index=1)
        driver.wait_for(ADMIN_TAB, timeout=45)

        # The incoming identity's thread list must come up empty — the
        # outgoing identity's thread must not have leaked across the switch.
        threads = conv.list_threads()
        assert threads == [], (
            "the incoming identity must not inherit the outgoing identity's "
            f"conversations; got {[t.label for t in threads]!r}"
        )
    finally:
        driver.teardown()


@pytest.mark.feature("second-identity-in-its-own-window")
def test_linux_open_as_new_instance_spawns_a_bound_sibling(
    nest_instance, linux_app_path, request
):
    """The switcher's "open in new window" affordance (ui.yaml
    `account-open-new-instance-button`; `account-scoping.md` § Concurrent
    instances, the running instance's surface): clicking it on the non-active
    (admin) row spawns a SECOND `fauna-desktop` process bound to that account
    (`instance_remote::spawn_bound_instance`), while this window stays on its
    own. linux's twin of `test_account_switcher_apple.py`'s
    `test_apple_open_as_new_instance_spawns_a_bound_sibling` — same shape,
    ported.

    The spawned child is observed directly: `spawn_bound_instance` inherits
    this process's OWN environment wholesale (same XDG base, same credential
    dir — the whole point of a second instance of the same install; no
    `_shared_instance_world` needed, unlike the collision test in
    `test_launch_instance_chooser_linux.py`, since the child is a real OS
    child process, not a second driver-launched one) and only adds
    `FAUNA_BOUND_ACCOUNT` plus its own fresh `FAUNA_E2E_AGENT_PORT`, reported
    in the parent's `spawned_instances` state — so the test polls the child's
    own agent for "authenticated as the admin", exactly as apple's does. The
    child dies with the parent's process group at `driver.teardown()`
    (`terminate_tree` — no orphan)."""
    seed, user_actor, admin_actor = _seed_two_accounts(nest_instance)

    driver = create_driver("linux")
    driver.launch({
        "app_path": linux_app_path,
        "url": nest_instance["url"],
        "seed_credentials": seed,
        "environment": _seeded_environment(request, nest_instance),
    })
    try:
        driver.wait_for_state(
            lambda s: s.get("session", {}).get("actor_id") == user_actor
            and bool(s.get("session", {}).get("authenticated")),
            timeout=45,
        )
        # Row 0 = user (active), row 1 = admin. Since linux's W5.6 (account-data-plane.md § Workstreams) retirement
        # (2026-08-15) the button renders on EVERY row, active included
        # (account-scoping.md § Concurrent instances — the restriction that
        # reserved it for non-active rows only has lapsed), so `index=1`
        # targets the admin row explicitly rather than relying on the bare ID
        # being the only match.
        _wait_switcher_count(driver, 2, timeout=30)
        driver.click(OPEN_NEW_INSTANCE_BUTTON, index=1)

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
        # parent stays on the user.
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


def _seed_one_account(nest_instance):
    """A SINGLE registered regular-user account, seeded active — the pre-append
    single-identity state a first-run install lands in. Returns (seed_map,
    user_actor_id)."""
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
    with one active. The append switches to the newly-added account, which tears
    down and rebuilds the main window (dropping the Account sub-page), so
    re-navigate each cycle. The switcher reads the local ``AccountRegistry``
    (client-side), NOT the nest — the post-append live re-auth stays offline (the
    append-derived nest_url is https, Pillar C uniform-https, which the plain-http
    tier_3 nest can't serve), but the registry WRITE and the UI that reads it are
    both client-side, so this asserts the append's effect reconnect-independently.
    This is the linux equivalent of web's ``localStorage['fauna/index']`` read."""
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
        f"last SWITCHER_ITEM count={last}"
    )


def _wait_live_switcher_count(driver, n, timeout=15):
    """Poll the CURRENT (already-navigated) switcher page — with NO re-navigation —
    until it lists exactly ``n`` items. This is what asserts a *live* refresh of the
    build-once Account page: `_wait_switcher_count` re-navigates each cycle, which
    rebuilds the switcher group from the (now-shrunken) registry and would pass even
    with no live update. Removing a non-active account does not switch (no
    teardown/rebuild), so the only way the row can vanish without navigating is an
    in-place live refresh of the group."""
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
    file-backed ``CredentialStore`` writes verbatim — the linux analog of
    web's ``localStorage['fauna/index']``. Reading the store file directly asserts
    the append's effect **client-side**, independent of the post-switch reconnect.
    The file is the same per-app store the driver seeded (linux.py
    `seed_credentials` → ``{tmp}/creds/{FAUNA_KEYRING_APP}.json``). Returns the
    index dict, or None.

    ⚠ **This docstring used to explain the post-switch shell being offline as the
    append-derived https nest_url meeting a plain-HTTP tier_3 nest. That
    explanation is WRONG on linux, and believing it hid a defect.** With the dial
    override installed (step (4)) the https derivation is a non-issue — and the
    append still does not switch: the app stays authenticated as the OUTGOING
    account while this index reports the new one active. **So do not read a passing index assertion as
    "the append worked"** — `handle_wizard_done` moves `active` before the switch
    is even attempted, which is precisely why this file's assertion could not see
    the difference."""
    cred_file = os.path.join(
        driver._tmp_dir, "creds", f"fauna-e2e-agent-{driver._agent_port}.json"
    )
    try:
        with open(cred_file) as f:
            store = json.load(f)
    except (OSError, ValueError):
        return None
    idx = store.get("fauna/index")
    if idx is None:
        return None
    return json.loads(idx) if isinstance(idx, str) else idx


def _read_store_slot(driver, key):
    """One raw logical slot out of the file-backed ``CredentialStore``.

    The sibling of :func:`_read_registry_index`, which reads the ``fauna/index``
    blob: the per-actor three-slot contract (``fauna/{actor}/{secret,nest_url,
    device_id}`` — ``long-term-store.md`` § The three slots) lives in slots of its
    own, so the index alone cannot answer what URL an account will dial on its
    next launch. Returns the raw string, or None."""
    cred_file = os.path.join(
        driver._tmp_dir, "creds", f"fauna-e2e-agent-{driver._agent_port}.json"
    )
    try:
        with open(cred_file) as f:
            return json.load(f).get(key)
    except (OSError, ValueError):
        return None


def _wait_registry_index(driver, n, timeout=30, *, active=None):
    """Poll this app's registry file until it lists exactly ``n`` accounts (and,
    given ``active``, names it active); return it. Shared loop:
    ``helpers.waiting.wait_registry_index``."""
    return wait_registry_index(
        lambda: _read_registry_index(driver), n, active=active, budget_s=timeout
    )


@pytest.mark.feature("multiple-accounts")
def test_linux_switch_scopes_account_state_dirs_and_preserves_the_outgoing_account(
    nest_instance, linux_app_path, request
):
    """Per-account (actor-id-scoped) state placement (`account-scoping.md`
    § Serialized switching, the linux gap-ledger row): `mls_state.db` and the
    P2P DBs move from a flat, actor-blind
    `<config_home>/fauna/` layout to `<config_home>/fauna/<actor-id-hex>/`, so
    a switch can never leak or clobber another account's local state. Asserts
    the on-disk shape directly (headless-observable), not just the switch's
    UI outcome, which the base switcher test already covers."""
    seed, user_actor, admin_actor = _seed_two_accounts(nest_instance)

    driver = create_driver("linux")
    driver.launch({
        "app_path": linux_app_path,
        "url": nest_instance["url"],
        "seed_credentials": seed,
        "environment": _seeded_environment(request, nest_instance),
    })
    try:
        driver.wait_for_state(
            lambda s: bool(s.get("session", {}).get("authenticated")), timeout=30
        )
        # Delegates to the shared `common.scope_store.XdgScopeStore` rather than rebuilding the path — the
        # same resolver `test_sign_out.py`'s directory-erase test drives.
        fauna_dir = str(XdgScopeStore("linux", driver.config_home, "fauna").scope_roots()[0])

        def account_mls_db(actor_id):
            return os.path.join(fauna_dir, actor_id, "mls_state.db")

        # (1) Launched active on the regular user: its scoped MLS db exists;
        # the admin's does not yet (never initialized this process).
        deadline = time.monotonic() + 15
        while time.monotonic() < deadline and not os.path.exists(account_mls_db(user_actor)):
            time.sleep(0.3)
        assert os.path.exists(account_mls_db(user_actor)), (
            f"the active account's MLS state must land under its own actor-scoped "
            f"dir, not the flat {fauna_dir}"
        )
        assert not os.path.exists(account_mls_db(admin_actor)), (
            "an account that was never active this process must have no scoped dir yet"
        )
        assert not os.path.exists(os.path.join(fauna_dir, "mls_state.db")), (
            "MLS state must never land flat/actor-blind directly under fauna/"
        )

        # (2) Switch to the admin (live in-session reconnect, no relaunch). The
        # switcher list only renders on the Account settings sub-page, so
        # navigate there first (mirrors every other test in this module).
        _wait_switcher_count(driver, 2, timeout=30)
        driver.click(SWITCHER_ITEM, index=1)
        driver.wait_for(ADMIN_TAB, timeout=45)

        # (3) The admin's OWN scoped MLS db now exists, and the outgoing
        # user's is UNTOUCHED (still present — no-user-data-loss; a switch
        # must never destroy the account it switched away from).
        deadline = time.monotonic() + 15
        while time.monotonic() < deadline and not os.path.exists(account_mls_db(admin_actor)):
            time.sleep(0.3)
        assert os.path.exists(account_mls_db(admin_actor)), (
            "switching to the admin account must initialize ITS OWN scoped MLS state"
        )
        assert os.path.exists(account_mls_db(user_actor)), (
            "switching away from the regular user must not delete its scoped state"
        )
    finally:
        driver.teardown()


@pytest.mark.feature("multiple-accounts")
def test_linux_add_account_appends_second_identity_to_registry(
    nest_instance, linux_app_path, request
):
    # (0) A pre-registered SECOND identity to import in the append wizard — the
    # "import an identity I already have" paste-secret path (devices.md). Its actor
    # is registered on `nest_instance` exactly like the switcher accounts.
    admin_sk = nest_instance["admin"]["signing_key"]
    second = create_actor_and_register(nest_instance["port"], admin_signing_key=admin_sk)
    second_actor = second["actor_id_hex"]
    second_secret = bytes(second["signing_key"]).hex()

    seed, first_actor = _seed_one_account(nest_instance)

    driver = create_driver("linux")
    driver.launch({
        "app_path": linux_app_path,
        "url": nest_instance["url"],
        "seed_credentials": seed,
        "environment": _seeded_environment(request, nest_instance),
    })
    try:
        # (1) Launched active on the single seeded account → authenticated; a
        # regular (non-admin) identity has no admin shell. The Account page lists
        # exactly ONE account before Add account. Use the re-navigating poll (same
        # helper as the post-append assertion): a single fire-and-wait nav can be
        # dropped by the just-built main window if it fires before the window is
        # ready to route it, so re-navigate each cycle until the switcher renders.
        driver.wait_for_state(
            lambda s: bool(s.get("session", {}).get("authenticated")), timeout=30
        )
        assert driver.is_absent(ADMIN_TAB), (
            "a regular (non-admin) active identity must have no admin shell"
        )
        _wait_switcher_count(driver, 1, timeout=30)

        # (2) "Add account" → append-mode onboarding window. Under e2e the wizard is
        # shown without `present()`, but a newly-mapped toplevel becomes the app's
        # active_window, which the automation finder searches first (find.rs
        # `search_roots`), so its widgets are reachable with no tethering.
        # `launch_add_account_wizard` also registers THIS wizard's machine with the
        # test agent (main.rs `set_active_onboarding_machine`) so the step-4 handle-
        # check injection reaches it rather than the stale startup machine.
        driver.click(ADD_BUTTON)
        driver.wait_for(IMPORT_IDENTITY_BUTTON, timeout=30)

        # (3) Import the pre-registered second identity (the paste-secret path). The
        # wizard registers it and, on the LoggedIn outcome, appends + switches.
        driver.click(IMPORT_IDENTITY_BUTTON)
        driver.wait_for(PASTE_SECRET_FIELD, timeout=15)
        driver.clear_and_type(PASTE_SECRET_FIELD, second_secret)
        driver.click(IMPORT_SUBMIT_BUTTON)
        driver.wait_for(HANDLE_INPUT, timeout=15)

        # (4) Install the dial override BEFORE the append completes: the switch it
        # triggers is a launch **from the store**, and the URL sitting there will be
        # the wizard's uniform-https derivation, which this plain-HTTP nest cannot
        # serve. One gesture covers both halves — the machine's HTTP providers and,
        # mirrored, the process-global store-read dial (`fauna_launch_machine::dial`),
        # which is the leg linux takes at `main.rs`'s store-read launch. Proven on
        # tui first (2026-08-14): without this the switch lands `Offline(transient)`
        # and step (6) fails with `session={'authenticated': False}, nav=[welcome]`.
        #
        # No teardown clear is needed: this driver owns its own app process and kills
        # it below, and the override is in-process state that dies with it.
        driver.set_provider_base_urls({"nest": nest_instance["url"]})

        # (5) Inject the AlreadyOnNest (welcome-back) handle-check outcome so Continue
        # lands `WizardOutcome::LoggedIn` — the append divergence's trigger
        # (`registry.add_account` + switch). The real silent-challenge → AlreadyOnNest
        # path is covered by the onboarding suites; here we drive the NOVEL linux
        # append branch that fires on LoggedIn.
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
        # account, with the original account preserved (no-user-data-loss). Assert
        # via the persisted AccountIndex (client-side, reconnect-independent — the
        # linux analog of web's localStorage index read).
        index = _wait_registry_index(driver, 2, timeout=30, active=second_actor)
        actor_ids = [a["actor_id"] for a in index["accounts"]]
        assert first_actor in actor_ids, (
            "the original account must survive the append (no-user-data-loss)"
        )
        assert second_actor in actor_ids, "the imported account must be appended"
        assert index["active"] == second_actor, (
            "the append switches to the newly-added account (design Decision 1)"
        )

        # (7) The append landed a WORKING session as the new identity — not just a
        # registry row saying so. This is the assertion the registry-only check in
        # (6) structurally cannot make: `handle_wizard_done` runs `add_account` +
        # `set_active` BEFORE the append branch triggers the switch, so the index
        # reaches its final shape whether or not the switch ever happens.
        #
        # ⚠ STANDING RED on a real linux defect, landed
        # red deliberately on 2026-08-16 rather than left commented out — a red
        # assertion is a tracked defect, a commented-out one is invisible. Both
        # assertions are GREEN on tui (`test_account_switcher_tui.py`, steps (7)
        # and (8)), so the shape is not in question, only linux's path.
        #
        # The cause is MEASURED — do not re-derive it, and do not go looking at
        # the switch glue, which is where row 23 originally pointed. The append
        # never reaches the switch at all: `handle_wizard_done`'s append branch
        # (`views/onboarding/mod.rs`) re-reads the identity triple via
        # `client::load_credentials()` and calls `registry.add_account`, which
        # fails with `invalid secret: expected 32 bytes (64 hex chars), got 0` —
        # an EMPTY secret — then takes the `Err` arm that dismisses the wizard,
        # repeatedly (thousands of identical lines in ~50 ms; an 18 MB app.err).
        # So the live session correctly still belongs to the outgoing account:
        # nothing ever asked it to change. Two threads to pull: why
        # `load_credentials()` yields `Some` with an empty secret instead of
        # `None`, and why the append branch re-enters in a loop.
        await_session_actor(driver, second_actor, budget_s=APP_RELAUNCH_S,
                            what="the append")
        stored_url = _read_store_slot(driver, f"fauna/{second_actor}/nest_url")
        assert stored_url == f"https://localhost:{nest_instance['port']}"
    finally:
        driver.teardown()


@pytest.mark.feature("multiple-accounts")
def test_linux_add_account_pending_invite_submit_switches_the_live_session(
    nest_instance, linux_app_path, request
):
    """A pending-invite submit during "Add account" moves the registry's active
    pointer AND the running session together — never the registry alone.

    The linux twin of
    ``test_account_switcher_windows.py::test_windows_add_account_pending_invite_submit_switches_the_live_session``
    and its apple sibling (same journey, same assertions — priority #1).
    ``onboarding.md`` § Multi-account: the append wizard's pending-invite state is
    not an exit, so "the append glue adopts on the submit return — register the
    append identity in the account registry, write its per-actor pending-invite
    slot, switch to it", and the wizard at ``invite_request`` is then simply the
    newly-active account's launch surface (what a relaunch would show).

    Before this fix linux ran the shared ``persist_pending_invite`` writer and
    stopped: its switch handler could only rebuild an AUTHENTICATED session
    (``launch_authenticated`` against the target's ``nest_url``, which a pending
    account does not have). The rebuild now routes through the same launch
    classification ``build_ui`` runs at a cold start.

    Driven through the app's own UI (convention 8) up to the one hop a test nest
    forces — ``navigate_to_invite_request_for_known_nest``, because a test nest is
    not DNS-discoverable (the same hop ``test_pending_invite_journey.py`` makes).
    Every wait is a deadline poll on state the app reaches (convention 14)."""
    seed, first_actor = _seed_one_account(nest_instance)

    joiner_key = SigningKey.generate()
    joiner_secret = bytes(joiner_key).hex()
    joiner_actor = bytes(joiner_key.verify_key).hex()
    handle = f"addpending-{int(time.time() * 1000)}"

    driver = create_driver("linux")
    driver.launch({
        "app_path": linux_app_path,
        "url": nest_instance["url"],
        "seed_credentials": seed,
        "environment": _seeded_environment(request, nest_instance),
    })
    try:
        # (1) One seeded account, authenticated; the generation read here is the
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

        # (2) "Add account" → append-mode wizard → import the joiner's secret (the
        # paste-secret path) → the handle page.
        driver.click(ADD_BUTTON)
        driver.wait_for(IMPORT_IDENTITY_BUTTON, timeout=45)
        driver.click(IMPORT_IDENTITY_BUTTON)
        driver.wait_for(PASTE_SECRET_FIELD, timeout=20)
        driver.clear_and_type(PASTE_SECRET_FIELD, joiner_secret)
        driver.click(IMPORT_SUBMIT_BUTTON)
        driver.wait_for(HANDLE_INPUT, timeout=20)

        # (3) Hop to `invite_request` for this claimed nest, then the REAL submit.
        # `launch_add_account_wizard` pointed the agent's machine cell at THIS
        # wizard, so the hop drives the append wizard, not the startup machine.
        driver.call_machine_method(
            "navigate_to_invite_request_for_known_nest",
            json.dumps([nest_instance["url"], handle]),
        )
        driver.wait_for(INVITE_SUBMIT_BUTTON, timeout=30)
        driver.click(INVITE_SUBMIT_BUTTON)

        # (4) The registry half — the shared writer's own contract: the joiner is
        # registered and active, the original survives (no-user-data-loss), and
        # the per-actor pending-invite slot is written.
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
        # registry. `register_switch_account_handler` counts its teardown at its
        # top (`record_session_teardown`), so a generation that never moves means
        # the registry changed under a session that never noticed.
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
        assert session_generation(driver) == generation_before + 1, (
            f"the adoption is ONE switch — exactly one counted teardown: {_diagnose()}"
        )
        state = driver.wait_for_state(
            lambda s: not s.get("session", {}).get("authenticated"),
            timeout=30,
        )
        assert state.get("session", {}).get("actor_id") != first_actor, (
            "the running session still names the outgoing identity while the "
            f"registry's active account is the joiner: {_diagnose()}"
        )

        # (6) …and what renders is the joiner's OWN launch surface: the invite page
        # in `PendingReview`, hydrated from the slot written in (4) — the recheck
        # affordance exists only in that state.
        driver.wait_for(INVITE_RECHECK_BUTTON, timeout=45)
    finally:
        driver.teardown()


@pytest.mark.feature("multiple-accounts")
def test_linux_remove_account_shrinks_switcher_live(nest_instance, linux_app_path, request):
    """Removing a non-active account LIVE-refreshes the build-once Account page: the
    switcher row disappears in place, with NO page rebuild (the slice-2 live-refresh
    follow-up in `settings/account.rs`). A `remove` without a switch never tears down
    and rebuilds the main window (unlike a switch), so the row can only vanish via an
    in-place refresh of the switcher group — asserted on the current page with no
    re-navigation (a re-navigating poll would rebuild from the shrunken registry and
    pass even without the live update)."""
    seed, _user_actor, admin_actor = _seed_two_accounts(nest_instance)

    driver = create_driver("linux")
    driver.launch({
        "app_path": linux_app_path,
        "url": nest_instance["url"],
        "seed_credentials": seed,
        "environment": _seeded_environment(request, nest_instance),
    })
    try:
        driver.wait_for_state(
            lambda s: bool(s.get("session", {}).get("authenticated")), timeout=30
        )
        # Both accounts listed, active on the regular user (row 0); the admin (row 1)
        # is the non-active row, so it carries the (only) remove button. Leaves the
        # Account page navigated-to and rendered.
        _wait_switcher_count(driver, 2, timeout=30)

        # Remove the non-active admin account. Remove is offered ONLY for non-active
        # accounts, so there is exactly one account-remove-button on the page.
        driver.click(REMOVE_BUTTON)

        # LIVE: the removed row vanishes from the SAME page (no re-navigation, no
        # rebuild), and the surviving (regular) account stays active.
        _wait_live_switcher_count(driver, 1, timeout=15)
        assert driver.count(ACTIVE_INDICATOR) == 1, (
            "the surviving (regular) account stays active after the removal"
        )

        # The store-level remove also landed: the file-backed registry dropped 2->1,
        # dropping the ADMIN (non-active) account, never the active user.
        index = _wait_registry_index(driver, 1, timeout=30)
        actor_ids = [a["actor_id"] for a in index["accounts"]]
        assert admin_actor not in actor_ids, (
            "the removed (admin) account must be gone from the file-backed registry"
        )
    finally:
        driver.teardown()


# ---------------------------------------------------------------------------
# Abandoned-append DURABLE recovery, across a full-app relaunch (real keyring)
# ---------------------------------------------------------------------------
# The linux counterpart of web's
# `test_web_add_account_abandon_recovers_prior_identity_on_current_load`. Web's
# append wizard runs IN the authenticated SPA, so an abandoned import corrupts the
# *current load's* in-memory identity and web must re-read the healed slot without
# a reload. Linux's "Add account" wizard is a SEPARATE window, so the running
# session's in-memory identity is never touched — and since 2026-09-24 there is
# no durable heal to run either: an append-mode confirm writes nothing
# (`fauna_client_accounts::persist_confirmed_identity`'s append rule), so a
# force-quit mid-append leaves the registry exactly as it was and the relaunch
# routes on its active account.
#
# The append rule is pinned by the shared unit test
# (`an_append_confirm_writes_nothing_and_the_launch_still_routes_on_the_active_account`);
# this e2e is the confirmatory full-app version, driving the append mutation through
# the real UI (as a real user would, not an API shortcut) with real-libsecret persistence across a
# genuine process relaunch (the default file-backed driver mode is port-namespaced,
# so it can't survive a relaunch — hence `use_real_keyring`).
#
# `docs/goal/architecture/long-term-store.md` § Multi-account evolution
# (abandoned-append recovery) + § Implementation status today.


@pytest.fixture
def real_keyring_store(tmp_path):
    """A `LibsecretCredStore` for one durable-relaunch case: a unique, swept
    namespace + a stable XDG base on a real Secret Service the store runs
    PRIVATELY for this test (`drivers/secret_service.py`) — the only place
    persisted slots survive a force-quit + relaunch, and never the developer's
    desktop keyring, whose daemon this test's force-quit used to crash
    machine-wide. Skips only where the daemon is not installed. Swept before
    AND after so a crashed prior run can't leak in and this run can't leak out;
    `close()` stops the daemon."""
    if not secret_service_available():
        skip_environment(
            "linux's real-keyring launches run a private gnome-keyring-daemon, which this box cannot supply"
        )
    store = LibsecretCredStore(unique_namespace(), str(tmp_path / "xdg"))
    store.clear()
    try:
        yield store
    finally:
        store.clear()
        store.close()


def _real_keyring_config(linux_app_path, node_url, store, request, nest_instance):
    """Driver launch config for the `use_real_keyring` mode: a STABLE keyring
    namespace + STABLE XDG dirs and the store's private Secret Service on the
    launch's bus (no file store), so every persisted slot is read back across
    a relaunch (drivers/linux.py)."""
    config = store.launch_config(linux_app_path, node_url)
    config["environment"] = {
        **config["environment"],
        **_seeded_environment(request, nest_instance),
    }
    return config


@pytest.mark.feature("multiple-accounts")
def test_linux_add_account_abandon_recovers_prior_identity_on_relaunch(
    nest_instance, linux_app_path, real_keyring_store, request
):
    """Abandoning an append-mode "Add account" mid-wizard, then force-quitting and
    relaunching, must come back to the prior (active-registry) identity — and
    the recovery is STRUCTURAL, not a heal: an append-mode confirm writes
    nothing to the store (`fauna_client_accounts::persist_confirmed_identity`'s
    append rule, the shape tui always had), so the imported identity lives only
    in the wizard machine until `LoggedIn` → `add_account`. Leaving before
    Continue therefore leaves the registry untouched (one account, active on #1)
    and no slot anywhere pointing at the abandoned identity; the relaunch routes
    on the registry's active account. Until 2026-09-24 linux's append confirm
    overwrote the pre-registry single slot and a boot re-mirror had to heal it —
    this test used to pin that heal and now pins that there is nothing to heal.

    Uses real-libsecret persistence (`use_real_keyring`) so the state survives a
    genuine process relaunch; the append mutation is driven through the switcher UI
    (as a real user would, not an API shortcut). Account #1 is the nest's own claimed ADMIN, so
    `admin-tab` presence is a clean "routed to the active #1" discriminator: if
    routing followed the abandoned #2 (a regular user), the authenticated shell
    would have NO admin-tab.
    """
    store = real_keyring_store

    # (0) Account #1 = the nest's own claimed ADMIN (so `am-i-admin` → admin-tab).
    # A regular SECOND identity is registered to import-then-ABANDON (matches web's
    # abandon test, which registers its second identity too).
    admin_sk = nest_instance["admin"]["signing_key"]
    admin_actor = bytes(admin_sk.verify_key).hex()
    admin_secret = bytes(admin_sk).hex()
    second = create_actor_and_register(nest_instance["port"], admin_signing_key=admin_sk)
    second_secret = bytes(second["signing_key"]).hex()

    # Seed #1 in the registry shape (a first-run install's shape: one account,
    # active on the admin).
    store.inject_identity(secret_hex=admin_secret, node_url=nest_instance["url"])
    second_actor = bytes(second["signing_key"].verify_key).hex()

    config = _real_keyring_config(
        linux_app_path, nest_instance["url"], store, request, nest_instance
    )
    driver = create_driver("linux")
    driver.launch(config)
    try:
        # (1) LAUNCH #1: authenticate as the admin over the seeded registry.
        # admin-tab confirms the active identity is the admin.
        driver.wait_for_state(
            lambda s: bool(s.get("session", {}).get("authenticated")), timeout=30
        )
        driver.wait_for(ADMIN_TAB, timeout=45)
        assert driver.is_visible(ADMIN_TAB), (
            "launch #1 must authenticate as the admin identity (admin-tab present)"
        )
        assert store.read_field(INDEX_ACCOUNT) is not None, (
            "launch #1 must have kept the seeded fauna/index"
        )
        _wait_switcher_count(driver, 1, timeout=30)

        # (2) APPEND via the UI: Add account → import the second identity. The
        # wizard is a second window whose newly-mapped toplevel the automation
        # finder searches first, and `launch_add_account_wizard` re-points the test
        # agent's machine at it (main.rs `set_active_onboarding_machine`).
        # Reaching HANDLE_INPUT proves the import was confirmed. NO Continue →
        # the append is never committed.
        driver.click(ADD_BUTTON)
        driver.wait_for(IMPORT_IDENTITY_BUTTON, timeout=30)
        driver.click(IMPORT_IDENTITY_BUTTON)
        driver.wait_for(PASTE_SECRET_FIELD, timeout=15)
        driver.clear_and_type(PASTE_SECRET_FIELD, second_secret)
        driver.click(IMPORT_SUBMIT_BUTTON)
        driver.wait_for(HANDLE_INPUT, timeout=15)

        # An append-mode confirm writes NOTHING: no per-actor slot for #2, no
        # single-slot residue, and the registry is unchanged (one account, still
        # active on the admin — no `add_account` until LoggedIn).
        assert store.read_field(f"fauna/{second_actor}/secret") is None, (
            "an append-mode confirm must not register the imported #2"
        )
        assert store.read_field(ACCT_SECRET_KEY) is None, (
            "nothing on linux writes the pre-registry single slot any more"
        )
        idx = json.loads(store.read_field(INDEX_ACCOUNT))
        assert len(idx["accounts"]) == 1 and idx["active"] == admin_actor, (
            "abandoning before Continue must NOT append to the registry"
        )

        # (3) ABANDON: force-quit mid-wizard (the driver kills the process group,
        # no Continue) + relaunch with the SAME real-keyring namespace, so every
        # persisted slot is read back from the real Secret Service — the faithful
        # "closed the app mid-append, reopened it" durable-recovery path.
        driver.teardown()
        assert store.service.daemon_exits == [], (
            "the force-quit killed the private Secret Service daemon "
            f"(exits={store.service.daemon_exits}) — the relaunch below would read "
            "a restarted one, and on a desktop keyring this is the crash that "
            "locked the login keyring machine-wide"
        )
        driver.launch(config)

        # (4) Launch routing reads the registry's active account — the admin —
        # so the relaunch authenticates as the admin, NOT the abandoned #2:
        driver.wait_for_state(
            lambda s: bool(s.get("session", {}).get("authenticated")), timeout=30
        )
        driver.wait_for(ADMIN_TAB, timeout=45)
        assert driver.is_visible(ADMIN_TAB), (
            "the relaunch must route to the ACTIVE admin (admin-tab), not the "
            "abandoned #2 — the append wrote nothing the routing could read"
        )
        assert store.read_field(f"fauna/{second_actor}/secret") is None, (
            "the abandoned #2 must not have been materialized by the relaunch"
        )
        idx2 = json.loads(store.read_field(INDEX_ACCOUNT))
        assert len(idx2["accounts"]) == 1 and idx2["active"] == admin_actor, (
            "an abandoned append leaves the registry unchanged (one account, admin active)"
        )
    finally:
        driver.teardown()


def _remap_account_page(driver):
    """Leave the Account sub-page and come back, so its on-visible refresh fires.

    The page is built ONCE (`views::settings_shell`), so its Stage-2 toggles only
    re-read the registry when the page *becomes* the visible child. The admin
    auto-default writes the flag after the shell is built, so this is exactly what a
    user does to see it: open Settings → Account once the observation has landed.
    Re-navigating to the page you are already on is a no-op (the stack's
    visible-child name never changes), hence the detour via Status."""
    driver.set_state(
        {"nav": {"stack": [{"view": "settings"}, {"view": "settings", "id": "status"}]}}
    )
    time.sleep(0.3)
    driver.set_state(ACCOUNT_PAGE_NAV)
    driver.wait_for(SWITCHER_LIST, timeout=10)


def _wait_flag(driver, actor, want, key="require_confirm_to_activate", timeout=20):
    """Poll the persisted index until ``actor``'s ``key`` reads ``want``; return the
    final per-actor map for the failure message."""
    deadline = time.monotonic() + timeout
    flags = None
    while time.monotonic() < deadline:
        index = _read_registry_index(driver) or {}
        flags = {a["actor_id"]: a.get(key, False) for a in index.get("accounts", [])}
        if flags.get(actor) is want:
            return flags
        time.sleep(0.3)
    raise AssertionError(
        f"{key} for {actor} never became {want} within {timeout}s; last {flags!r}"
    )


@pytest.mark.feature("multiple-accounts")
def test_linux_require_confirm_gates_switch_decline_then_approve(
    nest_instance, linux_app_path, request
):
    """Stage 2 (`long-term-store.md` § Multi-account evolution, ratified 2026-07-16):
    flagging an account via its `account-require-confirm-toggle` makes activating it
    demand a re-auth confirmation first. Linux has no native OS prompt, so it renders
    the in-app `account-activate-reauth-prompt` (the shape linux ratifies; web + tui
    adopt it). Declining is a PURE NO-OP — registry untouched, no teardown, the
    current account stays active — and approving completes the same switch journey
    the unflagged path takes. Both the flag write and the confirm are driven through
    the UI (testing.md point 8) and asserted against the persisted registry index
    (headless observable)."""
    seed, user_actor, admin_actor = _seed_two_accounts(nest_instance)

    driver = create_driver("linux")
    driver.launch({
        "app_path": linux_app_path,
        "url": nest_instance["url"],
        "seed_credentials": seed,
        "environment": _seeded_environment(request, nest_instance),
    })
    try:
        driver.wait_for_state(
            lambda s: bool(s.get("session", {}).get("authenticated")), timeout=30
        )
        _wait_switcher_count(driver, 2, timeout=30)

        # (1) Flag the ADMIN row (row 1) through its own toggle — the UI write path.
        # Setting the flag itself never prompts; only activation does.
        driver.click(REQUIRE_CONFIRM_TOGGLE, scope=f"{SWITCHER_ITEM}[1]")
        flagged = _wait_flag(driver, admin_actor, True, timeout=10)
        assert flagged.get(user_actor) is False, (
            f"the row toggle must flag only the tapped account; got {flagged!r}"
        )

        # (2) DECLINE: activating the flagged row raises the in-app prompt, and
        # cancelling it must be a PURE no-op — no registry mutation, no teardown,
        # the regular user stays authenticated and the switcher page stays rendered
        # (a teardown would blank it).
        driver.click(SWITCHER_ITEM, scope=f"{SWITCHER_ITEM}[1]")
        driver.wait_for(REAUTH_PROMPT, timeout=15)
        assert driver.is_visible(REAUTH_PROMPT), (
            "activating a flagged account must raise the in-app re-auth prompt "
            "(linux has no native OS prompt to defer to)"
        )
        # Convention 14: the absence is anchored to causal order, not to a
        # window. Declining dismisses the dialog (the handler's own completion
        # observable), then the barrier drains the glib queue — linux counts a
        # switch synchronously in its handler, *before* the 100 ms teardown
        # deferral, precisely so this check can see one.
        assert_no_relaunch(
            driver,
            lambda: driver.click(REAUTH_CANCEL_BUTTON),
            lambda: not driver.is_visible(REAUTH_PROMPT),
            what="declining the re-auth prompt",
        )
        state = driver.get_state()
        assert state["session"]["actor_id"] == user_actor, (
            "a DECLINED re-auth must leave the original account active (pure no-op); "
            f"got {state.get('session')!r}"
        )
        assert state["session"]["authenticated"] is True, (
            "a declined re-auth must not tear the live session down"
        )
        index = _read_registry_index(driver)
        assert index["active"] == user_actor, (
            "a declined re-auth must leave the persisted registry untouched"
        )
        assert driver.is_absent(ADMIN_TAB), (
            "a declined re-auth must not reveal the admin shell"
        )

        # (3) APPROVE: the same tap, confirmed, completes the switch (live in-session
        # reconnect, exactly the unflagged journey).
        _wait_live_switcher_count(driver, 2, timeout=15)
        driver.click(SWITCHER_ITEM, scope=f"{SWITCHER_ITEM}[1]")
        driver.wait_for(REAUTH_PROMPT, timeout=15)
        driver.click(REAUTH_CONFIRM_BUTTON)
        driver.wait_for(ADMIN_TAB, timeout=45)
        assert driver.is_visible(ADMIN_TAB), (
            "an APPROVED re-auth must complete the switch to the flagged account "
            "(set_active_confirmed — the post-re-auth path)"
        )
        index = _read_registry_index(driver)
        assert index["active"] == admin_actor, (
            f"an approved re-auth must persist the activation; got {index!r}"
        )
    finally:
        driver.teardown()


@pytest.mark.feature("multiple-accounts")
def test_linux_admin_auto_default_flags_admin_and_explicit_off_sticks(
    nest_instance, linux_app_path, request
):
    """The admin auto-default (`long-term-store.md` § Multi-account evolution —
    "Default off; a client turns it on for its admin identity"): launching
    authenticated as the ADMIN account auto-enables its require-confirm flag at
    linux's am-i-admin observation (the nav gate that reveals the Admin sidebar row),
    with NO user tap — asserted against the persisted index; the regular account
    stays unflagged. And the user's explicit OFF sticks: turn the toggle off,
    re-trigger the observation by switching away and back (both switches unflagged →
    no confirm involved), and the auto-default must NOT re-flip it —
    `require_confirm_user_set` pins the user's choice."""
    seed, user_actor, admin_actor = _seed_two_accounts(nest_instance, active="admin")

    driver = create_driver("linux")
    driver.launch({
        "app_path": linux_app_path,
        "url": nest_instance["url"],
        "seed_credentials": seed,
        "environment": _seeded_environment(request, nest_instance),
    })
    try:
        driver.wait_for_state(
            lambda s: s.get("session", {}).get("actor_id") == admin_actor, timeout=30
        )
        _wait_switcher_count(driver, 2, timeout=30)

        # (1) The am-i-admin observation auto-enabled the ADMIN row's flag — no tap.
        flags = _wait_flag(driver, admin_actor, True, timeout=30)
        assert flags.get(user_actor) is False, (
            f"the auto-default must only flag the admin identity; got {flags!r}"
        )

        # (2) The auto-defaulted flag must also RENDER: bring the build-once page
        # back into view (as a user would, once the observation has landed) and the
        # admin's toggle must read ON. Without the on-visible refresh it renders its
        # stale build-time OFF over a registry that says ON — and then the user can
        # never turn the flag off, because their tap on an OFF-looking switch writes
        # ON. That is a real bug this arm pins, not a harness detail.
        _remap_account_page(driver)
        assert (
            driver.get_attr(REQUIRE_CONFIRM_TOGGLE, "state", scope=f"{SWITCHER_ITEM}[1]")
            == "on"
        ), (
            "the admin row's toggle must render the auto-defaulted flag once the page "
            "is shown; a stale OFF makes the flag impossible to turn off"
        )

        # (3) Explicit user OFF (the admin is the ACTIVE row 1 — its toggle is
        # tappable even on the active row, which is exactly why the toggle cannot
        # live in the non-active branch that hosts account-remove-button).
        driver.click(REQUIRE_CONFIRM_TOGGLE, scope=f"{SWITCHER_ITEM}[1]")
        _wait_flag(driver, admin_actor, False, timeout=10)
        index = _read_registry_index(driver)
        admin_entry = next(a for a in index["accounts"] if a["actor_id"] == admin_actor)
        assert admin_entry.get("require_confirm_user_set") is True, (
            "the toggle write must mark the flag user-set (the OFF-sticks pin); "
            f"got {admin_entry!r}"
        )

        # (4) Re-trigger the observation: switch to the user (admin is unflagged now,
        # the user always was — no confirm anywhere), then back to the admin. The
        # rebuilt session re-fires check_admin_status() on every switch.
        driver.click(SWITCHER_ITEM, scope=f"{SWITCHER_ITEM}[0]")
        driver.wait_for_state(
            lambda s: s.get("session", {}).get("actor_id") == user_actor, timeout=45
        )
        _wait_switcher_count(driver, 2, timeout=30)
        driver.click(SWITCHER_ITEM, scope=f"{SWITCHER_ITEM}[1]")
        driver.wait_for_state(
            lambda s: s.get("session", {}).get("actor_id") == admin_actor, timeout=45
        )
        # Give the refused auto-default a bounded window, then pin the user's OFF.
        time.sleep(4.0)
        index = _read_registry_index(driver)
        admin_entry = next(a for a in index["accounts"] if a["actor_id"] == admin_actor)
        assert admin_entry.get("require_confirm_to_activate") is False, (
            "an explicit user OFF must stick against the admin auto-default across "
            f"a fresh am-i-admin observation; got {admin_entry!r}"
        )
    finally:
        driver.teardown()



# ---------------------------------------------------------------------------
# The tui-first witnesses, lifted to linux: remove erases only that
# identity's data (6), an abandoned first-run identity never ghosts (8), an
# unlaunchable switch is refused and said (9), device settings stay while a
# draft follows its identity (10). tui twins: `test_account_switcher_tui.py`.
# ---------------------------------------------------------------------------

AUTOSTART_TOGGLE = "settings-autostart-toggle"
GENERAL_PAGE_NAV = {
    "nav": {"stack": [{"view": "settings"}, {"view": "settings", "id": "general"}]}
}
COMPOSE_FIELD = "compose-text-field"
CREATE_IDENTITY_BUTTON = "create-identity-button"
IDENTITY_CONTINUE_BUTTON = "identity-continue-button"
IDENTITY_CREATED_BACK_BUTTON = "identity-created-back-button"
RECOVERY_KIT_SKIP_BUTTON = "recovery-kit-skip-button"
HANDLE_ENTRY_BACK_BUTTON = "handle-entry-back-button"


def _await_working_session(driver, actor_id, what):
    """Authenticated as ``actor_id`` AND its conversations rail is live
    (``data.conv_real_backend_active``) — the incoming account's own MLS engine
    opened, which a session still holding the outgoing account's engine never
    reaches. tui's helper of the same name."""

    def _served() -> bool:
        s = driver.get_state() or {}
        return (
            s.get("session", {}).get("actor_id") == actor_id
            and s.get("data", {}).get("conv_real_backend_active") is True
        )

    wait_until(
        _served,
        APP_RELAUNCH_S,
        diagnose=lambda: (
            f"after {what}: session={(driver.get_state() or {}).get('session')!r} "
            "conv_real_backend_active="
            f"{(driver.get_state() or {}).get('data', {}).get('conv_real_backend_active')!r}"
        ),
    )


def _wait_for_path(path, what):
    wait_until(
        lambda: os.path.exists(path),
        UI_SETTLE_S,
        diagnose=lambda: f"{what}: {path} never appeared",
    )


@pytest.mark.feature("multiple-accounts")
def test_linux_removing_an_identity_deletes_its_data_and_leaves_the_others(
    nest_instance, linux_app_path, request
):
    """Removing an identity deletes THAT identity's data on this device and
    leaves every other identity's data untouched (`account-scoping.md` § The
    scoping taxonomy; linux's erase door is `account_scope::remove_account`).

    `test_linux_remove_account_shrinks_switcher_live` proves the row and the
    registry entry go; neither says anything about the account's on-device
    state — the half a user cannot see and cannot get back. Both identities
    first run a real session each (the admin's through a switch), so each owns
    a populated scope; the removal must then leave no admin scope under ANY
    base the erase sweeps (`common.scope_store.XdgScopeStore`) while the user's
    `mls_state.db` and secret are still there. Every wait is on state."""
    seed, user_actor, admin_actor = _seed_two_accounts(nest_instance)

    driver = create_driver("linux")
    driver.launch({
        "app_path": linux_app_path,
        "url": nest_instance["url"],
        "seed_credentials": seed,
        "environment": _seeded_environment(request, nest_instance),
    })
    try:
        _await_working_session(driver, user_actor, "the launch as the user")
        store = XdgScopeStore("linux", driver.config_home, "fauna")
        user_db = store.scope_dirs(user_actor)[0] / "mls_state.db"
        _wait_for_path(user_db, "the user's own MLS state")

        # Give the admin a real session of its own, so it has data to lose.
        _wait_switcher_count(driver, 2, timeout=30)
        driver.click(SWITCHER_ITEM, index=1)
        _await_working_session(driver, admin_actor, "the switch to the admin")
        admin_dirs = store.scope_dirs(admin_actor)
        _wait_for_path(admin_dirs[0] / "mls_state.db", "the admin's own MLS state")

        # Back to the user: the admin row is now the non-active one, the only
        # row the switcher offers a remove button on.
        _wait_switcher_count(driver, 2, timeout=30)
        driver.click(SWITCHER_ITEM, index=0)
        _await_working_session(driver, user_actor, "the switch back to the user")

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
                f"{[str(d) for d in admin_dirs if d.exists()]}; error-message: "
                f"{driver.get_text('error-message') if driver.is_visible('error-message') else ''!r}"
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


@pytest.mark.feature("multiple-accounts")
def test_linux_switching_to_an_identity_the_device_cannot_sign_in_as_is_refused(
    nest_instance, linux_app_path, request
):
    """Switching to an identity this device can no longer sign in as is
    refused, says so, and leaves the user on the identity they were using
    (`long-term-store.md` § Multi-account evolution, "Activating refuses an
    account it cannot launch as").

    The seed leaves the admin listed with no `fauna/<actor>/secret` slot — the
    shape a keystore write that silently never landed produces. The switch
    handler refuses before any teardown, and the click paints the SHARED line
    (`fauna_client_accounts::switch_refused_copy`) on the Account page's
    `error-message`; no relaunch happens, and the live session and the
    persisted active pointer are both still the user's. The absence of a
    relaunch is anchored causally (`assert_no_relaunch`): the refusal line is
    the handler's own completion observable."""
    seed, user_actor, admin_actor = _seed_two_accounts(nest_instance)
    seed.pop(f"fauna/{admin_actor}/secret")

    driver = create_driver("linux")
    driver.launch({
        "app_path": linux_app_path,
        "url": nest_instance["url"],
        "seed_credentials": seed,
        "environment": _seeded_environment(request, nest_instance),
    })
    try:
        _await_working_session(driver, user_actor, "the launch as the user")
        _wait_switcher_count(driver, 2, timeout=30)

        label = driver.get_text(ITEM_HANDLE, index=1)
        expected = S.settings.switch_refused_no_secret(account=label)
        assert_no_relaunch(
            driver,
            lambda: driver.click(SWITCHER_ITEM, index=1),
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
def test_linux_switching_identity_keeps_device_settings_and_moves_drafts(
    nest_instance, linux_app_path, request
):
    """Settings that describe this device stay as they are across a switch,
    while a draft — a choice tied to an identity — follows its identity
    (`account-scoping.md` § Serialized switching and § The scoping taxonomy,
    class 2 vs class 1).

    linux's device setting is start-at-login (`settings-autostart-toggle`),
    persisted install-wide by `crate::autostart` and named by no actor scope.
    The identity-tied half is the feed composer's draft, which rests in that
    identity's own sealed `__drafts` plane on the nest. The arc: as the user,
    turn start-at-login off and leave a draft (confirmed on the nest before
    switching, so the switch tests scoping, not the debounce); switch to the
    admin → still off, and the composer does NOT hold the user's draft; switch
    back → the user's draft is there again and start-at-login still off. The
    drafts read is the sanctioned side-channel verification (convention 8's
    carve-out); the mutations are all UI. tui's twin keeps its external-media
    choice instead — each app's own class-2 setting."""
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

    driver = create_driver("linux")
    driver.launch({
        "app_path": linux_app_path,
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
        _wait_switcher_count(driver, 2, timeout=30)
        driver.click(SWITCHER_ITEM, index=1)
        _await_working_session(driver, admin_actor, "the switch to the admin")
        assert start_at_login() == "false", (
            "a setting that describes this device must survive the switch; "
            f"start-at-login reads {driver.get_attr(AUTOSTART_TOGGLE, 'checked')!r}"
        )
        assert composer_body() != draft, (
            "the user's draft must not follow the device to another identity"
        )

        # (3) Switch back: the user's draft is theirs again, the device choice
        # is still the one made.
        _wait_switcher_count(driver, 2, timeout=30)
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


@pytest.mark.feature("multiple-accounts")
def test_linux_abandoned_created_identity_never_shows_among_your_identities(
    nest_instance, linux_app_path, request
):
    """A fresh install creates identity A, walks back out of it, and onboards a
    DIFFERENT identity B instead: the switcher lists exactly one identity — B —
    and A never shows up among your identities (`long-term-store.md`
    § Multi-account evolution; the retirement is shared Rust,
    `AccountRegistry::retire_superseded_provisionals`, run by B's commit).

    Moment 1 registers AND activates A at `identity-continue-button`, before
    any nest has heard of it, and nothing on the Back path retracts that row —
    so this is the case that can ghost. B arrives by import, never a second
    create (create → Back → create re-offers the same generated secret, one
    identity, not two). tui's twin:
    `test_tui_abandoned_created_identity_never_shows_among_your_identities`;
    windows': `test_windows_abandoned_create_identity_does_not_ghost_the_switcher`."""
    admin_sk = nest_instance["admin"]["signing_key"]
    second = create_actor_and_register(nest_instance["port"], admin_signing_key=admin_sk)
    second_actor = second["actor_id_hex"]
    second_secret = bytes(second["signing_key"]).hex()

    driver = create_driver("linux")
    driver.launch({
        "app_path": linux_app_path,
        "url": nest_instance["url"],
        "environment": _seeded_environment(request, nest_instance),
    })
    try:
        # (1) Fresh install → create A → Continue: moment 1 writes A's row.
        driver.wait_for(CREATE_IDENTITY_BUTTON, timeout=45)
        driver.click(CREATE_IDENTITY_BUTTON)
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
