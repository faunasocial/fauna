"""What a real nest's reserved ``/app`` answers under the admin's web-app origin
choice — ``web-content-hosting.md`` § Same-origin security model → *The
nest-served `/app/` and the central origin*, on a real ``fauna-nest`` binary.

* While the central origin is not live (``fauna_protocol::web_app_origin::
  CENTRAL_APP_ORIGIN_LIVE`` is ``false`` — § *While the central origin is not
  live, the choice is not offered*), ``fauna.admin.web_app_origin.set`` →
  ``central`` is refused with ``fauna.admin.web_app_origin.central_not_live``
  and writes nothing, ``GET /app/`` stays bundled, and
  ``fauna.admin.web_app_origin.get`` says ``central_live`` false. ``bundled`` is
  always accepted. A domainless box refuses the same way.
* A non-admin cannot set it.

**Where "bundled" is observed.** A tier_3 nest ships no SPA (``node.static_dir``
unset), so its bundled ``/app`` answers **404** — the path is still reserved and
never falls through to user content (invariant 3); an image nest bundles its SPA
build, so there bundled answers it with a **200**. Either way bundled is never a
redirect, and the nest handle's ``serves_spa`` says which status to expect, so
the release-candidate run asserts the bundled shape it ships rather than
blanking the cell (``testing.md`` § Default app and nest mode). What the SPA itself serves
under bundled is pinned in Rust against the real ``mount_spa``
(``bins/fauna-nest/tests/spa_web_app_origin.rs``); what this file adds is the
state → kind → live router flow, which only a running nest has.
"""

import urllib.error

import pytest

from clients.ws_rpc_admin_client import RpcCallError, WsRpcAdminClient
from clients.ws_rpc_anon_client import WsRpcAnonClient
from common.auth import no_redirect_opener

pytestmark = pytest.mark.tier_3

CENTRAL_NOT_LIVE = "fauna.admin.web_app_origin.central_not_live"


# A redirect is the assertion here, so it is handed back rather than
# followed — under the floor-cert rule a TLS-serving nest needs
# (`common.auth.no_redirect_opener`).
_OPENER = no_redirect_opener()


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


def _bundled_status(nest) -> int:
    """What `/app/` answers under bundled: the SPA build, or the reserved 404."""
    return 200 if nest["serves_spa"] else 404


def _status(nest) -> dict:
    with WsRpcAnonClient(nest["url"]) as anon:
        return anon.call("fauna.setup.status", {})


@pytest.mark.feature("admin-nest", "nest-serves-the-app")
def test_central_is_refused_while_the_central_origin_is_not_live(web_hosting_nest):
    """Rules 2-4 of *While the central origin is not live, the choice is not
    offered*: the projection says ``central_live`` false, a ``central`` set is
    refused with the typed error and writes nothing, ``/app/`` stays bundled,
    and ``bundled`` is always accepted. The flip-to-central journey comes back
    in the commit that flips ``CENTRAL_APP_ORIGIN_LIVE``; until then the redirect
    itself is pinned at tier_1 (``bins/fauna-nest/tests/spa_web_app_origin.rs``)."""
    nest = web_hosting_nest
    with _client(nest, nest["admin"]["signing_key"]) as admin:
        before = admin.call("fauna.admin.web_app_origin.get", {})
        assert before["mode"] == "bundled", f"absent row must read bundled: {before}"
        assert not before.get("redirect_target")
        assert before.get("central_live") is False, (
            f"the nest must say the central origin is not live: {before}"
        )

        with pytest.raises(RpcCallError) as refused:
            admin.call("fauna.admin.web_app_origin.set", {"mode": "central"})
        assert refused.value.code == CENTRAL_NOT_LIVE, refused.value

        after = admin.call("fauna.admin.web_app_origin.get", {})
        assert after["mode"] == "bundled", f"a refused set must write nothing: {after}"

    status, headers = _get(nest["url"], "/app/")
    assert status == _bundled_status(nest) and headers.get("Location") is None, (
        f"/app/ must stay bundled ({_bundled_status(nest)}, serves_spa="
        f"{nest['serves_spa']}), never redirect: {status}"
    )
    s = _status(nest)
    assert s["web_app_origin"] == "bundled"
    assert not s.get("web_app_origin_target")

    back = _set(nest, "bundled")
    assert back["mode"] == "bundled" and not back.get("redirect_target")
    assert back.get("central_live") is False


@pytest.mark.feature("admin-nest")
def test_a_domainless_box_refuses_central_too(domainless_nest):
    """The refusal is decided before the domainless arm: a domainless box says
    "not live", never stores central, and serves bundled."""
    nest = domainless_nest
    with pytest.raises(RpcCallError) as refused:
        _set(nest, "central")
    assert refused.value.code == CENTRAL_NOT_LIVE, refused.value
    status, headers = _get(nest["url"], "/app/")
    assert status != 302 and headers.get("Location") is None, status
    s = _status(nest)
    assert s["web_app_origin"] == "bundled"
    assert not s.get("web_app_origin_target")


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
