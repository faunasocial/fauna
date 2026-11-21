"""The direct package's sync agent registers its first on-demand root right after the
deployment: a folder the user binds through the packaged app becomes a sync root under
the package's own identity, and the root connects — whatever number of refusals the OS
answers the agent's `Register` with first.

Goal doc: `docs/goal/architecture/installers/windows.md` § Direct download (MSIX app +
nest MSI) → *5. Identity, coexistence and data*, the *Removal* bullet: `Register` from
inside a freshly deployed package answers `0x80070490` at first and then registers,
measured there through a probe (`test_direct_package_install_witness.py`, item 4),
because the production agent registers a root only for a nest-backed on-demand binding,
which that witness cannot mint. The retry under test is owned by
`docs/goal/behavior/file-sync.md` § the registration lifecycle: a refused registration
is tried again within the call (`fauna_cfapi::SHELL_REGISTER_ATTEMPTS`), and a root
still refused is connected later at the serve loop's connect-retry edge
(`bridge::Connected::Retry`). This witness is the product's own path, which the probe
stood in for: a packaged agent, started by the packaged app, registering the root of a
folder bound through the app's UI right after the install.

What runs, and why it is the package's own mechanism
---------------------------------------------------
* **The package.** The Debug FaunaApp, which carries the e2e seam the sign-in goes
  through (convention 15 keeps it out of a release artifact), registered as a full
  package of the DIRECT channel by the packer's own `render_manifest` and `_stage`
  (`helpers/taskbar_badge.py::registered_package`, deployment path 3: Developer Mode,
  no admin), its `fauna-sync-agent.exe` the real agent this checkout built
  (`sync_agent_binary`) at the package root, where the shipped package keeps it. The
  identity is test-only and FRESH PER RUN (`IDENTITY_PREFIX` plus this process's id),
  a family never registered before — the case the measured refusal came in.
* **The agent is the one the packaged app finds and starts itself.** The launch sets
  the agent pin EMPTY, the contract's own "not pinned"
  (`drivers/windows.py::_default_sync_agent_pin`), so the app walks the production
  probes from its own exe to the package root's agent, and starts it as its child, in
  the package's context; with no pin the app's gate needs `FAUNA_E2E_REAL_SYNC_AGENT`.
  The pipe and the agent's data dir stay this test's own (`FAUNA_E2E_SYNC_PIPE`,
  `FAUNA_E2E_SYNC_AGENT_DATA_DIR`), so the box's per-SID pipe and real agent state are
  never touched — the shape `test_direct_package_agent_after_update.py` established.
* **The binding is the user's.** A fresh account and a set created custody-first on the
  session nest (the seat runs' own setup, `tests/test_filesync_seats.py`); the app
  signed in through the e2e session login and the set's folder bound through the
  Folders page (`helpers/sync_seats.AppSeatDriver`, the seat runs' one construction of
  an app seat, handed a package-context launch). A fresh windows binding starts
  on-demand (`on-demand-files.md` § On-Demand Files, *The choice is the user's*), which
  is what makes the agent register a root at all; the row's switch is read to say so.
* **The folder is on the system volume, in the user's profile.** Not the scratch, which
  sits on Windows's ReFS Dev Drive, where the cloud-files filter does not attach; not
  under AppData, where the package's write virtualization stands between the package
  and the path (the install witness's `_sync_root_folder` measured both).

What is read, all from outside the agent
-----------------------------------------
The filter's answer that the folder is a root and its provider connected
(`helpers/windows_cfapi.provider_connected`), the shell registration the OS records for
it in `SyncRootManager` and the `AUMID` it records there, which must name the test
package (`<family>!FaunaApp`) — the proof that the registration was made in the
package's context, the subject — and the agent process serving the test's pipe, its
image and package identity off the OS. The measurement itself is the agent's own log:
each `StorageProviderSyncRootManager::Register refused 0x80070490; trying again`
warning (a refusal inside a call, `fauna_cfapi::register_sync_root_with_shell`), each
`cfapi sync-root registration failed` error (a call whose every try was refused,
`engine_driver.rs`), and the `connected at a later try` line (the connect-retry edge,
`bridge.rs`), printed as `MEASURED` lines once the agent has shut down and its log is
whole. No count is pinned: the refusal does not come every time (the install witness's
run of 2026-11-17 afternoon met none), so the verdict is the root's, the count the
record.

Then the binding is ended through the UI, and the ended binding's whole teardown is
witnessed from inside the package too: no filter root, no `SyncRootManager` entry and
no Explorer namespace keys left (`file-sync.md` § the registration lifecycle: every
whole teardown deletes what `Unregister` keeps of a root registered from inside a
package), the user's folder kept. The namespace keys are found two ways: the three the
root's own key named, and any whose class targets the run's folder
(`windows_cfapi.namespace_keys_targeting`), which also finds the class a refused
`Register` left with no key naming it. A process in a package's context deletes an
HKCU key only from its own view, so the packaged agent makes those deletes through
`reg.exe` started outside the package (`fauna_cfapi::outside_package`;
`installers/windows.md` § Direct download, 5, *Removal*, measured and built
2026-11-20); this check is what proves they reach the real hive.

Borrow and give back: the binding is ended through the UI; the app stopped and the
agent asked to shut down over its own pipe; any root a failed run left is given back
from outside the package in the agent's own teardown order
(`windows_cfapi.give_back_root`), and every namespace key whose class targets the
run's folder deleted from outside it (`windows_cfapi.give_back_namespace_keys_under`);
the package removed unconditionally (which also stops
its processes); the folder and the scratch removed. Every launch relocates
`LOCALAPPDATA` and the credential store into this test's scratch, outside the
`%LocalAppData%` write virtualization. Latency-independent (convention 14): every wait
polls the OS's own state up to a named budget.

Tier 4: a deployment artifact's shape under the real OS deployment stack.
"""

