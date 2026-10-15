"""`fakes/fake_dns.py`'s TCP and dynamic-UPDATE arms, in process on loopback.

`docs/goal/architecture/e2e-automation-surface-gating.md` § The ACME directory
seed → *The name server's reach*: the fake answers over TCP (two-byte length
framing) beside UDP on its one port, and applies an RFC 2136 UPDATE — an add
(class IN) per TXT, a delete-this-RR (class NONE) per withdrawn value — to an
in-memory overlay above its records file. The app's e2e fake DNS provider
writes the managed challenge that way (`e2e_acme_seam::mirror_fake_txt`), and
the propagation probe reads it back over either transport.
"""

import json
import os
import socket
import struct
import sys

import pytest

pytestmark = pytest.mark.tier_1

_REPO_ROOT = os.path.dirname(
    os.path.dirname(os.path.dirname(os.path.dirname(os.path.abspath(__file__))))
)
sys.path.insert(0, os.path.join(_REPO_ROOT, "tests", "e2e-unified"))

from fakes.fake_dns import FakeDns  # noqa: E402

_TXT, _SOA = 16, 6
_IN, _NONE = 1, 254


def _name(name: str) -> bytes:
    return b"".join(bytes([len(p)]) + p.encode() for p in name.rstrip(".").split(".")) + b"\x00"


def _query(name: str, txn: int = 0x1234) -> bytes:
    return struct.pack(">HHHHHH", txn, 0, 1, 0, 0, 0) + _name(name) + struct.pack(">HH", _TXT, _IN)


def _txt_rr(name: str, value: str, klass: int, ttl: int) -> bytes:
    raw = value.encode()
    rdata = bytes([len(raw)]) + raw
    return _name(name) + struct.pack(">HHIH", _TXT, klass, ttl, len(rdata)) + rdata


def _update(zone: str, adds=(), deletes=(), txn: int = 0x4242) -> bytes:
    rrs = [_txt_rr(n, v, _NONE, 0) for n, v in deletes] + [_txt_rr(n, v, _IN, 60) for n, v in adds]
    header = struct.pack(">HHHHHH", txn, 5 << 11, 1, 0, len(rrs), 0)
    return header + _name(zone) + struct.pack(">HH", _SOA, _IN) + b"".join(rrs)


def _udp(port: int, msg: bytes) -> bytes:
    with socket.socket(socket.AF_INET, socket.SOCK_DGRAM) as s:
        s.settimeout(3)
        s.sendto(msg, ("127.0.0.1", port))
        return s.recv(4096)


def _tcp(port: int, msg: bytes) -> bytes:
    with socket.create_connection(("127.0.0.1", port), timeout=3) as s:
        s.sendall(struct.pack(">H", len(msg)) + msg)
        (n,) = struct.unpack(">H", _recv_exactly(s, 2))
        return _recv_exactly(s, n)


def _recv_exactly(s: socket.socket, n: int) -> bytes:
    buf = b""
    while len(buf) < n:
        chunk = s.recv(n - len(buf))
        assert chunk, "the fake closed the connection mid-message"
        buf += chunk
    return buf


def _txt_values(reply: bytes) -> list[str]:
    """The TXT values a reply answers (the fake points every answer name at the
    question, `0xC00C`)."""
    _, _, qd, an, _, _ = struct.unpack(">HHHHHH", reply[:12])
    off = 12
    for _ in range(qd):
        while reply[off]:
            off += 1 + reply[off]
        off += 5
    out = []
    for _ in range(an):
        assert reply[off] & 0xC0 == 0xC0, "the fake's answer names are pointers"
        rtype, _, _, rdlen = struct.unpack(">HHIH", reply[off + 2 : off + 12])
        rdata = reply[off + 12 : off + 12 + rdlen]
        off += 12 + rdlen
        if rtype == _TXT:
            segs, i = [], 0
            while i < len(rdata):
                segs.append(rdata[i + 1 : i + 1 + rdata[i]].decode())
                i += 1 + rdata[i]
            out.append("".join(segs))
    return out


