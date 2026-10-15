"""tier_3 API e2e: **a third-party principal reads a served folder over DAV**,
through the production consent path (``docs/goal/behavior/webdav-server.md``
§ Key model → *A principal's read*; the scope arm,
``docs/goal/behavior/authorization-server.md`` § Scope grammar → *The read
arm*).

The app's client document declares the BARE ``fauna:folder:read`` — the
folder plane's qualifier is the user's, so no static document can name one
(``authorization-server.md`` § Scope grammar → *The folder plane's qualifier
is the user's*); the approve carries the card's folder choice, and a
qualified request against the same document is the re-consent form. Every
step is a production one, app-less:

- the owner (this module's alice, no app) creates its sets custody-first and
  serves them through the shared ``FoldersAuthor::serve_enable`` — a genesis
  content key and the serve-on stamp in custody, then the nest's flag
  (``fauna_ffi.harness_serve_set``) — and writes a file into the served set
  through the apps' own engine (``harness_sync_pass``), so its bytes and its
  name rest sealed under the set's content key;
- a device-form principal (a public client, its X25519 holder key attested at
  PAR) asks for the scope; the owner approves through the consent machine's
  shared ``answer_consent`` over the seams the apps wire
  (``harness_answer_consent``) — the folder read twin planned from the
  owner's custody, its ``Mint`` recorded, deposited, then the resolve;
- the principal fetches its grant over its own session
  (``fauna.capabilities.fetch``), opens it into the ``Fauna-Folder-Keys``
  header (the production ``open_folder_read_keys``), and reads the owner's
  file over the real Go MDA's bearer door with its DPoP-bound token:
  byte-identical, the folder listed at the root by its id;
- the same token reaches no other folder — a second served folder of the
  owner's is neither listed nor readable — and the token without the keys
  opens nothing;
- after the owner unserves the folder (``serve_disable``, which also revokes
  the principal's twin), the principal's next request is refused: the door
  admits every request through the nest, nothing is cached;
- refusals before any grant: a confidential client's push for the scope,
  bare or qualified, is ``invalid_scope``; an approve over a folder the owner
  does not serve is refused by the planner with the card's reason, and the
  request stays pending.
"""

import re
import secrets
import ssl
import time
import urllib.error
import urllib.request

import pytest
from cryptography.hazmat.primitives import serialization
from cryptography.hazmat.primitives.asymmetric import ec, x25519

import fauna_ffi
from helpers.atproto_consent import (  # noqa: F401 — consent_nest/consent_bridge are pytest fixtures adopted by import
    HANDLE_DOMAIN,
    consent_bridge,
    consent_nest,
)
from helpers.budgets import RPC_ROUNDTRIP_S
from helpers.client_metadata_server import ClientMetadataServer, client_document
from helpers.oauth_client import NONCE_HEADER, OAuthClient
from helpers.principal_session import PrincipalSession, alice_client, upgrade

pytestmark = pytest.mark.tier_3

PUBLISHER = "example.com"
DOCUMENT = "/folder-read-client.json"
BARE_SCOPE = "fauna:folder:read"
REDIRECT_URI = "http://127.0.0.1:17777/callback"
#: The MDA's mail domain — any local domain the nest holds; the bearer door
#: routes by path, never by host.
MDA_DOMAIN = "principal-dav.test"
#: The DAV path's user segment — the bearer door addresses by folder id.
DAV_USER = "alice"
#: The owner's one writing device for the module, stable across its passes as
#: a real seat's is — a fresh id per pass would register a device per set and
#: run into the tier's device cap.
OWNER_DEVICE = secrets.token_bytes(32)


@pytest.fixture(scope="module")
def metadata_server(tmp_path_factory):
    server = ClientMetadataServer(PUBLISHER, tmp_path_factory.mktemp("client-metadata"))
    yield server
    server.close()


@pytest.fixture(scope="module")
def consent_nest_env(metadata_server):
    """The shared consent nest's env: its metadata fetcher pointed at
    ``metadata_server``."""
    return metadata_server.nest_env()


