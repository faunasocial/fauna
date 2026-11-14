"""tier_3 e2e (tui, linux, macos, web): a later retry of a sign-out's leftovers never removes a
store a later sign-in adopted — whichever window retries.

``docs/goal/architecture/apps/account-scoping.md`` § The scoping taxonomy →
*Erasure follows scope* → *the residue surface*, rule 3 (*a path written since
it was recorded is someone's store again*), both of its arms:

* **the base arm** (ruled and built 2026-10-15): a base the sign-out could not
  list carries no fingerprint and is never erased under by a re-sweep — it
  stays on the record, untouched, while it still holds an actor scope. Until
  then the base's fingerprint was its own directory entry, which a later
  sign-in adopting ``<base>/<actor>/account-store/`` never moves, so the next
  re-sweep removed the adopted store, un-pushed rows and all;
* **the actor-scope arm**: a recorded actor scope's fingerprint covers its
  whole tree, so a later sign-in writing one level down (into the surviving
  ``account-store/``) moves it, and the re-sweep drops the scope from the
  record without touching it.

**Why two windows.** A signed-in tui never runs the re-sweep: the launch
re-check runs only with an empty registry (``account_scope::
recheck_residue_at_launch``) and Remove Again lives on the signed-out
``identity_choice``. So the adoption has to happen in ANOTHER window sharing
the store root — the sibling no serving lock can see once it has exited, the
exact case the rule names — while the window holding the residue view stays
signed out. Window A signs out with the fault held and keeps the residue view;
window B (same install world, bound to the same account — the "open as new
instance" shape of ``test_erase_refused_other_instance.py``) signs in, adopts
the surviving store, writes into it and exits; then A presses Remove Again.

B's sign-in is arranged state (a seeded registry, convention 8's precondition
carve-out): the gesture under test is A's Remove Again, driven through the UI.
What B writes is whatever its sign-in writes into the adopted store — the
local tree is what the rule protects (a compose draft syncs to the nest, not
into this tree, so typing one would witness nothing more here); the test
checks that B's write really moved the tree before it relies on it.

**POSIX hosts only** (``host_platform``): both faults are permission bits — an
unlistable base (mode 0o000) and a read-only scope tree (every directory
0o555). Windows has no deny-the-listing bit an unprivileged test can set, and
the held-SQLite-lock shape ``test_sign_out.py`` uses there makes ONE file
undeletable, never a base unlistable.

**tui and linux** (the two-window shape of ``test_erase_refused_other_
instance.py``): both apps keep a flat base under ``XDG_CONFIG_HOME`` beside the
one unified store root (``XdgScopeStore``) and run the shared
``fauna_client_accounts`` residue retry, so one journey parametrized over the
app witnesses both.

**macos** joins with the same two windows: its install world is the state home
+ credential store pair of ``test_erase_refused_other_instance.py``'s macos
world (both windows under one relocated ``HOME``), its flat base is
``AccountStateDir.base`` (``<Application Support>/Fauna``, home of the residue
record) and its store root the ``sync`` sibling beside it (``AppleScopeStore``),
and its Remove Again re-runs the same shared ``fauna_client_accounts`` routine
(``SignOutResidueSurface.finish``). iOS has no leg of this shape: the OS admits
one instance per app (``account-scoping.md`` § Concurrent instances), so no
second window can adopt the store while the first stays signed out.

**android** has one process and one registry, so no window can adopt the store
while another stays signed out: there the promise rests on decision 3 alone
(``account-scoping.md`` § Erasure follows scope → *The sign-out record is on
all 7 apps*, *the registry gate*) — a run nobody asked for, the launch, neither
re-sweeps nor paints while the registry names an account, and Remove Again is
painted only on the signed-out ``identity_choice``. Its arm
(``test_android_…``) is that gate's witness: the same two faults, injected as
the app's own uid with ``run-as`` (the e2e build is debuggable) on the account
store container (``files/account-store``, ``AccountStores.kt``); the sign-in
again is a seeded relaunch (the precondition carve-out window B uses), and a
second relaunch stands in for the later window. Nothing the sign-in adopted
and wrote may be lost, the residue record must be left as the sign-out wrote
it (the record waits), and the line must not be painted while signed in —
on the add-account wizard's ``identity_choice`` included.

**web** has no filesystem store root and no unlistable base, so it has the arm
its own residue class takes (§ *Erasure follows scope*, the web paragraph: a
store delete another connection blocks is the residue, and *an account the
registry names again is someone's store again, left untouched and dropped from
the record*). Its two windows are two tabs of one browser context, which share
the origin's registry and stores as two windows of one app share a store root:
tab A signs out while the test holds the account's database open, so the
delete is blocked and the residue view comes up; tab B opens, signs into the
same identity once the hold is let go — the browser runs the delete it had
waiting, so B writes a store of its own — and closes; then A presses Remove
Again. web's every load re-sweeps (decision 2), so B opens while the store is
still held, and B's sign-in itself runs no sweep: A's press is the one that
decides.
"""
from __future__ import annotations

