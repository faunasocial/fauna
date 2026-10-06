"""Web sign-out erases the credential namespace and the bearer it minted.

`long-term-store.md` § Cleanup contract: sign-out wipes the client's *whole*
credential namespace — every account's `fauna/{actor_id}/*` slots and the
`fauna/index` blob — not merely the active account. Two properties hang
on that, and web violated both until a fix:

1. **The signed-out secret is gone.** A `fauna/{actor}/secret` that outlives
   sign-out leaves the user's Ed25519 key recoverable in a shared browser.
2. **The next full page load really does route to fresh onboarding.** An index
   that outlives sign-out still names the signed-out account active, so
   `accountsBoot()` boots that account from its still-stored secret —
   silently signing the user back in as the identity they signed out of, and
   shadowing any identity onboarded since.

A third, independent leak rode on the same `goto`: the bearer cache was keyed on
the nest URL with no identity in the key, so an identity signing in *after* the
sign-out — in the same document, since `signOut()` navigates client-side rather
than reloading — was handed the signed-out actor's bearer and talked to the nest
as that account.

**Why this module exists instead of a web arm on
`test_onboarding_launch_routing_smoke.py` case G** (the cross-app cleanup-
contract coverage, green on `[linux, cli]`): that case drives `driver.reset()`,
and web's `reset` sweeps the `fauna/` prefix out of `localStorage` *itself*
(`web-bridge/agent.js`) before navigating. A reset-driven assertion on web would
therefore go green on the strength of the **test agent's** cleanup and stay green
with the product erase reverted — it would pin nothing. Case G's other half
(force-quit → relaunch) has no browser analogue either: tearing a web driver down
discards the whole browser profile, so the namespace it is supposed to inspect
dies with it. Both halves have to be replaced, so the honest shape is a module
that drives the real Settings sign-out UI and reads `localStorage` back through
the page.
"""
from __future__ import annotations

import json

from helpers import web_store
import time

import pytest

from actions import ActionLayer
from common.auth import create_actor_and_register
from drivers import create_driver

pytestmark = [pytest.mark.tier_3, pytest.mark.web]


def _ls(driver, key: str):
    """One `localStorage` value, or None when the key is absent."""
    return driver.eval_js(f"localStorage.getItem({json.dumps(key)})")


def _registry_keys(driver) -> list[str]:
    """Every `fauna/`-prefixed key — the account registry's own namespace, the
    only identity store web has. These are the keys that fix was about.
    """
    return sorted(
        driver.eval_js(
            "Object.keys(localStorage).filter(k => k.startsWith('fauna/'))"
        )
    )


def _seed_registry_identity(driver, *, secret_hex: str, node_url: str) -> str:
    """Write one signed-in identity in the registry shape `identity.init()`
    reads (`helpers/web_store.seed_identity`); the nest URL rides along so the
    silent sign-in on boot has somewhere to go. Returns the actor id."""
    return web_store.seed_identity(driver, secret_hex, nest_url=node_url)


def _wait_registry_active(driver, timeout: float = 30.0) -> str:
    """Poll until `fauna/index` names an active actor, and return it.

    The seed writes the registry shape directly, so this is a readiness check
    on the store the product boots from — what makes the erase assertion below
    meaningful: there is something real to erase.
    """
    deadline = time.monotonic() + timeout
    raw = None
    while time.monotonic() < deadline:
        raw = _ls(driver, "fauna/index")
        if raw:
            active = json.loads(raw).get("active")
            if active:
                return active
        time.sleep(0.5)
    raise AssertionError(
        f"`fauna/index` names no active account within {timeout}s, so there is no "
        f"registry for sign-out to erase and this test would pass vacuously; "
        f"last read: {raw!r}"
    )


