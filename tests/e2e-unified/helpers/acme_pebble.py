"""The shared certificate-test venue's client: a pebble ACME CA plus a fake
authoritative name server, for walks that open a real certificate order FROM AN
APP.

The certificate authority is a constant no human chooses
(`tls-certificates.md` § ACME settings — constants, not choices), so an app
reaches this CA only through the compile-gated e2e ACME directory seed
(`e2e-gated-seeds.md` § The ACME directory seed):
``FAUNA_E2E_ACME_DIRECTORY_URL`` / ``FAUNA_E2E_ACME_CA_PEM`` /
``FAUNA_E2E_ACME_NAMESERVER``, read by `fauna_client_dns::e2e_acme_seam` and
installed by the shared native admin-dns machine builder. :meth:`AcmePebble.env`
is exactly that seed.

The stack is ONE standing venue on the Linux dev VM (`e2e-gated-seeds.md` § The ACME
directory seed → *The shared venue*), created and kept current by
``helpers/acme_venue.py`` (``just acme-venue ensure``): two containers under
docker's ``unless-stopped`` policy, published on fixed ports
(:data:`VENUE_DIRECTORY_PORT`, :data:`VENUE_NAME_SERVER_PORT` for TCP and UDP)
on exactly two addresses — the Linux dev VM's loopback and its address on the VMs'
private network (:data:`VENUE_PRIVATE_ADDR`). Every machine's harness uses it:
on the Linux dev VM it seeds loopback (so the android launch's reverse and the web
reading's loopback-only nest stand unchanged), on the macOS and Windows dev VMs — which
have no docker by design — the private-network address. Which one is derived
from the machine (:func:`venue_reach`); the harness's ``--acme-venue-reach``
option overrides it for the proof that walks the private address from the Linux dev VM.

- ``fake_dns`` (`fakes/fake_dns.py`) — the name server pebble validates DNS-01
  against AND the app's propagation gate polls. The harness writes the
  challenge the admin "pastes" as an RFC 2136 UPDATE
  (:meth:`AcmePebble.publish_txt`), into the in-memory overlay the app's
  managed-path mirror writes too, and reads how often a name was asked from the
  fake's own query count (:meth:`AcmePebble.txt_queries`).
- ``pebble`` — its directory served with a leaf the venue minted for both
  addresses, the minting authority behind it in the chain; the harness reads
  that authority off the directory's own TLS handshake
  (:func:`directory_authority_pem`) and hands it to the app as PEM text, so the
  app's ACME client trusts that CA and nothing else.

Concurrent runs stay apart by name, not by zone: every marked walk orders for a
domain carrying a random label minted in the test (decision 5). The docker-tier
HTTP-01 test keeps its own per-test stack and shares only the image pin, the
config builder, the start and the CA extraction from here.
"""

from __future__ import annotations

import functools
import os
import secrets
import socket
import ssl
import struct
import subprocess
import sys
import tempfile
import time

#: Pinned to the pebble release whose ACME wire shape `instant-acme 0.7.2`
#: parses — same pin + rationale as `libs/fauna-client-dns/tests/pebble_dns01.rs`.
PEBBLE_IMAGE = "ghcr.io/letsencrypt/pebble:2.9.0"
PEBBLE_CA_PATH = "/test/certs/pebble.minica.pem"

#: The machine the shared venue stands on.
VENUE_OS = "linux"
#: The Linux dev VM's address on the VMs' private network, the host the fleet
#: broker listens on — the venue gating test
#: pins the two together. The venue is published here and on loopback, never
#: on ``0.0.0.0`` or another interface.
VENUE_PRIVATE_ADDR = "10.1.8.35"
VENUE_LOOPBACK_ADDR = "127.0.0.1"
#: Fixed ports, both below the kernel's ephemeral range (32768 up) that the
#: harness's free-port rule draws from, so no harness nest ever takes one —
#: and off pebble's own 14000, which the Rust pebble testkit
#: (`libs/fauna-pebble-testkit`) binds on the host network.
VENUE_DIRECTORY_PORT = 14400
VENUE_NAME_SERVER_PORT = 14453
#: The values ``--acme-venue-reach`` takes.
REACHES = ("loopback", "private")