import contextlib
import json
import os
import stat
import sys
import tempfile
import time
from pathlib import Path

import pytest

from actions import ActionLayer
from common import build_registry_seed, create_actor_and_register
from common.scope_store import AppleScopeStore, XdgScopeStore
from conftest import _seeded_environment, get_available_apps
from drivers import create_driver
from helpers import app_surface
from helpers.waiting import await_session_actor
from i18n.strings import S

pytestmark = [pytest.mark.tier_3]


def _desktop(test):
    """The two-window arms' marks — per test, not module-wide, so the android
    arm below is neither selected for these apps nor gated on a host OS."""
    for mark in (
        pytest.mark.tui,
        pytest.mark.linux,
        pytest.mark.macos,
        pytest.mark.host_platform("linux", "macos"),
    ):
        test = mark(test)
    return test

#: The xdg apps and each one's flat base under ``XDG_CONFIG_HOME``
#: (``XdgScopeStore``).
_FLAT_BASE = {"tui": "fauna-tui", "linux": "fauna"}

#: The apps this journey runs on: the xdg pair, and macos (``AppleScopeStore``).
_APPS = (*_FLAT_BASE, "macos")

#: Convention 14: generous ceilings on latency-independent state.
LAUNCH_BUDGET_S = 60.0
LOCAL_BUDGET_S = 30.0

#: The residue record's file under the app's install base
#: (``fauna_client_accounts::SIGN_OUT_RESIDUE_FILE``).
RESIDUE_FILE = "sign-out-residue.json"


@pytest.fixture(params=[a for a in _APPS if a in get_available_apps()])
def residue_app(request):
    """``(app, app_path)``; the app's id lands in the test name (``[tui]``/
    ``[linux]``/``[macos]``), which is what conftest's ``--app`` filter reads."""
    return request.param, request.getfixturevalue(f"{request.param}_app_path")


def _world(app):
    """One throwaway install world for both windows (``test_erase_refused_
    other_instance._shared_instance_world``'s xdg shape)."""
    base = tempfile.mkdtemp(prefix=f"fauna-e2e-{app}-residue-world-")
    if app == "macos":
        return {
            "home": os.path.join(base, "home"),
            "credential_dir": os.path.join(base, "credentials"),
        }
    world = {
        "xdg_base": os.path.join(base, "xdg"),
        "credential_dir": os.path.join(base, "credentials"),
        "keyring_app": f"fauna-e2e-{app}-residue-world-{os.path.basename(base)}",
    }
    if sys.platform == "darwin":
        # tui on macOS relocates its install home per launch, and the account
        # store follows that home (`drivers/tui.py::build_launch_env`), not
        # `XDG_CONFIG_HOME` — so the two windows share it only when the world
        # pins it, the way the macos arm above pins its `home`.
        world["home"] = os.path.join(base, "home")
    return world


def _seed(nest_instance, actor, secret, device):
    return build_registry_seed(
        [{"actor_id": actor, "secret_hex": secret, "nest_url": nest_instance["url"],
          "device_id": device, "handle": device}],
        active=actor,
    )


def _launch(app, path, nest_instance, request, world, seed, *, bound=None):
    environment = _seeded_environment(request, nest_instance)
    if bound is not None:
        environment = {**environment, "FAUNA_BOUND_ACCOUNT": bound}
    driver = create_driver(app)
    driver.launch({
        "app_path": path,
        "url": nest_instance["url"],
        "environment": environment,
        "seed_credentials": seed,
        **world,
    })
    return driver


