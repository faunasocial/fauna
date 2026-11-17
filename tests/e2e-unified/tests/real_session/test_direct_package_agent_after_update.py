"""The direct package's update, last inch: after the update has closed the app and the
sync agent, the next signed-in start of the app brings the agent back, served from the
NEW version's install location.

Goal doc: `docs/goal/architecture/installers/windows.md` § Direct download (MSIX app +
nest MSI) → *2. Updates*, the measured bullet. The install witness
(`test_direct_package_install_witness.py`, item 3) measured the rest of that bullet
with the production `full` payload: a plain deployment of N+1 over a running app and
agent refuses `0x80073D02`, and `-ForceApplicationShutdown` installs N+1, closes both
and keeps the data. It could not reach this inch, because the windows app starts its
agent only from a signed-in session (`App.xaml.cs`, `StartHydrationSession`, behind
`HydrationSessionEnabled`), and a production build cannot be signed in by a test.

What runs, and why it is the package's own mechanism
---------------------------------------------------
* **The package.** The Debug FaunaApp, which carries the e2e seam the sign-in goes
  through (convention 15 keeps it out of a release artifact), registered as a full
  package of the DIRECT channel by the packer's own `render_manifest` and `_stage`
  (`helpers/taskbar_badge.py::registered_package`, deployment path 3: Developer Mode,
  no admin), under a test-only identity so it can neither collide with nor displace a
  real install. Unlike the badge witnesses' packages, its `fauna-sync-agent.exe` entry
  is the real agent this checkout built (`sync_agent_binary`), at the package root
  beside `App\\`, where the shipped package keeps it.
* **The agent is the one the packaged app finds for itself.** A windows e2e launch
  normally pins the agent binary (`FAUNA_E2E_SYNC_AGENT_BIN`, convention 10), and a
  pin wins over every probe (`agent_spawner.rs::windows_agent_candidates`), which
  would swap the subject out. This launch sets the pin EMPTY, the contract's own "not
  pinned" (`drivers/windows.py::_default_sync_agent_pin`, the installed journey's
  `_apply_installed_product_agent_env` does the same), so the app walks the production
  probes from its own exe: `App\\fauna-sync-agent.exe` (absent: the Debug output has no
  agent), then `App\\..\\fauna-sync-agent.exe`, the package root. With no pin the app's
  gate needs `FAUNA_E2E_REAL_SYNC_AGENT` (`HydrationSessionEnabled`); the pipe and the
  agent's data dir stay this test's own (`FAUNA_E2E_SYNC_PIPE`,
  `FAUNA_E2E_SYNC_AGENT_DATA_DIR`, which the spawner forwards as `--pipe-name` and
  `--data-dir`), so the box's per-SID pipe and real agent state are never touched.
* **The next start is a real restart.** Version N is signed in through the e2e
  session login, which writes the account into the app's credential store as a real
  sign-in does (`App.xaml.cs`, the `session` patch's registry enrol). The update kills
  that process; the next start is a fresh driver over the SAME store and data roots
  (`helpers/skew_client.state_home_config`, the seam that hands one client state to a
  sequence of launches), started in the package's context from N+1's install
  location, and it signs itself in from the store, through the launch machine's
  `Online` phase and `StartMainAppAsync`, the path a user's restart takes. No login is
  replayed: a replay would go through the e2e seam instead.
* **Which agent answered is read from the OS.** The pid serving the test's pipe
  (`GetNamedPipeServerProcessId`), that pid's image path
  (`QueryFullProcessImageNameW`) and its package identity (`GetPackageFullName`) —
  never the app's log or its belief.

Borrow and give back: the package is removed unconditionally (which also stops its
processes), the agent is asked to shut down over its own pipe first, every launch
relocates `LOCALAPPDATA` and the credential store into this test's scratch, which is
removed. Latency-independent (convention 14): every wait polls the OS's own state up to
a named budget.

Tier 4: a deployment artifact's shape under the real OS deployment stack.
"""

from __future__ import annotations

import os
import shutil
import sys
from pathlib import Path

import pytest

from conftest import _seeded_environment
from drivers import create_driver
from helpers import sync_agent_ipc as ipc
from helpers import taskbar_badge
from helpers import windows_store_install as catalogue
from helpers import windows_sync_agent as agents
from helpers.budgets import APP_EXIT_S, APP_RELAUNCH_S, RPC_ROUNDTRIP_S, SERVICE_BOOT_S
from helpers.skew_client import state_home_config
from helpers.waiting import wait_until

