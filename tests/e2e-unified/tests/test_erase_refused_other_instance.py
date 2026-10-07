"""tier_3 e2e: every user-facing erase on tui and linux refuses while a SIBLING
instance serves the account, and goes through once the sibling is gone.

``docs/goal/architecture/apps/account-scoping.md`` § Concurrent instances →
*An erase refuses while a sibling serves the account*, the *Every erasing
gesture asks the same question* bullet: sign-out, remove-account and the
unreadable-index floor's start-over each ask through the door before their
first destructive step. The native twin of web's two-tab pair
(``test_remove_account_refused_other_tab_web.py``,
``test_sign_out_refused_other_tab_web.py``).

**Why a two-process witness.** The guard is pinned at its own site
(``account_scope::{remove_account_under, all_accounts_erase_blocked_under}``,
over temp bases), and each gesture helper (``sign_out_confirm_unless``,
``start_over_unless``/``start_over_confirm_unless``,
``remove_account_confirm_unless``) takes its guard by injection so a unit
test never probes the developer's real config dirs. What only a real pair of
processes witnesses is the *production wiring* — which function each gesture
is handed — and the process's own-lock release
(``InstanceHolder::without_own_lock``), which needs a process that really
serves the account through its own launch path. Each journey below has both
arms, and each arm reddens a different class of regression:

* **refused while the sibling serves** — reddens a gesture rewired past the
  guard (tui ``session::remove_account`` calling the registry directly; any
  of the six call sites handed ``|_| None`` / ``|| None``);
* **goes through once the sibling is gone** — reddens a release that leaves
  the process's own lock up (``without_own_lock`` skipping its put-downs), so
  a LONE instance mistakes its own reflection for a sibling and can never
  sign out.

**The world.** Both instances share one install world (XDG base + credential
file + keyring namespace — ``test_account_instance_lock_{tui,linux}.py``'s
launch-and-bind shape): the first is a plain launch serving the store-active
account, the second a bound launch (``FAUNA_BOUND_ACCOUNT``) — the shape a
user's "open as new instance" produces. The floor's world differs: its second
launch reads a malformed index (the staging of
``test_account_index_unreadable_launch.py``) written into the shared store
while the first serves.

tier_3: a real ``fauna-nest`` (``nest_instance``) and real app binaries.
"""
from __future__ import annotations

import json
import os
import tempfile

import pytest

from actions import ActionLayer
from common import build_registry_seed, create_actor_and_register
from common.scope_store import XdgScopeStore
from conftest import _seeded_environment, get_available_apps
from drivers import create_driver
from helpers.instance_guard import is_app_alive
from helpers.registry_store import (
    credential_store_path,
    read_registry_index,
    read_store_map,
    read_store_slot,
)
from helpers.waiting import wait_until
from i18n.strings import S

pytestmark = [pytest.mark.tier_3, pytest.mark.tui, pytest.mark.linux]

#: The two apps whose six gesture call sites this module witnesses.
_SUPPORTED_APPS = ("tui", "linux")

#: Each app's flat base under ``XDG_CONFIG_HOME`` (``XdgScopeStore``).
_FLAT_BASE = {"tui": "fauna-tui", "linux": "fauna"}

# tests/e2e-unified/ui.yaml § settings (switcher) + § onboarding (launch).
SWITCHER_LIST = "account-switcher-list"
SWITCHER_ITEM = "account-switcher-item"
REMOVE_BUTTON = "account-remove-button"
ERROR_MESSAGE = "error-message"
CREATE_IDENTITY = "create-identity-button"
INDEX_REFUSAL = "account-index-refusal-warning"
INDEX_RESET = "account-index-reset-button"
INDEX_RESET_CONFIRM = "account-index-reset-confirm-button"

ACCOUNT_PAGE_NAV = {
    "nav": {"stack": [{"view": "settings"}, {"view": "settings", "id": "account"}]}
}

#: Not JSON at all — the malformed verdict, whose floor is the start-over
#: (``test_account_index_unreadable_launch.MALFORMED_INDEX``).
MALFORMED_INDEX = "this is not an account index {{{"

