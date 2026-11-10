"""The kernel-text parsers under the no-announce/no-scan witness
(``helpers/agent_sockets.py``), pinned on fixed ``/proc`` (linux) and
``lsof`` / ``netstat`` / ``ifconfig`` (macos) samples so a parse slip cannot
pass the journey vacuously."""

import ipaddress
from collections import Counter

import pytest

from helpers import agent_sockets as a

pytestmark = pytest.mark.tier_1

UDP = """\
  sl  local_address rem_address   st tx_queue rx_queue tr tm->when retrnsmt   uid  timeout inode ref pointer drops
  12: 00000000:C350 00000000:0000 07 00000000:00000000 00:00000000 00000000  1000        0 4242 2 0000000000000000 0
  13: 0100007F:14E9 FB0000E0:14E9 01 00000000:00000000 00:00000000 00000000  1000        0 4343 2 0000000000000000 0
"""

UDP6 = """\
  sl  local_address                         remote_address                        st tx_queue rx_queue tr tm->when retrnsmt   uid  timeout inode ref pointer drops
47167: 00000000000000000000000000000000:C351 00000000000000000000000000000000:0000 07 00000000:00000000 00:00000000 00000000  1000        0 4444 2 0000000000000000 0
"""

IGMP = """\
Idx\tDevice    : Count Querier\tGroup    Users Timer\tReporter
1\tlo        :     2      V3
\t\t\t\tFB0000E0     1 0:00000000\t\t0
\t\t\t\t010000E0     1 0:00000000\t\t0
2\tenp0s5    :     2      V3
\t\t\t\tFB0000E0     2 0:00000000\t\t0
"""

IGMP6 = """\
1    lo              ff0200000000000000000000000000fb     1 00000004 0
2    enp0s5          ff020000000000000000000000000001     1 0000000C 0
"""


def test_udp_rows_decode_address_port_and_inode():
    rows = a.parse_udp_table(UDP, "udp")
    assert [(str(r.local), r.local_port, str(r.remote), r.remote_port, r.inode) for r in rows] == [
        ("0.0.0.0", 50000, "0.0.0.0", 0, 4242),
        ("127.0.0.1", 5353, "224.0.0.251", 5353, 4343),
    ]
    (v6,) = a.parse_udp_table(UDP6, "udp6")
    assert (str(v6.local), v6.local_port, v6.inode) == ("::", 50001, 4444)


def test_v6_words_are_host_order():
    text = "  sl hdr\n 0: 000080FE00000000FF020000FEDCBA98:0035 00000000000000000000000000000000:0000 07 0 0 0 0 0 4545 2 0 0\n"
    (row,) = a.parse_udp_table(text, "udp6")
    assert row.local == ipaddress.ip_address("fe80::2ff:98ba:dcfe")


def test_socket_inodes_keep_only_sockets():
    assert a.socket_inodes(["socket:[4242]", "pipe:[9]", "/dev/null", "socket:[7]"]) == {4242, 7}


def test_igmp_tables_count_users_per_interface():
    assert a.parse_igmp(IGMP) == Counter({
        ("lo", "224.0.0.251"): 1, ("lo", "224.0.0.1"): 1, ("enp0s5", "224.0.0.251"): 2,
    })
    assert a.parse_igmp6(IGMP6) == Counter({
        ("lo", "ff02::fb"): 1, ("enp0s5", "ff02::1"): 1,
    })


def test_findings_name_discovery_ports_multicast_broadcast_and_raw():
    bcast = {ipaddress.ip_address("10.211.55.255")}
    clean = a.AgentSockets(1, a.parse_udp_table(UDP, "udp")[:1], set())
    assert a.findings(clean, bcast) == []

    dirty_rows = a.parse_udp_table(UDP, "udp")
    dirty_rows.append(a.UdpSocket(
        "udp", ipaddress.ip_address("0.0.0.0"), 40000,
        ipaddress.ip_address("10.211.55.255"), 9, 1,
    ))
    found = a.findings(a.AgentSockets(1, dirty_rows, {99}), bcast)
    text = "\n".join(found)
    assert "mDNS port 5353" in text
    assert "multicast address 224.0.0.251" in text
    assert "broadcast address 10.211.55.255" in text
    assert "raw or packet socket" in text


