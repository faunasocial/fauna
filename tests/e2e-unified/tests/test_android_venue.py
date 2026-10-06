"""tier_1: android's run venue — `helpers/android_venue.py`.

Authority: `docs/goal/architecture/testing.md` § Default app and nest mode →
*Android's run venue* (the A2 ruling: the driver is a pure adb client of a
tunnelled server, and "the bridge forward and android-mode test nests use fixed,
pre-forwarded port ranges rather than `find_free_port()`").

WHY THIS FILE EXISTS. A tunnel carries only the
ports named when it was dialed. So the harness and the tunnel command must agree
on every port without ever talking to each other, across two machines, and a
disagreement shows up as an app that cannot reach its nest with an error naming
nothing. Everything that agreement rests on is checkable with no emulator and no
tunnel: the constants, the leases that hand each port to one holder, and the
command generated from the same constants.

No real venue port is listened on, and the machine-wide lease directory is never
touched: every lease below is taken in a private directory, over scratch port
numbers wherever the test does not need the real range.
"""

import os
import socket
import subprocess
import sys

import pytest

sys.path.insert(0, os.path.join(os.path.dirname(__file__), ".."))

from helpers import android_device, android_venue  # noqa: E402

pytestmark = pytest.mark.tier_1

_E2E_ROOT = os.path.join(os.path.dirname(__file__), "..")
_REPO_ROOT = os.path.join(_E2E_ROOT, "..", "..")

#: Port NUMBERS for lease-only tests (`must_bind=False`): nothing binds them.
_SCRATCH = range(40000, 40003)


@pytest.fixture(autouse=True)
def _private_lease_dir_and_no_venue(tmp_path, monkeypatch):
    monkeypatch.setattr(android_venue, "lease_dir", lambda: str(tmp_path / "leases"))
    before = android_device.adb_server()
    android_device.set_adb_server(None)
    yield
    android_device.set_adb_server(before)


# ── the constants ───────────────────────────────────────────────────────────


def test_the_ranges_do_not_overlap_each_other_or_the_adb_ports():
    bridge, nests = set(android_venue.BRIDGE_FORWARD_PORTS), set(android_venue.NEST_PORTS)
    assert not bridge & nests
    singles = {android_venue.ADB_SERVER_PORT, android_venue.REMOTE_ADB_PORT, 18500}
    assert len(singles) == 3
    assert not singles & (bridge | nests)


def test_every_venue_port_is_below_the_ephemeral_range():
    # `find_free_port()` binds port 0, so a sibling session is handed an
    # ephemeral port (32768+ on linux) and can never be handed one of these.
    ports = [android_venue.ADB_SERVER_PORT, *android_venue.BRIDGE_FORWARD_PORTS,
             *android_venue.NEST_PORTS]
    assert max(ports) < 32768


def test_the_dev_vm_adb_listener_is_not_adbs_default_port():
    # A bare `adb` call on the dev VM starts a local server on 5037; the venue's
    # listener there must be a port no bare adb call can take or reach.
    assert android_venue.ADB_SERVER_PORT != android_venue.REMOTE_ADB_PORT == 5037


def test_the_advertised_adb_server_spec_is_one_the_harness_accepts():
    assert android_device.resolve_adb_server(android_venue.ADB_SERVER_SPEC, {}) == (
        f"tcp:127.0.0.1:{android_venue.ADB_SERVER_PORT}"
    )


# ── leases ──────────────────────────────────────────────────────────────────


def test_two_leases_get_distinct_ports():
    a = android_venue.lease_port(_SCRATCH, must_bind=False)
    b = android_venue.lease_port(_SCRATCH, must_bind=False)
    assert (a.port, b.port) == (40000, 40001)


def test_a_released_port_is_leased_again():
    a = android_venue.lease_port(_SCRATCH, must_bind=False)
    b = android_venue.lease_port(_SCRATCH, must_bind=False)
    a.release()
    a.release()  # idempotent
    assert android_venue.lease_port(_SCRATCH, must_bind=False).port == 40000
    b.release()