#: Convention 14: generous ceilings on latency-independent state. A launch is
#: a process boot plus a nest round-trip; a refusal or an erase is local.
LAUNCH_BUDGET_S = 60.0
LOCAL_BUDGET_S = 30.0


def _apps():
    available = get_available_apps()
    return [a for a in _SUPPORTED_APPS if a in available]


@pytest.fixture(params=_apps())
def app(request):
    """The app under test; its id lands in the test name (``[tui]``/``[linux]``),
    which is what conftest's ``--app`` filter reads."""
    return request.param


@pytest.fixture
def app_path(app, request):
    return request.getfixturevalue(f"{app}_app_path")


# ── the world ────────────────────────────────────────────────────────────────


def _shared_instance_world(app):
    """One throwaway install world for two instances to share — the same-OS-
    login premise of the guard (``test_account_instance_lock_{tui,linux}.py``).
    Each driver otherwise gives every launch a fresh base, which would put the
    two processes in different installs contending on nothing."""
    base = tempfile.mkdtemp(prefix=f"fauna-e2e-{app}-erase-world-")
    return {
        "xdg_base": os.path.join(base, "xdg"),
        "credential_dir": os.path.join(base, "credentials"),
        "keyring_app": f"fauna-e2e-{app}-erase-world-{os.path.basename(base)}",
    }


def _account(nest_instance, handle):
    """A freshly-registered regular account: (actor_id_hex, secret_hex)."""
    user = create_actor_and_register(
        nest_instance["port"], admin_signing_key=nest_instance["admin"]["signing_key"]
    )
    return user["actor_id_hex"], bytes(user["signing_key"]).hex()


def _seed(nest_instance, accounts, active):
    return build_registry_seed(
        [
            {"actor_id": actor, "secret_hex": secret, "nest_url": nest_instance["url"],
             "device_id": f"erase-{handle}", "handle": handle}
            for handle, (actor, secret) in accounts.items()
        ],
        active=active,
    )


def _launch(app, app_path, nest_instance, request, world, *, seed=None, bound=None):
    """Launch one instance into ``world``. Only the first launch seeds — a
    re-seed would clobber the registry under the instance already serving."""
    environment = _seeded_environment(request, nest_instance)
    if bound is not None:
        environment = {**environment, "FAUNA_BOUND_ACCOUNT": bound}
    config = {
        "app_path": app_path,
        "url": nest_instance["url"],
        "environment": environment,
        **world,
    }
    if seed is not None:
        config["seed_credentials"] = seed
    driver = create_driver(app)
    driver.launch(config)
    return driver


def _wait_serving(driver, actor):
    """Authenticated as ``actor`` in ONE poll tick (linux flips
    ``authenticated`` a tick before ``actor_id`` lands —
    ``test_account_instance_lock_linux._wait_authenticated_as``)."""
    return driver.wait_for_state(
        lambda s: s.get("session", {}).get("actor_id") == actor
        and bool(s.get("session", {}).get("authenticated")),
        timeout=LAUNCH_BUDGET_S,
    )


def _scope_dirs(app, driver, actor):
    return XdgScopeStore(app, driver.config_home, _FLAT_BASE[app]).scope_dirs(actor)


def _errors(driver) -> list[str]:
    try:
        return [t for t in driver.get_texts(ERROR_MESSAGE) if t]
    except Exception:  # noqa: BLE001 — a surface with no error line reads as none
        return []


def _wait_error(driver, expected, what):
    """The page's ``error-message`` carrying exactly the shared refusal line."""
    return wait_until(
        lambda: expected in _errors(driver),
        LOCAL_BUDGET_S,
        diagnose=lambda: (
            f"{what}: error-message never read the shared refusal {expected!r}; "
            f"it reads {_errors(driver)!r}"
        ),
    )


def _refusals_logged(driver) -> list[str]:
    """The app's own ``REFUSED`` log lines — each names the accounts it found
    served, which is what a refusal that should not have happened needs."""
    try:
        text = driver.app_stderr_text()
    except Exception as e:  # noqa: BLE001 — a diagnosis must never mask the failure
        return [f"<stderr unreadable: {e}>"]
    return [line for line in text.splitlines() if "REFUSED" in line]