#: Set once by the conftest from ``--acme-venue-reach``; None derives the reach
#: from the machine.
_reach_override: str | None = None


def pebble_config(http_port: int = 5002, *, validity_secs: int | None = None) -> dict:
    """pebble's baked default config (extracted from the 2.9.0 image), with
    ``httpPort`` settable — the docker-tier HTTP-01 test repoints it at the
    nest's challenge listener. Mounted over the image's default config path so
    `/app -dnsserver <addr>` picks it up; relative cert paths resolve against
    pebble's WorkingDir `/`, so the baked `/test/certs/localhost/*` directory
    cert (SANs `localhost, pebble, 127.0.0.1`) serves the ACME API. The shared
    venue replaces the two certificate paths with its own minted leaf.

    ``validity_secs`` sets the ``default`` profile's certificate lifetime — the
    profile an ACME client that names none (the nest's) is issued under — so a
    renewal witness can be handed a leaf already inside the nest's 30-day
    renewal lead."""
    config = {
        "pebble": {
            "listenAddress": "0.0.0.0:14000",
            "managementListenAddress": "0.0.0.0:15000",
            "certificate": "test/certs/localhost/cert.pem",
            "privateKey": "test/certs/localhost/key.pem",
            "httpPort": http_port,
            "tlsPort": 5001,
            "ocspResponderURL": "",
            "externalAccountBindingRequired": False,
            "domainBlocklist": ["blocked-domain.example"],
            "retryAfter": {"authz": 3, "order": 5},
            "keyAlgorithm": "ecdsa",
        }
    }
    if validity_secs is not None:
        config["pebble"]["profiles"] = {
            "default": {
                "description": "short-lived, for the renewal witness",
                "validityPeriod": validity_secs,
            }
        }
    return config


def start_pebble(
    network: str,
    name: str,
    config_path: str,
    dnsserver: str,
    host_port: int,
    *,
    va_nosleep: bool = False,
) -> None:
    """Run pebble on `network` with the mounted config, validating DNS through
    `dnsserver`, its directory published at ``127.0.0.1:<host_port>`` — the
    docker-tier HTTP-01 test's own per-test pebble.
    `PEBBLE_VA_ALWAYS_VALID=0` keeps real validation; `PEBBLE_WFE_NONCEREJECT=0`
    makes nonces deterministic; `va_nosleep` skips pebble's artificial
    validation delay."""
    from tests.platform.docker.helpers import fence_cpuset_args, remove_container

    remove_container(name)
    env = ["-e", "PEBBLE_VA_ALWAYS_VALID=0", "-e", "PEBBLE_WFE_NONCEREJECT=0"]
    if va_nosleep:
        env += ["-e", "PEBBLE_VA_NOSLEEP=1"]
    r = subprocess.run(
        ["docker", "run", "-d", *fence_cpuset_args(), "--name", name, "--network", network,
         "-p", f"127.0.0.1:{host_port}:14000",
         "-v", f"{config_path}:/test/config/pebble-config.json:ro",
         *env,
         PEBBLE_IMAGE, "-dnsserver", dnsserver],
        capture_output=True, text=True, timeout=60,
    )
    if r.returncode != 0:
        raise RuntimeError(f"docker run pebble failed:\n{r.stderr}")


def extract_pebble_ca(name: str) -> bytes:
    """`docker cp` pebble's throwaway directory CA out (retried while the
    container filesystem comes up)."""
    dest = os.path.join(tempfile.gettempdir(), f"{name}-{os.getpid()}-minica.pem")
    for attempt in range(40):
        cp = subprocess.run(
            ["docker", "cp", f"{name}:{PEBBLE_CA_PATH}", dest],
            capture_output=True, text=True, timeout=30,
        )
        if cp.returncode == 0 and os.path.exists(dest):
            with open(dest, "rb") as f:
                data = f.read()
            os.remove(dest)
            return data
        if attempt == 39:
            raise RuntimeError(f"could not copy pebble CA from {name}:{PEBBLE_CA_PATH}: {cp.stderr}")
        time.sleep(0.25)
    raise AssertionError("unreachable")


