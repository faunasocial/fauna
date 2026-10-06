"""tier_3 e2e: fauna-tui multi-account account switcher.

Multi-account clients let one install (one OS-user context) hold several Fauna
identities and switch between them (`docs/goal/architecture/long-term-store.md`
§ Multi-account evolution). This is the tui leg — the seventh client adopting the
switcher + the Stage-2 in-app re-auth prompt (`apps/tui.md` § Account
switcher). tui uses the SAME file-backed `CredentialStore` as linux and, having
no native OS re-auth prompt, the SAME in-app `account-activate-reauth-prompt`
shape linux/web ratified — so this ports the linux reference near-verbatim.

Only the driver-call *form* differs from linux where tui's scope resolution
requires it: a row is switched by its GLOBAL occurrence (`click(SWITCHER_ITEM,
index=i)`, as the first linux journey already does) rather than a self-scoped
`scope=SWITCHER_ITEM[i]`, because tui's row anchor is a top-level element and a
scoped query matches only its `within(...)`-nested children (the toggle). The
element IDs and the user actions are identical across clients (priority #1/#3).

The append-mode "Add account" journeys (`account-add-button`) run the onboarding
wizard OVER the live session via tui's `adding_account` surface mode — the
immediate-mode analog of linux's separate append window and apple's sheet
(long-term-store.md § Multi-account evolution). Two divergences from linux, both
architectural, not cosmetic: (1) tui's abandon test rides the file-backed store
pinned across a relaunch (`preserve_state_across_relaunch()`) rather than linux's
`use_real_keyring` — cross-platform, so it runs on macOS with no Secret Service;
(2) an append moves the active pointer only at its terminal — the confirm
writes nothing, and a provisioning run's custody mint registers the appended
identity INACTIVE (`onboarding.md` § Multi-account; the shared writer never
activates mid-run, `test_add_account_provisioning.py`) — so an abandoned append
cannot pollute the routing — the abandon journey therefore asserts prior-identity
survival via the registry index + routing (tui's actual launch-routing source),
a *stronger* property than linux's retired index-gated legacy-slot heal.

tier_3: needs a real ``fauna-nest`` binary (``nest_instance``); tui driver only.
"""
from __future__ import annotations

import os
import sqlite3
import time
from types import SimpleNamespace

import pytest

from common import build_registry_seed, create_actor_and_register
from common.scope_store import XdgScopeStore
from conftest import _seeded_environment
from drivers import create_driver
from drivers.machine_test_setter import set_handle_check_snapshot
from helpers.budgets import APP_RELAUNCH_S, RPC_ROUNDTRIP_S, UI_SETTLE_S
from helpers.registry_store import read_registry_index, read_store_slot
from helpers.waiting import (
    assert_no_relaunch,
    await_session_actor,
    wait_registry_index,
    wait_until,
)
from i18n.strings import S

pytestmark = [pytest.mark.tier_3, pytest.mark.tui]

# tests/e2e-unified/ui.yaml § settings (switcher) + navigation (admin-tab).
SWITCHER_LIST = "account-switcher-list"
SWITCHER_ITEM = "account-switcher-item"
ITEM_HANDLE = "account-item-handle"
ACTIVE_INDICATOR = "account-item-active-indicator"
REMOVE_BUTTON = "account-remove-button"
ADMIN_TAB = "admin-tab"

# Stage 2 (re-auth-on-activate). tui has no native OS re-auth prompt, so it
# renders the in-app confirm surface — the shape linux ratified and web + tui
# adopt (long-term-store.md § Multi-account evolution → Per-account re-auth).
# There is deliberately NO `reauth-result` file seam (that is apple/android,
# whose OS sheet carries no test ID): this prompt is in-app and drivable, so the
# journeys click the real UI a user clicks (testing.md point 8).
REQUIRE_CONFIRM_TOGGLE = "account-require-confirm-toggle"
REAUTH_PROMPT = "account-activate-reauth-prompt"
REAUTH_CONFIRM_BUTTON = "account-activate-reauth-confirm-button"
REAUTH_CANCEL_BUTTON = "account-activate-reauth-cancel-button"

# tests/e2e-unified/ui.yaml § settings (`account-add-button`) + § onboarding — the
# append-mode "Add account" wizard (the SAME shared onboarding element IDs every
# app renders, priority #1/#3).
ADD_BUTTON = "account-add-button"
IMPORT_IDENTITY_BUTTON = "import-identity-button"
PASTE_SECRET_FIELD = "paste-secret-field"
IMPORT_SUBMIT_BUTTON = "import-submit-button"
HANDLE_INPUT = "handle-input"
HANDLE_CONTINUE_BUTTON = "handle-entry-continue-button"

# Two-element nav to the Account sub-page of the Settings shell; mirrors the
# mail/admin actions (`settings.md` § Navigation model).
ACCOUNT_PAGE_NAV = {
    "nav": {"stack": [{"view": "settings"}, {"view": "settings", "id": "account"}]}
}


def _seed_two_accounts(nest_instance, active="user"):
    """A claimed-admin account + a freshly-registered regular-user account, seeded
    into the registry active on the regular user (``active="admin"`` flips it, which
    the admin auto-default journey needs). Returns (seed_map, user_actor_id,
    admin_actor_id). The admin is `nest_instance`'s own claimed identity (so
    `am-i-admin` is true for it); the user is registered via the admin key.

    ``build_registry_seed`` writes the exact logical-key layout the
    ``CredentialStore`` File backend reads verbatim (the `fauna/index` blob +
    per-actor `fauna/{actor}/{secret,nest_url,device_id}` slots) — no app code."""
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


def _seed_one_account(nest_instance, active_admin=False):
    """A SINGLE registered account, seeded active — the pre-append single-identity
    state a first-run install lands in. ``active_admin=True`` makes it the nest's own
    claimed ADMIN (so `am-i-admin` → `admin-tab` is a clean "routed to #1"
    discriminator, which the abandon-recovery journey needs). Returns (seed_map,
    actor_id)."""
    admin_sk = nest_instance["admin"]["signing_key"]
    if active_admin:
        actor = bytes(admin_sk.verify_key).hex()
        secret = bytes(admin_sk).hex()
        handle = "admin"
    else:
        user = create_actor_and_register(nest_instance["port"], admin_signing_key=admin_sk)
        actor = user["actor_id_hex"]
        secret = bytes(user["signing_key"]).hex()
        handle = "user"
    seed = build_registry_seed(
        [{"actor_id": actor, "secret_hex": secret, "nest_url": nest_instance["url"],
          "device_id": "add-account-seed", "handle": handle}],
        active=actor,
    )
    return seed, actor


