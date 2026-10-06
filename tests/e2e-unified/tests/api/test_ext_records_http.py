"""tier_3 API e2e: **the HTTP record door** — a remote principal's own
``ext.*`` records over plain HTTPS
(``docs/goal/architecture/third-party-kinds.md`` § Kind namespacing → *Two
doors onto the same plane*, § The record doors; the classification,
``api-layers.md`` § HTTP residue; the token,
``docs/goal/behavior/authorization-server.md`` § Tokens authorize transport).

The consent is ``test_ext_kinds``'s own (its client document, manifest and
consent-time grant, imported): a client attesting both keys holds
``fauna:records:rw:ext.example.com.*``. Then, over HTTP alone with the
DPoP-bound token:

- ``PUT /api/v1/records/{kind}/{key}`` a row the principal sealed as its
  writer, ``GET`` it back byte-identical, put a second, walk the kind with a
  cursor to its end;
- one plane, two doors: a principal session over WS-RPC lists the same rows,
  the same bytes;
- ``DELETE`` puts the principal's sealed tombstone, and the item reads back as
  that tombstone;
- refusals: another publisher's kind (outside this token's prefix), a
  ``fauna.*`` kind by name, a ``Bearer`` (non-DPoP) presentation, a key that is
  not a 32-byte blinded ``item_key``, and an entry that is not a sealed
  envelope;
- revoke: the door closes ``401``.
"""

import base64
import json
import secrets
import time
import urllib.error
import urllib.parse
import urllib.request

import pytest
from cryptography.hazmat.primitives.asymmetric import ed25519, x25519

import fauna_ffi
from helpers.atproto_consent import (  # noqa: F401 — consent_nest/consent_bridge are pytest fixtures adopted by import
    HANDLE_DOMAIN,
    consent_bridge,
    consent_nest,
)
from helpers.budgets import RPC_ROUNDTRIP_S
from helpers.oauth_client import NONCE_HEADER, OAuthClient
from tests.api.test_ext_kinds import (  # noqa: F401 — metadata_server/consent_nest_env are fixtures adopted by import
    NOTES,
    RECORDS,
    REDIRECT_URI,
    STATE_FEED,
    _approve_with_grant,
    _raw,
    _secret,
    consent_nest_env,
    metadata_server,
)
from tests.api.test_third_party_session import _alice, _Session, _upgrade

pytestmark = pytest.mark.tier_3


def _http(nest, client, token, method, path, body=None, query=None, scheme="DPoP"):
    """One request at the record door with the token and a fresh DPoP proof
    over ``method`` + the door's ``https`` URL (no query — RFC 9449's ``htu``);
    one ``use_dpop_nonce`` retry, the protocol's own. Returns
    ``(status, json body)``."""
    htu = f"https://{HANDLE_DOMAIN}{path}"
    url = f"{nest['url']}{path}"
    if query:
        url += "?" + urllib.parse.urlencode(query)

    def send():
        headers = {
            "Authorization": f"{scheme} {token}",
            "DPoP": client.dpop_proof(method, htu, access_token=token),
        }
        if body is not None:
            headers["Content-Type"] = "application/octet-stream"
        request = urllib.request.Request(url, data=body, method=method, headers=headers)
        try:
            with urllib.request.urlopen(request, timeout=RPC_ROUNDTRIP_S) as resp:
                return resp.status, dict(resp.headers), json.loads(resp.read() or b"{}")
        except urllib.error.HTTPError as refused:
            raw = refused.read()
            try:
                parsed = json.loads(raw) if raw else {}
            except ValueError:
                parsed = {"raw": raw.decode(errors="replace")}
            return refused.code, dict(refused.headers), parsed

    status, headers, parsed = send()
    lowered = {k.lower(): v for k, v in headers.items()}
    if status == 401 and "use_dpop_nonce" in lowered.get("www-authenticate", ""):
        client.nonce = lowered.get(NONCE_HEADER)
        status, _, parsed = send()
    return status, parsed


def _entry(record) -> bytes:
    raw = record["entry"]
    return base64.urlsafe_b64decode(raw + "=" * (-len(raw) % 4))


def _walk(nest, client, token, kind):
    """Walk ``kind`` from the start, cursor by cursor, to the first empty page.
    Returns every record and how many pages it took."""
    records, cursor, pages = [], 0, 0
    while True:
        status, page = _http(
            nest, client, token, "GET", f"/api/v1/records/{kind}", query={"cursor": cursor}
        )
        assert status == 200, f"the walk pages: {status} {page!r}"
        pages += 1
        if not page["records"]:
            return records, pages
        records.extend(page["records"])
        assert page["cursor"] > cursor, f"the cursor moves on: {page!r}"
        cursor = page["cursor"]
        assert pages < 50, "the walk ends"


