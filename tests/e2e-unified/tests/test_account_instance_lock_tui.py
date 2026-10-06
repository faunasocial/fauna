"""tier_3 e2e: tui (OS login, account) instance lock — the first RETIRED app.

tui's leg onto the shared ``fauna_client_accounts::AccountInstanceLock``
(`docs/goal/architecture/apps/account-scoping.md` § Concurrent instances).
Since W5.6 (account-data-plane.md § Workstreams) (2026-08-15) tui serves ``ServingMode::Concurrent``: same-account
instances **coexist** over the multi-process-safe account store, each holding
a *shared* per-account lock, and the launch-collision refusal is retired —
what survives exclusive is the three critical sections (schema migration, the
engine-singleton role, the conversations-engine role lock beside
``mls_state.db``) plus the mode-independent bound-or-refuse gate.

Coverage:

1. Two plain launches of the install's ONLY account in one shared install
   world → the second renders the chooser with nothing to offer (the raise
   channel survives as plain-launch UX, and the not-currently-served filter
   must never offer the served account — a *shared* holder still reads as
   served, which this case now also pins end-to-end). The first instance is
   untouched.
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
3. A bound launch naming a DIFFERENT account than the store-active one →
   **coexists**, exactly as case 2's already-served account does: the second
   instance authenticates as its OWN bound account while the first keeps
   serving the active one. This exact launch shape asserted a terminal
   refusal until this fix — tui had no bound session build yet (falling back
   to the active account's session is what bound-or-refuse actually forbade),
   not a design distinction from case 2.
4. **The binding follows the account** — the bound seat of case 2's world
   runs the identity-theft succession ceremony to completion: the process
   stays alive (until 2026-08-27 bound-or-refuse re-fired on the successor's
   id and exited it mid-ceremony), renders the successor's closing-act kit,
   comes back up authenticated as the successor, and keeps its sweep view —
   which, from the seat whose engine the first instance's role lock refused,
   is the genuine ``no_engine`` arm no other journey produces.
5. **A bound launch follows the chain** — after case 4's ceremony a THIRD
   instance launched bound to the RETIRED id comes up as the successor: the
   binding names the account, the account is the successor now, and the
   registry resolves it before the session account does (rider 2 of the same
   ruling; until 2026-08-27 that launch exited as ``BoundMismatch``).

⚠ History: case 1 asserted a terminal refusal until 2026-08-02 (overtaken by
the ratified chooser), case 2 until 2026-08-15 (overtaken by the ratified
W5.6 retirement), case 3 until this fix (tui's bound launch resolved the
store-active account's material rather than its own — the same one-app-
lagging gap linux closed 2026-07-23 — so `session::stored_account` fed the
mode-independent bound-or-refuse gate a mismatched actor and it fired
correctly on the WRONG cause). When one of these reds, read the goal doc
before "fixing" the app — three times now the test was the stale side.

tier_3: needs a real ``fauna-nest`` binary (``nest_instance``); tui driver only.
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

pytestmark = [pytest.mark.tier_3, pytest.mark.tui]

CHOOSER_ANCHOR = "launch-instance-chooser"
CHOOSER_ITEM = "launch-instance-chooser-item"

#: The muted-words term case 2 converges across the two instances.
CONVERGED_TERM = "coexist-probe"

#: Ceiling for cross-instance store convergence: the writer's synchronous
#: dual write + the reader's `data_version` observer poll (a 10 s cadence) +
#: a page re-read. A deadline poll exits the moment the term appears, so a
#: green run pays only the real latency; the ceiling is sized far above any
#: non-pathological delay (convention 14), never to a quiet machine.
CONVERGENCE_BUDGET_S = 90.0


def _seed_one_account(nest_instance):
    """A SINGLE registered regular-user account, seeded active — the shape the
    "nowhere else to go" collision needs: with the install's only account
    served, the chooser's honest offer list is empty. Returns
    (seed_map, user_actor_id)."""
    admin_sk = nest_instance["admin"]["signing_key"]
    user = create_actor_and_register(nest_instance["port"], admin_signing_key=admin_sk)
    user_actor = user["actor_id_hex"]

    seed = build_registry_seed(
        [
            {"actor_id": user_actor, "secret_hex": bytes(user["signing_key"]).hex(),
             "nest_url": nest_instance["url"],
             "device_id": "instance-user", "handle": "user"},
        ],
        active=user_actor,
    )
    return seed, user_actor


def _seed_two_accounts(nest_instance):
    """A claimed-admin account + a freshly-registered regular-user account,
    active on the regular user (the shape ``test_account_switcher_tui.py``
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
    guard. tui's driver otherwise gives every launch a fresh ``mkdtemp`` base
    (convention 10), which would put the two processes in different installs
    and so contend on nothing; pinning all three knobs is what makes them one
    install reading one registry and one lock file under
    ``<xdg_base>/config/fauna-tui/``."""
    base = tempfile.mkdtemp(prefix="fauna-e2e-tui-instance-world-")
    return {
        "xdg_base": os.path.join(base, "xdg"),
        "credential_dir": os.path.join(base, "credentials"),
        "keyring_app": f"fauna-e2e-tui-instance-world-{os.path.basename(base)}",
    }


def _wait_authenticated(driver):
    return driver.wait_for_state(
        lambda s: bool(s.get("session", {}).get("authenticated")), timeout=45
    )


def _wait_authenticated_as(driver, actor_id_hex):
    """Like ``_wait_authenticated``, but also waits for ``session.actor_id`` to
    resolve to ``actor_id_hex`` in the SAME poll tick (mirrors linux's twin —
    ``test_account_instance_lock_linux.py``)."""
    return driver.wait_for_state(
        lambda s: s.get("session", {}).get("actor_id") == actor_id_hex
        and bool(s.get("session", {}).get("authenticated")),
        timeout=45,
    )


def test_tui_second_plain_launch_cannot_become_the_only_served_account(
    nest_instance, tui_app_path, request
):
    """A single-account install, already served: a second PLAIN launch must
    never silently become that account. It renders the chooser with NOTHING
    to offer (the only honest list — the one account is taken) and leaves the
    first untouched.

    Under the W5.6 successor this is the raise channel surviving as
    plain-launch UX: a plain collision still routes to the chooser (a second
    same-account instance is an explicit act — the bound launch of case 2 —
    never the accidental outcome of an icon re-click), and the ``0 rows``
    assertion is the sharpest available check on the not-currently-served
    filter, which now also pins end-to-end that a *shared* holder still reads
    as served — a filter that leaked would offer the served account itself,
    precisely the row that must never appear."""
    seed, user_actor = _seed_one_account(nest_instance)
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
            # Reaching the chooser at all is the survival proof: a refused
            # launch exits before any frame paints.
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
def test_tui_bound_launch_onto_the_served_account_coexists(
    nest_instance, tui_app_path, request
):
    """**The W5.6 success condition** (`account-data-plane.md` § Multi-instance
    concurrency; `account-scoping.md` § Concurrent instances): a bound launch
    onto the account a live instance already serves passes the bound-or-refuse
    gate AND the (now shared) instance lock — two same-account tui instances
    run concurrently against one store dir. Three observables, each a half the
    other two cannot fake:

    1. **Coexistence** — the second instance authenticates as the account and
       the first stays live and authenticated (until 2026-08-15 this exact
       launch was terminally refused).
    2. **Convergence** — a muted-words term added through the FIRST instance's
       UI becomes visible in the SECOND (the multi-process-safe store + the
       W5.2 ``data_version`` observer poll; the mutation is a driver UI action
       per convention 8, the wait a deadline poll per convention 14).
    3. **The honest conversations refusal** — the first instance holds the
       conversations-engine role lock over the shared ``mls_state.db``, so the
       second's conversations page surfaces the ruled "served in another
       instance" on ``error-message`` (never a silent unwired page), while
       everything else in it — the muted-words leg above ran in that very
       process — works normally."""
    seed, user_actor = _seed_one_account(nest_instance)
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
                "environment": {
                    **_seeded_environment(request, nest_instance),
                    "FAUNA_BOUND_ACCOUNT": user_actor,
                },
                **world,
            })
            # Observable 1 — coexistence. Authenticating at all is the flip's
            # survival proof: the pre-W5.6 law exited this process before any
            # frame painted.
            state = _wait_authenticated(second)
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
def test_tui_bound_launch_for_another_account_coexists_as_that_account(
    nest_instance, tui_app_path, request
):
    """Two accounts, two live instances, each authenticated as ITS OWN — the
    whole point of the (OS login, account) unit (mirrors linux's twin exactly,
    `test_account_instance_lock_linux.py`).

    The load-bearing assertion is ``session.actor_id``, not merely that a
    second instance came up. A bound launch that read the store-active
    account would route the launch machine on the bound
    account and then build the session from the WRONG one — a second instance
    that looks right and IS the wrong account. That failure is silent by
    construction, so nothing but comparing the two live instances' resolved
    actors catches it (`session::stored_account`/`session::launch_persistence`
    now resolve the bound account's own material via
    `AccountRegistry::resolve_launch_binding` +
    `session_material`/`bound_launch_persistence`, never `active`).

    This case asserted a terminal refusal until this fix (tui had no bound
    session build yet, per the file's own prior docstring); the refusal was a
    statement about a missing build, not about the design — the same
    transition linux made 2026-07-23."""
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
        first_state = _wait_authenticated_as(first, user_actor)
        assert first_state["session"]["actor_id"] == user_actor, (
            "the plain instance must run as the store-active account"
        )

        second = create_driver("tui")
        try:
            # No re-seed (the world holds the registry): the two accounts'
            # scoped state lives under distinct per-actor dirs.
            second.launch({
                "app_path": tui_app_path,
                "url": nest_instance["url"],
                "environment": {
                    **_seeded_environment(request, nest_instance),
                    "FAUNA_BOUND_ACCOUNT": admin_actor,
                },
                **world,
            })
            second_state = _wait_authenticated_as(second, admin_actor)
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


@pytest.mark.feature("take-your-account-back")
def test_tui_a_bound_instance_survives_its_own_succession(
    nest_instance, tui_app_path, request
):
    """**The binding follows the account** (`account-scoping.md` § Concurrent
    instances → *The binding follows the account*): the bound seat of the
    two-instance world runs "my identity was stolen" to completion.

    A binding names an ACCOUNT by the actor id that identified it at launch;
    the ceremony re-points that account to a successor and switches to it. Until
    2026-08-27 the shared holder compared the successor's id against a binding
    still on the retired one and exited the process — mid-ceremony, on the one
    device the ceremony's closing act and the sweep's retry affordance exist
    for. Not data loss (the successor seed is persisted before the sweep runs;
    a relaunch came back as the successor), but the shown-once kit render and
    the in-memory sweep view died with the process. Four observables, in the
    order the ceremony produces them:

    1. **Survival, and the closing act.** The successor's fresh kit lands on
       screen in the SAME process (read first and with no navigation — the
       shown-once custody rule — exactly as the ceremony journey reads it).
    2. **The switch.** The Status page settles on a different actor id and
       the session is authenticated as it: the binding moved with the account,
       so the successor passed the gate the predecessor had.
    3. **The sweep view survived**, and it is the ``no_engine`` arm: this seat's
       conversations engine was refused at sign-in by the first instance's
       conversations-engine role lock (case 2's observable 3), so the sweep
       had no engine to run over — the one configuration that produces this
       arm without fault injection, and the one that could never survive
       producing it before. The shared retry-gate helper then asserts the
       button renders on it and that a press answers.
    4. **The first instance is untouched by the second's ceremony** — still
       alive, still serving; what its next connect does about the superseded
       identity is the own-device-fleet leg (`succession-aftermath.md`
       § Propagation), not this case's subject.

    ⚠ Do NOT "simplify" this into a lone bound launch: the first instance is
    what makes the seat *bound* in practice (a plain second launch renders the
    chooser and never authenticates) and what makes the sweep ``no_engine``.
    """
    seed, user_actor = _seed_one_account(nest_instance)
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
                "environment": {
                    **_seeded_environment(request, nest_instance),
                    "FAUNA_BOUND_ACCOUNT": user_actor,
                },
                **world,
            })
            state = _wait_authenticated(second)
            assert state.get("session", {}).get("actor_id") == user_actor, (
                "the bound instance must serve the bound account, "
                f"got {state.get('session')!r}"
            )
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


@pytest.mark.feature("take-your-account-back")
def test_tui_a_launch_bound_to_a_retired_id_comes_up_as_the_successor(
    nest_instance, tui_app_path, request
):
    """**A bound launch follows the chain** (`account-scoping.md` § Concurrent
    instances → *The binding follows the account*, rider 2): a spawn minted
    with the RETIRED id, after this install's registry has recorded the
    succession, comes up as the successor.

    The world is case 4's: the bound seat runs the ceremony, so the install's
    registry now holds the successor's row and the ``succeeded_by`` link, and
    ``active`` moved to the successor. A THIRD instance is then launched with
    ``FAUNA_BOUND_ACCOUNT=<old>`` — the shape of a spawn command minted before
    its spawner could observe the succession. The binding names an account, and
    the account is the successor now: ``resolve_launch_binding`` walks the
    chain and re-points the process binding before tui resolves its session
    (from ``active``, tui having no bound session build), so the holder's
    bound-or-refuse meets the successor on both sides. Until 2026-08-27 this
    launch exited — ``[launch-refused] for <successor>: launched bound to
    <retired> but the session resolved a different account`` — which is what
    the failure message quotes if it comes back.

    The first (plain) seat still serves the retired id throughout; what it
    does about being superseded is the own-device-fleet leg, not this case's.
    """
    seed, user_actor = _seed_one_account(nest_instance)
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
                "environment": {
                    **_seeded_environment(request, nest_instance),
                    "FAUNA_BOUND_ACCOUNT": user_actor,
                },
                **world,
            })
            _wait_authenticated(second)
            app = ActionLayer(second)
            old_actor, _held = succeed_identity_from(app)
            assert old_actor == user_actor
            new_actor = wait_for_successor_actor(app, old_actor, guard_alive=True)
            assert len(new_actor) == SECRET_HEX_LEN and new_actor != old_actor

            # ── the spawn minted with the retired id ─────────────────────────
            third = create_driver("tui")
            try:
                try:
                    third.launch({
                        "app_path": tui_app_path,
                        "url": nest_instance["url"],
                        "environment": {
                            **_seeded_environment(request, nest_instance),
                            "FAUNA_BOUND_ACCOUNT": old_actor,
                        },
                        **world,
                    })
                except RuntimeError as exited_early:
                    raise AssertionError(
                        "a launch bound to the RETIRED id exited instead of following "
                        "the chain to the successor (account-scoping.md § Concurrent "
                        "instances → the binding follows the account, rider 2). "
                        f"Launch error: {exited_early}. Captured stderr:\n"
                        f"{third.app_stderr_text()[-4000:]}"
                    ) from exited_early
                state = wait_alive_until(
                    third,
                    lambda: bool((third.get_state() or {}).get("session", {}).get("authenticated")),
                    45.0,
                    "before the seat bound to the retired id authenticated",
                    diagnose=lambda: f"session={((third.get_state() or {}).get('session'))!r}",
                )
                actor = (third.get_state() or {}).get("session", {}).get("actor_id")
                assert actor == new_actor, (
                    "the seat bound to the retired id must serve the SUCCESSOR — the "
                    f"binding names the account, and the account is {new_actor!r} now; "
                    f"got {actor!r}"
                )
                assert (third.get_state() or {}).get("session", {}).get("actor_id") != old_actor, (
                    "a seat serving the retired id would meet the nest's superseded refusal "
                    "on its first connect — the binding must have followed the chain"
                )
            finally:
                third.teardown()

            # The first seat is untouched by either launch.
            assert is_app_alive(first) is not False
        finally:
            second.teardown()
    finally:
        first.teardown()
