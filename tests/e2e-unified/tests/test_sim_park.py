"""The simulator park — a verify drain's iOS seat devices kept booted across
its pytest processes (mac-simulator-hold.md § Implementation status today,
*The park*; `helpers/sim_park.py`).

Every `simctl` call goes to a fake that keeps a device table, so these pin the
park's mechanism — lease, hand-back, reset, the orphan-reclaim naming, close —
and the conftest's two hooks (lease before clone, park instead of delete; no
boot and no grant for a leased device) without a simulator.
"""

import json
import os
import subprocess
import sys

import pytest

pytestmark = pytest.mark.tier_1

import conftest  # noqa: E402
from helpers import sim_park  # noqa: E402

RT = "com.apple.CoreSimulator.SimRuntime.iOS-27-0"
DT = "com.apple.CoreSimulator.SimDeviceType.iPhone-17-Pro"


class FakeSimctl:
    """A device table and the `simctl` calls made against it."""

    def __init__(self):
        self.devices: dict[str, dict] = {}
        self.calls: list[list[str]] = []

    def add(self, udid, name, state="Booted"):
        self.devices[udid] = {"udid": udid, "name": name, "state": state}

    def __call__(self, cmd, timeout=300):
        self.calls.append(cmd)
        verb = cmd[2]
        out, rc = "", 0
        if verb == "list":
            out = json.dumps({"devices": {RT: list(self.devices.values())}})
        elif verb == "rename":
            if cmd[3] in self.devices:
                self.devices[cmd[3]]["name"] = cmd[4]
            else:
                rc = 1
        elif verb == "shutdown":
            if cmd[3] in self.devices:
                self.devices[cmd[3]]["state"] = "Shutdown"
        elif verb == "delete":
            self.devices.pop(cmd[3], None)
        return subprocess.CompletedProcess(cmd, rc, out, "")

    def verbs(self, udid):
        return [c[2:] for c in self.calls if udid in c and c[2] != "list"]


@pytest.fixture
def park(tmp_path):
    d = tmp_path / "sim-park"
    sim_park.open_park(d, os.getpid(), run=FakeSimctl())
    return d


def test_no_park_env_means_no_park(monkeypatch):
    monkeypatch.delenv(sim_park.PARK_ENV, raising=False)
    assert sim_park.park_dir() is None
    assert sim_park.park_dir({sim_park.PARK_ENV: "/x/park"}) == sim_park.Path("/x/park")


def test_a_handed_back_device_is_reset_renamed_for_the_owner_and_leased_booted(park):
    """Two processes back to back on one device: the first's exit hands it back
    booted after the reset (apps uninstalled, keychain and privacy reset) under
    the OWNER's pid, so the orphan reclaim keeps it while the drain lives; the
    second process leases it under its own name with no boot."""
    sim = FakeSimctl()
    sim.add("U1", "fauna-e2e-ios-111-1")
    assert sim_park.park(park, "U1", RT, DT, ("social.fauna.fauna",), run=sim)
    assert sim.devices["U1"]["name"] == f"fauna-e2e-ios-{os.getpid()}-1"
    assert sim.devices["U1"]["state"] == "Booted"
    assert sim.verbs("U1")[:4] == [
        ["terminate", "U1", "social.fauna.fauna"],
        ["uninstall", "U1", "social.fauna.fauna"],
        ["keychain", "U1", "reset"],
        ["privacy", "U1", "reset", "all"],
    ]
    sim.calls.clear()
    assert sim_park.lease(park, RT, DT, "fauna-e2e-ios-222-1", run=sim) == "U1"
    assert sim.devices["U1"]["name"] == "fauna-e2e-ios-222-1"
    assert not any(c[2] in ("boot", "clone", "create") for c in sim.calls), sim.calls
    # One device, one lessee: the park is empty now.
    assert sim_park.lease(park, RT, DT, "fauna-e2e-ios-333-1", run=sim) is None


def test_the_orphan_reclaim_reads_a_parked_name_as_the_owners():
    """Job C keys on the FIRST digit run of the name: a parked device's is the
    drain's pid, its seat suffix never read as a pid."""
    import re

    name_re = re.compile(r"^(?:fauna-e2e-ios-|fauna-fp-evict-|fauna-probe-)(\d+)(?:-\d+)?$")
    m = name_re.match(f"{sim_park.DEVICE_PREFIX}4242-3")
    assert m and m.group(1) == "4242"


def test_a_shut_down_device_or_a_closed_park_is_not_parked(park, tmp_path):
    sim = FakeSimctl()
    sim.add("U2", "fauna-e2e-ios-111-1", state="Shutdown")
    assert not sim_park.park(park, "U2", RT, DT, (), run=sim)
    sim.add("U3", "fauna-e2e-ios-111-2")
    assert not sim_park.park(tmp_path / "no-park", "U3", RT, DT, (), run=sim)


def test_a_park_whose_owner_exited_takes_and_gives_nothing(tmp_path):
    d = tmp_path / "sim-park"
    d.mkdir()
    (d / "owner").write_text("999999999\n")
    sim = FakeSimctl()
    sim.add("U4", "fauna-e2e-ios-111-1")
    assert not sim_park.park(d, "U4", RT, DT, (), run=sim)
    assert sim_park.lease(d, RT, DT, "n", run=sim) is None