def _admin(nest):
    from clients.ws_rpc_admin_client import WsRpcAdminClient

    admin = nest["admin"]
    return WsRpcAdminClient(
        nest["url"],
        actor_id=bytes(admin["signing_key"].verify_key),
        signing_key=bytes(admin["signing_key"]),
    )


@pytest.fixture(scope="module")
def principal_mda(consent_nest, mail_bridge_binary, tmp_path_factory):
    """The real Go MDA against ``consent_nest``, its WebDAV mount live —
    arrangement, not the subject: the admin turns WebDAV on and holds a local
    domain (the MDA's TLS cert is the domain's), as the admin console does.
    Yields the spawned bridge; its ``caldav_port`` is the DAV listener."""
    from conftest import _spawn_mda_bridge

    nest = consent_nest
    with _admin(nest) as ws:
        ws.call("fauna.bridges.set_mail_enabled", {"enabled": True})
        ws.call(
            "fauna.bridges.add_local_domain",
            {"domain": MDA_DOMAIN, "mta_sts_cert_mode": "per_host"},
        )
        ws.call("fauna.bridges.set_webdav_enabled", {"enabled": True})
    spawned = _spawn_mda_bridge(
        mail_bridge_binary=mail_bridge_binary,
        nest_instance=nest,
        tmp=tmp_path_factory.mktemp("principal-mda"),
        bridge_id=f"principal-mda-{secrets.token_hex(3)}",
        domain=MDA_DOMAIN,
    )
    try:
        yield spawned
    finally:
        spawned.cleanup()


def _raw(public_key) -> bytes:
    return public_key.public_bytes(serialization.Encoding.Raw, serialization.PublicFormat.Raw)


def _secret(private_key) -> bytes:
    return private_key.private_bytes(
        serialization.Encoding.Raw, serialization.PrivateFormat.Raw, serialization.NoEncryption()
    )


def _owner_secret(nest) -> bytes:
    return bytes(nest["user"]["signing_key"])


def _served_set_with_file(nest, tmp_path, name: str, files: dict[str, bytes]) -> int:
    """Arrangement: the owner creates ``name`` custody-first, serves it, and
    writes ``files`` into it through the apps' own engine — so each file's
    bytes and name rest sealed under the set's content key, exactly what an
    owner's app leaves. Returns the folder id."""
    secret = _owner_secret(nest)
    folder_id = fauna_ffi.harness_create_set(nest["url"], secret, {"name": name})["id"]
    fauna_ffi.harness_serve_set(nest["url"], secret, name, True)
    if files:
        watch = tmp_path / f"{name}-watch"
        watch.mkdir()
        for rel, body in files.items():
            (watch / rel).write_bytes(body)
        recorded, pending = fauna_ffi.harness_sync_pass(
            nest["url"], secret, f"local:{folder_id}", watch, tmp_path / f"{name}-state",
            OWNER_DEVICE,
        )
        assert sorted(recorded) == sorted(files) and pending == 0, (
            f"the owner's engine wrote every file into the served set: {recorded!r}, "
            f"{pending} pending"
        )
    return folder_id


def _client(metadata_server, nest, scope: str):
    """A device-form principal (a public client) whose document declares the
    bare folder-read scope, asking for ``scope``, with a fresh attested X25519
    holder key. Returns the client and the holder's private key."""
    metadata_server.documents[DOCUMENT] = client_document(
        metadata_server.client_id(DOCUMENT), REDIRECT_URI, BARE_SCOPE
    )
    holder = x25519.X25519PrivateKey.generate()
    client = OAuthClient(
        base=nest["url"],
        htu_origin=f"https://{HANDLE_DOMAIN}",
        redirect_uri=REDIRECT_URI,
        scope=scope,
        holder_x25519=_raw(holder.public_key()),
        client_id_url=metadata_server.client_id(DOCUMENT),
    )
    return client, holder