@pytest.mark.feature("account")
def test_web_sign_out_erases_the_account_registry(nest_instance, spa_url):
    """Sign out through the UI ⇒ no `fauna/index`, no `fauna/{actor}/secret`.

    Red on `7f9a95590^`: `logout()` removed only the four flat `fauna_*` keys, so
    the index and every per-actor secret survived the erase.
    """
    actor_a = create_actor_and_register(
        nest_instance["port"],
        admin_signing_key=nest_instance["admin"]["signing_key"],
    )
    secret_a = bytes(actor_a["signing_key"]).hex()

    driver = create_driver("web")
    driver.launch({"url": spa_url + "/app/"})
    try:
        _seed_registry_identity(driver, secret_hex=secret_a, node_url=spa_url)
        # A full document load is what boots the SPA over the seeded registry.
        # `hard_reload()` is a plain `location.reload()`; `driver.reset()` would
        # clear the very keys under test.
        driver.hard_reload()

        active = _wait_registry_active(driver)
        assert active == actor_a["actor_id_hex"], (
            f"the registry should name the seeded identity as active; "
            f"got {active!r}, expected {actor_a['actor_id_hex']!r}"
        )
        assert _ls(driver, f"fauna/{active}/secret") == secret_a, (
            "precondition: the identity secret must sit in its per-actor slot — "
            "that slot is what sign-out has to erase"
        )

        ActionLayer(driver).settings.sign_out()

        survivors = _registry_keys(driver)
        assert survivors == [], (
            "sign-out must erase the whole credential namespace, but these registry "
            f"keys survived it: {survivors}. `fauna/index` among them boots the "
            "signed-out account from its stored secret on the next page load, silently "
            "signing the user back in; a surviving `fauna/{actor}/secret` leaves their "
            "Ed25519 key in a shared browser."
        )
        assert _ls(driver, f"fauna/{active}/secret") is None, (
            "the identity secret must be erased with the index"
        )
    finally:
        driver.teardown()


# ── The account store and the enrollment ─────────────────────────────────────
#
# `account-scoping.md` § The scoping taxonomy → *Erasure follows scope*, the
# paragraph "Web's account store is in the erase too", and
# `account-client-lifecycle.md` § The client-side lifecycle → *Ruling (4), the
# teardown rider*. Since web hosts the account runtime, each account has a
# store in the origin — one IndexedDB database and one OPFS directory, both
# named `fauna-account-store/<actor>` — and a writer the nest holds a grant
# for. A sign-out retires the grant, severs the writer on the plane and erases
# the store, in that order behind one durable record.

#: The root every account store's name sits under
#: (`fauna_account_store::root::PLATFORM_ROOT`).
ACCOUNT_STORE_ROOT = "fauna-account-store"

#: The install-scoped sign-out record (`fauna_client_accounts::sign_out_record`)
#: — outside the `fauna/` prefix the registry and the e2e agent sweep.
SIGN_OUT_RECORD_KEY = "fauna_sign_out_record"

#: The second sign-in's fresh replica reading the first writer's `Removed` row:
#: one escrow recovery plus one walk of the account's plane.
REMOVED_VISIBLE_S = 240.0

#: A reload finishing a recorded sign-out: one boot, one credential wipe, one
#: database delete.
RECONCILE_S = 60.0


def _account_store_names(driver) -> dict:
    """Every account store this origin holds, by home: the IndexedDB databases
    and the OPFS root's directories under the account-store root. The OPFS
    directory is the database name folded to one path component, so both are
    matched on the root prefix."""
    return driver.eval_js(
        "(async () => {"
        " const dbs = (await indexedDB.databases()).map(d => d.name);"
        " const dirs = [];"
        " try {"
        "  const root = await navigator.storage.getDirectory();"
        "  for await (const name of root.keys()) dirs.push(name);"
        " } catch (e) { dirs.push('opfs-unreadable: ' + e); }"
        f" const ours = n => String(n).startsWith({json.dumps(ACCOUNT_STORE_ROOT)})"
        " || String(n).startsWith('opfs-unreadable');"
        " return { indexeddb: dbs.filter(ours).sort(), opfs: dirs.filter(ours).sort() };"
        "})()"
    )


