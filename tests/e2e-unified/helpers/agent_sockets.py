"""What a sync agent's own sockets say about how it finds its siblings.

Owner: ``docs/goal/behavior/p2p.md`` § LAN detection (no broadcast, mDNS or
network scanning: the LAN check is arithmetic against addresses a peer already
advertised over an authenticated channel) and § Wormability posture rule 1 (no
DHT, no broadcast, no global address space to scan).

**The observation, per platform.** One journey
(``test_device_direct_path_never_announces_or_scans.py``) asks one question —
*does this agent hold a socket that announces, listens for announcements, or
scans?* — and each platform answers it from what its own kernel shows about the
agent's process:

- **linux** (here): the agent pid's socket inodes (``/proc/<pid>/fd``) matched
  against its network namespace's ``udp``/``udp6``/``raw``/``raw6``/``packet``
  tables, plus the namespace's multicast memberships (``igmp``/``igmp6``).
- **macos**: the agent pid's open files from ``lsof -nP -a -p <pid> -F pftPn``
  — its UDP sockets with their local and remote endpoints, and as the raw
  class every other internet socket that is neither UDP nor TCP (``ICMP``, a
  raw IP socket) plus any ``/dev/bpf*`` device (darwin's packet capture, what
  an ARP sweep opens); the system's multicast memberships from ``netstat -g``
  (darwin has no network namespaces, so the table is the box's); broadcast
  addresses from ``ifconfig``. ``netstat -g`` lists each group once per
  interface with no user count, so a join of a group the box already holds
  (``mDNSResponder``'s 224.0.0.251, the all-hosts groups) does not rise and
  is not attributable by timing: those groups are the discovery ones, and an
  agent joining them to listen binds their port, which the socket reading
  names.
- **windows**: owed (``GetExtendedUdpTable`` by owning pid). It lands as one
  more function under :func:`observe`, never as a separate test.

Everything that parses kernel text is a pure function of that text, pinned by
tier_1 tests with no process in sight. OBSERVE ONLY: nothing here signals,
connects to or alters the agent.
"""

from __future__ import annotations

import ipaddress
import json
import os
import subprocess
import sys
from collections import Counter
from dataclasses import dataclass
from pathlib import Path

# Ports whose only use is local-network discovery: mDNS, SSDP (UPnP — what a
# port mapper speaks to find the router), LLMNR, and the WS-Discovery / NetBIOS
# name service pair. An agent bound to any of them is listening for, or
# answering, announcements.
DISCOVERY_PORTS = {
    5353: "mDNS",
    1900: "SSDP (UPnP discovery)",
    5355: "LLMNR",
    3702: "WS-Discovery",
    137: "NetBIOS name service",
}

_IPV4_LIMITED_BROADCAST = ipaddress.ip_address("255.255.255.255")


@dataclass(frozen=True)
class UdpSocket:
    family: str  # "udp" | "udp6"
    local: ipaddress.IPv4Address | ipaddress.IPv6Address
    local_port: int
    remote: ipaddress.IPv4Address | ipaddress.IPv6Address
    remote_port: int
    inode: int

    def describe(self) -> str:
        return f"{self.family} {self.local}:{self.local_port} -> {self.remote}:{self.remote_port}"


@dataclass
class AgentSockets:
    """One reading of an agent process's sockets."""

    pid: int
    udp: list[UdpSocket]
    raw_inodes: set[int]  # raw / raw6 / packet sockets the agent holds

    def describe(self) -> str:
        lines = [f"agent pid {self.pid}: {len(self.udp)} UDP socket(s)"]
        lines += [f"  {s.describe()}" for s in self.udp]
        if self.raw_inodes:
            lines.append(f"  raw/packet socket inodes: {sorted(self.raw_inodes)}")
        return "\n".join(lines)


# ── pure parsers ─────────────────────────────────────────────────────────────


def _hex_v4(word: str) -> ipaddress.IPv4Address:
    # /proc prints the in-memory (network-order) address as a host-order u32.
    return ipaddress.IPv4Address(int(word, 16).to_bytes(4, "little"))


def _hex_v6(text: str) -> ipaddress.IPv6Address:
    # Four host-order u32 words, each printed as 8 hex digits.
    raw = b"".join(int(text[i:i + 8], 16).to_bytes(4, "little") for i in range(0, 32, 8))
    return ipaddress.IPv6Address(raw)


