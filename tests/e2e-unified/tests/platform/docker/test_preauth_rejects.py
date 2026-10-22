"""tier_4 e2e: every listener the acceptance nest exposes refuses a stranger's
bad first moves, witnessed on the real image.

The owner lifts the acceptance nest's firewall only after the pre-auth surface
review's fixes have landed *and* these refusals have run green against the image
that carries them (``docs/goal/architecture/deployment-threat-model.md`` § Concern
1a → *The pre-auth surface, reviewed 2026-10-17*, the recommendation paragraph).
Until now the refusals were witnessed piecemeal — ``test_mail_security_relay.py``
for the relay and the submission gate, ``test_mail_abuse_refused.py`` (tier_3),
the G1 port probe — so no single suite could be pointed at for "what does a
stranger get on each port". This module is that suite: one raw-socket client per
listener, one test per reject, each naming the bound it observes.

What it dials, all from outside the container, through the published ports:

  * **tcp 443** — the ``fauna-sni-router`` (an L4 splice) fronting the nest.
    The nest's anonymous WS-RPC plane (``GET /api/v1/ws``), its federation
    channel (``/api/v1/federation/ws``) and the ActivityPub inbox.
  * **tcp 80** — the nest's ACME HTTP-01 listener (container port 8080).
  * **tcp 25 / 465 / 587 / 993** — the Go MTA and MDA.
  * **udp 7842** — the relay's QUIC address discovery.

**Rejects that wait on a fix are ``xfail``**, named by the row that lands the
fix; the ``strict`` ones go red — telling their owner to drop the marker — the
moment the fix lands: the HTTP-01 listener's foreign-``Host`` and connection-cap
behaviour, the ActivityPub verify-before-fetch (not strict: an SSRF guard that refuses the
dial would also pass it) and the ``/internal/router-status`` loopback gate.

**Not covered, and why.** The IMAP pre-auth *idle* bound is ours, not the
vendored fork's: 60 s from accept to a successful authentication
(`connlimit.PreAuthDeadline`, `docs/goal/behavior/smtp-server.md` § Connection-time
limits → *Pre-auth connection deadline*), pinned in-process by the Go witnesses
`internal/connlimit/preauth_test.go` and `internal/mda/imap/preauth_test.go` with
an injected clock. A test here that sleeps to see the close would be defunct by
convention 14, so none is added.
"""

import base64
import contextlib
import http.client
import json
import secrets
import smtplib
import socket
import ssl
import struct
import subprocess
import threading
import time

import cbor2
import pytest

from .helpers import (
    IMAGE_TAG,
    bridge_diag,
    bring_bridges_to_serving,
    claim_admin_api,
    docker_build,
    find_free_ports,
    get_repo_root,
    node_id,
    provision_mail_recipient,
    quic_version_negotiation,
    register_primary_domain,
    relax_spam_policy,
    remove_container,
    start_container_with_ports,
    wait_for_health,
)

try:
    subprocess.run(["docker", "info"], capture_output=True, timeout=10)
    HAS_DOCKER = True
except Exception:
    HAS_DOCKER = False

pytestmark = [
    pytest.mark.skipif(not HAS_DOCKER, reason="Docker not available"),
    pytest.mark.tier_4,
]

DOMAIN = "localhost"
CLAIM_CODE = "PREAUT"
RECIPIENT_LOCAL = "stranger-target"
RECIPIENT_PASSWORD = "preauth-rejects-password-1"
DISCOVERY_PORT = 7842
AP_DOMAIN = "ap.preauth.test"

# A resolvable sender domain: the nest's sender-domain check has no loopback
# exemption, and the peer here is the network gateway. `--add-host` makes the
# reserved `.test` name resolve inside the container.
SENDER_HOST = "probe.test"
SENDER = f"sender@{SENDER_HOST}"

# `Frame` envelope keys and the Request/Reply tags (`libs/fauna-protocol`), as the
# harness's own WS-RPC client writes them (`clients/_ws_rpc_core.py::_attempt`).
_FRAME_REQUEST = 0
_FRAME_REPLY = 1


# ── Fixture ───────────────────────────────────────────────────────────


@pytest.fixture(scope="module")
def docker_image():
    docker_build(get_repo_root())
    yield IMAGE_TAG