@pytest.mark.feature("account")
def test_web_sign_out_retires_the_enrollment_and_erases_the_account_store(
    app, request, nest_instance, test_user
):
    """Sign out through Settings ⇒ the named row carries no grant, the origin
    holds no account store, and the next sign-in's replica reads the signed-out
    writer `removed`.

    Red before the erase and the sign-out stop: the database survived the
    sign-out, the named row kept its principal, and the next sign-in read both
    writers `enrolled` — one stranded fleet member per cycle.
    """
    from conftest import _E2E_LOGIN_DEVICE_ID, _make_user

    from helpers import enrollment, fleet
    from helpers.waiting import wait_until

    nest_url = nest_instance["url"]
    user = _make_user(nest_instance)
    actor = user["actor_id_hex"]
    named_row = _E2E_LOGIN_DEVICE_ID
    driver = app.driver

    enrollment.sign_in(app, request, nest_instance, user)
    writer, _row, _ = enrollment.await_enrollment(app, nest_url, user)
    first_member = fleet.fleet_id_hex(writer)
    before = _account_store_names(driver)
    assert f"{ACCOUNT_STORE_ROOT}/{actor}" in before["indexeddb"], (
        "precondition: the signed-in account's store must exist in this origin — it is "
        f"what the sign-out has to erase; the origin holds {before}"
    )

    app.settings.sign_out()

    after = _account_store_names(driver)
    assert not after["indexeddb"] and not after["opfs"], (
        "a web sign-out must erase the account store of every account it reaches "
        f"(account-scoping.md § Erasure follows scope), but the origin still holds {after}. "
        "The replica rests readable and holds key custody; kept, the next sign-in in this "
        "browser silently re-adopts it."
    )
    assert driver.eval_js(f"localStorage.getItem({json.dumps(SIGN_OUT_RECORD_KEY)})") is None, (
        "the sign-out record must go once every recorded store is gone — a record left "
        "behind re-runs the erase at every later load"
    )
    enrollment.await_grant_cleared(nest_url, user, named_row, "the settings sign-out")

    enrollment.sign_in(app, request, nest_instance, user)
    next_writer, _row, _ = enrollment.await_enrollment(app, nest_url, user)
    assert next_writer != writer, "the sign-out erased the writer key, so this one is fresh"
    fleet.require_device_set_reader(driver)
    last: dict = {}

    def first_writer_removed():
        fleet.poked_pass(driver, what="the second sign-in's fleet walk")
        last.clear()
        last.update(fleet.device_set_state(driver, first_member))
        return last.get("found") and last.get("state") == "removed"

    wait_until(
        first_writer_removed,
        REMOVED_VISIBLE_S,
        interval=1.0,
        diagnose=lambda: (
            f"the signed-out writer {first_member} never read `removed` on the next "
            f"sign-in's replica (last device-set read: {last!r}). `enrolled` = the sign-out "
            "stopped the runtime with the plain shutdown, so the fleet keeps a member "
            "nobody holds the key of (account-data-taxonomy.md § The generation machinery, "
            "clause (4)); grep the browser log for \"the machine's enrollment retirement\"."
        ),
    )


@pytest.mark.feature("account")
def test_web_load_after_a_tab_closed_mid_sign_out_finishes_the_sign_out(
    app, request, nest_instance, test_user
):
    """A tab closed while the sign-out waited for its runtime's stop leaves the
    sign-out record and everything else in place; the next load finds the
    record, wipes the credentials and erases the recorded store before routing,
    and lands on onboarding.

    **The record is seeded, as a documented precondition** (e2e point 8(b)): it
    is exactly the state the gesture leaves before its first await — the record
    naming the reached accounts with the credential wipe still owed — and
    closing a tab inside that window is not something a test can time. The
    journey under test is the load.
    """
    from conftest import _make_user

    from helpers import enrollment
    from helpers.waiting import wait_until

    nest_url = nest_instance["url"]
    user = _make_user(nest_instance)
    actor = user["actor_id_hex"]
    driver = app.driver

    enrollment.sign_in(app, request, nest_instance, user)
    enrollment.await_enrollment(app, nest_url, user)
    before = _account_store_names(driver)
    assert f"{ACCOUNT_STORE_ROOT}/{actor}" in before["indexeddb"], (
        f"precondition: the account's store must exist for the load to erase; got {before}"
    )
    assert _ls(driver, f"fauna/{actor}/secret"), (
        "precondition: the account's secret must still be stored — the tab closed "
        "before the wipe"
    )

    record = json.dumps({"wipe_owed": True, "accounts": [actor]})
    driver.eval_js(
        f"localStorage.setItem({json.dumps(SIGN_OUT_RECORD_KEY)}, {json.dumps(record)})"
    )
    driver.hard_reload()

    driver.wait_for("create-identity-button", timeout=RECONCILE_S)
    last: dict = {}

    def finished():
        last.clear()
        last.update(
            registry=_registry_keys(driver),
            stores=_account_store_names(driver),
            record=_ls(driver, SIGN_OUT_RECORD_KEY),
        )
        return (
            not last["registry"]
            and not last["stores"]["indexeddb"]
            and not last["stores"]["opfs"]
            and last["record"] is None
        )

    wait_until(
        finished,
        RECONCILE_S,
        interval=0.5,
        diagnose=lambda: (
            "a load that finds the sign-out record must finish the sign-out — no registry "
            f"key, no account store, no record — but the origin reads {last}. A surviving "
            "`fauna/index` signs the user back in as the account they confirmed signing out "
            "of (account-scoping.md § Erasure follows scope, the web paragraph, decision 2)."
        ),
    )