def _refusal_log(driver) -> list[str]:
    return _refusals_logged(driver)[-5:]


def _press_until_through(driver, press, done, what):
    """Once the sibling is gone, press the gesture until it goes through.

    "Gone" is the sibling's lock released, which no driver observes directly:
    teardown can return while the app's process group is still dying, and a
    press in that window is refused, correctly. So this is a deadline poll
    (convention 14), and a re-press only follows a press the app logged as
    refused: the user's own remedy, "close it, then … again". A seat whose
    own lock answers for a sibling is refused on every press, so this still
    fails then."""
    seen = len(_refusals_logged(driver))
    press()

    def through():
        nonlocal seen
        if done():
            return True
        now = len(_refusals_logged(driver))
        if now > seen:
            seen = now
            press()
        return False

    wait_until(
        through,
        LOCAL_BUDGET_S,
        diagnose=lambda: (
            f"{what}; error-message {_errors(driver)!r}, "
            f"refusals logged {_refusal_log(driver)!r}"
        ),
    )


def _registry_actors(driver) -> list[str]:
    return [a["actor_id"] for a in (read_registry_index(driver) or {}).get("accounts", [])]


def _assert_sibling_serving(driver, actor, what):
    """The refusal's premise, checked rather than assumed: a sibling that died
    has released its lock, and an erase that then goes through is correct."""
    assert is_app_alive(driver) is not False, (
        f"{what}: the sibling instance exited, so nothing served the account — "
        f"its stderr tail:\n{driver.app_stderr_text()[-3000:]}"
    )
    session = (driver.get_state() or {}).get("session") or {}
    assert session.get("actor_id") == actor and session.get("authenticated"), (
        f"{what}: the sibling must still serve {actor}; got {session!r}"
    )


# ── remove-account (tui M5, linux M8) ────────────────────────────────────────


@pytest.mark.feature("multiple-accounts")
def test_remove_account_refuses_while_another_instance_serves_it(
    app, app_path, nest_instance, request
):
    """Two accounts U (store-active) and V. The first instance serves U; a
    second, bound to V, offers U for removal (it does not serve U — the switcher
    keys "in use" on the account the instance serves). Pressing it is REFUSED:
    the shared ``settings.remove_account_blocked_other_window`` line, U still
    in the registry, its secret slot and its scope on disk. With the first
    instance gone, the same press removes U and erases its scope."""
    u, v = _account(nest_instance, "u"), _account(nest_instance, "v")
    u_actor, v_actor = u[0], v[0]
    world = _shared_instance_world(app)

    first = _launch(app, app_path, nest_instance, request, world,
                    seed=_seed(nest_instance, {"u": u, "v": v}, active=u_actor))
    second = None
    try:
        _wait_serving(first, u_actor)
        u_scopes = _scope_dirs(app, first, u_actor)
        wait_until(
            lambda: any(d.exists() for d in u_scopes),
            LAUNCH_BUDGET_S,
            diagnose=lambda: f"U's scope never appeared under {[str(d) for d in u_scopes]}",
        )

        second = _launch(app, app_path, nest_instance, request, world, bound=v_actor)
        _wait_serving(second, v_actor)

        second.set_state(ACCOUNT_PAGE_NAV)
        second.wait_for(SWITCHER_LIST, timeout=LOCAL_BUDGET_S)
        wait_until(
            lambda: second.count(SWITCHER_ITEM) == 2 and second.count(REMOVE_BUTTON) == 1,
            LOCAL_BUDGET_S,
            diagnose=lambda: (
                f"the V instance must list both accounts and offer exactly U for "
                f"removal; items={second.count(SWITCHER_ITEM)}, "
                f"remove buttons={second.count(REMOVE_BUTTON)}"
            ),
        )

        # ── refused while the first instance serves U ───────────────────────
        _assert_sibling_serving(first, u_actor, "before the refused removal")
        second.click(REMOVE_BUTTON)
        _wait_error(second, S.settings.remove_account_blocked_other_window,
                    "remove-account under a live sibling")
        assert u_actor in _registry_actors(second), (
            f"a refused removal erases nothing, but U left the registry: "
            f"{read_registry_index(second)!r}"
        )
        assert read_store_slot(second, f"fauna/{u_actor}/secret") == u[1], (
            "a refused removal must leave U's secret slot in place"
        )
        assert any(d.exists() for d in u_scopes), (
            f"a refused removal must leave U's scope on disk; none of "
            f"{[str(d) for d in u_scopes]} exists"
        )
        _assert_sibling_serving(first, u_actor, "after the refused removal")

        # ── goes through once the sibling is gone ───────────────────────────
        first.teardown()
        first = None
        second.set_state(ACCOUNT_PAGE_NAV)
        second.wait_for(REMOVE_BUTTON, timeout=LOCAL_BUDGET_S)
        _press_until_through(
            second,
            lambda: second.click(REMOVE_BUTTON),
            lambda: _registry_actors(second) == [v_actor],
            "with no sibling serving U, the removal must go through",
        )
        wait_until(
            lambda: not any(d.exists() for d in u_scopes),
            LOCAL_BUDGET_S,
            diagnose=lambda: (
                f"U's scope survived the removal under "
                f"{[str(d) for d in u_scopes if d.exists()]}"
            ),
        )
        assert read_store_slot(second, f"fauna/{u_actor}/secret") is None, (
            "the removed account's secret must be gone from this device"
        )
    finally:
        if second is not None:
            second.teardown()
        if first is not None:
            first.teardown()