def _scope_roots(app, driver) -> tuple[Path, Path]:
    """``(flat, store_root)`` of this window's install world: the base holding
    the residue record, and the unified store root holding each actor scope."""
    if app == "macos":
        roots = AppleScopeStore(app, driver.app_support_dir()).scope_roots()
    else:
        # The store root the driver's own launch resolved (`<home>/Library/
        # Application Support/Fauna/sync` for tui on macOS, `%LOCALAPPDATA%`
        # on windows, XDG on linux) — never re-derived from `config_home`.
        roots = XdgScopeStore(
            app, driver.config_home, _FLAT_BASE[app],
            getattr(driver, "_resolved_store_root", None),
        ).scope_roots()
    return roots[0], roots[-1]


def _wait_serving(driver, actor):
    return driver.wait_for_state(
        lambda s: s.get("session", {}).get("actor_id") == actor
        and bool(s.get("session", {}).get("authenticated")),
        timeout=LAUNCH_BUDGET_S,
    )


def _tree(root: Path) -> dict[str, tuple[bool, int, int]]:
    """Every entry under `root`: relative path → (is_dir, size, mtime_ns)."""
    out: dict[str, tuple[bool, int, int]] = {}
    if not root.exists():
        return out
    for p in sorted(root.rglob("*")):
        with contextlib.suppress(OSError):
            st = p.lstat()
            out[str(p.relative_to(root))] = (p.is_dir(), st.st_size, st.st_mtime_ns)
    return out


def _record(flat: Path) -> dict | None:
    try:
        return json.loads((flat / RESIDUE_FILE).read_text())
    except (OSError, ValueError):
        return None


def _recorded(record: dict | None, flat: Path) -> dict[str, object]:
    """Recorded path (absolute) → its fingerprint (None for none)."""
    out: dict[str, object] = {}
    for entry in (record or {}).get("paths", []):
        p = Path(entry["path"])
        out[str(p if p.is_absolute() else flat / p)] = entry.get("fingerprint")
    return out


def _deny_or_skip(path: Path, mode: int) -> None:
    """Probe that `mode` really denies this process, else skip as an
    environment fact (root writes and lists through any mode bit)."""
    if mode & stat.S_IRUSR == 0:
        try:
            os.listdir(path)
        except PermissionError:
            return
    else:
        probe = path / ".fault-injection-probe"
        try:
            probe.touch()
        except PermissionError:
            return
        probe.unlink(missing_ok=True)
    app_surface.skip_environment(
        "this process reads/writes through the injected mode bits (root in a "
        "container?), so the fault is never injected and the test would pass for "
        "the wrong reason"
    )


@contextlib.contextmanager
def _modes(paths_and_modes):
    """Set each path's mode; restore every original on exit (a mode change moves
    ctime, never mtime, and the residue fingerprint reads no ctime)."""
    originals = [(p, stat.S_IMODE(p.stat().st_mode)) for p, _ in paths_and_modes]
    try:
        for p, mode in paths_and_modes:
            os.chmod(p, mode)
        yield
    finally:
        for p, mode in reversed(originals):
            with contextlib.suppress(FileNotFoundError):
                os.chmod(p, mode)


def _press_retry_until_not_blocked(a: ActionLayer) -> str:
    """Press Remove Again until the retry is not refused by a sibling that is
    still dying (B's teardown can return before its serving lock is down — a
    refusal then is correct, and the user's own remedy is to press again)."""
    blocked = S.settings.sign_out_residue_retry_blocked_other_window
    deadline = time.monotonic() + LOCAL_BUDGET_S
    while True:
        a.onboarding.retry_sign_out_residue()
        line = a.onboarding.sign_out_residue_text()
        if line != blocked or time.monotonic() >= deadline:
            return line
        a.driver.wait_for("sign-out-residue-retry-button", timeout=LOCAL_BUDGET_S)


