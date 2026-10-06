"""tier_3 e2e: windows (OS login, account) instance scoping.

Windows' leg onto the shared ``fauna_client_accounts::AccountInstanceLock``
(`docs/goal/architecture/apps/account-scoping.md` § Concurrent instances): the
per-account lock is acquired in ``App.OnLaunched`` right after the secret loads into
``ICryptoService`` — the point the session account resolves — and before the MLS
store, feed drafts or backup-coordinator paths derive from ``ActorIdHex``.

**Windows RETIRED its same-account refusal 2026-08-24** (``ServingMode::Concurrent``,
passed across the FFI as ``FfiServingMode.Concurrent`` — windows is the only app
that reaches the holder over UniFFI). Serving now takes a **shared** lock, so
same-account instances coexist and exclusivity survives only in the three genuinely
exclusive critical sections. Case 2 below is the coexistence proof that replaced the
refusal it used to assert.

The ``Local\\FaunaApp-SingleInstance`` mutex is NOT what these tests exercise. It
survives as the plain-launch raise layer but is disabled under ``FAUNA_E2E_BRIDGE``
(the harness runs concurrent instances), and a bound launch skips it in production
too — so, exactly like linux's ``NON_UNIQUE`` framing and apple's bare-binary-vs-
LaunchServices framing, what runs here is the channel the friendly layer never sees.

Coverage:

1. A **single-account** install, already served: a second plain launch cannot become
   that account. Since 2026-07-23 it does not *exit* either — it renders the
   launch-collision chooser with **zero** offerable rows (there is no other account
   to offer), whose remaining exits raise the running window or forward an
   add-account intent. That is the ratified design, not a weakening of the guard:
   the plain-launch terminal refusal was explicitly subsumed by the chooser
   ("focus-existing ... subsuming the pre-re-key raise-on-relaunch UX of the
   platform guards"). What still holds absolutely, and is what this asserts, is that
   the second process **never authenticates as the served account**.
2. A bound launch (``FAUNA_BOUND_ACCOUNT``) onto the account a live instance already
   serves → **coexists** (since 2026-08-24; until then this was the file's one
   terminal refusal). It passes the bound-or-refuse gate — the binding names the
   account the session resolves — and now passes the shared instance lock too, so
   both instances run against one store dir and a write in either converges to the
   other. What still refuses is an *exclusive* holder (the stricter law) and a bound MISMATCH, which is
   mode-independent — neither is reachable from a same-version harness, so no case
   here exercises ``expect_launch_refused`` any more.
3. A bound launch for a DIFFERENT account **coexists** — two live instances, two
   accounts, each authenticated as its own.

⚠ The plain-launch half of case 1 changed shape when the chooser landed. The
companion ``test_launch_instance_chooser_windows.py`` covers the multi-account side
of the same collision (chooser offers the not-currently-served accounts; picking one
binds this process to it), so between them the collision is pinned in both the
"somewhere else to go" and "nowhere else to go" shapes.

Test 3 was windows-only when written — linux's third test could then only assert
that a mismatched binding refuses. It no longer diverges: linux's own bound
session build landed and its case 3 was reconciled to this same coexistence
shape (2026-07-23). Resolving session material through the registry for the
BOUND account is what lets either client assert the thing that actually matters
— and it is the
assertion that catches the blocker apple found the hard way, where the launch
machine routed on the bound account while the session came up as the ACTIVE one.
Asserting ``session.actor_id`` per instance is what separates "a second window
opened" from "a second window opened as the right account".

tier_3: needs a real ``fauna-nest`` binary (``nest_instance``); windows driver only.
"""
from __future__ import annotations

import os
import shutil
import tempfile
import time

import pytest

from actions import ActionLayer
from common import build_registry_seed, create_actor_and_register
from conftest import _seeded_environment
from drivers import create_driver
from helpers.instance_guard import is_app_alive, wait_alive_until
from helpers.succession_ceremony import (
    SUCCESSION_AND_RELAUNCH_S,
    wait_for_successor_actor,
    kit_on_screen,
    succeed_identity_from,
)
from helpers.succession_retry import (
    assert_the_retry_affordance_matches_the_sweep,
    sweep_owes_work,
)
from helpers.waiting import await_session_actor
from i18n.strings import S