@pytest.fixture(scope="module")
def nest(docker_image, run_seal_helper):
    """One claimed, serving container with every listener the acceptance nest
    exposes published: the SNI router's :443, the HTTP-01 listener's :8080 (a
    host port stands in for :80), the four mail ports, and the relay's udp 7842.
    The spam perimeter is relaxed so a peer arriving through docker-proxy (the
    network gateway, never loopback) reaches the envelope stage the tests probe;
    every refusal below is a *structural* one the perimeter does not gate."""
    api_port, http80_port, *mail = find_free_ports(6)
    mail_ports = dict(zip((25, 465, 587, 993), mail))
    udp_port = find_free_ports(1)[0]
    name = f"fauna-preauth-{api_port}"
    start_container_with_ports(
        name,
        {443: api_port, 8080: http80_port, **mail_ports},
        env={"FAUNA_CLAIM_CODE": CLAIM_CODE, "FAUNA_PORT": "3000"},
        add_hosts={SENDER_HOST: "host-gateway"},
        udp_port_map={DISCOVERY_PORT: udp_port},
    )
    try:
        wait_for_health(api_port, name)
        admin = claim_admin_api(api_port, CLAIM_CODE, handle="admin")
        handle = {
            "name": name,
            "port": api_port,
            "url": f"https://127.0.0.1:{api_port}",
            "admin": admin,
            "mail_ports": mail_ports,
            "router_port": api_port,
            "http80_port": http80_port,
            "udp_port": udp_port,
        }
        register_primary_domain(handle, DOMAIN)
        relax_spam_policy(handle)
        handle["recipient"] = provision_mail_recipient(
            handle, run_seal_helper, domain=DOMAIN,
            local_part=RECIPIENT_LOCAL, password=RECIPIENT_PASSWORD,
        )
        try:
            bring_bridges_to_serving(name, handle, mail_ports, DOMAIN)
        except (AssertionError, TimeoutError) as e:
            raise AssertionError(
                f"{e}\n\n── bridge diagnostics ──\n{bridge_diag(name)}") from e
        yield handle
    finally:
        remove_container(name)


# ── :443 — the nest behind the SNI router ─────────────────────────────


def _no_verify() -> dict:
    return {"cert_reqs": ssl.CERT_NONE, "check_hostname": False,
            "server_hostname": DOMAIN}


def _anon_ws(nest, subprotocols=("fauna.v1",)):
    """The anonymous WS-RPC upgrade, dialled through the router's :443 with the
    box's own name as SNI (what a client dialling ``https://<domain>`` sends)."""
    import websocket  # from websocket-client

    return websocket.create_connection(
        f"wss://127.0.0.1:{nest['router_port']}/api/v1/ws",
        subprotocols=list(subprotocols), sslopt=_no_verify(), timeout=15,
    )


def _request_frame(kind: str, payload=None, corr: int = 1) -> bytes:
    return cbor2.dumps(
        {0: _FRAME_REQUEST, 1: corr, 2: kind, 3: secrets.token_bytes(16),
         4: {} if payload is None else payload},
        canonical=True,
    )


def _close_code(ws, timeout: float = 20.0):
    """Read until the server's Close frame; its status code, or None when the
    socket dropped without one."""
    import websocket  # from websocket-client

    ws.settimeout(timeout)
    deadline = time.monotonic() + timeout
    while time.monotonic() < deadline:
        try:
            frame = ws.recv_frame()
        except websocket.WebSocketTimeoutException:
            continue
        except (websocket.WebSocketConnectionClosedException, OSError):
            return None
        if frame is None:
            continue
        if frame.opcode == websocket.ABNF.OPCODE_CLOSE:
            return struct.unpack("!H", frame.data[:2])[0] if len(frame.data) >= 2 else None
    raise AssertionError(f"no Close frame within {timeout}s")


def _reply(ws, timeout: float = 20.0) -> dict:
    ws.settimeout(timeout)
    frame = cbor2.loads(ws.recv())
    assert frame.get(0) == _FRAME_REPLY, f"expected a Reply frame, got {frame!r}"
    return frame


def _error_code(reply: dict):
    payload = reply.get(4)
    return payload.get("code") if isinstance(payload, dict) else None


def test_ws_text_frame_closes_the_connection(nest):
    """A text frame on the anonymous WS-RPC plane closes it 4400 (protocol
    violation): ``routes.rs::handle_ws_anonymous`` → ``Message::Text`` →
    ``FatalCloseReason::ProtocolViolation`` (``ws.rs``, code 4400)."""
    ws = _anon_ws(nest)
    try:
        ws.send("not binary")
        assert _close_code(ws) == 4400
    finally:
        ws.close()