def _endpoint(text: str, family: str):
    addr, port = text.split(":")
    ip = _hex_v4(addr) if family == "udp" else _hex_v6(addr)
    return ip, int(port, 16)


def parse_udp_table(text: str, family: str) -> list[UdpSocket]:
    """Rows of ``/proc/<pid>/net/udp`` (``family="udp"``) or ``udp6``."""
    rows = []
    for line in text.splitlines()[1:]:
        fields = line.split()
        if len(fields) < 10:
            continue
        local, local_port = _endpoint(fields[1], family)
        remote, remote_port = _endpoint(fields[2], family)
        rows.append(UdpSocket(family, local, local_port, remote, remote_port, int(fields[9])))
    return rows


def parse_raw_inodes(text: str) -> set[int]:
    """Socket inodes of ``/proc/<pid>/net/raw`` / ``raw6`` (inode in column 10)."""
    return {
        int(fields[9])
        for fields in (line.split() for line in text.splitlines()[1:])
        if len(fields) >= 10
    }


def parse_packet_inodes(text: str) -> set[int]:
    """Socket inodes of ``/proc/<pid>/net/packet`` (inode is the last column)."""
    return {
        int(fields[-1])
        for fields in (line.split() for line in text.splitlines()[1:])
        if fields
    }


def parse_igmp(text: str) -> Counter:
    """``/proc/<pid>/net/igmp`` → ``{(iface, group): users}``.

    Device lines (``<idx>\\t<dev> : <count> <querier>``) open a block; the
    indented lines under one are its groups (``<group-hex> <users> …``).
    """
    groups: Counter = Counter()
    dev = None
    for line in text.splitlines()[1:]:
        if not line.strip():
            continue
        if not line[0].isspace():
            dev = line.split()[1]
            continue
        fields = line.split()
        if dev is not None and len(fields) >= 2:
            groups[(dev, str(_hex_v4(fields[0])))] += int(fields[1])
    return groups


def parse_igmp6(text: str) -> Counter:
    """``/proc/<pid>/net/igmp6`` → ``{(iface, group): users}``."""
    groups: Counter = Counter()
    for line in text.splitlines():
        fields = line.split()
        if len(fields) >= 4:
            # Unlike udp6, igmp6 prints the address's bytes in network order.
            group = ipaddress.IPv6Address(bytes.fromhex(fields[2]))
            groups[(fields[1], str(group))] += int(fields[3])
    return groups


def socket_inodes(fd_links: list[str]) -> set[int]:
    """The inodes of every ``socket:[N]`` among an fd directory's link targets."""
    return {
        int(link[len("socket:["):-1])
        for link in fd_links
        if link.startswith("socket:[") and link.endswith("]")
    }


def findings(sockets: AgentSockets, broadcast_addresses: set) -> list[str]:
    """Every way ``sockets`` breaks the no-broadcast / no-discovery / no-scan
    posture, as one sentence each. Empty means none."""
    found = []
    for s in sockets.udp:
        if s.local_port in DISCOVERY_PORTS:
            found.append(
                f"{s.describe()}: bound to the {DISCOVERY_PORTS[s.local_port]} port {s.local_port}"
            )
        for end, ip in (("bound to", s.local), ("connected to", s.remote)):
            if ip.is_multicast:
                found.append(f"{s.describe()}: {end} the multicast address {ip}")
            elif ip == _IPV4_LIMITED_BROADCAST or ip in broadcast_addresses:
                found.append(f"{s.describe()}: {end} the broadcast address {ip}")
        if s.remote_port in DISCOVERY_PORTS:
            found.append(
                f"{s.describe()}: connected to the {DISCOVERY_PORTS[s.remote_port]} port"
            )
    if sockets.raw_inodes:
        found.append(
            f"holds {len(sockets.raw_inodes)} raw or packet socket(s) — what a network "
            f"scan or ARP sweep needs, and nothing a QUIC endpoint does"
        )
    return found