def _wait_switcher_count(driver, n, timeout=30):
    """Poll the registry-backed switcher UI until it lists exactly ``n`` accounts
    with one active. A switch tears the shell down and re-launches (dropping the
    Account sub-page), so re-navigate each cycle. The switcher reads the local
    ``AccountRegistry`` (client-side), NOT the nest, so it asserts the registry
    state reconnect-independently — the tui analog of web's
    ``localStorage['fauna/index']`` read."""
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
    until it lists exactly ``n`` items. This asserts a *live* in-place refresh:
    `_wait_switcher_count` re-navigates each cycle (which would rebuild from the
    now-shrunken registry and pass even with no live update). Removing a non-active
    account does not switch (no teardown/re-launch), so the row can only vanish
    without navigating via tui's in-place `refresh_accounts` after the mutation."""
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
    file-backed ``CredentialStore`` writes verbatim — the tui analog of web's
    ``localStorage['fauna/index']``. Reading the store file directly asserts a
    mutation's effect independent of the post-switch reconnect. The file is the
    same per-app store the driver seeded (drivers/tui.py `seed_credentials` →
    ``{credential_dir}/{FAUNA_KEYRING_APP}.json``).

    Reads from the driver's *resolved* store dir + keyring app, so it is correct
    both for the default per-launch config (``{tmp}/creds/fauna-e2e-agent-{port}
    .json``) AND for a pinned store that survives a relaunch (the abandon journey's
    ``preserve_state_across_relaunch()``, whose dirs differ from the fresh
    per-launch ``_tmp_dir``). Returns the index dict, or None. The read itself
    is the shared ``helpers/registry_store.py`` (one reader for every
    file-backed store; this wrapper keeps the module's vocabulary)."""
    return read_registry_index(driver)


def _wait_registry_index(driver, n, timeout=30, *, active=None):
    """Poll this app's registry file until it lists exactly ``n`` accounts (and,
    given ``active``, names it active); return it. Shared loop:
    ``helpers.waiting.wait_registry_index``."""
    return wait_registry_index(
        lambda: _read_registry_index(driver), n, active=active, budget_s=timeout
    )


def _read_store_slot(driver, key):
    """One raw logical slot out of the file-backed ``CredentialStore``.

    The sibling of :func:`_read_registry_index`, which reads the ``fauna/index``
    blob: the per-actor three-slot contract (``fauna/{actor}/{secret,nest_url,
    device_id}`` — ``long-term-store.md`` § The three slots) lives in slots of its
    own, so the index alone cannot answer what URL an account will dial on its
    next launch. Returns the raw string, or None (``helpers/registry_store.py``)."""
    return read_store_slot(driver, key)


def _remap_account_page(driver):
    """Leave the Account sub-page and come back, so its nav-edge refresh fires.

    tui reads the registry into a render snapshot at the nav edge
    (`settings::refresh_accounts`), NOT per frame, so a flag the admin auto-default
    writes AFTER the last visit only appears once the page is re-entered — exactly
    what a user does to see it. The detour via Status guarantees a fresh entry (and
    is what linux's build-once page needs; tui refreshes on every entry regardless,
    so this is uniform-and-harmless)."""
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
def test_tui_account_switcher_lists_switches_and_reveals_admin(
    nest_instance, tui_app_path, request
):
    seed, _user_actor, _admin_actor = _seed_two_accounts(nest_instance)

    driver = create_driver("tui")
    driver.launch({
        "app_path": tui_app_path,
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

        # (3) Account settings lists BOTH accounts, the active one marked.
        _wait_switcher_count(driver, 2, timeout=30)

        # (4) Tap the admin row (occurrence 1) → tear-down + re-launch as the admin
        # → the admin shell appears. Global-index click, not a self-scope: tui's
        # row anchor is top-level, so a `scope=SWITCHER_ITEM[1]` query matches only
        # its nested children (the toggle), never the anchor.
        driver.click(SWITCHER_ITEM, index=1)
        driver.wait_for(ADMIN_TAB, timeout=45)
        assert driver.is_visible(ADMIN_TAB), (
            "switching to the admin identity must reveal the admin shell"
        )
    finally:
        driver.teardown()


@pytest.mark.feature("multiple-accounts")
def test_tui_remove_account_shrinks_switcher_live(nest_instance, tui_app_path, request):
    """Removing a non-active account LIVE-refreshes the switcher in place: the row
    disappears with NO re-navigation (tui's `refresh_accounts` after the mutation).
    A `remove` never switches, so it never tears the shell down — the only way the
    row can vanish without navigating is the in-place refresh."""
    seed, _user_actor, admin_actor = _seed_two_accounts(nest_instance)

    driver = create_driver("tui")
    driver.launch({
        "app_path": tui_app_path,
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

        # LIVE: the removed row vanishes from the SAME page (no re-navigation), and
        # the surviving (regular) account stays active.
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


@pytest.mark.feature("multiple-accounts")
def test_tui_require_confirm_gates_switch_decline_then_approve(
    nest_instance, tui_app_path, request
):
    """Stage 2 (`long-term-store.md` § Per-account re-auth): flagging an account via
    its `account-require-confirm-toggle` makes activating it demand a re-auth
    confirmation first. tui has no native OS prompt, so it renders the in-app
    `account-activate-reauth-prompt`. Declining is a PURE NO-OP — registry
    untouched, no teardown, the current account stays active — and approving
    completes the same switch the unflagged path takes. Both the flag write and the
    confirm are driven through the UI (testing.md point 8) and asserted against the
    persisted registry index."""
    seed, user_actor, admin_actor = _seed_two_accounts(nest_instance)

    driver = create_driver("tui")
    driver.launch({
        "app_path": tui_app_path,
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
        # The toggle IS a `within(account-switcher-item, 1)` child, so it resolves
        # by scope (unlike the row anchor). Setting the flag never prompts.
        driver.click(REQUIRE_CONFIRM_TOGGLE, scope=f"{SWITCHER_ITEM}[1]")
        flagged = _wait_flag(driver, admin_actor, True, timeout=10)
        assert flagged.get(user_actor) is False, (
            f"the row toggle must flag only the tapped account; got {flagged!r}"
        )

        # (2) DECLINE: activating the flagged row raises the in-app prompt, and
        # cancelling it must be a PURE no-op — no registry mutation, no teardown,
        # the regular user stays authenticated and the switcher page stays rendered.
        driver.click(SWITCHER_ITEM, index=1)
        driver.wait_for(REAUTH_PROMPT, timeout=15)
        assert driver.is_visible(REAUTH_PROMPT), (
            "activating a flagged account must raise the in-app re-auth prompt "
            "(tui has no native OS prompt to defer to)"
        )
        # Convention 14: the absence is anchored to causal order, not to a
        # window. Declining closes the prompt (the handler's own completion
        # observable), then the barrier drains everything that handler queued —
        # a teardown initiated inside it would already have counted itself.
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

        # (3) APPROVE: the same tap, confirmed, completes the switch (tear-down +
        # re-launch as the admin — exactly the unflagged journey).
        _wait_live_switcher_count(driver, 2, timeout=15)
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
def test_tui_admin_auto_default_flags_admin_and_explicit_off_sticks(
    nest_instance, tui_app_path, request
):
    """The admin auto-default (`long-term-store.md` § Per-account re-auth — "Default
    off; a client turns it on for its admin identity"): launching authenticated as
    the ADMIN account auto-enables its require-confirm flag at tui's am-i-admin
    observation (the `GateLoaded` fold that reveals the admin sidebar row), with NO
    user tap — asserted against the persisted index; the regular account stays
    unflagged. And the user's explicit OFF sticks: turn the toggle off, re-trigger
    the observation by switching away and back (both switches unflagged → no confirm
    involved), and the auto-default must NOT re-flip it — `require_confirm_user_set`
    pins the user's choice."""
    seed, user_actor, admin_actor = _seed_two_accounts(nest_instance, active="admin")

    driver = create_driver("tui")
    driver.launch({
        "app_path": tui_app_path,
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

        # (2) The auto-defaulted flag must also RENDER: re-enter the page (as a user
        # would, once the observation has landed) so the nav-edge refresh picks up
        # the registry flag, and the admin's toggle must read ON. Without the
        # refresh it renders its stale snapshot OFF over a registry that says ON —
        # and then the user could never turn the flag off (a tap on an OFF-looking
        # switch writes ON). That is a real bug this arm pins.
        _remap_account_page(driver)
        assert (
            driver.get_attr(REQUIRE_CONFIRM_TOGGLE, "state", scope=f"{SWITCHER_ITEM}[1]")
            == "on"
        ), (
            "the admin row's toggle must render the auto-defaulted flag once the page "
            "is re-entered; a stale OFF makes the flag impossible to turn off"
        )

        # (3) Explicit user OFF (the admin is the ACTIVE row 1 — its toggle is
        # tappable even on the active row, which is why the toggle renders on every
        # row, not only the non-active branch that hosts account-remove-button).
        driver.click(REQUIRE_CONFIRM_TOGGLE, scope=f"{SWITCHER_ITEM}[1]")
        _wait_flag(driver, admin_actor, False, timeout=10)
        index = _read_registry_index(driver)
        admin_entry = next(a for a in index["accounts"] if a["actor_id"] == admin_actor)
        assert admin_entry.get("require_confirm_user_set") is True, (
            "the toggle write must mark the flag user-set (the OFF-sticks pin); "
            f"got {admin_entry!r}"
        )

        # (4) Re-trigger the observation: switch to the user (admin is unflagged now,
        # the user always was — no confirm anywhere), then back to the admin. Each
        # switch re-launches and re-fires the am-i-admin gate.
        driver.click(SWITCHER_ITEM, index=0)
        driver.wait_for_state(
            lambda s: s.get("session", {}).get("actor_id") == user_actor, timeout=45
        )
        _wait_switcher_count(driver, 2, timeout=30)
        driver.click(SWITCHER_ITEM, index=1)
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


@pytest.mark.feature("multiple-accounts")
def test_tui_add_account_appends_second_identity_to_registry(
    nest_instance, tui_app_path, request
):
    """Append-mode "Add account" (`long-term-store.md` § Multi-account evolution):
    tapping `account-add-button` runs the onboarding wizard OVER the live session
    (tui's `adding_account` surface mode — the immediate-mode analog of linux's
    append window / apple's sheet), imports a second identity, and on the `LoggedIn`
    outcome grows the registry 1→2 and switches to the new account (design Decision
    1, switch-first) with the original preserved (no-user-data-loss). The append is
    driven entirely through the wizard UI a real user clicks (testing.md point 8);
    the handle-check outcome is injected (the shared machine's `AlreadyOnNest` seam,
    exactly as the onboarding suites do) so Continue lands `LoggedIn`.

    **Both halves of the journey are asserted** — the persisted AccountIndex
    (client-side, reconnect-independent) AND that the account it switched to
    reaches a *working session*. The second half was unasserted on all 7 apps
    until 2026-08-14: every append test stopped at the registry row, so "Add
    account → you land in a working session as the new identity" was unproven
    across the apps, and an earlier session lost a whole run diagnosing the
    resulting dead end as a product regression in the windows switch glue.

    What made it unassertable was never the product: the wizard derives the
    incoming account's `nest_url` from the typed handle's domain, which is
    **uniform https** by ratified design (`fauna_provisioning::probe::
    resolve_handle_domain_with_local_port`), so the post-switch launch dialed
    `https://localhost:<port>` at a plain-HTTP tier_3 nest. The sanctioned
    harness answer is the `provider_base_urls["nest"]` override seam — named as
    such in `probe.rs`'s own resolution comment, and since 2026-08-13 mirrored
    into the process-global **store-read** dial (`fauna_launch_machine::dial`),
    which is precisely the leg an "Add account" switch takes. A `serve_tls=True`
    nest is NOT needed (and would have to reconcile the derived `localhost`
    authority against the nest's `127.0.0.1` one)."""
    # (0) A pre-registered SECOND identity to import in the append wizard.
    admin_sk = nest_instance["admin"]["signing_key"]
    second = create_actor_and_register(nest_instance["port"], admin_signing_key=admin_sk)
    second_actor = second["actor_id_hex"]
    second_secret = bytes(second["signing_key"]).hex()

    seed, first_actor = _seed_one_account(nest_instance)

    driver = create_driver("tui")
    driver.launch({
        "app_path": tui_app_path,
        "url": nest_instance["url"],
        "seed_credentials": seed,
        "environment": _seeded_environment(request, nest_instance),
    })
    try:
        # (1) Launched active on the single seeded (regular) account → authenticated,
        # NO admin shell; the switcher lists exactly ONE account before Add account.
        driver.wait_for_state(
            lambda s: bool(s.get("session", {}).get("authenticated")), timeout=30
        )
        assert driver.is_absent(ADMIN_TAB), (
            "a regular (non-admin) active identity must have no admin shell"
        )
        _wait_switcher_count(driver, 1, timeout=30)

        # (2) Add account → the append wizard runs OVER the live session (tui has no
        # separate window: `adding_account` routes the screen to the wizard while the
        # session stays live underneath). It shares the onboarding machine the
        # `call_machine_method` seam drives, so the step-4 injection reaches it.
        driver.click(ADD_BUTTON)
        driver.wait_for(IMPORT_IDENTITY_BUTTON, timeout=30)

        # (3) Import the pre-registered second identity (the paste-secret path).
        driver.click(IMPORT_IDENTITY_BUTTON)
        driver.wait_for(PASTE_SECRET_FIELD, timeout=15)
        driver.clear_and_type(PASTE_SECRET_FIELD, second_secret)
        driver.click(IMPORT_SUBMIT_BUTTON)
        driver.wait_for(HANDLE_INPUT, timeout=15)

        # (4) Install the dial override BEFORE the append completes: the switch it
        # triggers is a launch **from the store**, and the URL sitting there will be
        # the wizard's uniform-https derivation, which this plain-HTTP nest cannot
        # serve. One gesture covers both halves — the machine's HTTP providers and,
        # mirrored, the process-global store-read dial. Measured, not assumed:
        # without this the switch lands `Offline(transient)` and step (6) fails with
        # `session={'authenticated': False}, nav=[welcome]`.
        #
        # No teardown clear is needed (unlike the session-cached-`app` suites): this
        # driver owns its own tui process and kills it below, and the override is
        # in-process state that dies with it.
        driver.set_provider_base_urls({"nest": nest_instance["url"]})

        # (5) Inject the AlreadyOnNest (welcome-back) handle-check outcome so Continue
        # lands `WizardOutcome::LoggedIn` — the append trigger (`add_account` +
        # switch). The real silent-challenge → AlreadyOnNest path is covered by the
        # onboarding suites; here we drive the NOVEL append branch that fires on it.
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
        # account, with the original preserved.
        index = _wait_registry_index(driver, 2, timeout=30, active=second_actor)
        actor_ids = [a["actor_id"] for a in index["accounts"]]
        assert first_actor in actor_ids, (
            "the original account must survive the append (no-user-data-loss)"
        )
        assert second_actor in actor_ids, "the imported account must be appended"
        assert index["active"] == second_actor, (
            "the append switches to the newly-added account (design Decision 1)"
        )

        # (7) …and that account reaches a WORKING session, not just a registry
        # row. This is the half the append tests never asserted; it is the whole
        # point of "Add account".
        await_session_actor(
            driver, second_actor, budget_s=APP_RELAUNCH_S, what="the append"
        )

        # (8) The seam redirects the SOCKET, never the truth: what the append
        # persisted for the new account is still the literal derived from the
        # typed handle domain — an https URL this plain-HTTP nest never served.
        # Asserting the store is what keeps (7) honest: an override that leaked
        # into persistence would make every later launch, in a release build
        # with no override installed, dial a torn-down fixture
        # (`fauna_launch_machine::dial` — "It redirects the socket, never the
        # truth"; the shared-Rust twin is `dial_override_never_reaches_the_store`).
        stored_url = _read_store_slot(driver, f"fauna/{second_actor}/nest_url")
        assert stored_url == f"https://localhost:{nest_instance['port']}", (
            "the appended account must persist the URL the wizard derived from "
            "the typed handle domain, NOT the harness dial override; got "
            f"{stored_url!r}"
        )
    finally:
        driver.teardown()


@pytest.mark.feature("multiple-accounts")
def test_tui_add_account_abandon_recovers_prior_identity_on_relaunch(
    nest_instance, tui_app_path, tmp_path, request
):
    """Abandoning an append-mode "Add account" mid-wizard, then force-quitting and
    relaunching, must recover the prior (active-registry) identity. The append's
    confirm writes NOTHING (the shared `persist_confirmed_identity` append arm,
    2026-09-24), and nothing before its terminal ever moves the active pointer
    (a provisioning run's custody mint registers the appended identity inactive —
    `test_add_account_provisioning.py` covers that later write). So an abandoned
    append before any run cannot touch the store at all: the registry stays
    unchanged (one account, #1 active) and the relaunch routes to #1 by reading
    its intact per-actor slot — asserted via the registry index + `admin-tab`
    routing (linux's legacy-slot heal this once contrasted with is retired).

    Rides the file-backed store pinned across a genuine process relaunch
    (`preserve_state_across_relaunch()`) — cross-platform, so this runs on macOS with
    no Secret Service (linux's `use_real_keyring` is Linux-desktop-only). Account #1
    is the nest's own claimed ADMIN, so `admin-tab` is a clean "routed to the active
    #1" discriminator: had an abandoned #2 (a regular user) shadowed it, the shell
    would carry no admin-tab. The append is driven through the switcher UI a real
    user clicks (testing.md point 8).

    `docs/goal/architecture/long-term-store.md` § Multi-account evolution
    (abandoned-append recovery) + `apps/tui.md` § Implementation status."""
    # (0) Account #1 = the nest's own claimed ADMIN; a regular SECOND identity to
    # import-then-ABANDON.
    admin_sk = nest_instance["admin"]["signing_key"]
    admin_actor = bytes(admin_sk.verify_key).hex()
    second = create_actor_and_register(nest_instance["port"], admin_signing_key=admin_sk)
    second_secret = bytes(second["signing_key"]).hex()

    seed, seeded_actor = _seed_one_account(nest_instance, active_admin=True)
    assert seeded_actor == admin_actor

    # Pin the file store under the pytest tmp dir so it survives the force-quit +
    # relaunch (the default per-launch dirs are thrown away on relaunch).
    config = {
        "app_path": tui_app_path,
        "url": nest_instance["url"],
        "seed_credentials": seed,
        "xdg_base": str(tmp_path / "xdg"),
        "credential_dir": str(tmp_path / "creds"),
        "keyring_app": "add-account-abandon",
        "environment": _seeded_environment(request, nest_instance),
    }
    driver = create_driver("tui")
    driver.launch(config)
    try:
        # (1) Launch #1 as the admin → admin-tab confirms the active identity.
        driver.wait_for_state(
            lambda s: bool(s.get("session", {}).get("authenticated")), timeout=30
        )
        driver.wait_for(ADMIN_TAB, timeout=45)
        assert driver.is_visible(ADMIN_TAB), (
            "launch #1 must authenticate as the admin identity (admin-tab present)"
        )
        _wait_switcher_count(driver, 1, timeout=30)

        # (2) APPEND via the UI: Add account → import the second identity → reach the
        # handle step. NO Continue → the append is never committed.
        driver.click(ADD_BUTTON)
        driver.wait_for(IMPORT_IDENTITY_BUTTON, timeout=30)
        driver.click(IMPORT_IDENTITY_BUTTON)
        driver.wait_for(PASTE_SECRET_FIELD, timeout=15)
        driver.clear_and_type(PASTE_SECRET_FIELD, second_secret)
        driver.click(IMPORT_SUBMIT_BUTTON)
        driver.wait_for(HANDLE_INPUT, timeout=15)

        # tui's wizard persists nothing, so the registry is untouched (one account,
        # admin active) — no `add_account` until LoggedIn, and no legacy-slot
        # pollution is even possible (the linux-specific hazard is structurally absent).
        idx = _read_registry_index(driver)
        assert idx is not None and len(idx["accounts"]) == 1 and idx["active"] == admin_actor, (
            f"abandoning before Continue must NOT append to the registry; got {idx!r}"
        )

        # (3) ABANDON: pin the store, force-quit mid-wizard (kills the process group,
        # no Continue), relaunch reading the SAME pinned store (no re-seed).
        assert driver.preserve_state_across_relaunch(), (
            "the tui driver must be able to pin its file store across a relaunch"
        )
        driver.teardown()
        relaunch = dict(driver._launch_config)
        relaunch.pop("seed_credentials", None)
        driver.launch(relaunch)

        # (4) The relaunch routes to the ACTIVE admin (admin-tab), NOT the abandoned
        # #2 — the store was never polluted and #1's per-actor slot is intact.
        driver.wait_for_state(
            lambda s: bool(s.get("session", {}).get("authenticated")), timeout=30
        )
        driver.wait_for(ADMIN_TAB, timeout=45)
        assert driver.is_visible(ADMIN_TAB), (
            "the relaunch must route to the ACTIVE admin (admin-tab), not the "
            "abandoned #2 — tui's abandoned-append recovery"
        )
        idx2 = _read_registry_index(driver)
        assert len(idx2["accounts"]) == 1 and idx2["active"] == admin_actor, (
            f"an abandoned append leaves the registry unchanged (one admin account); got {idx2!r}"
        )
    finally:
        driver.teardown()


# ── Account-scoping isolation (docs/goal/architecture/apps/account-scoping.md
# § Serialized switching) — tui's gap-ledger row ──────────────────────────────


def _config_fauna_tui_dir(driver):
    """The tui app's per-app config dir this launch resolved
    (`session::config_dir()` == `$XDG_CONFIG_HOME/fauna-tui`) — the base
    `account_scope` scopes `mls_state.db` under. Delegates to the shared
    `common.scope_store.XdgScopeStore` rather
    than rebuilding the path — the same resolver `test_sign_out.py`'s
    directory-erase test drives."""
    return str(XdgScopeStore("tui", driver.config_home, "fauna-tui").scope_roots()[0])


def _wait_for_path(path, timeout=15):
    """Poll until `path` exists — the MLS engine opens its db file
    synchronously inside `session::establish` (no async lag before
    "authenticated" reports), so this is a short bounded settle window, not a
    real wait for a slow background op."""
    deadline = time.monotonic() + timeout
    while time.monotonic() < deadline:
        if os.path.exists(path):
            return
        time.sleep(0.2)
    raise AssertionError(f"{path} never appeared within {timeout}s")


@pytest.mark.feature("multiple-accounts")
def test_tui_account_switch_scopes_mls_state_per_actor(nest_instance, tui_app_path, request):
    """The isolation contract (`account-scoping.md` § Serialized switching):
    each account's MLS state lives under its OWN `<actor-id-hex>/mls_state.db`,
    never a single shared flat file both accounts could clobber (the
    cross-account leak this scoping closes). Seeds two accounts, launches
    active on the regular user, switches to the admin, and asserts both scoped
    directories land their own file — and that nothing ever lands at the old
    shared flat path."""
    seed, user_actor, admin_actor = _seed_two_accounts(nest_instance)

    driver = create_driver("tui")
    driver.launch({
        "app_path": tui_app_path,
        "url": nest_instance["url"],
        "seed_credentials": seed,
        "environment": _seeded_environment(request, nest_instance),
    })
    try:
        driver.wait_for_state(
            lambda s: bool(s.get("session", {}).get("authenticated")), timeout=30
        )
        _wait_switcher_count(driver, 2, timeout=30)

        base = _config_fauna_tui_dir(driver)
        user_db = os.path.join(base, user_actor, "mls_state.db")
        _wait_for_path(user_db, timeout=15)

        # Switch to the admin identity — a fresh MlsEngine opens under ITS OWN
        # scoped dir, never touching the user's.
        driver.click(SWITCHER_ITEM, index=1)
        driver.wait_for(ADMIN_TAB, timeout=45)

        admin_db = os.path.join(base, admin_actor, "mls_state.db")
        _wait_for_path(admin_db, timeout=15)

        assert user_db != admin_db, "the two accounts must not share one db path"
        assert os.path.exists(user_db), (
            "switching away must not delete the outgoing account's scoped state "
            "(a plain switch preserves, never erases)"
        )
        assert not os.path.exists(os.path.join(base, "mls_state.db")), (
            "no account's MLS state may land at the old shared flat base — "
            "each account must be isolated under its own scoped dir"
        )
    finally:
        driver.teardown()


def test_tui_account_scoping_never_adopts_a_flat_mls_db(
    nest_instance, tui_app_path, tmp_path, request
):
    """A flat `mls_state.db` at tui's base is never adopted into the account that
    logs in: the pre-scoping first-adopter hand-off was removed by the
    compat-remnant sweep (`version-compatibility.md` § Dimension 2, the fourth
    ratified exception), and this is its end-to-end refusal pin — the windows
    twin is `test_account_switcher_windows.py`'s isolation leg. The scoped store
    the app opens carries none of the flat file's content, and the flat file is
    left exactly as it was (no first-adopter marker is ever written beside it)."""
    seed, actor = _seed_one_account(nest_instance)

    xdg_base = str(tmp_path / "xdg")
    fauna_tui_dir = os.path.join(xdg_base, "config", "fauna-tui")
    os.makedirs(fauna_tui_dir, exist_ok=True)
    flat_db = os.path.join(fauna_tui_dir, "mls_state.db")
    # A real SQLite file with a marker TABLE, so "not adopted" is a content
    # assertion rather than an absence the scoped store could satisfy by luck.
    conn = sqlite3.connect(flat_db)
    conn.execute("CREATE TABLE adoption_marker (v TEXT)")
    conn.execute("INSERT INTO adoption_marker VALUES ('flat-marker')")
    conn.commit()
    conn.close()

    driver = create_driver("tui")
    driver.launch({
        "app_path": tui_app_path,
        "url": nest_instance["url"],
        "seed_credentials": seed,
        "xdg_base": xdg_base,
        "environment": _seeded_environment(request, nest_instance),
    })
    try:
        driver.wait_for_state(
            lambda s: bool(s.get("session", {}).get("authenticated")), timeout=30
        )
        scoped_db = os.path.join(fauna_tui_dir, actor, "mls_state.db")
        _wait_for_path(scoped_db, timeout=15)

        conn = sqlite3.connect(scoped_db)
        tables = {
            row[0]
            for row in conn.execute("SELECT name FROM sqlite_master WHERE type = 'table'")
        }
        conn.close()
        assert "adoption_marker" not in tables, (
            "the flat mls_state.db must never be adopted into the account's "
            f"scoped store; its tables: {sorted(tables)!r}"
        )
        assert os.path.exists(flat_db), "the flat file is not the app's to touch"
        assert not os.path.exists(os.path.join(fauna_tui_dir, "state-owner")), (
            "no first-adopter marker is ever written"
        )
    finally:
        driver.teardown()


# ── The in-memory half of the isolation contract, on tui ─────────────────────
# (account-scoping.md § Implementation status → Isolation-contract gap ledger,
# the `tui (in-memory)` row)


def _await_working_session(driver, actor_id, what):
    """Authenticated as ``actor_id`` AND its conversations rail is LIVE.

    ``session.actor_id`` alone says the launch machine reached ``Online``; what
    the in-memory contract adds is that the incoming account's own MLS engine
    opened. ``data.conv_real_backend_active`` is true only once the login-built
    ``ConversationsSession`` exists, which the engine-role refusal ("served in
    another instance of this app") never reaches — ``conv_backend::
    start_with_db`` returns before building one. The refusal paints only
    through the conversations page's error line, so the positive reading is the
    honest assert, and the mechanism rides in the diagnosis.
    """

    def _served() -> bool:
        s = driver.get_state() or {}
        return (
            s.get("session", {}).get("actor_id") == actor_id
            and s.get("data", {}).get("conv_real_backend_active") is True
        )

    def _diagnose() -> str:
        s = driver.get_state() or {}
        # The app's own account of the rail: the engine-role refusal and every
        # other reason the session stays unwired log under `conv-backend`
        # (convention 6 — the failure carries its cause).
        rail_lines = [
            line
            for line in driver.app_stderr_text().splitlines()
            if any(
                tag in line
                for tag in ("conv-backend", "served in another", "mls-sync", "harvest")
            )
        ]
        return (
            f"after {what}: session={s.get('session')!r} "
            f"conv_real_backend_active="
            f"{s.get('data', {}).get('conv_real_backend_active')!r}. A session that "
            "is authenticated but never brings its conversations rail up is the "
            "engine-role refusal: the OUTGOING account's engine — and the file lock "
            "on its store — is still alive in this process. App log, rail lines: "
            + " | ".join(rail_lines[-6:])
        )

    wait_until(_served, APP_RELAUNCH_S, diagnose=_diagnose)


@pytest.mark.feature("multiple-accounts")
def test_tui_switching_back_to_an_account_serves_its_conversations_again(
    nest_instance, tui_app_path, request
):
    """Switching A → B → A gives A a WORKING conversations rail again — the
    in-memory half of the isolation contract, on the surface where it bites
    hardest.

    `test_tui_account_switch_scopes_mls_state_per_actor` above proves the
    PERSISTED half (each account gets its own `mls_state.db`) and switches one
    way only — which is exactly why the in-memory defect was invisible there.
    Measured 2026-08-27: the outgoing account's MLS engine, and with it the
    one-engine-per-store file lock on its store, stayed alive in the process
    after the switch — a strong `Arc` cycle through the in-group succession
    witness (manager → FaunaMls backend → witness → anchors → manager), plus
    three background loops that held the engine until their next tick. Coming
    BACK, the incoming session found its own store held by its own zombie and
    degraded to the ratified "served in another instance of this app" refusal,
    with no other instance anywhere. The same hold is what made the
    post-succession sweep retry refuse the ceremony's own device
    (`test_succession_sweep_retry.py`). The mechanisms are pinned at tier_1
    (`fauna-client-recovery/tests/witness.rs`, the ownership pin;
    `fauna-conversations/tests/receive_cycle_poke_tests.rs`, the release pin);
    this is the one journey that proves the user-visible whole.

    Each leg asserts a WORKING session (rail up), not just a registry row or an
    actor id — the first two legs so that a red on the third is unambiguous.
    """
    seed, user_actor, admin_actor = _seed_two_accounts(nest_instance)

    driver = create_driver("tui")
    driver.launch({
        "app_path": tui_app_path,
        "url": nest_instance["url"],
        "seed_credentials": seed,
        "environment": _seeded_environment(request, nest_instance),
    })
    try:
        _await_working_session(driver, user_actor, "the launch as the user")
        _wait_switcher_count(driver, 2, timeout=30)

        # A → B: the user's session is torn down in place; the admin's engine
        # opens under ITS OWN scoped store.
        driver.click(SWITCHER_ITEM, index=1)
        _await_working_session(driver, admin_actor, "the switch to the admin")
        _wait_switcher_count(driver, 2, timeout=30)

        # B → A: the user's engine must open again — the store it opens is the
        # one the first session served, so this is the line the outgoing
        # session's holders would break.
        driver.click(SWITCHER_ITEM, index=0)
        _await_working_session(driver, user_actor, "the switch back to the user")
    finally:
        driver.teardown()


# ── The switcher's "open in new window" affordance, tui's door ───────────────
# (second-identity-in-its-own-window outcome 1 — feature-catalog.md's
# door-neutral re-authoring, user-approved 2026-09-01)

OPEN_NEW_INSTANCE_BUTTON = "account-open-new-instance-button"
OPEN_NEW_INSTANCE_COMMAND = "account-open-new-instance-command"


@pytest.mark.feature("second-identity-in-its-own-window")
def test_tui_open_as_new_instance_copies_the_launch_command(
    nest_instance, tui_app_path, request
):
    """tui cannot hand a spawned process its own tty the way a GUI toolkit
    hands a new process a window (unlike `test_account_switcher_linux.py`'s
    `test_linux_open_as_new_instance_spawns_a_bound_sibling`, which observes a
    REAL second `fauna-desktop` process), so clicking
    `account-open-new-instance-button` copies the `FAUNA_BOUND_ACCOUNT=<hex>
    <exe>` launch command to the clipboard (OSC 52) instead and paints what
    actually landed there as `account-open-new-instance-command` — OSC 52 is
    fire-and-forget into a terminal that may ignore it (the
    `admin/dns.rs`/`settings/web.rs` copy-confirmation doctrine). tui's
    door-neutral twin of the other desktop apps' real spawn
    (`account-scoping.md` § Concurrent instances, the running instance's
    surface).

    Offered even on the lone (therefore active) row — tui shipped straight
    onto the coexisting-lock shape, so there is no non-active-only era to
    test around here; `test_account_instance_lock_tui.py`'s coexistence pin
    is what proves running the copied command actually works end to end."""
    seed, actor = _seed_one_account(nest_instance)

    driver = create_driver("tui")
    driver.launch({
        "app_path": tui_app_path,
        "url": nest_instance["url"],
        "seed_credentials": seed,
        "environment": _seeded_environment(request, nest_instance),
    })
    try:
        driver.wait_for_state(
            lambda s: bool(s.get("session", {}).get("authenticated")), timeout=30
        )
        _wait_switcher_count(driver, 1, timeout=30)
        assert driver.is_absent(OPEN_NEW_INSTANCE_COMMAND), (
            "nothing is copied before the button is used"
        )

        driver.click(OPEN_NEW_INSTANCE_BUTTON)
        driver.wait_for(OPEN_NEW_INSTANCE_COMMAND, timeout=10)
        copied = driver.get_text(OPEN_NEW_INSTANCE_COMMAND)
        assert f"FAUNA_BOUND_ACCOUNT={actor}" in copied, (
            "the painted confirmation must carry the exact launch command for "
            f"this row's account; got {copied!r}"
        )
        assert "fauna-tui" in copied, (
            f"the copied command must name the tui binary; got {copied!r}"
        )
    finally:
        driver.teardown()


# ── Removing an identity erases ITS scope and only its scope ─────────────────
# (multiple-accounts outcome 6 — `account-scoping.md` § The scoping taxonomy;
# the erase door is `apps/fauna-tui/src/account_scope.rs::remove_account`)


@pytest.mark.feature("multiple-accounts")
def test_tui_removing_an_identity_deletes_its_data_and_leaves_the_others(
    nest_instance, tui_app_path, request
):
    """Removing an identity deletes THAT identity's data on this device and
    leaves every other identity's data untouched.

    `test_tui_remove_account_shrinks_switcher_live` above proves the switcher
    row and the registry entry go; neither says anything about the account's
    own on-device state, which is the half a user cannot see and cannot get
    back. Both identities first run a real session each (the admin's through
    a switch), so each owns a populated scope. Then the removal, through the
    switcher's own `account-remove-button`, must leave no admin scope behind
    under ANY base the erase sweeps (`common.scope_store.XdgScopeStore` — the
    flat base, its `backup/`, and the unified sync root) while the user's
    `mls_state.db` and secret are still there.

    Every wait is on state (a working session, the registry, a path) —
    `e2e-conventions.md` point 14."""
    seed, user_actor, admin_actor = _seed_two_accounts(nest_instance)

    driver = create_driver("tui")
    driver.launch({
        "app_path": tui_app_path,
        "url": nest_instance["url"],
        "seed_credentials": seed,
        "environment": _seeded_environment(request, nest_instance),
    })
    try:
        _await_working_session(driver, user_actor, "the launch as the user")
        _wait_switcher_count(driver, 2, timeout=30)
        store = XdgScopeStore("tui", driver.config_home, "fauna-tui")
        user_db = store.scope_dirs(user_actor)[0] / "mls_state.db"
        _wait_for_path(str(user_db), timeout=15)

        # Give the admin a real session of its own, so it has data to lose.
        driver.click(SWITCHER_ITEM, index=1)
        _await_working_session(driver, admin_actor, "the switch to the admin")
        admin_dirs = store.scope_dirs(admin_actor)
        _wait_for_path(str(admin_dirs[0] / "mls_state.db"), timeout=15)

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

        # Registry removal, then the erase, in one gesture — so a registry
        # that shrank means the erase is already running or done.
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


# ── Switching to an identity this device cannot sign in as ───────────────────
# (multiple-accounts outcome 9 — `long-term-store.md` § Multi-account evolution,
# "Activating refuses an account it cannot launch as")


@pytest.mark.feature("multiple-accounts")
def test_tui_switching_to_an_identity_the_device_cannot_sign_in_as_is_refused(
    nest_instance, tui_app_path, request
):
    """Switching to an identity this device can no longer sign in as is
    refused, says so, and leaves the user on the identity they were using.

    The listed-but-unlaunchable shape is reachable, not theoretical: the
    secret store's write is infallible by signature, so a keystore row that
    silently never landed (or that the OS or user removed) leaves the account
    in the index with no secret. The seed reproduces exactly that — the
    admin's `fauna/<actor>/secret` slot is simply absent. The shared registry
    refuses at `set_active` (`AccountError::NoStoredSecret`) before any
    teardown; what this pins is the whole user-visible contract: the click
    paints the shared refusal line (`fauna_client_accounts::
    switch_refused_copy`) on `error-message`, no relaunch happens, and the
    live session and the persisted active pointer are both still the user's.

    The absence of a relaunch is anchored causally (`assert_no_relaunch`,
    point 14): the refusal line is the handler's own completion observable."""
    seed, user_actor, admin_actor = _seed_two_accounts(nest_instance)
    seed.pop(f"fauna/{admin_actor}/secret")

    driver = create_driver("tui")
    driver.launch({
        "app_path": tui_app_path,
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
            lambda: driver.is_visible("error-message")
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


# ── Device settings stay; identity-tied choices follow the identity ──────────
# (multiple-accounts outcome 10 — `account-scoping.md` § Serialized switching
# and § The scoping taxonomy, class 2 vs class 1)

TUI_SETTINGS_NAV = {
    "nav": {"stack": [{"view": "settings"}, {"view": "settings", "id": "tui-settings"}]}
}
EXTERNAL_MEDIA_SELECT = "tui-settings-external-media"
COMPOSE_FIELD = "compose-text-field"


@pytest.mark.feature("multiple-accounts")
def test_tui_switching_identity_keeps_device_settings_and_moves_drafts(
    nest_instance, tui_app_path, request
):
    """Settings that describe this device stay as they are across a switch,
    while a draft — a choice tied to an identity — follows its identity.

    tui's device setting is the external-media handoff (`tui-settings`): how
    THIS terminal opens media it cannot play inline, persisted install-scoped
    in `prefs.json` beside the credential store and named by no actor scope
    (`apps/fauna-tui/src/settings/mod.rs`'s persistence note;
    `account_scope.rs`'s erase leaves it untouched by construction). tui's
    General page is static by ruling and grows no control here. The
    identity-tied half is the feed composer's draft, which rests in that
    identity's own sealed `__drafts` plane on the nest.

    The arc: as the user, choose `never` and leave a draft (confirmed on the
    nest before switching — a switch mid-debounce would test the debounce,
    not the scoping); switch to the admin → the choice is still `never` and
    the composer holds NOT the user's draft; switch back → the user's draft
    is there again and the choice still `never`. Every wait is on state
    (point 14); the drafts read is the sanctioned side-channel verification
    (convention 8's carve-out), the mutations are all UI."""
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

    driver = create_driver("tui")
    driver.launch({
        "app_path": tui_app_path,
        "url": nest_instance["url"],
        "seed_credentials": seed,
        "environment": _seeded_environment(request, nest_instance),
    })
    feed = FeedActions(driver)

    def external_media():
        driver.set_state(TUI_SETTINGS_NAV)
        driver.wait_for(EXTERNAL_MEDIA_SELECT, timeout=15)
        return driver.get_text(EXTERNAL_MEDIA_SELECT)

    def composer_body():
        feed.navigate()
        feed.open_composer()
        return feed.compose_body_text()

    try:
        _await_working_session(driver, user_actor, "the launch as the user")

        # (1) As the user: a device choice and a draft.
        assert external_media() == "ask", "precondition: the fresh install's default"
        driver.select(EXTERNAL_MEDIA_SELECT, "never")
        assert driver.get_text(EXTERNAL_MEDIA_SELECT) == "never"
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
        assert external_media() == "never", (
            "a setting that describes this device must survive the switch; "
            f"the external-media choice reads {driver.get_text(EXTERNAL_MEDIA_SELECT)!r}"
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
        assert external_media() == "never", (
            "the device setting must still hold after switching back; reads "
            f"{driver.get_text(EXTERNAL_MEDIA_SELECT)!r}"
        )
    finally:
        driver.teardown()


# ── An abandoned first-run identity never becomes a switcher row ─────────────
# (multiple-accounts outcome 8 — `long-term-store.md` § Multi-account evolution;
# the in-process twin is `apps/fauna-tui/src/wizard/mod.rs`'s
# `abandoning_a_created_identity_leaves_no_second_switcher_row`)

CREATE_IDENTITY_BUTTON = "create-identity-button"
IDENTITY_CONTINUE_BUTTON = "identity-continue-button"
IDENTITY_CREATED_BACK_BUTTON = "identity-created-back-button"
RECOVERY_KIT_SKIP_BUTTON = "recovery-kit-skip-button"
HANDLE_ENTRY_BACK_BUTTON = "handle-entry-back-button"


@pytest.mark.feature("multiple-accounts")
def test_tui_abandoned_created_identity_never_shows_among_your_identities(
    nest_instance, tui_app_path, request
):
    """A fresh install creates identity A, walks back out of it, and onboards a
    DIFFERENT identity B instead: the switcher lists exactly one identity — B —
    and A never shows up among your identities.

    `test_tui_add_account_abandon_recovers_prior_identity_on_relaunch` above is
    the append-mode abandon, where tui's wizard writes nothing until LoggedIn,
    so it cannot ghost by construction. The first-run wizard is the case that
    can: moment 1 registers AND activates A at `identity-continue-button`,
    before any nest has heard of it, and nothing on the Back path can retract
    that row — the retraction lives in B's commit
    (`AccountRegistry::retire_superseded_provisionals`, shared Rust). This is
    the user-visible whole of that mechanism, the windows journey's tui twin
    (`test_windows_abandoned_create_identity_does_not_ghost_the_switcher`).

    B arrives by import, never a second create: create → Back → create re-offers
    the same generated secret, which is one identity, not two."""
    admin_sk = nest_instance["admin"]["signing_key"]
    second = create_actor_and_register(nest_instance["port"], admin_signing_key=admin_sk)
    second_actor = second["actor_id_hex"]
    second_secret = bytes(second["signing_key"]).hex()

    driver = create_driver("tui")
    driver.launch({
        "app_path": tui_app_path,
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
        # outcome (the append journey's seam) so Continue lands LoggedIn — B's
        # commit, the write that must retire A. The dial override lets the
        # session B lands in actually come up on this plain-HTTP nest.
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