from __future__ import annotations

import os
import secrets
import shutil
import sys
import time
from pathlib import Path

import pytest

from common.accounts import actor_id_hex
from common.auth import user_create_folder
from conftest import _seeded_environment
from helpers import sync_agent_ipc as ipc
from helpers import sync_seats
from helpers import taskbar_badge
from helpers import windows_cfapi
from helpers import windows_store_install as catalogue
from helpers import windows_sync_agent as agents
from helpers.folder_content import open_bound_row
from helpers.skew_client import state_home_config
from helpers.waiting import wait_until
from tests.test_filesync_seats import STARTUP_WINDOW, harness_sign_in, register_run_account

pytestmark = [
    pytest.mark.skipif(sys.platform != "win32", reason="MSIX registration is Windows-only"),
    pytest.mark.tier_4,
    pytest.mark.windows,
]

#: The test-only identity's prefix; each run registers `<prefix><pid>`, a family never
#: registered before, and a crashed run's family is removed by the prefix.
IDENTITY_PREFIX = "FaunaSocialTest.AgentFirstRoot"
APP_ID = taskbar_badge.APPLICATION_ID
AGENT_EXE = "fauna-sync-agent.exe"
# The file-backed credential store's namespace, named so the store is the test's own
# (a caller-named store is never emptied by a driver: `drivers/windows.py`).
KEYRING_APP = "fauna-windows"
SET_NAME = "firstroot"
#: Where the bound folder's parent lives: this user's profile (module docstring).
ROOTS_PREFIX = "FaunaAgentFirstRoot-"

# The agent's own lines about the registration (module docstring names each source).
REFUSED = "StorageProviderSyncRootManager::Register refused 0x80070490; trying again"
CALL_FAILED = "cfapi sync-root registration failed"
LATER_TRY = "the on-demand root connected at a later try"

#: The ceiling on the root standing and connected after the bind, which a green run
#: never pays: a call refused through all its tries is tried again at the serve loop's
#: connect-retry edge 5 s later, the wait doubling up to the rescan interval, so 300 s
#: holds seven connects of four `Register` tries each.
ROOT_BUDGET_S = 300.0
#: The ceiling on the ended binding's teardown reaching the OS: the unbind is pushed
#: to the agent, which ends the root's engine and tears the root down whole.
TEARDOWN_BUDGET_S = 120.0
#: The test's own ceiling (convention 9), from its plan: the registration's
#: `Add-AppxPackage` (600 s at most), the seat's start and readiness, the root, the
#: teardown, and the give-back.
_TIMEOUT = int(600 + 2 * STARTUP_WINDOW + ROOT_BUDGET_S + TEARDOWN_BUDGET_S + 300)


