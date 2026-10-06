"""Minimal in-process stub external SMTP MX for tier_3 outbound tests.

The MTA bridge's outbound worker delivers to this stub over real
sockets — the `mail_bridge_mta` fixture points `external.test`'s MX here
via the operator-hatch ``mta_mx_override`` table. The stub speaks just
enough SMTP to accept MAIL/RCPT/DATA over a plaintext connection (it does
NOT advertise STARTTLS, so the bridge's opportunistic-STARTTLS sender
falls back to plaintext), captures the raw RFC 5322 bytes, and exposes
them to the test.

This is not a fake of a fauna binary — it is a real external SMTP peer
the bridge talks to over the wire, which is why the submission
round-trip test stays tier_3 (real nest, real bridge, real wire).
"""

from __future__ import annotations

import datetime
import ipaddress
import os
import socket
import ssl
import tempfile
import threading
import time


def _make_selfsigned_tls_context(bind_host: str) -> tuple[ssl.SSLContext, bytes]:
    """Generate a fresh self-signed EC cert (SAN = the bind IP + a stub
    hostname) and return ``(server_ssl_context, cert_der)``.

    ``cert_der`` is the exact leaf DER the stub serves, so a DANE test can
    publish ``sha256(cert_der)`` as the TLSA ``matching=1`` value and assert
    a pin match. DANE pinning uses ``InsecureSkipVerify`` on the bridge side
    (the TLSA record replaces WebPKI), so the cert need not chain to a public
    CA — a self-signed leaf is sufficient.
    """
    from cryptography import x509
    from cryptography.hazmat.primitives import hashes, serialization
    from cryptography.hazmat.primitives.asymmetric import ec
    from cryptography.x509.oid import NameOID

    key = ec.generate_private_key(ec.SECP256R1())
    subject = x509.Name([x509.NameAttribute(NameOID.COMMON_NAME, "stub-mx.dane.test")])
    now = datetime.datetime.now(datetime.timezone.utc)
    cert = (
        x509.CertificateBuilder()
        .subject_name(subject)
        .issuer_name(subject)
        .public_key(key.public_key())
        .serial_number(x509.random_serial_number())
        .not_valid_before(now - datetime.timedelta(minutes=5))
        .not_valid_after(now + datetime.timedelta(days=1))
        .add_extension(
            x509.SubjectAlternativeName(
                [
                    x509.DNSName("stub-mx.dane.test"),
                    x509.IPAddress(ipaddress.ip_address(bind_host)),
                ]
            ),
            critical=False,
        )
        .sign(key, hashes.SHA256())
    )
    cert_der = cert.public_bytes(serialization.Encoding.DER)
    cert_pem = cert.public_bytes(serialization.Encoding.PEM)
    key_pem = key.private_bytes(
        serialization.Encoding.PEM,
        serialization.PrivateFormat.TraditionalOpenSSL,
        serialization.NoEncryption(),
    )
    ctx = ssl.SSLContext(ssl.PROTOCOL_TLS_SERVER)
    # load_cert_chain reads the files synchronously, so they can be unlinked
    # immediately after.
    certf = tempfile.NamedTemporaryFile(delete=False, suffix=".crt")
    keyf = tempfile.NamedTemporaryFile(delete=False, suffix=".key")
    try:
        certf.write(cert_pem)
        certf.close()
        keyf.write(key_pem)
        keyf.close()
        ctx.load_cert_chain(certf.name, keyf.name)
    finally:
        os.unlink(certf.name)
        os.unlink(keyf.name)
    return ctx, cert_der


def _mail_from_addr(mail_from_line: str) -> str:
    """Extract the envelope sender address from a `MAIL FROM:<addr>` command.

    Tolerant of missing angle brackets and trailing ESMTP parameters (SIZE=,
    BODY=, …); returns "" for the null sender `<>` or when unparseable. Used
    by the SRS forward test to read the rewritten `SRS0=…@<domain>` envelope.
    """
    addr = mail_from_line
    if "<" in addr and ">" in addr:
        addr = addr[addr.index("<") + 1 : addr.index(">")]
    else:
        _, _, rest = addr.partition(":")
        addr = rest.strip().split()[0] if rest.strip() else ""
    return addr.strip()


def _rcpt_local_part(rcpt_line: str) -> str:
    """Extract the local part from a `RCPT TO:<local@domain>` command line.

    Tolerant of missing angle brackets and trailing ESMTP parameters;
    returns "" when no address is parseable.
    """
    addr = rcpt_line
    if "<" in addr and ">" in addr:
        addr = addr[addr.index("<") + 1 : addr.index(">")]
    else:
        # `RCPT TO:addr` form — take everything after the colon, first token.
        _, _, rest = addr.partition(":")
        addr = rest.strip().split()[0] if rest.strip() else ""
    return addr.split("@", 1)[0]