def _pending_consent(nest, client) -> tuple[dict, str]:
    """PAR → the browser page → the owner's pending row for it. Returns the
    row and the flow token the client polls."""
    request_uri = client.push_authorization_request()
    browser_code, flow_token = client.open_consent_page(request_uri)
    with alice_client(nest) as ws:
        pending = ws.call("fauna.bridges.atproto.list_pending_consents", {})["consents"]
    consent = next((c for c in pending if c["code"] == browser_code), None)
    assert consent, f"no pending consent carries {browser_code!r}: {pending!r}"
    return consent, flow_token


def _approve(nest, client, folder: int | None) -> dict:
    """The owner approves through the production path (``answer_consent``),
    sending ``folder`` as the card's choice, and the client redeems the code.
    Returns the token reply."""
    consent, flow_token = _pending_consent(nest, client)
    answer = fauna_ffi.harness_answer_consent(
        nest["url"], _owner_secret(nest), bytes(consent["consent_id"]), folder
    )
    assert answer == {"resolved": True}, f"the approve minted and resolved: {answer!r}"
    redirect = ""
    deadline = time.monotonic() + RPC_ROUNDTRIP_S
    while time.monotonic() < deadline:
        status, redirect = client.poll(flow_token)
        if status == "resolved":
            break
    assert redirect and "code=" in redirect, f"approval must release a code: {redirect!r}"
    tokens = client.exchange_code(redirect)
    assert tokens.get("access_token"), tokens
    return tokens


def _keys_header(nest, client, token: str, holder) -> str:
    """The principal's side of the twin: fetch its grants over its own
    session, open the one folder twin into the keys header."""
    session = PrincipalSession(upgrade(nest, client, token))
    try:
        ok, reply = session.call("fauna.capabilities.fetch", {})
    finally:
        session.ws.close()
    assert ok, reply
    grants = [bytes(g) for g in reply.get("grants", [])]
    assert len(grants) == 1, (
        f"a folder-only consent mints exactly the folder's twin, no consent grant: {reply!r}"
    )
    return fauna_ffi.folder_read_keys_header(grants[0], _secret(holder))


def _dav(mda, client, token: str, method: str, path: str, keys: str | None, depth="1"):
    """One bearer request to the MDA's DAV mount: ``Authorization: DPoP``, a
    fresh proof over the request's own URL, and the keys header when given;
    one ``use_dpop_nonce`` retry, the protocol's own. Returns ``(status,
    body)``."""
    origin = f"127.0.0.1:{mda.caldav_port}"
    htu = f"https://{origin}{path}"
    # The MDA's cert is self-signed for its mail domain; the door's subject is
    # the bearer admission, not the TLS chain.
    tls = ssl.create_default_context()
    tls.check_hostname = False
    tls.verify_mode = ssl.CERT_NONE

    def send():
        headers = {
            "Authorization": f"DPoP {token}",
            "DPoP": client.dpop_proof(method, htu, access_token=token),
        }
        if method == "PROPFIND":
            headers["Depth"] = depth
        if keys is not None:
            headers["Fauna-Folder-Keys"] = keys
        request = urllib.request.Request(htu, method=method, headers=headers)
        try:
            with urllib.request.urlopen(request, timeout=RPC_ROUNDTRIP_S, context=tls) as resp:
                return resp.status, dict(resp.headers), resp.read()
        except urllib.error.HTTPError as refused:
            return refused.code, dict(refused.headers), refused.read()

    status, headers, body = send()
    lowered = {k.lower(): v for k, v in headers.items()}
    if status == 401 and "use_dpop_nonce" in lowered.get("www-authenticate", ""):
        client.nonce = lowered.get(NONCE_HEADER)
        status, _, body = send()
    return status, body


def _hrefs(body: bytes) -> list[str]:
    return re.findall(r"<(?:\w+:)?href>([^<]*)</(?:\w+:)?href>", body.decode(errors="replace"))