# ── The store half is a residue class ────────────────────────────────────────
#
# `account-scoping.md` § Erasure follows scope, the web paragraph, decision 4:
# a database delete waits behind a connection somebody else holds, so the
# delete is bounded, the account stays in the sign-out record, the user is told
# with the shared residue line, and a later load sweeps again. The line rides
# the `sign-out-residue` view on `identity_choice` with the Remove Again copy
# (§ Erasure follows scope → *The residue surface*), and its retry button runs
# the same sweep behind the other-tab refusal.

#: The property the test's own holding connection is parked on.
_HELD_STORE = "__fauna_e2e_held_account_store"

#: The properties a tab's own hold of an engine-role Web Lock is parked on.
_ROLE_RELEASE = "__fauna_e2e_engine_role_release"
_ROLE_DONE = "__fauna_e2e_engine_role_done"

RESIDUE_RETRY_BUTTON = "sign-out-residue-retry-button"


def _hold_account_store(driver, name: str) -> None:
    """Open a second connection to the database `name` and keep it — the fault
    injection. It registers no `versionchange` handler, so it never closes for
    a delete: the browser answers the delete request `blocked` and leaves it
    waiting, which is what a connection another tab holds does."""
    opened = driver.eval_js(
        "(async () => {"
        f" window[{json.dumps(_HELD_STORE)}] = await new Promise((resolve, reject) => {{"
        f"  const request = indexedDB.open({json.dumps(name)});"
        "  request.onsuccess = () => resolve(request.result);"
        "  request.onerror = () => reject(request.error);"
        " });"
        f" return window[{json.dumps(_HELD_STORE)}].name;"
        "})()"
    )
    assert opened == name, f"the holding connection opened {opened!r}, expected {name!r}"


def _release_account_store(driver) -> None:
    driver.eval_js(
        f"(() => {{ window[{json.dumps(_HELD_STORE)}]?.close();"
        f" delete window[{json.dumps(_HELD_STORE)}]; return true; }})()"
    )


def _hold_engine_role(tab, actor: str) -> None:
    """Take `actor`'s conversations-engine role Web Lock in `tab` and keep it —
    what a tab serving the account holds, and the lock the erase probes
    (`$lib/webLocks::engineLockName`). The lock is taken directly because no
    signed-in tab can be arranged here: the sign-out already wiped the
    credentials a tab would sign in with."""
    name = f"fauna.mls.conversations-engine/{actor.lower()}"
    granted = tab.eval_js(
        "(async () => new Promise((granted) => {"
        f" const held = new Promise((release) => {{ window[{json.dumps(_ROLE_RELEASE)}] = release; }});"
        f" window[{json.dumps(_ROLE_DONE)}] = navigator.locks.request("
        f"  {json.dumps(name)}, {{ mode: 'exclusive', ifAvailable: true }},"
        "  (lock) => { granted(lock != null); return lock ? held : undefined; });"
        "}))()"
    )
    assert granted, f"the second tab could not take the engine role lock {name!r}"


def _release_engine_role(tab) -> None:
    tab.eval_js(
        "(async () => {"
        f" window[{json.dumps(_ROLE_RELEASE)}]?.();"
        f" await window[{json.dumps(_ROLE_DONE)}];"
        " return true; })()"
    )


