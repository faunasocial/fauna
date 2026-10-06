"""What a real nest's reserved ``/app`` answers under the admin's web-app origin
choice — ``web-content-hosting.md`` § Same-origin security model → *The
nest-served `/app/` and the central origin*, on a real ``fauna-nest`` binary.

* The admin kind drives it: ``fauna.admin.web_app_origin.set`` → ``central``
  flips ``GET /app/`` to a **302** (never 301/308) at the central origin with the
  same path and query and ``nest=<this nest's handle domain>``; ``bundled`` flips
  it back. ``fauna.admin.web_app_origin.get`` and the anonymous
  ``fauna.setup.status`` projection report the mode and the exact target.
* A domainless box serves bundled whatever the choice, and says so.
* A non-admin cannot set it.

**Where "bundled" is observed.** A tier_3 nest ships no SPA (``node.static_dir``
unset), so its bundled ``/app`` answers **404** — the path is still reserved and
never falls through to user content (invariant 3). What the SPA itself serves
under bundled is pinned in Rust against the real ``mount_spa``
(``bins/fauna-nest/tests/spa_web_app_origin.rs``); what this file adds is the
state → kind → live router flow, which only a running nest has.
"""

import urllib.error
import urllib.request

import pytest

from clients.ws_rpc_admin_client import RpcCallError, WsRpcAdminClient
from clients.ws_rpc_anon_client import WsRpcAnonClient

pytestmark = pytest.mark.tier_3

CENTRAL = "https://app.fauna.social"


class _NoRedirect(urllib.request.HTTPRedirectHandler):
    def redirect_request(self, req, fp, code, msg, headers, newurl):
        return None


_OPENER = urllib.request.build_opener(_NoRedirect)


def _get(url: str, path: str):
    """GET `path` WITHOUT following a redirect → (status, headers)."""
    try:
        resp = _OPENER.open(url.rstrip("/") + path)
        return resp.status, resp.headers
    except urllib.error.HTTPError as e:
        return e.code, e.headers


def _client(nest, key) -> WsRpcAdminClient:
    return WsRpcAdminClient(
        nest["url"],
        actor_id=bytes(key.verify_key),
        signing_key=bytes(key),
    )


def _set(nest, mode: str) -> dict:
    with _client(nest, nest["admin"]["signing_key"]) as admin:
        return admin.call("fauna.admin.web_app_origin.set", {"mode": mode})


def _status(nest) -> dict:
    with WsRpcAnonClient(nest["url"]) as anon:
        return anon.call("fauna.setup.status", {})


@pytest.mark.feature("admin-nest", "nest-serves-the-app")
def test_the_admin_choice_flips_app_to_the_central_origin_and_back(web_hosting_nest):
    nest = web_hosting_nest
    with _client(nest, nest["admin"]["signing_key"]) as admin:
        before = admin.call("fauna.admin.web_app_origin.get", {})
    assert before["mode"] == "bundled", f"absent row must read bundled: {before}"
    assert not before.get("redirect_target")
    status, headers = _get(nest["url"], "/app/")
    assert status == 404 and headers.get("Location") is None, (
        f"bundled with no shipped SPA must answer 404, not redirect: {status}"
    )

    from conftest import WEB_HOSTING_DOMAIN

    domain = WEB_HOSTING_DOMAIN
    target = f"{CENTRAL}/app/?nest={domain}"

    try:
        reply = _set(nest, "central")
        assert reply["mode"] == "central"
        assert reply["redirect_target"] == target, reply
        assert not reply.get("domainless")

        status, headers = _get(nest["url"], "/app/")
        assert status == 302, f"central must be a TEMPORARY redirect: {status}"
        assert headers["Location"] == target
        assert headers["Cache-Control"] == "no-store"
        assert headers["X-Frame-Options"] == "DENY", "invariant-#5 headers ride it"

        status, headers = _get(nest["url"], "/app/feed/x?tab=2")
        assert status == 302
        assert headers["Location"] == f"{CENTRAL}/app/feed/x?tab=2&nest={domain}"

        s = _status(nest)
        assert s["web_app_origin"] == "central"
        assert s["web_app_origin_target"] == target
        assert s["web_app_origin_domainless"] is False
    finally:
        back = _set(nest, "bundled")

    assert back["mode"] == "bundled" and not back.get("redirect_target")
    status, headers = _get(nest["url"], "/app/")
    assert status == 404 and headers.get("Location") is None, (
        "moving back to bundled must stop the redirect at once"
    )
    s = _status(nest)
    assert s["web_app_origin"] == "bundled"
    assert not s.get("web_app_origin_target")


@pytest.mark.feature("admin-nest")
def test_a_domainless_box_serves_bundled_under_central(domainless_nest):
    nest = domainless_nest
    try:
        reply = _set(nest, "central")
        assert reply["mode"] == "central"
        assert reply["domainless"] is True, reply
        assert not reply.get("redirect_target"), (
            "a domainless box has nothing to pre-fill — never nest=localhost"
        )
        status, headers = _get(nest["url"], "/app/")
        assert status != 302 and headers.get("Location") is None, status
        s = _status(nest)
        assert s["web_app_origin"] == "central"
        assert s["web_app_origin_domainless"] is True
        assert not s.get("web_app_origin_target")
    finally:
        _set(nest, "bundled")


@pytest.mark.feature("admin-nest")
def test_a_non_admin_cannot_set_the_choice(web_hosting_nest):
    nest = web_hosting_nest
    with _client(nest, nest["user"]["signing_key"]) as user:
        with pytest.raises(RpcCallError):
            user.call("fauna.admin.web_app_origin.set", {"mode": "central"})
    status, headers = _get(nest["url"], "/app/")
    assert status != 302 and headers.get("Location") is None, (
        "a refused set must not have moved what /app answers"
    )