def name_server_port(env: dict) -> int | None:
    """The port of the name server a launch environment's ACME seed names, or
    None when it names none — the second port an android launch must reverse."""
    ns = env.get("FAUNA_E2E_ACME_NAMESERVER")
    return int(ns.rsplit(":", 1)[1]) if ns else None


def directory_port(env: dict) -> int | None:
    """The port of the directory a launch environment's ACME seed names, or
    None when the environment carries no seed — the port an android launch
    must reverse."""
    from urllib.parse import urlparse

    url = env.get("FAUNA_E2E_ACME_DIRECTORY_URL")
    return urlparse(url).port if url else None


def wait_tcp(host: str, port: int, timeout: float) -> None:
    deadline = time.monotonic() + timeout
    while time.monotonic() < deadline:
        try:
            with socket.create_connection((host, port), timeout=1):
                return
        except OSError:
            time.sleep(0.25)
    raise RuntimeError(f"{host}:{port} not reachable within {timeout}s")


# ── where the venue is, from this harness (decision 3) ────────────────────────


def host_os() -> str:
    """The OS this harness runs on — ``linux``, ``macos`` or ``windows``: one
    dev VM each, and the venue stands on the Linux one."""
    if sys.platform.startswith("linux"):
        return "linux"
    if sys.platform == "darwin":
        return "macos"
    return "windows"


def set_reach_override(reach: str | None) -> None:
    """The conftest's ``--acme-venue-reach``: verifier wiring like ``--app``,
    never a product knob."""
    global _reach_override
    if reach is not None and reach not in REACHES:
        raise ValueError(f"--acme-venue-reach takes one of {REACHES}, not {reach!r}")
    _reach_override = reach
    venue_ready.cache_clear()


def venue_reach() -> str:
    """``loopback`` on the venue's own machine, ``private`` elsewhere, unless
    the harness option says otherwise."""
    if _reach_override:
        return _reach_override
    return "loopback" if host_os() == VENUE_OS else "private"


def venue_host(reach: str | None = None) -> str:
    """The address the harness seeds for the venue."""
    return VENUE_PRIVATE_ADDR if (reach or venue_reach()) == "private" else VENUE_LOOPBACK_ADDR


def directory_answers(host: str, timeout: float = 3) -> bool:
    """Whether the venue's directory port accepts a connection at ``host``."""
    try:
        with socket.create_connection((host, VENUE_DIRECTORY_PORT), timeout=timeout):
            return True
    except OSError:
        return False


@functools.lru_cache(maxsize=1)
def venue_ready() -> tuple[bool, str]:
    """``(True, "")`` when the venue's directory answers at the seeded address,
    else ``(False, why)`` — asked once a run (decision 7: reach is checked,
    never assumed).

    On the Linux dev VM a venue that does not answer is ensured first
    (``helpers/acme_venue.py``), and one still down after that fails the marked
    tests; elsewhere it is an environment skip naming the venue. Cached because
    the seed is asked at every app launch of the selection, marked test or not,
    and a raise there would be replayed at each of them."""
    host = venue_host()
    if directory_answers(host):
        return True, ""
    if host_os() == VENUE_OS:
        try:
            from helpers import acme_venue

            report = acme_venue.ensure()
        except Exception as exc:  # noqa: BLE001 — the reason is the report
            report = f"{type(exc).__name__}: {exc}"
        if directory_answers(host, timeout=10):
            return True, ""
        return False, (
            f"the shared ACME venue does not answer at {host}:{VENUE_DIRECTORY_PORT} "
            f"after `just acme-venue ensure`: {report}"
        )
    return False, (
        f"the shared ACME venue on the Linux dev VM does not answer at "
        f"{host}:{VENUE_DIRECTORY_PORT}"
    )


