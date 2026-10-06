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
_TYPE_MX = 15
_TYPE_TXT = 16
_CLASS_IN = 1

# Response RCODEs.
_RCODE_NOERROR = 0
_RCODE_NXDOMAIN = 3


class FakeDns:
    """A loopback/sidecar UDP DNS server answering a fixed set of A/TXT records.

    ``txt_records`` maps a fully-qualified name (lowercase, no trailing dot) to a
    single TXT string; ``a_records`` maps a name to a dotted-quad IPv4. A query
    for a configured (name, type) returns that record with ``AA=1``; any other
    name returns ``NXDOMAIN`` (an unconfigured *type* on a configured name
    returns NOERROR/NODATA). Only one RRset per (name, type) — enough for the
    DMARC/SPF policy the anti-spoofing guard publishes.

    ``records_path`` (optional) is a JSON file — ``{"txt": {name: val}, "a":
    {name: ip}}`` — re-read (mtime-gated) on every query and OVERLAID on the
    constructor records. It exists for the two-box SMTP-relay test
    (``test_mail_relay_two_nest_smtp.py``), which can only learn each box's
    container IP *after* both are started — so it points each box's resolver here
    (``--dns``) at boot, then writes the per-box A/SPF records into this file once
    the IPs are known. Without it the records are fixed at construction (the
    anti-spoofing guard's static path)."""

    def __init__(
        self,
        host: str = "0.0.0.0",
        port: int = 53,
        *,
        txt_records: dict[str, str] | None = None,
        a_records: dict[str, str] | None = None,
        mx_records: dict[str, str] | None = None,
        records_path: str | None = None,
        ttl: int = 60,
    ) -> None:
        self._base_txt = {k.rstrip(".").lower(): v for k, v in (txt_records or {}).items()}
        self._base_a = {k.rstrip(".").lower(): v for k, v in (a_records or {}).items()}
        self._base_mx = {k.rstrip(".").lower(): v for k, v in (mx_records or {}).items()}
        self._txt = dict(self._base_txt)
        self._a = dict(self._base_a)
        self._mx = dict(self._base_mx)
        self._records_path = records_path
        self._records_mtime: float | None = None
        self._ttl = ttl
        self._sock = socket.socket(socket.AF_INET, socket.SOCK_DGRAM)
        self._sock.setsockopt(socket.SOL_SOCKET, socket.SO_REUSEADDR, 1)
        self._sock.bind((host, port))
        self.host, self.port = self._sock.getsockname()[:2]
        self._stopped = threading.Event()
        self._thread = threading.Thread(target=self._serve, daemon=True)

    def start(self) -> "FakeDns":
        self._thread.start()
        return self

    def stop(self) -> None:
        self._stopped.set()
        try:
            self._sock.close()
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

    def _build_reply(self, query: bytes) -> bytes | None:
        self._maybe_reload()
        if len(query) < 12:
            return None
        txn_id, flags, qdcount = struct.unpack(">HHH", query[:6])
        if qdcount < 1:
            return None
        qname, offset = self._parse_name(query, 12)
        qtype, qclass = struct.unpack(">HH", query[offset : offset + 4])
        offset += 4
        question = query[12:offset]  # raw question section, echoed back

        name = qname.lower()
        answers = b""
        ancount = 0
        rcode = _RCODE_NOERROR

        if qclass == _CLASS_IN and qtype == _TYPE_TXT and name in self._txt:
            answers = self._txt_answer(self._txt[name])
            ancount = 1
        elif qclass == _CLASS_IN and qtype == _TYPE_A and name in self._a:
            answers = self._a_answer(self._a[name])
            ancount = 1
        elif qclass == _CLASS_IN and qtype == _TYPE_MX and name in self._mx:
            answers = self._mx_answer(self._mx[name])
            ancount = 1
        elif name in self._txt or name in self._a or name in self._mx:
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
        """Parse a (non-compressed — queries never use pointers) QNAME."""
        labels = []
        while True:
            length = msg[offset]
            offset += 1
            if length == 0:
                break
            labels.append(msg[offset : offset + length].decode("ascii", "replace"))
            offset += length
        return ".".join(labels), offset

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
        name = b"".join(
            bytes([len(label)]) + label.encode("ascii", "replace")
            for label in exchange.rstrip(".").split(".")
        ) + b"\x00"
        rdata = struct.pack(">H", pref) + name
        return self._rr_header(_TYPE_MX, len(rdata)) + rdata