class StubMX:
    """A loopback SMTP sink. Bind an ephemeral port, accept connections in
    a daemon thread, and record each accepted message's raw bytes."""

    def __init__(self, *, bind_host: str = "127.0.0.1", port: int = 0,
                 enable_starttls: bool = False, require_starttls: bool = False,
                 tls_context: "ssl.SSLContext | None" = None,
                 out_dir: str | None = None,
                 on_message: "callable | None" = None) -> None:
        self._sock = socket.socket(socket.AF_INET, socket.SOCK_STREAM)
        self._sock.setsockopt(socket.SOL_SOCKET, socket.SO_REUSEADDR, 1)
        # port=0 binds an ephemeral port (the in-process default, used by the
        # `mail_bridge_mta` fixture); a fixed port is needed when the stub runs
        # as a docker sidecar the bridge dials by container name + known port.
        self._sock.bind((bind_host, port))
        self._sock.listen(8)
        self.host, self.port = self._sock.getsockname()[:2]
        # When set, every accepted message is also written to `out_dir/<seq>.eml`
        # (atomically via a temp-file rename). The in-process fixture leaves this
        # None and reads `messages()`; a sidecar container sets it to a mounted
        # host dir so the host-side test can read deliveries out of its own memory
        # space (the sidecar's `_messages` list is unreachable cross-process).
        self._out_dir = out_dir
        if out_dir:
            os.makedirs(out_dir, exist_ok=True)
        self._seq = 0
        self._messages: list[bytes] = []
        # Parallel to `_messages`: the envelope MAIL FROM each was delivered
        # under (the SRS forward test reads the rewritten `SRS0=…` sender).
        self._records: list[tuple[str, bytes]] = []
        self._lock = threading.Lock()
        self._stopped = threading.Event()
        self._thread = threading.Thread(target=self._serve, daemon=True)
        # When enabled, the stub advertises + serves STARTTLS with a fresh
        # self-signed cert; `cert_der` is the leaf DER it presents (so a DANE
        # test can publish sha256(cert_der) as the TLSA value). Default off →
        # plaintext-only, identical to the original stub (the bridge's
        # opportunistic sender falls back to cleartext).
        #
        # `require_starttls` additionally rejects MAIL/RCPT/DATA before a TLS
        # upgrade with `530 5.7.0` (implies advertising). A well-behaved
        # opportunistic sender (RFC 7435) always upgrades when STARTTLS is
        # advertised, so the rejection never fires for it — which makes a
        # *successful* delivery proof that the message was relayed over TLS, not
        # cleartext. The deploy-image outbound STARTTLS round-trip uses this to
        # assert the bridge's opportunistic-TLS leg without needing to inspect
        # the cross-process socket state.
        #
        # `tls_context` lets a caller supply the server SSLContext instead of
        # generating one here. The docker sidecar (`_main`) does this: it runs in
        # a stdlib-only python image without `cryptography`, so it loads the
        # committed static test cert via stdlib `ssl` and passes it in, rather
        # than calling `_make_selfsigned_tls_context` (which imports
        # `cryptography` and would crash the sidecar on boot). When a context is
        # supplied, `cert_der` is None — only the in-process DANE tests, which
        # generate a fresh cert here, need the leaf DER for the TLSA pin.
        # `on_message`, when set, is invoked with each accepted message's raw
        # bytes *after* the `250 queued` is sent, in a fresh daemon thread (so a
        # slow callback never stalls the SMTP response or the accept loop). This
        # turns the passive sink into an AUTO-REPLYING external MX: the
        # auto-reply round-trip test sets it to a closure that parses the
        # message's `From:` and delivers a reply back inbound to the nest
        # (`helpers.mail_wire.build_reply_message`). Exceptions are caught +
        # logged so a callback fault can't kill the stub's serving thread.
        # Public + settable post-construction (the fixture builds the stub before
        # the test knows the reply target). Default None → the original passive
        # sink, unchanged for every existing test.
        self.on_message = on_message
        self._require_starttls = require_starttls
        self._enable_starttls = enable_starttls or require_starttls
        self.cert_der: bytes | None = None
        self._tls_ctx: ssl.SSLContext | None = tls_context
        if self._enable_starttls and self._tls_ctx is None:
            self._tls_ctx, self.cert_der = _make_selfsigned_tls_context(self.host)

    @property
    def target(self) -> str:
        """`host:port` string for the operator-hatch `mta_mx_override` value."""
        return f"{self.host}:{self.port}"

    def start(self) -> "StubMX":
        self._thread.start()
        return self

    def stop(self) -> None:
        self._stopped.set()
        try:
            self._sock.close()
        except OSError:
            pass

    def messages(self) -> list[bytes]:
        with self._lock:
            return list(self._messages)

    def records(self) -> list[tuple[str, bytes]]:
        """Each accepted message as `(envelope_mail_from, raw_bytes)`, in
        arrival order. Use this (not `messages()`) when the test must assert on
        the envelope sender — e.g. the SRS-rewritten `SRS0=…@<domain>` MAIL FROM
        of a forwarded message."""
        with self._lock:
            return list(self._records)

    def wait_for_message(self, timeout: float = 15.0) -> bytes | None:
        """Block until at least one message arrives or `timeout` elapses;
        return the first received message (raw bytes) or None."""
        deadline = time.monotonic() + timeout
        while time.monotonic() < deadline:
            msgs = self.messages()
            if msgs:
                return msgs[0]
            time.sleep(0.1)
        return None

    # ── internals ─────────────────────────────────────────────────────

    def _run_on_message(self, raw: bytes) -> None:
        """Invoke the auto-reply callback, swallowing + logging any fault so the
        stub's serving threads survive a misbehaving callback."""
        try:
            self.on_message(raw)
        except Exception as e:  # noqa: BLE001 - a callback fault must not kill the stub
            import traceback
            print(f"stub-mx on_message callback raised: {e}", flush=True)
            traceback.print_exc()

    def _write_out(self, raw: bytes) -> None:
        """Persist one accepted message to `out_dir/<seq>.eml` for a cross-process
        reader. Write to a temp name + rename so a polling reader never sees a
        half-written file. Caller holds `self._lock`."""
        self._seq += 1
        final = os.path.join(self._out_dir, f"{self._seq:04d}.eml")
        tmp = final + ".tmp"
        with open(tmp, "wb") as f:
            f.write(raw)
        os.rename(tmp, final)

    def _serve(self) -> None:
        while not self._stopped.is_set():
            try:
                conn, _ = self._sock.accept()
            except OSError:
                return  # socket closed by stop()
            threading.Thread(target=self._handle, args=(conn,), daemon=True).start()

    def _handle(self, conn: socket.socket) -> None:
        with conn:
            conn.settimeout(10.0)
            # `box` holds the live socket so STARTTLS can swap in the
            # TLS-wrapped one; the send/recv closures read box[0].
            box = [conn]
            buf = bytearray()

            def send(line: str) -> None:
                box[0].sendall((line + "\r\n").encode())

            def recv_line() -> str | None:
                nonlocal buf
                while b"\r\n" not in buf:
                    try:
                        chunk = box[0].recv(4096)
                    except OSError:
                        return None
                    if not chunk:
                        return None
                    buf.extend(chunk)
                line, _, rest = buf.partition(b"\r\n")
                buf = bytearray(rest)
                return line.decode("utf-8", errors="replace")

            send("220 stub-mx.external.test ESMTP ready")
            in_data = False
            in_tls = False
            data_lines: list[str] = []
            cur_mail_from = ""
            while True:
                line = recv_line()
                if line is None:
                    return
                if in_data:
                    if line == ".":
                        raw = ("\r\n".join(data_lines) + "\r\n").encode("utf-8", "replace")
                        with self._lock:
                            self._messages.append(raw)
                            self._records.append((cur_mail_from, raw))
                            if self._out_dir:
                                self._write_out(raw)
                        send("250 2.0.0 Ok: queued")
                        if self.on_message is not None:
                            threading.Thread(
                                target=self._run_on_message, args=(raw,), daemon=True
                            ).start()
                        in_data = False
                        data_lines = []
                        continue
                    # RFC 5321 §4.5.2 transparency: un-stuff a leading dot.
                    if line.startswith(".."):
                        line = line[1:]
                    data_lines.append(line)
                    continue
                upper = line.upper()
                if (self._require_starttls and not in_tls
                        and upper.startswith(("MAIL FROM", "RCPT TO", "DATA"))):
                    # Mail-flow before a TLS upgrade is refused (RFC 3207 §4),
                    # so a delivered message proves the peer used STARTTLS.
                    send("530 5.7.0 Must issue STARTTLS first")
                    continue
                if upper.startswith(("EHLO", "HELO")):
                    send("250-stub-mx.external.test")
                    # Advertise STARTTLS only when enabled and not already
                    # upgraded (RFC 3207); otherwise the bridge sends plaintext.
                    if self._enable_starttls and not in_tls:
                        send("250-STARTTLS")
                    send("250 SIZE 52428800")
                elif upper.startswith("STARTTLS") and self._enable_starttls and not in_tls:
                    send("220 2.0.0 Ready to start TLS")
                    try:
                        box[0] = self._tls_ctx.wrap_socket(box[0], server_side=True)
                    except (ssl.SSLError, OSError):
                        # A pin-mismatch handshake fails here (the bridge's
                        # VerifyPeerCertificate rejects the cert) — the test
                        # asserts non-delivery, so just drop the connection.
                        return
                    # RFC 3207: discard any state; the client re-EHLOs over TLS.
                    buf = bytearray()
                    in_tls = True
                elif upper.startswith("MAIL FROM"):
                    cur_mail_from = _mail_from_addr(line)
                    send("250 2.1.0 Ok")
                elif upper.startswith("RCPT TO"):
                    # Permanent-reject any recipient whose local part starts
                    # with "nonexistent" so the bounce/NDR path (T1.2) has a
                    # deterministic 5xx to classify as permanent; transient-
                    # reject "tempfail*" with a 4xx so the retry curve + 4 h
                    # delay-warning path (T1.3) exercises without an immediate
                    # bounce. Every other recipient is accepted, so existing
                    # round-trip tests (recipient@external.test) are unaffected.
                    local = _rcpt_local_part(line).lower()
                    if local.startswith("nonexistent"):
                        send("550 5.1.1 <recipient> Recipient address rejected: User unknown")
                    elif local.startswith("tempfail"):
                        send("451 4.3.0 <recipient> Temporarily unavailable, try again later")
                    else:
                        send("250 2.1.5 Ok")
                elif upper.startswith("DATA"):
                    send("354 End data with <CR><LF>.<CR><LF>")
                    in_data = True
                elif upper.startswith("QUIT"):
                    send("221 2.0.0 Bye")
                    return
                else:  # RSET / NOOP / anything else
                    send("250 2.0.0 Ok")


