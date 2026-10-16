"""Minimal in-process / sidecar fake DNS server for the tier_4 anti-spoofing
guard (``test_mail_security_relay.py::test_local_domain_spoofing_rejected``).

The inbound auth verifier ``libs/fauna-mail/src/auth.rs::verify_inbound`` builds
its resolver from the container's ``/etc/resolv.conf`` (``new_system_conf()``,
**no env override**), so the only way to publish a DMARC/SPF policy the MTA will
read in the deploy image is to point the container at a real nameserver. This
fake is run as a **sidecar container** on the nest's user-defined network and
wired in via ``docker run --dns <sidecar-ip>``; docker's embedded resolver then
forwards external queries (``_dmarc.<domain>`` TXT, the sender domain's SPF TXT)
here.

It answers exactly the records it is configured with — a single ``A`` or ``TXT``
RRset per name — and returns ``NXDOMAIN`` for everything else. That is enough to
make ``verify_inbound`` compute a DMARC ``Fail`` with a published ``p=reject``
policy: serve ``_dmarc.<spoofed-local-domain>`` ``TXT "v=DMARC1; p=reject"`` plus
``<spoofed-local-domain>`` ``TXT "v=spf1 -all"`` (a hard SPF fail that cannot
align), and an unauthenticated message whose ``From:`` is that local domain is
rejected ``550 5.7.1 DMARC reject`` by ``mta/auth_enforce.go::applyDMARCRejectGate``.

Like ``fake_clamd`` / ``fake_rspamd``, this is not a fake of a fauna binary — it
is a real external server the stack talks to over the wire (here, the DNS the
nest's own resolver queries), which is what lets the test stay a faithful
deploy-image (tier_4) mirror. Pure stdlib so it runs unchanged inside the
``python:3.14-slim`` sidecar image (no ``dnslib`` dependency).
"""

from __future__ import annotations

import json
import os
import socket
import struct
import threading

# DNS record TYPE codes (RFC 1035 §3.2.2) we answer.
_TYPE_A = 1
_TYPE_PTR = 12
_TYPE_MX = 15
_TYPE_TXT = 16
_CLASS_IN = 1
_CLASS_NONE = 254  # RFC 2136 § 2.5.4: delete this RR
_CLASS_ANY = 255  # RFC 2136 § 2.5.2: delete this RRset
_OPCODE_UPDATE = 5

# Response RCODEs.
_RCODE_NOERROR = 0
_RCODE_SERVFAIL = 2
_RCODE_NXDOMAIN = 3