pytestmark = [
    pytest.mark.skipif(sys.platform != "win32", reason="MSIX registration is Windows-only"),
    pytest.mark.tier_4,
    pytest.mark.windows,
]

# Test-only identity, distinct from every other witness's and any real build's.
TEST_IDENTITY = "FaunaSocialTest.FaunaDirectAgentUpdate"
APP_ID = taskbar_badge.APPLICATION_ID
AGENT_EXE = "fauna-sync-agent.exe"
# The file-backed credential store's namespace, named so the store is the test's own
# (a caller-named store is never emptied by a driver: `drivers/windows.py`).
KEYRING_APP = "fauna-windows"
_ON_SCREEN = "feed-tab"


@pytest.fixture
def scratch():
    """This test's scratch, outside the `%LocalAppData%` write virtualization
    (`taskbar_badge.unvirtualized_scratch`), and the give-back of its package."""
    catalogue.remove_package(TEST_IDENTITY)  # a crashed run's
    base = Path(taskbar_badge.unvirtualized_scratch("direct-agent-update"))
    try:
        yield base
    finally:
        catalogue.remove_package(TEST_IDENTITY)
        shutil.rmtree(base, ignore_errors=True)


def _server_of(pipe: str) -> int | None:
    try:
        return ipc.pipe_server_pid(pipe, timeout=2)
    except TimeoutError:
        return None


def _agent_lines(driver) -> str:
    """The app's and the agent's log lines about the agent, for a failure message:
    the spawner logs which candidate it started or that none was found."""
    text = driver.app_stderr_text() or ""
    lines = [ln for ln in text.splitlines()
             if "sync agent" in ln or "sync-agent" in ln or "agent_spawner" in ln]
    return "\n".join(lines[-25:]) or "(no agent lines in the app's or the agent's log)"


def _signed_in_as(driver, actor_id_hex: str) -> bool:
    """The app reports a signed-in session of ``actor_id_hex`` (the read
    `HttpBridgeDriver.hard_reload` waits on for a store's own auto-restore); a state
    the booting app cannot answer yet reads as not yet."""
    try:
        return bool(driver.get_state("session.authenticated")) \
            and driver.get_state("session.actor_id") == actor_id_hex
    except (LookupError, RuntimeError, OSError):
        return False


def _describe(pid: int | None) -> str:
    if pid is None:
        return "no process"
    return (f"pid {pid}, image {agents.process_image_path(pid)!r}, package "
            f"{catalogue.process_package_full_name(pid)!r}")