def _run(request, nest_instance, residue_app, fault):
    """The shared journey: A signs out under `fault(scope, store_root)`; B adopts
    and writes; A presses Remove Again. Returns what the caller asserts on."""
    user = create_actor_and_register(
        nest_instance["port"], admin_signing_key=nest_instance["admin"]["signing_key"]
    )
    actor, secret = user["actor_id_hex"], bytes(user["signing_key"]).hex()
    app, path = residue_app
    world = _world(app)
    a_driver = b_driver = None
    try:
        a_driver = _launch(app, path, nest_instance, request, world,
                           _seed(nest_instance, actor, secret, "residue-a"))
        _wait_serving(a_driver, actor)
        a = ActionLayer(a_driver)
        flat, store_root = _scope_roots(app, a_driver)
        scope = store_root / actor

        def built() -> bool:
            with contextlib.suppress(OSError):
                return scope.is_dir() and any(scope.iterdir())
            return False

        # A session reads "serving" before its account store is assembled on
        # every app that spawns the assembly (macOS starts the runtime on a task
        # after the bearer mint; shared Rust opens the store on a spawned task),
        # so the scope is waited for on the launch budget, never read at once —
        # `test_erase_refused_other_instance.py`'s poll over the same roots.
        deadline = time.monotonic() + LAUNCH_BUDGET_S
        while not built() and time.monotonic() < deadline:
            time.sleep(0.2)
        assert built(), (
            "precondition: the signed-in window must have built the account's scope "
            f"under the shared store root — that is the store B adopts — but {scope} "
            f"is missing or empty (root holds {sorted(_tree(store_root))[:20]})"
        )

        with fault(scope, store_root):
            a.settings.sign_out()
            line = a.onboarding.sign_out_residue_text()
            deadline = time.monotonic() + LOCAL_BUDGET_S
            while not line and time.monotonic() < deadline:
                line = a.onboarding.sign_out_residue_text()
            assert line, (
                "precondition: the faulted sign-out must leave a residue view up in "
                f"window A; sign-out-residue-message is absent (record="
                f"{_record(flat)!r}, app log tail:\n{a_driver.app_stderr_text()[-3000:]})"
            )
        recorded = _recorded(_record(flat), flat)

        # The fault is lifted. Window B signs into the same account, adopts the
        # surviving store and writes into it, then exits.
        before_b = _tree(scope)
        b_driver = _launch(app, path, nest_instance, request, world,
                           _seed(nest_instance, actor, secret, "residue-b"), bound=actor)
        _wait_serving(b_driver, actor)
        deadline = time.monotonic() + LAUNCH_BUDGET_S
        while _tree(scope) == before_b and time.monotonic() < deadline:
            time.sleep(0.2)
        b_driver.teardown()
        b_driver = None
        adopted = _tree(scope)
        assert adopted and adopted != before_b, (
            "precondition: window B's sign-in must have adopted the surviving store "
            f"and written into it, but {scope} reads the same as before B "
            f"({sorted(before_b)[:20]}) — the retry below would then be erasing a "
            "residue nobody uses, which is correct and witnesses nothing"
        )

        line = _press_retry_until_not_blocked(a)
        return {
            "scope": scope, "store_root": store_root, "recorded": recorded,
            "adopted": adopted, "after": _tree(scope), "line": line,
            "log": a_driver.app_stderr_text(),
        }
    finally:
        if b_driver is not None:
            b_driver.teardown()
        if a_driver is not None:
            a_driver.teardown()


def _lost(adopted, after) -> list[str]:
    return sorted(set(adopted) - set(after))


def _residue_log(log: str) -> str:
    return "\n".join(
        l for l in log.splitlines()
        if "sign-out residue" in l or "sign-out record" in l or "erase:" in l
    )[-4000:]