def test_a_membership_is_attributed_only_when_it_dies_with_the_agents():
    before = Counter({("eth0", "224.0.0.251"): 1, ("eth0", "224.0.0.1"): 1})
    during = Counter({
        ("eth0", "224.0.0.251"): 2,          # someone joined mDNS…
        ("eth0", "239.255.255.250"): 1,      # …and SSDP
        ("veth9", "224.0.0.1"): 1,           # a container came up
        ("eth0", "224.0.0.1"): 1,
    })
    # mDNS went back to its baseline once the agents exited; SSDP did not.
    after = Counter({("eth0", "224.0.0.251"): 1, ("eth0", "239.255.255.250"): 1,
                     ("eth0", "224.0.0.1"): 1})
    found = a.memberships_released(before, during, after)
    assert len(found) == 1 and found[0].startswith("224.0.0.251 on eth0")


# ── darwin ───────────────────────────────────────────────────────────────────

LSOF = """\
p556
fcwd
tDIR
n/Users/user
f3
tIPv4
PICMP
n*:*
f7
tIPv4
PUDP
n*:*
f8
tIPv6
PUDP
n*:51013
f9u
tIPv4
PUDP
n10.1.8.36:5000->224.0.0.251:5353
f10
tIPv6
PUDP
n[fe80::1%en0]:6000->[fe80::2%en0]:443
f11
tIPv4
PTCP
n127.0.0.1:8080->127.0.0.1:50000
f12r
tVCHR
n/dev/bpf3
"""

NETSTAT_G = """\
Link-layer Multicast Group Memberships
Group               \tLink-layer Address\tNetif
1:0:5e:0:0:fb       \t<none>          \ten0

IPv4 Multicast Group Memberships
Group               \tLink-layer Address\tNetif
224.0.0.251         \t<none>          \tlo0
224.0.0.251         \t1:0:5e:0:0:fb   \ten0

IPv6 Multicast Group Memberships
Group               \tLink-layer Address\tNetif
ff02::fb%lo0        \t<none>          \tlo0
ff02::2:ff20:7248%en0\t33:33:ff:20:72:48\ten0
"""

IFCONFIG = """\
lo0: flags=8049<UP,LOOPBACK,RUNNING,MULTICAST> mtu 16384
\tinet 127.0.0.1 netmask 0xff000000
\tinet6 ::1 prefixlen 128
en0: flags=8863<UP,BROADCAST,SMART,RUNNING,SIMPLEX,MULTICAST> mtu 1500
\tinet6 fe80::1%en0 prefixlen 64 secured scopeid 0xb
\tinet 10.1.8.36 netmask 0xffffff00 broadcast 10.1.8.255
utun0: flags=8051<UP,POINTOPOINT,RUNNING,MULTICAST> mtu 1500
"""


def test_lsof_udp_rows_decode_family_endpoints_and_fd():
    rows = a.parse_lsof_udp(a.parse_lsof_fields(LSOF))
    assert [
        (r.family, str(r.local), r.local_port, str(r.remote), r.remote_port, r.inode)
        for r in rows
    ] == [
        ("udp", "0.0.0.0", 0, "0.0.0.0", 0, 7),
        ("udp6", "::", 51013, "::", 0, 8),
        ("udp", "10.1.8.36", 5000, "224.0.0.251", 5353, 9),
        ("udp6", "fe80::1", 6000, "fe80::2", 443, 10),
    ]


ICMP = """\
  sl  local_address rem_address   st tx_queue rx_queue tr tm->when retrnsmt   uid  timeout inode ref pointer drops
  12: 00000000:04D2 00000000:0000 07 00000000:00000000 00:00000000 00000000  1000        0 424242 2 0000000000000000 0
"""


def test_linux_icmp_ping_socket_inode_is_read_from_column_10():
    assert a.parse_raw_inodes(ICMP) == {424242}