def _roots_parent() -> Path:
    return Path(os.environ["USERPROFILE"]) / f"{ROOTS_PREFIX}{os.getpid()}"


def _clear_crashed_runs() -> None:
    """A crashed run's package goes by the prefix; its folder goes once the roots in it
    are released (a root whose provider is gone; a packaged root's kept shell entry is
    the startup janitor's, since its family is no longer installed)."""
    catalogue.remove_package(f"{IDENTITY_PREFIX}*")
    for leftover in Path(os.environ["USERPROFILE"]).glob(f"{ROOTS_PREFIX}*"):
        windows_cfapi.give_back_namespace_keys_under(leftover)
        released = windows_cfapi.release_sync_roots(leftover)
        if released.spared:
            print(f"GIVE-BACK: a crashed run's roots under {leftover} stay: "
                  f"{released.spared}")
            continue
        shutil.rmtree(leftover, ignore_errors=True)


@pytest.fixture
def scratch():
    """This test's scratch, outside the `%LocalAppData%` write virtualization
    (`taskbar_badge.unvirtualized_scratch`), and the give-back of its package and its
    folder."""
    _clear_crashed_runs()
    base = Path(taskbar_badge.unvirtualized_scratch("agent-first-root"))
    try:
        yield base
    finally:
        catalogue.remove_package(f"{IDENTITY_PREFIX}*")
        shutil.rmtree(base, ignore_errors=True)
        shutil.rmtree(_roots_parent(), ignore_errors=True)


def _server_of(pipe: str) -> int | None:
    try:
        return ipc.pipe_server_pid(pipe, timeout=2)
    except TimeoutError:
        return None


def _describe(pid: int | None) -> str:
    if pid is None:
        return "no process"
    return (f"pid {pid}, image {agents.process_image_path(pid)!r}, package "
            f"{catalogue.process_package_full_name(pid)!r}")


def _root_connected(folder: Path) -> bool:
    try:
        return windows_cfapi.provider_connected(folder)
    except OSError:
        return False


def _root_state(folder: Path) -> str:
    """The OS's account of ``folder`` as a root, for a failure message (convention 6)."""
    try:
        provider = windows_cfapi.sync_root_provider(folder)
    except OSError as exc:
        provider = f"unreadable ({exc})"
    root_id = windows_cfapi.shell_sync_root_id_for(folder)
    aumid = windows_cfapi.shell_sync_root_aumid(root_id) if root_id else None
    return (f"filter root {windows_cfapi.is_sync_root(folder)}, provider {provider}, "
            f"shell entry {root_id!r}, AUMID {aumid!r}")


def _agent_locations(pipe: str) -> str:
    """The bindings the agent lists, for a failure message; never raises."""
    try:
        return repr(agents.list_locations(pipe))
    except Exception as exc:  # noqa: BLE001 — a diagnosis must not mask the failure
        return f"(unreadable: {exc})"


def _registration_lines(seat) -> list[str]:
    return [ln for ln in seat.agent_log_lines()
            if REFUSED in ln or CALL_FAILED in ln or LATER_TRY in ln]


def _print_measurement(seat) -> None:
    """The agent's own count of its tries, as `MEASURED` lines: a call's refusals
    before its last try are its warnings, a call refused through all its tries is one
    error, and the call that registered is the one after them."""
    lines = _registration_lines(seat)
    refused = sum(REFUSED in ln for ln in lines)
    failed = sum(CALL_FAILED in ln for ln in lines)
    later = sum(LATER_TRY in ln for ln in lines)
    print(f"MEASURED agent's Register refusals inside a call: {refused}; calls refused "
          f"through all their tries: {failed}; connected at a later try: {later}")
    for ln in lines:
        print(f"MEASURED agent log: {ln.strip()[:400]}")