@_desktop
@pytest.mark.feature("account")
def test_a_residue_retry_never_erases_under_a_base_a_later_sign_in_adopted(
    request, nest_instance, residue_app
):
    """Rule 3's base arm: the shared store root unlistable at sign-out ⇒ the
    record holds the BASE; B adopts ``<root>/<actor>/`` and writes; A's Remove
    Again must leave every entry B left, and keep the line up (the base still
    holds an actor scope, so the residue is still owed by the next sign-out)."""

    def fault(scope, store_root):
        @contextlib.contextmanager
        def ctx():
            with _modes([(store_root, 0o000)]):
                _deny_or_skip(store_root, 0o000)
                yield

        return ctx()

    out = _run(request, nest_instance, residue_app, fault)
    root = str(out["store_root"])
    assert root in out["recorded"], (
        "precondition: the unlistable store root must be on the residue record as "
        f"a BASE — the arm under test — but the record holds {out['recorded']!r}"
    )
    lost = _lost(out["adopted"], out["after"])
    assert not lost, (
        "a residue retry must never erase under a base the sign-out could not list "
        "(account-scoping.md § Erasure follows scope → the residue surface, rule 3's "
        f"base arm), but Remove Again removed what window B's sign-in adopted under "
        f"{out['scope']}: {lost[:30]} (line={out['line']!r}; residue log:\n"
        f"{_residue_log(out['log'])})"
    )
    assert out["recorded"][root] is None, (
        "a recorded base carries no fingerprint — nothing taken at sign-out vouches "
        f"for any scope under it (rule 3's base arm) — but the record gave {root} "
        f"{out['recorded'][root]!r}"
    )
    expected = S.settings.sign_out_residue(count="1")
    assert expected in (out["line"] or ""), (
        "the base still holds an actor scope, so it stays on the record and the "
        f"line stays honest — sign-out-residue-message reads {out['line']!r}, "
        f"expected {expected!r} (residue + sign-out record log:\n"
        f"{_residue_log(out['log'])})"
    )


@_desktop
@pytest.mark.feature("account")
def test_a_residue_retry_leaves_an_actor_scope_a_later_sign_in_wrote_below(
    request, nest_instance, residue_app
):
    """Rule 3's actor-scope arm: the scope tree read-only at sign-out ⇒ the
    record holds ``<root>/<actor>/`` with a whole-tree fingerprint; B writes one
    level down (into the adopted ``account-store/``); A's Remove Again must drop
    the scope from the record untouched — every entry B left still there, and
    the view gone (nothing else was left)."""

    def fault(scope, store_root):
        @contextlib.contextmanager
        def ctx():
            dirs = [scope, *(p for p in scope.rglob("*") if p.is_dir() and not p.is_symlink())]
            with _modes([(d, 0o555) for d in dirs]):
                _deny_or_skip(scope, 0o555)
                yield

        return ctx()

    out = _run(request, nest_instance, residue_app, fault)
    scope = str(out["scope"])
    assert scope in out["recorded"] and out["recorded"][scope] is not None, (
        "precondition: the read-only scope must be on the residue record as an "
        f"actor scope WITH a fingerprint — the arm under test — but the record holds "
        f"{out['recorded']!r}"
    )
    lost = _lost(out["adopted"], out["after"])
    assert not lost, (
        "a residue retry must leave alone an actor scope written since the sign-out "
        "recorded it (account-scoping.md § Erasure follows scope → the residue "
        f"surface, rule 3), but Remove Again removed what window B wrote under "
        f"{scope}: {lost[:30]} (line={out['line']!r}; residue log:\n"
        f"{_residue_log(out['log'])})"
    )
    assert not out["line"], (
        "the adopted scope is dropped from the record and nothing else was left, so "
        f"the view must be gone — it reads {out['line']!r}"
    )


# --- android: one process, one registry — the registry gate's witness -------

#: The account store container android hands shared Rust
#: (`AccountStores.accountStoreContainerDir()`), relative to the app's data
#: directory; each actor's scope is ``<container>/<actor>``.
_ANDROID_CONTAINER = "files/account-store"

#: The residue record under android's flat base (`filesDir`).
_ANDROID_RECORD = f"files/{RESIDUE_FILE}"


def _android_launch(request, nest_instance, seed):
    """Own the android driver (the shared ``app`` fixture's reset would wipe the
    seed — `test_account_switcher_android.py`'s reason too)."""
    from helpers import android_device

    driver = create_driver("android")
    driver.launch({
        "url": nest_instance["url"],
        "seed_credentials": seed,
        "environment": _seeded_environment(request, nest_instance),
        "app_path": request.getfixturevalue("android_app_path"),
        **android_device.run_launch_facts(Path(__file__).resolve().parents[3]),
    })
    return driver


def _android_recorded(raw: bytes | None) -> dict[str, object]:
    """Recorded path → its fingerprint, each path as the app wrote it (absolute
    under the app's data directory, or relative to `filesDir`)."""
    try:
        record = json.loads(raw) if raw else None
    except ValueError:
        return {}
    return {e["path"]: e.get("fingerprint") for e in (record or {}).get("paths", [])}


