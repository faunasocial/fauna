"""Your devices find each other without broadcasting, scanning your network or
announcing themselves on it.

Owner: ``docs/goal/behavior/p2p.md`` § LAN detection ("No broadcast, mDNS, or
network scanning is involved — the check is purely arithmetic against the
addresses a peer already advertised") and § Wormability posture rule 1
(endpoints are learned only over authenticated channels: no DHT, no broadcast,
no global address space to scan).

**The observable** is each seat's own sync agent — the process whose peer leg
carries the direct path — read from outside by the kernel, while that path is
up and has just carried a file:

- **its sockets** (``helpers/agent_sockets.observe``): no UDP socket bound to or
  connected to a discovery port (mDNS, SSDP, LLMNR, WS-Discovery, NetBIOS), a
  multicast group or a broadcast address, and no raw or packet socket (what a
  scan or ARP sweep needs);
- **its multicast memberships**: the network namespace's ``igmp``/``igmp6``
  tables (macos: the box's ``netstat -g``, with the limit the helper names), read before the seats start, while they run and after their agents
  have exited. The table is per namespace, not per process, so a join is
  attributed by timing: above the baseline while the agents ran and gone once
  they exited (``agent_sockets.memberships_released``).

**The reading is its own control.** Each agent must hold the UDP socket its peer
leg advertises (the port of its own ``GetPeerLegDialTarget`` LAN candidate,
the test-only arm of ``fauna-sync-agent``'s ``peer_probe``), so an empty
finding is never a read of the wrong process. And the siblings must have
admitted each other and moved a file before any reading, so the plane is live
rather than idle. The agent's iroh endpoint is built without the port mapper
and with no local discovery (``p2p.md`` § Implementation status today), so this
measures what the agent does, not what it is configured to do.

Two seats on one machine, no operator (convention 16); every wait is a deadline
poll on state (convention 14).
"""

import contextlib
import secrets
import shutil
import sys
import tempfile
from pathlib import Path

import pytest

from common import user_create_folder
from helpers import agent_sockets, sync_seats
from helpers.waiting import wait_until
from tests.test_device_to_device_file_transfer import (
    SIBLING_WINDOW,
    STARTUP_WINDOW,
    WINDOW,
    _account,
    _await_bytes,
    _await_siblings,
    _seat_factory,
)

pytestmark = [
    pytest.mark.tier_3,
    # tui leads; linux, windows and macos run their native pairs. The reading
    # is per platform (`agent_sockets.observe`); a platform whose reader has
    # not landed files its seats in `OWED_SEATS`, never a skip. android and iOS
    # run no seat pair (absent by design on the catalog page).
    pytest.mark.tui,
    pytest.mark.linux,
    pytest.mark.windows,
    pytest.mark.macos,
]

# Seat modes collected but not yet observable, per platform: the socket reader
# for the platform has not landed (`helpers/agent_sockets.py` names each one).
OWED_SEATS = {
    "win32": {"native": "no GetExtendedUdpTable reader in helpers/agent_sockets.py yet"},
}

_SEAT_PAIRS = [
    pytest.param(pair, id=sync_seats.seat_set_param_id(pair))
    for pair in sync_seats.seat_sets_for_platform(
        sys.platform, sync_seats.DEFAULT_PAIRS, unbuilt=OWED_SEATS
    )
]

EXIT_WINDOW = 60.0


def _process_has_exited(pid: int) -> bool:
    if sys.platform == "win32":
        from helpers.windows_sync_agent import process_has_exited
    else:
        from helpers.sync_agent_ipc_unix import process_has_exited
    return process_has_exited(pid)


def _advertised_ports(seat) -> set[int]:
    """The ports of ``seat``'s peer-leg LAN dial candidates, once bound."""
    found: set[int] = set()

    def bound() -> bool:
        client = seat.agent_client()
        if client is None:
            return False
        try:
            target = client.peer_leg_dial_target()
        except Exception:
            return False
        found.update(int(ep.rsplit(":", 1)[1]) for ep in target.get("lan_endpoints") or ())
        return bool(found)

    wait_until(
        bound, SIBLING_WINDOW, interval=2.0,
        diagnose=lambda: (
            f"seat {seat.name}'s agent never advertised a LAN dial candidate\n"
            f"{seat.diagnostics()}"
        ),
    )
    return found


