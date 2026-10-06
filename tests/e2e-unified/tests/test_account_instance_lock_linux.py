"""tier_3 e2e: linux (OS login, account) instance lock — RETIRED (W5.6 (account-data-plane.md § Workstreams) trickle-down).

Linux's re-keying leg onto the shared ``fauna_client_accounts::AccountInstanceLock``
(`docs/goal/architecture/apps/account-scoping.md` § Concurrent instances):
the per-account lock is acquired in ``app.rs``'s ``AuthSuccess`` arm, before any
of the account's scoped state opens. GApplication's D-Bus uniqueness remains the
raise-on-relaunch layer for PLAIN launches only — under e2e every launch runs
``NON_UNIQUE`` (and a bound launch does so in production too), so these tests
exercise precisely the channel that friendly layer never sees, the same framing
as apple's bare-binary tests vs. LaunchServices.

Since W5.6 (2026-08-15, tui-first; linux the same day) linux serves
``ServingMode::Concurrent``: same-account instances **coexist** over the
multi-process-safe account store, each holding a *shared* per-account lock, and
the launch-collision refusal is retired — what survives exclusive is the three
critical sections (schema migration, the engine-singleton role, the
conversations-engine role lock beside ``mls_state.db``) plus the
mode-independent bound-or-refuse gate.

Coverage (the twin of ``test_account_instance_lock_windows.py``'s set, before
windows' own W5.6 leg lands):

1. A **single-account** install, already served: a second plain launch cannot
   become that account. Since the chooser landed it does not
   *exit* either — it renders the launch-collision chooser with **zero**
   offerable rows (there is no other account to offer), whose remaining exits
   raise the running window or forward an add-account intent. That is the
   ratified design, not a weakening of the guard: the plain-launch terminal
   refusal was explicitly subsumed by the chooser ("focus-existing ...
   subsuming the pre-re-key raise-on-relaunch UX of the platform guards").
   What still holds absolutely, and is what this asserts, is that the second
   process **never authenticates as the served account**.
2. **The W5.6 success condition** — a bound launch (``FAUNA_BOUND_ACCOUNT``)
   onto the account a live instance already serves **coexists**: both
   authenticate as the account; a muted-words term added through the first
   instance's UI becomes visible in the second (the shared store +
   ``data_version`` poll — an update in one visible in the other); and the
   second's conversations page refuses honestly on ``error-message`` ("served
   in another instance" — the conversations-engine role lock held by the
   first) while the first's conversations stay live. This exact launch shape
   asserted a terminal refusal until 2026-08-15 — overtaken by the ratified
   W5.6 retirement, not by a regression.
3. A bound launch for a DIFFERENT account **coexists** — two live instances,
   two accounts, each authenticated as its own.
4. **The binding follows the account** — the bound seat of case 2's world
   runs the identity-theft succession ceremony to completion: it stays up,
   renders the successor's kit, comes back authenticated as the successor and
   keeps its ``no_engine`` sweep view (the first instance holds the
   conversations-engine role lock).
5. **A bound launch follows the chain** — after case 4's ceremony a THIRD
   instance launched bound to the RETIRED id comes up as the successor.

Cases 4 and 5 are the twins of ``test_account_instance_lock_tui.py``'s,
added 2026-09-19 as witnesses of behaviour linux already had: the bound
reads go through ``session_binding`` (``main.rs``, the shared
``AccountRegistry::resolve_launch_binding``), and the ceremony's
``record_succession`` re-points the binding before the in-process switch.

⚠ Cases 1 and 3 both changed shape under ratified design changes that landed
after this module was written, and both were red on ``origin/main`` when
reconciled here (2026-07-23) — case 1 when the chooser subsumed the plain-launch
refusal, case 3 when linux adopted the shared per-account session read (the
``session_material`` bound route) and a bound launch for a non-active account
started *working* instead of refusing. Case 2 changed shape a second time on
2026-08-15, overtaken by the ratified W5.6 retirement (mirrors
``test_account_instance_lock_tui.py``'s case 2 history exactly). None of these
reds was a regression; each was the ratified behavior meeting an assertion
written before it. The companion ``test_launch_instance_chooser_linux.py``
covers the multi-account side of the same collision, so between them the
collision is pinned in both the "somewhere else to go" and "nowhere else to
go" shapes.

tier_3: needs a real ``fauna-nest`` binary (``nest_instance``); linux driver only.
"""
from __future__ import annotations