class FakeDns:
    """A loopback/sidecar DNS server answering a fixed set of A/TXT records, over
    UDP and — on the same port number, two-byte length framing, one query per
    connection — TCP (RFC 7766).

    ``txt_records`` maps a fully-qualified name (lowercase, no trailing dot) to a
    single TXT string; ``a_records`` maps a name to a dotted-quad IPv4. A query
    for a configured (name, type) returns that record with ``AA=1``; any other
    name returns ``NXDOMAIN`` (an unconfigured *type* on a configured name
    returns NOERROR/NODATA). Only one RRset per (name, type) — enough for the
    DMARC/SPF policy the anti-spoofing guard publishes.

    ``ptr_records`` maps a dotted-quad IPv4 to the single name its reverse
    lookup answers: the fake serves it as ``PTR`` on the reversed-octet name
    under ``in-addr.arpa`` that a resolver asks. An address absent from the map NXDOMAINs like any
    other unconfigured name, which is how a peer with *no* reverse name looks
    (the mail perimeter's reverse-name and identity witnesses,
    ``test_mail_perimeter_dns.py``).

    ``records_path`` (optional) is a JSON file — ``{"txt": {name: val}, "a":
    {name: ip}, "mx": {name: val}, "ptr": {ip: name}}`` — re-read (mtime-gated) on every query and OVERLAID on the
    constructor records. It exists for the two-box SMTP-relay test
    (``test_mail_relay_two_nest_smtp.py``), which can only learn each box's
    container IP *after* both are started — so it points each box's resolver here
    (``--dns``) at boot, then writes the per-box A/SPF records into this file once
    the IPs are known. Without it the records are fixed at construction (the
    anti-spoofing guard's static path).

    The records file may also carry ``"servfail": [name, ...]``: a query for any
    of those names, whatever its type, is answered ``SERVFAIL`` — the resolver
    *error* of a lookup that cannot be answered, as distinct from NXDOMAIN's
    definite "no such name". The mail perimeter's fail-open witness publishes it
    (``test_mail_abuse_perimeter.py``).

    An RFC 2136 dynamic UPDATE (either transport; unsigned, zone-blind) edits
    an in-memory TXT overlay that sits ABOVE the file and constructor TXT: an
    add (class IN) joins the name's RRset, a delete-this-RR (class NONE) drops
    one value, a delete-RRset (class ANY) drops the name; a name the overlay
    holds is answered with its whole RRset. The app's e2e fake DNS provider
    writes a managed order's challenge this way
    (``e2e_acme_seam::mirror_fake_txt``,
    ``e2e-automation-surface-gating.md`` § The ACME directory seed → *The name
    server's reach*), so the records file stays a read-only mount.

    ``log_queries`` prints one ``fake_dns query <name> <type>`` line per query
    to stdout — the sidecar's ``docker logs`` — so a test can wait for a client's
    lookup to have HAPPENED (the ACME propagation gate's first probe missing,
    ``test_admin_dns_cert_manual_issue.py``) instead of sleeping past it."""

    def __init__(
        self,
        host: str = "0.0.0.0",
        port: int = 53,
        *,
        txt_records: dict[str, str] | None = None,
        a_records: dict[str, str] | None = None,
        mx_records: dict[str, str] | None = None,
        ptr_records: dict[str, str] | None = None,
        records_path: str | None = None,
        ttl: int = 60,
        log_queries: bool = False,
    ) -> None:
        self._log_queries = log_queries
        self._base_txt = {k.rstrip(".").lower(): v for k, v in (txt_records or {}).items()}
        self._base_a = {k.rstrip(".").lower(): v for k, v in (a_records or {}).items()}
        self._base_mx = {k.rstrip(".").lower(): v for k, v in (mx_records or {}).items()}
        self._txt = dict(self._base_txt)
        self._a = dict(self._base_a)
        self._base_ptr = self._reverse_names(ptr_records)
        self._mx = dict(self._base_mx)
        self._ptr = dict(self._base_ptr)
        self._servfail: set[str] = set()
        self._records_path = records_path
        self._records_mtime: float | None = None
        self._ttl = ttl
        # Dynamic-UPDATE overlay: name -> TXT RRset, ABOVE the file/constructor
        # TXT for that name. Guarded by `_lock`: the UDP and TCP threads both
        # read it and the TCP thread writes it.
        self._updates: dict[str, list[str]] = {}
        self._lock = threading.Lock()
        self._sock = socket.socket(socket.AF_INET, socket.SOCK_DGRAM)
        self._sock.setsockopt(socket.SOL_SOCKET, socket.SO_REUSEADDR, 1)
        self._sock.bind((host, port))
        self.host, self.port = self._sock.getsockname()[:2]
        # TCP on the same port number (RFC 7766): the propagation probe's retry
        # and the only transport an android venue's reverse carries.
        self._tcp = socket.socket(socket.AF_INET, socket.SOCK_STREAM)
        self._tcp.setsockopt(socket.SOL_SOCKET, socket.SO_REUSEADDR, 1)
        self._tcp.bind((host, self.port))
        self._tcp.listen(16)
        self._stopped = threading.Event()
        self._thread = threading.Thread(target=self._serve, daemon=True)
        self._tcp_thread = threading.Thread(target=self._serve_tcp, daemon=True)

    def start(self) -> "FakeDns":
        self._thread.start()
        self._tcp_thread.start()
        return self

    def stop(self) -> None:
        self._stopped.set()
        for sock in (self._sock, self._tcp):
            try:
                sock.close()
            except OSError:
                pass

    # ── internals ─────────────────────────────────────────────────────

    def _serve(self) -> None:
        while not self._stopped.is_set():
            try:
                data, addr = self._sock.recvfrom(4096)
            except OSError:
                return  # socket closed by stop()
            try:
                reply = self._build_reply(data)
            except Exception:
                # Drop this one query rather than letting any answer-building error
                # escape and kill the resolver thread — a dead resolver fails ALL
                # later lookups silently, which reads downstream as "no DMARC
                # policy" (unenforced) and masks the real cause (see _txt_answer).
                continue
            if reply is not None:
                try:
                    self._sock.sendto(reply, addr)
                except OSError:
                    pass

    def _serve_tcp(self) -> None:
        """One query per accepted connection, two-byte length framing (RFC
        1035 § 4.2.2). Like `_serve`, a bad message drops only that connection."""
        while not self._stopped.is_set():
            try:
                conn, _ = self._tcp.accept()
            except OSError:
                return  # socket closed by stop()
            with conn:
                try:
                    conn.settimeout(5)
                    (length,) = struct.unpack(">H", self._recv_exactly(conn, 2))
                    reply = self._build_reply(self._recv_exactly(conn, length))
                    if reply is not None:
                        conn.sendall(struct.pack(">H", len(reply)) + reply)
                except Exception:
                    continue

    @staticmethod
    def _recv_exactly(conn: socket.socket, n: int) -> bytes:
        buf = b""
        while len(buf) < n:
            chunk = conn.recv(n - len(buf))
            if not chunk:
                raise ConnectionError("peer closed mid-message")
            buf += chunk
        return buf

    def _apply_update(self, msg: bytes, txn_id: int, zocount: int, prcount: int,
                      upcount: int) -> bytes:
        """Apply an RFC 2136 dynamic UPDATE's TXT records to the overlay: class
        IN adds a value to the name's RRset, class NONE deletes that one value,
        class ANY deletes the name's RRset. Unsigned and zone-blind (the fake
        serves one flat namespace); prerequisites are skipped, not checked.
        Answers NOERROR echoing the zone section."""
        offset = 12
        zone_start = offset
        for _ in range(zocount):
            _, offset = self._parse_name(msg, offset)
            offset += 4
        zone_section = msg[zone_start:offset]
        for _ in range(prcount):
            offset = self._skip_rr(msg, offset)
        with self._lock:
            for _ in range(upcount):
                name, offset = self._parse_name(msg, offset)
                rtype, rclass, _ttl, rdlength = struct.unpack(">HHIH", msg[offset : offset + 10])
                offset += 10
                rdata = msg[offset : offset + rdlength]
                offset += rdlength
                if rtype != _TYPE_TXT and rclass != _CLASS_ANY:
                    continue
                key = name.lower()
                values = self._updates.setdefault(key, [])
                if rclass == _CLASS_IN:
                    value = self._txt_value(rdata)
                    if value not in values:
                        values.append(value)
                elif rclass == _CLASS_NONE:
                    value = self._txt_value(rdata)
                    if value in values:
                        values.remove(value)
                elif rclass == _CLASS_ANY:
                    values.clear()
                if self._log_queries:
                    print(f"fake_dns update {key} class {rclass}", flush=True)
                if not values:
                    del self._updates[key]
        # QR=1, Opcode UPDATE, RCODE NOERROR; the zone section echoed.
        flags = 0x8000 | (_OPCODE_UPDATE << 11)
        return struct.pack(">HHHHHH", txn_id, flags, zocount, 0, 0, 0) + zone_section

    def _skip_rr(self, msg: bytes, offset: int) -> int:
        _, offset = self._parse_name(msg, offset)
        (rdlength,) = struct.unpack(">H", msg[offset + 8 : offset + 10])
        return offset + 10 + rdlength

    @staticmethod
    def _txt_value(rdata: bytes) -> str:
        """The concatenated character-strings of a TXT RDATA."""
        out, i = b"", 0
        while i < len(rdata):
            out += rdata[i + 1 : i + 1 + rdata[i]]
            i += 1 + rdata[i]
        return out.decode("ascii", "replace")

    def _maybe_reload(self) -> None:
        """Re-read ``records_path`` when its mtime changed, overlaying the
        constructor records. Any read/parse error leaves the current records in
        place (a half-written file is a transient the next query retries) — the
        ``_serve`` guard's lesson: never let a records glitch kill the thread."""
        if not self._records_path:
            return
        try:
            mtime = os.path.getmtime(self._records_path)
        except OSError:
            return  # file not written yet — serve the constructor records
        if mtime == self._records_mtime:
            return
        try:
            with open(self._records_path, encoding="utf-8") as f:
                doc = json.load(f)
        except (OSError, ValueError):
            return
        self._records_mtime = mtime
        self._txt = {**self._base_txt,
                     **{k.rstrip(".").lower(): v for k, v in (doc.get("txt") or {}).items()}}
        self._a = {**self._base_a,
                   **{k.rstrip(".").lower(): v for k, v in (doc.get("a") or {}).items()}}
        self._mx = {**self._base_mx,
                    **{k.rstrip(".").lower(): v for k, v in (doc.get("mx") or {}).items()}}
        self._ptr = {**self._base_ptr, **self._reverse_names(doc.get("ptr"))}
        self._servfail = {n.rstrip(".").lower() for n in (doc.get("servfail") or [])}

    @staticmethod
    def _reverse_names(ptr_records: dict[str, str] | None) -> dict[str, str]:
        """Key each ``{ipv4: name}`` entry by the ``in-addr.arpa`` name a
        reverse lookup queries (RFC 1035 §3.5)."""
        return {
            ".".join(reversed(ip.split("."))) + ".in-addr.arpa": name.rstrip(".")
            for ip, name in (ptr_records or {}).items()
        }

    def _build_reply(self, query: bytes) -> bytes | None:
        self._maybe_reload()
        if len(query) < 12:
            return None
        txn_id, flags, qdcount, ancount, nscount = struct.unpack(">HHHHH", query[:10])
        if (flags >> 11) & 0xF == _OPCODE_UPDATE:
            # UPDATE reuses the counts: zone, prerequisite, update (RFC 2136 § 2).
            return self._apply_update(query, txn_id, qdcount, ancount, nscount)
        if qdcount < 1:
            return None
        qname, offset = self._parse_name(query, 12)
        qtype, qclass = struct.unpack(">HH", query[offset : offset + 4])
        offset += 4
        question = query[12:offset]  # raw question section, echoed back

        name = qname.lower()
        if self._log_queries:
            print(f"fake_dns query {name} {qtype}", flush=True)
        answers = b""
        ancount = 0
        rcode = _RCODE_NOERROR
        with self._lock:
            updated = list(self._updates.get(name, ()))

        if name in self._servfail:
            rcode = _RCODE_SERVFAIL
        elif qclass == _CLASS_IN and qtype == _TYPE_TXT and updated:
            # The UPDATE overlay sits above the file: its RRset, every value.
            answers = b"".join(self._txt_answer(v) for v in updated)
            ancount = len(updated)
        elif qclass == _CLASS_IN and qtype == _TYPE_TXT and name in self._txt:
            answers = self._txt_answer(self._txt[name])
            ancount = 1
        elif qclass == _CLASS_IN and qtype == _TYPE_A and name in self._a:
            answers = self._a_answer(self._a[name])
            ancount = 1
        elif qclass == _CLASS_IN and qtype == _TYPE_MX and name in self._mx:
            answers = self._mx_answer(self._mx[name])
            ancount = 1
        elif qclass == _CLASS_IN and qtype == _TYPE_PTR and name in self._ptr:
            answers = self._ptr_answer(self._ptr[name])
            ancount = 1
        elif (updated or name in self._txt or name in self._a or name in self._mx
              or name in self._ptr):
            rcode = _RCODE_NOERROR  # NODATA: name exists, no record of this type
        else:
            rcode = _RCODE_NXDOMAIN

        # QR=1, Opcode copied, AA=1, RD copied, RA=0, RCODE.
        rd = flags & 0x0100
        resp_flags = 0x8000 | 0x0400 | rd | rcode
        header = struct.pack(">HHHHHH", txn_id, resp_flags, 1, ancount, 0, 0)
        return header + question + answers

    @staticmethod
    def _parse_name(msg: bytes, offset: int) -> tuple[str, int]:
        """Parse a domain name at `offset` -> (name, offset past it). Follows
        compression pointers (RFC 1035 § 4.1.4): a query's QNAME never uses
        them, but an UPDATE from a real encoder repeats the zone's suffix in
        every record and points at it."""
        labels = []
        end = None
        for _ in range(128):  # a pointer loop is a malformed message, not a hang
            length = msg[offset]
            if length & 0xC0 == 0xC0:
                if end is None:
                    end = offset + 2
                offset = ((length & 0x3F) << 8) | msg[offset + 1]
                continue
            offset += 1
            if length == 0:
                break
            labels.append(msg[offset : offset + length].decode("ascii", "replace"))
            offset += length
        else:
            raise ValueError("compression pointer loop")
        return ".".join(labels), end if end is not None else offset

    def _rr_header(self, rtype: int, rdlength: int) -> bytes:
        # NAME = pointer to the question name at offset 12 (0xC00C); TYPE; CLASS
        # IN; TTL; RDLENGTH.
        return struct.pack(">HHHIH", 0xC00C, rtype, _CLASS_IN, self._ttl, rdlength)

    def _txt_answer(self, text: str) -> bytes:
        raw = text.encode("ascii", "replace")
        # TXT RDATA is one-or-more <len-byte><chars> character-strings, each ≤255
        # bytes (RFC 1035 §3.3.14). A value over 255 bytes — e.g. a DKIM `p=`
        # public key (~400 B) — MUST be split into multiple ≤255-byte segments,
        # exactly as real DNS serves DKIM keys; a single `bytes([len(raw)])` would
        # raise (len>255) and, uncaught, kill the resolver thread → every later
        # lookup for that domain silently fails (DMARC then reads as unenforced).
        rdata = b"".join(
            bytes([len(seg)]) + seg
            for seg in (raw[i : i + 255] for i in range(0, len(raw), 255))
        ) or b"\x00"  # empty TXT → a single zero-length character-string
        return self._rr_header(_TYPE_TXT, len(rdata)) + rdata

    def _a_answer(self, dotted: str) -> bytes:
        rdata = socket.inet_aton(dotted)
        return self._rr_header(_TYPE_A, len(rdata)) + rdata

    @staticmethod
    def _wire_name(name: str) -> bytes:
        """An uncompressed domain name: length-prefixed labels, zero-terminated."""
        return b"".join(
            bytes([len(label)]) + label.encode("ascii", "replace")
            for label in name.rstrip(".").split(".")
        ) + b"\x00"

    def _ptr_answer(self, target: str) -> bytes:
        # PTR RDATA (RFC 1035 §3.3.12) is a single domain name.
        rdata = self._wire_name(target)
        return self._rr_header(_TYPE_PTR, len(rdata)) + rdata

    def _mx_answer(self, value: str) -> bytes:
        # value is "<preference> <exchange>" or a bare "<exchange>" (preference 0).
        # MX RDATA (RFC 1035 §3.3.9) = 2-byte preference + an uncompressed domain
        # name (length-prefixed labels, terminated by a zero octet). The exchange
        # is the next-hop MX host the sender then resolves via its A record.
        parts = value.split(None, 1)
        if len(parts) == 2 and parts[0].isdigit():
            pref, exchange = int(parts[0]), parts[1]
        else:
            pref, exchange = 0, value
        rdata = struct.pack(">H", pref) + self._wire_name(exchange)
        return self._rr_header(_TYPE_MX, len(rdata)) + rdata