@pytest.mark.feature("connected-apps")
def test_a_remote_principal_reads_and_writes_its_records_over_http(
    consent_nest, consent_bridge, metadata_server
):
    nest = consent_nest
    holder = x25519.X25519PrivateKey.generate()
    writer = ed25519.Ed25519PrivateKey.generate()
    holder_pub, writer_pub = _raw(holder.public_key()), _raw(writer.public_key())
    client = OAuthClient(
        base=nest["url"],
        htu_origin=f"https://{HANDLE_DOMAIN}",
        redirect_uri=REDIRECT_URI,
        scope=RECORDS,
        holder_x25519=holder_pub,
        writer_ed25519=writer_pub,
        client_id_url=metadata_server.client_id(),
    )
    tokens = _approve_with_grant(nest, client, holder_pub, writer_pub, secrets.token_bytes(16))
    token = tokens["access_token"]

    # The grant the principal seals with — fetched over its session, as a
    # device app does; a remote server reads the same grant the same way.
    session = _Session(_upgrade(nest, client, token))
    ok, reply = session.call("fauna.capabilities.fetch", {})
    assert ok, reply
    grants = [bytes(g) for g in reply.get("grants", [])]
    assert len(grants) == 1, reply
    grant = grants[0]

    def seal(key: str, value: bytes, seq: int, tombstone: bool = False):
        return fauna_ffi.ext_kind_seal_row(
            grant, _secret(holder), _secret(writer), NOTES, key, value, seq,
            int(time.time() * 1000), tombstone=tombstone,
        )

    # ── PUT, then GET it back byte-identical. ──
    first_key, first = seal("first-note", b"hello", 1)
    first_path = f"/api/v1/records/{NOTES}/{first_key.hex()}"
    status, put = _http(nest, client, token, "PUT", first_path, first, {"writer_seq": 1})
    assert status == 200 and isinstance(put.get("seq"), int), (status, put)

    status, got = _http(nest, client, token, "GET", first_path)
    assert status == 200, (status, got)
    assert len(got["records"]) == 1, got
    record = got["records"][0]
    assert _entry(record) == first, "the door echoes the sealed bytes verbatim"
    assert record["writer"] == writer_pub.hex() and record["writer_seq"] == 1, record
    assert record["op"] == "state-put" and record["key"] == first_key.hex(), record

    # ── A second row, then the cursor walk to its end. A writer's sequence is
    # one per scope, so a spent one refuses `409` whichever item names it. ──
    second_key, spent = seal("second-note", b"world", 1)
    second_path = f"/api/v1/records/{NOTES}/{second_key.hex()}"
    status, refused = _http(nest, client, token, "PUT", second_path, spent, {"writer_seq": 1})
    assert status == 409 and refused["error"].endswith("stale_writer_seq"), (status, refused)
    _, second = seal("second-note", b"world", 2)
    status, put = _http(nest, client, token, "PUT", second_path, second, {"writer_seq": 2})
    assert status == 200, (status, put)
    walked, pages = _walk(nest, client, token, NOTES)
    by_key = {r["key"]: _entry(r) for r in walked}
    assert by_key == {first_key.hex(): first, second_key.hex(): second}, walked
    assert pages >= 2, "a walk ends on an empty page"

    # ── One plane, two doors: the session lists what HTTP put. ──
    ok, reply = session.call(
        "fauna.sync.changes.list", {"scope": f"ext:{NOTES}", "item_class": STATE_FEED}
    )
    assert ok, reply
    over_ws = {c["path_hash"]: bytes(c["entry"]) for c in reply["changes"]}
    assert over_ws == by_key, f"the WS-RPC door serves the same rows: {reply!r}"

    # ── DELETE: the principal's sealed tombstone supersedes its row. ──
    _, tomb = seal("first-note", b"", 3, tombstone=True)
    status, deleted = _http(nest, client, token, "DELETE", first_path, tomb, {"writer_seq": 3})
    assert status == 200, (status, deleted)
    status, got = _http(nest, client, token, "GET", first_path)
    assert status == 200, (status, got)
    assert [(r["op"], r["writer_seq"]) for r in got["records"]] == [("tombstone", 3)], got
    assert _entry(got["records"][0]) == tomb

    # ── Refusals. ──
    status, refused = _http(
        nest, client, token, "PUT", f"/api/v1/records/ext.other.org.thing/{first_key.hex()}",
        first, {"writer_seq": 4},
    )
    assert status == 403, f"another publisher's kind is outside this token's prefix: {refused!r}"
    status, refused = _http(
        nest, client, token, "GET", "/api/v1/records/fauna.account.settings"
    )
    assert status == 403, f"a fauna.* kind is reachable by no name: {refused!r}"
    status, refused = _http(
        nest, client, token, "PUT", f"/api/v1/records/fauna.account.settings/{first_key.hex()}",
        first, {"writer_seq": 4},
    )
    assert status == 403, f"nor written: {refused!r}"
    status, refused = _http(nest, client, token, "GET", first_path, scheme="Bearer")
    assert status == 401, f"a bearer presentation is not a DPoP one: {refused!r}"
    status, refused = _http(
        nest, client, token, "PUT", f"/api/v1/records/{NOTES}/first-note",
        first, {"writer_seq": 4},
    )
    assert status == 400, f"the key is the blinded item_key, never a logical key: {refused!r}"
    third_key, _ = seal("third-note", b"x", 4)
    status, refused = _http(
        nest, client, token, "PUT", f"/api/v1/records/{NOTES}/{third_key.hex()}",
        b"not a sealed envelope", {"writer_seq": 4},
    )
    assert status == 400, f"the floor: a sealed gen-0 envelope: {refused!r}"

    # ── Revoke closes the door. ──
    with _alice(nest) as ws:
        principal = next(
            p for p in ws.call("fauna.principals.list", {})["principals"]
            if p["client_id"] == client.client_id
        )
        revoked = ws.call(
            "fauna.principals.revoke", {"principal_id": bytes(principal["principal_id"])}
        )
        assert revoked["revoked"] is True, revoked
    status, refused = _http(nest, client, token, "GET", f"/api/v1/records/{NOTES}")
    assert status == 401, f"a revoked principal's token opens no door: {refused!r}"