def test_ws_binary_frame_over_2_mib_is_refused(nest):
    """A binary frame past ``MAX_RPC_WS_MESSAGE_SIZE`` (2 MiB, ``fauna-core``
    ``transport.rs``) is never answered: the upgrade's ``max_message_size`` /
    ``max_frame_size`` cut the connection before a decode."""
    import websocket  # from websocket-client

    ws = _anon_ws(nest)
    try:
        try:
            ws.send_binary(b"\x00" * (2 * 1024 * 1024 + 1))
        except (websocket.WebSocketException, OSError):
            return  # the nest already cut the socket mid-send: refused
        try:
            ws.settimeout(20)
            while True:
                frame = ws.recv_frame()
                assert frame.opcode != websocket.ABNF.OPCODE_BINARY, (
                    "the nest answered a 2 MiB+1 frame instead of refusing it")
                if frame.opcode == websocket.ABNF.OPCODE_CLOSE:
                    return
        except (websocket.WebSocketException, OSError):
            return
    finally:
        ws.close()


@pytest.mark.parametrize("offered,expect_status", [("other.v9", 426), ("fauna.v9", 101)],
                         ids=["not-a-fauna-subprotocol", "unserved-fauna-major"])
def test_ws_non_fauna_v1_subprotocol_is_refused(nest, offered, expect_status):
    """A subprotocol other than ``fauna.v1`` never reaches the dispatcher:
    ``classify_client_subprotocol`` answers a foreign token ``426`` and an
    unserved ``fauna.v<N>`` an upgrade that closes 4426
    (``upgrade_and_close_subprotocol_mismatch``)."""
    import websocket  # from websocket-client

    if expect_status == 426:
        with pytest.raises(websocket.WebSocketBadStatusException) as e:
            _anon_ws(nest, subprotocols=(offered,))
        assert e.value.status_code == 426
    else:
        ws = _anon_ws(nest, subprotocols=(offered,))
        try:
            assert _close_code(ws) == 4426
        finally:
            ws.close()


def test_ws_off_allowlist_kind_is_unauthenticated(nest):
    """A kind outside the pre-identity allowlist is answered
    ``fauna.protocol.unauthenticated`` (``pre_identity_allowlist.rs``) and the
    connection stays usable for the next frame."""
    ws = _anon_ws(nest)
    try:
        ws.send_binary(_request_frame("fauna.feed.list", corr=1))
        first = _reply(ws)
        assert not first.get(7)
        assert _error_code(first) == "fauna.protocol.unauthenticated"
        ws.send_binary(_request_frame("fauna.setup.status", corr=2))
        second = _reply(ws)
        assert second.get(1) == 2 and second.get(7), "the connection did not survive the refusal"
    finally:
        ws.close()


def test_ws_claim_admin_is_throttled_per_source(nest):
    """``fauna.auth.claim_admin`` is a credential guess against a short secret:
    10 attempts / 60 s per source IP (``anonymous_rate_limit.rs``). Past the cap
    the reply is ``fauna.protocol.rate_limited`` — before the handler runs."""
    codes = []
    # Several connections of a few frames each, so the per-connection frame
    # budget (30 burst, 10/s) is never what refuses.
    for _ in range(4):
        ws = _anon_ws(nest)
        try:
            for i in range(4):
                ws.send_binary(_request_frame(
                    "fauna.auth.claim_admin", {"claim_code": "WRONG"}, corr=i + 1))
                codes.append(_error_code(_reply(ws)))
        finally:
            ws.close()
    assert "fauna.protocol.rate_limited" in codes, codes
    assert codes[0] != "fauna.protocol.rate_limited", (
        f"the very first attempt was already throttled: {codes}")


def test_ws_frame_budget_closes_a_flooding_connection_1008(nest):
    """H2: an anonymous connection sending frames faster than the pre-identity
    frame budget (burst 30, 10/s — ``routes.rs::PRE_IDENTITY_FRAMES_PER_SECOND``,
    ``PRE_IDENTITY_FRAME_BURST``) is closed 1008 (policy violation,
    ``FatalCloseReason::PreIdentityFrameBudget``). The frames are well-formed and
    off-allowlist, so the close is the budget's, not a decode failure's."""
    import websocket  # from websocket-client

    ws = _anon_ws(nest)
    try:
        try:
            for i in range(300):
                ws.send_binary(_request_frame("fauna.feed.list", corr=i + 1))
        except (websocket.WebSocketException, OSError):
            pass  # the nest closed under the flood: read the close below
        ws.settimeout(20)
        code = None
        while True:
            try:
                frame = ws.recv_frame()
            except (websocket.WebSocketException, OSError):
                break
            if frame is None:
                break
            if frame.opcode == websocket.ABNF.OPCODE_CLOSE:
                code = struct.unpack("!H", frame.data[:2])[0] if len(frame.data) >= 2 else None
                break
        assert code == 1008, f"close code {code!r}, want 1008"
    finally:
        ws.close()


