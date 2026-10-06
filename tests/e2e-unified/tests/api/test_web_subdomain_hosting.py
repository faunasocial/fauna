"""Per-user subdomain web hosting opt-in — the nest-side RPC contract.

`web-content-hosting.md` § Routing (subdomain) + Architectural rule 8: a user
opts their own `web` content into serving at `https://<handle>.<domain>/` over a
per-subdomain HTTP-01 cert. The opt-in is **user-scoped** (the caller toggles
their OWN row — no `actor_id` param, exactly like `fauna.web.publish.*`), default
OFF (privacy / user-controls-their-data).

This is the API-side proof that the opt-in surface works
end-to-end on a real `fauna-nest` binary: the round-trip persists, it is
caller-scoped (one user's opt-in is invisible to another), and a plain User —
not just an Admin — may toggle it (unlike the Admin-only apex designation).

The *serving* half (`GET https://<handle>.<domain>/` returns that user's content)
shares the host-routed serve chain proven by `test_web_apex_hosting`
(`web_content_or_info` → `HostResolver::resolve` → `serve_web_content`), and the
subdomain-specific resolve/cert logic is proven at the Rust level
(`web_content::serve::tests::host_resolver_subdomain` / `host_resolver_remove_subdomain`,
`web_content::cert::tests::issues_per_subdomain_cert_for_opted_in_actor` /
`drops_subdomain_cert_when_actor_opts_out`). `test_subdomain_serves_via_host_header`
below now proves the full Host-routed subdomain serve on a real binary: the
resolver/cert `nest_domain` keys off the canonical resolved handle domain
(`registration.handle_domain ?? node.domain`), so a nest started with only
a domained claim (the shared `start_nest(claim_domain=…)` shape) serves
subdomains (web-content-hosting.md § Implementation status today — the
subdomain "Design note (follow-on)", now resolved).
"""

import time
import urllib.error
import urllib.request

import pytest

from clients.ws_rpc_admin_client import RpcCallError, WsRpcAdminClient
from common.auth import create_actor_and_register, register_handled_actor
from tests.api import ws_api
from tests.api.bare import sign_and_encode_post

pytestmark = pytest.mark.tier_3

INFO_PAGE_MARKER = "Fauna Nest API"
# A real (non-empty) handle domain so the subdomain resolver has a `.<domain>`
# suffix to strip — the shared `nest_instance` runs it empty, which is why the
# serve proof needs its own nest.
SUBDOMAIN_NEST_DOMAIN = "sub.example"


def _actor_client(url: str, actor) -> WsRpcAdminClient:
    return WsRpcAdminClient(
        url,
        actor_id=actor["actor_id_bytes"],
        signing_key=bytes(actor["signing_key"]),
    )


def _get_root_with_host(url: str, host: str) -> tuple[int, str]:
    """`GET /` on the nest with an explicit `Host` header, returning
    (status, body_text). The subdomain resolver routes off `Host`, so this is how
    a localhost-bound tier_3 nest is asked for `https://<handle>.<domain>/`."""
    req = urllib.request.Request(
        url.rstrip("/") + "/", method="GET", headers={"Host": host}
    )
    try:
        resp = urllib.request.urlopen(req)
        return resp.status, resp.read().decode("utf-8", "replace")
    except urllib.error.HTTPError as e:
        return e.code, e.read().decode("utf-8", "replace")


@pytest.fixture
def subdomain_nest(request, nest_mode, tmp_path_factory):
    """A nest with a configured handle domain + open self-service registration, so
    a *handled* user can be provisioned over the wire and resolved as
    `<handle>.<domain>`. Started with only `--handle-domain` (empty `node.domain`)
    — the production-mirror shape that exercises the canonical-handle-domain
    keying (web-content-hosting.md § Implementation status today).

    `claim_domain` is a wire act rather than a boot flag, so every nest mode
    honours it and this fixture routes through the mode provider like any other.
    """
    from conftest import _start_dedicated_nest

    nest, cleanup = _start_dedicated_nest(
        request, nest_mode, tmp_path_factory, "nest-subdomain",
        claim_domain=SUBDOMAIN_NEST_DOMAIN,
    )
    from common.auth import open_registration
    open_registration(nest)
    try:
        yield nest
    finally:
        cleanup()