@pytest.mark.feature("account")
def test_web_sign_out_that_cannot_erase_a_store_says_so_and_a_later_load_finishes(
    app, request, nest_instance, test_user
):
    """A sign-out whose store delete is blocked still completes, tells the user
    on the `sign-out-residue` view and keeps the account in the sign-out record;
    once the holder is gone, the next load finishes the erase and says nothing.

    Red before the bounded delete: `IndexedDbBackend::delete` waited on a
    request the browser had answered `blocked`, so `identity.logout()` never
    returned and the Settings sign-out never reached onboarding.
    """
    from conftest import _make_user
    from i18n.strings import S

    from helpers import enrollment
    from helpers.waiting import wait_until

    nest_url = nest_instance["url"]
    user = _make_user(nest_instance)
    actor = user["actor_id_hex"]
    store = f"{ACCOUNT_STORE_ROOT}/{actor}"
    driver = app.driver

    enrollment.sign_in(app, request, nest_instance, user)
    enrollment.await_enrollment(app, nest_url, user)
    assert store in _account_store_names(driver)["indexeddb"], (
        "precondition: the signed-in account's store must exist to be held open"
    )
    _hold_account_store(driver, store)

    expected = S.settings.sign_out_residue(count="1")
    try:
        app.settings.sign_out()

        last: dict = {}

        def told():
            last.clear()
            last.update(
                line=app.onboarding.sign_out_residue_text(),
                error=app.error_text(),
                record=_ls(driver, SIGN_OUT_RECORD_KEY),
            )
            return expected in (last["line"] or "")

        wait_until(
            told,
            RECONCILE_S,
            interval=0.5,
            diagnose=lambda: (
                "a sign-out that could not erase an account store must say so on "
                f"identity_choice's sign-out-residue-message with the shared line {expected!r} "
                f"(account-scoping.md § Erasure follows scope → The residue surface), but it "
                f"reads {last.get('line')!r}; error-message reads {last.get('error')!r}, the "
                f"sign-out record reads {last.get('record')!r} and the origin holds "
                f"{_account_store_names(driver)}."
            ),
        )
        assert "credentials" not in last["line"], (
            "web is out of the credential residue class — `removeItem` cannot fail — so "
            f"its line never names credentials; it reads {last['line']!r}"
        )
        assert driver.is_visible(RESIDUE_RETRY_BUTTON), (
            "the line names Remove Again, so the control must be beside it "
            "(account-scoping.md § Erasure follows scope → the residue surface)"
        )
        assert "Signed out" not in (app.error_text() or ""), (
            "the residue has a surface of its own, so error-message must not carry it too; "
            f"it reads {app.error_text()!r}"
        )
        record = json.loads(_ls(driver, SIGN_OUT_RECORD_KEY) or "null")
        assert record == {"wipe_owed": False, "accounts": [actor]}, (
            "the account whose store is still present must stay in the sign-out record, "
            f"with the credential wipe done; the record reads {record!r}"
        )
        assert _registry_keys(driver) == [], (
            "the credential wipe must not wait for the store erase; these registry keys "
            f"survived: {_registry_keys(driver)}"
        )
        assert store in _account_store_names(driver)["indexeddb"], (
            "the fault injection must still hold: the store is present while the line is up"
        )
    finally:
        _release_account_store(driver)

    driver.hard_reload()
    driver.wait_for("create-identity-button", timeout=RECONCILE_S)
    after: dict = {}

    def finished():
        after.clear()
        after.update(
            stores=_account_store_names(driver),
            record=_ls(driver, SIGN_OUT_RECORD_KEY),
        )
        return (
            not after["stores"]["indexeddb"]
            and not after["stores"]["opfs"]
            and after["record"] is None
        )

    wait_until(
        finished,
        RECONCILE_S,
        interval=0.5,
        diagnose=lambda: (
            "with the holder gone, the next load must finish the erase — no account "
            f"store, no record — but the origin reads {after}."
        ),
    )
    assert not app.onboarding.sign_out_residue_showing(), (
        "a clean re-sweep says nothing, but the residue view reads "
        f"{app.onboarding.sign_out_residue_text()!r}"
    )
    line = app.error_text()
    assert "Signed out" not in (line or ""), (
        f"a clean re-sweep says nothing, but onboarding's error-message reads {line!r}"
    )