# ── sign-out (tui M9, linux M10) + the own-lock release (M11) ────────────────


@pytest.mark.feature("account")
def test_sign_out_refuses_while_another_instance_serves_the_account(
    app, app_path, nest_instance, request
):
    """One account U, served by two coexisting instances (plain + bound). In
    the second, sign-out is REFUSED: the shared
    ``settings.sign_out_blocked_other_window`` line, still signed in, U still
    in the registry with its secret. With the first gone, the lone second
    instance signs out — which needs it to put its OWN instance and serving
    locks down for the question (``without_own_lock``): left up, they answer
    "served" and a lone instance could never sign out."""
    u = _account(nest_instance, "u")
    u_actor = u[0]
    world = _shared_instance_world(app)

    first = _launch(app, app_path, nest_instance, request, world,
                    seed=_seed(nest_instance, {"u": u}, active=u_actor))
    second = None
    try:
        _wait_serving(first, u_actor)
        second = _launch(app, app_path, nest_instance, request, world, bound=u_actor)
        _wait_serving(second, u_actor)
        settings = ActionLayer(second).settings

        # ── refused while the first instance serves U ───────────────────────
        _assert_sibling_serving(first, u_actor, "before the refused sign-out")
        settings.press_sign_out()
        _wait_error(second, S.settings.sign_out_blocked_other_window,
                    "sign-out under a live sibling")
        session = (second.get_state() or {}).get("session") or {}
        assert session.get("actor_id") == u_actor and session.get("authenticated"), (
            f"a refused sign-out must stay signed in; got {session!r}"
        )
        assert second.is_absent(CREATE_IDENTITY), (
            "a refused sign-out must not land on onboarding"
        )
        assert _registry_actors(second) == [u_actor], (
            f"a refused sign-out erases nothing; registry {read_registry_index(second)!r}"
        )
        assert read_store_slot(second, f"fauna/{u_actor}/secret") == u[1], (
            "a refused sign-out must leave the secret slot in place"
        )
        _assert_sibling_serving(first, u_actor, "after the refused sign-out")

        # ── the lone instance signs out ─────────────────────────────────────
        first.teardown()
        first = None
        _press_until_through(
            second,
            settings.press_sign_out,
            lambda: second.is_visible(CREATE_IDENTITY),
            "the lone instance never signed out — a refusal here means its own "
            "lock answered for a sibling",
        )
        assert read_store_slot(second, f"fauna/{u_actor}/secret") is None, (
            "sign-out must erase the account's secret"
        )
    finally:
        if second is not None:
            second.teardown()
        if first is not None:
            first.teardown()


