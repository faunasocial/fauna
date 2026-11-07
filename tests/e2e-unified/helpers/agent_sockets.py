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
- **windows**: the agent pid's handles (``NtQueryInformationProcess``,
  ``ProcessHandleInformation``), each file handle duplicated into a short-lived
  child of this process and asked whether ``\\Driver\\AFD`` (the Winsock
  driver) is in its path; every one that is answers ``SO_PROTOCOL_INFOW``,
  ``getsockname`` and ``getpeername``. That gives its UDP sockets with both
  endpoints, and as the raw class every ``SOCK_RAW`` socket. Windows hands an
  UNELEVATED process a raw ICMP socket (measured 2026-11-06), so the raw class
  is read, never assumed empty. An AFD handle Winsock refuses as a socket
  (``WSAENOTSOCK``) is an AFD helper: what an IOCP poller (tokio's, libuv's)
  opens on the driver to wait on sockets, with no endpoint behind it. A
  helper opened with no endpoint shows exactly that signature, while every
  socket Winsock made, raw ones included, answers it (both measured), so
  helpers are listed, never counted. The reading's boundary: an endpoint
  opened by bypassing Winsock with undocumented AFD calls would look like a
  helper. ``GetExtendedUdpTable`` is not used: its
  owner-pid rows carry no remote endpoint and no raw socket. Multicast
  memberships come from ``netsh interface ipv4|ipv6 show joins``, the box's
  table (the harness's processes all share the default network compartment);
  its ``References`` column counts the sockets holding a group, rising on a
  join and falling when the socket closes (measured), so a join is attributed
  by timing as on linux. Broadcast addresses come from the prefixes ``netsh
  interface ipv4 show addresses`` prints. Why the reading runs in a child: a
  query on a SYNCHRONOUS handle (a pipe with a read in flight) waits for that
  I/O, and Winsock's import of such a handle then holds every later Winsock
  call in the process (measured). In a child, such a handle costs one bounded
  wait and is reported as unread, a finding, never a pass. Nothing touches the
  pytest process's own sockets.

Everything that parses kernel text or bytes is a pure function of it, pinned by
tier_1 tests with no process in sight. OBSERVE ONLY: nothing here signals,
connects to or alters the agent. (windows: the duplicates are only queried,
and they are closed by the child's exit; closing a duplicate leaves the agent's
own handle, and its socket, as they were.)
"""

from __future__ import annotations

import ipaddress
import json
import os
import struct
import subprocess
import sys
from collections import Counter
from dataclasses import dataclass, field
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
    # raw / raw6 / packet sockets the agent holds (darwin: fds; windows: handles)
    raw_inodes: set[int]
    # Handles the reading could not classify, one sentence each (windows: a
    # synchronous handle with I/O in flight). Never silently dropped: each one
    # is a finding, because the reading cannot rule out a raw socket there.
    unread: list[str] = field(default_factory=list)
    # Handles on the socket driver that are not sockets (windows: AFD helpers,
    # see the module docstring). Listed so a reader sees them; not a finding.
    helpers: set[int] = field(default_factory=set)

    def describe(self) -> str:
        lines = [f"agent pid {self.pid}: {len(self.udp)} UDP socket(s)"]
        lines += [f"  {s.describe()}" for s in self.udp]
        if self.raw_inodes:
            lines.append(f"  raw/packet socket inodes: {sorted(self.raw_inodes)}")
        if self.helpers:
            lines.append(
                "  AFD helper handles (no socket behind them): "
                + ", ".join(f"{h:#x}" for h in sorted(self.helpers))
            )
        lines += [f"  unread: {u}" for u in self.unread]
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
    found += [
        f"{u} — the reading cannot rule out a raw socket there" for u in sockets.unread
    ]
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


def _ip_or_none(text: str):
    try:
        return ipaddress.ip_address(text.split("%")[0])
    except ValueError:
        return None


def parse_netsh_joins(text: str) -> Counter:
    """``netsh interface ipv4|ipv6 show joins`` → ``{(iface, group): references}``.

    An interface header (``Interface 7: Ethernet``) opens a block; each row
    under it is ``<scope> <references> <last> <group>``. Only the shape is read
    (the header's text after its first colon, a row's second and last fields),
    never the localized words."""
    groups: Counter = Counter()
    iface = None
    for line in text.splitlines():
        fields = line.split()
        if len(fields) >= 4 and fields[1].isdigit() and _ip_or_none(fields[-1]):
            if iface is not None:
                groups[(iface, str(_ip_or_none(fields[-1])))] += int(fields[1])
        elif ":" in line and not line.startswith((" ", "\t", "-")):
            iface = line.split(":", 1)[1].strip()
    return groups


def parse_netsh_broadcast(text: str) -> set:
    """The broadcast address of every IPv4 prefix ``netsh interface ipv4 show
    addresses`` prints (``10.1.8.0/24`` → ``10.1.8.255``). A /31 or /32 has no
    broadcast address (its "broadcast" would be a host), so it adds none."""
    found = set()
    for token in text.split():
        if "/" not in token:
            continue
        try:
            net = ipaddress.IPv4Network(token, strict=False)
        except ValueError:
            continue
        if net.prefixlen <= 30:
            found.add(net.broadcast_address)
    return found


# Windows address families and socket types, as Winsock numbers them.
_AF_INET, _AF_INET6 = 2, 23
_SOCK_DGRAM, _SOCK_RAW = 2, 3


def parse_process_handle_snapshot(buf: bytes) -> list[tuple[int, int, int]]:
    """A ``PROCESS_HANDLE_SNAPSHOT_INFORMATION`` (64-bit layout — every fleet
    windows python is 64-bit) → ``[(handle, granted_access, object_type_index)]``.

    Header: ``NumberOfHandles``, ``Reserved`` (pointer-sized each); then one
    40-byte ``PROCESS_HANDLE_TABLE_ENTRY_INFO`` per handle: ``HandleValue``,
    ``HandleCount``, ``PointerCount`` (pointer-sized), ``GrantedAccess``,
    ``ObjectTypeIndex``, ``HandleAttributes``, ``Reserved`` (ULONG each)."""
    (count,) = struct.unpack_from("<Q", buf, 0)
    rows = []
    for i in range(count):
        value, _, _, access, type_index, _, _ = struct.unpack_from("<QQQIIII", buf, 16 + 40 * i)
        rows.append((value, access, type_index))
    return rows


def decode_sockaddr(hex_text: str | None, family: int):
    """A Winsock ``SOCKADDR_IN``/``SOCKADDR_IN6`` (hex of its bytes) → ``(ip,
    port)``; ``None`` (an unbound socket's name, an unconnected one's peer) →
    the family's unspecified address and port 0, as linux prints it."""
    if not hex_text:
        return ipaddress.ip_address("::" if family == _AF_INET6 else "0.0.0.0"), 0
    raw = bytes.fromhex(hex_text)
    port = int.from_bytes(raw[2:4], "big")
    if family == _AF_INET6:
        return ipaddress.IPv6Address(raw[8:24]), port
    return ipaddress.IPv4Address(raw[4:8]), port


def windows_reading(pid: int, doc: dict) -> AgentSockets:
    """The child's JSON (:func:`_windows_socket_handles`) → an
    :class:`AgentSockets`: its ``SOCK_DGRAM`` internet sockets as UDP rows (the
    handle in ``inode``), its ``SOCK_RAW`` sockets as the raw class, its AFD
    helpers as listed handles, and every handle it could not classify as
    unread."""
    udp, raw = [], set()
    for s in doc.get("sockets", ()):
        family, kind = s["family"], s["type"]
        if kind == _SOCK_RAW:
            raw.add(s["handle"])
        elif kind == _SOCK_DGRAM and family in (_AF_INET, _AF_INET6):
            local, local_port = decode_sockaddr(s.get("local"), family)
            remote, remote_port = decode_sockaddr(s.get("remote"), family)
            udp.append(UdpSocket(
                "udp6" if family == _AF_INET6 else "udp", local, local_port,
                remote, remote_port, s["handle"],
            ))
    unread = [
        f"handle {u['handle']:#x} (access {u['access']:#x}): {u['why']}"
        for u in doc.get("unread", ())
    ]
    helpers = {h["handle"] for h in doc.get("helpers", ())}
    return AgentSockets(pid=pid, udp=udp, raw_inodes=raw, unread=unread, helpers=helpers)


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
        # The unprivileged ICMP "ping" sockets (SOCK_DGRAM + IPPROTO_ICMP): a
        # ping sweep needs exactly one, and the tables share raw's layout.
        | parse_raw_inodes(_read(net / "icmp"))
        | parse_raw_inodes(_read(net / "icmp6"))
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


# ── windows readings ─────────────────────────────────────────────────────────

# The flag this file answers when run as the windows reader's child.
_WINDOWS_CHILD_FLAG = "--windows-socket-handles"
# How long one handle may take to answer before it is reported unread. A
# handle that answers at all answers in milliseconds (measured); one that does
# not is waiting on I/O in flight, and waiting longer reads nothing more.
_WINDOWS_HANDLE_WAIT = 1.0
# The whole child's bound, for the parent: generous over a few hundred
# handles, each at most one wait.
_WINDOWS_READ_TIMEOUT = 300.0


def _windows_observe(pid: int) -> AgentSockets:
    """Read agent ``pid``'s sockets through a child of this process (the module
    docstring says why a child). The child runs this file in isolated mode
    (``-I``: no user site, no script directory on the path), so it imports
    nothing but the standard library."""
    out = subprocess.run(
        [sys.executable, "-I", __file__, _WINDOWS_CHILD_FLAG, str(pid)],
        capture_output=True, text=True, check=False, timeout=_WINDOWS_READ_TIMEOUT,
    )
    if out.returncode != 0:
        raise RuntimeError(
            f"the windows socket reader for pid {pid} exited {out.returncode}:\n"
            f"{out.stderr.strip()}"
        )
    return windows_reading(pid, json.loads(out.stdout))


def _windows_socket_handles(pid: int) -> dict:
    """The child's half: classify every file handle of ``pid``. Runs only in
    the child (:func:`_windows_observe`), and only on windows.

    Each handle is duplicated (same access) and held until the child exits, so
    no handle value is reused within the reading: Winsock remembers a socket
    by handle value, and a value reused after a close would answer as the
    socket it used to be (measured). Each query runs in its own thread with a
    bounded wait; a handle that does not answer is unread. Once one socket
    has not answered Winsock, every later Winsock call would wait behind it
    (measured), so the sockets after it are reported unasked rather than
    asked."""
    import ctypes
    import ctypes.wintypes as wt
    import msvcrt
    import socket  # noqa: F401 — its import starts Winsock for the ws2_32 calls
    import threading

    k32 = ctypes.WinDLL("kernel32", use_last_error=True)
    ntdll = ctypes.WinDLL("ntdll")
    ws2 = ctypes.WinDLL("ws2_32", use_last_error=True)
    k32.OpenProcess.restype = wt.HANDLE
    k32.OpenProcess.argtypes = [wt.DWORD, wt.BOOL, wt.DWORD]
    k32.GetCurrentProcess.restype = wt.HANDLE
    k32.DuplicateHandle.argtypes = [
        wt.HANDLE, wt.HANDLE, wt.HANDLE, ctypes.POINTER(wt.HANDLE), wt.DWORD, wt.BOOL, wt.DWORD,
    ]
    ntdll.NtQueryInformationProcess.restype = ctypes.c_long
    ntdll.NtQueryInformationProcess.argtypes = [
        wt.HANDLE, ctypes.c_int, ctypes.c_void_p, wt.ULONG, ctypes.POINTER(wt.ULONG),
    ]
    ntdll.NtQueryVolumeInformationFile.restype = ctypes.c_long
    ntdll.NtQueryVolumeInformationFile.argtypes = [
        wt.HANDLE, ctypes.c_void_p, ctypes.c_void_p, wt.ULONG, ctypes.c_int,
    ]
    ws2.getsockopt.argtypes = [
        ctypes.c_size_t, ctypes.c_int, ctypes.c_int, ctypes.c_char_p, ctypes.POINTER(ctypes.c_int),
    ]
    for fn in (ws2.getsockname, ws2.getpeername):
        fn.argtypes = [ctypes.c_size_t, ctypes.c_char_p, ctypes.POINTER(ctypes.c_int)]

    process_query_information, process_dup_handle = 0x0400, 0x0040
    process_handle_information = 51
    status_info_length_mismatch = 0xC0000004
    error_invalid_handle = 6
    wsaenotsock = 10038
    # Statuses that say "not on AFD": the device refuses the query (AFD itself
    # answers it — measured on UDP and raw sockets — and the answer is the
    # device's, the same for every socket), or the value was closed or reused
    # by a non-file object between the snapshot and the duplicate.
    not_a_socket = {
        0xC0000010: "STATUS_INVALID_DEVICE_REQUEST",
        0xC0000024: "STATUS_OBJECT_TYPE_MISMATCH",
        0xC0000008: "STATUS_INVALID_HANDLE",
    }
    file_fs_driver_path_information = 9
    duplicate_same_access = 0x2
    sol_socket, so_protocol_info_w = 0xFFFF, 0x2005
    afd = "\\Driver\\AFD".encode("utf-16-le")

    def handles_of(process) -> list[tuple[int, int, int]]:
        size = 64 * 1024
        while True:
            buf = ctypes.create_string_buffer(size)
            needed = wt.ULONG(0)
            status = ntdll.NtQueryInformationProcess(
                process, process_handle_information, buf, size, ctypes.byref(needed)
            ) & 0xFFFFFFFF
            if status == status_info_length_mismatch:
                size = max(needed.value, size * 2)
                continue
            if status != 0:
                raise OSError(f"NtQueryInformationProcess(ProcessHandleInformation): {status:#x}")
            return parse_process_handle_snapshot(buf.raw[: needed.value or size])

    def afd_in_path(handle: int):
        # FILE_FS_DRIVER_PATH_INFORMATION: BOOLEAN DriverInPath; ULONG
        # DriverNameLength; WCHAR DriverName[] — answered by the I/O manager.
        buf = ctypes.create_string_buffer(8 + len(afd))
        buf[4:8] = struct.pack("<I", len(afd))
        buf[8:] = afd
        iosb = (ctypes.c_ulonglong * 2)()
        status = ntdll.NtQueryVolumeInformationFile(
            handle, iosb, buf, len(buf), file_fs_driver_path_information
        ) & 0xFFFFFFFF
        if status in not_a_socket:
            return False
        if status != 0:
            return f"NtQueryVolumeInformationFile(FileFsDriverPathInformation): {status:#x}"
        return bool(buf.raw[0])

    def endpoint(fn, handle: int):
        buf = ctypes.create_string_buffer(128)
        size = ctypes.c_int(128)
        return buf.raw[: size.value].hex() if fn(handle, buf, ctypes.byref(size)) == 0 else None

    def socket_facts(handle: int):
        info = ctypes.create_string_buffer(1024)
        size = ctypes.c_int(1024)
        if ws2.getsockopt(handle, sol_socket, so_protocol_info_w, info, ctypes.byref(size)) != 0:
            return ctypes.get_last_error()
        # WSAPROTOCOL_INFOW: iAddressFamily at 76, iSocketType at 88, iProtocol at 92.
        family, = struct.unpack_from("<i", info.raw, 76)
        kind, protocol = struct.unpack_from("<ii", info.raw, 88)
        return {
            "family": family, "type": kind, "protocol": protocol,
            "local": endpoint(ws2.getsockname, handle),
            "remote": endpoint(ws2.getpeername, handle),
        }

    def answer(fn, *args):
        out: list = []
        thread = threading.Thread(target=lambda: out.append(fn(*args)), daemon=True)
        thread.start()
        thread.join(_WINDOWS_HANDLE_WAIT)
        return out[0] if out else None

    me = k32.GetCurrentProcess()
    agent = k32.OpenProcess(process_query_information | process_dup_handle, False, pid)
    if not agent:
        raise OSError(ctypes.get_last_error(), f"OpenProcess({pid}) failed")
    with open(os.devnull, "rb") as probe:
        own = {value: kind for value, _, kind in handles_of(me)}
        file_type = own[msvcrt.get_osfhandle(probe.fileno())]

    sockets: list[dict] = []
    helpers: list[dict] = []
    unread: list[dict] = []
    held = []  # every duplicate stays open until exit (no handle value reused)
    winsock_waiting = False
    for value, access, kind in handles_of(agent):
        if kind != file_type:
            continue
        dup = wt.HANDLE()
        if not k32.DuplicateHandle(agent, value, me, ctypes.byref(dup), 0, False,
                                   duplicate_same_access):
            error = ctypes.get_last_error()
            if error != error_invalid_handle:  # else: closed since the snapshot
                unread.append({"handle": value, "access": access, "why":
                               f"DuplicateHandle failed: Win32 error {error}"})
            continue
        held.append(dup)
        on_afd = answer(afd_in_path, dup.value)
        if on_afd is None:
            unread.append({"handle": value, "access": access, "why":
                           f"did not say within {_WINDOWS_HANDLE_WAIT:g} s whether it is a "
                           f"socket (a synchronous handle with I/O in flight)"})
        elif isinstance(on_afd, str):
            unread.append({"handle": value, "access": access, "why": on_afd})
        elif not on_afd:
            continue
        elif winsock_waiting:
            unread.append({"handle": value, "access": access, "why":
                           "a socket left unasked: Winsock waits behind an earlier "
                           "socket that did not answer"})
        else:
            facts = answer(socket_facts, dup.value)
            if facts is None:
                winsock_waiting = True
                unread.append({"handle": value, "access": access, "why":
                               f"a socket that did not answer Winsock within "
                               f"{_WINDOWS_HANDLE_WAIT:g} s"})
            elif facts == wsaenotsock:
                helpers.append({"handle": value, "access": access})
            elif isinstance(facts, int):
                unread.append({"handle": value, "access": access, "why":
                               f"getsockopt(SO_PROTOCOL_INFOW) failed: WSA error {facts}"})
            else:
                sockets.append({"handle": value, **facts})
    return {"sockets": sockets, "helpers": helpers, "unread": unread}


def observe(pid: int) -> AgentSockets:
    """Read agent ``pid``'s sockets on this platform."""
    if sys.platform.startswith("linux"):
        return _linux_observe(pid)
    if sys.platform == "darwin":
        return _darwin_observe(pid)
    if sys.platform == "win32":
        return _windows_observe(pid)
    raise NotImplementedError(
        f"no socket observation for {sys.platform} yet — the journey's OWED_SEATS "
        f"keeps this platform from collecting it"
    )


def same_network_namespace(pid: int) -> bool:
    """Whether ``pid`` shares this process's network namespace — the condition
    under which a membership baseline read here is a baseline for it."""
    if sys.platform in ("darwin", "win32"):
        # darwin has no network namespaces, and on windows every process the
        # harness starts shares the default network compartment: the box's one
        # membership table, so the baseline read here is always the agent's.
        return True
    return os.readlink(f"/proc/{pid}/ns/net") == os.readlink("/proc/self/ns/net")


def memberships(proc: str = "/proc/self") -> Counter:
    """This network namespace's multicast memberships, IPv4 and IPv6 (darwin:
    the box's, from ``netstat -g``; windows: the box's, from ``netsh … show
    joins``; ``proc`` is linux-only)."""
    if sys.platform == "darwin":
        return parse_netstat_groups(_run(["netstat", "-g", "-n"]))
    if sys.platform == "win32":
        return sum(
            (parse_netsh_joins(_run(["netsh", "interface", family, "show", "joins"]))
             for family in ("ipv4", "ipv6")),
            Counter(),
        )
    root = Path(proc) / "net"
    return parse_igmp(_read(root / "igmp")) + parse_igmp6(_read(root / "igmp6"))


def broadcast_addresses() -> set:
    """Every IPv4 broadcast address on this box's interfaces (``ip -j addr``;
    darwin: ``ifconfig``; windows: ``netsh interface ipv4 show addresses``)."""
    if sys.platform == "darwin":
        return parse_ifconfig_broadcast(_run(["ifconfig"]))
    if sys.platform == "win32":
        return parse_netsh_broadcast(_run(["netsh", "interface", "ipv4", "show", "addresses"]))
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
    "decode_sockaddr",
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
    "parse_netsh_broadcast",
    "parse_netsh_joins",
    "parse_netstat_groups",
    "parse_packet_inodes",
    "parse_process_handle_snapshot",
    "parse_raw_inodes",
    "parse_udp_table",
    "same_network_namespace",
    "socket_inodes",
    "windows_reading",
]


if __name__ == "__main__" and sys.argv[1:2] == [_WINDOWS_CHILD_FLAG]:
    # The windows reader's child (`_windows_observe`). `os._exit`, not a return:
    # a thread still waiting on an unanswered handle must not hold the exit.
    sys.stdout.write(json.dumps(_windows_socket_handles(int(sys.argv[2]))))
    sys.stdout.flush()
    os._exit(0)