@pytest.mark.feature("connected-apps")
def test_a_device_principal_reads_a_served_folder_over_dav_through_the_consent_path(
    consent_nest, consent_bridge, metadata_server, principal_mda, tmp_path
):
    nest = consent_nest
    marker = b"principal-read-plaintext-" + secrets.token_hex(8).encode()
    body = marker + b"\n" + secrets.token_bytes(4096)
    photos = _served_set_with_file(nest, tmp_path, "photos", {"report.bin": body})
    other = _served_set_with_file(nest, tmp_path, "elsewhere", {"other.bin": b"not yours"})

    client, holder = _client(metadata_server, nest, BARE_SCOPE)
    tokens = _approve(nest, client, photos)
    assert tokens.get("scope") == f"{BARE_SCOPE}:{photos}", (
        f"the ceremony qualified the bare scope with the card's choice: {tokens!r}"
    )
    token = tokens["access_token"]
    keys = _keys_header(nest, client, token, holder)

    # ── The read: listed at the root by id, its file byte-identical. ──
    status, listing = _dav(principal_mda, client, token, "PROPFIND", f"/webdav/{DAV_USER}/", keys)
    assert status == 207, f"the principal lists its folders: {status} {listing[:400]!r}"
    roots = _hrefs(listing)
    assert any(h.rstrip("/").endswith(f"/{photos}") for h in roots), (
        f"the scoped folder is listed at the root by its id: {roots!r}"
    )
    assert not any(h.rstrip("/").endswith(f"/{other}") for h in roots), (
        f"a folder outside the scope is not listed, served or not: {roots!r}"
    )
    status, listing = _dav(
        principal_mda, client, token, "PROPFIND", f"/webdav/{DAV_USER}/{photos}/", keys
    )
    assert status == 207 and any(h.endswith("/report.bin") for h in _hrefs(listing)), (
        f"the owner's file is listed under its unsealed name: {status} {listing[:600]!r}"
    )
    status, got = _dav(
        principal_mda, client, token, "GET", f"/webdav/{DAV_USER}/{photos}/report.bin", keys
    )
    assert status == 200 and got == body, (
        f"the owner's file reads back byte-identical: {status}, {len(got)} vs {len(body)} bytes"
    )

    # ── The same token reaches no other folder, and opens nothing keyless. ──
    status, _ = _dav(principal_mda, client, token, "PROPFIND", f"/webdav/{DAV_USER}/{other}/", keys)
    assert status == 404, f"the token cannot list a second folder: {status}"
    status, _ = _dav(
        principal_mda, client, token, "GET", f"/webdav/{DAV_USER}/{other}/other.bin", keys
    )
    assert status == 404, f"nor read one: {status}"
    status, got = _dav(
        principal_mda, client, token, "GET", f"/webdav/{DAV_USER}/{photos}/report.bin", None
    )
    assert status != 200 and marker not in got, (
        f"the token without the keys opens no byte: {status}"
    )

    # ── The owner unserves: the next request is refused. ──
    fauna_ffi.harness_serve_set(nest["url"], _owner_secret(nest), "photos", False)
    status, got = _dav(
        principal_mda, client, token, "GET", f"/webdav/{DAV_USER}/{photos}/report.bin", keys
    )
    assert status == 404 and marker not in got, (
        f"after the unserve the principal's next read is refused: {status}"
    )
    status, listing = _dav(principal_mda, client, token, "PROPFIND", f"/webdav/{DAV_USER}/", keys)
    assert not any(h.rstrip("/").endswith(f"/{photos}") for h in _hrefs(listing)), (
        f"nor is the folder listed any more: {status} {listing[:400]!r}"
    )


@pytest.mark.feature("connected-apps")
def test_a_qualified_request_against_the_bare_declaration_reads_without_a_choice(
    consent_nest, consent_bridge, metadata_server, principal_mda, tmp_path
):
    """The re-consent form: a request naming one folder's qualified string
    narrows the document's bare declaration, so PAR admits it, the approve
    takes no choice, and the principal reads as before."""
    nest = consent_nest
    body = b"re-consent-read-" + secrets.token_bytes(512)
    folder = _served_set_with_file(nest, tmp_path, "re-consent", {"note.bin": body})
    scope = f"{BARE_SCOPE}:{folder}"
    client, holder = _client(metadata_server, nest, scope)
    tokens = _approve(nest, client, None)
    assert tokens.get("scope") == scope, tokens
    token = tokens["access_token"]
    keys = _keys_header(nest, client, token, holder)
    status, got = _dav(
        principal_mda, client, token, "GET", f"/webdav/{DAV_USER}/{folder}/note.bin", keys
    )
    assert status == 200 and got == body, f"{status}, {len(got)} vs {len(body)} bytes"


