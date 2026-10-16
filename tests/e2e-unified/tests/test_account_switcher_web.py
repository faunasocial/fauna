"""tier_3 e2e: web multi-account account switcher (Stage 1 web).

Web twin of ``test_account_switcher_linux.py``. Multi-account clients let one
install (one browser profile) hold several Fauna identities and switch between
them (``docs/goal/architecture/long-term-store.md`` § Multi-account evolution;
switch-first design, Decision 1 — tracked internally). This drives the web
reference:

1. Seed TWO registered identities into localStorage as a full ``AccountRegistry``
   state (a claimed admin + a regular user), active on the regular user.
   ``build_registry_seed`` writes the exact logical-key layout the wasm
   ``LocalStorageSecretStore`` reads verbatim (the registry is the only identity
   source; nothing is mirrored). Web has no
   ``seed_credentials`` driver hook, so we write the keys via ``eval_js`` (the web
   escape hatch), then authenticate the SPA as the active account via the proven
   ``set_state`` session patch (in-memory auth, no silent-challenge race — the
   same mechanism ``logged_in_app`` uses) and land on the Account settings page.
2. Account settings lists BOTH accounts with the active one marked; the active
   (regular) identity has no admin shell (``admin-tab`` absent).
3. Tapping the admin account switches to it (``set_active`` + a
   full reload) → the admin shell appears.

This file also drives the **append-mode "Add account"** flow
(``test_web_add_account_appends_second_identity_to_registry``): the switcher test
above SEEDS a two-account registry directly, so nothing exercised the real "Add
account" path. That test clicks ``account-add-button`` → append-mode onboarding
(``?add=1``) → imports a second (pre-registered) identity → asserts the registry
grows 1→2, the new account becomes active, and the original account's per-actor
slot survives (the no-user-data-loss invariant).

tier_3: needs a real ``fauna-nest`` binary (``nest_instance``); web app only.
"""
from __future__ import annotations

import json
import time
from types import SimpleNamespace

import pytest
from nacl.signing import SigningKey

from common import build_registry_seed, create_actor_and_register
from drivers import create_driver
from tests.test_sign_out_web import (  # noqa: F401
    _ls,
    _wait_registry_active,
)
from drivers.machine_test_setter import set_handle_check_snapshot
from helpers import web_store
from helpers.budgets import APP_RELAUNCH_S, RPC_ROUNDTRIP_S
from helpers.waiting import assert_no_relaunch, await_session_actor, wait_until

pytestmark = [pytest.mark.tier_3, pytest.mark.web]

# tests/e2e-unified/ui.yaml § settings (switcher) + navigation (admin-tab).
SWITCHER_LIST = "account-switcher-list"
SWITCHER_ITEM = "account-switcher-item"
ITEM_HANDLE = "account-item-handle"
ACTIVE_INDICATOR = "account-item-active-indicator"
REMOVE_BUTTON = "account-remove-button"
ADD_BUTTON = "account-add-button"
ADMIN_TAB = "admin-tab"

# Stage 2 (re-auth-on-activate). Web has no native OS re-auth prompt, so it
# renders the in-app confirm surface — the shape the linux slice ratified
# (long-term-store.md § Multi-account evolution → Per-account re-auth; web + tui
# adopt it). As on linux, there is deliberately NO `reauth-result` file seam:
# apple needs one because an OS sheet carries no test ID, whereas this prompt is
# in-app and drivable, so the journeys below click the real UI a user clicks
# (testing.md point 8).
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
INVITE_SUBMIT_BUTTON = "invite-request-submit-button"

# Two-element nav to the Account sub-page: the web nav agent reads the sub-page id
# from stack[1].id (stack[0] is the top-level view), so /app/settings/account needs
# both frames — exactly as test_account_switcher_linux.py does.
ACCOUNT_PAGE_NAV = {
    "nav": {"stack": [{"view": "settings"}, {"view": "settings", "id": "account"}]}
}