def test_an_exhausted_range_is_loud_and_names_the_range():
    held = [android_venue.lease_port(_SCRATCH, must_bind=False) for _ in _SCRATCH]
    with pytest.raises(android_venue.AndroidVenueError, match="40000-40002"):
        android_venue.lease_port(_SCRATCH, must_bind=False)
    del held


_CHILD = """
import sys
sys.path.insert(0, sys.argv[1])
from helpers import android_venue
lease = android_venue.lease_port(range(40000, 40003), must_bind=False, directory=sys.argv[2])
print(lease.port)
"""


def _lease_in_a_child(directory) -> int:
    out = subprocess.run(
        [sys.executable, "-c", _CHILD, _E2E_ROOT, directory],
        capture_output=True, text=True, check=True, timeout=60,
    )
    return int(out.stdout.strip())


def test_another_process_is_refused_a_held_port_and_gets_the_next():
    mine = android_venue.lease_port(_SCRATCH, must_bind=False)
    assert mine.port == 40000
    assert _lease_in_a_child(android_venue.lease_dir()) == 40001


def test_a_dead_holders_port_is_free_again_with_no_cleanup():
    # The child leased 40000 and exited without releasing: the kernel dropped
    # its lock with the process, which is the whole crash-safety story.
    assert _lease_in_a_child(android_venue.lease_dir()) == 40000
    assert android_venue.lease_port(_SCRATCH, must_bind=False).port == 40000


def _listening_socket():
    s = socket.socket(socket.AF_INET, socket.SOCK_STREAM)
    s.bind(("127.0.0.1", 0))
    s.listen(1)
    return s, s.getsockname()[1]


def test_a_nest_lease_skips_a_port_something_already_listens_on():
    s, taken = _listening_socket()
    try:
        ports = range(taken, taken + 2)
        assert android_venue.lease_port(ports, must_bind=True).port == taken + 1
    finally:
        s.close()


def test_a_bridge_forward_lease_does_not_care_that_the_tunnel_listens_there():
    # Under the tunnel this machine's bridge-forward port IS listened on, by the
    # tunnel. A bind test would call every port of the range taken.
    s, taken = _listening_socket()
    try:
        assert android_venue.lease_port(range(taken, taken + 1), must_bind=False).port == taken
    finally:
        s.close()


# ── the nest-port seam ──────────────────────────────────────────────────────


def test_outside_a_venue_run_a_nest_port_is_find_free_port_untouched():
    port, release = android_venue.nest_port(lambda: 4242)
    assert port == 4242
    release()
    assert not os.path.exists(android_venue.lease_dir())  # no lease was taken


def test_in_a_venue_run_nest_ports_come_from_the_tunnelled_range():
    android_device.set_adb_server(android_venue.ADB_SERVER_SPEC)

    def never():
        raise AssertionError("a venue run asked find_free_port() for a nest port")

    first, release_first = android_venue.nest_port(never)
    second, release_second = android_venue.nest_port(never)
    assert first in android_venue.NEST_PORTS and second in android_venue.NEST_PORTS
    assert first != second
    release_first()
    again, release_again = android_venue.nest_port(never)
    assert again == first
    release_again()
    release_second()


def test_a_reverse_for_an_untunnelled_port_is_refused_with_the_cause():
    android_venue.require_tunnelled_nest_port(android_venue.NEST_PORTS[0])
    with pytest.raises(android_venue.AndroidVenueError, match="tunnelled range"):
        android_venue.require_tunnelled_nest_port(13001)


def _conftest_function_source(name: str) -> str:
    import ast

    with open(os.path.join(_E2E_ROOT, "conftest.py"), encoding="utf-8") as fh:
        text = fh.read()
    found = [
        ast.get_source_segment(text, node) or ""
        for node in ast.walk(ast.parse(text))
        if isinstance(node, ast.FunctionDef) and node.name == name
    ]
    assert found, f"conftest has no function named {name!r}"
    return "\n".join(found)