def test_challenge_issue_is_bounded_per_source(nest):
    """H2: ``fauna.auth.challenge`` mints an in-memory entry per call for any
    valid Ed25519 key. Two bounds sit in front of the store, and both answer
    ``fauna.protocol.rate_limited``: the dispatcher's anonymous per-source
    throttle (60/min, ``anonymous_rate_limit.rs``) and the store's own
    ``CHALLENGE_PER_SOURCE_CAP`` (128 outstanding, ``challenge_auth.rs``). From
    outside they are one observable, and the throttle binds first inside a
    minute, so this witnesses the COMBINED bound: 130 issues from one source are
    not all granted, and no more than 128 ever are. The cap's own edge is proven
    at a small cap by ``bins/fauna-nest/tests/pre_identity_budget.rs``; reaching
    it from the wire would need minutes of throttle windows, a wall-clock test."""
    import nacl.signing

    refused = 0
    issued = 0
    for _ in range(26):  # 26 connections × 5 frames
        ws = _anon_ws(nest)
        try:
            for i in range(5):
                pub = bytes(nacl.signing.SigningKey.generate().verify_key)
                ws.send_binary(_request_frame(
                    "fauna.auth.challenge", {"public_key": pub}, corr=i + 1))
                if _reply(ws).get(7):
                    issued += 1
                else:
                    refused += 1
        finally:
            ws.close()
    assert refused >= 1, f"130 challenges all issued ({issued}); no per-source bound applied"
    assert issued <= 128, f"{issued} challenges issued past the 128 cap"


@pytest.mark.xfail(strict=True, reason="waits on the loopback gate: /internal/router-status has no loopback gate yet")
def test_router_status_is_refused_from_a_non_loopback_source(nest):
    """L1: ``/internal/router-status`` serves the exact version and the user
    counts to any internet client today. Ruled loopback-only, fail-closed through
    the shared internal gate; a request through the published port arrives from
    the network gateway, never loopback, so it must not be a 200."""
    conn = http.client.HTTPSConnection(
        "127.0.0.1", nest["port"], timeout=15, context=ssl._create_unverified_context())
    conn.request("GET", "/internal/router-status")
    status = conn.getresponse().status
    conn.close()
    assert status in (401, 403, 404), status


def _count_shed(port: int, opened: int = 300, window: float = 6.0) -> tuple[int, int]:
    """Open `opened` idle sockets from this one source and count how many the
    server closes inside `window`. Returns (shed, opened)."""
    socks = []
    try:
        for _ in range(opened):
            s = socket.create_connection(("127.0.0.1", port), timeout=10)
            s.setblocking(False)
            socks.append(s)
        shed = 0
        deadline = time.monotonic() + window
        while time.monotonic() < deadline and shed == 0:
            for s in socks:
                try:
                    if s.recv(1) == b"":
                        shed += 1
                except BlockingIOError:
                    pass
                except OSError:
                    shed += 1
            time.sleep(0.1)
        return shed, len(socks)
    finally:
        for s in socks:
            with contextlib.suppress(OSError):
                s.close()


def _wait_router_recovers(port: int, timeout: float = 60.0) -> None:
    """Block until a fresh TLS dial through the router completes: the 300 closed
    sockets of the cap test are still counted against this source until the
    router has reaped them, and the next test must not meet that residue."""
    ctx = ssl._create_unverified_context()
    deadline = time.monotonic() + timeout
    while True:
        try:
            with socket.create_connection(("127.0.0.1", port), timeout=10) as raw:
                with ctx.wrap_socket(raw, server_hostname=DOMAIN):
                    return
        except OSError:
            if time.monotonic() > deadline:
                raise
            time.sleep(0.5)


def test_sni_router_sheds_connections_past_the_per_ip_cap(nest):
    """The router caps connections per source IP at 256
    (``fauna-conn-limit::DEFAULT_MAX_CONNS_PER_IP``, ``--max-connections-per-ip``
    in ``fauna-sni-router``). Open 300 idle sockets from this one source (via
    docker-proxy, one address); the router closes the surplus at accept — before
    its 10 s ClientHello deadline could close anything — so some sockets read EOF
    inside the window."""
    shed, opened = _count_shed(nest["router_port"])
    _wait_router_recovers(nest["router_port"])
    assert shed >= 1, "no connection was shed with 300 open from one source"
    assert shed < opened, "every connection was closed: the cap is not per-excess"


# ── :443 federation ───────────────────────────────────────────────────


def _federation_ws(nest):
    import websocket  # from websocket-client

    return websocket.create_connection(
        f"wss://127.0.0.1:{nest['router_port']}/api/v1/federation/ws",
        subprotocols=["fauna.federation.v1"], sslopt=_no_verify(), timeout=15,
    )


