"""tier_3 e2e: tui launch-collision chooser (`launch_instance_chooser` page,
`docs/goal/architecture/apps/account-scoping.md` § Concurrent instances →
"the colliding instance's surface").

A **plain** launch whose would-be account (the store-active one) is already
served by a live instance renders this chooser instead of authenticating
(`launch.rs::start_or_offer_chooser`) — mirrors
`test_launch_instance_chooser_linux.py`, the reference implementation this
leg lifts from.

tui's chooser differs from linux/windows in exactly the way its declared
platform absences predict: no `launch-instance-add-account-button` (no
app↔app IPC channel; platform-scoped to `[windows, linux]`, user decision
2026-08-01 — `tui.md` § Declared platform absences #5), and
`launch-instance-focus-existing-button` prints where the account is served
and exits rather than raising a window (tui claims no per-account activation
endpoint — `account-scoping.md` § Concurrent instances → the raise channel).

Coverage:

1. Instance A plainly launched and authenticated as the registry's active
   account (X). A second plain launch, sharing A's install world, must find
   X already served and render `launch-instance-chooser` with exactly the
   registry's other (not-currently-served) accounts as
   `launch-instance-chooser-item` rows. Picking one binds THIS (second)
   process to it (`App::switch_account`, the same mechanism the
   authenticated switcher uses) and completes routing as that account — no
   third process. The first instance is untouched throughout.

2. Focus-existing exits this process without disturbing the served instance
   — there is no receiving end to corroborate on tui (unlike linux/windows'
   raise channel), so `expect_exit_after_click` (liveness-only) is the whole
   assertion, exactly as its docstring prescribes for an exit-inducing click.

tier_3: needs a real `fauna-nest` binary (`nest_instance`); tui driver only.
"""
from __future__ import annotations

import os
import tempfile

import pytest

from common import build_registry_seed, create_actor_and_register
from conftest import _seeded_environment
from drivers import create_driver
from helpers.instance_guard import expect_exit_after_click

pytestmark = [pytest.mark.tier_3, pytest.mark.tui]

CHOOSER_ANCHOR = "launch-instance-chooser"
CHOOSER_ITEM = "launch-instance-chooser-item"
FOCUS_EXISTING = "launch-instance-focus-existing-button"


def _seed_two_accounts(nest_instance):
    """A claimed-admin account + a freshly-registered regular-user account,
    active on the regular user (the shape `test_account_instance_lock_tui.py`
    seeds). Returns (seed_map, user_actor_id, admin_actor_id)."""
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
    """One throwaway install world (XDG base + credential store + keyring
    namespace) for two instances to share — the same-OS-login premise the
    collision check runs under. Mirrors `test_account_instance_lock_tui.py`'s
    helper of the same name (its own private copy — this repo does not
    import test helpers across test modules)."""
    base = tempfile.mkdtemp(prefix="fauna-e2e-tui-chooser-world-")
    return {
        "xdg_base": os.path.join(base, "xdg"),
        "credential_dir": os.path.join(base, "credentials"),
        "keyring_app": f"fauna-e2e-tui-chooser-world-{os.path.basename(base)}",
    }


def _wait_authenticated(driver):
    return driver.wait_for_state(
        lambda s: bool(s.get("session", {}).get("authenticated")), timeout=45
    )