@pytest.mark.parametrize("function", ["_make_nest", "start"])
def test_both_nest_providers_take_their_port_from_the_seam(function):
    # The flow break this pins sits BETWEEN two working symbols: the seam is
    # tested above and the providers start nests fine, yet a provider calling
    # `find_free_port()` directly hands the device a port no tunnel carries.
    src = _conftest_function_source(function)
    assert "android_venue.nest_port(find_free_port)" in src
    assert "port = find_free_port()" not in src
    assert "release_port()" in src


# ── the tunnel ──────────────────────────────────────────────────────────────


def _forwards(argv):
    return [(argv[i], argv[i + 1]) for i, word in enumerate(argv) if word in ("-R", "-L")]


def test_the_tunnel_has_one_remote_forward_per_adb_port():
    remote = [spec for flag, spec in _forwards(android_venue.tunnel_command("user", "10.0.0.2"))
              if flag == "-R"]
    expected = ["127.0.0.1:18509:127.0.0.1:5037"] + [
        f"127.0.0.1:{p}:127.0.0.1:{p}" for p in android_venue.BRIDGE_FORWARD_PORTS
    ]
    assert remote == expected


def test_the_tunnel_has_one_local_forward_per_nest_port():
    local = [spec for flag, spec in _forwards(android_venue.tunnel_command("user", "10.0.0.2"))
             if flag == "-L"]
    assert local == [f"127.0.0.1:{p}:127.0.0.1:{p}" for p in android_venue.NEST_PORTS]


def test_every_forward_is_loopback_at_both_ends():
    # Constraint 2: nothing unauthenticated listens on a network interface, on
    # either machine. A forward without an explicit bind address would still be
    # loopback by ssh's default; it is spelled out so the default is not what
    # the constraint rests on.
    for _flag, spec in _forwards(android_venue.tunnel_command("user", "10.0.0.2")):
        bind, _listen, target, _port = spec.split(":")
        assert (bind, target) == ("127.0.0.1", "127.0.0.1"), spec


def test_the_tunnel_fails_whole_and_keeps_itself_alive():
    argv = android_venue.tunnel_command("user", "10.0.0.2")
    assert argv[:2] == ["ssh", "-N"]
    assert "ExitOnForwardFailure=yes" in argv and "ServerAliveInterval=30" in argv
    assert argv[argv.index("-i") + 1] == android_venue.TUNNEL_KEY
    assert argv[-1] == "user@10.0.0.2"


@pytest.mark.parametrize("login, address", [
    ("user; rm -rf ~", "10.0.0.2"), ("user", "10.0.0.2 -o ProxyCommand=x"),
    ("-oProxyCommand=x", "10.0.0.2"), ("", "10.0.0.2"), ("user", "$(id)"),
])
def test_tunnel_arguments_that_are_not_plain_words_are_refused(login, address):
    # The output is pasted into a shell on another machine.
    with pytest.raises(android_venue.AndroidVenueError):
        android_venue.tunnel_command(login, address)


def test_the_recipe_prints_the_command_and_the_run_flags():
    # The script form `just android-tunnel-spec` runs: no package path, no
    # pytest, both arguments given.
    script = os.path.join(_E2E_ROOT, "helpers", "android_venue.py")
    out = subprocess.run(
        [sys.executable, script, "user", "10.0.0.2"],
        capture_output=True, text=True, check=True, timeout=60,
    ).stdout
    assert "-R 127.0.0.1:18509:127.0.0.1:5037" in out
    assert out.count(" -L 127.0.0.1:") == len(android_venue.NEST_PORTS)
    assert "user@10.0.0.2" in out
    assert f"--adb-server {android_venue.ADB_SERVER_SPEC}" in out


def test_the_justfile_recipe_runs_this_module():
    with open(os.path.join(_REPO_ROOT, "justfile"), encoding="utf-8") as fh:
        justfile = fh.read()
    recipe = justfile.split("\nandroid-tunnel-spec ", 1)
    assert len(recipe) == 2, "the justfile has no `android-tunnel-spec` recipe"
    assert "tests/e2e-unified/helpers/android_venue.py" in recipe[1].split("\n\n", 1)[0]
