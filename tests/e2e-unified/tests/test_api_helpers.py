"""API helper tests — verify nest endpoints directly without UI.

These run on all platforms (pure HTTP, no client dependency). They help
diagnose whether test failures are UI issues or API issues.
"""
import json
import os
import secrets
import urllib.request
import urllib.error

import pytest

pytestmark = [pytest.mark.tier1, pytest.mark.tier_3]


def _api(nest_instance, path, method="GET", body=None, token=None):
    """Make an API request to the test nest."""
    url = f"{nest_instance['url']}{path}"
    data = json.dumps(body).encode() if body else None
    headers = {"Content-Type": "application/json"}
    if token:
        headers["Authorization"] = f"Bearer {token}"
    req = urllib.request.Request(url, data=data, headers=headers, method=method)
    try:
        resp = urllib.request.urlopen(req, timeout=10)
        raw = resp.read()
        if not raw or not raw.strip():
            return {"_status": resp.status}
        return json.loads(raw)
    except urllib.error.HTTPError as e:
        return {"_error": e.code, "_body": e.read().decode()[:500]}


def test_health_endpoint(nest_instance):
    """Verify nest health endpoint returns expected format."""
    data = _api(nest_instance, "/api/v1/health")
    assert data["status"] == "ok"
    assert "version" in data


@pytest.mark.feature("connect-and-sign-in")
def test_auth_token(nest_instance, test_user):
    """Verify auth token can be obtained."""
    token = test_user["token"]
    assert len(token) > 0


def _ws_url(nest_instance, actor_hex):
    """Convert the nest's HTTP url to its WS equivalent."""
    base = nest_instance["url"]
    if base.startswith("https://"):
        ws = "wss://" + base[len("https://"):]
    elif base.startswith("http://"):
        ws = "ws://" + base[len("http://"):]
    else:
        raise ValueError(f"unexpected nest url scheme: {base}")
    return f"{ws}/api/v1/ws/{actor_hex}"


def test_protocol_echo_round_trip(nest_instance, test_user):
    """fauna.protocol.echo via real WebSocket — Spec Y end-to-end smoke test."""
    import cbor2
    import websocket  # from websocket-client

    url = _ws_url(nest_instance, test_user["actor_id_hex"])
    ws = websocket.create_connection(
        url,
        subprotocols=["fauna.v1", f"bearer.{test_user['token']}"],
        timeout=5,
    )
    try:
        # Build Request frame: integer-keyed CBOR map per Spec Y § 1.1.
        # Integer keys: 0=type, 1=correlation_id, 2=kind, 3=idempotency_key,
        # 4=payload, 5=replay_forbidden (omitted), 6=deadline_ms (omitted).
        idempotency_key = secrets.token_bytes(16)
        echo_payload = cbor2.dumps({"data": b"hello-spec-y"})
        # The server expects the payload to itself be CBOR-decodable as
        # EchoRequest; we encode the typed payload above and embed it as
        # the Request.4 field. Decode it back into a CborValue-equivalent
        # so the outer encode is canonical CBOR with embedded payload.
        request_frame = cbor2.dumps({
            0: 0,                                  # type = Request
            1: 1,                                  # correlation_id
            2: "fauna.protocol.echo",
            3: idempotency_key,
            4: cbor2.loads(echo_payload),          # payload (decoded so cbor2 re-encodes it)
        }, canonical=True)
        ws.send_binary(request_frame)

        reply_bytes = ws.recv()
        assert isinstance(reply_bytes, (bytes, bytearray)), f"binary expected, got {type(reply_bytes)}"
        frame = cbor2.loads(reply_bytes)
        # Reply: 0=type(=1), 1=correlation_id, 4=payload, 7=ok.
        assert frame[0] == 1, f"expected Reply discriminant 1, got {frame[0]}"
        assert frame[1] == 1, f"correlation_id mismatch: {frame[1]}"
        assert frame[7] is True, f"reply not ok; payload={frame[4]!r}"
        # Decode EchoReply payload.
        reply_payload = frame[4]
        assert reply_payload.get("data") == b"hello-spec-y", reply_payload
    finally:
        ws.close()


#: A syntactically real browser origin that is deliberately NOT the built-in
#: default, so an assertion about it can only pass if the seed actually arrived.
CORS_SEEDED_ORIGIN = "http://127.0.0.1:45123"


@pytest.fixture
def cors_seeded_nest(request, nest_mode, tmp_path_factory):
    """A nest booted with an explicit browser-origin seed, for the CORS pins below.

    Its own nest rather than ``nest_instance`` because the seed is a BOOT input:
    the whole point of the pins is that the value reaches the running listener
    through the artifact's start path, so a nest that was already up cannot
    answer the question.
    """
    import conftest

    nest, cleanup = conftest._start_dedicated_nest(
        request, nest_mode, tmp_path_factory, "cors-seeded",
        cors_origins=[CORS_SEEDED_ORIGIN],
    )
    yield nest
    cleanup()


def _cors_allow_origin(nest, origin):
    """The ``Access-Control-Allow-Origin`` a nest answers ``/api/v1/health`` with
    for ``origin``, or None when it sends none (which is a browser's refusal)."""
    req = urllib.request.Request(
        f"{nest['url']}/api/v1/health", headers={"Origin": origin}, method="GET"
    )
    resp = urllib.request.urlopen(req, timeout=10)
    assert resp.status == 200, resp.status
    return resp.headers.get("Access-Control-Allow-Origin")


def test_a_seeded_browser_origin_is_allowed_cross_origin(cors_seeded_nest):
    """The harness's CORS seed must reach the LIVE listener, not just the config.

    This is the mechanism behind every web test that dials a nest **raw** — the
    provisioning wizard's Online health poll being the one that made it visible.
    A browser blocks a cross-origin response carrying no
    ``Access-Control-Allow-Origin``, and reqwest-wasm reports that as the generic
    "error sending request", so a nest that silently drops its seed looks exactly
    like a network fault and costs a debugging session to tell the two apart.
    Asserted here in plain HTTP, where the header is either present or it is not
    — no browser, no driver, seconds instead of minutes.

    It is also the regression pin for the seed's own delivery path: ``--cors-origin``
    used to be wired only into the NO-config-file branch of the nest's config
    build, so every invocation passing ``--config`` — the whole harness, and every
    deployment — dropped it in silence (``main.rs::resolve_cors_origins_seed``).
    """
    assert _cors_allow_origin(cors_seeded_nest, CORS_SEEDED_ORIGIN) == CORS_SEEDED_ORIGIN, (
        "the seeded origin must be echoed back — a browser treats a missing "
        "Access-Control-Allow-Origin as a refusal, whatever the status code"
    )


def test_an_unseeded_browser_origin_is_still_refused(cors_seeded_nest):
    """Seeding one origin must not open the nest to any other.

    The negative half matters as much as the positive: a seed that widened into a
    wildcard would make every web test pass while deleting the control it was
    meant to keep exercising (``provisioning/registry.md`` § Health-poll CORS —
    the allow-list is an exact membership check, and the live surface stays the
    app-set ``fauna.admin.set_cors_origins`` state).
    """
    assert _cors_allow_origin(cors_seeded_nest, "http://evil.test:8080") is None, (
        "an origin nobody seeded must get no Access-Control-Allow-Origin at all"
    )