def _seed_two_accounts(nest_instance, node_url, active="user", second="admin"):
    """A claimed-admin account + a freshly-registered regular-user account, seeded
    active on the regular user (``active="admin"`` flips it, which the admin
    auto-default journey needs). Returns (seed_map, user_actor, user_secret,
    admin_actor, admin_secret). The admin is `nest_instance`'s own claimed
    identity (so `am-i-admin` is true for it); the user is registered via the
    admin key. `node_url` is the SPA's API base (the spa_url proxy) so a
    cross-origin silent-challenge after the admin switch is same-origin.

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
        admin_actor = other["actor_id_hex"]
        admin_secret = bytes(other["signing_key"]).hex()
    else:
        admin_actor = bytes(admin_sk.verify_key).hex()
        admin_secret = bytes(admin_sk).hex()

    user = create_actor_and_register(nest_instance["port"], admin_signing_key=admin_sk)
    user_actor = user["actor_id_hex"]
    user_secret = bytes(user["signing_key"]).hex()

    seed = build_registry_seed(
        [
            # add order == display order → user is row 0, admin is row 1.
            {"actor_id": user_actor, "secret_hex": user_secret, "nest_url": node_url,
             "device_id": "switcher-user", "handle": "user"},
            {"actor_id": admin_actor, "secret_hex": admin_secret, "nest_url": node_url,
             "device_id": "switcher-admin", "handle": "admin"},
        ],
        active=admin_actor if active == "admin" else user_actor,
    )
    return seed, user_actor, user_secret, admin_actor, admin_secret


def _seed_registry(driver, seed):
    """Write the registry seed map into localStorage. Web has no pre-navigation
    hook, so mutate the loaded page's localStorage via eval_js (the fauna/index +
    fauna/{actor}/* keys the wasm registry reads verbatim)."""
    script = ";".join(
        f"localStorage.setItem({json.dumps(k)},{json.dumps(v)})" for k, v in seed.items()
    )
    driver.eval_js(script)


@pytest.mark.feature("multiple-accounts")
def test_web_account_switcher_lists_switches_and_reveals_admin(nest_instance, spa_url):
    seed, user_actor, user_secret, _admin_actor, _admin_secret = _seed_two_accounts(
        nest_instance, spa_url
    )

    driver = create_driver("web")
    driver.launch({"url": spa_url + "/app/"})
    try:
        # (1) Seed the registry, then authenticate as the active (regular) account
        # via the session patch (in-memory, no challenge) and land on the Account
        # settings page. The helper's re-boot over the seeded localStorage is what
        # keeps this deterministic — see its docstring for the redirect race.
        #
        # (2) Account settings lists BOTH accounts, the active one marked; a regular
        # (non-admin) active identity has no admin shell. The helper waits for the
        # ROWS (not just the container) — loadAccounts() populates the #each
        # asynchronously.
        _auth_on_account_page(driver, seed, user_secret, "user", user_actor, spa_url)
        assert driver.count(SWITCHER_ITEM) == 2, "both seeded accounts must be listed"
        assert driver.count(ACTIVE_INDICATOR) == 1, (
            "exactly one row (the active/user row) carries the active indicator"
        )
        _admin_tab_absent_off_settings(
            driver, "a regular (non-admin) active identity must have no admin shell"
        )

        # (3) Tap the admin row → set_active + full reload → the
        # admin shell appears (web's teardown+rebuild; am-i-admin per active actor).
        driver.click(SWITCHER_ITEM, index=1)
        driver.wait_for(ADMIN_TAB, timeout=30)
        assert driver.is_visible(ADMIN_TAB), (
            "switching to the admin identity must reveal the admin shell "
            "(live re-auth on reload)"
        )
    finally:
        driver.teardown()


@pytest.mark.feature("multiple-accounts")
def test_web_remove_account_shrinks_switcher_live(nest_instance, spa_url):
    """Removing a non-active account LIVE-refreshes the switcher in place: the
    row disappears with NO re-navigation or reload (`removeAccount`'s
    `loadAccounts()` re-fetch) — the web twin of linux/tui's own
    `..._remove_account_shrinks_switcher_live`."""
    seed, user_actor, user_secret, admin_actor, _admin_secret = _seed_two_accounts(
        nest_instance, spa_url
    )

    driver = create_driver("web")
    driver.launch({"url": spa_url + "/app/"})
    try:
        _auth_on_account_page(driver, seed, user_secret, "user", user_actor, spa_url)
        # Both accounts listed, active on the regular user (row 0); the admin
        # (row 1) is the non-active row, so it carries the (only) remove button.
        assert driver.count(SWITCHER_ITEM) == 2, "both seeded accounts must be listed"

        driver.click(REMOVE_BUTTON)

        # LIVE: the removed row vanishes from the SAME page (no re-navigation,
        # no reload), and the surviving (regular) account stays active.
        wait_until(
            lambda: driver.count(SWITCHER_ITEM) == 1,
            15.0,
            diagnose=lambda: f"switcher item count={driver.count(SWITCHER_ITEM)}",
        )
        assert driver.count(ACTIVE_INDICATOR) == 1, (
            "the surviving (regular) account stays active after the removal"
        )

        # The store-level remove also landed: the persisted registry dropped
        # 2->1, dropping the ADMIN (non-active) account, never the active user.
        result = {}

        def _removed():
            index = _read_registry_index(driver)
            result["index"] = index
            return index is not None and len(index.get("accounts", [])) == 1

        wait_until(_removed, 30.0, diagnose=lambda: f"index={result.get('index')!r}")
        actor_ids = [a["actor_id"] for a in result["index"]["accounts"]]
        assert admin_actor not in actor_ids, (
            "the removed (admin) account must be gone from the persisted registry"
        )
    finally:
        driver.teardown()


#: A switch's full reload, the silent sign-in and the account runtime's
#: assembly — the store exists once the runtime has opened it.
ACCOUNT_STORE_OPEN_S = 90.0


def _local_storage_keys_with(driver, needle: str) -> list[str]:
    """Every `localStorage` key carrying `needle` (an actor hex), sorted."""
    return sorted(driver.eval_js(
        f"Object.keys(localStorage).filter(k => k.includes({json.dumps(needle)}))"
    ))


@pytest.mark.feature("multiple-accounts")
def test_web_removing_an_identity_deletes_its_data_and_leaves_the_others(
    nest_instance, spa_url
):
    """Removing an identity deletes THAT identity's data in this browser and
    leaves every other identity's data untouched — the web twin of
    `test_tui_removing_an_identity_deletes_its_data_and_leaves_the_others`
    (`account-scoping.md` § Serialized switching → *Erasure follows scope*).

    `test_web_remove_account_shrinks_switcher_live` proves the switcher row and
    the registry entry go; this proves the account's own state goes with them:
    every `localStorage` key carrying the second identity's actor id (the registry's
    `fauna/<other>/*` slots and the actor-keyed keys outside that namespace,
    `account_scope::ACTOR_KEY_BUILDERS`) and the second identity's account store
    (`fauna-account-store/<other>`), while the user's secret, keys and store and
    every install-scoped key survive.

    Precondition seed: `fauna_backup_audit_<actor>` is written only by the
    Backups page's audit pass, which no session runs deterministically, so the
    test writes one for EACH identity by `eval_js` — the second identity's is what the
    erase must take, the user's what it must leave. Everything else is minted by
    real sessions (the second identity's through a switch). Every wait is on state
    (`e2e-conventions.md` point 14); the mutations are the switcher's own taps.

    The removal also retires this browser from the second identity, as a
    sign-out does (§ Erasure follows scope → *Removing an account retires this
    machine from it, as a sign-out does*): its named row stays on the nest with
    its grant cleared. The roster read sits behind causal barriers (convention
    14) — the second identity's own enrollment latch before the removal, the
    cleared roster within a named budget after it.

    The second identity is a fresh regular user, never the run's shared admin:
    the roster must be exact (``helpers.enrollment``), and a removal whose nest
    half were deferred would leave a grant no teardown could find to retire
    (`e2e-launch-isolation.md`, *A discarded driver retires its machine*)."""
    from helpers import enrollment

    seed, user_actor, user_secret, other_actor, other_secret = _seed_two_accounts(
        nest_instance, spa_url, second="user"
    )
    nest_url = nest_instance["url"]
    other = {
        "actor_id_hex": other_actor,
        "signing_key": SigningKey(bytes.fromhex(other_secret)),
    }
    other_store = f"{web_store.ACCOUNT_STORE_ROOT}/{other_actor}"
    user_store = f"{web_store.ACCOUNT_STORE_ROOT}/{user_actor}"

    def await_store(name, what):
        seen = {}

        def present():
            seen.update(web_store.account_store_names(driver))
            return name in seen.get("indexeddb", [])

        wait_until(
            present, ACCOUNT_STORE_OPEN_S, interval=1.0,
            diagnose=lambda: f"{what}: no {name} in this origin; it holds {seen!r}",
        )

    def switch_to(index, actor):
        driver.set_state(ACCOUNT_PAGE_NAV)
        driver.wait_for(SWITCHER_ITEM, timeout=30)
        _wait_switcher_rows(2)
        driver.click(SWITCHER_ITEM, index=index)
        _wait_session_actor(driver, actor, timeout=60)

    def _wait_switcher_rows(n):
        wait_until(
            lambda: driver.count(SWITCHER_ITEM) == n, 30.0,
            diagnose=lambda: f"switcher item count={driver.count(SWITCHER_ITEM)}",
        )

    driver = create_driver("web")
    driver.launch({"url": spa_url + "/app/"})
    try:
        _auth_on_account_page(driver, seed, user_secret, "user", user_actor, spa_url)

        # Give the second identity a real session of its own, so it has data to lose.
        switch_to(1, other_actor)
        await_store(other_store, "the second identity's session")
        # This browser is enrolled on it now: its named row carries a grant.
        _writer, other_row, _ = enrollment.await_enrollment(
            SimpleNamespace(driver=driver), nest_url, other
        )
        assert enrollment.principals(nest_url, other).get(other_row), (
            f"the precondition: the latched row {other_row} carries this browser's grant"
        )

        # Back to the user: the second identity's row is now the non-active one, the only
        # row the switcher offers a remove button on.
        switch_to(0, user_actor)
        await_store(user_store, "the user's session")
        web_store.seed(driver, {
            f"fauna_backup_audit_{other_actor}": "{}",
            f"fauna_backup_audit_{user_actor}": "{}",
        })
        other_keys = _local_storage_keys_with(driver, other_actor)
        assert f"fauna/{other_actor}/secret" in other_keys, (
            f"precondition: the second identity's secret is in this browser; its keys are {other_keys}"
        )
        kept_before = [
            k for k in driver.eval_js("Object.keys(localStorage)") if other_actor not in k
        ]

        # The switch's reload lands wherever the app routes a load; the remove
        # button lives on the Account page.
        driver.set_state(ACCOUNT_PAGE_NAV)
        driver.wait_for(SWITCHER_ITEM, timeout=30)
        _wait_switcher_rows(2)
        driver.click(REMOVE_BUTTON)
        index = _wait_registry_count(driver, 1, timeout=30)
        assert [a["actor_id"] for a in index["accounts"]] == [user_actor], (
            f"only the removed identity may leave the registry; got {index!r}"
        )

        # Registry removal, then the erase, in one gesture — so a registry
        # that shrank means the erase is already running or done.
        left = {}

        def other_gone():
            left["keys"] = _local_storage_keys_with(driver, other_actor)
            left["stores"] = web_store.account_store_names(driver)
            return not left["keys"] and other_store not in left["stores"]["indexeddb"]

        wait_until(
            other_gone, RPC_ROUNDTRIP_S, interval=1.0,
            diagnose=lambda: (
                "the removed identity's data survived in this browser: localStorage "
                f"{left.get('keys')!r}, stores {left.get('stores')!r}; error-message: "
                f"{driver.get_text('error-message') if driver.is_visible('error-message') else ''!r}"
            ),
        )
        lost = sorted(
            set(kept_before) - set(driver.eval_js("Object.keys(localStorage)"))
        )
        assert not lost, (
            "removing the second identity must leave every other key in this browser — the user's "
            f"and the install's — untouched; gone: {lost}"
        )
        assert _ls(driver, f"fauna/{user_actor}/secret") == user_secret, (
            "the remaining identity's secret must survive the removal"
        )
        assert user_store in web_store.account_store_names(driver)["indexeddb"], (
            "the remaining identity's account store must survive the removal"
        )
        # The nest half: the named row stays, its grant cleared — as after a
        # sign-out (the barrier's diagnosis names every other reading).
        enrollment.await_grant_cleared(
            nest_url, other, other_row, "after removing the identity through the switcher"
        )
    finally:
        driver.teardown()


def _seed_one_account(nest_instance, node_url):
    """A SINGLE registered regular-user account, seeded active — the pre-append
    single-identity state a first-run install lands in. Returns (seed_map,
    user_actor, user_secret)."""
    admin_sk = nest_instance["admin"]["signing_key"]
    user = create_actor_and_register(nest_instance["port"], admin_signing_key=admin_sk)
    user_actor = user["actor_id_hex"]
    user_secret = bytes(user["signing_key"]).hex()
    seed = build_registry_seed(
        [{"actor_id": user_actor, "secret_hex": user_secret, "nest_url": node_url,
          "device_id": "switcher-user", "handle": "user"}],
        active=user_actor,
    )
    return seed, user_actor, user_secret


def _auth_on_account_page(driver, seed, secret, handle, actor, spa_url):
    """Seed the registry + authenticate as ``actor`` via the session patch and land
    on the Account settings page — the shared arrangement of every test in this file.

    The ``hard_reload()`` is load-bearing, not tidiness. Web has no pre-navigation
    seeding hook, so ``launch()`` boots the SPA with EMPTY localStorage and the root
    layout's unauthenticated guard fires ``goto('/app/onboarding')``
    (``+layout.svelte`` § the ``/app`` root-index redirect). Seeding afterwards makes
    credentials appear *mid-boot*, so that redirect races the session patch's own
    nav — whoever lands last wins, and when the guard wins the Account page never
    mounts and every later step fails on a `count=0` element that was simply never
    navigated to. Re-booting over the now-seeded localStorage removes the race at the
    source: the guard sees credentials, takes no redirect, and the nav patch below is
    the only navigation in play (testing.md § point 14 — assert latency-independent
    state; delete the dependency rather than widen a timeout)."""
    _seed_registry(driver, seed)
    driver.hard_reload()
    driver.set_state({
        "session": {
            "authenticated": True,
            "node_url": spa_url,
            "secret_hex": secret,
            "handle": handle,
            "actor_id": actor,
        },
        **ACCOUNT_PAGE_NAV,
    })
    driver.wait_for(SWITCHER_LIST, timeout=30)
    driver.wait_for(SWITCHER_ITEM, timeout=30)


def _admin_tab_absent_off_settings(driver, what):
    """Assert ``admin-tab`` is absent from the app shell, then return to the Account page.

    ``admin-tab`` lives in the app sidebar, and Settings is the sidebar-SWAP shell
    (its rail replaces the app sidebar), so on ``/app/settings/*`` the element is
    not in the DOM for ANY identity — a negative read taken there is vacuous: a
    gate-bypass mutant (``{#if userIsAdmin}`` forced true) leaves it green
    (measured 2026-10-14). Leave Settings first; ``feed-tab`` (an ungated sibling
    in the same sidebar) is the causal anchor that the shell this read is about is
    the one mounted."""
    driver.set_state({"nav": {"stack": [{"view": "feed"}]}})
    driver.wait_for("feed-tab", timeout=30)
    assert driver.is_absent(ADMIN_TAB), what
    driver.set_state(ACCOUNT_PAGE_NAV)
    driver.wait_for(SWITCHER_ITEM, timeout=30)


def _read_registry_index(driver):
    """The `fauna/index` AccountIndex (`{active, accounts:[{actor_id,...}]}`) the
    wasm registry persists verbatim, or None if unset. The registry is the source
    of truth (`accounts.ts`); reading it directly asserts the append's effect
    independent of the post-switch reconnect (the onboarding-derived nest_url is
    https — Pillar C uniform-https — which the plain-http tier_3 nest can't serve,
    so the reload's live sign-in can't complete; the registry WRITE is client-side
    and does)."""
    raw = driver.eval_js("localStorage.getItem('fauna/index')")
    return json.loads(raw) if raw else None


def _wait_registry_active_is(driver, actor, timeout=30):
    """Poll `fauna/index` until its `active` names `actor`; return the index.

    The append is `accountsAdd` then `accountsSwitch` — two awaits — so the
    registry reads count 2 a tick before it reads the new account active.
    Asserting `active` straight after `_wait_registry_count` races that gap
    (convention 14); this waits for the switch itself.
    """
    last = {}

    def switched():
        last["index"] = _read_registry_index(driver)
        return last["index"] and last["index"].get("active") == actor

    wait_until(
        switched, timeout, interval=0.5,
        diagnose=lambda: f"`fauna/index` never named {actor} active; last index={last.get('index')!r}",
    )
    return last["index"]


def _wait_registry_count(driver, n, timeout=30):
    """Poll `fauna/index` until it lists exactly `n` accounts; return it."""
    deadline = time.monotonic() + timeout
    last = None
    while time.monotonic() < deadline:
        last = _read_registry_index(driver)
        if last and len(last.get("accounts", [])) == n:
            return last
        time.sleep(0.5)
    raise AssertionError(
        f"registry never reached {n} accounts within {timeout}s; last index={last!r}"
    )


def _wait_session_actor(driver, actor_id, timeout=30):
    """Poll the in-memory session identity until `session.actor_id == actor_id`.
    The abandoned-append recovery (store.ts `accountsBoot` re-resolve → re-`set` of
    the identity store) is async, so it lands a beat after the destination page
    mounts. `session.actor_id` has NO localStorage fallback (`web-bridge/agent.js`),
    so it isolates the in-memory store — the true discriminator for a current-load
    (no-reload) recovery."""
    deadline = time.monotonic() + timeout
    last = None
    while time.monotonic() < deadline:
        try:
            last = driver.get_state("session.actor_id")
        except Exception:
            # A full-reload switch (Stage 2 approve / the auto-default journeys)
            # can race the poll mid-navigation — keep polling until the deadline.
            last = None
        if last == actor_id:
            return
        time.sleep(0.5)
    raise AssertionError(
        f"session.actor_id never recovered to {actor_id} within {timeout}s; last={last!r}"
    )


@pytest.mark.feature("multiple-accounts")
def test_web_add_account_appends_second_identity_to_registry(nest_instance, spa_url):
    # (0) A pre-registered SECOND identity to import in the append wizard. Its
    # actor is registered on `nest_instance` exactly like the switcher accounts,
    # so it's a faithful "import an identity I already have" (devices.md paste-secret).
    admin_sk = nest_instance["admin"]["signing_key"]
    second = create_actor_and_register(nest_instance["port"], admin_signing_key=admin_sk)
    second_actor = second["actor_id_hex"]
    second_secret = bytes(second["signing_key"]).hex()

    seed, first_actor, first_secret = _seed_one_account(nest_instance, spa_url)

    driver = create_driver("web")
    driver.launch({"url": spa_url + "/app/"})
    try:
        # (1) Seed ONE account + authenticate as it (in-memory, no challenge) and
        # land on the Account settings page — the pre-append single-identity state.
        _auth_on_account_page(driver, seed, first_secret, "user", first_actor, spa_url)
        assert driver.count(SWITCHER_ITEM) == 1, "exactly one account before Add account"

        # (2) "Add account" → append-mode onboarding (`?add=1`), a fresh wizard at
        # identity_choice (onMount bypasses the launch-routing resume cases).
        driver.click(ADD_BUTTON)
        driver.wait_for(IMPORT_IDENTITY_BUTTON, timeout=30)

        # (2b) Install the dial override, HERE and not earlier. The wizard derives
        # the incoming account's `nest_url` from the typed handle domain — uniform
        # https by ratified design (`fauna_provisioning::probe`) — so the
        # post-switch launch dials `https://localhost:<port>` at a plain-HTTP
        # tier_3 nest. The sanctioned seam is the `provider_base_urls["nest"]`
        # override, NOT a `serve_tls=True` nest (`long-term-store.md`
        # § Implementation status today).
        #
        # ⚠ Placement is web-specific: web's `set_provider_base_urls` is a query
        # param + `hard_reload()`, not the shared `call_machine_method` bridge, and
        # a reload mid-wizard re-enters the `?add=1` branch and rebuilds a FRESH
        # wizard — which would discard an already-imported identity. At
        # `identity_choice` the re-boot is idempotent: we land back on this step.
        driver.set_provider_base_urls({"nest": nest_instance["url"]})
        driver.wait_for(IMPORT_IDENTITY_BUTTON, timeout=30)

        # (3) Import the pre-registered second identity (the paste-secret path). An
        # append-mode confirm writes nothing; the identity lives in the wizard
        # machine, and `accountsAdd` reads `effectiveSecret()` on LoggedIn.
        driver.click(IMPORT_IDENTITY_BUTTON)
        driver.wait_for(PASTE_SECRET_FIELD, timeout=15)
        driver.clear_and_type(PASTE_SECRET_FIELD, second_secret)
        driver.click(IMPORT_SUBMIT_BUTTON)
        driver.wait_for(HANDLE_INPUT, timeout=15)

        # (4) Inject the AlreadyOnNest (welcome-back) handle-check outcome so Continue
        # lands `WizardOutcome::LoggedIn` — the append divergence's trigger. The REAL
        # silent-challenge → AlreadyOnNest path is covered by test_onboarding_localhost.py
        # (browser WS-RPC) + test_handle_entry_outcomes.py (every outcome); here we drive
        # the NOVEL web append branch (accountsAdd + accountsSwitch) that fires on LoggedIn.
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

        # (5) The append grew the registry 1→2 and switched to the new account. Assert
        # via the registry index (client-side, reconnect-independent — see
        # `_read_registry_index`).
        index = _wait_registry_count(driver, 2, timeout=30)
        actor_ids = [a["actor_id"] for a in index["accounts"]]
        assert first_actor in actor_ids, (
            "the original account must survive the append (no-user-data-loss)"
        )
        assert second_actor in actor_ids, "the imported account must be appended"
        index = _wait_registry_active_is(driver, second_actor)
        assert index["active"] == second_actor, (
            "the append switches to the newly-added account (design Decision 1)"
        )
        # The original account's per-actor secret slot is untouched by the append.
        assert driver.eval_js(f"localStorage.getItem('fauna/{first_actor}/secret')"), (
            "the original account's stored secret must survive the append"
        )

        # (6) …and that account reaches a WORKING session, not just a registry
        # row. This is the half every app's append test skipped until 2026-08-14
        # (tui first); it is the whole point of "Add account".
        await_session_actor(
            driver, second_actor, budget_s=APP_RELAUNCH_S, what="the append"
        )

        # (7) The seam redirects the SOCKET, never the truth: what the append
        # persisted for the new account is still the literal derived from the
        # typed handle domain — an https URL this plain-HTTP nest never served.
        # Asserting the store is what keeps (6) honest: an override that leaked
        # into persistence would make every later launch, in a release build with
        # no override installed, dial a torn-down fixture
        # (`fauna_launch_machine::dial` — "It redirects the socket, never the
        # truth"; the shared-Rust twin is `dial_override_never_reaches_the_store`).
        stored_url = driver.eval_js(
            f"localStorage.getItem('fauna/{second_actor}/nest_url')"
        )
        assert stored_url == f"https://localhost:{nest_instance['port']}", (
            "the appended account must persist the URL the wizard derived from "
            "the typed handle domain, NOT the harness dial override; got "
            f"{stored_url!r}"
        )
    finally:
        driver.teardown()


@pytest.mark.feature("multiple-accounts")
def test_web_add_account_pending_invite_submit_switches_the_live_session(nest_instance, spa_url):
    """A pending-invite submit during "Add account" registers the joiner, writes its
    per-actor pending-invite slot and switches to it — registry and in-memory session
    together (`onboarding.md` § Multi-account). Web twin of
    ``test_apple_add_account_pending_invite_submit_switches_the_live_session``.

    The append IMPORT step already moved the in-memory session to the joiner
    (`identity.login`, store.ts); the SUBMIT's append arm
    (`persistPendingInvite` in `routes/onboarding/+page.svelte`) is what runs
    `accountsAdd` + `accountsSwitch` before the slot write. Deleting that block leaves
    the registry on the outgoing account while the session names the joiner, so the
    registry assertions below are the ones that redden. Web counts no
    `session_generation` teardown on this path, hence no generation witness.

    Mutation (2026-10-12): deleting the `accountsAdd`/`accountsSwitch` block turned the
    registry-count assertion red ("registry never reached 2 accounts"); restored, green."""
    seed, first_actor, first_secret = _seed_one_account(nest_instance, spa_url)

    joiner_key = SigningKey.generate()
    joiner_secret = bytes(joiner_key).hex()
    joiner_actor = bytes(joiner_key.verify_key).hex()
    handle = f"addpending-{int(time.time() * 1000)}"

    driver = create_driver("web")
    driver.launch({"url": spa_url + "/app/"})
    try:
        _auth_on_account_page(driver, seed, first_secret, "user", first_actor, spa_url)
        assert driver.count(SWITCHER_ITEM) == 1, "exactly one account before Add account"

        driver.click(ADD_BUTTON)
        driver.wait_for(IMPORT_IDENTITY_BUTTON, timeout=30)
        driver.click(IMPORT_IDENTITY_BUTTON)
        driver.wait_for(PASTE_SECRET_FIELD, timeout=15)
        driver.clear_and_type(PASTE_SECRET_FIELD, joiner_secret)
        driver.click(IMPORT_SUBMIT_BUTTON)
        driver.wait_for(HANDLE_INPUT, timeout=15)

        # A test nest is not DNS-discoverable, so hop to `invite_request` directly
        # (the same hop test_pending_invite_journey.py makes), then the REAL submit.
        driver.call_machine_method(
            "navigate_to_invite_request_for_known_nest",
            json.dumps([nest_instance["url"], handle]),
        )
        driver.wait_for(INVITE_SUBMIT_BUTTON, timeout=20)
        driver.click(INVITE_SUBMIT_BUTTON)

        index = _wait_registry_count(driver, 2, timeout=30)
        actor_ids = [a["actor_id"] for a in index["accounts"]]
        assert first_actor in actor_ids, (
            "the original account must survive the append (no-user-data-loss)"
        )
        assert joiner_actor in actor_ids, "the joiner must be registered on the submit return"
        index = _wait_registry_active_is(driver, joiner_actor)
        assert index["active"] == joiner_actor, (
            "the pending-invite submit activates the joiner (onboarding.md § Multi-account)"
        )
        assert driver.eval_js(
            f"localStorage.getItem('fauna/{joiner_actor}/pending_invite')"
        ), f"the joiner's per-actor pending-invite slot must be written; index={index!r}"
        assert driver.eval_js(f"localStorage.getItem('fauna/{first_actor}/secret')") == first_secret, (
            "the outgoing account's secret must survive the switch"
        )
        _wait_session_actor(driver, joiner_actor, timeout=30)
    finally:
        driver.teardown()


@pytest.mark.feature("multiple-accounts")
def test_web_add_account_abandon_recovers_prior_identity_on_current_load(nest_instance, spa_url):
    """Abandoning an append-mode "Add account" mid-wizard must recover the prior
    (active-registry) identity on the CURRENT load — not only after a reload.

    The append wizard's import step calls `identity.login(newSecret)` (store.ts),
    which sets the in-memory identity store to the new (abandoned) identity — and
    persists NOTHING: an append-mode confirm is a pure derivation
    (`persist_confirmed_identity`'s append rule), so the registry never grows and
    the active account is still #1. This test asserts the in-memory identity
    follows the registry's active account again on the current load
    (`identity.init()` re-reads it on every page mount), the previously-deferred
    single-load gap (`docs/goal/architecture/long-term-store.md` § Implementation
    status today).

    The abandon is driven as an in-SPA `goto` (`stores.goto`, NO hard reload — a
    reload would trivially self-heal via `loadIdentity()` and prove nothing), the
    faithful "browser-back out of the wizard" path.
    """
    admin_sk = nest_instance["admin"]["signing_key"]
    # A pre-registered SECOND identity to import then ABANDON (never appended).
    second = create_actor_and_register(nest_instance["port"], admin_signing_key=admin_sk)
    second_actor = second["actor_id_hex"]
    second_secret = bytes(second["signing_key"]).hex()

    seed, first_actor, first_secret = _seed_one_account(nest_instance, spa_url)

    driver = create_driver("web")
    driver.launch({"url": spa_url + "/app/"})
    try:
        # (1) One account, authenticated as it, on the Account settings page.
        _auth_on_account_page(driver, seed, first_secret, "user", first_actor, spa_url)
        assert driver.count(SWITCHER_ITEM) == 1, "exactly one account before Add account"

        # (2) Add account → append onboarding → import the second identity. The import
        # sets the in-memory identity store to it (and persists nothing);
        # reaching handle entry proves the import committed that state.
        driver.click(ADD_BUTTON)
        driver.wait_for(IMPORT_IDENTITY_BUTTON, timeout=30)
        driver.click(IMPORT_IDENTITY_BUTTON)
        driver.wait_for(PASTE_SECRET_FIELD, timeout=15)
        driver.clear_and_type(PASTE_SECRET_FIELD, second_secret)
        driver.click(IMPORT_SUBMIT_BUTTON)
        driver.wait_for(HANDLE_INPUT, timeout=15)

        # Mid-wizard the in-memory identity is the ABANDONED identity, nothing
        # durable names it, and the registry has NOT grown (no accountsAdd until
        # Continue → LoggedIn).
        _wait_session_actor(driver, second_actor, timeout=15)
        assert driver.eval_js(f"localStorage.getItem('fauna/{second_actor}/secret')") is None, (
            "an append-mode confirm must not register the imported identity"
        )
        idx = _read_registry_index(driver)
        assert idx and len(idx["accounts"]) == 1 and idx["active"] == first_actor, (
            "abandoning before Continue must NOT append to the registry"
        )

        # (3) ABANDON: navigate back into the app WITHOUT completing — an in-SPA goto
        # (`stores.goto`, no hard reload), mounting the Account page fresh so its
        # onMount re-runs `identity.init()` → `accountsBoot()`.
        driver.set_state({**ACCOUNT_PAGE_NAV})
        driver.wait_for(SWITCHER_LIST, timeout=30)

        # (4) The current load RECOVERED the prior identity — no reload:
        #   - the in-memory session identity is back to account #1 (the discriminator).
        #   - the switcher still lists exactly the one original account, active on #1.
        _wait_session_actor(driver, first_actor, timeout=30)
        assert driver.count(SWITCHER_ITEM) == 1, "still exactly the one original account"
        idx2 = _read_registry_index(driver)
        assert idx2 and idx2["active"] == first_actor and len(idx2["accounts"]) == 1, (
            "the registry is unchanged by an abandoned append"
        )
    finally:
        driver.teardown()


# ---------------------------------------------------------------------------
# Stage 2: re-auth-on-activate (web twin of the linux journeys in
# test_account_switcher_linux.py — long-term-store.md § Multi-account evolution
# → Per-account re-auth, ratified 2026-07-16)
# ---------------------------------------------------------------------------


def _wait_flag(driver, actor, want, key="require_confirm_to_activate", timeout=20):
    """Poll the persisted `fauna/index` until ``actor``'s ``key`` reads ``want``;
    return the final per-actor map for the failure message."""
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
def test_web_require_confirm_gates_switch_decline_then_approve(nest_instance, spa_url):
    """Stage 2: flagging an account via its `account-require-confirm-toggle` makes
    activating it demand a re-auth confirmation first. Web has no native OS prompt,
    so it renders the in-app `account-activate-reauth-prompt` (the linux-ratified
    shape). Declining is a PURE NO-OP — registry untouched, no reload, the current
    account stays active — and approving completes the same switch journey the
    unflagged path takes (set_active_confirmed + full reload). Both
    the flag write and the confirm are driven through the UI (testing.md point 8)
    and asserted against the persisted registry index (headless observable)."""
    seed, user_actor, user_secret, admin_actor, _admin_secret = _seed_two_accounts(
        nest_instance, spa_url
    )

    driver = create_driver("web")
    driver.launch({"url": spa_url + "/app/"})
    try:
        _auth_on_account_page(driver, seed, user_secret, "user", user_actor, spa_url)
        assert driver.count(SWITCHER_ITEM) == 2, "both seeded accounts must be listed"

        # (1) Flag the ADMIN row (row 1) through its own toggle — the UI write path.
        # Setting the flag itself never prompts; only activation does.
        driver.click(REQUIRE_CONFIRM_TOGGLE, scope=f"{SWITCHER_ITEM}[1]")
        flagged = _wait_flag(driver, admin_actor, True, timeout=10)
        assert flagged.get(user_actor) is False, (
            f"the row toggle must flag only the tapped account; got {flagged!r}"
        )

        # (2) DECLINE: activating the flagged row raises the in-app prompt, and
        # cancelling it must be a PURE no-op — no registry mutation, no reload, the
        # regular user stays the in-memory session identity, no admin shell.
        driver.click(SWITCHER_ITEM, index=1)
        driver.wait_for(REAUTH_PROMPT, timeout=15)
        # Convention 14: the absence is anchored to causal order, not to a
        # window. Closing the prompt is the handler's own completion observable,
        # so it doubles as this check's settle condition and as the assertion
        # the old settle-sleep made separately below.
        assert_no_relaunch(
            driver,
            lambda: driver.click(REAUTH_CANCEL_BUTTON),
            lambda: not driver.is_visible(REAUTH_PROMPT),
            what="declining the re-auth prompt",
        )
        assert driver.get_state("session.actor_id") == user_actor, (
            "a DECLINED re-auth must leave the original account active (pure no-op)"
        )
        index = _read_registry_index(driver)
        assert index["active"] == user_actor, (
            "a declined re-auth must leave the persisted registry untouched"
        )
        _admin_tab_absent_off_settings(
            driver, "a declined re-auth must not reveal the admin shell"
        )

        # (3) APPROVE: the same tap, confirmed, completes the switch (full
        # reload — exactly the unflagged journey).
        driver.click(SWITCHER_ITEM, index=1)
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
def test_web_admin_auto_default_flags_admin_and_explicit_off_sticks(
    nest_instance, spa_url
):
    """The admin auto-default (`long-term-store.md` § Multi-account evolution —
    "Default off; a client turns it on for its admin identity"): authenticating as
    the ADMIN account auto-enables its require-confirm flag at web's am-i-admin
    observation (the `+layout.svelte` nav-gate probe that reveals `admin-tab`),
    with NO user tap — asserted against the persisted index; the regular account
    stays unflagged. The auto-defaulted flag must also RENDER once the Account
    page re-reads the registry (a stale OFF would make the flag impossible to
    turn off — the user's tap on an OFF-looking switch writes ON). And the user's
    explicit OFF sticks: turn the toggle off, re-trigger the observation by
    switching away and back (both switches unflagged → no confirm involved), and
    the auto-default must NOT re-flip it — `require_confirm_user_set` pins the
    user's choice."""
    seed, user_actor, _user_secret, admin_actor, admin_secret = _seed_two_accounts(
        nest_instance, spa_url, active="admin"
    )

    driver = create_driver("web")
    driver.launch({"url": spa_url + "/app/"})
    try:
        _auth_on_account_page(driver, seed, admin_secret, "admin", admin_actor, spa_url)
        # NOTE: no admin-tab wait here — Settings is the sidebar-SWAP shell (its
        # rail replaces the app sidebar), so admin-tab is not in the DOM on
        # /app/settings/* at all. The root layout still runs its am-i-admin nav
        # gate regardless of the visible shell, and the auto-defaulted flag in the
        # persisted index (below) is the direct observable of that resolution.

        # (1) The am-i-admin observation auto-enabled the ADMIN row's flag — no tap.
        flags = _wait_flag(driver, admin_actor, True, timeout=30)
        assert flags.get(user_actor) is False, (
            f"the auto-default must only flag the admin identity; got {flags!r}"
        )

        # (2) The auto-defaulted flag must also RENDER: leave the Account sub-page
        # and come back (as a user would, once the observation has landed) so its
        # `current === 'account'` effect re-reads the registry; the admin's toggle
        # must read ON. A stale OFF over a registry that says ON is the bug linux
        # hit: the user could never turn the flag off.
        driver.set_state(
            {"nav": {"stack": [{"view": "settings"}, {"view": "settings", "id": "status"}]}}
        )
        time.sleep(0.5)
        driver.set_state(ACCOUNT_PAGE_NAV)
        driver.wait_for(SWITCHER_ITEM, timeout=15)
        assert (
            driver.get_attr(REQUIRE_CONFIRM_TOGGLE, "state", scope=f"{SWITCHER_ITEM}[1]")
            == "on"
        ), (
            "the admin row's toggle must render the auto-defaulted flag once the "
            "page re-reads the registry; a stale OFF makes the flag impossible to "
            "turn off"
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

        # (4) Re-trigger the observation: switch to the user (admin is unflagged
        # now, the user always was — no confirm anywhere; each switch is a full
        # reload whose relaunched nav gate re-fires checkIsAdmin), then back to the
        # admin. The fresh am-i-admin=true observation must NOT re-flip the flag.
        driver.click(SWITCHER_ITEM, index=0)
        _wait_session_actor(driver, user_actor, timeout=45)
        driver.set_state(ACCOUNT_PAGE_NAV)
        driver.wait_for(SWITCHER_ITEM, timeout=30)
        driver.click(SWITCHER_ITEM, index=1)
        _wait_session_actor(driver, admin_actor, timeout=45)
        # The nav gate resolving admin again (admin-tab) IS the fresh observation;
        # give the refused auto-default a bounded settle window, then pin the OFF.
        driver.wait_for(ADMIN_TAB, timeout=45)
        time.sleep(3.0)
        index = _read_registry_index(driver)
        admin_entry = next(a for a in index["accounts"] if a["actor_id"] == admin_actor)
        assert admin_entry.get("require_confirm_to_activate") is False, (
            "an explicit user OFF must stick against the admin auto-default across "
            f"a fresh am-i-admin observation; got {admin_entry!r}"
        )
    finally:
        driver.teardown()


# ---------------------------------------------------------------------------
# Nest-binding walk-away (account-scoping.md § Concurrent instances, the
# delete corollary) — the admin-nest "factory reset this nest" handler's
# post-reset cleanup must route through the registry, not a direct
# `localStorage.removeItem('fauna_node_url')`: the per-actor `nest_url` slot
# is the only place a binding lives, so a direct delete of a bare key would
# leave it and the binding would survive the reset. Exercised via the test-only
# `window.__fauna_clearNestBindingForTest` hook (`+layout.svelte`) — the same
# `accountsClearNestBinding` call the admin-nest page's handler makes — so the
# reload-resurrection property is proven without a live
# `fauna.admin.factory_reset` round trip + nest restart.
# ---------------------------------------------------------------------------


def test_web_clear_nest_binding_does_not_resurrect_on_reload(nest_instance, spa_url):
    seed, user_actor, user_secret, _admin_actor, _admin_secret = _seed_two_accounts(
        nest_instance, spa_url
    )

    driver = create_driver("web")
    driver.launch({"url": spa_url + "/app/"})
    try:
        _auth_on_account_page(driver, seed, user_secret, "user", user_actor, spa_url)

        # Precondition: the per-actor slot holds the bound nest url — what a
        # walk-away has to clear.
        assert driver.eval_js(f"localStorage.getItem('fauna/{user_actor}/nest_url')") == spa_url, (
            "precondition: the per-actor nest_url slot must be materialized"
        )

        # Drive the fix: the same registry-routed call the admin-nest page's
        # factory-reset handler makes post-reset.
        driver.eval_js(f"window.__fauna_clearNestBindingForTest({json.dumps(user_actor)})")

        assert driver.eval_js(f"localStorage.getItem('fauna/{user_actor}/nest_url')") is None, (
            "clearNestBinding must delete the per-actor nest_url slot"
        )

        # ...and it must STAY gone across a reload: the per-actor slot is the
        # only place a binding lives, so nothing can resurrect it.
        driver.hard_reload()
        _wait_registry_active(driver)
        assert driver.eval_js(f"localStorage.getItem('fauna/{user_actor}/nest_url')") is None, (
            "a reload must not resurrect the cleared nest binding"
        )
    finally:
        driver.teardown()



# ---------------------------------------------------------------------------
# The tui-first witnesses, lifted to web: an unlaunchable switch is
# refused and said (multiple-accounts 9), and an abandoned first-run identity
# never ghosts (8). tui twins: `test_account_switcher_tui.py`.
# ---------------------------------------------------------------------------

CREATE_IDENTITY_BUTTON = "create-identity-button"
IDENTITY_CONTINUE_BUTTON = "identity-continue-button"
IDENTITY_CREATED_BACK_BUTTON = "identity-created-back-button"
RECOVERY_KIT_SKIP_BUTTON = "recovery-kit-skip-button"
HANDLE_ENTRY_BACK_BUTTON = "handle-entry-back-button"


@pytest.mark.feature("multiple-accounts")
def test_web_switching_to_an_identity_the_device_cannot_sign_in_as_is_refused(
    nest_instance, spa_url
):
    """Switching to an identity this browser can no longer sign in as is
    refused, says so, and leaves the user on the identity they were using
    (`long-term-store.md` § Multi-account evolution, "Activating refuses an
    account it cannot launch as").

    The seed leaves the admin listed with no `fauna/<actor>/secret` slot. The
    wasm activation refuses before the reload, and rejects with the SHARED line
    (`fauna_client_accounts::switch_refused_copy`) that the Account page paints
    on `error-message`; no reload happens, and the session and the persisted
    active pointer are both still the user's. The absence of a reload is
    anchored causally (`assert_no_relaunch`): the refusal line is the
    handler's own completion observable."""
    from i18n.strings import S

    seed, user_actor, user_secret, admin_actor, _admin_secret = _seed_two_accounts(
        nest_instance, spa_url
    )
    seed.pop(f"fauna/{admin_actor}/secret")

    driver = create_driver("web")
    driver.launch({"url": spa_url + "/app/"})
    try:
        _auth_on_account_page(driver, seed, user_secret, "user", user_actor, spa_url)
        assert driver.count(SWITCHER_ITEM) == 2, "both seeded accounts must be listed"

        label = driver.get_text(ITEM_HANDLE, index=1)
        expected = S.settings.switch_refused_no_secret(account=label)
        assert_no_relaunch(
            driver,
            lambda: driver.click(SWITCHER_ITEM, index=1),
            lambda: driver.is_visible("error-message")
            and driver.get_text("error-message") == expected,
            what="switching to an identity with no secret in this browser",
        )
        assert driver.get_state("session.actor_id") == user_actor, (
            "a refused switch must leave the user where they were"
        )
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
def test_web_abandoned_created_identity_never_shows_among_your_identities(
    nest_instance, spa_url
):
    """A fresh browser creates identity A, walks back out of it, and onboards a
    DIFFERENT identity B instead: the switcher lists exactly one identity — B —
    and A never shows up among your identities (`long-term-store.md`
    § Multi-account evolution; the retirement is shared Rust,
    `AccountRegistry::retire_superseded_provisionals`, which web reaches
    through wasm).

    Web's eager boot migration is gone (retired 2026-09-24), so the one
    producer left is moment 1 registering A at `identity-continue-button`; this
    witnesses that B's commit retires it. B arrives by import, never a second
    create. tui's twin:
    `test_tui_abandoned_created_identity_never_shows_among_your_identities`."""
    admin_sk = nest_instance["admin"]["signing_key"]
    second = create_actor_and_register(nest_instance["port"], admin_signing_key=admin_sk)
    second_actor = second["actor_id_hex"]
    second_secret = bytes(second["signing_key"]).hex()

    driver = create_driver("web")
    driver.launch({"url": spa_url + "/app/"})
    try:
        # (0) The dial override, installed at identity_choice where web's
        # reload-based install is idempotent (see the append journey above).
        driver.wait_for(CREATE_IDENTITY_BUTTON, timeout=45)
        driver.set_provider_base_urls({"nest": nest_instance["url"]})

        # (1) Fresh browser → create A → Continue: moment 1 writes A's row.
        driver.wait_for(CREATE_IDENTITY_BUTTON, timeout=45)
        driver.click(CREATE_IDENTITY_BUTTON)
        driver.wait_for(IDENTITY_CONTINUE_BUTTON, timeout=15)
        driver.click(IDENTITY_CONTINUE_BUTTON)
        driver.wait_for(RECOVERY_KIT_SKIP_BUTTON, timeout=20)
        provisional = _wait_registry_count(driver, 1, timeout=20)
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
        # retire A.
        driver.click(IMPORT_IDENTITY_BUTTON)
        driver.wait_for(PASTE_SECRET_FIELD, timeout=15)
        driver.clear_and_type(PASTE_SECRET_FIELD, second_secret)
        driver.click(IMPORT_SUBMIT_BUTTON)
        driver.wait_for(HANDLE_INPUT, timeout=15)
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
        driver.set_state({**ACCOUNT_PAGE_NAV})
        driver.wait_for(SWITCHER_ITEM, timeout=30)
        index = _read_registry_index(driver)
        actor_ids = [a["actor_id"] for a in (index or {}).get("accounts", [])]
        assert actor_ids == [second_actor], (
            f"the abandoned identity {abandoned} still shows among your identities; "
            f"index={index!r}"
        )
        assert index["active"] == second_actor
        assert driver.count(SWITCHER_ITEM) == 1, (
            "the switcher must list exactly the one identity the user onboarded"
        )
    finally:
        driver.teardown()


# ── Device settings stay; identity-tied choices follow the identity ──────────
# (multiple-accounts outcome 10 — `account-scoping.md` § Serialized switching
# and § The scoping taxonomy, class 2 vs class 1)

GENERAL_PAGE_NAV = {
    "nav": {"stack": [{"view": "settings"}, {"view": "settings", "id": "general"}]}
}
PUSH_TOGGLE = "push-notifications-opt-in-toggle"
PUSH_SECTION = "push-notifications-section"
COMPOSE_FIELD = "compose-text-field"


@pytest.mark.feature("multiple-accounts")
def test_web_switching_identity_keeps_device_settings_and_moves_drafts(nest_instance, spa_url):
    """Settings that describe this device stay as they are across a switch,
    while a draft — a choice tied to an identity — follows its identity. The
    web twin of `test_tui_switching_identity_keeps_device_settings_and_moves_drafts`.

    Web's device setting is the push opt-in toggle (General): the install's
    own bit, kept in this browser's `localStorage` under no actor scope
    (`account-scoping.md` class 2), so no switch or erase touches it. The
    browser's push service is stood in for (headless Chromium has none —
    `test_push_settings.py`'s web section says why); the bit the toggle
    renders is the real one. The identity-tied half is the feed composer's
    draft, which rests in that identity's own sealed `__drafts` plane.

    The arc: as the user, switch push on and leave a draft (confirmed on the
    nest before switching — a switch mid-debounce would test the debounce,
    not the scoping); switch to the admin → push still on, and the composer
    holds NOT the user's draft; switch back → the user's draft is there again
    and push still on. Every wait is on state (point 14); the drafts read is
    the sanctioned side-channel verification (convention 8's carve-out), the
    mutations are all UI."""
    import os

    from actions.feed import FeedActions
    from clients.ws_rpc_admin_client import WsRpcAdminClient
    from helpers.budgets import RPC_ROUNDTRIP_S, UI_SETTLE_S
    from tests.test_push_settings import WEB_PUSH_STAND_IN

    seed, user_actor, user_secret, admin_actor, _admin_secret = _seed_two_accounts(
        nest_instance, spa_url
    )
    draft = f"half-written as the user {os.getpid()}"

    def user_drafts_blob():
        with WsRpcAdminClient(
            nest_instance["url"], actor_id=bytes.fromhex(user_actor),
            signing_key=bytes.fromhex(user_secret),
        ) as dev:
            return dev.call("fauna.drafts.get", {"path": "posts"}).get("blob")

    driver = create_driver("web")
    driver.launch({"url": spa_url + "/app/", "push_service_stand_in": WEB_PUSH_STAND_IN})
    feed = FeedActions(driver)

    def push_state():
        driver.set_state(GENERAL_PAGE_NAV)
        driver.wait_for(PUSH_SECTION, timeout=15)
        seen = []

        def painted():
            seen.append(driver.get_attr(PUSH_TOGGLE, "state"))
            return seen[-1] in ("on", "off")

        wait_until(painted, UI_SETTLE_S, diagnose=lambda: f"toggle state {seen[-1:]!r}")
        return seen[-1]

    def switch_to(index, actor):
        driver.set_state(ACCOUNT_PAGE_NAV)
        driver.wait_for(SWITCHER_ITEM, timeout=30)
        driver.click(SWITCHER_ITEM, index=index)
        _wait_session_actor(driver, actor, timeout=60)

    def composer_body():
        feed.navigate()
        feed.open_composer()
        return feed.compose_body_text()

    try:
        _auth_on_account_page(driver, seed, user_secret, "user", user_actor, spa_url)

        # (1) As the user: a device choice and a draft.
        assert push_state() == "off", "precondition: a fresh install is never opted in"
        driver.click(PUSH_TOGGLE)
        wait_until(
            lambda: driver.get_attr(PUSH_TOGGLE, "state") == "on", UI_SETTLE_S,
            diagnose=lambda: f"push toggle reads {driver.get_attr(PUSH_TOGGLE, 'state')!r}",
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
            diagnose=lambda: "the user's draft never reached its __drafts plane",
        )

        # (2) Switch to the admin.
        switch_to(1, admin_actor)
        assert push_state() == "on", (
            "a setting that describes this device must survive the switch"
        )
        assert composer_body() != draft, (
            "the user's draft must not follow the device to another identity"
        )

        # (3) Switch back: the user's draft is theirs again, the device choice
        # is still the one made.
        switch_to(0, user_actor)
        feed.navigate()
        feed.open_composer()
        wait_until(
            lambda: feed.compose_body_text() == draft, RPC_ROUNDTRIP_S,
            diagnose=lambda: f"the user's composer reads {feed.compose_body_text()!r}",
        )
        assert push_state() == "on", "the device setting must still hold after switching back"
    finally:
        driver.teardown()