@pytest.mark.feature("account")
def test_web_sign_out_residue_retry_refuses_beside_another_tab_and_finishes_the_erase(
    app, request, nest_instance, test_user
):
    """Remove Again runs the sign-out's own sweep over what the record still
    names: refused entirely while another tab serves a recorded account, still
    owing while the store is held, and clean — view, store and record all gone —
    once the holder lets go.

    Every press is observed through a change of the line, never through a wait:
    the refusal replaces the count line, the next press (role free, store still
    held) puts the count line back, and the last one removes the view.

    Red before the view: `sign-out-residue-message` never appeared — web painted
    the `no_retry` copy on `error-message` and had no retry control.
    """
    from conftest import _make_user
    from i18n.strings import S

    from helpers import enrollment
    from helpers.waiting import wait_until

    nest_url = nest_instance["url"]
    user = _make_user(nest_instance)
    actor = user["actor_id_hex"]
    store = f"{ACCOUNT_STORE_ROOT}/{actor}"
    driver = app.driver

    enrollment.sign_in(app, request, nest_instance, user)
    enrollment.await_enrollment(app, nest_url, user)
    assert store in _account_store_names(driver)["indexeddb"], (
        "precondition: the signed-in account's store must exist to be held open"
    )
    _hold_account_store(driver, store)

    owing = S.settings.sign_out_residue(count="1")
    refused = S.settings.sign_out_residue_retry_blocked_other_window
    owed_record = {"wipe_owed": False, "accounts": [actor]}
    last: dict = {}

    def await_line(expected: str, what: str) -> None:
        def reads():
            last.clear()
            last.update(line=app.onboarding.sign_out_residue_text())
            return expected in (last["line"] or "")

        wait_until(
            reads,
            RECONCILE_S,
            interval=0.5,
            diagnose=lambda: (
                f"{what}: sign-out-residue-message must read {expected!r} but reads "
                f"{last.get('line')!r}; error-message reads {app.error_text()!r}, the sign-out "
                f"record reads {_ls(driver, SIGN_OUT_RECORD_KEY)!r} and the origin holds "
                f"{_account_store_names(driver)}."
            ),
        )

    def assert_nothing_erased(what: str) -> None:
        assert store in _account_store_names(driver)["indexeddb"], (
            f"{what}: the store is still held, so it must still be here"
        )
        record = json.loads(_ls(driver, SIGN_OUT_RECORD_KEY) or "null")
        assert record == owed_record, (
            f"{what}: the account must stay in the sign-out record; it reads {record!r}"
        )

    other_tab = None
    try:
        app.settings.sign_out()
        await_line(owing, "after a sign-out whose store delete is blocked")

        # Another tab serves the recorded account: the retry refuses entirely.
        other_tab = driver.open_same_context_tab()
        _hold_engine_role(other_tab, actor)
        app.onboarding.retry_sign_out_residue()
        await_line(refused, "a retry while another tab holds the account's engine role")
        assert_nothing_erased("a refused retry")
        assert driver.is_visible(RESIDUE_RETRY_BUTTON), (
            "the refusal's remedy is pressing Remove Again once more, so it must still show"
        )

        # The role is free again and the store is still held: the retry runs,
        # removes nothing, and says what is left.
        _release_engine_role(other_tab)
        app.onboarding.retry_sign_out_residue()
        await_line(owing, "a retry that ran while the store was still held")
        assert_nothing_erased("a retry that could not remove the store")
    finally:
        _release_account_store(driver)
        if other_tab is not None:
            other_tab.teardown()

    # The holder is gone: the same button finishes the job.
    app.onboarding.retry_sign_out_residue()
    after: dict = {}

    def finished():
        after.clear()
        after.update(
            view=app.onboarding.sign_out_residue_text(),
            showing=app.onboarding.sign_out_residue_showing(),
            stores=_account_store_names(driver),
            record=_ls(driver, SIGN_OUT_RECORD_KEY),
        )
        return (
            not after["showing"]
            and not after["stores"]["indexeddb"]
            and not after["stores"]["opfs"]
            and after["record"] is None
        )

    wait_until(
        finished,
        RECONCILE_S,
        interval=0.5,
        diagnose=lambda: (
            "with the holder gone, Remove Again must finish the erase and the view must "
            f"go — no view, no account store, no record — but the page reads {after}."
        ),
    )
    line = app.error_text()
    assert "Signed out" not in (line or "") and "Not removed" not in (line or ""), (
        f"a clean re-sweep says nothing, but onboarding's error-message reads {line!r}"
    )