@pytest.mark.timeout(_TIMEOUT)
def test_the_packaged_agents_first_root_after_deployment_registers_and_connects(
        request, nest_instance, windows_app_path, sync_agent_binary, scratch):
    identity = f"{IDENTITY_PREFIX}{os.getpid()}"
    package = taskbar_badge.registered_package(
        str(windows_app_path), str(scratch / "package"), identity, channel="direct",
        payload_files={AGENT_EXE: str(sync_agent_binary)})
    pfn, layout, aumid = package["pfn"], Path(package["layout"]), package["aumid"]
    facts = catalogue.package_facts(identity)
    assert facts is not None and facts.status == "Ok" \
        and agents.same_dir(facts.install_location, layout), (
            f"{identity} is not registered Ok from {layout}: {facts}")
    print(f"MEASURED deployment: {identity} {facts.version} registered from {layout}")

    # The account and its set, custody-first, as the seat runs make them.
    port, node_url = nest_instance["port"], nest_instance["url"]
    secret_key = secrets.token_hex(32)
    actor_id = actor_id_hex(secret_key)
    handle = register_run_account(port, actor_id, node_url=node_url,
                                  admin_sk=nest_instance["admin"]["signing_key"])
    user_create_folder(port, SET_NAME, secret_key=secret_key, base_url=node_url)

    leaf = f"fauna-sync-e2e-first-root-{os.getpid()}"
    pipe = rf"\\.\pipe\{leaf}"
    holder = _server_of(pipe)
    assert holder is None, f"{pipe} is already served before the test starts: {_describe(holder)}"
    home, creds, agent_data = scratch / "home", scratch / "creds", scratch / "agent-data"
    for d in (home, creds, agent_data):
        d.mkdir(parents=True)
    folder = _roots_parent() / "watch"

    seat = sync_seats.AppSeatDriver(
        "windows",
        name="a",
        run_token=sync_seats.new_run_token(seats=1),
        path=folder,
        launch_config={
            "url": node_url,
            "app_path": package["exe"],
            "environment": {
                **_seeded_environment(request, nest_instance),
                # Not pinned: the app resolves the agent from its own exe.
                "FAUNA_E2E_SYNC_AGENT_BIN": "",
                # The gate an unpinned e2e launch needs to start the hydration session.
                "FAUNA_E2E_REAL_SYNC_AGENT": "1",
                "FAUNA_E2E_SYNC_PIPE": leaf,
                "FAUNA_E2E_SYNC_AGENT_DATA_DIR": str(agent_data),
            },
            "package_family_name": pfn,
            "package_app_id": APP_ID,
            **state_home_config("windows", home=home, creds=creds, keyring_app=KEYRING_APP),
        },
        sign_in=harness_sign_in(node_url, secret_key, actor_id, handle),
        folder=SET_NAME,
        node_url=node_url,
        agent_data_dir=agent_data,
    )
    root_id: str | None = None
    clsid: str | None = None
    agent_pid: int | None = None
    try:
        # Launched in the package's context, signed in, the set adopted and its folder
        # bound through the Folders page (`AppSeatDriver.start`).
        seat.start()
        bound_at = time.monotonic()
        running_as = seat.app.driver.app_package_full_name()
        assert running_as and running_as.startswith(f"{identity}_"), (
            f"FaunaApp is not running as {identity} (package {running_as!r}) — the "
            "package-context launch failed")
        seat.await_ready(STARTUP_WINDOW)

        agent_pid = _server_of(pipe)
        print(f"MEASURED agent: {_describe(agent_pid)}")
        assert agent_pid is not None, f"nothing serves {pipe}\n{seat.diagnostics()}"
        agent_package = catalogue.process_package_full_name(agent_pid)
        assert agents.same_file(agents.process_image_path(agent_pid), layout / AGENT_EXE) \
            and agent_package and agent_package.startswith(f"{identity}_"), (
                f"the agent serving the binding is not the package's own, run in its "
                f"context ({_describe(agent_pid)}; want {layout / AGENT_EXE} as "
                f"{identity})\n{seat.diagnostics()}")

        scope = open_bound_row(seat.app, SET_NAME, diagnose=seat.diagnostics)
        mode = seat.app.sync_locations.mode_toggle_state(scope=scope)
        assert mode == "on-demand", (
            f"a fresh windows binding starts on-demand (on-demand-files.md § On-Demand "
            f"Files, *The choice is the user's*); the row reads {mode!r}")

        # The verdict: the root stands, shell-registered and connected.
        wait_until(
            lambda: _root_connected(folder)
            and windows_cfapi.shell_sync_root_id_for(folder) is not None,
            ROOT_BUDGET_S, interval=0.5,
            diagnose=lambda: (
                f"the packaged agent's root over {folder} did not stand registered and "
                f"connected: {_root_state(folder)}. Its registration lines so far:\n"
                + "\n".join(_registration_lines(seat)) + f"\n{seat.diagnostics()}"))
        print(f"MEASURED root registered and connected "
              f"{time.monotonic() - bound_at:.0f} s after the bind")
        root_id = windows_cfapi.shell_sync_root_id_for(folder)
        clsid = windows_cfapi.namespace_clsid(root_id)
        recorded = windows_cfapi.shell_sync_root_aumid(root_id)
        print(f"MEASURED shell entry {root_id}, AUMID {recorded!r}, namespace {clsid}")
        assert recorded == aumid, (
            f"the root's shell registration records AUMID {recorded!r}, not the test "
            f"package's {aumid!r} — it was not made in the package's context")
        provider = windows_cfapi.sync_root_provider(folder)
        assert provider is not None and provider.name == windows_cfapi.FAUNA_PROVIDER_NAME, (
            f"the root over {folder} is not Fauna's: {provider}")

        # The binding ended through the UI: the root comes down whole.
        scope = open_bound_row(seat.app, SET_NAME, diagnose=seat.diagnostics)
        seat.app.sync_locations.remove_binding(scope=scope)
        wait_until(
            lambda: not windows_cfapi.is_sync_root(folder)
            and not windows_cfapi.shell_sync_root_listed(root_id),
            TEARDOWN_BUDGET_S, interval=0.5,
            diagnose=lambda: (
                f"the ended binding did not take its root down: {_root_state(folder)}; "
                f"the agent lists {_agent_locations(pipe)}; error="
                f"{seat.app.error_text()!r}\n{seat.diagnostics()}"))
        left = windows_cfapi.namespace_keys_left(clsid)
        targeting = windows_cfapi.namespace_keys_targeting(_roots_parent())
        print(f"MEASURED after the unbind: filter root {windows_cfapi.is_sync_root(folder)}, "
              f"shell entry {windows_cfapi.shell_sync_root_listed(root_id)}, namespace "
              f"keys left {left}, namespace keys targeting the folder {targeting}")
        assert folder.is_dir(), f"the unbind took the user's folder {folder}"
        assert not left and not targeting, (
            f"the ended binding's teardown, from inside the package, left the Explorer "
            f"namespace keys {sorted(set(left) | set(targeting))} of {root_id} in the "
            f"real hive: the packaged agent's HKCU deletes go through reg.exe started "
            f"outside the package (fauna_cfapi::outside_package; installers/windows.md "
            f"§ Direct download, 5, Removal). The agent's log on the teardown:\n"
            + "\n".join(ln.strip()[:400] for ln in seat.agent_log_lines()
                        if "namespace key" in ln or "shell unregister" in ln
                        or "reg.exe" in ln))
    finally:
        try:
            seat.stop()
        except Exception as exc:  # noqa: BLE001 — the give-back goes on
            print(f"GIVE-BACK: seat stop: {exc}")
        try:
            _print_measurement(seat)
        except OSError as exc:
            print(f"MEASURED: the agent log could not be read: {exc}")
        if root_id is None:
            root_id = windows_cfapi.shell_sync_root_id_for(folder)
            clsid = windows_cfapi.namespace_clsid(root_id) if root_id else None
        if root_id is not None:
            windows_cfapi.give_back_root(root_id, folder, clsid)
        elif windows_cfapi.is_sync_root(folder):
            released = windows_cfapi.release_sync_roots(folder)
            if released.spared:
                print(f"GIVE-BACK: the root over {folder} stays: {released.spared}")
        dropped = windows_cfapi.give_back_namespace_keys_under(_roots_parent())
        if dropped:
            print(f"GIVE-BACK: deleted the namespace keys targeting {_roots_parent()}: "
                  f"{dropped}")
        # The package's removal (the fixture) stops what runs in its context; this is
        # the fallback for an agent that does not, checked against this scratch.
        if agent_pid is not None and not agents.process_has_exited(agent_pid):
            agents.stop_process_under(agent_pid, scratch)
