"""Reaching an INSTALLED nest from outside it, at the box's own LAN address.

Shared by the desktop installers' install-then-reach tests (`tests/platform/
macos/test_installer.py`, `tests/platform/windows/test_installer.py`): each
installs its channel's nest, then claims it and reads its health at the LAN
address the channel promises other devices reach it at (installers/README.md
§ The channel contract) — never loopback, never a spawned stand-in.

Trust is the floor's SPKI pinned per authority (`common.auth.pin_nest_spki`);
no call here turns verification off.
"""

import ipaddress
import json
import socket
from urllib.parse import urlsplit


def lan_ipv4():
    """This box's own LAN IPv4 address — the source address the kernel picks for
    an off-box route (a UDP connect sends no packet). ``None`` when the box has
    no off-box route, or only a loopback / link-local one. Never falls back to
    loopback: a caller that needs the LAN address and gets ``None`` cannot run."""
    s = socket.socket(socket.AF_INET, socket.SOCK_DGRAM)
    try:
        s.connect(("192.0.2.1", 9))  # TEST-NET-1: routed, never answered
        addr = s.getsockname()[0]
    except OSError:
        return None
    finally:
        s.close()
    ip = ipaddress.ip_address(addr)
    if ip.is_loopback or ip.is_link_local or ip.is_unspecified:
        return None
    return addr


def pinned_health(base_url, timeout=5.0):
    """``GET /api/v1/health`` over a TLS session whose leaf SPKI equals the pin
    recorded for ``base_url``'s authority (``common.auth.open_pinned_tls``) —
    no verification-off path to the nest. Returns ``(status_code, body_json)``."""
    from common.auth import open_pinned_tls

    tls = open_pinned_tls(base_url, timeout)
    assert tls is not None, f"no SPKI pin recorded for {base_url}"
    host = urlsplit(base_url).netloc
    try:
        tls.sendall(
            f"GET /api/v1/health HTTP/1.1\r\nHost: {host}\r\n"
            "Accept: application/json\r\nConnection: close\r\n\r\n".encode())
        chunks = []
        while True:
            chunk = tls.recv(65536)
            if not chunk:
                break
            chunks.append(chunk)
    finally:
        tls.close()
    raw = b"".join(chunks)
    head, _, body = raw.partition(b"\r\n\r\n")
    status = int(head.split(b" ", 2)[1])
    if b"transfer-encoding: chunked" in head.lower():
        decoded, rest = b"", body
        while rest:
            size_line, _, rest = rest.partition(b"\r\n")
            size = int(size_line.split(b";")[0], 16)
            if size == 0:
                break
            decoded, rest = decoded + rest[:size], rest[size + 2:]
        body = decoded
    return status, json.loads(body) if body.strip() else None