@pytest.mark.feature("personal-website")
def test_subdomain_serves_via_host_header(subdomain_nest):
    """End-to-end serve proof: an opted-in handled user's web content is served at
    `https://<handle>.<domain>/`, and opting out reverts that host to the apex
    catch-all (here, the info page — no apex actor designated)."""
    url = subdomain_nest["url"]
    port = subdomain_nest["port"]

    # A handled user — the handle keys the subdomain — web-publishes a post (firing
    # the render trigger → a default index.html) and opts into subdomain hosting.
    handle = "alice"
    actor = register_handled_actor(port, handle=handle, domain=SUBDOMAIN_NEST_DOMAIN)
    # Title kept apostrophe-free: the default index HTML-escapes it (e.g.
    # `Alice&#x27;s`), which would defeat a literal substring assertion.
    post_bytes = sign_and_encode_post(
        actor["signing_key"],
        int(time.time() * 1_000_000),
        "Alice Subdomain Home\n\nServed straight from my own handle subdomain.",
    )
    post_id_hex = ws_api.create_post(port, actor, post_bytes)

    with _actor_client(url, actor) as client:
        client.call(
            "fauna.web.publish.set",
            {"post_id": bytes.fromhex(post_id_hex), "slug": "home"},
        )
        client.call("fauna.web.set_subdomain_enabled", {"enabled": True})

    # `GET /` with `Host: alice.sub.example` → the resolver strips `.sub.example`,
    # finds alice's opted-in subdomain mapping, and serves her content.
    fqdn = f"{handle}.{SUBDOMAIN_NEST_DOMAIN}"
    status, body = _get_root_with_host(url, fqdn)
    assert status == 200, body
    assert "Alice Subdomain Home" in body, f"expected subdomain content, got: {body}"
    assert INFO_PAGE_MARKER not in body, f"info page should be shadowed, got: {body}"

    # Opting out drops the mapping → the host falls through to the apex catch-all,
    # and with no apex actor designated that is the built-in info page.
    with _actor_client(url, actor) as client:
        client.call("fauna.web.set_subdomain_enabled", {"enabled": False})
    status, body = _get_root_with_host(url, fqdn)
    assert status == 200, body
    assert INFO_PAGE_MARKER in body, f"expected info page after opt-out, got: {body}"
    assert "Alice Subdomain Home" not in body, f"content should be gone, got: {body}"


@pytest.mark.feature("personal-website")
def test_subdomain_opt_in_round_trips(nest_instance):
    """A user opts into subdomain hosting; the flag persists and clears."""
    url = nest_instance["url"]
    port = nest_instance["port"]
    actor = create_actor_and_register(
        port, admin_signing_key=nest_instance["admin"]["signing_key"]
    )

    with _actor_client(url, actor) as client:
        # Default OFF — a fresh actor has never opted in.
        got = client.call("fauna.web.get_subdomain_enabled", {})
        assert got["enabled"] is False, got

        # Opt in → reply + read-back both report enabled.
        set_reply = client.call("fauna.web.set_subdomain_enabled", {"enabled": True})
        assert set_reply["enabled"] is True, set_reply
        assert client.call("fauna.web.get_subdomain_enabled", {})["enabled"] is True

        # Opt back out → reverts to OFF.
        clear_reply = client.call(
            "fauna.web.set_subdomain_enabled", {"enabled": False}
        )
        assert clear_reply["enabled"] is False, clear_reply
        assert client.call("fauna.web.get_subdomain_enabled", {})["enabled"] is False


@pytest.mark.feature("personal-website")
def test_subdomain_opt_in_is_per_actor(nest_instance):
    """The opt-in is caller-scoped: actor A opting in does not flip actor B."""
    url = nest_instance["url"]
    port = nest_instance["port"]
    admin_sk = nest_instance["admin"]["signing_key"]
    actor_a = create_actor_and_register(port, admin_signing_key=admin_sk)
    actor_b = create_actor_and_register(port, admin_signing_key=admin_sk)

    with _actor_client(url, actor_a) as a:
        a.call("fauna.web.set_subdomain_enabled", {"enabled": True})
        assert a.call("fauna.web.get_subdomain_enabled", {})["enabled"] is True

    # B never opted in — A's opt-in is invisible to B (no global flip).
    with _actor_client(url, actor_b) as b:
        assert b.call("fauna.web.get_subdomain_enabled", {})["enabled"] is False


@pytest.mark.feature("personal-website")
def test_subdomain_opt_in_is_user_class(nest_instance):
    """A plain User (not only an Admin) may toggle subdomain hosting — unlike the
    Admin-only apex designation. Proves the `User | Admin` allowlist arm: the call
    must NOT be rejected as a permission error for a normal registered actor."""
    url = nest_instance["url"]
    port = nest_instance["port"]
    actor = create_actor_and_register(
        port, admin_signing_key=nest_instance["admin"]["signing_key"]
    )
    with _actor_client(url, actor) as client:
        # Should succeed (User-class), not raise permission_denied.
        try:
            reply = client.call("fauna.web.set_subdomain_enabled", {"enabled": True})
        except RpcCallError as e:  # pragma: no cover - failure path
            assert "permission" not in str(e).lower(), (
                f"a User must be allowed to opt into subdomain hosting: {e}"
            )
            raise
        assert reply["enabled"] is True, reply