@pytest.mark.host_platform("linux")
def test_the_linux_reader_counts_an_unprivileged_ping_socket_as_raw():
    """The reader run for real, on this process: a ``SOCK_DGRAM`` +
    ``IPPROTO_ICMP`` socket is in the raw class."""
    import os
    import socket

    try:
        ping = socket.socket(socket.AF_INET, socket.SOCK_DGRAM, socket.IPPROTO_ICMP)
    except OSError:
        pytest.skip("net.ipv4.ping_group_range excludes this group")
    try:
        # A ping socket is listed in /proc/net/icmp only once bound (a send
        # autobinds); a merely created one is invisible to every reader.
        ping.bind(("0.0.0.0", 0))
        inode = os.stat(f"/proc/self/fd/{ping.fileno()}").st_ino
        assert inode in a._linux_observe(os.getpid()).raw_inodes
    finally:
        ping.close()


def test_lsof_raw_class_is_icmp_and_bpf_never_udp_or_tcp():
    assert a.parse_lsof_raw(a.parse_lsof_fields(LSOF)) == {3, 12}


def test_lsof_reading_feeds_the_same_findings():
    files = a.parse_lsof_fields(LSOF)
    text = "\n".join(a.findings(
        a.AgentSockets(1, a.parse_lsof_udp(files), a.parse_lsof_raw(files)), set()
    ))
    assert "connected to the multicast address 224.0.0.251" in text
    assert "raw or packet socket" in text


def test_netstat_groups_key_like_linux_and_skip_link_layer():
    assert a.parse_netstat_groups(NETSTAT_G) == Counter({
        ("lo0", "224.0.0.251"): 1, ("en0", "224.0.0.251"): 1,
        ("lo0", "ff02::fb"): 1, ("en0", "ff02::2:ff20:7248"): 1,
    })


def test_a_darwin_join_of_a_new_group_is_attributed():
    before = a.parse_netstat_groups(NETSTAT_G)
    during = before + Counter({("en0", "239.255.255.250"): 1})
    found = a.memberships_released(before, during, before)
    assert len(found) == 1 and found[0].startswith("239.255.255.250 on en0")


def test_ifconfig_broadcast_reads_inet_lines_only():
    assert a.parse_ifconfig_broadcast(IFCONFIG) == {ipaddress.ip_address("10.1.8.255")}


# ── windows ──────────────────────────────────────────────────────────────────

NETSH_JOINS_V4 = """\

Interface 1: Loopback Pseudo-Interface 1

Scope       References  Last  Address
----------  ----------  ----  ---------------------------------
0                    1  Yes   239.255.255.250

Interface 7: Ethernet

Scope       References  Last  Address
----------  ----------  ----  ---------------------------------
0                    0  Yes   224.0.0.1
0                    3  Yes   224.0.0.251
"""

NETSH_JOINS_V6 = """\

Interface 7: Ethernet

Scope       References  Last  Address
----------  ----------  ----  ---------------------------------
0                    0  Yes   ff02::1
0                    3  Yes   ff02::fb
0                    1  Yes   ff02::1:ffd8:7566
"""

NETSH_ADDRESSES = """\

Configuration for interface "Ethernet"
    DHCP enabled:                         Yes
    IP Address:                           10.1.8.31
    Subnet Prefix:                        10.1.8.0/24 (mask 255.255.255.0)
    Default Gateway:                      10.1.8.1

Configuration for interface "vpn"
    IP Address:                           10.9.9.9
    Subnet Prefix:                        10.9.9.9/32 (mask 255.255.255.255)

Configuration for interface "Loopback Pseudo-Interface 1"
    IP Address:                           127.0.0.1
    Subnet Prefix:                        127.0.0.0/8 (mask 255.0.0.0)
"""

# Measured 2026-11-06 off a live socket: getsockname/getpeername bytes.
SOCKADDR_V4_LOCAL = "0200da677f0000010000000000000000"   # 127.0.0.1:55911
SOCKADDR_V4_MDNS = "020014e97f0000010000000000000000"    # 127.0.0.1:5353
SOCKADDR_V6 = "1700c35100000000" + "fe800000000000000000000000000001" + "07000000"  # [fe80::1]:50001


def test_netsh_joins_key_like_linux_and_count_references():
    assert a.parse_netsh_joins(NETSH_JOINS_V4) + a.parse_netsh_joins(NETSH_JOINS_V6) == Counter({
        ("Loopback Pseudo-Interface 1", "239.255.255.250"): 1,
        ("Ethernet", "224.0.0.251"): 3,
        ("Ethernet", "ff02::fb"): 3,
        ("Ethernet", "ff02::1:ffd8:7566"): 1,
    })