# ── the unreadable-index floor's start-over (tui M6, linux M7) ───────────────


def _corrupt_index(driver):
    """Replace the shared store's ``fauna/index`` with a malformed blob,
    atomically (a reader never sees a half-written file), keeping every other
    slot — the serving instance's secret included."""
    path = credential_store_path(driver)
    assert path is not None, "the shared world must be file-backed"
    stored = read_store_map(driver)
    assert stored, f"the shared store at {path} is empty — nothing to corrupt"
    stored["fauna/index"] = MALFORMED_INDEX
    tmp = f"{path}.e2e-tmp"
    with open(tmp, "w") as f:
        json.dump(stored, f)
    os.replace(tmp, path)


@pytest.mark.feature("upgrades-never-lose-data")
def test_start_over_refuses_while_another_instance_serves_an_account(
    app, app_path, nest_instance, request
):
    """The first instance serves U; the index is then made malformed, so a
    second plain launch lands on the unreadable-index surface, whose floor is
    the factory reset. Its confirm is REFUSED — the shared
    ``onboarding.launch.index_malformed_reset_blocked_other_window`` line, the
    confirm still offered, U's secret and scope on disk — because the reset
    would erase what the first is running out of. With the first gone, the
    same confirm resets to a fresh install."""
    u = _account(nest_instance, "u")
    u_actor = u[0]
    world = _shared_instance_world(app)

    first = _launch(app, app_path, nest_instance, request, world,
                    seed=_seed(nest_instance, {"u": u}, active=u_actor))
    second = None
    try:
        _wait_serving(first, u_actor)
        u_scopes = _scope_dirs(app, first, u_actor)
        wait_until(
            lambda: any(d.exists() for d in u_scopes),
            LAUNCH_BUDGET_S,
            diagnose=lambda: f"U's scope never appeared under {[str(d) for d in u_scopes]}",
        )
        _corrupt_index(first)

        second = _launch(app, app_path, nest_instance, request, world)
        second.wait_for(INDEX_REFUSAL, timeout=LAUNCH_BUDGET_S)
        assert second.get_text(INDEX_REFUSAL) == S.onboarding.launch.index_malformed, (
            f"the second launch must read the malformed verdict; "
            f"got {second.get_text(INDEX_REFUSAL)!r}"
        )
        second.click(INDEX_RESET)
        second.wait_for(INDEX_RESET_CONFIRM, timeout=LOCAL_BUDGET_S)

        # ── refused while the first instance serves U ───────────────────────
        _assert_sibling_serving(first, u_actor, "before the refused start-over")
        second.click(INDEX_RESET_CONFIRM)
        _wait_error(second, S.onboarding.launch.index_malformed_reset_blocked_other_window,
                    "start-over under a live sibling")
        assert second.is_visible(INDEX_RESET_CONFIRM), (
            "a refused start-over keeps its confirm — closing the other window and "
            "confirming again is the whole remedy"
        )
        assert second.is_absent(CREATE_IDENTITY), (
            "a refused start-over must not land on a fresh install"
        )
        assert read_store_slot(second, f"fauna/{u_actor}/secret") == u[1], (
            "a refused start-over must leave the served account's secret in place"
        )
        assert any(d.exists() for d in u_scopes), (
            f"a refused start-over must leave the served account's scope on disk; "
            f"none of {[str(d) for d in u_scopes]} exists"
        )
        _assert_sibling_serving(first, u_actor, "after the refused start-over")

        # ── goes through once the sibling is gone ───────────────────────────
        first.teardown()
        first = None
        _press_until_through(
            second,
            lambda: second.click(INDEX_RESET_CONFIRM),
            lambda: second.is_visible(CREATE_IDENTITY),
            "with no sibling serving, the start-over must reset to a fresh install",
        )
        assert not any(d.exists() for d in u_scopes), (
            f"the reset must erase the scope it refused to before; survivors "
            f"{[str(d) for d in u_scopes if d.exists()]}"
        )
    finally:
        if second is not None:
            second.teardown()
        if first is not None:
            first.teardown()