pytestmark = [pytest.mark.tier_3, pytest.mark.windows]

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
    first-run install lands in, and the one where a collision has nothing else to
    offer."""
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
    """A claimed-admin account + a freshly-registered regular-user account, active
    on the regular user — the same shape ``test_account_switcher_windows.py`` seeds.
    Returns (seed_map, user_actor_id, admin_actor_id)."""
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
    """One throwaway install world for two instances to share — the same-OS-login
    premise of the guard.

    All three keys matter: ``credential_dir`` + ``keyring_app`` make both launches
    read ONE registry, and ``data_dir`` is what makes them contend on one lock file
    (windows keys the lock off ``AccountStateDir.Base``, i.e. ``BackupPaths.DataDir``).
    Supplying ``data_dir`` also means the driver will not delete it (``_owns_data_dir``),
    so the caller cleans up."""
    base = tempfile.mkdtemp(prefix="fauna-e2e-win-instance-world-")
    return base, {
        "credential_dir": os.path.join(base, "credentials"),
        "keyring_app": f"fauna-e2e-instance-world-{os.path.basename(base)}",
        "data_dir": os.path.join(base, "data"),
    }


def _wait_authenticated(driver, timeout=60):
    return driver.wait_for_state(
        lambda s: bool(s.get("session", {}).get("authenticated")), timeout=timeout
    )


def test_windows_second_plain_launch_cannot_become_the_only_served_account(
    nest_instance, windows_app_path, request
):
    """A single-account install, already served: a second plain launch must never
    become that account. It renders the chooser with NOTHING to offer (that is the
    only honest list — the one account is taken) and leaves the first untouched.

    This is the "nowhere else to go" half of the collision. Asserting an empty list
    rather than a dead process is what keeps the test honest about the ratified
    behavior change while still pinning the invariant that matters: at most one
    instance per account. The ``0 rows`` assertion is also the sharpest available
    check on the not-currently-served filter — a filter that leaked would offer the
    served account itself, which is precisely the row that must never appear."""
    seed, user_actor = _seed_one_account(nest_instance)
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
        # Positive liveness FIRST: the collision this test depends on is "some live
        # instance holds the account's lock", so a first instance that had quietly
        # died would make everything below vacuous (the method note — any test
        # resting on another process's state asserts that state explicitly).
        assert first.app_running(), (
            "the served instance must be live, or the collision below proves nothing"
        )

        second = create_driver("windows")
        try:
            # No re-seed: the shared store already holds the registry, and a
            # re-seed would clobber it under the first instance.
            second.launch({
                "app_path": windows_app_path,
                "url": nest_instance["url"],
                "environment": _seeded_environment(request, nest_instance),
                **world,
            })
            second.wait_for(CHOOSER_ANCHOR, timeout=45)
            # Reaching the chooser at all is the survival proof: a refused launch
            # exits before any window paints. Survival *past* this point is pinned by
            # the sibling chooser test, where the process is clicked and completes —
            # a stronger proof than a liveness poll, and deliberately not duplicated
            # here: `app_running()` reads the bridge's process handle and goes
            # false-negative under heavy machine load, so resting a product assertion
            # on it would buy flake for no coverage.
            assert second.count(CHOOSER_ITEM) == 0, (
                "the install's only account is already served, so the chooser must "
                "offer nothing — the served account must NEVER be offered"
            )
            # `None` is the expected reading, not a lookup failure: the app's test
            # agent starts only once the launch gets PAST the chooser, so a colliding
            # process that never picked has pushed no state at all. Accept either
            # shape and assert the invariant over both.
            session = (second.get_state() or {}).get("session") or {}
            assert not bool(session.get("authenticated")), (
                "the colliding process must not authenticate as the served account; "
                f"got {session!r}"
            )
        finally:
            second.teardown()

        # The collision is the loser's alone: the first instance still serves.
        state = first.get_state()
        assert state.get("session", {}).get("actor_id") == user_actor, (
            "the surviving instance must be untouched by the colliding launch"
        )
        assert bool(state.get("session", {}).get("authenticated")), (
            "the surviving instance must be untouched by the colliding launch"
        )
    finally:
        first.teardown()
        shutil.rmtree(world_base, ignore_errors=True)


@pytest.mark.feature("second-identity-in-its-own-window")
def test_windows_bound_launch_onto_the_served_account_coexists(
    nest_instance, windows_app_path, request
):
    """**The W5.6 (account-data-plane.md § Workstreams) success condition, windows' leg** (`account-data-plane.md`
    § Multi-instance concurrency; `account-scoping.md` § Concurrent instances —
    windows RETIRED 2026-08-24): a bound launch onto the account a live instance
    already serves passes the bound-or-refuse gate AND the (now shared) instance
    lock, so two same-account windows instances run concurrently against one
    store dir. Until this leg landed, this exact launch was terminally refused —
    that is what makes the second instance authenticating at all the flip's
    survival proof.

    Three observables, each a half (or third) the others cannot fake (mirrors
    ``test_linux_bound_launch_onto_the_served_account_coexists`` / apple's
    ``test_apple_bound_launch_onto_the_served_account_coexists``):

    1. **Coexistence** — the second instance authenticates as the account and
       the first stays live and authenticated.
    2. **Convergence** — a muted-words term added through the FIRST instance's
       UI becomes visible in the SECOND, proving the two really do share one
       multi-process-safe store rather than merely both being alive (the
       mutation is a driver UI action per convention 8, the wait a deadline
       poll per convention 14).
    3. **The non-role-holder's honest ``served_elsewhere``** on
       ``error-message`` (read on the SECOND instance BEFORE the muted-words
       leg, so the refusal is proven standing from login rather than an
       artifact of later activity) — and the role holder (the FIRST instance)
       carries no such refusal. Closed 2026-08-30:
       ``App.xaml.cs``'s seeded-launch path (what both logins here use) now
       also builds the real ``ConversationsSession`` under the e2e bridge —
       reusing ``BuildE2eConvSessionAsync``, the same builder the
       ``set_state`` login path already used, so the shared
       ``served_elsewhere`` arming (``nest_client.rs``'s
       ``set_engine_served_elsewhere``, inside
       ``conversations_session_over_manager``) now fires for a seeded launch
       too, mirroring linux ``conv_backend::start_conversations_session``
       ("the real session runs under e2e for EVERY login"). The windows half
       of this observable — reading the flag and rendering it ahead of the
       page's other two truths — was already pinned deterministically by
       ``ConversationsViewModelTests.
       ActiveSendErrorReason_served_elsewhere_outranks_a_standing_page_error``
       against the real FFI; this closes the end-to-end join.
    """
    seed, user_actor, _admin_actor = _seed_two_accounts(nest_instance)
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
        assert first.app_running(), (
            "the liveness probe must report a live instance as running"
        )

        second = create_driver("windows")
        try:
            # No re-seed: the shared world already holds the registry, and a
            # re-seed would clobber it under the first instance.
            second.launch({
                "app_path": windows_app_path,
                "url": nest_instance["url"],
                "environment": {
                    **_seeded_environment(request, nest_instance),
                    "FAUNA_BOUND_ACCOUNT": user_actor,
                },
                **world,
            })

            # Observable 1 — coexistence. Poll for `actor_id` alongside
            # `authenticated` (the established bound-launch idiom in the
            # coexistence case below): `actor_id` resolves a poll tick after
            # `authenticated` flips true on a bound launch.
            state = second.wait_for_state(
                lambda s: s.get("session", {}).get("actor_id") == user_actor
                and bool(s.get("session", {}).get("authenticated")),
                timeout=60,
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

            # Observable 3 first — it needs no store round-trip, and reading it
            # before the muted-words leg proves the refusal is standing from
            # login, not an artifact of later activity. The SECOND instance is
            # the non-holder: the first opened its engine at sign-in over the
            # shared per-account mls_state.db.
            #
            # A plain `error_text()` read right after `navigate()` races the
            # app's own async state serialization: `HttpBridgeDriver.get_state`'s
            # own docstring names a "50-150ms stale window" between a UI action
            # landing and the TestAgent's next state push (confirmed live here —
            # the served-elsewhere flag was already armed on the shared manager
            # by the time `navigate()`'s FlaUI wait returned, but the bridge's
            # cached state snapshot hadn't been re-pushed yet). Use `get_state`'s
            # own `wait_for` poll rather than a fixed sleep (e2e-conventions.md §
            # convention 14). 30s matches this file's other post-launch budgets
            # (CHOOSER_ANCHOR's 45s, `_wait_authenticated`'s 60s) rather than a
            # tight one sized to the sub-100ms happy path — this box's own
            # documented business can stretch even
            # a normally-instant UI update well past a short budget.
            second_app.conversations.navigate()
            refusal = second.get_state(
                "messages.error", wait_for=lambda v: bool(v), timeout=30.0
            ) or ""
            assert S.conversations.errors.served_elsewhere in refusal, (
                "the non-role-holder's conversations page must refuse honestly "
                f"on error-message; got {refusal!r}"
            )

            # Observable 2 — convergence, first → second. The add is a UI action
            # in the first instance; the second polls its own re-read of the
            # same store.
            first_app.muted_words.navigate()
            first_app.muted_words.add(CONVERGED_TERM)
            assert first_app.muted_words.wait_for_row_count(1), (
                f"the add did not land in the writer; "
                f"words={first_app.muted_words.words()!r}"
            )

            deadline = time.monotonic() + CONVERGENCE_BUDGET_S
            seen: list[str] = []
            while True:
                # Re-enter the page each attempt: navigation re-reads the store,
                # so the poll observes durable cross-process state, not a hot
                # in-memory list.
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
            # role holder serves normally. Reading BOTH instances (not just the
            # second's presence above) is what a red-verify demands: an
            # assertion that only ever checked the second for absence would
            # pass just as well with the polarity inverted.
            first_app.conversations.navigate()
            holder_error = first_app.error_text()
            assert S.conversations.errors.served_elsewhere not in (holder_error or ""), (
                f"the role holder must not report served-elsewhere; got {holder_error!r}"
            )
        finally:
            second.teardown()
    finally:
        first.teardown()
        shutil.rmtree(world_base, ignore_errors=True)


@pytest.mark.feature("second-identity-in-its-own-window")
def test_windows_bound_launch_for_another_account_coexists_as_that_account(
    nest_instance, windows_app_path, request
):
    """Two accounts, two live instances, each authenticated as ITS OWN — the whole
    point of the (OS login, account) unit.

    The load-bearing assertion is ``session.actor_id``, not merely that a second
    window came up. A bound launch that read the ACTIVE account's material would
    route the launch machine on the bound account and then build the session from
    the active one — a second window that looks right and IS the wrong
    account. That failure is silent by construction, so nothing but comparing the
    two live instances' resolved actors catches it."""
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
        assert first.get_state()["session"]["actor_id"] == user_actor, (
            "the plain instance must run as the store-active account"
        )

        second = create_driver("windows")
        try:
            # No re-seed (the world holds the registry) and no data_dir clash: the
            # two accounts' scoped state lives under distinct <data_dir>/<actor>/.
            second.launch({
                "app_path": windows_app_path,
                "url": nest_instance["url"],
                "environment": {
                    **_seeded_environment(request, nest_instance),
                    "FAUNA_BOUND_ACCOUNT": admin_actor,
                },
                **world,
            })
            _wait_authenticated(second)

            assert second.get_state()["session"]["actor_id"] == admin_actor, (
                "the BOUND instance must authenticate as its bound account, not the "
                "store-active one — reading the active account here is what makes a "
                "bound launch silently inert"
            )
            # ...and it must not have hijacked the primary on the way.
            first_state = first.get_state()
            assert first_state["session"]["actor_id"] == user_actor, (
                "the primary must still be its own account after a bound sibling starts"
            )
            assert bool(first_state["session"]["authenticated"]), (
                "a bound sibling must not disturb the running primary"
            )
        finally:
            second.teardown()
    finally:
        first.teardown()
        shutil.rmtree(world_base, ignore_errors=True)


