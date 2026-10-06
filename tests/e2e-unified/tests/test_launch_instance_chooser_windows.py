"""tier_3 e2e: windows launch-collision chooser (`launch_instance_chooser` page,
`docs/goal/architecture/apps/account-scoping.md` § Concurrent instances →
"the colliding instance's surface").

A **plain** launch whose would-be account (the store-active one) is already served
by a live instance renders this chooser instead of authenticating
(``App.DetectLaunchCollision`` / ``ShowLaunchInstanceChooserAsync``). linux
rendered it first (2026-07-22); this is windows' leg, and the companion to
``test_account_instance_lock_windows.py``, which covers the guard the chooser sits
on top of.

Under e2e the ``Local\\FaunaApp-SingleInstance`` mutex is disabled outright
(``SingleInstanceGate.Decide``, the windows twin of linux's ``NON_UNIQUE``), so it
never redirects the second launch away before the collision check runs — this test
does not (and must not) rely on that production redirect. It seeds the served
account and lets the collision check do the work, exactly as linux's does.

Coverage:

1. Instance A plainly launched and authenticated as the registry's active account
   (X). A second plain launch, sharing A's install world, must find X already served
   and render ``launch-instance-chooser`` with exactly the registry's other
   (not-currently-served) accounts as ``launch-instance-chooser-item`` rows. Picking
   one binds THIS (second) process to it (``bind_session_launch_to``) and completes
   routing as that account — **no third process**; spawning a sibling is the
   switcher's own affordance, covered in ``test_account_switcher_windows.py``. The
   first instance is untouched throughout.

2. The chooser's **focus-existing** exit against a **bound** serving instance — the
   per-(OS login, account) raise channel (``account-scoping.md`` § Concurrent
   instances, ratified 2026-07-23). A bound instance owns no app-wide rendezvous
   name, so this exit can reach it only over the per-account activation endpoint
   every serving instance claims. The observable is that the colliding process is
   handed off and exits, with the server still serving afterwards.

3. The same exit when the serving instance has **gone** between the chooser's
   render and the click: the shared decision re-probes the lock, finds it free,
   and this process continues as the plain launch it was.

tier_3: needs a real ``fauna-nest`` binary (``nest_instance``); windows driver only.
"""
from __future__ import annotations

import os
import shutil
import tempfile
import time

import pytest

from common import build_registry_seed, create_actor_and_register
from conftest import _seeded_environment
from drivers import create_driver
from helpers.budgets import APP_RELAUNCH_S
from helpers.waiting import wait_until

pytestmark = [pytest.mark.tier_3, pytest.mark.windows]

CHOOSER_ANCHOR = "launch-instance-chooser"
CHOOSER_ITEM = "launch-instance-chooser-item"
FOCUS_EXISTING = "launch-instance-focus-existing-button"

#: Generous budget for "the raised process finished exiting", polled to a
#: deadline — a green run pays only the real exit latency (conventions point
#: 14). Sized far above any non-pathological WinUI teardown, because the machine
#: routinely runs several parallel builds at once.
EXIT_BUDGET_S = 45.0


def _seed_two_accounts(nest_instance):
    """A claimed-admin account + a freshly-registered regular-user account, active
    on the regular user — the shape ``test_account_instance_lock_windows.py`` and
    ``test_account_switcher_windows.py`` both seed. Returns (seed_map,
    user_actor_id, admin_actor_id)."""
    admin_sk = nest_instance["admin"]["signing_key"]
    admin_actor = bytes(admin_sk.verify_key).hex()
    admin_secret = bytes(admin_sk).hex()

    user = create_actor_and_register(nest_instance["port"], admin_signing_key=admin_sk)
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
    """One throwaway install world for two instances to share — the same-OS-login
    premise the collision check runs under.

    All three keys matter: ``credential_dir`` + ``keyring_app`` make both launches
    read ONE registry, and ``data_dir`` is what makes them probe one set of lock
    files (windows keys the lock off ``AccountStateDir.Base``, i.e.
    ``BackupPaths.DataDir``). Supplying ``data_dir`` also means the driver will not
    delete it (``_owns_data_dir``), so the caller cleans up. A private copy of
    ``test_account_instance_lock_windows.py``'s helper of the same name — this repo
    does not import helpers across test modules."""
    base = tempfile.mkdtemp(prefix="fauna-e2e-win-chooser-world-")
    return base, {
        "credential_dir": os.path.join(base, "credentials"),
        "keyring_app": f"fauna-e2e-chooser-world-{os.path.basename(base)}",
        "data_dir": os.path.join(base, "data"),
    }