def test_federation_connection_with_no_hello_is_closed(nest):
    """The listener waits ``HANDSHAKE_DEADLINE`` (10 s,
    ``federation_channel.rs``) for the first-frame ``fauna.federation.hello``
    and closes a channel that sends nothing. Waiting for the close is a bounded
    poll on an event, not a sleep the assertion depends on."""
    ws = _federation_ws(nest)
    try:
        _close_code(ws, timeout=40.0)
    finally:
        ws.close()


def test_federation_hello_signed_by_another_key_is_refused(nest):
    """A hello naming one nest id but signed by a different key fails the
    sign-over-CID verify (``federation_channel.rs::verify_hello_and_build_reply``)
    and tears the channel down: no Reply is ever ok."""
    import nacl.signing
    import websocket  # from websocket-client

    from common.envelope import FEDERATION_HELLO_V1, sign_dagcbor_envelope
    from helpers.tls_spki import observed_spki_sha256_hex

    ws = _federation_ws(nest)
    try:
        spki = observed_spki_sha256_hex(getattr(ws, "sock", None))
        named = nacl.signing.SigningKey.generate()
        signer = nacl.signing.SigningKey.generate()
        initiator = bytes(named.verify_key).hex()
        nonce = secrets.token_bytes(32).hex()
        envelope = sign_dagcbor_envelope(
            {"initiator_nest_id": initiator,
             "listener_nest_id": node_id(nest).hex(),
             "channel_nonce": nonce, "spki_sha256": spki},
            signer, domain_tag=FEDERATION_HELLO_V1,
        )
        ws.send_binary(_request_frame("fauna.federation.hello", {
            "initiator_nest_id": initiator, "channel_nonce": nonce,
            "spki_sha256": spki, "envelope": envelope,
        }))
        try:
            ws.settimeout(20)
            raw = ws.recv()
        except (websocket.WebSocketException, OSError):
            return  # the channel was torn down without a reply: refused
        if isinstance(raw, (bytes, bytearray)):
            frame = cbor2.loads(raw)
            assert frame.get(0) == _FRAME_REPLY and not frame.get(7), frame
    finally:
        ws.close()


# ── :443 ActivityPub ──────────────────────────────────────────────────


def _unsigned_inbox_post(nest, body: bytes) -> int:
    """POST to the shared inbox through the router's :443 with the box's own
    name as SNI and no HTTP signature."""
    ctx = ssl._create_unverified_context()
    raw = socket.create_connection(("127.0.0.1", nest["router_port"]), timeout=20)
    conn = http.client.HTTPSConnection(nest["sni"], 443, timeout=20, context=ctx)
    conn.sock = ctx.wrap_socket(raw, server_hostname=nest["sni"])
    try:
        conn.request("POST", "/ap/inbox", body=body,
                     headers={"Content-Type": "application/activity+json"})
        resp = conn.getresponse()
        resp.read()
        return resp.status
    finally:
        conn.close()


@pytest.fixture(scope="module")
def ap_nest(docker_image):
    """A second, DOMAINED nest. The inbox answers 404 "ActivityPub not
    configured" on a domainless box (`inbox_routes.rs::process_inbox`), so on the
    main fixture both inbox tests would be vacuous. A claim carrying a mail domain
    makes that domain the deployment identity (`claim_admin_api`'s
    ``mail_domain``); only the router's :443 is published."""
    (api_port,) = find_free_ports(1)
    name = f"fauna-preauth-ap-{api_port}"
    start_container_with_ports(
        name, {443: api_port},
        env={"FAUNA_CLAIM_CODE": CLAIM_CODE, "FAUNA_PORT": "3000"},
        add_hosts={SENDER_HOST: "host-gateway"},
    )
    try:
        wait_for_health(api_port, name)
        claim_admin_api(api_port, CLAIM_CODE, handle="admin", mail_domain=AP_DOMAIN)
        yield {"name": name, "port": api_port, "router_port": api_port,
               "sni": AP_DOMAIN}
    finally:
        remove_container(name)


def test_activitypub_unsigned_inbox_post_is_401(ap_nest):
    nest = ap_nest
    """An inbox POST with no HTTP signature is refused 401
    (``activitypub/inbox_routes.rs``)."""
    activity = {"@context": "https://www.w3.org/ns/activitystreams", "type": "Follow",
                "actor": "https://remote.example/actor", "object": f"https://{AP_DOMAIN}/ap/users/x"}
    assert _unsigned_inbox_post(nest, json.dumps(activity).encode()) == 401