def memberships_released(before: Counter, during: Counter, after: Counter) -> list[str]:
    """Multicast memberships the agents held, attributed by timing.

    The kernel keeps one membership table per network namespace, not per
    process, so a reading taken while the agents run cannot say whose a join
    is. A membership dies with its socket, though: one ABOVE the baseline while
    the agents ran that is GONE once they have exited was theirs (a sibling's
    join that outlives the agents is not). An interface absent from the
    baseline is ignored — a container's veth coming up carries the kernel's own
    all-hosts joins, nobody's announcement.
    """
    ifaces = {iface for iface, _ in before}
    found = []
    for key, users in sorted(during.items()):
        iface, group = key
        if iface not in ifaces:
            continue
        rose = users - before.get(key, 0)
        released = users - after.get(key, 0)
        if rose > 0 and released > 0:
            found.append(
                f"{group} on {iface}: {before.get(key, 0)} member(s) before the agents "
                f"started, {users} while they ran, {after.get(key, 0)} after they exited"
            )
    return found


def parse_lsof_fields(text: str) -> list[dict[str, str]]:
    """``lsof -F pftPn`` output → one ``{field-letter: value}`` per open file.

    Each line is one field: its first character names it, the rest is the
    value; a ``p`` line opens a process, an ``f`` line opens a file under it.
    """
    files: list[dict[str, str]] = []
    for line in text.splitlines():
        if not line:
            continue
        key, value = line[0], line[1:]
        if key == "p":
            continue
        if key == "f":
            files.append({"f": value})
        elif files:
            files[-1][key] = value
    return files


def _lsof_endpoint(text: str, v6: bool):
    """``*:51013``, ``10.1.8.36:5000``, ``[fe80::1%en0]:5000``, ``*:*``."""
    host, _, port = text.rpartition(":")
    host = host.strip("[]").split("%")[0]
    if host in ("", "*"):
        ip = ipaddress.ip_address("::" if v6 else "0.0.0.0")
    else:
        ip = ipaddress.ip_address(host)
    return ip, 0 if port in ("", "*") else int(port)


def parse_lsof_udp(files: list[dict[str, str]]) -> list[UdpSocket]:
    """The UDP sockets among :func:`parse_lsof_fields` rows; ``inode`` holds the
    fd number (darwin sockets have no inode)."""
    rows = []
    for f in files:
        if f.get("P") != "UDP" or f.get("t") not in ("IPv4", "IPv6"):
            continue
        v6 = f["t"] == "IPv6"
        local_text, _, remote_text = f.get("n", "*:*").partition("->")
        local, local_port = _lsof_endpoint(local_text, v6)
        remote, remote_port = _lsof_endpoint(remote_text or "*:*", v6)
        fd = "".join(ch for ch in f["f"] if ch.isdigit())
        rows.append(UdpSocket(
            "udp6" if v6 else "udp", local, local_port, remote, remote_port,
            int(fd) if fd else -1,
        ))
    return rows


def parse_lsof_raw(files: list[dict[str, str]]) -> set[int]:
    """The fds of every scan-capable file among :func:`parse_lsof_fields` rows:
    an internet socket that is neither UDP nor TCP (ICMP, a raw IP socket) or
    an open ``/dev/bpf*`` device (darwin's packet socket)."""
    found = set()
    for f in files:
        raw_socket = f.get("t") in ("IPv4", "IPv6") and f.get("P") not in ("UDP", "TCP")
        bpf = f.get("n", "").startswith("/dev/bpf")
        fd = "".join(ch for ch in f["f"] if ch.isdigit())
        if (raw_socket or bpf) and fd:
            found.add(int(fd))
    return found


def parse_netstat_groups(text: str) -> Counter:
    """``netstat -g -n`` → ``{(iface, group): 1}`` for its IPv4 and IPv6 group
    sections (the link-layer section is skipped). darwin prints no user count,
    so each group present counts once; an IPv6 group's ``%scope`` is dropped
    to match the linux key."""
    groups: Counter = Counter()
    section = None
    for line in text.splitlines():
        if line.endswith("Multicast Group Memberships"):
            section = line.split()[0]
            continue
        fields = line.split()
        if section not in ("IPv4", "IPv6") or len(fields) < 3 or fields[0] == "Group":
            continue
        group = str(ipaddress.ip_address(fields[0].split("%")[0]))
        groups[(fields[-1], group)] = 1
    return groups