def _main() -> None:
    """Run the stub as a standalone process — used by the docker deploy-image
    outbound test, which runs this module as a sidecar container on the bridge's
    user-defined network (the in-process fixture instead constructs `StubMX`
    directly). The bridge resolves the sidecar by container name + the fixed
    `STUB_MX_PORT` (its `mta_mx_override` target) and delivers over the wire;
    each accepted message lands in `STUB_MX_OUT_DIR` (a mounted host dir) for the
    host-side test to read. Binds 0.0.0.0 so peers off-loopback can reach it.
    """
    bind_host = os.environ.get("STUB_MX_BIND_HOST", "0.0.0.0")
    port = int(os.environ.get("STUB_MX_PORT", "2525"))
    out_dir = os.environ.get("STUB_MX_OUT_DIR", "/out")
    # STUB_MX_REQUIRE_STARTTLS=1 makes the sidecar advertise STARTTLS and refuse
    # cleartext mail-flow, so a delivery proves the bridge's opportunistic-TLS
    # leg; STUB_MX_STARTTLS=1 advertises it but still accepts cleartext fallback.
    enable_starttls = os.environ.get("STUB_MX_STARTTLS") == "1"
    require_starttls = os.environ.get("STUB_MX_REQUIRE_STARTTLS") == "1"
    # The sidecar runs in a stdlib-only python image (no `cryptography`), so it
    # can't generate a cert at runtime like the in-process fixture — load the
    # committed self-signed test cert via stdlib ssl and hand it to StubMX. (A
    # missing/unreadable cert here would crash the sidecar; the helper that
    # starts it confirms the container stays Running.)
    tls_context = None
    if enable_starttls or require_starttls:
        here = os.path.dirname(os.path.abspath(__file__))
        tls_context = ssl.SSLContext(ssl.PROTOCOL_TLS_SERVER)
        tls_context.load_cert_chain(
            os.path.join(here, "stub_mx_tls_cert.pem"),
            os.path.join(here, "stub_mx_tls_key.pem"))
    stub = StubMX(bind_host=bind_host, port=port, out_dir=out_dir,
                  enable_starttls=enable_starttls, require_starttls=require_starttls,
                  tls_context=tls_context).start()
    print(f"stub-mx listening on {bind_host}:{stub.port}, writing to {out_dir} "
          f"(starttls={enable_starttls} require_starttls={require_starttls})",
          flush=True)
    try:
        while True:
            time.sleep(3600)
    except KeyboardInterrupt:
        stub.stop()


if __name__ == "__main__":
    _main()