def _android_lookup(recorded: dict[str, object], rel: str):
    """``(found, fingerprint)`` of the recorded entry naming app-data path
    `rel` (``files/…``), however the record spelled it."""
    tail = rel.removeprefix("files/")
    for path, fingerprint in recorded.items():
        if path.rstrip("/").endswith("/" + rel) or path.rstrip("/") == tail:
            return True, fingerprint
    return False, None


def _run_android(request, nest_instance, arm):
    """A faulted sign-out; the sign-in again (a seeded relaunch); a later
    relaunch. Asserts the shared android half and returns what the caller
    asserts on."""
    user = create_actor_and_register(
        nest_instance["port"], admin_signing_key=nest_instance["admin"]["signing_key"]
    )
    actor, secret = user["actor_id_hex"], bytes(user["signing_key"]).hex()
    scope = f"{_ANDROID_CONTAINER}/{actor}"
    driver = None
    originals: dict[str, int] = {}
    try:
        driver = _android_launch(request, nest_instance,
                                 _seed(nest_instance, actor, secret, "residue-android"))
        await_session_actor(driver, actor, budget_s=LAUNCH_BUDGET_S, what="the seeded launch")
        a = ActionLayer(driver)
        assert driver.app_data_tree(scope), (
            "precondition: the signed-in app must have built the account's scope in "
            "its account store container — the store the sign-in again adopts — but "
            f"{scope} is missing or empty ({driver.app_data_listing(_ANDROID_CONTAINER)[:20]})"
        )

        if arm == "base":
            container_mode = driver.app_data_dir_modes(_ANDROID_CONTAINER)[_ANDROID_CONTAINER]
            originals = {_ANDROID_CONTAINER: container_mode}
            faulted = {_ANDROID_CONTAINER: 0o000}
        else:
            originals = driver.app_data_dir_modes(scope)
            faulted = {d: 0o555 for d in originals}
        try:
            driver.app_data_chmod(faulted)
            probe = _ANDROID_CONTAINER if arm == "base" else scope
            if not driver.app_data_denies(probe, listing=arm == "base"):
                app_surface.skip_environment(
                    "the app's own uid reads/writes through the injected mode bits on "
                    "this device, so the fault is never injected and the test would "
                    "pass for the wrong reason"
                )
            a.settings.sign_out()
            line = a.onboarding.sign_out_residue_text()
            deadline = time.monotonic() + LOCAL_BUDGET_S
            while not line and time.monotonic() < deadline:
                line = a.onboarding.sign_out_residue_text()
            assert line, (
                "precondition: the faulted sign-out must leave the residue view up on "
                "identity_choice; sign-out-residue-message is absent (record="
                f"{driver.app_data_read(_ANDROID_RECORD)!r}, app log tail:\n"
                f"{driver.app_stderr_text()[-3000:]})"
            )
        finally:
            driver.app_data_chmod(originals)
        record = driver.app_data_read(_ANDROID_RECORD)
        recorded = _android_recorded(record)

        # Sign in to the same account again: a seeded relaunch (the launch
        # config still carries the seed), which adopts the surviving store and
        # writes into it — under a registry that names the account, so the
        # launch's finish must skip the re-sweep.
        before = driver.app_data_tree(scope)
        assert driver.recover(), "the relaunch that signs in again did not come up"
        await_session_actor(driver, actor, budget_s=LAUNCH_BUDGET_S, what="the sign-in again")
        deadline = time.monotonic() + LAUNCH_BUDGET_S
        adopted = driver.app_data_tree(scope)
        while adopted == before and time.monotonic() < deadline:
            time.sleep(0.2)
            adopted = driver.app_data_tree(scope)
        assert adopted and adopted != before, (
            "precondition: the sign-in again must have adopted the surviving store "
            f"and written into it, but {scope} reads the same as before it "
            f"({sorted(before)[:20]}) — nothing a later run could erase was written"
        )

        # A later launch, signed in (the store pinned across it).
        assert driver.preserve_state_across_relaunch(), "the android relaunch pin was refused"
        assert driver.recover(), "the later relaunch did not come up"
        await_session_actor(driver, actor, budget_s=LAUNCH_BUDGET_S, what="the later launch")
        after = driver.app_data_tree(scope)
        log = driver.app_stderr_text()

        lost = _lost(adopted, after)
        assert not lost, (
            "a launch while the registry names an account must not re-sweep the "
            "residue record (account-scoping.md § Erasure follows scope → the "
            "sign-out record is on all 7 apps, decision 3, the registry gate), but "
            f"what the sign-in again adopted under {scope} is gone: {lost[:30]} "
            f"(record={record!r}; residue log:\n{_residue_log(log)})"
        )
        now = driver.app_data_read(_ANDROID_RECORD)
        assert now == record, (
            "while the registry names an account the launch neither re-sweeps nor "
            "paints — the record waits (decision 3) — but the residue record moved "
            f"across the two signed-in launches: {record!r} → {now!r}"
        )
        assert not a.onboarding.sign_out_residue_showing(), (
            "the residue line must not be painted while signed in (decision 3)"
        )

        # The add-account wizard's identity_choice is signed in too: no line.
        driver.set_state({"nav": {"stack": [{"view": "settings"},
                                            {"view": "settings", "id": "account"}]}})
        driver.wait_for("account-add-button", timeout=LOCAL_BUDGET_S)
        driver.click("account-add-button")
        driver.wait_for("create-identity-button", timeout=LOCAL_BUDGET_S)
        assert not a.onboarding.sign_out_residue_showing(), (
            "the add-account wizard mounts identity_choice while the registry names "
            "an account: Remove Again must not be reachable there (decision 3), but "
            f"sign-out-residue is painted ({a.onboarding.sign_out_residue_text()!r})"
        )
        return {"recorded": recorded, "scope": scope}
    finally:
        if driver is not None:
            with contextlib.suppress(Exception):
                driver.app_data_chmod(originals)
            driver.teardown()
            # The device outlives the test: leave no residue record for a later
            # test's signed-out identity_choice to paint.
            with contextlib.suppress(Exception):
                driver.app_data_remove([_ANDROID_RECORD, scope, f"files/{actor}",
                                        f"databases/{actor}"])