def parse_ifconfig_broadcast(text: str) -> set:
    """Every ``broadcast <addr>`` on an ``inet`` line of ``ifconfig`` output."""
    found = set()
    for line in text.splitlines():
        fields = line.split()
        if fields[:1] == ["inet"] and "broadcast" in fields:
            found.add(ipaddress.ip_address(fields[fields.index("broadcast") + 1]))
    return found


# ── linux readings ───────────────────────────────────────────────────────────


def _read(path: Path) -> str:
    try:
        return path.read_text()
    except OSError:
        return ""


def _linux_observe(pid: int) -> AgentSockets:
    proc = Path(f"/proc/{pid}")
    fd_dir = proc / "fd"
    links = []
    for fd in fd_dir.iterdir():
        try:
            links.append(os.readlink(fd))
        except OSError:
            pass  # closed between listing and reading
    inodes = socket_inodes(links)
    net = proc / "net"
    udp = [
        s
        for family in ("udp", "udp6")
        for s in parse_udp_table(_read(net / family), family)
        if s.inode in inodes
    ]
    raw = (
        parse_raw_inodes(_read(net / "raw"))
        | parse_raw_inodes(_read(net / "raw6"))
        | parse_packet_inodes(_read(net / "packet"))
    )
    return AgentSockets(pid=pid, udp=udp, raw_inodes=raw & inodes)


# ── darwin readings ──────────────────────────────────────────────────────────


def _run(argv: list[str]) -> str:
    # lsof exits 1 when a selection matches nothing; the reading is then empty
    # and the journey's own control (the advertised port must be held) fails it.
    return subprocess.run(argv, capture_output=True, text=True, check=False).stdout


def _darwin_observe(pid: int) -> AgentSockets:
    files = parse_lsof_fields(_run(["lsof", "-nP", "-a", "-p", str(pid), "-F", "pftPn"]))
    return AgentSockets(pid=pid, udp=parse_lsof_udp(files), raw_inodes=parse_lsof_raw(files))


def observe(pid: int) -> AgentSockets:
    """Read agent ``pid``'s sockets on this platform."""
    if sys.platform.startswith("linux"):
        return _linux_observe(pid)
    if sys.platform == "darwin":
        return _darwin_observe(pid)
    raise NotImplementedError(
        f"no socket observation for {sys.platform} yet — the journey's OWED_SEATS "
        f"keeps this platform from collecting it"
    )


def same_network_namespace(pid: int) -> bool:
    """Whether ``pid`` shares this process's network namespace — the condition
    under which a membership baseline read here is a baseline for it."""
    if sys.platform == "darwin":
        # darwin has no network namespaces: every process shares the box's one
        # membership table, so the baseline read here is always the agent's.
        return True
    return os.readlink(f"/proc/{pid}/ns/net") == os.readlink("/proc/self/ns/net")


def memberships(proc: str = "/proc/self") -> Counter:
    """This network namespace's multicast memberships, IPv4 and IPv6 (darwin:
    the box's, from ``netstat -g``; ``proc`` is linux-only)."""
    if sys.platform == "darwin":
        return parse_netstat_groups(_run(["netstat", "-g", "-n"]))
    root = Path(proc) / "net"
    return parse_igmp(_read(root / "igmp")) + parse_igmp6(_read(root / "igmp6"))


def broadcast_addresses() -> set:
    """Every IPv4 broadcast address on this box's interfaces (``ip -j addr``;
    darwin: ``ifconfig``)."""
    if sys.platform == "darwin":
        return parse_ifconfig_broadcast(_run(["ifconfig"]))
    out = subprocess.run(
        ["ip", "-j", "-4", "addr", "show"], capture_output=True, text=True, check=True
    ).stdout
    found = set()
    for link in json.loads(out):
        for info in link.get("addr_info", []):
            if info.get("broadcast"):
                found.add(ipaddress.ip_address(info["broadcast"]))
    return found


__all__ = [
    "DISCOVERY_PORTS",
    "AgentSockets",
    "UdpSocket",
    "broadcast_addresses",
    "findings",
    "memberships",
    "memberships_released",
    "observe",
    "parse_ifconfig_broadcast",
    "parse_igmp",
    "parse_igmp6",
    "parse_lsof_fields",
    "parse_lsof_raw",
    "parse_lsof_udp",
    "parse_netstat_groups",
    "parse_packet_inodes",
    "parse_raw_inodes",
    "parse_udp_table",
    "same_network_namespace",
    "socket_inodes",
]
