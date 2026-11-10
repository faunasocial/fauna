"""tier_3: a hosted plugin's declared ingress answers on a real nest, and the
nest's own routes and a user's site stay the nest's
(``docs/goal/architecture/third-party.md`` § Implementation status today —
nest-terminated ingress, the path half and the whole-name (``sni``) half;
``docs/goal/architecture/security.md`` § Co-resident process trust boundary).

The admin installs the in-repo hello plugin over the nest API, its manifest
declaring one path (``/whoami/``) and one sub-name (``matrix``), and approves
the install card. Then, over plain HTTP to the real binary with an explicit
``Host`` header (the way a loopback-bound nest is asked for a name):

- the path on the nest's own host reaches the plugin, which answers with the
  nest-asserted actor (a user's bearer), sandboxed onto the nest's origin;
- every path on ``matrix.<apex>`` is the plugin's, on its own origin;
- the nest's health route still answers on its own host, and a user's
  published subdomain site still serves her page on the declared path.

The in-process twin over the router alone is
``bins/fauna-nest/tests/plugin_runner.rs``; this is the same promise on the
built binary. API-only, so no app is involved (``testing.md`` § Default app
and nest mode: a ``[nest]`` outcome's witness is an api-tier test).

**Why ``standalone_only``.** The nest must fetch the publisher's document and
module from a loopback server the test owns, through the ``test-hooks``
resolve map, which only a locally spawned nest carries (as
``tests/test_plugin_install.py``).
"""

from __future__ import annotations

import time
import urllib.error
import urllib.request

import pytest

from clients.ws_rpc_admin_client import WsRpcAdminClient
from helpers.admin_wire import admit_adult
from helpers.plugin_publisher import publish_hello_plugin
from tests.api import ws_api
from tests.api.bare import sign_and_encode_post

pytestmark = [pytest.mark.tier_3, pytest.mark.standalone_only]

APEX = "ingress.example"
SUB_NAME = "matrix"
PATH = "/whoami/"
INFO_PAGE_MARKER = "<h1>Fauna nest</h1>"


def _get(url: str, host: str, path: str, bearer: str | None = None):
    """One GET on the nest with an explicit ``Host``: ``(status, csp, body)``."""
    headers = {"Host": host}
    if bearer:
        headers["Authorization"] = f"Bearer {bearer}"
    req = urllib.request.Request(url.rstrip("/") + path, method="GET", headers=headers)
    try:
        resp = urllib.request.urlopen(req, timeout=30)
        return resp.status, resp.headers.get("content-security-policy"), resp.read()
    except urllib.error.HTTPError as e:
        return e.code, e.headers.get("content-security-policy"), e.read()


def _admin(nest) -> WsRpcAdminClient:
    sk = nest["admin"]["signing_key"]
    return WsRpcAdminClient(nest["url"], actor_id=bytes(sk.verify_key), signing_key=bytes(sk))


def _actor(nest, actor) -> WsRpcAdminClient:
    return WsRpcAdminClient(
        nest["url"], actor_id=actor["actor_id_bytes"], signing_key=bytes(actor["signing_key"])
    )


@pytest.fixture(scope="module")
def ingress_publisher(tmp_path_factory):
    publisher = publish_hello_plugin(
        tmp_path_factory.mktemp("plugin-ingress-publisher"),
        ingress=[{"path": PATH}, {"sni": SUB_NAME}],
    )
    yield publisher
    publisher["server"].close()


@pytest.fixture
def ingress_nest(nest_binary, tmp_path_factory, ingress_publisher):
    """A nest claimed onto ``APEX`` (so a sub-name and a user subdomain have a
    suffix) whose guarded fetcher reaches the publisher's loopback server."""
    from conftest import _make_nest

    nest, cleanup = _make_nest(
        nest_binary,
        tmp_path_factory,
        "plugin-ingress",
        claim_domain=APEX,
        extra_env=ingress_publisher["server"].nest_env(),
    )
    yield nest
    cleanup()


