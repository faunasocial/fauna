"""Admin web apex hosting — the nest-side serve→render→designate contract.

`web-content-hosting.md` § Admin apex hosting: the deployment admin designates
one actor (`fauna.web.set_apex_actor`, Admin-class) whose web-published content
serves the apex path `https://<domain>/`, the nest-wide-singleton analogue of the
catch-all *mail* actor. Clearing the designation reverts `/` to the built-in nest
info page.

This is the API-side proof that the previously-dormant
serve→render chain is live end-to-end on a real `fauna-nest` binary:

  1. baseline — no apex designated → `GET /` serves the info page;
  2. a user authors + web-publishes a post (`fauna.posts.create` +
     `fauna.web.publish.set`), which fires the render trigger;
  3. the admin designates that actor as the apex (`fauna.web.set_apex_actor`);
  4. `GET /` now serves the actor's rendered site (not the info page);
  5. clearing the designation (`actor_id: null`) reverts `/` to the info page.

The test nest's `node.domain` is empty, so the apex resolver's catch-all fires
for the nest's own loopback host — `GET /` needs no special Host header
(`HostResolver::resolve` → apex actor for the node domain / bare / unmatched
host, web-content-hosting.md § Routing). The default test nest has a blob store,
so `web_content_service` is `Some` and serving is active.
"""

import time
import urllib.error
import urllib.request

import pytest

from clients.ws_rpc_admin_client import RpcCallError, WsRpcAdminClient
from common.auth import create_actor_and_register
from tests.api import ws_api
from tests.api.bare import sign_and_encode_post

pytestmark = pytest.mark.tier_3

INFO_PAGE_MARKER = "Fauna Nest API"


def _admin_client(nest_instance) -> WsRpcAdminClient:
    admin = nest_instance["admin"]
    return WsRpcAdminClient(
        nest_instance["url"],
        actor_id=bytes(admin["signing_key"].verify_key),
        signing_key=bytes(admin["signing_key"]),
    )


def _get_root(url: str) -> tuple[int, str]:
    """`GET /` on the nest, returning (status, body_text)."""
    req = urllib.request.Request(url.rstrip("/") + "/", method="GET")
    try:
        resp = urllib.request.urlopen(req)
        return resp.status, resp.read().decode("utf-8", "replace")
    except urllib.error.HTTPError as e:
        return e.code, e.read().decode("utf-8", "replace")


@pytest.mark.feature("admin-front-page")
def test_admin_apex_hosting_serves_then_clears(nest_instance):
    url = nest_instance["url"]
    port = nest_instance["port"]
    admin_signing_key = nest_instance["admin"]["signing_key"]

    # 1. Baseline — no apex actor designated → the built-in info page.
    status, body = _get_root(url)
    assert status == 200, body
    assert INFO_PAGE_MARKER in body, f"expected info page at baseline, got: {body}"

    # 2. A normal user authors a post and web-publishes it (firing the render
    #    trigger, which writes a default index.html into web_rendered).
    actor = create_actor_and_register(port, admin_signing_key=admin_signing_key)
    post_bytes = sign_and_encode_post(
        actor["signing_key"],
        int(time.time() * 1_000_000),
        "My Apex Home\n\nServed straight from my own nest.",
    )
    post_id_hex = ws_api.create_post(port, actor, post_bytes)

    actor_client = WsRpcAdminClient(
        url,
        actor_id=actor["actor_id_bytes"],
        signing_key=bytes(actor["signing_key"]),
    )
    with actor_client:
        actor_client.call(
            "fauna.web.publish.set",
            {"post_id": bytes.fromhex(post_id_hex), "slug": "home"},
        )

    # 3. The admin designates that actor as the apex.
    with _admin_client(nest_instance) as admin:
        reply = admin.call(
            "fauna.web.set_apex_actor", {"actor_id": actor["actor_id_bytes"]}
        )
        assert bytes(reply["actor_id"]) == bytes(actor["actor_id_bytes"]), reply
        # get reads back the same designation.
        got = admin.call("fauna.web.get_apex_actor", {})
        assert bytes(got["actor_id"]) == bytes(actor["actor_id_bytes"]), got

    # 4. `GET /` now serves the apex actor's rendered content, not the info page.
    status, body = _get_root(url)
    assert status == 200, body
    assert "My Apex Home" in body, f"expected apex content at /, got: {body}"
    assert INFO_PAGE_MARKER not in body, f"info page should be shadowed, got: {body}"

    # 5. Clearing the designation reverts `/` to the info page.
    with _admin_client(nest_instance) as admin:
        cleared = admin.call("fauna.web.set_apex_actor", {"actor_id": None})
        assert cleared.get("actor_id") is None, cleared
        got = admin.call("fauna.web.get_apex_actor", {})
        assert got.get("actor_id") is None, got

    status, body = _get_root(url)
    assert status == 200, body
    assert INFO_PAGE_MARKER in body, f"expected info page after clear, got: {body}"
    assert "My Apex Home" not in body, f"apex content should be gone, got: {body}"


@pytest.mark.feature("admin-front-page")
def test_set_apex_actor_is_admin_only(nest_instance):
    """A plain User must not be able to designate the apex actor — it is an
    Admin-class deployment setting (bridge_method_allowlist), unlike the
    per-user `fauna.web.publish.*` kinds."""
    port = nest_instance["port"]
    url = nest_instance["url"]
    actor = create_actor_and_register(
        port, admin_signing_key=nest_instance["admin"]["signing_key"]
    )
    client = WsRpcAdminClient(
        url, actor_id=actor["actor_id_bytes"], signing_key=bytes(actor["signing_key"])
    )
    with client:
        with pytest.raises(RpcCallError) as ei:
            client.call("fauna.web.set_apex_actor", {"actor_id": actor["actor_id_bytes"]})
        # Denied at the allowlist gate (permission_denied), not a malformed/other error.
        assert "permission" in str(ei.value).lower(), str(ei.value)
