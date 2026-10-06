"""tier_3 e2e: linux launch-collision chooser (`launch_instance_chooser` page,
`docs/goal/architecture/apps/account-scoping.md` § Concurrent instances →
"the colliding instance's surface").

A **plain** launch whose would-be account (the store-active one) is already
served by a live instance renders this chooser instead of authenticating
(`main.rs::launch_collision_detected` / `show_launch_instance_chooser`) — the
concurrent-instances 5d slice; linux is the first client to
render it (windows/tui still owe it). This is the two-driver e2e verify
tracked as owed at build time
(`ui-actual-linux.yaml`'s `open_questions`), companion to
`test_account_switcher_linux.py`'s
`test_linux_open_as_new_instance_spawns_a_bound_sibling` (the OTHER 5d
surface, the running instance's spawn affordance).

Under e2e every launch already runs `NON_UNIQUE` (`main.rs::e2e_mode_enabled`),
so GApplication's D-Bus uniqueness never redirects the second launch away
before `launch_collision_detected()` runs — this test does not (and must not)
rely on that production redirect; it seeds the served account and lets the
collision check do the work.

Coverage:

1. Instance A plainly launched and authenticated as the registry's active
   account (X). A second plain launch, sharing A's install world, must find
   X already served and render `launch-instance-chooser` with exactly the
   registry's other (not-currently-served) accounts as
   `launch-instance-chooser-item` rows. Picking one binds THIS (second)
   process to it (`bind_account` + `bind_session_launch_to`) and completes
   routing as that account — no third process (spawning a sibling is the
   switcher's own affordance, covered separately). The first instance is
   untouched throughout.

2. Focus-existing reaches a **bound** server (the per-(OS login, account) raise
   channel, ratified 2026-07-23). This is the case the app-wide activation name
   structurally could not serve: a bound instance opts out of GApplication
   uniqueness and owns no app-wide name, so before the raise channel a
   collision against one surfaced `no_running_instance` and stranded the user.
   With the channel, the bound server owns `social.fauna.fauna.a<token>` for
   the account it serves, so focus-existing reaches it like any other server.
   Asserted from BOTH ends — the raiser exits (the call was delivered) and the
   server's `raises_served` advances (the raise actually arrived) — because
   either alone passes on a half-wired channel: a delivered call whose raise is
   never drained, or a drained raise nobody could deliver.

tier_3: needs a real `fauna-nest` binary (`nest_instance`); linux driver only.
"""
from __future__ import annotations

import os
import tempfile

import pytest

from common import build_registry_seed, create_actor_and_register
from conftest import _seeded_environment
from drivers import create_driver
from drivers.session_bus import PrivateSessionBus
from helpers.budgets import APP_RELAUNCH_S
from helpers.instance_guard import expect_exit_after_click
from helpers.waiting import wait_until

pytestmark = [pytest.mark.tier_3, pytest.mark.linux]

CHOOSER_ANCHOR = "launch-instance-chooser"
CHOOSER_ITEM = "launch-instance-chooser-item"
FOCUS_EXISTING = "launch-instance-focus-existing-button"


def _seed_two_accounts(nest_instance):
    """A claimed-admin account + a freshly-registered regular-user account,
    active on the regular user (the shape `test_account_switcher_linux.py` /
    `test_account_instance_lock_linux.py` seed). Returns (seed_map,
    user_actor_id, admin_actor_id)."""
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
            {"actor_id": user_actor, "secret_hex": user_secret, "nest_url": url,
             "device_id": "chooser-user", "handle": "user"},
            {"actor_id": admin_actor, "secret_hex": admin_secret, "nest_url": url,
             "device_id": "chooser-admin", "handle": "admin"},
        ],
        active=user_actor,
    )
    return seed, user_actor, admin_actor


def _shared_instance_world():
    """One throwaway install world (XDG base + credential store + keyring
    namespace) for two instances to share — the same-OS-login premise the
    collision check runs under. Pins exactly the keys
    `preserve_state_across_relaunch` pins, so both launches read one registry
    and probe one lock file under `<xdg_base>/config/fauna/`. Mirrors
    `test_account_instance_lock_linux.py`'s helper of the same name (its own
    private copy — this repo does not import test helpers across test
    modules; see that file for the sibling instance-guard coverage this
    chooser test complements)."""
    base = tempfile.mkdtemp(prefix="fauna-e2e-linux-chooser-world-")
    return {
        "xdg_base": os.path.join(base, "xdg"),
        "credential_dir": os.path.join(base, "credentials"),
        "keyring_app": f"fauna-e2e-chooser-world-{os.path.basename(base)}",
    }


def _lock_held(lock_path: str) -> bool:
    """Whether a live process holds `lock_path` — the product's own
    `AccountInstanceLock::is_served` probe (an exclusive non-blocking try on a
    fresh open file description, released at once). A missing file is free."""
    import fcntl

    try:
        fd = os.open(lock_path, os.O_RDWR)
    except FileNotFoundError:
        return False
    try:
        fcntl.flock(fd, fcntl.LOCK_EX | fcntl.LOCK_NB)
    except BlockingIOError:
        return True
    finally:
        os.close(fd)
    return False


