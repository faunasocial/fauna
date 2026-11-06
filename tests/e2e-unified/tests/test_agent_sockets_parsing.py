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