def _issuer(nest) -> str:
    import json

    with urllib.request.urlopen(
        f"{nest['url']}/.well-known/oauth-authorization-server", timeout=RPC_ROUNDTRIP_S
    ) as resp:
        return json.loads(resp.read())["issuer"]


@pytest.mark.feature("connected-apps")
@pytest.mark.parametrize("qualified", [False, True], ids=["bare", "qualified"])
def test_a_confidential_client_is_refused_the_folder_read_scope_at_the_push(
    consent_nest, consent_bridge, metadata_server, qualified
):
    """The form gate: a remote-form principal (a confidential client) is never
    granted a folder read — no DPoP device, no DAV door — so its push is
    ``invalid_scope``, bare or qualified (``authorization-server.md``
    § Scope grammar → *The read arm*)."""
    nest = consent_nest
    with alice_client(nest) as ws:
        folder = ws.call("fauna.folders.create", {"name": f"remote-{int(qualified)}"})["id"]
    scope = f"{BARE_SCOPE}:{folder}" if qualified else BARE_SCOPE
    # One document per case: the nest caches a fetched document, so a second
    # client key at the same URL would fail the assertion, not the scope.
    document = f"/folder-read-remote-{'qualified' if qualified else 'bare'}.json"
    client = OAuthClient(
        base=nest["url"],
        htu_origin=f"https://{HANDLE_DOMAIN}",
        redirect_uri=REDIRECT_URI,
        scope=scope,
        holder_x25519=_raw(x25519.X25519PrivateKey.generate().public_key()),
        client_id_url=metadata_server.client_id(document),
        assertion_key=ec.generate_private_key(ec.SECP256R1()),
        assertion_kid="k1",
        issuer=_issuer(nest),
    )
    metadata_server.documents[document] = client_document(
        client.client_id, REDIRECT_URI, BARE_SCOPE, jwk=client.assertion_jwk()
    )
    status, body = client.push_authorization_request_expecting_refusal()
    assert status == 400 and body.get("error") == "invalid_scope", (
        f"a confidential client's push for the folder read is refused: {status} {body!r}"
    )


@pytest.mark.feature("connected-apps")
def test_the_approve_refuses_a_folder_the_owner_does_not_serve(
    consent_nest, consent_bridge, metadata_server
):
    """Rule (2): the twin is mintable only over a set the owner's custody
    marks served — the approve refuses an unserved folder with the card's
    reason before anything is deposited, and never flips the serve flag; the
    request stays pending for the owner to serve the folder or decline."""
    nest = consent_nest
    folder = fauna_ffi.harness_create_set(
        nest["url"], _owner_secret(nest), {"name": "not-served"}
    )["id"]
    client, _ = _client(metadata_server, nest, BARE_SCOPE)
    consent, _ = _pending_consent(nest, client)
    answer = fauna_ffi.harness_answer_consent(
        nest["url"], _owner_secret(nest), bytes(consent["consent_id"]), folder
    )
    assert "not served over WebDAV" in answer.get("refused", ""), (
        f"the planner refuses an unserved folder with the card's reason: {answer!r}"
    )
    with alice_client(nest) as ws:
        pending = ws.call("fauna.bridges.atproto.list_pending_consents", {})["consents"]
        folders = ws.call("fauna.folders.list", {})["folders"]
    assert any(c["consent_id"] == consent["consent_id"] for c in pending), (
        "a refused approve resolves nothing: the request is still pending"
    )
    row = next(f for f in folders if f["id"] == folder)
    assert not row.get("webdav_enabled"), f"the approve never serves the folder: {row!r}"