def _wait_authenticated(driver, timeout=60):
    return driver.wait_for_state(
        lambda s: bool(s.get("session", {}).get("authenticated")), timeout=timeout
    )


def _reported_error(driver):
    """The app's own error text, read through the state protocol rather than the
    InfoBar element — the windows convention. Best-effort: a process that already
    exited answers nothing, which is exactly the success case here."""
    try:
        return (driver.get_state() or {}).get("messages", {}).get("error")
    except Exception:
        return None


@pytest.mark.feature("second-identity-in-its-own-window")
def test_windows_second_plain_launch_renders_chooser_and_pick_completes_as_that_account(
    nest_instance, windows_app_path, request
):
    """Instance A serves the active account (user); a second plain launch, sharing
    A's install world, must render the chooser rather than authenticate — with
    exactly the OTHER registered account (accounts - served = 1) offered — and
    picking it must complete this (second) process's routing as that account,
    leaving A untouched."""
    seed, user_actor, admin_actor = _seed_two_accounts(nest_instance)
    world_base, world = _shared_instance_world()

    first = create_driver("windows")
    first.launch({
        "app_path": windows_app_path,
        "url": nest_instance["url"],
        "seed_credentials": seed,
        "environment": _seeded_environment(request, nest_instance),
        **world,
    })
    try:
        _wait_authenticated(first)
        # Positive liveness FIRST: the collision the chooser reports is "some live
        # instance holds this account's lock", so a first instance that had quietly
        # died would make the chooser's absence prove nothing (the standing rule —
        # any test whose subject is another process's liveness asserts that liveness
        # explicitly).
        assert first.app_running(), (
            "the served instance must be live, or a rendered chooser proves nothing"
        )

        second = create_driver("windows")
        try:
            # No re-seed: the shared store already holds the registry, and a re-seed
            # would clobber it under the first instance.
            second.launch({
                "app_path": windows_app_path,
                "url": nest_instance["url"],
                "environment": _seeded_environment(request, nest_instance),
                **world,
            })
            second.wait_for(CHOOSER_ANCHOR, timeout=45)
            assert second.count(CHOOSER_ITEM) == 1, (
                "with 2 registered accounts and 1 (the user) already served, the "
                "chooser must offer exactly the other one (the admin)"
            )

            second.click(CHOOSER_ITEM, index=0)
            state = second.wait_for_state(
                lambda s: s.get("session", {}).get("actor_id") == admin_actor
                and bool(s.get("session", {}).get("authenticated")),
                timeout=60,
            )
            assert state["session"]["actor_id"] == admin_actor, (
                "picking the only offered row must bind THIS process to the admin "
                f"account — no third process; got {state.get('session')!r}"
            )

            # The already-served instance is untouched by the collision + pick.
            first_state = first.get_state()
            assert (
                first_state.get("session", {}).get("actor_id") == user_actor
                and bool(first_state.get("session", {}).get("authenticated"))
            ), (
                "the already-served instance must be untouched by a colliding "
                f"sibling's pick; got {first_state.get('session')!r}"
            )
        finally:
            second.teardown()
    finally:
        first.teardown()
        shutil.rmtree(world_base, ignore_errors=True)