def _install_and_wait_running(nest, document_url: str) -> dict:
    """Install over the admin API, approve the card, and wait (bounded, on the
    nest's own state) for the runner to report ``running``."""
    with _admin(nest) as ws:
        card = ws.call("fauna.plugins.install", {"document_url": document_url})["consent"]
        install = card.get("install") or {}
        assert install.get("ingress_paths") == [PATH], card
        assert install.get("ingress_names") == [f"{SUB_NAME}.{APEX}"], card
        ws.call(
            "fauna.bridges.atproto.resolve_consent",
            {"consent_id": card["consent_id"], "approved": True},
        )
        deadline = time.monotonic() + 60
        plugins: list = []
        while time.monotonic() < deadline:
            plugins = ws.call("fauna.plugins.list", {})["plugins"]
            if plugins and plugins[0]["status"]["state"] == "running":
                return plugins[0]
            time.sleep(0.25)
    pytest.fail(f"the plugin never reported running; the nest lists: {plugins!r}")


@pytest.mark.feature("bridges")
def test_a_plugins_declared_path_and_sub_name_answer_and_the_nests_own_stay_put(
    ingress_nest, ingress_publisher
):
    url = ingress_nest["url"]

    # A user with a published subdomain site, before the plugin arrives.
    alice = admit_adult(ingress_nest, "alice", domain=APEX, token=True)
    post = sign_and_encode_post(
        alice["signing_key"],
        int(time.time() * 1_000_000),
        "Alice Ingress Home\n\nMy own site, not a plugin's.",
    )
    post_id_hex = ws_api.create_post(ingress_nest["port"], alice, post)
    with _actor(ingress_nest, alice) as client:
        client.call("fauna.web.publish.set", {"post_id": bytes.fromhex(post_id_hex), "slug": "home"})
        client.call("fauna.web.set_subdomain_enabled", {"enabled": True})
    alice_host = f"alice.{APEX}"
    status, _, body = _get(url, alice_host, "/")
    assert status == 200 and b"Alice Ingress Home" in body, (status, body[:400])

    _install_and_wait_running(ingress_nest, ingress_publisher["document_url"])
    token = alice["token"]
    alice_id = alice["actor_id_bytes"]

    # The path half: the nest's own host, the nest-asserted actor, sandboxed.
    status, csp, body = _get(url, APEX, PATH + "me", bearer=token)
    assert (status, body) == (200, alice_id), (status, csp, body[:200])
    assert csp == "sandbox", f"plugin output on the nest origin must be sandboxed: {csp!r}"
    # No bearer → anonymous, which the hello plugin answers 401.
    assert _get(url, APEX, PATH + "me")[0] == 401

    # The whole-name half: every path on the claimed name is the plugin's,
    # on its own origin (no forced sandbox) — the nest's own route included.
    sub_host = f"{SUB_NAME}.{APEX}"
    status, csp, body = _get(url, sub_host, PATH + "me", bearer=token)
    assert (status, body) == (200, alice_id), (status, csp, body[:200])
    assert csp is None, f"a claimed name is its own origin: {csp!r}"
    assert _get(url, sub_host, "/api/v1/health")[0] == 404, "the plugin's 404, not the nest's health"

    # The nest's own route on its own host is untouched.
    status, csp, _ = _get(url, APEX, "/api/v1/health")
    assert status == 200 and csp != "sandbox", (status, csp)

    # A user's site is untouched: her host still serves her page, and the
    # declared path on her host is not proxied to the plugin.
    status, _, body = _get(url, alice_host, "/")
    assert status == 200 and b"Alice Ingress Home" in body, (status, body[:400])
    assert INFO_PAGE_MARKER.encode() not in body
    status, csp, body = _get(url, alice_host, PATH + "me", bearer=token)
    assert csp != "sandbox" and body != alice_id, (status, csp, body[:200])