def directory_authority_pem(host: str, port: int = VENUE_DIRECTORY_PORT) -> str:
    """The authority behind the directory's certificate, read off its own TLS
    handshake — the last certificate of the chain the server sends (decision
    4). Taken on first sight: a host-internal network, a test double's
    directory, trusted by a test build's seeded ACME client alone."""
    ctx = ssl.SSLContext(ssl.PROTOCOL_TLS_CLIENT)
    ctx.check_hostname = False
    ctx.verify_mode = ssl.CERT_NONE
    with socket.create_connection((host, port), timeout=10) as raw:
        with ctx.wrap_socket(raw, server_hostname=host) as tls:
            chain = tls.get_unverified_chain() or []
    if len(chain) < 2:
        raise RuntimeError(
            f"the directory at {host}:{port} sent {len(chain)} certificate(s); the venue's "
            "pebble must serve its leaf with the authority behind it"
        )
    last = chain[-1]
    der = last if isinstance(last, (bytes, bytearray)) else last.public_bytes(ssl.ENCODING_DER)
    return ssl.DER_cert_to_PEM_cert(bytes(der))


# ── the name server, in its own protocol (decision 6) ─────────────────────────

_TYPE_TXT, _TYPE_SOA = 16, 6
_CLASS_IN, _CLASS_ANY = 1, 255
_OPCODE_UPDATE = 5
#: Mirrors `fakes/fake_dns.py`'s ``QUERY_COUNT_LABEL`` (the venue gating test
#: pins the two spellings together).
QUERY_COUNT_LABEL = "_fake-dns-queries"


def _wire_name(name: str) -> bytes:
    return b"".join(
        bytes([len(label)]) + label.encode("ascii")
        for label in name.rstrip(".").split(".")
    ) + b"\x00"


def _txt_rdata(value: str) -> bytes:
    raw = value.encode("ascii")
    return b"".join(
        bytes([len(seg)]) + seg for seg in (raw[i : i + 255] for i in range(0, len(raw), 255))
    ) or b"\x00"


def replace_txt_update(name: str, value: str, txn: int | None = None) -> bytes:
    """One RFC 2136 UPDATE replacing ``name``'s TXT RRset with ``value``: a
    delete-RRset (class ANY) and an add (class IN), zone the name's parent."""
    name = name.rstrip(".").lower()
    zone = name.split(".", 1)[1] if "." in name else name
    txn = secrets.randbelow(0x10000) if txn is None else txn
    header = struct.pack(">HHHHHH", txn, _OPCODE_UPDATE << 11, 1, 0, 2, 0)
    delete = _wire_name(name) + struct.pack(">HHIH", _TYPE_TXT, _CLASS_ANY, 0, 0)
    rdata = _txt_rdata(value)
    add = _wire_name(name) + struct.pack(">HHIH", _TYPE_TXT, _CLASS_IN, 60, len(rdata)) + rdata
    return header + _wire_name(zone) + struct.pack(">HH", _TYPE_SOA, _CLASS_IN) + delete + add


def txt_query(name: str, txn: int | None = None) -> bytes:
    txn = secrets.randbelow(0x10000) if txn is None else txn
    return (struct.pack(">HHHHHH", txn, 0, 1, 0, 0, 0) + _wire_name(name)
            + struct.pack(">HH", _TYPE_TXT, _CLASS_IN))


def _skip_name(msg: bytes, offset: int) -> int:
    while True:
        length = msg[offset]
        if length & 0xC0 == 0xC0:
            return offset + 2
        offset += 1 + length
        if length == 0:
            return offset


def txt_answers(reply: bytes) -> tuple[int, list[str]]:
    """``(rcode, [TXT value, ...])`` of a DNS reply."""
    _, flags, qdcount, ancount, _, _ = struct.unpack(">HHHHHH", reply[:12])
    offset = 12
    for _ in range(qdcount):
        offset = _skip_name(reply, offset) + 4
    values = []
    for _ in range(ancount):
        offset = _skip_name(reply, offset)
        rtype, _, _, rdlength = struct.unpack(">HHIH", reply[offset : offset + 10])
        rdata = reply[offset + 10 : offset + 10 + rdlength]
        offset += 10 + rdlength
        if rtype == _TYPE_TXT:
            out, i = b"", 0
            while i < len(rdata):
                out += rdata[i + 1 : i + 1 + rdata[i]]
                i += 1 + rdata[i]
            values.append(out.decode("ascii", "replace"))
    return flags & 0x000F, values