@pytest.mark.android
@pytest.mark.feature("account")
def test_android_a_launch_never_re_sweeps_a_base_a_later_sign_in_adopted(
    request, nest_instance
):
    """Rule 3's base arm on android: the account store container unlistable at
    sign-out ⇒ the record holds it as a BASE with no fingerprint; the sign-in
    again adopts ``<container>/<actor>/`` and writes; no later launch removes
    any of it, and the record waits."""
    out = _run_android(request, nest_instance, "base")
    found, fingerprint = _android_lookup(out["recorded"], _ANDROID_CONTAINER)
    assert found and fingerprint is None, (
        "precondition: the unlistable container must be on the residue record as a "
        "BASE (no fingerprint) — the arm under test — but the record holds "
        f"{out['recorded']!r}"
    )


@pytest.mark.android
@pytest.mark.feature("account")
def test_android_a_launch_never_re_sweeps_an_actor_scope_a_later_sign_in_wrote_below(
    request, nest_instance
):
    """Rule 3's actor-scope arm on android: the scope tree read-only at
    sign-out ⇒ the record holds ``<container>/<actor>/`` with a fingerprint;
    the sign-in again writes one level down; no later launch removes any of
    it, and the record waits."""
    out = _run_android(request, nest_instance, "actor_scope")
    found, fingerprint = _android_lookup(out["recorded"], out["scope"])
    assert found and fingerprint is not None, (
        "precondition: the read-only scope must be on the residue record as an "
        "actor scope WITH a fingerprint — the arm under test — but the record holds "
        f"{out['recorded']!r}"
    )


# ── web: two tabs of one browser context ─────────────────────────────────────

#: A load or a press finishing a recorded sign-out: one boot, one credential
#: wipe, one database delete (`test_sign_out_web.RECONCILE_S`).
RECONCILE_S = 60.0