@pytest.mark.timeout(int(2 * STARTUP_WINDOW + SIBLING_WINDOW + WINDOW + EXIT_WINDOW + 180))
@pytest.mark.parametrize("seat_pair", _SEAT_PAIRS)
@pytest.mark.feature("device-to-device-file-transfer")
def test_your_devices_never_broadcast_scan_or_announce_themselves(
    nest_instance, request, seat_pair
):
    """Two seats of one account find each other and move a file directly; all
    the while neither agent holds a discovery, multicast, broadcast or raw
    socket, and neither joins a multicast group."""
    account = _account(nest_instance)
    port, url, secret_key = account[0], account[1], account[2]
    folder = f"quiet-{secrets.token_hex(4)}"
    user_create_folder(port, folder, secret_key=secret_key, base_url=url)

    run_token = sync_seats.new_run_token(seats=2)
    name = f"{run_token}-quiet.bin"
    body = secrets.token_bytes(64 * 1024 + 3)
    tmp = Path(tempfile.mkdtemp(prefix=f"fauna-{folder}-"))
    seat = _seat_factory(nest_instance, request, account, folder, tmp, run_token, seat_pair)
    names = sync_seats.seat_names(2)

    before = agent_sockets.memberships()
    broadcast = agent_sockets.broadcast_addresses()
    during = before.copy()
    pids: dict[str, int] = {}

    def sample() -> None:
        for key, users in agent_sockets.memberships().items():
            during[key] = max(during.get(key, 0), users)

    try:
        with contextlib.ExitStack() as stack:
            a = stack.enter_context(seat(names[0]))
            b = stack.enter_context(seat(names[1]))
            for s in (a, b):
                s.await_ready(STARTUP_WINDOW)
            sample()
            _await_siblings(a, b, "before the reading")
            _await_siblings(b, a, "before the reading")
            (a.path / name).write_bytes(body)
            _await_bytes(b, name, body, "the file the direct path carries", a)
            sample()

            for s in (a, b):
                pid = s.agent_pid()
                where = f"\n--- seat {s.name} ---\n{s.diagnostics()}"
                assert pid, f"seat {s.name} publishes no agent to read{where}"
                assert agent_sockets.same_network_namespace(pid), (
                    f"seat {s.name}'s agent (pid {pid}) runs in another network "
                    f"namespace, so this test's membership baseline is not its own{where}"
                )
                pids[s.name] = pid
                advertised = _advertised_ports(s)
                reading = agent_sockets.observe(pid)
                print(f"[quiet] seat {s.name}: {reading.describe()}", flush=True)

                held = {u.local_port for u in reading.udp}
                assert advertised & held, (
                    f"seat {s.name}'s agent (pid {pid}) holds none of the UDP ports "
                    f"its peer leg advertises ({sorted(advertised)}) — the reading is "
                    f"not of the process that carries the direct path:\n"
                    f"{reading.describe()}{where}"
                )
                found = agent_sockets.findings(reading, broadcast)
                assert not found, (
                    f"seat {s.name}'s agent announces, listens for announcements, or "
                    f"can scan, while the direct path is up:\n  - "
                    + "\n  - ".join(found) + f"\n{reading.describe()}{where}"
                )
            sample()

        # The seats are down; their agents exit with them (unix: the app's
        # group child, reaped by teardown; windows: stopped by its Shutdown).
        wait_until(
            lambda: all(_process_has_exited(pid) for pid in pids.values()),
            EXIT_WINDOW, interval=1.0,
            diagnose=lambda: f"the seats' agents {pids} outlived their apps",
        )
        after = agent_sockets.memberships()
        joined = agent_sockets.memberships_released(before, during, after)
        assert not joined, (
            "a multicast membership rose while the agents ran and fell when they "
            "exited — they joined a group:\n  - " + "\n  - ".join(joined)
        )
    finally:
        shutil.rmtree(tmp, ignore_errors=True)