def test_a_parked_device_found_shut_down_is_deleted_and_another_pins_is_left(park):
    sim = FakeSimctl()
    sim.add("U5", "a")
    sim.add("U6", "b")
    assert sim_park.park(park, "U5", "other-runtime", DT, (), run=sim)
    assert sim_park.park(park, "U6", RT, DT, (), run=sim)
    sim.devices["U6"]["state"] = "Shutdown"
    assert sim_park.lease(park, RT, DT, "n", run=sim) is None
    assert "U6" not in sim.devices
    assert "U5" in sim.devices and (park / "U5.json").exists()


def test_close_deletes_every_parked_device_and_the_park(park):
    sim = FakeSimctl()
    sim.add("U7", "a")
    sim.add("U8", "b")
    assert sim_park.park(park, "U7", RT, DT, (), run=sim)
    assert sim_park.park(park, "U8", RT, DT, (), run=sim)
    assert sorted(sim_park.close(park, run=sim)) == ["U7", "U8"]
    assert sim.devices == {}
    assert not park.exists()


def test_open_deletes_what_an_unclosed_park_left(tmp_path):
    d = tmp_path / "sim-park"
    sim = FakeSimctl()
    sim_park.open_park(d, os.getpid(), run=sim)
    sim.add("U9", "a")
    assert sim_park.park(d, "U9", RT, DT, (), run=sim)
    assert sim_park.open_park(d, os.getpid(), run=sim) == ["U9"]
    assert sim.devices == {}
    assert (d / "owner").exists()


# ── the conftest's hooks ─────────────────────────────────────────────────────


def test_the_conftest_leases_before_it_clones_and_skips_the_leased_boot(park, monkeypatch):
    """A seat's device comes from the park when it holds one (no clone), its
    held boot is skipped (no grant, `bootstatus` only), and at exit the device
    goes back to the park instead of being deleted."""
    import drivers.ios

    sim = FakeSimctl()
    sim.add("U10", "x")
    assert sim_park.park(park, "U10", RT, DT, (), run=sim)
    monkeypatch.setenv(sim_park.PARK_ENV, str(park))
    for fn in ("park", "lease"):  # each passes its `run` on to every simctl call
        orig = getattr(sim_park, fn)
        monkeypatch.setattr(sim_park, fn,
                            (lambda o: lambda *a, **k: o(*a, **k, run=sim))(orig))
    monkeypatch.setattr(conftest, "_find_ios_runtime_and_devicetype",
                        lambda: (RT, DT, "27.0", "iPhone 17 Pro"))
    monkeypatch.setattr(conftest, "_clone_ios_golden",
                        lambda *a, **k: pytest.fail("cloned although the park held a device"))
    monkeypatch.setattr(conftest, "_E2E_SIMULATORS", [])
    monkeypatch.setattr(conftest, "_E2E_LEASED", set())
    monkeypatch.setattr(conftest, "_note_simulators", lambda: None)
    exits = []
    monkeypatch.setattr(conftest.atexit, "register", lambda fn, *a: exits.append((fn, a)))
    udid = conftest._create_ephemeral_simulator()
    assert udid == "U10"
    assert sim.devices["U10"]["name"] == f"fauna-e2e-ios-{os.getpid()}-1"

    booted = []
    monkeypatch.setattr(drivers.ios, "boot_and_wait_until_usable", booted.append)
    monkeypatch.setattr(conftest, "_build_slot_module",
                        lambda: pytest.fail("a grant was asked for a leased device"))
    conftest._boot_simulator_held(udid)
    assert booted == ["U10"]
    assert udid not in conftest._E2E_LEASED  # a later re-check boots it held

    (fn, args), = exits
    assert fn is conftest._park_or_delete_simulator and args == ("U10", RT, DT)
    monkeypatch.setattr(conftest, "_delete_simulator",
                        lambda u: pytest.fail("deleted although the park is open"))
    fn(*args)
    assert (park / "U10.json").exists()


def test_without_a_park_the_device_is_deleted_at_exit(monkeypatch):
    monkeypatch.delenv(sim_park.PARK_ENV, raising=False)
    deleted = []
    monkeypatch.setattr(conftest, "_delete_simulator", deleted.append)
    conftest._park_or_delete_simulator("U11", RT, DT)
    assert deleted == ["U11"]


def test_the_file_provider_seat_is_never_parked():
    """`fp_device` needs a device whose domain was never registered: its
    devices keep their plain delete at exit."""
    import inspect

    src = inspect.getsource(conftest.create_ios_fp_device)
    assert "atexit.register(_delete_simulator, udid)" in src
    assert "sim_park" not in src


def test_the_cli_opens_and_closes_a_park(tmp_path, capsys):
    d = tmp_path / "p"
    assert sim_park.main(["open", str(d), "--owner-pid", str(os.getpid())]) == 0
    assert (d / "owner").read_text().strip() == str(os.getpid())
    assert sim_park.main(["close", str(d)]) == 0
    assert not d.exists()
    assert "deleted 0 parked device(s)" in capsys.readouterr().out