import os
import tempfile
import time

import pytest

from actions import ActionLayer
from common import build_registry_seed, create_actor_and_register
from conftest import _seeded_environment
from drivers import create_driver
from helpers.instance_guard import is_app_alive, wait_alive_until
from helpers.succession_ceremony import (
    SECRET_HEX_LEN,
    SUCCESSION_AND_RELAUNCH_S,
    wait_for_successor_actor,
    kit_on_screen,
    succeed_identity_from,
)
from helpers.succession_retry import (
    assert_the_retry_affordance_matches_the_sweep,
    sweep_owes_work,
)
from i18n.strings import S

pytestmark = [pytest.mark.tier_3, pytest.mark.linux]

CHOOSER_ANCHOR = "launch-instance-chooser"
CHOOSER_ITEM = "launch-instance-chooser-item"

#: The muted-words term case 2 converges across the two instances.
CONVERGED_TERM = "coexist-probe"

#: Ceiling for cross-instance store convergence — see
#: ``test_account_instance_lock_tui.py``'s twin constant for the derivation
#: (the writer's synchronous dual write + the reader's ``data_version``
#: observer poll + a page re-read; a deadline poll exits the moment the term
#: appears, so a green run pays only the real latency).
CONVERGENCE_BUDGET_S = 90.0


def _seed_one_account(nest_instance):
    """A SINGLE registered regular-user account, seeded active — the shape a
    first-run install lands in, and the one where a collision has nothing else
    to offer."""
    admin_sk = nest_instance["admin"]["signing_key"]
    user = create_actor_and_register(nest_instance["port"], admin_signing_key=admin_sk)
    user_actor = user["actor_id_hex"]
    user_secret = bytes(user["signing_key"]).hex()
    seed = build_registry_seed(
        [{"actor_id": user_actor, "secret_hex": user_secret,
          "nest_url": nest_instance["url"], "device_id": "instance-user",
          "handle": "user"}],
        active=user_actor,
    )
    return seed, user_actor


