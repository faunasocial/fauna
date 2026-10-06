"""Raw-XRPC client helpers shared by the ATProto PDS tier_3 tests.

The bridge's ``--xrpc-listen`` terminates its own ``pds.<domain>`` TLS; a
tier_3 nest serves the self-signed floor cert, so this client skips
verification — it dials the loopback directly, and the property under test is
never the cert chain (that is tier_4 ``test_atproto_pds_sni_router.py``).
"""

from __future__ import annotations

import json
import ssl
import urllib.error
import urllib.request
from pathlib import Path

TLS_INSECURE = ssl._create_unverified_context()

# The committed cross-language credential fixture (minted once by the Rust
# `compute_app_credential_verifier`, verified by both the Go and Rust twins).
_FIXTURE = (
    Path(__file__).resolve().parents[3]
    / "bins/fauna-bridges/internal/auth/testdata/atproto_app_credential_phc.txt"
)


def load_app_credential_fixture() -> tuple[str, str]:
    """Return (secret, phc_verifier) from the committed fixture."""
    lines = [
        ln.strip()
        for ln in _FIXTURE.read_text().splitlines()
        if ln.strip() and not ln.startswith("#")
    ]
    assert len(lines) == 2, f"fixture shape: {lines!r}"
    return lines[0], lines[1]


def xrpc_post(base: str, nsid: str, body: dict | None, bearer: str | None = None):
    data = json.dumps(body or {}).encode()
    req = urllib.request.Request(
        f"{base}/xrpc/{nsid}", data=data, method="POST",
        headers={"Content-Type": "application/json"},
    )
    if bearer:
        req.add_header("Authorization", f"Bearer {bearer}")
    return _do(req)


def xrpc_post_bytes(base: str, nsid: str, data: bytes, content_type: str,
                    bearer: str | None = None):
    """POST a raw byte body (``com.atproto.repo.uploadBlob``'s shape).

    ``content_type`` is sent verbatim — the PDS sniffs the bytes and never
    trusts it, which is exactly what a test asserting sniffed-not-declared
    wants to be able to lie about.
    """
    req = urllib.request.Request(
        f"{base}/xrpc/{nsid}", data=data, method="POST",
        headers={"Content-Type": content_type},
    )
    if bearer:
        req.add_header("Authorization", f"Bearer {bearer}")
    return _do(req)


def xrpc_get(base: str, nsid: str, bearer: str | None = None,
             extra_headers: dict | None = None):
    req = urllib.request.Request(f"{base}/xrpc/{nsid}", method="GET")
    if bearer:
        req.add_header("Authorization", f"Bearer {bearer}")
    for k, v in (extra_headers or {}).items():
        req.add_header(k, v)
    return _do(req)


def put_blob(nest_url: str, token: str, cid_b32: str, data: bytes) -> None:
    """Upload bytes to the NEST's own CID-addressed blob route (not the
    bridge's XRPC ``uploadBlob``) — the same surface every Fauna app
    uploads media through, and the one the bridge reads back when it resolves
    a ``Profile``'s picture ``ContentHash``. The nest verifies
    ``blake3(body) == cid.digest()``.
    """
    req = urllib.request.Request(
        f"{nest_url}/api/v1/blob/{cid_b32}",
        data=data,
        method="PUT",
        headers={
            "Authorization": f"Bearer {token}",
            "Content-Type": "application/octet-stream",
        },
    )
    with urllib.request.urlopen(req, context=TLS_INSECURE, timeout=30) as resp:
        assert resp.status == 200, f"blob PUT returned {resp.status}"


def _do(req):
    """Return (status, body_dict). XRPC errors carry a JSON body on 4xx too."""
    try:
        with urllib.request.urlopen(req, timeout=15, context=TLS_INSECURE) as resp:
            return resp.status, json.loads(resp.read() or b"{}")
    except urllib.error.HTTPError as e:
        raw = e.read() or b"{}"
        try:
            return e.code, json.loads(raw)
        except json.JSONDecodeError:
            return e.code, {"_raw": raw.decode(errors="replace")}