def _rcode(reply: bytes) -> int:
    return struct.unpack(">H", reply[2:4])[0] & 0x000F


@pytest.fixture
def fake(tmp_path):
    records = tmp_path / "records.json"
    dns = FakeDns(host="127.0.0.1", port=0, records_path=str(records)).start()
    yield dns, records
    dns.stop()


def test_a_txt_query_is_answered_over_tcp(fake):
    dns, records = fake
    records.write_text(json.dumps({"txt": {"file.test": "from-file"}}))
    assert _txt_values(_tcp(dns.port, _query("file.test"))) == ["from-file"]
    assert _txt_values(_udp(dns.port, _query("file.test"))) == ["from-file"]


def test_an_update_add_is_served_over_both_transports(fake):
    dns, _ = fake
    reply = _tcp(dns.port, _update("renew.test", adds=[("_acme-challenge.renew.test", "tok")]))
    assert _rcode(reply) == 0, reply
    assert struct.unpack(">H", reply[:2])[0] == 0x4242
    assert (struct.unpack(">H", reply[2:4])[0] >> 11) & 0xF == 5, "the reply is an UPDATE response"
    assert _txt_values(_udp(dns.port, _query("_acme-challenge.renew.test"))) == ["tok"]
    assert _txt_values(_tcp(dns.port, _query("_ACME-challenge.Renew.test."))) == ["tok"]


def test_a_class_none_delete_removes_only_the_withdrawn_value(fake):
    dns, _ = fake
    name = "_acme-challenge.both.test"
    _tcp(dns.port, _update("both.test", adds=[(name, "apex"), (name, "wildcard")]))
    assert sorted(_txt_values(_udp(dns.port, _query(name)))) == ["apex", "wildcard"]
    _tcp(dns.port, _update("both.test", deletes=[(name, "apex")]))
    assert _txt_values(_udp(dns.port, _query(name))) == ["wildcard"]
    _tcp(dns.port, _update("both.test", deletes=[(name, "wildcard")]))
    assert _rcode(_udp(dns.port, _query(name))) == 3, "an emptied name NXDOMAINs again"


def test_the_file_records_are_still_served_under_the_update_overlay(fake):
    dns, records = fake
    records.write_text(json.dumps({"txt": {"file.test": "from-file", "both.test": "file-value"}}))
    _tcp(dns.port, _update("both.test", adds=[("both.test", "update-value")]))
    assert _txt_values(_udp(dns.port, _query("file.test"))) == ["from-file"]
    assert _txt_values(_udp(dns.port, _query("both.test"))) == ["update-value"], (
        "the overlay sits above the file"
    )
    _tcp(dns.port, _update("both.test", deletes=[("both.test", "update-value")]))
    assert _txt_values(_udp(dns.port, _query("both.test"))) == ["file-value"]


def test_an_update_with_compressed_names_is_applied(fake):
    """A real sender (hickory's encoder) compresses repeated names; the fake
    must follow the pointers."""
    dns, _ = fake
    zone = _name("c.test")
    header = struct.pack(">HHHHHH", 7, 5 << 11, 1, 0, 1, 0)
    # Update RR name `_acme-challenge` + a pointer to the zone name at offset 12.
    rr_name = bytes([15]) + b"_acme-challenge" + struct.pack(">H", 0xC000 | 12)
    rdata = b"\x03tok"
    rr = rr_name + struct.pack(">HHIH", _TXT, _IN, 60, len(rdata)) + rdata
    msg = header + zone + struct.pack(">HH", _SOA, _IN) + rr
    assert _rcode(_tcp(dns.port, msg)) == 0
    assert _txt_values(_udp(dns.port, _query("_acme-challenge.c.test"))) == ["tok"]