@pytest.mark.feature("second-identity-in-its-own-window")
def test_windows_focus_existing_raises_a_bound_sibling_over_the_per_account_channel(
    nest_instance, windows_app_path, request
):
    """focus-existing must reach a **bound** serving instance and end this process.

    The account-scoping guarantee under test (``account-scoping.md`` § Concurrent
    instances → *The per-(OS login, account) raise channel*, ratified 2026-07-23):
    every serving instance — plain **and bound** — claims
    ``Local\\FaunaApp-Activate-<token>``, and focus-existing targets that endpoint
    *uniformly*.

    Why the server must be BOUND for this to prove anything: a bound launch
    deliberately skips ``SingleInstanceManager.TryClaim`` and so owns **no** app-wide
    rendezvous name. Against the pre-leg code — which raised over the app-wide
    ``Local\\FaunaApp-Activate`` — this click had nobody to call, surfaced
    ``no_running_instance`` on ``error-message``, and left the second process alive.
    A *plain* server would have masked that entirely, because it happens to own the
    app-wide name too. So the discriminator is precisely: **does the colliding
    process exit?**

    Note the raise is asserted by its *effect on this process* (it hands the user
    off and exits), not by watching pixels move on the other window: the exit is a
    state transition the harness can observe, and the serving instance's continued
    health is asserted beside it so a "raise" that killed the wrong process cannot
    pass."""
    seed, user_actor, _admin_actor = _seed_two_accounts(nest_instance)
    world_base, world = _shared_instance_world()

    first = create_driver("windows")
    # BOUND to the store-active account: it serves `user_actor` (so a plain launch
    # collides on it) while owning no app-wide name (so only the per-account
    # endpoint can reach it).
    first.launch({
        "app_path": windows_app_path,
        "url": nest_instance["url"],
        "seed_credentials": seed,
        "environment": {
            **_seeded_environment(request, nest_instance),
            "FAUNA_BOUND_ACCOUNT": user_actor,
        },
        **world,
    })
    try:
        first_state = _wait_authenticated(first)
        assert first_state["session"]["actor_id"] == user_actor, (
            "the bound instance must serve the account the second launch will "
            f"collide on; got {first_state.get('session')!r}"
        )
        # Positive liveness FIRST: the payoff assertion is that the SECOND process
        # dies, and a first instance that had quietly died would free the lock,
        # render no chooser at all, and make the whole test vacuously green.
        assert first.app_running(), (
            "the bound server must be live, or a raised-and-exited sibling proves nothing"
        )

        second = create_driver("windows")
        raised = False
        try:
            second.launch({
                "app_path": windows_app_path,
                "url": nest_instance["url"],
                "environment": _seeded_environment(request, nest_instance),
                **world,
            })
            second.wait_for(CHOOSER_ANCHOR, timeout=45)

            second.click(FOCUS_EXISTING)

            # Deadline poll on latency-independent state — the process is either
            # gone or it is not; no settle-sleep, and a green run pays only the
            # real teardown latency.
            deadline = time.monotonic() + EXIT_BUDGET_S
            while time.monotonic() < deadline and second.app_running():
                time.sleep(0.5)
            raised = not second.app_running()

            assert raised, (
                "focus-existing against a BOUND sibling must raise it over the "
                "per-account activation endpoint and exit this process. It is still "
                "running, which is the pre-leg failure: the raise went to the "
                "app-wide name that a bound instance never claims. App-reported "
                f"error: {_reported_error(second)!r}"
            )

            # Causal link, asserted on the RECEIVING side, and NOT redundant with the
            # exit assertion above — do not delete it. Planting the pre-leg app-wide
            # raise proved the exit alone is satisfiable the WRONG way: on a box where
            # the installed production app is live, it owns `Local\FaunaApp-Activate`
            # (per-logon-session, and the installed app is not under the e2e bridge),
            # so the app-wide raise finds *that* window, and the colliding process
            # exits "successfully" having raised the installed app instead of this
            # bound server. Only this line — the bound server's own record that it got
            # the activation — distinguishes "raised the right instance" from "raised
            # something". (Both instances share `data_dir`, hence one log file.)
            log = first.app_log_text() or ""
            assert "[instance-endpoint] raised for" in log, (
                "the bound server must have RECEIVED the activation, not merely "
                "owned a name the raiser could open. Log tail:\n" + log[-4000:]
            )

            # The raise must not have taken the server down with it — otherwise
            # "the colliding process exited" would pass for a crash.
            still = first.get_state()
            assert (
                still.get("session", {}).get("actor_id") == user_actor
                and bool(still.get("session", {}).get("authenticated"))
            ), (
                "the raised instance must still be serving its account afterwards; "
                f"got {still.get('session')!r}"
            )
        finally:
            second.teardown()
    finally:
        first.teardown()
        shutil.rmtree(world_base, ignore_errors=True)