@pytest.mark.xfail(strict=False, reason="waits on verify-before-fetch: the inbox fetches the actor its body names before verifying the signature")
def test_activitypub_unsigned_inbox_post_makes_no_outbound_fetch(ap_nest):
    nest = ap_nest
    """M1: an unsigned POST naming an actor URL must not make the nest dial that
    URL. The actor URL points at a listener this test runs on the host, reachable
    from the container as ``probe.test``; any request it sees is the nest
    fetching before it verified."""
    hits = []
    server = socket.socket(socket.AF_INET, socket.SOCK_STREAM)
    server.setsockopt(socket.SOL_SOCKET, socket.SO_REUSEADDR, 1)
    server.bind(("0.0.0.0", 0))
    server.listen(8)
    server.settimeout(0.5)
    port = server.getsockname()[1]
    stop = threading.Event()

    def accept():
        while not stop.is_set():
            try:
                conn, _ = server.accept()
            except (socket.timeout, OSError):
                continue
            hits.append(conn.recv(4096))
            conn.close()

    t = threading.Thread(target=accept, daemon=True)
    t.start()
    try:
        activity = {"@context": "https://www.w3.org/ns/activitystreams", "type": "Follow",
                    "actor": f"http://{SENDER_HOST}:{port}/actor",
                    "object": f"https://{AP_DOMAIN}/ap/users/x"}
        _unsigned_inbox_post(nest, json.dumps(activity).encode())
        # The fetch, if any, is issued before the 401 is written: by the time
        # the response is read, the listener has already seen it.
    finally:
        stop.set()
        t.join(timeout=3)
        server.close()
    assert not hits, f"the nest fetched the unverified actor URL: {hits[0][:120]!r}"


# ── :80 — the ACME HTTP-01 listener ───────────────────────────────────


def _http80(nest, path: str, host: str | None):
    conn = http.client.HTTPConnection("127.0.0.1", nest["http80_port"], timeout=15)
    conn.putrequest("GET", path, skip_host=True, skip_accept_encoding=True)
    if host is not None:
        conn.putheader("Host", host)
    conn.endheaders()
    resp = conn.getresponse()
    status, location = resp.status, resp.getheader("Location")
    resp.read()
    conn.close()
    return status, location


def test_http80_unknown_acme_token_is_404(nest):
    """``GET /.well-known/acme-challenge/<unknown>`` → 404
    (``fauna-acme-http01`` ``handle_challenge``)."""
    status, _ = _http80(nest, f"/.well-known/acme-challenge/{secrets.token_hex(16)}", DOMAIN)
    assert status == 404


def test_http80_without_host_is_400_on_a_domainless_nest(nest):
    """No ``Host`` and no domain of its own to fall back to → 400
    (``http01_router``'s fallback). Asserted only because this fixture's nest
    boots domainless: a domained nest falls back to its own name and redirects."""
    status, _ = _http80(nest, "/anything", None)
    assert status == 400


@pytest.mark.xfail(strict=True, reason="waits on the :80 fix: the fallback reflects the Host header into a 308")
def test_http80_foreign_host_gets_404_and_never_a_redirect(nest):
    """M2: only a host this nest serves is ever redirected to; any other ``Host``
    is a 404 with no ``Location`` (ruled)."""
    status, location = _http80(nest, "/anything", "attacker.example")
    assert location is None and status == 404, (status, location)


@pytest.mark.xfail(strict=True, reason="waits on the :80 fix: :80 is a bare axum::serve with no connection cap")
def test_http80_sheds_connections_past_the_per_ip_cap(nest):
    """M2: once :80 is served through the admitted accept loop it sheds a source's
    connections past the per-IP cap (256, ``fauna-conn-limit``)."""
    shed, _ = _count_shed(nest["http80_port"])
    assert shed >= 1


# ── :25 ───────────────────────────────────────────────────────────────


def _smtp_tls_session(port: int) -> smtplib.SMTP:
    ctx = ssl._create_unverified_context()
    # Constructed with the host so smtplib has a name for `server_hostname`.
    client = smtplib.SMTP("127.0.0.1", port, timeout=30)
    client.ehlo(SENDER_HOST)
    client.starttls(context=ctx)
    client.ehlo(SENDER_HOST)
    return client


def _mail_from_ok(client: smtplib.SMTP) -> None:
    code, text = client.docmd("MAIL", f"FROM:<{SENDER}>")
    assert code == 250, f"MAIL FROM was not accepted: {code} {text!r}"


def test_port25_mail_from_before_starttls_is_530(nest):
    """``MAIL FROM`` on a connection that has not run STARTTLS is refused 530
    (``mta/server.go`` ``requireStartTLS``)."""
    client = smtplib.SMTP(timeout=30)
    try:
        client.connect("127.0.0.1", nest["mail_ports"][25])
        client.ehlo(SENDER_HOST)
        code, _ = client.docmd("MAIL", f"FROM:<{SENDER}>")
        assert code == 530
    finally:
        client.close()