def _wait_authenticated(driver):
    return driver.wait_for_state(
        lambda s: bool(s.get("session", {}).get("authenticated")), timeout=45
    )


@pytest.mark.feature("second-identity-in-its-own-window")
def test_linux_second_plain_launch_renders_chooser_and_pick_completes_as_that_account(
    nest_instance, linux_app_path, request
):
    """Instance A serves the active account (user); a second plain launch,
    sharing A's install world, must render the chooser rather than
    authenticate — with exactly the OTHER registered account
    (accounts - served = 1) offered — and picking it must complete this
    (second) process's routing as that account, leaving A untouched."""
    seed, user_actor, admin_actor = _seed_two_accounts(nest_instance)
    world = _shared_instance_world()

    first = create_driver("linux")
    first.launch({
        "app_path": linux_app_path,
        "url": nest_instance["url"],
        "seed_credentials": seed,
        "environment": _seeded_environment(request, nest_instance),
        **world,
    })
    try:
        _wait_authenticated(first)

        second = create_driver("linux")
        try:
            # No re-seed: the shared store already holds the registry, and a
            # re-seed would clobber it under the first instance.
            second.launch({
                "app_path": linux_app_path,
                "url": nest_instance["url"],
                "environment": _seeded_environment(request, nest_instance),
                **world,
            })
            second.wait_for(CHOOSER_ANCHOR, timeout=30)
            assert second.count(CHOOSER_ITEM) == 1, (
                "with 2 registered accounts and 1 (the user) already served, "
                "the chooser must offer exactly the other one (the admin)"
            )

            second.click(CHOOSER_ITEM, index=0)
            state = second.wait_for_state(
                lambda s: s.get("session", {}).get("actor_id") == admin_actor
                and bool(s.get("session", {}).get("authenticated")),
                timeout=45,
            )
            assert state["session"]["actor_id"] == admin_actor, (
                f"picking the only offered row must bind this process to the "
                f"admin account; got {state.get('session')!r}"
            )

            # The already-served instance is untouched by the collision + pick.
            first_state = first.get_state()
            assert (
                first_state.get("session", {}).get("actor_id") == user_actor
                and bool(first_state.get("session", {}).get("authenticated"))
            ), (
                "the already-served instance must be untouched by a "
                f"colliding sibling's pick; got {first_state.get('session')!r}"
            )
        finally:
            second.teardown()
    finally:
        first.teardown()


@pytest.mark.feature("second-identity-in-its-own-window")
def test_linux_focus_existing_raises_a_bound_sibling(
    nest_instance, linux_app_path, request
):
    """Focus-existing must reach a server that owns NO app-wide name — the
    case the per-(OS login, account) raise channel exists for.

    Instance A is launched **bound** to the store-active account, which is how
    production produces an endpoint-less-under-the-old-design server: a bound
    launch opts out of GApplication uniqueness (`NON_UNIQUE`) and owns no
    app-wide name, so the pre-2026-07-23 focus-existing — an `Activate` on the
    app id — had nobody to call and the chooser surfaced `no_running_instance`.
    (Under e2e *every* launch is `NON_UNIQUE`, so the app-wide name is unowned
    here regardless; binding A is what makes the run match the production shape
    it stands for, and it is also the shape the goal doc names as the open gap
    this channel closes.)

    Asserted from both ends on purpose. The raiser exiting proves the call was
    *delivered* — `raise_account_instance` returns true only when the
    per-account name was owned and its `Activate` returned — but says nothing
    about whether a raise happened; `raises_served` on A proves it *arrived and
    was handled*. A half-wired channel passes either assertion alone: an
    exported endpoint whose flag nothing drains, or a drain nobody can reach.
    """
    seed, user_actor, _admin_actor = _seed_two_accounts(nest_instance)
    world = _shared_instance_world()

    # ONE session bus for both instances — the other half of the same-OS-login
    # premise `_shared_instance_world` establishes for the store. The driver
    # gives every launch a PRIVATE bus by default (convention 10), which is
    # right for every other suite and fatal here: two instances on two buses
    # can never see each other's names, so focus-existing would fail for a
    # reason that has nothing to do with the product. A caller-passed
    # `DBUS_SESSION_BUS_ADDRESS` is the sanctioned carve-out — the bus is still
    # this test's own child, never the ambient desktop session, so the
    # isolation convention's actual guarantee (nothing machine-global leaks in)
    # holds: no service activation, no tray host, nothing but our two apps.
    bus = PrivateSessionBus().start()
    shared_bus = {"DBUS_SESSION_BUS_ADDRESS": bus.address}

    bound_server = create_driver("linux")
    bound_server.launch({
        "app_path": linux_app_path,
        "url": nest_instance["url"],
        "seed_credentials": seed,
        "environment": {
            **_seeded_environment(request, nest_instance),
            "FAUNA_BOUND_ACCOUNT": user_actor,
            **shared_bus,
        },
        **world,
    })
    try:
        served_state = bound_server.wait_for_state(
            lambda s: s.get("session", {}).get("actor_id") == user_actor
            and bool(s.get("session", {}).get("authenticated")),
            timeout=45,
        )
        assert served_state["session"]["actor_id"] == user_actor, (
            "the bound instance must serve its bound account before the "
            f"collision below means anything; got {served_state.get('session')!r}"
        )
        assert (bound_server.get_state() or {}).get("raises_served", 0) == 0, (
            "no raise has been sent yet — a non-zero baseline would make the "
            "assertion below meaningless"
        )

        raiser = create_driver("linux")
        try:
            # No re-seed: the shared store already holds the registry.
            raiser.launch({
                "app_path": linux_app_path,
                "url": nest_instance["url"],
                "environment": {
                    **_seeded_environment(request, nest_instance),
                    **shared_bus,
                },
                **world,
            })
            raiser.wait_for(CHOOSER_ANCHOR, timeout=30)
            expect_exit_after_click(
                raiser,
                FOCUS_EXISTING,
                "focus-existing must reach the instance serving this account "
                "over its per-account endpoint and exit — a bound server owns "
                "no app-wide name, which is the gap the raise channel closes",
            )
        finally:
            raiser.teardown()

        # The receiving end: the raise arrived and was handled, not merely
        # delivered to a name.
        raised = bound_server.wait_for_state(
            lambda s: (s or {}).get("raises_served", 0) >= 1, timeout=15
        )
        assert raised.get("raises_served", 0) >= 1, (
            "the served instance must have handled the raise; got "
            f"raises_served={raised.get('raises_served')!r}"
        )
        assert bool(raised.get("session", {}).get("authenticated")), (
            "raising must not disturb the served session"
        )
    finally:
        bound_server.teardown()
        bus.stop()