def _lock_held(lock_path: str) -> bool:
    """Whether a live process holds `lock_path` — the product's own
    `AccountInstanceLock::is_served` probe, windows spelling: the Rust lock is a
    whole-file `LockFileEx`, so a non-blocking byte-range try on byte 0 through
    a fresh handle conflicts exactly while a holder lives, and is released at
    once. A missing file is free. linux's twin probes with `flock`."""
    import msvcrt

    try:
        fd = os.open(lock_path, os.O_RDWR)
    except FileNotFoundError:
        return False
    try:
        msvcrt.locking(fd, msvcrt.LK_NBLCK, 1)
    except OSError:
        return True
    else:
        msvcrt.locking(fd, msvcrt.LK_UNLCK, 1)
    finally:
        os.close(fd)
    return False


@pytest.mark.feature("second-identity-in-its-own-window")
def test_windows_focus_existing_onto_an_instance_that_has_gone_starts_normally(
    nest_instance, windows_app_path, request
):
    """If the instance the user asked to go back to has already gone, the app
    starts normally instead of failing (`account-scoping.md` § Concurrent
    instances → the raise channel, the ratified degrade: re-probe the lock — no
    longer served, continue as a plain launch).

    The test above covers the still-served arm (raise, then exit). This is the
    other arm: the chooser rendered while the sibling was alive, and by the time
    the user chose "go back to it" they had closed it. Through the shared
    decision (`fauna_client_accounts::resolve_focus_existing`, reached over
    UniFFI by `LaunchCollisionGate.ResolveFocusExisting`) the click finds the
    per-account endpoint unowned, re-probes the lock and, finding it free,
    routes this process as the plain launch it was — authenticated as the
    account that was served. tui's twin is
    `test_tui_focus_existing_onto_an_instance_that_has_gone_starts_normally`,
    linux's `test_linux_focus_existing_onto_an_instance_that_has_gone_starts_normally`.

    The order is causal, not timed: the first instance is torn down and its
    account lock observed released (the probe the click re-runs) before the
    click."""
    seed, user_actor, _admin_actor = _seed_two_accounts(nest_instance)
    world_base, world = _shared_instance_world()

    first = create_driver("windows")
    second = create_driver("windows")
    try:
        first.launch({
            "app_path": windows_app_path,
            "url": nest_instance["url"],
            "seed_credentials": seed,
            "environment": _seeded_environment(request, nest_instance),
            **world,
        })
        _wait_authenticated(first)
        assert first.app_running(), (
            "the served instance must be live, or a rendered chooser proves nothing"
        )

        second.launch({
            "app_path": windows_app_path,
            "url": nest_instance["url"],
            "environment": _seeded_environment(request, nest_instance),
            **world,
        })
        second.wait_for(CHOOSER_ANCHOR, timeout=45)

        # The instance the user would go back to goes away — and its account lock
        # with it. Wait on the lock itself, the exact probe the click will run:
        # the process being reaped is not the same moment as the kernel dropping
        # its handle's lock. Only the FIRST app: a default teardown also kills every
        # other instance owning the shared data dir, which is the chooser itself.
        first.teardown(sweep_data_dir_peers=False)
        assert not first.app_running(), "the served instance must be gone"
        lock_path = os.path.join(world["data_dir"], f"instance-{user_actor}.lock")
        wait_until(
            lambda: not _lock_held(lock_path),
            APP_RELAUNCH_S,
            diagnose=lambda: f"{lock_path} still held after the served instance was torn down",
        )

        second.click(FOCUS_EXISTING)
        state = second.wait_for_state(
            lambda s: s.get("session", {}).get("actor_id") == user_actor
            and bool(s.get("session", {}).get("authenticated")),
            timeout=60,
        )
        assert state["session"]["actor_id"] == user_actor
        assert second.app_running(), (
            "with nothing left to go back to, focus-existing must start this process "
            f"normally, never exit it; app-reported error: {_reported_error(second)!r}"
        )
    finally:
        second.teardown()
        first.teardown()
        shutil.rmtree(world_base, ignore_errors=True)