def dns_over_tcp(host: str, port: int, msg: bytes, timeout: float = 10) -> bytes:
    """One DNS message over TCP, two-byte length framing (RFC 1035 § 4.2.2)."""

    def recv_exactly(s: socket.socket, n: int) -> bytes:
        buf = b""
        while len(buf) < n:
            chunk = s.recv(n - len(buf))
            if not chunk:
                raise ConnectionError("the name server closed mid-reply")
            buf += chunk
        return buf

    with socket.create_connection((host, port), timeout=timeout) as s:
        s.sendall(struct.pack(">H", len(msg)) + msg)
        (length,) = struct.unpack(">H", recv_exactly(s, 2))
        return recv_exactly(s, length)


class AcmePebble:
    """The shared venue, as one test session sees it. :meth:`start` checks the
    venue answers and reads its authority; :meth:`env` is the app's
    launch-environment seed; :meth:`stop` leaves the venue standing."""

    def __init__(self, host: str | None = None) -> None:
        self.host = host or venue_host()
        self.directory_url = ""
        self.ca_pem = ""
        self.nameserver = ""

    def start(self) -> "AcmePebble":
        ok, why = venue_ready()
        if not ok:
            raise RuntimeError(why)
        self.ca_pem = directory_authority_pem(self.host)
        self.directory_url = f"https://{self.host}:{VENUE_DIRECTORY_PORT}/dir"
        self.nameserver = f"{self.host}:{VENUE_NAME_SERVER_PORT}"
        return self

    def env(self) -> dict[str, str]:
        """The e2e ACME directory seed for an app launch."""
        return {
            "FAUNA_E2E_ACME_DIRECTORY_URL": self.directory_url,
            "FAUNA_E2E_ACME_CA_PEM": self.ca_pem,
            # Also where the e2e fake DNS provider writes every TXT it publishes
            # (an UPDATE over TCP), so a MANAGED order's challenge is served
            # exactly as a real provider's zone would serve it.
            "FAUNA_E2E_ACME_NAMESERVER": self.nameserver,
        }

    def publish_txt(self, name: str, value: str) -> None:
        """Serve ``name TXT value`` from the venue's name server from the next
        query on — the admin "pasting" the challenge at their registrar. One
        UPDATE replacing the name's RRset, into the overlay the app's managed
        mirror writes too."""
        reply = dns_over_tcp(self.host, VENUE_NAME_SERVER_PORT, replace_txt_update(name, value))
        rcode, _ = txt_answers(reply)
        if rcode != 0:
            raise RuntimeError(f"the venue's name server refused the UPDATE for {name}: rcode {rcode}")

    def txt_queries(self, name: str) -> int:
        """How many ``TXT`` queries for ``name`` the venue's name server has
        answered so far — its own count under the reserved label, so a test can
        wait for a probe to have happened rather than sleep past it."""
        probe = f"{QUERY_COUNT_LABEL}.{name.rstrip('.').lower()}"
        _, values = txt_answers(dns_over_tcp(self.host, VENUE_NAME_SERVER_PORT, txt_query(probe)))
        return int(values[0]) if values else 0

    def stop(self) -> None:
        """Nothing: the venue stands for every machine's runs."""


def skip_android_until_it_reaches_the_acme_stack(request) -> None:
    """Android is a declared environment skip (convention 7) for every witness
    that orders from pebble: the witness is unverified.

    The whole stack reaches the app: `launchApp()` puts the three
    `FAUNA_E2E_ACME_*` keys on the intent and `MainActivity` re-exports them;
    the venue's directory and name server are on fixed loopback ports of
    the Linux dev VM the launch reverses (:func:`directory_port`,
    :func:`name_server_port`), and the propagation probe's TCP retry and
    the managed mirror's UPDATE both ride that TCP-only reverse. But no android
    e2e run has gone through the venue yet, so it stays skipped until that first
    run shows it green.

    Call it from an autouse fixture. It reads the app from the parametrization,
    not the driver, so the skip comes before the app fixture builds and installs
    an APK it will never drive."""
    from helpers.app_surface import skip_environment

    if request.node.callspec.params.get("app") not in ("android", "android-kids"):
        return
    skip_environment(
        "android: the ACME seed is forwarded and pebble's and the name server's "
        "ports are reversed, but no android run has gone through the venue yet "
        "to verify this test"
    )