def test_a_windows_join_is_attributed_by_its_references():
    # Measured: a socket's IP_ADD_MEMBERSHIP raises the group's References by
    # one, and closing the socket lowers it again.
    before = a.parse_netsh_joins(NETSH_JOINS_V4)
    during = before + Counter({("Ethernet", "224.0.0.251"): 1})
    found = a.memberships_released(before, during, before)
    assert len(found) == 1 and found[0].startswith("224.0.0.251 on Ethernet")


def test_netsh_broadcast_reads_prefixes_and_skips_host_routes():
    assert a.parse_netsh_broadcast(NETSH_ADDRESSES) == {
        ipaddress.ip_address("10.1.8.255"), ipaddress.ip_address("127.255.255.255"),
    }


def test_process_handle_snapshot_decodes_value_access_and_type():
    import struct

    entries = [(0x1A4, 0x16019F, 42), (0x1B0, 0x120089, 42), (0x8, 0x1F0003, 17)]
    buf = struct.pack("<QQ", len(entries), 0) + b"".join(
        struct.pack("<QQQIIII", value, 1, 2, access, kind, 0, 0)
        for value, access, kind in entries
    )
    assert a.parse_process_handle_snapshot(buf) == entries


def test_sockaddrs_decode_v4_v6_and_absent():
    v4, v6 = 2, 23
    assert a.decode_sockaddr(SOCKADDR_V4_LOCAL, v4) == (ipaddress.ip_address("127.0.0.1"), 55911)
    assert a.decode_sockaddr(SOCKADDR_V4_MDNS, v4) == (ipaddress.ip_address("127.0.0.1"), 5353)
    assert a.decode_sockaddr(SOCKADDR_V6, v6) == (ipaddress.ip_address("fe80::1"), 50001)
    assert a.decode_sockaddr(None, v4) == (ipaddress.ip_address("0.0.0.0"), 0)
    assert a.decode_sockaddr(None, v6) == (ipaddress.ip_address("::"), 0)


def test_windows_reading_feeds_the_same_findings():
    doc = {
        "sockets": [
            {"handle": 0x1A4, "family": 2, "type": 2, "protocol": 17,
             "local": SOCKADDR_V4_LOCAL, "remote": SOCKADDR_V4_MDNS},
            {"handle": 0x1A8, "family": 23, "type": 2, "protocol": 17,
             "local": SOCKADDR_V6, "remote": None},
            {"handle": 0x1AC, "family": 2, "type": 3, "protocol": 0, "local": None, "remote": None},
            {"handle": 0x1B4, "family": 2, "type": 1, "protocol": 6,
             "local": SOCKADDR_V4_LOCAL, "remote": None},
        ],
        "helpers": [{"handle": 0x1B8, "access": 0x100000}],
        "unread": [{"handle": 0x1B0, "access": 0x120089, "why": "did not say"}],
    }
    reading = a.windows_reading(7, doc)
    assert reading.helpers == {0x1B8} and "0x1b8" in reading.describe()
    assert [(r.family, r.local_port, str(r.remote), r.remote_port, r.inode) for r in reading.udp] == [
        ("udp", 55911, "127.0.0.1", 5353, 0x1A4),
        ("udp6", 50001, "::", 0, 0x1A8),
    ]
    assert reading.raw_inodes == {0x1AC}
    text = "\n".join(a.findings(reading, set()))
    assert "connected to the mDNS port" in text
    assert "raw or packet socket" in text
    assert "handle 0x1b0 (access 0x120089): did not say" in text
    assert "0x1b8" not in text  # a helper is listed, never a finding