@pytest.mark.feature("second-identity-in-its-own-window")
def test_linux_focus_existing_onto_an_instance_that_has_gone_starts_normally(
    nest_instance, linux_app_path, request
):
    """If the instance the user asked to go back to has already gone, the app
    starts normally instead of failing (`account-scoping.md` § Concurrent
    instances → the raise channel, the ratified degrade: re-probe the lock —
    no longer served, continue as a plain launch).

    The test above covers the still-served arm (raise, then exit). This is the
    other arm: the chooser rendered while the sibling was alive, and by the
    time the user chose "go back to it" they had closed it. Quitting then
    would leave them with nothing running. Through the shared decision
    (`fauna_client_accounts::resolve_focus_existing`) the click re-probes the
    lock and, finding it free, routes this process as the plain launch it was
    — authenticated as the account that was served. tui's twin is
    `test_tui_focus_existing_onto_an_instance_that_has_gone_starts_normally`.

    The order is causal, not timed: the first instance is torn down and its
    account lock observed released (the probe the click re-runs) before the
    click."""
    seed, user_actor, _admin_actor = _seed_two_accounts(nest_instance)
    world = _shared_instance_world()

    first = create_driver("linux")
    second = create_driver("linux")
    try:
        first.launch({
            "app_path": linux_app_path,
            "url": nest_instance["url"],
            "seed_credentials": seed,
            "environment": _seeded_environment(request, nest_instance),
            **world,
        })
        _wait_authenticated(first)

        second.launch({
            "app_path": linux_app_path,
            "url": nest_instance["url"],
            "environment": _seeded_environment(request, nest_instance),
            **world,
        })
        second.wait_for(CHOOSER_ANCHOR, timeout=30)

        # The instance the user would go back to goes away — and its account
        # lock with it. `teardown()` returns once the `xvfb-run` wrapper is
        # reaped, which on a loaded box is before `fauna-desktop` itself has
        # finished exiting and the kernel has dropped its lock: clicking then
        # re-probes a lock still held and lands the still-served arm. Wait on
        # the lock itself, the exact probe the click will run.
        first.teardown()
        assert not first.is_app_alive(), "the served instance must be gone"
        lock_path = os.path.join(
            world["xdg_base"], "config", "fauna", f"instance-{user_actor}.lock"
        )
        wait_until(
            lambda: not _lock_held(lock_path),
            APP_RELAUNCH_S,
            diagnose=lambda: f"{lock_path} still held after the served instance was torn down",
        )

        second.click(FOCUS_EXISTING)
        state = second.wait_for_state(
            lambda s: s.get("session", {}).get("actor_id") == user_actor
            and bool(s.get("session", {}).get("authenticated")),
            timeout=45,
        )
        assert state["session"]["actor_id"] == user_actor
        assert second.is_app_alive(), (
            "with nothing left to go back to, focus-existing must start this "
            "process normally, never exit it"
        )
    finally:
        second.teardown()
        first.teardown()