def test_port25_recipient_cap_is_452_past_100(nest):
    """A transaction past ``srv.MaxRecipients = 100`` (``mta/server.go``) gets
    452 on the 101st ``RCPT TO``."""
    rcpt = nest["recipient"]["username"]
    client = _smtp_tls_session(nest["mail_ports"][25])
    try:
        _mail_from_ok(client)
        codes = [client.docmd("RCPT", f"TO:<{rcpt}>")[0] for _ in range(101)]
        assert codes[:100] == [250] * 100, codes[:100]
        assert codes[100] == 452, codes[100]
    finally:
        client.close()


def test_port25_data_over_the_size_cap_is_552(nest):
    """A message body one byte past the advertised ``SIZE`` is refused 552
    (``mta/server.go`` ``Data``: the ``maxMessageBytes+1`` read, then 552)."""
    rcpt = nest["recipient"]["username"]
    client = _smtp_tls_session(nest["mail_ports"][25])
    try:
        size = int(client.esmtp_features["size"])
        assert size <= 128 * 1024 * 1024, f"advertised cap {size} is too large to stream in a test"
        _mail_from_ok(client)
        assert client.docmd("RCPT", f"TO:<{rcpt}>")[0] == 250
        assert client.docmd("DATA")[0] == 354
        chunk = (b"x" * 998 + b"\r\n") * 1024
        sent = 0
        while sent <= size:
            client.sock.sendall(chunk)
            sent += len(chunk)
        client.sock.sendall(b"\r\n.\r\n")
        code, _ = client.getreply()
        assert code == 552
    finally:
        client.close()


def test_port25_unknown_recipient_is_550(nest):
    """A local-domain recipient that does not exist is refused 550 after the
    escalating tarpit (``mta/fauna_recipient.go``, ``server.go`` ``reject``)."""
    client = _smtp_tls_session(nest["mail_ports"][25])
    try:
        _mail_from_ok(client)
        code, _ = client.docmd("RCPT", f"TO:<nobody-{secrets.token_hex(4)}@{DOMAIN}>")
        assert code == 550
    finally:
        client.close()


# ── :465 / :587 ───────────────────────────────────────────────────────


def _plain(user: str, password: str) -> str:
    return base64.b64encode(f"\0{user}\0{password}".encode()).decode()


def test_port587_auth_before_tls_is_refused(nest):
    """``AllowInsecureAuth = false`` on the submission listeners: AUTH on a 587
    connection that has not run STARTTLS is a 5xx, never a 235."""
    client = smtplib.SMTP(timeout=30)
    try:
        client.connect("127.0.0.1", nest["mail_ports"][587])
        client.ehlo(SENDER_HOST)
        code, _ = client.docmd("AUTH", f"PLAIN {_plain('admin', 'x')}")
        assert 500 <= code < 600, code
    finally:
        client.close()


def test_port587_unknown_sasl_mechanism_is_504(nest):
    """Only PLAIN and OAUTHBEARER are offered (``mta/submission.go``); any other
    mechanism is 504 5.5.4."""
    client = _smtp_tls_session(nest["mail_ports"][587])
    try:
        code, _ = client.docmd("AUTH", "CRAM-MD5")
        assert code == 504
    finally:
        client.close()


def test_port587_wrong_passwords_hit_a_lockout_that_never_says_why(nest):
    """The 31st wrong password from one source inside a minute crosses the
    30/min lockout (``mta/submission.go``, the per-credential bucket). Up to it
    every reply is the same 535; past it the reply is a 421 "try again later" —
    a transient failure that names no credential, so the lockout never confirms
    or denies a guess, and no wrong password is ever a 235."""
    user = nest["recipient"]["username"]
    replies = []
    for _ in range(31):
        client = _smtp_tls_session(nest["mail_ports"][587])
        try:
            replies.append(client.docmd(
                "AUTH", f"PLAIN {_plain(user, 'wrong-' + secrets.token_hex(4))}"))
        finally:
            client.close()
    codes = [c for c, _ in replies]
    assert codes[:30] == [535] * 30, codes
    assert len({t for _, t in replies[:30]}) == 1, "wrong-password replies differ from each other"
    assert codes[30] == 421, replies[30]


# ── :993 ──────────────────────────────────────────────────────────────