def _open_afd_helper() -> int:
    """An AFD handle with no endpoint behind it — what an IOCP poller (tokio's
    mio, libuv) opens on the socket driver to wait on sockets: ``NtCreateFile``
    of ``\\Device\\Afd\\…`` with no extended attributes."""
    import ctypes
    import ctypes.wintypes as wt

    class UnicodeString(ctypes.Structure):
        _fields_ = [("Length", wt.USHORT), ("MaximumLength", wt.USHORT),
                    ("Buffer", ctypes.c_wchar_p)]

    class ObjectAttributes(ctypes.Structure):
        _fields_ = [("Length", wt.ULONG), ("RootDirectory", wt.HANDLE),
                    ("ObjectName", ctypes.POINTER(UnicodeString)), ("Attributes", wt.ULONG),
                    ("SecurityDescriptor", ctypes.c_void_p),
                    ("SecurityQualityOfService", ctypes.c_void_p)]

    ntdll = ctypes.WinDLL("ntdll")
    ntdll.NtCreateFile.restype = ctypes.c_long
    ntdll.NtCreateFile.argtypes = [
        ctypes.POINTER(wt.HANDLE), wt.DWORD, ctypes.POINTER(ObjectAttributes), ctypes.c_void_p,
        ctypes.c_void_p, wt.ULONG, wt.ULONG, wt.ULONG, wt.ULONG, ctypes.c_void_p, wt.ULONG,
    ]
    name = "\\Device\\Afd\\AgentSocketsTest"
    text = UnicodeString(len(name) * 2, len(name) * 2 + 2, name)
    attrs = ObjectAttributes(ctypes.sizeof(ObjectAttributes), None, ctypes.pointer(text),
                             0x40, None, None)  # OBJ_CASE_INSENSITIVE
    handle, iosb = wt.HANDLE(), (ctypes.c_ulonglong * 2)()
    synchronize, share_read_write, file_open = 0x00100000, 0x3, 1
    status = ntdll.NtCreateFile(ctypes.byref(handle), synchronize, ctypes.byref(attrs), iosb,
                                None, 0, share_read_write, file_open, 0, None, 0)
    assert status == 0, f"NtCreateFile(\\Device\\Afd): {status & 0xFFFFFFFF:#x}"
    return handle.value


@pytest.mark.host_platform("windows")
def test_the_windows_reader_sees_raw_and_udp_sockets_and_names_what_it_cannot_read():
    """The reader run for real, on this process: an unprivileged raw ICMP
    socket and a connected UDP socket are read, an AFD poll helper is listed
    as one (not raw, not unread), and a pipe with a read in flight — the handle
    that would hang an in-process reader — is reported unread while the
    reading still completes."""
    import ctypes
    import msvcrt
    import os
    import socket
    import threading

    from helpers.waiting import wait_until

    helper = _open_afd_helper()
    raw = socket.socket(socket.AF_INET, socket.SOCK_RAW, socket.IPPROTO_ICMP)
    udp = socket.socket(socket.AF_INET, socket.SOCK_DGRAM)
    udp.bind(("127.0.0.1", 0))
    udp.connect(("127.0.0.1", 5353))
    port, raw_handle = udp.getsockname()[1], raw.fileno()
    r, w = os.pipe()
    pipe_handle = msvcrt.get_osfhandle(r)
    reader = threading.Thread(target=os.read, args=(r, 1), daemon=True)
    reader.start()
    readings: list = []

    def pipe_read_in_flight() -> bool:
        # Re-read until the reader thread's read is in the kernel — a state,
        # not a delay: the pipe is unread exactly while that read waits.
        readings.append(a.observe(os.getpid()))
        return any(u.startswith(f"handle {pipe_handle:#x} ") for u in readings[-1].unread)

    try:
        wait_until(
            pipe_read_in_flight, 60, interval=0.5,
            diagnose=lambda: readings[-1].describe() if readings else "no reading",
        )
    finally:
        os.write(w, b"x")
        reader.join(5)
        os.close(r)
        os.close(w)
        udp.close()
        raw.close()
        ctypes.WinDLL("kernel32").CloseHandle(ctypes.c_void_p(helper))
    reading = readings[-1]
    mine = [u for u in reading.udp if u.local_port == port]
    assert [(str(u.remote), u.remote_port) for u in mine] == [("127.0.0.1", 5353)], reading.describe()
    assert raw_handle in reading.raw_inodes, reading.describe()
    assert helper in reading.helpers and helper not in reading.raw_inodes, reading.describe()
    assert not any(u.startswith(f"handle {helper:#x} ") for u in reading.unread), reading.describe()