def _seed_two_accounts(nest_instance):
    """A claimed-admin account + a freshly-registered regular-user account,
    active on the regular user (the shape ``test_account_switcher_linux.py``
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
             "device_id": "instance-user", "handle": "user"},
            {"actor_id": admin_actor, "secret_hex": admin_secret, "nest_url": url,
             "device_id": "instance-admin", "handle": "admin"},
        ],
        active=user_actor,
    )
    return seed, user_actor, admin_actor


def _shared_instance_world():
    """One throwaway install world (XDG base + credential store + keyring
    namespace) for two instances to share — the same-OS-login premise of the
    guard. Pins exactly the keys ``preserve_state_across_relaunch`` pins, so
    both launches read one registry and contend on one lock file under
    ``<xdg_base>/config/fauna/``."""
    base = tempfile.mkdtemp(prefix="fauna-e2e-linux-instance-world-")
    return {
        "xdg_base": os.path.join(base, "xdg"),
        "credential_dir": os.path.join(base, "credentials"),
        "keyring_app": f"fauna-e2e-instance-world-{os.path.basename(base)}",
    }


def _wait_authenticated(driver):
    return driver.wait_for_state(
        lambda s: bool(s.get("session", {}).get("authenticated")), timeout=45
    )


def _wait_authenticated_as(driver, actor_id_hex):
    """Like ``_wait_authenticated``, but also waits for ``session.actor_id`` to
    resolve to ``actor_id_hex`` in the SAME poll tick — `update_shared_state`
    (`main.rs`) flips `authenticated` the instant `AppState` is bound, a poll
    tick before `AppState.actor_id` itself is populated, so a bare
    `_wait_authenticated` followed by an immediate separate `actor_id` read can
    observe `None` (convention 14: fix the brittle assert, don't schedule
    around it — this raced under load even for `first`, a plain launch
    untouched by W5.6, so a fixed sleep would not have helped either)."""
    return driver.wait_for_state(
        lambda s: s.get("session", {}).get("actor_id") == actor_id_hex
        and bool(s.get("session", {}).get("authenticated")),
        timeout=45,
    )


def test_linux_second_plain_launch_cannot_become_the_only_served_account(
    nest_instance, linux_app_path, request
):
    """A single-account install, already served: a second plain launch must
    never become that account. It renders the chooser with NOTHING to offer
    (that is the only honest list — the one account is taken) and leaves the
    first untouched.

    This is the "nowhere else to go" half of the collision. Asserting an empty
    list rather than a dead process is what keeps the test honest about the
    ratified behavior change while still pinning the invariant that matters: at
    most one instance per account. The ``0 rows`` assertion is also the sharpest
    available check on the not-currently-served filter — a filter that leaked
    would offer the served account itself, which is precisely the row that must
    never appear."""
    seed, user_actor = _seed_one_account(nest_instance)
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
            # Reaching the chooser at all is the survival proof: a refused
            # launch exits before any window paints.
            second.wait_for(CHOOSER_ANCHOR, timeout=30)
            assert second.count(CHOOSER_ITEM) == 0, (
                "the install's only account is already served, so the chooser "
                "must offer nothing — the served account must NEVER be offered"
            )
            # `None` is an expected reading, not a lookup failure: a colliding
            # process that never picked has not built a session. Accept either
            # shape and assert the invariant over both.
            session = (second.get_state() or {}).get("session") or {}
            assert not bool(session.get("authenticated")), (
                "the colliding process must not authenticate as the served "
                f"account; got {session!r}"
            )
        finally:
            second.teardown()

        # The collision is the loser's alone: the first instance still serves.
        state = first.get_state()
        assert state.get("session", {}).get("actor_id") == user_actor and bool(
            state.get("session", {}).get("authenticated")
        ), "the surviving instance must be untouched by the colliding launch"
    finally:
        first.teardown()


@pytest.mark.feature("second-identity-in-its-own-window")
def test_linux_bound_launch_onto_the_served_account_coexists(
    nest_instance, linux_app_path, request
):
    """**The W5.6 success condition** (`account-data-plane.md` § Multi-instance
    concurrency; `account-scoping.md` § Concurrent instances): a bound launch
    onto the account a live instance already serves passes the bound-or-refuse
    gate AND the (now shared) instance lock — two same-account linux instances
    run concurrently against one store dir. Three observables, each a half the
    other two cannot fake (mirrors ``test_tui_bound_launch_onto_the_served_
    account_coexists`` exactly — tui is the proven pattern this leg mirrors):

    1. **Coexistence** — the second instance authenticates as the account and
       the first stays live and authenticated (until 2026-08-15 this exact
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
    seed, user_actor = _seed_one_account(nest_instance)
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
        _wait_authenticated_as(first, user_actor)

        second = create_driver("linux")
        try:
            # No re-seed: the shared store already holds the registry, and a
            # re-seed would clobber it under the first instance.
            second.launch({
                "app_path": linux_app_path,
                "url": nest_instance["url"],
                "environment": {
                    **_seeded_environment(request, nest_instance),
                    "FAUNA_BOUND_ACCOUNT": user_actor,
                },
                **world,
            })
            # Observable 1 — coexistence. Authenticating at all is the flip's
            # survival proof: the pre-W5.6 law exited this process before any
            # window painted. Poll for `actor_id` alongside `authenticated`
            # (mirrors `test_linux_bound_launch_for_another_account_coexists_
            # as_that_account`'s established idiom for a bound launch) rather
            # than a bare `_wait_authenticated` — `actor_id` resolves a poll
            # tick after `authenticated` flips true on a bound launch.
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
            second_app.conversations.navigate()
            refusal = second_app.error_text()
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


@pytest.mark.feature("second-identity-in-its-own-window")
def test_linux_bound_launch_for_another_account_coexists_as_that_account(
    nest_instance, linux_app_path, request
):
    """Two accounts, two live instances, each authenticated as ITS OWN — the
    whole point of the (OS login, account) unit.

    The load-bearing assertion is ``session.actor_id``, not merely that a
    second window came up. A bound launch that read the store-active
    account would route the launch machine on the bound account and then build the
    session from the ACTIVE account — a second window that
    looks right and IS the wrong account. That failure is silent by
    construction, so nothing but comparing the two live instances' resolved
    actors catches it. (This case asserted a *refusal* until 2026-07-23, when
    linux adopted the shared per-account session read; the refusal was a
    statement about a missing bound session build, not about the design.)"""
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
        first_state = _wait_authenticated_as(first, user_actor)
        assert first_state["session"]["actor_id"] == user_actor, (
            "the plain instance must run as the store-active account"
        )

        second = create_driver("linux")
        try:
            # No re-seed (the world holds the registry): the two accounts'
            # scoped state lives under distinct per-actor dirs.
            second.launch({
                "app_path": linux_app_path,
                "url": nest_instance["url"],
                "environment": {
                    **_seeded_environment(request, nest_instance),
                    "FAUNA_BOUND_ACCOUNT": admin_actor,
                },
                **world,
            })
            second_state = second.wait_for_state(
                lambda s: s.get("session", {}).get("actor_id") == admin_actor
                and bool(s.get("session", {}).get("authenticated")),
                timeout=45,
            )
            assert second_state["session"]["actor_id"] == admin_actor, (
                "the bound instance must run as its BOUND account, not the "
                f"store-active one; got {second_state.get('session')!r}"
            )

            # And the first is still its own account — the pair is the point.
            first_state = first.get_state()
            assert first_state["session"]["actor_id"] == user_actor and bool(
                first_state["session"]["authenticated"]
            ), (
                "the plain instance must keep serving the active account while "
                f"a bound sibling runs; got {first_state.get('session')!r}"
            )
        finally:
            second.teardown()
    finally:
        first.teardown()


def _bound_launch(nest_instance, linux_app_path, request, bound_actor, world):
    """The launch config of a seat bound to ``bound_actor`` in ``world`` — no
    re-seed: the shared store already holds the registry, and a re-seed would
    clobber it under the first instance."""
    return {
        "app_path": linux_app_path,
        "url": nest_instance["url"],
        "environment": {
            **_seeded_environment(request, nest_instance),
            "FAUNA_BOUND_ACCOUNT": bound_actor,
        },
        **world,
    }


@pytest.mark.feature("take-your-account-back")
def test_linux_a_bound_instance_survives_its_own_succession(
    nest_instance, linux_app_path, request
):
    """**The binding follows the account** (`account-scoping.md` § Concurrent
    instances → *The binding follows the account*): the bound seat of the
    two-instance world runs "my identity was stolen" to completion. Twin of
    ``test_tui_a_bound_instance_survives_its_own_succession`` — read its
    docstring for why each observable matters. Four observables, in the order
    the ceremony produces them:

    1. **Survival, and the closing act** — the successor's fresh kit lands on
       screen in the SAME process, read with no navigation (the shown-once
       custody rule).
    2. **The switch** — the Status page settles on a different actor id and
       the session is authenticated as it. On linux this is the in-process
       switch (`settings::apply_recovery_succeeded` → `trigger_switch_account`),
       whose rebuild passes the bound-or-refuse gate only because
       ``record_succession`` re-pointed the binding first.
    3. **The sweep view survived**, and it is the ``no_engine`` arm: the first
       instance holds the conversations-engine role lock, so this seat's
       ceremony swept with no engine.
    4. **The first instance is untouched** — still alive, still serving.

    ⚠ Do NOT "simplify" this into a lone bound launch: the first instance is
    what makes the sweep ``no_engine``.
    """
    seed, user_actor = _seed_one_account(nest_instance)
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
        _wait_authenticated_as(first, user_actor)

        second = create_driver("linux")
        try:
            second.launch(
                _bound_launch(nest_instance, linux_app_path, request, user_actor, world)
            )
            _wait_authenticated_as(second, user_actor)
            app = ActionLayer(second)

            # ── the ceremony, exactly as the sibling journeys drive it ────────
            old_actor, held = succeed_identity_from(app)
            assert old_actor == user_actor, (
                f"the Status page must render the bound account; got {old_actor!r}"
            )

            # ── 1. survival, and the closing act ─────────────────────────────
            wait_alive_until(
                second,
                lambda: len(kit_on_screen(app)) == SECRET_HEX_LEN,
                SUCCESSION_AND_RELAUNCH_S,
                "before the successor's kit was shown",
                diagnose=lambda: (
                    f"no kit on screen for the successor (reads {kit_on_screen(app)!r}), "
                    f"error={app.error_text()!r}"
                ),
            )
            assert kit_on_screen(app) != held, (
                "the successor must mint a FRESH RecoveryKey — the old one "
                "retired with the old identity"
            )

            # ── 2. the switch ────────────────────────────────────────────────
            new_actor = wait_for_successor_actor(app, old_actor, guard_alive=True)
            assert len(new_actor) == SECRET_HEX_LEN and new_actor != old_actor, (
                f"the successor id is a fresh 64-hex actor id, got {new_actor!r}"
            )
            # `_wait_authenticated_as`, not a bare read: linux flips
            # `authenticated` a poll tick before `actor_id` resolves.
            state = _wait_authenticated_as(second, new_actor)
            assert state.get("session", {}).get("actor_id") == new_actor, (
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


@pytest.mark.feature("take-your-account-back")
def test_linux_a_launch_bound_to_a_retired_id_comes_up_as_the_successor(
    nest_instance, linux_app_path, request
):
    """**A bound launch follows the chain** (`account-scoping.md` § Concurrent
    instances → *The binding follows the account*, rider 2): a spawn minted
    with the RETIRED id, after this install's registry has recorded the
    succession, comes up as the successor. Twin of
    ``test_tui_a_launch_bound_to_a_retired_id_comes_up_as_the_successor``.

    On linux both bound reads (``launch_persistence`` and
    ``launch_credentials``) resolve through ``main.rs::session_binding``, the
    shared ``AccountRegistry::resolve_launch_binding``, which walks the
    ``succeeded_by`` chain to the terminal successor before the session
    account resolves. A launch that exits instead fails with its captured
    stderr in the message.
    """
    seed, user_actor = _seed_one_account(nest_instance)
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
        _wait_authenticated_as(first, user_actor)
        second = create_driver("linux")
        try:
            second.launch(
                _bound_launch(nest_instance, linux_app_path, request, user_actor, world)
            )
            _wait_authenticated_as(second, user_actor)
            app = ActionLayer(second)
            old_actor, _held = succeed_identity_from(app)
            assert old_actor == user_actor
            new_actor = wait_for_successor_actor(app, old_actor, guard_alive=True)
            assert len(new_actor) == SECRET_HEX_LEN and new_actor != old_actor

            # ── the spawn minted with the retired id ─────────────────────────
            third = create_driver("linux")
            try:
                try:
                    third.launch(
                        _bound_launch(nest_instance, linux_app_path, request, old_actor, world)
                    )
                except RuntimeError as exited_early:
                    raise AssertionError(
                        "a launch bound to the RETIRED id exited instead of following "
                        "the chain to the successor (account-scoping.md § Concurrent "
                        "instances → the binding follows the account, rider 2). "
                        f"Launch error: {exited_early}. Captured stderr:\n"
                        f"{third.app_stderr_text()[-4000:]}"
                    ) from exited_early

                def _third_session():
                    return (third.get_state() or {}).get("session") or {}

                wait_alive_until(
                    third,
                    lambda: _third_session().get("actor_id") == new_actor
                    and bool(_third_session().get("authenticated")),
                    45.0,
                    "before the seat bound to the retired id authenticated as the successor",
                    diagnose=lambda: (
                        f"session={_third_session()!r} "
                        f"(retired={old_actor!r}, successor={new_actor!r})"
                    ),
                )
            finally:
                third.teardown()

            # The first seat is untouched by either launch.
            assert is_app_alive(first) is not False
        finally:
            second.teardown()
    finally:
        first.teardown()