def test_the_agent_returns_from_the_new_version_after_an_update(
        request, nest_instance, test_user, windows_app_path, sync_agent_binary, scratch):
    n = taskbar_badge.registered_package(
        windows_app_path, str(scratch / "n"), TEST_IDENTITY, channel="direct",
        payload_files={AGENT_EXE: str(sync_agent_binary)})
    pfn, layout_n = n["pfn"], Path(n["layout"])
    before = catalogue.package_facts(TEST_IDENTITY)
    assert before is not None and agents.same_dir(before.install_location, layout_n), (
        f"{TEST_IDENTITY} is not registered from {layout_n}: {before}")
    layout_n1 = scratch / "n1" / "layout"
    version_n1 = catalogue.next_version_layout(layout_n, layout_n1)

    leaf = f"fauna-sync-e2e-direct-update-{os.getpid()}"
    pipe = rf"\\.\pipe\{leaf}"
    holder = _server_of(pipe)
    assert holder is None, f"{pipe} is already served before the test starts: {_describe(holder)}"
    home, creds, agent_data = scratch / "home", scratch / "creds", scratch / "agent-data"
    for d in (home, creds, agent_data):
        d.mkdir(parents=True)

    def launch(exe: Path):
        driver = create_driver("windows")
        driver.launch({
            "url": nest_instance["url"],
            "app_path": str(exe),
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
        })
        return driver

    def await_agent(driver, not_pid: int | None = None) -> int:
        return wait_until(
            lambda: (pid := _server_of(pipe)) and pid != not_pid and pid,
            SERVICE_BOOT_S,
            diagnose=lambda: (f"nothing new serves {pipe} (server: {_describe(_server_of(pipe))}). "
                              f"Agent log lines:\n{_agent_lines(driver)}"))

    drivers = []
    seen_agents: list[int] = []
    try:
        # ── Version N, signed in: the app spawns the package's agent ───────────────
        first = launch(layout_n / "App" / "FaunaApp.exe")
        drivers.append(first)
        package = first.app_package_full_name()
        assert package and package.startswith(f"{TEST_IDENTITY}_{before.version}_"), (
            f"FaunaApp is not running as {TEST_IDENTITY} {before.version} (package "
            f"{package!r}) — the package-context launch failed")
        first.set_state({
            "session": {
                "authenticated": True,
                "node_url": nest_instance["url"],
                "secret_hex": test_user["signing_key"].encode().hex(),
                "handle": "e2e-user",
                "actor_id": test_user["actor_id_hex"],
                "device_id": "direct-update-e2e",
            },
            "nav": {"stack": [{"view": "feed"}]},
        })
        first.wait_for(_ON_SCREEN, timeout=RPC_ROUNDTRIP_S)
        agent_n = await_agent(first)
        seen_agents.append(agent_n)
        image_n = agents.process_image_path(agent_n)
        print(f"MEASURED version {before.version}: agent {_describe(agent_n)}")
        assert agents.same_file(image_n, layout_n / AGENT_EXE), (
            f"the agent the signed-in app spawned runs from {image_n!r}, not the package "
            f"root's {layout_n / AGENT_EXE}. Agent log lines:\n{_agent_lines(first)}")

        # ── The update: N+1 over the running app and agent, allowed to close them ──
        after = catalogue.register_layout(layout_n1 / "AppxManifest.xml", name=TEST_IDENTITY,
                                          force_application_shutdown=True)
        print(f"MEASURED update: version {after.version}, status {after.status}, "
              f"from {after.install_location}")
        assert (after.version, after.family_name, after.status) == (version_n1, pfn, "Ok") \
            and agents.same_dir(after.install_location, layout_n1), (
                f"the forced update did not install {version_n1} in {pfn} from {layout_n1}: "
                f"{after}")
        assert first.wait_app_exit(APP_EXIT_S), (
            f"the app of version {before.version} still runs after the forced update")
        wait_until(lambda: agents.process_has_exited(agent_n), APP_EXIT_S, diagnose=lambda: (
            f"the agent of version {before.version} ({_describe(agent_n)}) outlived the "
            "update that installed the next version, so the next start would find the "
            "pipe served by the old agent"))
        first.teardown()
        drivers.remove(first)

        # ── The next start, from the new version's install location ───────────────
        second = launch(layout_n1 / "App" / "FaunaApp.exe")
        drivers.append(second)
        package = second.app_package_full_name()
        assert package and package.startswith(f"{TEST_IDENTITY}_{version_n1}_"), (
            f"the relaunched FaunaApp is not running as {TEST_IDENTITY} {version_n1} "
            f"(package {package!r})")
        wait_until(
            lambda: _signed_in_as(second, test_user["actor_id_hex"]),
            APP_RELAUNCH_S,
            diagnose=lambda: (
                "the app did not sign itself in from its store after the update "
                f"(session.authenticated {second.get_state('session.authenticated')!r}, "
                f"actor {second.get_state('session.actor_id')!r}, want "
                f"{test_user['actor_id_hex']!r})"))
        agent_n1 = await_agent(second, not_pid=agent_n)
        seen_agents.append(agent_n1)
        image_n1 = agents.process_image_path(agent_n1)
        print(f"MEASURED version {version_n1}: agent {_describe(agent_n1)}")
        assert agents.same_file(image_n1, layout_n1 / AGENT_EXE), (
            f"after the update the signed-in app's agent runs from {image_n1!r}, not the "
            f"new version's {layout_n1 / AGENT_EXE}. Agent log lines:\n{_agent_lines(second)}")
    finally:
        for driver in drivers:
            try:
                driver.teardown()
            except Exception as exc:  # noqa: BLE001 — the give-back goes on
                print(f"GIVE-BACK: driver teardown: {exc}")
        try:
            agents.stop_agent_if_serving(pipe)
        except (AssertionError, OSError) as exc:
            print(f"GIVE-BACK: the agent on {pipe} did not shut down: {exc}")
        # The package's removal (the fixture) stops what runs in its context; this is
        # the fallback for an agent that does not, checked against this scratch.
        for pid in seen_agents:
            if not agents.process_has_exited(pid):
                agents.stop_process_under(pid, scratch)