@pytest.mark.web
@pytest.mark.feature("account")
def test_a_residue_retry_leaves_a_store_another_tab_signed_back_into(
    app, request, nest_instance
):
    """Rule 3's web arm: a recorded account the registry names again — tab B
    signed back into it after A's sign-out left its store behind — is someone's
    store again. A's Remove Again must leave B's store whole and drop the
    account from the record, and the view goes (nothing else was left)."""
    from conftest import _make_user

    from helpers import enrollment, web_store
    from helpers.waiting import wait_until

    user = _make_user(nest_instance)
    actor = user["actor_id_hex"]
    store = f"{web_store.ACCOUNT_STORE_ROOT}/{actor}"
    tab_a = app.driver
    owing = S.settings.sign_out_residue(count="1")

    def record():
        return json.loads(web_store.read_key(tab_a, web_store.SIGN_OUT_RECORD_KEY) or "null")

    def await_line(a: ActionLayer, what: str) -> None:
        last: dict = {}

        def reads():
            last["line"] = a.onboarding.sign_out_residue_text()
            return owing in (last["line"] or "")

        wait_until(
            reads, RECONCILE_S, interval=0.5,
            diagnose=lambda: (
                f"{what}: sign-out-residue-message must read {owing!r} but reads "
                f"{last.get('line')!r}; error-message {a.error_text()!r}, record "
                f"{record()!r}, origin {web_store.account_store_names(tab_a)}"
            ),
        )

    enrollment.sign_in(app, request, nest_instance, user)
    enrollment.await_enrollment(app, nest_instance["url"], user)
    assert store in web_store.account_store_names(tab_a)["indexeddb"], (
        "precondition: the signed-in account's store must exist to be held open"
    )

    tab_b = None
    web_store.hold_account_store(tab_a, store)
    try:
        app.settings.sign_out()
        await_line(app, "tab A, after a sign-out whose store delete is blocked")
        # Opened while the store is still held: B's load re-sweeps too, and its
        # delete waits behind the same holder, so the record keeps the account.
        tab_b = tab_a.open_same_context_tab()
        b = ActionLayer(tab_b)
        await_line(b, "tab B's load, over the still-held store")
    finally:
        # The browser runs the deletes it had waiting: the old store goes.
        web_store.release_account_store(tab_a)
    assert record() == {"wipe_owed": False, "accounts": [actor]}, (
        "precondition: the account whose store survived the sign-out must be on the "
        f"sign-out record — the residue A's press retries — but it reads {record()!r}"
    )

    try:
        # B signs back into the identity and writes a store of its own.
        enrollment.sign_in(b, request, nest_instance, user)
        enrollment.await_enrollment(b, nest_instance["url"], user)
        written = web_store.account_store_counts(tab_b, store)
        assert written and any(written.values()), (
            "precondition: tab B's sign-in must have written the account's store, but "
            f"it reads {written!r} — the retry below would then be erasing nothing "
            "anybody uses, which is correct and witnesses nothing"
        )
    finally:
        if tab_b is not None:
            tab_b.teardown()
    adopted = web_store.account_store_counts(tab_a, store)
    named = web_store.read_index(tab_a) or {}
    assert actor in json.dumps(named) and record() == {
        "wipe_owed": False, "accounts": [actor]
    }, (
        "precondition: with B closed, the registry must name the account again while "
        f"the record still owes it — registry {named!r}, record {record()!r}; B's "
        "sign-in must run no sweep, so A's press is the one that decides"
    )

    app.onboarding.retry_sign_out_residue()
    after: dict = {}

    def settled() -> bool:
        after.update(
            showing=app.onboarding.sign_out_residue_showing(),
            line=app.onboarding.sign_out_residue_text(),
            record=record(),
            counts=web_store.account_store_counts(tab_a, store),
        )
        return not after["showing"] and after["record"] is None

    wait_until(
        settled, RECONCILE_S, interval=0.5,
        diagnose=lambda: (
            "the account the registry names again leaves the record and nothing else "
            f"was left, so Remove Again must clear the record and the view — {after}"
        ),
    )
    lost = sorted(
        name for name, count in (adopted or {}).items()
        if (after["counts"] or {}).get(name, 0) < count
    )
    assert after["counts"] is not None and not lost, (
        "a residue retry must leave alone a store an account signed back into "
        "(account-scoping.md § Erasure follows scope → the residue surface, rule 3; "
        "the web paragraph: an account the registry names again is someone's store "
        f"again), but Remove Again took {store!r} from {adopted!r} to "
        f"{after['counts']!r} (object stores that lost records: {lost})"
    )