@pytest.mark.feature("take-your-account-back")
def test_windows_a_launch_bound_to_a_retired_id_comes_up_as_the_successor(
    nest_instance, windows_app_path, request
):
    """**A bound launch follows the chain** (`account-scoping.md` § Concurrent
    instances → *The binding follows the account*, rider 2): a spawn minted
    with the RETIRED id, after this install's registry has recorded the
    succession, comes up as the successor. Mirrors
    ``test_tui_a_launch_bound_to_a_retired_id_comes_up_as_the_successor`` /
    ``test_apple_a_launch_bound_to_a_retired_id_comes_up_as_the_successor``
    (macOS) — tui is the proven pattern this leg follows. windows' driver
    gained its own ``app_stderr_text()`` — the
    app log + the isolated sync agent's log — so a launch-refusal failure
    here now reports captured stderr too, not just the launcher's own
    exception text.

    The world is this file's own bound-coexistence case
    (``test_windows_bound_launch_onto_the_served_account_coexists``): a
    second (bound) instance onto the account a live first instance already
    serves. That second seat then runs the theft ceremony
    (``test_identity_succession_ceremony.py``'s recipe, proven green on
    windows 2026-08-24), so the install's registry now holds the successor's
    row and the ``succeeded_by`` link, and ``active`` moved to the successor.
    A THIRD instance is then launched with ``FAUNA_BOUND_ACCOUNT=<old>`` —
    the shape of a spawn command minted before its spawner could observe the
    succession. The binding names an account, and the account is the
    successor now: ``FfiAccountRegistry.ResolveLaunchBinding()`` walks the
    chain and re-points the process binding before ``App.xaml.cs``'s
    bind-or-refuse gate reads it, so the gate meets the successor on both
    sides. Before this leg landed, the third seat resolved the retired id,
    passed the gate, and met the nest's ``superseded`` refusal on its first
    connect.

    The first (plain) seat still serves the retired id throughout; what it
    does about being superseded is the own-device-fleet leg, not this case's.
    """
    seed, user_actor = _seed_one_account(nest_instance)
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
        assert first.app_running(), (
            "the served instance must be live, or the collision below proves nothing"
        )

        second = create_driver("windows")
        try:
            # No re-seed: the shared store already holds the registry, and a
            # re-seed would clobber it under the first instance.
            second.launch({
                "app_path": windows_app_path,
                "url": nest_instance["url"],
                "environment": {
                    **_seeded_environment(request, nest_instance),
                    "FAUNA_BOUND_ACCOUNT": user_actor,
                },
                **world,
            })
            second.wait_for_state(
                lambda s: s.get("session", {}).get("actor_id") == user_actor
                and bool(s.get("session", {}).get("authenticated")),
                timeout=60,
            )
            app = ActionLayer(second)

            # ── run the theft ceremony from the bound seat ───────────────────
            old_actor, _held = succeed_identity_from(app)
            assert old_actor == user_actor, (
                f"the bound seat must render its own actor id before the "
                f"ceremony; got {old_actor!r}, error: {app.error_text()!r}"
            )

            new_actor = wait_for_successor_actor(app, old_actor)
            assert new_actor and new_actor != old_actor, (
                f"the bound seat must settle on a NEW actor id; got {new_actor!r}"
            )

            # ── the spawn minted with the retired id ─────────────────────────
            third = create_driver("windows")
            try:
                try:
                    third.launch({
                        "app_path": windows_app_path,
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
                        f"instances → the binding follows the account, rider 2). "
                        f"Launch error: {exited_early}"
                    ) from exited_early
                await_session_actor(
                    third, new_actor, budget_s=45.0,
                    what="the seat bound to the retired id settling as the successor",
                )
                actor = (third.get_state() or {}).get("session", {}).get("actor_id")
                assert actor == new_actor, (
                    "the seat bound to the retired id must serve the SUCCESSOR — the "
                    f"binding names the account, and the account is {new_actor!r} now; "
                    f"got {actor!r}"
                )
            finally:
                third.teardown()

            fstate = first.get_state()
            assert fstate.get("session", {}).get("actor_id") == user_actor and bool(
                fstate.get("session", {}).get("authenticated")
            ), "the first (plain) seat must be untouched by either launch"
        finally:
            second.teardown()
    finally:
        first.teardown()
        shutil.rmtree(world_base, ignore_errors=True)


@pytest.mark.feature("take-your-account-back")
def test_windows_a_bound_instance_survives_its_own_succession(
    nest_instance, windows_app_path, request
):
    """**The binding follows the account** (`account-scoping.md` § Concurrent
    instances → *The binding follows the account*): the bound seat of the
    two-instance world runs "my identity was stolen" to completion.

    Mirrors ``test_tui_a_bound_instance_survives_its_own_succession``
    (`test_account_instance_lock_tui.py`) — the proven pattern this leg
    follows, macOS's own variant (`test_account_switcher_apple.py`) the
    closer structural model. A binding names an ACCOUNT by the actor id that
    identified it at launch; the ceremony re-points that account to a
    successor and switches to it, and — since the shared holder fix — a
    seat bound to the account it is IN THE MIDDLE of re-pointing must not be
    torn down by its own binding re-check mid-ceremony. Four observables, in
    the order the ceremony produces them:

    1. **Survival, and the closing act.** The successor's fresh kit lands on
       screen in the SAME process (read first and with no navigation — the
       shown-once custody rule — exactly as the ceremony journey reads it).
    2. **The switch.** The Status page settles on a different actor id and
       the session is authenticated as it: the binding moved with the
       account, so the successor passed the gate the predecessor had.
    3. **The sweep view survived**, and it is the ``no_engine`` arm: this
       seat's conversations engine was refused at sign-in by the first
       instance's conversations-engine role lock (the coexistence case's own
       observable 3), so the sweep had no engine to run over — the one
       configuration that produces this arm without fault injection. The
       shared retry-gate helper then asserts the button renders on it and
       that a press answers.
    4. **The first instance is untouched by the second's ceremony** — still
       alive, still serving; what its next connect does about the
       superseded identity is the own-device-fleet leg
       (`succession-aftermath.md` § Propagation), not this case's subject.

    ⚠ Do NOT "simplify" this into a lone bound launch: the first instance is
    what makes the seat *bound* in practice (a plain second launch renders
    the chooser and never authenticates) and what makes the sweep
    ``no_engine``.
    """
    seed, user_actor = _seed_one_account(nest_instance)
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

        second = create_driver("windows")
        try:
            # No re-seed: the shared store already holds the registry, and a
            # re-seed would clobber it under the first instance.
            second.launch({
                "app_path": windows_app_path,
                "url": nest_instance["url"],
                "environment": {
                    **_seeded_environment(request, nest_instance),
                    "FAUNA_BOUND_ACCOUNT": user_actor,
                },
                **world,
            })
            state = second.wait_for_state(
                lambda s: s.get("session", {}).get("actor_id") == user_actor
                and bool(s.get("session", {}).get("authenticated")),
                timeout=60,
            )
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
                lambda: len(kit_on_screen(app)) > 0,
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
            assert new_actor and new_actor != old_actor, (
                f"the successor id must be a fresh actor id, got {new_actor!r}"
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
        shutil.rmtree(world_base, ignore_errors=True)