@pytest.mark.feature("second-identity-in-its-own-window")
def test_tui_second_plain_launch_renders_chooser_and_pick_completes_as_that_account(
    nest_instance, tui_app_path, request
):
    """Instance A serves the active account (user); a second plain launch,
    sharing A's install world, must render the chooser rather than
    authenticate — with exactly the OTHER registered account
    (accounts - served = 1) offered — and picking it must complete this
    (second) process's routing as that account, leaving A untouched."""
    seed, user_actor, admin_actor = _seed_two_accounts(nest_instance)
    world = _shared_instance_world()

    first = create_driver("tui")
    first.launch({
        "app_path": tui_app_path,
        "url": nest_instance["url"],
        "seed_credentials": seed,
        "environment": _seeded_environment(request, nest_instance),
        **world,
    })
    try:
        _wait_authenticated(first)

        second = create_driver("tui")
        try:
            # No re-seed: the shared store already holds the registry, and a
            # re-seed would clobber it under the first instance.
            second.launch({
                "app_path": tui_app_path,
                "url": nest_instance["url"],
                "environment": _seeded_environment(request, nest_instance),
                **world,
            })
            second.wait_for(CHOOSER_ANCHOR, timeout=30)
            assert second.count(CHOOSER_ITEM) == 1, (
                "with 2 registered accounts and 1 (the user) already served, "
                "the chooser must offer exactly the other one (the admin)"
            )
            # tui's declared absence: no add-account forward on this platform
            # (`tui.md` § Declared platform absences #5).
            assert second.count("launch-instance-add-account-button") == 0, (
                "launch-instance-add-account-button is windows+linux only "
                "(platform_elements) — tui has no app<->app IPC channel"
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


def test_tui_focus_existing_exits_without_disturbing_the_served_instance(
    nest_instance, tui_app_path, request
):
    """tui claims no per-account activation endpoint and raises no window —
    a declared platform absence (`tui.md` § Declared platform absences: "no
    window manager can raise a terminal"). Its focus-existing exit prints
    where the account is served and exits
    (`account-scoping.md` § Concurrent instances → the raise channel: "tui
    neither claims an endpoint... nor raises — its focus-existing exit
    prints where the account is served and exits").

    Unlike linux/windows there is no receiving end to corroborate (no
    `raises_served` counter — nothing was raised): liveness of the SECOND
    process's exit, and the FIRST instance staying untouched, is the whole
    assertion, exactly as `expect_exit_after_click`'s own docstring
    prescribes for an exit-inducing click ("a dropped reply is not a failure
    here; liveness afterwards is the assertion").
    """
    seed, user_actor, _admin_actor = _seed_two_accounts(nest_instance)
    world = _shared_instance_world()

    first = create_driver("tui")
    first.launch({
        "app_path": tui_app_path,
        "url": nest_instance["url"],
        "seed_credentials": seed,
        "environment": _seeded_environment(request, nest_instance),
        **world,
    })
    try:
        _wait_authenticated(first)

        second = create_driver("tui")
        try:
            second.launch({
                "app_path": tui_app_path,
                "url": nest_instance["url"],
                "environment": _seeded_environment(request, nest_instance),
                **world,
            })
            second.wait_for(CHOOSER_ANCHOR, timeout=30)
            expect_exit_after_click(
                second,
                FOCUS_EXISTING,
                "focus-existing must exit this process — tui has no window "
                "to raise, so printing and exiting is the whole behavior",
            )
        finally:
            second.teardown()

        first_state = first.get_state()
        assert (
            first_state.get("session", {}).get("actor_id") == user_actor
            and bool(first_state.get("session", {}).get("authenticated"))
        ), (
            "the served instance must be untouched by a sibling's "
            f"focus-existing exit; got {first_state.get('session')!r}"
        )
    finally:
        first.teardown()


@pytest.mark.feature("second-identity-in-its-own-window")
def test_tui_focus_existing_onto_an_instance_that_has_gone_starts_normally(
    nest_instance, tui_app_path, request
):
    """If the instance the user asked to go back to has already gone, the app
    starts normally instead of failing (`account-scoping.md` § Concurrent
    instances → the raise channel, the ratified degrade: re-probe the lock —
    no longer served, continue as a plain launch).

    The test above covers the still-served arm (print where it is served, and
    exit). This is the other arm, and the one a user actually meets: the
    chooser rendered while the sibling was alive, and by the time they chose
    "go back to it" they had closed that terminal. Exiting then would print a
    claim that is no longer true and leave them with nothing running. Through
    the shared decision (`fauna_client_accounts::resolve_focus_existing`) the
    click re-probes the lock and, finding it free, routes this process as the
    plain launch it was — authenticated as the account that was served.

    The order is causal, not timed: the first instance is torn down (its
    process gone, so the kernel has released its lock) before the click."""
    seed, user_actor, _admin_actor = _seed_two_accounts(nest_instance)
    world = _shared_instance_world()

    first = create_driver("tui")
    second = create_driver("tui")
    try:
        first.launch({
            "app_path": tui_app_path,
            "url": nest_instance["url"],
            "seed_credentials": seed,
            "environment": _seeded_environment(request, nest_instance),
            **world,
        })
        _wait_authenticated(first)

        second.launch({
            "app_path": tui_app_path,
            "url": nest_instance["url"],
            "environment": _seeded_environment(request, nest_instance),
            **world,
        })
        second.wait_for(CHOOSER_ANCHOR, timeout=30)

        # The instance the user would go back to goes away.
        first.teardown()
        assert not first.is_app_alive(), "the served instance must be gone"

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