class _Imap:
    """A raw IMAPS client: a pre-auth conversation, line by line."""

    def __init__(self, port: int):
        raw = socket.create_connection(("127.0.0.1", port), timeout=20)
        ctx = ssl._create_unverified_context()
        self.sock = ctx.wrap_socket(raw, server_hostname=DOMAIN)
        self.sock.settimeout(20)
        self.buf = b""
        self.greeting = self.line()

    def line(self) -> bytes:
        while b"\r\n" not in self.buf:
            chunk = self.sock.recv(4096)
            if not chunk:
                raise EOFError("server closed the connection")
            self.buf += chunk
        out, self.buf = self.buf.split(b"\r\n", 1)
        return out

    def send(self, data: bytes) -> None:
        self.sock.sendall(data)

    def until_tagged(self, tag: str) -> bytes:
        while True:
            ln = self.line()
            if ln.startswith(tag.encode() + b" "):
                return ln

    def close(self) -> None:
        with contextlib.suppress(OSError):
            self.sock.close()


def test_imap_wrong_login_fails_opaquely(nest):
    """A wrong password for a real user and any password for an unknown user get
    the same tagged ``NO`` (``mda/imap/auth.go``: lockout, then the dummy KDF for
    an unknown user, then Argon2id): the reply does not say which."""
    port = nest["mail_ports"][993]
    replies = []
    for user in (nest["recipient"]["username"], f"nobody-{secrets.token_hex(4)}@{DOMAIN}"):
        c = _Imap(port)
        try:
            c.send(f'a1 LOGIN "{user}" "wrong-password"\r\n'.encode())
            replies.append(c.until_tagged("a1"))
        finally:
            c.close()
    assert all(r.startswith(b"a1 NO") for r in replies), replies
    assert replies[0] == replies[1], f"the reply tells a real user from an unknown one: {replies}"


def test_imap_pre_auth_literal_is_not_accepted_unbounded(nest):
    """A pre-auth ``LOGIN`` whose first argument is a 4 GiB synchronising literal
    is not granted a continuation: the server must answer a tagged ``NO``/``BAD``
    or close, never ``+`` (the vendored fork's literal bound, pinned from outside
    because the fork's source is unread by rule)."""
    c = _Imap(nest["mail_ports"][993])
    try:
        c.send(b"a1 LOGIN {4294967295}\r\n")
        try:
            ln = c.line()
        except (EOFError, OSError, ssl.SSLError):
            return  # closed: refused
        assert not ln.startswith(b"+"), f"the server asked for a 4 GiB pre-auth literal: {ln!r}"
    finally:
        c.close()


# ── udp 7842 ──────────────────────────────────────────────────────────


def test_udp_7842_unsolicited_datagram_draws_only_version_negotiation(nest):
    """An unsolicited 1200-byte QUIC datagram with a reserved version draws at
    most the Version Negotiation packet — 55 bytes as recorded in the review
    (``deployment-threat-model.md`` § Concern 1a) — and no handshake: the reply
    is far smaller than the datagram (no amplification) and offers versions."""
    got = quic_version_negotiation("127.0.0.1", nest["udp_port"])
    assert got is not None, "no Version Negotiation: the discovery port is not answering"
    versions, reply_len = got
    assert reply_len <= 128, f"reply of {reply_len} bytes to a 1200-byte probe"
    assert versions, "a Version Negotiation packet names at least one version"


# ── :25 — last: it spends the port's per-IP connection rate ───────────


def test_port25_connection_flood_is_421(nest):
    """A source past the per-IP connection rate is answered 421 4.7.0 at
    session open, before any mail command (``mta/server.go`` ``NewSession`` →
    ``RateLimiter.Allow``). The limit is the admin policy's ``max_conn_per_min``;
    the fixture's 1000 is lowered to 5 so a short burst crosses it. A saved
    policy reaches the running bridge on its next config push, so the burst is a
    bounded poll, not a sleep. **Keep this test last in the module**: it spends
    the port's connection rate for the rest of the minute, and the policy is put
    back at the end."""
    from .helpers import admin_ws
    from .mail_perimeter import session_opening

    with admin_ws(nest) as admin:
        admin.call("fauna.bridges.put_spam_policy", {
            "dnsbl_servers": [], "greylist_enabled": False, "greylist_delay_secs": 0,
            "fcrdns_mode": "off", "max_conn_per_min": 5,
            "baseline_standing_publish": False,
        })
    try:
        seen = []
        deadline = time.monotonic() + 90
        while time.monotonic() < deadline:
            reply = session_opening(nest["mail_ports"][25], helo=SENDER_HOST)
            seen.append(reply[:3])
            if reply.startswith("421"):
                break
            time.sleep(0.5)
        assert "421" in seen, f"no 421 within the poll; saw {seen[-8:]}"
    finally:
        relax_spam_policy(nest)
